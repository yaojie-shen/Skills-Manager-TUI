//! Shared presentation rules for tags and presets.

use crate::tui::app::Ctx;
use crate::tui::theme::Theme;
use crate::tui::widgets::{CardGrid, ScrollTrack, fit, pad, render_vertical_scrollbar, width};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::{
    Frame,
    layout::{Margin, Rect},
    widgets::BorderType,
};
use skills::preset::Preset;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Tag,
    Preset,
}

impl Kind {
    pub fn marker(self, ctx: &Ctx) -> &'static str {
        match self {
            Self::Tag => "●",
            Self::Preset => crate::tui::icons::preset_caps(ctx.settings.ui.icons).0,
        }
    }
}

/// Tags and presets use the same neutral fallback for missing or invalid colours.
pub fn color(explicit: Option<&str>, theme: &Theme) -> Color {
    explicit
        .and_then(|value| value.parse().ok())
        .unwrap_or(theme.tag)
}

pub fn tag_fill(name: &str, ctx: &Ctx) -> Color {
    color(
        ctx.ws
            .config
            .tags
            .iter()
            .find(|tag| tag.name == name)
            .and_then(|tag| tag.color.as_deref()),
        &ctx.settings.theme,
    )
}

pub fn preset_fill(preset: &Preset, ctx: &Ctx) -> Color {
    color(preset.color.as_deref(), &ctx.settings.theme)
}

/// One identity row plus the optional description, enclosed by two border rows.
pub fn card_height(description: Option<&str>) -> u16 {
    3 + u16::from(description.is_some_and(|value| !value.trim().is_empty()))
}

/// Navigation sidebars share a coloured dot, a bold name and a skill count.
pub fn sidebar_card(
    name: &str,
    count: usize,
    description: Option<&str>,
    color: Color,
    columns: usize,
    ctx: &Ctx,
) -> Vec<Line<'static>> {
    card_lines(
        Kind::Tag,
        name,
        &format!("{count} skills"),
        description,
        color,
        columns,
        ctx,
    )
}

fn card_lines(
    kind: Kind,
    name: &str,
    count: &str,
    description: Option<&str>,
    color: Color,
    columns: usize,
    ctx: &Ctx,
) -> Vec<Line<'static>> {
    let th = &ctx.settings.theme;
    let count = fit(count, columns);
    let room = columns.saturating_sub(width(&count) + usize::from(columns > width(&count)));
    let mut identity = if kind == Kind::Preset {
        Badge::for_kind(kind, name, color).render(ctx, room)
    } else {
        let marker = fit(&format!("{} ", kind.marker(ctx)), room);
        let name = pad(name, room.saturating_sub(width(&marker)));
        vec![
            Span::styled(marker, Style::default().fg(color)),
            Span::styled(name, th.bold()),
        ]
    };
    let used = identity.iter().map(Span::width).sum::<usize>();
    identity.push(Span::raw(
        " ".repeat(columns.saturating_sub(used + width(&count))),
    ));
    identity.push(Span::styled(count, th.skill_count()));
    let mut lines = vec![Line::from(identity)];
    if let Some(desc) = description.filter(|s| !s.trim().is_empty()) {
        let plain = tui_markdown::from_str(desc)
            .lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(" ");
        lines.push(Line::from(Span::styled(
            fit(&plain, columns),
            Style::default().fg(th.placeholder),
        )));
    }
    lines
}

/// Fixed skill packages share their identity badge with skill metadata.
pub fn preset_card(
    preset: &Preset,
    ctx: &Ctx,
    columns: usize,
    coverage: Option<(usize, usize)>,
) -> Vec<Line<'static>> {
    let count = match coverage {
        Some((installed, total)) => format!("{installed}/{total} skills"),
        None => format!("{} skills", preset.members().len()),
    };
    card_lines(
        Kind::Preset,
        &preset.name,
        &count,
        preset.description.as_deref(),
        preset_fill(preset, ctx),
        columns,
        ctx,
    )
}

/// Borders, identity and an optional description.
pub fn preset_card_height(preset: &Preset) -> u16 {
    card_height(preset.description.as_deref())
}

/// The package identity remains visible when a terminal resize clips a card.
pub fn fit_preset_card(mut lines: Vec<Line<'static>>, rows: u16) -> Vec<Line<'static>> {
    lines.truncate(rows as usize);
    lines
}

/// Packages have square borders; their focus and padding match the card grid.
pub fn preset_frame(f: &mut Frame, area: Rect, selected: bool, focused: bool, ctx: &Ctx) -> Rect {
    let th = &ctx.settings.theme;
    let border = if selected && focused {
        th.accent().add_modifier(Modifier::BOLD)
    } else if selected {
        th.bold()
    } else {
        Style::default().fg(th.border)
    };
    let block = th
        .block("", selected && focused)
        .border_type(BorderType::Plain)
        .border_style(border);
    let inner = block.inner(area).inner(Margin {
        horizontal: 1,
        vertical: 0,
    });
    f.render_widget(block, area);
    inner
}

/// Filled labels own their palette: ANSI colours can be remapped by the
/// terminal, making a calculated foreground wrong after a theme switch.
fn pill_fill(color: Color, theme: &Theme) -> Color {
    let palette = theme.pill_palette;
    let index = match color {
        Color::Black => 0,
        Color::Red => 1,
        Color::Green => 2,
        Color::Yellow => 3,
        Color::Blue => 4,
        Color::Magenta => 5,
        Color::Cyan => 6,
        Color::Gray => 7,
        Color::DarkGray => 8,
        Color::LightRed => 9,
        Color::LightGreen => 10,
        Color::LightYellow => 11,
        Color::LightBlue => 12,
        Color::LightMagenta => 13,
        Color::LightCyan => 14,
        Color::White => 15,
        Color::Reset => 7,
        Color::Rgb(..) => return color,
        Color::Indexed(i) if i < 16 => i as usize,
        Color::Indexed(i) if i >= 232 => {
            let v = 8 + (i - 232) * 10;
            return Color::Rgb(v, v, v);
        }
        Color::Indexed(i) => {
            let levels = [0, 95, 135, 175, 215, 255];
            let i = (i - 16) as usize;
            return Color::Rgb(levels[i / 36], levels[i / 6 % 6], levels[i % 6]);
        }
    };
    let (r, g, b) = palette[index];
    Color::Rgb(r, g, b)
}

fn luminance(fill: Color) -> f64 {
    let Color::Rgb(r, g, b) = fill else {
        unreachable!()
    };
    let linear = |c: u8| {
        let c = f64::from(c) / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
}

/// Choose whichever ink has the greater contrast against the rendered RGB.
fn ink(fill: Color) -> Color {
    let light = luminance(fill);
    if (light + 0.05) / 0.05 >= 1.05 / (light + 0.05) {
        Color::Rgb(0, 0, 0)
    } else {
        Color::Rgb(255, 255, 255)
    }
}

/// Shared filled identity: rounded tags and slanted preset packages.
pub struct Badge<'a> {
    pub kind: Kind,
    pub name: &'a str,
    pub fill: Color,
    pub coverage: Option<(usize, usize)>,
    pub selected: bool,
    pub focused: bool,
}

pub type TagLabel<'a> = Badge<'a>;

impl<'a> Badge<'a> {
    pub fn new(name: &'a str, fill: Color) -> Self {
        Self::for_kind(Kind::Tag, name, fill)
    }

    pub fn for_kind(kind: Kind, name: &'a str, fill: Color) -> Self {
        Self {
            kind,
            name,
            fill,
            coverage: None,
            selected: false,
            focused: false,
        }
    }

    pub fn render(&self, ctx: &Ctx, max_width: usize) -> Vec<Span<'static>> {
        let (mark, count) = match self.coverage {
            Some((installed, total)) => {
                let mark = if total == 0 {
                    "◦ "
                } else if installed == total {
                    "✓ "
                } else if installed == 0 {
                    "◌ "
                } else {
                    "◐ "
                };
                (mark, format!(" {installed}/{total}"))
            }
            None => ("", String::new()),
        };
        let (left, right) = match self.kind {
            Kind::Tag => {
                crate::tui::icons::tag_caps(ctx.settings.ui.icons, ctx.settings.ui.pill_caps)
            }
            Kind::Preset => crate::tui::icons::preset_caps(ctx.settings.ui.icons),
        };
        let fixed = width(left) + width(right) + width(mark) + width(&count) + 2;
        if max_width < fixed {
            return vec![];
        }
        let body = format!(" {mark}{}{count} ", fit(self.name, max_width - fixed));
        let fill = pill_fill(self.fill, &ctx.settings.theme);
        let mut style = Style::default().bg(fill).fg(ink(fill));
        if self.selected {
            style = style.add_modifier(if self.focused {
                Modifier::BOLD | Modifier::UNDERLINED
            } else {
                Modifier::BOLD
            });
        }
        vec![
            Span::styled(left, Style::default().fg(fill)),
            Span::styled(body, style),
            Span::styled(right, Style::default().fg(fill)),
        ]
    }
}

/// One packing policy for membership badges in cards and full detail rows.
pub fn membership_badges(
    kind: Kind,
    names: &[String],
    ctx: &Ctx,
    max_w: usize,
    max_names: usize,
) -> Vec<Span<'static>> {
    if kind == Kind::Tag && !ctx.settings.tags_enabled {
        return vec![];
    }
    let names = names.iter().collect::<std::collections::BTreeSet<_>>();
    let badges = names
        .into_iter()
        .map(|name| {
            let fill = match kind {
                Kind::Tag => tag_fill(name, ctx),
                Kind::Preset => ctx
                    .snap
                    .presets
                    .get(name)
                    .map_or(ctx.settings.theme.tag, |preset| preset_fill(preset, ctx)),
            };
            Badge::for_kind(kind, name, fill)
        })
        .collect::<Vec<_>>();
    pack_badges(&badges, ctx, max_w, max_names)
}

/// Coverage is derived from the caller's current member set, never Tag ownership.
pub fn tag_coverage_pills(
    coverages: &[skills::preset::TagCoverage],
    ctx: &Ctx,
    max_w: usize,
) -> Vec<Span<'static>> {
    if !ctx.settings.tags_enabled {
        return vec![];
    }
    let mut coverages = coverages.iter().collect::<Vec<_>>();
    coverages.sort_by(|a, b| a.name.cmp(&b.name));
    let badges = coverages
        .into_iter()
        .map(|coverage| Badge {
            coverage: Some((coverage.included, coverage.total)),
            ..Badge::new(&coverage.name, tag_fill(&coverage.name, ctx))
        })
        .collect::<Vec<_>>();
    pack_badges(&badges, ctx, max_w, usize::MAX)
}

fn pack_badges(
    badges: &[Badge<'_>],
    ctx: &Ctx,
    max_w: usize,
    max_names: usize,
) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    let mut used = 0;
    for (i, badge) in badges.iter().enumerate() {
        let rest = badges.len() - i - 1;
        let reserve = if rest > 0 {
            width(&format!(" +{rest}"))
        } else {
            0
        };
        let pill = badge.render(
            ctx,
            if max_names == 1 {
                max_w.saturating_sub(reserve)
            } else {
                usize::MAX
            },
        );
        let w = pill.iter().map(Span::width).sum::<usize>() + usize::from(i > 0);
        if i >= max_names || pill.is_empty() || used + w + reserve > max_w {
            let count = fit(
                &format!("{}+{}", if used > 0 { " " } else { "" }, rest + 1),
                max_w.saturating_sub(used),
            );
            if !count.is_empty() {
                out.push(Span::styled(count, ctx.settings.theme.dim()));
            }
            break;
        }
        if i > 0 {
            out.push(Span::raw(" "));
        }
        out.extend(pill);
        used += w;
    }
    out
}

/// Every group card list uses the same scrollbar geometry and styling.
pub fn draw_track(
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
        render_vertical_scrollbar(
            f,
            rect,
            grid.grid_rows(),
            vis,
            selected.unwrap_or(0) / grid.cols(),
            th.dim(),
        );
    } else {
        track.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filled_labels_keep_readable_contrast() {
        let dir = skills::ops::DownloadDir::new("pill-contrast").unwrap();
        let ws = skills::Workspace::open(dir.path()).unwrap();
        let snap = ws.scan().unwrap();
        let settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &settings,
        };
        for fill in (0..=255).map(Color::Indexed).chain([
            Color::Black,
            Color::Red,
            Color::Green,
            Color::Yellow,
            Color::Blue,
            Color::Magenta,
            Color::Cyan,
            Color::Gray,
            Color::DarkGray,
            Color::LightRed,
            Color::LightGreen,
            Color::LightYellow,
            Color::LightBlue,
            Color::LightMagenta,
            Color::LightCyan,
            Color::White,
            Color::Reset,
            Color::Rgb(128, 128, 128),
            Color::Rgb(0, 180, 0),
            Color::Rgb(255, 0, 128),
        ]) {
            for kind in [Kind::Tag, Kind::Preset] {
                let spans = Badge::for_kind(kind, "label", fill).render(&ctx, usize::MAX);
                let bg = spans[1].style.bg.unwrap();
                let fg = spans[1].style.fg.unwrap();
                assert!(matches!(bg, Color::Rgb(..)));
                assert_eq!(spans[0].style.fg, Some(bg));
                assert_eq!(spans[2].style.fg, Some(bg));
                let (a, b) = (luminance(fg), luminance(bg));
                assert!(
                    (a.max(b) + 0.05) / (a.min(b) + 0.05) >= 4.5,
                    "{kind:?} {fill:?}"
                );
            }
        }
    }

    #[test]
    fn group_colours_share_the_theme_default() {
        let theme = Theme::default();
        assert_eq!(color(None, &theme), theme.tag);
        assert_eq!(color(Some("not a colour"), &theme), theme.tag);
        assert_eq!(color(Some("#b87e54"), &theme), Color::Rgb(184, 126, 84));
        assert_eq!(color(Some("blue"), &theme), Color::Blue);
    }

    #[test]
    fn group_card_height_tracks_its_visible_description() {
        let dir = skills::ops::DownloadDir::new("group-card").unwrap();
        let mut ws = skills::Workspace::open(dir.path()).unwrap();
        ws.config.tags.push(skills::config::TagConfig {
            name: "dev".into(),
            skills: vec!["shared".into(), "tool".into()],
            color: None,
            description: None,
        });
        let snap = ws.scan().unwrap();
        let settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &settings,
        };
        for description in [None, Some(""), Some(" \n "), Some("**Useful** tools")] {
            let lines = sidebar_card("tools", 12, description, settings.theme.tag, 40, &ctx);
            assert_eq!(usize::from(card_height(description)), lines.len() + 2);
            assert!(lines[0].to_string().starts_with(Kind::Tag.marker(&ctx)));
            assert!(lines[0].to_string().ends_with("12 skills"));
            for skills in [vec![], vec!["shared".into(), "shared".into()]] {
                let preset = Preset {
                    name: "tools".into(),
                    skills,
                    description: description.map(str::to_owned),
                    ..Preset::default()
                };
                let total = usize::from(!preset.skills.is_empty());
                let lines = preset_card(&preset, &ctx, 40, None);
                assert_eq!(usize::from(preset_card_height(&preset)), lines.len() + 2);
                assert!(lines[0].to_string().ends_with(&format!("{total} skills")));
                assert!(lines[0].to_string().starts_with(Kind::Preset.marker(&ctx)));
                assert_eq!(
                    lines[0].spans.last().unwrap().style,
                    settings.theme.skill_count()
                );
                assert!(
                    !lines.iter().any(|line| line.to_string().contains("dev")),
                    "overlapping Tags do not add composition rows to fixed packages"
                );
                for rows in 0..=3 {
                    let clipped = fit_preset_card(lines.clone(), rows);
                    assert!(clipped.len() <= rows as usize);
                    if rows > 0 {
                        assert_eq!(clipped[0], lines[0]);
                    }
                    if rows as usize >= lines.len() {
                        assert_eq!(clipped, lines);
                    }
                }
                for columns in [0, 1, 6, 15, 40] {
                    let lines = preset_card(&preset, &ctx, columns, Some((1, total)));
                    assert!(lines.iter().all(|line| line.width() <= columns));
                }
            }
        }
    }
}
