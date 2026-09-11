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
        Self(path.canonicalize().unwrap())
    }
    fn cli(&self, args: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_skills"))
            .current_dir(&self.0)
            .env("HOME", self.0.join("fake-home"))
            .env("SKILLS_HOME", self.0.join("wrong-global-root"))
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
fn local_cli_installs_into_standard_directory_and_deploys_to_project_agents() {
    let f = Fixture::new("cli");
    skill(&f.0.join("source"));
    success(f.cli(&["--local", "init"]));
    success(f.cli(&["--local", "agents", "add", "cursor"]));
    success(f.cli(&[
        "--local", "install", "./source", "--name", "sample", "--deploy", "cursor", "--deploy",
        "codex",
    ]));
    assert!(f.0.join(".agents/skills/sample/SKILL.md").is_file());
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
    assert!(f.0.join(".agents/skills/sample/SKILL.md").is_file());
    assert!(
        !f.cli(&["--local", "--root", f.0.to_str().unwrap(), "status"])
            .status
            .success()
    );
}

#[test]
fn shared_root_is_deployed_and_cannot_be_converted_relinked_or_pruned() {
    let f = Fixture::new("shared");
    skill(&f.0.join(".agents/skills/sample"));
    let mut ws = Workspace::open_local(&f.0, false).unwrap();
    ws.config.agents.retain(|a| a.key == "codex");
    ws.config.deploy.all_to_all = false;
    let snap = ws.scan().unwrap();
    assert_eq!(snap.agent("codex").unwrap().mode, AgentDirMode::SharedRoot);
    assert_eq!(snap.get("sample").unwrap().deployed_to(), vec!["codex"]);
    assert!(deploy::plan_convert(&ws, &snap, "codex").is_err());
    assert!(
        deploy::plan_relink(&ws, &snap, "codex", &[])
            .unwrap()
            .iter()
            .all(|a| !a.is_change())
    );
    assert!(
        deploy::plan_sync(&ws, &snap)
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
    deploy::apply(&plan).unwrap();
    let snap = ws.scan().unwrap();
    assert_eq!(snap.skills.len(), 1);
    assert!(snap.get(key).unwrap().deployed_to().contains(&"codex"));
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
    assert!(deploy::apply(&conflict).is_err());
    std::fs::write(
        ws.root.join(key).join("SKILL.md"),
        "---\nname: repo-sample\n---\nbody",
    )
    .unwrap();
    deploy::apply(
        &deploy::plan_deploy(&ws, &ws.scan().unwrap(), &[key.into()], &["codex".into()]).unwrap(),
    )
    .unwrap();
    let alias = ws.root.join(skills::repository::default_deploy_name(key));
    assert!(alias.exists());
    skills::ops::edit::remove(&ws, &ws.scan().unwrap(), key, false).unwrap();
    assert!(std::fs::symlink_metadata(alias).is_err());
    assert!(ws.root.join("renamed/SKILL.md").is_file());
}

#[test]
fn shared_readers_sync_union_of_presets_and_undeploy_only_once() {
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
    ws.config.deploy.all_to_all = false;
    ws.config.deploy.presets = vec!["only-gemini-cli".into()];
    let key = "repos/example/sample";
    skill(&ws.root.join(key));
    ws.presets
        .save(&skills::preset::Preset {
            name: "only-gemini-cli".into(),
            description: None,
            skills: vec![key.into()],
            agents: vec!["gemini-cli".into()],
        })
        .unwrap();
    let plan = deploy::plan_sync(&ws, &ws.scan().unwrap()).unwrap();
    assert_eq!(
        plan.iter()
            .filter(|a| matches!(a, deploy::Action::Link { .. }))
            .count(),
        1
    );
    deploy::apply(&plan).unwrap();
    let snap = ws.scan().unwrap();
    assert!(snap.get(key).unwrap().deployed_to().contains(&"codex"));
    assert!(snap.get(key).unwrap().deployed_to().contains(&"gemini-cli"));
    let plan = deploy::plan_undeploy(
        &ws,
        &snap,
        &[key.into()],
        &["codex".into(), "gemini-cli".into()],
    )
    .unwrap();
    assert_eq!(plan.iter().filter(|a| a.is_change()).count(), 1);
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
    std::fs::write(
        Config::path(&root),
        "# keep my comment\n[deploy]\nall_to_all = false\n",
    )
    .unwrap();
    Config::add_agent(&root, &skills::agents::BUILTINS[2].config(false), false).unwrap();
    let text = std::fs::read_to_string(Config::path(&root)).unwrap();
    assert!(text.starts_with("# keep my comment\n"));
    let cfg = Config::load(&root).unwrap();
    assert!(!cfg.deploy.all_to_all);
    assert_eq!(cfg.agent("claude").unwrap().skills_dir, "~/.claude/skills");
    assert_eq!(cfg.agent("cursor").unwrap().skills_dir, "~/.cursor/skills");
}

#[test]
fn git_repository_install_deploys_into_local_shared_root() {
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
    let ws = Workspace::open_local(&f.0, false).unwrap();
    let snap = ws.scan().unwrap();
    assert_eq!(snap.skills.len(), 1);
    assert!(snap.skills[0].key.starts_with("repos/"));
    assert!(snap.skills[0].deployed_to().contains(&"codex"));
    assert!(
        std::fs::symlink_metadata(ws.root.join(snap.skills[0].deployment_name()))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}
