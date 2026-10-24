//! The skill preview: the full record and the rendered SKILL.md.
//!
//! Search panels can embed a preview beside results. Overlays retain the
//! originating page's query, selection and scroll position when closed.

use crate::tui::app::Ctx;
use crate::tui::components::skill::{status_glyph, status_text};
use crate::tui::text::{highlight_line, highlight_spans};
use crate::tui::theme::Theme;
use crate::tui::widgets::{PopupSurface, fit};
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
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
    rect: Rect,
    expanded_fields: bool,
    browser: Option<super::file_browser::Browser>,
    skill_root: Option<std::path::PathBuf>,
    metadata_rect: Rect,
    metadata_scroll: usize,
    metadata_lines: usize,
    metadata_height: usize,
    metadata_focus: bool,
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
        self.expanded_fields = false;
        self.browser = None;
        self.skill_root = None;
        self.metadata_rect = Rect::default();
        self.metadata_scroll = 0;
        self.metadata_focus = false;
    }
    pub fn expand_fields(&mut self) {
        self.expanded_fields = true;
        self.metadata_focus = true;
        self.metadata_scroll = 0;
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
        self.skill_root = Some(path.clone());
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
        self.browser = None;
        self.skill_root = None;
    }
    pub fn is_open(&self) -> bool {
        self.key.is_some()
    }
    pub fn accepts_file_browser(&self, output: &super::file_browser::Output) -> bool {
        self.browser
            .as_ref()
            .is_some_and(|browser| browser.accepts(output))
    }

    pub fn apply_file_browser(
        &mut self,
        output: super::file_browser::Output,
    ) -> Vec<super::file_browser::Request> {
        if let Some(browser) = &mut self.browser {
            browser.apply(output);
            browser.take_requests()
        } else {
            vec![]
        }
    }

    pub fn take_file_browser_requests(&mut self) -> Vec<super::file_browser::Request> {
        self.browser
            .as_mut()
            .map(super::file_browser::Browser::take_requests)
            .unwrap_or_default()
    }

    pub fn hints(&self) -> Option<crate::tui::app::Hints> {
        if !self.is_open() {
            None
        } else if self.metadata_focus {
            Some(&[
                ("j/k ↑/↓", "scroll fields"),
                ("PgUp/PgDn", "page"),
                ("Home/End", "top/bottom"),
                ("Tab", "content"),
                ("e", "less"),
                ("Esc/q", "close"),
            ])
        } else if self
            .browser
            .as_ref()
            .is_some_and(|browser| browser.focus == super::file_browser::Focus::Tree)
        {
            Some(&[
                ("↑↓/j/k", "files"),
                ("→/Tab", "content"),
                ("Enter/l", "open/expand"),
                ("←/h", "collapse/parent"),
                ("e", "details"),
                (".", "hidden"),
                ("PgUp/PgDn", "page"),
                ("Home/End", "first/last"),
                ("Esc/q", "close"),
            ])
        } else {
            Some(&[
                ("↑↓/j/k", "scroll content"),
                ("←/Tab", "files"),
                ("PgUp/PgDn", "page"),
                ("Home/End", "top/bottom"),
                ("e", "details"),
                ("Esc/q", "close"),
            ])
        }
    }

    fn scroll_metadata_by(&mut self, delta: isize) {
        let max = self.metadata_lines.saturating_sub(self.metadata_height);
        self.metadata_scroll = self.metadata_scroll.saturating_add_signed(delta).min(max);
    }

    /// Consume every page key while the overlay is open, including unbound
    /// keys, so hidden page actions cannot run underneath the preview.
    pub fn handle_key(&mut self, k: KeyEvent) -> bool {
        if !self.is_open() {
            return false;
        }
        if self.metadata_focus {
            match k.code {
                KeyCode::Esc | KeyCode::Char('q') => self.close(),
                KeyCode::Char('e') => {
                    self.expanded_fields = false;
                    self.metadata_focus = false;
                    self.metadata_scroll = 0;
                }
                KeyCode::Tab => {
                    self.metadata_focus = false;
                    if let Some(browser) = &mut self.browser {
                        browser.focus_preview();
                    }
                }
                KeyCode::Down | KeyCode::Char('j') => self.scroll_metadata_by(1),
                KeyCode::Up | KeyCode::Char('k') => self.scroll_metadata_by(-1),
                KeyCode::PageDown | KeyCode::Char(' ') => {
                    self.scroll_metadata_by(self.metadata_height.saturating_sub(1) as isize)
                }
                KeyCode::PageUp => {
                    self.scroll_metadata_by(-(self.metadata_height.saturating_sub(1) as isize))
                }
                KeyCode::Home | KeyCode::Char('g') => self.metadata_scroll = 0,
                KeyCode::End | KeyCode::Char('G') => self.scroll_metadata_by(isize::MAX),
                _ => {}
            }
            return true;
        }
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') => self.close(),
            KeyCode::Char('e') => {
                self.expanded_fields = !self.expanded_fields;
                self.metadata_scroll = 0;
                self.metadata_focus = self.expanded_fields;
            }
            KeyCode::Char('f') => {
                self.metadata_focus = false;
                if let Some(browser) = &mut self.browser {
                    browser.focus_tree();
                }
            }
            _ => {
                if let Some(browser) = &mut self.browser {
                    browser.handle_key(k);
                }
            }
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
        match crate::tui::widgets::popup_mouse(m, self.rect) {
            crate::tui::widgets::PopupMouse::OutsidePress => {
                self.close();
                return true;
            }
            crate::tui::widgets::PopupMouse::Inside => {}
            _ => return true,
        }
        if self.metadata_rect.contains((m.column, m.row).into()) {
            let amount = ctx.settings.interaction.wheel_rows.max(1) as usize;
            match m.kind {
                MouseEventKind::ScrollDown => {
                    self.expanded_fields = true;
                    self.metadata_focus = true;
                    let max = self.metadata_lines.saturating_sub(self.metadata_height);
                    self.metadata_scroll = self.metadata_scroll.saturating_add(amount).min(max);
                }
                MouseEventKind::ScrollUp => {
                    self.expanded_fields = true;
                    self.metadata_focus = true;
                    self.metadata_scroll = self.metadata_scroll.saturating_sub(amount);
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    self.expanded_fields = true;
                    self.metadata_focus = true;
                }
                _ => {}
            }
        } else if let Some(browser) = &mut self.browser
            && browser.handle_mouse(m, ctx.settings.interaction.wheel_rows)
        {
            self.metadata_focus = false;
        }
        true
    }

    fn popup_rect(area: Rect) -> Rect {
        let available = area.width.saturating_sub(4);
        let responsive = ((area.width as u32 * 4 / 5) as u16).max(100);
        let width = available.min(responsive.min(160)).max(1).min(area.width);
        let height = area.height.saturating_sub(4).max(6).min(area.height);
        Rect {
            x: area.x + area.width.saturating_sub(width) / 2,
            y: area.y + area.height.saturating_sub(height) / 2,
            width,
            height,
        }
    }

    fn metadata_height(&self, available: u16, lines: usize) -> u16 {
        if available < 8 {
            return 0;
        }
        let cap = if self.expanded_fields {
            available / 2
        } else {
            available / 3
        }
        .max(3);
        u16::try_from(lines)
            .unwrap_or(u16::MAX)
            .saturating_add(2)
            .min(cap)
            .min(available.saturating_sub(3))
    }

    /// Draw the unified metadata and file preview over `area`.
    pub fn draw(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        let Some(key) = self.key.clone() else {
            self.rect = Rect::default();
            return;
        };
        let th = &ctx.settings.theme;
        let rect = Self::popup_rect(area);
        self.rect = rect;
        if self.skill_root.is_none() {
            self.skill_root = self
                .agent_preview
                .as_ref()
                .map(|preview| preview.path.clone())
                .or_else(|| ctx.snap.get(&key).map(|record| record.path.clone()));
        }
        if self.browser.is_none()
            && let Some(root) = self.skill_root.clone()
        {
            self.browser = Some(super::file_browser::Browser::open(key.clone(), root));
        }

        let display_name = self
            .agent_preview
            .as_ref()
            .and_then(|preview| preview.doc.as_ref().ok())
            .map(|doc| doc.name.as_str())
            .or_else(|| {
                ctx.snap
                    .get(&key)
                    .map(crate::tui::components::skill::display_name)
            })
            .unwrap_or(&key);
        let title = format!(
            " {} ",
            fit(
                &skills::util::display_safe(display_name),
                rect.width.saturating_sub(4) as usize
            )
        );
        let block = th.block(title, true);
        let inner = block.inner(rect);
        f.render_widget(PopupSurface, rect);
        f.render_widget(block, rect);

        let metadata_width = inner.width.saturating_sub(2) as usize;
        let mut metadata = if let Some(preview) = &self.agent_preview {
            let mut lines = Vec::new();
            if let Ok(doc) = &preview.doc {
                lines.push(kv("name", &doc.name, th));
            }
            lines.extend([
                kv("agent", &preview.agent, th),
                kv(
                    "path",
                    preview.path.join("SKILL.md").display().to_string(),
                    th,
                ),
                kv("entry", &preview.ownership, th),
            ]);
            if let Err(error) = &preview.doc {
                lines.push(Line::from(Span::styled(
                    skills::util::display_safe(error).into_owned(),
                    th.err(),
                )));
            }
            lines
        } else if let Some(record) = ctx.snap.get(&key) {
            record_metadata_lines(
                record,
                ctx,
                &[],
                metadata_width,
                self.expanded_fields,
                MetadataMode::Overlay,
            )
        } else {
            vec![Line::from(Span::styled("not in the skills root", th.err()))]
        };
        metadata = metadata
            .into_iter()
            .map(|line| {
                single_line(
                    line,
                    if self.expanded_fields {
                        usize::MAX
                    } else {
                        metadata_width
                    },
                )
            })
            .collect();
        let metadata_paragraph = Paragraph::new(metadata).wrap(Wrap { trim: false });
        self.metadata_lines = metadata_paragraph.line_count(metadata_width.max(1) as u16);
        let metadata_height = self.metadata_height(inner.height, self.metadata_lines);
        let (metadata_area, browser_area) = if metadata_height == 0 {
            (Rect::default(), inner)
        } else {
            let areas = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(metadata_height), Constraint::Min(3)])
                .split(inner);
            (areas[0], areas[1])
        };
        if metadata_height > 0 {
            let metadata_block = th.block(" details ", self.metadata_focus);
            let content = metadata_block.inner(metadata_area);
            self.metadata_rect = content;
            self.metadata_height = content.height as usize;
            let max = self.metadata_lines.saturating_sub(self.metadata_height);
            self.metadata_scroll = self.metadata_scroll.min(max);
            f.render_widget(metadata_block, metadata_area);
            f.render_widget(
                metadata_paragraph.scroll((self.metadata_scroll.min(u16::MAX as usize) as u16, 0)),
                content,
            );
        } else {
            self.metadata_rect = Rect::default();
            self.metadata_height = 0;
            self.metadata_scroll = 0;
        }

        if let Some(browser) = &mut self.browser {
            browser.draw(f, browser_area, ctx, !self.metadata_focus);
        } else {
            f.render_widget(
                Paragraph::new("Files are unavailable for this entry")
                    .block(th.block(" Files ", false)),
                browser_area,
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
    record_lines(r, ctx, terms, available_width, false)
}

fn record_lines<'a>(
    r: &'a SkillRecord,
    ctx: &'a Ctx,
    terms: &[String],
    available_width: usize,
    expanded: bool,
) -> Vec<Line<'a>> {
    let mut lines = record_metadata_lines(
        r,
        ctx,
        terms,
        available_width,
        expanded,
        MetadataMode::Inline,
    );
    if let Some(d) = &r.description {
        lines.extend(markdown_section(
            "Description",
            d,
            available_width,
            terms,
            &ctx.settings.theme,
        ));
    }
    if let Some(b) = &r.body {
        lines.extend(markdown_section(
            "SKILL.md",
            b,
            available_width,
            terms,
            &ctx.settings.theme,
        ));
    }
    lines
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MetadataMode {
    Inline,
    Overlay,
}

fn record_metadata_lines<'a>(
    r: &'a SkillRecord,
    ctx: &'a Ctx,
    terms: &[String],
    available_width: usize,
    expanded: bool,
    mode: MetadataMode,
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
    if (mode == MetadataMode::Inline || expanded)
        && let Some(source) = r.source.as_ref().filter(|source| source.is_remote())
    {
        lines.push(kv(
            "location",
            crate::tui::icons::source(ctx.settings.ui.icons, source),
            th,
        ));
    }
    if (mode == MetadataMode::Inline || expanded) && r.external {
        lines.push(kv("path", format!("{} (symlink)", r.path.display()), th));
    }
    if mode == MetadataMode::Inline && !expanded {
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
    if (mode == MetadataMode::Inline || expanded)
        && let Some(n) = &r.note
    {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled("note", th.bold().fg(th.tag))));
        for l in n.lines() {
            lines.push(Line::from(highlight_spans(l, terms, Style::default(), th)));
        }
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
                skills::util::display_safe(&span.content)
                    .chars()
                    .map(|c| {
                        if matches!(c, '\n' | '\t' | '\u{2028}' | '\u{2029}') {
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

    fn settle_browser(overlay: &mut Overlay) {
        let mut requests = overlay.take_file_browser_requests();
        while let Some(request) = requests.pop() {
            requests.extend(overlay.apply_file_browser(super::super::file_browser::load(&request)));
        }
    }

    #[test]
    fn metadata_tree_and_content_focus_keep_highlights_and_hints_together() {
        let root = skills::ops::DownloadDir::new("preview-focus-hints").unwrap();
        skills::config::Config {
            agents: vec![],
            ..Default::default()
        }
        .save(root.path())
        .unwrap();
        std::fs::create_dir(root.path().join("sample")).unwrap();
        std::fs::write(
            root.path().join("sample/SKILL.md"),
            "---\nname: sample\ndescription: Sample\n---\nBody",
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
        overlay.open("sample".into());
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 30)).unwrap();
        terminal
            .draw(|frame| overlay.draw(frame, frame.area(), &ctx))
            .unwrap();
        overlay.handle_key(KeyCode::Left.into());
        assert!(overlay.hints().unwrap().contains(&("↑↓/j/k", "files")));
        overlay.handle_key(KeyCode::Right.into());
        assert!(
            overlay
                .hints()
                .unwrap()
                .contains(&("↑↓/j/k", "scroll content"))
        );
        assert!(
            !overlay
                .hints()
                .unwrap()
                .iter()
                .any(|(key, _)| key.contains("Enter"))
        );
        overlay.handle_key(KeyCode::Char('e').into());
        terminal
            .draw(|frame| overlay.draw(frame, frame.area(), &ctx))
            .unwrap();
        assert!(
            overlay
                .hints()
                .unwrap()
                .contains(&("j/k ↑/↓", "scroll fields"))
        );
        let buffer = terminal.backend().buffer();
        let browser_top = overlay.metadata_rect.bottom() + 1;
        assert_eq!(
            buffer[(overlay.rect.x + 1, browser_top)].fg,
            settings.theme.border,
            "the file tree cannot look focused while metadata owns the keys"
        );
        overlay.handle_mouse(
            MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: overlay.rect.right() - 3,
                row: overlay.rect.bottom() - 3,
                modifiers: crossterm::event::KeyModifiers::NONE,
            },
            &ctx,
        );
        assert!(!overlay.metadata_focus);
        assert!(
            overlay
                .hints()
                .unwrap()
                .contains(&("↑↓/j/k", "scroll content"))
        );
        overlay.handle_key(KeyCode::Left.into());
        assert!(overlay.hints().unwrap().contains(&("↑↓/j/k", "files")));
    }

    #[test]
    fn file_browser_overlay_neutralizes_control_characters_in_skill_key() {
        let root = skills::ops::DownloadDir::new("browser-overlay-key").unwrap();
        let ws = skills::Workspace::open(root.path()).unwrap();
        let snap = ws.scan().unwrap();
        let settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &settings,
        };
        let mut overlay = Overlay::default();
        overlay.open("unsafe\x1b[2J".into());
        overlay.skill_root = Some(root.path().to_path_buf());
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| overlay.draw(frame, frame.area(), &ctx))
            .unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(!rendered.contains('\x1b'));
        assert!(rendered.contains("unsafe\u{fffd}[2J"));
    }

    #[test]
    fn unified_overlay_opens_files_directly_and_scales_responsively() {
        let root = skills::ops::DownloadDir::new("unified-overlay").unwrap();
        let skill = root.path().join("sample");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            "---\nname: sample\ndescription: compact metadata\n---\n# Direct body\nread me",
        )
        .unwrap();
        let ws = skills::Workspace::open(root.path()).unwrap();
        let mut snap = ws.scan().unwrap();
        let record = snap
            .skills
            .iter_mut()
            .find(|record| record.key == "sample")
            .unwrap();
        record.tags = (0..40).map(|index| format!("tag-{index:02}")).collect();
        record.note = Some(
            (0..30)
                .map(|index| format!("note line {index}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        let settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &settings,
        };
        let mut overlay = Overlay::default();
        overlay.open("sample".into());
        overlay.metadata_scroll = 4;
        overlay.expand_fields();
        assert!(overlay.expanded_fields);
        assert!(overlay.metadata_focus);
        assert_eq!(overlay.metadata_scroll, 0);
        overlay.handle_key(KeyCode::Char('e').into());
        let mut widths = Vec::new();
        for width in [60, 100, 200, 300] {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 30)).unwrap();
            terminal
                .draw(|frame| overlay.draw(frame, frame.area(), &ctx))
                .unwrap();
            settle_browser(&mut overlay);
            terminal
                .draw(|frame| overlay.draw(frame, frame.area(), &ctx))
                .unwrap();
            let rendered = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            assert!(rendered.contains("sample"));
            assert!(rendered.contains("Direct body"));
            if width >= 100 {
                assert!(rendered.contains("Files"));
                assert!(rendered.contains("SKILL.md"));
            }
            widths.push(overlay.rect.width);
        }
        assert_eq!(widths, [56, 96, 160, 160]);

        overlay.handle_key(KeyCode::Char('f').into());
        assert_eq!(
            overlay.browser.as_ref().unwrap().focus,
            super::super::file_browser::Focus::Tree
        );
        overlay.handle_key(KeyCode::Char('e').into());
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        terminal
            .draw(|frame| overlay.draw(frame, frame.area(), &ctx))
            .unwrap();
        assert!(overlay.metadata_lines > overlay.metadata_height);
        overlay.handle_key(KeyCode::End.into());
        assert!(overlay.metadata_scroll > 0);
        overlay.handle_key(KeyCode::Tab.into());
        assert!(!overlay.metadata_focus);
        assert_eq!(
            overlay.browser.as_ref().unwrap().focus,
            super::super::file_browser::Focus::Preview,
            "Tab from metadata always returns to content"
        );
        let rendered = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Direct body"));

        overlay.open("sample".into());
        terminal
            .draw(|frame| overlay.draw(frame, frame.area(), &ctx))
            .unwrap();
        overlay.handle_mouse(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: overlay.metadata_rect.x,
                row: overlay.metadata_rect.y,
                modifiers: crossterm::event::KeyModifiers::NONE,
            },
            &ctx,
        );
        assert!(overlay.expanded_fields);
        assert!(overlay.metadata_focus);
        overlay.handle_key(KeyCode::Char('e').into());
        assert!(!overlay.expanded_fields);
        assert!(!overlay.metadata_focus);

        overlay.handle_key(KeyCode::Esc.into());
        assert!(!overlay.is_open(), "Esc closes the unified overlay once");
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
