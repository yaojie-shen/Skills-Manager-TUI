use skills::{
    Workspace,
    config::{AgentConfig, Config},
    ops::targets,
};
use std::path::PathBuf;

struct Fixture(PathBuf);
impl Fixture {
    fn new(name: &str) -> Self {
        let p = std::env::temp_dir().join(format!("skills-targets-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(p.join("source/sample")).unwrap();
        std::fs::create_dir_all(p.join("project")).unwrap();
        std::fs::write(
            p.join("source/sample/SKILL.md"),
            "---\nname: sample\ndescription: example\n---\ncontent\n",
        )
        .unwrap();
        Config {
            agents: vec![],
            ..Default::default()
        }
        .save(&p.join("source"))
        .unwrap();
        Self(p.canonicalize().unwrap())
    }
    fn ws(&self) -> Workspace {
        Workspace::open(&self.0.join("source")).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn local_deployment_is_rediscovered_without_registering_the_target() {
    let f = Fixture::new("local");
    let ws = f.ws();
    let project = f.0.join("project");
    let agents = targets::candidates(&ws, Some(&project)).unwrap();
    assert_eq!(agents.len(), skills::agents::BUILTINS.len());
    assert_eq!(std::fs::read_dir(&project).unwrap().count(), 0);
    let selected = agents
        .into_iter()
        .find(|a| a.name.starts_with("Cursor"))
        .unwrap();
    let (message, intent) = targets::apply(
        &ws,
        &["sample".into()],
        &[(selected.clone(), true)],
        Some(&project),
    )
    .unwrap();
    assert!(message.contains("added"));
    assert!(intent.is_some());
    let reopened = f.ws();
    assert!(reopened.config.agent(&selected.key).is_none());
    assert!(
        targets::scan_deployed(&reopened, &selected)
            .unwrap()
            .contains("sample")
    );
    assert!(
        targets::candidates(&reopened, None)
            .unwrap()
            .iter()
            .all(|a| a.key != selected.key)
    );
    targets::apply(
        &reopened,
        &["sample".into()],
        &[(selected.clone(), false)],
        Some(&project),
    )
    .unwrap();
    assert!(std::fs::symlink_metadata(selected.skills_path().join("sample")).is_err());
    assert!(ws.root.join("sample/SKILL.md").is_file());
}

#[test]
fn local_store_can_deploy_to_an_explicit_global_destination_without_registering_it() {
    let f = Fixture::new("global");
    let project = f.0.join("project");
    let ws = Workspace::open_local(&project, true).unwrap();
    std::fs::create_dir_all(ws.root.join("sample")).unwrap();
    std::fs::copy(
        f.0.join("source/sample/SKILL.md"),
        ws.root.join("sample/SKILL.md"),
    )
    .unwrap();
    let agent = AgentConfig {
        key: "test-global".into(),
        name: "Test global".into(),
        skills_dir: f.0.join("fake-home/agent/skills").display().to_string(),
    };
    targets::apply(&ws, &["sample".into()], &[(agent.clone(), true)], None).unwrap();
    let reopened = Workspace::open_local(&project, false).unwrap();
    assert!(reopened.config.agent(&agent.key).is_none());
    assert_eq!(
        std::fs::read_link(agent.skills_path().join("sample")).unwrap(),
        ws.root.join("sample")
    );
    assert!(ws.root.join("sample/SKILL.md").is_file());
}

#[test]
fn shared_agents_are_linked_once_and_conflicting_choices_are_rejected() {
    let f = Fixture::new("shared");
    let ws = f.ws();
    let project = f.0.join("project");
    let agents = targets::candidates(&ws, Some(&project)).unwrap();
    let shared: Vec<_> = agents
        .into_iter()
        .filter(|a| a.skills_path() == project.join(".agents/skills"))
        .collect();
    assert!(shared.len() > 1);
    assert!(
        targets::apply(
            &ws,
            &["sample".into()],
            &[(shared[0].clone(), true), (shared[1].clone(), false)],
            Some(&project)
        )
        .is_err()
    );
    assert!(!project.join(".agents").exists());
    let on: Vec<_> = shared.iter().cloned().map(|a| (a, true)).collect();
    targets::apply(&ws, &["sample".into()], &on, Some(&project)).unwrap();
    let off: Vec<_> = shared.into_iter().map(|a| (a, false)).collect();
    targets::apply(&f.ws(), &["sample".into()], &off, Some(&project)).unwrap();
    assert!(std::fs::symlink_metadata(project.join(".agents/skills/sample")).is_err());
}

#[test]
fn escaping_local_paths_and_foreign_content_are_preserved() {
    let f = Fixture::new("safety");
    let ws = f.ws();
    let project = f.0.join("project");
    let target = AgentConfig {
        key: "custom".into(),
        name: "Custom".into(),
        skills_dir: project.join("escape/skills").display().to_string(),
    };
    std::os::unix::fs::symlink(f.0.join("source"), project.join("escape")).unwrap();
    assert!(targets::apply(&ws, &["sample".into()], &[(target, true)], Some(&project)).is_err());
    let target = AgentConfig {
        key: "owned".into(),
        name: "Owned".into(),
        skills_dir: project.join("owned").display().to_string(),
    };
    std::fs::create_dir_all(target.skills_path().join("sample")).unwrap();
    std::fs::write(target.skills_path().join("sample/keep"), "owned").unwrap();
    assert!(
        targets::apply(
            &ws,
            &["sample".into()],
            &[(target.clone(), true)],
            Some(&project),
        )
        .is_err()
    );
    assert_eq!(
        std::fs::read_to_string(target.skills_path().join("sample/keep")).unwrap(),
        "owned"
    );
}

#[test]
fn old_registry_and_sync_config_have_no_effect() {
    let f = Fixture::new("legacy-registry");
    let ws = f.ws();
    let path = ws.root.join(".skills-meta/deployment-targets.toml");
    let legacy = "this is deliberately not valid TOML";
    std::fs::write(&path, legacy).unwrap();
    let config_path = Config::path(&ws.root);
    let mut config_text = std::fs::read_to_string(&config_path).unwrap();
    config_text.push_str("\n[deploy]\nall_to_all = true\npresets = [\"missing-preset\"]\n");
    std::fs::write(&config_path, &config_text).unwrap();
    let project = f.0.join("project");
    let reopened = f.ws();
    let target = targets::candidates(&reopened, Some(&project))
        .unwrap()
        .remove(0);
    targets::set_deployed(&reopened, &target, Some(&project), &["sample".into()], true).unwrap();
    assert_eq!(std::fs::read_to_string(path).unwrap(), legacy);
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_skills"))
        .args(["--root", ws.root.to_str().unwrap(), "sync", "--dry-run"])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("choose a sync subcommand"));
    assert!(target.skills_path().join("sample").is_symlink());
    assert_eq!(std::fs::read_to_string(config_path).unwrap(), config_text);
}

#[test]
fn reviewed_local_actions_can_be_undone_without_a_registered_agent() {
    use skills::{history, ops::deploy};
    let f = Fixture::new("scoped-actions-undo");
    let ws = f.ws();
    let project = f.0.join("project");
    let target = targets::candidates(&ws, Some(&project)).unwrap().remove(0);
    let mut scoped = ws.clone();
    scoped.config.agents = vec![target.clone()];
    let actions = deploy::plan_deploy(
        &scoped,
        &scoped.scan_for_links().unwrap(),
        &["sample".into()],
        std::slice::from_ref(&target.key),
    )
    .unwrap();
    let (_, intent) = targets::apply_actions(&ws, &target, Some(&project), &actions).unwrap();
    assert!(ws.config.agent(&target.key).is_none());
    let history::Plan::Write { apply, .. } =
        history::undo_plan(&ws, &ws.scan().unwrap(), &intent.unwrap()).unwrap()
    else {
        panic!("expected scoped undo")
    };
    apply.apply(&ws).unwrap();
    assert!(!target.skills_path().join("sample").exists());
    // Cleaning an unknown broken link must not promise to restore it as a new link.
    std::os::unix::fs::symlink(f.0.join("gone"), target.skills_path().join("broken")).unwrap();
    let actions =
        deploy::plan_clean(&scoped, &scoped.scan_for_links().unwrap(), &target.key, &[]).unwrap();
    let (_, intent) = targets::apply_actions(&ws, &target, Some(&project), &actions).unwrap();
    assert!(!intent.unwrap().reversible());
    assert!(std::fs::symlink_metadata(target.skills_path().join("broken")).is_err());
}

#[test]
fn installing_into_a_whole_directory_reader_is_a_noop() {
    let f = Fixture::new("whole-directory-noop");
    let ws = f.ws();
    let path = f.0.join("reader");
    std::os::unix::fs::symlink(&ws.root, &path).unwrap();
    let target = AgentConfig {
        key: "reader".into(),
        name: "Reader".into(),
        skills_dir: path.display().to_string(),
    };
    let (_, intent) = targets::set_deployed(&ws, &target, None, &["sample".into()], true).unwrap();
    assert!(intent.is_none());
    assert!(targets::set_deployed(&ws, &target, None, &["sample".into()], false).is_err());
    assert!(ws.skill_path("sample").is_dir());
    assert_eq!(std::fs::read_link(path).unwrap(), ws.root);
}

#[test]
fn discovered_scopes_use_only_the_launch_directory_without_ancestor_scopes_or_writes() {
    let f = Fixture::new("discovery");
    let outer = f.0.join("project");
    let inner = outer.join("apps/web");
    let cwd = inner.join("src");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::create_dir(outer.join(".git")).unwrap();
    std::fs::write(inner.join(".git"), "gitdir: /unused/worktree\n").unwrap();
    let scopes = targets::discover_scopes(&cwd).unwrap();
    assert!(scopes[0].project.is_none());
    assert_eq!(scopes[1].project.as_ref(), Some(&cwd));
    assert!(!scopes[1].repository);
    assert_eq!(scopes.len(), 2);
    let root_scopes = targets::discover_scopes(&inner).unwrap();
    assert_eq!(
        root_scopes
            .iter()
            .filter(|s| s.project.as_ref() == Some(&inner))
            .count(),
        1
    );
    assert!(!cwd.join(".agents").exists());
    assert!(!outer.join(".skills-meta").exists());
}

#[test]
fn overlapping_presets_use_current_members_and_uninstall_without_ownership() {
    use skills::{ops::deploy, preset::Preset};
    let f = Fixture::new("overlapping-presets");
    let mut ws = f.ws();
    let project = f.0.join("project");
    let target = targets::candidates(&ws, Some(&project)).unwrap().remove(0);
    ws.config.agents = vec![target.clone()];
    let first = Preset {
        name: "first".into(),
        skills: vec!["sample".into()],
        ..Default::default()
    };
    let second = Preset {
        name: "second".into(),
        ..first.clone()
    };
    for preset in [&first, &second] {
        ws.presets.save(preset).unwrap();
    }
    targets::set_deployed(&ws, &target, Some(&project), &first.skills, true).unwrap();
    assert_eq!(
        deploy::preset_status(
            &ws.scan().unwrap(),
            &second,
            std::slice::from_ref(&target.key)
        )
        .installed,
        1
    );
    let (_, undo) =
        targets::set_deployed(&ws, &target, Some(&project), &second.skills, false).unwrap();
    assert_eq!(
        deploy::preset_status(
            &ws.scan().unwrap(),
            &first,
            std::slice::from_ref(&target.key)
        )
        .installed,
        0
    );
    assert!(!target.skills_path().join("sample").exists());
    let skills::history::Plan::Write { apply, .. } =
        skills::history::undo_plan(&ws, &ws.scan().unwrap(), &undo.unwrap()).unwrap()
    else {
        panic!("undo")
    };
    apply.apply(&ws).unwrap();
    assert!(target.skills_path().join("sample").is_symlink());
    ws.presets
        .save(&Preset {
            skills: vec![],
            ..second
        })
        .unwrap();
    let current = ws.presets.load("second").unwrap().unwrap();
    targets::set_deployed(&ws, &target, Some(&project), &current.skills, false).unwrap();
    assert!(target.skills_path().join("sample").is_symlink());
    assert!(
        !ws.root
            .join(".skills-meta/deployment-targets.toml")
            .exists()
    );
}

#[test]
fn deleted_links_stay_deleted_until_explicitly_deployed_again() {
    let f = Fixture::new("deleted-link");
    let ws = f.ws();
    let project = f.0.join("project");
    let target = targets::candidates(&ws, Some(&project)).unwrap().remove(0);
    targets::set_deployed(&ws, &target, Some(&project), &["sample".into()], true).unwrap();
    std::fs::remove_file(target.skills_path().join("sample")).unwrap();
    assert!(targets::scan_deployed(&f.ws(), &target).unwrap().is_empty());
    assert!(!target.skills_path().join("sample").exists());
    targets::set_deployed(&f.ws(), &target, Some(&project), &["sample".into()], true).unwrap();
    assert!(target.skills_path().join("sample").is_symlink());
}

#[test]
fn foreign_target_is_rejected_without_recording_a_successful_install() {
    let f = Fixture::new("foreign-selection");
    let ws = f.ws();
    let project = f.0.join("project");
    let agent = targets::candidates(&ws, Some(&project)).unwrap().remove(0);
    std::fs::create_dir_all(agent.skills_path().join("sample")).unwrap();
    std::fs::write(agent.skills_path().join("sample/keep"), "own content").unwrap();
    assert!(targets::set_deployed(&ws, &agent, Some(&project), &["sample".into()], true).is_err());
    assert!(targets::scan_deployed(&ws, &agent).unwrap().is_empty());
    assert_eq!(
        std::fs::read_to_string(agent.skills_path().join("sample/keep")).unwrap(),
        "own content"
    );
    assert!(
        !ws.root
            .join(".skills-meta/deployment-targets.toml")
            .exists()
    );
}

#[test]
fn central_edits_only_update_explicitly_scanned_targets() {
    let f = Fixture::new("rename-references");
    let project = f.0.join("project");
    let agent = targets::candidates(&f.ws(), Some(&project))
        .unwrap()
        .remove(0);
    targets::set_deployed(&f.ws(), &agent, Some(&project), &["sample".into()], true).unwrap();
    let mut ws = f.ws();
    ws.config.agents = vec![agent.clone()];
    skills::ops::edit::rename(&ws, &ws.scan().unwrap(), "sample", "renamed").unwrap();
    let selection = targets::scan_deployed(&ws, &agent).unwrap();
    assert!(selection.contains("renamed"));
    assert!(!selection.contains("sample"));
    skills::ops::edit::remove(&ws, &ws.scan().unwrap(), "renamed", false).unwrap();
    assert!(targets::scan_deployed(&ws, &agent).unwrap().is_empty());
}

#[test]
fn central_edits_do_not_find_or_rewrite_historical_project_links() {
    let f = Fixture::new("historical-target");
    let ws = f.ws();
    let project = f.0.join("project");
    let target = targets::candidates(&ws, Some(&project)).unwrap().remove(0);
    targets::set_deployed(&ws, &target, Some(&project), &["sample".into()], true).unwrap();
    skills::ops::edit::rename(&ws, &ws.scan().unwrap(), "sample", "renamed").unwrap();
    assert_eq!(
        std::fs::read_link(target.skills_path().join("sample")).unwrap(),
        ws.root.join("sample")
    );
    assert!(!target.skills_path().join("renamed").exists());
}

#[test]
fn project_moves_are_rescanned_and_library_moves_are_not_guessed() {
    let f = Fixture::new("move");
    let ws = f.ws();
    let project = f.0.join("project");
    let target = targets::candidates(&ws, Some(&project)).unwrap().remove(0);
    targets::set_deployed(&ws, &target, Some(&project), &["sample".into()], true).unwrap();
    let moved = f.0.join("moved-project");
    std::fs::rename(&project, &moved).unwrap();
    let current = AgentConfig {
        skills_dir: moved
            .join(target.skills_path().strip_prefix(&project).unwrap())
            .display()
            .to_string(),
        ..target
    };
    assert!(
        targets::scan_deployed(&ws, &current)
            .unwrap()
            .contains("sample")
    );
    assert!(!project.exists());
    let new_root = f.0.join("new-library");
    std::fs::rename(&ws.root, &new_root).unwrap();
    let ws = Workspace::open(&new_root).unwrap();
    assert!(targets::scan_deployed(&ws, &current).unwrap().is_empty());
    let old_link = std::fs::read_link(current.skills_path().join("sample")).unwrap();
    assert!(targets::set_deployed(&ws, &current, Some(&moved), &["sample".into()], true).is_err());
    assert_eq!(
        std::fs::read_link(current.skills_path().join("sample")).unwrap(),
        old_link
    );
}

#[test]
fn shared_directory_readers_observe_the_same_physical_state() {
    let f = Fixture::new("shared-state");
    let project = f.0.join("project");
    let readers: Vec<_> = targets::candidates(&f.ws(), Some(&project))
        .unwrap()
        .into_iter()
        .filter(|a| a.skills_path() == project.join(".agents/skills"))
        .collect();
    assert!(readers.len() > 1);
    let keys = ["sample".into()];
    targets::set_deployed(&f.ws(), &readers[0], Some(&project), &keys, true).unwrap();
    assert!(
        targets::scan_deployed(&f.ws(), &readers[1])
            .unwrap()
            .contains("sample")
    );
    targets::set_deployed(&f.ws(), &readers[1], Some(&project), &keys, false).unwrap();
    assert!(
        targets::scan_deployed(&f.ws(), &readers[0])
            .unwrap()
            .is_empty()
    );
}

#[test]
fn scope_refresh_reuses_library_content_and_reads_current_links() {
    let f = Fixture::new("scope-snapshot");
    let ws = f.ws();
    let snapshot = ws.scan().unwrap();
    let project = f.0.join("project");
    let agent = targets::candidates(&ws, Some(&project)).unwrap().remove(0);
    std::fs::create_dir_all(agent.skills_path()).unwrap();
    std::os::unix::fs::symlink(ws.root.join("sample"), agent.skills_path().join("sample")).unwrap();
    std::fs::write(
        ws.root.join("sample/SKILL.md"),
        "---\nname: changed\ndescription: changed\n---\nnew body\n",
    )
    .unwrap();
    let scoped = skills::reconcile::rescope(&snapshot, std::slice::from_ref(&agent)).unwrap();
    assert_eq!(
        scoped.get("sample").unwrap().name.as_deref(),
        Some("sample"),
        "scope changes retain the existing library snapshot"
    );
    assert_eq!(
        scoped.get("sample").unwrap().deploy[&agent.key],
        skills::reconcile::DeployState::Deployed
    );
    assert!(targets::deployed_in(&scoped, &agent).contains("sample"));
    assert!(
        !ws.root
            .join(".skills-meta/deployment-targets.toml")
            .exists()
    );
    std::fs::remove_file(agent.skills_path().join("sample")).unwrap();
    let next = skills::reconcile::rescope(&snapshot, std::slice::from_ref(&agent)).unwrap();
    assert_eq!(
        next.get("sample").unwrap().deploy[&agent.key],
        skills::reconcile::DeployState::NotDeployed
    );
    assert_eq!(
        ws.scan().unwrap().get("sample").unwrap().name.as_deref(),
        Some("changed"),
        "a full scan still reloads source content"
    );
}

#[test]
fn scope_refresh_compares_shadow_contents_fresh_each_time() {
    let f = Fixture::new("scope-shadow");
    let ws = f.ws();
    let snapshot = ws.scan().unwrap();
    let project = f.0.join("project");
    let agent = targets::candidates(&ws, Some(&project)).unwrap().remove(0);
    let copy = agent.skills_path().join("sample");
    std::fs::create_dir_all(&copy).unwrap();
    std::fs::copy(ws.root.join("sample/SKILL.md"), copy.join("SKILL.md")).unwrap();
    let scoped = skills::reconcile::rescope(&snapshot, std::slice::from_ref(&agent)).unwrap();
    assert_eq!(
        scoped.agent(&agent.key).unwrap().entries["sample"],
        skills::reconcile::EntryState::Shadow { same_content: true }
    );
    std::fs::write(copy.join("SKILL.md"), "different content").unwrap();
    let scoped = skills::reconcile::rescope(&snapshot, std::slice::from_ref(&agent)).unwrap();
    assert_eq!(
        scoped.agent(&agent.key).unwrap().entries["sample"],
        skills::reconcile::EntryState::Shadow {
            same_content: false
        }
    );
}

#[test]
fn repeat_install_is_a_noop_without_creating_metadata() {
    let f = Fixture::new("repeat-install");
    let ws = f.ws();
    let project = f.0.join("project");
    let agent = targets::candidates(&ws, Some(&project)).unwrap().remove(0);
    targets::set_deployed(&ws, &agent, Some(&project), &["sample".into()], true).unwrap();
    let (_, intent) =
        targets::set_deployed(&ws, &agent, Some(&project), &["sample".into()], true).unwrap();
    assert!(intent.is_none());
    assert!(
        !ws.root
            .join(".skills-meta/deployment-targets.toml")
            .exists()
    );
}

#[test]
fn physical_scopes_keep_shared_private_and_current_directory_separate() {
    let f = Fixture::new("physical-scopes");
    let ws = f.ws();
    let cwd = f.0.join("project/nested/work");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::create_dir_all(f.0.join("project/.git")).unwrap();
    let codex = skills::agents::BUILTINS
        .iter()
        .find(|a| a.key == "codex")
        .unwrap()
        .config(false);
    let scopes = targets::locations(&ws, &codex, &cwd).unwrap();
    assert!(scopes.iter().any(|s| s.name() == "Global shared"));
    assert!(scopes.iter().any(|s| s.name() == "Global .codex"));
    let local: Vec<_> = scopes.iter().filter(|s| s.project.is_some()).collect();
    assert_eq!(local.len(), 2);
    assert_eq!(
        local[0].directory.as_ref().unwrap(),
        &cwd.join(".agents/skills")
    );
    assert_eq!(local[0].project.as_ref().unwrap(), &cwd);
    let old = targets::candidates(&ws, Some(&cwd)).unwrap();
    let target = targets::scope_agent(&ws, &codex, local[0]).unwrap();
    assert_eq!(
        target.key,
        old.iter()
            .find(|a| a.key.starts_with("codex-local-"))
            .unwrap()
            .key
    );
    let claude = skills::agents::BUILTINS
        .iter()
        .find(|a| a.key == "claude")
        .unwrap()
        .config(false);
    let scopes = targets::locations(&ws, &claude, &cwd).unwrap();
    assert_eq!(scopes.len(), 2);
    assert!(!scopes.iter().any(|s| s.name().contains("shared")));
    assert_eq!(std::fs::read_dir(&cwd).unwrap().count(), 0);
}

#[test]
fn linked_scopes_merge_in_both_directions_and_share_preset_operations() {
    for reverse in [false, true] {
        let f = Fixture::new(if reverse {
            "linked-reverse"
        } else {
            "linked-forward"
        });
        let ws = f.ws();
        let project = f.0.join("project");
        let shared = project.join(".agents/skills");
        let claude = project.join(".claude/skills");
        let (source, destination, relative) = if reverse {
            (&shared, &claude, "../.claude/skills")
        } else {
            (&claude, &shared, "../.agents/skills")
        };
        std::fs::create_dir_all(destination).unwrap();
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(relative, source).unwrap();
        let definition = |key| {
            skills::agents::BUILTINS
                .iter()
                .find(|a| a.key == key)
                .unwrap()
                .config(false)
        };
        let scopes = targets::locations(&ws, &definition("cursor"), &project).unwrap();
        let merged: Vec<_> = scopes
            .iter()
            .filter(|s| s.project.is_some() && !s.links.is_empty())
            .collect();
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].directory.as_ref(), Some(destination));
        assert_eq!(merged[0].links, vec![(source.clone(), destination.clone())]);
        let target = |key| {
            let agent = definition(key);
            let scope = targets::locations(&ws, &agent, &project)
                .unwrap()
                .into_iter()
                .find(|s| s.project.is_some() && !s.links.is_empty())
                .unwrap();
            targets::scope_agent(&ws, &agent, &scope).unwrap()
        };
        let claude = target("claude");
        let codex = target("codex");
        assert_eq!(claude.skills_path(), codex.skills_path());
        targets::set_deployed(&ws, &claude, Some(&project), &["sample".into()], true).unwrap();
        assert!(destination.join("sample").is_symlink());
        assert!(
            targets::scan_deployed(&f.ws(), &codex)
                .unwrap()
                .contains("sample")
        );
        targets::set_deployed(&f.ws(), &codex, Some(&project), &["sample".into()], false).unwrap();
        assert!(!destination.join("sample").exists());
        assert!(
            source.is_symlink(),
            "the directory link must survive removing skills"
        );
        assert_eq!(std::fs::read_link(source).unwrap(), PathBuf::from(relative));
    }
}

#[test]
fn foreign_broken_and_cyclic_directory_links_are_not_merged() {
    let f = Fixture::new("unrecognized-links");
    let ws = f.ws();
    let project = f.0.join("project");
    for dir in [".claude", ".agents", ".cursor", ".codex"] {
        std::fs::create_dir_all(project.join(dir)).unwrap();
    }
    std::os::unix::fs::symlink(&ws.root, project.join(".claude/skills")).unwrap();
    std::os::unix::fs::symlink("missing", project.join(".agents/skills")).unwrap();
    std::os::unix::fs::symlink("../.codex/skills", project.join(".cursor/skills")).unwrap();
    std::os::unix::fs::symlink("../.cursor/skills", project.join(".codex/skills")).unwrap();
    let agent = skills::agents::BUILTINS
        .iter()
        .find(|a| a.key == "cursor")
        .unwrap()
        .config(false);
    let scopes = targets::locations(&ws, &agent, &project).unwrap();
    let locals: Vec<_> = scopes.iter().filter(|s| s.project.is_some()).collect();
    assert_eq!(locals.len(), 4);
    assert!(locals.iter().all(|s| s.links.is_empty()));
}

#[test]
fn inventory_hides_config_only_products_and_ignores_shared_directories() {
    let f = Fixture::new("installed-only");
    let project = f.0.join("project");
    let home = f.0.join("fake-home");
    std::fs::create_dir_all(home.join(".agents/skills")).unwrap();
    std::fs::create_dir_all(project.join(".agents/skills")).unwrap();
    let mut ws = f.ws();
    ws.config.agents = skills::agents::defaults(false);
    for key in ["shared", "legacy-storage"] {
        ws.config.agents.push(AgentConfig {
            key: key.into(),
            name: key.into(),
            skills_dir: home.join(".agents/skills").display().to_string(),
        });
    }
    targets::discover_in(&mut ws, &project, &home).unwrap();
    assert_eq!(targets::visible_agents(&ws).count(), 0);
    assert_eq!(ws.config.agents.len(), 4, "saved destinations are retained");
    for dir in [
        ".codex",
        ".cursor",
        ".claude",
        ".gemini",
        ".config/opencode",
    ] {
        std::fs::create_dir_all(home.join(dir).join("skills")).unwrap();
        std::fs::create_dir_all(project.join(dir).join("skills")).unwrap();
    }
    targets::discover_in(&mut ws, &project, &home).unwrap();
    assert_eq!(
        targets::visible_agents(&ws).count(),
        0,
        "leftover product directories are not installs"
    );
    use std::os::unix::fs::PermissionsExt;
    let binary = home.join(".local/bin/codex");
    std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
    std::fs::write(&binary, "not executed").unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    targets::discover_in(&mut ws, &project, &home).unwrap();
    assert!(targets::visible_agents(&ws).count() > 0);
    assert!(targets::visible_agents(&ws).all(|a| targets::product_key(a) == "codex"));
    std::fs::remove_file(binary).unwrap();
    targets::discover_in(&mut ws, &project, &home).unwrap();
    assert_eq!(
        targets::visible_agents(&ws).count(),
        0,
        "discovery refreshes evidence"
    );
}

#[test]
fn executable_agent_is_detected_before_any_configuration_directory_exists() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new("executable-detection");
    let bin = f.0.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    for (name, mode) in [("claude", 0o755), ("codex", 0o644)] {
        let path = bin.join(name);
        std::fs::write(&path, "must not execute").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }
    assert_eq!(
        skills::agents::detect_in(&f.0.join("home"), &f.0.join("project"), &[bin]),
        std::collections::BTreeSet::from(["claude".into()])
    );
}

#[test]
fn trae_cli_generations_follow_public_installer_links() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let f = Fixture::new("trae-cli-generations");
    let home = f.0.join("home");
    let project = f.0.join("project");
    let bin = home.join(".local/bin");
    std::fs::create_dir_all(&bin).unwrap();
    for root in [".trae", ".trae-cn", ".traecli", ".agents"] {
        std::fs::create_dir_all(home.join(root).join("skills")).unwrap();
    }
    let detect = || skills::agents::detect_in(&home, &project, &[]);
    assert!(
        detect().is_empty(),
        "shared directories are not installed products"
    );

    let v1 = home.join(".local/share/trae-cli/trae-cli");
    let v2 = home.join(".local/share/traecli/releases/0.204.1-tob/traex");
    for binary in [&v1, &v2] {
        std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
        std::fs::write(binary, "fixture: must never execute").unwrap();
        std::fs::set_permissions(binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    symlink(&v1, bin.join("trae-cli")).unwrap();
    assert!(
        detect().is_empty(),
        "standalone trae-cli is ambiguous with Trae Agent"
    );
    symlink(&v1, bin.join("traecli")).unwrap();
    assert_eq!(
        detect(),
        std::collections::BTreeSet::from(["trae-cli-v1".into()])
    );

    std::fs::remove_file(bin.join("traecli")).unwrap();
    symlink(&v2, bin.join("traex")).unwrap();
    symlink("traex", bin.join("traecli")).unwrap();
    assert_eq!(
        detect(),
        std::collections::BTreeSet::from(["trae-cli".into()])
    );
    std::fs::remove_file(bin.join("traex")).unwrap();
    assert!(
        detect().is_empty(),
        "broken launchers are not installations"
    );

    symlink(&v2, bin.join("traex")).unwrap();
    std::fs::remove_file(bin.join("traecli")).unwrap();
    symlink(&v1, bin.join("traecli")).unwrap();
    assert_eq!(
        detect(),
        std::collections::BTreeSet::from(["trae-cli".into(), "trae-cli-v1".into()])
    );
}

#[test]
fn trae_cli_roots_match_each_generation_and_share_existing_selections() {
    let f = Fixture::new("trae-shared-roots");
    let project = f.0.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let home = f.0.join("home");
    for (key, globals, locals) in [
        (
            "trae-cli",
            vec!["~/.trae/skills", "~/.agents/skills"],
            vec![".agents/skills", ".trae/skills"],
        ),
        (
            "trae-cli-v1",
            vec!["~/.traecli/skills", "~/.trae-cn/skills"],
            vec![".traecli/skills", ".trae/skills"],
        ),
    ] {
        let definition = skills::agents::BUILTINS
            .iter()
            .find(|a| a.key == key)
            .unwrap();
        assert_eq!(definition.search_dirs(false), globals);
        assert_eq!(definition.search_dirs(true), locals);
    }
    let candidates = targets::all_candidates_in(&f.ws(), Some(&project), &home).unwrap();
    let readers: Vec<_> = candidates
        .iter()
        .filter(|a| a.skills_path() == project.join(".trae/skills"))
        .collect();
    assert_eq!(
        readers.len(),
        4,
        "both IDE editions and both CLI generations share this project root"
    );
    targets::set_deployed(
        &f.ws(),
        readers[0],
        Some(&project),
        &["sample".into()],
        true,
    )
    .unwrap();
    for reader in &readers {
        assert!(
            targets::scan_deployed(&f.ws(), reader)
                .unwrap()
                .contains("sample")
        );
    }
    targets::set_deployed(
        &f.ws(),
        readers[3],
        Some(&project),
        &["sample".into()],
        false,
    )
    .unwrap();
    for reader in readers {
        assert!(targets::scan_deployed(&f.ws(), reader).unwrap().is_empty());
    }
}

#[test]
fn standalone_solo_apps_do_not_imply_an_ide_installation() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new("solo-not-ide");
    let home = f.0.join("home");
    for name in ["TRAE SOLO.app", "TRAE SOLO CN.app"] {
        let bundle = home.join("Applications").join(name).join("Contents");
        std::fs::create_dir_all(bundle.join("MacOS")).unwrap();
        std::fs::write(bundle.join("Info.plist"), "fixture").unwrap();
        let binary = bundle.join("MacOS/SOLO");
        std::fs::write(&binary, "must not execute").unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    assert!(skills::agents::detect_in(&home, &f.0.join("project"), &[]).is_empty());
}

#[test]
fn mixed_scope_changes_validate_before_writing_without_registering_destinations() {
    let f = Fixture::new("mixed");
    let ws = f.ws();
    let project = f.0.join("project");
    let home = AgentConfig {
        key: "home".into(),
        name: "Home".into(),
        skills_dir: f.0.join("fake-home/skills").display().to_string(),
    };
    let local = AgentConfig {
        key: "local".into(),
        name: "Local".into(),
        skills_dir: project.join(".claude/skills").display().to_string(),
    };
    assert!(
        targets::apply_scoped(
            &ws,
            &["sample".into()],
            &[
                (home.clone(), true, Some(project.clone())),
                (local.clone(), true, Some(project.clone()))
            ]
        )
        .is_err()
    );
    assert!(!home.skills_path().exists());
    targets::apply_scoped(
        &ws,
        &["sample".into()],
        &[
            (home.clone(), true, None),
            (local.clone(), true, Some(project.clone())),
        ],
    )
    .unwrap();
    let reopened = f.ws();
    assert!(reopened.config.agents.is_empty());
    assert!(
        targets::scan_deployed(&reopened, &home)
            .unwrap()
            .contains("sample")
    );
    assert!(
        targets::scan_deployed(&reopened, &local)
            .unwrap()
            .contains("sample")
    );
    assert!(
        !reopened
            .root
            .join(".skills-meta/deployment-targets.toml")
            .exists()
    );
}

#[test]
fn mixed_scope_undo_preserves_preexisting_links_in_both_destinations() {
    let f = Fixture::new("mixed-reasons-undo");
    let ws = f.ws();
    let home = AgentConfig {
        key: "sample-home".into(),
        name: "Home".into(),
        skills_dir: f.0.join("home/skills").display().to_string(),
    };
    let project = f.0.join("project");
    let local = AgentConfig {
        key: "sample-local".into(),
        name: "Local".into(),
        skills_dir: project.join(".custom/skills").display().to_string(),
    };
    let keys = vec!["sample".into()];
    targets::set_deployed(&ws, &local, Some(&project), &keys, true).unwrap();
    let (_, intent) = targets::apply_scoped(
        &ws,
        &keys,
        &[
            (home.clone(), true, None),
            (local.clone(), true, Some(project.clone())),
        ],
    )
    .unwrap();
    assert!(
        targets::scan_deployed(&ws, &home)
            .unwrap()
            .contains("sample")
    );
    assert!(
        targets::scan_deployed(&ws, &local)
            .unwrap()
            .contains("sample")
    );
    let reopened = f.ws();
    let skills::history::Plan::Write { apply, .. } =
        skills::history::undo_plan(&reopened, &reopened.scan().unwrap(), &intent.unwrap()).unwrap()
    else {
        panic!("expected grouped selection undo")
    };
    apply.apply(&reopened).unwrap();
    assert!(!home.skills_path().join("sample").exists());
    assert!(local.skills_path().join("sample").exists());
    let selection = targets::scan_deployed(&reopened, &local).unwrap();
    assert!(selection.contains("sample"));
}

#[test]
fn detection_requires_valid_app_bundle_or_registered_extension_payload() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new("installed-evidence");
    let home = f.0.join("home");
    let project = f.0.join("project");
    let bundle = home.join("Applications/Trae.app/Contents");
    std::fs::create_dir_all(bundle.join("MacOS")).unwrap();
    std::fs::write(bundle.join("Info.plist"), "fixture").unwrap();
    assert!(skills::agents::detect_in(&home, &project, &[]).is_empty());
    let binary = bundle.join("MacOS/Trae");
    std::fs::write(&binary, "not executed").unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    let extensions = home.join(".vscode/extensions");
    let extension = extensions.join("anthropic.claude-code-1.0.0");
    std::fs::create_dir_all(&extension).unwrap();
    std::fs::write(
        extension.join("package.json"),
        r#"{"publisher":"anthropic","name":"claude-code","main":"extension.js"}"#,
    )
    .unwrap();
    std::fs::write(extension.join("extension.js"), "fixture").unwrap();
    assert_eq!(
        skills::agents::detect_in(&home, &project, &[]),
        std::collections::BTreeSet::from(["trae".into()])
    );
    std::fs::write(extensions.join("extensions.json"), r#"[{"identifier":{"id":"anthropic.claude-code"},"relativeLocation":"anthropic.claude-code-1.0.0"}]"#).unwrap();
    assert_eq!(
        skills::agents::detect_in(&home, &project, &[]),
        std::collections::BTreeSet::from(["claude".into(), "trae".into()])
    );
    std::fs::remove_file(extension.join("extension.js")).unwrap();
    assert!(!skills::agents::detect_in(&home, &project, &[]).contains("claude"));
}

#[test]
fn detection_combines_cli_aliases_system_apps_and_browser_extensions() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new("combined-installations");
    let home = f.0.join("home");
    let project = f.0.join("project");
    let bin = f.0.join("bin");
    let apps = f.0.join("Applications");
    for directory in [&bin, &home.join(".local/bin")] {
        std::fs::create_dir_all(directory).unwrap();
    }
    for path in [bin.join("copilot"), home.join(".local/bin/opencode")] {
        std::fs::write(&path, "fixture, never executed").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let bundle = apps.join("Cursor.app/Contents");
    std::fs::create_dir_all(bundle.join("MacOS")).unwrap();
    std::fs::write(bundle.join("Info.plist"), "fixture").unwrap();
    let binary = bundle.join("MacOS/Cursor");
    std::fs::write(&binary, "fixture, never executed").unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();

    let extensions = home.join(".vscode/extensions");
    let extension = extensions.join("codex-fixture");
    std::fs::create_dir_all(&extension).unwrap();
    std::fs::write(
        extension.join("package.json"),
        r#"{"publisher":"OpenAI","name":"ChatGPT","browser":"browser.js"}"#,
    )
    .unwrap();
    std::fs::write(extension.join("browser.js"), "fixture").unwrap();
    std::fs::write(
        extensions.join("extensions.json"),
        serde_json::json!([
            {"identifier": {"id": "OPENAI.CHATGPT"}, "location": {"path": extension}}
        ])
        .to_string(),
    )
    .unwrap();

    let expected = std::collections::BTreeSet::from([
        "github-copilot".into(),
        "opencode".into(),
        "cursor".into(),
        "codex".into(),
    ]);
    assert_eq!(
        skills::agents::detect_with_applications(
            &home,
            &project,
            std::slice::from_ref(&bin),
            std::slice::from_ref(&apps)
        ),
        expected
    );
    // A registered extension with a different package identity is not evidence.
    std::fs::write(
        extension.join("package.json"),
        r#"{"publisher":"different","name":"ChatGPT","browser":"browser.js"}"#,
    )
    .unwrap();
    assert!(
        !skills::agents::detect_with_applications(&home, &project, &[bin], &[apps])
            .contains("codex")
    );
}

#[test]
fn shared_directory_links_have_identical_base_scan_and_scope_reports() {
    for reverse in [false, true] {
        let f = Fixture::new(if reverse {
            "base-shared-reverse"
        } else {
            "base-shared-forward"
        });
        let mut ws = f.ws();
        let project = f.0.join("project");
        let claude = project.join(".claude/skills");
        let shared = project.join(".agents/skills");
        let (source, target) = if reverse {
            (&shared, &claude)
        } else {
            (&claude, &shared)
        };
        std::fs::create_dir_all(target.join("invalid")).unwrap();
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(target, source).unwrap();
        std::os::unix::fs::symlink(ws.root.join("sample"), target.join("sample")).unwrap();
        std::os::unix::fs::symlink(project.join("absent"), target.join("broken")).unwrap();
        ws.config.agents = vec![
            AgentConfig {
                key: "claude".into(),
                name: "Claude".into(),
                skills_dir: claude.display().to_string(),
            },
            AgentConfig {
                key: "codex".into(),
                name: "Codex".into(),
                skills_dir: shared.display().to_string(),
            },
        ];
        let snap = ws.scan().unwrap();
        for report in &snap.agents {
            assert_eq!(report.mode, skills::reconcile::AgentDirMode::Real);
            assert_eq!(report.documents.len(), 1);
            assert_eq!(
                report.entries.len(),
                3,
                "health still sees invalid and broken entries"
            );
            assert_eq!(
                report.valid_count(|s| matches!(s, skills::reconcile::EntryState::Deployed)),
                1
            );
        }
        assert_eq!(snap.agents[0].entries, snap.agents[1].entries);
        let scoped = skills::reconcile::rescope(&snap, &ws.config.agents).unwrap();
        assert_eq!(scoped.agents[0].entries, snap.agents[0].entries);
    }
}

#[test]
fn name_choices_keep_one_or_none_without_deleting_library_sources() {
    for keep in [Some("first"), Some("second"), None] {
        let f = Fixture::new(&format!("choices-{}", keep.unwrap_or("none")));
        for key in ["first", "second"] {
            std::fs::create_dir_all(f.0.join("source").join(key)).unwrap();
            std::fs::write(
                f.0.join("source").join(key).join("SKILL.md"),
                "---\nname: duplicate\ndescription: example\n---\nbody",
            )
            .unwrap();
        }
        let ws = f.ws();
        let agent = AgentConfig {
            key: "test".into(),
            name: "Test".into(),
            skills_dir: f.0.join("project/.agents/skills").display().to_string(),
        };
        let error = targets::set_deployed(
            &ws,
            &agent,
            None,
            &["first".into(), "second".into(), "sample".into()],
            true,
        )
        .unwrap_err();
        let pending = error
            .downcast_ref::<skills::ops::name_choices::Pending>()
            .unwrap();
        assert!(!agent.skills_path().exists(), "review never writes");
        assert_eq!(pending.groups.len(), 1);
        let choice = keep.map(|key| {
            pending.groups[0]
                .candidates
                .iter()
                .position(|c| c.key.as_deref() == Some(key))
                .unwrap()
        });
        pending.apply(&ws, &[choice]).unwrap();
        for key in ["first", "second"] {
            assert_eq!(agent.skills_path().join(key).exists(), keep == Some(key));
            assert!(ws.root.join(key).join("SKILL.md").exists());
        }
        assert!(
            agent.skills_path().join("sample").exists(),
            "unrelated selection survives"
        );
        let selection = targets::scan_deployed(&ws, &agent).unwrap();
        assert_eq!(selection.len(), 1 + usize::from(keep.is_some()));
    }
}

#[test]
fn name_choices_archive_only_excluded_owned_entries_and_reject_stale_content() {
    for keep_owned in [true, false] {
        let f = Fixture::new(&format!("owned-choice-{keep_owned}"));
        let ws = f.ws();
        let agent = AgentConfig {
            key: "test".into(),
            name: "Test".into(),
            skills_dir: f.0.join("project/.agents/skills").display().to_string(),
        };
        let owned = agent.skills_path().join("own-folder");
        std::fs::create_dir_all(&owned).unwrap();
        let content = "---\nname: sample\ndescription: owned\n---\nprecious content";
        std::fs::write(owned.join("SKILL.md"), content).unwrap();
        let get_pending = || {
            targets::set_deployed(&ws, &agent, None, &["sample".into()], true)
                .unwrap_err()
                .downcast::<skills::ops::name_choices::Pending>()
                .unwrap()
        };
        let stale = get_pending();
        std::fs::write(owned.join("extra"), "changed").unwrap();
        assert!(stale.apply(&ws, &[None]).is_err());
        assert!(owned.join("SKILL.md").exists());
        let pending = get_pending();
        let choice = pending.groups[0]
            .candidates
            .iter()
            .position(|c| c.key.is_none() == keep_owned)
            .unwrap();
        let (message, _) = pending.apply(&ws, &[Some(choice)]).unwrap();
        assert_eq!(owned.exists(), keep_owned);
        assert_eq!(agent.skills_path().join("sample").exists(), !keep_owned);
        if !keep_owned {
            let backup = message
                .split("Archived agent-owned entry to ")
                .last()
                .unwrap();
            assert_eq!(
                std::fs::read_to_string(PathBuf::from(backup).join("SKILL.md")).unwrap(),
                content
            );
        }
    }
}
