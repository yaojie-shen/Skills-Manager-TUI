//! The skill preview: the full record and the rendered SKILL.md.
//!
//! Search panels can embed a preview beside results. Overlays retain the
//! originating page's query, selection and scroll position when closed.

use crate::tui::app::Ctx;
use crate::tui::components::skill::{status_glyph, status_text};
use crate::tui::text::{highlight_line, highlight_spans};
use crate::tui::theme::Theme;
use crate::tui::widgets::{OverlayClear as Clear, render_vertical_scrollbar, scrollbar_gutter};
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use skills::reconcile::{DeployState, SkillRecord};

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
    expanded_fields: bool,
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
        self.expanded_fields = false;
    }
    pub fn expand_fields(&mut self) {
        self.expanded_fields = true;
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
            ("e", "expand/collapse fields"),
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
            KeyCode::Char('e') => {
                self.expanded_fields = !self.expanded_fields;
                self.scroll = 0;
            }
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
    pub fn handle_mouse(&mut self, m: MouseEvent, ctx: &Ctx) -> bool {
        if !self.is_open() {
            return false;
        }
        match m.kind {
            MouseEventKind::ScrollDown => self.scroll_by(ctx.settings.interaction.wheel_rows),
            MouseEventKind::ScrollUp => self.scroll_by(-ctx.settings.interaction.wheel_rows),
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
        let th = &ctx.settings.theme;
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
                    .or_else(|| {
                        ctx.snap
                            .get(key)
                            .map(crate::tui::components::skill::display_name)
                    })
                    .unwrap_or(key)
                    .to_string(),
                th.bold(),
            ),
            Span::styled("  e fields · Esc closes ", th.dim()),
        ]);
        let block = th.block(title, true);
        let inner = block.inner(rect);
        let (content, track) = scrollbar_gutter(inner);
        f.render_widget(block, rect);
        self.height = content.height;
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
            if let Ok(doc) = &preview.doc {
                lines.push(kv("name", &doc.name, th));
            }
            if !self.expanded_fields {
                lines = lines
                    .into_iter()
                    .map(|line| single_line(line, content.width as usize))
                    .collect();
            }
            match &preview.doc {
                Ok(doc) => {
                    lines.extend(markdown_section(
                        "Description",
                        &doc.description,
                        content.width as usize,
                        &[],
                        th,
                    ));
                    lines.extend(markdown_section(
                        "SKILL.md",
                        &doc.body,
                        content.width as usize,
                        &[],
                        th,
                    ));
                }
                Err(error) => lines.push(Line::from(Span::styled(error.clone(), th.err()))),
            }
            lines
        } else if let Some(r) = ctx.snap.get(key) {
            record_lines(r, ctx, &[], content.width as usize, self.expanded_fields)
        } else {
            vec![Line::from(Span::styled("not in the skills root", th.err()))]
        };
        let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
        self.lines = paragraph.line_count(content.width);
        let max = self
            .lines
            .saturating_sub(content.height as usize)
            .min(u16::MAX as usize) as u16;
        self.scroll = self.scroll.min(max);
        f.render_widget(paragraph.scroll((self.scroll, 0)), content);
        render_vertical_scrollbar(
            f,
            track,
            self.lines,
            content.height as usize,
            self.scroll as usize,
            th.dim(),
        );
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
    record_lines(r, ctx, terms, available_width, false)
}

fn record_lines<'a>(
    r: &'a SkillRecord,
    ctx: &'a Ctx,
    terms: &[String],
    available_width: usize,
    expanded: bool,
) -> Vec<Line<'a>> {
    let th = &ctx.settings.theme;
    let mut lines = vec![Line::from(vec![
        Span::styled(
            crate::tui::components::skill::display_name(r),
            th.bold().fg(th.accent),
        ),
        Span::raw("  "),
        status_glyph(&r.status, th),
        Span::raw(" "),
        Span::styled(status_text(&r.status), th.dim()),
    ])];
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
        crate::tui::components::skill::repository_badge(r, ctx.settings.ui.icons)
            .unwrap_or_else(|| r.source_kind().into()),
        th,
    ));
    if let Some(source) = r.source.as_ref().filter(|source| source.is_remote()) {
        lines.push(kv(
            "location",
            crate::tui::icons::source(ctx.settings.ui.icons, source),
            th,
        ));
    }
    if r.external {
        lines.push(kv("path", format!("{} (symlink)", r.path.display()), th));
    }
    if !expanded {
        lines = lines
            .into_iter()
            .map(|line| single_line(line, available_width))
            .collect();
    }
    let mut memberships = Vec::new();
    for (kind, label, names) in [
        (crate::tui::components::group::Kind::Tag, "tags", &r.tags),
        (
            crate::tui::components::group::Kind::Preset,
            "presets",
            &r.presets,
        ),
    ] {
        if kind == crate::tui::components::group::Kind::Tag && !ctx.settings.tags_enabled {
            continue;
        }
        let mut spans = vec![Span::styled(format!("{label:<9}"), th.dim())];
        if names.is_empty() {
            spans.push(Span::styled("none", th.dim()));
        } else {
            spans.extend(crate::tui::components::group::membership_badges(
                kind,
                names,
                ctx,
                usize::MAX,
                usize::MAX,
            ));
        }
        memberships.push(Line::from(spans));
    }
    // Memberships wrap instead of truncating: details expose every association.
    lines.splice(1..1, memberships);
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

/// Metadata is one physical row, even when stored values contain line breaks.
fn single_line(line: Line<'_>, columns: usize) -> Line<'static> {
    use unicode_segmentation::UnicodeSegmentation;
    use unicode_width::UnicodeWidthStr;
    let mut spans: Vec<Span<'static>> = line
        .spans
        .into_iter()
        .map(|span| {
            Span::styled(
                span.content
                    .chars()
                    .map(|c| {
                        if c.is_control() || matches!(c, '\u{2028}' | '\u{2029}') {
                            ' '
                        } else {
                            c
                        }
                    })
                    .collect::<String>(),
                span.style,
            )
        })
        .collect();
    if spans.iter().map(Span::width).sum::<usize>() <= columns {
        return Line::from(spans).style(line.style);
    }
    let mut left = columns.saturating_sub(1);
    let mut clipped = Vec::new();
    for span in spans.drain(..) {
        let mut text = String::new();
        for glyph in span.content.graphemes(true) {
            let width = UnicodeWidthStr::width(glyph);
            if width > left {
                break;
            }
            text.push_str(glyph);
            left -= width;
        }
        let complete = text.len() == span.content.len();
        clipped.push(Span::styled(text, span.style));
        if !complete || left == 0 {
            break;
        }
    }
    if columns > 0 {
        clipped.push(Span::raw("…"));
    }
    Line::from(clipped).style(line.style)
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scrolling_cjk_overlay_keeps_borders_and_internal_track_separate() {
        let root = skills::ops::DownloadDir::new("cjk-overlay-boundaries").unwrap();
        let skill = root.path().join("cjk");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            format!(
                "---\nname: cjk\ndescription: {}\n---\n{}",
                "中文❤️".repeat(40),
                "中文中文❤️中文中文\n".repeat(80)
            ),
        )
        .unwrap();
        let ws = skills::Workspace::open(root.path()).unwrap();
        let snap = ws.scan().unwrap();
        let settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &settings,
        };
        let mut overlay = Overlay::default();
        overlay.open("cjk".into());
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        for scroll in [0, u16::MAX] {
            overlay.scroll = scroll;
            terminal
                .draw(|frame| overlay.draw(frame, frame.area(), &ctx))
                .unwrap();
            let buffer = terminal.backend().buffer();
            let rect = overlay.rect;
            let track_x = rect.right() - 2;
            for y in rect.y + 1..rect.bottom() - 1 {
                assert_eq!(buffer[(rect.x, y)].symbol(), "│");
                assert_eq!(buffer[(rect.right() - 1, y)].symbol(), "│");
                assert!(matches!(buffer[(track_x, y)].symbol(), "║" | "█" | " "));
            }
            assert!(overlay.scroll <= (overlay.lines - overlay.height as usize) as u16);
        }
    }

    #[test]
    fn metadata_stays_on_one_row_with_wide_text_and_control_characters() {
        for columns in 0..80 {
            let row = single_line(
                Line::from(vec![
                    Span::raw("source   "),
                    Span::styled(
                        "https://example.org/文档\nnext\tfield\u{2028}value".repeat(8),
                        Style::default(),
                    ),
                ]),
                columns,
            );
            assert!(row.width() <= columns);
            assert!(
                row.spans
                    .iter()
                    .all(|s| !s.content.contains(['\n', '\t', '\u{2028}']))
            );
            let paragraph = Paragraph::new(vec![row]).wrap(Wrap { trim: false });
            if columns > 0 {
                assert_eq!(paragraph.line_count(columns as u16), 1);
            }
        }
    }
}
