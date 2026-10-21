//! Behavioral checks for deferred content verification and asynchronous refresh.
use super::*;
use crate::tui::event::ScanOutcome;
use ratatui::{Terminal, backend::TestBackend};
use skills::meta::{Baseline, SkillMeta, Source};
use skills::reconcile::SkillStatus;
use std::sync::mpsc::{Receiver, channel};

const KEY: &str = "repos/demo/review";

fn fixture() -> (skills::ops::DownloadDir, App, Receiver<Msg>) {
    let temp = skills::ops::DownloadDir::new("deferred-verification-ui").unwrap();
    Config {
        agents: vec![],
        ..Default::default()
    }
    .save(temp.path())
    .unwrap();
    let path = temp.path().join(KEY);
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(
        path.join("SKILL.md"),
        "---\nname: review\ndescription: Review code\n---\nBody\n",
    )
    .unwrap();
    let ws = Workspace::open(temp.path()).unwrap();
    ws.meta
        .save(
            KEY,
            &SkillMeta {
                source: Some(Source::Archive {
                    url: "https://example.com/skills.tar.gz".into(),
                    subpath: Some("review".into()),
                    revision: Some("v1".into()),
                }),
                baseline: Some(Baseline {
                    hash: skills::hash::hash_directory(&path).unwrap(),
                    hash_algo: skills::hash::HASH_ALGO,
                }),
                ..Default::default()
            },
        )
        .unwrap();
    let (tx, rx) = channel();
    let app = App::new_with_launch_directory(ws, tx, Some(temp.path())).unwrap();
    (temp, app, rx)
}

fn outcome(app: &App, snapshot: Result<Snapshot>) -> ScanOutcome {
    ScanOutcome {
        snapshot,
        stamp: None,
        config: app.ws.config.clone(),
        inventory_products: app.ws.inventory_products.clone(),
        inventory_configured_agents: app.ws.inventory_configured_agents.clone(),
        warnings: vec![],
    }
}

fn footer(app: &App, width: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, 2)).unwrap();
    terminal
        .draw(|frame| app.draw_footer(frame, Rect::new(0, 0, width, 1)))
        .unwrap();
    let buffer = terminal.backend().buffer();
    (0..width)
        .map(|x| buffer[(x, 0)].symbol())
        .collect::<Vec<_>>()
        .join("")
}

#[test]
fn startup_is_browsable_and_truthfully_unchecked_at_narrow_and_wide_sizes() {
    let (_temp, app, _rx) = fixture();
    let record = app.snap.get(KEY).unwrap();
    assert_eq!(record.status, SkillStatus::Unchecked);
    assert!(record.current_hash.is_none());
    assert!(record.status.is_browsable());
    assert!(!record.status.is_healthy());
    assert!(app.verification_task.is_none());
    for width in [60, 120] {
        assert!(footer(&app, width).contains("1 unchecked"));
    }
}

#[test]
fn health_requests_one_async_verification_and_explicit_request_can_recheck() {
    let (_temp, mut app, rx) = fixture();
    app.apply(Action::SwitchTab(Tab::Health));
    let first = app
        .verification_task
        .expect("Health must request verification");
    assert_eq!(app.snap.get(KEY).unwrap().status, SkillStatus::Unchecked);
    app.verify_content(true);
    app.verify_content(false);
    assert_eq!(app.verification_task, Some(first));
    let message = wait_for_msg_matching(
        &rx,
        "content verification",
        |msg| matches!(msg, Msg::Task(id, _) if *id == first),
    );
    app.handle(message);
    assert!(app.verification_task.is_none());
    assert_eq!(app.snap.get(KEY).unwrap().status, SkillStatus::Repository);
    app.apply(Action::SwitchTab(Tab::Search));
    app.verify_content(true);
    let second = app
        .verification_task
        .expect("Explicit verification must recheck a healthy snapshot");
    assert_ne!(second, first);
    let message = wait_for_msg_matching(
        &rx,
        "content recheck",
        |msg| matches!(msg, Msg::Task(id, _) if *id == second),
    );
    app.handle(message);
    assert!(app.verification_task.is_none());
}

#[test]
fn explicit_verification_waits_for_inventory_even_outside_health() {
    let (_temp, mut app, rx) = fixture();
    app.latest_scan_task = Some(100);
    app.next_task_id = 100;
    app.verify_content(true);
    assert!(app.verification_task.is_none());
    assert!(app.verification_queued);
    let mut refreshed = outcome(&app, app.ws.inventory());
    refreshed.config.tags_enabled = !app.ws.config.tags_enabled;
    let expected_tags = refreshed.config.tags_enabled;
    refreshed.config.save(&app.ws.root).unwrap();
    let actions = app.on_task(100, TaskOutput::Scan(refreshed));
    assert_eq!(app.ws.config.tags_enabled, expected_tags);
    assert!(
        actions
            .iter()
            .any(|action| matches!(action, Action::VerifyContent { explicit: true }))
    );
    for action in actions {
        app.apply(action);
    }
    let id = app
        .verification_task
        .expect("queued palette request must start after inventory");
    let message = wait_for_msg_matching(
        &rx,
        "queued verification",
        |msg| matches!(msg, Msg::Task(task, _) if *task == id),
    );
    app.handle(message);
    assert_eq!(app.ws.config.tags_enabled, expected_tags);
}

#[test]
fn stale_verification_cannot_replace_new_inventory_in_either_completion_order() {
    for verification_first in [true, false] {
        let (_temp, mut app, _rx) = fixture();
        let old = app.ws.scan().unwrap();
        std::fs::write(
            app.ws.root.join(KEY).join("new-file"),
            b"edited after the old check",
        )
        .unwrap();
        app.verification_task = Some(100);
        app.latest_scan_task = Some(101);
        let verified = TaskOutput::Verified(outcome(&app, Ok(old)));
        let inventory = TaskOutput::Scan(outcome(&app, app.ws.inventory()));
        if verification_first {
            assert!(app.on_task(100, verified).is_empty());
            assert!(app.on_task(101, inventory).is_empty());
        } else {
            assert!(app.on_task(101, inventory).is_empty());
            assert!(app.on_task(100, verified).is_empty());
        }
        assert_eq!(app.snap.get(KEY).unwrap().status, SkillStatus::Unchecked);
        assert!(app.snap.get(KEY).unwrap().current_hash.is_none());
        assert!(app.verification_task.is_none());
        assert_eq!(
            app.ws.scan().unwrap().get(KEY).unwrap().status,
            SkillStatus::Modified
        );
    }
}

#[test]
fn old_inventory_cannot_overwrite_newer_verified_state() {
    let (_temp, mut app, _rx) = fixture();
    let inventory = outcome(&app, app.ws.inventory());
    app.latest_scan_task = Some(102);
    app.verification_task = Some(102);
    assert!(
        app.on_task(102, TaskOutput::Verified(outcome(&app, app.ws.scan())))
            .is_empty()
    );
    assert!(app.on_task(101, TaskOutput::Scan(inventory)).is_empty());
    assert_eq!(app.snap.get(KEY).unwrap().status, SkillStatus::Repository);
}

#[test]
fn failed_verification_keeps_unchecked_and_only_explicit_retry_restarts_it() {
    let (_temp, mut app, rx) = fixture();
    app.tab = Tab::Health;
    app.verification_task = Some(100);
    app.latest_scan_task = Some(100);
    app.next_task_id = 100;
    let actions = app.on_task(
        100,
        TaskOutput::Verified(outcome(&app, Err(anyhow::anyhow!("test read failure")))),
    );
    assert!(actions.iter().any(
        |action| matches!(action, Action::Error(message) if message.contains("test read failure"))
    ));
    assert_eq!(app.snap.get(KEY).unwrap().status, SkillStatus::Unchecked);
    assert!(app.verification_task.is_none());
    app.verify_content(false);
    assert!(
        app.verification_task.is_none(),
        "automatic retries must not loop"
    );
    app.verify_content(true);
    let id = app
        .verification_task
        .expect("explicit retry remains available");
    let message = wait_for_msg_matching(
        &rx,
        "verification retry",
        |msg| matches!(msg, Msg::Task(task, _) if *task == id),
    );
    app.handle(message);
}

#[test]
fn refresh_shortcut_is_inventory_normally_and_verifies_in_health() {
    let (_temp, mut app, _rx) = fixture();
    let key = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL);
    assert!(matches!(
        app.on_key(key).as_slice(),
        [Action::Rescan, Action::Toast(_)]
    ));
    app.tab = Tab::Health;
    assert!(matches!(
        app.on_key(key).as_slice(),
        [Action::VerifyContent { explicit: true }, Action::Toast(_)]
    ));
}

#[test]
fn queued_explicit_recheck_runs_once_after_invalidated_verification_and_inventory() {
    for verification_first in [true, false] {
        let (_temp, mut app, rx) = fixture();
        let old = app.ws.scan().unwrap();
        app.verification_task = Some(100);
        app.latest_scan_task = Some(101);
        app.next_task_id = 101;
        app.verify_content(true);
        let verified = TaskOutput::Verified(outcome(&app, Ok(old)));
        let inventory = TaskOutput::Scan(outcome(&app, app.ws.inventory()));
        let mut actions = if verification_first {
            let mut actions = app.on_task(100, verified);
            actions.extend(app.on_task(101, inventory));
            actions
        } else {
            let mut actions = app.on_task(101, inventory);
            actions.extend(app.on_task(100, verified));
            actions
        };
        assert_eq!(
            actions
                .iter()
                .filter(|action| matches!(action, Action::VerifyContent { explicit: true }))
                .count(),
            1
        );
        for action in actions.drain(..) {
            app.apply(action);
        }
        let id = app
            .verification_task
            .expect("one queued explicit check must run");
        assert_eq!(id, 102);
        assert!(footer(&app, 60).contains("Checking content"));
        let message = wait_for_msg_matching(
            &rx,
            "queued recheck",
            |msg| matches!(msg, Msg::Task(task, _) if *task == id),
        );
        app.handle(message);
        assert!(app.verification_task.is_none());
        assert!(!app.verification_queued);
    }
}

#[test]
fn completed_attempt_with_unreadable_records_does_not_retry_automatically() {
    let (_temp, mut app, _rx) = fixture();
    app.verification_task = Some(100);
    app.latest_scan_task = Some(100);
    let unchecked = app.ws.inventory().unwrap();
    assert!(
        app.on_task(100, TaskOutput::Verified(outcome(&app, Ok(unchecked))))
            .is_empty()
    );
    assert_eq!(app.snap.get(KEY).unwrap().status, SkillStatus::Unchecked);
    assert!(!app.needs_content_verification());
    app.verify_content(false);
    assert!(app.verification_task.is_none());
    assert!(footer(&app, 120).contains("unchecked"));
}

#[test]
fn mouse_health_tab_requests_the_same_verification_as_keyboard_navigation() {
    let (_temp, mut app, rx) = fixture();
    app.tab_rects = vec![(Rect::new(0, 0, 10, 1), Tab::Health)];
    app.handle(Msg::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 2,
        row: 0,
        modifiers: KeyModifiers::NONE,
    }));
    assert_eq!(app.tab, Tab::Health);
    let id = app.verification_task.expect("mouse entry must verify too");
    let message = wait_for_msg_matching(
        &rx,
        "mouse-triggered verification",
        |msg| matches!(msg, Msg::Task(task, _) if *task == id),
    );
    app.handle(message);
}

#[test]
fn failed_inventory_does_not_automatically_verify_the_old_snapshot() {
    let (_temp, mut app, _rx) = fixture();
    app.tab = Tab::Health;
    app.latest_scan_task = Some(100);
    app.verification_attempted = false;
    let actions = app.on_task(
        100,
        TaskOutput::Scan(outcome(&app, Err(anyhow::anyhow!("inventory unavailable")))),
    );
    assert!(
        actions
            .iter()
            .any(|action| matches!(action, Action::Error(_)))
    );
    assert!(
        !actions
            .iter()
            .any(|action| matches!(action, Action::VerifyContent { .. }))
    );
    assert!(app.verification_task.is_none());
    assert_eq!(app.snap.get(KEY).unwrap().status, SkillStatus::Unchecked);
}

#[test]
fn verification_failure_is_visible_after_the_transient_toast_and_clears_on_retry() {
    let (_temp, mut app, rx) = fixture();
    app.tab = Tab::Health;
    app.verification_task = Some(100);
    app.latest_scan_task = Some(100);
    app.next_task_id = 100;
    // Deliberately do not apply the returned error action: only persistent
    // verification state, rather than a toast, can make this failure visible.
    app.on_task(
        100,
        TaskOutput::Verified(outcome(&app, Err(anyhow::anyhow!("content unavailable")))),
    );
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    let text = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<Vec<_>>()
        .join("");
    assert!(text.to_lowercase().contains("failed"), "{text}");
    app.verify_content(true);
    let id = app
        .verification_task
        .expect("manual retry must remain available");
    terminal.draw(|frame| app.draw(frame)).unwrap();
    let text = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<Vec<_>>()
        .join("");
    assert!(!text.contains("content unavailable"));
    let message = wait_for_msg_matching(
        &rx,
        "persistent-error retry",
        |msg| matches!(msg, Msg::Task(task, _) if *task == id),
    );
    app.handle(message);
}

#[test]
fn explicit_verification_reloads_configuration_and_agent_inventory() {
    let (_temp, mut app, rx) = fixture();
    let agent_temp = skills::ops::DownloadDir::new("deferred-fake-agent").unwrap();
    let mut config = app.ws.config.clone();
    config.tags_enabled = !config.tags_enabled;
    config.agents = vec![skills::config::AgentConfig {
        key: "fake".into(),
        name: "Fake test agent".into(),
        skills_dir: agent_temp.path().to_string_lossy().into_owned(),
    }];
    config.save(&app.ws.root).unwrap();
    app.verify_content(true);
    let id = app.verification_task.unwrap();
    let message = wait_for_msg_matching(
        &rx,
        "fresh-config verification",
        |msg| matches!(msg, Msg::Task(task, _) if *task == id),
    );
    app.handle(message);
    assert_eq!(app.ws.config.tags_enabled, config.tags_enabled);
    assert!(app.snap.agents.iter().any(|agent| agent.key == "fake"));
}

#[test]
fn undo_preview_keeps_the_displayed_verified_snapshot() {
    let (_temp, mut app, _rx) = fixture();
    app.snap = app.ws.scan().unwrap();
    app.verification_attempted = true;
    app.on_snapshot();
    app.history
        .record(history::Intent::Install { skill: KEY.into() });
    app.step(Step::Undo);
    assert_eq!(app.snap.get(KEY).unwrap().status, SkillStatus::Repository);
    assert!(app.snap.get(KEY).unwrap().current_hash.is_some());
    assert!(app.verification_attempted);
    assert!(app.modal.is_some());
}
