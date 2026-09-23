use skills::Workspace;
use skills::config::{AgentConfig, Config, TagConfig};
use skills::history::{self, Intent, Plan};
use skills::ops::{deploy, edit};
use skills::preset::Preset;
use std::path::PathBuf;
use std::process::Command;

struct Fixture {
    base: PathBuf,
    ws: Workspace,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let base =
            std::env::temp_dir().join(format!("skills-preset-refs-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("library");
        std::fs::create_dir_all(&root).unwrap();
        Config {
            agents: vec![AgentConfig {
                key: "a".into(),
                name: "A".into(),
                skills_dir: base.join("agent").display().to_string(),
            }],
            tags: vec![
                tag("work", &["one", "two"]),
                tag("study", &["one", "three"]),
                tag("empty", &[]),
            ],
            tags_enabled: true,
            ..Default::default()
        }
        .save(&root)
        .unwrap();
        for key in ["one", "two", "three", "four", "extra"] {
            let dir = root.join(key);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("SKILL.md"),
                format!("---\nname: {key}\ndescription: {key}\n---\n"),
            )
            .unwrap();
        }
        Self {
            ws: Workspace::open(&root).unwrap(),
            base,
        }
    }

    fn preset(&self) -> Preset {
        self.ws.presets.load("daily").unwrap().unwrap()
    }

    fn cli(&self, args: &[&str]) -> serde_json::Value {
        let output = Command::new(env!("CARGO_BIN_EXE_skills"))
            .args(["--root", self.ws.root.to_str().unwrap(), "--json"])
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn tag(name: &str, members: &[&str]) -> TagConfig {
    TagConfig {
        name: name.into(),
        skills: members.iter().map(|key| (*key).into()).collect(),
        color: Some("blue".into()),
        description: None,
    }
}

fn history_step(ws: &Workspace, intent: &Intent, undo: bool) {
    let snap = ws.scan().unwrap();
    let plan = if undo {
        history::undo_plan(ws, &snap, intent)
    } else {
        history::redo_plan(ws, &snap, intent)
    }
    .unwrap();
    let Plan::Write { apply, .. } = plan else {
        panic!("expected metadata write")
    };
    apply.apply(ws).unwrap();
}

#[test]
fn tag_undo_never_mutates_an_externally_reused_name() {
    for operation in ["delete", "rename"] {
        for timing in ["before-plan", "after-plan", "after-done-prerequisite-plan"] {
            let f = Fixture::new(&format!("undo-conflict-{operation}-{timing}"));
            f.ws.presets
                .save(&Preset {
                    name: "daily".into(),
                    skills: vec!["one".into(), "two".into()],
                    ..Default::default()
                })
                .unwrap();
            let (_, intent) = history::tag_edit(&f.ws, |ws| {
                if operation == "delete" {
                    edit::tag_delete(ws, "work")
                } else {
                    edit::tag_rename(ws, "work", "renamed")
                }
                .map(|count| count.to_string())
            })
            .unwrap();
            let intent = intent.unwrap();
            if timing == "after-done-prerequisite-plan" {
                Config::edit_tags(&f.ws.root, |tags| {
                    if operation == "delete" {
                        tags.push(tag("work", &["one", "two"]));
                    } else {
                        tags.iter_mut()
                            .find(|tag| tag.name == "renamed")
                            .unwrap()
                            .name = "work".into();
                    }
                })
                .unwrap();
            }
            let plan = (timing != "before-plan")
                .then(|| history::undo_plan(&f.ws, &f.ws.scan().unwrap(), &intent).unwrap());
            Config::edit_tags(&f.ws.root, |tags| {
                tags.retain(|tag| tag.name != "work");
                let mut replacement = tag("work", &["extra"]);
                replacement.color = Some("red".into());
                tags.push(replacement);
                // A rename prerequisite validates both names, so recreating the
                // renamed entry after an already-done plan makes the conflict explicit.
                if operation == "rename" && timing == "after-done-prerequisite-plan" {
                    tags.push(tag("renamed", &["one", "two"]));
                }
            })
            .unwrap();
            let before_config = std::fs::read(Config::path(&f.ws.root)).unwrap();
            let before_preset = std::fs::read(f.ws.presets.path("daily")).unwrap();
            let plan = plan.unwrap_or_else(|| {
                history::undo_plan(&f.ws, &f.ws.scan().unwrap(), &intent).unwrap()
            });
            let message = match plan {
                Plan::Nothing(message) => message,
                Plan::Write { apply, .. } => apply.apply(&f.ws).unwrap(),
                Plan::Links(_) => panic!("metadata undo must not plan links"),
            };
            assert!(
                message.contains("left as they are") || message.starts_with("already done:"),
                "{operation}/{timing}: {message}"
            );
            assert_eq!(
                std::fs::read(Config::path(&f.ws.root)).unwrap(),
                before_config,
                "{operation}/{timing}"
            );
            assert_eq!(
                std::fs::read(f.ws.presets.path("daily")).unwrap(),
                before_preset,
                "{operation}/{timing}"
            );
        }
    }
}

#[test]
fn deleting_a_hand_written_tag_round_trips_its_membership_as_a_set() {
    let f = Fixture::new("undo-unsorted-tag");
    let mut config = f.ws.load_config().unwrap();
    config
        .tags
        .iter_mut()
        .find(|tag| tag.name == "work")
        .unwrap()
        .skills = vec!["two".into(), "one".into(), "one".into()];
    config.save(&f.ws.root).unwrap();
    f.ws.presets
        .save(&Preset {
            name: "daily".into(),
            skills: vec!["one".into(), "two".into()],
            ..Default::default()
        })
        .unwrap();
    let (_, intent) = history::tag_edit(&f.ws, |ws| {
        edit::tag_delete(ws, "work").map(|count| count.to_string())
    })
    .unwrap();
    let intent = intent.unwrap();
    history_step(&f.ws, &intent, true);
    assert_eq!(f.preset().members(), ["one", "two"]);
    assert_eq!(
        f.ws.load_config()
            .unwrap()
            .tags
            .iter()
            .find(|tag| tag.name == "work")
            .unwrap()
            .skills,
        ["one", "two"]
    );
    history_step(&f.ws, &intent, false);
    assert_eq!(f.preset().members(), ["one", "two"]);
    assert!(
        f.ws.load_config()
            .unwrap()
            .tags
            .iter()
            .all(|tag| tag.name != "work")
    );
}

#[test]
fn legacy_migration_snapshots_members_once_and_keeps_exact_backups() {
    let f = Fixture::new("migration");
    std::fs::create_dir_all(&f.ws.presets.dir).unwrap();
    let original = "# keep the original format for recovery\nname = 'daily'\ndescription = 'Daily tools'\ncolor = '#b87e54'\nskills = ['extra', 'one', 'extra', 'missing']\ntags = ['work', 'study', 'work']\nagents = ['a']\n";
    let empty = "name = 'empty'\ntags = []\nskills = ['two', 'two']\n";
    let modern = "# A fixed preset must not be inferred or rewritten\nname = 'modern'\nskills = ['two', 'one', 'one']\n";
    std::fs::write(f.ws.presets.path("daily"), original).unwrap();
    std::fs::write(f.ws.presets.path("empty"), empty).unwrap();
    std::fs::write(f.ws.presets.path("modern"), modern).unwrap();
    assert!(f.ws.presets.load("daily").is_err());
    let mut ws = Workspace::open(&f.ws.root).unwrap();
    let report = ws.preset_migration.as_ref().unwrap();
    assert_eq!(report.migrated_names, ["daily", "empty"]);
    assert!(
        report
            .backup_dir
            .starts_with(f.ws.root.join(".skills-meta/backups"))
    );
    assert_eq!(
        std::fs::read(report.backup_dir.join("daily.toml")).unwrap(),
        original.as_bytes()
    );
    assert_eq!(
        std::fs::read(report.backup_dir.join("empty.toml")).unwrap(),
        empty.as_bytes()
    );
    assert_eq!(
        std::fs::read(f.ws.presets.path("modern")).unwrap(),
        modern.as_bytes()
    );
    let fixed = ws.presets.load("daily").unwrap().unwrap();
    assert_eq!(fixed.members(), ["extra", "missing", "one", "three", "two"]);
    assert_eq!(fixed.description.as_deref(), Some("Daily tools"));
    assert_eq!(fixed.color.as_deref(), Some("#b87e54"));
    assert_eq!(fixed.agents, ["a"]);
    assert!(
        !std::fs::read_to_string(ws.presets.path("daily"))
            .unwrap()
            .contains("tags =")
    );
    assert_eq!(ws.presets.load("empty").unwrap().unwrap().skills, ["two"]);
    assert!(
        !f.base.join("agent").exists(),
        "migration must not deploy any members"
    );
    let backup_parent = report.backup_dir.parent().unwrap().to_path_buf();
    assert!(
        Workspace::open(&ws.root)
            .unwrap()
            .preset_migration
            .is_none()
    );
    assert_eq!(std::fs::read_dir(backup_parent).unwrap().count(), 1);
    Config::edit_tags(&ws.root, |tags| tags.clear()).unwrap();
    ws.config = ws.load_config().unwrap();
    assert_eq!(ws.presets.load("daily").unwrap().unwrap(), fixed);
    assert_eq!(
        ws.scan().unwrap().get("missing").unwrap().presets,
        ["daily"]
    );
}

#[test]
fn migration_preflights_every_file_before_changing_any_definition() {
    for invalid in [
        "name = 'bad'\ntags = ['unknown']\n",
        "name = 'bad'\ntags = 7\n",
        "not valid TOML",
    ] {
        let f = Fixture::new(&format!("migration-invalid-{}", invalid.len()));
        std::fs::create_dir_all(&f.ws.presets.dir).unwrap();
        let original = "name = 'daily'\ntags = ['work']\n";
        std::fs::write(f.ws.presets.path("daily"), original).unwrap();
        std::fs::write(f.ws.presets.path("z-bad"), invalid).unwrap();
        let error = Workspace::open(&f.ws.root).unwrap_err();
        assert!(format!("{error:#}").contains("z-bad.toml"));
        assert_eq!(
            std::fs::read(f.ws.presets.path("daily")).unwrap(),
            original.as_bytes()
        );
        assert_eq!(
            std::fs::read(f.ws.presets.path("z-bad")).unwrap(),
            invalid.as_bytes()
        );
        assert!(!f.ws.root.join(".skills-meta/backups").exists());
    }
}

#[test]
fn local_migration_uses_only_the_project_tag_members() {
    let f = Fixture::new("migration-local");
    let project = f.base.join("project");
    let root = project.join(".agents/skills");
    std::fs::create_dir_all(&root).unwrap();
    Config {
        agents: vec![],
        tags: vec![tag("work", &["local-only"])],
        ..Default::default()
    }
    .save(&root)
    .unwrap();
    let store = skills::preset::PresetStore::new(&root);
    std::fs::create_dir_all(&store.dir).unwrap();
    let original = "name = 'daily'\ntags = ['work']\n";
    std::fs::write(store.path("daily"), original).unwrap();
    let local = Workspace::open_local(&project, false).unwrap();
    assert_eq!(
        local.presets.load("daily").unwrap().unwrap().skills,
        ["local-only"]
    );
    assert!(
        local
            .preset_migration
            .unwrap()
            .backup_dir
            .starts_with(std::fs::canonicalize(root).unwrap())
    );
    assert!(f.ws.presets.list().unwrap().is_empty());
}

#[test]
fn cli_migration_reports_backups_on_stderr_without_polluting_json() {
    let f = Fixture::new("migration-cli");
    std::fs::create_dir_all(&f.ws.presets.dir).unwrap();
    std::fs::write(
        f.ws.presets.path("daily"),
        "name = 'daily'\ntags = ['work']\n",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_skills"))
        .args([
            "--root",
            f.ws.root.to_str().unwrap(),
            "--json",
            "preset",
            "show",
            "daily",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["skills"], serde_json::json!(["one", "two"]));
    assert!(value.get("tags").is_none());
    assert!(String::from_utf8_lossy(&output.stderr).contains("presets-before-fixed-members-"));
}

#[test]
fn cli_tag_selection_materializes_current_members_for_create_add_and_remove() {
    let f = Fixture::new("cli-fixed");
    let value = f.cli(&[
        "preset", "create", "daily", "--tag", "work", "--tag", "work", "--skill", "extra",
    ]);
    assert!(value.get("tags").is_none());
    assert_eq!(f.preset().skills, ["extra", "one", "two"]);
    Config::edit_tags(&f.ws.root, |tags| {
        tags.iter_mut().find(|t| t.name == "work").unwrap().skills = vec!["four".into()]
    })
    .unwrap();
    assert_eq!(f.preset().skills, ["extra", "one", "two"]);
    f.cli(&[
        "preset", "add", "daily", "one", "--tag", "work", "--tag", "study",
    ]);
    assert_eq!(f.preset().skills, ["extra", "four", "one", "three", "two"]);
    f.cli(&["preset", "remove", "daily", "extra", "--tag", "work"]);
    assert_eq!(f.preset().skills, ["one", "three", "two"]);
    for operation in ["add", "remove"] {
        let before = std::fs::read(f.ws.presets.path("daily")).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_skills"))
            .args([
                "--root",
                f.ws.root.to_str().unwrap(),
                "preset",
                operation,
                "daily",
                "one",
                "--tag",
                "unknown",
            ])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("unknown"));
        assert_eq!(std::fs::read(f.ws.presets.path("daily")).unwrap(), before);
    }
    f.cli(&["preset", "deploy", "daily", "--agent", "a"]);
    for key in ["one", "two", "three"] {
        assert!(f.base.join("agent").join(key).is_dir());
    }
    Config::edit_tags(&f.ws.root, |tags| tags.clear()).unwrap();
    f.cli(&["preset", "undeploy", "daily", "--agent", "a"]);
    for key in ["one", "two", "three"] {
        assert!(!f.base.join("agent").join(key).exists());
    }
}

#[test]
fn snapshot_indexes_fixed_members_including_missing_sources_and_refreshes_query_filters() {
    use skills::search::{Query, Searcher};
    let f = Fixture::new("index-query");
    for (name, members) in [
        ("daily", vec!["one", "missing", "one"]),
        ("other", vec!["one", "two"]),
    ] {
        f.ws.presets
            .save(&Preset {
                name: name.into(),
                skills: members.into_iter().map(String::from).collect(),
                ..Default::default()
            })
            .unwrap();
    }
    let snap = f.ws.scan().unwrap();
    assert_eq!(
        snap.presets.get("daily").unwrap().members(),
        ["missing", "one"]
    );
    assert_eq!(
        snap.presets
            .for_skill("one")
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>(),
        ["daily", "other"]
    );
    assert_eq!(snap.get("missing").unwrap().presets, ["daily"]);
    assert!(!snap.get("missing").unwrap().status.is_present());
    let scoped = skills::reconcile::rescope(&snap, &[]).unwrap();
    assert_eq!(scoped.presets.by_skill, snap.presets.by_skill);
    assert_eq!(scoped.get("one").unwrap().presets, ["daily", "other"]);
    let mut searcher = Searcher::new();
    let keys = |searcher: &mut Searcher, snap: &skills::reconcile::Snapshot, query: &str| {
        searcher
            .search(&snap.skills, &Query::parse(query))
            .into_iter()
            .map(|hit| snap.skills[hit.index].key.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        keys(&mut searcher, &snap, "preset:DAILY"),
        ["missing", "one"]
    );
    assert_eq!(keys(&mut searcher, &snap, "preset:daily tag:work"), ["one"]);
    assert_eq!(
        keys(&mut searcher, &snap, "preset:daily preset:other"),
        ["one"]
    );
    assert!(keys(&mut searcher, &snap, "preset:unknown").is_empty());
    history::preset_edit(&f.ws, "daily", |skills| {
        skills.clear();
        skills.push("two".into());
    })
    .unwrap();
    let snap = f.ws.scan().unwrap();
    assert_eq!(keys(&mut searcher, &snap, "preset:daily"), ["two"]);
    assert_eq!(snap.get("one").unwrap().presets, ["other"]);
    f.ws.presets.rename("daily", "renamed").unwrap();
    let snap = f.ws.scan().unwrap();
    assert!(keys(&mut searcher, &snap, "preset:daily").is_empty());
    assert_eq!(keys(&mut searcher, &snap, "preset:renamed"), ["two"]);
    f.ws.presets.remove("renamed").unwrap();
    let snap = f.ws.scan().unwrap();
    assert!(keys(&mut searcher, &snap, "preset:renamed").is_empty());
}

#[test]
fn removing_a_skill_removes_its_key_from_every_preset() {
    let f = Fixture::new("remove-skill");
    for (name, members) in [
        ("daily", vec!["one", "two", "one"]),
        ("weekly", vec!["one", "three"]),
        ("untouched", vec!["two", "four"]),
    ] {
        f.ws.presets
            .save(&Preset {
                name: name.into(),
                description: Some(format!("{name} description")),
                skills: members.into_iter().map(String::from).collect(),
                agents: vec!["a".into()],
                ..Default::default()
            })
            .unwrap();
    }

    let daily_path = f.ws.presets.path("daily");
    let daily = std::fs::read_to_string(&daily_path).unwrap();
    std::fs::write(
        &daily_path,
        format!("# keep this comment\ncustom = \"keep\"\n{daily}"),
    )
    .unwrap();
    let untouched = std::fs::read(f.ws.presets.path("untouched")).unwrap();
    let log = edit::remove(&f.ws, &f.ws.scan().unwrap(), "one", false).unwrap();

    for name in ["daily", "weekly"] {
        let preset = f.ws.presets.load(name).unwrap().unwrap();
        assert!(!preset.skills.iter().any(|skill| skill == "one"));
        assert_eq!(preset.description, Some(format!("{name} description")));
        assert_eq!(preset.agents, ["a"]);
        assert!(log.contains(&format!("updated preset {name}")));
    }
    assert_eq!(f.ws.presets.load("daily").unwrap().unwrap().skills, ["two"]);
    let daily = std::fs::read_to_string(daily_path).unwrap();
    assert!(daily.contains("# keep this comment"));
    assert!(daily.contains("custom = \"keep\""));
    assert_eq!(
        f.ws.presets.load("weekly").unwrap().unwrap().skills,
        ["three"]
    );
    assert_eq!(
        f.ws.presets.load("untouched").unwrap().unwrap().skills,
        ["four", "two"]
    );
    assert_eq!(
        std::fs::read(f.ws.presets.path("untouched")).unwrap(),
        untouched
    );
    assert!(!log.contains(&"updated preset untouched".to_string()));
}

#[test]
fn keeping_metadata_keeps_group_membership_for_a_restorable_missing_skill() {
    let f = Fixture::new("remove-skill-keep-meta");
    f.ws.presets
        .save(&Preset {
            name: "daily".into(),
            skills: vec!["one".into()],
            ..Default::default()
        })
        .unwrap();

    edit::remove(&f.ws, &f.ws.scan().unwrap(), "one", true).unwrap();

    assert_eq!(f.ws.presets.load("daily").unwrap().unwrap().skills, ["one"]);
    assert!(
        Config::load(&f.ws.root)
            .unwrap()
            .skill_tags("one")
            .contains(&"work".into())
    );
    let record = f.ws.scan().unwrap().get("one").unwrap().clone();
    assert_eq!(record.status, skills::reconcile::SkillStatus::Missing);
    assert_eq!(record.presets, ["daily"]);
}

#[test]
fn coverage_is_a_set_intersection_and_includes_empty_or_uncovered_tags() {
    use skills::preset::{TagCoverage, tag_coverages, tag_members};
    let config = Config {
        tags_enabled: false,
        tags: vec![
            tag("work", &["one", "one", "two"]),
            tag("work", &["three"]),
            tag("empty", &[]),
            tag("other", &["four"]),
        ],
        ..Default::default()
    };
    assert_eq!(
        tag_members(&config, &["work".into(), "work".into()]).unwrap(),
        ["one", "three", "two"]
    );
    assert!(tag_members(&config, &["unknown".into()]).is_err());
    assert_eq!(
        tag_coverages(
            &config,
            &["one".into(), "two".into(), "unrelated".into()]
                .into_iter()
                .collect()
        ),
        vec![
            TagCoverage {
                name: "empty".into(),
                included: 0,
                total: 0
            },
            TagCoverage {
                name: "other".into(),
                included: 0,
                total: 1
            },
            TagCoverage {
                name: "work".into(),
                included: 2,
                total: 3
            },
        ]
    );
}

#[test]
fn missing_fixed_members_stay_in_coverage_denominator() {
    let f = Fixture::new("missing-coverage");
    let preset = Preset {
        name: "daily".into(),
        skills: vec!["one".into(), "missing".into(), "one".into()],
        ..Default::default()
    };
    f.ws.presets.save(&preset).unwrap();
    let scope = vec!["a".into()];
    deploy::apply(
        &deploy::plan_preset_activate(&f.ws, &f.ws.scan().unwrap(), &preset, &scope).unwrap(),
    )
    .unwrap();
    let status = deploy::preset_status(&f.ws.scan().unwrap(), &preset, &scope);
    assert_eq!((status.installed, status.total), (1, 2));
    assert_eq!(status.absent, ["missing"]);
    assert_eq!(status.state(), deploy::PresetState::Partial);
    assert_eq!(
        deploy::preset_status(&f.ws.scan().unwrap(), &preset, &[]).state(),
        deploy::PresetState::Empty
    );
}

#[test]
fn ordinary_tag_edits_and_undo_never_change_fixed_preset_definitions() {
    let f = Fixture::new("tag-fixed");
    f.ws.presets
        .save(&Preset {
            name: "daily".into(),
            skills: vec!["one".into(), "two".into()],
            ..Default::default()
        })
        .unwrap();
    let original = std::fs::read(f.ws.presets.path("daily")).unwrap();
    for operation in ["rename", "merge", "delete"] {
        let (_, intent) = history::tag_edit(&f.ws, |ws| {
            match operation {
                "rename" => edit::tag_rename(ws, "work", "renamed"),
                "merge" => edit::tag_rename(ws, "work", "study"),
                _ => edit::tag_delete(ws, "work"),
            }
            .map(|count| count.to_string())
        })
        .unwrap();
        assert_eq!(std::fs::read(f.ws.presets.path("daily")).unwrap(), original);
        history_step(&f.ws, &intent.unwrap(), true);
        assert_eq!(std::fs::read(f.ws.presets.path("daily")).unwrap(), original);
    }
}
