//! Configure and run root-wide Git synchronization.
//! Mode descriptions keep previews and focused help aligned with sync execution.
//! Compact previews cache the exact cell-wrapped display rows at the current
//! draw width; keyboard and mouse navigation count that same model. Wide
//! previews retain a status list and a separately visible plan.
use super::{
    app::{Action, Ctx, Hints},
    components::choice_footer::{self, ChoiceEvent, ChoiceFocus},
    event::Task,
    sync_coordinator::{SyncActivity, SyncPresentation},
    widgets::{Input, ListNav, PopupClass, PopupSurface, fit, popup_size, popup_too_small, width},
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

struct ModeDescription {
    heading: &'static str,
    primary: &'static str,
    operations: &'static str,
    note: &'static str,
}

impl ModeDescription {
    fn for_mode(mode: Mode) -> Self {
        match mode {
            Mode::Sync => Self {
                heading: "Sync",
                primary: "✓ Run sync",
                operations: "commit local changes if needed → fetch and merge remote updates → push",
                note: "With no local commit, fetch and attach the remote branch first if available. Merge conflicts preserve the local backup commit.",
            },
            Mode::Push => Self {
                heading: "Push",
                primary: "✓ Run push",
                operations: "commit local changes if needed → push local history",
                note: "Push requires a local commit; remote divergence may reject the push.",
            },
            Mode::Pull => Self {
                heading: "Pull",
                primary: "✓ Run pull",
                operations: "commit local changes if needed → fetch and merge remote updates",
                note: "With no local commit, fetch and attach the remote branch first if available. Merge conflicts preserve the local backup commit.",
            },
        }
    }

    fn preview_help(&self) -> String {
        format!("Preview: {}.", self.operations)
    }

    fn apply_help(&self) -> String {
        format!("Run: {}. {}", self.operations, self.note)
    }

    fn plan(&self) -> String {
        format!("Plan: {}. {}", self.operations, self.note)
    }
}

#[derive(Clone, Copy)]
enum OverviewAction {
    Changes,
    Configure,
    Check,
    Sync(Mode),
    Enable,
    Disable,
}

enum Step {
    Overview,
    Configure { fields: [Input; 2], field: usize },
    Preview { request: Request, report: Report },
    Changes,
    Disable,
}

pub struct SyncPicker {
    settings: Settings,
    status: Option<skills::ops::sync::Status>,
    presentation: SyncPresentation,
    checking: bool,
    error: Option<String>,
    step: Step,
    list: ListNav,
    focus: ChoiceFocus,
    rect: Rect,
    field_rects: [Rect; 2],
    buttons: [Rect; 2],
    compact_preview_rows: Option<Vec<String>>,
}
impl SyncPicker {
    pub fn new(ctx: &Ctx) -> anyhow::Result<Self> {
        Self::with_status(
            ctx,
            None,
            SyncPresentation {
                activity: SyncActivity::NotConfigured,
                automatic: false,
                detail: None,
            },
            false,
            None,
        )
    }

    pub fn with_status(
        ctx: &Ctx,
        status: Option<skills::ops::sync::Status>,
        presentation: SyncPresentation,
        checking: bool,
        error: Option<String>,
    ) -> anyhow::Result<Self> {
        let settings = status
            .as_ref()
            .map(|status| status.settings.clone())
            .unwrap_or(Settings::load(ctx.ws)?);
        let mut presentation = presentation;
        if status.is_none() {
            presentation.automatic = settings.enabled;
            presentation.activity = if settings.url.is_some() && settings.branch.is_some() {
                SyncActivity::Ready
            } else {
                SyncActivity::NotConfigured
            };
        }
        let mut picker = Self {
            settings,
            status,
            presentation,
            checking,
            error,
            step: Step::Overview,
            list: ListNav::default(),
            focus: ChoiceFocus::List,
            rect: Rect::default(),
            field_rects: [Rect::default(); 2],
            buttons: [Rect::default(); 2],
            compact_preview_rows: None,
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

    pub fn set_status(&mut self, status: skills::ops::sync::Status, error: Option<String>) {
        self.settings = status.settings.clone();
        self.presentation.automatic = status.settings.enabled;
        self.presentation.activity =
            if status.settings.url.is_none() || status.settings.branch.is_none() {
                SyncActivity::NotConfigured
            } else if !status.changes.is_empty() || status.ahead > 0 || status.behind > 0 {
                SyncActivity::Waiting
            } else {
                SyncActivity::Ready
            };
        self.presentation.detail = error.clone();
        self.status = Some(status);
        self.checking = false;
        self.error = error;
        if matches!(self.step, Step::Overview) {
            self.list.clamp(self.actions().len());
        }
    }

    pub fn set_checking(&mut self, checking: bool) {
        self.checking = checking;
    }

    pub fn set_error(&mut self, error: String) {
        self.error = Some(error);
    }

    fn configured(&self) -> bool {
        self.settings.url.is_some() && self.settings.branch.is_some()
    }

    fn actions(&self) -> Vec<OverviewAction> {
        if !self.configured() {
            vec![OverviewAction::Configure]
        } else {
            let mut actions = vec![OverviewAction::Sync(Mode::Sync)];
            if self
                .status
                .as_ref()
                .is_some_and(|status| !status.changes.is_empty())
            {
                actions.push(OverviewAction::Changes);
            }
            actions.extend([
                OverviewAction::Check,
                OverviewAction::Sync(Mode::Push),
                OverviewAction::Sync(Mode::Pull),
                OverviewAction::Configure,
                if self.presentation.automatic {
                    OverviewAction::Disable
                } else {
                    OverviewAction::Enable
                },
            ]);
            actions
        }
    }

    fn row_count(&self) -> usize {
        match &self.step {
            Step::Overview => self.actions().len(),
            Step::Preview { report, .. } => self
                .compact_preview_rows
                .as_ref()
                .map_or_else(|| report.status.lines().count().max(1), Vec::len),
            Step::Changes => self
                .status
                .as_ref()
                .map_or(0, |status| status.changes.len())
                .max(1),
            Step::Configure { .. } => 2,
            Step::Disable => 1,
        }
    }

    pub fn hints(&self) -> Hints {
        if let Some(hints) = self.focus.button_hints() {
            return hints;
        }

        match self.step {
            Step::Configure { .. } => &[
                ("Tab", "next field"),
                ("Enter", "continue"),
                ("Esc", "back"),
            ],
            Step::Preview { .. } | Step::Changes => {
                &[("↑↓", "scroll"), ("Enter/Tab", "buttons"), ("Esc", "back")]
            }
            Step::Disable => &[("Enter/Tab", "buttons"), ("Esc", "back")],
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

    pub fn footer_help(&self) -> crate::tui::footer::Help<'_> {
        let detail: std::borrow::Cow<'_, str> = match &self.step {
            Step::Overview => self
                .list
                .selected()
                .and_then(|i| self.actions().get(i).copied())
                .map_or("".into(), |action| match action {
                    OverviewAction::Changes => "Review uncommitted Library changes".into(),
                    OverviewAction::Check => {
                        "Fetch remote state without changing Library files".into()
                    }
                    OverviewAction::Configure => {
                        "Configure the Git remote and branch for Library backup".into()
                    }
                    OverviewAction::Sync(mode) => {
                        ModeDescription::for_mode(mode).preview_help().into()
                    }
                    OverviewAction::Enable => {
                        "Enable automatic Library backup and synchronization".into()
                    }
                    OverviewAction::Disable => {
                        "Stop automatic synchronization; keep Git history and files".into()
                    }
                }),
            Step::Configure { field, .. } => if *field == 0 {
                "Git remote URL or local repository path"
            } else {
                "Remote branch used for this Library's backup"
            }
            .into(),
            Step::Preview { request, .. } => ModeDescription::for_mode(request.mode)
                .preview_help()
                .into(),
            Step::Changes => "These changes are committed before the next sync".into(),
            Step::Disable => {
                "Git history and files remain available after automatic sync is disabled".into()
            }
        };
        let apply: std::borrow::Cow<'_, str> = match &self.step {
            Step::Configure { fields, .. }
                if fields.iter().any(|field| field.value().trim().is_empty()) =>
            {
                "Enter both a remote URL and branch before enabling backup".into()
            }
            Step::Configure { .. } => {
                "Save the configuration in local Git settings and enable backup".into()
            }
            Step::Preview { request, .. } => {
                ModeDescription::for_mode(request.mode).apply_help().into()
            }
            Step::Changes => "Return to the backup overview".into(),
            Step::Disable => "Turn off automatic sync while retaining Git history".into(),
            Step::Overview => detail.clone(),
        };
        self.focus.help(
            self.hints(),
            detail,
            apply,
            if matches!(self.step, Step::Overview) {
                "Close backup settings"
            } else {
                "Return to the backup overview without applying changes"
            },
        )
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
        vec![Action::Spawn(Task::Sync(request))]
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
            OverviewAction::Changes => {
                self.step = Step::Changes;
                self.focus = ChoiceFocus::List;
                self.list.first(self.row_count());
                vec![]
            }
            OverviewAction::Check => vec![Action::RefreshSyncStatus {
                remote: true,
                reason: super::sync_coordinator::ProbeReason::Manual,
            }],
            OverviewAction::Configure => {
                self.configure();
                vec![]
            }
            OverviewAction::Enable => {
                vec![Action::Spawn(Task::SyncEnable)]
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
            Step::Changes => self.back(),
            Step::Disable => vec![Action::Spawn(Task::SyncDisable)],
            Step::Configure { fields, .. } => {
                let url = fields[0].value().trim().to_string();
                let branch = fields[1].value().trim().to_string();
                if url.is_empty() {
                    return vec![Action::Error("Remote URL is required".into())];
                }
                if branch.is_empty() {
                    return vec![Action::Error("Branch is required".into())];
                }
                vec![Action::Spawn(Task::SyncConfigure { url, branch })]
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
        if matches!(self.step, Step::Changes) {
            match (self.focus, key.code) {
                (ChoiceFocus::List, KeyCode::Tab | KeyCode::BackTab | KeyCode::Enter) => {
                    self.focus = ChoiceFocus::Cancel;
                    return vec![];
                }
                (ChoiceFocus::List, KeyCode::Down)
                    if self.list.selected() == Some(self.row_count().saturating_sub(1)) =>
                {
                    self.focus = ChoiceFocus::Cancel;
                    return vec![];
                }
                (_, KeyCode::Tab | KeyCode::BackTab | KeyCode::Up)
                    if self.focus != ChoiceFocus::List =>
                {
                    self.focus = ChoiceFocus::List;
                    return vec![];
                }
                (_, KeyCode::Enter | KeyCode::Char(' ')) if self.focus != ChoiceFocus::List => {
                    return self.back();
                }
                _ if self.focus != ChoiceFocus::List => return vec![],
                _ => {}
            }
        } else if let Some(actions) = self.handle_choice(key.code) {
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
            KeyCode::Enter
                if matches!(
                    self.step,
                    Step::Preview { .. } | Step::Changes | Step::Disable
                ) =>
            {
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
        match super::widgets::popup_mouse(event, self.rect) {
            super::widgets::PopupMouse::OutsidePress => return vec![Action::CloseModal],
            super::widgets::PopupMouse::Inside => {}
            _ => return vec![],
        }
        let at = ratatui::layout::Position::new(event.column, event.row);
        match event.kind {
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp
                if self.list.rows.contains(at) && !matches!(self.step, Step::Configure { .. }) =>
            {
                self.focus = ChoiceFocus::List;
                self.list.move_by(
                    if event.kind == MouseEventKind::ScrollDown {
                        3
                    } else {
                        -3
                    },
                    self.row_count(),
                );
            }
            MouseEventKind::Down(MouseButton::Left) if self.buttons[0].contains(at) => {
                self.focus = ChoiceFocus::Apply;
                return self.primary();
            }
            MouseEventKind::Down(MouseButton::Left) if self.buttons[1].contains(at) => {
                self.focus = ChoiceFocus::Cancel;
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
            Step::Preview { ref report, .. } => {
                (report.status.lines().count().clamp(1, 11) as u16) + 10
            }
            Step::Changes => {
                self.status
                    .as_ref()
                    .map_or(1, |s| s.changes.len().clamp(1, 14)) as u16
                    + 7
            }
            Step::Overview => self.actions().len() as u16 + 15,
            Step::Disable => 13,
        };
        let size = popup_size(area, PopupClass::Preview, height, (12, 5));
        let area = size.outer;
        self.rect = area;
        f.render_widget(PopupSurface, area);
        let title = match self.step {
            Step::Overview => " Root backup ",
            Step::Configure { .. } => " Root backup · configure ",
            Step::Preview { .. } => " Root backup · review ",
            Step::Changes => " Root backup · local changes ",
            Step::Disable => " Root backup · turn off automatic sync ",
        };
        let th = &ctx.settings.theme;
        let block = th.block(title, true);
        let inner = block.inner(area);
        f.render_widget(block, area);
        self.buttons = [Rect::default(); 2];
        self.field_rects = [Rect::default(); 2];
        self.list.rows = Rect::default();
        self.compact_preview_rows = None;
        if let Step::Configure { fields, .. } = &mut self.step {
            for field in fields {
                field.clear_area();
            }
        }
        if !size.usable {
            popup_too_small(f, size, th);
            return;
        }
        let minimum_height = match self.step {
            Step::Configure { .. } => 13,
            Step::Overview => 14,
            Step::Preview { .. } => 9,
            Step::Changes => 6,
            Step::Disable => 7,
        };
        if inner.width < 26 || inner.height < minimum_height {
            self.draw_compact(f, inner, ctx);
            return;
        }
        match &mut self.step {
            Step::Overview => {
                let configured = self.configured();
                let icon = |nerd, text| super::icons::decoration(ctx.settings.ui.icons, nerd, text);
                let activity = if self.checking {
                    SyncActivity::Checking
                } else {
                    self.presentation.activity
                };
                let (hero_icon, hero, detail, hero_style) = match activity {
                    SyncActivity::NotConfigured => (
                        icon("", "!"),
                        "Set up backup",
                        "Connect a Git remote",
                        th.warn(),
                    ),
                    SyncActivity::Ready => (
                        icon("", "✓"),
                        "Backed up",
                        "Everything is current",
                        th.ok(),
                    ),
                    SyncActivity::Waiting => (
                        icon("", "↻"),
                        "Sync pending",
                        "Changes are waiting",
                        th.warn(),
                    ),
                    SyncActivity::Checking => (
                        icon("", "?"),
                        "Checking…",
                        "Reading remote state",
                        th.accent(),
                    ),
                    SyncActivity::Syncing => (
                        icon("", "↻"),
                        "Syncing…",
                        "Saving and merging",
                        th.accent(),
                    ),
                    SyncActivity::Retrying => (
                        icon("", "↻"),
                        "Retrying",
                        "Waiting for the next check",
                        th.warn(),
                    ),
                    SyncActivity::Attention => (
                        icon("", "!"),
                        "Needs attention",
                        "Open Git to resolve",
                        th.err(),
                    ),
                };
                let badge = if !configured {
                    " NOT CONFIGURED "
                } else if self.presentation.automatic {
                    icon("  AUTO ON ", " ↻ AUTO ON ")
                } else {
                    " AUTO OFF "
                };
                let badge_width = width(badge) as u16;
                f.render_widget(
                    Paragraph::new(Line::from(vec![Span::styled(
                        format!("{hero_icon}  {hero}"),
                        hero_style,
                    )])),
                    Rect::new(inner.x + 1, inner.y, inner.width - 2, 1),
                );
                f.render_widget(
                    Paragraph::new(badge).style(if self.presentation.automatic {
                        th.ok()
                    } else {
                        th.warn()
                    }),
                    Rect::new(
                        inner.right().saturating_sub(badge_width + 1),
                        inner.y,
                        badge_width,
                        1,
                    ),
                );
                let detail = self.error.as_deref().unwrap_or(detail);
                f.render_widget(
                    Paragraph::new(fit(detail, inner.width.saturating_sub(2) as usize))
                        .style(th.description()),
                    Rect::new(inner.x + 1, inner.y + 1, inner.width - 2, 1),
                );

                let status = self.status.as_ref();
                let changes = status.map_or("○ unknown".into(), |status| {
                    if status.changes.is_empty() {
                        format!("{} clean", icon("", "✓"))
                    } else {
                        format!("{} {} changes", icon("", "!"), status.changes.len())
                    }
                });
                let push = status.map_or(format!("{} unknown", icon("", "↑")), |status| {
                    if status.ahead == 0 {
                        format!("{} 0 to push", icon("", "↑"))
                    } else {
                        format!("{} {} to push", icon("", "↑"), status.ahead)
                    }
                });
                let pull = status.map_or(format!("{} unknown", icon("", "↓")), |status| {
                    if self.checking {
                        format!("{} checking…", icon("", "?"))
                    } else if !status.remote_checked {
                        format!("{} not checked", icon("󰄱", "○"))
                    } else {
                        format!("{} {} to pull", icon("", "↓"), status.behind)
                    }
                });
                let branch = self.settings.branch.as_deref().unwrap_or("—");
                let remote = self.settings.url.as_deref().unwrap_or("No remote");
                let summary_y = inner.y + 3;
                if inner.width >= 100 {
                    let gap = 5;
                    let card_width = (inner.width.saturating_sub(gap + 2)) / 2;
                    let local = Rect::new(inner.x + 1, summary_y, card_width, 5);
                    let remote_card = Rect::new(local.right() + gap, summary_y, card_width, 5);
                    let local_inner = super::components::layout::frame(f, local, false, false, th);
                    let remote_inner =
                        super::components::layout::frame(f, remote_card, false, false, th);
                    f.render_widget(
                        Paragraph::new(Line::from(vec![
                            Span::styled(icon("  LOCAL", "LOCAL"), th.bold()),
                            Span::styled("  Library", th.description()),
                        ])),
                        Rect::new(local_inner.x, local_inner.y, local_inner.width, 1),
                    );
                    f.render_widget(
                        Paragraph::new(format!("{changes}\n{push}")),
                        Rect::new(local_inner.x, local_inner.y + 1, local_inner.width, 2),
                    );
                    f.render_widget(
                        Paragraph::new(Line::from(vec![
                            Span::styled(icon("  REMOTE", "REMOTE"), th.bold()),
                            Span::styled(
                                format!("  {} {branch}", icon("", "branch")),
                                th.description(),
                            ),
                        ])),
                        Rect::new(remote_inner.x, remote_inner.y, remote_inner.width, 1),
                    );
                    f.render_widget(
                        Paragraph::new(format!(
                            "{}\n{pull}",
                            fit(remote, remote_inner.width as usize)
                        )),
                        Rect::new(remote_inner.x, remote_inner.y + 1, remote_inner.width, 2),
                    );
                    f.render_widget(
                        Paragraph::new(icon("", "↻")).style(th.accent()),
                        Rect::new(local.right() + 2, summary_y + 2, 2, 1),
                    );
                } else {
                    f.render_widget(
                        Paragraph::new(Line::from(vec![
                            Span::styled(icon(" LOCAL", "LOCAL"), th.bold()),
                            Span::raw(format!("  {changes} · {push}")),
                        ])),
                        Rect::new(inner.x + 1, summary_y, inner.width - 2, 1),
                    );
                    f.render_widget(
                        Paragraph::new(icon("  ", "  ↻")).style(th.accent()),
                        Rect::new(inner.x + 1, summary_y + 1, inner.width - 2, 1),
                    );
                    f.render_widget(
                        Paragraph::new(Line::from(vec![
                            Span::styled(icon(" REMOTE", "REMOTE"), th.bold()),
                            Span::raw(format!("  {} {branch} · {pull}", icon("", "branch"))),
                        ])),
                        Rect::new(inner.x + 1, summary_y + 2, inner.width - 2, 1),
                    );
                }

                let actions = self.actions();
                self.list.rows = Rect::new(
                    inner.x + 1,
                    inner.y + 9,
                    inner.width - 2,
                    inner.height.saturating_sub(11),
                );
                self.list.item_height = 1;
                self.list.clamp(actions.len());
                let rows = actions
                    .iter()
                    .map(|action| {
                        let (icon, label, tail) = match action {
                            OverviewAction::Changes => ("", "View changes", "local"),
                            OverviewAction::Check => (
                                "",
                                if self.checking {
                                    "Checking…"
                                } else {
                                    "Check remote"
                                },
                                "refresh",
                            ),
                            OverviewAction::Configure if configured => {
                                ("", "Configure", "remote · branch")
                            }
                            OverviewAction::Configure => ("", "Set up backup", "recommended"),
                            OverviewAction::Sync(Mode::Sync) => ("", "Sync now", "recommended"),
                            OverviewAction::Sync(Mode::Push) => ("", "Push only", "advanced"),
                            OverviewAction::Sync(Mode::Pull) => ("", "Pull only", "advanced"),
                            OverviewAction::Enable => ("", "Turn on auto sync", "background"),
                            OverviewAction::Disable => ("", "Turn off auto sync", "keeps history"),
                        };
                        ListItem::new(Line::from(vec![
                            Span::raw(if ctx.settings.ui.icons == skills::config::Icons::Text {
                                format!(" {label}")
                            } else {
                                format!(" {icon}  {label}")
                            }),
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
                self.buttons = choice_footer::draw_with_labels(
                    f,
                    Rect::new(inner.x, inner.bottom() - 1, inner.width, 1),
                    self.focus,
                    true,
                    "",
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
                    "",
                    ("Enable", "Back"),
                    th,
                );
            }
            Step::Preview { request, report } => {
                let description = ModeDescription::for_mode(request.mode);
                let status = report.status.lines().collect::<Vec<_>>();
                f.render_widget(
                    Paragraph::new(format!(
                        "{} · {} local change{}",
                        description.heading,
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
                    Paragraph::new(description.plan())
                        .wrap(Wrap { trim: true })
                        .style(th.description()),
                    Rect::new(inner.x + 1, inner.bottom() - 5, inner.width - 2, 3),
                );
                self.buttons = choice_footer::draw_with_labels(
                    f,
                    Rect::new(inner.x, inner.bottom() - 2, inner.width, 2),
                    self.focus,
                    true,
                    "",
                    (description.primary, "Back"),
                    th,
                );
            }
            Step::Changes => {
                let changes = self
                    .status
                    .as_ref()
                    .map(|status| status.changes.as_slice())
                    .unwrap_or_default();
                f.render_widget(
                    Paragraph::new(format!(
                        "{} uncommitted change{}",
                        changes.len(),
                        if changes.len() == 1 { "" } else { "s" }
                    ))
                    .style(if changes.is_empty() {
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
                    inner.height.saturating_sub(5),
                );
                self.list.item_height = 1;
                self.list.clamp(changes.len().max(1));
                let rows = if changes.is_empty() {
                    vec![ListItem::new("  Working tree is clean")]
                } else {
                    changes
                        .iter()
                        .map(|change| {
                            ListItem::new(format!(
                                "  {}",
                                fit(change, inner.width.saturating_sub(4) as usize)
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
                self.buttons = choice_footer::draw_with_labels(
                    f,
                    Rect::new(inner.x, inner.bottom() - 2, inner.width, 2),
                    self.focus,
                    false,
                    "",
                    ("", "Back"),
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
                    "",
                    ("Turn off", "Back"),
                    th,
                );
            }
        }
    }

    /// Reserve actions first, then retain fields or a selected row in short windows.
    fn draw_compact(&mut self, f: &mut Frame, inner: Rect, ctx: &Ctx) {
        let th = &ctx.settings.theme;
        let overview = self.actions();
        let body = Rect::new(inner.x, inner.y, inner.width, inner.height - 1);
        let (primary, secondary, enabled) = match &mut self.step {
            Step::Configure { fields, field } => {
                for (i, label) in ["Remote URL", "Branch"].iter().enumerate() {
                    let y = inner.y + i as u16 * 2;
                    f.render_widget(
                        Paragraph::new(*label).style(th.dim()),
                        Rect::new(inner.x, y, inner.width, 1),
                    );
                    self.field_rects[i] = Rect::new(inner.x, y + 1, inner.width, 1);
                    fields[i].render(
                        f,
                        self.field_rects[i],
                        self.focus == ChoiceFocus::List && *field == i,
                        if i == 0 {
                            "https://… or /path/to/repo.git"
                        } else {
                            "main"
                        },
                        th,
                    );
                }
                (
                    "Enable",
                    "Back",
                    fields.iter().all(|field| !field.value().trim().is_empty()),
                )
            }
            Step::Disable => {
                f.render_widget(Paragraph::new("Automatic synchronization will stop. Git history and files remain unchanged.").wrap(Wrap { trim: true }), body);
                ("Turn off", "Back", true)
            }
            _ => {
                let (rows, primary, secondary, enabled) = match &self.step {
                    Step::Overview => (
                        overview
                            .iter()
                            .map(|action| {
                                match action {
                                    OverviewAction::Changes => "View changes",
                                    OverviewAction::Check => "Check remote",
                                    OverviewAction::Configure => "Configure",
                                    OverviewAction::Sync(Mode::Sync) => "Sync now",
                                    OverviewAction::Sync(Mode::Push) => "Push only",
                                    OverviewAction::Sync(Mode::Pull) => "Pull only",
                                    OverviewAction::Enable => "Turn on auto sync",
                                    OverviewAction::Disable => "Turn off auto sync",
                                }
                                .to_string()
                            })
                            .collect::<Vec<_>>(),
                        "Open",
                        "Close",
                        true,
                    ),
                    Step::Preview { request, report } => {
                        let description = ModeDescription::for_mode(request.mode);
                        let mut rows = report
                            .status
                            .lines()
                            .map(str::to_string)
                            .collect::<Vec<_>>();
                        if rows.is_empty() {
                            rows.push("Working tree is clean".into());
                        }
                        rows.extend(wrap_plan(&description.plan(), body.width as usize));
                        self.compact_preview_rows = Some(rows.clone());
                        (rows, description.primary, "Back", true)
                    }
                    Step::Changes => {
                        let mut rows = self
                            .status
                            .as_ref()
                            .map(|s| s.changes.clone())
                            .unwrap_or_default();
                        if rows.is_empty() {
                            rows.push("Working tree is clean".into());
                        }
                        (rows, "", "Back", false)
                    }
                    _ => unreachable!(),
                };
                self.list.rows = body;
                self.list.item_height = 1;
                self.list.clamp(rows.len());
                f.render_stateful_widget(
                    List::new(
                        rows.into_iter()
                            .map(|row| ListItem::new(fit(&row, body.width as usize))),
                    )
                    .highlight_style(if self.focus == ChoiceFocus::List {
                        th.selected()
                    } else {
                        th.selected_unfocused()
                    }),
                    body,
                    &mut self.list.state,
                );
                (primary, secondary, enabled)
            }
        };
        self.buttons = choice_footer::draw_with_labels(
            f,
            Rect::new(inner.x, inner.bottom() - 1, inner.width, 1),
            self.focus,
            enabled,
            "",
            (primary, secondary),
            th,
        );
    }
}

/// Wrap without truncating text or splitting graphemes, using terminal cells.
fn wrap_plan(text: &str, columns: usize) -> Vec<String> {
    use unicode_segmentation::UnicodeSegmentation;
    let mut rows = Vec::new();
    let mut row = String::new();
    let mut used = 0;
    for glyph in text.graphemes(true) {
        let cells = width(glyph);
        if used + cells > columns && !row.is_empty() {
            rows.push(std::mem::take(&mut row));
            used = 0;
        }
        row.push_str(glyph);
        used += cells;
    }
    if !row.is_empty() {
        rows.push(row);
    }
    rows
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_dirty_preview_reaches_entire_plan_by_keys_wheel_and_click() {
        use std::collections::BTreeMap;
        let temp = skills::ops::DownloadDir::new("backup-plan-navigation").unwrap();
        skills::config::Config {
            agents: vec![],
            ..Default::default()
        }
        .save(temp.path())
        .unwrap();
        let ws = skills::Workspace::open(temp.path()).unwrap();
        let snap = ws.scan().unwrap();
        let settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
        let ctx = context(&ws, &snap, &settings);
        let normalize = |text: &str| {
            text.chars()
                .filter(|c| !c.is_whitespace())
                .collect::<String>()
        };
        for mode in [Mode::Sync, Mode::Push, Mode::Pull] {
            for wheel in [false, true] {
                let mut picker = SyncPicker::preview(
                    &ctx,
                    Request {
                        mode,
                        dry_run: true,
                    },
                    Report {
                        status: (0..8).map(|i| format!(" M dirty-路径-{i}\n")).collect(),
                        ..Default::default()
                    },
                )
                .unwrap();
                let mut terminal =
                    ratatui::Terminal::new(ratatui::backend::TestBackend::new(40, 12)).unwrap();
                let mut seen = BTreeMap::new();
                for _ in 0..100 {
                    terminal
                        .draw(|f| picker.draw(f, Rect::new(0, 0, 40, 10), &ctx))
                        .unwrap();
                    for y in 0..picker.list.rows.height {
                        let text: String = (picker.list.rows.x..picker.list.rows.right())
                            .map(|x| {
                                terminal.backend().buffer()[(x, picker.list.rows.y + y)].symbol()
                            })
                            .collect();
                        seen.insert(picker.list.state.offset() + y as usize, text);
                    }
                    if picker.list.selected() == Some(picker.row_count() - 1) {
                        break;
                    }
                    let actions = if wheel {
                        picker.mouse(
                            MouseEvent {
                                kind: MouseEventKind::ScrollDown,
                                column: picker.list.rows.x,
                                row: picker.list.rows.y,
                                modifiers: crossterm::event::KeyModifiers::NONE,
                            },
                            &ctx,
                        )
                    } else {
                        picker.key(key(KeyCode::Down), &ctx)
                    };
                    assert!(actions.is_empty(), "navigation cannot execute a backup");
                }
                assert!(
                    picker.row_count() > 8,
                    "plan must be navigable display rows"
                );
                let displayed = seen.into_values().collect::<String>();
                let plan = ModeDescription::for_mode(mode).plan();
                assert!(
                    normalize(&displayed).contains(&normalize(&plan)),
                    "{mode:?}, wheel={wheel}: {displayed}"
                );
                let last = picker.row_count() - 1;
                assert_eq!(picker.list.selected(), Some(last));
                let row = picker.list.rows.y + (last - picker.list.state.offset()) as u16;
                assert!(
                    picker
                        .mouse(
                            MouseEvent {
                                kind: MouseEventKind::Down(MouseButton::Left),
                                column: picker.list.rows.x,
                                row,
                                modifiers: crossterm::event::KeyModifiers::NONE,
                            },
                            &ctx
                        )
                        .is_empty()
                );
                assert_eq!(picker.list.selected(), Some(last));
                assert!(picker.key(key(KeyCode::Down), &ctx).is_empty());
                assert_eq!(picker.focus, ChoiceFocus::Apply);
                assert!(matches!(picker.key(key(KeyCode::Enter), &ctx).as_slice(),
                    [Action::Spawn(Task::Sync(Request { mode: actual, dry_run: false }))]
                    if std::mem::discriminant(actual) == std::mem::discriminant(&mode)));
                picker.focus = ChoiceFocus::List;
                let mut wide =
                    ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 30)).unwrap();
                wide.draw(|f| picker.draw(f, Rect::new(0, 0, 120, 28), &ctx))
                    .unwrap();
                assert_eq!(picker.row_count(), 8);
                assert_eq!(picker.list.selected(), Some(7));
                let screen: String = wide
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .map(|c| c.symbol())
                    .collect();
                assert!(screen.contains("Plan:"));
                picker.key(key(KeyCode::Home), &ctx);
                assert_eq!(picker.list.selected(), Some(0));
                picker.key(key(KeyCode::End), &ctx);
                assert_eq!(picker.list.selected(), Some(7));
            }
        }
        assert!(!ws.root.join(".git").exists());
    }

    #[test]
    fn icon_mode_covers_whole_backup_overviews() {
        use skills::config::Icons;
        let root = skills::ops::DownloadDir::new("backup-icon-mode").unwrap();
        skills::config::Config {
            agents: vec![],
            ..Default::default()
        }
        .save(root.path())
        .unwrap();
        let ws = skills::Workspace::open(root.path()).unwrap();
        let snap = ws.scan().unwrap();
        let mut settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
        for mode in [Icons::Text, Icons::Nerd] {
            settings.ui.icons = mode;
            let ctx = context(&ws, &snap, &settings);
            for activity in [
                SyncActivity::NotConfigured,
                SyncActivity::Ready,
                SyncActivity::Waiting,
                SyncActivity::Checking,
                SyncActivity::Syncing,
                SyncActivity::Retrying,
                SyncActivity::Attention,
            ] {
                for (configured, automatic, variant) in [
                    (false, false, 0),
                    (true, false, 0),
                    (true, true, 1),
                    (true, true, 2),
                    (true, true, 3),
                ] {
                    let mut picker = SyncPicker::new(&ctx).unwrap();
                    picker.settings.url =
                        configured.then(|| "https://example.test/backup.git".into());
                    picker.settings.branch = configured.then(|| "main".into());
                    picker.presentation = SyncPresentation {
                        activity,
                        automatic,
                        detail: None,
                    };
                    picker.error =
                        (activity == SyncActivity::Attention).then(|| "Remote unavailable".into());
                    picker.checking = activity == SyncActivity::Checking;
                    if variant > 0 {
                        picker.status = Some(skills::ops::sync::Status {
                            changes: if variant == 1 {
                                vec![]
                            } else {
                                vec![" M example".into()]
                            },
                            ahead: variant - 1,
                            behind: variant,
                            remote_checked: variant != 3,
                            ..Default::default()
                        });
                    }
                    for columns in [60, 100, 180] {
                        let screen = render(&mut picker, &ctx, columns, 40);
                        assert!(screen.contains("Root backup"));
                        assert!(screen.contains("Configure") || screen.contains("Set up backup"));
                        assert_eq!(
                            crate::tui::icons::tests::has_pua(&screen),
                            mode == Icons::Nerd,
                            "mode={mode:?}, width={columns}, variant={variant}: {screen}"
                        );
                        if activity == SyncActivity::Attention {
                            assert!(screen.contains("Remote unavailable"));
                        }
                        if configured {
                            assert!(screen.contains("AUTO ON") || screen.contains("AUTO OFF"));
                            assert!(screen.contains("Push only") && screen.contains("Pull only"));
                            assert!(screen.contains(if automatic {
                                "Turn off auto sync"
                            } else {
                                "Turn on auto sync"
                            }));
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn responsive_sync_keeps_fields_lists_and_actions_in_bounds() {
        let tmp = skills::ops::DownloadDir::new("sync-responsive").unwrap();
        skills::config::Config {
            agents: vec![],
            ..Default::default()
        }
        .save(tmp.path())
        .unwrap();
        let ws = skills::Workspace::open(tmp.path()).unwrap();
        let snap = ws.scan().unwrap();
        let settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
        let ctx = context(&ws, &snap, &settings);
        for step in 0..5 {
            let mut picker = SyncPicker::new(&ctx).unwrap();
            match step {
                1 => picker.configure(),
                2 => {
                    picker.step = Step::Preview {
                        request: Request {
                            mode: Mode::Sync,
                            dry_run: true,
                        },
                        report: Report {
                            status: " M example\n".into(),
                            ..Default::default()
                        },
                    }
                }
                3 => picker.step = Step::Changes,
                4 => picker.step = Step::Disable,
                _ => {}
            }
            for (width, height) in [(160, 40), (112, 32), (60, 20), (40, 12), (4, 3), (1, 1)] {
                let mut terminal =
                    ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))
                        .unwrap();
                let area = Rect::new(0, 0, width, height.saturating_sub(2));
                terminal.draw(|f| picker.draw(f, area, &ctx)).unwrap();
                for hit in picker
                    .buttons
                    .into_iter()
                    .chain(picker.field_rects)
                    .chain([picker.list.rows])
                {
                    assert!(hit.is_empty() || hit.intersection(area) == hit);
                }
                if width == 40 {
                    assert!(!picker.buttons[1].is_empty());
                    if step == 1 {
                        assert!(picker.field_rects.iter().all(|hit| !hit.is_empty()));
                    } else if step != 4 {
                        assert!(picker.list.rows.height > 0);
                    }
                }
                if width < 4 {
                    assert_eq!(picker.list.rows, Rect::default());
                }
            }
        }
    }

    #[test]
    fn each_mode_describes_its_operations_and_dispatches_the_reviewed_request() {
        let root = skills::ops::DownloadDir::new("sync-mode-preview").unwrap();
        skills::config::Config {
            agents: vec![],
            ..Default::default()
        }
        .save(root.path())
        .unwrap();
        let ws = skills::Workspace::open(root.path()).unwrap();
        let snap = ws.scan().unwrap();
        let settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
        let ctx = context(&ws, &snap, &settings);
        for (mode, heading, button, merges, pushes) in [
            (Mode::Sync, "Sync", "✓ Run sync", true, true),
            (Mode::Push, "Push", "✓ Run push", false, true),
            (Mode::Pull, "Pull", "✓ Run pull", true, false),
        ] {
            let assert_operations = |text: &str| {
                assert!(text.contains("commit local changes if needed"), "{text}");
                assert_eq!(text.contains("fetch"), merges, "{text}");
                assert_eq!(text.contains("merge"), merges, "{text}");
                assert_eq!(text.contains("push"), pushes, "{text}");
            };
            let assert_request = |actions: Vec<Action>, dry_run| {
                assert!(matches!(
                    actions.as_slice(),
                    [Action::Spawn(Task::Sync(Request { mode: actual, dry_run: actual_dry_run }))]
                        if std::mem::discriminant(actual) == std::mem::discriminant(&mode)
                            && *actual_dry_run == dry_run
                ));
            };
            let mut overview = SyncPicker::new(&ctx).unwrap();
            overview.settings.url = Some("unused-local-remote".into());
            overview.settings.branch = Some("main".into());
            let index = overview
                .actions()
                .iter()
                .position(|action| {
                    matches!(
                        action,
                        OverviewAction::Sync(actual)
                            if std::mem::discriminant(actual) == std::mem::discriminant(&mode)
                    )
                })
                .unwrap();
            overview.list.select(Some(index));
            assert_operations(&overview.footer_help().detail);
            overview.focus = ChoiceFocus::Apply;
            assert_operations(&overview.footer_help().detail);
            assert_request(overview.key(key(KeyCode::Enter), &ctx), true);

            for status in ["", " M example/SKILL.md\n?? new-skill/\n"] {
                let mut picker = SyncPicker::preview(
                    &ctx,
                    Request {
                        mode,
                        dry_run: true,
                    },
                    Report {
                        status: status.into(),
                        preview: true,
                        ..Default::default()
                    },
                )
                .unwrap();
                let screen = render(&mut picker, &ctx, 120, 28);
                let changes = if status.is_empty() {
                    "0 local changes"
                } else {
                    "2 local changes"
                };
                assert!(
                    screen.contains(&format!("{heading} · {changes}")),
                    "{screen}"
                );
                assert!(screen.contains(button), "{screen}");
                assert_operations(&screen);
                if merges {
                    assert!(screen.contains("attach"), "{screen}");
                    assert!(screen.contains("remote branch first"), "{screen}");
                    assert!(screen.contains("local backup commit"), "{screen}");
                }
                assert_operations(&picker.footer_help().detail);
                assert!(picker.key(key(KeyCode::Enter), &ctx).is_empty());
                assert_eq!(picker.focus, ChoiceFocus::Apply);
                assert_operations(&picker.footer_help().detail);
                assert_request(picker.key(key(KeyCode::Enter), &ctx), false);
                picker.focus = ChoiceFocus::List;
                assert_request(picker.key(key(KeyCode::Char('y')), &ctx), false);

                picker.focus = ChoiceFocus::Cancel;
                assert!(
                    picker
                        .footer_help()
                        .detail
                        .contains("without applying changes")
                );
                assert!(picker.key(key(KeyCode::Enter), &ctx).is_empty());
                assert!(matches!(picker.step, Step::Overview));
            }

            let mut picker = SyncPicker::preview(
                &ctx,
                Request {
                    mode,
                    dry_run: true,
                },
                Report::default(),
            )
            .unwrap();
            render(&mut picker, &ctx, 120, 28);
            assert_request(
                picker.mouse(
                    MouseEvent {
                        kind: MouseEventKind::Down(MouseButton::Left),
                        column: picker.buttons[0].x,
                        row: picker.buttons[0].y,
                        modifiers: crossterm::event::KeyModifiers::NONE,
                    },
                    &ctx,
                ),
                false,
            );
        }
        assert!(
            !ws.root.join(".git").exists(),
            "preview UI must not run Git"
        );
    }

    #[test]
    fn local_changes_has_one_real_back_button_and_matching_footer_help() {
        let root = skills::ops::DownloadDir::new("sync-focus").unwrap();
        skills::config::Config {
            agents: vec![],
            ..Default::default()
        }
        .save(root.path())
        .unwrap();
        let ws = skills::Workspace::open(root.path()).unwrap();
        let snap = ws.scan().unwrap();
        let settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
        let ctx = context(&ws, &snap, &settings);
        let mut picker = SyncPicker::new(&ctx).unwrap();
        picker.step = Step::Changes;
        picker.list.first(picker.row_count());
        render(&mut picker, &ctx, 80, 24);
        assert_eq!(picker.buttons[0].width, 0, "no empty Apply button");
        assert!(picker.footer_help().detail.contains("committed before"));
        picker.key(key(KeyCode::Tab), &ctx);
        assert_eq!(picker.focus, ChoiceFocus::Cancel);
        assert!(
            picker
                .footer_help()
                .detail
                .contains("Return to the backup overview")
        );
        picker.key(key(KeyCode::Up), &ctx);
        assert_eq!(picker.focus, ChoiceFocus::List);
        picker.key(key(KeyCode::Enter), &ctx);
        assert_eq!(picker.focus, ChoiceFocus::Cancel);
        picker.key(key(KeyCode::Enter), &ctx);
        assert!(matches!(picker.step, Step::Overview));
    }

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
        assert!(screen.contains("NOT CONFIGURED"));
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
    fn startup_check_uses_local_auto_setting_before_status_arrives() {
        let temp = skills::ops::DownloadDir::new("root-sync-startup-state").unwrap();
        let ws = skills::Workspace::open(temp.path()).unwrap();
        skills::ops::sync::configure(&ws, "/tmp/remote.git", "main").unwrap();
        let snap = ws.scan().unwrap();
        let settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
        let ctx = context(&ws, &snap, &settings);
        let mut picker = SyncPicker::with_status(
            &ctx,
            None,
            SyncPresentation {
                activity: SyncActivity::NotConfigured,
                automatic: false,
                detail: None,
            },
            true,
            None,
        )
        .unwrap();

        let screen = render(&mut picker, &ctx, 100, 28);
        assert!(screen.contains("AUTO ON"), "{screen}");
        assert!(!screen.contains("AUTO OFF"), "{screen}");
        assert!(screen.contains("Checking"), "{screen}");
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
        picker.presentation.automatic = true;
        picker.list.first(picker.actions().len());

        let screen = render(&mut picker, &ctx, 100, 28);
        for label in [
            "Sync now",
            "Push only",
            "Pull only",
            "Configure",
            "Turn off auto sync",
        ] {
            assert!(screen.contains(label), "{label}");
        }
        picker.key(key(KeyCode::Down), &ctx);
        picker.key(key(KeyCode::Down), &ctx);
        assert!(matches!(
            picker.key(key(KeyCode::Enter), &ctx).as_slice(),
            [Action::Spawn(Task::Sync(Request {
                mode: Mode::Push,
                dry_run: true
            }))]
        ));

        for (shortcut, mode) in [('s', Mode::Sync), ('p', Mode::Push), ('P', Mode::Pull)] {
            let mut picker = SyncPicker::new(&ctx).unwrap();
            picker.settings.url = Some("/tmp/remote.git".into());
            picker.settings.branch = Some("main".into());
            assert!(matches!(
                picker.key(key(KeyCode::Char(shortcut)), &ctx).as_slice(),
                [Action::Spawn(Task::Sync(Request {
                        mode: actual,
                        dry_run: true
                    }))] if std::mem::discriminant(actual) == std::mem::discriminant(&mode)
            ));
        }
    }

    #[test]
    fn configured_auto_off_offers_enable_instead_of_disable() {
        let temp = skills::ops::DownloadDir::new("root-sync-enable-action").unwrap();
        let ws = skills::Workspace::open(temp.path()).unwrap();
        let snap = ws.scan().unwrap();
        let settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
        let ctx = context(&ws, &snap, &settings);
        let mut picker = SyncPicker::new(&ctx).unwrap();
        picker.settings.url = Some("/tmp/remote.git".into());
        picker.settings.branch = Some("main".into());
        picker.presentation.automatic = false;
        picker.list.select(Some(picker.actions().len() - 1));

        let screen = render(&mut picker, &ctx, 100, 28);
        assert!(screen.contains("AUTO OFF"), "{screen}");
        assert!(screen.contains("Turn on auto sync"), "{screen}");
        assert!(!screen.contains("Turn off auto sync"), "{screen}");
        assert!(matches!(
            picker.key(key(KeyCode::Enter), &ctx).as_slice(),
            [Action::Spawn(Task::SyncEnable)]
        ));
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
            [Action::Spawn(Task::Sync(Request { dry_run: false, .. }))]
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
            [Action::Spawn(Task::Sync(Request { dry_run: false, .. }))]
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
            [Action::Spawn(Task::SyncConfigure { url, branch })] if url == "/tmp/remote.git" && branch == "main"
        ));
        assert!(!ws.root.join(".git").exists());

        picker.configure();
        assert!(render(&mut picker, &ctx, 60, 14).contains("Remote URL"));

        picker.step = Step::Disable;
        picker.focus = ChoiceFocus::Apply;
        assert!(matches!(
            picker.key(key(KeyCode::Enter), &ctx).as_slice(),
            [Action::Spawn(Task::SyncDisable)]
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
        let row = picker.list.rows.y + 3;
        let column = picker.list.rows.x;
        let click = || MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: crossterm::event::KeyModifiers::NONE,
        };
        assert!(picker.mouse(click(), &ctx).is_empty());
        assert_eq!(picker.list.selected(), Some(3));
        assert!(matches!(
            picker.mouse(click(), &ctx).as_slice(),
            [Action::Spawn(Task::Sync(Request {
                mode: Mode::Pull,
                dry_run: true
            }))]
        ));
        assert!(picker.mouse(click(), &ctx).is_empty());
        for button in [MouseButton::Left, MouseButton::Right, MouseButton::Middle] {
            picker.configure();
            render(&mut picker, &ctx, 100, 28);
            let outside = MouseEvent {
                column: 0,
                row: 0,
                kind: MouseEventKind::ScrollDown,
                ..click()
            };
            assert!(picker.mouse(outside, &ctx).is_empty());
            assert!(matches!(picker.step, Step::Configure { .. }));
            assert!(matches!(
                picker
                    .mouse(
                        MouseEvent {
                            kind: MouseEventKind::Down(button),
                            ..outside
                        },
                        &ctx
                    )
                    .as_slice(),
                [Action::CloseModal]
            ));
            assert!(picker.key(key(KeyCode::Esc), &ctx).is_empty());
            assert!(matches!(picker.step, Step::Overview));
        }
    }
}
