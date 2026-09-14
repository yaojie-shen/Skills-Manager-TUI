use skills::{
    Workspace,
    ops::{
        DownloadDir, git, install,
        sync::{self, Settings},
    },
};
use std::path::Path;
fn workspace(path: &Path) -> Workspace {
    std::fs::create_dir_all(path).unwrap();
    let mut ws = Workspace::open(path).unwrap();
    ws.config.agents.clear();
    ws
}
fn skill(ws: &Workspace, key: &str, text: &str) {
    let path = ws.skill_path(key);
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(
        path.join("SKILL.md"),
        format!("---\nname: test\ndescription: test\n---\n{text}"),
    )
    .unwrap();
}
fn remote(ws: &Workspace, name: &str, path: &Path) {
    git(
        &[
            "init",
            "--bare",
            "--initial-branch=main",
            path.to_str().unwrap(),
        ],
        None,
    )
    .unwrap();
    Settings::load(ws)
        .unwrap()
        .add(ws, name, path.to_str().unwrap(), "main")
        .unwrap();
}
fn run(ws: &Workspace, name: &str, push: bool) -> anyhow::Result<Vec<sync::Change>> {
    sync::run(ws, name, push, &[], false, &mut |_| {})
}
#[test]
fn multiple_remotes_roundtrip_binding_switch_and_conflicts() {
    let tmp = DownloadDir::new("sync-test").unwrap();
    let ws = workspace(&tmp.path().join("one"));
    let public = tmp.path().join("public.git");
    let private = tmp.path().join("private.git");
    remote(&ws, "public", &public);
    remote(&ws, "private", &private);
    skill(&ws, "local/open", "public skill");
    skill(&ws, "local/secret", "private skill");
    skill(&ws, "local/unbound", "never upload");
    let mut settings = Settings::load(&ws).unwrap();
    settings
        .bind(&ws, &["local/open".into()], Some("public"))
        .unwrap();
    settings
        .bind(&ws, &["local/secret".into()], Some("private"))
        .unwrap();
    run(&ws, "public", true).unwrap();
    run(&ws, "private", true).unwrap();
    let tree = git(&["ls-tree", "-r", "--name-only", "main"], Some(&public)).unwrap();
    assert!(tree.contains("skills/local/open/SKILL.md"));
    assert!(
        !tree.contains("secret") && !tree.contains("unbound") && !tree.contains(".skills-meta")
    );
    let two = workspace(&tmp.path().join("two"));
    Settings::load(&two)
        .unwrap()
        .add(&two, "public", public.to_str().unwrap(), "main")
        .unwrap();
    run(&two, "public", false).unwrap();
    assert!(two.skill_path("local/open/SKILL.md").exists());
    assert!(!two.skill_path("local/secret").exists());
    skill(&two, "local/open", "second machine edit");
    run(&two, "public", true).unwrap();
    skill(&ws, "local/open", "first machine conflict");
    assert!(
        run(&ws, "public", false)
            .unwrap_err()
            .to_string()
            .contains("conflict")
    );
    assert!(
        run(&ws, "public", true)
            .unwrap_err()
            .to_string()
            .contains("conflict")
    );
    assert!(
        std::fs::read_to_string(ws.skill_path("local/open/SKILL.md"))
            .unwrap()
            .contains("first machine")
    );
    // Switching a destination preserves source/content and leaves previous remote history.
    Settings::load(&ws)
        .unwrap()
        .bind(&ws, &["local/open".into()], Some("private"))
        .unwrap();
    run(&ws, "private", true).unwrap();
    assert!(
        git(&["show", "main:skills/local/open/SKILL.md"], Some(&public))
            .unwrap()
            .contains("second machine")
    );
    assert!(
        git(&["show", "main:skills/local/open/SKILL.md"], Some(&private))
            .unwrap()
            .contains("first machine")
    );
}
#[test]
fn dry_run_symlinks_unbound_and_remote_races_are_safe() {
    let tmp = DownloadDir::new("sync-safe").unwrap();
    let ws = workspace(&tmp.path().join("root"));
    let bare = tmp.path().join("remote.git");
    remote(&ws, "backup", &bare);
    skill(&ws, "safe", "content");
    assert!(sync::run(&ws, "backup", true, &["safe".into()], false, &mut |_| {}).is_err());
    Settings::load(&ws)
        .unwrap()
        .bind(&ws, &["safe".into()], Some("backup"))
        .unwrap();
    sync::run(&ws, "backup", true, &[], true, &mut |_| {}).unwrap();
    assert!(
        git(&["ls-remote", bare.to_str().unwrap()], None)
            .unwrap()
            .is_empty()
    );
    assert!(
        Settings::load(&ws).unwrap().bindings["safe"]
            .baseline
            .is_none()
    );
    std::os::unix::fs::symlink(tmp.path(), ws.skill_path("safe/escape")).unwrap();
    assert!(run(&ws, "backup", true).is_err());
    std::fs::remove_file(ws.skill_path("safe/escape")).unwrap();
    run(&ws, "backup", true).unwrap();
    assert_eq!(run(&ws, "backup", true).unwrap()[0].action, "unchanged");
}
#[test]
fn imported_git_skill_keeps_source_and_modified_baseline_without_local_paths_or_notes() {
    let tmp = DownloadDir::new("sync-source").unwrap();
    let ws = workspace(&tmp.path().join("root"));
    let source = tmp.path().join("source");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(
        source.join("SKILL.md"),
        "---\nname: example\ndescription: test\n---\noriginal",
    )
    .unwrap();
    git(&["init", "--initial-branch=main"], Some(&source)).unwrap();
    git(&["add", "."], Some(&source)).unwrap();
    git(
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-m",
            "initial",
        ],
        Some(&source),
    )
    .unwrap();
    let key = install::install(
        &ws,
        &install::InstallRef::Git {
            url: source.to_string_lossy().into_owned(),
            branch: Some("main".into()),
            subpath: None,
        },
        Some("example"),
    )
    .unwrap();
    skill(&ws, &key, "local modification");
    let bare = tmp.path().join("remote.git");
    remote(&ws, "backup", &bare);
    Settings::load(&ws)
        .unwrap()
        .bind(&ws, std::slice::from_ref(&key), Some("backup"))
        .unwrap();
    run(&ws, "backup", true).unwrap();
    let two = workspace(&tmp.path().join("two"));
    Settings::load(&two)
        .unwrap()
        .add(&two, "backup", bare.to_str().unwrap(), "main")
        .unwrap();
    run(&two, "backup", false).unwrap();
    assert_eq!(
        two.meta.load(&key).unwrap().unwrap().source,
        ws.meta.load(&key).unwrap().unwrap().source
    );
    assert_eq!(
        two.scan().unwrap().get(&key).unwrap().status,
        skills::reconcile::SkillStatus::Modified
    );
}
#[test]
fn pull_rejects_manifest_traversal_and_symlink_ancestors() {
    let tmp = DownloadDir::new("sync-hostile").unwrap();
    let ws = workspace(&tmp.path().join("root"));
    let bare = tmp.path().join("remote.git");
    remote(&ws, "backup", &bare);
    skill(&ws, "local/example", "content");
    Settings::load(&ws)
        .unwrap()
        .bind(&ws, &["local/example".into()], Some("backup"))
        .unwrap();
    run(&ws, "backup", true).unwrap();
    let two = workspace(&tmp.path().join("two"));
    Settings::load(&two)
        .unwrap()
        .add(&two, "backup", bare.to_str().unwrap(), "main")
        .unwrap();
    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, two.root.join("local")).unwrap();
    assert!(run(&two, "backup", false).is_err());
    assert!(!outside.join("example").exists());
    std::fs::remove_file(two.root.join("local")).unwrap();
    let checkout = tmp.path().join("hostile");
    git(
        &["clone", bare.to_str().unwrap(), checkout.to_str().unwrap()],
        None,
    )
    .unwrap();
    std::fs::write(
        checkout.join(".skills-sync.json"),
        r#"{"skills":{"../outside":{"hash":"bad","source":null}}}"#,
    )
    .unwrap();
    git(&["add", "."], Some(&checkout)).unwrap();
    git(
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-m",
            "hostile",
        ],
        Some(&checkout),
    )
    .unwrap();
    git(&["push"], Some(&checkout)).unwrap();
    assert!(
        run(&two, "backup", false)
            .unwrap_err()
            .to_string()
            .contains("invalid skill")
    );
}
#[test]
fn direct_remote_edits_and_new_skills_pull_without_republishing_unbound_skills() {
    let tmp = DownloadDir::new("sync-direct").unwrap();
    let ws = workspace(&tmp.path().join("root"));
    let bare = tmp.path().join("remote.git");
    remote(&ws, "backup", &bare);
    skill(&ws, "example", "before");
    Settings::load(&ws)
        .unwrap()
        .bind(&ws, &["example".into()], Some("backup"))
        .unwrap();
    run(&ws, "backup", true).unwrap();
    let checkout = tmp.path().join("editor");
    git(
        &["clone", bare.to_str().unwrap(), checkout.to_str().unwrap()],
        None,
    )
    .unwrap();
    std::fs::write(
        checkout.join("skills/example/SKILL.md"),
        "---\nname: example\ndescription: test\n---\nremote edit",
    )
    .unwrap();
    std::fs::create_dir_all(checkout.join("skills/new")).unwrap();
    std::fs::write(
        checkout.join("skills/new/SKILL.md"),
        "---\nname: new\ndescription: test\n---\nnew skill",
    )
    .unwrap();
    git(&["add", "."], Some(&checkout)).unwrap();
    git(
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-m",
            "manual edits",
        ],
        Some(&checkout),
    )
    .unwrap();
    git(&["push"], Some(&checkout)).unwrap();
    run(&ws, "backup", false).unwrap();
    assert!(
        std::fs::read_to_string(ws.skill_path("example/SKILL.md"))
            .unwrap()
            .contains("remote edit")
    );
    assert!(ws.skill_path("new/SKILL.md").is_file());
    Settings::load(&ws)
        .unwrap()
        .bind(&ws, &["example".into()], None)
        .unwrap();
    run(&ws, "backup", false).unwrap();
    assert!(
        !Settings::load(&ws)
            .unwrap()
            .bindings
            .contains_key("example")
    );
}

#[test]
fn cli_keeps_json_clean_and_dry_run_does_not_publish() {
    let tmp = DownloadDir::new("sync-cli").unwrap();
    let ws = workspace(&tmp.path().join("root"));
    let bare = tmp.path().join("remote.git");
    remote(&ws, "backup", &bare);
    skill(&ws, "example", "content");
    let command = |args: &[&str]| {
        std::process::Command::new(env!("CARGO_BIN_EXE_skills"))
            .arg("--root")
            .arg(&ws.root)
            .arg("--json")
            .args(args)
            .output()
            .unwrap()
    };
    let bound = command(&["sync", "bind", "example", "--repo", "backup"]);
    assert!(
        bound.status.success(),
        "{}",
        String::from_utf8_lossy(&bound.stderr)
    );
    let preview = command(&["sync", "push", "--repo", "backup", "--dry-run"]);
    assert!(
        preview.status.success(),
        "{}",
        String::from_utf8_lossy(&preview.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&preview.stdout).unwrap();
    assert_eq!(json[0]["changes"][0]["action"], "push");
    assert!(
        git(&["ls-remote", bare.to_str().unwrap()], None)
            .unwrap()
            .is_empty()
    );
    assert!(!command(&["sync", "push"]).status.success());
    assert!(!command(&["sync", "--dry-run"]).status.success());
}
#[test]
fn switching_back_reuses_the_previous_remote_checkpoint() {
    let tmp = DownloadDir::new("sync-switch-back").unwrap();
    let ws = workspace(&tmp.path().join("root"));
    remote(&ws, "public", &tmp.path().join("public.git"));
    remote(&ws, "private", &tmp.path().join("private.git"));
    skill(&ws, "example", "first");
    Settings::load(&ws)
        .unwrap()
        .bind(&ws, &["example".into()], Some("public"))
        .unwrap();
    run(&ws, "public", true).unwrap();
    Settings::load(&ws)
        .unwrap()
        .bind(&ws, &["example".into()], Some("private"))
        .unwrap();
    skill(&ws, "example", "second");
    run(&ws, "private", true).unwrap();
    Settings::load(&ws)
        .unwrap()
        .bind(&ws, &["example".into()], Some("public"))
        .unwrap();
    run(&ws, "public", true).unwrap();
    assert!(
        git(
            &["show", "main:skills/example/SKILL.md"],
            Some(&tmp.path().join("public.git"))
        )
        .unwrap()
        .contains("second")
    );
}

#[test]
fn archive_backup_preserves_source_and_original_baseline() {
    let tmp = DownloadDir::new("sync-archive").unwrap();
    let ws = workspace(&tmp.path().join("root"));
    let key = "repos/archive/example";
    skill(&ws, key, "original");
    let meta = skills::meta::SkillMeta {
        source: Some(skills::meta::Source::Archive {
            url: "https://example.com/skills.tar.gz".into(),
            subpath: Some("example".into()),
            revision: Some("a".repeat(64)),
        }),
        baseline: Some(skills::meta::Baseline {
            hash: skills::hash::hash_directory(&ws.skill_path(key)).unwrap(),
            hash_algo: skills::hash::HASH_ALGO,
        }),
        ..Default::default()
    };
    ws.meta.save(key, &meta).unwrap();
    skill(&ws, key, "local edits");
    let bare = tmp.path().join("backup.git");
    remote(&ws, "backup", &bare);
    Settings::load(&ws)
        .unwrap()
        .bind(&ws, &[key.into()], Some("backup"))
        .unwrap();
    run(&ws, "backup", true).unwrap();
    let two = workspace(&tmp.path().join("two"));
    Settings::load(&two)
        .unwrap()
        .add(&two, "backup", bare.to_str().unwrap(), "main")
        .unwrap();
    run(&two, "backup", false).unwrap();
    let imported = two.meta.load(key).unwrap().unwrap();
    assert_eq!(imported.source, meta.source);
    assert_eq!(imported.baseline, meta.baseline);
    assert_eq!(
        two.scan().unwrap().get(key).unwrap().status,
        skills::reconcile::SkillStatus::Modified
    );
}
