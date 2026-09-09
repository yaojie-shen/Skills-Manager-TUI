//! The framed skill card, shared by every page that lays skills out in a grid.
//!
//! A card shows its name, description, tags and source inside a rounded frame. The frame carries the selection,
//! so a match highlighted inside keeps its own background instead of being
//! painted over.

use super::preview::highlight_spans;
use crate::tui::app::Ctx;
use crate::tui::theme::Theme;
use crate::tui::widgets::{fit, pad, width};
use ratatui::Frame;
use ratatui::layout::{Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders};
use skills::reconcile::SkillRecord;

/// Narrowest a card may get before the grid gives up a column. Below this the
/// name and description become too cramped to read.
pub const MIN_CARD_W: u16 = 40;
/// Four content lines: identity, two description lines, and source with tags.
pub const CARD_H: u16 = 6;
/// Status and selection share a slot, including a separator before the name.
pub const MARKER_W: usize = 4;

/// Columns that fit in `width`, always at least one.
pub fn cols_for(width: u16) -> usize {
    ((width / MIN_CARD_W) as usize).max(1)
}

/// Draw the frame of one card and hand back the padded area inside it.
/// `on` is the selection; `focused` says whether that selection is the one the
/// keyboard is on, which is told by weight rather than by a second colour.
pub fn frame(f: &mut Frame, cell: Rect, on: bool, focused: bool, th: &Theme) -> Rect {
    frame_styled(
        f,
        cell,
        if on && focused {
            th.accent().add_modifier(Modifier::BOLD)
        } else if on {
            th.accent()
        } else {
            th.dim()
        },
    )
}

/// The same frame in a colour of the caller's choosing, for a card that is
/// warning about something.
pub fn frame_styled(f: &mut Frame, cell: Rect, border: Style) -> Rect {
    let b = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border);
    let inner = b.inner(cell).inner(Margin {
        horizontal: 1,
        vertical: 0,
    });
    f.render_widget(b, cell);
    inner
}

/// Text colour that stays legible on a filled pill.
pub fn ink(fill: Color) -> Color {
    match fill {
        Color::DarkGray | Color::Black | Color::Blue | Color::Red | Color::Magenta => Color::White,
        _ => Color::Black,
    }
}

/// The colour a tag is filled with: the one `[[tags]]` gives it in the config,
/// else the theme's tag colour, so untitled tags still read as tags.
pub fn tag_fill(name: &str, ctx: &Ctx) -> Color {
    ctx.ws
        .config
        .tags
        .iter()
        .find(|t| t.name == name)
        .and_then(|t| t.color.as_deref())
        .and_then(|c| c.parse::<Color>().ok())
        .unwrap_or(ctx.theme.tag)
}

/// Tags as capsules, as many as fit in `max_w`, then a count for the rest. A
/// filled shape with round ends is told apart from the text around it at a
/// glance, which a coloured word is not.
pub fn tag_pills(tags: &[String], ctx: &Ctx, max_w: usize) -> Vec<Span<'static>> {
    let (lcap, rcap) = ctx.ws.config.ui.pill_caps.glyphs();
    let cap_w = width(lcap) + width(rcap);
    let mut out = Vec::new();
    let mut used = 0;
    for (i, t) in tags.iter().enumerate() {
        let body = format!(" {t} ");
        let w = cap_w + width(&body) + usize::from(i > 0);
        // Keep room for the "+n" so the last thing on the line is never a
        // pill cut in half.
        let rest = tags.len() - i - 1;
        let reserve = if rest > 0 {
            width(&format!(" +{rest}"))
        } else {
            0
        };
        if used + w + reserve > max_w {
            let count = fit(&format!(" +{}", rest + 1), max_w.saturating_sub(used));
            if !count.is_empty() {
                out.push(Span::styled(count, ctx.theme.dim()));
            }
            break;
        }
        let fill = tag_fill(t, ctx);
        if i > 0 {
            out.push(Span::raw(" "));
        }
        out.push(Span::styled(lcap.to_string(), Style::default().fg(fill)));
        out.push(Span::styled(body, Style::default().bg(fill).fg(ink(fill))));
        out.push(Span::styled(rcap.to_string(), Style::default().fg(fill)));
        used += w;
    }
    out
}

/// Human-facing identity; filesystem and deployment keys remain unchanged.
pub fn display_name(r: &SkillRecord) -> &str {
    r.name
        .as_deref()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| r.key.rsplit('/').next().unwrap_or(&r.key))
}

/// A readable Git source badge, separate from the skill's own name.
pub fn repository_badge(r: &SkillRecord, icons: skills::config::Icons) -> Option<String> {
    use skills::meta::Source;
    match &r.source {
        Some(Source::Git { url, .. }) => {
            let name = skills::repository::source_name(url).unwrap_or_else(|| url.clone());
            Some(format!("{} {name}", crate::tui::icons::git(icons, url)))
        }
        Some(Source::Local { .. }) => Some(crate::tui::icons::local(icons).into()),
        None => skills::repository::alias_of(&r.key)
            .map(|alias| format!("{} {alias}", crate::tui::icons::package(icons))),
    }
}

/// The checkbox preserves the status slot and separates it from the name.
pub fn checkbox_marker(checked: bool, th: &Theme) -> Span<'static> {
    Span::styled(
        if checked { "[✓] " } else { "[ ] " },
        if checked { th.accent() } else { th.dim() },
    )
}

/// A fixed-width leading slot, replaced by a checkbox in selection mode.
pub fn health_marker(r: &SkillRecord, th: &Theme) -> Span<'static> {
    use skills::reconcile::SkillStatus::*;
    let (glyph, style) = match &r.status {
        Managed { no_baseline: false } => ("●   ", th.ok()),
        Managed { no_baseline: true } => ("●   ", th.warn()),
        Unmanaged => ("○   ", th.dim()),
        Modified => ("~   ", th.warn()),
        Missing | Invalid { .. } | CorruptMeta { .. } => ("!   ", th.err()),
        Renamed { .. } => ("!   ", th.warn()),
    };
    Span::styled(glyph, style)
}

/// Render Markdown as readable text and wrap at words where possible. CJK and
/// long unbroken tokens wrap at grapheme boundaries, never splitting an emoji.
pub(super) fn summary_lines(markdown: &str, columns: usize) -> [String; 2] {
    let plain = tui_markdown::from_str(markdown)
        .lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if columns == 0 {
        return [String::new(), String::new()];
    }
    let span = Span::raw(plain.as_str());
    let mut used = 0;
    let mut end = 0;
    for g in span.styled_graphemes(Style::default()) {
        let next = width(g.symbol);
        if used + next > columns {
            break;
        }
        used += next;
        end += g.symbol.len();
    }
    if end == plain.len() {
        return [plain, String::new()];
    }
    // Prefer a word boundary unless it would leave more than half a line blank.
    if let Some(space) = plain[..end].rfind(' ')
        && width(&plain[..space]) >= columns / 2
    {
        end = space;
    }
    let first = plain[..end].trim_end().to_owned();
    let rest = plain[end..].trim_start();
    let second = if width(rest) <= columns {
        rest.to_owned()
    } else {
        let span = Span::raw(rest);
        let mut result = String::new();
        let mut used = 0;
        for g in span.styled_graphemes(Style::default()) {
            let next = width(g.symbol);
            if used + next > columns.saturating_sub(1) {
                break;
            }
            used += next;
            result.push_str(g.symbol);
        }
        result.push('…');
        result
    };
    [first, second]
}

/// Identity, a two-line readable summary, and source beside right-aligned tags.
/// `body` supplies a search excerpt; `tail` can add page-specific context.
pub fn skill_card(
    r: &SkillRecord,
    ctx: &Ctx,
    inner_w: usize,
    body: Option<&str>,
    tail: &str,
    terms: &[String],
) -> Vec<Line<'static>> {
    let th = ctx.theme;
    let source = repository_badge(r, ctx.ws.config.ui.icons)
        .unwrap_or_else(|| crate::tui::icons::local(ctx.ws.config.ui.icons).into());
    let source = if tail.is_empty() || tail == "git" || tail == "local" {
        source
    } else {
        format!("{source} · {tail}")
    };
    let mut head = vec![health_marker(r, th)];
    if inner_w < MARKER_W {
        head[0].content = fit(&head[0].content, inner_w).into();
    }
    head.extend(highlight_spans(
        &pad(display_name(r), inner_w.saturating_sub(MARKER_W)),
        terms,
        th.bold(),
        th,
    ));

    let summary = summary_lines(
        body.or(r.description.as_deref())
            .unwrap_or("No description"),
        inner_w,
    );
    let tags_budget = if r.tags.is_empty() { 0 } else { inner_w / 2 };
    let pills = tag_pills(&r.tags, ctx, tags_budget);
    let pills_w: usize = pills.iter().map(|s| width(&s.content)).sum();
    let source_budget = inner_w.saturating_sub(pills_w + usize::from(pills_w > 0));
    let source = fit(&source, source_budget);
    let mut foot = vec![Span::styled(source.clone(), th.dim())];
    foot.push(Span::raw(
        " ".repeat(inner_w.saturating_sub(width(&source) + pills_w)),
    ));
    foot.extend(pills);
    vec![
        Line::from(head),
        Line::from(highlight_spans(&summary[0], terms, th.dim(), th)),
        Line::from(highlight_spans(&summary[1], terms, th.dim(), th)),
        Line::from(foot),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summaries_wrap_readable_markdown_and_keep_graphemes_intact() {
        assert_eq!(summary_lines("**Hello** `world`", 20), ["Hello world", ""]);
        assert_eq!(
            summary_lines("中文测试日历管理", 8),
            ["中文测试", "日历管理"]
        );
        assert_eq!(
            summary_lines("one two three four five", 10),
            ["one two", "three fou…"]
        );
        assert_eq!(summary_lines("👩‍💻👩‍💻👩‍💻", 4), ["👩‍💻👩‍💻", "👩‍💻"]);
        assert_eq!(cols_for(280), 7);
    }

    #[test]
    fn cards_use_frontmatter_names_and_keep_repository_identity_separate() {
        let dir = skills::ops::DownloadDir::new("card-render-test").unwrap();
        let root = dir.path();
        skills::config::Config {
            agents: vec![],
            ..Default::default()
        }
        .save(root)
        .unwrap();
        let key = "repos/sampleorg--kit/skills--mock-calendar";
        let path = root.join(key);
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(
            path.join("SKILL.md"),
            "---\nname: mock-calendar\ndescription: 日历管理\n---\nCalendar tools\n",
        )
        .unwrap();
        let ws = skills::Workspace::open(root).unwrap();
        ws.meta
            .save(
                key,
                &skills::meta::SkillMeta {
                    source: Some(skills::meta::Source::Git {
                        url: "https://github.com/sampleorg/kit.git".into(),
                        branch: Some("main".into()),
                        subpath: Some("skills/mock-calendar".into()),
                        revision: None,
                    }),
                    ..Default::default()
                },
            )
            .unwrap();
        let snap = ws.scan().unwrap();
        let theme = Theme::default();
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            theme: &theme,
        };
        let mut record = snap.get(key).unwrap().clone();
        for status in [
            skills::reconcile::SkillStatus::Managed { no_baseline: false },
            skills::reconcile::SkillStatus::Unmanaged,
            skills::reconcile::SkillStatus::Modified,
            skills::reconcile::SkillStatus::Missing,
        ] {
            let mut marker_record = record.clone();
            marker_record.status = status;
            let marker = health_marker(&marker_record, &theme);
            assert_eq!(marker.width(), MARKER_W);
            assert!(marker.content.ends_with(' '));
        }
        for checked in [false, true] {
            let marker = checkbox_marker(checked, &theme);
            assert_eq!(marker.width(), MARKER_W);
            assert!(marker.content.ends_with("] "));
        }
        let lines = skill_card(&record, &ctx, 60, None, "name", &[]);
        assert!(lines[0].to_string().contains("mock-calendar"));
        assert!(!lines[0].to_string().contains("skills--"));
        assert!(lines[3].to_string().contains("󰊤 sampleorg/kit"));
        assert!(lines[3].to_string().contains("name"));
        record.description = Some("**Description emphasis** with `code`".into());
        let preview = super::super::preview::preview_lines(&record, &ctx, &[], 60);
        let description_start = preview
            .iter()
            .position(|line| line.to_string() == "Description")
            .unwrap();
        let body_start = preview
            .iter()
            .position(|line| line.to_string() == "SKILL.md")
            .unwrap();
        assert_eq!(preview[description_start + 1], preview[body_start + 1]);
        assert!(
            preview[description_start + 2..body_start]
                .iter()
                .flat_map(|line| &line.spans)
                .any(|span| span.content.contains("Description emphasis")
                    && span.style.add_modifier.contains(Modifier::BOLD))
        );

        assert!(preview[0].to_string().starts_with("mock-calendar"));
        assert!(
            !preview
                .iter()
                .any(|line| line.to_string().contains("≠ directory name"))
        );
        assert_eq!(record.key, key);
        assert_eq!(
            record.deployment_name(),
            "sampleorg--kit--skills--mock-calendar"
        );
        record.name = Some("中文日历".into());
        record.tags = vec!["A very long tag".into(), "中文标签".into(), "third".into()];
        for width in [0, 1, 2, 3, 8, 16, 24, 40, 80] {
            for line in skill_card(&record, &ctx, width, None, "git", &[]) {
                assert!(line.width() <= width);
            }
        }
        record.name = None;
        assert_eq!(display_name(&record), "skills--mock-calendar");
    }
}
