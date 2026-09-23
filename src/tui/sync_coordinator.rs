//! Root-backup observation and execution policy. `App` owns execution.

use anyhow::Result;
use skills::ops::sync::{AutoSyncDisposition, Status};
use std::time::{Duration, Instant};

const PROBE_INTERVAL: Duration = Duration::from_secs(60);
const PROBE_RETRY_DELAYS: [Duration; 4] = [
    Duration::from_secs(60),
    Duration::from_secs(2 * 60),
    Duration::from_secs(5 * 60),
    Duration::from_secs(15 * 60),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeReason {
    Startup,
    Periodic,
    Mutation,
    Manual,
    AfterRun,
    ObservedChange,
    Configuration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunState {
    Idle,
    Reconciling { automatic: bool },
    Publishing { automatic: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Fingerprint {
    changes: Vec<String>,
    local_revision: Option<String>,
    remote_revision: Option<String>,
}

impl From<&Status> for Fingerprint {
    fn from(status: &Status) -> Self {
        Self {
            changes: status.changes.clone(),
            local_revision: status.local_revision.clone(),
            remote_revision: status.remote_revision.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum AutoState {
    Idle,
    NeedsProbe,
    Ready,
    RetryAfterProbe,
    PausedFatal(Option<Fingerprint>),
}

#[derive(Debug, Clone, Copy)]
struct ActiveProbe {
    id: u64,
    remote: bool,
    reason: ProbeReason,
    library_generation: u64,
}

pub struct ProbeRequest {
    pub id: u64,
}

pub struct ProbeOutcome {
    pub accepted: bool,
    pub needs_validation: bool,
}

pub struct AutoSyncRequest {
    pub expected_changes: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncActivity {
    NotConfigured,
    Ready,
    Waiting,
    Checking,
    Syncing,
    Retrying,
    Attention,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncPresentation {
    pub activity: SyncActivity,
    pub automatic: bool,
    pub detail: Option<String>,
}

pub struct SyncCoordinator {
    pub status: Option<Status>,
    pub error: Option<String>,
    probe: Option<ActiveProbe>,
    next_probe_id: u64,
    next_remote_probe: Instant,
    probe_failures: usize,
    run: RunState,
    auto: AutoState,
    claimed_generation: Option<u64>,
    library_generation: u64,
}

impl Default for SyncCoordinator {
    fn default() -> Self {
        Self {
            status: None,
            error: None,
            probe: None,
            next_probe_id: 0,
            next_remote_probe: Instant::now(),
            probe_failures: 0,
            run: RunState::Idle,
            auto: AutoState::Idle,
            claimed_generation: None,
            library_generation: 0,
        }
    }
}

impl SyncCoordinator {
    pub fn probing(&self) -> bool {
        self.probe.is_some()
    }

    pub fn syncing(&self) -> bool {
        !matches!(self.run, RunState::Idle)
    }

    pub fn switching(&self) -> bool {
        matches!(self.run, RunState::Reconciling { .. })
    }

    pub fn presentation(&self) -> SyncPresentation {
        let configured = self.status.as_ref().is_some_and(|status| {
            status.settings.url.is_some() && status.settings.branch.is_some()
        });
        let automatic = self
            .status
            .as_ref()
            .is_some_and(|status| status.settings.enabled);
        let activity = if !configured {
            SyncActivity::NotConfigured
        } else if !matches!(self.run, RunState::Idle) {
            SyncActivity::Syncing
        } else if self.probe.is_some() {
            SyncActivity::Checking
        } else {
            match self.auto {
                AutoState::RetryAfterProbe => SyncActivity::Retrying,
                AutoState::PausedFatal(_) => SyncActivity::Attention,
                AutoState::NeedsProbe | AutoState::Ready => SyncActivity::Waiting,
                AutoState::Idle => {
                    if self.status.as_ref().is_some_and(needs_sync) {
                        SyncActivity::Waiting
                    } else {
                        SyncActivity::Ready
                    }
                }
            }
        };
        SyncPresentation {
            activity,
            automatic,
            detail: self.error.clone(),
        }
    }

    #[cfg(test)]
    pub fn pending(&self) -> bool {
        matches!(self.auto, AutoState::NeedsProbe | AutoState::Ready)
    }

    pub fn probe_due(&self, now: Instant) -> bool {
        self.probe.is_none() && now >= self.next_remote_probe
    }

    pub fn request_probe(&mut self, remote: bool, reason: ProbeReason) -> Option<ProbeRequest> {
        if self.switching() || self.probe.is_some() {
            return None;
        }
        self.next_probe_id = self.next_probe_id.wrapping_add(1);
        self.probe = Some(ActiveProbe {
            id: self.next_probe_id,
            remote,
            reason,
            library_generation: self.library_generation,
        });
        self.error = None;
        if remote {
            self.next_remote_probe = Instant::now() + PROBE_INTERVAL;
        }
        Some(ProbeRequest {
            id: self.next_probe_id,
        })
    }

    pub fn finish_probe(&mut self, id: u64, result: Result<Status>) -> ProbeOutcome {
        let Some(probe) = self.probe.filter(|probe| probe.id == id) else {
            return ProbeOutcome {
                accepted: false,
                needs_validation: false,
            };
        };
        self.probe = None;
        let needs_validation = probe.library_generation != self.library_generation;
        match result {
            Ok(status) => {
                self.probe_failures = 0;
                if probe.remote {
                    self.next_remote_probe = Instant::now() + PROBE_INTERVAL;
                }
                if !needs_validation {
                    self.update_auto_state(&status, probe.reason);
                }
                self.status = Some(status);
                self.error = None;
            }
            Err(error) => {
                self.probe_failures = (self.probe_failures + 1).min(PROBE_RETRY_DELAYS.len());
                self.next_remote_probe =
                    Instant::now() + PROBE_RETRY_DELAYS[self.probe_failures - 1];
                self.error = Some(format!("{error:#}"));
            }
        }
        ProbeOutcome {
            accepted: true,
            needs_validation,
        }
    }

    fn update_auto_state(&mut self, status: &Status, reason: ProbeReason) {
        let enabled = status.settings.enabled
            && status.settings.url.is_some()
            && status.settings.branch.is_some();
        if !enabled {
            self.auto = AutoState::Idle;
            return;
        }
        let next = if needs_sync(status) {
            AutoState::Ready
        } else {
            AutoState::Idle
        };
        self.auto = match &self.auto {
            AutoState::RetryAfterProbe if !probe_can_retry(reason) => AutoState::RetryAfterProbe,
            AutoState::PausedFatal(None) => AutoState::PausedFatal(Some(Fingerprint::from(status))),
            AutoState::PausedFatal(Some(failed)) if failed != &Fingerprint::from(status) => next,
            AutoState::PausedFatal(_) => self.auto.clone(),
            _ => next,
        };
    }

    pub fn library_changed(&mut self) {
        self.library_generation = self.library_generation.wrapping_add(1);
        if !matches!(self.auto, AutoState::PausedFatal(_)) {
            self.auto = AutoState::NeedsProbe;
        }
    }

    pub fn external_changed(&mut self) {
        self.library_changed();
    }

    pub fn take_auto_sync(&mut self, safe: bool) -> Option<AutoSyncRequest> {
        if !safe || !matches!(self.run, RunState::Idle) || !matches!(self.auto, AutoState::Ready) {
            return None;
        }
        let status = self.status.as_ref()?;
        if !status.settings.enabled {
            return None;
        }
        let expected_changes = status.changes.clone();
        self.auto = AutoState::Idle;
        self.claimed_generation = Some(self.library_generation);
        self.run = RunState::Reconciling { automatic: true };
        Some(AutoSyncRequest { expected_changes })
    }

    pub fn start_manual_sync(&mut self) {
        self.claimed_generation = Some(self.library_generation);
        self.run = RunState::Reconciling { automatic: false };
    }

    pub fn start_publishing(&mut self) {
        if let RunState::Reconciling { automatic } = self.run {
            self.run = RunState::Publishing { automatic };
        }
    }

    pub fn finish_manual_sync(&mut self, _success: bool) {
        self.run = RunState::Idle;
        let changed_during_run = self
            .claimed_generation
            .take()
            .is_some_and(|generation| generation != self.library_generation);
        self.auto = if changed_during_run {
            AutoState::NeedsProbe
        } else {
            AutoState::Idle
        };
    }

    pub fn finish_auto_sync(&mut self, result: std::result::Result<(), AutoSyncDisposition>) {
        let was_automatic = matches!(
            self.run,
            RunState::Reconciling { automatic: true } | RunState::Publishing { automatic: true }
        );
        self.run = RunState::Idle;
        if !was_automatic {
            return;
        }
        let changed_during_run = self
            .claimed_generation
            .take()
            .is_some_and(|generation| generation != self.library_generation);
        match result {
            Ok(()) => {
                self.auto = if changed_during_run {
                    AutoState::NeedsProbe
                } else {
                    AutoState::Idle
                };
            }
            Err(AutoSyncDisposition::Transient) => {
                self.auto = AutoState::RetryAfterProbe;
            }
            Err(AutoSyncDisposition::WorkingTreeChanged) => {
                self.auto = AutoState::NeedsProbe;
            }
            Err(AutoSyncDisposition::Fatal) => {
                self.auto = AutoState::PausedFatal(None);
            }
        }
    }

    pub fn disable(&mut self) {
        self.auto = AutoState::Idle;
    }

    #[cfg(test)]
    pub fn set_status(&mut self, status: Status) {
        self.status = Some(status);
        self.probe = None;
    }

    #[cfg(test)]
    pub fn set_error(&mut self, error: Option<String>) {
        self.error = error;
    }

    #[cfg(test)]
    pub fn set_probing(&mut self, probing: bool) {
        self.probe = probing.then_some(ActiveProbe {
            id: self.next_probe_id,
            remote: true,
            reason: ProbeReason::Manual,
            library_generation: self.library_generation,
        });
    }

    #[cfg(test)]
    pub fn set_syncing(&mut self, syncing: bool) {
        self.run = if syncing {
            RunState::Reconciling { automatic: false }
        } else {
            RunState::Idle
        };
    }
}

fn needs_sync(status: &Status) -> bool {
    !status.changes.is_empty() || status.ahead > 0 || status.behind > 0
}

fn probe_can_retry(reason: ProbeReason) -> bool {
    matches!(
        reason,
        ProbeReason::Periodic | ProbeReason::Mutation | ProbeReason::ObservedChange
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use skills::ops::sync::Settings;

    fn status(enabled: bool, changes: &[&str], local: &str, remote: &str) -> Status {
        Status {
            settings: Settings {
                url: Some("remote".into()),
                branch: Some("main".into()),
                enabled,
            },
            changes: changes.iter().map(|value| (*value).into()).collect(),
            ahead: usize::from(local != remote),
            behind: usize::from(local != remote),
            remote_checked: true,
            local_revision: Some(local.into()),
            remote_revision: Some(remote.into()),
        }
    }

    fn probe(sync: &mut SyncCoordinator, reason: ProbeReason, status: Status) {
        let id = sync.request_probe(true, reason).unwrap().id;
        assert!(sync.finish_probe(id, Ok(status)).accepted);
    }

    #[test]
    fn startup_probe_schedules_any_work_that_needs_convergence() {
        for pending in [
            status(true, &["?? changed"], "base", "base"),
            status(true, &[], "local", "remote"),
        ] {
            let mut sync = SyncCoordinator::default();
            probe(&mut sync, ProbeReason::Startup, pending);
            assert!(sync.pending());
            assert!(sync.take_auto_sync(true).is_some());
        }

        let mut clean = SyncCoordinator::default();
        probe(
            &mut clean,
            ProbeReason::Startup,
            status(true, &[], "base", "base"),
        );
        assert!(!clean.pending());
    }

    #[test]
    fn observed_external_changes_are_reprobed_and_synced() {
        let mut sync = SyncCoordinator::default();
        probe(
            &mut sync,
            ProbeReason::Startup,
            status(true, &[], "base", "base"),
        );
        sync.external_changed();
        assert!(sync.pending());
        assert!(sync.take_auto_sync(true).is_none());

        probe(
            &mut sync,
            ProbeReason::ObservedChange,
            status(true, &[" M skill"], "base", "base"),
        );
        assert!(sync.take_auto_sync(true).is_some());
    }

    #[test]
    fn working_tree_change_during_automatic_sync_requeues_latest_input() {
        let mut sync = SyncCoordinator::default();
        probe(
            &mut sync,
            ProbeReason::Startup,
            status(true, &[" M first"], "base", "base"),
        );
        sync.take_auto_sync(true).unwrap();
        sync.finish_auto_sync(Err(AutoSyncDisposition::WorkingTreeChanged));
        assert!(sync.pending());
        assert!(sync.take_auto_sync(true).is_none());

        probe(
            &mut sync,
            ProbeReason::ObservedChange,
            status(true, &[" M first", "?? second"], "base", "base"),
        );
        assert!(sync.take_auto_sync(true).is_some());
    }

    #[test]
    fn disabled_and_fatal_paused_states_still_accept_probes() {
        let mut sync = SyncCoordinator::default();
        probe(
            &mut sync,
            ProbeReason::Startup,
            status(false, &[], "local", "remote"),
        );
        assert!(!sync.pending());

        sync.library_changed();
        probe(
            &mut sync,
            ProbeReason::Mutation,
            status(true, &["?? changed"], "local", "remote"),
        );
        sync.take_auto_sync(true).unwrap();
        sync.finish_auto_sync(Err(AutoSyncDisposition::Fatal));
        probe(
            &mut sync,
            ProbeReason::AfterRun,
            status(true, &[], "backup", "remote"),
        );
        assert!(sync.probe.is_none());
        assert!(!sync.pending());
    }

    #[test]
    fn fatal_failure_retries_only_after_the_sync_input_changes() {
        let mut sync = SyncCoordinator::default();
        probe(
            &mut sync,
            ProbeReason::Startup,
            status(true, &[], "base", "base"),
        );
        sync.library_changed();
        probe(
            &mut sync,
            ProbeReason::Mutation,
            status(true, &[" M skill"], "base", "base"),
        );
        sync.take_auto_sync(true).unwrap();
        sync.finish_auto_sync(Err(AutoSyncDisposition::Fatal));

        let failed = status(true, &[], "backup", "remote");
        probe(&mut sync, ProbeReason::AfterRun, failed.clone());
        probe(&mut sync, ProbeReason::Periodic, failed);
        assert!(!sync.pending());
        assert!(sync.take_auto_sync(true).is_none());

        probe(
            &mut sync,
            ProbeReason::Periodic,
            status(true, &[], "backup", "remote-2"),
        );
        assert!(sync.take_auto_sync(true).is_some());
    }

    #[test]
    fn transient_failure_waits_for_a_normal_probe() {
        let mut sync = SyncCoordinator::default();
        probe(
            &mut sync,
            ProbeReason::Startup,
            status(true, &[], "base", "base"),
        );
        sync.library_changed();
        probe(
            &mut sync,
            ProbeReason::Mutation,
            status(true, &["?? changed"], "base", "base"),
        );
        sync.take_auto_sync(true).unwrap();
        sync.finish_auto_sync(Err(AutoSyncDisposition::Transient));

        probe(
            &mut sync,
            ProbeReason::AfterRun,
            status(true, &[], "backup", "base"),
        );
        assert!(sync.take_auto_sync(true).is_none());
        probe(
            &mut sync,
            ProbeReason::Periodic,
            status(true, &[], "backup", "base"),
        );
        assert!(sync.take_auto_sync(true).is_some());
    }

    #[test]
    fn probe_backoff_is_independent_of_execution_state() {
        let mut sync = SyncCoordinator::default();
        let start = Instant::now();
        for expected_delay in PROBE_RETRY_DELAYS {
            let id = sync.request_probe(true, ProbeReason::Periodic).unwrap().id;
            sync.finish_probe(id, Err(anyhow::anyhow!("offline")));
            let remaining = sync
                .next_remote_probe
                .saturating_duration_since(Instant::now());
            assert!(remaining <= expected_delay);
            assert!(remaining >= expected_delay.saturating_sub(Duration::from_secs(1)));
            sync.next_remote_probe = start;
        }

        sync.start_manual_sync();
        assert!(sync.probe_due(Instant::now()));
        sync.finish_manual_sync(false);
    }

    #[test]
    fn mutation_during_automatic_publish_is_retained() {
        let mut sync = SyncCoordinator::default();
        probe(
            &mut sync,
            ProbeReason::Startup,
            status(true, &[], "base", "base"),
        );
        sync.library_changed();
        probe(
            &mut sync,
            ProbeReason::Mutation,
            status(true, &["?? first"], "base", "base"),
        );
        sync.take_auto_sync(true).unwrap();
        sync.start_publishing();
        sync.library_changed();
        sync.finish_auto_sync(Ok(()));
        assert!(sync.pending());
    }

    #[test]
    fn probe_started_before_mutation_cannot_authorize_sync() {
        let mut sync = SyncCoordinator::default();
        let id = sync.request_probe(true, ProbeReason::Startup).unwrap().id;
        sync.library_changed();
        let outcome = sync.finish_probe(id, Ok(status(true, &[], "base", "base")));
        assert!(outcome.needs_validation);
        assert!(sync.take_auto_sync(true).is_none());

        probe(
            &mut sync,
            ProbeReason::Mutation,
            status(true, &["?? changed"], "base", "base"),
        );
        assert!(sync.take_auto_sync(true).is_some());
    }

    #[test]
    fn unchanged_app_event_does_not_clear_fatal_pause() {
        let mut sync = SyncCoordinator::default();
        probe(
            &mut sync,
            ProbeReason::Startup,
            status(true, &[], "base", "base"),
        );
        sync.library_changed();
        probe(
            &mut sync,
            ProbeReason::Mutation,
            status(true, &[" M skill"], "base", "base"),
        );
        sync.take_auto_sync(true).unwrap();
        sync.finish_auto_sync(Err(AutoSyncDisposition::Fatal));
        let failed = status(true, &[], "backup", "remote");
        probe(&mut sync, ProbeReason::AfterRun, failed.clone());

        sync.library_changed();
        probe(&mut sync, ProbeReason::Mutation, failed);
        assert!(sync.take_auto_sync(true).is_none());
    }

    #[test]
    fn failed_manual_sync_does_not_replay_an_existing_auto_intent() {
        let mut sync = SyncCoordinator::default();
        probe(
            &mut sync,
            ProbeReason::Startup,
            status(true, &[], "base", "base"),
        );
        sync.library_changed();
        probe(
            &mut sync,
            ProbeReason::Mutation,
            status(true, &["?? changed"], "base", "base"),
        );
        sync.start_manual_sync();
        sync.finish_manual_sync(false);
        assert!(sync.take_auto_sync(true).is_none());
    }

    #[test]
    fn mutation_during_manual_publish_is_retained() {
        let mut sync = SyncCoordinator::default();
        probe(
            &mut sync,
            ProbeReason::Startup,
            status(true, &[], "base", "base"),
        );
        sync.start_manual_sync();
        sync.start_publishing();
        sync.library_changed();
        sync.finish_manual_sync(true);
        assert!(sync.pending());

        probe(
            &mut sync,
            ProbeReason::AfterRun,
            status(true, &["?? later"], "manual", "manual"),
        );
        assert!(sync.take_auto_sync(true).is_some());
    }

    #[test]
    fn probes_do_not_overwrite_run_state() {
        let mut sync = SyncCoordinator::default();
        sync.start_manual_sync();
        assert!(sync.request_probe(true, ProbeReason::Periodic).is_none());
        sync.start_publishing();
        let id = sync.request_probe(true, ProbeReason::Periodic).unwrap().id;
        sync.finish_probe(id, Ok(status(true, &[], "local", "remote")));
        assert!(sync.syncing());
        sync.finish_manual_sync(true);
        assert!(!sync.syncing());
    }

    #[test]
    fn stale_probe_results_are_ignored() {
        let mut sync = SyncCoordinator::default();
        let current = sync.request_probe(true, ProbeReason::Manual).unwrap().id;
        assert!(
            !sync
                .finish_probe(current.wrapping_sub(1), Ok(status(true, &[], "old", "old")),)
                .accepted
        );
        sync.finish_probe(current, Ok(status(true, &[], "new", "new")));
        assert_eq!(
            sync.status.as_ref().unwrap().local_revision.as_deref(),
            Some("new")
        );
    }
}
