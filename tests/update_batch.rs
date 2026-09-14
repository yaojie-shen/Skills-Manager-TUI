use skills::{
    Workspace,
    ops::{
        DownloadDir, git, install,
        update::{Take, UpdateSession},
    },
};
use std::collections::BTreeMap;
#[test]
fn batch_skips_unchanged_repositories_and_downloads_changed_repository_once() {
    let tmp = DownloadDir::new("update-batch-test").unwrap();
    let source = tmp.path().join("source");
    std::fs::create_dir_all(&source).unwrap();
    git(&["init", "--initial-branch=main"], Some(&source)).unwrap();
    for key in ["one", "two"] {
        std::fs::create_dir_all(source.join(key)).unwrap();
        std::fs::write(
            source.join(key).join("SKILL.md"),
            format!("---\nname: {key}\ndescription: example\n---\noriginal"),
        )
        .unwrap();
    }
    let commit = || {
        git(&["add", "."], Some(&source)).unwrap();
        git(
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                "update",
            ],
            Some(&source),
        )
        .unwrap();
    };
    commit();
    let root = tmp.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let mut ws = Workspace::open(&root).unwrap();
    ws.config.agents.clear();
    for key in ["one", "two"] {
        install::install(
            &ws,
            &install::InstallRef::Git {
                url: source.to_string_lossy().into_owned(),
                branch: Some("main".into()),
                subpath: Some(key.into()),
            },
            Some(key),
        )
        .unwrap();
    }
    let mut session = UpdateSession::default();
    let mut messages = Vec::new();
    let snap = ws.scan().unwrap();
    for key in ["one", "two"] {
        let checked = session
            .check(&ws, key, &mut |m| messages.push(m.to_string()))
            .unwrap();
        let p = session
            .prepare(&snap, key, &mut |m| messages.push(m.to_string()))
            .unwrap();
        assert!(!checked.update_available);
        assert_eq!(checked.remote, p.to_revision);
        assert_eq!(p.from_revision.as_deref(), Some(p.to_revision.as_str()));
        p.cleanup();
    }
    assert_eq!(
        messages
            .iter()
            .filter(|m| m.starts_with("Checking "))
            .count(),
        1
    );
    assert_eq!(
        messages
            .iter()
            .filter(|m| m.starts_with("Downloading "))
            .count(),
        0
    );
    for key in ["one", "two"] {
        std::fs::write(source.join(key).join("new.txt"), "upstream").unwrap();
    }
    commit();
    // Both modified skills need the original baseline from the shared checkout.
    for key in ["one", "two"] {
        std::fs::write(ws.skill_path(key).join("mine.txt"), "local").unwrap();
    }
    let snap = ws.scan().unwrap();
    let mut session = UpdateSession::default();
    messages.clear();
    for key in ["one", "two"] {
        let checked = session
            .check(&ws, key, &mut |m| messages.push(m.to_string()))
            .unwrap();
        let p = session
            .prepare(&snap, key, &mut |m| messages.push(m.to_string()))
            .unwrap();
        assert!(checked.update_available);
        assert_eq!(checked.remote, p.to_revision);
        assert!(p.needs_resolution());
        assert!(p.baseline_dir.as_ref().unwrap().join("SKILL.md").exists());
        assert_eq!(
            p.files["mine.txt"],
            skills::ops::update::FileChange::LocalChanged
        );
        assert_eq!(
            p.files["new.txt"],
            skills::ops::update::FileChange::UpstreamChanged
        );
        skills::ops::update::apply(&ws, &p, Take::Upstream, &BTreeMap::new()).unwrap();
        assert!(!ws.skill_path(key).join("mine.txt").exists());
        assert!(ws.skill_path(key).join("new.txt").exists());
    }
    assert_eq!(
        messages
            .iter()
            .filter(|m| m.starts_with("Downloading "))
            .count(),
        1
    );
}
