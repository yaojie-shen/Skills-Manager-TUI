//! Batch repair must preserve user content and leave ambiguous moves untouched.
use skills::{
    Workspace,
    config::{AgentConfig, Config},
    ops::{DownloadDir, edit, repair},
    preset::Preset,
};
use std::{fs, process::Command};

struct Fixture {
    _tmp: DownloadDir,
    ws: Workspace,
    agent: std::path::PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let tmp = DownloadDir::new("repair-test").unwrap();
        let root = tmp.path().join("root");
        let agent = tmp.path().join("agent");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&agent).unwrap();
        let cfg = Config {
            agents: vec![AgentConfig {
                key: "test".into(),
                name: "Test".into(),
                skills_dir: agent.display().to_string(),
            }],
            ..Config::default()
        };
        cfg.save(&root).unwrap();
        let ws = Workspace::open(&root).unwrap();
        Self {
            _tmp: tmp,
            ws,
            agent,
        }
    }
    fn skill(&self, key: &str, body: &str) {
        let dir = self.ws.skill_path(key);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: sample\ndescription: Test\n---\n{body}\n"),
        )
        .unwrap();
    }
    fn track(&self, key: &str) {
        let meta = skills::meta::SkillMeta {
            source: Some(skills::meta::Source::Git {
                url: "https://example.invalid/skills.git".into(),
                subpath: Some(key.into()),
                branch: None,
                revision: None,
            }),
            baseline: Some(skills::meta::Baseline {
                hash: skills::hash::hash_directory(&self.ws.skill_path(key)).unwrap(),
                hash_algo: skills::hash::HASH_ALGO,
            }),
            ..Default::default()
        };
        self.ws.meta.save(key, &meta).unwrap();
    }
    fn moved(&self, old: &str, new: &str) {
        self.skill(old, old);
        self.track(old);
        edit::tag_add(&self.ws, old, &["keep".into()]).unwrap();
        fs::rename(self.ws.skill_path(old), self.ws.skill_path(new)).unwrap();
    }
    fn cli(&self, args: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_skills"))
            .env("HOME", self._tmp.path().join("home"))
            .env("PATH", "")
            .current_dir(self._tmp.path())
            .arg("--root")
            .arg(&self.ws.root)
            .arg("--json")
            .args(args)
            .output()
            .unwrap()
    }
}

#[test]
fn preview_is_read_only_and_apply_repairs_multiple_moves_links_and_presets() {
    let f = Fixture::new();
    f.moved("repos/demo/old-a", "repos/demo/new-a");
    f.moved("repos/demo/old-b", "repos/demo/new-b");
    std::os::unix::fs::symlink(f.ws.skill_path("repos/demo/old-a"), f.agent.join("old-a")).unwrap();
    f.ws.presets
        .save(&Preset {
            name: "keep".into(),
            skills: vec!["repos/demo/old-a".into(), "repos/demo/old-b".into()],
            ..Default::default()
        })
        .unwrap();
    let output = f.cli(&["repair"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let preview: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(preview["planned"], 2);
    assert_eq!(preview["applied"], false);
    assert!(f.ws.meta.exists("repos/demo/old-a"));
    assert!(!f.ws.meta.exists("repos/demo/new-a"));
    assert_eq!(
        fs::read_link(f.agent.join("old-a")).unwrap(),
        f.ws.skill_path("repos/demo/old-a")
    );
    let output = f.cli(&["repair", "--apply"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let applied: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(applied["repaired"], 2);
    assert!(!f.ws.meta.exists("repos/demo/old-a"));
    assert_eq!(
        f.ws.load_config().unwrap().skill_tags("repos/demo/new-a"),
        ["keep"]
    );
    assert_eq!(
        fs::read_link(f.agent.join("new-a")).unwrap(),
        f.ws.skill_path("repos/demo/new-a")
    );
    assert_eq!(
        f.ws.presets.load("keep").unwrap().unwrap().skills,
        ["repos/demo/new-a", "repos/demo/new-b"]
    );
    assert_eq!(repair::run(&f.ws, true).unwrap().repaired, 0);
    assert!(!f.cli(&["repair", "--apply", "--dry-run"]).status.success());
}

#[test]
fn ambiguous_missing_modified_invalid_and_external_entries_are_preserved() {
    let f = Fixture::new();
    f.moved("ambiguous", "candidate-a");
    f.skill("candidate-b", "ambiguous");
    f.skill("missing", "missing");
    f.track("missing");
    fs::remove_dir_all(f.ws.skill_path("missing")).unwrap();
    f.skill("modified", "before");
    f.track("modified");
    f.skill("modified", "after");
    fs::create_dir_all(f.ws.skill_path("invalid")).unwrap();
    fs::write(f.ws.skill_path("invalid/important.txt"), "keep").unwrap();
    std::os::unix::fs::symlink(f._tmp.path(), f.agent.join("foreign")).unwrap();
    std::os::unix::fs::symlink(f.ws.skill_path("gone"), f.agent.join("broken")).unwrap();
    let report = repair::run(&f.ws, true).unwrap();
    assert_eq!(report.repaired, 0);
    assert!(report.skipped + report.review + report.independent >= 6);
    assert!(f.ws.meta.exists("ambiguous"));
    assert!(f.ws.meta.exists("missing"));
    assert!(matches!(
        f.ws.scan().unwrap().get("modified").unwrap().status,
        skills::reconcile::SkillStatus::Modified
    ));
    assert_eq!(
        fs::read_to_string(f.ws.skill_path("invalid/important.txt")).unwrap(),
        "keep"
    );
    assert!(
        fs::symlink_metadata(f.agent.join("foreign"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(
        fs::symlink_metadata(f.agent.join("broken"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn blocked_migration_does_not_prevent_independent_repairs() {
    let f = Fixture::new();
    f.moved("repos/demo/blocked", "repos/demo/destination");
    f.moved("repos/demo/safe", "repos/demo/moved");
    std::os::unix::fs::symlink(
        f.ws.skill_path("repos/demo/blocked"),
        f.agent.join("blocked"),
    )
    .unwrap();
    fs::write(f.agent.join("destination"), "must survive").unwrap();
    let preview = repair::run(&f.ws, false).unwrap();
    assert_eq!(preview.planned, 1);
    assert!(
        preview
            .items
            .iter()
            .any(|i| i.skill == "repos/demo/blocked" && i.detail.contains("already exists"))
    );
    let applied = repair::run(&f.ws, true).unwrap();
    assert_eq!(applied.repaired, 1);
    assert!(f.ws.meta.exists("repos/demo/blocked"));
    assert_eq!(
        fs::read_to_string(f.agent.join("destination")).unwrap(),
        "must survive"
    );
    assert!(
        fs::symlink_metadata(f.agent.join("blocked"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn deployment_registry_is_not_a_missing_skill_and_repairs_preserve_it() {
    let f = Fixture::new();
    let registry = f.ws.meta.dir.join("deployment-targets.toml");
    let content = "agents = []\n[selections]\n[projects]\n";
    fs::write(&registry, content).unwrap();
    assert!(
        !f.ws
            .meta
            .list_keys()
            .unwrap()
            .contains(&"deployment-targets".into())
    );
    assert!(f.ws.scan().unwrap().get("deployment-targets").is_none());
    assert!(repair::run(&f.ws, true).unwrap().items.is_empty());
    assert_eq!(fs::read_to_string(registry).unwrap(), content);
}

#[test]
fn sync_bound_move_is_reported_without_stranding_its_binding() {
    let f = Fixture::new();
    f.moved("repos/demo/bound", "repos/demo/moved");
    let path = f.ws.meta.dir.join(".sync/settings.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let settings =
        r#"{"remotes":{},"bindings":{"repos/demo/bound":{"remote":"backup","baseline":null}}}"#;
    fs::write(&path, settings).unwrap();
    let report = repair::run(&f.ws, true).unwrap();
    assert_eq!(report.repaired, 0);
    assert!(
        report
            .items
            .iter()
            .any(|i| i.skill == "repos/demo/bound" && i.detail.contains("sync binding"))
    );
    assert!(f.ws.meta.exists("repos/demo/bound"));
    assert!(!f.ws.meta.exists("repos/demo/moved"));
    assert_eq!(fs::read_to_string(path).unwrap(), settings);
}

fn missing_with_source(f: &Fixture, key: &str) -> std::path::PathBuf {
    f.skill(key, key);
    f.track(key);
    edit::tag_add(&f.ws, key, &["preserve".into()]).unwrap();
    let source = f._tmp.path().join(format!("source-{key}"));
    fs::rename(f.ws.skill_path(key), &source).unwrap();
    let mut meta = f.ws.meta.load(key).unwrap().unwrap();
    meta.source = Some(skills::meta::Source::Git {
        url: source.display().to_string(),
        subpath: None,
        branch: None,
        revision: None,
    });
    meta.note = Some("keep my note".into());
    f.ws.meta.save(key, &meta).unwrap();
    source
}

#[test]
fn restore_preserves_metadata_and_repairs_existing_broken_links_without_deleting_sources() {
    let f = Fixture::new();
    let source = missing_with_source(&f, "restore-me");
    let before = fs::read(f.ws.meta.path("restore-me")).unwrap();
    std::os::unix::fs::symlink(f.ws.skill_path("restore-me"), f.agent.join("restore-me")).unwrap();
    let options = repair::Options {
        restore_missing: true,
        clean_links: true,
        ..Default::default()
    };
    let preview = repair::plan(&f.ws, &options).unwrap();
    assert_eq!(preview.planned, 2);
    assert!(!f.ws.skill_path("restore-me").exists());
    assert!(!f.ws.meta.dir.join(".repair-backups").exists());
    let result = repair::apply(&f.ws, &preview).unwrap();
    assert_eq!(result.repaired, 1);
    assert_eq!(result.failed, 0);
    assert!(source.join("SKILL.md").exists());
    assert!(f.agent.join("restore-me/SKILL.md").exists());
    assert_eq!(fs::read(f.ws.meta.path("restore-me")).unwrap(), before);
    assert_eq!(
        fs::read(result.backup.unwrap().join("repos/.root.toml")).unwrap(),
        before
    );
}

#[test]
fn stale_restore_preview_rejects_modified_source_and_preserves_metadata() {
    let f = Fixture::new();
    let source = missing_with_source(&f, "restore-me");
    let preview = repair::plan(
        &f.ws,
        &repair::Options {
            restore_missing: true,
            ..Default::default()
        },
    )
    .unwrap();
    fs::write(source.join("SKILL.md"), "edited after preview").unwrap();
    let result = repair::apply(&f.ws, &preview).unwrap();
    assert_eq!(result.repaired, 0);
    assert_eq!(result.failed, 1);
    assert!(!f.ws.skill_path("restore-me").exists());
    assert!(f.ws.meta.exists("restore-me"));
}

#[test]
fn forgetting_archives_metadata_and_cleans_presets_but_keeps_source() {
    let f = Fixture::new();
    let source = missing_with_source(&f, "obsolete");
    f.ws.presets
        .save(&Preset {
            name: "p".into(),
            skills: vec!["obsolete".into()],
            ..Default::default()
        })
        .unwrap();
    let before = fs::read(f.ws.meta.path("obsolete")).unwrap();
    let options = repair::Options {
        forget_missing: true,
        ..Default::default()
    };
    let preview = repair::plan(&f.ws, &options).unwrap();
    assert!(f.ws.meta.exists("obsolete"));
    let result = repair::apply(&f.ws, &preview).unwrap();
    assert_eq!(result.repaired, 1);
    let backup = result.backup.unwrap();
    assert_eq!(fs::read(backup.join("repos/.root.toml")).unwrap(), before);
    assert!(backup.join("presets/p.toml").exists());
    assert!(f.ws.presets.load("p").unwrap().unwrap().skills.is_empty());
    assert!(!f.ws.meta.exists("obsolete"));
    assert!(source.join("SKILL.md").exists());
}

#[test]
fn explicit_edited_move_into_deep_categories_preserves_baseline_and_references() {
    let f = Fixture::new();
    f.skill("old", "before");
    f.track("old");
    let new = "local/design/documents/pdf";
    fs::create_dir_all(f.ws.skill_path("local/design/documents")).unwrap();
    fs::rename(f.ws.skill_path("old"), f.ws.skill_path(new)).unwrap();
    f.skill(new, "edited after move");
    std::os::unix::fs::symlink(f.ws.skill_path("old"), f.agent.join("old")).unwrap();
    assert!(f.ws.scan().unwrap().get(new).is_some());
    let options = repair::Options {
        moves: [("old".into(), new.into())].into(),
        ..Default::default()
    };
    let result = repair::run_with_options(&f.ws, true, &options).unwrap();
    assert_eq!(result.repaired, 1);
    assert_eq!(result.failed, 0);
    assert!(matches!(
        f.ws.scan().unwrap().get(new).unwrap().status,
        skills::reconcile::SkillStatus::Modified
    ));
    assert!(f.agent.join("design--documents--pdf/SKILL.md").exists());
    assert!(!f.ws.meta.exists("old"));
}

#[test]
fn preview_never_expands_batch_and_rejects_returned_skill_before_forgetting() {
    let f = Fixture::new();
    missing_with_source(&f, "first");
    let options = repair::Options {
        forget_missing: true,
        ..Default::default()
    };
    let preview = repair::plan(&f.ws, &options).unwrap();
    missing_with_source(&f, "second");
    f.skill("first", "returned");
    let result = repair::apply(&f.ws, &preview).unwrap();
    assert_eq!(result.failed, 1);
    assert!(f.ws.meta.exists("first"));
    assert!(f.ws.meta.exists("second"));
    assert!(f.ws.skill_path("first/SKILL.md").exists());
}

#[test]
fn clean_links_leaves_foreign_links_and_retargeted_links_untouched() {
    let f = Fixture::new();
    let broken = f.agent.join("broken");
    std::os::unix::fs::symlink(f._tmp.path().join("gone"), &broken).unwrap();
    std::os::unix::fs::symlink(f._tmp.path(), f.agent.join("foreign")).unwrap();
    let options = repair::Options {
        clean_links: true,
        ..Default::default()
    };
    let preview = repair::plan(&f.ws, &options).unwrap();
    fs::remove_file(&broken).unwrap();
    std::os::unix::fs::symlink(f._tmp.path().join("different-gone"), &broken).unwrap();
    let result = repair::apply(&f.ws, &preview).unwrap();
    assert_eq!(result.failed, 1);
    assert!(fs::symlink_metadata(&broken).is_ok());
    assert!(f.agent.join("foreign").exists());
    let result = repair::run_with_options(&f.ws, true, &options).unwrap();
    assert_eq!(result.repaired, 1);
    assert!(fs::symlink_metadata(&broken).is_err());
    assert!(f.agent.join("foreign").exists());
}

#[test]
fn rescan_and_repair_cli_expose_recovery_without_writing_by_default() {
    let f = Fixture::new();
    missing_with_source(&f, "restore-me");
    assert!(f.cli(&["rescan"]).status.success());
    let out = f.cli(&["repair", "--restore-missing"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let preview: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(preview["planned"], 1);
    assert!(!f.ws.skill_path("restore-me").exists());
    assert!(
        !f.cli(&["repair", "--restore-missing", "--forget-missing"])
            .status
            .success()
    );
}

#[test]
fn watcher_detects_deep_moves_but_does_not_poll_repair_backups() {
    let f = Fixture::new();
    f.skill("local/a/b/c/deep", "before");
    let first = skills::reconcile::watch::stamp(&f.ws.root, &f.ws.config).unwrap();
    f.skill("local/a/b/c/deep", "modified content");
    let second = skills::reconcile::watch::stamp(&f.ws.root, &f.ws.config).unwrap();
    assert_ne!(first, second);
    let backup = f.ws.meta.dir.join(".repair-backups/test");
    fs::create_dir_all(&backup).unwrap();
    let before_backup_edit = skills::reconcile::watch::stamp(&f.ws.root, &f.ws.config).unwrap();
    fs::write(backup.join("old.toml"), "archived").unwrap();
    assert_eq!(
        before_backup_edit,
        skills::reconcile::watch::stamp(&f.ws.root, &f.ws.config).unwrap()
    );
}

#[test]
fn restoration_rejects_symlinked_destination_parent() {
    let f = Fixture::new();
    let source = missing_with_source(&f, "source");
    let old = f.ws.meta.load("source").unwrap().unwrap();
    f.ws.meta.remove("source").unwrap();
    f.ws.meta.save("local/category/skill", &old).unwrap();
    fs::create_dir_all(f.ws.skill_path("local")).unwrap();
    std::os::unix::fs::symlink(f._tmp.path(), f.ws.skill_path("local/category")).unwrap();
    let preview = repair::plan(
        &f.ws,
        &repair::Options {
            restore_missing: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(preview.planned, 0);
    assert!(
        preview
            .items
            .iter()
            .any(|i| i.detail.contains("escapes project"))
    );
    assert!(!f._tmp.path().join("skill").exists());
    assert!(source.join("SKILL.md").exists());
}

#[test]
fn independent_installations_and_unconfigured_directories_are_not_faults() {
    let f = Fixture::new();
    fs::create_dir_all(f.agent.join("own-skill")).unwrap();
    std::os::unix::fs::symlink(f._tmp.path(), f.agent.join("external")).unwrap();
    let mut ws = f.ws.clone();
    let missing = f._tmp.path().join("unused-agent");
    ws.config.agents.push(AgentConfig {
        key: "unused".into(),
        name: "Unused".into(),
        skills_dir: missing.display().to_string(),
    });
    let report = repair::run(&ws, false).unwrap();
    assert_eq!(report.planned, 0);
    assert_eq!(report.skipped, 0);
    assert_eq!(report.review, 1);
    assert_eq!(report.independent, 2);
    assert!(report.items.iter().any(|i| i.skill == "unused"
        && i.detail.contains("does not exist:")
        && i.detail.contains(&missing.display().to_string())));
    assert!(
        report
            .lines()
            .iter()
            .any(|s| s.contains("--apply would not change anything"))
    );
    let applied = repair::apply(&ws, &report).unwrap();
    assert_eq!(applied.remaining_issues, Some(0));
    assert_eq!(applied.review, 1);
    assert_eq!(applied.independent, 2);
    assert!(!missing.exists());
}

#[test]
fn startup_migrates_moves_and_archives_missing_metadata_preserving_sync_and_files() {
    let f = Fixture::new();
    f.moved("repos/demo/old", "repos/demo/moved");
    let source = missing_with_source(&f, "missing");
    f.skill("present", "before");
    f.track("present");
    f.skill("present", "edited");
    f.ws.presets
        .save(&Preset {
            name: "p".into(),
            skills: vec!["missing".into(), "repos/demo/old".into()],
            ..Default::default()
        })
        .unwrap();
    let sync_path = f.ws.meta.dir.join(".sync/settings.json");
    fs::create_dir_all(sync_path.parent().unwrap()).unwrap();
    let sync = r#"{"remotes":{},"bindings":{"missing":{"remote":"backup","baseline":null}}}"#;
    fs::write(&sync_path, sync).unwrap();
    std::os::unix::fs::symlink(f.ws.skill_path("missing"), f.agent.join("missing")).unwrap();
    let before = fs::read(f.ws.meta.path("missing")).unwrap();
    let report = repair::startup(&f.ws).unwrap();
    assert_eq!(report.repaired, 2);
    assert_eq!(report.failed, 0);
    assert!(!f.ws.meta.exists("missing"));
    assert!(!f.ws.meta.exists("repos/demo/old"));
    assert!(f.ws.meta.exists("repos/demo/moved"));
    assert!(f.ws.meta.exists("present"));
    assert_eq!(
        f.ws.scan().unwrap().get("present").unwrap().status,
        skills::reconcile::SkillStatus::Modified
    );
    assert!(source.join("SKILL.md").exists());
    assert!(
        fs::symlink_metadata(f.agent.join("missing"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read_to_string(sync_path).unwrap(), sync);
    assert!(f.ws.load_config().unwrap().skill_tags("missing").is_empty());
    assert_eq!(
        f.ws.presets.load("p").unwrap().unwrap().skills,
        ["repos/demo/moved"]
    );
    let backup = report.backup.unwrap();
    assert_eq!(fs::read(backup.join("repos/.root.toml")).unwrap(), before);
    let repeated = repair::startup(&f.ws).unwrap();
    assert_eq!(repeated.repaired, 0);
    assert!(repeated.backup.is_none());
}
