//! Agents tab: what a given agent actually has, and the presets that fill it.
//!
//! The page always looks at exactly one agent. Rolling several together needs
//! a preset to be counted against a set of agents rather than one, which reads
//! as a fraction of a fraction and answers nothing an agent's own page does not.
//! Entries are split by who owns them: skills linked from the central root are
//! ours to add and remove, anything else the agent brought itself is shown but
//! never written to.

use super::{View, split_panes, wheel};
use crate::tui::app::{Action, Ctx, Hints};
use crate::tui::widgets::{ListNav, fit, pad, width};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    List, ListItem, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap,
};
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

/// One row of the entry list: either a section header or an entry under it.
enum Row<'a> {
    Header(String),
    /// One skill name and the state it has in the agent on show.
    Entry {
        name: &'a str,
        state: Option<&'a EntryState>,
        managed: bool,
    },
}

#[derive(Default)]
pub struct AgentsView {
    /// Key of the agent on show. Empty only while none is configured.
    scope: String,
    focus: FocusState,
    presets: Vec<(Preset, PresetStatus)>,
    preset_cursor: usize,
    entries: ListNav,
    /// Compact rows are one line each; the default card gives an entry three.
    compact: bool,
    scope_rects: Vec<(Rect, String)>,
    preset_rects: Vec<Rect>,
    /// The one column the entry scrollbar occupies, empty while it all fits.
    entries_track: Rect,
    left: Rect,
    right: Rect,
    detail_scroll: u16,
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
    /// A header is a caption, not something to act on. A move that lands on one
    /// carries on in the direction it was going, and falls back the other way
    /// when there is nothing further ahead.
    fn snap_off_header(&mut self, rows: &[Row], dir: i32) {
        let Some(i) = self.entries.selected() else {
            return;
        };
        if !matches!(rows.get(i), Some(Row::Header(_))) {
            return;
        }
        let entry = |j: &usize| matches!(rows.get(*j), Some(Row::Entry { .. }));
        let ahead = (i + 1..rows.len()).find(entry);
        let behind = (0..i).rev().find(entry);
        let pick = if dir >= 0 {
            ahead.or(behind)
        } else {
            behind.or(ahead)
        };
        if let Some(j) = pick {
            self.entries.select(Some(j));
        }
    }

    /// Whether anything selectable sits above the selection. Nothing there means
    /// the arrow leaves the list for the pills above it.
    fn at_first_entry(&self, rows: &[Row]) -> bool {
        let Some(i) = self.entries.selected() else {
            return true;
        };
        !(0..i).any(|j| matches!(rows.get(j), Some(Row::Entry { .. })))
    }

    fn rows<'a>(&self, ctx: &'a Ctx) -> Vec<Row<'a>> {
        let Some(report) = ctx.snap.agent(&self.scope) else {
            return Vec::new();
        };
        let mut names: Vec<&str> = report.entries.keys().map(String::as_str).collect();
        names.sort_unstable();
        let mut managed: Vec<Row> = Vec::new();
        let mut local: Vec<Row> = Vec::new();
        for name in names {
            let state = report.entries.get(name);
            // Ours as soon as it links into the root; otherwise the agent's own,
            // whatever shape it takes.
            let is_managed = matches!(state, Some(EntryState::Deployed));
            let row = Row::Entry {
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
        let mut out = Vec::new();
        if !managed.is_empty() {
            out.push(Row::Header(format!("managed · {} linked", managed.len())));
            out.append(&mut managed);
        }
        if !local.is_empty() {
            out.push(Row::Header(format!(
                "the agent's own · {} left alone",
                local.len()
            )));
            out.append(&mut local);
        }
        out
    }

    fn selected_preset(&self) -> Option<&(Preset, PresetStatus)> {
        self.presets.get(self.preset_cursor)
    }

    fn activate(&self, ctx: &Ctx, on: bool) -> Vec<Action> {
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
                    if on { "activate" } else { "deactivate" },
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
        self.entries.clamp(rows.len());
        self.snap_off_header(&rows, 1);
    }

    fn handle_key(&mut self, k: KeyEvent, ctx: &Ctx) -> Vec<Action> {
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        match k.code {
            KeyCode::Char('q') | KeyCode::Esc => return vec![Action::Quit],
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
                    self.snap_off_header(&rows, 1);
                    vec![]
                }
                _ => vec![],
            },
            Focus::Entries => {
                let rows = self.rows(ctx);
                let n = rows.len();
                match k.code {
                    KeyCode::Down | KeyCode::Char('j') => {
                        self.entries.move_by(1, n);
                        self.snap_off_header(&rows, 1);
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        if self.at_first_entry(&rows) {
                            self.set_focus(Focus::Presets);
                        } else {
                            self.entries.move_by(-1, n);
                            self.snap_off_header(&rows, -1);
                        }
                    }
                    KeyCode::Home | KeyCode::Char('g') => {
                        self.entries.first(n);
                        self.snap_off_header(&rows, 1);
                    }
                    KeyCode::End | KeyCode::Char('G') => {
                        self.entries.last(n);
                        self.snap_off_header(&rows, -1);
                    }
                    KeyCode::Enter => {
                        if let Some(Row::Entry { name, .. }) =
                            self.entries.selected().and_then(|i| rows.get(i))
                        {
                            return vec![Action::Search {
                                query: name.to_string(),
                                focus_list: true,
                            }];
                        }
                    }
                    _ => {}
                }
                vec![]
            }
        }
    }

    fn handle_mouse(&mut self, m: MouseEvent, ctx: &Ctx) -> Vec<Action> {
        let at = (m.column, m.row).into();
        if let Some(d) = wheel(&m) {
            if self.left.contains(at) {
                self.set_focus(Focus::Entries);
                let rows = self.rows(ctx);
                self.entries.move_by(d, rows.len());
                self.snap_off_header(&rows, d);
            } else if self.right.contains(at) {
                self.detail_scroll = (self.detail_scroll as i32 + d).max(0) as u16;
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
            let n = self.rows(ctx).len();
            if n > 0 {
                let span = self.entries_track.height.max(1) as usize;
                let i = (m.row - self.entries_track.y) as usize * n / span;
                self.set_focus(Focus::Entries);
                self.entries.select(Some(i.min(n - 1)));
                let rows = self.rows(ctx);
                self.snap_off_header(&rows, 1);
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
            if let Some(i) = self.preset_rects.iter().position(|r| r.contains(at)) {
                self.preset_cursor = i;
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
                let rows = self.rows(ctx);
                self.entries.click(m.row, rows.len());
                self.snap_off_header(&rows, 1);
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
        for (i, (preset, status)) in self.presets.iter().enumerate() {
            // Part way on is a circle filled from the left, the way a gauge
            // fills: the top-and-bottom split (U+25D2) reads as a different
            // shape rather than as a fraction of the same one. It is one of the
            // two glyphs here whose width is Ambiguous, alongside the caps, so
            // a terminal that widens those shifts this row either way.
            let mark = match status.state() {
                PresetState::Active => "✓ ",
                PresetState::Partial => "◐ ",
                PresetState::Inactive => "◌ ",
                PresetState::Empty => "◦ ",
            };
            let body = match status.progress() {
                Some(count) => format!(" {mark}{} {count} ", preset.name),
                None => format!(" {mark}{} ", preset.name),
            };
            // The caps belong to the pill as far as the mouse is concerned.
            let (lcap, rcap) = ctx.ws.config.ui.pill_caps.glyphs();
            let w = width(&body) as u16 + width(lcap) as u16 + width(rcap) as u16;
            self.preset_rects.push(Rect::new(x, rows[1].y, w, 1));
            let base = match status.state() {
                PresetState::Active => th.ok,
                PresetState::Partial => th.warn,
                _ => th.dim,
            };
            let selected = i == self.preset_cursor;
            let focused = self.focus() == Focus::Presets;
            // With the keyboard here the pill is lit; without it the fill stays
            // put and only the text is underlined, so the place is still marked
            // but nothing competes with the list for attention.
            let fill = if selected && focused { lit(base) } else { base };
            let mut body_style = Style::default().bg(fill).fg(ink(fill));
            if selected {
                body_style = body_style.add_modifier(if focused {
                    Modifier::BOLD
                } else {
                    Modifier::UNDERLINED
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
        f.render_widget(Paragraph::new(Line::from(pills)), rows[1]);

        // Entries and detail.
        let (left, right) = split_panes(rows[2], 55);
        self.left = left;
        self.right = right;
        let rows_data = self.rows(ctx);
        let inner_w = left.width.saturating_sub(4) as usize; // borders + highlight symbol
        self.entries.item_height = if self.compact { 1 } else { 3 };
        let items: Vec<ListItem> = rows_data
            .iter()
            .map(|row| match row {
                Row::Header(text) => {
                    let head = Line::from(Span::styled(
                        text.clone(),
                        th.dim().add_modifier(ratatui::style::Modifier::ITALIC),
                    ));
                    if self.compact {
                        return ListItem::new(head);
                    }
                    // Every item of a list is the same height, so a header spends
                    // its two spare lines on the gap that separates the sections.
                    ListItem::new(vec![Line::from(""), head, Line::from("")])
                }
                Row::Entry {
                    name,
                    state,
                    managed,
                } => {
                    if !self.compact {
                        return entry_card(name, *state, *managed, ctx, inner_w);
                    }
                    let (glyph, style) = glyph_for(*state, th);
                    ListItem::new(Line::from(vec![
                        Span::raw("  "),
                        Span::styled(format!("{glyph} "), style),
                        Span::styled(
                            pad(name, 26),
                            if *managed { Style::default() } else { th.dim() },
                        ),
                        Span::styled(state.map(entry_note).unwrap_or_default(), th.dim()),
                    ]))
                }
            })
            .collect();
        // The picker above already says which agent this is; repeating it here
        // would only take up room the section headings use better.
        let title = " skills ".to_string();
        self.entries.set_area_from_block(left);
        let list = List::new(items)
            .block(th.block(title, self.focus() == Focus::Entries))
            .highlight_style(if self.focus() == Focus::Entries {
                th.selected()
            } else {
                th.selected_unfocused()
            })
            .highlight_symbol("▸ ");
        f.render_stateful_widget(list, left, &mut self.entries.state);

        // The scrollbar sits on the right border, showing where the selection
        // is among all the rows rather than how far the pixels have scrolled.
        let per_page = (left.height.saturating_sub(2) / self.entries.item_height.max(1)) as usize;
        self.entries_track = Rect::default();
        if rows_data.len() > per_page && left.height > 2 {
            let track = Rect::new(
                left.right().saturating_sub(1),
                left.y + 1,
                1,
                left.height - 2,
            );
            self.entries_track = track;
            let mut sb = ScrollbarState::new(rows_data.len())
                .position(self.entries.selected().unwrap_or(0))
                .viewport_content_length(per_page);
            f.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .begin_symbol(None)
                    .end_symbol(None)
                    .style(th.dim()),
                track,
                &mut sb,
            );
        }

        let block = th.block(" detail ", false);
        let inner = block.inner(right);
        f.render_widget(block, right);
        f.render_widget(
            Paragraph::new(self.detail(ctx))
                .wrap(Wrap { trim: false })
                .scroll((self.detail_scroll, 0)),
            inner,
        );
    }

    fn hints(&self) -> Hints {
        match self.focus() {
            Focus::Presets => &[
                ("Enter", "install / remove"),
                ("x", "remove"),
                ("←→", "pick preset"),
                ("↓", "entries"),
                ("[ ]", "scope"),
                ("s", "sync"),
                ("v", "density"),
            ],
            Focus::Entries => &[
                ("j/k", "move"),
                ("↑", "back to presets"),
                ("Enter", "open in search"),
                ("[ ]", "agent"),
                ("c", "convert dir-link"),
                ("v", "density"),
            ],
            Focus::Agents => &[
                ("←→", "pick agent"),
                ("↓", "presets"),
                ("s", "sync"),
                ("c", "convert dir-link"),
                ("v", "density"),
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

/// Text colour that stays legible on a filled pill.
fn ink(fill: Color) -> Color {
    match fill {
        Color::DarkGray | Color::Black | Color::Blue | Color::Red | Color::Magenta => Color::White,
        _ => Color::Black,
    }
}

/// Marker for one agent's relationship to a skill; `None` means that agent
/// does not have it at all.
fn glyph_for(state: Option<&EntryState>, th: &crate::tui::theme::Theme) -> (&'static str, Style) {
    match state {
        Some(EntryState::Deployed) => ("●", th.ok()),
        Some(EntryState::Broken { .. }) => ("!", th.err()),
        Some(EntryState::Shadow { .. }) => ("▪", th.warn()),
        Some(EntryState::Foreign { .. }) => ("→", th.warn()),
        Some(EntryState::AgentOnly) => ("▪", th.dim()),
        None => ("·", th.dim()),
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
        Some(EntryState::Deployed) => "managed",
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

/// A three-line card: the name and who has it, what the skill is, how it is filed.
fn entry_card(
    name: &str,
    state: Option<&EntryState>,
    managed: bool,
    ctx: &Ctx,
    inner_w: usize,
) -> ListItem<'static> {
    let th = ctx.theme;
    let (glyph, glyph_style) = glyph_for(state, th);
    let mark = Span::styled(format!("{glyph}  "), glyph_style);
    let name_w = inner_w.saturating_sub(width(&mark.content) + 2);
    let head = vec![
        Span::raw("  "),
        Span::styled(
            pad(name, name_w),
            if managed { th.bold() } else { th.dim() },
        ),
        mark,
    ];
    let record = ctx.snap.get(name);
    let body = match record.and_then(|r| r.description.clone()) {
        Some(d) => d,
        None => entry_summary(state),
    };
    let right = state_label(state);
    let tags = record.map(|r| r.tags.join(" · ")).unwrap_or_default();
    let tags_w = inner_w.saturating_sub(width(right) + 4);
    ListItem::new(vec![
        Line::from(head),
        Line::from(vec![
            Span::raw("  "),
            Span::styled(fit(&body, inner_w.saturating_sub(2)), th.dim()),
        ]),
        Line::from(vec![
            Span::raw("  "),
            Span::styled(pad(&tags, tags_w), th.tag()),
            Span::styled(
                right,
                th.dim().add_modifier(ratatui::style::Modifier::ITALIC),
            ),
        ]),
    ])
}

impl AgentsView {
    fn detail<'a>(&self, ctx: &'a Ctx) -> Vec<Line<'a>> {
        let th = ctx.theme;
        let mut lines = Vec::new();
        if let Some((preset, status)) = self.selected_preset() {
            lines.push(Line::from(Span::styled(
                preset.name.clone(),
                th.bold().fg(th.accent),
            )));
            if let Some(d) = &preset.description {
                lines.push(Line::from(Span::styled(d.clone(), th.dim())));
            }
            lines.push(Line::from(vec![
                Span::styled("in scope  ", th.dim()),
                Span::raw(format!(
                    "{} of {} skill-agent pairs",
                    status.installed, status.total
                )),
            ]));
            for skill in &preset.skills {
                let deployed: Vec<&str> = ctx
                    .snap
                    .get(skill)
                    .map(|r| r.deployed_to())
                    .unwrap_or_default();
                let mark = if status.absent.contains(skill) {
                    Span::styled("✗", th.err())
                } else if deployed.is_empty() {
                    Span::styled("○", th.dim())
                } else {
                    Span::styled("●", th.ok())
                };
                lines.push(Line::from(vec![
                    mark,
                    Span::raw(format!(" {}", pad(skill, 26))),
                    Span::styled(
                        if status.absent.contains(skill) {
                            "not in the root".to_string()
                        } else {
                            deployed.join(", ")
                        },
                        th.dim(),
                    ),
                ]));
            }
            lines.push(Line::from(""));
        }
        for a in &ctx.snap.agents {
            if a.key != self.scope {
                continue;
            }
            let mode = match &a.mode {
                AgentDirMode::Missing => "no directory yet".to_string(),
                AgentDirMode::DirLinked => {
                    "whole directory is one link; press c to split it".into()
                }
                AgentDirMode::DirForeign { target } => {
                    format!("directory links to {}", target.display())
                }
                AgentDirMode::Real => {
                    let d = a.count(|s| matches!(s, EntryState::Deployed));
                    format!("{d} linked, {} the agent's own", a.entries.len() - d)
                }
            };
            lines.push(Line::from(vec![
                Span::styled(format!("{:<10}", a.key), th.bold()),
                Span::styled(mode, th.dim()),
            ]));
            lines.push(Line::from(Span::styled(
                format!(
                    "           {}",
                    fit(&skills::paths::contract_tilde(&a.skills_dir), 60)
                ),
                th.dim(),
            )));
        }
        lines
    }
}
