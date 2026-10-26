//! CLI browsing uses the TUI inventory policy; verification remains explicit.
use skills::{
    Workspace,
    config::Config,
    meta::{Baseline, SkillMeta, Source},
    ops::DownloadDir,
};

#[test]
fn cli_browsing_and_explicit_verification_match_library_reads() {
    let temp = DownloadDir::new("read-policy").unwrap();
    Config {
        agents: vec![],
        ..Default::default()
    }
    .save(temp.path())
    .unwrap();
    let ws = Workspace::open(temp.path()).unwrap();
    let path = ws.root.join("example");
    std::fs::create_dir(&path).unwrap();
    std::fs::write(path.join("SKILL.md"), "---\nname: example\n---\nOriginal").unwrap();
    ws.meta
        .save(
            "example",
            &SkillMeta {
                source: Some(Source::Git {
                    url: "https://example.test/skills.git".into(),
                    branch: None,
                    subpath: None,
                    revision: Some("v1".into()),
                }),
                baseline: Some(Baseline {
                    hash: skills::hash::hash_directory(&path).unwrap(),
                    hash_algo: skills::hash::HASH_ALGO,
                }),
                ..Default::default()
            },
        )
        .unwrap();
    std::fs::write(path.join("payload"), "local edit").unwrap();
    let cli = |args: &[&str]| {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_skills"))
            .arg("--root")
            .arg(&ws.root)
            .arg("--json")
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()
    };
    assert_eq!(
        cli(&["list"])[0]["status"],
        ws.inventory().unwrap().skills[0].status.label()
    );
    assert_eq!(cli(&["list"])[0]["status"], "unchecked");
    assert_eq!(cli(&["list", "--verify"])[0]["status"], "modified");
    assert_eq!(cli(&["show", "example"])["status"]["kind"], "unchecked");
    assert!(cli(&["show", "example"])["current_hash"].is_null());
    assert_eq!(
        cli(&["show", "example", "--verify"])["status"]["kind"],
        "modified"
    );
    assert!(cli(&["show", "example", "--verify"])["current_hash"].is_string());
    assert_eq!(
        cli(&["list", "--verify", "status:modified"])
            .as_array()
            .unwrap()
            .len(),
        1
    );
}
