//! Application state: owns the snapshot, dispatches messages to the active
//! view or modal, and applies the `Action`s they return.

use super::event::{Msg, Task, TaskOutput, spawn_task};
use super::modal::Modal;
use super::theme::Theme;
use super::toast::Toasts;
use super::views::{
    View, agents::AgentsView, health::HealthView, presets::PresetsView, search::SearchView,
    tags::TagsView,
};
use super::widgets::{SPINNER, width};
use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use skills::Workspace;
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
}

impl Tab {
    pub const ALL: [Tab; 5] = [
        Tab::Search,
        Tab::Tags,
        Tab::Presets,
        Tab::Agents,
        Tab::Health,
    ];
    pub fn title(self) -> &'static str {
        match self {
            Tab::Search => "Search",
            Tab::Tags => "Tags",
            Tab::Presets => "Presets",
            Tab::Agents => "Agents",
            Tab::Health => "Health",
        }
    }
    pub fn index(self) -> usize {
        Tab::ALL.iter().position(|t| *t == self).unwrap_or(0)
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
    pub theme: &'a Theme,
}

pub struct App {
    pub ws: Workspace,
    pub snap: Snapshot,
    pub theme: Theme,
    pub tab: Tab,
    pub search: SearchView,
    pub tags: TagsView,
    pub presets: PresetsView,
    pub agents: AgentsView,
    pub health: HealthView,
    pub modal: Option<Modal>,
    pending_task_ui: VecDeque<Action>,
    batch_running: bool,
    pub toasts: Toasts,
    pub history: History,
    tasks_running: usize,
    next_task_id: u64,
    spinner: usize,
    tx: Sender<Msg>,
    external: Option<External>,
    quit: bool,
    tab_rects: Vec<(Rect, Tab)>,
    body: Rect,
}

impl App {
    pub fn new(ws: Workspace, tx: Sender<Msg>) -> Result<Self> {
        let snap = ws.scan()?;
        let mut app = Self {
            ws,
            snap,
            theme: Theme::default(),
            tab: Tab::Search,
            search: SearchView::default(),
            tags: TagsView::default(),
            presets: PresetsView::default(),
            agents: AgentsView::default(),
            health: HealthView::default(),
            modal: None,
            pending_task_ui: VecDeque::new(),
            batch_running: false,
            toasts: Toasts::default(),
            history: History::default(),
            tasks_running: 0,
            next_task_id: 0,
            spinner: 0,
            tx,
            external: None,
            quit: false,
            tab_rects: Vec::new(),
            body: Rect::default(),
        };
        app.on_snapshot();
        Ok(app)
    }

    pub fn should_quit(&self) -> bool {
        self.quit
    }

    fn on_snapshot(&mut self) {
        let ctx = Ctx {
            ws: &self.ws,
            snap: &self.snap,
            theme: &self.theme,
        };
        self.search.refresh(&ctx);
        self.tags.refresh(&ctx);
        self.presets.refresh(&ctx);
        self.agents.refresh(&ctx);
        self.health.refresh(&ctx);
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
        self.modal.is_some() || (self.tab == Tab::Tags && self.tags.input_focused())
    }

    fn on_task(&mut self, out: TaskOutput) -> Vec<Action> {
        match out {
            TaskOutput::Batch(outcome) => {
                self.batch_running = false;
                if let Some(intent) = outcome.intent {
                    self.history.record(intent);
                }
                self.search.batch_finished(&outcome.failed);
                if outcome.errors.is_empty() {
                    self.modal = None;
                    self.toast(outcome.message, Level::Ok);
                } else {
                    self.modal = Some(Modal::message(
                        format!(
                            "Batch result · {} skills need attention",
                            outcome.failed.len()
                        ),
                        outcome.errors,
                    ));
                    self.toast(
                        format!(
                            "{}; {} skills need attention",
                            outcome.message,
                            outcome.failed.len()
                        ),
                        Level::Error,
                    );
                }
                self.rescan();
                vec![]
            }

            TaskOutput::RepositoryFetched(_, Ok(fetched)) => {
                let ctx = Ctx {
                    ws: &self.ws,
                    snap: &self.snap,
                    theme: &self.theme,
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
                    selection.fetched.cleanup();
                    let mut actions: Vec<Action> = keys
                        .iter()
                        .map(|key| Action::Record(history::Intent::Install { skill: key.clone() }))
                        .collect();
                    actions.extend([
                        Action::Rescan,
                        Action::Toast(format!(
                            "installed {} skills — d deploys the selected skill",
                            keys.len()
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
                    actions
                }
                Err(e) => vec![
                    Action::Error(format!("install: {e:#}")),
                    Action::OpenModal(Box::new(Modal::Repository(Box::new(
                        super::repository_picker::RepositoryPicker::restore(*selection),
                    )))),
                ],
            },

            TaskOutput::Scan(Ok(snap)) => {
                self.snap = snap;
                self.on_snapshot();
                Vec::new()
            }
            TaskOutput::Scan(Err(e)) => vec![Action::Error(format!("scan failed: {e:#}"))],
            TaskOutput::Check(results) => {
                self.search.remember_checks(&results);
                let ctx = Ctx {
                    ws: &self.ws,
                    snap: &self.snap,
                    theme: &self.theme,
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
                Action::Toast(format!("installed {key} — press d to deploy it")),
                Action::Search {
                    query: key,
                    focus_list: true,
                },
            ],
            // Legacy git installs with several skills enter the shared repository picker.
            TaskOutput::Installed(reference, Err(e)) => {
                match e.downcast_ref::<skills::ops::install::NotOneSkill>() {
                    Some(_) => vec![Action::Spawn(Task::DiscoverRepository(reference))],
                    None => vec![Action::Error(format!("install {reference}: {e:#}"))],
                }
            }
        }
    }

    fn on_paste(&mut self, text: &str) -> Vec<Action> {
        if self.batch_running {
            return vec![];
        }
        let ctx = Ctx {
            ws: &self.ws,
            snap: &self.snap,
            theme: &self.theme,
        };
        if let Some(modal) = self.modal.as_mut() {
            return modal.paste(text, &ctx);
        }
        match self.tab {
            Tab::Search => self.search.paste(text, &ctx),
            Tab::Tags => self.tags.paste(text),
            _ => vec![],
        }
    }

    fn on_key(&mut self, k: KeyEvent) -> Vec<Action> {
        if self.batch_running {
            return vec![];
        }
        let ctx = Ctx {
            ws: &self.ws,
            snap: &self.snap,
            theme: &self.theme,
        };
        if let Some(m) = self.modal.as_mut() {
            return m.handle_key(k, &ctx);
        }
        if self.tab == Tab::Agents
            && let Some(actions) = self.agents.handle_matrix_key(k, &ctx)
        {
            return actions;
        }
        // Any text field that holds the keyboard keeps its digits and slashes;
        // the Tags page has one of its own for colours and merge targets.
        let in_search_input = (self.tab == Tab::Search && self.search.input_focused())
            || (self.tab == Tab::Tags && self.tags.input_focused());
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
            (KeyCode::F(1), _) => return vec![Action::OpenModal(Box::new(Modal::help()))],
            (KeyCode::Char('?'), _) if !in_search_input => {
                return vec![Action::OpenModal(Box::new(Modal::help()))];
            }
            (KeyCode::Char(c @ '1'..='5'), m)
                if !in_search_input || m.contains(KeyModifiers::ALT) =>
            {
                return vec![Action::SwitchTab(Tab::ALL[(c as u8 - b'1') as usize])];
            }
            (KeyCode::Tab, _) if self.tab != Tab::Search => {
                return vec![Action::SwitchTab(
                    Tab::ALL[(self.tab.index() + 1) % Tab::ALL.len()],
                )];
            }
            (KeyCode::BackTab, _) if self.tab != Tab::Search => {
                return vec![Action::SwitchTab(
                    Tab::ALL[(self.tab.index() + Tab::ALL.len() - 1) % Tab::ALL.len()],
                )];
            }
            (KeyCode::Char('/'), _) if !in_search_input => {
                return vec![Action::Search {
                    query: self.search.query(),
                    focus_list: false,
                }];
            }
            _ => {}
        }
        match self.tab {
            Tab::Search => self.search.handle_key(k, &ctx),
            Tab::Tags => self.tags.handle_key(k, &ctx),
            Tab::Presets => self.presets.handle_key(k, &ctx),
            Tab::Agents => self.agents.handle_key(k, &ctx),
            Tab::Health => self.health.handle_key(k, &ctx),
        }
    }

    fn on_mouse(&mut self, m: MouseEvent) -> Vec<Action> {
        if self.batch_running {
            return vec![];
        }
        let ctx = Ctx {
            ws: &self.ws,
            snap: &self.snap,
            theme: &self.theme,
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
        }
    }

    fn apply(&mut self, action: Action) {
        match action {
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
            Action::Quit => self.quit = true,
            Action::SelectSkills {
                keys,
                title,
                checked,
            } => {
                self.switch_tab(Tab::Search);
                let ctx = Ctx {
                    ws: &self.ws,
                    snap: &self.snap,
                    theme: &self.theme,
                };
                self.search.select_scope(keys, title, checked, &ctx);
            }
            Action::Toast(t) => self.toast(t, Level::Ok),
            Action::Error(t) => self.toast(t, Level::Error),
            Action::Rescan => self.rescan(),
            Action::Spawn(task) => self.spawn(task),
            Action::OpenModal(m) => self.modal = Some(*m),
            Action::CloseModal => self.modal = None,
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
            Action::Search { query, focus_list } => {
                self.switch_tab(Tab::Search);
                let ctx = Ctx {
                    ws: &self.ws,
                    snap: &self.snap,
                    theme: &self.theme,
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
                    self.modal = Some(Modal::NameConflict {
                        title,
                        actions,
                        rect: Rect::default(),
                    });
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
                self.modal = Some(if deploy::name_conflicts(&self.snap, &actions).is_empty() {
                    Modal::confirm(title, actions)
                } else {
                    Modal::NameConflict {
                        title,
                        actions,
                        rect: Rect::default(),
                    }
                });
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
                        self.toast(msg, Level::Ok);
                        // A write that left the files as they were is not a step.
                        if let Some(intent) = intent {
                            self.history.record(intent);
                        }
                    }
                    Err(e) => self.toast(format!("{e:#}"), Level::Error),
                }
                self.rescan();
            }
        }
    }

    /// Make `t` the active tab. The view is told only when the tab actually
    /// changes: a key for the tab already showing is not a return to it, and
    /// must not throw away a focus the user has just set.
    fn switch_tab(&mut self, t: Tab) {
        if t == self.tab {
            return;
        }
        self.search.clear_selection();
        self.tab = t;
        let view: &mut dyn View = match t {
            Tab::Search => &mut self.search,
            Tab::Tags => &mut self.tags,
            Tab::Presets => &mut self.presets,
            Tab::Agents => &mut self.agents,
            Tab::Health => &mut self.health,
        };
        view.enter();
    }

    /// Take one step back or forward. The plan is worked out against the tree
    /// as it stands, so anything changed since is skipped rather than forced.
    fn step(&mut self, dir: Step) -> Vec<Action> {
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
        self.tasks_running += 1;
        self.next_task_id += 1;
        let id = self.next_task_id;
        let label = match &task {
            Task::Scan => None,
            Task::DiscoverRepository(reference) => Some(format!("Clone {reference}")),
            Task::InstallRepository(selection) => {
                Some(format!("Install {}", selection.fetched.repository.alias))
            }
            Task::Install { reference, .. } => Some(format!("Install {reference}")),
            Task::Check(keys) => Some(format!("Check upstream: {} skills", keys.len())),
            Task::Prepare(key) => Some(format!("Prepare update: {key}")),
        };
        if let Some(label) = label {
            self.toasts.start(id, label);
        }
        spawn_task(self.ws.clone(), task, id, self.tx.clone());
    }

    pub fn rescan(&mut self) {
        // The config is read once when the workspace opens, and the scan
        // works from that copy. A preset rename rewrites `[deploy].presets`
        // on disk behind it, so the file is read again before every rescan:
        // that is what keeps the `auto` mark on a preset card, and the
        // desired state `sync` plans from, in step with what is written. A
        // file that no longer parses is reported and the last good copy kept,
        // since a hand edit in progress should not take the program down.
        match Config::load(&self.ws.root) {
            Ok(config) => self.ws.config = config,
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
            theme: &self.theme,
        };
        match self.tab {
            Tab::Search => self.search.draw(f, rows[1], &ctx),
            Tab::Tags => self.tags.draw(f, rows[1], &ctx),
            Tab::Presets => self.presets.draw(f, rows[1], &ctx),
            Tab::Agents => self.agents.draw(f, rows[1], &ctx),
            Tab::Health => self.health.draw(f, rows[1], &ctx),
        }
        self.draw_footer(f, rows[2]);
        if let Some(m) = self.modal.as_mut() {
            m.draw(f, area, &ctx);
        }
        // Above everything: a notification should be readable over a dialog.
        self.toasts.draw(f, area, &self.theme);
    }

    fn draw_header(&mut self, f: &mut Frame, area: Rect) {
        let th = &self.theme;
        let mut spans: Vec<Span> = vec![Span::styled(" skills ", th.bold().fg(th.accent))];
        self.tab_rects.clear();
        let mut x = area.x + width(" skills ") as u16;
        for (i, t) in Tab::ALL.iter().enumerate() {
            let label = format!(" {} {} ", i + 1, t.title());
            let style = if *t == self.tab {
                th.selected().fg(th.accent)
            } else {
                th.dim()
            };
            let w = width(&label) as u16;
            self.tab_rects.push((Rect::new(x, area.y, w, 1), *t));
            spans.push(Span::styled(label, style));
            spans.push(Span::raw(" "));
            x += w + 1;
        }
        let right = if self.tasks_running > 0 {
            format!("{} working  ", SPINNER[self.spinner])
        } else {
            format!("{}  ", skills::paths::contract_tilde(&self.snap.root))
        };
        let used = (x - area.x) as usize;
        let pad = (area.width as usize).saturating_sub(used + width(&right));
        spans.push(Span::raw(" ".repeat(pad)));
        spans.push(Span::styled(right, th.dim()));
        f.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    fn draw_footer(&self, f: &mut Frame, area: Rect) {
        let th = &self.theme;
        let hints = if let Some(m) = &self.modal {
            m.hints()
        } else {
            match self.tab {
                Tab::Search => self.search.hints(),
                Tab::Tags => self.tags.hints(),
                Tab::Presets => self.presets.hints(),
                Tab::Agents => self.agents.hints(),
                Tab::Health => self.health.hints(),
            }
        };
        // Results moved out to the notification stack, so the footer is only keys.
        let mut spans: Vec<Span> = Vec::new();
        let mut hint_w = 0usize;
        for (key, desc) in hints {
            let piece_w = width(key) + width(desc) + 3;
            if hint_w + piece_w + 1 > area.width as usize {
                break;
            }
            spans.push(Span::styled(*key, th.key_hint()));
            spans.push(Span::styled(format!(" {desc}  "), th.dim()));
            hint_w += piece_w;
        }
        let pad = (area.width as usize).saturating_sub(hint_w);
        let mut line = vec![Span::raw(" ".repeat(pad))];
        line.extend(spans);
        f.render_widget(Paragraph::new(Line::from(line)), area);
    }
}

/// Key hint pairs shown in the footer.
pub type Hints = &'static [(&'static str, &'static str)];

#[cfg(test)]
mod matrix_key_tests {
    use super::*;
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
        for code in [
            KeyCode::Tab,
            KeyCode::BackTab,
            KeyCode::Char('/'),
            KeyCode::Char('1'),
        ] {
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
