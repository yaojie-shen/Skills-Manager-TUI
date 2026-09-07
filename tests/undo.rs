//! Taking metadata writes back: tags, notes and preset membership, against a
//! tree that may have moved on in between.

use skills::Workspace;
use skills::config::{AgentConfig, Config, DeployConfig};
use skills::history::{self, History, Plan};
use skills::ops::edit;
use skills::preset::Preset;
use std::path::PathBuf;

struct Fixture {
    base: PathBuf,
    root: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let base = std::env::temp_dir().join(format!("skills-undo-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("skills");
        std::fs::create_dir_all(&root).unwrap();
        let cfg = Config {
            schema: 1,
            agents: vec![AgentConfig {
                key: "a".into(),
                name: "Agent A".into(),
                skills_dir: base.join("agent-a").display().to_string(),
            }],
            deploy: DeployConfig {
                all_to_all: false,
                presets: vec![],
            },
            tags: vec![],
            search: Default::default(),
            ui: Default::default(),
        };
        cfg.save(&root).unwrap();
        let fx = Self { base, root };
        for key in ["printer", "bicycle"] {
            fx.add_skill(key);
        }
        fx
    }

    fn add_skill(&self, key: &str) {
        let dir = self.root.join(key);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {key}\ndescription: a {key}\n---\n# {key}\n"),
        )
        .unwrap();
    }

    fn ws(&self) -> Workspace {
        Workspace::open(&self.root).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

/// One history step, as the TUI takes it: plan against the tree as it stands
/// now, apply, then move the entry between the stacks.
fn step(ws: &Workspace, log: &mut History, undo: bool) -> String {
    let entry = if undo { log.last() } else { log.next_redo() };
    let intent = entry.expect("a step to take").intent.clone();
    let snap = ws.scan().unwrap();
    let plan = if undo {
        history::undo_plan(ws, &snap, &intent)
    } else {
        history::redo_plan(ws, &snap, &intent)
    }
    .unwrap();
    let message = match plan {
        Plan::Write { apply, .. } => apply.apply(ws).unwrap(),
        Plan::Nothing(why) => why,
        Plan::Links(_) => panic!("a metadata step planned links"),
    };
    if undo {
        log.commit_undo();
    } else {
        log.commit_redo();
    }
    message
}

fn tags(ws: &Workspace, key: &str) -> Vec<String> {
    ws.meta
        .load(key)
        .unwrap()
        .map(|m| m.tags)
        .unwrap_or_default()
}

fn note(ws: &Workspace, key: &str) -> Option<String> {
    ws.meta.load(key).unwrap().and_then(|m| m.note)
}

fn tag_set(ws: &Workspace, log: &mut History, key: &str, tags: &[&str]) {
    let owned: Vec<String> = tags.iter().map(|t| t.to_string()).collect();
    let (_, intent) = history::tag_edit(ws, |ws| {
        edit::tag_set(ws, key, &owned).map(|_| String::new())
    })
    .unwrap();
    if let Some(intent) = intent {
        log.record(intent);
    }
}

fn note_set(ws: &Workspace, log: &mut History, key: &str, text: Option<&str>) {
    let (_, intent) = history::note_edit(ws, key, text).unwrap();
    if let Some(intent) = intent {
        log.record(intent);
    }
}

#[test]
fn adding_a_tag_that_is_already_there_is_not_a_step_and_undo_leaves_it() {
    let fx = Fixture::new("tag-present");
    let ws = fx.ws();
    let mut log = History::default();

    tag_set(&ws, &mut log, "printer", &["office"]);
    let (_, again) = history::tag_edit(&ws, |ws| {
        edit::tag_add(ws, "printer", &["office".to_string()]).map(|_| String::new())
    })
    .unwrap();
    assert!(
        again.is_none(),
        "a write that left the file as it was is not a step"
    );

    // Adding a second tag records only that tag, so undoing it takes back the
    // one thing it did and leaves the tag that was already there.
    tag_set(&ws, &mut log, "printer", &["office", "paper"]);
    step(&ws, &mut log, true);
    assert_eq!(tags(&ws, "printer"), ["office"]);
}

#[test]
fn undoing_a_note_puts_the_previous_text_back() {
    let fx = Fixture::new("note-restore");
    let ws = fx.ws();
    let mut log = History::default();

    note_set(&ws, &mut log, "printer", Some("out of paper"));
    note_set(&ws, &mut log, "printer", Some("jams on thick stock"));
    assert_eq!(note(&ws, "printer").as_deref(), Some("jams on thick stock"));

    step(&ws, &mut log, true);
    assert_eq!(note(&ws, "printer").as_deref(), Some("out of paper"));
    step(&ws, &mut log, false);
    assert_eq!(note(&ws, "printer").as_deref(), Some("jams on thick stock"));
}

#[test]
fn undoing_the_first_note_leaves_the_skill_with_none() {
    let fx = Fixture::new("note-first");
    let ws = fx.ws();
    let mut log = History::default();

    note_set(&ws, &mut log, "bicycle", Some("rear wheel is true"));
    step(&ws, &mut log, true);
    assert_eq!(
        note(&ws, "bicycle"),
        None,
        "there was no note to go back to"
    );
    assert!(
        ws.meta.exists("bicycle"),
        "the metadata file itself stays; only the note was taken back"
    );

    step(&ws, &mut log, false);
    assert_eq!(note(&ws, "bicycle").as_deref(), Some("rear wheel is true"));
}

#[test]
fn clearing_a_note_and_taking_it_back() {
    let fx = Fixture::new("note-clear");
    let ws = fx.ws();
    let mut log = History::default();

    note_set(&ws, &mut log, "printer", Some("needs a new drum"));
    note_set(&ws, &mut log, "printer", None);
    assert_eq!(note(&ws, "printer"), None);
    step(&ws, &mut log, true);
    assert_eq!(note(&ws, "printer").as_deref(), Some("needs a new drum"));
}

#[test]
fn repeated_undo_walks_back_through_every_kind_of_write() {
    let fx = Fixture::new("repeated");
    let ws = fx.ws();
    let mut log = History::default();
    ws.presets
        .save(&Preset {
            name: "commute".into(),
            ..Default::default()
        })
        .unwrap();

    tag_set(&ws, &mut log, "printer", &["office"]);
    note_set(&ws, &mut log, "printer", Some("second floor"));
    let (_, intent) = history::preset_edit(&ws, "commute", |m| m.push("bicycle".into())).unwrap();
    log.record(intent.unwrap());

    step(&ws, &mut log, true);
    assert!(
        ws.presets
            .load("commute")
            .unwrap()
            .unwrap()
            .skills
            .is_empty()
    );
    step(&ws, &mut log, true);
    assert_eq!(note(&ws, "printer"), None);
    step(&ws, &mut log, true);
    assert!(tags(&ws, "printer").is_empty());
    assert!(log.is_empty());

    // And forward again, in the order they were done.
    for _ in 0..3 {
        step(&ws, &mut log, false);
    }
    assert_eq!(tags(&ws, "printer"), ["office"]);
    assert_eq!(note(&ws, "printer").as_deref(), Some("second floor"));
    assert_eq!(
        ws.presets.load("commute").unwrap().unwrap().skills,
        ["bicycle"]
    );

    // A second round over the same entries behaves the same way.
    step(&ws, &mut log, true);
    step(&ws, &mut log, false);
    assert_eq!(
        ws.presets.load("commute").unwrap().unwrap().skills,
        ["bicycle"]
    );
}

#[test]
fn a_note_edited_behind_the_tools_back_is_left_alone() {
    let fx = Fixture::new("note-behind");
    let ws = fx.ws();
    let mut log = History::default();

    note_set(&ws, &mut log, "printer", Some("out of paper"));
    note_set(&ws, &mut log, "printer", Some("jams on thick stock"));

    // As if the file were edited in $EDITOR, or pulled in from another machine.
    let path = ws.meta.path("printer");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(
        &path,
        text.replace("jams on thick stock", "sent for repair"),
    )
    .unwrap();

    let message = step(&ws, &mut log, true);
    assert_eq!(
        note(&ws, "printer").as_deref(),
        Some("sent for repair"),
        "undo must not discard what someone else wrote"
    );
    assert!(
        message.contains("changed since"),
        "the reason has to reach the user: {message}"
    );
}

#[test]
fn tags_added_by_hand_survive_an_undo_of_the_write_around_them() {
    let fx = Fixture::new("tags-behind");
    let ws = fx.ws();
    let mut log = History::default();

    tag_set(&ws, &mut log, "bicycle", &["outdoor"]);
    tag_set(&ws, &mut log, "bicycle", &["outdoor", "commute"]);
    edit::tag_add(&ws, "bicycle", &["steel".to_string()]).unwrap();

    // Tags are a set, so undo takes off the one tag the step put on and leaves
    // everything else where it is.
    step(&ws, &mut log, true);
    assert_eq!(tags(&ws, "bicycle"), ["outdoor", "steel"]);
}

#[test]
fn a_step_someone_else_already_took_is_dropped_with_a_reason() {
    let fx = Fixture::new("already");
    let ws = fx.ws();
    let mut log = History::default();

    tag_set(&ws, &mut log, "printer", &["office"]);
    edit::tag_remove(&ws, "printer", &["office".to_string()]).unwrap();

    let message = step(&ws, &mut log, true);
    assert!(message.starts_with("already done"), "{message}");
    assert!(log.is_empty());
    assert!(tags(&ws, "printer").is_empty());
}

#[test]
fn renaming_a_tag_across_skills_comes_back_as_one_step() {
    let fx = Fixture::new("tag-rename");
    let ws = fx.ws();
    let mut log = History::default();

    tag_set(&ws, &mut log, "printer", &["paper"]);
    tag_set(&ws, &mut log, "bicycle", &["paper"]);
    let (_, intent) = history::tag_edit(&ws, |ws| {
        edit::tag_rename(ws, "paper", "stationery").map(|n| n.to_string())
    })
    .unwrap();
    log.record(intent.expect("a rename that touched two skills"));
    assert_eq!(tags(&ws, "printer"), ["stationery"]);

    step(&ws, &mut log, true);
    assert_eq!(tags(&ws, "printer"), ["paper"]);
    assert_eq!(tags(&ws, "bicycle"), ["paper"]);
}

#[test]
fn preset_membership_goes_back_and_forth() {
    let fx = Fixture::new("preset");
    let ws = fx.ws();
    let mut log = History::default();
    ws.presets
        .save(&Preset {
            name: "commute".into(),
            skills: vec!["bicycle".into()],
            ..Default::default()
        })
        .unwrap();

    let (message, intent) =
        history::preset_edit(&ws, "commute", |m| m.retain(|s| s != "bicycle")).unwrap();
    assert_eq!(message, "removed bicycle from commute");
    log.record(intent.unwrap());

    step(&ws, &mut log, true);
    assert_eq!(
        ws.presets.load("commute").unwrap().unwrap().skills,
        ["bicycle"]
    );
    step(&ws, &mut log, false);
    assert!(
        ws.presets
            .load("commute")
            .unwrap()
            .unwrap()
            .skills
            .is_empty()
    );

    // A preset deleted in between is not recreated by undoing a change to it.
    ws.presets.remove("commute").unwrap();
    let message = step(&ws, &mut log, true);
    assert!(message.contains("is gone"), "{message}");
    assert!(ws.presets.load("commute").unwrap().is_none());
}
