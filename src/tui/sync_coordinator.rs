//! Root-backup scheduling. The coordinator owns policy; `App` owns execution.

use anyhow::Result;
use skills::ops::sync::Status;
use std::time::{Duration, Instant};

const CHECK_INTERVAL: Duration = Duration::from_secs(60);
const RETRY_DELAYS: [Duration; 4] = [
    Duration::from_secs(60),
    Duration::from_secs(2 * 60),
    Duration::from_secs(5 * 60),
    Duration::from_secs(15 * 60),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Checking,
    Waiting,
    Syncing,
    Publishing,
    Conflict,
}

pub struct CheckRequest {
    pub id: u64,
}

pub struct CheckOutcome {
    pub accepted: bool,
}

pub struct SyncCoordinator {
    pub status: Option<Status>,
    pub error: Option<String>,
    pub phase: Phase,
    request_id: u64,
    remote_checking: bool,
    next_check: Instant,
    failures: usize,
    app_dirty: bool,
    dirty_at_start: bool,
    external_dirty: bool,
    status_current: bool,
    baseline_known: bool,
    sync_requested: bool,
}

impl Default for SyncCoordinator {
    fn default() -> Self {
        Self {
            status: None,
            error: None,
            phase: Phase::Idle,
            request_id: 0,
            remote_checking: false,
            next_check: Instant::now(),
            failures: 0,
            app_dirty: false,
            dirty_at_start: false,
            external_dirty: false,
            status_current: false,
            baseline_known: false,
            sync_requested: false,
        }
    }
}

impl SyncCoordinator {
    pub fn checking(&self) -> bool {
        matches!(self.phase, Phase::Checking)
    }

    pub fn syncing(&self) -> bool {
        matches!(self.phase, Phase::Syncing | Phase::Publishing)
    }

    pub fn switching(&self) -> bool {
        matches!(self.phase, Phase::Syncing)
    }

    #[cfg(test)]
    pub fn pending(&self) -> bool {
        self.sync_requested
    }

    pub fn due(&self, now: Instant) -> bool {
        !self.remote_checking && now >= self.next_check
    }

    pub fn request_check(&mut self, remote: bool, manual: bool) -> Option<CheckRequest> {
        if self.syncing() || (self.checking() && remote == self.remote_checking) {
            return None;
        }
        self.request_id = self.request_id.wrapping_add(1);
        self.remote_checking = remote;
        self.phase = Phase::Checking;
        self.error = None;
        if remote {
            // Manual checks bypass the current backoff, but starting one must
            // still retire the due time or the next tick starts it again.
            let _ = manual;
            self.next_check = Instant::now() + CHECK_INTERVAL;
        }
        Some(CheckRequest {
            id: self.request_id,
        })
    }

    pub fn finish_check(&mut self, id: u64, result: Result<Status>) -> CheckOutcome {
        if id != self.request_id {
            return CheckOutcome { accepted: false };
        }
        let remote = self.remote_checking;
        self.remote_checking = false;
        match result {
            Ok(status) => {
                self.failures = 0;
                if remote {
                    self.next_check = Instant::now() + CHECK_INTERVAL;
                }
                if !status.changes.is_empty() && (!self.baseline_known || !self.app_dirty) {
                    self.external_dirty = true;
                }
                self.baseline_known = true;
                let configured = status.settings.url.is_some() && status.settings.branch.is_some();
                let automatic_work = status.settings.enabled
                    && configured
                    && !self.external_dirty
                    && (status.changes.is_empty() || self.app_dirty)
                    && (self.app_dirty || status.ahead > 0 || status.behind > 0);
                if status.settings.enabled {
                    self.sync_requested |= automatic_work;
                } else {
                    self.sync_requested = false;
                }
                self.status = Some(status);
                self.status_current = true;
                self.error = None;
                self.phase = if self.sync_requested {
                    Phase::Waiting
                } else {
                    Phase::Idle
                };
            }
            Err(error) => {
                self.failures = (self.failures + 1).min(RETRY_DELAYS.len());
                self.next_check = Instant::now() + RETRY_DELAYS[self.failures - 1];
                self.error = Some(format!("{error:#}"));
                self.phase = if self.sync_requested {
                    Phase::Waiting
                } else {
                    Phase::Idle
                };
            }
        }
        CheckOutcome { accepted: true }
    }

    pub fn library_changed(&mut self) {
        self.app_dirty = true;
        self.status_current = false;
        self.sync_requested = true;
        if !self.syncing() {
            self.phase = Phase::Waiting;
        }
    }

    pub fn external_changed(&mut self) {
        self.external_dirty = true;
        self.status_current = false;
    }

    pub fn can_start_sync(&self, safe: bool) -> bool {
        safe && self.sync_requested
            && !self.syncing()
            && self.status_current
            && self
                .status
                .as_ref()
                .is_some_and(|status| status.settings.enabled)
            && !self.external_dirty
    }

    pub fn expected_changes(&self) -> Vec<String> {
        self.status
            .as_ref()
            .map(|status| status.changes.clone())
            .unwrap_or_default()
    }

    pub fn start_sync(&mut self) {
        self.sync_requested = false;
        self.dirty_at_start = self.app_dirty;
        self.app_dirty = false;
        self.phase = Phase::Syncing;
    }

    pub fn start_publishing(&mut self) {
        if self.phase == Phase::Syncing {
            self.phase = Phase::Publishing;
        }
    }

    pub fn finish_sync(&mut self, success: bool, conflict: bool) {
        if success {
            self.dirty_at_start = false;
            self.external_dirty = false;
            self.phase = if self.sync_requested {
                Phase::Waiting
            } else {
                Phase::Idle
            };
        } else if conflict {
            self.app_dirty |= self.dirty_at_start;
            self.dirty_at_start = false;
            self.sync_requested = false;
            self.phase = Phase::Conflict;
        } else {
            self.app_dirty |= self.dirty_at_start;
            self.dirty_at_start = false;
            // A failed network or Git operation is not retried in a tight UI
            // loop. The next successful probe can schedule fresh work.
            self.sync_requested = false;
            self.phase = Phase::Idle;
        }
    }

    pub fn disable(&mut self) {
        self.sync_requested = false;
        if !self.syncing() {
            self.phase = Phase::Idle;
        }
    }

    #[cfg(test)]
    pub fn set_status(&mut self, status: Status) {
        self.status = Some(status);
        self.status_current = true;
        self.phase = Phase::Idle;
    }

    #[cfg(test)]
    pub fn set_error(&mut self, error: Option<String>) {
        self.error = error;
    }

    #[cfg(test)]
    pub fn set_phase(&mut self, phase: Phase) {
        self.phase = phase;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use skills::ops::sync::Settings;

    fn status(enabled: bool, changes: usize, ahead: usize, behind: usize) -> Status {
        Status {
            settings: Settings {
                url: Some("remote".into()),
                branch: Some("main".into()),
                enabled,
            },
            changes: (0..changes).map(|n| format!("?? {n}")).collect(),
            ahead,
            behind,
            remote_checked: true,
        }
    }

    #[test]
    fn automatic_mode_reconciles_known_work_and_remote_updates() {
        let mut sync = SyncCoordinator::default();
        let id = sync.request_check(false, false).unwrap().id;
        sync.finish_check(id, Ok(status(true, 0, 0, 0)));
        sync.library_changed();
        let id = sync.request_check(true, false).unwrap().id;
        sync.finish_check(id, Ok(status(true, 1, 0, 1)));
        assert!(sync.pending());
        assert!(sync.can_start_sync(true));
        sync.start_sync();
        assert!(sync.syncing());
        sync.finish_sync(true, false);
        assert_eq!(sync.phase, Phase::Idle);
    }

    #[test]
    fn disabled_or_unknown_local_changes_never_auto_sync() {
        let mut sync = SyncCoordinator::default();
        let id = sync.request_check(true, false).unwrap().id;
        sync.finish_check(id, Ok(status(false, 0, 0, 1)));
        assert!(!sync.pending());

        let id = sync.request_check(true, true).unwrap().id;
        sync.finish_check(id, Ok(status(true, 1, 0, 1)));
        assert!(!sync.pending());
        sync.external_changed();
        sync.library_changed();
        assert!(!sync.can_start_sync(true));
    }

    #[test]
    fn changes_present_before_the_first_baseline_are_never_claimed_by_the_app() {
        let mut sync = SyncCoordinator::default();
        sync.library_changed();
        let id = sync.request_check(false, false).unwrap().id;
        sync.finish_check(id, Ok(status(true, 2, 0, 0)));
        assert!(!sync.can_start_sync(true));
    }

    #[test]
    fn stale_results_and_conflicts_do_not_loop() {
        let mut sync = SyncCoordinator::default();
        let old = sync.request_check(false, false).unwrap().id;
        let current = sync.request_check(true, true).unwrap().id;
        assert!(!sync.finish_check(old, Ok(status(true, 0, 0, 1))).accepted);
        sync.finish_check(current, Ok(status(true, 0, 0, 1)));
        assert!(sync.pending());
        sync.start_sync();
        sync.finish_sync(false, true);
        assert_eq!(sync.phase, Phase::Conflict);
        assert!(!sync.can_start_sync(true));
    }
}
