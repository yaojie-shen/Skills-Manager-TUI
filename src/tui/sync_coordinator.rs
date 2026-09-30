//! Root-backup observation and execution policy. `App` owns execution.

use anyhow::Result;
use skills::ops::sync::{AutoSyncDisposition, AutomaticSyncInput, Status, WorktreeSnapshot};
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
    Stability,
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
    worktree: Option<WorktreeSnapshot>,
    local_revision: Option<String>,
    remote_revision: Option<String>,
}

impl From<&Status> for Fingerprint {
    fn from(status: &Status) -> Self {
        Self {
            worktree: status.worktree.clone(),
            local_revision: status.local_revision.clone(),
            remote_revision: status.remote_revision.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum AutoState {
    Idle,
    NeedsProbe,
    Stabilizing,
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
    pub expected: AutomaticSyncInput,
}

#[derive(Debug, Clone)]
struct StabilityCandidate {
    snapshot: WorktreeSnapshot,
    stable_since: Instant,
    sampled_at: Instant,
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
    stability: Option<StabilityCandidate>,
    quiet_window: Duration,
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
            stability: None,
            quiet_window: super::settings::SyncSettings::default().quiet_window,
        }
    }
}

impl SyncCoordinator {
    /// Retain accumulated stability, but require a fresh successful observation
    /// before a changed policy can authorize an automatic run.
    pub fn set_quiet_window(&mut self, window: Duration) {
        if self.quiet_window != window {
            self.quiet_window = window;
            if self.stability.is_some() && matches!(self.auto, AutoState::Ready) {
                self.auto = AutoState::Stabilizing;
            }
        }
    }

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
                AutoState::NeedsProbe | AutoState::Stabilizing | AutoState::Ready => {
                    SyncActivity::Waiting
                }
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
    pub fn mature_stability(&mut self) {
        if let Some(candidate) = self.stability.as_mut() {
            candidate.stable_since = Instant::now() - self.quiet_window;
            candidate.sampled_at = Instant::now() - Duration::from_secs(60);
        }
    }

    #[cfg(test)]
    pub fn pending(&self) -> bool {
        matches!(
            self.auto,
            AutoState::NeedsProbe | AutoState::Stabilizing | AutoState::Ready
        )
    }

    pub fn probe_due(&self, now: Instant) -> bool {
        self.probe.is_none() && now >= self.next_remote_probe
    }

    pub fn stability_probe_due(&self, now: Instant, interval: Duration) -> bool {
        self.probe.is_none()
            && !self.switching()
            && self.stability.as_ref().is_some_and(|candidate| {
                now.saturating_duration_since(candidate.sampled_at) >= interval
            })
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
        self.finish_probe_at(id, result, Instant::now())
    }

    fn finish_probe_at(&mut self, id: u64, result: Result<Status>, now: Instant) -> ProbeOutcome {
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
                    self.update_auto_state(&status, probe.reason, now);
                }
                self.status = Some(status);
                self.error = None;
            }
            Err(error) => {
                self.probe_failures = (self.probe_failures + 1).min(PROBE_RETRY_DELAYS.len());
                self.next_remote_probe =
                    Instant::now() + PROBE_RETRY_DELAYS[self.probe_failures - 1];
                if let Some(candidate) = self.stability.as_mut() {
                    candidate.sampled_at = now;
                }
                self.error = Some(format!("{error:#}"));
            }
        }
        ProbeOutcome {
            accepted: true,
            needs_validation,
        }
    }

    fn update_auto_state(&mut self, status: &Status, reason: ProbeReason, now: Instant) {
        let enabled = status.settings.enabled
            && status.settings.url.is_some()
            && status.settings.branch.is_some();
        if !enabled {
            self.auto = AutoState::Idle;
            self.stability = None;
            return;
        }
        let next = if status
            .worktree
            .as_ref()
            .is_some_and(|snapshot| snapshot.has_local_changes)
        {
            let Some(snapshot) = status.worktree.clone() else {
                self.auto = AutoState::NeedsProbe;
                return;
            };
            match self.stability.as_mut() {
                Some(candidate) if candidate.snapshot == snapshot => {
                    candidate.sampled_at = now;
                    if now.saturating_duration_since(candidate.stable_since) >= self.quiet_window {
                        AutoState::Ready
                    } else {
                        AutoState::Stabilizing
                    }
                }
                _ => {
                    self.stability = Some(StabilityCandidate {
                        snapshot,
                        stable_since: now,
                        sampled_at: now,
                    });
                    if self.quiet_window.is_zero() {
                        AutoState::Ready
                    } else {
                        AutoState::Stabilizing
                    }
                }
            }
        } else {
            self.stability = None;
            if needs_sync(status) {
                AutoState::Ready
            } else {
                AutoState::Idle
            }
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
        let expected = AutomaticSyncInput {
            snapshot: status.worktree.clone()?,
        };
        self.auto = AutoState::Idle;
        self.claimed_generation = Some(self.library_generation);
        self.run = RunState::Reconciling { automatic: true };
        Some(AutoSyncRequest { expected })
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
                self.stability = None;
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
        self.stability = None;
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
        ProbeReason::Periodic
            | ProbeReason::Stability
            | ProbeReason::Mutation
            | ProbeReason::ObservedChange
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use skills::ops::sync::Settings;

    const STABILITY_WINDOW: Duration = Duration::from_secs(120);

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
            worktree: Some(WorktreeSnapshot {
                head: Some(local.into()),
                tree: format!("tree-{}", changes.join("|")),
                has_local_changes: !changes.is_empty(),
            }),
        }
    }

    fn probe_at(sync: &mut SyncCoordinator, reason: ProbeReason, status: Status, now: Instant) {
        let id = sync.request_probe(true, reason).unwrap().id;
        assert!(sync.finish_probe_at(id, Ok(status), now).accepted);
    }

    fn probe(sync: &mut SyncCoordinator, reason: ProbeReason, status: Status) {
        let now = Instant::now();
        probe_at(sync, reason, status.clone(), now);
        if !status.changes.is_empty() {
            probe_at(sync, ProbeReason::Stability, status, now + STABILITY_WINDOW);
        }
    }

    #[test]
    fn configurable_quiet_window_reloads_without_reusing_ready_authorization() {
        let start = Instant::now();
        let dirty = status(true, &[" M skill"], "base", "base");
        let mut sync = SyncCoordinator::default();
        sync.set_quiet_window(Duration::from_secs(5));
        probe_at(&mut sync, ProbeReason::Startup, dirty.clone(), start);
        probe_at(
            &mut sync,
            ProbeReason::Stability,
            dirty.clone(),
            start + Duration::from_secs(5),
        );
        assert!(matches!(sync.auto, AutoState::Ready));

        sync.set_quiet_window(Duration::from_secs(20));
        assert!(sync.take_auto_sync(true).is_none());
        probe_at(
            &mut sync,
            ProbeReason::Stability,
            dirty.clone(),
            start + Duration::from_secs(10),
        );
        assert!(sync.take_auto_sync(true).is_none());
        assert_eq!(sync.stability.as_ref().unwrap().stable_since, start);

        sync.set_quiet_window(Duration::from_secs(2));
        assert!(sync.take_auto_sync(true).is_none());
        probe_at(
            &mut sync,
            ProbeReason::Stability,
            dirty,
            start + Duration::from_secs(11),
        );
        assert!(sync.take_auto_sync(true).is_some());
    }

    #[test]
    fn zero_quiet_window_accepts_first_successful_probe_but_keeps_safety_gate() {
        let mut sync = SyncCoordinator::default();
        sync.set_quiet_window(Duration::ZERO);
        probe_at(
            &mut sync,
            ProbeReason::Mutation,
            status(true, &[" M skill"], "base", "base"),
            Instant::now(),
        );
        assert!(sync.take_auto_sync(false).is_none());
        assert!(sync.take_auto_sync(true).is_some());
    }

    #[test]
    fn local_changes_require_two_minutes_of_identical_semantic_snapshots() {
        let start = Instant::now();
        let dirty = status(true, &[" M skill"], "base", "base");
        let mut sync = SyncCoordinator::default();

        probe_at(&mut sync, ProbeReason::Startup, dirty.clone(), start);
        assert!(sync.pending());
        assert!(sync.take_auto_sync(true).is_none());

        probe_at(
            &mut sync,
            ProbeReason::Stability,
            dirty.clone(),
            start + STABILITY_WINDOW - Duration::from_millis(1),
        );
        assert!(sync.take_auto_sync(true).is_none());

        probe_at(
            &mut sync,
            ProbeReason::Stability,
            dirty,
            start + STABILITY_WINDOW,
        );
        assert!(sync.take_auto_sync(true).is_some());
    }

    #[test]
    fn changed_tree_resets_stability_even_when_status_lines_match() {
        let start = Instant::now();
        let first = status(true, &[" M skill"], "base", "base");
        let mut second = first.clone();
        second.worktree.as_mut().unwrap().tree = "different-content".into();
        let mut sync = SyncCoordinator::default();

        probe_at(&mut sync, ProbeReason::Startup, first, start);
        probe_at(
            &mut sync,
            ProbeReason::Stability,
            second.clone(),
            start + STABILITY_WINDOW,
        );
        assert!(sync.take_auto_sync(true).is_none());
        probe_at(
            &mut sync,
            ProbeReason::Stability,
            second,
            start + STABILITY_WINDOW + STABILITY_WINDOW,
        );
        assert!(sync.take_auto_sync(true).is_some());
    }

    #[test]
    fn stability_probe_cadence_requires_a_successful_new_sample() {
        let start = Instant::now();
        let dirty = status(true, &["?? skill"], "base", "base");
        let mut sync = SyncCoordinator::default();
        probe_at(&mut sync, ProbeReason::Startup, dirty, start);

        assert!(!sync.stability_probe_due(start + Duration::from_secs(1), Duration::from_secs(2)));
        assert!(sync.stability_probe_due(start + Duration::from_secs(2), Duration::from_secs(2)));
        assert!(sync.take_auto_sync(true).is_none());
    }

    #[test]
    fn failed_stability_probe_observes_retry_backoff() {
        let start = Instant::now();
        let dirty = status(true, &[" M skill"], "base", "base");
        let mut sync = SyncCoordinator::default();
        probe_at(&mut sync, ProbeReason::Startup, dirty, start);

        let failed_at = start + Duration::from_secs(2);
        let id = sync
            .request_probe(false, ProbeReason::Stability)
            .unwrap()
            .id;
        sync.finish_probe_at(id, Err(anyhow::anyhow!("filesystem busy")), failed_at);

        assert!(
            !sync.stability_probe_due(failed_at + Duration::from_secs(1), Duration::from_secs(2))
        );
        assert!(
            sync.stability_probe_due(failed_at + Duration::from_secs(2), Duration::from_secs(2))
        );
        assert!(sync.take_auto_sync(true).is_none());
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
