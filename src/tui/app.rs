//! Application state: owns the snapshot, dispatches messages to the active
//! view or modal, and applies the `Action`s they return.

use super::event::{Msg, Task, TaskOutput, spawn_task};
use super::modal::Modal;
use super::settings::{LayoutScope, RuntimeSettings, SessionSettings};
use super::theme::Theme;
use super::toast::Toasts;
use super::views::{
    View, agents::AgentsView, health::HealthView, presets::PresetsView, repos::ReposView,
    search::SearchView, tags::TagsView,
};
use super::widgets::{SPINNER, fit, width};
use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use skills::Workspace;
#[cfg(test)]
use skills::config::Config;
use skills::history::{self, History, Plan};
use skills::ops::deploy;
use skills::reconcile::Snapshot;
use std::collections::VecDeque;
use std::sync::mpsc::Sender;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Search,
    Tags,
    Presets,
    Agents,
    Health,
    Repos,
}

impl Tab {
    pub const ALL: [Tab; 6] = [
        Tab::Search,
        Tab::Tags,
        Tab::Presets,
        Tab::Agents,
        Tab::Health,
        Tab::Repos,
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
    pending_task_ui: VecDeque<Action>,
    batch_running: bool,
    batch_modal_owned: bool,
    pub toasts: Toasts,
    pub history: History,
    tasks_running: usize,
    next_task_id: u64,
    spinner: usize,
    last_root_poll: std::time::Instant,
    root_stamp: Option<skills::reconcile::watch::Stamp>,
    tx: Sender<Msg>,
    external: Option<External>,
    quit: bool,
    quit_prompt: Option<QuitPrompt>,
    tab_rects: Vec<(Rect, Tab)>,
    body: Rect,
}

/// Kept separate from the editing modal so cancelling quit preserves its input.
#[derive(Default)]
struct QuitPrompt {
    quit_selected: bool,
    area: Rect,
    quit_button: Rect,
    cancel_button: Rect,
}

impl QuitPrompt {
    fn draw(&mut self, f: &mut Frame, outer: Rect, th: &Theme, tasks: Vec<String>) {
        use super::widgets::{OverlayClear, button, fit};
        let w = outer.width.saturating_sub(2).min(86);
        let h = outer
            .height
            .saturating_sub(2)
            .min(tasks.len() as u16 + 6)
            .max(1);
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
        f.render_widget(Paragraph::new(lines), inner);
        let y = inner.bottom().saturating_sub(1);
        self.cancel_button = Rect::new(
            inner.right().saturating_sub(10).max(inner.x),
            y,
            inner.width.min(10),
            u16::from(inner.height > 0),
        );
        self.quit_button = Rect::new(
            self.cancel_button.x.saturating_sub(10).max(inner.x),
            y,
            self.cancel_button.x.saturating_sub(inner.x).min(8),
            u16::from(inner.height > 0),
        );
        f.render_widget(
            Paragraph::new(Line::from(button("Quit", self.quit_selected, th))),
            self.quit_button,
        );
        f.render_widget(
            Paragraph::new(Line::from(button("Cancel", !self.quit_selected, th))),
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
        let startup_repair = skills::ops::repair::startup(&ws);
        // Repairs can migrate or remove tag membership; display the persisted result.
        if !matches!(&startup_repair, Ok(report) if report.repaired == 0 && report.failed == 0) {
            ws.config = ws.load_config()?;
            Self::discover_local_agents(&mut ws)?;
        }
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
            pending_task_ui: VecDeque::new(),
            batch_running: false,
            batch_modal_owned: false,
            history: History::default(),
            tasks_running: 0,
            next_task_id: 0,
            spinner: 0,
            last_root_poll: std::time::Instant::now(),
            root_stamp,
            tx,
            external: None,
            quit: false,
            quit_prompt: None,
            tab_rects: Vec::new(),
            body: Rect::default(),
        };
        app.on_snapshot();
        match startup_repair {
            Ok(report) if report.repaired > 0 || report.failed > 0 => {
                app.toast(
                    format!(
                        "Startup repair: {} repaired, {} failed",
                        report.repaired, report.failed
                    ),
                    if report.failed > 0 {
                        Level::Error
                    } else {
                        Level::Ok
                    },
                );
            }
            Err(error) => app.toast(
                format!("Startup repair: {error:#}; review Health"),
                Level::Error,
            ),
            _ => {}
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
            Ok((skill, Some(text))) => match history::note_edit(&self.ws, &skill, Some(&text)) {
                Ok((msg, intent)) => {
                    self.toast(msg, Level::Ok);
                    if let Some(intent) = intent {
                        self.history.record(intent);
                    }
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
                if self.tasks_running == 0
                    && !self.task_ui_blocked()
                    && self.last_root_poll.elapsed() >= self.settings.interaction.root_poll_interval
                {
                    self.last_root_poll = std::time::Instant::now();
                    self.spawn(Task::PollRoot);
                }
                Vec::new()
            }
            Msg::Resize => Vec::new(),
            Msg::Progress(id, detail) => {
                self.toasts.progress(id, detail);
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
    }

    fn task_ui_blocked(&self) -> bool {
        self.quit_prompt.is_some()
            || self.modal.is_some()
            || (self.tab == Tab::Tags && self.tags.input_focused())
    }

    fn on_task(&mut self, out: TaskOutput) -> Vec<Action> {
        match out {
            TaskOutput::RepairPlan(result) => match result {
                Ok(report) => vec![Action::OpenModal(Box::new(Modal::HealthRepair(Box::new(
                    super::views::health::RepairDialog::preview(report),
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
            TaskOutput::Sync(request, result) => match result {
                Err(e) => vec![Action::Error(format!("Sync {}: {e:#}", request.remote))],
                Ok(changes) if request.dry_run => {
                    let ctx = Ctx {
                        ws: &self.ws,
                        snap: &self.snap,
                        settings: &self.settings,
                    };
                    match super::sync_picker::SyncPicker::preview(&ctx, request, changes) {
                        Ok(p) => vec![Action::OpenModal(Box::new(Modal::Sync(Box::new(p))))],
                        Err(e) => vec![Action::Error(format!("{e:#}"))],
                    }
                }
                Ok(changes) => vec![
                    Action::Toast(format!(
                        "Synced {} skills with {}",
                        changes.len(),
                        request.remote
                    )),
                    Action::Rescan,
                ],
            },
            TaskOutput::Batch(outcome) => {
                self.batch_running = false;
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
                self.rescan();
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
                    actions.extend([
                        Action::Rescan,
                        Action::Toast(format!(
                            "installed {} skills{} — choose destination agents",
                            keys.len(),
                            if aliases.is_empty() {
                                String::new()
                            } else {
                                format!(
                                    "; warning: folder aliases {} (declared names unchanged)",
                                    aliases.join(", ")
                                )
                            }
                        )),
                        Action::Search {
                            query: format!(
                                "repo:{}",
                                skills::repository::source_name(&selection.fetched.repository.url)
                                    .unwrap_or_else(|| selection.fetched.repository.alias.clone())
                            ),
                            focus_list: true,
                        },
                    ]);
                    actions.push(Action::OpenModal(Box::new(Modal::batch_deploy(
                        keys,
                        &Ctx {
                            ws: &self.ws,
                            snap: &self.snap,
                            settings: &self.settings,
                        },
                    ))));
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
                if prepared.from_revision.as_deref() == Some(prepared.to_revision.as_str()) {
                    prepared.cleanup();
                    return vec![Action::Toast(format!("{key} is up to date"))];
                }
                vec![Action::OpenModal(Box::new(Modal::resolve(prepared)))]
            }
            TaskOutput::Prepared(key, Err(e)) => {
                vec![Action::Error(format!("update {key}: {e:#}"))]
            }
            // Land on the new skill so the next thing to do, deploying it, is
            // one key away.
            TaskOutput::Installed(_, Ok(key)) => vec![
                Action::Record(history::Intent::Install { skill: key.clone() }),
                Action::Rescan,
                Action::Toast(format!("installed {key} — choose destination agents")),
                Action::Search {
                    query: key.clone(),
                    focus_list: true,
                },
                Action::OpenModal(Box::new(Modal::batch_deploy(
                    vec![key],
                    &Ctx {
                        ws: &self.ws,
                        snap: &self.snap,
                        settings: &self.settings,
                    },
                ))),
            ],
            // Multi-skill sources use the same repository picker as direct discovery.
            TaskOutput::Installed(reference, Err(e)) => {
                match e.downcast_ref::<skills::ops::install::NotOneSkill>() {
                    Some(_) => vec![Action::Spawn(Task::DiscoverRepository(reference))],
                    None => vec![Action::Error(format!("install {reference}: {e:#}"))],
                }
            }
        }
    }

    fn on_paste(&mut self, text: &str) -> Vec<Action> {
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
            Tab::Agents => self.agents.paste(text),
        }
    }

    fn on_key(&mut self, k: KeyEvent) -> Vec<Action> {
        if let Some(prompt) = self.quit_prompt.as_mut() {
            match k.code {
                KeyCode::Esc => self.quit_prompt = None,
                KeyCode::Left | KeyCode::Right => {
                    prompt.quit_selected = !prompt.quit_selected;
                }
                KeyCode::Enter => {
                    self.quit = prompt.quit_selected;
                    self.quit_prompt = None;
                }
                _ => {}
            }
            return vec![];
        }
        if k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL) {
            return vec![Action::Quit];
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
        let in_search_input = (self.tab == Tab::Search && self.search.input_focused())
            || (self.tab == Tab::Tags && self.tags.input_focused())
            || (self.tab == Tab::Presets && self.presets.input_focused())
            || (self.tab == Tab::Health && self.health.input_focused())
            || (self.tab == Tab::Repos && self.repos.input_focused());
        match (k.code, k.modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => return vec![Action::Quit],
            (KeyCode::Char('z'), KeyModifiers::CONTROL) => return self.step(Step::Undo),
            (KeyCode::Char('y'), KeyModifiers::CONTROL) => return self.step(Step::Redo),
            (KeyCode::Char('r'), KeyModifiers::CONTROL) => {
                return vec![Action::Rescan, Action::Toast("rescanning".into())];
            }
            (KeyCode::Char('R'), _) if !in_search_input => {
                return vec![Action::OpenModal(Box::new(Modal::repositories(&ctx)))];
            }
            (KeyCode::F(2), _) => {
                let enabled = !self.settings.tags_enabled;
                return vec![Action::OpenModal(Box::new(Modal::confirm_write(
                    "Settings · Tags".into(),
                    vec![format!("Tags: {} → {}", !enabled, enabled), "Hide or show tag classification throughout the interface. Existing data and preset membership are preserved.".into()],
                    Box::new(move |ws| { skills::config::Config::set_tags_enabled(&ws.root, enabled)?; Ok(format!("Tags {}", if enabled { "enabled" } else { "disabled" })) })
                )))];
            }
            (KeyCode::F(5), _) => return vec![Action::Rescan, Action::Toast("rescanning".into())],
            (KeyCode::F(6), _) => {
                return vec![Action::OpenModal(Box::new(Modal::HealthRepair(
                    Box::default(),
                )))];
            }
            (KeyCode::F(7), _) => {
                return vec![Action::OpenModal(Box::new(super::sync_picker::open(&ctx)))];
            }
            (KeyCode::F(1), _) => return vec![Action::OpenModal(Box::new(Modal::help()))],
            (KeyCode::Char('?'), _) if !in_search_input => {
                return vec![Action::OpenModal(Box::new(Modal::help()))];
            }
            (KeyCode::Char(c @ '1'..='6'), KeyModifiers::NONE) if !in_search_input => {
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

    fn on_mouse(&mut self, m: MouseEvent) -> Vec<Action> {
        if let Some(prompt) = self.quit_prompt.as_ref() {
            if m.kind == MouseEventKind::Down(MouseButton::Left) {
                let point = (m.column, m.row).into();
                if prompt.quit_button.contains(point) {
                    self.quit = true;
                    self.quit_prompt = None;
                } else if prompt.cancel_button.contains(point) || !prompt.area.contains(point) {
                    self.quit_prompt = None;
                }
            }
            return vec![];
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
        if let MouseEventKind::Down(MouseButton::Left) = m.kind
            && let Some((_, tab)) = self
                .tab_rects
                .iter()
                .find(|(r, _)| r.contains((m.column, m.row).into()))
        {
            return vec![Action::SwitchTab(*tab)];
        }
        if !self.body.contains((m.column, m.row).into()) {
            return Vec::new();
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
        if self.batch_running
            && matches!(
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
            )
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
                if self.tasks_running == 0 {
                    self.quit = true;
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
            Action::Spawn(task) => self.spawn(task),
            Action::OpenModal(m) => self.modal = Some(*m),
            Action::CloseModal => {
                self.modal = match self.modal.take() {
                    Some(Modal::Message { return_to, .. }) => return_to.map(|modal| *modal),
                    _ => None,
                };
            }
            Action::SubmitInput(actions) => {
                let prompt = self.modal.take();
                for action in actions {
                    let result = match action {
                        Action::Error(error) => Err(anyhow::anyhow!(error)),
                        Action::Write(write) => {
                            let result = write(&self.ws);
                            self.rescan();
                            result.map(|message| self.toast(message, Level::Ok))
                        }
                        Action::WriteMeta(write) => {
                            let result = write(&self.ws);
                            self.rescan();
                            result.map(|(message, intent)| {
                                self.toast(message, Level::Ok);
                                if let Some(intent) = intent {
                                    self.history.record(intent);
                                }
                            })
                        }
                        other => {
                            self.apply(other);
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
            Action::SwitchTab(t) => {
                self.switch_tab(t);
                if t == Tab::Search {
                    self.search.focus_input();
                }
            }
            Action::SelectPreset(name) => self.presets.select(&name),
            Action::SelectTag(name) => self.tags.select(&name, &self.snap),
            Action::Search { query, focus_list } => {
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
            Action::Write(f) => {
                match f(&self.ws) {
                    Ok(msg) => self.toast(msg, Level::Ok),
                    Err(e) => self.toast(format!("{e:#}"), Level::Error),
                }
                self.rescan();
            }
            Action::BackgroundWrite { title, write, keys } => {
                self.spawn_batch(super::event::BatchWork::Metadata(write, keys), title);
            }
            Action::BatchMeta(write, keys) => {
                self.spawn_batch(
                    super::event::BatchWork::Metadata(write, keys.clone()),
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
            Action::WriteMeta(f) => {
                match f(&self.ws) {
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
                    }
                }
                self.rescan();
            }
        }
    }

    /// Make `t` the active tab. The view is told only when the tab actually
    /// changes: a key for the tab already showing is not a return to it, and
    /// must not throw away a focus the user has just set.
    fn switch_tab(&mut self, t: Tab) {
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
        if matches!(task, Task::RepairApply(_)) {
            self.batch_running = true;
        }
        self.tasks_running += 1;
        self.next_task_id += 1;
        let id = self.next_task_id;
        let label = match &task {
            Task::RepairPlan(_) => Some("Scan and preview health repairs".into()),
            Task::RepairApply(_) => Some("Apply health repairs".into()),
            Task::Scan | Task::PollRoot => None,
            Task::DiscoverRepository(reference) => Some(format!("Fetch {reference}")),
            Task::InstallRepository(selection) => {
                Some(format!("Install {}", selection.fetched.repository.alias))
            }
            Task::Install { reference, .. } => Some(format!("Install {reference}")),
            Task::Check(keys) => Some(format!("Check upstream: {} skills", keys.len())),
            Task::Sync(r) => Some(format!(
                "Sync {}{}",
                r.remote,
                if r.dry_run { " (preview)" } else { "" }
            )),
            Task::Prepare(key) => Some(format!("Prepare update: {key}")),
        };
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

    // ---- drawing ----------------------------------------------------------

    pub fn draw(&mut self, f: &mut Frame) {
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
        self.draw_footer(f, rows[2]);
        if let Some(m) = self.modal.as_mut() {
            m.draw(f, area, &ctx);
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
        if area.width < self.settings.layout.narrow_header_width {
            let title = format!(" {} ", self.tab.title());
            self.tab_rects.push((
                Rect::new(area.x, area.y, (width(&title) as u16).min(area.width), 1),
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
                area,
            );
            return;
        }
        let compact = area.width < self.settings.layout.compact_header_width;
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
                th.selected()
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
        let right = if self.tasks_running > 0 {
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
        if used + width(&right) < area.width as usize {
            let pad = area.width as usize - used - width(&right);
            spans.push(Span::raw(" ".repeat(pad)));
            spans.push(Span::styled(right, th.dim()));
        }
        f.render_widget(
            Paragraph::new(Line::from(spans)),
            Rect::new(area.x, area.y, area.width, 1),
        );
    }

    fn draw_footer(&self, f: &mut Frame, area: Rect) {
        let th = &self.settings.theme;
        let ctx = Ctx {
            ws: &self.ws,
            snap: &self.snap,
            settings: &self.settings,
        };
        let status = if self.modal.is_some() {
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
        let hints = if self.batch_running && self.batch_modal_owned {
            &[("…", "saving changes")][..]
        } else if let Some(m) = &self.modal {
            m.hints()
        } else {
            match self.tab {
                Tab::Search => self.search.hints(),
                Tab::Tags => self.tags.hints(),
                Tab::Presets => self.presets.hints(),
                Tab::Agents => self.agents.hints(),
                Tab::Health => self.health.hints(),
                Tab::Repos => self.repos.hints(),
            }
        };
        // Results moved out to the notification stack, so the footer is only keys.
        let mut spans: Vec<Span> = Vec::new();
        let mut hint_w = 0usize;
        let escape = hints.iter().find(|(key, _)| key.contains("Esc"));
        let reserve = escape.map_or(0, |(key, desc)| width(key) + width(desc) + 3)
            + if self.modal.is_none() { 9 } else { 0 };
        for (key, desc) in hints {
            if (!self.settings.tags_enabled && *key == "t")
                || key.contains("Esc")
                || (*key == "F1" && self.modal.is_none())
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
        if self.modal.is_none() && hint_w + 9 <= budget {
            spans.push(Span::styled("F1", th.key_hint()));
            spans.push(Span::styled(" help  ", Style::default().fg(th.placeholder)));
            hint_w += 9;
        }
        let pad = budget.saturating_sub(hint_w);
        let mut line = vec![
            Span::styled(status, th.skill_count()),
            Span::raw(" ".repeat(pad)),
        ];
        line.extend(spans);
        f.render_widget(Paragraph::new(Line::from(line)), area);
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
        let Msg::Task(id, _) = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap() else {
            panic!("expected scan completion");
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
        app.handle(Msg::Key(KeyEvent::new(
            KeyCode::Char('a'),
            KeyModifiers::NONE,
        )));
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
        app.handle(Msg::Key(KeyEvent::new(
            KeyCode::Char('a'),
            KeyModifiers::NONE,
        )));
        assert!(app.batch_running);
        let task = app.next_task_id;
        app.handle(Msg::Key(KeyEvent::new(
            KeyCode::Char('a'),
            KeyModifiers::NONE,
        )));
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
        for tab in [
            Tab::Tags,
            Tab::Presets,
            Tab::Agents,
            Tab::Health,
            Tab::Repos,
        ] {
            app.switch_tab(tab);
            app.handle(Msg::Key(KeyEvent::new(
                KeyCode::Char('q'),
                KeyModifiers::NONE,
            )));
            assert_eq!(app.tab, Tab::Search);
            assert!(!app.quit);
            assert!(app.quit_prompt.is_none());
            assert_eq!(app.tasks_running, 1);
        }
        app.handle(Msg::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        if app.quit_prompt.is_none() {
            app.handle(Msg::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        }
        assert!(
            app.quit_prompt.is_some(),
            "empty Search Esc must use the quit guard"
        );
        app.handle(Msg::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        assert!(!app.quit);
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
        app.handle(key(KeyCode::Esc));
        let Some(Modal::Repository(picker)) = &app.modal else {
            panic!("expected second result")
        };
        assert_eq!(picker.selection.fetched.repository.alias, "second");
        assert!(!root.join(".downloads/first").exists());
        assert!(root.join(".downloads/second").exists());
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
        assert!(matches!(
            app.on_key(key(KeyCode::Tab)).as_slice(),
            [Action::SwitchTab(Tab::Health)]
        ));
        assert!(matches!(
            app.on_key(key(KeyCode::BackTab)).as_slice(),
            [Action::SwitchTab(Tab::Presets)]
        ));
        for code in [KeyCode::Char('/'), KeyCode::Char('1')] {
            assert!(app.on_key(key(code)).is_empty());
            assert_eq!(app.tab, Tab::Agents);
        }
        assert!(app.on_key(key(KeyCode::Esc)).is_empty());
        assert!(matches!(
            app.on_key(key(KeyCode::Tab)).as_slice(),
            [Action::SwitchTab(Tab::Health)]
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
        app.handle(Msg::Key(KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE)));
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
mod startup_repair_tests {
    use super::*;

    #[test]
    fn startup_removes_absent_metadata_before_building_the_first_snapshot() {
        let temp = skills::ops::DownloadDir::new("tui-startup-repair").unwrap();
        let root = temp.path().join("root");
        skills::config::Config {
            agents: vec![],
            ..Default::default()
        }
        .save(&root)
        .unwrap();
        let ws = Workspace::open(&root).unwrap();
        ws.meta
            .save(
                "gone",
                &skills::meta::SkillMeta {
                    source: Some(skills::meta::Source::Git {
                        url: "https://example.invalid/source.git".into(),
                        branch: None,
                        subpath: None,
                        revision: None,
                    }),
                    ..Default::default()
                },
            )
            .unwrap();
        let (tx, _) = std::sync::mpsc::channel();
        let app = App::new_with_launch_directory(ws, tx, Some(temp.path())).unwrap();
        assert!(!app.ws.meta.exists("gone"));
        assert!(app.snap.get("gone").is_none());
        assert!(app.ws.meta.dir.join(".repair-backups").is_dir());
    }
}
