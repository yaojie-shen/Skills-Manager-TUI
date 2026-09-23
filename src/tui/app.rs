//! Application state: owns the snapshot, dispatches messages to the active
//! view or modal, and applies the `Action`s they return.

use super::event::{Msg, Task, TaskOutput, spawn_sync_status, spawn_task};
use super::keymap;
use super::modal::Modal;
use super::settings::{LayoutScope, RuntimeSettings, SessionSettings};
use super::sync_coordinator::{ProbeReason, SyncCoordinator};
use super::theme::Theme;
use super::toast::Toasts;
use super::views::{
    View, agents::AgentsView, health::HealthView, presets::PresetsView, repos::ReposView,
    search::SearchView, tags::TagsView,
};
use super::widgets::{SPINNER, fit, width};
use crate::tui::components::command_palette::{
    Command as AppCommand, CommandPalette, Event as PaletteEvent,
};
use crate::tui::components::context_menu::{ContextMenu, MenuEvent};
use anyhow::Result;
#[cfg(test)]
use crossterm::event::KeyModifiers;
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use skills::Workspace;
#[cfg(test)]
use skills::config::Config;
use skills::history::{self, History, Plan};
use skills::ops::{MutationScope, deploy};
use skills::reconcile::Snapshot;
use std::collections::VecDeque;
use std::sync::mpsc::Sender;
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Search,
    Tags,
    Presets,
    Agents,
    Repos,
    Health,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppFocus {
    Tabs,
    Page,
}

impl Tab {
    pub const ALL: [Tab; 6] = [
        Tab::Search,
        Tab::Tags,
        Tab::Presets,
        Tab::Agents,
        Tab::Repos,
        Tab::Health,
    ];
    pub fn visible(tags_enabled: bool) -> Vec<Tab> {
        Self::ALL
            .into_iter()
            .filter(|t| tags_enabled || *t != Tab::Tags)
            .collect()
    }
    pub fn title(self) -> &'static str {
        match self {
            Tab::Search => "Library",
            Tab::Tags => "Tags",
            Tab::Presets => "Presets",
            Tab::Agents => "Agents",
            Tab::Health => "Health",
            Tab::Repos => "Repos",
        }
    }
}

/// A write to perform on the workspace; returns the toast text.
pub type WriteFn = Box<dyn FnOnce(&Workspace) -> Result<String> + Send>;

/// A metadata write: the toast text, plus what it changed so the log can take
/// it back. The values are read inside the write, since only there is the state
/// it replaced still on disk.
pub type MetaFn = Box<dyn FnOnce(&Workspace) -> Result<(String, Option<history::Intent>)> + Send>;

/// Requests a view or modal hands back to the app.
pub enum Action {
    SetLayout {
        scope: LayoutScope,
        layout: skills::config::UiLayout,
    },
    SelectAgentSkills {
        keys: Vec<String>,
        title: String,
        checked: Option<String>,
        agent: String,
    },
    BackToParent,
    Quit,
    SelectSkills {
        keys: Vec<String>,
        title: String,
        checked: Option<String>,
    },
    Toast(String),
    Error(String),
    /// Re-scan in the background.
    Rescan,
    /// Refresh cached root backup status without blocking interaction.
    RefreshSyncStatus {
        remote: bool,
        reason: ProbeReason,
    },
    /// Re-scan after a successful Library mutation and schedule root sync.
    LibraryChanged,
    /// Mark a nested mutation as Agent-only; it must not schedule root sync.
    Deployment(Box<Action>),
    Spawn(Task),
    OpenModal(Box<Modal>),
    CloseModal,
    /// Keep the input and cursor until validation and synchronous writes succeed.
    SubmitInput(Vec<Action>),
    SwitchTab(Tab),
    /// Land on a preset by name once the list next reloads: after creating
    /// or renaming one, the card to look at is the one that has just changed.
    SelectPreset(String),
    SelectTag(String),
    /// Jump to the search tab with this query; `focus_list` selects the list pane.
    Search {
        query: String,
        focus_list: bool,
    },
    /// Open the note of a skill in $EDITOR.
    EditNote(String),
    /// Apply planned link actions immediately (used for small toggles).
    ApplyLinks {
        title: String,
        actions: Vec<deploy::Action>,
    },
    /// Ask before applying.
    ConfirmLinks {
        title: String,
        actions: Vec<deploy::Action>,
    },
    /// Run a write, toast its result, rescan.
    Write(WriteFn),
    /// Run a write that can be taken back, and log what it changed.
    WriteMeta(MetaFn),
    BatchMeta(MetaFn, Vec<String>),
    BackgroundWrite {
        title: String,
        write: MetaFn,
        keys: Vec<String>,
    },
    BatchLinks {
        title: String,
        actions: Vec<deploy::Action>,
        keys: Vec<String>,
    },
    /// Log something that has already happened.
    Record(history::Intent),
    /// Move the history after a confirmed undo or redo went through.
    Step(Step),
}

impl Action {
    pub fn deployment(action: Action) -> Self {
        Self::Deployment(Box::new(action))
    }

    #[cfg(test)]
    pub fn into_scoped(self) -> (MutationScope, Action) {
        match self {
            Self::Deployment(action) => (MutationScope::Deployment, *action),
            action => (MutationScope::Library, action),
        }
    }
}

/// Which way the history moves once a confirmed change has been applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Undo,
    Redo,
}

/// Work that must run outside the alternate screen.
#[derive(Debug, Clone)]
pub enum External {
    EditNote { skill: String, initial: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Ok,
    Error,
}

fn auto_sync_failure_action(
    disposition: skills::ops::sync::AutoSyncDisposition,
    conflict: bool,
    detail: &str,
) -> Action {
    match disposition {
        skills::ops::sync::AutoSyncDisposition::Transient => Action::Error(
            "Root sync will retry after the next status check; local changes retained".into(),
        ),
        skills::ops::sync::AutoSyncDisposition::Fatal if conflict => Action::Error(
            "Root sync conflict; local backup retained. Resolve with Git, then retry".into(),
        ),
        skills::ops::sync::AutoSyncDisposition::Fatal => {
            Action::Error(format!("Root sync stopped: {detail}"))
        }
        skills::ops::sync::AutoSyncDisposition::WorkingTreeChanged => {
            Action::Toast("Library changed externally; automatic sync paused".into())
        }
    }
}

/// Read-only context handed to views while drawing and handling input.
pub struct Ctx<'a> {
    pub ws: &'a Workspace,
    pub snap: &'a Snapshot,
    pub settings: &'a RuntimeSettings,
}

pub struct App {
    pub ws: Workspace,
    pub snap: Snapshot,
    pub settings: RuntimeSettings,
    session_settings: SessionSettings,
    pub tab: Tab,
    pub search: SearchView,
    pub tags: TagsView,
    pub presets: PresetsView,
    pub agents: AgentsView,
    agents_dirty: bool,
    pub health: HealthView,
    pub repos: ReposView,
    pub modal: Option<Modal>,
    context_menu: Option<ContextMenu>,
    command_palette: Option<CommandPalette>,
    pending_task_ui: VecDeque<Action>,
    batch_running: bool,
    batch_modal_owned: bool,
    pub toasts: Toasts,
    pub history: History,
    tasks_running: usize,
    sync: SyncCoordinator,
    next_task_id: u64,
    spinner: usize,
    last_root_poll: std::time::Instant,
    root_stamp: Option<skills::reconcile::watch::Stamp>,
    tx: Sender<Msg>,
    external: Option<External>,
    focus: AppFocus,
    quit: bool,
    quit_prompt: Option<QuitPrompt>,
    tab_rects: Vec<(Rect, Tab)>,
    sync_button: Rect,
    body: Rect,
}

/// Kept separate from the editing modal so cancelling quit preserves its input.
#[derive(Default)]
struct QuitPrompt {
    force_exit_selected: bool,
    sync_in_progress: bool,
    area: Rect,
    force_exit_button: Rect,
    cancel_button: Rect,
}

impl QuitPrompt {
    fn for_sync() -> Self {
        Self {
            sync_in_progress: true,
            ..Self::default()
        }
    }

    fn draw(&mut self, f: &mut Frame, outer: Rect, th: &Theme, tasks: Vec<String>) {
        use super::widgets::{OverlayClear, button, fit};
        let w = outer.width.saturating_sub(2).min(86);
        let h = if self.sync_in_progress {
            outer.height.saturating_sub(2).clamp(1, 6)
        } else {
            outer
                .height
                .saturating_sub(2)
                .min(tasks.len() as u16 + 6)
                .max(1)
        };
        self.area = Rect::new(
            outer.x + (outer.width - w) / 2,
            outer.y + (outer.height - h) / 2,
            w,
            h,
        );
        f.render_widget(OverlayClear, self.area);
        let block = th.block(" quit ", true);
        let inner = block.inner(self.area);
        f.render_widget(block, self.area);
        let lines = if self.sync_in_progress {
            vec![Line::raw(fit(
                "Root sync is still in progress.",
                inner.width as usize,
            ))]
        } else {
            let available = inner.height.saturating_sub(3) as usize;
            let total = tasks.len();
            let mut lines: Vec<Line> = tasks
                .into_iter()
                .take(available)
                .map(|task| Line::raw(fit(&task, inner.width as usize)))
                .collect();
            if total == 0 {
                lines.push(Line::raw("Background work has finished. Quit now?"));
            } else {
                if total > available
                    && let Some(last) = lines.last_mut()
                {
                    *last = Line::raw(format!("… {} more tasks running", total - available + 1));
                }
                lines.push(Line::raw(fit(
                    "Quit now and abandon running work?",
                    inner.width as usize,
                )));
            }
            lines
        };
        f.render_widget(Paragraph::new(lines), inner);
        let y = inner.bottom().saturating_sub(1);
        self.cancel_button = Rect::new(
            inner.right().saturating_sub(12).max(inner.x),
            y,
            inner.width.min(12),
            u16::from(inner.height > 0),
        );
        self.force_exit_button = Rect::new(
            self.cancel_button.x.saturating_sub(12).max(inner.x),
            y,
            self.cancel_button.x.saturating_sub(inner.x).min(12),
            u16::from(inner.height > 0),
        );
        f.render_widget(
            Paragraph::new(Line::from(button(
                if self.sync_in_progress {
                    "Force exit"
                } else {
                    "Quit"
                },
                self.force_exit_selected,
                th,
            ))),
            self.force_exit_button,
        );
        f.render_widget(
            Paragraph::new(Line::from(button("Cancel", !self.force_exit_selected, th))),
            self.cancel_button,
        );
    }
}

impl App {
    #[cfg(test)]
    pub(super) fn benchmark_apply(&mut self, action: Action) {
        self.apply(action);
    }

    #[cfg(test)]
    pub(super) fn benchmark_drain(&mut self, rx: &std::sync::mpsc::Receiver<Msg>) {
        while self.tasks_running > 0 {
            self.handle(rx.recv_timeout(std::time::Duration::from_secs(60)).unwrap());
        }
    }

    #[cfg(test)]
    pub fn new(ws: Workspace, tx: Sender<Msg>) -> Result<Self> {
        Self::new_with_launch_directory(ws, tx, None)
    }

    pub fn new_with_launch_directory(
        mut ws: Workspace,
        tx: Sender<Msg>,
        start: Option<&std::path::Path>,
    ) -> Result<Self> {
        if let Some(start) = start {
            ws.inventory_project = Some(start.to_path_buf());
        }
        Self::discover_local_agents(&mut ws)?;
        let local_project = ws
            .inventory_project
            .clone()
            .or_else(|| ws.project.clone())
            .unwrap_or(std::env::current_dir()?);
        let mut agents = AgentsView::default();
        agents.discover(&local_project)?;
        let snap = ws.scan()?;
        let root_stamp = skills::reconcile::watch::stamp(&ws.root, &ws.config).ok();
        let settings = RuntimeSettings::new(&ws.config);
        let mut app = Self {
            ws,
            snap,
            toasts: Toasts::new(settings.interaction),
            settings,
            session_settings: SessionSettings::default(),
            tab: Tab::Search,
            search: SearchView::default(),
            tags: TagsView::default(),
            presets: PresetsView::default(),
            agents,
            agents_dirty: true,
            health: HealthView::default(),
            repos: ReposView::default(),
            modal: None,
            context_menu: None,
            command_palette: None,
            pending_task_ui: VecDeque::new(),
            batch_running: false,
            batch_modal_owned: false,
            history: History::default(),
            tasks_running: 0,
            sync: SyncCoordinator::default(),
            next_task_id: 0,
            spinner: 0,
            last_root_poll: std::time::Instant::now(),
            root_stamp,
            tx,
            external: None,
            focus: AppFocus::Page,
            quit: false,
            quit_prompt: None,
            tab_rects: Vec::new(),
            sync_button: Rect::default(),
            body: Rect::default(),
        };
        app.on_snapshot();
        app.refresh_sync_status(true, ProbeReason::Startup);
        if let Some(report) = &app.ws.preset_migration {
            app.toast(
                format!(
                    "Converted {} presets to fixed members · backup: {}",
                    report.migrated_names.len(),
                    report.backup_dir.display()
                ),
                Level::Info,
            );
        }
        Ok(app)
    }

    fn discover_local_agents(ws: &mut Workspace) -> Result<()> {
        let project = ws
            .inventory_project
            .clone()
            .or_else(|| ws.project.clone())
            .unwrap_or(std::env::current_dir()?);
        skills::ops::targets::discover(ws, &project)
    }

    pub fn should_quit(&self) -> bool {
        self.quit
    }

    fn on_snapshot(&mut self) {
        self.context_menu = None;
        self.command_palette = None;
        self.settings
            .reload(&self.ws.config, &self.session_settings);
        if !self.settings.tags_enabled && self.tab == Tab::Tags {
            self.tab = Tab::Search;
        }
        let ctx = Ctx {
            ws: &self.ws,
            snap: &self.snap,
            settings: &self.settings,
        };
        self.search.refresh(&ctx);
        self.tags.refresh(&ctx);
        self.presets.refresh(&ctx);
        self.agents_dirty = true;
        if self.tab == Tab::Agents {
            self.agents.refresh(&ctx);
            self.agents_dirty = false;
        }
        self.health.refresh(&ctx);
        self.repos.refresh(&ctx);
        if let Some(m) = self.modal.as_mut() {
            m.refresh(&ctx);
        }
    }

    pub fn toast(&mut self, text: impl Into<String>, level: Level) {
        self.toasts.push(text, level);
    }

    // ---- external ---------------------------------------------------------

    pub fn take_external(&mut self) -> Option<External> {
        self.external.take()
    }

    pub fn run_external(&mut self, req: External) -> Result<(String, Option<String>)> {
        match req {
            External::EditNote { skill, initial } => {
                let text = crate::cli::edit_in_editor(&initial)?;
                Ok((skill, text))
            }
        }
    }

    pub fn finish_external(&mut self, outcome: Result<(String, Option<String>)>) {
        match outcome {
            Ok((skill, None)) => {
                self.toast(format!("note unchanged on {skill}"), Level::Info);
                return;
            }
            Ok((skill, Some(text))) => match self.run_meta(
                MutationScope::Library,
                Box::new(move |ws| history::note_edit(ws, &skill, Some(&text))),
            ) {
                Ok((msg, intent)) => {
                    self.toast(msg, Level::Ok);
                    if let Some(intent) = intent {
                        self.history.record(intent);
                    }
                    self.library_changed();
                    return;
                }
                Err(e) => self.toast(format!("note failed: {e:#}"), Level::Error),
            },
            Err(e) => self.toast(format!("editor: {e:#}"), Level::Error),
        }
        self.rescan();
    }

    // ---- messages ---------------------------------------------------------

    pub fn handle(&mut self, msg: Msg) {
        let background = matches!(&msg, Msg::Task(..));
        let actions = match msg {
            Msg::Tick => {
                self.spinner = (self.spinner + 1) % SPINNER.len();
                self.toasts.expire();
                if self.sync.probe_due(Instant::now()) {
                    self.refresh_sync_status(true, ProbeReason::Periodic);
                }
                if self.tasks_running == 0
                    && !self.task_ui_blocked()
                    && self.last_root_poll.elapsed() >= self.settings.interaction.root_poll_interval
                {
                    self.last_root_poll = std::time::Instant::now();
                    self.spawn(Task::PollRoot);
                }
                Vec::new()
            }
            Msg::Resize => {
                self.context_menu = None;
                self.command_palette = None;
                Vec::new()
            }
            Msg::SyncPublishing => {
                self.sync.start_publishing();
                Vec::new()
            }
            Msg::Progress(id, detail) => {
                self.toasts.progress(id, detail);
                Vec::new()
            }
            Msg::SyncStatus(id, result) => {
                let displayed = result.as_ref().ok().cloned();
                let error = result.as_ref().err().map(|error| format!("{error:#}"));
                let outcome = self.sync.finish_probe(id, result);
                if outcome.accepted {
                    if let Some(status) = displayed {
                        if let Some(Modal::Sync(picker)) = self.modal.as_mut() {
                            picker.set_status(status.clone(), None);
                        }
                    } else if let Some(message) = error
                        && let Some(Modal::Sync(picker)) = self.modal.as_mut()
                    {
                        picker.set_checking(false);
                        picker.set_error(message.clone());
                    }
                    if outcome.needs_validation {
                        self.refresh_sync_status(false, ProbeReason::Mutation);
                    }
                }
                Vec::new()
            }
            Msg::Task(id, out) => {
                self.toasts.finish(id);
                self.tasks_running = self.tasks_running.saturating_sub(1);
                self.on_task(*out)
            }
            Msg::Paste(text) => self.on_paste(&text),
            Msg::Key(k) => self.on_key(k),
            Msg::Mouse(m) => self.on_mouse(m),
        };
        let mut queued = false;
        for action in actions {
            if background
                && matches!(action, Action::OpenModal(_) | Action::Search { .. })
                && (self.task_ui_blocked() || !self.pending_task_ui.is_empty())
            {
                self.pending_task_ui.push_back(action);
                queued = true;
            } else {
                self.apply(action);
            }
        }
        if queued {
            self.toast("task ready — waiting for the current dialog", Level::Info);
        }
        // Drain only after the whole event, so CloseModal + OpenModal transitions
        // and failed input submissions cannot expose a queued dialog in between.
        while !self.quit && !self.task_ui_blocked() {
            let Some(action) = self.pending_task_ui.pop_front() else {
                break;
            };
            self.apply(action);
        }
        if !self.quit {
            self.sync_if_ready();
        }
    }

    pub fn sync_if_ready(&mut self) {
        let safe = self.tasks_running == 0
            && !self.batch_running
            && !self.library_edit_active()
            && self.external.is_none();
        if let Some(request) = self.sync.take_auto_sync(safe) {
            self.spawn(Task::AutoSync(request.expected_changes));
        }
    }

    fn refresh_sync_status(&mut self, remote: bool, reason: ProbeReason) {
        let Some(request) = self.sync.request_probe(remote, reason) else {
            return;
        };
        if let Some(Modal::Sync(picker)) = self.modal.as_mut() {
            picker.set_checking(true);
        }
        spawn_sync_status(self.ws.clone(), remote, request.id, self.tx.clone());
    }

    fn open_sync(&mut self, check_remote: bool) -> Vec<Action> {
        let ctx = Ctx {
            ws: &self.ws,
            snap: &self.snap,
            settings: &self.settings,
        };
        match super::sync_picker::SyncPicker::with_status(
            &ctx,
            self.sync.status.clone(),
            self.sync.probing() || check_remote,
            self.sync.error.clone(),
        ) {
            Ok(picker) => vec![
                Action::OpenModal(Box::new(Modal::Sync(Box::new(picker)))),
                Action::RefreshSyncStatus {
                    remote: check_remote,
                    reason: ProbeReason::Manual,
                },
            ],
            Err(error) => vec![Action::Error(format!("{error:#}"))],
        }
    }

    fn task_ui_blocked(&self) -> bool {
        self.quit_prompt.is_some()
            || self.modal.is_some()
            || self.context_menu.is_some()
            || self.command_palette.is_some()
            || (self.tab == Tab::Tags && self.tags.input_focused())
    }

    fn library_edit_active(&self) -> bool {
        self.modal.as_ref().is_some_and(Modal::library_edit_active)
            || (self.tab == Tab::Tags && self.tags.input_focused())
    }

    fn on_task(&mut self, out: TaskOutput) -> Vec<Action> {
        match out {
            TaskOutput::RepairPlan(options, result) => match result {
                Ok(plan) => vec![Action::OpenModal(Box::new(Modal::HealthRepair(Box::new(
                    super::views::health::RepairDialog::preview(options, plan),
                ))))],
                Err(e) => vec![Action::Error(format!("Repair preview: {e:#}"))],
            },
            TaskOutput::RepairApplied(result) => {
                self.batch_running = false;
                match result {
                    Ok(report) => vec![
                        Action::Rescan,
                        Action::OpenModal(Box::new(Modal::message(
                            "Repair results",
                            report.lines(),
                        ))),
                    ],
                    Err(e) => vec![
                        Action::Rescan,
                        Action::Error(format!(
                            "Repair: {e:#}; inspect status; earlier actions may have completed"
                        )),
                    ],
                }
            }
            TaskOutput::SyncConfigured(result) => {
                self.batch_running = false;
                match result {
                    Ok(()) => vec![
                        Action::Rescan,
                        Action::RefreshSyncStatus {
                            remote: false,
                            reason: ProbeReason::Configuration,
                        },
                        Action::Toast("Root auto-sync enabled".into()),
                    ],
                    Err(e) => vec![Action::Error(format!("Root sync setup: {e:#}"))],
                }
            }
            TaskOutput::SyncDisabled(result) => {
                self.batch_running = false;
                match result {
                    Ok(()) => {
                        self.sync.disable();
                        vec![
                            Action::RefreshSyncStatus {
                                remote: false,
                                reason: ProbeReason::Configuration,
                            },
                            Action::Toast(
                                "Automatic root sync disabled; Git history retained".into(),
                            ),
                        ]
                    }
                    Err(e) => vec![Action::Error(format!("Disable root sync: {e:#}"))],
                }
            }
            TaskOutput::Sync(request, result) => {
                self.batch_running = false;
                if self
                    .quit_prompt
                    .as_ref()
                    .is_some_and(|prompt| prompt.sync_in_progress)
                {
                    self.quit_prompt = None;
                    self.quit = true;
                }
                match result {
                    Ok(report) if request.dry_run => {
                        let ctx = Ctx {
                            ws: &self.ws,
                            snap: &self.snap,
                            settings: &self.settings,
                        };
                        match super::sync_picker::SyncPicker::preview(&ctx, request, report) {
                            Ok(p) => vec![Action::OpenModal(Box::new(Modal::Sync(Box::new(p))))],
                            Err(e) => vec![Action::Error(format!("{e:#}"))],
                        }
                    }
                    result => {
                        self.rescan();
                        match result {
                            Ok(report) => {
                                self.sync.finish_manual_sync(true);
                                if report.pulled {
                                    self.history = History::default();
                                }
                                vec![
                                    Action::RefreshSyncStatus {
                                        remote: true,
                                        reason: ProbeReason::AfterRun,
                                    },
                                    Action::Toast(format!(
                                        "Root synced: backup {}, pull {}, push {}",
                                        report.committed, report.pulled, report.pushed
                                    )),
                                ]
                            }
                            Err(e) => {
                                self.sync.finish_manual_sync(false);
                                vec![
                                    Action::RefreshSyncStatus {
                                        remote: true,
                                        reason: ProbeReason::AfterRun,
                                    },
                                    Action::Error(format!(
                                        "Root sync pending: {e:#}; local changes retained"
                                    )),
                                ]
                            }
                        }
                    }
                }
            }
            TaskOutput::AutoSync(result) => {
                self.batch_running = false;
                if self
                    .quit_prompt
                    .as_ref()
                    .is_some_and(|prompt| prompt.sync_in_progress)
                {
                    self.quit_prompt = None;
                    self.quit = true;
                }
                self.rescan();
                match result {
                    Ok(report) => {
                        self.sync.finish_auto_sync(Ok(()));
                        if report.pulled {
                            self.history = History::default();
                        }
                        vec![
                            Action::RefreshSyncStatus {
                                remote: true,
                                reason: ProbeReason::AfterRun,
                            },
                            Action::Toast(format!(
                                "Root synced: backup {}, pull {}, push {}",
                                report.committed, report.pulled, report.pushed
                            )),
                        ]
                    }
                    Err(error) => {
                        let disposition = error.disposition;
                        self.sync.finish_auto_sync(Err(disposition));
                        let message = auto_sync_failure_action(
                            disposition,
                            error.is_conflict(),
                            &error.to_string(),
                        );
                        vec![
                            Action::RefreshSyncStatus {
                                remote: true,
                                reason: ProbeReason::AfterRun,
                            },
                            message,
                        ]
                    }
                }
            }
            TaskOutput::Batch(outcome) => {
                self.batch_running = false;
                let changed_scope = outcome.changed_scope;
                let return_to = if self.batch_modal_owned
                    && matches!(self.modal, Some(Modal::Batch(_) | Modal::PresetSkills(_)))
                {
                    if let Some(Modal::Batch(batch)) = self.modal.as_mut() {
                        batch.set_busy(false);
                    }
                    self.modal.take().map(Box::new)
                } else {
                    None
                };
                self.batch_modal_owned = false;
                if let Some(pending) = outcome.conflict {
                    return vec![Action::OpenModal(Box::new(Modal::DeploymentChoices(
                        Box::new(super::name_choices::NameChoices::new(pending)),
                    )))];
                }
                let undo_hint = if outcome.intent.is_some() {
                    " · Ctrl+Z undo"
                } else {
                    ""
                };
                if let Some(intent) = outcome.intent {
                    self.history.record(intent);
                }
                self.search.batch_finished(&outcome.failed);
                self.tags.batch_finished(&outcome.failed);
                self.presets.batch_finished(&outcome.failed);
                self.repos.batch_finished(&outcome.failed);
                if outcome.errors.is_empty() {
                    self.toast(format!("{}{undo_hint}", outcome.message), Level::Ok);
                } else {
                    self.toast(
                        format!("{}; errors: {}", outcome.message, outcome.errors.len()),
                        Level::Error,
                    );
                }
                if let Some(scope) = changed_scope {
                    self.mutation_finished(scope);
                } else {
                    self.rescan();
                }
                if outcome.errors.is_empty() {
                    vec![]
                } else {
                    vec![Action::OpenModal(Box::new(Modal::Message {
                        title: "Operation needs attention".into(),
                        lines: outcome.errors,
                        scroll: 0,
                        return_to,
                    }))]
                }
            }

            TaskOutput::RepositoryFetched(_, Ok(fetched)) => {
                let ctx = Ctx {
                    ws: &self.ws,
                    snap: &self.snap,
                    settings: &self.settings,
                };
                vec![Action::OpenModal(Box::new(Modal::Repository(Box::new(
                    super::repository_picker::RepositoryPicker::new(fetched, &ctx),
                ))))]
            }
            TaskOutput::RepositoryFetched(reference, Err(e)) => {
                vec![Action::Error(format!("discover {reference}: {e:#}"))]
            }
            TaskOutput::RepositoryRefreshed(alias, result) => {
                if !self.repos.inventory_result(&alias, &result) {
                    return Vec::new();
                }
                match result {
                    Ok(inventory) => {
                        let available = inventory
                            .entries
                            .iter()
                            .filter(|entry| {
                                matches!(
                                    entry.state,
                                    skills::repository::RepositoryInventoryState::Available
                                        | skills::repository::RepositoryInventoryState::PossibleMove { .. }
                                )
                            })
                            .count();
                        let updates = inventory
                            .entries
                            .iter()
                            .filter(|entry| {
                                matches!(
                                    entry.state,
                                    skills::repository::RepositoryInventoryState::Update { .. }
                                )
                            })
                            .count();
                        vec![Action::Toast(format!(
                            "refreshed {alias}: {available} available · {updates} updates"
                        ))]
                    }
                    Err(error) => vec![Action::Error(format!("refresh {alias}: {error:#}"))],
                }
            }
            TaskOutput::RepositoryInstalled(selection, result) => match result {
                Ok(keys) => {
                    let aliases: Vec<_> = selection
                        .names
                        .iter()
                        .filter(|(path, name)| **name != selection.fetched.local_name(path))
                        .map(|(path, name)| format!("{path} → {name}"))
                        .collect();
                    selection.fetched.cleanup();
                    if keys.is_empty() {
                        return vec![Action::Toast(
                            "Already installed; skipped without changes".into(),
                        )];
                    }

                    let mut actions: Vec<Action> = keys
                        .iter()
                        .map(|key| Action::Record(history::Intent::Install { skill: key.clone() }))
                        .collect();
                    let message = format!(
                        "Installed {} skills{}.",
                        keys.len(),
                        if aliases.is_empty() {
                            String::new()
                        } else {
                            format!(
                                "; warning: folder aliases {} (declared names unchanged)",
                                aliases.join(", ")
                            )
                        }
                    );
                    actions.extend([
                        Action::LibraryChanged,
                        Action::Search {
                            query: skills::search::source_query_token(
                                &selection.fetched.repository.display_name(),
                            ),
                            focus_list: true,
                        },
                        Action::OpenModal(Box::new(Modal::install_complete(keys, message))),
                    ]);
                    actions
                }
                Err(e) => vec![
                    Action::Error(format!("install: {e:#}")),
                    Action::OpenModal(Box::new(Modal::Repository(Box::new(
                        super::repository_picker::RepositoryPicker::restore(*selection),
                    )))),
                ],
            },

            TaskOutput::RootStamp(Ok(stamp)) => {
                let changed = self.root_stamp.as_ref() != Some(&stamp);
                if !self.task_ui_blocked() && self.tasks_running == 0 {
                    self.root_stamp = Some(stamp);
                    if changed {
                        self.sync.external_changed();
                        self.refresh_sync_status(false, ProbeReason::ObservedChange);
                        return vec![Action::Rescan];
                    }
                }
                Vec::new()
            }
            // A move can temporarily remove a directory while it is being polled.
            // Keep the last good snapshot and retry; explicit rescans report errors.
            TaskOutput::RootStamp(Err(_)) => Vec::new(),
            TaskOutput::Scan(Ok(snap), stamp) => {
                self.root_stamp = stamp;
                self.snap = snap;
                self.on_snapshot();
                Vec::new()
            }
            TaskOutput::Scan(Err(e), _) => {
                self.root_stamp = None;
                vec![Action::Error(format!("scan failed: {e:#}"))]
            }
            TaskOutput::Check(results) => {
                self.search.remember_checks(&results);
                self.repos.remember_checks(&results);
                let ctx = Ctx {
                    ws: &self.ws,
                    snap: &self.snap,
                    settings: &self.settings,
                };
                if self.tab == Tab::Health || results.len() > 1 {
                    self.health.on_check(&results, &ctx)
                } else {
                    let mut acts = self.search.on_check(&results, &ctx);
                    acts.extend(
                        self.health
                            .on_check(&results, &ctx)
                            .into_iter()
                            .filter(|a| !matches!(a, Action::Toast(_))),
                    );
                    acts
                }
            }
            TaskOutput::Prepared(key, Ok(prepared)) => {
                if prepared.from_revision.as_deref() == Some(prepared.to_revision.as_str())
                    && !prepared.needs_resolution()
                {
                    prepared.cleanup();
                    return vec![Action::Toast(format!("{key} is up to date"))];
                }
                vec![Action::OpenModal(Box::new(Modal::resolve(prepared)))]
            }
            TaskOutput::Prepared(key, Err(e)) => {
                vec![Action::Error(format!("update {key}: {e:#}"))]
            }
            // Land on the new skill and let deployment remain an explicit next step.
            TaskOutput::Installed(_, Ok(key)) => vec![
                Action::Record(history::Intent::Install { skill: key.clone() }),
                Action::LibraryChanged,
                Action::Search {
                    query: key.clone(),
                    focus_list: true,
                },
                Action::OpenModal(Box::new(Modal::install_complete(
                    vec![key.clone()],
                    format!("Installed {key}."),
                ))),
            ],
            // Multi-skill sources use the same repository picker as direct discovery.
            TaskOutput::Installed(reference, Err(e)) => {
                match e.downcast_ref::<skills::ops::install::NotOneSkill>() {
                    Some(_) => match skills::ops::install::parse_ref(&reference, None, None) {
                        Ok(parsed) => vec![Action::Spawn(Task::DiscoverRepository {
                            label: reference,
                            reference: parsed,
                        })],
                        Err(error) => {
                            vec![Action::Error(format!("discover {reference}: {error:#}"))]
                        }
                    },
                    None => vec![Action::Error(format!("install {reference}: {e:#}"))],
                }
            }
        }
    }

    fn on_paste(&mut self, text: &str) -> Vec<Action> {
        if self.context_menu.is_some() {
            return vec![];
        }
        if let Some(palette) = self.command_palette.as_mut() {
            return palette
                .paste(text)
                .err()
                .map_or_else(Vec::new, |error| vec![Action::Error(error)]);
        }
        if self.focus == AppFocus::Tabs && self.modal.is_none() {
            return vec![];
        }

        if self.quit_prompt.is_some()
            || (self.batch_running
                && matches!(self.modal, Some(Modal::Batch(_) | Modal::PresetSkills(_))))
        {
            return vec![];
        }
        let ctx = Ctx {
            ws: &self.ws,
            snap: &self.snap,
            settings: &self.settings,
        };
        if let Some(modal) = self.modal.as_mut() {
            return modal.paste(text, &ctx);
        }
        match self.tab {
            Tab::Search => self.search.paste(text, &ctx),
            Tab::Tags => self.tags.paste(text, &ctx),
            Tab::Presets => self.presets.paste(text, &ctx),
            Tab::Health => self.health.paste(text, &ctx),
            Tab::Repos => self.repos.paste(text, &ctx),
            Tab::Agents => self.agents.paste(text, &ctx),
        }
    }

    fn on_key(&mut self, k: KeyEvent) -> Vec<Action> {
        if let Some(prompt) = self.quit_prompt.as_mut() {
            match k.code {
                KeyCode::Esc | KeyCode::Char('q') => self.quit_prompt = None,
                KeyCode::Left | KeyCode::Right => {
                    prompt.force_exit_selected = !prompt.force_exit_selected;
                }
                KeyCode::Enter => {
                    self.quit = prompt.force_exit_selected;
                    self.quit_prompt = None;
                }
                _ => {}
            }
            return vec![];
        }
        if keymap::control(k, 'c') {
            return vec![Action::Quit];
        }
        if let Some(menu) = self.context_menu.as_mut() {
            let event = menu.key(k);
            return self.context_event(event);
        }
        if let Some(palette) = self.command_palette.as_mut() {
            let event = palette.key(k);
            return self.palette_event(event);
        }
        if self.batch_running
            && matches!(self.modal, Some(Modal::Batch(_) | Modal::PresetSkills(_)))
        {
            return vec![];
        }
        let ctx = Ctx {
            ws: &self.ws,
            snap: &self.snap,
            settings: &self.settings,
        };
        if let Some(m) = self.modal.as_mut() {
            return m.handle_key(k, &ctx);
        }
        if self.focus == AppFocus::Page && self.active_view().overlay_open() {
            return match self.tab {
                Tab::Search => self.search.handle_key(k, &ctx),
                Tab::Tags => self.tags.handle_key(k, &ctx),
                Tab::Presets => self.presets.handle_key(k, &ctx),
                Tab::Agents => self.agents.handle_key(k, &ctx),
                Tab::Health => self.health.handle_key(k, &ctx),
                Tab::Repos => self.repos.handle_key(k, &ctx),
            };
        }
        match k.code {
            KeyCode::Char('r') if keymap::control(k, 'r') => {
                return vec![Action::Rescan, Action::Toast("rescanning".into())];
            }
            KeyCode::Char('o') if keymap::control(k, 'o') => {
                let enabled = !self.settings.tags_enabled;
                return vec![Action::OpenModal(Box::new(Modal::confirm_write(
                    "Settings · Tags".into(),
                    vec![format!("Tags: {} → {}", !enabled, enabled), "Hide or show tag classification throughout the interface. Existing data and preset membership are preserved.".into()],
                    Box::new(move |ws| { skills::config::Config::set_tags_enabled(&ws.root, enabled)?; Ok(format!("Tags {}", if enabled { "enabled" } else { "disabled" })) })
                )))];
            }
            KeyCode::Char('g') if keymap::control(k, 'g') => {
                return vec![Action::OpenModal(Box::new(Modal::help()))];
            }
            KeyCode::Char('z') if keymap::control(k, 'z') => return self.step(Step::Undo),
            KeyCode::Char('y') if keymap::control(k, 'y') => return self.step(Step::Redo),
            _ => {}
        }
        let in_search_input = self.text_input_focused();
        if in_search_input && !matches!(k.code, KeyCode::Tab | KeyCode::BackTab) {
            return match self.tab {
                Tab::Search => self.search.handle_key(k, &ctx),
                Tab::Tags => self.tags.handle_key(k, &ctx),
                Tab::Presets => self.presets.handle_key(k, &ctx),
                Tab::Agents => self.agents.handle_key(k, &ctx),
                Tab::Health => self.health.handle_key(k, &ctx),
                Tab::Repos => self.repos.handle_key(k, &ctx),
            };
        }
        if !keymap::plain(k) {
            if self.focus == AppFocus::Tabs {
                return vec![];
            }
            return match self.tab {
                Tab::Search => self.search.handle_control_key(k, &ctx),
                Tab::Tags => self.tags.handle_control_key(k, &ctx),
                Tab::Presets => self.presets.handle_control_key(k, &ctx),
                Tab::Agents => self.agents.handle_control_key(k, &ctx),
                Tab::Health => self.health.handle_control_key(k, &ctx),
                Tab::Repos => self.repos.handle_control_key(k, &ctx),
            };
        }
        if keymap::character(k, ':') {
            self.command_palette = Some(CommandPalette::default());
            return vec![];
        }
        let actions_menu = if self.focus == AppFocus::Page && keymap::character(k, 'a') {
            match self.tab {
                Tab::Search => self.search.actions_menu(&ctx),
                Tab::Tags => self.tags.actions_menu(&ctx),
                Tab::Presets => self.presets.actions_menu(&ctx),
                Tab::Agents => self.agents.actions_menu(&ctx),
                Tab::Health => self.health.actions_menu(&ctx),
                Tab::Repos => self.repos.actions_menu(&ctx),
            }
        } else {
            None
        };
        if let Some(mut request) = actions_menu {
            request.title = format!("Actions · {}", request.title);
            self.context_menu = Some(ContextMenu::keyboard(request));
            return vec![];
        }
        if keymap::character(k, 'a') {
            return vec![];
        }
        if self.focus == AppFocus::Tabs {
            let tabs = Tab::visible(self.settings.tags_enabled);
            let index = tabs.iter().position(|t| *t == self.tab).unwrap_or(0);
            match k.code {
                KeyCode::Esc | KeyCode::Char('q') => return vec![Action::Quit],
                KeyCode::Enter | KeyCode::Down => {
                    self.enter_page();
                    if k.code == KeyCode::Down {
                        match self.tab {
                            Tab::Search => self.search.focus_input(),
                            Tab::Tags => self.tags.focus_from_above(),
                            Tab::Presets => self.presets.focus_from_above(),
                            Tab::Repos => self.repos.focus_from_above(),
                            Tab::Health => self.health.focus_input(),
                            _ => {}
                        }
                    }
                    return vec![];
                }
                KeyCode::Left | KeyCode::BackTab => {
                    return vec![Action::SwitchTab(
                        tabs[(index + tabs.len() - 1) % tabs.len()],
                    )];
                }
                KeyCode::Right | KeyCode::Tab => {
                    return vec![Action::SwitchTab(tabs[(index + 1) % tabs.len()])];
                }
                KeyCode::Char(c @ '1'..='6') if keymap::plain(k) => {
                    return tabs
                        .get((c as u8 - b'1') as usize)
                        .map(|t| vec![Action::SwitchTab(*t)])
                        .unwrap_or_default();
                }
                KeyCode::Char('?') if keymap::plain(k) => {
                    return vec![Action::OpenModal(Box::new(Modal::help()))];
                }
                _ => return vec![],
            }
        }
        if self.tab == Tab::Agents && self.agents.group_popup_open() {
            return self.agents.handle_key(k, &ctx);
        }
        if matches!(k.code, KeyCode::Tab | KeyCode::BackTab) {
            if self.tab == Tab::Tags && self.tags.dialog_open() {
                return vec![];
            }
            let tabs = Tab::visible(self.settings.tags_enabled);
            let index = tabs.iter().position(|t| *t == self.tab).unwrap_or(0);
            let delta = if k.code == KeyCode::Tab {
                1
            } else {
                tabs.len() - 1
            };
            return vec![Action::SwitchTab(tabs[(index + delta) % tabs.len()])];
        }
        if self.tab == Tab::Agents
            && let Some(actions) = self.agents.handle_matrix_key(k, &ctx)
        {
            return actions;
        }
        // Any text field that holds the keyboard keeps its digits and slashes;
        // the Tags page has one of its own for colours and merge targets.
        if self.modal.is_none()
            && self.tab == Tab::Agents
            && (self.agents.editing() || k.code == KeyCode::Char('/'))
        {
            return self.agents.handle_key(
                k,
                &Ctx {
                    ws: &self.ws,
                    snap: &self.snap,
                    settings: &self.settings,
                },
            );
        }
        match k.code {
            KeyCode::Char('R') if keymap::plain(k) => {
                return vec![Action::OpenModal(Box::new(Modal::repositories(&ctx)))];
            }
            KeyCode::Char('?') if keymap::plain(k) => {
                return vec![Action::OpenModal(Box::new(Modal::help()))];
            }
            KeyCode::Char(c @ '1'..='6') if keymap::plain(k) => {
                return Tab::visible(self.settings.tags_enabled)
                    .get((c as u8 - b'1') as usize)
                    .map(|t| vec![Action::SwitchTab(*t)])
                    .unwrap_or_default();
            }
            _ => {}
        }
        match self.tab {
            Tab::Search => self.search.handle_key(k, &ctx),
            Tab::Tags => self.tags.handle_key(k, &ctx),
            Tab::Presets => self.presets.handle_key(k, &ctx),
            Tab::Agents => self.agents.handle_key(k, &ctx),
            Tab::Health => self.health.handle_key(k, &ctx),
            Tab::Repos => self.repos.handle_key(k, &ctx),
        }
    }

    fn palette_event(&mut self, event: PaletteEvent) -> Vec<Action> {
        match event {
            PaletteEvent::Stay => vec![],
            PaletteEvent::Close => {
                self.command_palette = None;
                vec![]
            }
            PaletteEvent::Execute(command) => {
                self.command_palette = None;
                let ctx = Ctx {
                    ws: &self.ws,
                    snap: &self.snap,
                    settings: &self.settings,
                };
                match command {
                    AppCommand::Rescan => vec![Action::Rescan, Action::Toast("rescanning".into())],
                    AppCommand::Install => vec![Action::OpenModal(Box::new(Modal::install()))],
                    AppCommand::Repositories => {
                        vec![Action::OpenModal(Box::new(Modal::repositories(&ctx)))]
                    }
                    AppCommand::Repair => vec![Action::OpenModal(Box::new(Modal::HealthRepair(
                        Box::default(),
                    )))],
                    AppCommand::RootSync => self.open_sync(true),
                    AppCommand::ToggleTags => {
                        let enabled = !self.settings.tags_enabled;
                        vec![Action::OpenModal(Box::new(Modal::confirm_write(
                            "Settings · Tags".into(),
                            vec![format!("Tags: {} → {}", !enabled, enabled), "Hide or show tag classification throughout the interface. Existing data and preset membership are preserved.".into()],
                            Box::new(move |ws| { skills::config::Config::set_tags_enabled(&ws.root, enabled)?; Ok(format!("Tags {}", if enabled { "enabled" } else { "disabled" })) })
                        )))]
                    }
                    AppCommand::Undo => self.step(Step::Undo),
                    AppCommand::Redo => self.step(Step::Redo),
                    AppCommand::Help => vec![Action::OpenModal(Box::new(Modal::help()))],
                }
            }
        }
    }

    fn text_input_focused(&self) -> bool {
        self.focus == AppFocus::Page
            && ((self.tab == Tab::Search && self.search.input_focused())
                || (self.tab == Tab::Tags && self.tags.input_focused())
                || (self.tab == Tab::Presets && self.presets.input_focused())
                || (self.tab == Tab::Health && self.health.input_focused())
                || (self.tab == Tab::Repos && self.repos.input_focused())
                || (self.tab == Tab::Agents && self.agents.editing()))
    }

    fn actions_available(&self) -> bool {
        if self.focus != AppFocus::Page || self.text_input_focused() {
            return false;
        }
        let ctx = Ctx {
            ws: &self.ws,
            snap: &self.snap,
            settings: &self.settings,
        };
        match self.tab {
            Tab::Search => self.search.actions_menu(&ctx),
            Tab::Tags => self.tags.actions_menu(&ctx),
            Tab::Presets => self.presets.actions_menu(&ctx),
            Tab::Agents => self.agents.actions_menu(&ctx),
            Tab::Health => self.health.actions_menu(&ctx),
            Tab::Repos => self.repos.actions_menu(&ctx),
        }
        .is_some()
    }

    fn active_view(&self) -> &dyn View {
        match self.tab {
            Tab::Search => &self.search,
            Tab::Tags => &self.tags,
            Tab::Presets => &self.presets,
            Tab::Agents => &self.agents,
            Tab::Health => &self.health,
            Tab::Repos => &self.repos,
        }
    }

    fn global_hints_visible(&self) -> bool {
        self.modal.is_none()
            && self.quit_prompt.is_none()
            && self.context_menu.is_none()
            && self.command_palette.is_none()
            && !self.text_input_focused()
            && (self.focus == AppFocus::Tabs || !self.active_view().overlay_open())
    }

    fn context_event(&mut self, event: MenuEvent) -> Vec<Action> {
        match event {
            MenuEvent::Stay => vec![],
            MenuEvent::Close => {
                self.context_menu = None;
                vec![]
            }
            MenuEvent::Execute(command) => {
                let Some(menu) = self.context_menu.take() else {
                    return vec![];
                };
                let ctx = Ctx {
                    ws: &self.ws,
                    snap: &self.snap,
                    settings: &self.settings,
                };
                let target = &menu.request.target;
                match self.tab {
                    Tab::Search => self.search.context_execute(target, command, &ctx),
                    Tab::Tags => self.tags.context_execute(target, command, &ctx),
                    Tab::Presets => self.presets.context_execute(target, command, &ctx),
                    Tab::Repos => self.repos.context_execute(target, command, &ctx),
                    Tab::Agents => self.agents.context_execute(target, command, &ctx),
                    Tab::Health => self.health.context_execute(target, command, &ctx),
                }
            }
        }
    }

    fn on_mouse(&mut self, m: MouseEvent) -> Vec<Action> {
        if let Some(prompt) = self.quit_prompt.as_ref() {
            if m.kind == MouseEventKind::Down(MouseButton::Left) {
                let point = (m.column, m.row).into();
                if prompt.force_exit_button.contains(point) {
                    self.quit = true;
                    self.quit_prompt = None;
                } else if prompt.cancel_button.contains(point) || !prompt.area.contains(point) {
                    self.quit_prompt = None;
                }
            }
            return vec![];
        }
        if m.kind == MouseEventKind::Down(MouseButton::Left)
            && self.modal.is_none()
            && self.context_menu.is_none()
            && self.command_palette.is_none()
            && self.sync_button.contains((m.column, m.row).into())
        {
            return self.open_sync(true);
        }
        if let Some(menu) = self.context_menu.as_mut() {
            let event = menu.mouse(m);
            return self.context_event(event);
        }
        if let Some(palette) = self.command_palette.as_mut() {
            let event = palette.mouse(m);
            return self.palette_event(event);
        }
        if self.batch_running
            && matches!(self.modal, Some(Modal::Batch(_) | Modal::PresetSkills(_)))
        {
            return vec![];
        }
        let ctx = Ctx {
            ws: &self.ws,
            snap: &self.snap,
            settings: &self.settings,
        };
        if let Some(modal) = self.modal.as_mut() {
            return modal.handle_mouse(m, &ctx);
        }
        if self.tab == Tab::Agents && self.agents.group_popup_open() {
            return self.agents.handle_mouse(m, &ctx);
        }
        if m.kind == MouseEventKind::Down(MouseButton::Right) {
            let request = match self.tab {
                Tab::Search => self.search.context_menu(m.column, m.row, &ctx),
                Tab::Tags => self.tags.context_menu(m.column, m.row, &ctx),
                Tab::Presets => self.presets.context_menu(m.column, m.row, &ctx),
                Tab::Repos => self.repos.context_menu(m.column, m.row, &ctx),
                Tab::Agents => self.agents.context_menu(m.column, m.row, &ctx),
                Tab::Health => self.health.context_menu(m.column, m.row, &ctx),
            };
            if let Some(request) = request {
                self.focus = AppFocus::Page;
                self.context_menu = Some(ContextMenu::new(request, m.column, m.row));
            }
            return vec![];
        }
        if let MouseEventKind::Down(MouseButton::Left) = m.kind
            && let Some((_, tab)) = self
                .tab_rects
                .iter()
                .find(|(r, _)| r.contains((m.column, m.row).into()))
        {
            let tab = *tab;
            self.switch_tab(tab);
            self.focus = AppFocus::Page;
            return vec![];
        }
        if !self.body.contains((m.column, m.row).into()) {
            return Vec::new();
        }
        if matches!(m.kind, MouseEventKind::Down(_)) {
            self.focus = AppFocus::Page;
        } else if self.focus == AppFocus::Tabs {
            return vec![];
        }
        match self.tab {
            Tab::Search => self.search.handle_mouse(m, &ctx),
            Tab::Tags => self.tags.handle_mouse(m, &ctx),
            Tab::Presets => self.presets.handle_mouse(m, &ctx),
            Tab::Agents => self.agents.handle_mouse(m, &ctx),
            Tab::Health => self.health.handle_mouse(m, &ctx),
            Tab::Repos => self.repos.handle_mouse(m, &ctx),
        }
    }

    fn apply(&mut self, action: Action) {
        self.apply_scoped(action, MutationScope::Library);
    }

    fn apply_scoped(&mut self, action: Action, scope: MutationScope) {
        if let Action::Deployment(action) = action {
            self.apply_scoped(*action, MutationScope::Deployment);
            return;
        }
        let writes = matches!(
            &action,
            Action::Write(_)
                | Action::Spawn(_)
                | Action::EditNote(_)
                | Action::WriteMeta(_)
                | Action::SubmitInput(_)
                | Action::ApplyLinks { .. }
                | Action::BatchMeta(..)
                | Action::BatchLinks { .. }
                | Action::BackgroundWrite { .. }
        );
        if writes
            && (self.batch_running || (self.sync.switching() && scope == MutationScope::Library))
        {
            self.toast(
                "An operation is still running; retry when it finishes",
                Level::Info,
            );
            return;
        }

        match action {
            Action::SetLayout { scope, layout } => {
                self.session_settings.set_layout(scope, layout);
                self.settings
                    .reload(&self.ws.config, &self.session_settings);
            }
            Action::SelectAgentSkills {
                keys,
                title,
                checked,
                agent,
            } => {
                self.apply(Action::SelectSkills {
                    keys,
                    title,
                    checked,
                });
                self.search.restrict_agent(agent);
            }
            Action::Quit => {
                self.sync_if_ready();
                if self.tasks_running == 0 {
                    self.quit = true;
                } else if self.sync.syncing() {
                    self.quit_prompt = Some(QuitPrompt::for_sync());
                } else {
                    self.quit_prompt = Some(QuitPrompt::default());
                }
            }
            Action::SelectSkills {
                keys,
                title,
                checked,
            } => {
                self.switch_tab(Tab::Search);
                let ctx = Ctx {
                    ws: &self.ws,
                    snap: &self.snap,
                    settings: &self.settings,
                };
                self.search.select_scope(keys, title, checked, &ctx);
            }
            Action::Toast(t) => self.toast(t, Level::Ok),
            Action::Error(t) => self.toast(t, Level::Error),
            Action::Rescan => self.rescan(),
            Action::RefreshSyncStatus { remote, reason } => {
                self.refresh_sync_status(remote, reason)
            }
            Action::LibraryChanged => self.library_changed(),
            Action::Deployment(_) => unreachable!("deployment actions are unwrapped above"),
            Action::Spawn(task) => {
                if let Task::RefreshRepository(alias) = &task {
                    self.repos.refreshing(alias);
                }
                self.spawn(task)
            }
            Action::OpenModal(m) => {
                self.context_menu = None;
                self.command_palette = None;
                self.modal = Some(*m);
            }
            Action::CloseModal => {
                self.modal = match self.modal.take() {
                    Some(Modal::Message { return_to, .. }) => return_to.map(|modal| *modal),
                    _ => None,
                };
            }
            Action::SubmitInput(actions) => {
                let prompt = self.modal.take();
                for action in actions {
                    let (scope, action) = match action {
                        Action::Deployment(action) => (MutationScope::Deployment, *action),
                        action => (scope, action),
                    };
                    let result = match action {
                        Action::Error(error) => Err(anyhow::anyhow!(error)),
                        Action::Write(write) => {
                            let result = self.run_write(scope, write);
                            if result.is_ok() {
                                self.mutation_finished(scope);
                            } else {
                                self.rescan();
                            }
                            result.map(|message| self.toast(message, Level::Ok))
                        }
                        Action::WriteMeta(write) => {
                            let result = self.run_meta(scope, write);
                            if result.is_ok() {
                                self.mutation_finished(scope);
                            } else {
                                self.rescan();
                            }
                            result.map(|(message, intent)| {
                                self.toast(message, Level::Ok);
                                if let Some(intent) = intent {
                                    self.history.record(intent);
                                }
                            })
                        }
                        other => {
                            self.apply_scoped(other, scope);
                            Ok(())
                        }
                    };
                    if let Err(error) = result {
                        self.modal = prompt;
                        self.toast(format!("{error:#}"), Level::Error);
                        break;
                    }
                }
            }
            Action::BackToParent => self.focus = AppFocus::Tabs,
            Action::SwitchTab(t) => {
                self.switch_tab(t);
                self.focus = AppFocus::Tabs;
            }
            Action::SelectPreset(name) => self.presets.select(&name),
            Action::SelectTag(name) => self.tags.select(&name, &self.snap),
            Action::Search { query, focus_list } => {
                self.focus = AppFocus::Page;
                self.switch_tab(Tab::Search);
                let ctx = Ctx {
                    ws: &self.ws,
                    snap: &self.snap,
                    settings: &self.settings,
                };
                self.search.set_query(&query, &ctx);
                if focus_list {
                    self.search.focus_list();
                } else {
                    self.search.focus_input();
                }
            }
            Action::EditNote(skill) => {
                let initial = self
                    .snap
                    .get(&skill)
                    .and_then(|r| r.note.clone())
                    .unwrap_or_default();
                self.external = Some(External::EditNote { skill, initial });
            }
            Action::ApplyLinks { title, actions } => {
                if !deploy::name_conflicts(&self.snap, &actions).is_empty() {
                    match skills::ops::name_choices::Pending::for_actions(
                        &self.ws, &self.snap, &actions,
                    ) {
                        Ok(Some(pending)) => {
                            self.modal = Some(Modal::DeploymentChoices(Box::new(
                                super::name_choices::NameChoices::new(pending),
                            )))
                        }
                        Err(error) => self.toast(format!("{error:#}"), Level::Error),
                        Ok(None) => {}
                    }
                    return;
                }
                if !actions.iter().any(|a| a.is_change()) {
                    let reason = actions.iter().find_map(|a| match a {
                        deploy::Action::Skip { reason, .. } => Some(reason.clone()),
                        _ => None,
                    });
                    self.toast(
                        reason.unwrap_or_else(|| "nothing to do".into()),
                        Level::Info,
                    );
                    return;
                }
                match deploy::apply(&actions) {
                    Ok(_) => {
                        self.toast(deploy::summarize(&actions), Level::Ok);
                        if let Some(intent) = history::Intent::from_actions(&actions) {
                            self.history.record(intent);
                        }
                    }
                    Err(e) => self.toast(format!("{title} failed: {e:#}"), Level::Error),
                }
                self.rescan();
            }
            Action::ConfirmLinks { title, actions } => {
                match skills::ops::name_choices::Pending::for_actions(
                    &self.ws, &self.snap, &actions,
                ) {
                    Ok(Some(pending)) => {
                        self.modal = Some(Modal::DeploymentChoices(Box::new(
                            super::name_choices::NameChoices::new(pending),
                        )))
                    }
                    Ok(None) => self.modal = Some(Modal::confirm(title, actions)),
                    Err(error) => self.toast(format!("{error:#}"), Level::Error),
                }
            }

            Action::Record(intent) => self.history.record(intent),
            Action::Step(Step::Undo) => self.history.commit_undo(),
            Action::Step(Step::Redo) => self.history.commit_redo(),
            Action::Write(write) => match self.run_write(scope, write) {
                Ok(msg) => {
                    self.toast(msg, Level::Ok);
                    self.mutation_finished(scope);
                }
                Err(e) => {
                    self.toast(format!("{e:#}"), Level::Error);
                    self.rescan();
                }
            },
            Action::BackgroundWrite { title, write, keys } => {
                self.spawn_batch(super::event::BatchWork::Metadata(scope, write, keys), title);
            }
            Action::BatchMeta(write, keys) => {
                self.spawn_batch(
                    super::event::BatchWork::Metadata(scope, write, keys.clone()),
                    format!("Applying metadata to {} skills…", keys.len()),
                );
            }
            Action::BatchLinks {
                title,
                actions,
                keys,
            } => {
                self.spawn_batch(super::event::BatchWork::Links(actions, keys), title);
            }
            Action::WriteMeta(write) => {
                match self.run_meta(scope, write) {
                    Ok((msg, intent)) => {
                        let undo_hint = if intent.is_some() {
                            " · Ctrl+Z undo"
                        } else {
                            ""
                        };
                        self.toast(format!("{msg}{undo_hint}"), Level::Ok);
                        // A write that left the files as they were is not a step.
                        if let Some(intent) = intent {
                            self.history.record(intent);
                        }
                        self.mutation_finished(scope);
                    }
                    Err(e) => {
                        if let Some(pending) =
                            e.downcast_ref::<skills::ops::name_choices::Pending>()
                        {
                            self.modal = Some(Modal::DeploymentChoices(Box::new(
                                super::name_choices::NameChoices::new(pending.clone()),
                            )));
                        } else {
                            self.toast(format!("{e:#}"), Level::Error);
                        }
                        self.rescan();
                    }
                }
            }
        }
    }

    fn enter_page(&mut self) {
        self.focus = AppFocus::Page;
        let view: &mut dyn View = match self.tab {
            Tab::Search => &mut self.search,
            Tab::Tags => &mut self.tags,
            Tab::Presets => &mut self.presets,
            Tab::Agents => &mut self.agents,
            Tab::Repos => &mut self.repos,
            Tab::Health => &mut self.health,
        };
        view.focus_root();
    }

    /// Make `t` the active tab. The view is told only when the tab actually
    /// changes: a key for the tab already showing is not a return to it, and
    /// must not throw away a focus the user has just set.
    fn switch_tab(&mut self, t: Tab) {
        self.context_menu = None;
        self.command_palette = None;
        let t = if t == Tab::Tags && !self.settings.tags_enabled {
            Tab::Search
        } else {
            t
        };
        if t == self.tab {
            return;
        }
        self.search.clear_selection();
        self.search.restore_results(&Ctx {
            ws: &self.ws,
            snap: &self.snap,
            settings: &self.settings,
        });
        self.tab = t;
        if t == Tab::Agents && self.agents_dirty {
            self.agents.refresh(&Ctx {
                ws: &self.ws,
                snap: &self.snap,
                settings: &self.settings,
            });
            self.agents_dirty = false;
        }
        let view: &mut dyn View = match t {
            Tab::Search => &mut self.search,
            Tab::Tags => &mut self.tags,
            Tab::Presets => &mut self.presets,
            Tab::Agents => &mut self.agents,
            Tab::Health => &mut self.health,
            Tab::Repos => &mut self.repos,
        };
        view.enter();
    }

    /// Take one step back or forward. The plan is worked out against the tree
    /// as it stands, so anything changed since is skipped rather than forced.
    fn step(&mut self, dir: Step) -> Vec<Action> {
        if self.batch_running {
            return vec![Action::Toast(
                "An operation is still running; retry when it finishes".into(),
            )];
        }

        let entry = match dir {
            Step::Undo => self.history.last(),
            Step::Redo => self.history.next_redo(),
        };
        let Some(entry) = entry else {
            return vec![Action::Toast(match dir {
                Step::Undo => "nothing to undo".into(),
                Step::Redo => "nothing to redo".into(),
            })];
        };
        let intent = entry.intent.clone();
        let what = intent.describe();
        // Time has passed since the step was taken, so plan against the tree as
        // it is now rather than the snapshot on screen.
        let snap = match self.ws.scan() {
            Ok(s) => {
                self.snap = s;
                &self.snap
            }
            Err(e) => return vec![Action::Error(format!("scan failed: {e:#}"))],
        };
        let plan = match dir {
            Step::Undo => history::undo_plan(&self.ws, snap, &intent),
            Step::Redo => history::redo_plan(&self.ws, snap, &intent),
        };
        match plan {
            Ok(Plan::Links(actions)) => {
                let verb = if dir == Step::Undo { "undo" } else { "redo" };
                self.modal = Some(Modal::confirm_then(
                    format!("{verb}: {what}"),
                    actions,
                    Some(dir),
                ));
                vec![]
            }
            Ok(Plan::Write { describe, apply }) => {
                self.modal = Some(Modal::undo_write(describe, apply, dir));
                vec![]
            }
            // Nothing left for this step to do, either because someone already
            // put things where it would leave them or because they went
            // somewhere else entirely. Drop the entry rather than offer a
            // change with nothing in it, and say which it was.
            Ok(Plan::Nothing(why)) => {
                match dir {
                    Step::Undo => self.history.commit_undo(),
                    Step::Redo => self.history.commit_redo(),
                }
                vec![Action::Toast(why)]
            }
            Err(e) => vec![Action::Error(format!("{e:#}"))],
        }
    }

    fn spawn_batch(&mut self, work: super::event::BatchWork, title: String) {
        if self.batch_running {
            return;
        }
        self.batch_running = true;
        self.batch_modal_owned =
            matches!(self.modal, Some(Modal::Batch(_) | Modal::PresetSkills(_)));
        self.next_task_id += 1;
        let id = self.next_task_id;
        self.tasks_running += 1;
        self.toasts.start(id, title);
        if let Some(Modal::Batch(batch)) = self.modal.as_mut() {
            batch.set_busy(true);
        }
        if let Err(e) = super::event::spawn_batch(self.ws.clone(), work, id, self.tx.clone()) {
            self.batch_running = false;
            self.tasks_running = self.tasks_running.saturating_sub(1);
            self.toasts.finish(id);
            if let Some(Modal::Batch(batch)) = self.modal.as_mut() {
                batch.set_busy(false);
            }
            self.toast(format!("Cannot start batch: {e}"), Level::Error);
        }
    }

    fn spawn(&mut self, task: Task) {
        if self.sync.switching() && task.writes_library() {
            self.toast(
                "Root sync is updating the Library; retry when it finishes",
                Level::Info,
            );
            return;
        }
        if task.is_root_operation() {
            if self.tasks_running > 0 {
                self.toast(
                    "Wait for the current operation before changing root backup",
                    Level::Info,
                );
                return;
            }
            if task.is_root_sync() {
                if let Task::Sync(request) = &task
                    && !request.dry_run
                    && !self.sync.syncing()
                {
                    self.sync.start_manual_sync();
                }
            } else {
                self.batch_running = true;
            }
        }
        if matches!(task, Task::RepairApply(_)) {
            self.batch_running = true;
        }
        self.tasks_running += 1;
        self.next_task_id += 1;
        let id = self.next_task_id;
        let label = task.label();
        if let Some(label) = label {
            self.toasts.start(id, label);
        }
        spawn_task(self.ws.clone(), task, id, self.tx.clone());
    }

    pub fn rescan(&mut self) {
        // Refresh one configuration snapshot for every view. Invalid edits keep
        // the last valid settings and report the error without disrupting input.
        match self.ws.load_config() {
            Ok(config) => {
                self.ws.config = config;
                if let Err(e) = Self::discover_local_agents(&mut self.ws) {
                    self.toast(format!("{e:#}"), Level::Error);
                }
                // The last successful inventory stays visible while scanning.
                // Publish valid settings together with their dependent caches
                // now, so a slow or failed scan cannot leave old search rules.
                self.on_snapshot();
            }
            Err(e) => self.toast(format!("{e:#}"), Level::Error),
        }
        self.spawn(Task::Scan);
    }

    fn library_changed(&mut self) {
        self.sync.library_changed();
        // A running read may have captured the tree before this mutation.
        // Retire it and replace it without joining either worker.
        self.refresh_sync_status(false, ProbeReason::Mutation);
        self.rescan();
    }

    fn mutation_finished(&mut self, scope: MutationScope) {
        match scope {
            MutationScope::Library => self.library_changed(),
            MutationScope::Deployment => self.rescan(),
        }
    }

    fn run_write(&self, scope: MutationScope, write: WriteFn) -> Result<String> {
        let _guard = scope.guard(&self.ws, "Library write")?;
        write(&self.ws)
    }

    fn run_meta(
        &self,
        scope: MutationScope,
        write: MetaFn,
    ) -> Result<(String, Option<history::Intent>)> {
        let _guard = scope.guard(&self.ws, "Library metadata write")?;
        write(&self.ws)
    }

    // ---- drawing ----------------------------------------------------------

    pub fn draw(&mut self, f: &mut Frame) {
        if self.modal.is_some() || self.quit_prompt.is_some() {
            self.context_menu = None;
            self.command_palette = None;
        }
        let area = f.area();
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Min(3),
                Constraint::Length(1),
            ])
            .split(area);
        self.draw_header(f, rows[0]);
        self.body = rows[1];
        let ctx = Ctx {
            ws: &self.ws,
            snap: &self.snap,
            settings: &self.settings,
        };
        match self.tab {
            Tab::Search => self.search.draw(f, rows[1], &ctx),
            Tab::Tags => self.tags.draw(f, rows[1], &ctx),
            Tab::Presets => self.presets.draw(f, rows[1], &ctx),
            Tab::Agents => self.agents.draw(f, rows[1], &ctx),
            Tab::Health => self.health.draw(f, rows[1], &ctx),
            Tab::Repos => self.repos.draw(f, rows[1], &ctx),
        }
        if self.focus == AppFocus::Tabs {
            let th = &self.settings.theme;
            for y in rows[1].y..rows[1].bottom() {
                for x in rows[1].x..rows[1].right() {
                    let cell = &mut f.buffer_mut()[(x, y)];
                    if cell.fg == th.border_focus {
                        cell.fg = th.border;
                    }
                    if cell.bg == th.selection_bg {
                        cell.bg = ratatui::style::Color::Reset;
                    }
                    cell.modifier.remove(Modifier::BOLD);
                    cell.modifier.insert(Modifier::DIM);
                }
            }
            if let Some((rect, _)) = self.tab_rects.iter().find(|(_, t)| *t == self.tab) {
                f.set_cursor_position((rect.x, rect.y));
            }
        }
        self.draw_footer(f, rows[2]);
        if let Some(m) = self.modal.as_mut() {
            m.draw(f, area, &ctx);
        }
        if let Some(menu) = self.context_menu.as_mut() {
            menu.draw(f, area, &self.settings.theme);
        }
        if let Some(palette) = self.command_palette.as_mut() {
            palette.draw(f, area, &ctx);
        }
        // Above everything: a notification should be readable over a dialog.
        self.toasts.draw(f, area, &self.settings.theme);
        if let Some(prompt) = self.quit_prompt.as_mut() {
            let mut tasks = self.toasts.running_details();
            let scans = self.tasks_running.saturating_sub(tasks.len());
            if scans > 0 {
                tasks.push(format!("Scan skills: {scans} running"));
            }
            prompt.draw(f, area, &self.settings.theme, tasks);
        }
    }

    fn draw_header(&mut self, f: &mut Frame, area: Rect) {
        let th = &self.settings.theme;
        self.tab_rects.clear();
        self.sync_button = Rect::default();
        let (button, button_style) = self.sync_button_label(area.width >= 72);
        let button_width = (width(&button) as u16).min(area.width);
        if button_width > 0 {
            self.sync_button = Rect::new(area.right() - button_width, area.y, button_width, 1);
        }
        let left_area = Rect::new(
            area.x,
            area.y,
            area.width.saturating_sub(button_width),
            area.height,
        );
        if left_area.width < self.settings.layout.narrow_header_width {
            let title = format!(" {} ", self.tab.title());
            self.tab_rects.push((
                Rect::new(
                    area.x,
                    area.y,
                    (width(&title) as u16).min(left_area.width),
                    1,
                ),
                self.tab,
            ));
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(title, th.selected()),
                    Span::styled(
                        "  Tab → next · Shift+Tab ←",
                        Style::default().fg(th.placeholder),
                    ),
                ])),
                left_area,
            );
            f.render_widget(Paragraph::new(button).style(button_style), self.sync_button);
            return;
        }
        let compact = left_area.width < self.settings.layout.compact_header_width;
        let mut spans: Vec<Span> = if compact {
            vec![]
        } else {
            vec![Span::styled(" skills ", th.bold().fg(th.accent))]
        };
        let mut x = area.x + if compact { 0 } else { width(" skills ") as u16 };
        for (i, t) in Tab::visible(self.settings.tags_enabled).iter().enumerate() {
            let label = if compact {
                format!(" {} ", t.title())
            } else {
                format!(" {} {} ", i + 1, t.title())
            };
            let style = if *t == self.tab {
                if self.focus == AppFocus::Tabs {
                    th.selected().add_modifier(Modifier::BOLD)
                } else {
                    th.bold()
                }
            } else {
                th.dim()
            };
            let w = width(&label) as u16;
            self.tab_rects.push((Rect::new(x, area.y, w, 1), *t));
            spans.push(Span::styled(label, style));
            spans.push(Span::raw(" "));
            x += w + 1;
        }
        spans.push(Span::styled(" Tab ↔ ", Style::default().fg(th.placeholder)));
        x += width(" Tab ↔ ") as u16;
        let used = (x - area.x) as usize;
        let right = if self.tasks_running > 0 && !self.sync.syncing() {
            format!("{} working  ", SPINNER[self.spinner])
        } else {
            format!(
                "{}{}  ",
                if self.ws.project.is_some() {
                    "local: "
                } else {
                    ""
                },
                skills::paths::contract_tilde(&self.snap.root)
            )
        };
        if used + width(&right) < left_area.width as usize {
            let pad = left_area.width as usize - used - width(&right);
            spans.push(Span::raw(" ".repeat(pad)));
            spans.push(Span::styled(right, th.dim()));
        }
        f.render_widget(Paragraph::new(Line::from(spans)), left_area);
        f.render_widget(Paragraph::new(button).style(button_style), self.sync_button);
    }

    fn sync_button_label(&self, expanded: bool) -> (String, Style) {
        let th = &self.settings.theme;
        let (compact, text, style): (String, String, Style) = if self.sync.syncing() {
            (SPINNER[self.spinner].into(), "Syncing…".into(), th.accent())
        } else if self.sync.probing() {
            (SPINNER[self.spinner].into(), "Checking…".into(), th.dim())
        } else if self.sync.error.is_some() {
            ("×".into(), "× Check failed".into(), th.err())
        } else if let Some(status) = &self.sync.status {
            if status.settings.url.is_none() || status.settings.branch.is_none() {
                ("○".into(), "○ Set up backup".into(), th.warn())
            } else if !status.changes.is_empty() || status.ahead > 0 || status.behind > 0 {
                let mut counts = Vec::new();
                if !status.changes.is_empty() {
                    counts.push(format!("● {}", status.changes.len()));
                }
                if status.ahead > 0 {
                    counts.push(format!("↑ {}", status.ahead));
                }
                if status.behind > 0 {
                    counts.push(format!("↓ {}", status.behind));
                }
                let compact = if !status.changes.is_empty() {
                    "●"
                } else if status.ahead > 0 && status.behind > 0 {
                    "↑↓"
                } else if status.ahead > 0 {
                    "↑"
                } else {
                    "↓"
                };
                (compact.into(), counts.join(" · "), th.warn())
            } else if status.remote_checked {
                ("✓".into(), "✓ Backed up".into(), th.ok())
            } else {
                ("○".into(), "○ Backup ready".into(), th.dim())
            }
        } else {
            ("○".into(), "○ Backup".into(), th.dim())
        };
        (
            if expanded {
                let text = if text.starts_with(&compact) {
                    text
                } else {
                    format!("{compact} {text}")
                };
                format!("[{text}]")
            } else {
                format!("[{compact}]")
            },
            style.add_modifier(Modifier::BOLD),
        )
    }

    fn draw_footer(&self, f: &mut Frame, area: Rect) {
        if self.context_menu.is_some() {
            f.render_widget(Paragraph::new(" ".repeat(area.width as usize)), area);
            return;
        }

        let th = &self.settings.theme;
        let ctx = Ctx {
            ws: &self.ws,
            snap: &self.snap,
            settings: &self.settings,
        };
        let status = if self.modal.is_some() || self.quit_prompt.is_some() {
            String::new()
        } else {
            match self.tab {
                Tab::Search => self.search.status(&ctx),
                Tab::Tags => self.tags.status(&ctx),
                Tab::Presets => self.presets.status(&ctx),
                _ => String::new(),
            }
        };
        let status = fit(&status, area.width as usize / 3);
        let status_width = width(&status);
        let budget = area.width as usize - status_width;
        let hints = self.footer_hints();
        // Results moved out to the notification stack, so the footer is only keys.
        let mut spans: Vec<Span> = Vec::new();
        let mut hint_w = 0usize;
        let escape = hints.iter().find(|(key, _)| key.contains("Esc"));
        let show_globals = self.global_hints_visible();
        let global_hints = [
            self.actions_available().then_some(("a", "actions")),
            Some((":", "commands")),
            Some(("?", "help")),
        ];
        let global_width = if show_globals {
            global_hints
                .iter()
                .flatten()
                .map(|(key, desc)| width(key) + width(desc) + 3)
                .sum()
        } else {
            0
        };
        let reserve = escape.map_or(0, |(key, desc)| width(key) + width(desc) + 3) + global_width;
        for (key, desc) in hints {
            if (!self.settings.tags_enabled && *key == "t")
                || key.contains("Esc")
                || (show_globals && global_hints.contains(&Some((*key, *desc))))
            {
                continue;
            }
            let piece_w = width(key) + width(desc) + 3;
            if hint_w + piece_w + reserve > budget {
                break;
            }
            spans.push(Span::styled(*key, th.key_hint()));
            spans.push(Span::styled(
                format!(" {desc}  "),
                Style::default().fg(th.placeholder),
            ));
            hint_w += piece_w;
        }
        if let Some((key, desc)) = escape {
            let piece_w = width(key) + width(desc) + 3;
            if hint_w + piece_w <= budget {
                spans.push(Span::styled(*key, th.key_hint()));
                spans.push(Span::styled(
                    format!(" {desc}  "),
                    Style::default().fg(th.placeholder),
                ));
                hint_w += piece_w;
            }
        }
        if show_globals {
            for (key, desc) in global_hints.into_iter().flatten() {
                let piece_w = width(key) + width(desc) + 3;
                if hint_w + piece_w > budget {
                    break;
                }
                spans.push(Span::styled(key, th.key_hint()));
                spans.push(Span::styled(
                    format!(" {desc}  "),
                    Style::default().fg(th.placeholder),
                ));
                hint_w += piece_w;
            }
        }
        let pad = budget.saturating_sub(hint_w);
        let mut line = vec![
            Span::styled(status, th.skill_count()),
            Span::raw(" ".repeat(pad)),
        ];
        line.extend(spans);
        f.render_widget(Paragraph::new(Line::from(line)), area);
    }

    /// Return the shortcuts owned by the surface that currently receives
    /// keyboard input. Keeping this decision in one place prevents the global
    /// footer from retaining a page hint while a modal or quit prompt owns
    /// focus.
    fn footer_hints(&self) -> Hints {
        if self.quit_prompt.is_some() {
            return &[("←→", "buttons"), ("Enter", "confirm"), ("Esc/q", "cancel")];
        }
        if self.batch_running && self.batch_modal_owned {
            return &[("…", "saving changes")];
        }
        if let Some(m) = &self.modal {
            return m.hints();
        }
        if let Some(palette) = &self.command_palette {
            return palette.hints();
        }
        if self.focus == AppFocus::Tabs {
            return &[
                ("←→/Tab", "tabs"),
                ("Enter/↓", "enter"),
                (":", "commands"),
                ("Esc/q", "quit"),
            ];
        }
        match self.tab {
            Tab::Search => self.search.hints(),
            Tab::Tags => self.tags.hints(),
            Tab::Presets => self.presets.hints(),
            Tab::Agents => self.agents.hints(),
            Tab::Health => self.health.hints(),
            Tab::Repos => self.repos.hints(),
        }
    }
}

/// Key hint pairs shown in the footer.
pub type Hints = &'static [(&'static str, &'static str)];

/// Fit the root into its header allocation while keeping the directory tail.
pub(crate) fn middle_ellipsis(text: &str, max: usize) -> String {
    use unicode_segmentation::UnicodeSegmentation;
    if width(text) <= max {
        return text.to_owned();
    }
    if max == 0 {
        return String::new();
    }
    let prefix_budget = (max - 1) / 2;
    let mut prefix = String::new();
    for glyph in text.graphemes(true) {
        if width(&prefix) + width(glyph) > prefix_budget {
            break;
        }
        prefix.push_str(glyph);
    }
    let suffix_budget = max - 1 - width(&prefix);
    let mut suffix = Vec::new();
    let mut used = 0;
    for glyph in text.graphemes(true).rev() {
        if used + width(glyph) > suffix_budget {
            break;
        }
        used += width(glyph);
        suffix.push(glyph);
    }
    format!("{prefix}…{}", suffix.into_iter().rev().collect::<String>())
}

#[cfg(test)]
mod matrix_key_tests {
    use super::*;

    #[test]
    fn config_reload_refreshes_search_before_scan_and_keeps_valid_rules_on_scan_failure() {
        let tmp = skills::ops::DownloadDir::new("settings-reload").unwrap();
        std::fs::create_dir(tmp.path().join("printer")).unwrap();
        std::fs::write(
            tmp.path().join("printer/SKILL.md"),
            "---\nname: printer\ndescription: Print documents\n---\n",
        )
        .unwrap();
        let mut config = Config {
            agents: vec![],
            ..Default::default()
        };
        config.save(tmp.path()).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let mut app = App::new_with_launch_directory(
            Workspace::open(tmp.path()).unwrap(),
            tx,
            Some(tmp.path()),
        )
        .unwrap();
        app.apply(Action::Search {
            query: "prnter".into(),
            focus_list: true,
        });
        let selected = |app: &App| {
            app.search.panel_keys(&Ctx {
                ws: &app.ws,
                snap: &app.snap,
                settings: &app.settings,
            })
        };
        assert_eq!(selected(&app), vec!["printer"]);

        config.search.fuzzy = false;
        config.save(tmp.path()).unwrap();
        app.rescan();
        assert!(!app.settings.search.fuzzy);
        assert_eq!(app.search.query(), "prnter");
        assert!(
            selected(&app).is_empty(),
            "cached search rules must refresh immediately"
        );

        // Receive the worker result before dropping its temporary root, then
        // exercise the failed-scan path while retaining the known inventory.
        let id = loop {
            match rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap() {
                Msg::Task(id, out) if matches!(*out, TaskOutput::Scan(..)) => break id,
                _ => {}
            }
        };
        app.handle(Msg::Task(
            id,
            Box::new(TaskOutput::Scan(
                Err(anyhow::anyhow!("scan unavailable")),
                None,
            )),
        ));
        assert!(app.snap.get("printer").is_some());
        assert!(selected(&app).is_empty());
        assert!(!app.settings.search.fuzzy);

        std::fs::write(Config::path(tmp.path()), "[invalid").unwrap();
        app.rescan();
        assert!(
            !app.settings.search.fuzzy,
            "invalid edits keep the last valid settings"
        );
        assert_eq!(app.search.query(), "prnter");
        assert!(selected(&app).is_empty());
        app.handle(rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap());
        assert!(selected(&app).is_empty());
    }

    #[test]
    fn member_picker_apply_saves_once_and_returns_to_results() {
        let tmp = skills::ops::DownloadDir::new("member-picker-finish").unwrap();
        std::fs::create_dir(tmp.path().join("alpha")).unwrap();
        std::fs::write(
            tmp.path().join("alpha/SKILL.md"),
            "---\nname: alpha\n---\nBody",
        )
        .unwrap();
        Config {
            agents: vec![],
            ..Default::default()
        }
        .save(tmp.path())
        .unwrap();
        let ws = Workspace::open(tmp.path()).unwrap();
        skills::history::preset_create(&ws, "example").unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let mut app = App::new(ws, tx).unwrap();
        let ctx = Ctx {
            ws: &app.ws,
            snap: &app.snap,
            settings: &app.settings,
        };
        let mut picker = SearchView::preset_members("example", &ctx);
        picker.focus_list();
        picker.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE), &ctx);
        app.modal = Some(Modal::PresetSkills(Box::new(picker)));
        // Force a real filesystem error after the selection has been staged.
        let path = app.ws.presets.path("example");
        let original = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        app.handle(Msg::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)));
        app.handle(Msg::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
        while app.batch_running || app.tasks_running > 0 {
            app.handle(rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap());
        }
        assert!(matches!(
            app.modal,
            Some(Modal::Message {
                return_to: Some(_),
                ..
            })
        ));
        app.handle(Msg::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        assert!(matches!(app.modal, Some(Modal::PresetSkills(_))));
        std::fs::remove_dir(&path).unwrap();
        std::fs::write(&path, &original).unwrap();
        // Retry without selecting again: the original staged selection survives.
        app.handle(Msg::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
        assert!(app.batch_running);
        let task = app.next_task_id;
        app.handle(Msg::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
        assert_eq!(
            app.next_task_id, task,
            "repeated apply must not submit twice"
        );
        while app.batch_running || app.tasks_running > 0 {
            app.handle(rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap());
        }
        assert!(app.modal.is_none(), "successful apply finishes editing");
        assert_eq!(
            app.ws.presets.load("example").unwrap().unwrap().skills,
            ["alpha"]
        );
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        let text: String = term
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("Ctrl+Z undo"));
    }

    #[test]
    fn confirmation_ignores_modified_keys_and_help_pages_without_closing() {
        let tmp = skills::ops::DownloadDir::new("modal-key-audit").unwrap();
        let ws = Workspace::open(tmp.path()).unwrap();
        let snap = ws.scan().unwrap();
        let theme = Theme::default();
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &{
                let mut settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
                settings.theme = theme;
                settings
            },
        };
        for mut modal in [
            Modal::confirm("test".into(), vec![]),
            Modal::confirm_write("test".into(), vec![], Box::new(|_| Ok("saved".into()))),
        ] {
            for modifiers in [KeyModifiers::CONTROL, KeyModifiers::ALT] {
                for code in [KeyCode::Char('y'), KeyCode::Char('n'), KeyCode::Enter] {
                    assert!(
                        modal
                            .handle_key(KeyEvent::new(code, modifiers), &ctx)
                            .is_empty()
                    );
                }
            }
            if matches!(modal, Modal::ConfirmWrite { .. }) {
                assert!(
                    !modal
                        .handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE), &ctx)
                        .is_empty()
                );
            }
        }
        let mut help = Modal::help();
        assert!(
            help.handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE), &ctx)
                .is_empty()
        );
        assert!(matches!(help, Modal::Help { scroll: 10 }));
        assert!(
            help.handle_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE), &ctx)
                .is_empty()
        );
        assert!(matches!(help, Modal::Help { scroll: 0 }));
    }

    #[test]
    fn tag_completion_reaches_modal_and_keeps_close_hint_in_small_windows() {
        let tmp = skills::ops::DownloadDir::new("tag-key-route").unwrap();
        std::fs::create_dir(tmp.path().join("alpha")).unwrap();
        std::fs::write(
            tmp.path().join("alpha/SKILL.md"),
            "---\nname: alpha\n---\nBody",
        )
        .unwrap();
        let mut ws = Workspace::open(tmp.path()).unwrap();
        skills::ops::edit::tag_add(&ws, "alpha", &["meta-skill".into()]).unwrap();
        ws.config = ws.load_config().unwrap();
        let (tx, _) = std::sync::mpsc::channel();
        let mut app = App::new(ws, tx).unwrap();
        let ctx = Ctx {
            ws: &app.ws,
            snap: &app.snap,
            settings: &app.settings,
        };
        app.modal = Some(Modal::batch_tags(vec!["alpha".into()], &ctx));
        for c in "met".chars() {
            assert!(
                app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
                    .is_empty()
            );
        }
        assert!(
            app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))
                .is_empty()
        );
        for width in [40, 80] {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 24)).unwrap();
            terminal.draw(|f| app.draw(f)).unwrap();
            let buffer = terminal.backend().buffer();
            let text: String = buffer.content.iter().map(|c| c.symbol()).collect();
            assert!(text.contains("› meta-skill"));
            assert!(text.contains("Tags · alpha"));
            let footer: String = (0..width).map(|x| buffer[(x, 23)].symbol()).collect();
            assert!(footer.contains("Enter remove"));
            assert!(footer.contains("Esc done"));
            assert!(!footer.contains("add / create"));
        }
    }

    #[test]
    fn header_paths_keep_the_tail_with_unicode_safe_middle_ellipsis() {
        let path = "/temporary/long-parent-directory/project/skills";
        let shortened = middle_ellipsis(path, 16);
        assert!(shortened.starts_with("/tempor"));
        assert!(shortened.ends_with("/skills"));
        assert!(shortened.contains('…'));
        for max in 0..50 {
            assert!(width(&middle_ellipsis("/文件系统/打印机/skills", max)) <= max);
        }
        assert_eq!(middle_ellipsis("/skills", 30), "/skills");
    }

    #[test]
    fn unchanged_editor_buffer_does_not_create_metadata_or_start_a_scan() {
        let root =
            std::env::temp_dir().join(format!("skills-editor-unchanged-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("printer")).unwrap();
        std::fs::write(
            root.join("printer/SKILL.md"),
            "---\nname: printer\ndescription: Print documents\n---\n",
        )
        .unwrap();
        skills::config::Config {
            agents: vec![],
            ..Default::default()
        }
        .save(&root)
        .unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(Workspace::open(&root).unwrap(), tx).unwrap();
        let running = app.tasks_running;
        app.finish_external(Ok(("printer".into(), None)));
        assert!(app.ws.meta.load("printer").unwrap().is_none());
        assert_eq!(app.tasks_running, running);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 24)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        let rendered: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(rendered.contains("note unchanged on printer"));
        assert!(!rendered.contains("note saved"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn background_poll_refreshes_external_moves_and_deletions_and_waits_for_dialogs() {
        let tmp = skills::ops::DownloadDir::new("tui-external-refresh").unwrap();
        let root = tmp.path();
        std::fs::create_dir(root.join("old")).unwrap();
        std::fs::write(root.join("old/SKILL.md"), "---\nname: old\n---\nBody").unwrap();
        skills::config::Config {
            agents: vec![],
            ..Default::default()
        }
        .save(root)
        .unwrap();
        let ws = Workspace::open(root).unwrap();
        skills::ops::edit::tag_add(&ws, "old", &["keep".into()]).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let mut app = App::new(ws, tx).unwrap();
        std::fs::rename(root.join("old"), root.join("new")).unwrap();
        app.last_root_poll = std::time::Instant::now() - std::time::Duration::from_secs(3);
        app.modal = Some(Modal::help());
        app.handle(Msg::Tick);
        assert_eq!(app.tasks_running, 0);
        app.modal = None;
        app.handle(Msg::Tick);
        assert_eq!(app.tasks_running, 1);
        while app.tasks_running > 0 {
            app.handle(rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap());
        }
        assert!(app.snap.get("new").is_some());
        assert!(matches!(
            app.snap.get("old").unwrap().status,
            skills::reconcile::SkillStatus::Missing
        ));
        std::fs::remove_dir_all(root.join("new")).unwrap();
        app.last_root_poll = std::time::Instant::now() - std::time::Duration::from_secs(3);
        app.handle(Msg::Tick);
        while app.tasks_running > 0 {
            app.handle(rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap());
        }
        assert!(app.snap.get("new").is_none());
        assert_eq!(
            app.snap.get("old").unwrap().status,
            skills::reconcile::SkillStatus::Missing
        );
        assert_eq!(
            skills::config::Config::load(&app.ws.root)
                .unwrap()
                .skill_tags("old"),
            vec!["keep"]
        );
        app.last_root_poll = std::time::Instant::now() - std::time::Duration::from_secs(3);
        app.handle(Msg::Tick);
        let id = app.next_task_id;
        app.handle(rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap());
        assert_eq!(
            app.next_task_id, id,
            "unchanged roots must not launch a scan"
        );
    }

    #[test]
    fn background_write_keeps_navigation_responsive_and_preserves_new_dialog() {
        let tmp = skills::ops::DownloadDir::new("responsive-write").unwrap();
        skills::config::Config {
            agents: vec![],
            ..Default::default()
        }
        .save(tmp.path())
        .unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let mut app = App::new(Workspace::open(tmp.path()).unwrap(), tx).unwrap();
        let (release, wait) = std::sync::mpsc::channel();
        app.apply(Action::BackgroundWrite {
            title: "Slow operation".into(),
            keys: vec![],
            write: Box::new(move |_| {
                wait.recv().unwrap();
                Ok(("done".into(), None))
            }),
        });
        assert!(app.batch_running);
        app.handle(Msg::Key(KeyEvent::from(KeyCode::Tab)));
        assert_eq!(app.tab, Tab::Tags);
        app.apply(Action::Write(Box::new(|_| {
            panic!("overlapping writes must be blocked")
        })));
        app.modal = Some(Modal::help());
        release.send(()).unwrap();
        app.benchmark_drain(&rx);
        assert!(
            app.modal.is_some(),
            "worker completion must not close a newer dialog"
        );
        assert!(!app.batch_running);
    }

    #[test]
    fn quitting_running_work_requires_explicit_confirmation_and_preserves_input() {
        let root = std::env::temp_dir().join(format!("skills-quit-guard-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        Config {
            agents: vec![],
            ..Config::default()
        }
        .save(&root)
        .unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(Workspace::open(&root).unwrap(), tx).unwrap();
        app.tasks_running = 1;
        app.toasts.start(1, "Check upstream: 30 skills".into());
        app.handle(Msg::Progress(
            1,
            "12/30 complete · querying printer…".into(),
        ));
        for tab in Tab::ALL {
            app.switch_tab(tab);
            app.enter_page();
            for _ in 0..8 {
                if app.focus == AppFocus::Tabs {
                    break;
                }
                app.handle(Msg::Key(KeyEvent::new(
                    KeyCode::Char('q'),
                    KeyModifiers::NONE,
                )));
                assert_eq!(app.tab, tab);
                assert!(!app.quit);
                assert!(app.quit_prompt.is_none());
            }
            assert_eq!(app.focus, AppFocus::Tabs);
            app.handle(Msg::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
            assert!(
                app.quit_prompt.is_some(),
                "tab strip uses the running-work quit guard"
            );
            app.handle(Msg::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
            assert!(!app.quit);
        }
        app.modal = Some(Modal::set_source("printer", None));
        app.handle(Msg::Paste("https://example.com/team/tools".into()));
        app.batch_running = true;
        app.handle(Msg::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        )));
        assert!(app.quit_prompt.is_some());
        assert!(!app.quit);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 24)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        let rendered: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(rendered.contains("12/30 complete"));
        assert!(rendered.contains("Quit now and abandon running work?"));
        app.handle(Msg::Paste("unexpected paste".into()));
        app.handle(Msg::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
        assert!(!app.quit);
        assert!(app.quit_prompt.is_none());
        let Some(Modal::Input { input, .. }) = &app.modal else {
            panic!("lost editing dialog")
        };
        assert_eq!(input.value(), "https://example.com/team/tools");
        app.handle(Msg::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        )));
        app.tasks_running = 0;
        app.toasts.finish(1);
        terminal.draw(|f| app.draw(f)).unwrap();
        let rendered: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(rendered.contains("Background work has finished"));
        app.handle(Msg::Key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)));
        app.handle(Msg::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
        assert!(app.quit);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn root_sync_keeps_navigation_live_and_dismisses_its_exit_prompt_when_done() {
        let temp = skills::ops::DownloadDir::new("sync-exit-prompt").unwrap();
        Config {
            agents: vec![],
            ..Default::default()
        }
        .save(temp.path())
        .unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(Workspace::open(temp.path()).unwrap(), tx).unwrap();
        app.sync.start_manual_sync();
        app.batch_running = true;
        app.tasks_running = 1;

        app.handle(Msg::Key(KeyEvent::from(KeyCode::Tab)));
        assert_eq!(app.tab, Tab::Tags, "sync must not swallow navigation");

        app.apply(Action::Quit);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 24)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        let rendered: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(rendered.contains("Root sync is still in progress."));
        assert!(rendered.contains("Force exit"));
        assert!(!app.quit);

        app.handle(Msg::Key(KeyEvent::from(KeyCode::Enter)));
        assert!(!app.quit, "Cancel must be selected by default");
        assert!(app.quit_prompt.is_none());

        app.apply(Action::Quit);
        app.handle(Msg::Key(KeyEvent::from(KeyCode::Left)));
        app.handle(Msg::Key(KeyEvent::from(KeyCode::Enter)));
        assert!(app.quit, "Force exit must leave immediately");
        app.quit = false;
        app.apply(Action::Quit);

        app.handle(Msg::Task(
            1,
            Box::new(TaskOutput::Sync(
                super::super::sync_picker::Request {
                    mode: skills::ops::sync::Mode::Sync,
                    dry_run: false,
                },
                Ok(skills::ops::sync::Report::default()),
            )),
        ));
        assert!(app.quit_prompt.is_none());
        assert!(app.quit, "a completed sync must finish the pending exit");
    }

    #[test]
    fn source_paste_never_submits_or_dispatches_shortcuts() {
        let root = std::env::temp_dir().join(format!("skills-source-paste-{}", std::process::id()));
        std::fs::create_dir_all(root.join("sample")).unwrap();
        std::fs::write(
            root.join("sample/SKILL.md"),
            "---\nname: sample\ndescription: Sample\n---\nBody\n",
        )
        .unwrap();
        Config {
            agents: vec![],
            ..Config::default()
        }
        .save(&root)
        .unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(Workspace::open(&root).unwrap(), tx).unwrap();
        app.modal = Some(Modal::set_source("sample", None));
        app.handle(Msg::Paste(
            "https://example.com/team/tools\nset -g junk 1\nmore junk".into(),
        ));
        let Some(Modal::Input {
            input,
            kind: super::super::modal::InputKind::SetSource { skill },
            ..
        }) = &app.modal
        else {
            panic!("paste changed the dialog");
        };
        assert_eq!(skill, "sample");
        assert!(input.is_empty());
        assert!(!root.join(".skills-meta/sample.toml").exists());
        app.handle(Msg::Paste("https://example.com/team/tools".into()));
        let Some(Modal::Input { input, .. }) = &app.modal else {
            panic!("paste submitted the dialog");
        };
        assert_eq!(input.value(), "https://example.com/team/tools");
        assert!(!root.join(".skills-meta/sample.toml").exists());
        app.handle(Msg::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        app.handle(Msg::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
        app.handle(Msg::Paste("sqx123\n".into()));
        assert!(app.modal.is_none());
        assert!(!app.quit);
        assert_eq!(app.tab, Tab::Search);
        assert!(!root.join(".skills-meta/sample.toml").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn batch_worker_keeps_ticks_live_and_rejects_duplicate_submission() {
        let root = std::env::temp_dir().join(format!("skills-async-batch-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        Config {
            agents: vec![],
            ..Config::default()
        }
        .save(&root)
        .unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let mut app = App::new(Workspace::open(&root).unwrap(), tx).unwrap();
        let (release, wait) = std::sync::mpsc::channel();
        app.apply(Action::BatchMeta(
            Box::new(move |_| {
                wait.recv().unwrap();
                Ok(("done".into(), None))
            }),
            vec![],
        ));
        assert!(app.batch_running);
        let tick = app.spinner;
        app.handle(Msg::Tick);
        assert_ne!(tick, app.spinner);
        let id = app.next_task_id;
        app.apply(Action::BatchMeta(
            Box::new(|_| panic!("duplicate work must not execute")),
            vec![],
        ));
        assert_eq!(id, app.next_task_id);
        app.handle(Msg::Key(KeyEvent::new(
            KeyCode::Char('q'),
            KeyModifiers::NONE,
        )));
        assert!(!app.quit);
        release.send(()).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while app.batch_running || app.tasks_running > 0 {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            app.handle(rx.recv_timeout(remaining).unwrap());
        }
        assert!(app.modal.is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn background_dialogs_wait_for_editing_and_keep_arrival_order() {
        let root = std::env::temp_dir().join(format!("skills-task-dialogs-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        Config {
            agents: vec![],
            ..Config::default()
        }
        .save(&root)
        .unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(Workspace::open(&root).unwrap(), tx).unwrap();
        app.modal = Some(Modal::new_preset());
        let key = |code| Msg::Key(KeyEvent::new(code, KeyModifiers::NONE));
        app.handle(key(KeyCode::Char('a')));
        app.handle(key(KeyCode::Char('/')));
        app.handle(key(KeyCode::Left));
        for reference in ["first", "second"] {
            let workdir = root.join(".downloads").join(reference);
            for skill in ["printer", "reader"] {
                std::fs::create_dir_all(workdir.join(skill)).unwrap();
                std::fs::write(
                    workdir.join(skill).join("SKILL.md"),
                    format!("---\nname: {skill}\ndescription: Sample skill\n---\nBody\n"),
                )
                .unwrap();
            }
            let fetched = skills::repository::FetchedRepository {
                repository: skills::repository::Repository {
                    kind: Default::default(),
                    name: None,
                    alias: reference.into(),
                    url: format!("https://example.com/sample/{reference}"),
                    branch: "main".into(),
                },
                revision: "0000000000000000000000000000000000000001".into(),
                workdir,
                choices: vec!["printer".into(), "reader".into()],
                invalid: Default::default(),
            };
            app.handle(Msg::Task(
                0,
                Box::new(TaskOutput::RepositoryFetched(reference.into(), Ok(fetched))),
            ));
        }
        assert_eq!(app.pending_task_ui.len(), 2);
        app.handle(key(KeyCode::Enter)); // Invalid preset name: keep editing.
        assert!(matches!(&app.modal, Some(Modal::Input { .. })));
        assert_eq!(app.pending_task_ui.len(), 2);
        app.handle(key(KeyCode::Char('b')));
        let Some(Modal::Input { input, .. }) = &app.modal else {
            panic!("lost input")
        };
        assert_eq!(input.value(), "ab/");
        app.handle(key(KeyCode::Delete));
        app.handle(key(KeyCode::Enter)); // Save; only the first task may appear.
        assert!(app.ws.presets.load("ab").unwrap().is_some());
        let Some(Modal::Repository(picker)) = &app.modal else {
            panic!("expected first result")
        };
        assert_eq!(picker.selection.fetched.repository.alias, "first");
        app.handle(key(KeyCode::Enter)); // Leave candidate search input.
        app.handle(key(KeyCode::Esc));
        let Some(Modal::Repository(picker)) = &app.modal else {
            panic!("expected second result")
        };
        assert_eq!(picker.selection.fetched.repository.alias, "second");
        assert!(!root.join(".downloads/first").exists());
        assert!(root.join(".downloads/second").exists());
        app.handle(key(KeyCode::Enter)); // Leave candidate search input.
        app.handle(key(KeyCode::Esc));
        assert!(app.modal.is_none());
        assert!(app.pending_task_ui.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_inputs_keep_their_text_and_cursor_until_a_successful_retry() {
        let root = std::env::temp_dir().join(format!("skills-input-retry-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        Config {
            agents: vec![],
            ..Config::default()
        }
        .save(&root)
        .unwrap();
        let ws = Workspace::open(&root).unwrap();
        ws.presets
            .save(&skills::preset::Preset {
                name: "reading".into(),
                ..Default::default()
            })
            .unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(ws, tx).unwrap();
        let press =
            |app: &mut App, code| app.handle(Msg::Key(KeyEvent::new(code, KeyModifiers::NONE)));
        let value = |app: &App| match &app.modal {
            Some(Modal::Input { input, .. }) => input.value().to_string(),
            _ => panic!("input must remain open"),
        };
        app.modal = Some(Modal::rename_preset("reading"));
        press(&mut app, KeyCode::End);
        press(&mut app, KeyCode::Char('/'));
        press(&mut app, KeyCode::Left);
        press(&mut app, KeyCode::Enter);
        assert_eq!(value(&app), "reading/");
        press(&mut app, KeyCode::Char('s'));
        assert_eq!(value(&app), "readings/"); // Cursor stayed before the slash.
        press(&mut app, KeyCode::Delete);
        press(&mut app, KeyCode::Enter);
        assert!(app.modal.is_none());
        assert!(app.ws.presets.load("readings").unwrap().is_some());

        app.modal = Some(Modal::new_preset());
        for c in "readings".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        press(&mut app, KeyCode::Enter); // Write closure rejects the duplicate.
        assert_eq!(value(&app), "readings");
        press(&mut app, KeyCode::Char('2'));
        press(&mut app, KeyCode::Enter);
        assert!(app.modal.is_none());
        assert!(app.ws.presets.load("readings2").unwrap().is_some());

        // A metadata write can fail after validation, too.
        app.modal = Some(Modal::rename_preset("readings2"));
        press(&mut app, KeyCode::Backspace);
        press(&mut app, KeyCode::Enter); // Existing destination.
        assert_eq!(value(&app), "readings");
        press(&mut app, KeyCode::Char('3'));
        press(&mut app, KeyCode::Enter);
        assert!(app.modal.is_none());
        assert!(app.ws.presets.load("readings3").unwrap().is_some());
        app.modal = Some(Modal::new_preset());
        press(&mut app, KeyCode::Esc);
        assert!(app.modal.is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn matrix_receives_keys_before_global_shortcuts() {
        let root =
            std::env::temp_dir().join(format!("skills-matrix-routing-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        Config {
            agents: vec![],
            ..Config::default()
        }
        .save(&root)
        .unwrap();
        let ws = Workspace::open(&root).unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(ws, tx).unwrap();
        app.tab = Tab::Agents;
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        assert!(app.on_key(key(KeyCode::Char('M'))).is_empty());
        for code in [
            KeyCode::Tab,
            KeyCode::BackTab,
            KeyCode::Char(':'),
            KeyCode::Char('a'),
        ] {
            assert!(app.on_key(key(code)).is_empty());
            assert!(app.command_palette.is_none());
            assert!(app.context_menu.is_none());
            assert_eq!(app.tab, Tab::Agents);
        }
        for code in [KeyCode::Char('/'), KeyCode::Char('1')] {
            assert!(app.on_key(key(code)).is_empty());
            assert_eq!(app.tab, Tab::Agents);
        }
        assert!(app.on_key(key(KeyCode::Esc)).is_empty());
        assert!(matches!(
            app.on_key(key(KeyCode::Tab)).as_slice(),
            [Action::SwitchTab(Tab::Repos)]
        ));
        assert!(matches!(
            app.on_key(key(KeyCode::BackTab)).as_slice(),
            [Action::SwitchTab(Tab::Presets)]
        ));
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod scope_tests {
    use super::*;

    #[test]
    fn successful_repository_install_offers_deploy_without_opening_targets() {
        let tmp = skills::ops::DownloadDir::new("repository-install-complete").unwrap();
        Config {
            agents: vec![],
            ..Default::default()
        }
        .save(tmp.path())
        .unwrap();
        let (tx, _) = std::sync::mpsc::channel();
        let mut app = App::new(Workspace::open(tmp.path()).unwrap(), tx).unwrap();
        let workdir = tmp.path().join("fetched");
        std::fs::create_dir_all(&workdir).unwrap();
        let selection = super::super::repository_picker::InstallSelection {
            fetched: skills::repository::FetchedRepository {
                repository: skills::repository::Repository {
                    alias: "demo".into(),
                    name: None,
                    kind: Default::default(),
                    url: "https://example.test/demo.git".into(),
                    branch: "main".into(),
                },
                revision: "0000000000000000000000000000000000000001".into(),
                workdir,
                choices: vec!["sample".into()],
                invalid: Default::default(),
            },
            paths: vec!["sample".into()],
            names: Default::default(),
        };

        let actions = app.on_task(TaskOutput::RepositoryInstalled(
            Box::new(selection),
            Ok(vec!["sample".into()]),
        ));

        assert!(
            actions
                .iter()
                .any(|action| matches!(action, Action::LibraryChanged))
        );
        assert!(
            actions
                .iter()
                .any(|action| matches!(action, Action::Search { .. }))
        );
        assert!(actions.iter().any(|action| matches!(
            action,
            Action::OpenModal(modal) if matches!(modal.as_ref(), Modal::InstallComplete { .. })
        )));
        assert!(!actions.iter().any(|action| matches!(
            action,
            Action::OpenModal(modal) if matches!(modal.as_ref(), Modal::DeployTargets(_))
        )));
    }

    #[test]
    fn deployment_scopes_do_not_switch_the_central_library() {
        let base = std::env::temp_dir().join(format!("skills-tui-scope-{}", std::process::id()));
        std::fs::create_dir_all(base.join("root")).unwrap();
        std::fs::create_dir_all(base.join("project/.git")).unwrap();
        skills::config::Config {
            agents: vec![],
            ..Default::default()
        }
        .save(&base.join("root"))
        .unwrap();
        let (tx, _) = std::sync::mpsc::channel();
        let mut app = App::new(Workspace::open(&base.join("root")).unwrap(), tx).unwrap();
        app.agents.discover(&base.join("project")).unwrap();
        app.on_snapshot();
        app.tab = Tab::Agents;
        app.history.record(history::Intent::Install {
            skill: "session-step".into(),
        });
        let root = app.ws.root.clone();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 30)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        app.handle(Msg::Key(KeyEvent::new(
            KeyCode::Char('p'),
            KeyModifiers::CONTROL,
        )));
        assert_eq!(app.ws.root, root);
        assert!(!app.history.is_empty());
        assert!(!base.join("project/.agents").exists());
        std::fs::remove_dir_all(base).unwrap();
    }
}

#[cfg(test)]
mod panel_navigation_tests {
    use super::*;

    #[test]
    fn header_keeps_current_page_and_navigation_visible_at_small_widths() {
        let root = std::env::temp_dir().join(format!("skills-header-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        Config {
            agents: vec![],
            ..Default::default()
        }
        .save(&root)
        .unwrap();
        let (tx, _) = std::sync::mpsc::channel();
        let mut app = App::new(Workspace::open(&root).unwrap(), tx).unwrap();
        for width in [40, 60, 70, 80, 120] {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 1)).unwrap();
            for tab in Tab::visible(true) {
                app.tab = tab;
                terminal.draw(|f| app.draw_header(f, f.area())).unwrap();
                let text: String = terminal
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .map(|c| c.symbol())
                    .collect();
                assert!(text.contains(tab.title()), "{width}: {text}");
                assert!(text.contains("Tab"), "{width}: {text}");
                assert!(app.tab_rects.iter().all(|(rect, _)| rect.right() <= width));
            }
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn tab_changes_pages_from_inputs_and_returning_clears_temporary_library_scope() {
        let root = std::env::temp_dir().join(format!("skills-tab-panels-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        Config {
            agents: vec![],
            ..Default::default()
        }
        .save(&root)
        .unwrap();
        for name in ["alpha", "beta"] {
            std::fs::create_dir_all(root.join(name)).unwrap();
            std::fs::write(
                root.join(name).join("SKILL.md"),
                format!("---\nname: {name}\ndescription: tools\n---\nBody"),
            )
            .unwrap();
        }
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(Workspace::open(&root).unwrap(), tx).unwrap();
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        app.search.focus_input();
        assert!(matches!(
            app.on_key(key(KeyCode::Tab)).as_slice(),
            [Action::SwitchTab(Tab::Tags)]
        ));
        app.apply(Action::SelectSkills {
            keys: vec!["alpha".into()],
            title: "Temporary selection".into(),
            checked: None,
        });
        app.switch_tab(Tab::Tags);
        app.switch_tab(Tab::Search);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 36)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("beta"));
        assert!(text.contains("2/2 local"));
        assert!(!text.contains("Temporary selection"));
        app.switch_tab(Tab::Tags);
        app.on_key(key(KeyCode::Char('/')));
        for c in "tag123/".chars() {
            assert!(app.on_key(key(KeyCode::Char(c))).is_empty());
        }
        assert_eq!(app.tab, Tab::Tags);
        assert!(matches!(
            app.on_key(key(KeyCode::Tab)).as_slice(),
            [Action::SwitchTab(Tab::Presets)]
        ));
        app.modal = Some(Modal::new_preset());
        assert!(app.on_key(key(KeyCode::Tab)).is_empty());
        assert!(app.modal.is_some());
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod escape_hierarchy_tests {
    use super::*;

    #[test]
    fn global_shortcuts_do_not_conflict_with_page_or_text_input_keys() {
        let (_root, mut app) = app();
        for focus in [AppFocus::Tabs, AppFocus::Page] {
            app.focus = focus;
            let query = app.search.query().to_owned();
            for key in ['g', 'o'] {
                let actions = app.on_key(KeyEvent::new(KeyCode::Char(key), KeyModifiers::CONTROL));
                assert!(
                    matches!(actions.as_slice(), [Action::OpenModal(_)]),
                    "{key}"
                );
            }
            for key in ['p', 'b'] {
                assert!(
                    app.on_key(KeyEvent::new(KeyCode::Char(key), KeyModifiers::CONTROL))
                        .is_empty(),
                    "Ctrl-{key} must not leak into page shortcuts"
                );
            }
            assert!(matches!(
                app.on_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL))
                    .as_slice(),
                [Action::Rescan, Action::Toast(_)]
            ));
            for number in 1..=12 {
                assert!(
                    app.on_key(KeyEvent::new(KeyCode::F(number), KeyModifiers::NONE))
                        .is_empty()
                );
            }
            assert_eq!(app.search.query(), query);
        }
        app.on_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE));
        assert_eq!(app.search.query(), "g");
        app.modal = Some(Modal::help());
        assert!(
            app.on_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL))
                .is_empty()
        );
    }

    #[test]
    fn actions_and_command_palette_are_keyboard_discoverable() {
        let (_root, mut app) = app();
        app.focus = AppFocus::Page;
        app.search.focus_list();

        assert!(
            app.on_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE))
                .is_empty()
        );
        assert!(app.context_menu.is_some());
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.context_menu.is_none());

        assert!(
            app.on_key(KeyEvent::new(KeyCode::Char(':'), KeyModifiers::SHIFT))
                .is_empty()
        );
        assert!(app.command_palette.is_some());
        for character in "backup".chars() {
            app.on_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
        }
        let actions = app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(
            actions.as_slice(),
            [
                Action::OpenModal(_),
                Action::RefreshSyncStatus {
                    remote: true,
                    reason: ProbeReason::Manual,
                }
            ]
        ));
        assert!(app.command_palette.is_none());
    }

    #[test]
    fn select_all_reaches_member_panels_without_modified_key_fallthrough() {
        use crate::tui::components::context_menu::Target;
        for tab in [Tab::Search, Tab::Tags, Tab::Presets, Tab::Repos] {
            let (_root, mut app) = app();
            app.switch_tab(tab);
            app.enter_page();
            if tab != Tab::Search {
                app.on_key(KeyCode::Enter.into());
            }
            app.on_key(KeyCode::Char('m').into());
            app.on_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL));
            app.on_key(KeyCode::Char('a').into());
            let menu = app
                .context_menu
                .as_ref()
                .expect("Actions should open on selected skills");
            assert!(
                matches!(&menu.request.target, Target::Batch { all, .. } if !all.is_empty()),
                "{tab:?}"
            );
        }
        let (_root, mut app) = app();
        app.focus = AppFocus::Tabs;
        assert!(
            app.on_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::ALT))
                .is_empty()
        );
        app.enter_page();
        assert!(
            app.on_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL))
                .is_empty()
        );
        assert!(app.modal.is_none());
    }

    #[test]
    fn group_delete_uses_capital_d_while_x_is_reserved_for_members() {
        for tab in [Tab::Tags, Tab::Presets] {
            let (_root, mut app) = app();
            app.apply(Action::SwitchTab(tab));
            key(&mut app, KeyCode::Enter);
            key(&mut app, KeyCode::Char('x'));
            assert!(app.modal.is_none(), "x must not delete the {tab:?} group");
            key(&mut app, KeyCode::Char('D'));
            assert!(
                matches!(app.modal, Some(Modal::ConfirmWrite { .. })),
                "D should confirm deletion of the {tab:?} group"
            );
        }
    }

    #[test]
    fn palette_blocks_background_dialogs_and_clears_on_transitions() {
        let (_root, mut app) = app();
        app.enter_page();
        app.on_key(KeyCode::Char(':').into());
        assert!(app.task_ui_blocked());
        app.pending_task_ui
            .push_back(Action::OpenModal(Box::new(Modal::help())));
        app.handle(Msg::Key(KeyCode::Char('x').into()));
        assert!(app.modal.is_none());
        app.handle(Msg::Key(KeyCode::Esc.into()));
        assert!(matches!(app.modal, Some(Modal::Help { .. })));
        app.modal = None;
        app.on_key(KeyCode::Char(':').into());
        app.handle(Msg::Resize);
        assert!(app.command_palette.is_none());
        app.on_key(KeyCode::Char(':').into());
        app.switch_tab(Tab::Tags);
        assert!(app.command_palette.is_none());
    }

    #[test]
    fn actions_key_remains_text_while_search_is_editing() {
        let (_root, mut app) = app();
        app.focus = AppFocus::Page;
        app.search.focus_input();
        app.on_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        assert_eq!(app.search.query(), "a");
        assert!(app.context_menu.is_none());
    }

    #[test]
    fn mouse_tab_selection_keeps_the_page_active() {
        let (_root, mut app) = app();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 30)).unwrap();
        for tab in [Tab::Agents, Tab::Agents, Tab::Search] {
            app.focus = AppFocus::Tabs;
            terminal.draw(|f| app.draw(f)).unwrap();
            let rect = app.tab_rects.iter().find(|(_, t)| *t == tab).unwrap().0;
            app.handle(Msg::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: rect.x,
                row: rect.y,
                modifiers: KeyModifiers::NONE,
            }));
            assert_eq!(app.tab, tab);
            assert_eq!(app.focus, AppFocus::Page);
        }
    }

    fn app() -> (skills::ops::DownloadDir, App) {
        let root = skills::ops::DownloadDir::new("escape-hierarchy").unwrap();
        Config {
            agents: vec![],
            tags: vec![skills::config::TagConfig {
                name: "sample".into(),
                skills: vec!["sample".into()],
                color: None,
                description: None,
            }],
            ..Default::default()
        }
        .save(root.path())
        .unwrap();
        std::fs::create_dir_all(root.path().join("sample")).unwrap();
        std::fs::write(
            root.path().join("sample/SKILL.md"),
            "---\nname: sample\ndescription: sample\n---\nBody",
        )
        .unwrap();
        let ws = Workspace::open(root.path()).unwrap();
        ws.presets
            .save(&skills::preset::Preset {
                name: "sample".into(),
                skills: vec!["sample".into()],
                ..Default::default()
            })
            .unwrap();
        let (tx, _) = std::sync::mpsc::channel();
        (root, App::new(ws, tx).unwrap())
    }
    fn key(app: &mut App, code: KeyCode) {
        app.handle(Msg::Key(KeyEvent::new(code, KeyModifiers::NONE)));
    }

    #[test]
    fn library_down_from_tabs_enters_search_before_results() {
        let (_root, mut app) = app();
        app.apply(Action::SwitchTab(Tab::Search));
        key(&mut app, KeyCode::Down);
        assert_eq!(app.focus, AppFocus::Page);
        assert!(app.search.input_focused());
        key(&mut app, KeyCode::Down);
        assert!(!app.search.input_focused());
        key(&mut app, KeyCode::Up);
        assert!(app.search.input_focused());
        key(&mut app, KeyCode::Up);
        assert_eq!(app.focus, AppFocus::Tabs);
        assert_eq!(app.tab, Tab::Search);
        assert!(!app.quit);
    }

    #[test]
    fn enter_targets_groups_while_down_targets_split_page_filters() {
        let (_root, mut app) = app();
        for tab in [Tab::Tags, Tab::Presets] {
            app.apply(Action::SwitchTab(tab));
            key(&mut app, KeyCode::Enter);
            assert!(
                !app.text_input_focused(),
                "Enter should target {tab:?} groups"
            );
            key(&mut app, KeyCode::Char('a'));
            assert!(app.context_menu.is_some(), "a should open {tab:?} Actions");
            key(&mut app, KeyCode::Esc);
            key(&mut app, KeyCode::Esc);
            assert_eq!(app.focus, AppFocus::Tabs);
            key(&mut app, KeyCode::Down);
            assert!(
                app.text_input_focused(),
                "Down should target {tab:?} filter"
            );
            key(&mut app, KeyCode::Esc);
            key(&mut app, KeyCode::Up);
        }
    }

    #[test]
    fn split_pages_follow_spatial_search_and_result_navigation() {
        let (_root, mut app) = app();
        let input = |app: &App| match app.tab {
            Tab::Tags => app.tags.input_focused(),
            Tab::Presets => app.presets.input_focused(),
            Tab::Repos => app.repos.input_focused(),
            _ => false,
        };
        for tab in [Tab::Tags, Tab::Presets, Tab::Repos] {
            app.apply(Action::SwitchTab(tab));
            key(&mut app, KeyCode::Down);
            assert!(input(&app), "top left search {tab:?}");
            key(&mut app, KeyCode::Right);
            assert!(input(&app), "right search {tab:?}");
            key(&mut app, KeyCode::Down);
            assert!(!input(&app), "right results {tab:?}");
            key(&mut app, KeyCode::Up);
            assert!(input(&app), "right search from cards {tab:?}");
            key(&mut app, KeyCode::Left);
            assert!(input(&app), "left search {tab:?}");
            key(&mut app, KeyCode::Down);
            assert!(!input(&app), "left results {tab:?}");
            key(&mut app, KeyCode::Right);
            assert!(!input(&app), "right results from left results {tab:?}");
            key(&mut app, KeyCode::Up);
            key(&mut app, KeyCode::Up);
            assert_eq!(app.focus, AppFocus::Tabs, "up through right search {tab:?}");
            assert_eq!(app.tab, tab);
            assert!(!app.quit);
        }
    }

    #[test]
    fn health_down_enters_filter_before_results() {
        let (_root, mut app) = app();
        app.apply(Action::SwitchTab(Tab::Health));
        key(&mut app, KeyCode::Down);
        assert_eq!(app.focus, AppFocus::Page);
        assert!(app.health.input_focused());
        key(&mut app, KeyCode::Down);
        assert!(!app.health.input_focused());
        key(&mut app, KeyCode::Up);
        assert!(app.health.input_focused());
        key(&mut app, KeyCode::Up);
        assert_eq!(app.focus, AppFocus::Tabs);
    }

    #[test]
    fn up_reaches_tabs_from_all_roots_and_nested_skill_panels() {
        let (_root, mut app) = app();
        for tab in Tab::ALL {
            app.apply(Action::SwitchTab(tab));
            key(&mut app, KeyCode::Enter);
            key(&mut app, KeyCode::Home);
            for _ in 0..3 {
                if app.focus == AppFocus::Tabs {
                    break;
                }
                key(&mut app, KeyCode::Up);
            }
            assert_eq!(app.focus, AppFocus::Tabs, "root {tab:?}");
            assert_eq!(app.tab, tab);
            assert!(!app.quit);
        }
        for tab in [Tab::Tags, Tab::Presets, Tab::Repos] {
            app.apply(Action::SwitchTab(tab));
            key(&mut app, KeyCode::Enter);
            key(&mut app, KeyCode::Enter);
            for _ in 0..5 {
                if app.focus == AppFocus::Tabs {
                    break;
                }
                key(&mut app, KeyCode::Up);
            }
            assert_eq!(app.focus, AppFocus::Tabs, "nested {tab:?}");
            assert_eq!(app.tab, tab);
        }
    }

    #[test]
    fn tab_strip_is_a_distinct_level_and_all_roots_return_without_switching() {
        let (_root, mut app) = app();
        assert_eq!(app.focus, AppFocus::Page);
        assert!(app.search.input_focused());
        for tab in Tab::ALL {
            app.apply(Action::SwitchTab(tab));
            assert_eq!(app.focus, AppFocus::Tabs);
            key(&mut app, KeyCode::Enter);
            assert_eq!(app.focus, AppFocus::Page);
            key(&mut app, KeyCode::Esc);
            assert_eq!(app.focus, AppFocus::Tabs, "{tab:?}");
            assert_eq!(app.tab, tab);
            assert!(!app.quit);
        }
        app.settings.tags_enabled = false;
        app.apply(Action::SwitchTab(Tab::Search));
        key(&mut app, KeyCode::Right);
        assert_eq!(app.tab, Tab::Presets);
        assert_eq!(app.focus, AppFocus::Tabs);
        key(&mut app, KeyCode::Char('5'));
        assert_eq!(app.tab, Tab::Health);
        key(&mut app, KeyCode::Char('q'));
        assert!(app.quit);
    }

    #[test]
    fn child_panels_clear_search_then_return_to_left_then_tabs() {
        let (_root, mut app) = app();
        for tab in [Tab::Tags, Tab::Presets, Tab::Repos] {
            app.apply(Action::SwitchTab(tab));
            key(&mut app, KeyCode::Enter); // page root
            key(&mut app, KeyCode::Enter); // child skill panel
            key(&mut app, KeyCode::Char('/'));
            app.handle(Msg::Paste("sample".into()));
            key(&mut app, KeyCode::Esc); // clear input
            assert_eq!(app.focus, AppFocus::Page);
            key(&mut app, KeyCode::Esc); // input -> results
            key(&mut app, KeyCode::Esc); // results -> left
            assert_eq!(app.focus, AppFocus::Page, "{tab:?}");
            key(&mut app, KeyCode::Esc); // left -> tabs
            assert_eq!(app.focus, AppFocus::Tabs, "{tab:?}");
            assert_eq!(app.tab, tab);
        }
    }

    #[test]
    fn library_back_closes_preview_then_multi_then_filter_and_keeps_q_as_text() {
        let (_root, mut app) = app();
        key(&mut app, KeyCode::Char('q'));
        assert_eq!(app.search.query(), "q");
        key(&mut app, KeyCode::Esc);
        assert!(app.search.input_focused());
        key(&mut app, KeyCode::Esc);
        assert!(!app.search.input_focused());
        key(&mut app, KeyCode::Char('/'));
        app.handle(Msg::Paste("sample".into()));
        key(&mut app, KeyCode::Enter);
        key(&mut app, KeyCode::Char('m'));
        key(&mut app, KeyCode::Enter); // preview
        key(&mut app, KeyCode::Esc); // close preview, keep multi and query
        assert_eq!(app.search.query(), "sample");
        key(&mut app, KeyCode::Esc); // cancel multi
        assert_eq!(app.search.query(), "sample");
        key(&mut app, KeyCode::Char('q')); // clear query
        assert!(app.search.query().is_empty());
        assert_eq!(app.focus, AppFocus::Page);
        key(&mut app, KeyCode::Esc); // tabs
        assert_eq!(app.focus, AppFocus::Tabs);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 30)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("enter") && text.contains("quit"));
        let point = (app.body.x + 2, app.body.y + 2);
        app.handle(Msg::Mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            column: point.0,
            row: point.1,
            modifiers: KeyModifiers::NONE,
        }));
        assert_eq!(app.focus, AppFocus::Tabs);
        app.handle(Msg::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: point.0,
            row: point.1,
            modifiers: KeyModifiers::NONE,
        }));
        assert_eq!(app.focus, AppFocus::Page);
    }
}

#[cfg(test)]
mod context_menu_tests {
    use super::*;
    use crate::tui::components::context_menu::{Command, Item, Request, Target};
    use crossterm::event::{KeyModifiers, MouseEventKind};
    use ratatui::{Terminal, backend::TestBackend};
    use skills::{
        Workspace,
        config::{AgentConfig, TagConfig},
        preset::Preset,
        repository::Repository,
    };

    fn draw_app(app: &mut App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    fn interactive_app() -> (skills::ops::DownloadDir, App) {
        let root = skills::ops::DownloadDir::new("context-interaction").unwrap();
        let library = root.path().join("library");
        let agent_dir = root.path().join("agent");
        std::fs::create_dir_all(&library).unwrap();
        Config {
            agents: vec![AgentConfig {
                key: "sample-agent".into(),
                name: "Sample agent".into(),
                skills_dir: agent_dir.display().to_string(),
            }],
            tags: vec![TagConfig {
                name: "work".into(),
                skills: vec!["sample".into()],
                color: None,
                description: None,
            }],
            ..Default::default()
        }
        .save(&library)
        .unwrap();
        std::fs::create_dir_all(library.join("sample")).unwrap();
        std::fs::write(
            library.join("sample/SKILL.md"),
            "---\nname: sample\ndescription: Sample skill\n---\nBody",
        )
        .unwrap();
        std::fs::create_dir_all(library.join("invalid-item")).unwrap();
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::os::unix::fs::symlink(library.join("sample"), agent_dir.join("sample")).unwrap();
        std::os::unix::fs::symlink(library.join("missing-target"), agent_dir.join("broken"))
            .unwrap();

        let ws = Workspace::open(&library).unwrap();
        Repository {
            alias: "demo".into(),
            name: Some("Demo skills".into()),
            kind: skills::meta::SourceKind::Git,
            url: "https://example.test/demo.git".into(),
            branch: "main".into(),
        }
        .save(&ws)
        .unwrap();
        let repo_skill = library.join("repos/demo/repo-skill");
        std::fs::create_dir_all(&repo_skill).unwrap();
        std::fs::write(
            repo_skill.join("SKILL.md"),
            "---\nname: repo-skill\ndescription: Repository skill\n---\nBody",
        )
        .unwrap();
        ws.presets
            .save(&Preset {
                name: "Office".into(),
                skills: vec!["sample".into()],
                ..Preset::default()
            })
            .unwrap();
        let (tx, _) = std::sync::mpsc::channel();
        let launch_directory = library;
        let mut app = App::new_with_launch_directory(ws, tx, Some(&launch_directory)).unwrap();
        app.ws.inventory_products = Some(std::collections::BTreeSet::from(["sample-agent".into()]));
        let ctx = Ctx {
            ws: &app.ws,
            snap: &app.snap,
            settings: &app.settings,
        };
        app.agents.refresh(&ctx);
        (root, app)
    }

    fn open_first_menu(app: &mut App, width: u16, height: u16) -> (u16, u16) {
        draw_app(app, width, height);
        let body = Rect::new(0, 1, width, height.saturating_sub(2));
        for row in body.y..body.bottom() {
            for column in body.x..body.right() {
                app.handle(Msg::Mouse(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Right),
                    column,
                    row,
                    modifiers: KeyModifiers::NONE,
                }));
                if app.context_menu.is_some() {
                    return (column, row);
                }
            }
        }
        panic!("no context-menu target rendered for {:?}", app.tab);
    }

    fn outside_menu(app: &App, width: u16, height: u16) -> (u16, u16) {
        let area = app.context_menu.as_ref().unwrap().menu_area();
        [
            (0, 0),
            (width - 1, 0),
            (0, height - 1),
            (width - 1, height - 1),
        ]
        .into_iter()
        .find(|(x, y)| !area.contains((*x, *y).into()))
        .unwrap_or((0, 0))
    }

    fn footer_line(app: &mut App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        (0..width)
            .map(|x| terminal.backend().buffer()[(x, height - 1)].symbol())
            .collect()
    }

    fn rendered_footer_hints(app: &mut App, width: u16, height: u16) -> Vec<(String, String)> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        let buffer = terminal.backend().buffer();
        let key_style = app.settings.theme.key_hint();
        let is_key = |x: u16| {
            let cell = &buffer[(x, height - 1)];
            cell.fg == key_style.fg.unwrap_or(ratatui::style::Color::Reset)
                && cell.modifier.contains(key_style.add_modifier)
        };
        let mut result = Vec::new();
        let mut x = 0;
        while x < width {
            if !is_key(x) {
                x += 1;
                continue;
            }
            let key_start = x;
            while x < width && is_key(x) {
                x += 1;
            }
            let key: String = (key_start..x)
                .map(|column| buffer[(column, height - 1)].symbol())
                .filter(|symbol| !symbol.is_empty())
                .collect();
            let desc_start = x;
            while x < width && !is_key(x) {
                x += 1;
            }
            let desc: String = (desc_start..x)
                .map(|column| buffer[(column, height - 1)].symbol())
                .collect::<String>()
                .trim()
                .to_owned();
            result.push((key, desc));
        }
        result
    }

    fn expected_footer_hints(app: &App) -> Vec<(String, String)> {
        expected_rendered_hints(app, app.footer_hints())
    }

    fn expected_rendered_hints(app: &App, hints: Hints) -> Vec<(String, String)> {
        let escape = hints.iter().find(|(key, _)| key.contains("Esc"));
        let show_globals = app.global_hints_visible();
        let globals = [
            app.actions_available().then_some(("a", "actions")),
            Some((":", "commands")),
            Some(("?", "help")),
        ];
        let mut expected = Vec::new();
        for (key, desc) in hints {
            if (!app.settings.tags_enabled && *key == "t")
                || key.contains("Esc")
                || (show_globals && globals.contains(&Some((*key, *desc))))
            {
                continue;
            }
            expected.push(((*key).to_owned(), (*desc).to_owned()));
        }
        if let Some((key, desc)) = escape {
            expected.push(((*key).to_owned(), (*desc).to_owned()));
        }
        if show_globals {
            expected.extend(
                globals
                    .into_iter()
                    .flatten()
                    .map(|(key, desc)| (key.into(), desc.into())),
            );
        }
        expected
    }

    fn assert_footer_exact(app: &mut App, label: &str, hints: Hints) {
        let footer = footer_line(app, 400, 40);
        assert_eq!(
            app.footer_hints(),
            hints,
            "{label}: active footer hints do not belong to the expected focus"
        );
        let rendered = rendered_footer_hints(app, 400, 40);
        let expected = expected_rendered_hints(app, hints);
        assert_eq!(
            rendered, expected,
            "{label}: rendered footer hints differ; rendered={rendered:?}, expected={expected:?}, line={footer:?}"
        );
    }

    fn assert_footer_matches_focus(app: &mut App, label: &str) {
        // A wide test surface keeps every applicable hint visible. The footer
        // still goes through the production budget and ordering code.
        let footer = footer_line(app, 400, 40);
        assert!(
            app.modal.is_none(),
            "modal footer must be checked by the modal-specific test: {label}"
        );
        let expected = expected_footer_hints(app);
        let rendered = rendered_footer_hints(app, 400, 40);
        assert_eq!(
            rendered, expected,
            "{label}: footer hints do not match the focused panel; rendered={rendered:?}, expected={expected:?}, line={footer:?}"
        );
    }

    fn key(app: &mut App, code: KeyCode) {
        app.handle(Msg::Key(KeyEvent::new(code, KeyModifiers::NONE)));
    }

    fn left_click(app: &mut App, column: u16, row: u16) {
        app.handle(Msg::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }));
    }

    fn add_footer_fixture_variants(root: &skills::ops::DownloadDir, app: &mut App) {
        let library = app.ws.root.clone();
        let agent = root.path().join("agent");
        // A malformed root skill gives Health a root issue row. The remaining
        // entries exercise all agent repair states without touching any user
        // path: every directory belongs to this DownloadDir fixture.
        std::fs::create_dir_all(library.join("invalid-root")).unwrap();
        std::fs::write(library.join("invalid-root/SKILL.md"), "not frontmatter").unwrap();
        std::fs::create_dir_all(library.join("shadow")).unwrap();
        std::fs::write(
            library.join("shadow/SKILL.md"),
            "---\nname: shadow\ndescription: shadow\n---\nBody",
        )
        .unwrap();
        std::fs::create_dir_all(library.join("shadow-diff")).unwrap();
        std::fs::write(
            library.join("shadow-diff/SKILL.md"),
            "---\nname: shadow-diff\ndescription: shadow\n---\nRoot",
        )
        .unwrap();
        std::fs::create_dir_all(agent.join("shadow")).unwrap();
        std::fs::write(
            agent.join("shadow/SKILL.md"),
            "---\nname: shadow\ndescription: shadow\n---\nBody",
        )
        .unwrap();
        std::fs::create_dir_all(agent.join("shadow-diff")).unwrap();
        std::fs::write(
            agent.join("shadow-diff/SKILL.md"),
            "---\nname: shadow-diff\ndescription: shadow\n---\nAgent copy",
        )
        .unwrap();
        std::fs::create_dir_all(agent.join("agent-only")).unwrap();
        std::fs::write(
            agent.join("agent-only/SKILL.md"),
            "---\nname: agent-only\ndescription: only agent\n---\nBody",
        )
        .unwrap();
        let foreign = root.path().join("foreign-target");
        std::fs::create_dir_all(&foreign).unwrap();
        std::fs::write(
            foreign.join("SKILL.md"),
            "---\nname: foreign\ndescription: foreign\n---\nBody",
        )
        .unwrap();
        std::os::unix::fs::symlink(&foreign, agent.join("foreign")).unwrap();

        app.ws.config.tags.push(TagConfig {
            name: "personal".into(),
            skills: vec!["sample".into()],
            color: None,
            description: None,
        });
        app.snap = skills::reconcile::scan(&app.ws.root, &app.ws.config).unwrap();
        app.on_snapshot();
    }

    #[test]
    fn footer_shortcuts_follow_focus_across_all_pages_on_isolated_fixture() {
        let (root, mut app) = interactive_app();
        add_footer_fixture_variants(&root, &mut app);

        // Library: input, result list, preview and multi-select each own a
        // distinct footer. Esc then walks back through the same hierarchy.
        app.apply(Action::SwitchTab(Tab::Search));
        app.enter_page();
        assert_footer_matches_focus(&mut app, "search list");
        key(&mut app, KeyCode::Char('/'));
        assert_footer_matches_focus(&mut app, "search input");
        // A completion popup is a third input surface: it keeps input focus,
        // but replaces the normal input hints until it is accepted or closed.
        for c in "status:loc".chars() {
            key(&mut app, KeyCode::Char(c));
        }
        assert_footer_exact(
            &mut app,
            "search completion popup",
            &[
                ("↑↓", "suggestions"),
                ("Enter", "complete"),
                ("Esc", "close suggestions"),
            ],
        );
        key(&mut app, KeyCode::Down);
        assert_footer_exact(
            &mut app,
            "search completion after moving suggestion",
            &[
                ("↑↓", "suggestions"),
                ("Enter", "complete"),
                ("Esc", "close suggestions"),
            ],
        );
        key(&mut app, KeyCode::Esc);
        assert_footer_exact(
            &mut app,
            "search input after closing completion",
            &[
                ("↑↓", "select"),
                ("↓", "list"),
                ("Enter", "list"),
                ("Esc", "clear/results"),
            ],
        );
        key(&mut app, KeyCode::Esc);
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::Down);
        assert_footer_matches_focus(&mut app, "search list after input");
        key(&mut app, KeyCode::Home);
        key(&mut app, KeyCode::Enter);
        let preview_footer = footer_line(&mut app, 400, 40);
        assert!(
            preview_footer.contains("scroll"),
            "search preview: {preview_footer:?}"
        );
        assert_footer_matches_focus(&mut app, "search preview");
        key(&mut app, KeyCode::Esc);
        key(&mut app, KeyCode::Char('m'));
        assert_footer_matches_focus(&mut app, "search multi list");
        key(&mut app, KeyCode::Char('/'));
        assert_footer_matches_focus(&mut app, "search multi input");
        key(&mut app, KeyCode::Esc);
        assert_footer_matches_focus(&mut app, "search multi list after input");
        key(&mut app, KeyCode::Esc);
        assert_footer_matches_focus(&mut app, "search list after cancelling multi");
        key(&mut app, KeyCode::Esc);
        assert_eq!(app.focus, AppFocus::Tabs);
        assert_footer_matches_focus(&mut app, "tab strip after search");

        // Tags: the left filter, member panel, member preview and tag prompts.
        app.apply(Action::SwitchTab(Tab::Tags));
        app.enter_page();
        assert_footer_matches_focus(&mut app, "tags root");
        key(&mut app, KeyCode::Char('/'));
        assert_footer_matches_focus(&mut app, "tags filter");
        key(&mut app, KeyCode::Esc);
        key(&mut app, KeyCode::Enter);
        assert_footer_matches_focus(&mut app, "tags members");
        key(&mut app, KeyCode::Char('/'));
        assert_footer_matches_focus(&mut app, "tags member filter");
        key(&mut app, KeyCode::Down);
        assert_footer_matches_focus(&mut app, "tags member list");
        key(&mut app, KeyCode::Enter);
        assert_footer_matches_focus(&mut app, "tags member preview");
        key(&mut app, KeyCode::Esc);
        key(&mut app, KeyCode::Esc);
        assert_footer_matches_focus(&mut app, "tags root after members");
        key(&mut app, KeyCode::Char('m'));
        assert_footer_matches_focus(&mut app, "tags merge prompt");
        key(&mut app, KeyCode::Esc);
        key(&mut app, KeyCode::Char('C'));
        assert_footer_matches_focus(&mut app, "tags colour prompt");
        key(&mut app, KeyCode::Esc);
        assert_footer_matches_focus(&mut app, "tags root after prompts");

        // Presets: colour, matrix and the embedded member panel.
        app.apply(Action::SwitchTab(Tab::Presets));
        app.enter_page();
        assert_footer_matches_focus(&mut app, "presets root");
        key(&mut app, KeyCode::Char('C'));
        assert_footer_matches_focus(&mut app, "presets colour prompt");
        key(&mut app, KeyCode::Esc);
        key(&mut app, KeyCode::Char('M'));
        assert_footer_matches_focus(&mut app, "presets matrix");
        key(&mut app, KeyCode::Esc);
        key(&mut app, KeyCode::Enter);
        assert_footer_matches_focus(&mut app, "preset members");
        key(&mut app, KeyCode::Char('/'));
        assert_footer_matches_focus(&mut app, "preset member filter");
        key(&mut app, KeyCode::Down);
        assert_footer_matches_focus(&mut app, "preset member list");
        key(&mut app, KeyCode::Enter);
        assert_footer_matches_focus(&mut app, "preset member preview");
        key(&mut app, KeyCode::Esc);
        key(&mut app, KeyCode::Esc);
        assert_footer_matches_focus(&mut app, "presets root after members");

        // Repositories: source filter and the shared skill panel.
        app.apply(Action::SwitchTab(Tab::Repos));
        app.enter_page();
        assert_footer_matches_focus(&mut app, "repos root");
        key(&mut app, KeyCode::Char('/'));
        assert_footer_matches_focus(&mut app, "repos filter");
        key(&mut app, KeyCode::Esc);
        key(&mut app, KeyCode::Enter);
        assert_footer_matches_focus(&mut app, "repo skills");
        key(&mut app, KeyCode::Char('/'));
        assert_footer_matches_focus(&mut app, "repo skill filter");
        key(&mut app, KeyCode::Down);
        assert_footer_matches_focus(&mut app, "repo skill list");
        key(&mut app, KeyCode::Enter);
        assert_footer_matches_focus(&mut app, "repo skill preview");
        key(&mut app, KeyCode::Esc);
        key(&mut app, KeyCode::Esc);
        assert_footer_matches_focus(&mut app, "repos root after skills");

        // Agents: selector, scopes, quick groups, group filters, skill search,
        // repair-specific rows and preview. The fixture contains linked,
        // broken, shadow, foreign and agent-only entries.
        app.apply(Action::SwitchTab(Tab::Agents));
        app.enter_page();
        assert_footer_matches_focus(&mut app, "agents selector");
        key(&mut app, KeyCode::Down);
        assert_footer_matches_focus(&mut app, "agents scopes");
        key(&mut app, KeyCode::Down);
        assert_footer_matches_focus(&mut app, "agents groups");
        key(&mut app, KeyCode::Char('/'));
        assert_footer_matches_focus(&mut app, "agents skill filter");
        key(&mut app, KeyCode::Esc);
        key(&mut app, KeyCode::Up);
        assert_footer_matches_focus(&mut app, "agents group filters");
        key(&mut app, KeyCode::Down);
        assert_footer_matches_focus(&mut app, "agents linked entry");
        key(&mut app, KeyCode::Down);
        assert_footer_matches_focus(&mut app, "agents broken entry");
        key(&mut app, KeyCode::Enter);
        assert!(
            app.modal.is_none(),
            "opening an entry should use the inline preview"
        );
        assert_footer_matches_focus(&mut app, "agents broken preview");
        key(&mut app, KeyCode::Esc);
        key(&mut app, KeyCode::Char('['));
        assert_footer_matches_focus(&mut app, "agents after scope switch");

        // Health: filtering, the first issue row, and returning to the tabs.
        app.apply(Action::SwitchTab(Tab::Health));
        app.enter_page();
        assert_footer_matches_focus(&mut app, "health issue");
        key(&mut app, KeyCode::Char('/'));
        assert_footer_matches_focus(&mut app, "health filter");
        key(&mut app, KeyCode::Esc);
        assert_footer_matches_focus(&mut app, "health issue after filter");
        key(&mut app, KeyCode::Esc);
        assert_eq!(app.focus, AppFocus::Tabs);
        assert_footer_matches_focus(&mut app, "tab strip after health");
    }

    #[test]
    fn footer_shortcuts_follow_mouse_focus_and_wheel_scrolling() {
        let (root, mut app) = interactive_app();
        add_footer_fixture_variants(&root, &mut app);
        let width = 400;
        let height = 40;

        // Search input/list and the list scrollbar are separate mouse targets.
        app.apply(Action::SwitchTab(Tab::Search));
        app.enter_page();
        draw_app(&mut app, width, height);
        let body = app.body;
        left_click(&mut app, body.x + 2, body.y + 1);
        assert!(app.search.input_focused());
        assert_footer_matches_focus(&mut app, "mouse search input");
        app.handle(Msg::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: body.x + 2,
            row: body.y + 6,
            modifiers: KeyModifiers::NONE,
        }));
        assert!(!app.search.input_focused());
        assert_footer_matches_focus(&mut app, "mouse search list after input wheel");
        left_click(&mut app, body.x + 2, body.y + 6);
        assert!(!app.search.input_focused());
        assert_footer_matches_focus(&mut app, "mouse search list");
        app.handle(Msg::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: body.x + 2,
            row: body.y + 6,
            modifiers: KeyModifiers::NONE,
        }));
        assert_footer_matches_focus(&mut app, "mouse search list after wheel");

        // The split pages move focus to the right panel when its cards are
        // clicked; the same click also keeps the panel's footer active.
        for (tab, label) in [
            (Tab::Tags, "tags"),
            (Tab::Presets, "presets"),
            (Tab::Repos, "repos"),
        ] {
            app.apply(Action::SwitchTab(tab));
            app.enter_page();
            draw_app(&mut app, width, height);
            let body = app.body;
            left_click(&mut app, body.x + 2, body.y + 1);
            assert!(match tab {
                Tab::Tags => app.tags.input_focused(),
                Tab::Presets => app.presets.input_focused(),
                Tab::Repos => app.repos.input_focused(),
                _ => unreachable!(),
            });
            assert_footer_matches_focus(&mut app, &format!("mouse {label} filter"));
            // A wheel event transfers focus just like a click. This catches
            // stale parent focus when entering the embedded skill panel.
            app.handle(Msg::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: body.x + 220,
                row: body.y + 10,
                modifiers: KeyModifiers::NONE,
            }));
            assert!(!match tab {
                Tab::Tags => app.tags.input_focused(),
                Tab::Presets => app.presets.input_focused(),
                Tab::Repos => app.repos.input_focused(),
                _ => unreachable!(),
            });
            assert_footer_matches_focus(
                &mut app,
                &format!("mouse {label} skill wheel from filter"),
            );
            left_click(&mut app, body.x + 2, body.y + 6);
            assert!(!match tab {
                Tab::Tags => app.tags.input_focused(),
                Tab::Presets => app.presets.input_focused(),
                Tab::Repos => app.repos.input_focused(),
                _ => unreachable!(),
            });
            left_click(&mut app, body.x + 220, body.y + 6);
            assert_footer_matches_focus(&mut app, &format!("mouse {label} skill panel"));
            left_click(&mut app, body.x + 2, body.y + 1);
            app.handle(Msg::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: body.x + 2,
                row: body.y + 6,
                modifiers: KeyModifiers::NONE,
            }));
            assert!(!match tab {
                Tab::Tags => app.tags.input_focused(),
                Tab::Presets => app.presets.input_focused(),
                Tab::Repos => app.repos.input_focused(),
                _ => unreachable!(),
            });
            assert_footer_matches_focus(&mut app, &format!("mouse {label} skill wheel"));
            if tab == Tab::Repos {
                // Repositories have a scrollable details band above skills;
                // it owns the source footer when selected by the mouse.
                left_click(&mut app, body.x + 220, body.y + 2);
                assert_footer_matches_focus(&mut app, "mouse repos details");
            }
        }

        // Agents has stacked selector/scope/group/entry bands. Mouse clicks
        // land on the same owner that keyboard navigation reaches.
        app.apply(Action::SwitchTab(Tab::Agents));
        app.enter_page();
        draw_app(&mut app, width, height);
        let body = app.body;
        left_click(&mut app, body.x + 2, body.y + 1);
        assert_footer_matches_focus(&mut app, "mouse agents selector");
        left_click(&mut app, body.x + 2, body.y + 7);
        assert_footer_matches_focus(&mut app, "mouse agents scope");
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::Down);
        draw_app(&mut app, width, height);
        let body = app.body;
        left_click(&mut app, body.x + 220, body.bottom().saturating_sub(3));
        assert_footer_matches_focus(&mut app, "mouse agents entries");
        app.handle(Msg::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: body.x + 220,
            row: body.bottom().saturating_sub(3),
            modifiers: KeyModifiers::NONE,
        }));
        assert_footer_matches_focus(&mut app, "mouse agents entries wheel");

        // Health's issue list and filter are independent focus owners.
        app.apply(Action::SwitchTab(Tab::Health));
        app.enter_page();
        draw_app(&mut app, width, height);
        let body = app.body;
        left_click(&mut app, body.x + 2, body.y + 1);
        assert!(app.health.input_focused());
        assert_footer_matches_focus(&mut app, "mouse health filter");
        app.handle(Msg::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: body.right().saturating_sub(2),
            row: body.y + 6,
            modifiers: KeyModifiers::NONE,
        }));
        assert!(!app.health.input_focused());
        assert_footer_matches_focus(&mut app, "mouse health detail wheel from filter");
        left_click(&mut app, body.x + 2, body.y + 1);
        assert!(app.health.input_focused());
        for row in body.y..body.bottom() {
            left_click(&mut app, body.x + 2, row);
            if !app.health.input_focused() {
                break;
            }
        }
        assert!(!app.health.input_focused());
        assert_footer_matches_focus(&mut app, "mouse health issues");
        app.handle(Msg::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: body.x + 2,
            row: body.bottom().saturating_sub(1),
            modifiers: KeyModifiers::NONE,
        }));
        assert_footer_matches_focus(&mut app, "mouse health issues wheel");
    }

    #[test]
    fn footer_hides_tag_action_and_remains_safe_in_narrow_layouts() {
        let (_root, mut app) = interactive_app();
        // This is a session-only setting: the fixture config on disk remains
        // unchanged while the footer exercises the reduced tab set.
        app.settings.tags_enabled = false;
        app.apply(Action::SwitchTab(Tab::Search));
        app.enter_page();
        key(&mut app, KeyCode::Char('m'));
        let footer = footer_line(&mut app, 40, 12);
        assert!(!footer.contains("t tags"), "hidden tags action: {footer:?}");

        for tab in [
            Tab::Search,
            Tab::Presets,
            Tab::Agents,
            Tab::Repos,
            Tab::Health,
        ] {
            app.apply(Action::SwitchTab(tab));
            app.enter_page();
            let footer = footer_line(&mut app, 40, 12);
            assert_eq!(footer.chars().count(), 40, "narrow footer {tab:?}");
        }
    }

    #[test]
    fn footer_switches_to_modal_and_quit_popup_controls_without_page_hints() {
        let (_root, mut app) = interactive_app();

        app.modal = Some(Modal::help());
        assert_footer_exact(
            &mut app,
            "help modal",
            &[("↑↓", "scroll"), ("PgUp/PgDn", "page"), ("Esc", "close")],
        );

        app.modal = Some(Modal::message("status", vec!["line".into()]));
        assert_footer_exact(
            &mut app,
            "message modal",
            &[("↑↓", "scroll"), ("PgUp/PgDn", "page"), ("Esc", "close")],
        );

        app.modal = Some(Modal::Message {
            title: "status".into(),
            lines: vec!["line".into()],
            scroll: 0,
            return_to: Some(Box::new(Modal::help())),
        });
        assert_footer_exact(
            &mut app,
            "message modal returning to selection",
            &[
                ("↑↓", "scroll"),
                ("PgUp/PgDn", "page"),
                ("Esc", "back to selection"),
            ],
        );

        app.modal = Some(Modal::new_preset());
        assert_footer_exact(
            &mut app,
            "input save modal",
            &[("Enter", "save"), ("Esc", "cancel")],
        );
        app.modal = Some(Modal::install());
        assert_footer_exact(
            &mut app,
            "input install modal",
            &[("Enter", "discover"), ("Esc", "cancel")],
        );

        let link = skills::ops::deploy::Action::Link {
            agent: "sample-agent".into(),
            skill: "sample".into(),
            path: app.ws.root.join("agent/sample"),
            target: app.ws.root.join("sample"),
        };
        app.modal = Some(Modal::confirm("confirm".into(), vec![link]));
        assert_footer_exact(
            &mut app,
            "confirm apply focused",
            &[("Enter/y", "apply"), ("Esc/n", "cancel"), ("←→", "buttons")],
        );
        key(&mut app, KeyCode::Right);
        assert_footer_exact(
            &mut app,
            "confirm cancel focused",
            &[("Enter/Esc/n", "cancel"), ("y", "apply"), ("←→", "buttons")],
        );

        app.modal = Some(Modal::confirm_write(
            "write".into(),
            vec!["write".into()],
            Box::new(|_| Ok("done".into())),
        ));
        assert_footer_exact(
            &mut app,
            "destructive write cancel focused",
            &[("Enter/Esc/n", "cancel"), ("y", "apply"), ("←→", "buttons")],
        );
        key(&mut app, KeyCode::Left);
        assert_footer_exact(
            &mut app,
            "destructive write apply focused",
            &[("Enter/y", "apply"), ("Esc/n", "cancel"), ("←→", "buttons")],
        );

        let ctx = Ctx {
            ws: &app.ws,
            snap: &app.snap,
            settings: &app.settings,
        };
        app.modal = Some(Modal::repositories(&ctx));
        assert_footer_exact(
            &mut app,
            "repository picker input",
            &[
                ("type", "filter"),
                ("↓", "list"),
                ("Enter", "browse"),
                ("Esc", "close"),
            ],
        );
        key(&mut app, KeyCode::Down);
        assert_footer_exact(
            &mut app,
            "repository picker list",
            &[
                ("Enter", "browse"),
                ("u", "check repo"),
                ("U", "update repo"),
                ("↑↓", "move"),
                ("/", "filter"),
                ("Esc", "close"),
            ],
        );
        key(&mut app, KeyCode::Char('/'));
        assert_footer_exact(
            &mut app,
            "repository picker input after list",
            &[
                ("type", "filter"),
                ("↓", "list"),
                ("Enter", "browse"),
                ("Esc", "close"),
            ],
        );

        let ctx = Ctx {
            ws: &app.ws,
            snap: &app.snap,
            settings: &app.settings,
        };
        app.modal = Some(Modal::preset_members("Office", &ctx));
        assert_footer_exact(
            &mut app,
            "preset skill picker list",
            &[
                ("Enter/Space", "toggle"),
                ("Ctrl+A", "select all results"),
                ("/", "filter"),
                ("o", "preview"),
                ("Tab", "apply / cancel"),
                ("Esc", "cancel"),
            ],
        );
        key(&mut app, KeyCode::Tab);
        assert_footer_exact(
            &mut app,
            "preset skill picker apply button",
            &[
                ("←→", "buttons"),
                ("Tab/Shift+Tab", "next / previous"),
                ("↑", "list"),
                ("Enter/Space", "activate"),
                ("Esc/q", "cancel"),
            ],
        );
        key(&mut app, KeyCode::Tab);
        assert_footer_exact(
            &mut app,
            "preset skill picker cancel button",
            &[
                ("←→", "buttons"),
                ("Tab/Shift+Tab", "next / previous"),
                ("↑", "list"),
                ("Enter/Space", "activate"),
                ("Esc/q", "cancel"),
            ],
        );
        key(&mut app, KeyCode::Up);
        assert_footer_exact(
            &mut app,
            "preset skill picker list after button",
            &[
                ("Enter/Space", "toggle"),
                ("Ctrl+A", "select all results"),
                ("/", "filter"),
                ("o", "preview"),
                ("Tab", "apply / cancel"),
                ("Esc", "cancel"),
            ],
        );
        let ctx = Ctx {
            ws: &app.ws,
            snap: &app.snap,
            settings: &app.settings,
        };
        app.modal = Some(Modal::batch_deploy(vec!["sample".into()], &ctx));
        assert_footer_exact(
            &mut app,
            "deployment target modal",
            &[
                ("Tab/Shift+Tab", "list / buttons"),
                ("Enter/Space", "select / activate"),
                ("↑↓", "move"),
                ("p", "project path"),
                ("Esc", "cancel"),
            ],
        );
        let ctx = Ctx {
            ws: &app.ws,
            snap: &app.snap,
            settings: &app.settings,
        };
        app.modal = Some(Modal::batch_tags(vec!["sample".into()], &ctx));
        assert_footer_exact(
            &mut app,
            "batch tag modal",
            &[
                ("Enter", "add / create"),
                ("↑↓", "choose"),
                ("Tab", "complete"),
                ("Backspace", "select last · again removes"),
                ("Esc", "done"),
            ],
        );

        app.batch_running = true;
        app.batch_modal_owned = true;
        assert_footer_exact(&mut app, "busy batch modal", &[("…", "saving changes")]);
        app.batch_running = false;
        app.batch_modal_owned = false;

        let pending = skills::ops::name_choices::Pending {
            changes: vec![],
            groups: vec![skills::ops::name_choices::Group {
                name: "sample".into(),
                directory: app.ws.root.join("agent"),
                candidates: vec![skills::ops::name_choices::Candidate {
                    key: Some("sample".into()),
                    path: app.ws.root.join("agent/sample"),
                    archive_hash: None,
                }],
            }],
        };
        app.modal = Some(Modal::DeploymentChoices(Box::new(
            crate::tui::name_choices::NameChoices::new(pending),
        )));
        assert_footer_exact(
            &mut app,
            "name conflict modal",
            &[
                ("↑↓", "option"),
                ("Enter/Space", "choose"),
                ("←→", "conflict"),
                ("Tab/Shift+Tab", "list / buttons"),
                ("Esc", "cancel"),
            ],
        );

        app.modal = None;
        app.quit_prompt = Some(QuitPrompt::default());
        assert_footer_exact(
            &mut app,
            "quit confirmation popup",
            &[("←→", "buttons"), ("Enter", "confirm"), ("Esc/q", "cancel")],
        );
        let footer = footer_line(&mut app, 400, 40);
        assert!(
            !footer.contains("Ctrl-G help"),
            "quit popup must not expose help: {footer:?}"
        );
        assert!(
            footer.contains("←→ buttons") && footer.contains("Enter confirm"),
            "quit popup must expose only its controls: {footer:?}"
        );
    }

    #[test]
    fn context_menu_owns_popup_footer_and_hides_global_page_shortcuts() {
        let (_root, mut app) = interactive_app();
        app.apply(Action::SwitchTab(Tab::Search));
        app.enter_page();
        open_first_menu(&mut app, 140, 35);
        let global_footer = footer_line(&mut app, 140, 35);
        assert_eq!(
            global_footer.trim(),
            "",
            "context menu must clear the global page footer"
        );
        assert!(!global_footer.contains("Ctrl-G") && !global_footer.contains("preview"));

        draw_app(&mut app, 140, 35);
        let request = app.context_menu.as_ref().unwrap().request.clone();
        let Some(disabled) = request.items.iter().find(|item| item.disabled.is_some()) else {
            panic!("fixture should expose a disabled context-menu action");
        };
        let disabled_rect = app
            .context_menu
            .as_ref()
            .unwrap()
            .hit_rect_for(disabled.command)
            .unwrap();
        app.handle(Msg::Mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            column: disabled_rect.x,
            row: disabled_rect.y,
            modifiers: KeyModifiers::NONE,
        }));
        let disabled_footer = draw_app(&mut app, 140, 35);
        assert!(
            disabled_footer.contains(disabled.disabled.as_deref().unwrap()),
            "disabled item must explain why it cannot run"
        );
        assert!(!disabled_footer.contains("↑↓ select"));

        let Some(enabled) = request.items.iter().find(|item| item.disabled.is_none()) else {
            panic!("fixture should expose an enabled context-menu action");
        };
        let enabled_rect = app
            .context_menu
            .as_ref()
            .unwrap()
            .hit_rect_for(enabled.command)
            .unwrap();
        app.handle(Msg::Mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            column: enabled_rect.x,
            row: enabled_rect.y,
            modifiers: KeyModifiers::NONE,
        }));
        let enabled_footer = draw_app(&mut app, 140, 35);
        assert!(enabled_footer.contains("Click or shown keys run"));
        assert!(!enabled_footer.contains("Ctrl-G help"));
    }

    #[test]
    fn matrix_and_hidden_group_popups_replace_the_agents_footer() {
        let (_root, mut app) = interactive_app();
        app.apply(Action::SwitchTab(Tab::Agents));
        app.enter_page();
        key(&mut app, KeyCode::Char('M'));
        assert_footer_exact(
            &mut app,
            "agents matrix popup",
            &[
                ("↑↓←→", "cell"),
                ("Enter/Space", "toggle"),
                ("A", "toggle row"),
                ("Esc", "close"),
            ],
        );
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::Right);
        assert_footer_exact(
            &mut app,
            "agents matrix after moving cell",
            &[
                ("↑↓←→", "cell"),
                ("Enter/Space", "toggle"),
                ("A", "toggle row"),
                ("Esc", "close"),
            ],
        );
        key(&mut app, KeyCode::Esc);

        // Add enough in-memory groups to force the compact group strip to use
        // a hidden `+N tags` hit target. These tags live only in the temporary
        // DownloadDir fixture and are never written to the user's config.
        for i in 0..18 {
            app.ws.config.tags.push(TagConfig {
                name: format!("extra-{i}"),
                skills: vec!["sample".into()],
                color: None,
                description: None,
            });
        }
        app.snap = skills::reconcile::scan(&app.ws.root, &app.ws.config).unwrap();
        app.on_snapshot();
        app.enter_page();
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::Down);
        let width: u16 = 60;
        let height: u16 = 24;
        let body = Rect::new(0, 1, width, height.saturating_sub(2));
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        let Some((column, row)) = (body.y..body.bottom()).find_map(|y| {
            let line: String = (body.x..body.right())
                .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                .collect();
            if !line.contains('+') || !line.contains("tags") {
                return None;
            }
            (body.x..body.right()).find_map(|x| {
                (terminal.backend().buffer()[(x, y)].symbol() == "+").then_some((x, y))
            })
        }) else {
            panic!("narrow agent fixture should render a hidden group hit");
        };
        left_click(&mut app, column, row);
        assert!(
            app.agents.group_popup_open(),
            "hidden group should open popup"
        );
        assert_footer_exact(
            &mut app,
            "hidden group popup",
            &[
                ("↑↓←→", "group"),
                ("Enter", "install/uninstall / expand"),
                ("Esc/q", "back"),
            ],
        );
        key(&mut app, KeyCode::Down);
        assert_footer_exact(
            &mut app,
            "hidden group popup after selection",
            &[
                ("↑↓←→", "group"),
                ("Enter", "install/uninstall / expand"),
                ("Esc/q", "back"),
            ],
        );
        key(&mut app, KeyCode::Esc);
        assert!(!app.agents.group_popup_open());
    }

    #[test]
    fn agents_and_health_repair_footers_follow_every_fixture_state() {
        let (root, mut app) = interactive_app();
        add_footer_fixture_variants(&root, &mut app);

        app.apply(Action::SwitchTab(Tab::Agents));
        app.enter_page();
        key(&mut app, KeyCode::Down); // scopes
        key(&mut app, KeyCode::Down); // groups
        key(&mut app, KeyCode::Char('/')); // skill filter
        key(&mut app, KeyCode::Esc); // entries
        key(&mut app, KeyCode::Up); // group filters
        key(&mut app, KeyCode::Down); // first entry

        let mut linked = false;
        let mut broken = false;
        let mut relink = false;
        let mut adopt = false;
        let mut no_repair = false;
        for _ in 0..48 {
            assert_footer_matches_focus(&mut app, "agents repair-state row");
            let hints = app.footer_hints();
            linked |= hints
                .iter()
                .any(|(key, desc)| *key == "m" && *desc == "multi-uninstall");
            broken |= hints
                .iter()
                .any(|(key, desc)| *key == "x" && *desc == "remove broken link");
            relink |= hints
                .iter()
                .any(|(key, desc)| *key == "r" && *desc == "relink");
            let ctx = Ctx {
                ws: &app.ws,
                snap: &app.snap,
                settings: &app.settings,
            };
            adopt |= app
                .agents
                .actions_menu(&ctx)
                .is_some_and(|menu| menu.allows(Command::Adopt));
            no_repair |= hints
                .iter()
                .all(|(key, _)| !matches!(*key, "x" | "r" | "a"));
            key(&mut app, KeyCode::Right);
        }
        assert!(linked, "deployed entry should expose multi-uninstall");
        assert!(broken, "broken entry should expose remove broken link");
        assert!(relink, "identical shadow should expose relink");
        assert!(adopt, "agent-only entry should expose adopt");
        assert!(
            no_repair,
            "different shadow must not expose a repair shortcut"
        );

        app.apply(Action::SwitchTab(Tab::Health));
        app.enter_page();
        for _ in 0..64 {
            assert_footer_matches_focus(&mut app, "health repair-state row");
            let hints = app.footer_hints();
            assert!(
                hints
                    .iter()
                    .any(|(key, desc)| *key == "a" && *desc == "actions")
            );
            assert!(hints.iter().all(|(key, _)| !matches!(*key, "x" | "r")));
            key(&mut app, KeyCode::Down);
        }
    }

    #[test]
    fn mouse_context_menu_flow_works_for_every_page_on_isolated_fixture() {
        let (_root, mut app) = interactive_app();
        for tab in Tab::ALL {
            app.apply(Action::SwitchTab(tab));
            app.enter_page();
            let (_column, _row) = open_first_menu(&mut app, 140, 35);
            let request = app.context_menu.as_ref().unwrap().request.clone();
            match tab {
                Tab::Search | Tab::Tags | Tab::Presets => {
                    assert!(matches!(request.target, Target::Skill(_)));
                }
                Tab::Repos => assert!(matches!(request.target, Target::Skill(_))),
                Tab::Agents => assert!(matches!(
                    request.target,
                    Target::Entry { ref scope, .. } if scope == "sample-agent"
                )),
                Tab::Health => assert!(matches!(
                    request.target,
                    Target::Entry { ref scope, .. } if scope.is_empty()
                )),
            }
            assert!(!request.items.is_empty());

            // A right click inside the menu is consumed by the menu itself.
            draw_app(&mut app, 140, 35);
            let open_rect = app
                .context_menu
                .as_ref()
                .unwrap()
                .hit_rect_for(Command::Open)
                .unwrap();
            app.handle(Msg::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Right),
                column: open_rect.x,
                row: open_rect.y,
                modifiers: KeyModifiers::NONE,
            }));
            assert!(app.context_menu.is_some());

            if let Some(item) = request.items.iter().find(|item| item.disabled.is_some()) {
                let command = item.command;
                let reason = item.disabled.clone().unwrap();
                let rect = app
                    .context_menu
                    .as_ref()
                    .unwrap()
                    .hit_rect_for(command)
                    .unwrap();
                app.handle(Msg::Mouse(MouseEvent {
                    kind: MouseEventKind::Moved,
                    column: rect.x,
                    row: rect.y,
                    modifiers: KeyModifiers::NONE,
                }));
                let screen = draw_app(&mut app, 140, 35);
                assert!(
                    screen.contains(&reason),
                    "missing disabled reason for {command:?}"
                );
            }

            let (column, row) = outside_menu(&app, 140, 35);
            app.handle(Msg::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Right),
                column,
                row,
                modifiers: KeyModifiers::NONE,
            }));
            assert!(app.context_menu.is_none());
        }

        // Exercise an actual enabled mouse click through App::handle. Open is
        // side-effect free on disk and closes the menu after opening preview.
        app.apply(Action::SwitchTab(Tab::Search));
        app.enter_page();
        open_first_menu(&mut app, 140, 35);
        draw_app(&mut app, 140, 35);
        let open_rect = app
            .context_menu
            .as_ref()
            .unwrap()
            .hit_rect_for(Command::Open)
            .unwrap();
        app.handle(Msg::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: open_rect.x,
            row: open_rect.y,
            modifiers: KeyModifiers::NONE,
        }));
        assert!(app.context_menu.is_none());
    }

    #[test]
    fn mouse_context_menu_stays_bounded_when_the_terminal_is_small() {
        let (_root, mut app) = interactive_app();
        app.apply(Action::SwitchTab(Tab::Search));
        app.enter_page();
        open_first_menu(&mut app, 80, 12);
        draw_app(&mut app, 80, 12);
        let area = app.context_menu.as_ref().unwrap().menu_area();
        assert!(area.right() <= 80 && area.bottom() <= 12);
        for _ in 0..20 {
            app.handle(Msg::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: area.x,
                row: area.y,
                modifiers: KeyModifiers::NONE,
            }));
        }
        draw_app(&mut app, 80, 12);
        let area = app.context_menu.as_ref().unwrap().menu_area();
        assert!(area.right() <= 80 && area.bottom() <= 12);
    }

    #[test]
    fn context_menu_blocks_page_input_and_closes_for_modal_resize_snapshot() {
        let root = skills::ops::DownloadDir::new("context-app").unwrap();
        Config {
            agents: vec![],
            ..Default::default()
        }
        .save(root.path())
        .unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new_with_launch_directory(
            Workspace::open(root.path()).unwrap(),
            tx,
            Some(root.path()),
        )
        .unwrap();
        let make = || {
            ContextMenu::new(
                Request {
                    title: "test".into(),
                    detail: "single".into(),
                    target: Target::Skill("test".into()),
                    items: vec![Item::new(
                        Command::Open,
                        "View",
                        KeyCode::Enter,
                        true,
                        "",
                        0,
                    )],
                },
                5,
                5,
            )
        };
        app.context_menu = Some(make());
        assert!(app.on_key(KeyEvent::from(KeyCode::Char('4'))).is_empty());
        assert_eq!(app.tab, Tab::Search);
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        let point = app
            .tab_rects
            .iter()
            .find(|(_, t)| *t == Tab::Agents)
            .unwrap()
            .0;
        app.on_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: point.x,
            row: point.y,
            modifiers: KeyModifiers::NONE,
        });
        assert!(app.context_menu.is_none());
        assert_eq!(app.tab, Tab::Search);
        app.context_menu = Some(make());
        app.handle(Msg::Resize);
        assert!(app.context_menu.is_none());
        app.context_menu = Some(make());
        app.apply(Action::OpenModal(Box::new(Modal::help())));
        assert!(app.context_menu.is_none());
        assert!(matches!(
            app.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))
                .as_slice(),
            [Action::Quit]
        ));
    }
}

#[cfg(test)]
mod root_sync_tests {
    use super::*;

    fn app() -> App {
        let temp = skills::ops::DownloadDir::new("tui-sync-scope").unwrap();
        Config {
            agents: vec![],
            ..Default::default()
        }
        .save(temp.path())
        .unwrap();
        let (tx, _) = std::sync::mpsc::channel();
        App::new(Workspace::open(temp.path()).unwrap(), tx).unwrap()
    }

    fn header(app: &mut App, width: u16) -> String {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 1)).unwrap();
        terminal.draw(|f| app.draw_header(f, f.area())).unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    fn status(
        changes: usize,
        ahead: usize,
        behind: usize,
        remote_checked: bool,
    ) -> skills::ops::sync::Status {
        skills::ops::sync::Status {
            settings: skills::ops::sync::Settings {
                url: Some("/tmp/remote.git".into()),
                branch: Some("main".into()),
                enabled: true,
            },
            changes: (0..changes).map(|i| format!("?? file-{i}")).collect(),
            ahead,
            behind,
            remote_checked,
            local_revision: Some("local".into()),
            remote_revision: Some("remote".into()),
        }
    }

    #[test]
    fn automatic_sync_failures_use_concise_policy_messages() {
        use skills::ops::sync::AutoSyncDisposition;

        let Action::Error(transient) = auto_sync_failure_action(
            AutoSyncDisposition::Transient,
            false,
            "git push origin failed: /tmp/private",
        ) else {
            panic!("transient failures must be errors");
        };
        assert!(transient.contains("next status check"));
        assert!(transient.contains("local changes retained"));
        assert!(!transient.contains("backup retained"));
        assert!(!transient.contains("git push"));
        assert!(!transient.contains("/tmp/"));

        let Action::Error(conflict) = auto_sync_failure_action(
            AutoSyncDisposition::Fatal,
            true,
            "internal merge command failed",
        ) else {
            panic!("conflicts must be errors");
        };
        assert!(conflict.contains("conflict"));
        assert!(conflict.contains("Resolve with Git"));
        assert!(!conflict.contains("internal merge command"));

        let Action::Error(fatal) = auto_sync_failure_action(
            AutoSyncDisposition::Fatal,
            false,
            "root is on feature; expected main",
        ) else {
            panic!("fatal failures must be errors");
        };
        assert!(fatal.contains("root is on feature; expected main"));
        assert!(!fatal.contains("backup retained"));

        assert!(matches!(
            auto_sync_failure_action(AutoSyncDisposition::WorkingTreeChanged, false, "ignored"),
            Action::Toast(message) if message.contains("automatic sync paused")
        ));
    }

    #[test]
    fn header_backup_button_is_compact_clickable_and_uses_cached_status() {
        let mut app = app();
        app.sync.set_status(status(3, 2, 1, true));
        let wide = header(&mut app, 120);
        assert!(wide.contains("● 3 · ↑ 2 · ↓ 1"), "{wide}");
        assert!(app.sync_button.right() <= 120);

        let narrow = header(&mut app, 40);
        assert!(narrow.ends_with("[●]"), "{narrow}");
        assert!(app.sync_button.right() <= 40);

        let actions = app.on_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: app.sync_button.x,
            row: app.sync_button.y,
            modifiers: KeyModifiers::NONE,
        });
        assert!(matches!(
            actions.as_slice(),
            [
                Action::OpenModal(_),
                Action::RefreshSyncStatus {
                    remote: true,
                    reason: ProbeReason::Manual,
                }
            ]
        ));
    }

    #[test]
    fn header_backup_states_and_stale_status_results_are_stable() {
        let mut app = app();
        app.sync.set_status(status(0, 0, 0, true));
        assert!(header(&mut app, 100).contains("✓ Backed up"));

        app.sync.set_error(Some("offline".into()));
        assert!(header(&mut app, 100).contains("× Check failed"));
        app.sync.set_error(None);
        app.sync.set_probing(true);
        assert!(header(&mut app, 100).contains("Checking…"));
        app.sync.set_syncing(true);
        assert!(header(&mut app, 100).contains("Syncing…"));

        app.sync.set_syncing(false);
        app.sync.set_probing(false);
        let current = app
            .sync
            .request_probe(true, ProbeReason::Manual)
            .unwrap()
            .id;
        app.handle(Msg::SyncStatus(
            current.wrapping_sub(1),
            Ok(status(9, 9, 9, true)),
        ));
        assert_eq!(app.sync.status.as_ref().unwrap().changes.len(), 0);
        assert_eq!(app.tasks_running, 0);
    }

    #[test]
    fn startup_rescan_and_deployment_mutations_do_not_schedule_root_sync() {
        let mut app = app();
        assert!(!app.sync.pending());

        app.rescan();
        assert!(!app.sync.pending());

        app.apply(Action::deployment(Action::Write(Box::new(|_| {
            Ok("deployed".into())
        }))));
        assert!(!app.sync.pending());
    }

    #[test]
    fn successful_library_mutation_schedules_root_sync() {
        let mut app = app();
        app.apply(Action::Write(Box::new(|_| Ok("saved".into()))));
        assert!(app.sync.pending());
    }

    #[test]
    fn metadata_write_schedules_one_root_backup_and_blocks_overlapping_writes() {
        let temp = skills::ops::DownloadDir::new("tui-root-sync").unwrap();
        let root = temp.path().join("root");
        let remote = temp.path().join("remote.git");
        std::fs::create_dir_all(&root).unwrap();
        Config {
            agents: vec![],
            ..Default::default()
        }
        .save(&root)
        .unwrap();
        let ws = Workspace::open(&root).unwrap();
        skills::ops::git(
            &[
                "init",
                "--bare",
                "--initial-branch=main",
                remote.to_str().unwrap(),
            ],
            None,
        )
        .unwrap();
        skills::ops::sync::configure(&ws, remote.to_str().unwrap(), "main").unwrap();
        skills::ops::sync::automatic(&ws).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let mut app = App::new_with_launch_directory(ws, tx, Some(temp.path())).unwrap();
        app.sync.start_manual_sync();
        app.apply(Action::Write(Box::new(|_| {
            panic!("must not write during checkout")
        })));
        assert!(
            matches!(
                app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))
                    .as_slice(),
                [Action::SwitchTab(_)]
            ),
            "root sync must not swallow navigation shortcuts"
        );
        app.sync.finish_manual_sync(false);
        app.apply(Action::Write(Box::new(|ws| {
            std::fs::write(ws.root.join("saved.txt"), "automatically backed up")?;
            Ok("saved".into())
        })));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while app.tasks_running > 0 || app.sync.pending() {
            assert!(
                std::time::Instant::now() < deadline,
                "auto-sync did not settle"
            );
            app.sync_if_ready();
            // Status probes are deliberately not counted as blocking tasks.
            // The production event loop still receives them while idle.
            app.handle(rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap());
        }
        assert_eq!(
            skills::ops::git(&["show", "HEAD:saved.txt"], Some(&remote)).unwrap(),
            "automatically backed up"
        );
        let before = skills::ops::git(&["rev-parse", "HEAD"], Some(&root)).unwrap();
        app.sync_if_ready();
        assert_eq!(app.tasks_running, 0);
        assert_eq!(
            before,
            skills::ops::git(&["rev-parse", "HEAD"], Some(&root)).unwrap()
        );
    }
}
