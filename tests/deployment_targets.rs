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
fn external_moves_update_registered_target_references_and_links() {
    let f = Fixture::new("external-move-references");
    let project = f.0.join("project");
    let agent = targets::candidates(&f.ws(), Some(&project))
        .unwrap()
        .remove(0);
    let ws = f.ws();
    skills::ops::edit::tag_add(&ws, "sample", &["keep".into()]).unwrap();
    targets::set_installed(&ws, &agent, Some(&project), &["sample".into()], None, true).unwrap();
    std::fs::rename(ws.root.join("sample"), ws.root.join("moved")).unwrap();
    // The workspace predates registration: migration must reload destinations.
    skills::ops::edit::migrate_meta(&ws, "sample", "moved").unwrap();
    let selection = targets::selection(&ws, &agent).unwrap();
    assert!(selection.manual.contains("moved"));
    assert!(!selection.manual.contains("sample"));
    assert_eq!(
        std::fs::read_link(agent.skills_path().join("moved")).unwrap(),
        ws.root.join("moved")
    );
    assert!(!skills::util::is_symlink(
        &agent.skills_path().join("sample")
    ));
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
        targets::set_installed(
            &ws,
            &claude,
            Some(&project),
            &["sample".into()],
            Some("shared"),
            true,
        )
        .unwrap();
        assert!(destination.join("sample").is_symlink());
        assert!(
            targets::selection(&f.ws(), &codex)
                .unwrap()
                .presets
                .contains_key("shared")
        );
        targets::set_installed(
            &f.ws(),
            &codex,
            Some(&project),
            &["sample".into()],
            Some("shared"),
            false,
        )
        .unwrap();
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
    targets::discover_in(&mut ws, &project, &home).unwrap();
    assert_eq!(targets::visible_agents(&ws).count(), 0);
    assert_eq!(ws.config.agents.len(), 2, "saved destinations are retained");
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
fn mixed_scope_selection_registers_each_project_and_validates_before_writing() {
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
    assert!(
        targets::candidates(&reopened, None)
            .unwrap()
            .iter()
            .any(|a| a.key == home.key)
    );
    assert!(
        !targets::candidates(&reopened, None)
            .unwrap()
            .iter()
            .any(|a| a.key == local.key)
    );
    assert!(
        targets::candidates(&reopened, Some(&project))
            .unwrap()
            .iter()
            .any(|a| a.key == local.key)
    );
}

#[test]
fn mixed_scope_changes_preserve_preset_ownership_and_undo_both_destinations() {
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
    targets::set_installed(&ws, &local, Some(&project), &keys, Some("keep"), true).unwrap();
    assert!(
        targets::apply_scoped(
            &ws,
            &keys,
            &[
                (home.clone(), true, None),
                (local.clone(), false, Some(project.clone()))
            ]
        )
        .is_err()
    );
    assert!(!home.skills_path().exists());
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
        targets::selection(&ws, &home)
            .unwrap()
            .manual
            .contains("sample")
    );
    assert!(
        targets::selection(&ws, &local)
            .unwrap()
            .manual
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
    let selection = targets::selection(&reopened, &local).unwrap();
    assert!(selection.manual.is_empty());
    assert!(selection.presets.contains_key("keep"));
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
