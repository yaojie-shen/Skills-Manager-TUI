use skills::{
    Workspace,
    config::{AgentConfig, Config},
    ops::{deploy, edit, install, update},
    reconcile::DeployState,
    repository::{FetchedRepository, Repository},
};
use std::{collections::BTreeMap, path::PathBuf, process::Command};

struct Fixture {
    dir: PathBuf,
    ws: Workspace,
    repo: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "skills-repositories-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let root = dir.join("root");
        let repo = dir.join("upstream");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        Config {
            agents: vec![AgentConfig {
                key: "sample".into(),
                name: "Sample".into(),
                skills_dir: dir.join("agent").display().to_string(),
            }],
            ..Default::default()
        }
        .save(&root)
        .unwrap();
        let ws = Workspace::open(&root).unwrap();
        let f = Self { dir, ws, repo };
        f.git(&["init", "-q", "-b", "main"]);
        f
    }
    fn git(&self, args: &[&str]) {
        let o = Command::new("git")
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    }
    fn put(&self, path: &str, name: &str, body: &str) {
        let d = self.repo.join(path);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: Sample skill\n---\n{body}\n"),
        )
        .unwrap();
    }
    fn commit(&self) {
        self.git(&["add", "."]);
        self.git(&[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-qm",
            "Sample change",
        ]);
    }
    fn fetch(&self, alias: &str) -> FetchedRepository {
        FetchedRepository::fetch(
            &self.ws,
            &install::parse_ref(
                &format!("file://{}", self.repo.display()),
                Some("main"),
                None,
            )
            .unwrap(),
            Some(alias),
        )
        .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn repositories_preserve_sources_and_aliases_through_update_deployment_and_undo() {
    let f = Fixture::new();
    f.put("frontend/review", "review", "first");
    f.put("backend/review", "review", "second");
    f.commit();
    let fetched = f.fetch("sample--tools");
    let keys = fetched
        .install(
            &f.ws,
            &["frontend/review".into(), "backend/review".into()],
            &BTreeMap::new(),
        )
        .unwrap();
    fetched.cleanup();
    assert_eq!(
        keys,
        [
            "repos/sample--tools/frontend--review",
            "repos/sample--tools/review"
        ]
    );
    let snap = f.ws.scan().unwrap();
    assert_eq!(snap.skills.len(), 2);
    assert_eq!(f.ws.meta.list_keys().unwrap().len(), 2);
    assert_eq!(Repository::list(&f.ws.root).unwrap().len(), 1);
    let first = &keys[0];
    let second = &keys[1];
    let agent = vec!["sample".into()];
    let plan = deploy::plan_deploy(&f.ws, &snap, std::slice::from_ref(first), &agent).unwrap();
    deploy::apply(&plan).unwrap();
    let snap = f.ws.scan().unwrap();
    assert_eq!(
        snap.get(first).unwrap().deploy["sample"],
        DeployState::Deployed
    );
    let plan = deploy::plan_deploy(&f.ws, &snap, std::slice::from_ref(second), &agent).unwrap();
    assert!(deploy::resolve_names(&snap, &plan, None).is_err());
    assert_eq!(
        deploy::resolve_names(&snap, &plan, Some("coexist")).unwrap(),
        plan
    );
    let replace = deploy::resolve_names(&snap, &plan, Some("replace")).unwrap();
    let intent = skills::history::Intent::from_actions(&replace).unwrap();
    deploy::apply(&replace).unwrap();
    assert!(f.ws.skill_path(first).is_dir());
    assert!(f.ws.skill_path(second).is_dir());
    let snap = f.ws.scan().unwrap();
    assert_eq!(
        snap.get(first).unwrap().deploy["sample"],
        DeployState::NotDeployed
    );
    let skills::history::Plan::Links(undo) =
        skills::history::undo_plan(&f.ws, &snap, &intent).unwrap()
    else {
        panic!("expected link undo")
    };
    deploy::apply(&undo).unwrap();
    assert_eq!(
        f.ws.scan().unwrap().get(first).unwrap().deploy["sample"],
        DeployState::Deployed
    );
    edit::tag_set(&f.ws, first, &["sample".into()]).unwrap();
    f.put("frontend/review", "review", "updated");
    f.commit();
    assert!(update::check(&f.ws, first).unwrap().update_available);
    let prepared = update::prepare(&f.ws, &f.ws.scan().unwrap(), first).unwrap();
    update::apply(&f.ws, &prepared, update::Take::Upstream, &BTreeMap::new()).unwrap();
    let snap = f.ws.scan().unwrap();
    assert!(
        snap.get(first)
            .unwrap()
            .body
            .as_ref()
            .unwrap()
            .contains("updated")
    );
    assert_eq!(snap.get(first).unwrap().tags, ["sample"]);
    assert_eq!(
        snap.get(first).unwrap().deploy["sample"],
        DeployState::Deployed
    );
}

#[test]
fn nested_choices_are_rejected_and_basename_installs_keep_nested_contents() {
    let f = Fixture::new();
    for path in [
        "tools",
        "tools/reader",
        "tools/reader/formatter",
        "other/printer",
        "a/b",
        "a--b",
    ] {
        f.put(path, "sample", "body");
    }
    f.commit();
    let fetched = f.fetch("sample--tools");
    assert!(fetched.choices.contains(&"tools/reader/formatter".into()));
    assert!(
        fetched
            .install(
                &f.ws,
                &["tools".into(), "tools/reader/formatter".into()],
                &BTreeMap::new()
            )
            .is_err()
    );
    assert!(!f.ws.root.join("repos").exists());
    let names = BTreeMap::from([("a--b".into(), "literal-a--b".into())]);
    let keys = fetched
        .install(
            &f.ws,
            &[
                "a/b".into(),
                "a--b".into(),
                "tools/reader".into(),
                "other/printer".into(),
            ],
            &names,
        )
        .unwrap();
    assert_eq!(keys.len(), 4);
    assert!(
        f.ws.skill_path(&keys[2])
            .join("formatter/SKILL.md")
            .is_file()
    );
    assert_eq!(f.ws.scan().unwrap().skills.len(), 4); // nested contents are not scanned twice
    assert!(
        fetched
            .install(&f.ws, &["a/b".into()], &BTreeMap::new())
            .is_err()
    );
    fetched.cleanup();
}

#[test]
fn repo_alias_is_a_label_and_cli_lists_then_installs_selected_paths() {
    let f = Fixture::new();
    f.put("reader", "reader", "body");
    f.put("printer", "printer", "body");
    f.commit();
    let cli = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_skills"))
            .arg("--root")
            .arg(&f.ws.root)
            .arg("--json")
            .args(args)
            .output()
            .unwrap()
    };
    let url = format!("file://{}", f.repo.display());
    let out = cli(&[
        "install",
        &url,
        "--list",
        "--repo-alias",
        "owner--repo--name",
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!f.ws.root.join("repos").exists());
    let out = cli(&[
        "install",
        &url,
        "--select",
        "reader",
        "--repo-alias",
        "owner--repo--name",
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        f.ws.root
            .join("repos/owner--repo--name/reader/SKILL.md")
            .is_file()
    );
    let out = cli(&["list", "repo:owner--repo--name"]);
    assert!(out.status.success());
    let list: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(list.to_string().contains("repos/owner--repo--name/reader"));
    let mut other = f.fetch("owner--repo--name");
    other.repository.url = "https://example.com/different.git".into();
    assert!(
        other
            .install(&f.ws, &["printer".into()], &BTreeMap::new())
            .is_err()
    );
    other.cleanup();
}

#[test]
fn replace_never_deletes_an_agents_own_same_named_directory() {
    let f = Fixture::new();
    f.put("printer", "printer", "body");
    f.commit();
    let fetched = f.fetch("sample--tools");
    let keys = fetched
        .install(&f.ws, &["printer".into()], &BTreeMap::new())
        .unwrap();
    fetched.cleanup();
    let own = f.dir.join("agent/own-printer");
    std::fs::create_dir_all(&own).unwrap();
    std::fs::copy(
        f.ws.skill_path(&keys[0]).join("SKILL.md"),
        own.join("SKILL.md"),
    )
    .unwrap();
    let snap = f.ws.scan().unwrap();
    let plan = deploy::plan_deploy(&f.ws, &snap, &keys, &["sample".into()]).unwrap();
    assert!(deploy::resolve_names(&snap, &plan, Some("replace")).is_err());
    deploy::apply(&deploy::resolve_names(&snap, &plan, Some("coexist")).unwrap()).unwrap();
    assert!(own.is_dir());
}

#[test]
fn repository_work_reports_clone_scan_and_install_stages() {
    let f = Fixture::new();
    f.put("tools/printer", "printer", "body");
    f.commit();
    let reference =
        install::parse_ref(&format!("file://{}", f.repo.display()), None, None).unwrap();
    let mut messages = Vec::new();
    let fetched =
        FetchedRepository::fetch_with_progress(&f.ws, &reference, Some("progress"), &mut |s| {
            messages.push(s.to_string())
        })
        .unwrap();
    assert!(fetched.workdir.starts_with(std::env::temp_dir()));
    assert!(!fetched.workdir.starts_with(&f.ws.root));
    assert!(
        !f.ws.root.join(".skills-meta/.staging").exists(),
        "discovery should not write staging in the managed root"
    );
    assert!(messages.iter().any(|s| s.starts_with("Clone:")));
    assert!(
        messages
            .iter()
            .any(|s| s.starts_with("Scan: found tools/printer"))
    );
    fetched
        .install_with_progress(
            &f.ws,
            &["tools/printer".into()],
            &BTreeMap::new(),
            &mut |s| messages.push(s.to_string()),
        )
        .unwrap();
    assert!(messages.iter().any(|s| s.starts_with("Copy: 1/1")));
    assert!(messages.iter().any(|s| s.starts_with("Save: 1/1")));
    assert!(f.ws.root.join("repos/progress/printer/SKILL.md").is_file());
    fetched.cleanup();
    assert!(!fetched.workdir.exists());
}

#[test]
fn download_workspace_cleans_up_on_error_and_can_transfer_ownership() {
    let path = {
        let dir = skills::ops::DownloadDir::new("test-cleanup").unwrap();
        let path = dir.path().to_path_buf();
        std::fs::write(path.join("partial-download"), "partial").unwrap();
        path
    };
    assert!(!path.exists());
    let dir = skills::ops::DownloadDir::new("test-keep").unwrap();
    let path = dir.keep();
    assert!(path.is_dir());
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn invalid_skill_fixtures_are_reported_with_paths_before_installation() {
    let f = Fixture::new();
    f.put("skills/good", "good", "real skill");
    f.put("tests/bad", "bad", "fixture");
    std::fs::write(
        f.repo.join("tests/bad/SKILL.md"),
        "---\ndescription: missing name\n---\nfixture",
    )
    .unwrap();
    f.commit();
    let fetched = f.fetch("validation");
    assert!(fetched.choices.contains(&"tests/bad".into()));
    assert_eq!(
        fetched.invalid.get("tests/bad").unwrap(),
        "frontmatter has no `name`"
    );
    let error = fetched
        .install(&f.ws, &["tests/bad".into()], &BTreeMap::new())
        .unwrap_err();
    assert!(format!("{error:#}").contains("tests/bad/SKILL.md"));
    assert!(!f.ws.root.join("repos/validation").exists());
    let output = Command::new(env!("CARGO_BIN_EXE_skills"))
        .args(["--root", f.ws.root.to_str().unwrap(), "--json", "install"])
        .arg(format!("file://{}", f.repo.display()))
        .args(["--repo-alias", "cli-valid", "--all"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        result["installed"],
        serde_json::json!(["repos/cli-valid/good"])
    );

    fetched
        .install(&f.ws, &["skills/good".into()], &BTreeMap::new())
        .unwrap();
    fetched.cleanup();
}

#[test]
fn local_names_resolve_basename_collisions_deterministically() {
    use skills::repository::resolve_local_names;
    use std::collections::BTreeSet;
    let paths = vec!["z/review".into(), "a/review".into(), "solo/printer".into()];
    let names = resolve_local_names(&paths, &BTreeMap::new(), &BTreeSet::new(), "root").unwrap();
    assert_eq!(names["a/review"], "review");
    assert_eq!(names["z/review"], "z--review");
    assert_eq!(names["solo/printer"], "printer");
    let reversed: Vec<_> = paths.iter().rev().cloned().collect();
    assert_eq!(
        names,
        resolve_local_names(&reversed, &BTreeMap::new(), &BTreeSet::new(), "root").unwrap()
    );
    let occupied = BTreeSet::from(["review".into(), "a--review".into(), "a--review--2".into()]);
    let names = resolve_local_names(&paths, &BTreeMap::new(), &occupied, "root").unwrap();
    assert_eq!(names["a/review"], "a--review--3");
    assert_eq!(names["z/review"], "z--review");
    let roots = resolve_local_names(
        &["".into()],
        &BTreeMap::new(),
        &BTreeSet::from(["root".into()]),
        "root",
    )
    .unwrap();
    assert_eq!(roots[""], "root--2");
}

#[test]
fn explicit_local_names_are_reserved_and_never_silently_changed() {
    use skills::repository::resolve_local_names;
    use std::collections::BTreeSet;
    let paths = vec!["a/review".into(), "z/custom".into()];
    let overrides = BTreeMap::from([
        ("z/custom".into(), "review".into()),
        ("not-selected".into(), "../ignored".into()),
    ]);
    let names = resolve_local_names(&paths, &overrides, &BTreeSet::new(), "root").unwrap();
    assert_eq!(names["z/custom"], "review");
    assert_eq!(names["a/review"], "a--review");
    assert!(
        resolve_local_names(
            &paths,
            &overrides,
            &BTreeSet::from(["review".into()]),
            "root"
        )
        .is_err()
    );
    let duplicate = BTreeMap::from([
        ("a/review".into(), "chosen".into()),
        ("z/custom".into(), "chosen".into()),
    ]);
    assert!(resolve_local_names(&paths, &duplicate, &BTreeSet::new(), "root").is_err());
    let invalid = BTreeMap::from([("a/review".into(), "../escape".into())]);
    assert!(resolve_local_names(&paths, &invalid, &BTreeSet::new(), "root").is_err());
}

#[test]
fn later_installs_reserve_existing_files_symlinks_and_metadata_names() {
    let f = Fixture::new();
    for path in ["a/review", "b/review", "c/review"] {
        f.put(path, "review", "body");
    }
    f.commit();
    let fetched = f.fetch("reserved");
    let first = fetched
        .install(&f.ws, &["a/review".into()], &BTreeMap::new())
        .unwrap();
    assert_eq!(first, ["repos/reserved/review"]);
    let root = f.ws.root.join("repos/reserved");
    std::fs::write(root.join("b--review"), "keep this file").unwrap();
    std::os::unix::fs::symlink(root.join("missing"), root.join("b--review--2")).unwrap();
    f.ws.meta
        .save("repos/reserved/b--review--3", &Default::default())
        .unwrap();
    let paths = vec!["b/review".into()];
    let names = fetched
        .resolved_names(&f.ws, &paths, &BTreeMap::new())
        .unwrap();
    assert_eq!(names["b/review"], "b--review--4");
    let second = fetched.install(&f.ws, &paths, &BTreeMap::new()).unwrap();
    assert_eq!(second, ["repos/reserved/b--review--4"]);
    assert_eq!(
        std::fs::read_to_string(root.join("b--review")).unwrap(),
        "keep this file"
    );
    assert!(root.join("b--review--2").is_symlink());
    assert!(f.ws.skill_path(&first[0]).join("SKILL.md").is_file());
    let collision = BTreeMap::from([("c/review".into(), "review".into())]);
    assert!(
        fetched
            .install(&f.ws, &["c/review".into()], &collision)
            .is_err()
    );
    fetched.cleanup();
}
