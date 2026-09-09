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
    targets::apply(
        &ws,
        &["sample".into()],
        &[(target.clone(), true)],
        Some(&project),
    )
    .unwrap();
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
