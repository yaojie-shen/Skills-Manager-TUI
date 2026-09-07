//! Presets tab: what each preset holds, and adding to or taking from it.
//!
//! Turning a preset on or off is the Agents page's job, one agent at a time.
//! This page is where a preset is defined: which skills belong to it. The
//! cards on the left say, per agent, how much of the preset is in place, so
//! the definition and its effect can be read together without switching tabs.

use super::cards::{self, CARD_H, cols_for, frame, frame_styled, rule, skill_card};
use super::matrix::Matrix;
use super::preview::Overlay;
use super::{View, split_panes, wheel};
use crate::tui::app::{Action, Ctx, Hints, Tab};
use crate::tui::modal::Modal;
use crate::tui::widgets::{CardGrid, ScrollTrack, fit, pad, width};
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState};
use skills::history;
use skills::ops::deploy::{PresetState, preset_status};
use skills::preset::Preset;

#[derive(Default)]
pub struct PresetsView {
    presets: Vec<Preset>,
    list: CardGrid,
    members: CardGrid,
    focus_members: bool,
    left: Rect,
    right: Rect,
    list_track: ScrollTrack,
    members_track: ScrollTrack,
    /// Which track a drag started on, so it keeps hold of the thumb even when
    /// the pointer wanders off the column.
    drag: Option<Pane>,
    /// A member opened for reading, over the page rather than instead of it.
    preview: Overlay,
    /// The whole preset × agent picture, over the page.
    matrix: Matrix,
    /// A preset to land on when the list next reloads, by name, because the
    /// list is sorted and a preset just created or renamed can appear
    /// anywhere in it.
    pending: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pane {
    List,
    Members,
}

impl PresetsView {
    fn selected(&self) -> Option<&Preset> {
        self.list.selected().and_then(|i| self.presets.get(i))
    }

    /// Land on `name` once it shows up in the list.
    pub fn select(&mut self, name: &str) {
        self.pending = Some(name.to_string());
    }

    /// Prompts that need a preset under the cursor, or say what to do
    /// instead.
    fn with_selected(&self, open: impl FnOnce(&Preset) -> Modal) -> Vec<Action> {
        match self.selected() {
            Some(p) => vec![Action::OpenModal(Box::new(open(p)))],
            None => vec![Action::Error(
                "no preset selected; press c to create one".into(),
            )],
        }
    }

    fn member_count(&self) -> usize {
        self.selected().map(|p| p.skills.len()).unwrap_or(0)
    }

    fn selected_member(&self) -> Option<String> {
        self.selected()
            .and_then(|p| self.members.selected().and_then(|i| p.skills.get(i)))
            .cloned()
    }

    fn remove_member(&self) -> Vec<Action> {
        let (Some(p), Some(skill)) = (self.selected(), self.selected_member()) else {
            return vec![];
        };
        let name = p.name.clone();
        vec![Action::WriteMeta(Box::new(move |ws| {
            history::preset_edit(ws, &name, |members| members.retain(|s| s != &skill))
        }))]
    }

    fn add_members(&self, ctx: &Ctx) -> Vec<Action> {
        // Membership is edited by picking from the library, never by typing
        // a name from memory.
        self.with_selected(|p| Modal::preset_members(p, ctx.snap))
    }

    /// Reading a member must not cost the place on this page, so it opens in a
    /// window over it rather than by jumping to the search tab.
    fn open_member(&mut self) -> Vec<Action> {
        if let Some(k) = self.selected_member() {
            self.preview.open(k);
        }
        vec![]
    }

    /// The three lines of a preset card: name and size, what it is for, and
    /// how much of it each agent has. The last is derived from the links on
    /// disk exactly as the pills on the Agents page are.
    fn preset_card(&self, p: &Preset, ctx: &Ctx, inner_w: usize) -> Vec<Line<'static>> {
        let th = ctx.theme;
        let auto = ctx.ws.config.deploy.presets.contains(&p.name);
        let count = match p.skills.len() {
            1 => "1 skill".to_string(),
            n => format!("{n} skills"),
        };
        let right = if auto {
            format!("{count} · auto")
        } else {
            count
        };
        let name_w = inner_w.saturating_sub(width(&right) + 1);
        let head = vec![
            Span::styled(pad(&p.name, name_w), th.bold()),
            Span::raw(" "),
            Span::styled(right, th.dim()),
        ];
        let body = match &p.description {
            Some(d) => Span::styled(fit(d, inner_w.saturating_sub(2)), th.dim()),
            None => Span::styled("no description", th.dim()),
        };
        // One mark per agent the preset applies to. An agent outside the
        // preset's own list is left out rather than shown as inactive, which
        // would read as something to fix.
        let agents: Vec<String> = if p.agents.is_empty() {
            ctx.ws.config.agent_keys()
        } else {
            p.agents.clone()
        };
        let mut foot = vec![Span::raw("  ")];
        for (i, key) in agents.iter().enumerate() {
            if i > 0 {
                foot.push(Span::raw("   "));
            }
            let st = preset_status(ctx.snap, p, std::slice::from_ref(key));
            let (mark, style) = match st.state() {
                PresetState::Active => ("✓", th.ok()),
                PresetState::Partial => ("◐", th.warn()),
                PresetState::Inactive => ("◌", th.dim()),
                PresetState::Empty => ("◦", th.dim()),
            };
            foot.push(Span::styled(format!("{key} "), th.dim()));
            foot.push(Span::styled(mark.to_string(), style));
            if let Some(progress) = st.progress() {
                foot.push(Span::styled(format!(" {progress}"), style));
            }
        }
        vec![
            Line::from(head),
            Line::from(vec![Span::raw("  "), body]),
            rule(inner_w, th),
            Line::from(foot),
        ]
    }

    fn draw_presets(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        let th = ctx.theme;
        let block = th.block(" presets ", !self.focus_members);
        let inner = block.inner(area);
        f.render_widget(block, area);
        let content = Rect {
            width: inner.width.saturating_sub(1),
            ..inner
        };
        self.list.layout(content, 1, CARD_H, 0, self.presets.len());
        if self.presets.is_empty() {
            f.render_widget(
                Paragraph::new(Span::styled(
                    "no presets yet — press c to create one",
                    th.dim(),
                )),
                Rect {
                    height: 1,
                    ..content
                },
            );
            self.list_track.clear();
            return;
        }
        let selected = self.list.selected();
        for i in self.list.visible() {
            let Some(cell) = self.list.cell(i) else {
                continue;
            };
            let on = selected == Some(i);
            let ci = frame(f, cell, on, !self.focus_members, th);
            let lines = self.preset_card(&self.presets[i], ctx, ci.width as usize);
            f.render_widget(Paragraph::new(lines), ci);
        }
        draw_track(f, inner, &self.list, selected, &mut self.list_track, th);
    }

    fn draw_members(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        let th = ctx.theme;
        let Some(p) = self.selected().cloned() else {
            let block = th.block(" members ", self.focus_members);
            f.render_widget(block, area);
            self.members_track.clear();
            return;
        };
        let title = Line::from(vec![
            Span::raw(" members of "),
            Span::styled(p.name.clone(), th.bold()),
            Span::raw(" "),
        ]);
        let block = th.block(title, self.focus_members);
        let inner = block.inner(area);
        f.render_widget(block, area);
        let content = Rect {
            width: inner.width.saturating_sub(1),
            ..inner
        };
        let cols = cols_for(content.width);
        self.members.layout(
            content,
            cols,
            CARD_H,
            if cols > 1 { 1 } else { 0 },
            p.skills.len(),
        );
        if p.skills.is_empty() {
            f.render_widget(
                Paragraph::new(Span::styled("empty — press a to add skills", th.dim())),
                Rect {
                    height: 1,
                    ..content
                },
            );
            self.members_track.clear();
            return;
        }
        let selected = self.members.selected();
        let agents = &ctx.snap.agents;
        for i in self.members.visible() {
            let Some(cell) = self.members.cell(i) else {
                continue;
            };
            let on = selected == Some(i);
            let key = &p.skills[i];
            let lines = match ctx.snap.get(key) {
                Some(r) => {
                    let ci = frame(f, cell, on, self.focus_members, th);
                    let tail = r
                        .source
                        .as_ref()
                        .map(|s| s.kind().to_string())
                        .unwrap_or_default();
                    skill_card(r, ctx, agents, ci.width as usize, None, &tail, &[])
                        .into_iter()
                        .map(|l| (ci, l))
                        .collect::<Vec<_>>()
                }
                // A member with no directory behind it is the one thing on
                // this page that needs fixing, so its frame says so.
                None => {
                    let border = if on {
                        th.err().add_modifier(Modifier::BOLD)
                    } else {
                        th.err()
                    };
                    let ci = frame_styled(f, cell, border);
                    vec![
                        (
                            ci,
                            Line::from(vec![
                                Span::styled("? ", th.err()),
                                Span::styled(key.clone(), th.bold()),
                            ]),
                        ),
                        (
                            ci,
                            Line::from(Span::styled("  not in the skills root", th.err())),
                        ),
                        (
                            ci,
                            Line::from(Span::styled("  x takes it off the preset", th.dim())),
                        ),
                    ]
                }
            };
            if let Some((ci, _)) = lines.first() {
                let ci = *ci;
                f.render_widget(
                    Paragraph::new(lines.into_iter().map(|(_, l)| l).collect::<Vec<_>>()),
                    ci,
                );
            }
        }
        draw_track(
            f,
            inner,
            &self.members,
            selected,
            &mut self.members_track,
            th,
        );
    }
}

/// The scrollbar for a grid, in grid rows, drawn only once there is something
/// to scroll to.
fn draw_track(
    f: &mut Frame,
    inner: Rect,
    grid: &CardGrid,
    selected: Option<usize>,
    track: &mut ScrollTrack,
    th: &crate::tui::theme::Theme,
) {
    let vis = grid.visible_rows();
    if grid.grid_rows() > vis && inner.height > 0 {
        let rect = Rect {
            x: inner.right().saturating_sub(1),
            y: inner.y,
            width: 1,
            height: inner.height,
        };
        track.set(rect);
        let mut sb = ScrollbarState::new(grid.grid_rows())
            .position(selected.unwrap_or(0) / grid.cols())
            .viewport_content_length(vis);
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .style(th.dim()),
            rect,
            &mut sb,
        );
    } else {
        track.clear();
    }
}

impl View for PresetsView {
    fn refresh(&mut self, ctx: &Ctx) {
        // The list is sorted by name, so a preset keeps its place only by
        // name: one created or deleted above the cursor would otherwise move
        // the selection onto a neighbour.
        let keep = self
            .pending
            .clone()
            .or_else(|| self.selected().map(|p| p.name.clone()));
        self.presets = ctx.ws.presets.list().unwrap_or_default();
        if let Some(i) = keep.and_then(|k| self.presets.iter().position(|p| p.name == k)) {
            self.list.select(Some(i));
            self.pending = None;
        }
        self.list.clamp(self.presets.len());
        self.members.clamp(self.member_count());
    }

    fn handle_key(&mut self, k: KeyEvent, ctx: &Ctx) -> Vec<Action> {
        if self.preview.handle_key(k) {
            return vec![];
        }
        if let Some(acts) = self.matrix.handle_key(k, ctx) {
            return acts;
        }
        if k.code == KeyCode::Char('M') {
            self.matrix.open(ctx);
            return vec![];
        }
        let n = self.presets.len();
        let m = self.member_count();
        if self.focus_members {
            return match k.code {
                KeyCode::Esc | KeyCode::Char('h') | KeyCode::BackTab => {
                    self.focus_members = false;
                    vec![]
                }
                // Along a row while there is a row; off the left edge is back
                // to the preset list, which is where the eye goes anyway.
                KeyCode::Left => {
                    if self
                        .members
                        .selected()
                        .unwrap_or(0)
                        .is_multiple_of(self.members.cols())
                    {
                        self.focus_members = false;
                    } else {
                        self.members.move_by(-1, m);
                    }
                    vec![]
                }
                KeyCode::Right | KeyCode::Char('l') => {
                    self.members.move_by(1, m);
                    vec![]
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.members.move_rows(1, m);
                    vec![]
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.members.move_rows(-1, m);
                    vec![]
                }
                KeyCode::Home | KeyCode::Char('g') => {
                    self.members.first(m);
                    vec![]
                }
                KeyCode::End | KeyCode::Char('G') => {
                    self.members.last(m);
                    vec![]
                }
                KeyCode::Char('x') | KeyCode::Delete => self.remove_member(),
                KeyCode::Char('a') => self.add_members(ctx),
                KeyCode::Enter => self.open_member(),
                _ => vec![],
            };
        }
        match k.code {
            KeyCode::Char('q') => vec![Action::Quit],
            // Esc means "back" everywhere else in the program, so here it goes
            // back to the search page rather than out of the door.
            KeyCode::Esc => vec![Action::SwitchTab(Tab::Search)],
            KeyCode::Down | KeyCode::Char('j') => {
                self.list.move_by(1, n);
                self.members.clamp(self.member_count());
                vec![]
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.list.move_by(-1, n);
                self.members.clamp(self.member_count());
                vec![]
            }
            KeyCode::Home | KeyCode::Char('g') => {
                self.list.first(n);
                self.members.clamp(self.member_count());
                vec![]
            }
            KeyCode::End | KeyCode::Char('G') => {
                self.list.last(n);
                self.members.clamp(self.member_count());
                vec![]
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') | KeyCode::Tab => {
                if m > 0 {
                    self.focus_members = true;
                    self.members.clamp(m);
                }
                vec![]
            }
            KeyCode::Char('c') => vec![Action::OpenModal(Box::new(Modal::new_preset()))],
            KeyCode::Char('a') => self.add_members(ctx),
            KeyCode::Char('e') => {
                self.with_selected(|p| Modal::preset_description(&p.name, p.description.as_deref()))
            }
            KeyCode::Char('r') => self.with_selected(|p| Modal::rename_preset(&p.name)),
            // Deleting a whole preset is the one destructive key here, and it
            // is the capital so a slip on `x` in the member list cannot reach it.
            KeyCode::Char('D') => match self.selected() {
                Some(p) => vec![Action::OpenModal(Box::new(Modal::delete_preset(&p.name)))],
                None => vec![],
            },
            _ => vec![],
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
        let mcount = self.member_count();
        if let Some(d) = wheel(&m) {
            if self.left.contains(at) {
                self.list.move_by(d.signum(), self.presets.len());
                self.members.clamp(self.member_count());
            } else if self.right.contains(at) {
                self.members.move_rows(d.signum(), mcount);
            }
            return vec![];
        }
        let pressing = matches!(m.kind, MouseEventKind::Down(MouseButton::Left));
        let dragging = matches!(m.kind, MouseEventKind::Drag(MouseButton::Left));
        // The tracks sit inside the panes, so they get first refusal.
        let pane = if pressing && self.list_track.hit(m.column, m.row) {
            Some(Pane::List)
        } else if pressing && self.members_track.hit(m.column, m.row) {
            Some(Pane::Members)
        } else if dragging {
            self.drag
        } else {
            None
        };
        if let Some(pane) = pane {
            self.drag = Some(pane);
            match pane {
                Pane::List => {
                    self.focus_members = false;
                    if let Some(r) = self.list_track.index_at(m.row, self.list.grid_rows()) {
                        self.list.select_row(r);
                        self.members.clamp(self.member_count());
                    }
                }
                Pane::Members => {
                    self.focus_members = true;
                    if let Some(r) = self.members_track.index_at(m.row, self.members.grid_rows()) {
                        self.members.select_row(r);
                    }
                }
            }
            return vec![];
        }
        if !dragging {
            self.drag = None;
        }
        if pressing {
            if self.left.contains(at) {
                self.focus_members = false;
                if self.list.click(m.column, m.row).is_some() {
                    self.members.clamp(self.member_count());
                }
            } else if self.right.contains(at)
                && let Some((_, double)) = self.members.click(m.column, m.row)
            {
                self.focus_members = true;
                if double {
                    return self.open_member();
                }
            }
        }
        let _ = ctx;
        vec![]
    }

    fn draw(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        let (left, right) = split_panes(area, 38);
        self.left = left;
        self.right = right;
        self.draw_presets(f, left, ctx);
        self.draw_members(f, right, ctx);
        self.preview.draw(f, area, ctx);
        self.matrix.draw(f, area, ctx);
    }

    fn hints(&self) -> Hints {
        if self.focus_members {
            &[
                ("a", "add skills"),
                ("x", "remove"),
                ("Enter", "preview"),
                ("←/Esc", "presets"),
            ]
        } else {
            &[
                ("c", "create"),
                ("M", "matrix"),
                ("a", "add skills"),
                ("e", "description"),
                ("r", "rename"),
                ("Enter/→", "members"),
                ("D", "delete preset"),
                ("q", "quit"),
            ]
        }
    }
}

// Keep the module's own name for the card constants in scope for callers
// that only import this view.
#[allow(unused_imports)]
use cards::MIN_CARD_W as _;
