//! Reproducible, opt-in timings. Uses disposable roots and fake destinations only.
use super::app::{Action, App};
use skills::{
    Workspace,
    config::Config,
    ops::{edit, targets},
};
use std::time::Instant;

#[test]
#[ignore = "release-mode latency benchmark; run with --ignored --nocapture"]
fn representative_tui_latency() {
    let temp = skills::ops::DownloadDir::new("latency-bench").unwrap();
    let root = temp.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let config = Config {
        agents: skills::agents::BUILTINS
            .iter()
            .map(|definition| {
                let mut agent = definition.config(false);
                agent.skills_dir = temp
                    .path()
                    .join("agents")
                    .join(&agent.key)
                    .display()
                    .to_string();
                agent
            })
            .collect(),
        ..Default::default()
    };
    config.save(&root).unwrap();
    let ws = Workspace::open(&root).unwrap();
    let count: usize = std::env::var("SKILLS_BENCH_COUNT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(300);
    let payload = vec![b'x'; 16 * 1024];
    for i in 0..count {
        let key = format!("repos/fixture/bench-{i:04}");
        let path = root.join(&key);
        std::fs::create_dir_all(path.join("scripts")).unwrap();
        std::fs::write(
            path.join("SKILL.md"),
            format!(
                "---\nname: bench-{i:04}\ndescription: disposable benchmark skill\n---\n{}",
                "This is sample searchable content.\n".repeat(50)
            ),
        )
        .unwrap();
        for f in 0..16 {
            std::fs::write(path.join(format!("scripts/{f}.txt")), &payload).unwrap();
        }
        edit::accept(&ws, &key).unwrap();
    }
    let target = ws.config.agents[0].clone();
    let keys: Vec<_> = (0..count.min(50))
        .map(|i| format!("repos/fixture/bench-{i:04}"))
        .collect();
    let (tx, rx) = std::sync::mpsc::channel();
    let start = Instant::now();
    let mut app = App::new(ws, tx).unwrap();
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 40)).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    println!(
        "LATENCY startup_ms {:.3}",
        start.elapsed().as_secs_f64() * 1000.0
    );
    for (label, preset, on) in [
        ("preset_install", Some("benchmark"), true),
        ("preset_uninstall", Some("benchmark"), false),
        ("skill_install", None, true),
        ("skill_unlink", None, false),
    ] {
        let agent = target.clone();
        let selected = if preset.is_some() {
            keys.clone()
        } else {
            vec![keys[0].clone()]
        };
        let start = Instant::now();
        app.benchmark_apply(Action::WriteMeta(Box::new(move |ws| {
            targets::set_installed(ws, &agent, None, &selected, preset, on)
        })));
        let dispatch = start.elapsed();
        app.benchmark_drain(&rx);
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let selection = targets::selection(&app.ws, &target).unwrap();
        for key in if preset.is_some() {
            keys.clone()
        } else {
            vec![keys[0].clone()]
        } {
            assert_eq!(selection.skills().contains(&key), on);
            assert_eq!(
                target
                    .skills_path()
                    .join(skills::repository::default_deploy_name(&key))
                    .is_symlink(),
                on
            );
        }
        println!(
            "LATENCY {label}_dispatch_ms {:.3}",
            dispatch.as_secs_f64() * 1000.0
        );
        println!(
            "LATENCY {label}_total_ms {:.3}",
            start.elapsed().as_secs_f64() * 1000.0
        );
    }
    let key = keys[0].clone();
    let start = Instant::now();
    app.modal = Some(super::modal::Modal::remove(&key));
    app.handle(super::event::Msg::Key(crossterm::event::KeyEvent::from(
        crossterm::event::KeyCode::Char('y'),
    )));
    let dispatch = start.elapsed();
    app.benchmark_drain(&rx);
    terminal.draw(|frame| app.draw(frame)).unwrap();
    println!(
        "LATENCY skill_delete_dispatch_ms {:.3}",
        dispatch.as_secs_f64() * 1000.0
    );
    println!(
        "LATENCY skill_delete_total_ms {:.3}",
        start.elapsed().as_secs_f64() * 1000.0
    );
    assert!(app.snap.get(&keys[0]).is_none());
    assert!(!target.skills_path().join("bench-0000").exists());
}
