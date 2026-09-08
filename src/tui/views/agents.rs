//! Agents tab: what a given agent actually has, and the presets that fill it.
//!
//! The page always looks at exactly one agent. Rolling several together needs
//! a preset to be counted against a set of agents rather than one, which reads
//! as a fraction of a fraction and answers nothing an agent's own page does not.
//! Entries are split by who owns them: skills linked from the central root are
//! ours to add and remove, anything else the agent brought itself is shown but
//! never written to.

use super::cards::{self, CARD_H, cols_for, frame, rule, skill_card};
use super::matrix::Matrix;
use super::preview::Overlay;
use super::{View, wheel};
use crate::tui::app::{Action, Ctx, Hints, Tab};
use crate::tui::modal::Modal;
use crate::tui::widgets::{CardGrid, fit, pad, width};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState};
use skills::ops::deploy::{
    self, PresetState, PresetStatus, plan_preset_activate, plan_preset_deactivate, preset_status,
};
use skills::preset::Preset;

use skills::reconcile::{AgentDirMode, EntryState};

/// The three bands of the page, top to bottom. Arrows move between them, so
/// there is never a control the keyboard cannot reach. `[` and `]` still switch
/// agent from anywhere, since that is the frame everything else sits in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Agents,
    Presets,
    Entries,
}

/// One entry of the agent's directory: a skill name and what shape it is in.
/// The two groups used to carry captions of their own; each card now says which
/// group it is in, which is what let the whole list become a grid.
struct Row<'a> {
    name: &'a str,
    state: Option<&'a EntryState>,
    managed: bool,
}

/// Which repairs an entry admits. Kept one per row from the last refresh so
/// the footer can be filtered without a context in hand, the way the Health
/// page keeps its own. The two never hold at once: an entry is either a link
/// or a directory.
#[derive(Debug, Clone, Copy, Default)]
struct Caps {
    /// A link with nothing behind it; removing it loses nothing.
    clean: bool,
    managed: bool,
    /// The agent's own copy, byte for byte what the root has.
    relink: bool,
}

impl Caps {
    fn of(state: Option<&EntryState>) -> Caps {
        Caps {
            managed: matches!(state, Some(EntryState::Deployed)),
            clean: matches!(state, Some(EntryState::Broken { .. })),
            relink: matches!(state, Some(EntryState::Shadow { same_content: true })),
        }
    }
}

#[derive(Default)]
pub struct AgentsView {
    /// Key of the agent on show. Empty only while none is configured.
    scope: String,
    focus: FocusState,
    presets: Vec<(Preset, PresetStatus)>,
    preset_cursor: usize,
    preset_offset: usize,
    entries: CardGrid,
    /// One per entry row, in the grid's order.
    caps: Vec<Caps>,
    /// One line per entry instead of a card. A session switch only: the shape
    /// of this page is not something the config decides.
    compact: bool,
    scope_rects: Vec<(Rect, String)>,
    preset_rects: Vec<(usize, Rect)>,
    /// The one column the entry scrollbar occupies, empty while it all fits.
    entries_track: Rect,
    left: Rect,
    /// An entry opened for reading, over the page rather than instead of it.
    preview: Overlay,
    /// The whole preset × agent picture, over the page.
    matrix: Matrix,
}

/// `Focus` needs a default for `#[derive(Default)]` on the view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FocusState(Focus);

impl Default for FocusState {
    fn default() -> Self {
        FocusState(Focus::Presets)
    }
}

impl AgentsView {
    /// An open matrix owns keyboard input before application shortcuts.
    pub fn handle_matrix_key(&mut self, k: KeyEvent, ctx: &Ctx) -> Option<Vec<Action>> {
        self.matrix.handle_key(k, ctx)
    }

    /// The scope as the planners want it: one key, or nothing at all.
    fn scope_agents(&self) -> Vec<String> {
        if self.scope.is_empty() {
            Vec::new()
        } else {
            vec![self.scope.clone()]
        }
    }

    fn focus(&self) -> Focus {
        self.focus.0
    }

    fn set_focus(&mut self, f: Focus) {
        self.focus = FocusState(f);
    }

    /// Entry rows for the current scope, grouped into ours and theirs.
    /// Ours first, then the agent's own; alphabetical inside each group.
    fn rows<'a>(&self, ctx: &'a Ctx) -> Vec<Row<'a>> {
        let Some(report) = ctx.snap.agent(&self.scope) else {
            return Vec::new();
        };
        let mut names: Vec<&str> = report.entries.keys().map(String::as_str).collect();
        names.sort_unstable();
        let (mut managed, mut local): (Vec<Row>, Vec<Row>) = (Vec::new(), Vec::new());
        for name in names {
            let state = report.entries.get(name);
            let is_managed = matches!(state, Some(EntryState::Deployed));
            let row = Row {
                name,
                state,
                managed: is_managed,
            };
            if is_managed {
                managed.push(row)
            } else {
                local.push(row)
            }
        }
        managed.append(&mut local);
        managed
    }

    fn select_skills(&self, ctx: &Ctx, checked: Option<String>) -> Vec<Action> {
        let keys: Vec<_> = self
            .rows(ctx)
            .into_iter()
            .filter(|row| row.managed)
            .filter_map(|row| {
                ctx.snap
                    .skills
                    .iter()
                    .find(|skill| skill.deployment_name() == row.name)
            })
            .map(|skill| skill.key.clone())
            .collect();
        if keys.is_empty() {
            return vec![];
        }
        vec![Action::SelectAgentSkills {
            keys,
            title: format!("Agent: {}", self.scope),
            agent: self.scope.clone(),
            checked,
        }]
    }

    fn preview_entry(&mut self, ctx: &Ctx) {
        let rows = self.rows(ctx);
        let Some(row) = self.entries.selected().and_then(|i| rows.get(i)) else {
            return;
        };
        let Some(agent) = ctx.snap.agent(&self.scope) else {
            return;
        };
        self.preview.open_agent(
            row.name.to_string(),
            agent.name.clone(),
            agent.skills_dir.join(row.name),
            if row.managed {
                "managed link to central skill".into()
            } else {
                row.state
                    .map(entry_note)
                    .unwrap_or_else(|| "unknown".into())
            },
        );
    }

    fn selected_preset(&self) -> Option<&(Preset, PresetStatus)> {
        self.presets.get(self.preset_cursor)
    }

    fn selected_caps(&self) -> Caps {
        self.entries
            .selected()
            .and_then(|i| self.caps.get(i))
            .copied()
            .unwrap_or_default()
    }

    /// Plan one repair on the selected entry and put it up for confirmation.
    /// Both repairs delete something — a link with nothing behind it, or a
    /// copy the root already has — so unlike a pill they stop to show the
    /// plan first. The planner is handed the one name, so the plan is exactly
    /// the row in hand and nothing beside it.
    fn repair(&self, ctx: &Ctx, rows: &[Row], clean: bool) -> Vec<Action> {
        let Some(row) = self.entries.selected().and_then(|i| rows.get(i)) else {
            return vec![Action::Error("nothing selected".into())];
        };
        let caps = Caps::of(row.state);
        if clean && !caps.clean {
            return vec![Action::Error(format!(
                "{}: clean applies to links whose target is gone",
                row.name
            ))];
        }
        if !clean && !caps.relink {
            return vec![Action::Error(format!(
                "{}: relink applies to the agent's own copies that match the root",
                row.name
            ))];
        }
        let skill = vec![row.name.to_string()];
        let plan = if clean {
            deploy::plan_clean(ctx.ws, ctx.snap, &self.scope, &skill)
        } else {
            deploy::plan_relink(ctx.ws, ctx.snap, &self.scope, &skill)
        };
        match plan {
            Ok(actions) => vec![Action::ConfirmLinks {
                title: format!(
                    "{} {}/{}",
                    if clean { "clean" } else { "relink" },
                    self.scope,
                    row.name
                ),
                actions,
            }],
            Err(e) => vec![Action::Error(format!("{e:#}"))],
        }
    }

    fn activate(&self, ctx: &Ctx, on: bool) -> Vec<Action> {
        if !self
            .preset_rects
            .iter()
            .any(|(i, _)| *i == self.preset_cursor)
        {
            return vec![];
        }
        let Some((preset, status)) = self.selected_preset() else {
            return vec![Action::Error("no preset here yet".into())];
        };
        let scope = self.scope_agents();
        let plan = if on {
            plan_preset_activate(ctx.ws, ctx.snap, preset, &scope)
        } else {
            plan_preset_deactivate(ctx.ws, ctx.snap, preset, &scope)
        };
        match plan {
            // A pill is a switch, and a switch that stops to ask is a bad
            // switch. What happened is said in the notification, and undo takes
            // it back; a dialog here would only be in the way of trying things.
            Ok(actions) => vec![Action::ApplyLinks {
                title: format!(
                    "{} {} · {}",
                    if on { "deploy" } else { "undeploy" },
                    preset.name,
                    self.scope
                ),
                actions,
            }],
            Err(e) => {
                let _ = status;
                vec![Action::Error(format!("{e:#}"))]
            }
        }
    }

    /// Section labels carry the focus: the active one is accented, the rest dim.
    fn label_style(&self, section: Focus, th: &crate::tui::theme::Theme) -> Style {
        if self.focus() == section {
            th.accent().add_modifier(ratatui::style::Modifier::BOLD)
        } else {
            th.dim()
        }
    }

    fn move_scope(&mut self, delta: i32, ctx: &Ctx) {
        let keys = ctx.ws.config.agent_keys();
        if keys.is_empty() {
            return;
        }
        let cur = keys.iter().position(|x| *x == self.scope).unwrap_or(0) as i32;
        let next = (cur + delta).rem_euclid(keys.len() as i32);
        self.scope = keys[next as usize].clone();
    }
}

impl View for AgentsView {
    /// Coming back to this page puts the keyboard on the pills, whatever it
    /// was doing when the user left: that band is what the page is for, and a
    /// focus left in the grid is invisible until Enter does the wrong thing.
    fn enter(&mut self) {
        self.set_focus(Focus::Presets);
        self.preview.close();
        self.matrix.close();
    }

    fn refresh(&mut self, ctx: &Ctx) {
        // A scope pinned to an agent that is gone from the config, or never set,
        // falls back to the first one there is.
        if ctx.ws.config.agent(&self.scope).is_none() {
            self.scope = ctx
                .ws
                .config
                .agent_keys()
                .first()
                .cloned()
                .unwrap_or_default();
        }
        let scope = self.scope_agents();
        self.presets = ctx
            .ws
            .presets
            .list()
            .unwrap_or_default()
            .into_iter()
            .map(|p| {
                let st = preset_status(ctx.snap, &p, &scope);
                (p, st)
            })
            .collect();
        self.preset_cursor = self.preset_cursor.min(self.presets.len().saturating_sub(1));
        let rows = self.rows(ctx);
        self.caps = rows.iter().map(|r| Caps::of(r.state)).collect();
        self.entries.clamp(rows.len());
    }

    fn handle_key(&mut self, k: KeyEvent, ctx: &Ctx) -> Vec<Action> {
        if self.preview.handle_key(k) {
            return vec![];
        }
        if let Some(acts) = self.matrix.handle_key(k, ctx) {
            return acts;
        }
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        match k.code {
            KeyCode::Char('q') => return vec![Action::SwitchTab(Tab::Search)],
            KeyCode::Char('M') => {
                self.matrix.open(ctx);
                return vec![];
            }
            KeyCode::Esc => return vec![Action::SwitchTab(Tab::Search)],
            // Scope is switchable from anywhere: it frames everything else.
            KeyCode::Char('[') => {
                self.move_scope(-1, ctx);
                return vec![Action::Rescan];
            }
            KeyCode::Char(']') => {
                self.move_scope(1, ctx);
                return vec![Action::Rescan];
            }
            KeyCode::Char('v') => {
                self.compact = !self.compact;
                return vec![];
            }
            KeyCode::Char('s') => {
                return match deploy::plan_sync(ctx.ws, ctx.snap) {
                    Ok(actions) => vec![Action::ConfirmLinks {
                        title: "sync agents to the desired state".into(),
                        actions,
                    }],
                    Err(e) => vec![Action::Error(format!("{e:#}"))],
                };
            }
            KeyCode::Char('c') => {
                let agent = self.scope.clone();
                if agent.is_empty() {
                    return vec![Action::Error("no agent is configured".into())];
                }
                return match deploy::plan_convert(ctx.ws, ctx.snap, &agent) {
                    Ok(actions) => vec![Action::ConfirmLinks {
                        title: format!("convert {agent} to per-skill links"),
                        actions,
                    }],
                    Err(e) => vec![Action::Error(format!("{e:#}"))],
                };
            }
            _ => {}
        }
        match self.focus() {
            Focus::Agents => match k.code {
                KeyCode::Left | KeyCode::Char('h') => {
                    self.move_scope(-1, ctx);
                    vec![Action::Rescan]
                }
                KeyCode::Right | KeyCode::Char('l') => {
                    self.move_scope(1, ctx);
                    vec![Action::Rescan]
                }
                KeyCode::Down | KeyCode::Char('j') | KeyCode::Enter | KeyCode::Tab => {
                    self.set_focus(Focus::Presets);
                    vec![]
                }
                _ => vec![],
            },
            Focus::Presets => match k.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    self.set_focus(Focus::Agents);
                    vec![]
                }
                KeyCode::Left | KeyCode::Char('h') => {
                    self.preset_cursor = self.preset_cursor.saturating_sub(1);
                    vec![]
                }
                KeyCode::Right | KeyCode::Char('l') => {
                    self.preset_cursor =
                        (self.preset_cursor + 1).min(self.presets.len().saturating_sub(1));
                    vec![]
                }
                KeyCode::Enter if shift => self.activate(ctx, false),
                KeyCode::Enter | KeyCode::Char(' ') => {
                    let on = !matches!(
                        self.selected_preset().map(|(_, st)| st.state()),
                        Some(PresetState::Active)
                    );
                    self.activate(ctx, on)
                }
                KeyCode::Char('x') | KeyCode::Backspace => self.activate(ctx, false),
                // Focus follows the arrows rather than a separate key, so there
                // is nothing invisible to remember.
                KeyCode::Down | KeyCode::Char('j') => {
                    self.set_focus(Focus::Entries);
                    let rows = self.rows(ctx);
                    self.entries.clamp(rows.len());
                    vec![]
                }
                _ => vec![],
            },
            Focus::Entries => {
                let rows = self.rows(ctx);
                let n = rows.len();
                match k.code {
                    KeyCode::Char('m') => return self.select_skills(ctx, None),
                    // Down and up cross a whole row of cards; left and right
                    // walk along one, and only mean anything once there is more
                    // than one column to walk.
                    KeyCode::Down | KeyCode::Char('j') => {
                        self.entries.move_rows(1, n);
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        if self.entries.selected().unwrap_or(0) < self.entries.cols() {
                            self.set_focus(Focus::Presets);
                        } else {
                            self.entries.move_rows(-1, n);
                        }
                    }
                    KeyCode::Right => self.entries.move_by(1, n),
                    // On a copy that can be relinked, `l` is the relink key;
                    // everywhere else it is the vim way of moving right. The
                    // footer says which it is on the row in hand, and the
                    // arrow still moves regardless.
                    KeyCode::Char('l') => {
                        if self.selected_caps().relink {
                            return self.repair(ctx, &rows, false);
                        }
                        self.entries.move_by(1, n)
                    }
                    KeyCode::Left | KeyCode::Char('h') => self.entries.move_by(-1, n),
                    KeyCode::Char('x') => return self.repair(ctx, &rows, true),
                    KeyCode::Home | KeyCode::Char('g') => {
                        self.entries.first(n);
                    }
                    KeyCode::End | KeyCode::Char('G') => {
                        self.entries.last(n);
                    }
                    KeyCode::Enter => self.preview_entry(ctx),
                    // Only an entry the root knows nothing about can be taken
                    // in; everything else here is either already ours or the
                    // agent's to keep.
                    KeyCode::Char('a') => {
                        let Some(row) = self.entries.selected().and_then(|i| rows.get(i)) else {
                            return vec![Action::Error("nothing selected".into())];
                        };
                        if !matches!(row.state, Some(EntryState::AgentOnly)) {
                            return vec![Action::Error(format!(
                                "{}: adopt applies to entries the root does not have",
                                row.name
                            ))];
                        }
                        let Some(report) = ctx.snap.agent(&self.scope) else {
                            return vec![];
                        };
                        return vec![Action::OpenModal(Box::new(Modal::adopt(
                            &self.scope,
                            row.name,
                            report.skills_dir.join(row.name),
                        )))];
                    }
                    _ => {}
                }
                vec![]
            }
        }
    }

    fn handle_mouse(&mut self, m: MouseEvent, ctx: &Ctx) -> Vec<Action> {
        if self.preview.handle_mouse(m) {
            return vec![];
        }
        if let Some(acts) = self.matrix.handle_mouse(m, ctx) {
            return acts;
        }
        let at = (m.column, m.row).into();
        if let Some(d) = wheel(&m) {
            if self.left.contains(at) {
                self.set_focus(Focus::Entries);
                // A wheel notch is a row of cards, however many are on it.
                self.entries.move_rows(d.signum(), self.rows(ctx).len());
            }
            return vec![];
        }
        // Dragging the thumb reads the same as clicking the track: both put the
        // selection where the pointer is.
        if matches!(
            m.kind,
            MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Drag(MouseButton::Left)
        ) && self.entries_track.contains(at)
        {
            let n = self.entries.grid_rows();
            if n > 0 {
                let span = self.entries_track.height.max(1) as usize;
                let r = (m.row.saturating_sub(self.entries_track.y)) as usize * n / span;
                self.set_focus(Focus::Entries);
                self.entries.select_row(r.min(n - 1));
            }
            return vec![];
        }
        if let MouseEventKind::Down(MouseButton::Left) = m.kind {
            if let Some((_, scope)) = self
                .scope_rects
                .iter()
                .find(|(r, _)| r.contains(at))
                .map(|(r, s)| (*r, s.clone()))
            {
                self.scope = scope;
                self.set_focus(Focus::Agents);
                return vec![Action::Rescan];
            }
            if let Some((i, _)) = self.preset_rects.iter().find(|(_, r)| r.contains(at)) {
                self.preset_cursor = *i;
                self.set_focus(Focus::Presets);
                // A pill is a switch: clicking an installed one takes it off
                // again rather than re-running an install that has nothing to do.
                // Shift forces removal whatever the state.
                let on = !m.modifiers.contains(KeyModifiers::SHIFT)
                    && !matches!(
                        self.selected_preset().map(|(_, st)| st.state()),
                        Some(PresetState::Active)
                    );
                return self.activate(ctx, on);
            }
            if self.left.contains(at) {
                self.set_focus(Focus::Entries);
                if let Some((index, double)) = self.entries.click(m.column, m.row) {
                    if !self.compact
                        && self.entries.cell(index).is_some_and(|cell| {
                            m.row == cell.y + 1 && (cell.x + 2..cell.x + 5).contains(&m.column)
                        })
                    {
                        let rows = self.rows(ctx);
                        if let Some(row) = rows.get(index).filter(|row| row.managed)
                            && let Some(skill) = ctx
                                .snap
                                .skills
                                .iter()
                                .find(|skill| skill.deployment_name() == row.name)
                        {
                            return self.select_skills(ctx, Some(skill.key.clone()));
                        }
                    }
                    if double {
                        self.preview_entry(ctx);
                    }
                }
            }
        }
        vec![]
    }

    fn draw(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        let th = ctx.theme;
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(4),
                Constraint::Length(2),
                Constraint::Min(1),
            ])
            .split(area);

        // Agent picker. Big enough to aim at, and it takes the keyboard like
        // anything else on the page rather than hiding behind a bracket key.
        self.scope_rects.clear();
        let mut x = rows[0].x + 1;
        if ctx.ws.config.agents.is_empty() {
            f.render_widget(
                Paragraph::new(Span::styled(" no agent configured", th.dim())),
                rows[0],
            );
        }
        for a in &ctx.ws.config.agents {
            let report = ctx.snap.agent(&a.key);
            let sub = match report.map(|r| {
                (
                    &r.mode,
                    r.count(|s| matches!(s, EntryState::Deployed)),
                    r.entries.len(),
                )
            }) {
                None | Some((AgentDirMode::Missing, ..)) => "no directory".to_string(),
                Some((AgentDirMode::DirLinked, ..)) => "whole dir linked".into(),
                Some((AgentDirMode::DirForeign { .. }, ..)) => "dir links elsewhere".into(),
                Some((AgentDirMode::Real, linked, total)) if total > linked => {
                    format!("{linked} linked · {} own", total - linked)
                }
                Some((AgentDirMode::Real, linked, _)) => format!("{linked} linked"),
            };
            let name = a.display_name().to_string();
            let w = (width(&name).max(width(&sub)) + 4).max(14) as u16;
            if x + w > rows[0].right() {
                break;
            }
            let rect = Rect::new(x, rows[0].y, w, 4);
            let on = a.key == self.scope;
            let holding = on && self.focus() == Focus::Agents;
            let border = if on { th.accent() } else { th.dim() };
            let block = ratatui::widgets::Block::default()
                .borders(ratatui::widgets::Borders::ALL)
                .border_type(if holding {
                    ratatui::widgets::BorderType::Thick
                } else {
                    ratatui::widgets::BorderType::Rounded
                })
                .border_style(border);
            let inner = block.inner(rect).inner(ratatui::layout::Margin {
                horizontal: 1,
                vertical: 0,
            });
            f.render_widget(block, rect);
            let title = if on {
                th.bold().fg(th.accent)
            } else {
                th.dim()
            };
            f.render_widget(
                Paragraph::new(vec![
                    Line::from(Span::styled(name, title)),
                    Line::from(Span::styled(sub, th.dim())),
                ]),
                inner,
            );
            self.scope_rects.push((rect, a.key.clone()));
            x += w + 1;
        }

        // Preset pills.
        self.preset_rects.clear();
        let mut pills = vec![Span::styled(
            " presets ",
            self.label_style(Focus::Presets, th),
        )];
        let mut x = rows[1].x + width(" presets ") as u16;
        if self.presets.is_empty() {
            pills.push(Span::styled(
                "none yet — create one on the Presets tab",
                th.dim(),
            ));
        }
        let (lcap, rcap) = ctx.ws.config.ui.pill_caps.glyphs();
        let caps_w = width(lcap) + width(rcap);
        // Reserve an indicator on each side; every mouse target is a whole pill.
        let budget = rows[1].width.saturating_sub(13) as usize;
        let bodies: Vec<String> = self
            .presets
            .iter()
            .map(|(preset, status)| {
                let mark = match status.state() {
                    PresetState::Active => "✓ ",
                    PresetState::Partial => "◐ ",
                    PresetState::Inactive => "◌ ",
                    PresetState::Empty => "◦ ",
                };
                let suffix = status
                    .progress()
                    .map(|p| format!(" {p} "))
                    .unwrap_or(" ".into());
                let name_w = budget.saturating_sub(caps_w + width(mark) + width(&suffix) + 2);
                format!(" {mark}{}{suffix}", fit(&preset.name, name_w))
            })
            .collect();
        let widths: Vec<usize> = bodies.iter().map(|b| width(b) + caps_w + 1).collect();
        let visible = pill_window(&widths, self.preset_cursor, &mut self.preset_offset, budget);
        pills.push(Span::styled(
            if visible.start > 0 { "‹ " } else { "  " },
            th.dim(),
        ));
        x += 2;
        for i in visible.clone() {
            let (_, status) = &self.presets[i];
            let body = bodies[i].clone();
            let w = (widths[i] - 1) as u16;
            self.preset_rects.push((i, Rect::new(x, rows[1].y, w, 1)));
            let base = match status.state() {
                PresetState::Active => th.ok,
                PresetState::Partial => th.warn,
                _ => th.dim,
            };
            let selected = i == self.preset_cursor;
            let focused = self.focus() == Focus::Presets;
            // With the keyboard on this row the pill is lit and underlined, so
            // the cursor is visible without reading the colours against each
            // other. Once the focus moves on, the underline goes and bold alone
            // remembers the place, which marks it without competing with the
            // list for attention.
            let fill = if selected && focused { lit(base) } else { base };
            let mut body_style = Style::default().bg(fill).fg(cards::ink(fill));
            if selected {
                body_style = body_style.add_modifier(if focused {
                    Modifier::BOLD | Modifier::UNDERLINED
                } else {
                    Modifier::BOLD
                });
            }
            // The caps carry the fill as foreground against the page, which is
            // what rounds the ends off; reversing them would square the pill.
            let cap = Style::default().fg(fill);
            pills.push(Span::styled(lcap, cap));
            pills.push(Span::styled(body, body_style));
            pills.push(Span::styled(rcap, cap));
            pills.push(Span::raw(" "));
            x += w + 1;
        }
        pills.push(Span::styled(
            if visible.end < self.presets.len() {
                "›"
            } else {
                " "
            },
            th.dim(),
        ));
        f.render_widget(Paragraph::new(Line::from(pills)), rows[1]);

        // The whole width goes to the entries. A detail pane here only ever had
        // the selected preset's members to show, which the pills already count
        // and the list below already spells out one skill at a time.
        let left = rows[2];
        self.left = left;
        let rows_data = self.rows(ctx);
        let cards = !self.compact;
        let counts = match (
            rows_data.iter().filter(|r| r.managed).count(),
            rows_data.iter().filter(|r| !r.managed).count(),
        ) {
            (0, 0) => "nothing here yet".to_string(),
            (n, 0) => format!("{n} linked"),
            (0, m) => format!("{m} the agent's own"),
            (n, m) => format!("{n} linked · {m} the agent's own"),
        };
        let block = th.block(
            format!(" skills · {counts} "),
            self.focus() == Focus::Entries,
        );
        let inner = block.inner(left);
        f.render_widget(block, left);

        let cell_h = if cards { CARD_H } else { 1 };
        // One column is held back for the scrollbar so the column count does not
        // shift the moment the list outgrows a screen.
        let usable = inner.width.saturating_sub(1);
        let cols = if cards { cols_for(usable) } else { 1 };
        let content = Rect {
            width: usable,
            ..inner
        };
        self.entries.layout(
            content,
            cols,
            cell_h,
            if cols > 1 { 1 } else { 0 },
            rows_data.len(),
        );

        let selected = self.entries.selected();
        for i in self.entries.visible() {
            let Some(cell) = self.entries.cell(i) else {
                continue;
            };
            let row = &rows_data[i];
            let on = selected == Some(i);
            if cards {
                let ci = frame(f, cell, on, self.focus() == Focus::Entries, th);
                let lines = match ctx
                    .snap
                    .skills
                    .iter()
                    .find(|s| s.deployment_name() == row.name)
                {
                    // A skill the root knows is drawn the way every page draws
                    // it. Being in this grid already says it is linked, so the
                    // tail says where it came from rather than "managed" again.
                    Some(r) if row.managed => {
                        let tail = r
                            .source
                            .as_ref()
                            .map(|s| s.kind().to_string())
                            .unwrap_or_default();
                        skill_card(r, ctx, ci.width as usize, None, &tail, &[])
                    }
                    // The agent's own: there may be no record behind it, and
                    // even when there is, what matters is the shape it is in.
                    _ => {
                        let (glyph, gs) = glyph_for(row.state, th);
                        let label = state_label(row.state);
                        let name_w = (ci.width as usize).saturating_sub(4);
                        vec![
                            Line::from(vec![
                                Span::styled(format!("{glyph} "), gs),
                                Span::styled(pad(row.name, name_w), th.dim()),
                            ]),
                            Line::from(vec![
                                Span::raw("  "),
                                Span::styled(
                                    fit(
                                        &entry_summary(row.state),
                                        (ci.width as usize).saturating_sub(2),
                                    ),
                                    th.dim(),
                                ),
                            ]),
                            rule(ci.width as usize, th),
                            Line::from(vec![
                                Span::raw("  "),
                                Span::styled(label, th.dim().add_modifier(Modifier::ITALIC)),
                            ]),
                        ]
                    }
                };
                f.render_widget(Paragraph::new(lines), ci);
            } else {
                let style = if on {
                    if self.focus() == Focus::Entries {
                        th.selected()
                    } else {
                        th.selected_unfocused()
                    }
                } else {
                    Style::default()
                };
                let (glyph, gs) = glyph_for(row.state, th);
                let managed = row
                    .managed
                    .then(|| {
                        ctx.snap
                            .skills
                            .iter()
                            .find(|r| r.deployment_name() == row.name)
                    })
                    .flatten();
                let mut spans = vec![
                    Span::styled(if on { "▸ " } else { "  " }, th.accent()),
                    managed
                        .map(|r| cards::health_marker(r, th))
                        .unwrap_or_else(|| Span::styled(format!("{glyph} "), gs)),
                ];
                if let Some(r) = managed {
                    let available = (cell.width as usize).saturating_sub(5);
                    let badge = cards::repository_badge(r, ctx.ws.config.ui.icons);
                    let badge_w = badge
                        .as_deref()
                        .map(|text| width(text).min(available / 2))
                        .unwrap_or(0);
                    let badge_space = badge_w + usize::from(badge_w > 0);
                    let name_w = 26.min(available.saturating_sub(badge_space));
                    spans.push(Span::styled(
                        pad(cards::display_name(r), name_w),
                        Style::default(),
                    ));
                    if let Some(badge) = badge.filter(|_| badge_w > 0) {
                        spans.push(Span::styled(format!(" {}", pad(&badge, badge_w)), th.dim()));
                    }
                    let note_w = available.saturating_sub(name_w + badge_space);
                    spans.push(Span::styled(
                        fit(&row.state.map(entry_note).unwrap_or_default(), note_w),
                        th.dim(),
                    ));
                } else {
                    spans.push(Span::styled(
                        pad(row.name, 26),
                        if row.managed {
                            Style::default()
                        } else {
                            th.dim()
                        },
                    ));
                    spans.push(Span::styled(
                        row.state.map(entry_note).unwrap_or_default(),
                        th.dim(),
                    ));
                }
                let line = Line::from(spans);
                f.render_widget(Paragraph::new(line).style(style), cell);
            }
        }

        // The thumb measures grid rows, which is what a click on the track lands on.
        let vis = self.entries.visible_rows();
        self.entries_track = Rect::default();
        if self.entries.grid_rows() > vis && inner.height > 0 {
            let track = Rect::new(inner.right().saturating_sub(1), inner.y, 1, inner.height);
            self.entries_track = track;
            let mut sb = ScrollbarState::new(self.entries.grid_rows())
                .position(selected.unwrap_or(0) / self.entries.cols())
                .viewport_content_length(vis);
            f.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .begin_symbol(None)
                    .end_symbol(None)
                    .style(th.dim()),
                track,
                &mut sb,
            );
        }
        self.preview.draw(f, area, ctx);
        self.matrix.draw(f, area, ctx);
    }

    fn hints(&self) -> Hints {
        if let Some(hints) = self.matrix.hints() {
            return hints;
        }
        if let Some(hints) = self.preview.hints() {
            return hints;
        }
        match self.focus() {
            Focus::Presets => &[
                ("Enter", "deploy / undeploy"),
                ("x", "undeploy"),
                ("M", "matrix"),
                ("←→", "pick preset"),
                ("↓", "entries"),
                ("[ ]", "agent"),
                ("s", "sync"),
                ("v", "layout"),
            ],
            // A repair key is shown only on a row it applies to, so the footer
            // never offers something the page would refuse.
            Focus::Entries => match self.selected_caps() {
                Caps { clean: true, .. } => &[
                    ("j/k", "move"),
                    ("↑", "back to presets"),
                    ("Enter", "preview"),
                    ("x", "clean"),
                    ("a", "adopt"),
                    ("[ ]", "agent"),
                    ("c", "convert dir-link"),
                    ("v", "layout"),
                ],
                Caps { relink: true, .. } => &[
                    ("j/k", "move"),
                    ("↑", "back to presets"),
                    ("Enter", "preview"),
                    ("l", "relink"),
                    ("a", "adopt"),
                    ("[ ]", "agent"),
                    ("c", "convert dir-link"),
                    ("v", "layout"),
                ],
                Caps { managed: true, .. } => &[
                    ("j/k", "move"),
                    ("↑", "back to presets"),
                    ("Enter", "preview"),
                    ("m", "multi-select"),
                    ("[ ]", "agent"),
                    ("v", "layout"),
                ],
                Caps { .. } => &[
                    ("j/k", "move"),
                    ("↑", "back to presets"),
                    ("Enter", "preview"),
                    ("a", "adopt"),
                    ("[ ]", "agent"),
                    ("c", "convert dir-link"),
                    ("v", "layout"),
                ],
            },
            Focus::Agents => &[
                ("←→", "pick agent"),
                ("↓", "presets"),
                ("s", "sync"),
                ("c", "convert dir-link"),
                ("v", "layout"),
            ],
        }
    }
}

/// The bright twin of a pill colour, used for the pill holding the keyboard.
/// Anything outside the basic palette is left alone; the bold text still marks it.
fn lit(c: Color) -> Color {
    match c {
        Color::Green => Color::LightGreen,
        Color::Yellow => Color::LightYellow,
        Color::Red => Color::LightRed,
        Color::Cyan => Color::LightCyan,
        Color::Blue => Color::LightBlue,
        Color::Magenta => Color::LightMagenta,
        Color::DarkGray => Color::Gray,
        other => other,
    }
}

/// Marker for one agent's relationship to a skill; `None` means that agent
/// does not have it at all.
fn glyph_for(state: Option<&EntryState>, th: &crate::tui::theme::Theme) -> (&'static str, Style) {
    match state {
        Some(EntryState::Deployed) => ("✓", th.ok()),
        Some(EntryState::Broken { .. }) => ("!", th.err()),
        Some(EntryState::Shadow { .. }) => ("▪", th.warn()),
        Some(EntryState::Foreign { .. }) => ("→", th.warn()),
        Some(EntryState::AgentOnly) => ("▪", th.dim()),
        None => ("—", th.dim()),
    }
}

fn entry_note(state: &EntryState) -> String {
    match state {
        EntryState::Deployed => String::new(),
        EntryState::Broken { .. } => "broken link".into(),
        EntryState::Shadow { same_content: true } => "the agent's own copy, same content".into(),
        EntryState::Shadow {
            same_content: false,
        } => "the agent's own copy, differs".into(),
        EntryState::Foreign { target } => format!("links outside the root → {}", target.display()),
        EntryState::AgentOnly => "only here, not in the root".into(),
    }
}

/// What an entry is when the root holds no record to describe it.
fn entry_summary(state: Option<&EntryState>) -> String {
    match state {
        Some(EntryState::Broken { .. }) => {
            "the link points at something the root no longer has".into()
        }
        Some(EntryState::Foreign { target }) => {
            format!("links outside the root to {}", target.display())
        }
        Some(EntryState::AgentOnly) => "only in this agent, not in the root".into(),
        _ => "no description".into(),
    }
}

/// Short status word for the right of a card.
fn state_label(state: Option<&EntryState>) -> &'static str {
    match state {
        Some(EntryState::Deployed) => "linked",
        Some(EntryState::Broken { .. }) => "broken link",
        Some(EntryState::Shadow { same_content: true }) => "shadow",
        Some(EntryState::Shadow {
            same_content: false,
        }) => "shadow, differs",
        Some(EntryState::Foreign { .. }) => "foreign",
        Some(EntryState::AgentOnly) => "the agent's own",
        None => "",
    }
}

/// Fit whole pills, moving the start only when selection leaves the viewport.
fn pill_window(
    widths: &[usize],
    selected: usize,
    offset: &mut usize,
    budget: usize,
) -> std::ops::Range<usize> {
    if widths.is_empty() {
        *offset = 0;
        return 0..0;
    }
    let selected = selected.min(widths.len() - 1);
    *offset = (*offset).min(selected);
    while *offset < selected && widths[*offset..=selected].iter().sum::<usize>() > budget {
        *offset += 1;
    }
    let mut end = *offset;
    let mut used = 0;
    while end < widths.len() && used + widths[end] <= budget {
        used += widths[end];
        end += 1;
    }
    *offset..end
}

#[cfg(test)]
mod overflow_tests {
    use super::*;
    use crate::tui::theme::Theme;
    use ratatui::{Terminal, backend::TestBackend};
    use skills::{Workspace, config::AgentConfig};

    #[test]
    fn linked_unmanaged_skill_keeps_its_health_marker_across_layouts() {
        let root =
            std::env::temp_dir().join(format!("skills-agent-markers-{}", std::process::id()));
        let central = root.join("central");
        let agent_dir = root.join("agent");
        std::fs::create_dir_all(central.join("printer")).unwrap();
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(
            central.join("printer/SKILL.md"),
            "---\nname: printer\ndescription: Print documents\n---\nPrint documents.\n",
        )
        .unwrap();
        std::os::unix::fs::symlink(central.join("printer"), agent_dir.join("printer")).unwrap();
        let mut ws = Workspace::open(&central).unwrap();
        ws.config.agents = vec![AgentConfig {
            key: "sample".into(),
            name: "Sample Agent".into(),
            skills_dir: agent_dir.display().to_string(),
        }];
        let snap = ws.scan().unwrap();
        let theme = Theme::default();
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            theme: &theme,
        };
        let mut view = AgentsView {
            scope: "sample".into(),
            ..AgentsView::default()
        };
        view.refresh(&ctx);
        let record = snap.get("printer").unwrap();
        assert!(matches!(
            record.status,
            skills::reconcile::SkillStatus::Unmanaged
        ));
        assert!(
            view.rows(&ctx)
                .iter()
                .any(|row| row.name == "printer" && row.managed)
        );
        for compact in [false, true] {
            view.compact = compact;
            let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
            terminal.draw(|f| view.draw(f, f.area(), &ctx)).unwrap();
            let buf = terminal.backend().buffer();
            let text = (0..30)
                .map(|y| (0..120).map(|x| buf[(x, y)].symbol()).collect::<String>())
                .collect::<Vec<_>>()
                .join("\n");
            let line = text.lines().find(|line| line.contains("printer")).unwrap();
            assert!(line.contains("○"), "compact={compact}: {line}");
            assert!(!line.contains("●"), "compact={compact}: {line}");
        }
        assert_eq!(glyph_for(Some(&EntryState::Deployed), &theme).0, "✓");
        assert_eq!(glyph_for(None, &theme).0, "—");
        assert!(!central.join(".skills-meta").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_preview_reads_the_selected_entry_instead_of_its_root_namesake() {
        let root =
            std::env::temp_dir().join(format!("skills-agent-preview-{}", std::process::id()));
        let central = root.join("central");
        let agent_dir = root.join("agent");
        std::fs::create_dir_all(&central).unwrap();
        std::fs::create_dir_all(&agent_dir).unwrap();
        let put = |dir: &std::path::Path, body: &str| {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(
                dir.join("SKILL.md"),
                format!("---\nname: printer\ndescription: Print documents\n---\n{body}\n"),
            )
            .unwrap();
        };
        put(&central.join("printer"), "CENTRAL CONTENT");
        put(&agent_dir.join("printer"), "AGENT COPY CONTENT");
        put(&agent_dir.join("reader"), "AGENT ONLY CONTENT");
        put(&root.join("external"), "EXTERNAL CONTENT");
        std::os::unix::fs::symlink(root.join("external"), agent_dir.join("external")).unwrap();
        std::os::unix::fs::symlink(root.join("absent"), agent_dir.join("broken")).unwrap();
        std::fs::create_dir_all(agent_dir.join("invalid")).unwrap();
        std::fs::write(agent_dir.join("invalid/SKILL.md"), "invalid frontmatter").unwrap();
        let mut ws = Workspace::open(&central).unwrap();
        ws.config.agents = vec![AgentConfig {
            key: "sample".into(),
            name: "Sample Agent".into(),
            skills_dir: agent_dir.display().to_string(),
        }];
        let snap = ws.scan().unwrap();
        let theme = Theme::default();
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            theme: &theme,
        };
        let mut view = AgentsView {
            scope: "sample".into(),
            ..AgentsView::default()
        };
        view.refresh(&ctx);
        view.set_focus(Focus::Entries);
        assert!(
            view.select_skills(&ctx, None).is_empty(),
            "agent-owned entries must not enter central skill selection"
        );
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        for (name, expected) in [
            ("printer", "AGENT COPY CONTENT"),
            ("reader", "AGENT ONLY CONTENT"),
            ("external", "EXTERNAL CONTENT"),
            ("broken", "missing SKILL.md"),
            ("invalid", "no YAML frontmatter"),
        ] {
            let rows = view.rows(&ctx);
            let index = rows.iter().position(|r| r.name == name).unwrap();
            view.entries.first(rows.len());
            view.entries.move_by(index as i32, rows.len());
            assert!(view.handle_key(key(KeyCode::Enter), &ctx).is_empty());
            terminal
                .draw(|f| view.preview.draw(f, f.area(), &ctx))
                .unwrap();
            let buf = terminal.backend().buffer();
            let text = (0..40)
                .map(|y| (0..120).map(|x| buf[(x, y)].symbol()).collect::<String>())
                .collect::<Vec<_>>()
                .join("\n");
            assert!(text.contains(expected), "{name}: {text}");
            assert!(text.contains("Sample Agent"));
            assert!(text.contains(&format!("{name}/SKILL.md")));
            assert!(!text.contains("CENTRAL CONTENT"));
            assert!(view.handle_key(key(KeyCode::Esc), &ctx).is_empty());
        }
        assert!(!central.join(".skills-meta").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn selected_pill_is_visible_and_mouse_targets_are_clipped() {
        let root = std::env::temp_dir().join(format!("skills-pills-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let mut ws = Workspace::open(&root).unwrap();
        ws.config.agents = vec![AgentConfig {
            key: "sample".into(),
            name: "Sample".into(),
            skills_dir: "../sample".into(),
        }];
        for i in 0..24 {
            ws.presets
                .save(&Preset {
                    name: format!("group-{i:02}-long-name"),
                    ..Preset::default()
                })
                .unwrap();
        }
        let snap = ws.scan().unwrap();
        let theme = Theme::default();
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            theme: &theme,
        };
        let mut view = AgentsView::default();
        view.refresh(&ctx);
        for (w, h) in [(100, 30), (80, 24), (120, 40)] {
            let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
            for i in (0..24).chain((0..24).rev()) {
                view.preset_cursor = i;
                terminal.draw(|f| view.draw(f, f.area(), &ctx)).unwrap();
                assert!(view.preset_rects.iter().any(|(index, _)| *index == i));
                for (_, rect) in &view.preset_rects {
                    assert!(rect.right() <= w);
                    assert!(rect.bottom() <= h);
                }
                let (_, rect) = view
                    .preset_rects
                    .iter()
                    .find(|(index, _)| *index == i)
                    .unwrap();
                let actions = view.handle_mouse(
                    MouseEvent {
                        kind: MouseEventKind::Down(MouseButton::Left),
                        column: rect.x,
                        row: rect.y,
                        modifiers: KeyModifiers::NONE,
                    },
                    &ctx,
                );
                assert_eq!(view.preset_cursor, i);
                assert!(!actions.is_empty());
            }
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
