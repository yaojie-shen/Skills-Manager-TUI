//! Taking metadata writes back: tags, notes and preset membership, against a
//! tree that may have moved on in between.

use skills::Workspace;
use skills::config::{AgentConfig, Config, DeployConfig};
use skills::history::{self, History, Plan};
use skills::meta::{Baseline, SkillMeta, Source};
use skills::ops::install::InstallRef;
use skills::ops::{deploy, edit};
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

/// Rename through the core the way the TUI's confirmation does, and log it.
fn rename(ws: &Workspace, log: &mut History, from: &str, to: &str) {
    let snap = ws.scan().unwrap();
    edit::rename(ws, &snap, from, to).unwrap();
    log.record(history::Intent::Rename {
        from: from.into(),
        to: to.into(),
    });
}

fn source(ws: &Workspace, key: &str) -> Option<Source> {
    ws.meta.load(key).unwrap().and_then(|m| m.source)
}

#[test]
fn renaming_a_skill_goes_back_and_forth_with_its_link() {
    let fx = Fixture::new("rename");
    let ws = fx.ws();
    let mut log = History::default();
    let agent = fx.base.join("agent-a");
    tag_set(&ws, &mut log, "printer", &["office"]);
    let snap = ws.scan().unwrap();
    let plan = deploy::plan_deploy(&ws, &snap, &["printer".into()], &["a".into()]).unwrap();
    deploy::apply(&plan).unwrap();

    rename(&ws, &mut log, "printer", "etcd");
    assert!(fx.root.join("etcd").is_dir());
    assert!(!fx.root.join("printer").exists());
    assert_eq!(tags(&ws, "etcd"), ["office"], "metadata moved with it");
    assert_eq!(
        std::fs::read_link(agent.join("etcd")).unwrap(),
        fx.root.join("etcd")
    );
    assert!(!agent.join("printer").exists());

    // Back: the directory, the metadata and the link all return to the old name.
    let message = step(&ws, &mut log, true);
    assert_eq!(message, "renamed etcd to printer");
    assert!(fx.root.join("printer").is_dir());
    assert!(!fx.root.join("etcd").exists());
    assert_eq!(tags(&ws, "printer"), ["office"]);
    assert_eq!(
        std::fs::read_link(agent.join("printer")).unwrap(),
        fx.root.join("printer")
    );
    assert!(!agent.join("etcd").exists());

    // And forward again.
    step(&ws, &mut log, false);
    assert!(fx.root.join("etcd").is_dir());
    assert!(agent.join("etcd").exists());
    assert!(!fx.root.join("printer").exists());
}

#[test]
fn undoing_a_rename_stops_when_the_old_name_is_taken() {
    let fx = Fixture::new("rename-taken");
    let ws = fx.ws();
    let mut log = History::default();

    rename(&ws, &mut log, "printer", "etcd");
    // Something new has moved in under the old name since.
    fx.add_skill("printer");
    std::fs::write(fx.root.join("printer").join("extra.txt"), "new one").unwrap();

    let message = step(&ws, &mut log, true);
    assert!(message.contains("printer is taken"), "{message}");
    assert!(
        fx.root.join("printer").join("extra.txt").is_file(),
        "the skill that took the name is left alone"
    );
    assert!(
        fx.root.join("etcd").is_dir(),
        "and the renamed one stays put"
    );
    assert!(log.is_empty(), "a step with nothing to do is dropped");
}

#[test]
fn setting_a_source_goes_back_and_forth_with_its_revision() {
    let fx = Fixture::new("source");
    let ws = fx.ws();
    let mut log = History::default();
    let installed = Source::Git {
        url: "https://github.com/acme/printer".into(),
        subpath: None,
        branch: Some("main".into()),
        revision: Some("0123456789abcdef".into()),
    };
    ws.meta
        .save(
            "printer",
            &SkillMeta {
                source: Some(installed.clone()),
                baseline: Some(Baseline {
                    hash: "x".into(),
                    hash_algo: 1,
                }),
                ..Default::default()
            },
        )
        .unwrap();

    let (message, intent) = history::source_edit(
        &ws,
        "printer",
        &InstallRef::Git {
            url: "https://github.com/acme/printers".into(),
            branch: None,
            subpath: Some("printer".into()),
        },
    )
    .unwrap();
    assert_eq!(
        message,
        "source of printer set to https://github.com/acme/printers/printer"
    );
    log.record(intent.expect("a different source is a step"));
    assert!(matches!(
        source(&ws, "printer"),
        Some(Source::Git { revision: None, .. })
    ));

    // Undo brings the whole recorded source back, revision included.
    step(&ws, &mut log, true);
    assert_eq!(source(&ws, "printer"), Some(installed));
    step(&ws, &mut log, false);
    assert!(matches!(
        source(&ws, "printer"),
        Some(Source::Git { ref url, .. }) if url == "https://github.com/acme/printers"
    ));

    // A skill that never had metadata gets its source taken back to none.
    let (_, intent) = history::source_edit(
        &ws,
        "bicycle",
        &InstallRef::Git {
            url: "https://github.com/acme/bicycle".into(),
            branch: None,
            subpath: None,
        },
    )
    .unwrap();
    log.record(intent.unwrap());
    step(&ws, &mut log, true);
    assert_eq!(source(&ws, "bicycle"), None);
    assert!(
        ws.meta.exists("bicycle"),
        "the file stays; only the source went"
    );
}

fn description(ws: &Workspace, name: &str) -> Option<String> {
    ws.presets.load(name).unwrap().unwrap().description
}

fn describe(ws: &Workspace, log: &mut History, name: &str, text: Option<&str>) -> String {
    let (message, intent) = history::preset_description_edit(ws, name, text).unwrap();
    if let Some(intent) = intent {
        log.record(intent);
    }
    message
}

#[test]
fn a_preset_description_goes_back_and_forth() {
    let fx = Fixture::new("preset-description");
    let ws = fx.ws();
    let mut log = History::default();
    ws.presets
        .save(&Preset {
            name: "commute".into(),
            ..Default::default()
        })
        .unwrap();

    assert_eq!(
        describe(&ws, &mut log, "commute", Some("rides to work")),
        "description saved on commute"
    );
    describe(&ws, &mut log, "commute", Some("  weekend rides  "));
    assert_eq!(
        description(&ws, "commute").as_deref(),
        Some("weekend rides"),
        "the prompt's padding is not part of the sentence"
    );
    assert!(
        history::preset_description_edit(&ws, "commute", Some("weekend rides"))
            .unwrap()
            .1
            .is_none(),
        "saving the same text is not a step"
    );

    step(&ws, &mut log, true);
    assert_eq!(
        description(&ws, "commute").as_deref(),
        Some("rides to work")
    );
    step(&ws, &mut log, true);
    assert_eq!(
        description(&ws, "commute"),
        None,
        "there was no description to go back to"
    );
    step(&ws, &mut log, false);
    assert_eq!(
        description(&ws, "commute").as_deref(),
        Some("rides to work")
    );
    step(&ws, &mut log, false);
    assert_eq!(
        description(&ws, "commute").as_deref(),
        Some("weekend rides")
    );

    // Clearing through the prompt is an empty field, and comes back too.
    assert_eq!(
        describe(&ws, &mut log, "commute", Some("   ")),
        "description cleared on commute"
    );
    assert_eq!(description(&ws, "commute"), None);
    step(&ws, &mut log, true);
    assert_eq!(
        description(&ws, "commute").as_deref(),
        Some("weekend rides")
    );
}

#[test]
fn a_preset_description_edited_behind_the_tools_back_is_left_alone() {
    let fx = Fixture::new("preset-description-behind");
    let ws = fx.ws();
    let mut log = History::default();
    ws.presets
        .save(&Preset {
            name: "commute".into(),
            ..Default::default()
        })
        .unwrap();
    describe(&ws, &mut log, "commute", Some("rides to work"));
    describe(&ws, &mut log, "commute", Some("weekend rides"));

    // As if the file were edited in $EDITOR, or pulled in from another machine.
    let path = ws.presets.path("commute");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, text.replace("weekend rides", "rides in the rain")).unwrap();

    let message = step(&ws, &mut log, true);
    assert_eq!(
        description(&ws, "commute").as_deref(),
        Some("rides in the rain"),
        "undo must not discard what someone else wrote"
    );
    assert!(
        message.contains("changed since"),
        "the reason has to reach the user: {message}"
    );
}

/// A config written by hand: the auto-deploy list names the preset and
/// carries a comment, which a rename has to leave in place.
fn config_by_hand(fx: &Fixture) {
    let agent = fx.base.join("agent-a").display().to_string();
    std::fs::write(
        Config::path(&fx.root),
        format!(
            "schema = 1\n\n[[agents]]\nkey = \"a\"\nname = \"Agent A\"\nskills_dir = \"{agent}\"\n\n\
             [deploy]\nall_to_all = false\n# the everyday set\npresets = [\"commute\"] # goes first\n"
        ),
    )
    .unwrap();
}

#[test]
fn renaming_a_preset_goes_back_and_forth_with_its_auto_deploy_entry() {
    let fx = Fixture::new("preset-rename");
    config_by_hand(&fx);
    let ws = fx.ws();
    let mut log = History::default();
    ws.presets
        .save(&Preset {
            name: "commute".into(),
            description: Some("rides to work".into()),
            skills: vec!["bicycle".into()],
            ..Default::default()
        })
        .unwrap();
    assert_eq!(ws.config.deploy.presets, ["commute"]);

    let (message, intent) = history::preset_rename(&ws, "commute", "errands").unwrap();
    assert_eq!(
        message,
        "renamed preset commute to errands, config.toml too"
    );
    log.record(intent.unwrap());
    let moved = ws.presets.load("errands").unwrap().unwrap();
    assert_eq!(moved.name, "errands", "the name inside the file moved too");
    assert_eq!(moved.description.as_deref(), Some("rides to work"));
    assert_eq!(moved.skills, ["bicycle"]);
    assert!(ws.presets.load("commute").unwrap().is_none());
    assert_eq!(Config::load(&fx.root).unwrap().deploy.presets, ["errands"]);
    let text = std::fs::read_to_string(Config::path(&fx.root)).unwrap();
    assert!(
        text.contains("# the everyday set\npresets = [\"errands\"] # goes first"),
        "only the name changed; the comments around it stay: {text}"
    );

    // Back: the file and the config entry both return to the old name.
    let message = step(&ws, &mut log, true);
    assert_eq!(
        message,
        "renamed preset errands to commute, config.toml too"
    );
    assert_eq!(
        ws.presets.load("commute").unwrap().unwrap().skills,
        ["bicycle"]
    );
    assert!(ws.presets.load("errands").unwrap().is_none());
    assert_eq!(Config::load(&fx.root).unwrap().deploy.presets, ["commute"]);

    // And forward again.
    step(&ws, &mut log, false);
    assert!(ws.presets.load("errands").unwrap().is_some());
    assert!(ws.presets.load("commute").unwrap().is_none());
    assert_eq!(Config::load(&fx.root).unwrap().deploy.presets, ["errands"]);

    // A preset the config does not list leaves the config alone.
    let before = std::fs::read_to_string(Config::path(&fx.root)).unwrap();
    ws.presets
        .save(&Preset {
            name: "weekend".into(),
            ..Default::default()
        })
        .unwrap();
    let (message, _) = history::preset_rename(&ws, "weekend", "sunday").unwrap();
    assert_eq!(message, "renamed preset weekend to sunday");
    assert_eq!(
        std::fs::read_to_string(Config::path(&fx.root)).unwrap(),
        before
    );
}

#[test]
fn undoing_a_preset_rename_stops_when_the_old_name_is_taken() {
    let fx = Fixture::new("preset-rename-taken");
    let ws = fx.ws();
    let mut log = History::default();
    ws.presets
        .save(&Preset {
            name: "commute".into(),
            skills: vec!["bicycle".into()],
            ..Default::default()
        })
        .unwrap();
    let (_, intent) = history::preset_rename(&ws, "commute", "errands").unwrap();
    log.record(intent.unwrap());

    // Something new has moved in under the old name since.
    ws.presets
        .save(&Preset {
            name: "commute".into(),
            description: Some("the new one".into()),
            ..Default::default()
        })
        .unwrap();

    let message = step(&ws, &mut log, true);
    assert!(message.contains("preset commute is taken"), "{message}");
    assert_eq!(
        description(&ws, "commute").as_deref(),
        Some("the new one"),
        "the preset that took the name is left alone"
    );
    assert_eq!(
        ws.presets.load("errands").unwrap().unwrap().skills,
        ["bicycle"],
        "and the renamed one stays put"
    );
    assert!(log.is_empty(), "a step with nothing to do is dropped");

    // Renaming onto a name that exists is refused outright, before anything
    // moves.
    let err = history::preset_rename(&ws, "errands", "commute").unwrap_err();
    assert!(err.to_string().contains("already exists"), "{err:#}");
    assert!(ws.presets.load("errands").unwrap().is_some());
}
