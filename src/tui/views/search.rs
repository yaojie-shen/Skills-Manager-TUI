//! Search tab: input, result list, preview.

use super::cards::{self, CARD_H, cols_for, frame, skill_card};
use super::preview::{Overlay, highlight_spans, preview_lines};
use super::{View, split_panes, status_glyph, wheel};
use crate::tui::app::{Action, Ctx, Hints};
use crate::tui::event::Task;
use crate::tui::modal::Modal;
use crate::tui::widgets::{CardGrid, Input, ScrollTrack, fit, pad, width};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap};
use skills::config::{UiDensity, UiLayout};
use skills::ops::edit;
use skills::reconcile::{SkillRecord, SkillStatus};
use skills::search::{Hit, Query, Searcher};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Input,
    List,
    Preview,
}

pub struct SearchView {
    input: Input,
    focus: Focus,
    hits: Vec<Hit>,
    grid: CardGrid,
    preview_scroll: u16,
    preview_lines: usize,
    preview_height: u16,
    input_rect: Rect,
    list_rect: Rect,
    list_track: ScrollTrack,
    /// A drag keeps hold of the track even when the pointer wanders off it.
    track_drag: bool,
    preview_rect: Rect,
    esc_armed: bool,
    /// Session overrides for what `config.toml` set. Flipping these is a way to
    /// try a layout on for size; what the next start looks like stays the file's
    /// business, so neither is written back.
    layout: Option<UiLayout>,
    density: Option<UiDensity>,
    /// Grid layout has no standing preview pane, so it opens over the results.
    overlay: Overlay,
    searcher: Searcher,
}

impl Default for SearchView {
    fn default() -> Self {
        Self {
            input: Input::default(),
            focus: Focus::Input,
            hits: Vec::new(),
            grid: CardGrid::default(),
            preview_scroll: 0,
            preview_lines: 0,
            preview_height: 0,
            input_rect: Rect::default(),
            list_rect: Rect::default(),
            list_track: ScrollTrack::default(),
            track_drag: false,
            preview_rect: Rect::default(),
            esc_armed: false,
            layout: None,
            density: None,
            overlay: Overlay::default(),
            searcher: Searcher::new(),
        }
    }
}

impl SearchView {
    pub fn query(&self) -> String {
        self.input.value().to_string()
    }
    pub fn input_focused(&self) -> bool {
        self.focus == Focus::Input
    }
    pub fn focus_input(&mut self) {
        self.focus = Focus::Input;
    }
    pub fn focus_list(&mut self) {
        self.focus = Focus::List;
    }
    pub fn set_query(&mut self, q: &str, ctx: &Ctx) {
        self.input.set(q);
        self.run_search(ctx, false);
    }

    fn layout(&self, ctx: &Ctx) -> UiLayout {
        self.layout.unwrap_or(ctx.ws.config.ui.layout)
    }
    fn density(&self, ctx: &Ctx) -> UiDensity {
        self.density.unwrap_or(ctx.ws.config.ui.density)
    }

    fn selected<'a>(&self, ctx: &'a Ctx) -> Option<&'a SkillRecord> {
        self.grid
            .selected()
            .and_then(|i| self.hits.get(i))
            .map(|h| &ctx.snap.skills[h.index])
    }

    /// Re-run the query. `keep` preserves the selected skill (after a rescan);
    /// typing always jumps back to the best match.
    fn run_search(&mut self, ctx: &Ctx, keep: bool) {
        let key = if keep {
            self.selected(ctx).map(|r| r.key.clone())
        } else {
            None
        };
        let q = Query::parse(self.input.value());
        self.hits = self.searcher.search(&ctx.snap.skills, &q);
        let idx = key.and_then(|k| {
            self.hits
                .iter()
                .position(|h| ctx.snap.skills[h.index].key == k)
        });
        self.grid
            .select(idx.or(if self.hits.is_empty() { None } else { Some(0) }));
        if !keep {
            self.preview_scroll = 0;
        }
    }

    fn selected_terms(&self) -> &[String] {
        self.grid
            .selected()
            .and_then(|i| self.hits.get(i))
            .map(|h| h.terms.as_slice())
            .unwrap_or(&[])
    }

    /// One step along a row, which in a single column is one step down the list.
    fn move_sel(&mut self, delta: i32) {
        self.grid.move_by(delta, self.hits.len());
        self.preview_scroll = 0;
    }

    /// One step down or up, which crosses a whole grid row when there are several.
    fn move_row(&mut self, delta: i32) {
        self.grid.move_rows(delta, self.hits.len());
        self.preview_scroll = 0;
    }

    /// Split layout has a pane to step into; grid layout opens a window over
    /// the results instead, which is the same thing every other page does.
    fn open_preview(&mut self, ctx: &Ctx) {
        if self.hits.is_empty() {
            return;
        }
        if self.layout(ctx) == UiLayout::Grid {
            if let Some(r) = self.selected(ctx) {
                self.overlay.open(r.key.clone());
            }
        } else {
            self.focus = Focus::Preview;
        }
    }

    fn scroll_preview(&mut self, delta: i32) {
        let max = (self.preview_lines as i32 - self.preview_height as i32).max(0);
        self.preview_scroll = (self.preview_scroll as i32 + delta).clamp(0, max) as u16;
    }

    pub fn on_check(
        &mut self,
        results: &[(String, anyhow::Result<skills::ops::update::CheckResult>)],
        _ctx: &Ctx,
    ) -> Vec<Action> {
        let mut acts = Vec::new();
        for (key, r) in results {
            acts.push(match r {
                Ok(c) if c.update_available => Action::Toast(format!(
                    "{key}: update available {} → {}",
                    c.installed
                        .as_deref()
                        .map(skills::meta::short_rev)
                        .unwrap_or("-"),
                    skills::meta::short_rev(&c.remote)
                )),
                Ok(c) => Action::Toast(format!(
                    "{key}: up to date ({})",
                    skills::meta::short_rev(&c.remote)
                )),
                Err(e) => Action::Error(format!("{key}: {e:#}")),
            });
        }
        acts
    }

    // ---- actions on the selected skill ------------------------------------

    #[allow(clippy::result_large_err)]
    fn need_present<'a>(&self, ctx: &'a Ctx, what: &str) -> Result<&'a SkillRecord, Action> {
        match self.selected(ctx) {
            Some(r) if r.status.is_present() => Ok(r),
            Some(r) => Err(Action::Error(format!(
                "{}: cannot {what} a {} skill",
                r.key,
                r.status.label()
            ))),
            None => Err(Action::Error("nothing selected".into())),
        }
    }

    fn act_tags(&self, ctx: &Ctx) -> Vec<Action> {
        match self.need_present(ctx, "tag") {
            Ok(r) => vec![Action::OpenModal(Box::new(Modal::tags(&r.key, &r.tags)))],
            Err(a) => vec![a],
        }
    }
    fn act_note(&self, ctx: &Ctx) -> Vec<Action> {
        match self.need_present(ctx, "annotate") {
            Ok(r) => vec![Action::EditNote(r.key.clone())],
            Err(a) => vec![a],
        }
    }
    fn act_deploy(&self, ctx: &Ctx) -> Vec<Action> {
        match self.need_present(ctx, "deploy") {
            Ok(r) => vec![Action::OpenModal(Box::new(Modal::agent_pick(&r.key)))],
            Err(a) => vec![a],
        }
    }
    fn act_rename(&self, ctx: &Ctx) -> Vec<Action> {
        match self.need_present(ctx, "rename") {
            Ok(r) => vec![Action::OpenModal(Box::new(Modal::rename(&r.key)))],
            Err(a) => vec![a],
        }
    }
    fn act_set_source(&self, ctx: &Ctx) -> Vec<Action> {
        match self.need_present(ctx, "set the source of") {
            Ok(r) => vec![Action::OpenModal(Box::new(Modal::set_source(
                &r.key,
                r.source.as_ref(),
            )))],
            Err(a) => vec![a],
        }
    }
    fn act_accept(&self, ctx: &Ctx) -> Vec<Action> {
        let Some(r) = self.selected(ctx) else {
            return vec![];
        };
        if !matches!(
            r.status,
            SkillStatus::Modified | SkillStatus::Managed { no_baseline: true }
        ) {
            return vec![Action::Error(
                "accept applies to modified skills or skills without a baseline".into(),
            )];
        }
        let key = r.key.clone();
        vec![Action::Write(Box::new(move |ws| {
            edit::accept(ws, &key).map(|_| format!("baseline updated for {key}"))
        }))]
    }
    fn act_migrate(&self, ctx: &Ctx) -> Vec<Action> {
        let Some(r) = self.selected(ctx) else {
            return vec![];
        };
        let SkillStatus::Renamed { to } = &r.status else {
            return vec![Action::Error("migrate applies to renamed? skills".into())];
        };
        let (old, new) = (r.key.clone(), to.clone());
        vec![Action::Write(Box::new(move |ws| {
            edit::migrate_meta(ws, &old, &new).map(|_| format!("metadata moved {old} → {new}"))
        }))]
    }
    fn act_check(&self, ctx: &Ctx) -> Vec<Action> {
        let Some(r) = self.selected(ctx) else {
            return vec![];
        };
        if !matches!(r.source, Some(skills::meta::Source::Git { .. })) {
            return vec![Action::Error(format!("{} has no git source", r.key))];
        }
        vec![
            Action::Spawn(Task::Check(vec![r.key.clone()])),
            Action::Toast(format!("checking {}…", r.key)),
        ]
    }
    fn act_update(&self, ctx: &Ctx) -> Vec<Action> {
        let Some(r) = self.selected(ctx) else {
            return vec![];
        };
        if !matches!(r.source, Some(skills::meta::Source::Git { .. })) {
            return vec![Action::Error(format!("{} has no git source", r.key))];
        }
        vec![
            Action::Spawn(Task::Prepare(r.key.clone())),
            Action::Toast(format!("fetching {}…", r.key)),
        ]
    }
    fn act_remove(&self, ctx: &Ctx) -> Vec<Action> {
        let Some(r) = self.selected(ctx) else {
            return vec![];
        };
        vec![Action::OpenModal(Box::new(Modal::remove(&r.key)))]
    }
}

/// Drawing, split by band. `draw` itself only decides which of these run.
impl SearchView {
    fn draw_input(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        let th = ctx.theme;
        let title = Line::from(vec![
            Span::raw(" "),
            Span::styled(
                format!("{}/{}", self.hits.len(), ctx.snap.skills.len()),
                th.dim(),
            ),
            Span::raw(" "),
        ]);
        let block = th.block(title, self.focus == Focus::Input);
        let inner = block.inner(area);
        f.render_widget(block, area);
        self.input_rect = area;
        let prompt = Rect {
            x: inner.x + 1,
            width: 2,
            ..inner
        };
        f.render_widget(Paragraph::new(Span::styled("› ", th.accent())), prompt);
        let field = Rect {
            x: inner.x + 3,
            width: inner.width.saturating_sub(4),
            ..inner
        };
        self.input.render(
            f,
            field,
            self.focus == Focus::Input,
            "search skills…   tag:x  agent:y  status:modified  untagged",
            th,
        );
    }

    /// The results, as a grid of cells that happens to be one column wide in
    /// the split layout. Drawing cell by cell rather than through `List` is what
    /// lets a card carry its own frame and lets several sit on a row.
    fn draw_results(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        let th = ctx.theme;
        let agents = &ctx.snap.agents;
        let searching = !self.input.value().trim().is_empty()
            && !Query::parse(self.input.value()).text.is_empty();
        let cards = self.density(ctx) == UiDensity::Cards;

        let mut legend = vec![Span::raw(" skills ")];
        for a in agents {
            legend.push(Span::styled(
                format!("{} ", cards::abbrev(&a.key)),
                th.dim(),
            ));
        }
        let block = th.block(Line::from(legend), self.focus == Focus::List);
        let inner = block.inner(area);
        f.render_widget(block, area);

        // A framed card is three lines of content plus its own border; a row is
        // one line, two while an excerpt has something to say.
        let cell_h = if cards {
            CARD_H
        } else if searching {
            2
        } else {
            1
        };
        // One column is always kept back for the scrollbar so the column count
        // does not change under the user the moment the list grows past a screen.
        let usable = inner.width.saturating_sub(1);
        let cols = if self.layout(ctx) == UiLayout::Grid && cards {
            cols_for(usable)
        } else {
            1
        };
        let gap = if cols > 1 { 1 } else { 0 };
        let content = Rect {
            width: usable,
            ..inner
        };
        self.grid
            .layout(content, cols, cell_h, gap, self.hits.len());

        if self.hits.is_empty() {
            let msg = if ctx.snap.skills.is_empty() {
                "no skills in this root"
            } else {
                "no match"
            };
            f.render_widget(
                Paragraph::new(Span::styled(msg, th.dim())),
                Rect {
                    height: 1,
                    ..content
                },
            );
            self.list_track.clear();
            return;
        }

        let selected = self.grid.selected();
        for i in self.grid.visible() {
            let Some(cell) = self.grid.cell(i) else {
                continue;
            };
            let h = &self.hits[i];
            let r = &ctx.snap.skills[h.index];
            let on = selected == Some(i);
            if cards {
                let ci = frame(f, cell, on, self.focus == Focus::List, th);
                // While searching the card shows the excerpt around the match
                // and names the fields it matched in; a hit on the name or a
                // tag has no excerpt, so the description stays.
                let body = h
                    .excerpt
                    .as_ref()
                    .filter(|_| searching)
                    .map(|e| e.text.as_str());
                let tail = if searching {
                    h.fields
                        .iter()
                        .map(|f| f.label())
                        .collect::<Vec<_>>()
                        .join("·")
                } else {
                    r.source
                        .as_ref()
                        .map(|s| s.kind().to_string())
                        .unwrap_or_default()
                };
                f.render_widget(
                    Paragraph::new(skill_card(
                        r,
                        ctx,
                        agents,
                        ci.width as usize,
                        body,
                        &tail,
                        &h.terms,
                    )),
                    ci,
                );
            } else {
                let style = if on {
                    if self.focus == Focus::List {
                        th.selected()
                    } else {
                        th.selected_unfocused()
                    }
                } else {
                    Style::default()
                };
                let lines = row_lines(
                    r,
                    h,
                    ctx,
                    agents,
                    cell.width.saturating_sub(2) as usize,
                    searching,
                    on,
                );
                f.render_widget(Paragraph::new(lines).style(style), cell);
            }
        }

        // Item space here is grid rows, which is what the thumb is measuring and
        // what a click on the track has to land on.
        let vis = self.grid.visible_rows();
        if self.grid.grid_rows() > vis && inner.height > 0 {
            let track = Rect {
                x: inner.right().saturating_sub(1),
                y: inner.y,
                width: 1,
                height: inner.height,
            };
            self.list_track.set(track);
            let mut sb = ScrollbarState::new(self.grid.grid_rows())
                .position(selected.unwrap_or(0) / self.grid.cols())
                .viewport_content_length(vis);
            f.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .begin_symbol(None)
                    .end_symbol(None),
                track,
                &mut sb,
            );
        } else {
            self.list_track.clear();
        }
    }

    fn draw_preview(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        let th = ctx.theme;
        let block = th.block(" preview ", self.focus == Focus::Preview);
        let inner = block.inner(area);
        f.render_widget(block, area);
        self.preview_height = inner.height;
        let Some(r) = self.selected(ctx) else {
            f.render_widget(
                Paragraph::new(Span::styled("select a skill to preview", th.dim())),
                inner,
            );
            return;
        };
        let terms: Vec<String> = self.selected_terms().to_vec();
        let lines = preview_lines(r, ctx, &terms);
        // Count wrapped lines for scroll clamping (approximate: by display width).
        let w = inner.width.max(1) as usize;
        self.preview_lines = lines
            .iter()
            .map(|l| width(&l.to_string()).max(1).div_ceil(w))
            .sum();
        let max = self.preview_lines.saturating_sub(inner.height as usize) as u16;
        self.preview_scroll = self.preview_scroll.min(max);
        f.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .scroll((self.preview_scroll, 0)),
            inner,
        );
        if self.preview_lines > inner.height as usize {
            let mut sb =
                ScrollbarState::new(self.preview_lines.saturating_sub(inner.height as usize))
                    .position(self.preview_scroll as usize);
            f.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .begin_symbol(None)
                    .end_symbol(None),
                area.inner(ratatui::layout::Margin {
                    vertical: 1,
                    horizontal: 0,
                }),
                &mut sb,
            );
        }
    }
}

impl View for SearchView {
    fn refresh(&mut self, ctx: &Ctx) {
        self.searcher.configure(
            ctx.ws.config.search.clone(),
            skills::dict::Dictionaries::load(&ctx.ws.root, &ctx.ws.config.search.dictionaries),
        );
        self.searcher.index(&ctx.snap.skills);
        self.run_search(ctx, true);
    }

    fn handle_key(&mut self, k: KeyEvent, ctx: &Ctx) -> Vec<Action> {
        if self.overlay.handle_key(k) {
            return vec![];
        }
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let mut acts = Vec::new();
        match self.focus {
            Focus::Input => match k.code {
                KeyCode::Esc => {
                    if self.input.is_empty() {
                        if self.esc_armed {
                            return vec![Action::Quit];
                        }
                        self.esc_armed = true;
                        return vec![Action::Toast("press Esc again to quit".into())];
                    }
                    self.input.clear();
                    self.run_search(ctx, false);
                }
                // Focus moves even with nothing to select: the list is where
                // the action keys live, and installing the first skill needs them.
                KeyCode::Enter | KeyCode::Tab | KeyCode::Down => self.focus = Focus::List,
                KeyCode::Up => self.move_sel(-1),
                KeyCode::Char('n') if ctrl => self.move_sel(1),
                KeyCode::Char('p') if ctrl => self.move_sel(-1),
                _ => {
                    if self.input.handle_key(k) {
                        self.run_search(ctx, false);
                    }
                }
            },
            Focus::List => match k.code {
                KeyCode::Esc => self.focus = Focus::Input,
                KeyCode::Char('q') => return vec![Action::Quit],
                KeyCode::Down | KeyCode::Char('j') => self.move_row(1),
                KeyCode::Up | KeyCode::Char('k') => self.move_row(-1),
                KeyCode::PageDown | KeyCode::Char('f') if k.code == KeyCode::PageDown || ctrl => {
                    self.move_sel(self.grid.page())
                }
                KeyCode::PageUp | KeyCode::Char('b') if k.code == KeyCode::PageUp || ctrl => {
                    self.move_sel(-self.grid.page())
                }
                KeyCode::Home | KeyCode::Char('g') => self.grid.first(self.hits.len()),
                KeyCode::End | KeyCode::Char('G') => self.grid.last(self.hits.len()),
                // Along a row when there is a row to walk; otherwise the old
                // meaning, which is to step across into the preview.
                KeyCode::Right | KeyCode::Char('l') if self.grid.cols() > 1 => self.move_sel(1),
                KeyCode::Left | KeyCode::Char('h') if self.grid.cols() > 1 => self.move_sel(-1),
                KeyCode::Tab | KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                    self.open_preview(ctx)
                }
                KeyCode::Char('t') => acts = self.act_tags(ctx),
                KeyCode::Char('n') => acts = self.act_note(ctx),
                KeyCode::Char('d') => acts = self.act_deploy(ctx),
                KeyCode::Char('r') => acts = self.act_rename(ctx),
                KeyCode::Char('s') => acts = self.act_set_source(ctx),
                KeyCode::Char('a') => acts = self.act_accept(ctx),
                KeyCode::Char('m') => acts = self.act_migrate(ctx),
                KeyCode::Char('u') => acts = self.act_check(ctx),
                KeyCode::Char('U') => acts = self.act_update(ctx),
                KeyCode::Char('x') => acts = self.act_remove(ctx),
                KeyCode::Char('v') => {
                    self.density = Some(match self.density(ctx) {
                        UiDensity::Cards => UiDensity::Rows,
                        UiDensity::Rows => UiDensity::Cards,
                    })
                }
                KeyCode::Char('V') => {
                    self.layout = Some(match self.layout(ctx) {
                        UiLayout::Split => UiLayout::Grid,
                        UiLayout::Grid => UiLayout::Split,
                    });
                    self.overlay.close();
                }
                KeyCode::Char('i') => acts = vec![Action::OpenModal(Box::new(Modal::install()))],
                _ => {}
            },
            Focus::Preview => match k.code {
                KeyCode::Esc | KeyCode::Left | KeyCode::Char('h') | KeyCode::BackTab => {
                    self.focus = Focus::List;
                }
                KeyCode::Tab => self.focus = Focus::Input,
                KeyCode::Char('q') => return vec![Action::Quit],
                KeyCode::Down | KeyCode::Char('j') => self.scroll_preview(1),
                KeyCode::Up | KeyCode::Char('k') => self.scroll_preview(-1),
                KeyCode::PageDown | KeyCode::Char(' ') => {
                    self.scroll_preview(self.preview_height as i32 - 2)
                }
                KeyCode::PageUp => self.scroll_preview(-(self.preview_height as i32 - 2)),
                KeyCode::Char('d') if ctrl => self.scroll_preview(self.preview_height as i32 / 2),
                KeyCode::Char('u') if ctrl => {
                    self.scroll_preview(-(self.preview_height as i32 / 2))
                }
                KeyCode::Home | KeyCode::Char('g') => self.preview_scroll = 0,
                KeyCode::End | KeyCode::Char('G') => self.scroll_preview(i32::MAX / 2),
                KeyCode::Char('t') => acts = self.act_tags(ctx),
                KeyCode::Char('n') => acts = self.act_note(ctx),
                KeyCode::Char('d') => acts = self.act_deploy(ctx),
                KeyCode::Char('u') => acts = self.act_check(ctx),
                KeyCode::Char('U') => acts = self.act_update(ctx),
                _ => {}
            },
        }
        self.esc_armed = false;
        acts
    }

    fn handle_mouse(&mut self, m: MouseEvent, ctx: &Ctx) -> Vec<Action> {
        if self.overlay.handle_mouse(m) {
            return vec![];
        }
        let at = (m.column, m.row).into();
        if let Some(d) = wheel(&m) {
            if self.preview_rect.contains(at) {
                self.scroll_preview(d);
            } else if self.list_rect.contains(at) {
                // A wheel notch is a row of cards, however many are on it.
                self.move_row(d.signum());
            }
            return vec![];
        }
        // The track is inside `list_rect`, so it has to claim the event before
        // the cell hit-test below turns it into a click on a card.
        let dragging = matches!(m.kind, MouseEventKind::Drag(MouseButton::Left));
        let pressing = matches!(m.kind, MouseEventKind::Down(MouseButton::Left));
        if (pressing && self.list_track.hit(m.column, m.row)) || (dragging && self.track_drag) {
            self.track_drag = true;
            self.focus = Focus::List;
            if let Some(r) = self.list_track.index_at(m.row, self.grid.grid_rows()) {
                self.grid.select_row(r);
                self.preview_scroll = 0;
            }
            return vec![];
        }
        if !dragging {
            self.track_drag = false;
        }
        if let MouseEventKind::Down(MouseButton::Left) = m.kind {
            if self.input_rect.contains(at) {
                self.focus = Focus::Input;
                self.input.click(m.column);
            } else if self.preview_rect.contains(at) {
                self.focus = Focus::Preview;
            } else if self.list_rect.contains(at) {
                self.focus = Focus::List;
                if let Some((_, double)) = self.grid.click(m.column, m.row) {
                    self.preview_scroll = 0;
                    if double {
                        self.open_preview(ctx);
                    }
                }
            }
        }
        if let MouseEventKind::Down(MouseButton::Right) = m.kind
            && self.list_rect.contains(at)
            && self.grid.click(m.column, m.row).is_some()
        {
            self.focus = Focus::List;
            return self.act_deploy(ctx);
        }
        vec![]
    }

    fn draw(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Min(1)])
            .split(area);
        self.draw_input(f, rows[0], ctx);

        // Split keeps a preview open beside the results and so gets one column
        // of cards; grid spends the whole width on cards and puts the preview
        // over them when it is wanted.
        let grid = self.layout(ctx) == UiLayout::Grid;
        let (left, right) = if grid {
            (rows[1], Rect::default())
        } else {
            split_panes(rows[1], 38)
        };
        self.list_rect = left;
        self.draw_results(f, left, ctx);
        if grid {
            self.preview_rect = Rect::default();
            self.overlay.draw(f, rows[1], ctx);
        } else {
            self.preview_rect = right;
            self.draw_preview(f, right, ctx);
        }
    }

    fn hints(&self) -> Hints {
        match self.focus {
            Focus::Input => &[
                ("↑↓", "select"),
                ("Enter", "list"),
                ("Esc", "clear/quit"),
                ("Alt-1..5", "tabs"),
                ("F1", "help"),
            ],
            Focus::List => &[
                ("t", "tags"),
                ("n", "note"),
                ("d", "deploy"),
                ("r", "rename"),
                ("s", "source"),
                ("a", "accept"),
                ("u/U", "check/update"),
                ("x", "remove"),
                ("Enter", "preview"),
                ("i", "install"),
                ("v/V", "layout"),
            ],
            Focus::Preview => &[
                ("j/k", "scroll"),
                ("Esc", "back"),
                ("t", "tags"),
                ("n", "note"),
                ("d", "deploy"),
                ("/", "search"),
            ],
        }
    }
}

/// The compact density: one line, and a second carrying the excerpt while a
/// query is running. `on` draws the selection marker the list widget used to.
#[allow(clippy::too_many_arguments)]
fn row_lines<'a>(
    r: &'a SkillRecord,
    h: &'a Hit,
    ctx: &'a Ctx,
    agents: &'a [skills::reconcile::AgentReport],
    inner_w: usize,
    searching: bool,
    on: bool,
) -> Vec<Line<'a>> {
    let th = ctx.theme;
    let dep_w = agents.len() * 3;
    let key_w = 26.min(inner_w.saturating_sub(dep_w + 6));
    let mut spans = vec![
        Span::styled(if on { "▸ " } else { "  " }, th.accent()),
        status_glyph(&r.status, th),
        Span::raw(" "),
    ];
    spans.extend(highlight_spans(
        &pad(&r.key, key_w),
        &h.terms,
        Style::default(),
        th,
    ));
    let tags_w = inner_w.saturating_sub(key_w + 4 + dep_w + 2);
    if !r.tags.is_empty() && tags_w > 3 {
        spans.push(Span::raw(" "));
        spans.extend(highlight_spans(
            &pad(&r.tags.join(","), tags_w - 1),
            &h.terms,
            th.tag(),
            th,
        ));
    } else {
        spans.push(Span::raw(" ".repeat(tags_w)));
    }
    spans.push(Span::raw(" "));
    for a in agents {
        let (g, style) = cards::deploy_glyph(r.deploy.get(&a.key), th);
        spans.push(Span::styled(format!("{g}  "), style));
    }
    if !searching {
        return vec![Line::from(spans)];
    }
    let mut sub = vec![Span::raw("    ")];
    let fields: Vec<&str> = h.fields.iter().map(|f| f.label()).collect();
    sub.push(Span::styled(
        format!("{} ", fields.join("·")),
        th.dim().add_modifier(ratatui::style::Modifier::ITALIC),
    ));
    let avail = inner_w.saturating_sub(6 + width(&fields.join("·")));
    if let Some(e) = &h.excerpt {
        sub.extend(highlight_spans(
            &fit(&e.text, avail),
            &h.terms,
            th.dim(),
            th,
        ))
    }
    vec![Line::from(spans), Line::from(sub)]
}
