//! End-to-end flow over a temporary skills root with fake agent directories.

use skills::Workspace;
use skills::config::{AgentConfig, Config, DeployConfig};
use skills::ops::deploy::{self, Action};
use skills::ops::{edit, install};
use skills::reconcile::{AgentDirMode, DeployState, EntryState, SkillStatus};
use std::path::{Path, PathBuf};

struct Fixture {
    base: PathBuf,
    root: PathBuf,
    agent_a: PathBuf,
    agent_b: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let base = std::env::temp_dir().join(format!("skills-flow-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("skills");
        let agent_a = base.join("agent-a");
        let agent_b = base.join("agent-b");
        std::fs::create_dir_all(&root).unwrap();
        let cfg = Config {
            schema: 1,
            agents: vec![
                AgentConfig {
                    key: "a".into(),
                    name: "Agent A".into(),
                    skills_dir: agent_a.display().to_string(),
                },
                AgentConfig {
                    key: "b".into(),
                    name: "Agent B".into(),
                    skills_dir: agent_b.display().to_string(),
                },
            ],
            deploy: DeployConfig {
                all_to_all: true,
                presets: vec![],
            },
            tags: vec![],
        };
        cfg.save(&root).unwrap();
        Self {
            base,
            root,
            agent_a,
            agent_b,
        }
    }

    fn add_skill(&self, key: &str, desc: &str) -> PathBuf {
        let dir = self.root.join(key);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {key}\ndescription: {desc}\n---\n# {key}\n"),
        )
        .unwrap();
        dir
    }

    fn ws(&self) -> Workspace {
        Workspace::open(&self.root).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn link_state(dir: &Path, key: &str) -> Option<PathBuf> {
    std::fs::read_link(dir.join(key)).ok()
}

#[test]
fn scan_tags_notes_and_baseline() {
    let f = Fixture::new("meta");
    f.add_skill("alpha", "first skill");
    f.add_skill("beta", "second skill");
    let ws = f.ws();

    let snap = ws.scan().unwrap();
    assert_eq!(snap.skills.len(), 2);
    assert!(
        snap.skills
            .iter()
            .all(|s| s.status == SkillStatus::Unmanaged)
    );
    assert!(
        !ws.meta.dir.join("alpha.toml").exists(),
        "scan must not write"
    );

    edit::tag_add(&ws, "alpha", &["ops".into(), "ml".into()]).unwrap();
    edit::note_set(&ws, "alpha", Some("hello\nworld")).unwrap();
    let snap = ws.scan().unwrap();
    let a = snap.get("alpha").unwrap();
    assert_eq!(a.tags, vec!["ops", "ml"]);
    assert_eq!(a.note.as_deref(), Some("hello\nworld"));
    assert_eq!(
        a.status,
        SkillStatus::Managed { no_baseline: false },
        "first write records a baseline"
    );

    // Local edit -> modified; accept -> managed again.
    std::fs::write(f.root.join("alpha/extra.md"), "more").unwrap();
    let snap = ws.scan().unwrap();
    assert_eq!(snap.get("alpha").unwrap().status, SkillStatus::Modified);
    edit::accept(&ws, "alpha").unwrap();
    let snap = ws.scan().unwrap();
    assert_eq!(
        snap.get("alpha").unwrap().status,
        SkillStatus::Managed { no_baseline: false }
    );

    // Tag rename/delete across skills.
    edit::tag_add(&ws, "beta", &["ops".into()]).unwrap();
    assert_eq!(edit::tag_rename(&ws, "ops", "operations").unwrap(), 2);
    assert_eq!(edit::tag_delete(&ws, "ml").unwrap(), 1);
    let snap = ws.scan().unwrap();
    assert_eq!(snap.get("alpha").unwrap().tags, vec!["operations"]);
    assert_eq!(snap.get("beta").unwrap().tags, vec!["operations"]);

    // Metadata dir must never look like a skill.
    assert!(snap.get(".skills-meta").is_none());
}

#[test]
fn missing_and_rename_detection() {
    let f = Fixture::new("rename");
    f.add_skill("gamma", "g");
    let ws = f.ws();
    edit::tag_add(&ws, "gamma", &["x".into()]).unwrap();

    std::fs::rename(f.root.join("gamma"), f.root.join("gamma2")).unwrap();
    let snap = ws.scan().unwrap();
    assert_eq!(
        snap.get("gamma").unwrap().status,
        SkillStatus::Renamed {
            to: "gamma2".into()
        }
    );
    assert_eq!(snap.get("gamma2").unwrap().status, SkillStatus::Unmanaged);

    edit::migrate_meta(&ws, "gamma", "gamma2").unwrap();
    let snap = ws.scan().unwrap();
    assert!(snap.get("gamma").is_none());
    assert_eq!(snap.get("gamma2").unwrap().tags, vec!["x"]);

    std::fs::remove_dir_all(f.root.join("gamma2")).unwrap();
    let snap = ws.scan().unwrap();
    assert_eq!(snap.get("gamma2").unwrap().status, SkillStatus::Missing);
}

#[test]
fn deploy_undeploy_sync_and_convert() {
    let f = Fixture::new("deploy");
    f.add_skill("one", "1");
    f.add_skill("two", "2");
    // Agent A: whole-directory link (legacy layout). Agent B: missing.
    std::os::unix::fs::symlink(&f.root, &f.agent_a).unwrap();
    let ws = f.ws();

    let snap = ws.scan().unwrap();
    assert_eq!(snap.agent("a").unwrap().mode, AgentDirMode::DirLinked);
    assert_eq!(snap.agent("b").unwrap().mode, AgentDirMode::Missing);
    assert_eq!(snap.get("one").unwrap().deploy["a"], DeployState::Deployed);
    assert_eq!(
        snap.get("one").unwrap().deploy["b"],
        DeployState::NoAgentDir
    );

    // Sync creates agent B with both links, skips dir-linked A.
    let actions = deploy::plan_sync(&ws, &snap).unwrap();
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::Mkdir { agent, .. } if agent == "b"))
    );
    assert_eq!(
        actions
            .iter()
            .filter(|a| matches!(a, Action::Link { .. }))
            .count(),
        2
    );
    deploy::apply(&actions).unwrap();
    assert_eq!(link_state(&f.agent_b, "one").unwrap(), f.root.join("one"));

    // Convert A to per-skill links.
    let snap = ws.scan().unwrap();
    let actions = deploy::plan_convert(&ws, &snap, "a").unwrap();
    deploy::apply(&actions).unwrap();
    let snap = ws.scan().unwrap();
    assert_eq!(snap.agent("a").unwrap().mode, AgentDirMode::Real);
    assert_eq!(
        snap.agent("a").unwrap().entries["two"],
        EntryState::Deployed
    );

    // Undeploy one from A only.
    let actions = deploy::plan_undeploy(&ws, &snap, &["one".into()], &["a".into()]).unwrap();
    deploy::apply(&actions).unwrap();
    let snap = ws.scan().unwrap();
    assert_eq!(
        snap.get("one").unwrap().deploy["a"],
        DeployState::NotDeployed
    );
    assert_eq!(snap.get("one").unwrap().deploy["b"], DeployState::Deployed);

    // Shadow: a real copy in agent B is never touched.
    std::fs::remove_file(f.agent_b.join("two")).unwrap();
    std::fs::create_dir_all(f.agent_b.join("two")).unwrap();
    std::fs::write(f.agent_b.join("two/SKILL.md"), "---\nname: two\n---\n").unwrap();
    let snap = ws.scan().unwrap();
    assert_eq!(
        snap.agent("b").unwrap().entries["two"],
        EntryState::Shadow {
            same_content: false
        }
    );
    let actions = deploy::plan_deploy(&ws, &snap, &["two".into()], &["b".into()]).unwrap();
    assert!(matches!(actions[0], Action::Skip { .. }));

    // Broken link is repaired by sync; sync also re-links `one` to A (all_to_all).
    std::fs::remove_dir_all(f.agent_b.join("two")).unwrap();
    std::os::unix::fs::symlink(f.root.join("nonexistent"), f.agent_b.join("two")).unwrap();
    let snap = ws.scan().unwrap();
    assert!(matches!(
        snap.agent("b").unwrap().entries["two"],
        EntryState::Broken { .. }
    ));
    let actions = deploy::plan_sync(&ws, &snap).unwrap();
    deploy::apply(&actions).unwrap();
    let snap = ws.scan().unwrap();
    assert_eq!(snap.get("two").unwrap().deploy["b"], DeployState::Deployed);
    assert_eq!(snap.get("one").unwrap().deploy["a"], DeployState::Deployed);
}

#[test]
fn install_local_rename_remove() {
    let f = Fixture::new("install");
    let ws = f.ws();
    let src = f.base.join("srcskill");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("SKILL.md"),
        "---\nname: srcskill\ndescription: d\n---\nbody\n",
    )
    .unwrap();

    let r = install::parse_ref(src.to_str().unwrap(), None, None).unwrap();
    let key = install::install(&ws, &r, None).unwrap();
    assert_eq!(key, "srcskill");
    let snap = ws.scan().unwrap();
    let rec = snap.get("srcskill").unwrap();
    assert_eq!(rec.status, SkillStatus::Managed { no_baseline: false });
    assert!(matches!(
        rec.source,
        Some(skills::meta::Source::Local { .. })
    ));
    assert!(
        !ws.meta
            .dir
            .join(".staging")
            .read_dir()
            .map(|mut d| d.next().is_some())
            .unwrap_or(false),
        "staging cleaned"
    );

    // Installing again with the same name fails and leaves nothing behind.
    assert!(install::install(&ws, &r, None).is_err());

    // Deploy, rename, check links follow.
    let actions = deploy::plan_deploy(&ws, &snap, &["srcskill".into()], &["a".into()]).unwrap();
    deploy::apply(&actions).unwrap();
    let snap = ws.scan().unwrap();
    edit::rename(&ws, &snap, "srcskill", "renamed").unwrap();
    let snap = ws.scan().unwrap();
    assert!(snap.get("srcskill").is_none());
    assert_eq!(
        snap.get("renamed").unwrap().deploy["a"],
        DeployState::Deployed
    );
    assert_eq!(
        link_state(&f.agent_a, "renamed").unwrap(),
        f.root.join("renamed")
    );

    // Remove: link gone, dir gone, meta gone.
    edit::remove(&ws, &snap, "renamed", false).unwrap();
    let snap = ws.scan().unwrap();
    assert!(snap.get("renamed").is_none());
    assert!(!f.agent_a.join("renamed").exists());
    assert!(!ws.meta.dir.join("renamed.toml").exists());
}

#[test]
fn adopt_from_agent_dir() {
    let f = Fixture::new("adopt");
    let ws = f.ws();
    std::fs::create_dir_all(f.agent_a.join("local-only")).unwrap();
    std::fs::write(
        f.agent_a.join("local-only/SKILL.md"),
        "---\nname: local-only\n---\n",
    )
    .unwrap();
    let snap = ws.scan().unwrap();
    assert_eq!(
        snap.agent("a").unwrap().entries["local-only"],
        EntryState::AgentOnly
    );

    let key = install::adopt(&ws, &f.agent_a.join("local-only"), None).unwrap();
    assert_eq!(key, "local-only");
    let snap = ws.scan().unwrap();
    assert_eq!(
        snap.get("local-only").unwrap().deploy["a"],
        DeployState::Deployed
    );
    assert_eq!(
        link_state(&f.agent_a, "local-only").unwrap(),
        f.root.join("local-only")
    );
}

/// Git-sourced install, check, and update with a local conflict, using a bare repo on disk.
#[test]
fn git_install_check_update_conflict() {
    use skills::ops::update::{self, FileChange, Take};
    let f = Fixture::new("git");
    let ws = f.ws();
    let git = |args: &[&str], cwd: &Path| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {:?}: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    // Upstream repo with skills/up/SKILL.md.
    let up = f.base.join("upstream");
    std::fs::create_dir_all(up.join("skills/up")).unwrap();
    git(&["init", "-q", "-b", "main"], &up);
    git(&["config", "user.email", "t@example.com"], &up);
    git(&["config", "user.name", "t"], &up);
    std::fs::write(
        up.join("skills/up/SKILL.md"),
        "---\nname: up\ndescription: v1\n---\nbody v1\n",
    )
    .unwrap();
    std::fs::write(up.join("skills/up/extra.md"), "extra v1\n").unwrap();
    git(&["add", "."], &up);
    git(&["commit", "-q", "-m", "v1"], &up);
    let rev1 = git(&["rev-parse", "HEAD"], &up);

    let url = format!("file://{}", up.display());
    let r = install::parse_ref(&url, Some("main"), Some("skills/up")).unwrap();
    let key = install::install(&ws, &r, None).unwrap();
    assert_eq!(key, "up");
    let snap = ws.scan().unwrap();
    let rec = snap.get("up").unwrap();
    assert_eq!(rec.status, SkillStatus::Managed { no_baseline: false });
    match &rec.source {
        Some(skills::meta::Source::Git {
            revision,
            subpath,
            branch,
            ..
        }) => {
            assert_eq!(revision.as_deref(), Some(rev1.as_str()));
            assert_eq!(subpath.as_deref(), Some("skills/up"));
            assert_eq!(branch.as_deref(), Some("main"));
        }
        other => panic!("unexpected source {other:?}"),
    }

    // No update yet.
    let c = update::check(&ws, "up").unwrap();
    assert!(!c.update_available);

    // Upstream changes SKILL.md; local changes extra.md -> both sides touched different files.
    std::fs::write(
        up.join("skills/up/SKILL.md"),
        "---\nname: up\ndescription: v2\n---\nbody v2\n",
    )
    .unwrap();
    git(&["commit", "-qam", "v2"], &up);
    let rev2 = git(&["rev-parse", "HEAD"], &up);
    std::fs::write(f.root.join("up/extra.md"), "extra local\n").unwrap();

    let c = update::check(&ws, "up").unwrap();
    assert!(c.update_available);
    assert_eq!(c.remote, rev2);

    let snap = ws.scan().unwrap();
    assert_eq!(snap.get("up").unwrap().status, SkillStatus::Modified);
    let prepared = update::prepare(&ws, &snap, "up").unwrap();
    assert!(prepared.needs_resolution());
    assert_eq!(prepared.files["SKILL.md"], FileChange::UpstreamChanged);
    assert_eq!(prepared.files["extra.md"], FileChange::LocalChanged);

    // Take upstream by default but keep the local extra.md.
    let mut per_file = std::collections::BTreeMap::new();
    per_file.insert("extra.md".to_string(), Take::Local);
    update::apply(&ws, &prepared, Take::Upstream, &per_file).unwrap();
    assert_eq!(
        std::fs::read_to_string(f.root.join("up/SKILL.md")).unwrap(),
        "---\nname: up\ndescription: v2\n---\nbody v2\n"
    );
    assert_eq!(
        std::fs::read_to_string(f.root.join("up/extra.md")).unwrap(),
        "extra local\n"
    );
    let snap = ws.scan().unwrap();
    let rec = snap.get("up").unwrap();
    assert_eq!(
        rec.status,
        SkillStatus::Managed { no_baseline: false },
        "baseline refreshed after update"
    );
    match &rec.source {
        Some(skills::meta::Source::Git { revision, .. }) => {
            assert_eq!(revision.as_deref(), Some(rev2.as_str()))
        }
        _ => unreachable!(),
    }
    assert!(
        !ws.meta
            .dir
            .join(".staging")
            .read_dir()
            .map(|mut d| d.next().is_some())
            .unwrap_or(false),
        "staging cleaned"
    );
}
