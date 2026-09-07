//! Search tab: input, result list, preview.

use super::{View, split_panes, status_glyph, status_text, wheel};
use crate::tui::app::{Action, Ctx, Hints};
use crate::tui::event::Task;
use crate::tui::modal::Modal;
use crate::tui::widgets::{Input, ListNav, fit, pad, width};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    List, ListItem, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap,
};
use skills::ops::deploy;
use skills::ops::edit;
use skills::reconcile::{DeployState, SkillRecord, SkillStatus};
use skills::search::{Hit, Query, Searcher, highlight_ranges};

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
    list: ListNav,
    preview_scroll: u16,
    preview_lines: usize,
    preview_height: u16,
    input_rect: Rect,
    list_rect: Rect,
    preview_rect: Rect,
    esc_armed: bool,
    searcher: Searcher,
}

impl Default for SearchView {
    fn default() -> Self {
        Self {
            input: Input::default(),
            focus: Focus::Input,
            hits: Vec::new(),
            list: ListNav::default(),
            preview_scroll: 0,
            preview_lines: 0,
            preview_height: 0,
            input_rect: Rect::default(),
            list_rect: Rect::default(),
            preview_rect: Rect::default(),
            esc_armed: false,
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

    fn selected<'a>(&self, ctx: &'a Ctx) -> Option<&'a SkillRecord> {
        self.list
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
        self.list
            .select(idx.or(if self.hits.is_empty() { None } else { Some(0) }));
        if !keep {
            self.preview_scroll = 0;
        }
    }

    fn selected_terms(&self) -> &[String] {
        self.list
            .selected()
            .and_then(|i| self.hits.get(i))
            .map(|h| h.terms.as_slice())
            .unwrap_or(&[])
    }

    fn move_sel(&mut self, delta: i32) {
        self.list.move_by(delta, self.hits.len());
        self.preview_scroll = 0;
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
    fn act_toggle_agent(&self, ctx: &Ctx, idx: usize) -> Vec<Action> {
        let Ok(r) = self.need_present(ctx, "deploy") else {
            return vec![];
        };
        let Some(agent) = ctx.ws.config.agents.get(idx) else {
            return vec![];
        };
        let deployed = matches!(r.deploy.get(&agent.key), Some(DeployState::Deployed));
        let plan = if deployed {
            deploy::plan_undeploy(
                ctx.ws,
                ctx.snap,
                std::slice::from_ref(&r.key),
                std::slice::from_ref(&agent.key),
            )
        } else {
            deploy::plan_deploy(
                ctx.ws,
                ctx.snap,
                std::slice::from_ref(&r.key),
                std::slice::from_ref(&agent.key),
            )
        };
        match plan {
            Ok(actions) => vec![Action::ApplyLinks {
                title: format!(
                    "{} {} on {}",
                    if deployed { "undeploy" } else { "deploy" },
                    r.key,
                    agent.key
                ),
                actions,
            }],
            Err(e) => vec![Action::Error(format!("{e:#}"))],
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
                KeyCode::Enter | KeyCode::Tab | KeyCode::Down => {
                    if !self.hits.is_empty() {
                        self.focus = Focus::List;
                    }
                }
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
                KeyCode::Down | KeyCode::Char('j') => self.move_sel(1),
                KeyCode::Up | KeyCode::Char('k') => self.move_sel(-1),
                KeyCode::PageDown | KeyCode::Char('f') if k.code == KeyCode::PageDown || ctrl => {
                    self.move_sel(self.list.page())
                }
                KeyCode::PageUp | KeyCode::Char('b') if k.code == KeyCode::PageUp || ctrl => {
                    self.move_sel(-self.list.page())
                }
                KeyCode::Home | KeyCode::Char('g') => self.list.first(self.hits.len()),
                KeyCode::End | KeyCode::Char('G') => self.list.last(self.hits.len()),
                KeyCode::Tab | KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                    if !self.hits.is_empty() {
                        self.focus = Focus::Preview;
                    }
                }
                KeyCode::Char('t') => acts = self.act_tags(ctx),
                KeyCode::Char('n') => acts = self.act_note(ctx),
                KeyCode::Char('d') => acts = self.act_deploy(ctx),
                KeyCode::Char('a') => acts = self.act_accept(ctx),
                KeyCode::Char('m') => acts = self.act_migrate(ctx),
                KeyCode::Char('u') => acts = self.act_check(ctx),
                KeyCode::Char('U') => acts = self.act_update(ctx),
                KeyCode::Char('x') => acts = self.act_remove(ctx),
                KeyCode::Char(c @ '1'..='9') if ctrl => {
                    acts = self.act_toggle_agent(ctx, (c as u8 - b'1') as usize)
                }
                _ => {}
            },
            Focus::Preview => match k.code {
                KeyCode::Esc | KeyCode::Left | KeyCode::Char('h') | KeyCode::BackTab => {
                    self.focus = Focus::List
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
        let at = (m.column, m.row).into();
        if let Some(d) = wheel(&m) {
            if self.list_rect.contains(at) {
                self.move_sel(d);
            } else if self.preview_rect.contains(at) {
                self.scroll_preview(d);
            }
            return vec![];
        }
        if let MouseEventKind::Down(MouseButton::Left) = m.kind {
            if self.input_rect.contains(at) {
                self.focus = Focus::Input;
                self.input.click(m.column);
            } else if self.list_rect.contains(at) {
                self.focus = Focus::List;
                if let Some((_, double)) = self.list.click(m.row, self.hits.len()) {
                    self.preview_scroll = 0;
                    if double {
                        self.focus = Focus::Preview;
                    }
                }
            } else if self.preview_rect.contains(at) {
                self.focus = Focus::Preview;
            }
        }
        if let MouseEventKind::Down(MouseButton::Right) = m.kind
            && self.list_rect.contains(at)
            && self.list.click(m.row, self.hits.len()).is_some()
        {
            self.focus = Focus::List;
            return self.act_deploy(ctx);
        }
        vec![]
    }

    fn draw(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        let th = ctx.theme;
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Min(1)])
            .split(area);

        // Input.
        let title = Line::from(vec![
            Span::raw(" "),
            Span::styled(
                format!("{}/{}", self.hits.len(), ctx.snap.skills.len()),
                th.dim(),
            ),
            Span::raw(" "),
        ]);
        let block = th.block(title, self.focus == Focus::Input);
        let inner = block.inner(rows[0]);
        f.render_widget(block, rows[0]);
        self.input_rect = rows[0];
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

        let (left, right) = split_panes(rows[1], 38);
        self.list_rect = left;
        self.preview_rect = right;

        // List.
        let agents = &ctx.snap.agents;
        let inner_w = left.width.saturating_sub(4) as usize; // borders + highlight symbol
        let dep_w = agents.len() * 3;
        let key_w = self
            .hits
            .iter()
            .map(|h| width(&ctx.snap.skills[h.index].key))
            .max()
            .unwrap_or(8)
            .min(inner_w.saturating_sub(dep_w + 6));
        let searching = !self.input.value().trim().is_empty()
            && !Query::parse(self.input.value()).text.is_empty();
        self.list.item_height = if searching { 2 } else { 1 };
        // The selected row paints its own background instead of relying on
        // `highlight_style`, which is patched over span styles and would
        // erase the highlighter background on a match.
        let selected = self.list.selected();
        let items: Vec<ListItem> = self
            .hits
            .iter()
            .enumerate()
            .map(|(row, h)| {
                let row_style = if selected == Some(row) {
                    if self.focus == Focus::List {
                        th.selected()
                    } else {
                        th.selected_unfocused()
                    }
                } else {
                    Style::default()
                };
                let r = &ctx.snap.skills[h.index];
                let mut spans = vec![status_glyph(&r.status, th), Span::raw(" ")];
                spans.extend(highlight_spans(
                    &pad(&r.key, key_w),
                    &h.terms,
                    Style::default(),
                    th,
                ));
                let tags_w = inner_w.saturating_sub(key_w + 2 + dep_w + 2);
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
                    let (g, style) = match r.deploy.get(&a.key) {
                        Some(DeployState::Deployed) => ("✓", th.ok()),
                        Some(DeployState::Broken) => ("!", th.err()),
                        Some(DeployState::Shadow { .. }) | Some(DeployState::Foreign) => {
                            ("~", th.warn())
                        }
                        _ => ("·", th.dim()),
                    };
                    spans.push(Span::styled(format!("{g}  "), style));
                }
                if !searching {
                    return ListItem::new(Line::from(spans)).style(row_style);
                }
                // Second line: where it matched and an excerpt with highlights.
                let mut sub = vec![Span::raw("  ")];
                let fields: Vec<&str> = h.fields.iter().map(|f| f.label()).collect();
                sub.push(Span::styled(
                    format!("{} ", fields.join("·")),
                    th.dim().add_modifier(ratatui::style::Modifier::ITALIC),
                ));
                let avail = inner_w.saturating_sub(4 + fields.join("·").len());
                if let Some(e) = &h.excerpt {
                    sub.extend(highlight_spans(
                        &fit(&e.text, avail),
                        &h.terms,
                        th.dim(),
                        th,
                    ))
                }
                ListItem::new(vec![Line::from(spans), Line::from(sub)]).style(row_style)
            })
            .collect();
        let mut legend = vec![Span::raw(" skills ")];
        for a in agents {
            legend.push(Span::styled(format!("{} ", abbrev(&a.key)), th.dim()));
        }
        let list_title = Line::from(legend);
        let block = th.block(list_title, self.focus == Focus::List);
        self.list.set_area_from_block(left);
        let list = List::new(items).block(block).highlight_symbol("▸ ");
        f.render_stateful_widget(list, left, &mut self.list.state);
        if self.hits.is_empty() {
            let msg = if ctx.snap.skills.is_empty() {
                "no skills in this root"
            } else {
                "no match"
            };
            f.render_widget(
                Paragraph::new(Span::styled(msg, th.dim())),
                Rect {
                    x: left.x + 2,
                    y: left.y + 1,
                    width: left.width.saturating_sub(4),
                    height: 1,
                },
            );
        }

        // Preview.
        let block = th.block(" preview ", self.focus == Focus::Preview);
        let inner = block.inner(right);
        f.render_widget(block, right);
        self.preview_height = inner.height;
        if let Some(r) = self.selected(ctx) {
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
            let p = Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .scroll((self.preview_scroll, 0));
            f.render_widget(p, inner);
            if self.preview_lines > inner.height as usize {
                let mut sb =
                    ScrollbarState::new(self.preview_lines.saturating_sub(inner.height as usize))
                        .position(self.preview_scroll as usize);
                let track = right.inner(ratatui::layout::Margin {
                    vertical: 1,
                    horizontal: 0,
                });
                f.render_stateful_widget(
                    Scrollbar::new(ScrollbarOrientation::VerticalRight)
                        .begin_symbol(None)
                        .end_symbol(None),
                    track,
                    &mut sb,
                );
            }
        } else {
            f.render_widget(
                Paragraph::new(Span::styled("select a skill to preview", th.dim())),
                inner,
            );
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
                ("a", "accept"),
                ("u/U", "check/update"),
                ("x", "remove"),
                ("Enter", "preview"),
                ("/", "search"),
                ("?", "help"),
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

/// Two-letter agent abbreviation used as a column header.
fn abbrev(key: &str) -> String {
    key.chars().take(2).collect()
}

fn kv<'a>(k: &'a str, v: impl Into<String>, th: &crate::tui::theme::Theme) -> Line<'a> {
    Line::from(vec![
        Span::styled(format!("{k:<9}"), th.dim()),
        Span::raw(v.into()),
    ])
}

fn preview_lines<'a>(r: &'a SkillRecord, ctx: &'a Ctx, terms: &[String]) -> Vec<Line<'a>> {
    let th = ctx.theme;
    let mut lines = vec![Line::from(vec![
        Span::styled(r.key.as_str(), th.bold().fg(th.accent)),
        Span::raw("  "),
        status_glyph(&r.status, th),
        Span::raw(" "),
        Span::styled(status_text(&r.status), th.dim()),
    ])];
    if r.name_mismatch {
        lines.push(kv(
            "name",
            format!("{}  ≠ directory name", r.name.as_deref().unwrap_or("")),
            th,
        ));
    }
    let mut tag_line = vec![Span::styled(format!("{:<9}", "tags"), th.dim())];
    if r.tags.is_empty() {
        tag_line.push(Span::styled("none", th.dim()));
    } else {
        for t in &r.tags {
            tag_line.push(Span::styled(
                format!(" {t} "),
                th.tag().bg(ctx.theme.selection_bg),
            ));
            tag_line.push(Span::raw(" "));
        }
    }
    lines.push(Line::from(tag_line));
    let mut dep = vec![Span::styled(format!("{:<9}", "deploy"), th.dim())];
    for a in &ctx.snap.agents {
        let (txt, style) = match r.deploy.get(&a.key) {
            Some(DeployState::Deployed) => ("✓", th.ok()),
            Some(DeployState::NotDeployed) => ("·", th.dim()),
            Some(DeployState::Shadow { same_content: true }) => ("shadow", th.warn()),
            Some(DeployState::Shadow {
                same_content: false,
            }) => ("shadow≠", th.warn()),
            Some(DeployState::Foreign) => ("foreign", th.warn()),
            Some(DeployState::Broken) => ("broken", th.err()),
            Some(DeployState::NoAgentDir) | None => ("no dir", th.dim()),
        };
        dep.push(Span::raw(format!("{} ", a.key)));
        dep.push(Span::styled(format!("{txt}   "), style));
    }
    lines.push(Line::from(dep));
    lines.push(kv(
        "source",
        r.source
            .as_ref()
            .map(|s| s.summary())
            .unwrap_or_else(|| "none".into()),
        th,
    ));
    if r.external {
        lines.push(kv("path", format!("{} (symlink)", r.path.display()), th));
    }
    if let Some(n) = &r.note {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled("note", th.bold().fg(th.tag))));
        for l in n.lines() {
            lines.push(Line::from(highlight_spans(l, terms, Style::default(), th)));
        }
    }
    if let Some(d) = &r.description {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "description",
            th.bold().fg(th.accent),
        )));
        lines.push(Line::from(highlight_spans(d, terms, Style::default(), th)));
    }
    if let Some(b) = &r.body {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "SKILL.md",
            th.bold().fg(th.accent),
        )));
        lines.push(Line::from(Span::styled("─".repeat(24), th.dim())));
        for line in tui_markdown::from_str(b).lines {
            lines.push(highlight_line(line, terms, th));
        }
    }
    lines
}

/// Split `text` into spans, styling the parts that match `terms`.
fn highlight_spans<'a>(
    text: &str,
    terms: &[String],
    base: Style,
    th: &crate::tui::theme::Theme,
) -> Vec<Span<'a>> {
    let ranges = highlight_ranges(text, terms);
    if ranges.is_empty() {
        return vec![Span::styled(text.to_string(), base)];
    }
    // The hit keeps none of the surrounding style: a highlighter covers what
    // is under it, and the dimmed grey of an excerpt would be unreadable on yellow.
    let hl = th.match_hit();
    let mut out = Vec::new();
    let mut pos = 0;
    for (s, e) in ranges {
        if s > pos {
            out.push(Span::styled(text[pos..s].to_string(), base));
        }
        out.push(Span::styled(text[s..e].to_string(), hl));
        pos = e;
    }
    if pos < text.len() {
        out.push(Span::styled(text[pos..].to_string(), base));
    }
    out
}

/// Apply highlighting to every span of an already styled line (markdown output).
fn highlight_line<'a>(line: Line<'a>, terms: &[String], th: &crate::tui::theme::Theme) -> Line<'a> {
    if terms.is_empty() {
        return line;
    }
    let mut spans = Vec::new();
    for sp in line.spans {
        let base = sp.style;
        spans.extend(highlight_spans(&sp.content, terms, base, th));
    }
    Line::from(spans)
        .style(line.style)
        .alignment(line.alignment.unwrap_or_default())
}
