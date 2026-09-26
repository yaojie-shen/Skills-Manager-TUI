//! Skill identity and state rendered consistently in cards, lists and compact rows.
//!
//! Pages supply semantic state; they never patch the resulting spans. Entries
//! owned by an agent use the same renderer even without a Library record.

use std::borrow::Cow;

use crate::tui::app::Ctx;
use crate::tui::components::group::{Kind, membership_badges};
use crate::tui::text::highlight_spans;
use crate::tui::theme::Theme;
use crate::tui::widgets::{fit, pad, width};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use skills::reconcile::{EntryState, SkillRecord, SkillStatus};

/// Human-facing identity; filesystem and deployment keys remain unchanged.
pub fn display_name(r: &SkillRecord) -> &str {
    r.name
        .as_deref()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| r.key.rsplit('/').next().unwrap_or(&r.key))
}

/// A readable source badge, separate from the skill's own name.
pub fn repository_badge(r: &SkillRecord, icons: skills::config::Icons) -> Option<String> {
    use skills::meta::Source;
    match &r.source {
        Some(source @ (Source::Git { .. } | Source::Archive { .. })) => {
            let name = r.source_display_name().unwrap_or("Unregistered source");
            Some(format!(
                "{} {name}",
                crate::tui::icons::source_icon(icons, source)
            ))
        }
        Some(Source::Local { .. }) => Some(crate::tui::icons::local(icons).into()),
        None => r
            .source_display_name()
            .map(|name| format!("{} {name}", crate::tui::icons::package(icons))),
    }
}

#[derive(Clone, Copy)]
enum Tone {
    Normal,
    Healthy,
    Warning,
    Error,
}

impl Tone {
    fn style(self, th: &Theme) -> Style {
        match self {
            Self::Normal => th.dim(),
            Self::Healthy => th.ok(),
            Self::Warning => th.warn(),
            Self::Error => th.err(),
        }
    }
}

fn status_marker(status: &SkillStatus) -> (&'static str, Tone) {
    match status {
        SkillStatus::Local | SkillStatus::Repository => ("●", Tone::Healthy),
        SkillStatus::MissingBaseline => ("●", Tone::Warning),
        SkillStatus::Modified => ("✎", Tone::Warning),
        SkillStatus::Missing => ("✗", Tone::Error),
        SkillStatus::Renamed { .. } => ("↪", Tone::Warning),
        SkillStatus::MissingSource => ("!", Tone::Warning),
        SkillStatus::Invalid { .. } | SkillStatus::CorruptMeta { .. } => ("!", Tone::Error),
    }
}

/// Health and preview badges use the same glyph and severity as skill cards.
pub fn status_glyph(status: &SkillStatus, th: &Theme) -> Span<'static> {
    let (glyph, tone) = status_marker(status);
    Span::styled(glyph, tone.style(th))
}

pub fn status_text(status: &SkillStatus) -> String {
    match status {
        SkillStatus::MissingBaseline => "repository · missing baseline".into(),
        SkillStatus::Renamed { to } => format!("renamed? → {to}"),
        SkillStatus::Invalid { reason } => format!("invalid: {reason}"),
        SkillStatus::CorruptMeta { error } => format!("corrupt metadata: {error}"),
        other => other.label().into(),
    }
}

/// Content shared by all densities and by both managed and agent-owned skills.
pub struct SkillPresentation<'a> {
    name: &'a str,
    description: Cow<'a, str>,
    source: String,
    tags: &'a [String],
    presets: &'a [String],
    marker: &'static str,
    tone: Tone,
    warning: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillDecoration {
    Update,
    Attention(String),
}

/// Transient selection/search state. This is input to rendering, never a patch
/// to its output. `checked: None` means ordinary single-selection mode.
pub struct SkillRenderState<'a> {
    pub checked: Option<bool>,
    pub update_available: bool,
    pub decoration: Option<&'a SkillDecoration>,
    pub excerpt: Option<&'a str>,
    pub terms: &'a [String],
    pub context: Option<&'a str>,
    pub show_tags: bool,
    /// The enclosing group is already named by its page or membership picker.
    pub hidden_group: Option<(Kind, &'a str)>,
    pub show_match_details: bool,
}

impl Default for SkillRenderState<'_> {
    fn default() -> Self {
        Self {
            checked: None,
            update_available: false,
            decoration: None,
            excerpt: None,
            terms: &[],
            context: None,
            show_tags: true,
            hidden_group: None,
            show_match_details: false,
        }
    }
}

impl<'a> SkillPresentation<'a> {
    /// A group's missing deployment keeps its Library identity and health details.
    pub fn not_deployed(mut self) -> Self {
        if self.warning.is_none() {
            self.marker = "◌";
            self.tone = Tone::Normal;
            self.warning = Some("not deployed");
        }
        self
    }

    pub fn managed(r: &'a SkillRecord, ctx: &Ctx) -> Self {
        let (marker, tone) = status_marker(&r.status);
        Self {
            name: display_name(r),
            description: Cow::Borrowed(r.description.as_deref().unwrap_or("No description")),
            source: repository_badge(r, ctx.settings.ui.icons)
                .unwrap_or_else(|| crate::tui::icons::local(ctx.settings.ui.icons).into()),
            tags: &r.tags,
            presets: &r.presets,
            marker,
            tone,
            warning: (!r.status.is_healthy()).then(|| r.status.label()),
        }
    }

    /// Agent inventory has no synthetic Library record. The relationship is
    /// mapped here once, so every layout uses the same marker and fallback text.
    pub fn entry(
        name: &'a str,
        description: Option<&'a str>,
        state: Option<&'a EntryState>,
    ) -> Self {
        let (marker, tone, label) = match state {
            Some(EntryState::Deployed) => ("✓", Tone::Healthy, "linked"),
            Some(EntryState::Broken { .. }) => ("!", Tone::Error, "broken link"),
            Some(EntryState::Shadow { same_content: true }) => ("▪", Tone::Warning, "shadow"),
            Some(EntryState::Shadow {
                same_content: false,
            }) => ("▪", Tone::Warning, "shadow, differs"),
            Some(EntryState::Foreign { .. }) => ("→", Tone::Warning, "foreign"),
            Some(EntryState::AgentOnly) => ("▪", Tone::Normal, "the agent's own"),
            None => ("—", Tone::Normal, ""),
        };
        let description = description
            .filter(|description| !description.trim().is_empty())
            .map(Cow::Borrowed)
            .unwrap_or_else(|| match state {
                Some(EntryState::Broken { .. }) => {
                    Cow::Borrowed("the link points at something the root no longer has")
                }
                Some(EntryState::Foreign { target }) => {
                    Cow::Owned(format!("links outside the root to {}", target.display()))
                }
                Some(EntryState::AgentOnly) => Cow::Borrowed("only in this agent, not in the root"),
                _ => Cow::Borrowed("No description"),
            });
        Self {
            name,
            description,
            source: label.into(),
            tags: &[],
            presets: &[],
            marker,
            tone,
            warning: matches!(tone, Tone::Warning | Tone::Error).then_some(label),
        }
    }

    fn marker(&self, state: &SkillRenderState, ctx: &Ctx) -> Span<'static> {
        let th = &ctx.settings.theme;
        let (text, style): (String, Style) = if let Some(checked) = state.checked {
            (
                if checked {
                    "[✓] ".into()
                } else {
                    "[ ] ".into()
                },
                if self.warning.is_some() {
                    self.tone.style(th)
                } else if checked {
                    th.accent()
                } else {
                    th.dim()
                },
            )
        } else if matches!(self.tone, Tone::Error) {
            (format!(" {}  ", self.marker), self.tone.style(th))
        } else if let Some(SkillDecoration::Attention(_)) = state.decoration {
            ("   ".into(), th.warn())
        } else if matches!(self.tone, Tone::Warning) {
            (format!(" {}  ", self.marker), self.tone.style(th))
        } else if state.update_available
            || matches!(state.decoration, Some(SkillDecoration::Update))
        {
            ("   ".into(), th.accent())
        } else {
            (format!(" {}  ", self.marker), self.tone.style(th))
        };
        Span::styled(pad(&text, ctx.settings.layout.marker_width), style)
    }

    fn identity(&self, columns: usize, state: &SkillRenderState, ctx: &Ctx) -> Vec<Span<'static>> {
        let th = &ctx.settings.theme;
        let marker = self.marker(state, ctx);
        let mut head = vec![Span::styled(fit(&marker.content, columns), marker.style)];
        let available = columns.saturating_sub(ctx.settings.layout.marker_width);
        let status = if matches!(self.tone, Tone::Error) {
            self.warning
                .map(|label| (format!("! {label}"), self.tone.style(th)))
        } else if let Some(SkillDecoration::Attention(label)) = state.decoration {
            Some((label.clone(), th.warn()))
        } else if matches!(self.tone, Tone::Warning) {
            self.warning
                .map(|label| (format!("! {label}"), self.tone.style(th)))
        } else if state.update_available
            || matches!(state.decoration, Some(SkillDecoration::Update))
        {
            Some(("Update".into(), th.accent()))
        } else {
            None
        };
        let status = status
            .map(|(label, style)| (format!(" {label}"), style))
            .filter(|(label, _)| available > width(label) + 8);
        let name_width =
            available.saturating_sub(status.as_ref().map(|(label, _)| width(label)).unwrap_or(0));
        head.extend(highlight_spans(
            &pad(self.name, name_width),
            state.terms,
            th.bold(),
            th,
        ));
        if let Some((label, style)) = status {
            head.push(Span::styled(label, style));
        }
        head
    }

    fn source(&self, state: &SkillRenderState) -> String {
        match state
            .context
            .filter(|tail| !tail.is_empty() && !matches!(*tail, "local" | "git" | "archive"))
        {
            Some(tail) => format!("{} · {tail}", self.source),
            None => self.source.clone(),
        }
    }

    fn memberships(&self, kind: Kind, ctx: &Ctx, state: &SkillRenderState) -> Cow<'a, [String]> {
        let names = match kind {
            Kind::Tag if !state.show_tags || !ctx.settings.tags_enabled => &[][..],
            Kind::Tag => self.tags,
            Kind::Preset => self.presets,
        };
        match state.hidden_group {
            Some((hidden_kind, hidden_name)) if hidden_kind == kind => Cow::Owned(
                names
                    .iter()
                    .filter(|name| name.as_str() != hidden_name)
                    .cloned()
                    .collect(),
            ),
            _ => Cow::Borrowed(names),
        }
    }

    /// Source stays left; visible Tag and Preset badges pack against the right edge.
    fn metadata(&self, ctx: &Ctx, columns: usize, state: &SkillRenderState) -> Vec<Span<'static>> {
        let source = self.source(state);
        let tags = self.memberships(Kind::Tag, ctx, state);
        let presets = self.memberships(Kind::Preset, ctx, state);
        if tags.is_empty() && presets.is_empty() {
            return vec![Span::styled(
                fit(&source, columns),
                ctx.settings.theme.source(),
            )];
        }
        let full_tag = membership_badges(Kind::Tag, &tags, ctx, usize::MAX, 1);
        let full_preset = membership_badges(Kind::Preset, &presets, ctx, usize::MAX, 1);
        let full_tag_width = full_tag.iter().map(Span::width).sum::<usize>();
        let full_preset_width = full_preset.iter().map(Span::width).sum::<usize>();
        let full_gap = usize::from(full_tag_width > 0 && full_preset_width > 0);
        let source_separator = usize::from(full_tag_width + full_preset_width > 0);
        let full_width =
            width(&source) + source_separator + full_tag_width + full_gap + full_preset_width;
        let (source_width, tag, preset) = if full_width <= columns {
            (width(&source), full_tag, full_preset)
        } else {
            let source_width = width(&source).min(columns / 3);
            let available = columns.saturating_sub(source_width + 1);
            let (tag_width, preset_width) = if tags.is_empty() {
                (0, available)
            } else if presets.is_empty() {
                (available, 0)
            } else {
                let group_width = available.saturating_sub(1);
                (group_width / 2, group_width - group_width / 2)
            };
            (
                source_width,
                membership_badges(Kind::Tag, &tags, ctx, tag_width, 1),
                membership_badges(Kind::Preset, &presets, ctx, preset_width, 1),
            )
        };
        let tag_used = tag.iter().map(Span::width).sum::<usize>();
        let preset_used = preset.iter().map(Span::width).sum::<usize>();
        let source = fit(&source, source_width);
        let mut out = vec![Span::styled(source.clone(), ctx.settings.theme.source())];
        let gap = usize::from(tag_used > 0 && preset_used > 0);
        let used = width(&source) + tag_used + preset_used + gap;
        let spare = columns.saturating_sub(used);
        out.push(Span::raw(" ".repeat(spare)));
        out.extend(tag);
        if gap > 0 {
            out.push(Span::raw(" "));
        }
        out.extend(preset);
        out
    }

    /// Identity, two description lines, then the shared membership metadata.
    pub fn card(&self, ctx: &Ctx, columns: usize, state: &SkillRenderState) -> Vec<Line<'static>> {
        let th = &ctx.settings.theme;
        let summary = summary_lines(state.excerpt.unwrap_or(&self.description), columns);
        vec![
            Line::from(self.identity(columns, state, ctx)),
            Line::from(highlight_spans(
                &summary[0],
                state.terms,
                th.description(),
                th,
            )),
            Line::from(highlight_spans(
                &summary[1],
                state.terms,
                th.description(),
                th,
            )),
            Line::from(self.metadata(ctx, columns, state)),
        ]
    }

    pub fn list(
        &self,
        ctx: &Ctx,
        columns: usize,
        selected: bool,
        state: &SkillRenderState,
    ) -> Vec<Line<'static>> {
        self.card(ctx, columns.saturating_sub(2), state)
            .into_iter()
            .enumerate()
            .map(|(index, line)| {
                let mut spans = vec![Span::styled(
                    fit(if selected && index == 0 { "▸ " } else { "  " }, columns),
                    ctx.settings.theme.accent(),
                )];
                spans.extend(line.spans);
                Line::from(spans)
            })
            .collect()
    }

    /// Dense identity row with the same marker and membership metadata.
    pub fn compact(
        &self,
        ctx: &Ctx,
        columns: usize,
        selected: bool,
        state: &SkillRenderState,
    ) -> Vec<Line<'static>> {
        let th = &ctx.settings.theme;
        let content_width = columns.saturating_sub(2 + ctx.settings.layout.marker_width);
        let mut metadata_min = width(&self.source).min(10);
        let mut groups = 0;
        for kind in [Kind::Tag, Kind::Preset] {
            let names = self.memberships(kind, ctx, state);
            if names.is_empty() {
                continue;
            }
            groups += 1;
            let badge = membership_badges(kind, &names, ctx, usize::MAX, 1);
            let count_width = if names.len() > 1 {
                width(&format!(" +{}", names.len() - 1))
            } else {
                0
            };
            metadata_min += badge
                .iter()
                .map(Span::width)
                .sum::<usize>()
                .min(8 + count_width)
                + 1;
        }
        let minimum_name = width(self.name).min(8).min(content_width);
        metadata_min = if groups > 0 {
            metadata_min.min(content_width.saturating_sub(minimum_name + 1))
        } else {
            metadata_min.min(content_width / 2)
        };
        let name_width = ctx
            .settings
            .layout
            .compact_name_width
            .min(width(self.name))
            .min(content_width.saturating_sub(metadata_min + usize::from(metadata_min > 0)));
        let marker = self.marker(state, ctx);
        let mut head = vec![
            Span::styled(
                fit(if selected { "▸ " } else { "  " }, columns),
                th.accent(),
            ),
            Span::styled(
                fit(&marker.content, columns.saturating_sub(2)),
                marker.style,
            ),
        ];
        head.extend(highlight_spans(
            &pad(self.name, name_width),
            state.terms,
            th.bold(),
            th,
        ));
        let metadata_width = content_width.saturating_sub(name_width);
        if metadata_width > 0 {
            head.push(Span::raw(" "));
            head.extend(self.metadata(ctx, metadata_width - 1, state));
        }
        let mut lines = vec![Line::from(head)];
        if state.show_match_details {
            let context = state
                .context
                .filter(|context| !context.is_empty())
                .map(|context| format!("{context} "))
                .unwrap_or_default();
            let prefix = fit(&format!("    {context}"), columns);
            let available = columns.saturating_sub(width(&prefix));
            let mut sub = vec![Span::styled(
                prefix,
                th.description().add_modifier(Modifier::ITALIC),
            )];
            sub.extend(highlight_spans(
                &fit(state.excerpt.unwrap_or(""), available),
                state.terms,
                th.description(),
                th,
            ));
            lines.push(Line::from(sub));
        }
        lines
    }
}

/// Render Markdown as readable text and wrap at words where possible. CJK and
/// long unbroken tokens wrap at grapheme boundaries, never splitting an emoji.
pub fn summary_lines(markdown: &str, columns: usize) -> [String; 2] {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::settings::RuntimeSettings;
    use ratatui::style::Color;

    #[test]
    fn memberships_pack_right_in_every_density_and_hide_only_the_enclosing_group() {
        let root = skills::ops::DownloadDir::new("skill-membership-alignment").unwrap();
        let ws = skills::Workspace::open(root.path()).unwrap();
        let snap = ws.scan().unwrap();
        let mut settings = RuntimeSettings::new(&ws.config);
        let tags = ["alpha".into(), "beta".into()];
        let presets = ["alpha".into(), "gamma".into()];
        for (icons, tag, preset, other_tag, other_preset) in [
            (
                skills::config::Icons::Text,
                "( alpha )",
                "/ alpha /",
                "( beta )",
                "/ gamma /",
            ),
            (
                skills::config::Icons::Nerd,
                " alpha ",
                " alpha ",
                " beta ",
                " gamma ",
            ),
        ] {
            settings.ui.icons = icons;
            let ctx = Ctx {
                ws: &ws,
                snap: &snap,
                settings: &settings,
            };
            for (tag_count, preset_count, hidden_group, suffix) in [
                (1, 0, None, tag.to_owned()),
                (0, 1, None, preset.to_owned()),
                (1, 1, None, format!("{tag} {preset}")),
                (2, 2, None, format!("{tag} +1 {preset} +1")),
                (
                    2,
                    2,
                    Some((Kind::Tag, "alpha")),
                    format!("{other_tag} {preset} +1"),
                ),
                (
                    2,
                    2,
                    Some((Kind::Preset, "alpha")),
                    format!("{tag} +1 {other_preset}"),
                ),
                (1, 1, Some((Kind::Tag, "alpha")), preset.to_owned()),
                (1, 1, Some((Kind::Preset, "alpha")), tag.to_owned()),
            ] {
                let mut skill = SkillPresentation::entry("sample", None, None);
                skill.source = "local".into();
                skill.tags = &tags[..tag_count];
                skill.presets = &presets[..preset_count];
                let state = SkillRenderState {
                    hidden_group,
                    ..Default::default()
                };
                for lines in [
                    skill.card(&ctx, 80, &state),
                    skill.list(&ctx, 80, false, &state),
                    skill.compact(&ctx, 80, false, &state),
                ] {
                    let metadata = lines.last().unwrap();
                    let text = metadata.to_string();
                    assert_eq!(metadata.width(), 80, "{text:?}");
                    assert!(
                        text.ends_with(&suffix),
                        "expected right-aligned {suffix:?}: {text:?}"
                    );
                    assert!(text.contains("local "), "{text:?}");
                }
                for columns in 0..100 {
                    for lines in [
                        skill.card(&ctx, columns, &state),
                        skill.list(&ctx, columns, false, &state),
                        skill.compact(&ctx, columns, false, &state),
                    ] {
                        assert!(
                            lines.iter().all(|line| line.width() <= columns),
                            "columns={columns}: {lines:?}"
                        );
                    }
                }
            }
            let mut skill = SkillPresentation::entry("sample", None, None);
            skill.source = "local".into();
            skill.tags = &tags[..1];
            assert_eq!(
                Line::from(skill.metadata(
                    &ctx,
                    80,
                    &SkillRenderState {
                        hidden_group: Some((Kind::Tag, "alpha")),
                        ..Default::default()
                    }
                ))
                .to_string(),
                "local"
            );

            skill.source = "󰏗 merlin-skills".into();
            let metadata = Line::from(skill.metadata(&ctx, 41, &SkillRenderState::default()));
            assert_eq!(metadata.width(), 41);
            assert!(metadata.to_string().starts_with("󰏗 merlin-skills"));
            assert!(metadata.to_string().ends_with(tag));
            assert!(!metadata.to_string().contains('…'));
        }
    }

    #[test]
    fn all_densities_share_source_tag_and_description_styles() {
        let root = skills::ops::DownloadDir::new("skill-presentation-styles").unwrap();
        let mut ws = skills::Workspace::open(root.path()).unwrap();
        std::fs::create_dir_all(root.path().join("calendar")).unwrap();
        std::fs::write(
            root.path().join("calendar/SKILL.md"),
            "---\nname: calendar\ndescription: Calendar details\n---\nCalendar",
        )
        .unwrap();
        ws.config.tags = vec![skills::config::TagConfig {
            name: "events".into(),
            color: Some("#b87e54".into()),
            skills: vec!["calendar".into()],
            description: None,
        }];
        ws.config.save(root.path()).unwrap();
        let snap = ws.scan().unwrap();
        let mut settings = RuntimeSettings::new(&ws.config);
        // Deliberately change the theme to verify that every density reads the
        // supplied settings instead of reproducing a hard-coded style.
        settings.theme.source = Color::Rgb(97, 81, 138);
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &settings,
        };
        let skill = SkillPresentation::managed(snap.get("calendar").unwrap(), &ctx);
        let state = SkillRenderState::default();
        for lines in [
            skill.card(&ctx, 80, &state),
            skill.list(&ctx, 80, true, &state),
            skill.compact(&ctx, 80, true, &state),
        ] {
            let spans: Vec<_> = lines.iter().flat_map(|line| &line.spans).collect();
            assert_eq!(
                spans
                    .iter()
                    .find(|span| span.content.contains("local"))
                    .unwrap()
                    .style,
                settings.theme.source()
            );
            let tag = spans
                .iter()
                .find(|span| span.content.contains("events"))
                .unwrap();
            assert_eq!(tag.style.bg, Some(Color::Rgb(184, 126, 84)));
            assert!(
                spans
                    .iter()
                    .find(|span| span.content.contains("calendar"))
                    .unwrap()
                    .style
                    .add_modifier
                    .contains(Modifier::BOLD)
            );
        }
        for presentation in [
            skill,
            SkillPresentation::entry(
                "calendar",
                Some("Calendar details"),
                Some(&EntryState::AgentOnly),
            ),
        ] {
            let lines = presentation.card(&ctx, 80, &state);
            assert_eq!(lines[1].spans[0].style, settings.theme.description());
            let lines = presentation.compact(
                &ctx,
                80,
                false,
                &SkillRenderState {
                    excerpt: Some("Calendar details"),
                    show_match_details: true,
                    ..Default::default()
                },
            );
            assert_eq!(
                lines[1].spans.last().unwrap().style,
                settings.theme.description()
            );
        }
        let mut settings = settings.clone();
        settings.tags_enabled = false;
        let ctx = Ctx {
            settings: &settings,
            ..ctx
        };
        let skill = SkillPresentation::managed(snap.get("calendar").unwrap(), &ctx);
        for lines in [
            skill.card(&ctx, 80, &state),
            skill.list(&ctx, 80, true, &state),
            skill.compact(&ctx, 80, true, &state),
        ] {
            assert!(!lines.iter().any(|line| line.to_string().contains("events")));
        }
    }

    #[test]
    fn repository_decorations_share_markers_and_preserve_local_priority() {
        let root = skills::ops::DownloadDir::new("skill-repository-decorations").unwrap();
        let ws = skills::Workspace::open(root.path()).unwrap();
        let snap = ws.scan().unwrap();
        let settings = RuntimeSettings::new(&ws.config);
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &settings,
        };

        let healthy = SkillPresentation::entry("healthy", None, Some(&EntryState::Deployed));
        for (decoration, marker) in [
            (SkillDecoration::Update, ""),
            (SkillDecoration::Attention("Missing upstream".into()), ""),
        ] {
            let state = SkillRenderState {
                decoration: Some(&decoration),
                ..Default::default()
            };
            for lines in [
                healthy.card(&ctx, 60, &state),
                healthy.list(&ctx, 60, false, &state),
                healthy.compact(&ctx, 60, false, &state),
            ] {
                assert!(lines[0].to_string().contains(marker));
            }
            let card = healthy.card(&ctx, 60, &state)[0].to_string();
            assert!(card.contains(match decoration {
                SkillDecoration::Update => "Update",
                SkillDecoration::Attention(_) => "Missing upstream",
            }));
        }

        let update = SkillDecoration::Update;
        let shadow = EntryState::Shadow {
            same_content: false,
        };
        let warning = SkillPresentation::entry("shadow", None, Some(&shadow));
        let state = SkillRenderState {
            decoration: Some(&update),
            ..Default::default()
        };
        assert!(warning.card(&ctx, 60, &state)[0].to_string().contains('▪'));
        assert!(!warning.card(&ctx, 60, &state)[0].to_string().contains(''));

        let broken = EntryState::Broken {
            target: root.path().join("gone"),
        };
        let error = SkillPresentation::entry("broken", None, Some(&broken));
        assert!(error.card(&ctx, 60, &state)[0].to_string().contains('!'));
        assert!(!error.card(&ctx, 60, &state)[0].to_string().contains(''));

        let checked = SkillRenderState {
            checked: Some(true),
            decoration: Some(&update),
            ..Default::default()
        };
        assert!(
            healthy.card(&ctx, 60, &checked)[0]
                .to_string()
                .contains("[✓]")
        );
    }

    #[test]
    fn selection_and_update_state_preserve_alignment_and_clipping() {
        let root = skills::ops::DownloadDir::new("skill-presentation-bounds").unwrap();
        let ws = skills::Workspace::open(root.path()).unwrap();
        let snap = ws.scan().unwrap();
        let mut settings = RuntimeSettings::new(&ws.config);
        settings.layout.marker_width = 6;
        settings.layout.compact_name_width = 31;
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &settings,
        };
        let broken = EntryState::Broken {
            target: root.path().join("gone"),
        };
        for status in [
            Some(&EntryState::AgentOnly),
            Some(&EntryState::Deployed),
            Some(&broken),
            None,
        ] {
            let skill = SkillPresentation::entry(
                "中文 👩‍💻 skill",
                Some("**Read** these calendar notes and more"),
                status,
            );
            for checked in [None, Some(false), Some(true)] {
                let state = SkillRenderState {
                    checked,
                    update_available: true,
                    excerpt: Some("long excerpt with 中文 👩‍💻"),
                    context: Some("name·description"),
                    show_match_details: true,
                    ..Default::default()
                };
                for columns in 0..100 {
                    for lines in [
                        skill.card(&ctx, columns, &state),
                        skill.list(&ctx, columns, true, &state),
                        skill.compact(&ctx, columns, true, &state),
                    ] {
                        assert!(
                            lines.iter().all(|line| line.width() <= columns),
                            "columns={columns}, lines={lines:?}"
                        );
                    }
                }
                let card = skill.card(&ctx, 80, &state)[0].to_string();
                let compact = skill.compact(&ctx, 80, true, &state)[0].to_string();
                assert_eq!(
                    width(&card[..card.find("中文").unwrap()]),
                    ctx.settings.layout.marker_width
                );
                assert_eq!(
                    width(&compact[..compact.find("中文").unwrap()]),
                    2 + ctx.settings.layout.marker_width
                );
                if checked.is_some() && matches!(status, Some(EntryState::Broken { .. })) {
                    assert!(card.contains("! broken link"));
                }
            }
        }
    }
}
