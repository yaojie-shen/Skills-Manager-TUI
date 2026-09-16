use super::{View, health::HealthView, presets::PresetsView, tags::TagsView};
use crate::tui::app::{Action, Ctx};
use crate::tui::theme::Theme;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use skills::{Workspace, config::Config, ops::edit, preset::Preset};

#[test]
fn library_tag_and_preset_pages_share_skill_styles_and_group_colours() {
    use crate::tui::settings::RuntimeSettings;
    use ratatui::{
        Terminal,
        backend::TestBackend,
        buffer::Buffer,
        style::{Color, Modifier},
    };
    use skills::config::{Icons, TagConfig, UiLayout};

    // Inspect real page output with distinctive settings, so a page that bypasses
    // the shared renderer cannot accidentally pass by using today's defaults.
    let root = skills::ops::DownloadDir::new("cross-page-skill-styles").unwrap();
    let tint = Color::Rgb(36, 87, 105);
    let mut config = Config {
        agents: vec![],
        tags: vec![TagConfig {
            name: "team".into(),
            skills: vec!["sample".into()],
            color: Some("#245769".into()),
            description: None,
        }],
        ..Default::default()
    };
    config.ui.icons = Icons::Text;
    config.save(root.path()).unwrap();
    std::fs::create_dir_all(root.path().join("sample")).unwrap();
    std::fs::write(
        root.path().join("sample/SKILL.md"),
        "---\nname: sample\ndescription: Shared skill description\n---\nBody",
    )
    .unwrap();
    let mut ws = Workspace::open(root.path()).unwrap();
    ws.presets
        .save(&Preset {
            name: "bundle".into(),
            skills: vec!["sample".into()],
            color: Some("#245769".into()),
            ..Default::default()
        })
        .unwrap();
    let snap = ws.scan().unwrap();

    let render = |view: &mut dyn View, ctx: &Ctx| -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(180, 36)).unwrap();
        terminal
            .draw(|frame| view.draw(frame, frame.area(), ctx))
            .unwrap();
        terminal.backend().buffer().clone()
    };
    let has_styled_text =
        |buffer: &Buffer, text: &str, fg: Option<Color>, bg: Option<Color>, dim: bool| {
            assert!(text.is_ascii());
            buffer
                .content
                .chunks(buffer.area.width as usize)
                .any(|row| {
                    row.windows(text.len()).any(|cells| {
                        cells.iter().zip(text.chars()).all(|(cell, symbol)| {
                            cell.symbol() == symbol.to_string()
                                && fg.is_none_or(|expected| cell.fg == expected)
                                && bg.is_none_or(|expected| cell.bg == expected)
                                && (!dim || cell.modifier.contains(Modifier::DIM))
                        })
                    })
                })
        };

    for layout in [UiLayout::Grid, UiLayout::List, UiLayout::Compact] {
        ws.config.ui.layout = layout;
        let mut settings = RuntimeSettings::new(&ws.config);
        settings.theme.source = Color::Rgb(17, 29, 43);
        settings.theme.dim = Color::Rgb(71, 83, 97);
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &settings,
        };
        let mut library = super::search::SearchView::default();
        library.refresh(&ctx);
        let mut tags = TagsView::default();
        tags.refresh(&ctx);
        tags.select("team", &snap);
        let mut presets = PresetsView::default();
        presets.refresh(&ctx);
        presets.select("bundle");

        let mut repos = super::repos::ReposView::default();
        repos.refresh(&ctx);
        for (name, view) in [
            ("Library", &mut library as &mut dyn View),
            ("Tags", &mut tags as &mut dyn View),
            ("Presets", &mut presets as &mut dyn View),
            ("Repos", &mut repos as &mut dyn View),
        ] {
            let buffer = render(view, &ctx);
            assert!(
                has_styled_text(&buffer, "local", Some(settings.theme.source), None, false),
                "{name} {layout:?} must use the shared source style"
            );
            let metadata = buffer
                .content
                .chunks(buffer.area.width as usize)
                .filter(|row| row.iter().any(|cell| cell.fg == settings.theme.source))
                .map(|row| {
                    row.iter()
                        .take(if layout == UiLayout::Grid {
                            row.len()
                        } else {
                            if name == "Library" { 38 } else { 76 }
                        })
                        .map(|cell| cell.symbol())
                        .collect::<String>()
                })
                .find(|row| row.contains("local"))
                .unwrap();
            // The sidebar/header already identifies the enclosing group. Its
            // badge disappears from skill metadata, while the other kind stays.
            match name {
                "Tags" => {
                    assert!(!metadata.contains("team"), "{layout:?}: {metadata}");
                    assert!(metadata.contains("bundle"), "{layout:?}: {metadata}");
                }
                "Presets" => {
                    assert!(!metadata.contains("bundle"), "{layout:?}: {metadata}");
                    assert!(metadata.contains("team"), "{layout:?}: {metadata}");
                }
                _ => {}
            }
            if name == "Library" && layout == UiLayout::Compact {
                for cap in ["(", "/"] {
                    assert!(
                        has_styled_text(&buffer, cap, Some(tint), None, false),
                        "compact metadata must retain distinct Tag/Preset outlines"
                    );
                }
            }
            if layout != UiLayout::Compact {
                assert!(
                    has_styled_text(
                        &buffer,
                        "Shared skill description",
                        Some(settings.theme.dim),
                        None,
                        true
                    ),
                    "{name} {layout:?} must use the shared description style"
                );
            }
            if name == "Tags" {
                assert!(
                    buffer
                        .content
                        .iter()
                        .any(|cell| cell.symbol() == "●" && cell.fg == tint)
                );
            } else {
                assert!(
                    has_styled_text(&buffer, "team", None, Some(tint), false),
                    "{name} {layout:?} must use the configured pill fill"
                );
            }
        }
    }
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

#[test]
fn tag_and_preset_first_item_move_up_to_filter_and_down_to_results() {
    let ws = fixture("group-filter-navigation");
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
    let mut tags = TagsView::default();
    tags.refresh(&ctx);
    tags.handle_key(key(KeyCode::Up), &ctx);
    assert!(tags.input_focused());
    tags.handle_key(key(KeyCode::Down), &ctx);
    assert!(!tags.input_focused());
    let mut presets = PresetsView::default();
    presets.refresh(&ctx);
    presets.handle_key(key(KeyCode::Up), &ctx);
    assert!(presets.input_focused());
    presets.handle_key(key(KeyCode::Down), &ctx);
    assert!(!presets.input_focused());
}
fn fixture(label: &str) -> Workspace {
    let root = std::env::temp_dir().join(format!("skills-panel-{label}-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    Config {
        agents: vec![],
        ..Default::default()
    }
    .save(&root)
    .unwrap();
    for name in ["alpha", "beta"] {
        std::fs::create_dir_all(root.join(name)).unwrap();
        std::fs::write(
            root.join(name).join("SKILL.md"),
            format!("---\nname: {name}\ndescription: tools\n---\nBody"),
        )
        .unwrap();
    }
    let mut ws = Workspace::open(&root).unwrap();
    for name in ["alpha", "beta"] {
        edit::tag_add(&ws, name, &["team".into()]).unwrap();
    }
    edit::tag_add(&ws, "beta", &["other".into()]).unwrap();
    ws.presets
        .save(&Preset {
            name: "bundle".into(),
            skills: vec!["alpha".into(), "beta".into()],
            ..Default::default()
        })
        .unwrap();
    ws.config = ws.load_config().unwrap();
    ws
}

#[test]
fn create_empty_tag_then_add_members_with_shared_picker() {
    let mut ws = fixture("tag-create");
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
    let mut view = TagsView::default();
    view.refresh(&ctx);
    let Action::OpenModal(mut modal) = view.handle_key(key(KeyCode::Char('c')), &ctx).remove(0)
    else {
        panic!("create tag dialog")
    };
    modal.paste("empty", &ctx);
    let Action::SubmitInput(actions) = modal.handle_key(key(KeyCode::Enter), &ctx).remove(0) else {
        panic!("submit tag name")
    };
    for action in actions {
        if let Action::WriteMeta(write) = action {
            write(&ws).unwrap();
        }
    }
    ws.config = Config::load(&ws.root).unwrap();
    let snap = ws.scan().unwrap();
    assert!(
        ws.config
            .tags
            .iter()
            .any(|t| t.name == "empty" && t.skills.is_empty())
    );
    let ctx = Ctx {
        ws: &ws,
        snap: &snap,
        settings: &{
            let mut settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
            settings.theme = theme;
            settings
        },
    };
    view.refresh(&ctx);
    view.select("empty", &snap);
    let Action::OpenModal(mut modal) = view.handle_key(key(KeyCode::Char('a')), &ctx).remove(0)
    else {
        panic!("member picker")
    };
    modal.paste("alpha", &ctx);
    modal.handle_key(key(KeyCode::Down), &ctx);
    modal.handle_key(key(KeyCode::Char(' ')), &ctx);
    let Action::BatchMeta(write, _) = modal
        .handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL), &ctx)
        .remove(0)
    else {
        panic!("apply member selection")
    };
    write(&ws).unwrap();
    let config = Config::load(&ws.root).unwrap();
    assert_eq!(
        config
            .tags
            .iter()
            .find(|t| t.name == "empty")
            .unwrap()
            .skills,
        vec!["alpha"]
    );
    assert!(config.skill_tags("alpha").contains(&"team".into()));
    assert!(config.skill_tags("beta").contains(&"other".into()));
    std::fs::remove_dir_all(ws.root).unwrap();
}

#[test]
fn tag_panel_filters_locally_and_batch_writes_include_hidden_selections() {
    let mut ws = fixture("tags");
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
    let mut view = TagsView::default();
    view.refresh(&ctx);
    view.handle_key(key(KeyCode::Char('/')), &ctx);
    view.paste("tem", &ctx); // Fuzzy tag filter.
    view.handle_key(key(KeyCode::Enter), &ctx);
    view.handle_key(key(KeyCode::Right), &ctx);
    assert!(view.handle_key(key(KeyCode::Char('m')), &ctx).is_empty());
    view.handle_key(
        KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL),
        &ctx,
    );
    view.handle_key(key(KeyCode::Char('/')), &ctx);
    view.paste("alpha", &ctx);
    view.handle_key(key(KeyCode::Enter), &ctx);
    let actions = view.handle_key(key(KeyCode::Char('t')), &ctx);
    let Action::OpenModal(mut modal) = actions.into_iter().next().unwrap() else {
        panic!("batch dialog")
    };
    modal.paste("reviewed", &ctx);
    let actions = modal.handle_key(key(KeyCode::Enter), &ctx);
    let Action::WriteMeta(write) = actions.into_iter().next().unwrap() else {
        panic!("batch write")
    };
    write(&ws).unwrap();
    ws.config = ws.load_config().unwrap();
    let next = ws.scan().unwrap();
    assert!(next.get("alpha").unwrap().tags.contains(&"reviewed".into()));
    assert!(next.get("beta").unwrap().tags.contains(&"reviewed".into()));
    let ctx = Ctx {
        ws: &ws,
        snap: &next,
        settings: &{
            let mut settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
            settings.theme = theme;
            settings
        },
    };
    view.refresh(&ctx);
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 36)).unwrap();
    terminal.draw(|f| view.draw(f, f.area(), &ctx)).unwrap();
    let text: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|c| c.symbol())
        .collect();
    assert!(text.contains("Tag: team"));
    assert!(text.contains("alpha"));
    std::fs::remove_dir_all(&ws.root).unwrap();
}

#[test]
fn filtered_preset_removal_preserves_other_members_and_central_skills() {
    let ws = fixture("presets");
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
    let mut view = PresetsView::default();
    view.refresh(&ctx);
    view.handle_key(key(KeyCode::Char('/')), &ctx);
    view.paste("bndl", &ctx);
    view.handle_key(key(KeyCode::Enter), &ctx);
    view.handle_key(key(KeyCode::Right), &ctx);
    view.handle_key(key(KeyCode::Char('/')), &ctx);
    view.paste("alpha", &ctx);
    view.handle_key(key(KeyCode::Enter), &ctx);
    view.handle_key(key(KeyCode::Char('m')), &ctx);
    view.handle_key(
        KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL),
        &ctx,
    );
    let actions = view.handle_key(key(KeyCode::Char('x')), &ctx);
    let Action::WriteMeta(write) = actions.into_iter().next().unwrap() else {
        panic!("preset edit")
    };
    write(&ws).unwrap();
    assert_eq!(ws.presets.load("bundle").unwrap().unwrap().skills, ["beta"]);
    for name in ["alpha", "beta"] {
        assert!(ws.root.join(name).join("SKILL.md").exists());
    }
    std::fs::remove_dir_all(&ws.root).unwrap();
}

#[test]
fn health_filter_with_no_matches_never_claims_the_library_is_healthy() {
    let ws = fixture("health");
    std::fs::create_dir_all(ws.root.join("invalid")).unwrap();
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
    let mut view = HealthView::default();
    view.refresh(&ctx);
    view.handle_key(key(KeyCode::Char('/')), &ctx);
    view.paste("nonexistent", &ctx);
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
    terminal.draw(|f| view.draw(f, f.area(), &ctx)).unwrap();
    let text: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|c| c.symbol())
        .collect();
    assert!(text.contains("No matching entries"));
    assert!(!text.contains("everything is healthy"));
    assert_eq!(snap.skills.len(), 3);
    std::fs::remove_dir_all(&ws.root).unwrap();
}

#[test]
fn agent_skill_filter_survives_refresh_without_filtering_coverage() {
    let mut ws = fixture("agent-filter");
    ws.config.agents = vec![skills::config::AgentConfig {
        key: "fixture".into(),
        name: "Fixture".into(),
        skills_dir: ws.root.join("deployed").to_string_lossy().into_owned(),
    }];
    ws.presets
        .save(&Preset {
            name: "unrelated".into(),
            ..Default::default()
        })
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
    let mut view = super::agents::AgentsView::default();
    view.discover(&ws.root).unwrap();
    view.refresh(&ctx);
    view.handle_key(key(KeyCode::Enter), &ctx);
    view.handle_key(key(KeyCode::Enter), &ctx);
    view.handle_key(key(KeyCode::Char('/')), &ctx);
    assert!(view.editing());
    view.paste("bndl", &ctx);
    view.handle_key(key(KeyCode::Enter), &ctx);
    assert!(!view.editing());
    view.refresh(&ctx);
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 36)).unwrap();
    terminal.draw(|f| view.draw(f, f.area(), &ctx)).unwrap();
    let text: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|c| c.symbol())
        .collect();
    assert!(text.contains("skills") && text.contains("bndl"));
    assert!(
        text.contains("bundle"),
        "uninstalled preset is offered for deployment"
    );
    assert!(
        text.contains("unrelated"),
        "empty preset is offered with zero count"
    );
    assert_eq!(ws.presets.list().unwrap().len(), 2);
    std::fs::remove_dir_all(&ws.root).unwrap();
}

#[test]
fn member_search_stays_visible_and_presets_place_it_above_tags() {
    use ratatui::{Terminal, backend::TestBackend};
    let ws = fixture("persistent-search");
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
    let render = |view: &mut dyn View, height| {
        let mut terminal = Terminal::new(TestBackend::new(120, height)).unwrap();
        terminal.draw(|f| view.draw(f, f.area(), &ctx)).unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .chunks(120)
            .map(|row| row.iter().map(|c| c.symbol()).collect::<String>())
            .collect::<Vec<_>>()
    };
    let mut tags = TagsView::default();
    tags.refresh(&ctx);
    assert!(render(&mut tags, 30)[1].contains("search skills…"));
    tags.handle_key(key(KeyCode::Right), &ctx);
    tags.handle_key(key(KeyCode::Char('/')), &ctx);
    tags.paste("beta", &ctx);
    tags.handle_key(key(KeyCode::Enter), &ctx);
    tags.handle_key(key(KeyCode::Left), &ctx);
    assert!(render(&mut tags, 30)[1].contains("beta"));
    tags.refresh(&ctx);
    assert!(render(&mut tags, 30)[1].contains("beta"));

    let mut presets = PresetsView::default();
    presets.refresh(&ctx);
    let rows = render(&mut presets, 30);
    assert!(rows[1].contains("search skills…"));
    let coverage = rows
        .iter()
        .position(|row| row.contains("team") && row.contains("2/2"))
        .unwrap();
    assert!(coverage > 1 && coverage < 8);
    assert!(!rows[coverage].contains("[ ]") && !rows[coverage].contains("[✓]"));
    presets.handle_key(key(KeyCode::Right), &ctx); // Skills, skipping read-only tags.
    presets.handle_key(key(KeyCode::Up), &ctx); // Search above the composition row.
    assert!(presets.input_focused());
    presets.paste("alpha", &ctx);
    presets.handle_key(key(KeyCode::Down), &ctx); // Directly back to skills.
    assert!(!presets.input_focused());
    presets.handle_key(key(KeyCode::Up), &ctx); // Directly back to search.
    assert!(presets.input_focused());
    assert!(render(&mut presets, 30)[1].contains("alpha"));
    presets.handle_key(key(KeyCode::Enter), &ctx);
    presets.handle_key(key(KeyCode::Left), &ctx);
    presets.refresh(&ctx);
    assert!(render(&mut presets, 30)[1].contains("alpha"));
    for height in 1..10 {
        render(&mut tags, height);
        render(&mut presets, height);
    }
    std::fs::remove_dir_all(&ws.root).unwrap();
}
