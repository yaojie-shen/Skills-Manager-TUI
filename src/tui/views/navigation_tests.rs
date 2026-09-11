use super::{View, health::HealthView, presets::PresetsView, tags::TagsView};
use crate::tui::app::{Action, Ctx};
use crate::tui::theme::Theme;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use skills::{Workspace, config::Config, ops::edit, preset::Preset};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
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
    let ws = Workspace::open(&root).unwrap();
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
    ws
}

#[test]
fn tag_panel_filters_locally_and_batch_writes_exclude_hidden_selections() {
    let ws = fixture("tags");
    let snap = ws.scan().unwrap();
    let theme = Theme::default();
    let ctx = Ctx {
        ws: &ws,
        snap: &snap,
        theme: &theme,
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
    modal.handle_key(key(KeyCode::Enter), &ctx);
    let actions = modal.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL), &ctx);
    let Action::BatchMeta(write, keys) = actions.into_iter().next().unwrap() else {
        panic!("batch write")
    };
    assert_eq!(keys, ["alpha"]);
    write(&ws).unwrap();
    let next = ws.scan().unwrap();
    assert!(next.get("alpha").unwrap().tags.contains(&"reviewed".into()));
    assert!(!next.get("beta").unwrap().tags.contains(&"reviewed".into()));
    view.refresh(&Ctx { snap: &next, ..ctx });
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
        theme: &theme,
    };
    let mut view = PresetsView::default();
    view.refresh(&ctx);
    view.handle_key(key(KeyCode::Char('/')), &ctx);
    view.paste("bndl", &ctx);
    view.handle_key(key(KeyCode::Enter), &ctx);
    view.handle_key(key(KeyCode::Right), &ctx);
    view.handle_key(key(KeyCode::Right), &ctx); // Select team after other.
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
        theme: &theme,
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
    assert!(text.contains("No matching issues"));
    assert!(!text.contains("everything is healthy"));
    assert_eq!(snap.skills.len(), 3);
    std::fs::remove_dir_all(&ws.root).unwrap();
}

#[test]
fn agent_preset_filter_keeps_its_scope_after_refresh() {
    let ws = fixture("agent-filter");
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
        theme: &theme,
    };
    let mut view = super::agents::AgentsView::default();
    view.refresh(&ctx);
    view.handle_key(key(KeyCode::Char('/')), &ctx);
    assert!(view.editing());
    view.paste("bndl");
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
    assert!(text.contains("Presets") && text.contains("[bndl]"));
    assert!(text.contains("bundle"));
    assert!(!text.contains("unrelated"));
    assert_eq!(ws.presets.list().unwrap().len(), 2);
    std::fs::remove_dir_all(&ws.root).unwrap();
}
