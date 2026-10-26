//! Batch deployment must preserve conflict choices and fresh filesystem checks.

use skills::Workspace;
use skills::config::{AgentConfig, Config};
use skills::ops::{DownloadDir, deploy};
use std::path::{Path, PathBuf};

struct Fixture {
    _temp: DownloadDir,
    ws: Workspace,
    agents: [PathBuf; 2],
}

impl Fixture {
    fn new() -> Self {
        let temp = DownloadDir::new("deploy-batch").unwrap();
        let root = temp.path().join("library");
        let agents = [temp.path().join("a"), temp.path().join("b")];
        std::fs::create_dir(&root).unwrap();
        for agent in &agents {
            std::fs::create_dir(agent).unwrap();
        }
        Config {
            agents: agents
                .iter()
                .zip(["a", "b"])
                .map(|(path, key)| AgentConfig {
                    key: key.into(),
                    name: key.into(),
                    skills_dir: path.display().to_string(),
                })
                .collect(),
            ..Default::default()
        }
        .save(&root)
        .unwrap();
        Self {
            ws: Workspace::open(&root).unwrap(),
            agents,
            _temp: temp,
        }
    }

    fn put(path: &Path, name: &str) {
        std::fs::create_dir_all(path).unwrap();
        std::fs::write(
            path.join("SKILL.md"),
            format!("---\nname: {name}\n---\nExample\n"),
        )
        .unwrap();
    }

    fn skill(&self, key: &str, name: &str) {
        Self::put(&self.ws.root.join(key), name);
    }

    fn link(&self, agent: usize, key: &str, name: &str) -> deploy::Action {
        deploy::Action::Link {
            agent: ["a", "b"][agent].into(),
            skill: key.into(),
            path: self.agents[agent].join(name),
            target: self.ws.root.join(key),
        }
    }
}

#[test]
fn large_batch_preserves_order_and_is_idempotent_across_agents() {
    let f = Fixture::new();
    let keys: Vec<_> = (0..256).rev().map(|i| format!("skill-{i}")).collect();
    for key in &keys {
        f.skill(key, key);
    }
    let snap = f.ws.scan_for_links().unwrap();
    let plan = deploy::plan_deploy(&f.ws, &snap, &keys, &["a".into(), "b".into()]).unwrap();
    assert!(deploy::name_conflicts(&snap, &plan).is_empty());
    let planned: Vec<_> = plan
        .iter()
        .filter_map(|a| match a {
            deploy::Action::Link { skill, .. } => Some(skill.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        planned,
        keys.iter().chain(&keys).cloned().collect::<Vec<_>>()
    );
    assert_eq!(deploy::apply(&plan).unwrap(), keys.len() * 2);
    assert_eq!(deploy::apply(&plan).unwrap(), 0);
    for agent in &f.agents {
        for key in &keys {
            assert_eq!(
                std::fs::read_link(agent.join(key)).unwrap(),
                f.ws.root.join(key)
            );
        }
    }
}

#[test]
fn conflict_candidates_keep_entry_then_action_order_without_duplicates() {
    let f = Fixture::new();
    f.skill("first", "shared");
    f.skill("second", "shared");
    f.skill("third", "different");
    Fixture::put(&f.agents[0].join("a-owned"), "shared");
    Fixture::put(&f.agents[0].join("z-owned"), "shared");
    std::os::unix::fs::symlink(f.ws.root.join("missing"), f.agents[0].join("shared")).unwrap();
    let snap = f.ws.scan_for_links().unwrap();
    let actions = vec![
        f.link(0, "first", "shared"),
        f.link(0, "third", "shared"),
        f.link(1, "first", "shared"),
        f.link(0, "second", "shared"),
    ];
    let conflicts = deploy::name_conflicts(&snap, &actions);
    assert!(conflicts.iter().all(|c| c.agent == "a"));
    let first: Vec<_> = conflicts
        .iter()
        .filter(|c| c.skill == "first")
        .map(|c| {
            (
                c.other_path.file_name().unwrap().to_str().unwrap(),
                c.other_skill.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        first,
        vec![
            ("a-owned", None),
            ("shared", None),
            ("z-owned", None),
            ("shared", Some("third")),
            ("shared", Some("second")),
        ]
    );
    assert!(deploy::apply(&actions).is_err());
    assert!(!f.agents[1].join("shared").exists());
}

#[test]
fn preflight_reads_changed_destination_names_again_before_any_writes() {
    let f = Fixture::new();
    f.skill("first", "first");
    f.skill("second", "second");
    let owned = f.agents[0].join("owned");
    Fixture::put(&owned, "unrelated");
    let actions = vec![f.link(0, "first", "first"), f.link(0, "second", "second")];
    deploy::preflight(&actions).unwrap();
    Fixture::put(&owned, "second");
    assert!(
        deploy::apply(&actions)
            .unwrap_err()
            .to_string()
            .contains("deployment conflict")
    );
    assert!(!f.agents[0].join("first").exists());
    assert!(!f.agents[0].join("second").exists());
    assert!(owned.is_dir());
}

#[test]
fn preflight_reads_changed_source_names_again_and_rejects_invalid_late_sources() {
    let f = Fixture::new();
    f.skill("first", "first");
    f.skill("second", "second");
    Fixture::put(&f.agents[0].join("owned"), "occupied");
    let actions = vec![f.link(0, "first", "first"), f.link(0, "second", "second")];
    deploy::preflight(&actions).unwrap();
    f.skill("second", "occupied");
    assert!(deploy::apply(&actions).is_err());
    assert!(!f.agents[0].join("first").exists());
    std::fs::remove_file(f.ws.root.join("second/SKILL.md")).unwrap();
    assert!(deploy::apply(&actions).is_err());
    assert!(!f.agents[0].join("first").exists());
}

#[test]
fn same_name_is_allowed_in_separate_directories_but_not_physical_aliases() {
    let f = Fixture::new();
    f.skill("first", "shared");
    f.skill("second", "shared");
    let actions = vec![f.link(0, "first", "shared"), f.link(1, "second", "shared")];
    assert!(deploy::name_conflicts(&f.ws.scan_for_links().unwrap(), &actions).is_empty());
    assert_eq!(deploy::apply(&actions).unwrap(), 2);
    for agent in &f.agents {
        std::fs::remove_file(agent.join("shared")).unwrap();
    }

    let alias = f._temp.path().join("alias");
    std::os::unix::fs::symlink(f._temp.path(), &alias).unwrap();
    let mut second = f.link(1, "second", "alternate");
    if let deploy::Action::Link { path, .. } = &mut second {
        *path = alias.join("a/alternate");
    }
    let actions = vec![f.link(0, "first", "shared"), second];
    // Preview leaves batch choices unresolved; applying must reject them.
    deploy::preflight(&actions).unwrap();
    assert!(deploy::apply(&actions).is_err());
    assert!(!f.agents[0].join("shared").exists());
    assert!(!f.agents[0].join("alternate").exists());
}

#[test]
fn batch_path_collision_is_rejected_even_when_names_are_different() {
    let f = Fixture::new();
    f.skill("first", "first");
    f.skill("second", "second");
    let actions = vec![
        f.link(0, "first", "same-path"),
        f.link(0, "second", "same-path"),
    ];
    let conflicts = deploy::name_conflicts(&f.ws.scan_for_links().unwrap(), &actions);
    assert_eq!(conflicts.len(), 2);
    assert!(deploy::apply(&actions).is_err());
    assert!(!f.agents[0].join("same-path").exists());
}

#[test]
fn conflict_identity_is_resolved_again_after_a_link_is_retargeted() {
    let f = Fixture::new();
    f.skill("first", "shared");
    f.skill("second", "shared");
    deploy::apply(&[f.link(0, "first", "shared")]).unwrap();
    let snap = f.ws.scan_for_links().unwrap();
    let actions = vec![f.link(0, "first", "shared")];
    assert!(deploy::name_conflicts(&snap, &actions).is_empty());
    std::fs::remove_file(f.agents[0].join("shared")).unwrap();
    std::os::unix::fs::symlink(f.ws.root.join("second"), f.agents[0].join("shared")).unwrap();
    let conflicts = deploy::name_conflicts(&snap, &actions);
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0].other_skill.as_deref(), Some("second"));
    assert!(deploy::apply(&actions).is_err());
}

#[test]
fn replacement_excludes_only_the_explicitly_removed_link() {
    let f = Fixture::new();
    f.skill("first", "shared");
    f.skill("second", "shared");
    deploy::apply(&[f.link(0, "first", "shared")]).unwrap();
    let snap = f.ws.scan_for_links().unwrap();
    let incoming = vec![f.link(0, "second", "shared")];
    let conflicts = deploy::name_conflicts(&snap, &incoming);
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0].other_skill.as_deref(), Some("first"));
    let actions = deploy::resolve_names(&snap, &incoming, Some("replace")).unwrap();
    assert!(deploy::name_conflicts(&snap, &actions).is_empty());
    deploy::preflight(&actions).unwrap();

    // A second entry with that declared name is still a conflict, even though
    // this batch is allowed to remove the original destination.
    Fixture::put(&f.agents[0].join("owned"), "shared");
    assert!(deploy::apply(&actions).is_err());
    assert_eq!(
        std::fs::read_link(f.agents[0].join("shared")).unwrap(),
        f.ws.root.join("first")
    );
    std::fs::remove_dir_all(f.agents[0].join("owned")).unwrap();
    assert_eq!(deploy::apply(&actions).unwrap(), 2);
    assert_eq!(
        std::fs::read_link(f.agents[0].join("shared")).unwrap(),
        f.ws.root.join("second")
    );
}
