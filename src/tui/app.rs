//! Application state: owns the snapshot, dispatches messages to the active
//! view or modal, and applies the `Action`s they return.

use super::event::{Msg, Task, TaskOutput, spawn_task};
use super::modal::Modal;
use super::theme::Theme;
use super::views::{
    View, agents::AgentsView, health::HealthView, presets::PresetsView, search::SearchView,
    tags::TagsView,
};
use super::widgets::{SPINNER, fit, width};
use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use skills::Workspace;
use skills::ops::deploy;
use skills::ops::edit;
use skills::reconcile::Snapshot;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

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

/// Requests a view or modal hands back to the app.
pub enum Action {
    Quit,
    Toast(String),
    Error(String),
    /// Re-scan in the background.
    Rescan,
    Spawn(Task),
    OpenModal(Box<Modal>),
    CloseModal,
    SwitchTab(Tab),
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

pub struct Toast {
    pub text: String,
    pub level: Level,
    pub at: Instant,
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
    pub toast: Option<Toast>,
    tasks_running: usize,
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
            toast: None,
            tasks_running: 0,
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
        self.toast = Some(Toast {
            text: text.into(),
            level,
            at: Instant::now(),
        });
    }

    // ---- external ---------------------------------------------------------

    pub fn take_external(&mut self) -> Option<External> {
        self.external.take()
    }

    pub fn run_external(&mut self, req: External) -> Result<(String, String)> {
        match req {
            External::EditNote { skill, initial } => {
                let text = crate::cli::edit_in_editor(&initial)?;
                Ok((skill, text))
            }
        }
    }

    pub fn finish_external(&mut self, outcome: Result<(String, String)>) {
        match outcome {
            Ok((skill, text)) => match edit::note_set(&self.ws, &skill, Some(&text)) {
                Ok(_) => self.toast(format!("note saved on {skill}"), Level::Ok),
                Err(e) => self.toast(format!("note failed: {e:#}"), Level::Error),
            },
            Err(e) => self.toast(format!("editor: {e:#}"), Level::Error),
        }
        self.rescan();
    }

    // ---- messages ---------------------------------------------------------

    pub fn handle(&mut self, msg: Msg) {
        let actions = match msg {
            Msg::Tick => {
                self.spinner = (self.spinner + 1) % SPINNER.len();
                if let Some(t) = &self.toast
                    && t.level != Level::Error
                    && t.at.elapsed() > Duration::from_secs(6)
                {
                    self.toast = None;
                }
                Vec::new()
            }
            Msg::Resize => Vec::new(),
            Msg::Task(out) => {
                self.tasks_running = self.tasks_running.saturating_sub(1);
                self.on_task(*out)
            }
            Msg::Key(k) => self.on_key(k),
            Msg::Mouse(m) => self.on_mouse(m),
        };
        for a in actions {
            self.apply(a);
        }
    }

    fn on_task(&mut self, out: TaskOutput) -> Vec<Action> {
        match out {
            TaskOutput::Scan(Ok(snap)) => {
                self.snap = snap;
                self.on_snapshot();
                Vec::new()
            }
            TaskOutput::Scan(Err(e)) => vec![Action::Error(format!("scan failed: {e:#}"))],
            TaskOutput::Check(results) => {
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
        }
    }

    fn on_key(&mut self, k: KeyEvent) -> Vec<Action> {
        let ctx = Ctx {
            ws: &self.ws,
            snap: &self.snap,
            theme: &self.theme,
        };
        if let Some(m) = self.modal.as_mut() {
            return m.handle_key(k, &ctx);
        }
        let in_search_input = self.tab == Tab::Search && self.search.input_focused();
        match (k.code, k.modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => return vec![Action::Quit],
            (KeyCode::Char('r'), KeyModifiers::CONTROL) => {
                return vec![Action::Rescan, Action::Toast("rescanning".into())];
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
            Action::Quit => self.quit = true,
            Action::Toast(t) => self.toast(t, Level::Ok),
            Action::Error(t) => self.toast(t, Level::Error),
            Action::Rescan => self.rescan(),
            Action::Spawn(task) => self.spawn(task),
            Action::OpenModal(m) => self.modal = Some(*m),
            Action::CloseModal => self.modal = None,
            Action::SwitchTab(t) => {
                self.tab = t;
                if t == Tab::Search {
                    self.search.focus_input();
                }
            }
            Action::Search { query, focus_list } => {
                self.tab = Tab::Search;
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
                    Ok(n) => self.toast(format!("{title}: {n} change(s)"), Level::Ok),
                    Err(e) => self.toast(format!("{title} failed: {e:#}"), Level::Error),
                }
                self.rescan();
            }
            Action::ConfirmLinks { title, actions } => {
                self.modal = Some(Modal::confirm(title, actions));
            }
            Action::Write(f) => {
                match f(&self.ws) {
                    Ok(msg) => self.toast(msg, Level::Ok),
                    Err(e) => self.toast(format!("{e:#}"), Level::Error),
                }
                self.rescan();
            }
        }
    }

    fn spawn(&mut self, task: Task) {
        self.tasks_running += 1;
        spawn_task(self.ws.clone(), task, self.tx.clone());
    }

    pub fn rescan(&mut self) {
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
        let mut spans: Vec<Span> = Vec::new();
        let mut left_w = 0usize;
        if let Some(t) = &self.toast {
            let style = match t.level {
                Level::Info => th.dim(),
                Level::Ok => th.ok(),
                Level::Error => th.err(),
            };
            let text = format!(
                " {} ",
                fit(&t.text, (area.width as usize).saturating_sub(2))
            );
            left_w = width(&text);
            spans.push(Span::styled(text, style));
        }
        let mut hint_spans: Vec<Span> = Vec::new();
        let mut hint_w = 0usize;
        for (key, desc) in hints {
            let piece_w = width(key) + width(desc) + 3;
            if left_w + hint_w + piece_w + 1 > area.width as usize {
                break;
            }
            hint_spans.push(Span::styled(*key, th.key_hint()));
            hint_spans.push(Span::styled(format!(" {desc}  "), th.dim()));
            hint_w += piece_w;
        }
        let pad = (area.width as usize).saturating_sub(left_w + hint_w);
        spans.push(Span::raw(" ".repeat(pad)));
        spans.extend(hint_spans);
        f.render_widget(Paragraph::new(Line::from(spans)), area);
    }
}

/// Key hint pairs shown in the footer.
pub type Hints = &'static [(&'static str, &'static str)];
