//! Presets tab: what each preset holds, and adding to or taking from it.
//!
//! Turning a preset on or off is the Agents page's job, one agent at a time.
//! This page is where a preset is defined: which skills belong to it. The
//! cards summarize each preset's purpose and members; deployment lives on Agents.

use super::cards::{self, CARD_H, cols_for, frame, frame_styled, skill_card};
use super::matrix::Matrix;
use super::preview::Overlay;
use super::{View, split_panes, wheel};
use crate::tui::app::{Action, Ctx, Hints, Tab};
use crate::tui::modal::Modal;
use crate::tui::widgets::{CardGrid, ScrollTrack, fit, pad, width};
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState};
use skills::history;
use skills::preset::Preset;

#[derive(Default)]
pub struct PresetsView {
    presets: Vec<Preset>,
    tag_groups: Vec<(String, Vec<String>)>,
    tag_cursor: usize,
    tag_offset: usize,
    tag_rects: Vec<(usize, Rect)>,
    focus_tags: bool,
    visible_members: Vec<String>,
    tags_enabled: bool,

    all_presets: Vec<Preset>,
    filter: super::filter::Filter,
    skill_search: Option<(String, super::search::SearchView)>,
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
    pub fn batch_finished(&mut self, failed: &[String]) {
        if let Some((_, view)) = self.skill_search.as_mut() {
            view.batch_finished(failed);
        }
    }

    pub fn input_focused(&self) -> bool {
        self.filter.editing
            || (self.focus_members
                && self
                    .skill_search
                    .as_ref()
                    .is_some_and(|(_, v)| v.input_focused()))
    }
    pub fn paste(&mut self, text: &str, ctx: &Ctx) -> Vec<Action> {
        if self.filter.editing {
            let actions = self.filter.paste(text);
            self.refilter();
            return actions;
        }
        if self.focus_members
            && let Some((_, view)) = self.skill_search.as_mut()
        {
            return view.paste(text, ctx);
        }
        vec![]
    }
    fn refilter(&mut self) {
        let selected = self.selected().map(|p| p.name.clone());
        self.presets = self
            .all_presets
            .iter()
            .filter(|p| {
                self.filter.matches(&format!(
                    "{} {}",
                    p.name,
                    p.description.as_deref().unwrap_or("")
                ))
            })
            .cloned()
            .collect();
        self.list
            .select(selected.and_then(|name| self.presets.iter().position(|p| p.name == name)));
        self.list.clamp(self.presets.len());
        if self
            .skill_search
            .as_ref()
            .is_some_and(|(name, _)| self.selected().is_none_or(|p| &p.name != name))
        {
            self.skill_search = None;
        }
    }

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
        self.visible_members.len()
    }

    fn refresh_groups(&mut self, ctx: &Ctx) {
        self.tags_enabled = ctx.ws.config.tags_enabled;
        let selected = self.tag_groups.get(self.tag_cursor).map(|g| g.0.clone());
        let selected_all = self.tag_cursor == 0;
        let selected_untagged =
            self.tag_groups.len() > 1 && self.tag_cursor == self.tag_groups.len() - 1;
        let mut all: Vec<String> = ctx
            .snap
            .skills
            .iter()
            .filter(|r| r.status.is_present())
            .map(|r| r.key.clone())
            .collect();
        if let Some(p) = self.selected() {
            all.extend(p.skills.clone());
        }
        all.sort();
        all.dedup();
        self.tag_groups = vec![("All".into(), all.clone())];
        if self.tags_enabled {
            let mut names: std::collections::BTreeSet<String> =
                ctx.ws.config.tags.iter().map(|t| t.name.clone()).collect();
            names.extend(ctx.snap.all_tags().into_keys());
            for name in names {
                let keys = ctx
                    .snap
                    .skills
                    .iter()
                    .filter(|r| r.status.is_present() && r.tags.contains(&name))
                    .map(|r| r.key.clone())
                    .collect();
                self.tag_groups.push((name, keys));
            }
            self.tag_groups.push((
                "Untagged".into(),
                all.into_iter()
                    .filter(|key| ctx.snap.get(key).is_some_and(|r| r.tags.is_empty()))
                    .collect(),
            ));
        } else {
            self.focus_tags = false;
        }
        self.tag_cursor = if !self.tags_enabled || selected_all {
            0
        } else if selected_untagged {
            self.tag_groups.len() - 1
        } else {
            selected
                .and_then(|name| {
                    self.tag_groups[1..self.tag_groups.len() - 1]
                        .iter()
                        .position(|g| g.0 == name)
                        .map(|i| i + 1)
                })
                .unwrap_or(0)
        };
        self.visible_members = self.tag_groups[self.tag_cursor].1.clone();
        self.members.clamp(self.member_count());
    }

    fn edit_members(&self, keys: Vec<String>, add: bool) -> Vec<Action> {
        let Some(p) = self.selected() else {
            return vec![];
        };
        let name = p.name.clone();
        vec![Action::WriteMeta(Box::new(move |ws| {
            history::preset_edit(ws, &name, |members| {
                if add {
                    for key in &keys {
                        if !members.contains(key) {
                            members.push(key.clone());
                        }
                    }
                } else {
                    members.retain(|key| !keys.contains(key));
                }
            })
        }))]
    }

    fn toggle_members(&self, keys: Vec<String>) -> Vec<Action> {
        let add = self
            .selected()
            .is_some_and(|p| keys.iter().any(|key| !p.skills.contains(key)));
        self.edit_members(keys, add)
    }

    fn draw_tags(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) -> Rect {
        self.tag_rects.clear();
        if !self.tags_enabled || area.height < 3 {
            return area;
        }
        let (left, right) = ctx.ws.config.ui.pill_caps.glyphs();
        let cap_width = width(left) + width(right);
        let budget = area.width.saturating_sub(4) as usize;
        let members = self
            .selected()
            .map(|p| p.skills.clone())
            .unwrap_or_default();
        let bodies: Vec<_> = self
            .tag_groups
            .iter()
            .map(|(name, keys)| {
                let count = keys.iter().filter(|key| members.contains(key)).count();
                let mark = if count == 0 {
                    "◌"
                } else if count == keys.len() {
                    "✓"
                } else {
                    "◐"
                };
                let suffix = format!(" {count}/{} ", keys.len());
                format!(
                    " {mark} {}{suffix}",
                    fit(name, budget.saturating_sub(cap_width + width(&suffix) + 4))
                )
            })
            .collect();
        let widths: Vec<_> = bodies
            .iter()
            .map(|body| width(body) + cap_width + 1)
            .collect();
        let visible =
            super::agents::pill_window(&widths, self.tag_cursor, &mut self.tag_offset, budget);
        let mut spans = vec![Span::styled(
            if visible.start > 0 { "‹ " } else { "  " },
            ctx.theme.dim(),
        )];
        let mut x = area.x + 2;
        for i in visible.clone() {
            let keys = &self.tag_groups[i].1;
            let count = keys.iter().filter(|key| members.contains(key)).count();
            let fill = if count > 0 && count == keys.len() {
                ctx.theme.ok
            } else if count > 0 {
                ctx.theme.warn
            } else {
                ctx.theme.dim
            };
            let mut style = Style::default().bg(fill).fg(cards::ink(fill));
            if i == self.tag_cursor {
                style = style.add_modifier(if self.focus_tags {
                    Modifier::BOLD | Modifier::UNDERLINED
                } else {
                    Modifier::BOLD
                });
            }
            spans.push(Span::styled(left, Style::default().fg(fill)));
            spans.push(Span::styled(bodies[i].clone(), style));
            spans.push(Span::styled(right, Style::default().fg(fill)));
            spans.push(Span::raw(" "));
            self.tag_rects
                .push((i, Rect::new(x, area.y, (widths[i] - 1) as u16, 1)));
            x += widths[i] as u16;
        }
        spans.push(Span::styled(
            if visible.end < self.tag_groups.len() {
                "›"
            } else {
                " "
            },
            ctx.theme.dim(),
        ));
        f.render_widget(
            Paragraph::new(Line::from(spans)),
            Rect { height: 1, ..area },
        );
        Rect {
            y: area.y + 2,
            height: area.height - 2,
            ..area
        }
    }

    fn select_skills(&mut self, checked: Option<String>, ctx: &Ctx) -> Vec<Action> {
        if let Some(preset) = self.selected() {
            let name = preset.name.clone();
            let mut view = super::search::SearchView::panel(
                self.visible_members.clone(),
                format!("Preset: {name}"),
                ctx,
            );
            view.start_multi(checked);
            self.skill_search = Some((name, view));
            self.focus_members = true;
        }
        vec![]
    }

    fn selected_member(&self) -> Option<String> {
        self.members
            .selected()
            .and_then(|i| self.visible_members.get(i))
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
        self.with_selected(|p| Modal::preset_members(&p.name, ctx))
    }

    /// Reading a member must not cost the place on this page, so it opens in a
    /// window over it rather than by jumping to the search tab.
    fn open_member(&mut self) -> Vec<Action> {
        if let Some(k) = self.selected_member() {
            self.preview.open(k);
        }
        vec![]
    }

    /// Match skill cards: identity, two description lines, and member summary.
    fn preset_card(&self, p: &Preset, ctx: &Ctx, inner_w: usize) -> Vec<Line<'static>> {
        let th = ctx.theme;
        let count = match p.skills.len() {
            0 => "Empty".to_string(),
            1 => "1 skill".to_string(),
            n => format!("{n} skills"),
        };
        let count = fit(&count, inner_w);
        let gap = usize::from(inner_w > width(&count));
        let head = vec![
            Span::styled(
                pad(&p.name, inner_w.saturating_sub(width(&count) + gap)),
                th.bold(),
            ),
            Span::raw(" ".repeat(gap)),
            Span::styled(count, th.dim()),
        ];
        let description = cards::summary_lines(
            p.description
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or("No description"),
            inner_w,
        );
        let missing = p
            .skills
            .iter()
            .filter(|key| {
                ctx.snap.get(key).is_none_or(|r| {
                    matches!(
                        r.status,
                        skills::reconcile::SkillStatus::Missing
                            | skills::reconcile::SkillStatus::Renamed { .. }
                    )
                })
            })
            .count();
        let auto = if ctx.ws.config.deploy.presets.contains(&p.name) {
            "auto"
        } else {
            ""
        };
        let auto = fit(auto, inner_w);
        let available = inner_w.saturating_sub(width(&auto) + usize::from(!auto.is_empty()));
        let warning = if missing > 0 {
            fit(&format!("! {missing} missing"), available)
        } else {
            String::new()
        };
        let separator = if !warning.is_empty() && available > width(&warning) + 3 {
            " · "
        } else {
            ""
        };
        let names: Vec<&str> = p
            .skills
            .iter()
            .map(|key| {
                ctx.snap
                    .get(key)
                    .map(cards::display_name)
                    .unwrap_or_else(|| key.rsplit('/').next().unwrap_or(key))
            })
            .collect();
        let summary = member_summary(
            &names,
            available.saturating_sub(width(&warning) + width(separator)),
        );
        let used = width(&warning) + width(separator) + width(&summary) + width(&auto);
        let foot = vec![
            Span::styled(warning, th.warn()),
            Span::styled(separator, th.dim()),
            Span::styled(summary, th.dim()),
            Span::raw(" ".repeat(inner_w.saturating_sub(used))),
            Span::styled(auto, th.dim()),
        ];
        vec![
            Line::from(head),
            Line::from(Span::styled(description[0].clone(), th.dim())),
            Line::from(Span::styled(description[1].clone(), th.dim())),
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
                    if self.all_presets.is_empty() {
                        "no presets yet — press c to create one"
                    } else {
                        "no matching presets"
                    },
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
            self.visible_members.len(),
        );
        if self.visible_members.is_empty() {
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
        for i in self.members.visible() {
            let Some(cell) = self.members.cell(i) else {
                continue;
            };
            let on = selected == Some(i);
            let key = &self.visible_members[i];
            let lines = match ctx.snap.get(key) {
                Some(r) => {
                    let ci = frame(f, cell, on, self.focus_members, th);
                    let tail = r
                        .source
                        .as_ref()
                        .map(|s| s.kind().to_string())
                        .unwrap_or_default();
                    let mut lines = skill_card(r, ctx, ci.width as usize, None, &tail, &[]);
                    lines[0].spans[0] = cards::checkbox_marker(p.skills.contains(key), th);
                    lines.into_iter().map(|l| (ci, l)).collect::<Vec<_>>()
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
        self.all_presets = ctx.ws.presets.list().unwrap_or_default();
        self.refilter();
        if let Some(i) = keep.and_then(|k| self.presets.iter().position(|p| p.name == k)) {
            self.list.select(Some(i));
            self.pending = None;
        }
        self.list.clamp(self.presets.len());
        self.refresh_groups(ctx);
        if let Some((_, view)) = self.skill_search.as_mut() {
            let keys = self.visible_members.clone();
            view.update_panel(keys, ctx);
        }
    }

    fn handle_key(&mut self, k: KeyEvent, ctx: &Ctx) -> Vec<Action> {
        if self.preview.handle_key(k) {
            return vec![];
        }
        if let Some(actions) = self.matrix.handle_key(k, ctx) {
            return actions;
        }
        self.refresh_groups(ctx);
        if self.focus_members && self.focus_tags {
            match k.code {
                KeyCode::Left | KeyCode::Char('h') if self.tag_cursor > 0 => self.tag_cursor -= 1,
                KeyCode::Right | KeyCode::Char('l') => {
                    self.tag_cursor = (self.tag_cursor + 1).min(self.tag_groups.len() - 1)
                }
                KeyCode::Down | KeyCode::Char('j') => self.focus_tags = false,
                KeyCode::Esc | KeyCode::Left => {
                    self.focus_members = false;
                    self.focus_tags = false;
                }
                KeyCode::Enter | KeyCode::Char(' ') => {
                    return self.toggle_members(self.visible_members.clone());
                }
                KeyCode::Char('a') => return self.edit_members(self.visible_members.clone(), true),
                KeyCode::Char('x') | KeyCode::Delete => {
                    return self.edit_members(self.visible_members.clone(), false);
                }
                KeyCode::Char('/' | 'm') => {
                    self.focus_tags = false;
                    return self.handle_key(k, ctx);
                }
                _ => return vec![],
            }
            self.visible_members = self.tag_groups[self.tag_cursor].1.clone();
            self.members.first(self.member_count());
            self.skill_search = None;
            return vec![];
        }
        if self.focus_members && self.skill_search.is_none() {
            if k.code == KeyCode::Up
                && self.members.selected().unwrap_or(0) < self.members.cols()
                && self.tags_enabled
            {
                self.focus_tags = true;
                return vec![];
            }
            if k.code == KeyCode::Char(' ') {
                return self.toggle_members(self.selected_member().into_iter().collect());
            }
        }

        if !self.focus_members && self.filter.key(k) {
            self.refilter();
            return vec![];
        }
        if self.focus_members
            && let Some((name, view)) = self.skill_search.as_mut()
        {
            if k.code == KeyCode::Left && view.panel_back() {
                self.focus_members = false;
                return vec![];
            }
            if view.panel_actions_ready() {
                if matches!(k.code, KeyCode::Char('x') | KeyCode::Delete) && k.modifiers.is_empty()
                {
                    let name = name.clone();
                    let keys = view.panel_keys(ctx);
                    return vec![Action::WriteMeta(Box::new(move |ws| {
                        history::preset_edit(ws, &name, |members| {
                            members.retain(|key| !keys.contains(key))
                        })
                    }))];
                }
                if k.code == KeyCode::Char('a') && k.modifiers.is_empty() {
                    let keys = view.panel_keys(ctx);
                    return self.edit_members(keys, true);
                }
            }
            return view.handle_key(k, ctx);
        }
        if self.focus_members && matches!(k.code, KeyCode::Char('/' | 'm')) {
            if let Some(preset) = self.selected() {
                let name = preset.name.clone();
                let mut view = super::search::SearchView::panel(
                    self.visible_members.clone(),
                    format!("Preset: {name}"),
                    ctx,
                );
                if k.code == KeyCode::Char('m') {
                    view.focus_list();
                    view.handle_key(k, ctx);
                }
                self.skill_search = Some((name, view));
            }
            return vec![];
        }
        if k.code == KeyCode::Char('M') {
            self.matrix.open(ctx);
            return vec![];
        }
        let n = self.presets.len();
        let m = self.member_count();
        if self.focus_members {
            return match k.code {
                KeyCode::Char('m') => self.select_skills(None, ctx),
                KeyCode::Esc | KeyCode::Char('h') => {
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
            KeyCode::Char('q') => vec![Action::SwitchTab(Tab::Search)],
            // Esc means "back" everywhere else in the program, so here it goes
            // back to the search page rather than out of the door.
            KeyCode::Esc => vec![Action::SwitchTab(Tab::Search)],
            KeyCode::Down | KeyCode::Char('j') => {
                self.skill_search = None;
                self.list.move_by(1, n);
                self.members.clamp(self.member_count());
                vec![]
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.skill_search = None;
                self.list.move_by(-1, n);
                self.members.clamp(self.member_count());
                vec![]
            }
            KeyCode::Home | KeyCode::Char('g') => {
                self.skill_search = None;
                self.list.first(n);
                self.members.clamp(self.member_count());
                vec![]
            }
            KeyCode::End | KeyCode::Char('G') => {
                self.skill_search = None;
                self.list.last(n);
                self.members.clamp(self.member_count());
                vec![]
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                if m > 0 {
                    self.focus_members = true;
                    self.focus_tags = self.tags_enabled;
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
        if m.kind == MouseEventKind::Down(MouseButton::Left)
            && let Some((index, _)) = self.tag_rects.iter().find(|(_, rect)| rect.contains(at))
        {
            self.tag_cursor = *index;
            self.visible_members = self.tag_groups[*index].1.clone();
            self.members.first(self.member_count());
            self.focus_members = true;
            self.focus_tags = true;
            self.skill_search = None;
            return vec![];
        }

        if m.kind == MouseEventKind::Down(MouseButton::Left) && self.filter.rect.contains(at) {
            self.focus_members = false;
            self.filter.editing = true;
            return vec![];
        }
        if self.right.contains(at) {
            self.focus_tags = false;
        }
        if self.right.contains(at)
            && let Some((_, view)) = self.skill_search.as_mut()
        {
            self.focus_members = true;
            return view.handle_mouse(m, ctx);
        }
        if self.left.contains(at) {
            self.skill_search = None;
        }
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
                && let Some((index, double)) = self.members.click(m.column, m.row)
            {
                self.focus_members = true;
                if self.members.cell(index).is_some_and(|cell| {
                    m.row == cell.y + 1
                        && (cell.x + 2..cell.x + 2 + cards::MARKER_W as u16).contains(&m.column)
                }) {
                    return self.toggle_members(self.selected_member().into_iter().collect());
                }
                if double {
                    return self.open_member();
                }
            }
        }
        let _ = ctx;
        vec![]
    }

    fn draw(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        self.refresh_groups(ctx);
        let (left, right) = split_panes(area, 38);
        self.left = left;
        self.right = right;
        let content = self.filter.draw(f, left, "Filter presets", ctx);
        self.draw_presets(f, content, ctx);
        let right = self.draw_tags(f, right, ctx);
        if let Some((_, view)) = self.skill_search.as_mut() {
            view.set_panel_active(self.focus_members);
            view.draw(f, right, ctx);
        } else {
            self.draw_members(f, right, ctx);
        }
        self.preview.draw(f, area, ctx);
        self.matrix.draw(f, area, ctx);
    }

    fn hints(&self) -> Hints {
        if self.focus_members && self.focus_tags {
            return &[
                ("←→", "tags"),
                ("↓", "skills"),
                ("Enter/Space", "toggle group"),
                ("a/x", "add/remove group"),
                ("Esc", "presets"),
            ];
        }
        if self.filter.editing {
            return &[("Enter/↓", "presets"), ("Esc", "finish filter")];
        }
        if self.focus_members
            && let Some((_, view)) = self.skill_search.as_ref()
        {
            return view.preset_panel_hints();
        }
        if let Some(hints) = self.matrix.hints() {
            return hints;
        }
        if let Some(hints) = self.preview.hints() {
            return hints;
        }
        if self.focus_members {
            &[
                ("Space", "toggle member"),
                ("/", "filter skills"),
                ("a", "add skills"),
                ("x", "remove"),
                ("m", "multi-select"),
                ("Enter", "preview"),
                ("←/Esc", "presets"),
            ]
        } else {
            &[
                ("/", "filter presets"),
                ("c", "create"),
                ("M", "matrix"),
                ("a", "add skills"),
                ("e", "description"),
                ("r", "rename"),
                ("Enter/→", "members"),
                ("D", "delete preset"),
                ("q", "library"),
            ]
        }
    }
}

// Keep the module's own name for the card constants in scope for callers
// that only import this view.
#[allow(unused_imports)]
use cards::MIN_CARD_W as _;

/// Preserve whole member names where possible and count entries that do not fit.
fn member_summary(names: &[&str], columns: usize) -> String {
    if names.is_empty() {
        return fit("No members", columns);
    }
    let mut shown = String::new();
    for (i, name) in names.iter().enumerate() {
        let separator = if shown.is_empty() { "" } else { " · " };
        let remaining = names.len() - i - 1;
        let suffix = if remaining > 0 {
            format!(" · +{remaining}")
        } else {
            String::new()
        };
        if width(&shown) + width(separator) + width(name) + width(&suffix) > columns {
            if shown.is_empty() {
                if columns > width(&suffix) + 3 {
                    return format!("{}{}", fit(name, columns - width(&suffix)), suffix);
                }
                return fit(&format!("+{}", names.len()), columns);
            }
            return fit(&format!("{shown} · +{}", names.len() - i), columns);
        }
        shown.push_str(separator);
        shown.push_str(name);
    }
    shown
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::theme::Theme;
    use crossterm::event::KeyModifiers;
    use skills::{Workspace, config::Config};

    #[test]
    fn preset_cards_show_members_and_fit_unicode_without_agent_status() {
        let root = skills::ops::DownloadDir::new("preset-card-test").unwrap();
        Config {
            agents: vec![],
            ..Default::default()
        }
        .save(root.path())
        .unwrap();
        let path = root.path().join("document");
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(
            path.join("SKILL.md"),
            "---\nname: document-tools\ndescription: Document tools\n---\nBody",
        )
        .unwrap();
        let mut ws = Workspace::open(root.path()).unwrap();
        ws.config.deploy.presets.push("Office".into());
        let snap = ws.scan().unwrap();
        let theme = Theme::default();
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            theme: &theme,
        };
        let preset = Preset {
            name: "Office".into(),
            description: Some("**Document tools** with 中文说明".into()),
            skills: vec!["document".into(), "missing".into()],
            agents: vec!["SampleAgent".into()],
        };
        let view = PresetsView::default();
        let lines = view.preset_card(&preset, &ctx, 65);
        assert_eq!(lines.len(), 4);
        assert!(lines[0].to_string().ends_with("2 skills"));
        assert!(lines[1].to_string().starts_with("Document tools"));
        assert!(lines[3].to_string().contains("document-tools"));
        assert!(lines[3].to_string().contains("! 1 missing"));
        assert!(lines[3].to_string().ends_with("auto"));
        assert!(
            !lines
                .iter()
                .any(|line| line.to_string().contains("SampleAgent"))
        );
        for width in 0..80 {
            assert!(
                view.preset_card(&preset, &ctx, width)
                    .iter()
                    .all(|line| line.width() <= width)
            );
            assert!(
                crate::tui::widgets::width(&member_summary(
                    &["printer", "calendar", "document"],
                    width
                )) <= width
            );
        }
        assert_eq!(
            member_summary(&["printer", "calendar", "document"], 16),
            "printer · +2"
        );
    }

    #[test]
    fn preview_blocks_member_removal_until_closed() {
        let root = std::env::temp_dir().join(format!(
            "skills-preview-keys-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let config = Config {
            agents: vec![],
            ..Config::default()
        };
        config.save(&root).unwrap();
        let ws = Workspace::open(&root).unwrap();
        ws.presets
            .save(&Preset {
                name: "reading".into(),
                skills: vec!["printer".into()],
                ..Preset::default()
            })
            .unwrap();
        let before = std::fs::read(ws.presets.path("reading")).unwrap();
        let snap = ws.scan().unwrap();
        let theme = Theme::default();
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            theme: &theme,
        };
        let mut view = PresetsView::default();
        view.refresh(&ctx);
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        assert!(view.handle_key(key(KeyCode::Char('m')), &ctx).is_empty());
        assert!(view.handle_key(key(KeyCode::Right), &ctx).is_empty());
        let actions = view.handle_key(key(KeyCode::Char('m')), &ctx);
        assert!(actions.is_empty());
        assert!(view.skill_search.is_some());
        view.skill_search = None;
        assert!(view.handle_key(key(KeyCode::Enter), &ctx).is_empty());
        assert!(view.preview.is_open());

        for code in [KeyCode::Char('x'), KeyCode::Delete, KeyCode::Char('a')] {
            assert!(view.handle_key(key(code), &ctx).is_empty());
            assert!(view.preview.is_open());
            assert!(view.focus_members);
            assert_eq!(view.selected_member().as_deref(), Some("printer"));
            assert_eq!(std::fs::read(ws.presets.path("reading")).unwrap(), before);
        }

        assert!(view.handle_key(key(KeyCode::Esc), &ctx).is_empty());
        assert!(!view.preview.is_open());
        assert!(view.focus_members);
        let mut actions = view.handle_key(key(KeyCode::Char('x')), &ctx);
        assert_eq!(actions.len(), 1);
        let Action::WriteMeta(write) = actions.remove(0) else {
            panic!("member removal should resume after closing the preview");
        };
        write(&ws).unwrap();
        assert!(
            ws.presets
                .load("reading")
                .unwrap()
                .unwrap()
                .skills
                .is_empty()
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod tag_group_tests {
    use super::*;
    use crossterm::event::KeyModifiers;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn tag_pills_show_coverage_edit_members_and_disappear_when_disabled() {
        let temp = skills::ops::DownloadDir::new("preset-tags").unwrap();
        let root = temp.path();
        skills::config::Config {
            agents: vec![],
            ..Default::default()
        }
        .save(root)
        .unwrap();
        for key in ["alpha", "beta", "gamma"] {
            std::fs::create_dir(root.join(key)).unwrap();
            std::fs::write(
                root.join(key).join("SKILL.md"),
                format!("---\nname: {key}\ndescription: example\n---\n"),
            )
            .unwrap();
        }
        let mut ws = skills::Workspace::open(root).unwrap();
        skills::ops::edit::tag_add(&ws, "alpha", &["python".into(), "testing".into()]).unwrap();
        skills::ops::edit::tag_add(&ws, "beta", &["python".into()]).unwrap();
        ws.presets
            .save(&Preset {
                name: "dev".into(),
                skills: vec!["alpha".into()],
                ..Default::default()
            })
            .unwrap();
        ws.config = ws.load_config().unwrap();
        let snap = ws.scan().unwrap();
        let theme = crate::tui::theme::Theme::default();
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            theme: &theme,
        };
        let mut view = PresetsView::default();
        view.refresh(&ctx);
        let mut terminal = Terminal::new(TestBackend::new(130, 30)).unwrap();
        terminal.draw(|f| view.draw(f, f.area(), &ctx)).unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(screen.contains("python 1/2"));
        assert!(screen.contains("testing 1/1"));
        assert!(screen.contains("Untagged 0/1"));
        let (index, rect) = view
            .tag_rects
            .iter()
            .find(|(i, _)| view.tag_groups[*i].0 == "python")
            .cloned()
            .unwrap();
        view.handle_mouse(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: rect.x,
                row: rect.y,
                modifiers: KeyModifiers::NONE,
            },
            &ctx,
        );
        assert_eq!(view.tag_cursor, index);
        assert_eq!(view.visible_members, ["alpha", "beta"]);
        let keys = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let Action::WriteMeta(add) = view.handle_key(keys(KeyCode::Char('a')), &ctx).remove(0)
        else {
            panic!("group edit")
        };
        add(&ws).unwrap();
        assert_eq!(
            ws.presets.load("dev").unwrap().unwrap().skills,
            ["alpha", "beta"]
        );
        assert!(ws.scan().unwrap().get("alpha").unwrap().deploy.is_empty());
        // Removing a tag group leaves a member from another group alone.
        skills::history::preset_edit(&ws, "dev", |members| members.push("gamma".into())).unwrap();
        view.refresh(&ctx);
        let Action::WriteMeta(remove) = view.handle_key(keys(KeyCode::Char('x')), &ctx).remove(0)
        else {
            panic!("group edit")
        };
        remove(&ws).unwrap();
        assert_eq!(ws.presets.load("dev").unwrap().unwrap().skills, ["gamma"]);
        assert_eq!(
            skills::config::Config::load(root)
                .unwrap()
                .skill_tags("alpha"),
            ["python", "testing"]
        );
        skills::config::Config::set_tags_enabled(root, false).unwrap();
        ws.config = ws.load_config().unwrap();
        let snap = ws.scan().unwrap();
        assert!(snap.skills.iter().all(|r| r.tags.is_empty()));
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            theme: &theme,
        };
        view.refresh(&ctx);
        terminal.draw(|f| view.draw(f, f.area(), &ctx)).unwrap();
        assert!(view.tag_rects.is_empty());
        assert_eq!(view.visible_members.len(), 3);
        assert!(!Tab::visible(false).contains(&Tab::Tags));
        assert!(Tab::visible(true).contains(&Tab::Tags));
        assert_eq!(
            skills::config::Config::load(root)
                .unwrap()
                .skill_tags("alpha"),
            ["python", "testing"]
        );
        assert!(!root.join(".skills-meta/local.toml").exists());
    }
}
