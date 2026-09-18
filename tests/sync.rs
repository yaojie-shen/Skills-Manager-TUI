use skills::{
    Workspace,
    ops::{
        DownloadDir, git,
        sync::{self, Mode},
    },
};
use std::{fs, path::Path, process::Command};
fn ws(path: &Path) -> Workspace {
    fs::create_dir_all(path).unwrap();
    Workspace::open(path).unwrap()
}
fn write(ws: &Workspace, name: &str, text: &str) {
    let path = ws.root.join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}
fn run(ws: &Workspace) -> anyhow::Result<sync::Report> {
    sync::run(ws, Mode::Sync, false, &mut |_| {})
}
fn remote(path: &Path) {
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
}
fn configure(ws: &Workspace, path: &Path) {
    sync::configure(ws, path.to_str().unwrap(), "main").unwrap();
}
fn head(ws: &Workspace) -> String {
    git(&["rev-parse", "HEAD"], Some(&ws.root)).unwrap()
}
#[test]
fn whole_root_roundtrip_includes_metadata_deletions_and_merges_independent_edits() {
    let tmp = DownloadDir::new("root-sync").unwrap();
    let repo = tmp.path().join("remote.git");
    remote(&repo);
    let one = ws(&tmp.path().join("one"));
    configure(&one, &repo);
    write(&one, "local/a/SKILL.md", "initial");
    write(&one, ".skills-meta/notes.toml", "note = 'shared'");
    write(
        &one,
        ".skills-meta/presets/sample.toml",
        "name = 'sample'\nskills = []",
    );
    for name in [
        ".skills-meta/.staging/tmp",
        ".skills-meta/.repair-backups/old.toml",
        ".skills-meta/backups/old.toml",
        ".skills-meta/.sync/settings.json",
        ".skills-meta/.metadata.lock",
    ] {
        write(&one, name, "runtime");
    }
    assert!(run(&one).unwrap().committed);
    let tree = git(&["ls-tree", "-r", "--name-only", "HEAD"], Some(&one.root)).unwrap();
    assert!(
        tree.contains("local/a/SKILL.md")
            && tree.contains(".skills-meta/notes.toml")
            && tree.contains(".skills-meta/presets/sample.toml")
    );
    assert!(
        !tree.contains(".staging")
            && !tree.contains(".repair-backups")
            && !tree.contains("backups/")
            && !tree.contains(".sync/")
            && !tree.contains(".lock")
    );
    let before = head(&one);
    assert!(!run(&one).unwrap().committed);
    assert_eq!(before, head(&one));
    let two = ws(&tmp.path().join("two"));
    configure(&two, &repo);
    assert!(run(&two).unwrap().pulled);
    assert_eq!(
        fs::read_to_string(two.root.join(".skills-meta/notes.toml")).unwrap(),
        "note = 'shared'"
    );
    write(&one, "first.txt", "first machine");
    run(&one).unwrap();
    write(&two, "second.txt", "second machine");
    let report = run(&two).unwrap();
    assert!(report.committed && report.pulled && report.pushed);
    run(&one).unwrap();
    assert!(one.root.join("second.txt").exists());
    fs::remove_dir_all(one.root.join("local/a")).unwrap();
    run(&one).unwrap();
    run(&two).unwrap();
    assert!(!two.root.join("local/a").exists());
}
#[test]
fn conflicts_abort_merge_and_retain_local_backup_then_allow_retry() {
    let tmp = DownloadDir::new("root-conflict").unwrap();
    let repo = tmp.path().join("remote.git");
    remote(&repo);
    let one = ws(&tmp.path().join("one"));
    configure(&one, &repo);
    write(&one, "shared", "base\n");
    run(&one).unwrap();
    let two = ws(&tmp.path().join("two"));
    configure(&two, &repo);
    run(&two).unwrap();
    write(&one, "shared", "remote\n");
    run(&one).unwrap();
    write(&two, "shared", "local\n");
    assert!(
        run(&two)
            .unwrap_err()
            .to_string()
            .contains("local backup retained")
    );
    assert_eq!(
        fs::read_to_string(two.root.join("shared")).unwrap(),
        "local\n"
    );
    assert!(!two.root.join(".git/MERGE_HEAD").exists());
    assert!(
        git(&["status", "--porcelain"], Some(&two.root))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        git(&["show", "HEAD:shared"], Some(&repo)).unwrap(),
        "remote\n"
    );
    git(
        &[
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@localhost",
            "-c",
            "commit.gpgSign=false",
            "merge",
            "--no-edit",
            "-X",
            "ours",
            "refs/remotes/origin/skills-root-sync",
        ],
        Some(&two.root),
    )
    .unwrap();
    run(&two).unwrap();
    run(&one).unwrap();
    assert_eq!(
        fs::read_to_string(one.root.join("shared")).unwrap(),
        "local\n"
    );
}
#[test]
fn preview_disable_offline_and_lock_preserve_work() {
    let tmp = DownloadDir::new("root-offline").unwrap();
    let repo = tmp.path().join("remote.git");
    remote(&repo);
    let one = ws(&tmp.path().join("one"));
    configure(&one, &repo);
    write(&one, "file", "first");
    run(&one).unwrap();
    let before = head(&one);
    write(&one, "file", "next");
    let preview = sync::run(&one, Mode::Sync, true, &mut |_| {}).unwrap();
    assert!(preview.preview);
    assert_eq!(before, head(&one));
    sync::disable(&one).unwrap();
    assert!(sync::automatic(&one).unwrap().is_none());
    assert_eq!(before, head(&one));
    configure(&one, &repo);
    fs::write(one.root.join(".git/skills-sync.lock"), "").unwrap();
    assert!(run(&one).is_err());
    fs::remove_file(one.root.join(".git/skills-sync.lock")).unwrap();
    fs::rename(&repo, tmp.path().join("offline.git")).unwrap();
    assert!(run(&one).is_err());
    assert_ne!(before, head(&one));
    assert_eq!(
        git(&["show", "HEAD:file"], Some(&one.root)).unwrap(),
        "next"
    );
    assert!(!one.root.join(".git/skills-sync.lock").exists());
    fs::rename(tmp.path().join("offline.git"), &repo).unwrap();
    run(&one).unwrap();
}
#[test]
fn rejects_nested_repos_wrong_branch_and_unrelated_remote_without_overwrite() {
    let tmp = DownloadDir::new("root-guards").unwrap();
    let repo = tmp.path().join("remote.git");
    remote(&repo);
    let one = ws(&tmp.path().join("one"));
    configure(&one, &repo);
    write(&one, "file", "remote");
    run(&one).unwrap();
    let two = ws(&tmp.path().join("two"));
    write(&two, "file", "local");
    configure(&two, &repo);
    assert!(run(&two).is_err());
    assert_eq!(fs::read_to_string(two.root.join("file")).unwrap(), "local");
    git(&["checkout", "-b", "other"], Some(&one.root)).unwrap();
    assert!(run(&one).is_err());
    git(&["checkout", "main"], Some(&one.root)).unwrap();
    let nested = one.root.join("nested");
    fs::create_dir_all(&nested).unwrap();
    git(&["init"], Some(&nested)).unwrap();
    assert!(run(&one).unwrap_err().to_string().contains("nested Git"));
}
#[test]
fn cli_operations_back_up_automatically_but_preview_is_read_only() {
    let tmp = DownloadDir::new("root-cli").unwrap();
    let repo = tmp.path().join("remote.git");
    remote(&repo);
    let one = ws(&tmp.path().join("one"));
    configure(&one, &repo);
    write(
        &one,
        "a/SKILL.md",
        "---\nname: a\ndescription: sample\n---\nBody",
    );
    one.meta
        .save(
            "a",
            &skills::meta::SkillMeta {
                source: Some(skills::meta::Source::Git {
                    url: "https://example.invalid/skills.git".into(),
                    branch: None,
                    subpath: None,
                    revision: None,
                }),
                ..Default::default()
            },
        )
        .unwrap();
    run(&one).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_skills"))
        .args([
            "--root",
            one.root.to_str().unwrap(),
            "note",
            "set",
            "a",
            "backed up note",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let remote_files = git(&["ls-tree", "-r", "--name-only", "HEAD"], Some(&repo)).unwrap();
    assert!(remote_files.contains(".skills-meta/"));
    assert_eq!(
        head(&one),
        git(&["rev-parse", "HEAD"], Some(&repo)).unwrap()
    );
    let before = head(&one);
    write(&one, "unsaved", "change");
    let output = Command::new(env!("CARGO_BIN_EXE_skills"))
        .args(["--root", one.root.to_str().unwrap(), "repair"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(before, head(&one));
}

#[test]
fn explicit_pull_does_not_push_and_push_does_not_overwrite_remote_history() {
    let tmp = DownloadDir::new("root-direction").unwrap();
    let repo = tmp.path().join("remote.git");
    remote(&repo);
    let one = ws(&tmp.path().join("one"));
    configure(&one, &repo);
    write(&one, "one", "base");
    run(&one).unwrap();
    let two = ws(&tmp.path().join("two"));
    configure(&two, &repo);
    run(&two).unwrap();
    write(&one, "one", "remote");
    run(&one).unwrap();
    let remote_head = git(&["rev-parse", "HEAD"], Some(&repo)).unwrap();
    write(&two, "two", "local");
    assert!(sync::run(&two, Mode::Push, false, &mut |_| {}).is_err());
    assert_eq!(
        git(&["rev-parse", "HEAD"], Some(&repo)).unwrap(),
        remote_head
    );
    let report = sync::run(&two, Mode::Pull, false, &mut |_| {}).unwrap();
    assert!(report.pulled && !report.pushed);
    assert_eq!(
        git(&["rev-parse", "HEAD"], Some(&repo)).unwrap(),
        remote_head
    );
    assert_eq!(fs::read_to_string(two.root.join("one")).unwrap(), "remote");
    run(&two).unwrap();
    assert_eq!(git(&["show", "HEAD:two"], Some(&repo)).unwrap(), "local");
}

#[test]
fn refuses_remote_runtime_files_and_metadata_symlinks_before_checkout() {
    let tmp = DownloadDir::new("root-unsafe").unwrap();
    let repo = tmp.path().join("remote.git");
    remote(&repo);
    let one = ws(&tmp.path().join("one"));
    configure(&one, &repo);
    write(&one, "one", "safe");
    run(&one).unwrap();
    let two = ws(&tmp.path().join("two"));
    configure(&two, &repo);
    run(&two).unwrap();
    write(&one, ".skills-meta/.staging/unwanted", "transient");
    git(
        &["add", "-f", ".skills-meta/.staging/unwanted"],
        Some(&one.root),
    )
    .unwrap();
    git(
        &[
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@localhost",
            "-c",
            "commit.gpgSign=false",
            "commit",
            "-m",
            "bad runtime data",
        ],
        Some(&one.root),
    )
    .unwrap();
    git(&["push", "origin", "main"], Some(&one.root)).unwrap();
    let before = head(&two);
    assert!(run(&two).unwrap_err().to_string().contains("runtime file"));
    assert_eq!(before, head(&two));
    assert!(!two.root.join(".skills-meta/.staging/unwanted").exists());
    git(
        &["rm", "--cached", ".skills-meta/.staging/unwanted"],
        Some(&one.root),
    )
    .unwrap();
    fs::create_dir_all(one.root.join(".skills-meta")).unwrap();
    std::os::unix::fs::symlink("/tmp", one.root.join(".skills-meta/escape")).unwrap();
    git(&["add", ".skills-meta/escape"], Some(&one.root)).unwrap();
    git(
        &[
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@localhost",
            "-c",
            "commit.gpgSign=false",
            "commit",
            "-m",
            "bad metadata symlink",
        ],
        Some(&one.root),
    )
    .unwrap();
    git(&["push", "origin", "main"], Some(&one.root)).unwrap();
    assert!(
        run(&two)
            .unwrap_err()
            .to_string()
            .contains("metadata cannot be a symlink")
    );
    assert_eq!(before, head(&two));
}

#[test]
fn first_pull_preserves_ignored_local_files_and_legacy_backups_require_migration() {
    let tmp = DownloadDir::new("root-bootstrap").unwrap();
    let repo = tmp.path().join("remote.git");
    remote(&repo);
    let one = ws(&tmp.path().join("one"));
    configure(&one, &repo);
    write(&one, "private.txt", "remote contents");
    run(&one).unwrap();
    let two = ws(&tmp.path().join("two"));
    configure(&two, &repo);
    let exclude = two.root.join(".git/info/exclude");
    let mut text = fs::read_to_string(&exclude).unwrap();
    text.push_str("\n/private.txt\n");
    fs::write(exclude, text).unwrap();
    write(&two, "private.txt", "local private contents");
    assert!(run(&two).is_err());
    assert_eq!(
        fs::read_to_string(two.root.join("private.txt")).unwrap(),
        "local private contents"
    );
    write(&one, ".skills-sync.json", "{\"skills\":{}}");
    git(&["add", ".skills-sync.json"], Some(&one.root)).unwrap();
    git(
        &[
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@localhost",
            "-c",
            "commit.gpgSign=false",
            "commit",
            "-m",
            "legacy layout",
        ],
        Some(&one.root),
    )
    .unwrap();
    git(&["push", "origin", "main"], Some(&one.root)).unwrap();
    let three = ws(&tmp.path().join("three"));
    configure(&three, &repo);
    assert!(
        run(&three)
            .unwrap_err()
            .to_string()
            .contains("legacy per-skill backup")
    );
    assert!(!three.root.join(".skills-sync.json").exists());
}
