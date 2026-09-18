#[test]
fn pills_share_coverage_spacing_focus_and_width_for_every_cap_style() {
    let root = skills::ops::DownloadDir::new("pill-format").unwrap();
    let mut ws = skills::Workspace::open(root.path()).unwrap();
    let snap = ws.scan().unwrap();
    let theme = Theme::default();
    for caps in [
        skills::config::PillCaps::Round,
        skills::config::PillCaps::Block,
        skills::config::PillCaps::None,
    ] {
        ws.config.ui.pill_caps = caps;
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &{
                let mut settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
                settings.theme = theme;
                settings
            },
        };
        for (coverage, text) in [
            ((0, 0), " ◦ lark 0/0 "),
            ((0, 28), " ◌ lark 0/28 "),
            ((1, 28), " ◐ lark 1/28 "),
            ((28, 28), " ✓ lark 28/28 "),
        ] {
            let pill = TagLabel {
                coverage: Some(coverage),
                ..TagLabel::new("lark", theme.tag)
            };
            let spans = pill.render(&ctx, 100);
            assert_eq!(spans[1].content, text);
            assert_eq!(spans[0].content, caps.glyphs().0);
            assert_eq!(spans[2].content, caps.glyphs().1);
            let selected = TagLabel {
                selected: true,
                focused: true,
                ..pill
            };
            let focused = selected.render(&ctx, 100);
            assert_eq!(focused[1].style.bg, spans[1].style.bg);
            assert!(
                focused[1]
                    .style
                    .add_modifier
                    .contains(Modifier::BOLD | Modifier::UNDERLINED)
            );
            for width in 0..30 {
                assert!(
                    selected
                        .render(&ctx, width)
                        .iter()
                        .map(Span::width)
                        .sum::<usize>()
                        <= width
                );
            }
        }
    }
}
use crate::tui::{
    app::Ctx,
    components::{
        group::TagLabel,
        skill::{SkillPresentation, SkillRenderState, display_name, summary_lines},
    },
    theme::Theme,
};
use ratatui::{
    style::{Color, Modifier},
    text::Span,
};

#[test]
fn group_badges_share_focus_and_colour_with_distinct_text_and_nerd_outlines() {
    use crate::tui::components::group::{Badge, Kind};
    let root = skills::ops::DownloadDir::new("badge-outline").unwrap();
    let ws = skills::Workspace::open(root.path()).unwrap();
    let snap = ws.scan().unwrap();
    for icons in [skills::config::Icons::Nerd, skills::config::Icons::Text] {
        let mut settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
        settings.ui.icons = icons;
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &settings,
        };
        let tag = Badge::for_kind(Kind::Tag, "tools", settings.theme.tag).render(&ctx, 30);
        let preset = Badge::for_kind(Kind::Preset, "tools", settings.theme.tag).render(&ctx, 30);
        assert_eq!(tag[1], preset[1]);
        assert_ne!(tag[0].content, preset[0].content);
        assert_ne!(tag[2].content, preset[2].content);
        if icons == skills::config::Icons::Text {
            assert_eq!(
                tag.iter().map(|s| s.content.as_ref()).collect::<String>(),
                "( tools )"
            );
            assert_eq!(
                preset
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>(),
                "/ tools /"
            );
        }
        for kind in [Kind::Tag, Kind::Preset] {
            let badge = Badge {
                selected: true,
                focused: true,
                ..Badge::for_kind(kind, "中文 👩‍💻 tools", settings.theme.tag)
            };
            for width in 0..30 {
                let spans = badge.render(&ctx, width);
                assert!(spans.iter().map(Span::width).sum::<usize>() <= width);
                if let Some(body) = spans.get(1) {
                    assert!(
                        body.style
                            .add_modifier
                            .contains(Modifier::BOLD | Modifier::UNDERLINED)
                    );
                    assert_eq!(body.style.bg, tag[1].style.bg);
                }
            }
        }
    }
}

#[test]
fn fixed_membership_metadata_is_shared_sorted_and_independent_of_tag_visibility() {
    use skills::preset::{Preset, PresetStore};
    let root = skills::ops::DownloadDir::new("membership-presentation").unwrap();
    let keys = ["repos/one--tools/review", "repos/two--tools/review"];
    for key in keys {
        std::fs::create_dir_all(root.path().join(key)).unwrap();
        std::fs::write(
            root.path().join(key).join("SKILL.md"),
            "---\nname: review\ndescription: Review code\n---\nReview",
        )
        .unwrap();
    }
    let mut ws = skills::Workspace::open(root.path()).unwrap();
    ws.config.tags = ["zeta", "alpha"]
        .into_iter()
        .map(|name| skills::config::TagConfig {
            name: name.into(),
            color: Some("#b87e54".into()),
            description: None,
            skills: vec![keys[0].into()],
        })
        .collect();
    for name in ["Zed", "Pack"] {
        PresetStore::new(root.path())
            .save(&Preset {
                name: name.into(),
                color: Some("#8c7ba3".into()),
                skills: vec![keys[0].into()],
                ..Default::default()
            })
            .unwrap();
    }
    let snap = ws.scan().unwrap();
    let mut settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
    settings.ui.icons = skills::config::Icons::Text;
    let ctx = Ctx {
        ws: &ws,
        snap: &snap,
        settings: &settings,
    };
    let record = snap.get(keys[0]).unwrap();
    assert_eq!(record.presets, ["Pack", "Zed"]);
    assert!(snap.get(keys[1]).unwrap().presets.is_empty());
    let skill = SkillPresentation::managed(record, &ctx);
    let state = SkillRenderState::default();
    assert_eq!(skill.card(&ctx, 100, &state).len(), 4);
    for lines in [
        skill.card(&ctx, 100, &state),
        skill.list(&ctx, 100, true, &state),
        skill.compact(&ctx, 120, true, &state),
    ] {
        let text = lines.iter().map(ToString::to_string).collect::<String>();
        assert!(text.contains("( alpha ) +1"), "{text}");
        assert!(text.contains("/ Pack / +1"), "{text}");
        assert!(!text.contains("zeta") && !text.contains("Zed"));
        let spans = lines
            .iter()
            .flat_map(|line| &line.spans)
            .collect::<Vec<_>>();
        assert_eq!(
            spans
                .iter()
                .find(|s| s.content.contains("alpha"))
                .unwrap()
                .style
                .bg,
            Some(Color::Rgb(184, 126, 84))
        );
        assert_eq!(
            spans
                .iter()
                .find(|s| s.content.contains("Pack"))
                .unwrap()
                .style
                .bg,
            Some(Color::Rgb(140, 123, 163))
        );
    }
    for columns in 0..100 {
        for lines in [
            skill.card(&ctx, columns, &state),
            skill.list(&ctx, columns, false, &state),
            skill.compact(&ctx, columns, false, &state),
        ] {
            assert!(
                lines.iter().all(|line| line.width() <= columns),
                "width={columns}: {lines:?}"
            );
        }
    }
    let preview = super::preview::preview_lines(record, &ctx, &[], 20);
    let text = preview.iter().map(ToString::to_string).collect::<String>();
    for name in ["alpha", "zeta", "Pack", "Zed"] {
        assert!(text.contains(name), "{text}");
    }
    let mut settings = settings.clone();
    settings.tags_enabled = false;
    let ctx = Ctx {
        settings: &settings,
        ..ctx
    };
    for lines in [
        skill.card(&ctx, 100, &state),
        skill.compact(&ctx, 120, false, &state),
        super::preview::preview_lines(record, &ctx, &[], 20),
    ] {
        let text = lines.iter().map(ToString::to_string).collect::<String>();
        assert!(!text.contains("alpha") && !text.contains("zeta"));
        assert!(text.contains("Pack"), "{text}");
    }
}

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
        settings: &{
            let mut settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
            settings.theme = theme;
            settings
        },
    };
    let mut record = snap.get(key).unwrap().clone();
    for status in [
        skills::reconcile::SkillStatus::Repository,
        skills::reconcile::SkillStatus::MissingSource,
        skills::reconcile::SkillStatus::Modified,
        skills::reconcile::SkillStatus::Missing,
    ] {
        let mut marker_record = record.clone();
        marker_record.status = status;
        let marker = SkillPresentation::managed(&marker_record, &ctx).card(
            &ctx,
            40,
            &SkillRenderState::default(),
        )[0]
        .spans[0]
            .clone();
        assert_eq!(marker.width(), ctx.settings.layout.marker_width);
        assert!(marker.content.ends_with(' '));
    }
    for checked in [false, true] {
        let marker = SkillPresentation::managed(&record, &ctx).card(
            &ctx,
            40,
            &SkillRenderState {
                checked: Some(checked),
                ..Default::default()
            },
        )[0]
        .spans[0]
            .clone();
        assert_eq!(marker.width(), ctx.settings.layout.marker_width);
        assert!(marker.content.ends_with("] "));
    }
    let lines = SkillPresentation::managed(&record, &ctx).card(
        &ctx,
        60,
        &SkillRenderState {
            context: Some("name"),
            ..Default::default()
        },
    );
    assert!(lines[0].to_string().contains("mock-calendar"));
    assert!(!lines[0].to_string().contains("skills--"));
    assert!(lines[3].to_string().contains("󰊤 sampleorg/kit"));
    assert!(lines[3].to_string().contains("name"));
    record.description = Some("**Description emphasis** with `code`".into());
    let preview = super::preview::preview_lines(&record, &ctx, &[], 60);
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
    assert_eq!(record.deployment_name(), Some("mock-calendar"));
    record.name = Some("中文日历".into());
    record.tags = vec!["A very long tag".into(), "中文标签".into(), "third".into()];
    for width in [0, 1, 2, 3, 8, 16, 24, 40, 80] {
        for line in SkillPresentation::managed(&record, &ctx).card(
            &ctx,
            width,
            &SkillRenderState::default(),
        ) {
            assert!(line.width() <= width);
        }
    }
    record.name = None;
    assert_eq!(display_name(&record), "skills--mock-calendar");
}
