//! The framed skill card, shared by every page that lays skills out in a grid.
//!
//! A card is three lines inside a rounded frame: what it is and where it is
//! deployed, what it does, how it is filed. The frame carries the selection,
//! so a match highlighted inside keeps its own background instead of being
//! painted over.

use super::preview::highlight_spans;
use super::status_glyph;
use crate::tui::app::Ctx;
use crate::tui::theme::Theme;
use crate::tui::widgets::{fit, pad, width};
use ratatui::Frame;
use ratatui::layout::{Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders};
use skills::reconcile::{AgentReport, DeployState, SkillRecord};

/// Narrowest a card may get before the grid gives up a column. Below this the
/// name and the deployment marks stop fitting on one line together.
pub const MIN_CARD_W: u16 = 40;
/// Most columns worth having: past this a card holds less than it costs to scan.
pub const MAX_COLS: usize = 4;
/// Four lines of content and the frame around them: identity, description, a
/// rule, and the tags. The rule is not padding — the tag pills are filled
/// shapes, and pressed straight up against the description they read as a
/// smudge under the text; a bare blank line left the card looking half empty,
/// so the gap is drawn as a thin line instead.
pub const CARD_H: u16 = 6;

/// The line between a card's text and its tags.
pub fn rule(inner_w: usize, th: &Theme) -> Line<'static> {
    Line::from(Span::styled("─".repeat(inner_w), th.dim()))
}

/// Columns that fit in `width`, always at least one.
pub fn cols_for(width: u16) -> usize {
    ((width / MIN_CARD_W) as usize).clamp(1, MAX_COLS)
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

/// How a skill stands with one agent.
pub fn deploy_glyph(state: Option<&DeployState>, th: &Theme) -> (&'static str, Style) {
    match state {
        Some(DeployState::Deployed) => ("✓", th.ok()),
        Some(DeployState::Broken) => ("!", th.err()),
        Some(DeployState::Shadow { .. }) | Some(DeployState::Foreign) => ("~", th.warn()),
        _ => ("·", th.dim()),
    }
}

/// Two-letter agent abbreviation used beside a deployment mark.
pub fn abbrev(key: &str) -> String {
    key.chars().take(2).collect()
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
        let reserve = if rest > 0 { 4 } else { 0 };
        if used + w + reserve > max_w {
            if rest + 1 > 0 {
                out.push(Span::styled(format!(" +{}", rest + 1), ctx.theme.dim()));
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
pub fn repository_badge(r: &SkillRecord) -> Option<String> {
    if let Some(skills::meta::Source::Git { url, .. }) = &r.source
        && let Some(name) = skills::repository::source_name(url)
    {
        return Some(format!("⎇ {name}"));
    }
    skills::repository::alias_of(&r.key).map(|alias| format!("⎇ {alias}"))
}

/// The lines of a skill card. `body` replaces the description when the page
/// has something better to show there (the search page puts the matching
/// excerpt); `tail` sits at the right of the last line: the source kind, or
/// whatever the page wants to say about this skill in particular; `terms`
/// are highlighted wherever they appear in the name or the body.
pub fn skill_card(
    r: &SkillRecord,
    ctx: &Ctx,
    agents: &[AgentReport],
    inner_w: usize,
    body: Option<&str>,
    tail: &str,
    terms: &[String],
) -> Vec<Line<'static>> {
    let th = ctx.theme;
    let source = match repository_badge(r) {
        Some(badge) if tail.is_empty() || tail == "git" => badge,
        Some(badge) => format!("{badge} · {tail}"),
        None => tail.to_string(),
    };
    let tail = fit(&source, inner_w.saturating_sub(4));
    let tail = tail.as_str();
    let label = display_name(r);
    let deploy: Vec<Span> = agents
        .iter()
        .flat_map(|a| {
            let (g, style) = deploy_glyph(r.deploy.get(&a.key), th);
            [
                Span::styled(format!("{g} "), style),
                Span::styled(format!("{}  ", abbrev(&a.key)), th.dim()),
            ]
        })
        .collect();
    let deploy_w: usize = deploy.iter().map(|s| width(&s.content)).sum();
    let name_w = inner_w.saturating_sub(deploy_w + 3);
    let mut head = vec![status_glyph(&r.status, th), Span::raw(" ")];
    head.extend(highlight_spans(&pad(label, name_w), terms, th.bold(), th));
    head.extend(deploy);

    let body: Vec<Span> = match body.or(r.description.as_deref()) {
        Some(d) => highlight_spans(&fit(d, inner_w.saturating_sub(2)), terms, th.dim(), th),
        None => vec![Span::styled("no description", th.dim())],
    };

    let tags_w = inner_w.saturating_sub(width(tail) + 3);
    let mut foot = vec![Span::raw("  ")];
    let pills = tag_pills(&r.tags, ctx, tags_w);
    let pills_w: usize = pills.iter().map(|s| width(&s.content)).sum();
    foot.extend(pills);
    foot.push(Span::raw(" ".repeat(tags_w.saturating_sub(pills_w) + 1)));
    foot.push(Span::styled(
        tail.to_string(),
        th.dim().add_modifier(Modifier::ITALIC),
    ));
    let mut body_line = vec![Span::raw("  ")];
    body_line.extend(body);
    vec![
        Line::from(head),
        Line::from(body_line),
        rule(inner_w, th),
        Line::from(foot),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let lines = skill_card(&record, &ctx, &[], 60, None, "name", &[]);
        assert!(lines[0].to_string().contains("mock-calendar"));
        assert!(!lines[0].to_string().contains("skills--"));
        assert!(lines[3].to_string().contains("⎇ sampleorg/kit"));
        assert!(lines[3].to_string().contains("name"));
        let preview = super::super::preview::preview_lines(&record, &ctx, &[], 60);
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
        for width in [16, 24, 40, 80] {
            for line in skill_card(&record, &ctx, &[], width, None, "git", &[]) {
                assert!(line.width() <= width);
            }
        }
        record.name = None;
        assert_eq!(display_name(&record), "skills--mock-calendar");
    }
}
