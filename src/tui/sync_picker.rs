//! Configure and run root-wide Git synchronization.
use super::{
    app::{Action, Ctx, Hints},
    components::choice_footer::{self, ChoiceEvent, ChoiceFocus},
    event::Task,
    modal::Modal,
    widgets::{Input, ListNav, OverlayClear, centered, fit},
};
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Span},
    widgets::{List, ListItem, Paragraph, Wrap},
};
use skills::ops::sync::{Mode, Report, Settings};

#[derive(Debug, Clone)]
pub struct Request {
    pub mode: Mode,
    pub dry_run: bool,
}

#[derive(Clone, Copy)]
enum OverviewAction {
    Configure,
    Sync(Mode),
    Disable,
}

enum Step {
    Overview,
    Configure { fields: [Input; 2], field: usize },
    Preview { request: Request, report: Report },
    Disable,
}

pub struct SyncPicker {
    settings: Settings,
    step: Step,
    list: ListNav,
    focus: ChoiceFocus,
    rect: Rect,
    field_rects: [Rect; 2],
    buttons: [Rect; 2],
}
impl SyncPicker {
    pub fn new(ctx: &Ctx) -> anyhow::Result<Self> {
        let mut picker = Self {
            settings: Settings::load(ctx.ws)?,
            step: Step::Overview,
            list: ListNav::default(),
            focus: ChoiceFocus::List,
            rect: Rect::default(),
            field_rects: [Rect::default(); 2],
            buttons: [Rect::default(); 2],
        };
        picker.list.first(picker.actions().len());
        Ok(picker)
    }

    pub fn preview(ctx: &Ctx, request: Request, report: Report) -> anyhow::Result<Self> {
        let mut picker = Self::new(ctx)?;
        picker.step = Step::Preview { request, report };
        picker.list.first(picker.row_count());
        Ok(picker)
    }

    fn configured(&self) -> bool {
        self.settings.url.is_some() && self.settings.branch.is_some()
    }

    fn actions(&self) -> Vec<OverviewAction> {
        if !self.configured() {
            vec![OverviewAction::Configure]
        } else {
            vec![
                OverviewAction::Sync(Mode::Sync),
                OverviewAction::Sync(Mode::Push),
                OverviewAction::Sync(Mode::Pull),
                OverviewAction::Configure,
                OverviewAction::Disable,
            ]
        }
    }

    fn row_count(&self) -> usize {
        match &self.step {
            Step::Overview => self.actions().len(),
            Step::Preview { report, .. } => report.status.lines().count().max(1),
            Step::Configure { .. } => 2,
            Step::Disable => 1,
        }
    }

    pub fn hints(&self) -> Hints {
        match self.step {
            Step::Configure { .. } => &[
                ("Tab", "next field"),
                ("Enter", "continue"),
                ("Esc", "back"),
            ],
            Step::Preview { .. } | Step::Disable => {
                &[("Tab", "buttons"), ("Enter", "choose"), ("Esc", "back")]
            }
            Step::Overview if !self.configured() => {
                &[("↑↓", "choose"), ("Enter", "set up"), ("Esc", "close")]
            }
            Step::Overview => &[
                ("↑↓", "choose"),
                ("Enter", "open"),
                ("s/p/P", "sync / push / pull"),
                ("Esc", "close"),
            ],
        }
    }

    pub fn paste(&mut self, text: &str) -> Vec<Action> {
        if let Step::Configure { fields, field } = &mut self.step
            && let Err(e) = fields[*field].paste(text)
        {
            return vec![Action::Error(e.into())];
        }
        vec![]
    }

    fn back(&mut self) -> Vec<Action> {
        if matches!(self.step, Step::Overview) {
            return vec![Action::CloseModal];
        }
        self.step = Step::Overview;
        self.focus = ChoiceFocus::List;
        self.list.first(self.actions().len());
        vec![]
    }

    fn configure(&mut self) {
        self.step = Step::Configure {
            fields: [
                Input::with_value(self.settings.url.as_deref().unwrap_or("")),
                Input::with_value(self.settings.branch.as_deref().unwrap_or("main")),
            ],
            field: 0,
        };
        self.focus = ChoiceFocus::List;
    }

    fn run(request: Request) -> Vec<Action> {
        vec![Action::CloseModal, Action::Spawn(Task::Sync(request))]
    }

    fn activate_overview(&mut self) -> Vec<Action> {
        let Some(action) = self
            .list
            .selected()
            .and_then(|index| self.actions().get(index).copied())
        else {
            return vec![];
        };
        match action {
            OverviewAction::Configure => {
                self.configure();
                vec![]
            }
            OverviewAction::Disable => {
                self.step = Step::Disable;
                self.focus = ChoiceFocus::List;
                self.list.first(1);
                vec![]
            }
            OverviewAction::Sync(mode) => Self::run(Request {
                mode,
                dry_run: true,
            }),
        }
    }

    fn primary(&mut self) -> Vec<Action> {
        match &self.step {
            Step::Overview => self.activate_overview(),
            Step::Preview { request, .. } => Self::run(Request {
                dry_run: false,
                ..request.clone()
            }),
            Step::Disable => vec![Action::CloseModal, Action::Spawn(Task::SyncDisable)],
            Step::Configure { fields, .. } => {
                let url = fields[0].value().trim().to_string();
                let branch = fields[1].value().trim().to_string();
                if url.is_empty() {
                    return vec![Action::Error("Remote URL is required".into())];
                }
                if branch.is_empty() {
                    return vec![Action::Error("Branch is required".into())];
                }
                vec![
                    Action::CloseModal,
                    Action::Spawn(Task::SyncConfigure { url, branch }),
                ]
            }
        }
    }

    fn handle_choice(&mut self, key: KeyCode) -> Option<Vec<Action>> {
        let at_end = self.row_count() == 0
            || self.list.selected() == Some(self.row_count().saturating_sub(1));
        self.focus.key(key, at_end).map(|event| match event {
            ChoiceEvent::Apply => self.primary(),
            ChoiceEvent::Cancel => self.back(),
            ChoiceEvent::Moved => vec![],
        })
    }

    pub fn key(&mut self, key: KeyEvent, _: &Ctx) -> Vec<Action> {
        if key.code == KeyCode::Esc {
            return self.back();
        }
        if let Step::Configure { fields, field } = &mut self.step {
            match self.focus {
                ChoiceFocus::Apply | ChoiceFocus::Cancel => {
                    if let Some(actions) = self.handle_choice(key.code) {
                        return actions;
                    }
                }
                ChoiceFocus::List => match key.code {
                    KeyCode::Tab | KeyCode::BackTab => {
                        if (key.code == KeyCode::Tab && *field == 0)
                            || (key.code == KeyCode::BackTab && *field == 1)
                        {
                            *field = 1 - *field;
                        } else {
                            self.focus = if key.code == KeyCode::Tab {
                                ChoiceFocus::Apply
                            } else {
                                ChoiceFocus::Cancel
                            };
                        }
                    }
                    KeyCode::Up => *field = 0,
                    KeyCode::Down => *field = 1,
                    KeyCode::Enter if *field == 0 => *field = 1,
                    KeyCode::Enter => self.focus = ChoiceFocus::Apply,
                    _ => {
                        fields[*field].handle_key(key);
                    }
                },
            }
            return vec![];
        }
        if let Some(actions) = self.handle_choice(key.code) {
            return actions;
        }
        match key.code {
            KeyCode::Down => self.list.move_by(1, self.row_count()),
            KeyCode::Up => self.list.move_by(-1, self.row_count()),
            KeyCode::PageDown => self.list.move_by(10, self.row_count()),
            KeyCode::PageUp => self.list.move_by(-10, self.row_count()),
            KeyCode::Home => self.list.first(self.row_count()),
            KeyCode::End => self.list.last(self.row_count()),
            KeyCode::Enter if matches!(self.step, Step::Overview) => {
                return self.activate_overview();
            }
            KeyCode::Enter if matches!(self.step, Step::Preview { .. } | Step::Disable) => {
                self.focus = ChoiceFocus::Apply;
            }
            KeyCode::Char('a') if matches!(self.step, Step::Overview) => self.configure(),
            KeyCode::Char('d') if matches!(self.step, Step::Overview) && self.configured() => {
                self.step = Step::Disable;
                self.focus = ChoiceFocus::List;
            }
            KeyCode::Char('s' | 'p' | 'P')
                if matches!(self.step, Step::Overview) && self.configured() =>
            {
                return Self::run(Request {
                    mode: match key.code {
                        KeyCode::Char('p') => Mode::Push,
                        KeyCode::Char('P') => Mode::Pull,
                        _ => Mode::Sync,
                    },
                    dry_run: true,
                });
            }
            KeyCode::Char('y') if matches!(self.step, Step::Preview { .. }) => {
                return self.primary();
            }
            _ => {}
        }
        vec![]
    }

    pub fn mouse(&mut self, event: MouseEvent, _: &Ctx) -> Vec<Action> {
        let at = ratatui::layout::Position::new(event.column, event.row);
        match event.kind {
            MouseEventKind::ScrollDown => self.list.move_by(3, self.row_count()),
            MouseEventKind::ScrollUp => self.list.move_by(-3, self.row_count()),
            MouseEventKind::Down(MouseButton::Left) if !self.rect.contains(at) => {
                return self.back();
            }
            MouseEventKind::Down(MouseButton::Left) if self.buttons[0].contains(at) => {
                return self.primary();
            }
            MouseEventKind::Down(MouseButton::Left) if self.buttons[1].contains(at) => {
                return self.back();
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if let Step::Configure { field, fields } = &mut self.step {
                    if let Some(index) = self.field_rects.iter().position(|rect| rect.contains(at))
                    {
                        *field = index;
                        self.focus = ChoiceFocus::List;
                        fields[index].click(event.column);
                    }
                } else if let Some((_, double)) = self.list.click(event.row, self.row_count()) {
                    self.focus = ChoiceFocus::List;
                    if double && matches!(self.step, Step::Overview) {
                        return self.activate_overview();
                    }
                }
            }
            _ => {}
        }
        vec![]
    }

    pub fn draw(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        let height = match self.step {
            Step::Configure { .. } => 17,
            Step::Preview { .. } | Step::Overview => 21,
            Step::Disable => 13,
        };
        let area = centered(area, 82, height);
        self.rect = area;
        f.render_widget(OverlayClear, area);
        let title = match self.step {
            Step::Overview => " Root backup ",
            Step::Configure { .. } => " Root backup · configure ",
            Step::Preview { .. } => " Root backup · review ",
            Step::Disable => " Root backup · turn off automatic sync ",
        };
        let th = &ctx.settings.theme;
        let block = th.block(title, true);
        let inner = block.inner(area);
        f.render_widget(block, area);
        self.buttons = [Rect::default(); 2];
        self.field_rects = [Rect::default(); 2];
        let minimum_height = match self.step {
            Step::Configure { .. } => 13,
            Step::Overview | Step::Preview { .. } => 8,
            Step::Disable => 7,
        };
        if inner.width < 26 || inner.height < minimum_height {
            f.render_widget(Paragraph::new("Enlarge terminal · Esc back"), inner);
            return;
        }
        match &mut self.step {
            Step::Overview => {
                let configured = self.configured();
                let status = if self.settings.enabled {
                    "● Automatic sync is on"
                } else if configured {
                    "○ Automatic sync is off"
                } else {
                    "○ Backup is not configured"
                };
                f.render_widget(
                    Paragraph::new(status).style(if self.settings.enabled {
                        th.ok()
                    } else {
                        th.warn()
                    }),
                    Rect::new(inner.x + 1, inner.y, inner.width - 2, 1),
                );
                let details = if configured {
                    format!(
                        "Remote  {}\nBranch  {}",
                        fit(
                            self.settings.url.as_deref().unwrap_or_default(),
                            inner.width.saturating_sub(10) as usize
                        ),
                        self.settings.branch.as_deref().unwrap_or("—")
                    )
                } else {
                    "Set a remote repository and branch to back up this skills root.".into()
                };
                f.render_widget(
                    Paragraph::new(details).style(th.dim()),
                    Rect::new(inner.x + 1, inner.y + 2, inner.width - 2, 2),
                );
                let actions = self.actions();
                self.list.rows = Rect::new(
                    inner.x + 1,
                    inner.y + 5,
                    inner.width - 2,
                    inner.height.saturating_sub(8),
                );
                self.list.item_height = 1;
                self.list.clamp(actions.len());
                let rows = actions
                    .iter()
                    .map(|action| {
                        let (label, tail) = match action {
                            OverviewAction::Configure if configured => {
                                ("Configure backup", "settings")
                            }
                            OverviewAction::Configure => ("Set up backup", "recommended"),
                            OverviewAction::Sync(Mode::Sync) => ("Sync now", "recommended"),
                            OverviewAction::Sync(Mode::Push) => {
                                ("Push local changes only", "advanced")
                            }
                            OverviewAction::Sync(Mode::Pull) => {
                                ("Pull remote changes only", "advanced")
                            }
                            OverviewAction::Disable => {
                                ("Turn off automatic sync", "keeps Git history")
                            }
                        };
                        ListItem::new(Line::from(vec![
                            Span::raw(format!("  {label}")),
                            Span::styled(format!("  {tail}"), th.description()),
                        ]))
                    })
                    .collect::<Vec<_>>();
                f.render_stateful_widget(
                    List::new(rows).highlight_style(if self.focus == ChoiceFocus::List {
                        th.selected()
                    } else {
                        th.selected_unfocused()
                    }),
                    self.list.rows,
                    &mut self.list.state,
                );
                f.render_widget(
                    Paragraph::new("Sync saves local work, merges remote updates, then pushes. Conflicts stop without force/reset.")
                        .wrap(Wrap { trim: true })
                        .style(th.description()),
                    Rect::new(inner.x + 1, inner.bottom() - 3, inner.width - 2, 2),
                );
                self.buttons = choice_footer::draw_with_labels(
                    f,
                    Rect::new(inner.x, inner.bottom() - 1, inner.width, 1),
                    self.focus,
                    true,
                    "↑↓ choose",
                    ("Open", "Close"),
                    th,
                );
            }
            Step::Configure { fields, field } => {
                f.render_widget(
                    Paragraph::new("Back up the entire skills root to one Git remote.")
                        .style(th.bold()),
                    Rect::new(inner.x + 1, inner.y, inner.width - 2, 1),
                );
                for (index, label) in ["Remote URL", "Branch"].iter().enumerate() {
                    let y = inner.y + 2 + index as u16 * 3;
                    f.render_widget(
                        Paragraph::new(*label).style(th.dim()),
                        Rect::new(inner.x + 1, y, inner.width - 2, 1),
                    );
                    let outer = Rect::new(inner.x + 1, y + 1, inner.width - 2, 1);
                    self.field_rects[index] = outer;
                    fields[index].render(
                        f,
                        outer,
                        self.focus == ChoiceFocus::List && *field == index,
                        if index == 0 {
                            "https://… or /path/to/repo.git"
                        } else {
                            "main"
                        },
                        th,
                    );
                }
                f.render_widget(
                    Paragraph::new("Skills, metadata, notes, presets and machine paths are shared. Deletions propagate; runtime files stay local.")
                        .wrap(Wrap { trim: true })
                        .style(th.description()),
                    Rect::new(inner.x + 1, inner.bottom() - 5, inner.width - 2, 3),
                );
                let ready =
                    !fields[0].value().trim().is_empty() && !fields[1].value().trim().is_empty();
                self.buttons = choice_footer::draw_with_labels(
                    f,
                    Rect::new(inner.x, inner.bottom() - 2, inner.width, 2),
                    self.focus,
                    ready,
                    "Configuration is stored in local Git settings",
                    ("Enable", "Back"),
                    th,
                );
            }
            Step::Preview { request, report } => {
                let status = report.status.lines().collect::<Vec<_>>();
                f.render_widget(
                    Paragraph::new(format!(
                        "{:?} · {} local change{}",
                        request.mode,
                        status.len(),
                        if status.len() == 1 { "" } else { "s" }
                    ))
                    .style(if status.is_empty() {
                        th.ok()
                    } else {
                        th.warn()
                    }),
                    Rect::new(inner.x + 1, inner.y, inner.width - 2, 1),
                );
                self.list.rows = Rect::new(
                    inner.x + 1,
                    inner.y + 2,
                    inner.width - 2,
                    inner.height.saturating_sub(8),
                );
                self.list.item_height = 1;
                self.list.clamp(status.len().max(1));
                let rows = if status.is_empty() {
                    vec![ListItem::new("  Working tree is clean")]
                } else {
                    status
                        .iter()
                        .map(|line| {
                            ListItem::new(format!(
                                "  {}",
                                fit(line, inner.width.saturating_sub(4) as usize)
                            ))
                        })
                        .collect()
                };
                f.render_stateful_widget(
                    List::new(rows).highlight_style(if self.focus == ChoiceFocus::List {
                        th.selected()
                    } else {
                        th.selected_unfocused()
                    }),
                    self.list.rows,
                    &mut self.list.state,
                );
                f.render_widget(
                    Paragraph::new("Plan: commit local changes → fetch and merge remote updates → push. The remote is checked again before writing.")
                        .wrap(Wrap { trim: true })
                        .style(th.description()),
                    Rect::new(inner.x + 1, inner.bottom() - 5, inner.width - 2, 3),
                );
                self.buttons = choice_footer::draw_with_labels(
                    f,
                    Rect::new(inner.x, inner.bottom() - 2, inner.width, 2),
                    self.focus,
                    true,
                    "Conflicts preserve the local backup commit",
                    ("✓ Run sync", "Back"),
                    th,
                );
            }
            Step::Disable => {
                f.render_widget(
                    Paragraph::new(vec![
                        Line::from(Span::styled(
                            "Automatic synchronization will stop.",
                            th.warn(),
                        )),
                        Line::from(""),
                        Line::from("The Git history, remote and current files remain unchanged. You can still sync manually or enable it again later."),
                    ])
                    .wrap(Wrap { trim: true }),
                    Rect::new(
                        inner.x + 1,
                        inner.y + 1,
                        inner.width - 2,
                        inner.height.saturating_sub(4),
                    ),
                );
                self.list.rows = Rect::new(inner.x + 1, inner.y + 1, inner.width - 2, 1);
                self.buttons = choice_footer::draw_with_labels(
                    f,
                    Rect::new(inner.x, inner.bottom() - 2, inner.width, 2),
                    self.focus,
                    true,
                    "Git history is retained",
                    ("Turn off", "Back"),
                    th,
                );
            }
        }
    }
}
pub fn open(ctx: &Ctx) -> Modal {
    match SyncPicker::new(ctx) {
        Ok(p) => Modal::Sync(Box::new(p)),
        Err(e) => Modal::message("Root sync", vec![format!("{e:#}")]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, crossterm::event::KeyModifiers::NONE)
    }

    fn context<'a>(
        ws: &'a skills::Workspace,
        snap: &'a skills::reconcile::Snapshot,
        settings: &'a crate::tui::settings::RuntimeSettings,
    ) -> Ctx<'a> {
        Ctx { ws, snap, settings }
    }

    fn render(picker: &mut SyncPicker, ctx: &Ctx, width: u16, height: u16) -> String {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| picker.draw(f, f.area(), ctx)).unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn unconfigured_overview_offers_setup_and_keyboard_back() {
        let temp = skills::ops::DownloadDir::new("root-sync-dialog").unwrap();
        let ws = skills::Workspace::open(temp.path()).unwrap();
        let snap = ws.scan().unwrap();
        let settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
        let ctx = context(&ws, &snap, &settings);
        let mut picker = SyncPicker::new(&ctx).unwrap();
        assert_eq!(picker.actions().len(), 1);
        let screen = render(&mut picker, &ctx, 100, 28);
        assert!(screen.contains("Backup is not configured"));
        assert!(screen.contains("Set up backup"));
        assert!(!screen.contains("Sync now"));
        for shortcut in ['s', 'p', 'P', 'd'] {
            assert!(picker.key(key(KeyCode::Char(shortcut)), &ctx).is_empty());
            assert!(matches!(picker.step, Step::Overview));
        }

        assert!(picker.key(key(KeyCode::Enter), &ctx).is_empty());
        assert!(matches!(picker.step, Step::Configure { .. }));
        assert!(picker.key(key(KeyCode::Esc), &ctx).is_empty());
        assert!(matches!(picker.step, Step::Overview));
        assert!(matches!(
            picker.key(key(KeyCode::Esc), &ctx).as_slice(),
            [Action::CloseModal]
        ));
    }

    #[test]
    fn configured_overview_navigates_all_actions_and_keeps_shortcuts() {
        let temp = skills::ops::DownloadDir::new("root-sync-actions").unwrap();
        let ws = skills::Workspace::open(temp.path()).unwrap();
        let snap = ws.scan().unwrap();
        let settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
        let ctx = context(&ws, &snap, &settings);
        let mut picker = SyncPicker::new(&ctx).unwrap();
        picker.settings = Settings {
            url: Some("/tmp/remote.git".into()),
            branch: Some("main".into()),
            enabled: true,
        };
        picker.list.first(picker.actions().len());

        let screen = render(&mut picker, &ctx, 100, 28);
        for label in [
            "Sync now",
            "Push local changes only",
            "Pull remote changes only",
            "Configure backup",
            "Turn off automatic sync",
        ] {
            assert!(screen.contains(label), "{label}");
        }
        picker.key(key(KeyCode::Down), &ctx);
        assert!(matches!(
            picker.key(key(KeyCode::Enter), &ctx).as_slice(),
            [
                Action::CloseModal,
                Action::Spawn(Task::Sync(Request {
                    mode: Mode::Push,
                    dry_run: true
                }))
            ]
        ));

        for (shortcut, mode) in [('s', Mode::Sync), ('p', Mode::Push), ('P', Mode::Pull)] {
            let mut picker = SyncPicker::new(&ctx).unwrap();
            picker.settings.url = Some("/tmp/remote.git".into());
            picker.settings.branch = Some("main".into());
            assert!(matches!(
                picker.key(key(KeyCode::Char(shortcut)), &ctx).as_slice(),
                [
                    Action::CloseModal,
                    Action::Spawn(Task::Sync(Request {
                        mode: actual,
                        dry_run: true
                    }))
                ] if std::mem::discriminant(actual) == std::mem::discriminant(&mode)
            ));
        }
    }

    #[test]
    fn preview_requires_explicit_run_sync_confirmation() {
        let temp = skills::ops::DownloadDir::new("root-sync-preview").unwrap();
        let ws = skills::Workspace::open(temp.path()).unwrap();
        let snap = ws.scan().unwrap();
        let settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
        let ctx = context(&ws, &snap, &settings);
        let mut picker = SyncPicker::preview(
            &ctx,
            Request {
                mode: Mode::Sync,
                dry_run: true,
            },
            Report::default(),
        )
        .unwrap();
        assert!(picker.key(key(KeyCode::Enter), &ctx).is_empty());
        assert_eq!(picker.focus, ChoiceFocus::Apply);
        assert!(matches!(
            picker.key(key(KeyCode::Enter), &ctx).as_slice(),
            [
                Action::CloseModal,
                Action::Spawn(Task::Sync(Request { dry_run: false, .. }))
            ]
        ));
        let mut picker = SyncPicker::preview(
            &ctx,
            Request {
                mode: Mode::Sync,
                dry_run: true,
            },
            Report::default(),
        )
        .unwrap();
        assert!(matches!(
            picker.key(key(KeyCode::Char('y')), &ctx).as_slice(),
            [
                Action::CloseModal,
                Action::Spawn(Task::Sync(Request { dry_run: false, .. }))
            ]
        ));
        for (width, height) in [(80, 24), (48, 12)] {
            assert!(render(&mut picker, &ctx, width, height).contains("Root backup"));
        }
        assert!(!ws.root.join(".git").exists());
    }

    #[test]
    fn configure_and_disable_submit_background_tasks_without_writing() {
        let temp = skills::ops::DownloadDir::new("root-sync-background").unwrap();
        let ws = skills::Workspace::open(temp.path()).unwrap();
        let snap = ws.scan().unwrap();
        let settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
        let ctx = context(&ws, &snap, &settings);
        let mut picker = SyncPicker::new(&ctx).unwrap();
        picker.configure();
        let Step::Configure { fields, field } = &mut picker.step else {
            panic!("configure step expected");
        };
        fields[0].set("/tmp/remote.git");
        *field = 1;
        picker.focus = ChoiceFocus::Apply;
        assert!(matches!(
            picker.key(key(KeyCode::Enter), &ctx).as_slice(),
            [
                Action::CloseModal,
                Action::Spawn(Task::SyncConfigure { url, branch })
            ] if url == "/tmp/remote.git" && branch == "main"
        ));
        assert!(!ws.root.join(".git").exists());

        picker.configure();
        assert!(render(&mut picker, &ctx, 60, 14).contains("Enlarge terminal"));

        picker.step = Step::Disable;
        picker.focus = ChoiceFocus::Apply;
        assert!(matches!(
            picker.key(key(KeyCode::Enter), &ctx).as_slice(),
            [Action::CloseModal, Action::Spawn(Task::SyncDisable)]
        ));
        assert!(!ws.root.join(".git").exists());
    }

    #[test]
    fn mouse_selects_and_double_clicks_an_overview_action() {
        let temp = skills::ops::DownloadDir::new("root-sync-mouse").unwrap();
        let ws = skills::Workspace::open(temp.path()).unwrap();
        let snap = ws.scan().unwrap();
        let settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
        let ctx = context(&ws, &snap, &settings);
        let mut picker = SyncPicker::new(&ctx).unwrap();
        picker.settings.url = Some("/tmp/remote.git".into());
        picker.settings.branch = Some("main".into());
        render(&mut picker, &ctx, 100, 28);
        let row = picker.list.rows.y + 2;
        let column = picker.list.rows.x;
        let click = || MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: crossterm::event::KeyModifiers::NONE,
        };
        assert!(picker.mouse(click(), &ctx).is_empty());
        assert_eq!(picker.list.selected(), Some(2));
        assert!(matches!(
            picker.mouse(click(), &ctx).as_slice(),
            [
                Action::CloseModal,
                Action::Spawn(Task::Sync(Request {
                    mode: Mode::Pull,
                    dry_run: true
                }))
            ]
        ));
    }
}
