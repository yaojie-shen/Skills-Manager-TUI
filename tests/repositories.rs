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
        static NEXT_FIXTURE: std::sync::atomic::AtomicUsize =
            std::sync::atomic::AtomicUsize::new(0);
        let id = NEXT_FIXTURE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "skills-repositories-{}-{}-{id}",
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
fn repository_root_skill_uses_declared_name_instead_of_project_name() {
    let f = Fixture::new();
    f.put("", "review-article-architecture", "review");
    f.commit();
    let fetched = f.fetch("Boom5426--Nature-Paper-Skills");
    assert_eq!(fetched.local_name(""), "review-article-architecture");
    let keys = fetched
        .install(&f.ws, &[String::new()], &BTreeMap::new())
        .unwrap();
    fetched.cleanup();
    assert_eq!(
        keys,
        ["repos/Boom5426--Nature-Paper-Skills/review-article-architecture"]
    );
}

#[test]
fn repository_deployment_preserves_skill_name_without_source_prefix() {
    let f = Fixture::new();
    let name = "review-article-architecture";
    f.put(&format!("skills/{name}"), name, "review");
    f.commit();
    let fetched = f.fetch("Boom5426--Nature-Paper-Skills");
    let keys = fetched
        .install(&f.ws, &[format!("skills/{name}")], &BTreeMap::new())
        .unwrap();
    fetched.cleanup();
    let snap = f.ws.scan().unwrap();
    assert_eq!(snap.get(&keys[0]).unwrap().deployment_name(), name);
    let plan = deploy::plan_deploy(&f.ws, &snap, &keys, &["sample".into()]).unwrap();
    deploy::apply(&plan).unwrap();
    assert_eq!(
        std::fs::read_link(f.dir.join("agent").join(name)).unwrap(),
        f.ws.skill_path(&keys[0])
    );
    assert!(
        !f.dir
            .join("agent/Boom5426--Nature-Paper-Skills--review-article-architecture")
            .exists()
    );
    let snap = f.ws.scan().unwrap();
    assert_eq!(
        snap.get(&keys[0]).unwrap().deploy["sample"],
        DeployState::Deployed
    );
    edit::remove(&f.ws, &snap, &keys[0], false).unwrap();
    assert!(!f.dir.join("agent").join(name).is_symlink());
}

#[test]
fn reinstall_same_source_is_a_noop_even_with_another_alias() {
    let f = Fixture::new();
    f.put("some/folder", "review", "review");
    f.commit();
    let first = f.fetch("first");
    let keys = first
        .install(&f.ws, &["some/folder".into()], &BTreeMap::new())
        .unwrap();
    assert_eq!(keys, ["repos/first/review"]);
    let before = f.ws.meta.load(&keys[0]).unwrap();
    let second = f.fetch("second");
    assert!(
        second
            .install(&f.ws, &["some/folder".into()], &BTreeMap::new())
            .unwrap()
            .is_empty()
    );
    assert_eq!(f.ws.meta.load(&keys[0]).unwrap(), before);
    assert!(!f.ws.root.join("repos/second").exists());
    first.cleanup();
    second.cleanup();
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
    assert!(deploy::resolve_names(&snap, &plan, Some("coexist")).is_err());
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
            .unwrap()
            .is_empty()
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
    assert!(deploy::resolve_names(&snap, &plan, Some("coexist")).is_err());
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
        .install(
            &f.ws,
            &["skills/good".into()],
            &BTreeMap::from([("skills/good".into(), "validation-good".into())]),
        )
        .unwrap();
    fetched.cleanup();
}

#[test]
fn local_names_resolve_basename_collisions_deterministically() {
    use skills::repository::resolve_local_names;
    use std::collections::BTreeSet;
    let paths = vec!["z/review".into(), "a/review".into(), "solo/printer".into()];
    let declared = BTreeMap::from([
        ("a/review".into(), "review".into()),
        ("z/review".into(), "review".into()),
        ("solo/printer".into(), "printer".into()),
        ("".into(), "root".into()),
    ]);
    let names = resolve_local_names(&paths, &BTreeMap::new(), &BTreeSet::new(), &declared).unwrap();
    assert_eq!(names["a/review"], "review");
    assert_eq!(names["z/review"], "z--review");
    assert_eq!(names["solo/printer"], "printer");
    let reversed: Vec<_> = paths.iter().rev().cloned().collect();
    assert_eq!(
        names,
        resolve_local_names(&reversed, &BTreeMap::new(), &BTreeSet::new(), &declared).unwrap()
    );
    let occupied = BTreeSet::from(["review".into(), "a--review".into(), "a--review--2".into()]);
    let names = resolve_local_names(&paths, &BTreeMap::new(), &occupied, &declared).unwrap();
    assert_eq!(names["a/review"], "a--review--3");
    assert_eq!(names["z/review"], "z--review");
    let roots = resolve_local_names(
        &["".into()],
        &BTreeMap::new(),
        &BTreeSet::from(["root".into()]),
        &declared,
    )
    .unwrap();
    assert_eq!(roots[""], "root--2");
}

#[test]
fn explicit_local_names_are_reserved_and_never_silently_changed() {
    use skills::repository::resolve_local_names;
    use std::collections::BTreeSet;
    let paths = vec!["a/review".into(), "z/custom".into()];
    let declared = BTreeMap::from([
        ("a/review".into(), "review".into()),
        ("z/custom".into(), "custom".into()),
    ]);
    let overrides = BTreeMap::from([
        ("z/custom".into(), "review".into()),
        ("not-selected".into(), "../ignored".into()),
    ]);
    let names = resolve_local_names(&paths, &overrides, &BTreeSet::new(), &declared).unwrap();
    assert_eq!(names["z/custom"], "review");
    assert_eq!(names["a/review"], "a--review");
    assert!(
        resolve_local_names(
            &paths,
            &overrides,
            &BTreeSet::from(["review".into()]),
            &declared
        )
        .is_err()
    );
    let duplicate = BTreeMap::from([
        ("a/review".into(), "chosen".into()),
        ("z/custom".into(), "chosen".into()),
    ]);
    assert!(resolve_local_names(&paths, &duplicate, &BTreeSet::new(), &declared).is_err());
    let invalid = BTreeMap::from([("a/review".into(), "../escape".into())]);
    assert!(resolve_local_names(&paths, &invalid, &BTreeSet::new(), &declared).is_err());
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

#[test]
fn updates_preserve_local_skill_and_deployment_when_upstream_identity_breaks() {
    for case in ["rename", "move", "delete", "invalid", "illegal-name"] {
        let f = Fixture::new();
        f.put("tools/review", "review", "original");
        f.commit();
        let fetched = f.fetch("fixed-source");
        let keys = fetched
            .install(&f.ws, &["tools/review".into()], &BTreeMap::new())
            .unwrap();
        fetched.cleanup();
        let key = &keys[0];
        let snap = f.ws.scan().unwrap();
        deploy::apply(&deploy::plan_deploy(&f.ws, &snap, &keys, &["sample".into()]).unwrap())
            .unwrap();
        let original = std::fs::read(f.ws.skill_path(key).join("SKILL.md")).unwrap();
        let metadata = std::fs::read(f.ws.meta.path(key)).unwrap();
        let link = f.ws.config.agents[0].skills_path().join("review");
        let target = std::fs::read_link(&link).unwrap();
        match case {
            "rename" => f.put("tools/review", "audit", "new"),
            "move" => {
                std::fs::rename(f.repo.join("tools/review"), f.repo.join("tools/moved")).unwrap()
            }
            "delete" => std::fs::remove_dir_all(f.repo.join("tools/review")).unwrap(),
            "invalid" => {
                std::fs::write(f.repo.join("tools/review/SKILL.md"), "not a skill").unwrap()
            }
            "illegal-name" => f.put("tools/review", "bad/name", "new"),
            _ => unreachable!(),
        }
        f.commit();
        let error = update::prepare(&f.ws, &f.ws.scan().unwrap(), key).unwrap_err();
        assert!(
            format!("{error:#}").contains("keeping local"),
            "{case}: {error:#}"
        );
        assert_eq!(
            std::fs::read(f.ws.skill_path(key).join("SKILL.md")).unwrap(),
            original
        );
        assert_eq!(std::fs::read(f.ws.meta.path(key)).unwrap(), metadata);
        assert_eq!(std::fs::read_link(&link).unwrap(), target);
        assert!(link.join("SKILL.md").is_file());
    }
}

#[test]
fn update_rechecks_local_state_and_prepared_name_before_writing() {
    let f = Fixture::new();
    f.put("tools/review", "review", "original");
    f.commit();
    let fetched = f.fetch("fixed-source");
    let keys = fetched
        .install(&f.ws, &["tools/review".into()], &BTreeMap::new())
        .unwrap();
    fetched.cleanup();
    let key = &keys[0];
    f.put("tools/review", "review", "new");
    f.commit();
    let prepared = update::prepare(&f.ws, &f.ws.scan().unwrap(), key).unwrap();
    let document = f.ws.skill_path(key).join("SKILL.md");
    let original = std::fs::read(&document).unwrap();
    std::fs::write(&document, "---\nname: review\n---\nuser edit").unwrap();
    assert!(update::apply(&f.ws, &prepared, update::Take::Upstream, &BTreeMap::new()).is_err());
    assert!(
        std::fs::read_to_string(&document)
            .unwrap()
            .contains("user edit")
    );
    std::fs::write(&document, &original).unwrap();
    std::fs::write(
        prepared.upstream_dir.join("SKILL.md"),
        "---\nname: renamed\n---\nchanged",
    )
    .unwrap();
    assert!(update::apply(&f.ws, &prepared, update::Take::Upstream, &BTreeMap::new()).is_err());
    assert_eq!(std::fs::read(&document).unwrap(), original);
    prepared.cleanup();
}

#[test]
fn declared_names_aliases_and_invalid_names_are_handled_before_install() {
    let f = Fixture::new();
    f.put("z/unrelated-folder", "review", "second");
    f.put("a/another-folder", "review", "first");
    for (path, name) in [
        ("invalid-space", "Bad Name"),
        ("invalid-path", "../escape"),
        ("invalid-empty", ""),
        ("invalid-hyphen", "bad--name"),
    ] {
        f.put(path, name, "invalid");
    }
    f.commit();
    let fetched = f.fetch("names");
    assert_eq!(fetched.invalid.len(), 4);
    let paths = vec!["z/unrelated-folder".into(), "a/another-folder".into()];
    let preview = fetched
        .resolved_names(&f.ws, &paths, &BTreeMap::new())
        .unwrap();
    assert_eq!(preview["a/another-folder"], "review");
    let mut notices = Vec::new();
    let keys = fetched
        .install_with_progress(&f.ws, &paths, &BTreeMap::new(), &mut |s| {
            notices.push(s.to_string())
        })
        .unwrap();
    assert!(notices.iter().any(|s| s.starts_with("Warning:")));
    for (path, key) in paths.iter().zip(&keys) {
        let doc = skills::skill::SkillDoc::load(&f.ws.skill_path(key)).unwrap();
        assert_eq!(doc.name, "review");
        let meta = f.ws.meta.load(key).unwrap().unwrap();
        assert_eq!(meta.installed_name.as_deref(), Some("review"));
        assert!(
            matches!(meta.source, Some(skills::meta::Source::Git { subpath: Some(p), .. }) if &p == path)
        );
    }
    assert!(
        fetched
            .install(&f.ws, &["invalid-path".into()], &BTreeMap::new())
            .is_err()
    );
    fetched.cleanup();
}

#[test]
fn same_folder_different_sources_require_choice_and_track_actual_link() {
    let f = Fixture::new();
    f.put("first", "review", "first");
    f.put("second", "audit", "second");
    f.commit();
    let first = f.fetch("one");
    let a = first
        .install(&f.ws, &["first".into()], &BTreeMap::new())
        .unwrap()
        .remove(0);
    let second = f.fetch("two");
    let b = second
        .install(
            &f.ws,
            &["second".into()],
            &BTreeMap::from([("second".into(), "review".into())]),
        )
        .unwrap()
        .remove(0);
    let snap = f.ws.scan().unwrap();
    let both =
        deploy::plan_deploy(&f.ws, &snap, &[a.clone(), b.clone()], &["sample".into()]).unwrap();
    assert!(deploy::apply(&both).is_err());
    assert!(!f.dir.join("agent").exists());
    let only_a =
        deploy::plan_deploy(&f.ws, &snap, std::slice::from_ref(&a), &["sample".into()]).unwrap();
    deploy::apply(&only_a).unwrap();
    let snap = f.ws.scan().unwrap();
    assert_eq!(
        snap.get(&a).unwrap().deploy["sample"],
        DeployState::Deployed
    );
    assert_eq!(
        snap.get(&b).unwrap().deploy["sample"],
        DeployState::NotDeployed
    );
    let plan =
        deploy::plan_deploy(&f.ws, &snap, std::slice::from_ref(&b), &["sample".into()]).unwrap();
    let pending = skills::ops::name_choices::Pending::for_actions(&f.ws, &snap, &plan)
        .unwrap()
        .unwrap();
    assert_eq!(pending.groups.len(), 1);
    assert_eq!(pending.groups[0].candidates.len(), 2);
    let choice = pending.groups[0]
        .candidates
        .iter()
        .position(|c| c.key.as_ref() == Some(&b))
        .unwrap();
    pending.apply(&f.ws, &[Some(choice)]).unwrap();
    assert_eq!(
        std::fs::read_link(f.dir.join("agent/review")).unwrap(),
        f.ws.skill_path(&b)
    );
    let snap = f.ws.scan().unwrap();
    let remove_a =
        deploy::plan_undeploy(&f.ws, &snap, std::slice::from_ref(&a), &["sample".into()]).unwrap();
    deploy::apply(&remove_a).unwrap();
    assert_eq!(
        std::fs::read_link(f.dir.join("agent/review")).unwrap(),
        f.ws.skill_path(&b)
    );
    assert!(f.ws.skill_path(&a).is_dir());
    first.cleanup();
    second.cleanup();
}

#[test]
fn local_name_changes_cannot_redefine_the_update_identity() {
    let f = Fixture::new();
    f.put("folder", "original", "v1");
    f.commit();
    let fetched = f.fetch("identity");
    let key = fetched
        .install(&f.ws, &["folder".into()], &BTreeMap::new())
        .unwrap()
        .remove(0);
    fetched.cleanup();
    std::fs::write(
        f.ws.skill_path(&key).join("SKILL.md"),
        "---\nname: renamed\n---\nlocal",
    )
    .unwrap();
    f.put("folder", "renamed", "v2");
    f.commit();
    let before = f.ws.meta.load(&key).unwrap();
    assert!(update::prepare(&f.ws, &f.ws.scan().unwrap(), &key).is_err());
    assert_eq!(f.ws.meta.load(&key).unwrap(), before);
}

#[test]
fn cross_scope_duplicate_is_a_warning_and_new_upstream_skills_are_not_installed() {
    let f = Fixture::new();
    f.put("original-folder", "review", "v1");
    f.commit();
    let fetched = f.fetch("scopes");
    let keys = fetched
        .install(&f.ws, &["original-folder".into()], &BTreeMap::new())
        .unwrap();
    fetched.cleanup();
    skills::ops::targets::set_installed(&f.ws, &f.ws.config.agents[0], None, &keys, None, true)
        .unwrap();
    let project = f.dir.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let project = project.canonicalize().unwrap();
    let local = AgentConfig {
        key: "sample-local".into(),
        name: "Sample local".into(),
        skills_dir: project.join(".agents/skills").display().to_string(),
    };
    let (message, _) =
        skills::ops::targets::set_installed(&f.ws, &local, Some(&project), &keys, None, true)
            .unwrap();
    assert!(message.contains("warning:"), "{message}");
    assert!(local.skills_path().join("review").is_symlink());
    assert!(f.dir.join("agent/review").is_symlink());
    f.put("original-folder", "review", "v2");
    f.put("new-folder", "new-skill", "new");
    f.commit();
    let prepared = update::prepare(&f.ws, &f.ws.scan().unwrap(), &keys[0]).unwrap();
    assert_eq!(prepared.new_skills, ["new-folder"]);
    update::apply(&f.ws, &prepared, update::Take::Upstream, &BTreeMap::new()).unwrap();
    assert_eq!(f.ws.scan().unwrap().skills.len(), 1);
    for path in [
        local.skills_path().join("review"),
        f.dir.join("agent/review"),
    ] {
        assert_eq!(
            skills::skill::SkillDoc::load(&path).unwrap().body.trim(),
            "v2"
        );
    }
}
