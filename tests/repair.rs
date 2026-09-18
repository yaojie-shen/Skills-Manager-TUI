use skills::{
    Workspace,
    config::{AgentConfig, Config},
    ops::{DownloadDir, repair},
    reconcile::AgentDirMode,
};
use std::fs;

struct Fixture {
    _tmp: DownloadDir,
    ws: Workspace,
    agent: std::path::PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let tmp = DownloadDir::new("repair-deployments").unwrap();
        let root = tmp.path().join("root");
        let agent = tmp.path().join("agent");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&agent).unwrap();
        Config {
            agents: vec![AgentConfig {
                key: "test".into(),
                name: "Test".into(),
                skills_dir: agent.display().to_string(),
            }],
            ..Default::default()
        }
        .save(&root)
        .unwrap();
        Self {
            ws: Workspace::open(&root).unwrap(),
            _tmp: tmp,
            agent,
        }
    }

    fn skill(&self, key: &str, name: &str) {
        let path = self.ws.skill_path(key);
        fs::create_dir_all(&path).unwrap();
        fs::write(
            path.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: Test\n---\n{key}\n"),
        )
        .unwrap();
    }

    fn broken(&self, name: &str) -> std::path::PathBuf {
        let old = self._tmp.path().join("gone").join(name);
        std::os::unix::fs::symlink(&old, self.agent.join(name)).unwrap();
        old
    }
}

#[test]
fn reconnects_a_unique_declared_name_even_when_folder_differs() {
    let f = Fixture::new();
    f.skill("repos/example/folder-a", "skill-b");
    let old = f.broken("skill-b");

    let plan = repair::build_plan(&f.ws, &Default::default()).unwrap();
    assert_eq!(plan.analysis.ready, 1);
    assert_eq!(plan.actions.len(), 1);
    assert_eq!(fs::read_link(f.agent.join("skill-b")).unwrap(), old);

    let result = repair::apply_plan(&f.ws, &plan).unwrap();
    assert_eq!(result.repaired, 1);
    assert_eq!(
        fs::canonicalize(f.agent.join("skill-b")).unwrap(),
        fs::canonicalize(f.ws.skill_path("repos/example/folder-a")).unwrap()
    );
}

#[test]
fn duplicate_declared_names_require_an_explicit_library_key() {
    let f = Fixture::new();
    f.skill("repos/one/folder-a", "shared");
    f.skill("repos/two/folder-b", "shared");
    f.broken("shared");

    let unresolved = repair::build_plan(&f.ws, &Default::default()).unwrap();
    assert_eq!(unresolved.analysis.unresolved, 1);
    assert!(unresolved.actions.is_empty());

    let options = repair::Options {
        deployments: [("test/shared".into(), "repos/two/folder-b".into())]
            .into_iter()
            .collect(),
    };
    let plan = repair::build_plan(&f.ws, &options).unwrap();
    assert_eq!(plan.actions.len(), 1);
    repair::apply_plan(&f.ws, &plan).unwrap();
    assert_eq!(
        fs::canonicalize(f.agent.join("shared")).unwrap(),
        fs::canonicalize(f.ws.skill_path("repos/two/folder-b")).unwrap()
    );
}

#[test]
fn unmatched_broken_link_is_left_unchanged() {
    let f = Fixture::new();
    let old = f.broken("unknown");
    let plan = repair::build_plan(&f.ws, &Default::default()).unwrap();
    assert_eq!(plan.analysis.unresolved, 1);
    assert!(plan.actions.is_empty());
    assert_eq!(fs::read_link(f.agent.join("unknown")).unwrap(), old);
}

#[test]
fn healthy_managed_link_with_outdated_name_is_renamed() {
    let f = Fixture::new();
    f.skill("repos/example/folder-a", "new-name");
    std::os::unix::fs::symlink(
        f.ws.skill_path("repos/example/folder-a"),
        f.agent.join("old-name"),
    )
    .unwrap();

    let plan = repair::build_plan(&f.ws, &Default::default()).unwrap();
    assert_eq!(plan.actions.len(), 1);
    let result = repair::apply_plan(&f.ws, &plan).unwrap();
    assert_eq!(result.repaired, 1);
    assert!(fs::symlink_metadata(f.agent.join("old-name")).is_err());
    assert_eq!(
        fs::canonicalize(f.agent.join("new-name")).unwrap(),
        fs::canonicalize(f.ws.skill_path("repos/example/folder-a")).unwrap()
    );
}

#[test]
fn outdated_name_does_not_overwrite_an_occupied_destination() {
    let f = Fixture::new();
    f.skill("folder-a", "new-name");
    std::os::unix::fs::symlink(f.ws.skill_path("folder-a"), f.agent.join("old-name")).unwrap();
    fs::write(f.agent.join("new-name"), "external").unwrap();

    let plan = repair::build_plan(&f.ws, &Default::default()).unwrap();
    assert_eq!(plan.analysis.unresolved, 1);
    assert!(plan.actions.is_empty());
    assert!(f.agent.join("old-name").is_symlink());
    assert_eq!(
        fs::read_to_string(f.agent.join("new-name")).unwrap(),
        "external"
    );
}

#[test]
fn stale_plan_does_not_overwrite_a_replacement() {
    let f = Fixture::new();
    f.skill("folder-a", "tool");
    f.broken("tool");
    let plan = repair::build_plan(&f.ws, &Default::default()).unwrap();
    fs::remove_file(f.agent.join("tool")).unwrap();
    fs::write(f.agent.join("tool"), "keep").unwrap();

    let result = repair::apply_plan(&f.ws, &plan).unwrap();
    assert_eq!(result.failed, 1);
    assert_eq!(fs::read_to_string(f.agent.join("tool")).unwrap(), "keep");
}

#[test]
fn read_only_agent_directories_never_produce_repair_actions() {
    let f = Fixture::new();
    f.skill("tool", "tool");
    fs::remove_dir(&f.agent).unwrap();
    std::os::unix::fs::symlink(&f.ws.root, &f.agent).unwrap();

    let snap = f.ws.scan().unwrap();
    assert!(matches!(
        snap.agent("test").unwrap().mode,
        AgentDirMode::ReadOnly { .. }
    ));
    assert!(snap.agent("test").unwrap().entries.is_empty());
    let plan = repair::build_plan(&f.ws, &Default::default()).unwrap();
    assert!(plan.analysis.issues.is_empty());
    assert!(plan.actions.is_empty());
}

#[test]
fn overlapping_real_agent_directory_is_read_only() {
    let f = Fixture::new();
    f.skill("nested-agent/tool", "tool");
    let config = Config {
        agents: vec![AgentConfig {
            key: "nested".into(),
            name: "Nested".into(),
            skills_dir: f.ws.root.join("nested-agent").display().to_string(),
        }],
        ..Default::default()
    };
    let snap = skills::reconcile::scan(&f.ws.root, &config).unwrap();
    assert!(matches!(
        snap.agent("nested").unwrap().mode,
        AgentDirMode::ReadOnly { .. }
    ));
}

#[test]
fn missing_agent_directory_inside_library_is_read_only_and_not_created() {
    let f = Fixture::new();
    f.skill("tool", "tool");
    let path = f.ws.root.join("future-agent");
    let config = Config {
        agents: vec![AgentConfig {
            key: "nested".into(),
            name: "Nested".into(),
            skills_dir: path.display().to_string(),
        }],
        ..Default::default()
    };
    let mut ws = Workspace::open(&f.ws.root).unwrap();
    ws.config = config.clone();
    let snap = skills::reconcile::scan(&ws.root, &config).unwrap();
    assert!(matches!(
        snap.agent("nested").unwrap().mode,
        AgentDirMode::ReadOnly { .. }
    ));
    let error =
        skills::ops::targets::set_deployed(&ws, &config.agents[0], None, &["tool".into()], true)
            .unwrap_err();
    assert!(format!("{error:#}").contains("read-only"));
    assert!(!path.exists());
}

#[test]
fn missing_agent_directory_beneath_symlink_into_library_is_read_only() {
    let f = Fixture::new();
    f.skill("tool", "tool");
    let alias = f._tmp.path().join("library-alias");
    std::os::unix::fs::symlink(&f.ws.root, &alias).unwrap();
    let path = alias.join("future-agent");
    let config = Config {
        agents: vec![AgentConfig {
            key: "nested".into(),
            name: "Nested".into(),
            skills_dir: path.display().to_string(),
        }],
        ..Default::default()
    };
    let snap = skills::reconcile::scan(&f.ws.root, &config).unwrap();
    assert!(matches!(
        snap.agent("nested").unwrap().mode,
        AgentDirMode::ReadOnly { .. }
    ));
    assert!(!f.ws.root.join("future-agent").exists());
}
