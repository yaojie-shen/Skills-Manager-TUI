//! The skill preview: the full record and the rendered SKILL.md.
//!
//! The search page shows it in a pane; every other page shows it in an
//! overlay on top of what is already there. Opening a skill from a preset or
//! an agent's list used to jump to the search tab, which lost the place the
//! user was working in; a window over the page keeps it.

use super::{status_glyph, status_text};
use crate::tui::app::Ctx;
use crate::tui::theme::Theme;
use crate::tui::widgets::OverlayClear as Clear;
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::{Margin, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap};
use skills::reconcile::{DeployState, SkillRecord};
use skills::search::highlight_ranges;

/// A preview floating over a page. Lives in the view that opened it, so the
/// page underneath keeps its focus and cursor exactly as they were.
#[derive(Default)]
pub struct Overlay {
    key: Option<String>,
    agent_preview: Option<AgentPreview>,
    scroll: u16,
    rect: Rect,
    lines: usize,
    height: u16,
}

struct AgentPreview {
    agent: String,
    path: std::path::PathBuf,
    ownership: String,
    doc: Result<skills::skill::SkillDoc, String>,
}

impl Overlay {
    pub fn open(&mut self, key: String) {
        self.agent_preview = None;
        self.key = Some(key);
        self.scroll = 0;
    }
    /// Read the selected agent entry once, never substitute a same-named root skill.
    pub fn open_agent(
        &mut self,
        key: String,
        agent: String,
        path: std::path::PathBuf,
        ownership: String,
    ) {
        self.open(key);
        let doc = skills::skill::SkillDoc::load(&path).map_err(|e| format!("{e:#}"));
        self.agent_preview = Some(AgentPreview {
            agent,
            path,
            ownership,
            doc,
        });
    }
    pub fn close(&mut self) {
        self.agent_preview = None;
        self.key = None;
    }
    pub fn is_open(&self) -> bool {
        self.key.is_some()
    }
    pub fn hints(&self) -> Option<crate::tui::app::Hints> {
        self.is_open().then_some(&[
            ("↑↓/j/k", "scroll"),
            ("PgUp/PgDn", "page"),
            ("Home/End", "top/bottom"),
            ("Esc/Enter", "close"),
        ])
    }
    fn scroll_by(&mut self, delta: i32) {
        let max = (self.lines as i32 - self.height as i32).max(0);
        self.scroll = (self.scroll as i32 + delta).clamp(0, max) as u16;
    }

    /// Consume every page key while the overlay is open, including unbound
    /// keys, so hidden page actions cannot run underneath the preview.
    pub fn handle_key(&mut self, k: KeyEvent) -> bool {
        if !self.is_open() {
            return false;
        }
        match k.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => self.close(),
            KeyCode::Down | KeyCode::Char('j') => self.scroll_by(1),
            KeyCode::Up | KeyCode::Char('k') => self.scroll_by(-1),
            KeyCode::PageDown | KeyCode::Char(' ') => self.scroll_by(self.height as i32 - 2),
            KeyCode::PageUp => self.scroll_by(-(self.height as i32 - 2)),
            KeyCode::Home | KeyCode::Char('g') => self.scroll = 0,
            KeyCode::End | KeyCode::Char('G') => self.scroll_by(i32::MAX / 2),
            _ => {}
        }
        true
    }

    /// Mouse while the overlay is up: wheel scrolls it, a click outside
    /// closes it, anything else is swallowed so the page does not react to a
    /// click it cannot see.
    pub fn handle_mouse(&mut self, m: MouseEvent) -> bool {
        if !self.is_open() {
            return false;
        }
        match m.kind {
            MouseEventKind::ScrollDown => self.scroll_by(3),
            MouseEventKind::ScrollUp => self.scroll_by(-3),
            MouseEventKind::Down(MouseButton::Left)
                if !self.rect.contains((m.column, m.row).into()) =>
            {
                self.close()
            }
            _ => {}
        }
        true
    }

    /// Draw over `area`. Sized to the page rather than to the text, so the
    /// window stays put while the user scrolls through a long SKILL.md.
    pub fn draw(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        let Some(key) = &self.key else {
            self.rect = Rect::default();
            return;
        };
        let th = ctx.theme;
        let w = area.width.saturating_sub(8).clamp(20, 100);
        let h = area.height.saturating_sub(4).max(6);
        let rect = Rect {
            x: area.x + (area.width - w) / 2,
            y: area.y + (area.height - h) / 2,
            width: w,
            height: h,
        };
        self.rect = rect;
        f.render_widget(Clear, rect);
        let title = Line::from(vec![
            Span::raw(" "),
            Span::styled(
                self.agent_preview
                    .as_ref()
                    .and_then(|p| p.doc.as_ref().ok())
                    .map(|d| d.name.as_str())
                    .or_else(|| ctx.snap.get(key).map(super::cards::display_name))
                    .unwrap_or(key)
                    .to_string(),
                th.bold(),
            ),
            Span::styled("  Esc closes ", th.dim()),
        ]);
        let block = th.block(title, true);
        let inner = block.inner(rect);
        f.render_widget(block, rect);
        self.height = inner.height;
        let lines = if let Some(preview) = &self.agent_preview {
            let mut lines = vec![
                kv("agent", &preview.agent, th),
                kv(
                    "path",
                    preview.path.join("SKILL.md").display().to_string(),
                    th,
                ),
                kv("entry", &preview.ownership, th),
                Line::from(""),
            ];
            match &preview.doc {
                Ok(doc) => {
                    lines.push(kv("name", &doc.name, th));
                    lines.extend(markdown_section(
                        "Description",
                        &doc.description,
                        inner.width as usize,
                        &[],
                        th,
                    ));
                    lines.extend(markdown_section(
                        "SKILL.md",
                        &doc.body,
                        inner.width as usize,
                        &[],
                        th,
                    ));
                }
                Err(error) => lines.push(Line::from(Span::styled(error.clone(), th.err()))),
            }
            lines
        } else if let Some(r) = ctx.snap.get(key) {
            preview_lines(r, ctx, &[], inner.width as usize)
        } else {
            vec![Line::from(Span::styled("not in the skills root", th.err()))]
        };
        let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
        self.lines = paragraph.line_count(inner.width);
        let max = self
            .lines
            .saturating_sub(inner.height as usize)
            .min(u16::MAX as usize) as u16;
        self.scroll = self.scroll.min(max);
        f.render_widget(paragraph.scroll((self.scroll, 0)), inner);
        if self.lines > inner.height as usize {
            let mut sb = ScrollbarState::new(self.lines.saturating_sub(inner.height as usize))
                .position(self.scroll as usize);
            f.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .begin_symbol(None)
                    .end_symbol(None),
                rect.inner(Margin {
                    vertical: 1,
                    horizontal: 0,
                }),
                &mut sb,
            );
        }
    }
}

pub fn kv<'a>(k: &'a str, v: impl Into<String>, th: &Theme) -> Line<'a> {
    Line::from(vec![
        Span::styled(format!("{k:<9}"), th.dim()),
        Span::raw(v.into()),
    ])
}

pub fn preview_lines<'a>(
    r: &'a SkillRecord,
    ctx: &'a Ctx,
    terms: &[String],
    available_width: usize,
) -> Vec<Line<'a>> {
    let th = ctx.theme;
    let mut lines = vec![Line::from(vec![
        Span::styled(super::cards::display_name(r), th.bold().fg(th.accent)),
        Span::raw("  "),
        status_glyph(&r.status, th),
        Span::raw(" "),
        Span::styled(status_text(&r.status), th.dim()),
    ])];
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
            Some(DeployState::NotDeployed) => ("—", th.dim()),
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
            .map(|s| crate::tui::icons::source(ctx.ws.config.ui.icons, s))
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
        lines.extend(markdown_section(
            "Description",
            d,
            available_width,
            terms,
            th,
        ));
    }
    if let Some(b) = &r.body {
        lines.extend(markdown_section("SKILL.md", b, available_width, terms, th));
    }
    lines
}

/// Description and SKILL.md share a heading, divider, Markdown and highlighting.
fn markdown_section(
    title: &str,
    body: &str,
    width: usize,
    terms: &[String],
    th: &Theme,
) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::default(),
        Line::from(Span::styled(title.to_string(), th.bold().fg(th.accent))),
        Line::from(Span::styled("─".repeat(width.min(24)), th.dim())),
    ];
    lines.extend(
        crate::tui::markdown::render(body, width)
            .into_iter()
            .map(|line| highlight_line(line, terms, th)),
    );
    lines
}

/// Split `text` into spans, styling the parts that match `terms`.
pub fn highlight_spans<'a>(text: &str, terms: &[String], base: Style, th: &Theme) -> Vec<Span<'a>> {
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
pub fn highlight_line<'a>(line: Line<'a>, terms: &[String], th: &Theme) -> Line<'a> {
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
