use skills::{
    Workspace,
    config::AgentConfig,
    ops::{
        DownloadDir,
        agent_links::{self, Repair},
    },
    reconcile::EntryState,
};
use std::{fs, path::PathBuf, process::Command};

fn fixture() -> (DownloadDir, Workspace, PathBuf, PathBuf) {
    let temp = DownloadDir::new("foreign-repair-test").unwrap();
    let root = temp.path().join("central");
    let agent = temp.path().join("agent");
    let external = temp.path().join("external");
    for path in [&root, &agent, &external] {
        fs::create_dir_all(path).unwrap();
    }
    fs::write(
        external.join("SKILL.md"),
        "---\nname: printer\ndescription: Print documents\n---\nPrinter instructions\n",
    )
    .unwrap();
    fs::write(external.join("data.txt"), "preserve this file").unwrap();
    std::os::unix::fs::symlink(&external, agent.join("printer")).unwrap();
    let mut ws = Workspace::open(&root).unwrap();
    ws.config.agents = vec![AgentConfig {
        key: "sample".into(),
        name: "Sample".into(),
        skills_dir: agent.display().to_string(),
    }];
    ws.config.save(&root).unwrap();
    (temp, ws, agent, external)
}

#[test]
fn foreign_repairs_preserve_external_content_and_reject_stale_plans() {
    let (_temp, ws, agent, external) = fixture();
    let original = skills::hash::hash_directory(&external).unwrap();
    let remove = agent_links::plan(&ws, "sample", "printer", Repair::Remove).unwrap();
    let adopt = agent_links::plan(&ws, "sample", "printer", Repair::Adopt).unwrap();
    fs::remove_file(agent.join("printer")).unwrap();
    std::os::unix::fs::symlink(&ws.root, agent.join("printer")).unwrap();
    assert!(remove.apply(&ws).is_err());
    assert!(adopt.apply(&ws).is_err());
    assert_eq!(fs::read_link(agent.join("printer")).unwrap(), ws.root);
    assert!(!ws.root.join("printer").exists());
    fs::remove_file(agent.join("printer")).unwrap();
    std::os::unix::fs::symlink(&external, agent.join("printer")).unwrap();
    agent_links::plan(&ws, "sample", "printer", Repair::Adopt)
        .unwrap()
        .apply(&ws)
        .unwrap();
    assert_eq!(skills::hash::hash_directory(&external).unwrap(), original);
    assert_eq!(
        skills::hash::hash_directory(&ws.root.join("printer")).unwrap(),
        original
    );
    assert_eq!(
        fs::read_link(agent.join("printer")).unwrap(),
        ws.root.join("printer")
    );
    assert!(matches!(
        ws.scan().unwrap().agent("sample").unwrap().entries["printer"],
        EntryState::Deployed
    ));
    assert!(agent_links::plan(&ws, "sample", "printer", Repair::Remove).is_err());
    std::os::unix::fs::symlink(&external, agent.join("other-printer")).unwrap();
    agent_links::plan(&ws, "sample", "other-printer", Repair::Remove)
        .unwrap()
        .apply(&ws)
        .unwrap();
    assert!(fs::symlink_metadata(agent.join("other-printer")).is_err());
    assert_eq!(skills::hash::hash_directory(&external).unwrap(), original);
}

#[test]
fn invalid_targets_and_replaced_agent_directories_are_never_adopted() {
    let (_temp, ws, agent, external) = fixture();
    let plan = agent_links::plan(&ws, "sample", "printer", Repair::Adopt).unwrap();
    fs::remove_file(external.join("SKILL.md")).unwrap();
    assert!(plan.apply(&ws).is_err());
    assert!(agent_links::plan(&ws, "sample", "printer", Repair::Adopt).is_err());
    assert!(!ws.root.join("printer").exists());
    let remove = agent_links::plan(&ws, "sample", "printer", Repair::Remove).unwrap();
    let moved = agent.with_extension("moved");
    fs::rename(&agent, &moved).unwrap();
    std::os::unix::fs::symlink(&moved, &agent).unwrap();
    assert!(remove.apply(&ws).is_err());
    assert!(
        fs::symlink_metadata(moved.join("printer"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        fs::read_to_string(external.join("data.txt")).unwrap(),
        "preserve this file"
    );
}

#[test]
fn cli_foreign_repairs_preview_require_confirmation_and_emit_json() {
    let (_temp, ws, agent, external) = fixture();
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_skills"))
            .arg("--root")
            .arg(&ws.root)
            .arg("--json")
            .args(args)
            .output()
            .unwrap()
    };
    let preview = run(&["agents", "adopt-link", "sample", "printer", "--dry-run"]);
    assert!(
        preview.status.success(),
        "{}",
        String::from_utf8_lossy(&preview.stderr)
    );
    let plan: serde_json::Value = serde_json::from_slice(&preview.stdout).unwrap();
    assert_eq!(plan["operation"], "adopt");
    assert!(!ws.root.join("printer").exists());
    assert!(
        !run(&["agents", "remove-link", "sample", "printer"])
            .status
            .success()
    );
    assert_eq!(fs::read_link(agent.join("printer")).unwrap(), external);
    let result = run(&["agents", "adopt-link", "sample", "printer", "--yes"]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert!(
        result["message"]
            .as_str()
            .unwrap()
            .contains("external target preserved")
    );
    assert!(external.join("SKILL.md").is_file());
}
