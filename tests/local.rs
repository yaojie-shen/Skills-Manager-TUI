//! Project scope must never fall back to home directories or treat source skills as copies.
use skills::{Workspace, config::Config, ops::deploy, reconcile::AgentDirMode};
use std::{
    path::{Path, PathBuf},
    process::Command,
};

struct Fixture(PathBuf);
impl Fixture {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("skills-local-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        std::fs::create_dir(path.join("central")).unwrap();
        Self(path.canonicalize().unwrap())
    }
    fn cli(&self, args: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_skills"))
            .current_dir(&self.0)
            .env("HOME", self.0.join("fake-home"))
            .env("SKILLS_HOME", self.0.join("central"))
            .args(args)
            .output()
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn skill(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    std::fs::write(
        path.join("SKILL.md"),
        "---\nname: sample\ndescription: Sample skill\n---\nHello\n",
    )
    .unwrap();
}
fn success(output: std::process::Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn project_tags_use_central_library() {
    let f = Fixture::new("configless-tags");
    let root = f.0.join("central");
    skill(&root.join("sample"));
    success(f.cli(&["--local", "tag", "add", "sample", "work"]));
    let local = success(f.cli(&["--local", "--json", "tag", "list"]));
    let global = success(f.cli(&["--root", root.to_str().unwrap(), "--json", "tag", "list"]));
    assert_eq!(local, global);
    assert!(local.contains("work"));
    assert!(!Config::path(&root).exists());
}

#[test]
fn local_cli_installs_centrally_and_deploys_to_project_agents() {
    let f = Fixture::new("cli");
    skill(&f.0.join("source"));
    success(f.cli(&["--local", "init"]));
    success(f.cli(&["--local", "agents", "add", "cursor"]));
    success(f.cli(&[
        "--local", "install", "./source", "--name", "sample", "--deploy", "cursor", "--deploy",
        "codex",
    ]));
    assert!(f.0.join("central/sample/SKILL.md").is_file());
    assert!(
        std::fs::symlink_metadata(f.0.join(".cursor/skills/sample"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(!f.0.join("fake-home").exists());
    assert!(!f.0.join("wrong-global-root").exists());
    let agents = success(f.cli(&["agents", "--local", "--json"]));
    assert!(agents.contains(&f.0.display().to_string()));
    assert!(!agents.contains("~"));
    success(f.cli(&[
        "--project",
        f.0.to_str().unwrap(),
        "undeploy",
        "sample",
        "--agent",
        "codex",
    ]));
    assert!(f.0.join("central/sample/SKILL.md").is_file());
    assert!(!f.0.join(".agents/skills/sample").exists());
    assert!(!f.0.join(".agents/skills/.skills-meta").exists());
    success(f.cli(&[
        "--local",
        "--root",
        f.0.join("central").to_str().unwrap(),
        "status",
    ]));
}

#[test]
fn shared_root_is_read_only_and_cannot_be_relinked_or_pruned() {
    let f = Fixture::new("shared");
    skill(&f.0.join(".agents/skills/sample"));
    let mut ws = Workspace::open_local(&f.0, false).unwrap();
    ws.config.agents.retain(|a| a.key == "codex");
    let snap = ws.scan().unwrap();
    assert!(matches!(
        snap.agent("codex").unwrap().mode,
        AgentDirMode::ReadOnly { .. }
    ));
    assert!(snap.get("sample").unwrap().deployed_to().is_empty());
    assert!(
        deploy::plan_relink(&ws, &snap, "codex", &[])
            .unwrap()
            .iter()
            .all(|a| !a.is_change())
    );
    assert!(
        deploy::plan_undeploy(&ws, &snap, &["sample".into()], &["codex".into()])
            .unwrap()
            .iter()
            .all(|a| !a.is_change())
    );
    assert!(f.0.join(".agents/skills/sample/SKILL.md").is_file());
}

#[test]
fn project_paths_are_stable_on_reload_and_reject_escape() {
    let f = Fixture::new("paths");
    let ws = Workspace::open_local(&f.0, true).unwrap();
    Config::add_agent(&ws.root, &skills::agents::BUILTINS[2].config(true), true).unwrap();
    let loaded = ws.load_config().unwrap();
    assert_eq!(
        loaded.agent("cursor").unwrap().skills_path(),
        f.0.join(".cursor/skills")
    );
    assert_eq!(loaded.agent("codex").unwrap().skills_path(), ws.root);
    let before = std::fs::read(Config::path(&ws.root)).unwrap();
    assert!(Config::add_agent(&ws.root, &skills::agents::BUILTINS[2].config(true), true).is_err());
    assert_eq!(before, std::fs::read(Config::path(&ws.root)).unwrap());
    assert!(
        !f.cli(&["--local", "agents", "add", "outside", "--dir", "../outside"])
            .status
            .success()
    );
    std::os::unix::fs::symlink(f.0.parent().unwrap(), f.0.join("escape")).unwrap();
    assert!(
        !f.cli(&[
            "--local",
            "agents",
            "add",
            "outside",
            "--dir",
            "escape/skills"
        ])
        .status
        .success()
    );
}

#[test]
fn project_root_symlink_cannot_redirect_installation_outside_project() {
    let f = Fixture::new("root-escape");
    std::os::unix::fs::symlink(f.0.parent().unwrap(), f.0.join(".agents")).unwrap();
    assert!(Workspace::open_local(&f.0, true).is_err());
}

#[test]
fn shared_root_repository_alias_is_not_discovered_as_a_duplicate_skill() {
    let f = Fixture::new("repo-alias");
    let ws = Workspace::open_local(&f.0, true).unwrap();
    let key = "repos/example/sample";
    skill(&ws.root.join(key));
    let snap = ws.scan().unwrap();
    let plan = deploy::plan_deploy(&ws, &snap, &[key.into()], &["codex".into()]).unwrap();
    assert!(plan.iter().all(|action| !action.is_change()));
    deploy::apply(&plan).unwrap();
    let snap = ws.scan().unwrap();
    assert_eq!(snap.skills.len(), 1);
    assert!(snap.get(key).unwrap().deployed_to().is_empty());
    let plan = deploy::plan_undeploy(&ws, &snap, &[key.into()], &["codex".into()]).unwrap();
    deploy::apply(&plan).unwrap();
    assert!(ws.root.join(key).join("SKILL.md").is_file());
}

#[test]
fn shared_source_rename_preserves_directory_and_shared_alias_removal_cleans_links() {
    let f = Fixture::new("mutations");
    let ws = Workspace::open_local(&f.0, true).unwrap();
    skill(&ws.root.join("sample"));
    skills::ops::edit::rename(&ws, &ws.scan().unwrap(), "sample", "renamed").unwrap();
    assert!(ws.root.join("renamed/SKILL.md").is_file());
    assert!(
        !std::fs::symlink_metadata(ws.root.join("renamed"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    let key = "repos/example/sample";
    skill(&ws.root.join(key));
    let conflict =
        deploy::plan_deploy(&ws, &ws.scan().unwrap(), &[key.into()], &["codex".into()]).unwrap();
    assert!(conflict.iter().all(|action| !action.is_change()));
    deploy::apply(&conflict).unwrap();
    std::fs::write(
        ws.root.join(key).join("SKILL.md"),
        "---\nname: repo-sample\n---\nbody",
    )
    .unwrap();
    deploy::apply(
        &deploy::plan_deploy(&ws, &ws.scan().unwrap(), &[key.into()], &["codex".into()]).unwrap(),
    )
    .unwrap();
    let alias = ws.root.join("repo-sample");
    assert!(!alias.exists());
    assert!(
        !ws.config.agents[0]
            .skills_path()
            .join("repo-sample")
            .exists()
    );
    skills::ops::edit::remove(&ws, &ws.scan().unwrap(), key, false).unwrap();
    assert!(
        std::fs::symlink_metadata(ws.config.agents[0].skills_path().join("repo-sample")).is_err()
    );
    assert!(ws.root.join("renamed/SKILL.md").is_file());
}

#[test]
fn shared_readers_observe_deployment_and_undeploy_only_once() {
    let f = Fixture::new("shared-readers");
    let mut ws = Workspace::open_local(&f.0, true).unwrap();
    Config::add_agent(
        &ws.root,
        &skills::agents::BUILTINS
            .iter()
            .find(|a| a.key == "gemini-cli")
            .unwrap()
            .config(true),
        true,
    )
    .unwrap();
    ws.config = ws.load_config().unwrap();
    let key = "repos/example/sample";
    skill(&ws.root.join(key));
    ws.presets
        .save(&skills::preset::Preset {
            color: None,
            name: "only-gemini-cli".into(),
            description: None,
            skills: vec![key.into()],
        })
        .unwrap();
    let plan = deploy::plan_deploy(
        &ws,
        &ws.scan().unwrap(),
        &[key.into()],
        &["gemini-cli".into()],
    )
    .unwrap();
    assert!(plan.iter().all(|action| !action.is_change()));
    deploy::apply(&plan).unwrap();
    let snap = ws.scan().unwrap();
    assert!(snap.get(key).unwrap().deployed_to().is_empty());
    let plan = deploy::plan_undeploy(
        &ws,
        &snap,
        &[key.into()],
        &["codex".into(), "gemini-cli".into()],
    )
    .unwrap();
    assert!(plan.iter().all(|action| !action.is_change()));
    deploy::apply(&plan).unwrap();
    assert!(ws.root.join(key).join("SKILL.md").is_file());
}

#[test]
fn catalog_needs_no_root_and_agent_registration_preserves_global_config() {
    let f = Fixture::new("catalog");
    let catalog = success(f.cli(&["agents", "catalog", "--json"]));
    let catalog: serde_json::Value = serde_json::from_str(&catalog).unwrap();
    assert_eq!(
        catalog.as_array().unwrap().len(),
        skills::agents::BUILTINS.len()
    );
    let root = f.0.join("global");
    std::fs::create_dir_all(root.join(".skills-meta")).unwrap();
    std::fs::write(Config::path(&root), "# keep my comment\nschema = 2\n").unwrap();
    Config::add_agent(&root, &skills::agents::BUILTINS[2].config(false), false).unwrap();
    let text = std::fs::read_to_string(Config::path(&root)).unwrap();
    assert!(text.starts_with("# keep my comment\n"));
    let cfg = Config::load(&root).unwrap();
    assert!(!toml::to_string(&cfg).unwrap().contains("all_to_all"));
    assert_eq!(cfg.agent("claude").unwrap().skills_dir, "~/.claude/skills");
    assert_eq!(cfg.agent("cursor").unwrap().skills_dir, "~/.cursor/skills");
}

#[test]
fn git_repository_install_uses_central_library_and_project_links() {
    let f = Fixture::new("git");
    let repo = f.0.join("upstream");
    skill(&repo.join("sample"));
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["add", "."],
        vec![
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-qm",
            "fixture",
        ],
    ] {
        success(
            Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .output()
                .unwrap(),
        );
    }
    let url = format!("file://{}", repo.display());
    success(f.cli(&["--local", "install", &url, "--all", "--deploy", "codex"]));
    let ws = Workspace::open(&f.0.join("central")).unwrap();
    let snap = ws.inventory().unwrap();
    assert_eq!(snap.skills.len(), 1);
    assert!(snap.skills[0].key.starts_with("repos/"));
    let link = f.0.join(".agents/skills/sample");
    assert_eq!(
        link.canonicalize().unwrap(),
        ws.root.join(&snap.skills[0].key)
    );
    assert!(!f.0.join(".agents/skills/.skills-meta").exists());
}

#[test]
fn project_scope_is_transient_and_presets_use_central_members() {
    let f = Fixture::new("central-preset");
    let root = f.0.join("central");
    skill(&root.join("sample"));
    success(f.cli(&["init"]));
    let before = std::fs::read(Config::path(&root)).unwrap();
    success(f.cli(&["--local", "preset", "create", "work", "--skill", "sample"]));
    success(f.cli(&[
        "--local",
        "preset",
        "deploy",
        "work",
        "--agent",
        "cursor",
        "--dry-run",
    ]));
    assert!(!f.0.join(".cursor").exists());
    success(f.cli(&["--local", "preset", "deploy", "work", "--agent", "cursor"]));
    assert_eq!(
        f.0.join(".cursor/skills/sample").canonicalize().unwrap(),
        root.join("sample")
    );
    let by_query = success(f.cli(&["--local", "--json", "list", "agent:cursor"]));
    let by_flag = success(f.cli(&["--local", "--json", "list", "--agent", "cursor"]));
    assert_eq!(by_query, by_flag);
    assert!(by_query.contains("sample"));
    success(f.cli(&["--local", "preset", "undeploy", "work", "--agent", "cursor"]));
    assert!(!f.0.join(".cursor/skills/sample").exists());
    assert_eq!(std::fs::read(Config::path(&root)).unwrap(), before);
    assert!(
        Workspace::open(&root)
            .unwrap()
            .presets
            .load("work")
            .unwrap()
            .is_some()
    );
    assert!(!f.0.join("fake-home").exists());
}

#[test]
fn project_all_agents_uses_configured_products_and_confines_custom_paths() {
    let f = Fixture::new("central-targets");
    let root = f.0.join("central");
    skill(&root.join("sample"));
    success(f.cli(&["init"]));
    success(
        f.cli(&[
            "agents",
            "add",
            "outside",
            "--dir",
            f.0.parent()
                .unwrap()
                .join("outside-agent")
                .to_str()
                .unwrap(),
        ]),
    );
    success(f.cli(&[
        "--local",
        "agents",
        "add",
        "inside",
        "--dir",
        ".custom/skills",
    ]));
    success(f.cli(&["--local", "deploy", "sample", "--all-agents"]));
    for dir in [".claude", ".agents", ".custom"] {
        assert_eq!(
            f.0.join(dir).join("skills/sample").canonicalize().unwrap(),
            root.join("sample")
        );
    }
    assert!(!f.0.join(".cursor").exists());
    let output = f.cli(&["--local", "deploy", "sample", "--agent", "outside"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("no destination in this project"));
    assert!(!f.0.join("fake-home").exists());
}

#[test]
fn project_does_not_fallback_to_legacy_store_when_central_root_is_missing() {
    let f = Fixture::new("no-fallback");
    skill(&f.0.join(".agents/skills/sample"));
    std::fs::remove_dir(f.0.join("central")).unwrap();
    assert!(!f.cli(&["--local", "list"]).status.success());
    success(f.cli(&[
        "--root",
        f.0.join(".agents/skills").to_str().unwrap(),
        "list",
    ]));
}

#[test]
fn explicit_global_builtin_uses_picker_target_without_registration() {
    let f = Fixture::new("global-explicit");
    let root = f.0.join("central");
    skill(&root.join("sample"));
    success(f.cli(&["init"]));
    let before = std::fs::read(Config::path(&root)).unwrap();
    success(f.cli(&["deploy", "sample", "--agent", "cursor"]));
    assert_eq!(
        f.0.join("fake-home/.cursor/skills/sample")
            .canonicalize()
            .unwrap(),
        root.join("sample")
    );
    let rows = success(f.cli(&["--json", "list", "agent:cursor"]));
    assert!(rows.contains("sample"));
    assert!(
        !f.cli(&["deploy", "sample", "--agent", "cursor", "--all-agents"])
            .status
            .success()
    );
    success(f.cli(&["undeploy", "sample", "--agent", "cursor"]));
    assert!(!f.0.join("fake-home/.cursor/skills/sample").exists());
    assert_eq!(std::fs::read(Config::path(&root)).unwrap(), before);
}
