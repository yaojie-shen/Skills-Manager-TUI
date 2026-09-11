use skills::{
    Workspace,
    ops::{edit, install},
};

#[test]
fn local_namespace_install_metadata_scan_and_removal() {
    let base = std::env::temp_dir().join(format!("skills-local-storage-{}", std::process::id()));
    std::fs::create_dir_all(base.join("root/local")).unwrap();
    std::fs::create_dir_all(base.join("source")).unwrap();
    std::fs::write(
        base.join("source/SKILL.md"),
        "---\nname: example\ndescription: Example\n---\nContent\n",
    )
    .unwrap();
    let mut ws = Workspace::open(&base.join("root")).unwrap();
    ws.config.agents.truncate(1);
    ws.config.agents[0].skills_dir = base.join("agent").to_string_lossy().into_owned();
    std::fs::create_dir_all(base.join("agent")).unwrap();
    let key = install::install(
        &ws,
        &install::InstallRef::Local(base.join("source")),
        Some("local/project--example"),
    )
    .unwrap();
    assert!(ws.meta.list_keys().unwrap().is_empty());
    let snap = ws.scan().unwrap();
    assert_eq!(snap.skills.len(), 1);
    assert!(snap.get(&key).is_some());
    assert_eq!(
        skills::repository::default_deploy_name(&key),
        "project--example"
    );
    assert!(!skills::repository::valid_id("local/../escape"));
    skills::ops::deploy::apply(
        &skills::ops::deploy::plan_deploy(
            &ws,
            &snap,
            std::slice::from_ref(&key),
            &[ws.config.agents[0].key.clone()],
        )
        .unwrap(),
    )
    .unwrap();
    let new = "local/project/example";
    edit::rename(&ws, &ws.scan().unwrap(), &key, new).unwrap();
    assert!(!ws.skill_path(&key).exists());
    assert_eq!(
        std::fs::read_link(base.join("agent/project--example")).unwrap(),
        ws.skill_path(new)
    );
    assert!(ws.meta.list_keys().unwrap().is_empty());
    let snap = ws.scan().unwrap();
    assert_eq!(snap.skills.len(), 1);
    assert!(snap.get(new).is_some());
    assert!(
        snap.get(new)
            .unwrap()
            .deployed_to()
            .contains(&ws.config.agents[0].key.as_str())
    );
    assert!(!skills::repository::valid_id("local/project/../escape"));
    edit::remove(&ws, &snap, new, false).unwrap();
    assert!(!base.join("agent/project--example").exists());
    assert!(!ws.skill_path(&key).exists());
    assert!(ws.meta.list_keys().unwrap().is_empty());
    assert!(base.join("source/SKILL.md").is_file());
    std::fs::remove_dir_all(base).unwrap();
}
