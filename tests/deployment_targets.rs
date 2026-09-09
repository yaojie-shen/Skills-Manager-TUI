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
fn selecting_local_targets_does_not_write_until_apply_and_survives_reload() {
    let f = Fixture::new("local");
    let ws = f.ws();
    let project = f.0.join("project");
    let agents = targets::candidates(&ws, Some(&project)).unwrap();
    assert_eq!(agents.len(), 19);
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
    assert!(reopened.config.agent(&selected.key).is_some());
    assert!(
        reopened
            .scan()
            .unwrap()
            .get("sample")
            .unwrap()
            .deployed_to()
            .contains(&selected.key.as_str())
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
fn local_store_can_explicitly_register_a_global_destination_without_changing_source() {
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
    assert!(reopened.config.agent(&agent.key).is_some());
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
fn sync_does_not_expand_a_picker_selection_to_every_skill() {
    let f = Fixture::new("sync");
    let ws = f.ws();
    std::fs::create_dir_all(ws.root.join("other")).unwrap();
    std::fs::write(
        ws.root.join("other/SKILL.md"),
        "---\nname: other\ndescription: other\n---\n",
    )
    .unwrap();
    let project = f.0.join("project");
    let agent = targets::candidates(&ws, Some(&project))
        .unwrap()
        .into_iter()
        .find(|a| a.name.starts_with("Cursor"))
        .unwrap();
    targets::apply(
        &ws,
        &["sample".into()],
        &[(agent.clone(), true)],
        Some(&project),
    )
    .unwrap();
    let ws = f.ws();
    let plan = skills::ops::deploy::plan_sync(&ws, &ws.scan().unwrap()).unwrap();
    assert!(plan.iter().all(|a| !a.is_change()));
    assert!(!agent.skills_path().join("other").exists());
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
fn target_presets_preserve_other_reasons_and_undo_restores_the_selection() {
    let f = Fixture::new("reasons");
    let ws = f.ws();
    let project = f.0.join("project");
    let agent = targets::candidates(&ws, Some(&project)).unwrap().remove(0);
    let keys = vec!["sample".into()];
    targets::set_installed(&ws, &agent, Some(&project), &keys, Some("first"), true).unwrap();
    targets::set_installed(&ws, &agent, Some(&project), &keys, Some("second"), true).unwrap();
    targets::set_installed(&ws, &agent, Some(&project), &keys, Some("first"), false).unwrap();
    assert!(agent.skills_path().join("sample").is_symlink());
    assert!(targets::set_installed(&ws, &agent, Some(&project), &keys, None, false).is_err());
    let (_, intent) =
        targets::set_installed(&ws, &agent, Some(&project), &keys, Some("second"), false).unwrap();
    assert!(!agent.skills_path().join("sample").exists());
    let ws = f.ws();
    let plan = skills::history::undo_plan(&ws, &ws.scan().unwrap(), &intent.unwrap()).unwrap();
    let skills::history::Plan::Write { apply, .. } = plan else {
        panic!("selection undo must restore metadata and links")
    };
    apply.apply(&ws).unwrap();
    assert!(agent.skills_path().join("sample").is_symlink());
    assert!(
        targets::selection(&ws, &agent)
            .unwrap()
            .presets
            .contains_key("second")
    );
    targets::set_installed(&ws, &agent, Some(&project), &keys, None, true).unwrap();
    targets::set_installed(&ws, &agent, Some(&project), &keys, Some("second"), false).unwrap();
    assert!(
        agent.skills_path().join("sample").exists(),
        "manual install survives preset removal"
    );
    assert!(ws.root.join("sample/SKILL.md").is_file());
}

#[test]
fn lost_links_are_restored_without_expanding_or_crossing_scopes() {
    let f = Fixture::new("desired-selection");
    let ws = f.ws();
    let project = f.0.join("project");
    let agent = targets::candidates(&ws, Some(&project)).unwrap().remove(0);
    targets::set_installed(&ws, &agent, Some(&project), &["sample".into()], None, true).unwrap();
    std::fs::remove_file(agent.skills_path().join("sample")).unwrap();
    let ws = f.ws();
    let actions = skills::ops::deploy::plan_sync(&ws, &ws.scan().unwrap()).unwrap();
    assert!(actions.iter().any(|a| matches!(a, skills::ops::deploy::Action::Link { agent: key, skill, .. } if key == &agent.key && skill == "sample")));
    skills::ops::deploy::apply(&actions).unwrap();
    assert!(agent.skills_path().join("sample").is_symlink());
    targets::set_installed(&ws, &agent, Some(&project), &["sample".into()], None, false).unwrap();
    let ws = f.ws();
    assert!(
        !skills::ops::deploy::plan_sync(&ws, &ws.scan().unwrap())
            .unwrap()
            .iter()
            .any(skills::ops::deploy::Action::is_change)
    );
    assert!(!project.join(".codex").exists());
}

#[test]
fn foreign_target_is_rejected_without_recording_a_successful_install() {
    let f = Fixture::new("foreign-selection");
    let ws = f.ws();
    let project = f.0.join("project");
    let agent = targets::candidates(&ws, Some(&project)).unwrap().remove(0);
    std::fs::create_dir_all(agent.skills_path().join("sample")).unwrap();
    std::fs::write(agent.skills_path().join("sample/keep"), "own content").unwrap();
    assert!(
        targets::set_installed(&ws, &agent, Some(&project), &["sample".into()], None, true)
            .is_err()
    );
    assert!(targets::selection(&ws, &agent).unwrap().skills().is_empty());
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
fn central_renames_and_removals_update_target_installation_references() {
    let f = Fixture::new("rename-references");
    let project = f.0.join("project");
    let agent = targets::candidates(&f.ws(), Some(&project))
        .unwrap()
        .remove(0);
    targets::set_installed(
        &f.ws(),
        &agent,
        Some(&project),
        &["sample".into()],
        None,
        true,
    )
    .unwrap();
    let ws = f.ws();
    skills::ops::edit::rename(&ws, &ws.scan().unwrap(), "sample", "renamed").unwrap();
    let selection = targets::selection(&ws, &agent).unwrap();
    assert!(selection.manual.contains("renamed"));
    assert!(!selection.manual.contains("sample"));
    skills::ops::edit::remove(&ws, &ws.scan().unwrap(), "renamed", false).unwrap();
    assert!(targets::selection(&ws, &agent).unwrap().skills().is_empty());
}

#[test]
fn moving_a_library_and_its_project_keeps_relative_target_identity() {
    let f = Fixture::new("portable");
    let project = f.0.join("project");
    let agent = targets::candidates(&f.ws(), Some(&project))
        .unwrap()
        .remove(0);
    targets::set_installed(
        &f.ws(),
        &agent,
        Some(&project),
        &["sample".into()],
        None,
        true,
    )
    .unwrap();
    let registry =
        std::fs::read_to_string(f.ws().root.join(".skills-meta/deployment-targets.toml")).unwrap();
    assert!(!registry.contains(&f.0.display().to_string()));
    let moved = f.0.join("moved");
    std::fs::create_dir(&moved).unwrap();
    std::fs::rename(f.0.join("source"), moved.join("source")).unwrap();
    std::fs::rename(&project, moved.join("project")).unwrap();
    let ws = Workspace::open(&moved.join("source")).unwrap();
    let candidate = targets::candidates(&ws, Some(&moved.join("project")))
        .unwrap()
        .into_iter()
        .find(|a| a.key == agent.key)
        .unwrap();
    assert_eq!(
        candidate.skills_path(),
        moved
            .join("project")
            .join(agent.skills_path().strip_prefix(project).unwrap())
    );
    assert!(
        targets::selection(&ws, &candidate)
            .unwrap()
            .manual
            .contains("sample")
    );
    let actions = skills::ops::deploy::plan_sync(&ws, &ws.scan().unwrap()).unwrap();
    skills::ops::deploy::apply(&actions).unwrap();
    assert!(candidate.skills_path().join("sample/SKILL.md").is_file());
}

#[test]
fn shared_directory_readers_share_installation_reasons() {
    let f = Fixture::new("shared-reasons");
    let project = f.0.join("project");
    let candidates = targets::candidates(&f.ws(), Some(&project)).unwrap();
    let readers: Vec<_> = candidates
        .into_iter()
        .filter(|a| a.skills_path() == project.join(".agents/skills"))
        .collect();
    assert!(readers.len() > 1);
    let keys = ["sample".into()];
    targets::set_installed(
        &f.ws(),
        &readers[0],
        Some(&project),
        &keys,
        Some("shared"),
        true,
    )
    .unwrap();
    assert!(
        targets::selection(&f.ws(), &readers[1])
            .unwrap()
            .presets
            .contains_key("shared")
    );
    targets::set_installed(
        &f.ws(),
        &readers[1],
        Some(&project),
        &keys,
        Some("shared"),
        false,
    )
    .unwrap();
    assert!(
        targets::selection(&f.ws(), &readers[0])
            .unwrap()
            .skills()
            .is_empty()
    );
    assert!(
        !skills::ops::deploy::plan_sync(&f.ws(), &f.ws().scan().unwrap())
            .unwrap()
            .iter()
            .any(skills::ops::deploy::Action::is_change)
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
    assert!(
        targets::selection_from_snapshot(&ws, &agent, &scoped)
            .unwrap()
            .manual
            .contains("sample")
    );
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
fn repeating_an_install_does_not_rewrite_unchanged_selection_metadata() {
    use std::os::unix::fs::MetadataExt;
    let f = Fixture::new("unchanged-selection");
    let ws = f.ws();
    let project = f.0.join("project");
    let agent = targets::candidates(&ws, Some(&project)).unwrap().remove(0);
    let keys = ["sample".into()];
    targets::set_installed(
        &ws,
        &agent,
        Some(&project),
        &keys,
        Some("sample-preset"),
        true,
    )
    .unwrap();
    let path = ws.root.join(".skills-meta/deployment-targets.toml");
    let before = std::fs::metadata(&path).unwrap().ino();
    let (_, intent) = targets::set_installed(
        &ws,
        &agent,
        Some(&project),
        &keys,
        Some("sample-preset"),
        true,
    )
    .unwrap();
    assert!(intent.is_none());
    assert_eq!(std::fs::metadata(path).unwrap().ino(), before);
    assert!(agent.skills_path().join("sample").is_symlink());
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
    assert_eq!(local.len(), 1);
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
