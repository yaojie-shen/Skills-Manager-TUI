//! Agents tab: what a given agent actually has, and the presets that fill it.
//!
//! Everything on this page is scoped to one agent or to all of them at once.
//! Entries are split by who owns them: skills linked from the central root are
//! ours to add and remove, anything else the agent brought itself is shown but
//! never written to.

use super::{View, split_panes, wheel};
use crate::tui::app::{Action, Ctx, Hints};
use crate::tui::widgets::{ListNav, fit, pad, width};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, Paragraph, Wrap};
use skills::ops::deploy::{
    self, PresetState, PresetStatus, plan_preset_activate, plan_preset_deactivate, preset_status,
};
use skills::preset::Preset;
use skills::reconcile::{AgentDirMode, EntryState};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Only two things take the keyboard. The scope is switched by `[` and `]`
/// from anywhere, so it never needs to hold focus.
enum Focus {
    Presets,
    Entries,
}

/// One row of the entry list: either a section header or an entry under it.
enum Row<'a> {
    Header(String),
    /// One skill name, with the state it has in each agent of the scope.
    /// A skill present in several agents is one row, not one row per agent.
    Entry {
        name: &'a str,
        states: Vec<Option<&'a EntryState>>,
        managed: bool,
    },
}

#[derive(Default)]
pub struct AgentsView {
    /// None = all agents.
    scope: Option<String>,
    focus: FocusState,
    presets: Vec<(Preset, PresetStatus)>,
    preset_cursor: usize,
    entries: ListNav,
    /// Compact rows are one line each; the default card gives an entry three.
    compact: bool,
    scope_rects: Vec<(Rect, Option<String>)>,
    preset_rects: Vec<Rect>,
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
    /// Agents the current scope covers.
    fn scope_agents(&self, ctx: &Ctx) -> Vec<String> {
        match &self.scope {
            Some(k) => vec![k.clone()],
            None => ctx.ws.config.agent_keys(),
        }
    }

    fn scope_label(&self) -> &str {
        self.scope.as_deref().unwrap_or("all agents")
    }

    fn focus(&self) -> Focus {
        self.focus.0
    }

    fn set_focus(&mut self, f: Focus) {
        self.focus = FocusState(f);
    }

    /// Entry rows for the current scope, grouped into ours and theirs.
    fn rows<'a>(&self, ctx: &'a Ctx) -> Vec<Row<'a>> {
        let agents = self.scope_agents(ctx);
        // Collect names first so a skill deployed to several agents stays one row.
        let mut names: Vec<&str> = Vec::new();
        for key in &agents {
            if let Some(report) = ctx.snap.agent(key) {
                for name in report.entries.keys() {
                    if !names.contains(&name.as_str()) {
                        names.push(name);
                    }
                }
            }
        }
        names.sort_unstable();
        let mut managed: Vec<Row> = Vec::new();
        let mut local: Vec<Row> = Vec::new();
        for name in names {
            let states: Vec<Option<&EntryState>> = agents
                .iter()
                .map(|key| ctx.snap.agent(key).and_then(|r| r.entries.get(name)))
                .collect();
            // Ours as soon as any agent links it into the root; otherwise the
            // agent's own, however many agents happen to carry a copy.
            let is_managed = states
                .iter()
                .any(|s| matches!(s, Some(EntryState::Deployed)));
            let row = Row::Entry {
                name,
                states,
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
        let scope = self.scope_agents(ctx);
        let plan = if on {
            plan_preset_activate(ctx.ws, ctx.snap, preset, &scope)
        } else {
            plan_preset_deactivate(ctx.ws, ctx.snap, preset, &scope)
        };
        match plan {
            Ok(actions) => vec![Action::ConfirmLinks {
                title: format!(
                    "{} {} · {}",
                    if on { "activate" } else { "deactivate" },
                    preset.name,
                    self.scope_label()
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
        let cur = match &self.scope {
            None => 0i32,
            Some(k) => keys
                .iter()
                .position(|x| x == k)
                .map(|i| i as i32 + 1)
                .unwrap_or(0),
        };
        let next = (cur + delta).rem_euclid(keys.len() as i32 + 1);
        self.scope = if next == 0 {
            None
        } else {
            keys.get((next - 1) as usize).cloned()
        };
    }
}

impl View for AgentsView {
    fn refresh(&mut self, ctx: &Ctx) {
        // A scope pinned to an agent that no longer exists falls back to all.
        if let Some(k) = &self.scope
            && ctx.ws.config.agent(k).is_none()
        {
            self.scope = None;
        }
        let scope = self.scope_agents(ctx);
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
        self.entries.clamp(self.rows(ctx).len());
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
                let Some(agent) = self.scope.clone() else {
                    return vec![Action::Error("pick a single agent to convert".into())];
                };
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
            Focus::Presets => match k.code {
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
                    self.entries.clamp(self.rows(ctx).len());
                    vec![]
                }
                _ => vec![],
            },
            Focus::Entries => {
                let n = self.rows(ctx).len();
                match k.code {
                    KeyCode::Down | KeyCode::Char('j') => self.entries.move_by(1, n),
                    KeyCode::Up | KeyCode::Char('k') => {
                        if self.entries.selected().unwrap_or(0) == 0 {
                            self.set_focus(Focus::Presets);
                        } else {
                            self.entries.move_by(-1, n);
                        }
                    }
                    KeyCode::Home | KeyCode::Char('g') => self.entries.first(n),
                    KeyCode::End | KeyCode::Char('G') => self.entries.last(n),
                    KeyCode::Enter => {
                        if let Some(Row::Entry { name, .. }) = self
                            .entries
                            .selected()
                            .and_then(|i| self.rows(ctx).into_iter().nth(i))
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
                self.entries.move_by(d, self.rows(ctx).len());
            } else if self.right.contains(at) {
                self.detail_scroll = (self.detail_scroll as i32 + d).max(0) as u16;
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
                self.entries.click(m.row, self.rows(ctx).len());
            }
        }
        vec![]
    }

    fn draw(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        let th = ctx.theme;
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Length(2),
                Constraint::Min(1),
            ])
            .split(area);

        // Scope chips.
        self.scope_rects.clear();
        let mut spans = vec![Span::styled(" scope ", th.dim())];
        let mut x = rows[0].x + width(" scope ") as u16;
        let chip = |label: String,
                    key: Option<String>,
                    on: bool,
                    spans: &mut Vec<Span<'static>>,
                    x: &mut u16,
                    rects: &mut Vec<(Rect, Option<String>)>| {
            let text = format!(" {label} ");
            let w = width(&text) as u16;
            rects.push((Rect::new(*x, rows[0].y, w, 1), key));
            spans.push(Span::styled(
                text,
                if on {
                    th.selected().fg(th.accent)
                } else {
                    th.dim()
                },
            ));
            spans.push(Span::raw(" "));
            *x += w + 1;
        };
        chip(
            "all".into(),
            None,
            self.scope.is_none(),
            &mut spans,
            &mut x,
            &mut self.scope_rects,
        );
        for a in &ctx.ws.config.agents {
            let on = self.scope.as_deref() == Some(a.key.as_str());
            chip(
                a.key.clone(),
                Some(a.key.clone()),
                on,
                &mut spans,
                &mut x,
                &mut self.scope_rects,
            );
        }
        f.render_widget(Paragraph::new(Line::from(spans)), rows[0]);

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
            let mark = match status.state() {
                PresetState::Active => "✓ ",
                PresetState::Partial => "◐ ",
                PresetState::Inactive => "○ ",
                PresetState::Empty => "· ",
            };
            let text = format!(" {mark}{} {} ", preset.name, status.label());
            let w = width(&text) as u16;
            self.preset_rects.push(Rect::new(x, rows[1].y, w, 1));
            let style = match status.state() {
                PresetState::Active => th.ok(),
                PresetState::Partial => th.warn(),
                _ => th.dim(),
            };
            // Reversing paints the pill solid in its own state colour, which
            // reads as selected from across the screen; without focus a bold
            // outline is enough to remember the place.
            let style = if i == self.preset_cursor {
                if self.focus() == Focus::Presets {
                    style.add_modifier(ratatui::style::Modifier::REVERSED)
                } else {
                    style.add_modifier(ratatui::style::Modifier::BOLD)
                }
            } else {
                style
            };
            pills.push(Span::styled(text, style));
            pills.push(Span::raw(" "));
            x += w + 1;
        }
        f.render_widget(Paragraph::new(Line::from(pills)), rows[1]);

        // Entries and detail.
        let (left, right) = split_panes(rows[2], 55);
        self.left = left;
        self.right = right;
        let rows_data = self.rows(ctx);
        let show_agent = self.scope.is_none();
        let scope_agents = self.scope_agents(ctx);
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
                    states,
                    managed,
                } => {
                    if !self.compact {
                        return entry_card(name, states, *managed, &scope_agents, ctx, inner_w);
                    }
                    let mut spans = vec![Span::raw("  ")];
                    spans.push(Span::styled(
                        pad(name, 26),
                        if *managed { Style::default() } else { th.dim() },
                    ));
                    if show_agent {
                        // One column per agent, so a skill in both reads at a glance.
                        for (i, state) in states.iter().enumerate() {
                            let (glyph, style) = glyph_for(*state, th);
                            spans.push(Span::styled(
                                format!("{glyph} {} ", abbrev(&scope_agents[i])),
                                style,
                            ));
                        }
                    } else {
                        let state = states.first().copied().flatten();
                        let (glyph, style) = glyph_for(state, th);
                        spans.insert(1, Span::styled(format!("{glyph} "), style));
                        spans.push(Span::styled(
                            state.map(entry_note).unwrap_or_default(),
                            th.dim(),
                        ));
                    }
                    ListItem::new(Line::from(spans))
                }
            })
            .collect();
        let title = format!(" {} ", self.scope_label());
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
                ("[ ]", "scope"),
                ("c", "convert dir-link"),
                ("v", "density"),
            ],
        }
    }
}

/// Two-letter agent label used as a column head.
fn abbrev(key: &str) -> String {
    key.chars().take(2).collect()
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
    states: &[Option<&EntryState>],
    managed: bool,
    scope_agents: &[String],
    ctx: &Ctx,
    inner_w: usize,
) -> ListItem<'static> {
    let th = ctx.theme;
    let mut marks: Vec<Span> = Vec::new();
    for (i, state) in states.iter().enumerate() {
        let (glyph, style) = glyph_for(*state, th);
        marks.push(Span::styled(format!("{glyph} "), style));
        marks.push(Span::styled(
            format!("{}  ", abbrev(&scope_agents[i])),
            th.dim(),
        ));
    }
    let marks_w: usize = marks.iter().map(|s| width(&s.content)).sum();
    let name_w = inner_w.saturating_sub(marks_w + 2);
    let mut head = vec![
        Span::raw("  "),
        Span::styled(
            pad(name, name_w),
            if managed { th.bold() } else { th.dim() },
        ),
    ];
    head.extend(marks);

    // A skill several agents carry is filed by its root state, not by whichever
    // agent happens to come first in the scope.
    let state = states
        .iter()
        .flatten()
        .copied()
        .find(|s| matches!(s, EntryState::Deployed))
        .or_else(|| states.iter().flatten().copied().next());
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
            if self.scope.as_deref().is_some_and(|k| k != a.key) {
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
