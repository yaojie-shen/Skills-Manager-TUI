//! Tags tab: the tags themselves, and a look at what carries each one.
//!
//! Each pane filters its own collection. A tag can be edited directly: renamed, merged into another,
//! deleted, given a colour. The left column uses spaced text rows with colour
//! markers; the right
//! pane is the skills under the selected tag, as the cards the search and
//! presets pages use, so a skill reads the same wherever it turns up.

use super::cards::{self, CARD_H, cols_for, frame, skill_card, tag_fill};
use super::preview::Overlay;
use super::{View, split_panes, wheel};
use crate::tui::app::{Action, Ctx, Hints, Tab};
use crate::tui::modal::Modal;
use crate::tui::widgets::OverlayClear as Clear;
use crate::tui::widgets::{CardGrid, Input, ListNav, ScrollTrack, fit, pad, width};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    List, ListItem, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};
use skills::Workspace;
use skills::config::Config;
use skills::history;
use skills::ops::edit;
use skills::reconcile::Snapshot;

pub const UNTAGGED: &str = "(untagged)";

/// Colours a tag can be given by name, one row each in the colour prompt.
/// Anything `ratatui` can parse is accepted when typed, including `#rrggbb`;
/// these are just the ones worth listing.
const PALETTE: [&str; 16] = [
    "red",
    "green",
    "yellow",
    "blue",
    "magenta",
    "cyan",
    "white",
    "gray",
    "darkgray",
    "lightred",
    "lightgreen",
    "lightyellow",
    "lightblue",
    "lightmagenta",
    "lightcyan",
    "black",
];
/// The row in the colour prompt that takes a tag's colour away again.
const NO_COLOR: &str = "none";

#[derive(Default)]
pub struct TagsView {
    /// Every tag with how many skills carry it, `(untagged)` last.
    rows: Vec<(String, usize)>,
    all_rows: Vec<(String, usize)>,
    filter: super::filter::Filter,
    skill_search: Option<super::search::SearchView>,
    /// Skills under the selected tag, in snapshot order.
    members: Vec<String>,
    list: CardGrid,
    grid: CardGrid,
    focus_grid: bool,
    left: Rect,
    right: Rect,
    list_track: ScrollTrack,
    grid_track: ScrollTrack,
    /// Which track a drag started on, so it keeps hold of the thumb even when
    /// the pointer wanders off the column.
    drag: Option<Pane>,
    /// A skill opened for reading, over the page rather than instead of it.
    preview: Overlay,
    /// The merge target or colour being chosen, over the page.
    prompt: Option<Prompt>,
    /// The workspace with the config as it is on disk now. The app holds the
    /// config it started with and a page cannot reach in to reload it, so a
    /// colour just written would only show after a restart; this page reads
    /// `config.toml` again after every scan and draws with that copy.
    fresh: Option<Workspace>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pane {
    List,
    Grid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ask {
    /// Which tag the selected one is merged into.
    Merge,
    /// What colour the selected tag is filled with.
    Color,
}

/// A small chooser drawn over the page. It lives in the view rather than in
/// `Modal` because the modal's prompts each submit to one fixed action, and
/// this page needs two of its own. Typing filters the rows; for a colour the
/// text is also taken as it stands when it names one.
struct Prompt {
    ask: Ask,
    tag: String,
    input: Input,
    choices: Vec<String>,
    /// Indices into `choices` matching the filter, in display order.
    shown: Vec<usize>,
    list: ListNav,
    rect: Rect,
}

impl Prompt {
    fn new(ask: Ask, tag: &str, choices: Vec<String>) -> Self {
        let shown: Vec<usize> = (0..choices.len()).collect();
        let mut list = ListNav::default();
        list.clamp(shown.len());
        Self {
            ask,
            tag: tag.to_string(),
            input: Input::default(),
            choices,
            shown,
            list,
            rect: Rect::default(),
        }
    }

    fn refilter(&mut self) {
        let q = self.input.value().trim().to_lowercase();
        self.shown = (0..self.choices.len())
            .filter(|&i| q.is_empty() || self.choices[i].to_lowercase().contains(&q))
            .collect();
        self.list.clamp(self.shown.len());
    }

    fn chosen(&self) -> Option<&str> {
        self.list
            .selected()
            .and_then(|i| self.shown.get(i))
            .map(|&i| self.choices[i].as_str())
    }

    /// The colour the prompt would apply now: what was typed when that names
    /// one, else the highlighted row. `Some(None)` is "no colour".
    fn color(&self) -> Option<Option<Color>> {
        let typed = self.input.value().trim();
        if let Ok(c) = typed.parse::<Color>() {
            return Some(Some(c));
        }
        match self.chosen() {
            Some(NO_COLOR) => Some(None),
            Some(name) => name.parse::<Color>().ok().map(Some),
            None => None,
        }
    }

    /// The text written to the config for `color()`, as typed or as listed.
    fn color_text(&self) -> Option<Option<String>> {
        let typed = self.input.value().trim();
        if typed.parse::<Color>().is_ok() {
            return Some(Some(typed.to_string()));
        }
        match self.chosen() {
            Some(NO_COLOR) => Some(None),
            Some(name) => Some(Some(name.to_string())),
            None => None,
        }
    }
}

impl TagsView {
    /// Whether keys typed now belong to a text field on this page. The app
    /// routes digits, `/`, `?` and Tab to itself unless the search page's
    /// field has focus, so `app.rs` needs to consult this too before a hex
    /// colour with a 1 to 5 in it can be typed here; until it does, nothing
    /// calls it.
    pub fn paste(&mut self, text: &str, ctx: &Ctx) -> Vec<Action> {
        if self.filter.editing {
            let actions = self.filter.paste(text);
            self.refilter(ctx);
            return actions;
        }
        if self.focus_grid
            && let Some(view) = self.skill_search.as_mut()
        {
            return view.paste(text, ctx);
        }
        let Some(prompt) = self.prompt.as_mut() else {
            return vec![];
        };
        match prompt.input.paste(text) {
            Ok(true) => {
                prompt.refilter();
                vec![]
            }
            Ok(false) => vec![],
            Err(error) => vec![Action::Error(error.into())],
        }
    }

    pub fn input_focused(&self) -> bool {
        self.prompt.is_some()
            || self.filter.editing
            || (self.focus_grid
                && self
                    .skill_search
                    .as_ref()
                    .is_some_and(|v| v.input_focused()))
    }

    pub fn batch_finished(&mut self, failed: &[String]) {
        if let Some(view) = self.skill_search.as_mut() {
            view.batch_finished(failed);
        }
    }

    pub fn dialog_open(&self) -> bool {
        self.prompt.is_some()
    }

    fn refilter(&mut self, ctx: &Ctx) {
        let selected = self.selected_tag().map(str::to_owned);
        self.rows = self
            .all_rows
            .iter()
            .filter(|(tag, _)| self.filter.matches(tag))
            .cloned()
            .collect();
        self.list
            .select(selected.and_then(|tag| self.rows.iter().position(|r| r.0 == tag)));
        self.list.clamp(self.rows.len());
        self.sync_members(ctx.snap);
    }

    fn search_members(&mut self, ctx: &Ctx) {
        self.skill_search = Some(super::search::SearchView::panel(
            self.members.clone(),
            format!("Tag: {}", self.selected_tag().unwrap_or("none")),
            ctx,
        ));
        self.skill_search.as_mut().unwrap().hide_tags = true;
        self.focus_grid = true;
    }

    fn selected_tag(&self) -> Option<&str> {
        self.list
            .selected()
            .and_then(|i| self.rows.get(i))
            .map(|(t, _)| t.as_str())
    }

    /// A real tag under the cursor, which the untagged row is not.
    fn actionable_tag(&self) -> Option<String> {
        self.selected_tag()
            .filter(|t| *t != UNTAGGED)
            .map(str::to_string)
    }

    fn select_skills(&mut self, checked: Option<String>, ctx: &Ctx) -> Vec<Action> {
        self.search_members(ctx);
        self.skill_search.as_mut().unwrap().start_multi(checked);
        vec![]
    }

    fn selected_member(&self) -> Option<String> {
        self.grid
            .selected()
            .and_then(|i| self.members.get(i))
            .cloned()
    }

    /// Recompute the right pane for the tag under the cursor. Called after
    /// every move of the left one, so the two never disagree.
    fn sync_members(&mut self, snap: &Snapshot) {
        let old_members = self.members.clone();
        let tag = self.selected_tag().map(str::to_string);
        self.members = match tag.as_deref() {
            Some(UNTAGGED) => snap
                .skills
                .iter()
                .filter(|s| s.tags.is_empty() && s.status.is_present())
                .map(|s| s.key.clone())
                .collect(),
            Some(t) => snap
                .skills
                .iter()
                .filter(|s| s.tags.iter().any(|x| x == t))
                .map(|s| s.key.clone())
                .collect(),
            None => Vec::new(),
        };
        if old_members != self.members {
            self.skill_search = None;
        }
        self.grid.clamp(self.members.len());
    }

    /// Reading a skill must not cost the place on this page, so it opens in a
    /// window over it rather than by jumping to the search tab.
    fn open_member(&mut self) -> Vec<Action> {
        if let Some(k) = self.selected_member() {
            self.preview.open(k);
        }
        vec![]
    }

    fn ask_merge(&mut self) -> Vec<Action> {
        let Some(tag) = self.actionable_tag() else {
            return vec![];
        };
        let others: Vec<String> = self
            .rows
            .iter()
            .map(|(t, _)| t.clone())
            .filter(|t| t != &tag && t != UNTAGGED)
            .collect();
        if others.is_empty() {
            return vec![Action::Error("no other tag to merge into".into())];
        }
        self.prompt = Some(Prompt::new(Ask::Merge, &tag, others));
        vec![]
    }

    fn ask_color(&mut self) -> Vec<Action> {
        let Some(tag) = self.actionable_tag() else {
            return vec![];
        };
        let mut choices: Vec<String> = PALETTE.iter().map(|c| c.to_string()).collect();
        choices.push(NO_COLOR.into());
        self.prompt = Some(Prompt::new(Ask::Color, &tag, choices));
        vec![]
    }

    /// Carry out what the prompt has settled on and put it away.
    fn submit_prompt(&mut self) -> Vec<Action> {
        let Some(p) = self.prompt.as_ref() else {
            return vec![];
        };
        let tag = p.tag.clone();
        match p.ask {
            Ask::Merge => {
                let Some(into) = p.chosen().map(str::to_string) else {
                    return vec![Action::Error("pick a tag to merge into".into())];
                };
                self.prompt = None;
                vec![Action::WriteMeta(Box::new(move |ws| {
                    history::tag_edit(ws, |ws| {
                        edit::tag_rename(ws, &tag, &into)
                            .map(|n| format!("merged {tag} into {into} on {n} skill(s)"))
                    })
                }))]
            }
            Ask::Color => {
                let Some(color) = p.color_text() else {
                    return vec![Action::Error(format!(
                        "{:?} is not a colour: use a name or #rrggbb",
                        p.input.value().trim()
                    ))];
                };
                self.prompt = None;
                // Not a history step: the log knows tags on skills, not the
                // config, and a colour is cheap to set back by hand.
                vec![Action::Write(Box::new(move |ws| {
                    Config::set_tag_color(&ws.root, &tag, color.as_deref())?;
                    Ok(match color {
                        Some(c) => format!("{tag} is now {c}"),
                        None => format!("{tag} is back to the default colour"),
                    })
                }))]
            }
        }
    }

    fn prompt_key(&mut self, k: KeyEvent) -> Vec<Action> {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let Some(p) = self.prompt.as_mut() else {
            return vec![];
        };
        match k.code {
            KeyCode::Esc => {
                self.prompt = None;
            }
            KeyCode::Enter => return self.submit_prompt(),
            KeyCode::Down => p.list.move_by(1, p.shown.len()),
            KeyCode::Up => p.list.move_by(-1, p.shown.len()),
            KeyCode::Char('n') if ctrl => p.list.move_by(1, p.shown.len()),
            KeyCode::Char('p') if ctrl => p.list.move_by(-1, p.shown.len()),
            KeyCode::PageUp => p.list.first(p.shown.len()),
            KeyCode::PageDown => p.list.last(p.shown.len()),
            _ => {
                if p.input.handle_key(k) {
                    p.refilter();
                }
            }
        }
        vec![]
    }

    /// Mouse while the prompt is up: a click outside closes it, a click on a
    /// row picks it, and anything else stays with the prompt so the page does
    /// not react to a click it cannot see.
    fn prompt_mouse(&mut self, m: MouseEvent) -> Vec<Action> {
        let Some(p) = self.prompt.as_mut() else {
            return vec![];
        };
        let at = (m.column, m.row).into();
        match m.kind {
            MouseEventKind::ScrollDown => p.list.move_by(1, p.shown.len()),
            MouseEventKind::ScrollUp => p.list.move_by(-1, p.shown.len()),
            MouseEventKind::Down(MouseButton::Left) if !p.rect.contains(at) => {
                self.prompt = None;
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some((_, double)) = p.list.click(m.row, p.shown.len())
                    && double
                {
                    return self.submit_prompt();
                }
                p.input.click(m.column);
            }
            _ => {}
        }
        vec![]
    }

    /// One tag as a capsule, in the colour it has or the one being tried.
    fn pill(name: &str, fill: Color, ctx: &Ctx) -> Vec<Span<'static>> {
        let (lcap, rcap) = ctx.ws.config.ui.pill_caps.glyphs();
        vec![
            Span::styled(lcap.to_string(), Style::default().fg(fill)),
            Span::styled(
                format!(" {name} "),
                Style::default().bg(fill).fg(cards::ink(fill)),
            ),
            Span::styled(rcap.to_string(), Style::default().fg(fill)),
        ]
    }

    fn draw_tags(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        let th = ctx.theme;
        let focused = !self.focus_grid && self.prompt.is_none();
        let block = th.block(" tags ", focused);
        let inner = block.inner(area);
        f.render_widget(block, area);
        let content = Rect {
            width: inner.width.saturating_sub(1),
            ..inner
        };
        self.list.layout(content, 1, 1, 0, self.rows.len());
        let selected = self.list.selected();
        let w = content.width as usize;
        for i in self.list.visible() {
            let Some(cell) = self.list.cell(i) else {
                continue;
            };
            let (tag, count) = &self.rows[i];
            let on = selected == Some(i);
            let count = count.to_string();
            let count_w = width(&count).max(3);
            // The count sits at the right edge; whatever is left after the
            // marker and the count goes to the name and the description.
            let body_w = w.saturating_sub(2 + count_w + 1);
            let mut spans = vec![Span::styled(if on { "▸ " } else { "  " }, th.accent())];
            let mut used = 0;
            if tag == UNTAGGED {
                let s = fit(UNTAGGED, body_w);
                used += width(&s);
                spans.push(Span::raw(s));
            } else {
                let marker = fit("● ", body_w);
                used += width(&marker);
                spans.push(Span::styled(
                    marker,
                    Style::default().fg(tag_fill(tag, ctx)),
                ));
                let name = fit(tag, body_w.saturating_sub(used));
                used += width(&name);
                spans.push(Span::raw(name));
                let desc = ctx
                    .ws
                    .config
                    .tags
                    .iter()
                    .find(|t| &t.name == tag)
                    .and_then(|t| t.description.as_deref())
                    .unwrap_or("");
                if !desc.is_empty() && used + 3 < body_w {
                    let d = fit(desc, body_w - used - 2);
                    used += width(&d) + 2;
                    spans.push(Span::raw("  "));
                    spans.push(Span::styled(d, th.dim()));
                }
            }
            spans.push(Span::raw(" ".repeat(body_w.saturating_sub(used) + 1)));
            spans.push(Span::raw(pad(&count, count_w)));
            let style = if on && focused {
                th.selected()
            } else if on {
                th.selected_unfocused()
            } else {
                Style::default()
            };
            f.render_widget(Paragraph::new(Line::from(spans)).style(style), cell);
        }
        draw_track(f, inner, &self.list, selected, &mut self.list_track, th);
    }

    fn draw_members(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        let th = ctx.theme;
        let focused = self.focus_grid && self.prompt.is_none();
        let Some(tag) = self.selected_tag().map(str::to_string) else {
            let block = th.block(" skills ", focused);
            f.render_widget(block, area);
            self.grid_track.clear();
            return;
        };
        let count = match self.members.len() {
            1 => "1 skill".to_string(),
            n => format!("{n} skills"),
        };
        let mut title = vec![Span::raw(" ")];
        if tag == UNTAGGED {
            title.push(Span::styled("untagged", th.bold()));
        } else {
            title.push(Span::raw("tagged "));
            title.extend(Self::pill(&tag, tag_fill(&tag, ctx), ctx));
        }
        title.push(Span::styled(format!(" · {count} "), th.dim()));
        let block = th.block(Line::from(title), focused);
        let inner = block.inner(area);
        f.render_widget(block, area);
        let content = Rect {
            width: inner.width.saturating_sub(1),
            ..inner
        };
        let cols = cols_for(content.width);
        self.grid.layout(
            content,
            cols,
            CARD_H,
            if cols > 1 { 1 } else { 0 },
            self.members.len(),
        );
        if self.members.is_empty() {
            let text = if tag == UNTAGGED {
                "every skill has a tag"
            } else {
                "no skill carries this tag"
            };
            f.render_widget(
                Paragraph::new(Span::styled(text, th.dim())),
                Rect {
                    height: 1,
                    ..content
                },
            );
            self.grid_track.clear();
            return;
        }
        let selected = self.grid.selected();
        for i in self.grid.visible() {
            let (Some(cell), Some(r)) = (self.grid.cell(i), ctx.snap.get(&self.members[i])) else {
                continue;
            };
            let on = selected == Some(i);
            let ci = frame(f, cell, on, focused, th);
            let tail = r
                .source
                .as_ref()
                .map(|s| s.kind().to_string())
                .unwrap_or_default();
            let lines = skill_card(r, ctx, ci.width as usize, None, &tail, &[], false);
            f.render_widget(Paragraph::new(lines), ci);
        }
        draw_track(f, inner, &self.grid, selected, &mut self.grid_track, th);
    }

    fn draw_prompt(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        let th = ctx.theme;
        let Some(p) = self.prompt.as_mut() else {
            return;
        };
        let (title, placeholder) = match p.ask {
            Ask::Merge => (format!(" merge {} into ", p.tag), "type to filter…"),
            Ask::Color => (format!(" colour of {} ", p.tag), "a name, or #rrggbb"),
        };
        // Room for the field, a swatch line, the rows, and the frame.
        let want = p.shown.len() as u16 + 6;
        let w = 56.min(area.width.saturating_sub(2)).max(1);
        let h = want.clamp(8, area.height.saturating_sub(2)).max(1);
        let rect = Rect::new(
            area.x + (area.width - w) / 2,
            area.y + (area.height - h) / 2,
            w,
            h,
        );
        p.rect = rect;
        f.render_widget(Clear, rect);
        let block = th.block(title, true);
        let inner = block.inner(rect);
        f.render_widget(block, rect);
        let field = Rect {
            x: inner.x + 1,
            width: inner.width.saturating_sub(2),
            height: 1,
            ..inner
        };
        p.input.render(f, field, true, placeholder, th);
        // The swatch shows the pill as it would look, which is the only way
        // to judge a colour, and says so when the text is not one.
        let swatch = Rect {
            y: inner.y + 1,
            height: 1,
            ..field
        };
        let mut preview = vec![Span::raw("  ")];
        match p.ask {
            Ask::Merge => {
                preview.extend(Self::pill(&p.tag, tag_fill(&p.tag, ctx), ctx));
                preview.push(Span::styled(" → ", th.dim()));
                match p.chosen() {
                    Some(into) => preview.extend(Self::pill(into, tag_fill(into, ctx), ctx)),
                    None => preview.push(Span::styled("nothing matches", th.dim())),
                }
            }
            Ask::Color => match p.color() {
                Some(Some(c)) => preview.extend(Self::pill(&p.tag, c, ctx)),
                Some(None) => preview.extend(Self::pill(&p.tag, th.tag, ctx)),
                None => preview.push(Span::styled("not a colour", th.warn())),
            },
        }
        f.render_widget(Paragraph::new(Line::from(preview)), swatch);
        let list_area = Rect {
            y: inner.y + 3,
            height: inner.height.saturating_sub(3),
            ..inner
        };
        p.list.rows = list_area;
        let rows: Vec<ListItem> = p
            .shown
            .iter()
            .map(|&i| {
                let name = &p.choices[i];
                let spans = match p.ask {
                    Ask::Merge => Self::pill(name, tag_fill(name, ctx), ctx),
                    Ask::Color if name == NO_COLOR => {
                        vec![Span::styled("none — the default", th.dim())]
                    }
                    Ask::Color => {
                        let fill = name.parse::<Color>().unwrap_or(th.tag);
                        let mut s = Self::pill(&p.tag, fill, ctx);
                        s.push(Span::styled(format!("  {name}"), th.dim()));
                        s
                    }
                };
                ListItem::new(Line::from(spans))
            })
            .collect();
        f.render_stateful_widget(
            List::new(rows)
                .highlight_style(th.selected())
                .highlight_symbol("▸ "),
            list_area,
            &mut p.list.state,
        );
        if p.shown.is_empty() {
            f.render_widget(
                Paragraph::new(Span::styled("nothing matches", th.dim())),
                list_area,
            );
        }
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

impl View for TagsView {
    fn refresh(&mut self, ctx: &Ctx) {
        let selected = self.selected_tag().map(str::to_owned);
        let panel = self.skill_search.take();
        self.rows = ctx.snap.all_tags().into_iter().collect();
        let untagged = ctx
            .snap
            .skills
            .iter()
            .filter(|s| s.tags.is_empty() && s.status.is_present())
            .count();
        self.rows.push((UNTAGGED.into(), untagged));
        self.all_rows = self.rows.clone();
        self.rows.retain(|(tag, _)| self.filter.matches(tag));
        self.list.select(
            selected
                .as_ref()
                .and_then(|tag| self.rows.iter().position(|r| &r.0 == tag)),
        );
        self.list.clamp(self.rows.len());
        self.sync_members(ctx.snap);
        if self.selected_tag() == selected.as_deref()
            && let Some(mut view) = panel
        {
            view.update_panel(self.members.clone(), ctx);
            self.skill_search = Some(view);
        }
        self.fresh = ctx.ws.load_config().ok().map(|config| Workspace {
            config,
            ..ctx.ws.clone()
        });
    }

    fn handle_key(&mut self, k: KeyEvent, ctx: &Ctx) -> Vec<Action> {
        if self.prompt.is_some() {
            return self.prompt_key(k);
        }
        if self.focus_grid
            && let Some(view) = self.skill_search.as_mut()
        {
            if k.code == KeyCode::Left && view.panel_back() {
                self.focus_grid = false;
                return vec![];
            }
            return view.handle_key(k, ctx);
        }
        if !self.focus_grid && self.filter.key(k) {
            self.refilter(ctx);
            return vec![];
        }
        if self.focus_grid && matches!(k.code, KeyCode::Char('/' | 'm')) {
            self.search_members(ctx);
            let view = self.skill_search.as_mut().unwrap();
            if k.code == KeyCode::Char('m') {
                view.focus_list();
                return view.handle_key(k, ctx);
            }
            return vec![];
        }
        if self.preview.handle_key(k) {
            return vec![];
        }
        let n = self.rows.len();
        let m = self.members.len();
        if self.focus_grid {
            return match k.code {
                KeyCode::Char('m') => self.select_skills(None, ctx),
                KeyCode::Esc | KeyCode::Char('h') => {
                    self.focus_grid = false;
                    vec![]
                }
                // Along a row while there is a row; off the left edge is back
                // to the tag list, which is where the eye goes anyway.
                KeyCode::Left => {
                    if self
                        .grid
                        .selected()
                        .unwrap_or(0)
                        .is_multiple_of(self.grid.cols())
                    {
                        self.focus_grid = false;
                    } else {
                        self.grid.move_by(-1, m);
                    }
                    vec![]
                }
                KeyCode::Right | KeyCode::Char('l') => {
                    self.grid.move_by(1, m);
                    vec![]
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.grid.move_rows(1, m);
                    vec![]
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.grid.move_rows(-1, m);
                    vec![]
                }
                KeyCode::Home | KeyCode::Char('g') => {
                    self.grid.first(m);
                    vec![]
                }
                KeyCode::End | KeyCode::Char('G') => {
                    self.grid.last(m);
                    vec![]
                }
                KeyCode::Enter => self.open_member(),
                // Fixing one skill's tags from here saves a trip to search,
                // which is where the same key lives.
                KeyCode::Char('t') => match self.selected_member().and_then(|k| ctx.snap.get(&k)) {
                    Some(r) => vec![Action::OpenModal(Box::new(Modal::tags(&r.key, &r.tags)))],
                    None => vec![],
                },
                _ => vec![],
            };
        }
        match k.code {
            KeyCode::Char('q') => vec![Action::SwitchTab(Tab::Search)],
            // Esc means "back" everywhere else in the program, so here it goes
            // back to the search page rather than out of the door.
            KeyCode::Esc => vec![Action::SwitchTab(Tab::Search)],
            KeyCode::Down | KeyCode::Char('j') => {
                self.list.move_by(1, n);
                self.sync_members(ctx.snap);
                vec![]
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.list.move_by(-1, n);
                self.sync_members(ctx.snap);
                vec![]
            }
            KeyCode::Home | KeyCode::Char('g') => {
                self.list.first(n);
                self.sync_members(ctx.snap);
                vec![]
            }
            KeyCode::End | KeyCode::Char('G') => {
                self.list.last(n);
                self.sync_members(ctx.snap);
                vec![]
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                if m > 0 {
                    self.focus_grid = true;
                    self.grid.clamp(m);
                }
                vec![]
            }
            KeyCode::Char('r') => match self.actionable_tag() {
                Some(t) => vec![Action::OpenModal(Box::new(Modal::rename_tag(&t)))],
                None => vec![],
            },
            KeyCode::Char('m') => self.ask_merge(),
            KeyCode::Char('c') => self.ask_color(),
            KeyCode::Char('x') | KeyCode::Delete => match self.actionable_tag() {
                Some(t) => vec![Action::OpenModal(Box::new(Modal::delete_tag(&t)))],
                None => vec![],
            },
            _ => vec![],
        }
    }

    fn handle_mouse(&mut self, m: MouseEvent, ctx: &Ctx) -> Vec<Action> {
        if self.prompt.is_some() {
            return self.prompt_mouse(m);
        }
        if self.preview.handle_mouse(m) {
            return vec![];
        }
        let at = (m.column, m.row).into();
        if m.kind == MouseEventKind::Down(MouseButton::Left) && self.filter.rect.contains(at) {
            self.focus_grid = false;
            self.filter.editing = true;
            return vec![];
        }
        if self.right.contains(at)
            && let Some(view) = self.skill_search.as_mut()
        {
            self.focus_grid = true;
            return view.handle_mouse(m, ctx);
        }
        if let Some(d) = wheel(&m) {
            if self.left.contains(at) {
                self.list.move_by(d.signum(), self.rows.len());
                self.sync_members(ctx.snap);
            } else if self.right.contains(at) {
                self.grid.move_rows(d.signum(), self.members.len());
            }
            return vec![];
        }
        let pressing = matches!(m.kind, MouseEventKind::Down(MouseButton::Left));
        let dragging = matches!(m.kind, MouseEventKind::Drag(MouseButton::Left));
        // The tracks sit inside the panes, so they get first refusal.
        let pane = if pressing && self.list_track.hit(m.column, m.row) {
            Some(Pane::List)
        } else if pressing && self.grid_track.hit(m.column, m.row) {
            Some(Pane::Grid)
        } else if dragging {
            self.drag
        } else {
            None
        };
        if let Some(pane) = pane {
            self.drag = Some(pane);
            match pane {
                Pane::List => {
                    self.focus_grid = false;
                    if let Some(r) = self.list_track.index_at(m.row, self.list.grid_rows()) {
                        self.list.select_row(r);
                        self.sync_members(ctx.snap);
                    }
                }
                Pane::Grid => {
                    self.focus_grid = true;
                    if let Some(r) = self.grid_track.index_at(m.row, self.grid.grid_rows()) {
                        self.grid.select_row(r);
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
                self.focus_grid = false;
                if let Some((_, double)) = self.list.click(m.column, m.row) {
                    self.sync_members(ctx.snap);
                    if double && !self.members.is_empty() {
                        self.focus_grid = true;
                    }
                }
            } else if self.right.contains(at)
                && let Some((index, double)) = self.grid.click(m.column, m.row)
            {
                self.focus_grid = true;
                if self.grid.cell(index).is_some_and(|cell| {
                    m.row == cell.y + 1
                        && (cell.x + 2..cell.x + 2 + cards::MARKER_W as u16).contains(&m.column)
                }) {
                    return self.select_skills(self.selected_member(), ctx);
                }
                if double {
                    return self.open_member();
                }
            }
        }
        vec![]
    }

    fn draw(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        let (left, right) = split_panes(area, 38);
        self.left = left;
        self.right = right;
        // Draw with the config as it is on disk, so a pill shows its colour
        // the moment it is set. The reloaded workspace is taken out of `self`
        // for the duration, since a borrow of it would lock the rest.
        let fresh = self.fresh.take();
        let ctx = &Ctx {
            ws: fresh.as_ref().unwrap_or(ctx.ws),
            snap: ctx.snap,
            theme: ctx.theme,
        };
        let content = self.filter.draw(f, left, "Filter tags", ctx);
        self.draw_tags(f, content, ctx);
        if let Some(view) = self.skill_search.as_mut() {
            view.set_panel_active(self.focus_grid);
            view.draw(f, right, ctx);
        } else {
            self.draw_members(f, right, ctx);
        }
        self.preview.draw(f, area, ctx);
        self.draw_prompt(f, area, ctx);
        self.fresh = fresh;
    }

    fn hints(&self) -> Hints {
        if self.filter.editing {
            return &[("Enter/↓", "tags"), ("Esc", "finish filter")];
        }
        if self.focus_grid
            && let Some(view) = self.skill_search.as_ref()
        {
            return view.hints();
        }
        if let Some(hints) = self.preview.hints() {
            return hints;
        }
        match self.prompt.as_ref().map(|p| p.ask) {
            Some(Ask::Merge) => &[("↑/↓", "target"), ("Enter", "merge"), ("Esc", "cancel")],
            Some(Ask::Color) => &[
                ("type", "a name or #rrggbb"),
                ("Enter", "apply"),
                ("Esc", "cancel"),
            ],
            None if self.focus_grid => &[
                ("/", "filter skills"),
                ("Enter", "preview"),
                ("t", "edit tags"),
                ("m", "multi-select"),
                ("←/Esc", "tags"),
            ],
            None => &[
                ("/", "filter tags"),
                ("r", "rename"),
                ("m", "merge"),
                ("c", "colour"),
                ("x", "delete"),
                ("Enter/→", "skills"),
                ("q", "library"),
            ],
        }
    }
}
