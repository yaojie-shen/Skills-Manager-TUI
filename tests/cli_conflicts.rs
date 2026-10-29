//! Both front ends resolve the same reviewed conflicts, with explicit CLI choices.
use skills::{Workspace, config::Config, ops::DownloadDir};

#[test]
fn project_cli_reviews_conflicts_and_archives_only_after_explicit_choice() {
    let temp = DownloadDir::new("cli-conflict-options").unwrap();
    let root = temp.path().join("library");
    let project = temp.path().join("project");
    let agent = project.join(".cursor/skills");
    for dir in [root.join("sample"), agent.join("owned")] {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), "---\nname: sample\n---\nbody").unwrap();
    }
    Config {
        agents: vec![],
        ..Default::default()
    }
    .save(&root)
    .unwrap();
    let cli = |args: &[&str]| {
        std::process::Command::new(env!("CARGO_BIN_EXE_skills"))
            .env("HOME", temp.path().join("fake-home"))
            .arg("--root")
            .arg(&root)
            .arg("--project")
            .arg(&project)
            .arg("--json")
            .args(args)
            .output()
            .unwrap()
    };
    let preview = cli(&["deploy", "sample", "--agent", "cursor", "--dry-run"]);
    assert!(
        preview.status.success(),
        "{}",
        String::from_utf8_lossy(&preview.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&preview.stdout).unwrap();
    assert_eq!(report["choices"][0]["selector"], "cursor/sample");
    let denied = cli(&[
        "deploy",
        "sample",
        "--agent",
        "cursor",
        "--choose",
        "cursor/sample=skill:sample",
    ]);
    assert!(!denied.status.success());
    assert!(agent.join("owned/SKILL.md").exists());
    assert!(
        !cli(&[
            "tag",
            "create",
            "unwanted",
            "--choose",
            "cursor/sample=none"
        ])
        .status
        .success()
    );
    assert!(
        Workspace::open(&root)
            .unwrap()
            .tags
            .load("unwanted")
            .unwrap()
            .is_none()
    );
    let applied = cli(&[
        "deploy",
        "sample",
        "--agent",
        "cursor",
        "--choose",
        "cursor/sample=skill:sample",
        "--archive-conflicts",
    ]);
    assert!(
        applied.status.success(),
        "{}",
        String::from_utf8_lossy(&applied.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&applied.stdout).unwrap();
    assert!(report["message"].as_str().unwrap().contains("Archived"));
    assert_eq!(
        agent.join("sample").canonicalize().unwrap(),
        root.join("sample")
    );
    assert!(!agent.join("owned").exists());
    assert!(project.join(".cursor/.skills-manager-backups").is_dir());
    assert!(root.join("sample/SKILL.md").exists());
}
