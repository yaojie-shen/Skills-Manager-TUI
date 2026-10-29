//! Exercise the visible results, not just the shared dictionary loader.
use super::{View, health::HealthView, presets::PresetsView, repos::ReposView, tags::TagsView};
use crate::tui::{app::Ctx, settings::RuntimeSettings};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend};
use skills::{Workspace, config::Config, ops::DownloadDir};

#[test]
fn every_plain_filter_refreshes_external_dictionary_edits_and_deletion() {
    let temp = DownloadDir::new("page-dictionary-refresh").unwrap();
    let mut config = Config {
        agents: vec![],
        ..Default::default()
    };
    config.search.fuzzy = false;
    config.search.dictionaries.tech = 0.0;
    config.search.dictionaries.common = 0.0;
    config.save(temp.path()).unwrap();
    let mut ws = Workspace::open(temp.path()).unwrap();
    for name in ["amber", "cobalt"] {
        ws.tags
            .save(&skills::tag::Tag {
                name: name.into(),
                skills: vec![name.into()],
                color: None,
                description: None,
            })
            .unwrap();
        ws.presets
            .save(&skills::preset::Preset {
                name: name.into(),
                skills: vec![name.into()],
                ..Default::default()
            })
            .unwrap();
        skills::repository::Repository {
            alias: name.into(),
            name: Some(name.into()),
            kind: skills::meta::SourceKind::Git,
            url: format!("https://example.test/{name}.git"),
            branch: "main".into(),
        }
        .save(&ws)
        .unwrap();
    }
    ws.config = ws.load_config_cached().unwrap();
    let snap = ws.inventory().unwrap();
    let settings = RuntimeSettings::new(&ws.config);
    let ctx = Ctx {
        ws: &ws,
        snap: &snap,
        settings: &settings,
    };
    let path = skills::paths::meta_dir(&ws.root).join(skills::dict::USER_FILE);
    let mut views: Vec<(&str, Box<dyn View>)> = vec![
        ("tags", Box::<TagsView>::default()),
        ("presets", Box::<PresetsView>::default()),
        ("repositories", Box::<ReposView>::default()),
        ("health", Box::<HealthView>::default()),
    ];
    for (step, expected) in [Some("amber"), Some("cobalt"), None, Some("amber")]
        .into_iter()
        .enumerate()
    {
        if let Some(name) = expected {
            std::fs::write(&path, format!("{name}\t映射词\n")).unwrap();
        } else {
            std::fs::remove_file(&path).unwrap();
        }
        for (name, view) in &mut views {
            view.refresh(&ctx);
            if step == 0 {
                for code in [
                    KeyCode::Char('/'),
                    KeyCode::Char('映'),
                    KeyCode::Char('射'),
                    KeyCode::Char('词'),
                ] {
                    view.handle_key(KeyEvent::new(code, KeyModifiers::NONE), &ctx);
                }
            }
            let mut terminal = Terminal::new(TestBackend::new(120, 35)).unwrap();
            terminal.draw(|f| view.draw(f, f.area(), &ctx)).unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect();
            for candidate in ["amber", "cobalt"] {
                assert_eq!(
                    text.contains(candidate),
                    expected == Some(candidate),
                    "{name} step {step}: {text}"
                );
            }
        }
    }
}
