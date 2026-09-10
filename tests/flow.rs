//! End-to-end flow over a temporary skills root with fake agent directories.

use skills::Workspace;
use skills::config::{AgentConfig, Config, DeployConfig};
use skills::history::{self, Intent, Plan};
use skills::ops::deploy::{self, Action};
use skills::ops::{edit, install};
use skills::preset::Preset;
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
        let root = std::fs::canonicalize(root).unwrap();
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
            search: Default::default(),
            ui: Default::default(),
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
    std::os::unix::fs::symlink(&f.root, &f.agent_a).unwrap();

    std::fs::rename(f.root.join("gamma"), f.root.join("gamma2")).unwrap();
    let snap = ws.scan().unwrap();
    assert_eq!(
        snap.get("gamma").unwrap().status,
        SkillStatus::Renamed {
            to: "gamma2".into()
        }
    );
    assert_eq!(snap.get("gamma2").unwrap().status, SkillStatus::Unmanaged);
    assert_eq!(
        snap.get("gamma").unwrap().deploy["a"],
        DeployState::NotDeployed
    );

    edit::migrate_meta(&ws, "gamma", "gamma2").unwrap();
    let snap = ws.scan().unwrap();
    assert!(snap.get("gamma").is_none());
    assert_eq!(snap.get("gamma2").unwrap().tags, vec!["x"]);

    std::fs::remove_dir_all(f.root.join("gamma2")).unwrap();
    let snap = ws.scan().unwrap();
    assert_eq!(snap.get("gamma2").unwrap().status, SkillStatus::Missing);
    assert_eq!(
        snap.get("gamma2").unwrap().deploy["a"],
        DeployState::NotDeployed
    );
}

#[test]
fn external_move_repairs_links_presets_and_preserves_metadata() {
    let f = Fixture::new("external-move");
    f.add_skill("old", "move me");
    let ws = f.ws();
    edit::tag_add(&ws, "old", &["keep".into()]).unwrap();
    ws.presets
        .save(&Preset {
            name: "daily".into(),
            skills: vec!["old".into(), "local/new".into()],
            ..Default::default()
        })
        .unwrap();
    std::fs::create_dir_all(&f.agent_a).unwrap();
    std::fs::create_dir_all(&f.agent_b).unwrap();
    std::os::unix::fs::symlink(f.root.join("old"), f.agent_a.join("old")).unwrap();
    // An unrelated link with the old name must never be touched.
    std::os::unix::fs::symlink(f.base.join("elsewhere"), f.agent_b.join("old")).unwrap();
    std::fs::create_dir_all(f.root.join("local")).unwrap();
    std::fs::rename(f.root.join("old"), f.root.join("local/new")).unwrap();
    edit::migrate_meta(&ws, "old", "local/new").unwrap();
    assert!(!skills::util::is_symlink(&f.agent_a.join("old")));
    assert_eq!(
        link_state(&f.agent_a, "new"),
        Some(f.root.join("local/new"))
    );
    assert_eq!(
        link_state(&f.agent_b, "old"),
        Some(f.base.join("elsewhere"))
    );
    assert_eq!(
        ws.presets.load("daily").unwrap().unwrap().skills,
        vec!["local/new"]
    );
    let snap = ws.scan().unwrap();
    assert!(snap.get("old").is_none());
    assert_eq!(snap.get("local/new").unwrap().tags, vec!["keep"]);
}

#[test]
fn external_move_conflict_is_detected_before_any_link_or_metadata_changes() {
    let f = Fixture::new("external-conflict");
    f.add_skill("old", "move me");
    let ws = f.ws();
    edit::tag_add(&ws, "old", &["keep".into()]).unwrap();
    for agent in [&f.agent_a, &f.agent_b] {
        std::fs::create_dir_all(agent).unwrap();
        std::os::unix::fs::symlink(f.root.join("old"), agent.join("old")).unwrap();
    }
    std::fs::create_dir(f.agent_b.join("new")).unwrap();
    std::fs::rename(f.root.join("old"), f.root.join("new")).unwrap();
    assert!(edit::migrate_meta(&ws, "old", "new").is_err());
    assert!(ws.meta.exists("old"));
    assert!(!ws.meta.exists("new"));
    for agent in [&f.agent_a, &f.agent_b] {
        assert_eq!(link_state(agent, "old"), Some(f.root.join("old")));
    }
    assert!(!f.agent_a.join("new").exists());
}

#[test]
fn external_move_preserves_deployment_name_when_moving_between_repositories() {
    let f = Fixture::new("external-same-name");
    f.add_skill("repos/a/one", "move me");
    std::fs::write(
        f.root.join("repos/a/one/SKILL.md"),
        "---\nname: one\n---\nmove me",
    )
    .unwrap();
    let ws = f.ws();
    edit::tag_add(&ws, "repos/a/one", &["keep".into()]).unwrap();
    std::fs::create_dir_all(&f.agent_a).unwrap();
    std::os::unix::fs::symlink(f.root.join("repos/a/one"), f.agent_a.join("one")).unwrap();
    std::os::unix::fs::symlink(f.root.join("repos/a/one"), f.root.join("one")).unwrap();
    std::fs::create_dir_all(f.root.join("repos/b")).unwrap();
    std::fs::rename(f.root.join("repos/a/one"), f.root.join("repos/b/one")).unwrap();
    edit::migrate_meta(&ws, "repos/a/one", "repos/b/one").unwrap();
    for dir in [&f.agent_a, &f.root] {
        assert_eq!(link_state(dir, "one"), Some(f.root.join("repos/b/one")));
    }
}

#[test]
fn migration_rejects_existing_source_and_missing_or_external_destination() {
    let f = Fixture::new("external-validation");
    f.add_skill("old", "keep me");
    f.add_skill("new", "different");
    let ws = f.ws();
    edit::tag_add(&ws, "old", &["keep".into()]).unwrap();
    assert!(edit::migrate_meta(&ws, "old", "new").is_err());
    std::fs::remove_dir_all(f.root.join("old")).unwrap();
    assert!(edit::migrate_meta(&ws, "old", "missing").is_err());
    std::fs::rename(f.root.join("new"), f.base.join("outside")).unwrap();
    std::os::unix::fs::symlink(f.base.join("outside"), f.root.join("new")).unwrap();
    assert!(edit::migrate_meta(&ws, "old", "new").is_err());
    assert!(ws.meta.exists("old"));
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
    assert!(deploy::apply(&actions).is_err());

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

    // Mixed updates are rejected; keeping local preserves metadata too.
    let mut per_file = std::collections::BTreeMap::new();
    per_file.insert("extra.md".to_string(), Take::Local);
    assert!(update::apply(&ws, &prepared, Take::Upstream, &per_file).is_err());
    let before = ws.meta.load("up").unwrap();
    update::apply(
        &ws,
        &prepared,
        Take::Local,
        &std::collections::BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(ws.meta.load("up").unwrap(), before);
    assert_eq!(
        std::fs::read_to_string(f.root.join("up/extra.md")).unwrap(),
        "extra local\n"
    );
    let prepared = update::prepare(&ws, &ws.scan().unwrap(), "up").unwrap();
    update::apply(
        &ws,
        &prepared,
        Take::Upstream,
        &std::collections::BTreeMap::new(),
    )
    .unwrap();

    assert_eq!(
        std::fs::read_to_string(f.root.join("up/SKILL.md")).unwrap(),
        "---\nname: up\ndescription: v2\n---\nbody v2\n"
    );
    assert_eq!(
        std::fs::read_to_string(f.root.join("up/extra.md")).unwrap(),
        "extra v1\n"
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

/// Presets overlap, report progress per agent scope, and never touch local skills.
#[test]
fn preset_status_activation_and_overlap() {
    use skills::ops::deploy::{
        PresetState, plan_preset_activate, plan_preset_deactivate, preset_status,
    };
    use skills::preset::Preset;

    let f = Fixture::new("preset");
    for k in ["one", "two", "three"] {
        f.add_skill(k, k);
    }
    // A local skill only agent A has, plus one shadowing a managed name.
    std::fs::create_dir_all(f.agent_a.join("local-only")).unwrap();
    std::fs::write(
        f.agent_a.join("local-only/SKILL.md"),
        "---\nname: local-only\n---\n",
    )
    .unwrap();
    std::fs::create_dir_all(f.agent_a.join("two")).unwrap();
    std::fs::write(
        f.agent_a.join("two/SKILL.md"),
        "---\nname: two\n---\ndifferent\n",
    )
    .unwrap();

    let ws = f.ws();
    let daily = Preset {
        name: "daily".into(),
        skills: vec!["one".into(), "two".into()],
        ..Default::default()
    };
    let extra = Preset {
        name: "extra".into(),
        skills: vec!["two".into(), "three".into()],
        ..Default::default()
    };
    ws.presets.save(&daily).unwrap();
    ws.presets.save(&extra).unwrap();
    let scope = vec!["a".to_string(), "b".to_string()];

    // Nothing deployed yet: 2 skills over 2 agents is 4 pairs.
    let snap = ws.scan().unwrap();
    let st = preset_status(&snap, &daily, &scope);
    assert_eq!((st.installed, st.total), (0, 4));
    assert_eq!(st.state(), PresetState::Inactive);

    // A conflict blocks the whole batch until the conflicting source is excluded.
    let actions = plan_preset_activate(&ws, &snap, &daily, &scope).unwrap();
    assert!(deploy::apply(&actions).is_err());
    assert!(!f.agent_b.join("one").exists());
    let actions: Vec<_> = actions
        .into_iter()
        .filter(
            |a| !matches!(a, Action::Link { agent, skill, .. } if agent == "a" && skill == "two"),
        )
        .collect();
    deploy::apply(&actions).unwrap();
    let snap = ws.scan().unwrap();
    assert_eq!(
        snap.agent("a").unwrap().entries["two"],
        EntryState::Shadow {
            same_content: false
        }
    );
    assert_eq!(
        snap.agent("a").unwrap().entries["local-only"],
        EntryState::AgentOnly
    );
    // 3 of 4: one on both agents, two only on b, because a's slot is a local dir.
    let st = preset_status(&snap, &daily, &scope);
    assert_eq!((st.installed, st.total), (3, 4));
    assert_eq!(st.state(), PresetState::Partial);

    // The overlapping preset now reads as partly installed without being touched.
    let st = preset_status(&snap, &extra, &scope);
    assert_eq!(
        (st.installed, st.total),
        (1, 4),
        "only two-on-b is shared and deployed"
    );
    assert_eq!(st.state(), PresetState::Partial);

    // Clicking it fills in the rest.
    let actions = plan_preset_activate(&ws, &snap, &extra, &scope).unwrap();
    assert!(deploy::apply(&actions).is_err());
    let actions: Vec<_> = actions
        .into_iter()
        .filter(
            |a| !matches!(a, Action::Link { agent, skill, .. } if agent == "a" && skill == "two"),
        )
        .collect();
    deploy::apply(&actions).unwrap();
    let snap = ws.scan().unwrap();
    let st = preset_status(&snap, &extra, &scope);
    assert_eq!((st.installed, st.total), (3, 4));

    // Narrowing the scope to one agent recounts against that agent only.
    let st = preset_status(&snap, &extra, &["b".to_string()]);
    assert_eq!((st.installed, st.total), (2, 2));
    assert_eq!(st.state(), PresetState::Active);
    assert_eq!(
        st.progress(),
        None,
        "a preset that is all the way on says so by its colour, not by a count"
    );

    // Deactivating removes every member, including ones shared with daily.
    let actions = plan_preset_deactivate(&ws, &snap, &extra, &scope).unwrap();
    deploy::apply(&actions).unwrap();
    let snap = ws.scan().unwrap();
    let extra_after = preset_status(&snap, &extra, &scope);
    assert_eq!(extra_after.state(), PresetState::Inactive);
    assert_eq!(
        extra_after.progress(),
        None,
        "nor does one that is all the way off"
    );
    let daily_after = preset_status(&snap, &daily, &scope);
    assert_eq!(
        (daily_after.installed, daily_after.total),
        (2, 4),
        "daily lost the shared skill"
    );

    // Local skills survived all of it.
    assert!(f.agent_a.join("local-only/SKILL.md").is_file());
    assert_eq!(
        std::fs::read_to_string(f.agent_a.join("two/SKILL.md")).unwrap(),
        "---\nname: two\n---\ndifferent\n"
    );

    // A member missing from the root is reported, not counted.
    let ghost = Preset {
        name: "ghost".into(),
        skills: vec!["one".into(), "nope".into()],
        ..Default::default()
    };
    let st = preset_status(&snap, &ghost, &scope);
    assert_eq!(st.absent, vec!["nope"]);
    assert_eq!(st.total, 2);
}

/// Applying a plan against a tree that moved on: what is already right is left
/// alone, and what belongs to the agent is still refused.
#[test]
fn apply_tolerates_the_state_it_wanted_and_still_guards_the_agents_own() {
    let f = Fixture::new("tolerant");
    f.add_skill("one", "1");
    f.add_skill("two", "2");
    let ws = f.ws();
    let snap = ws.scan().unwrap();

    let plan =
        deploy::plan_deploy(&ws, &snap, &["one".into(), "two".into()], &["a".into()]).unwrap();
    deploy::apply(&plan).unwrap();

    // Re-applying the same plan is not an error: both links already point home.
    assert_eq!(deploy::apply(&plan).unwrap(), 0, "nothing left to do");
    assert!(f.agent_a.join("one").exists());

    // Removing something a third party already removed is likewise fine, and
    // must not abandon the rest of the batch.
    let snap = ws.scan().unwrap();
    let undo =
        deploy::plan_undeploy(&ws, &snap, &["one".into(), "two".into()], &["a".into()]).unwrap();
    std::fs::remove_file(f.agent_a.join("one")).unwrap();
    deploy::apply(&undo).unwrap();
    assert!(
        !f.agent_a.join("two").exists(),
        "the rest of the batch still ran"
    );

    // But a real directory the agent put there is never deleted.
    let snap = ws.scan().unwrap();
    let plan = deploy::plan_deploy(&ws, &snap, &["one".into()], &["a".into()]).unwrap();
    deploy::apply(&plan).unwrap();
    let snap = ws.scan().unwrap();
    let undo = deploy::plan_undeploy(&ws, &snap, &["one".into()], &["a".into()]).unwrap();
    std::fs::remove_file(f.agent_a.join("one")).unwrap();
    std::fs::create_dir_all(f.agent_a.join("one")).unwrap();
    std::fs::write(
        f.agent_a.join("one/SKILL.md"),
        "---\nname: one\n---\nmine\n",
    )
    .unwrap();
    assert!(
        deploy::apply(&undo).is_err(),
        "refuses to delete what it did not create"
    );
    assert_eq!(
        std::fs::read_to_string(f.agent_a.join("one/SKILL.md")).unwrap(),
        "---\nname: one\n---\nmine\n"
    );
}

/// Whether anything at all is at `path`, a dangling link included.
fn entry_exists(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

#[test]
fn clean_removes_a_broken_link_and_undo_relinks_once_the_skill_is_back() {
    let f = Fixture::new("clean");
    let dir = f.add_skill("printer", "prints");
    f.add_skill("bicycle", "rides");
    let ws = f.ws();
    let snap = ws.scan().unwrap();
    let deploy = deploy::plan_deploy(
        &ws,
        &snap,
        &["printer".into(), "bicycle".into()],
        &["a".into()],
    )
    .unwrap();
    deploy::apply(&deploy).unwrap();

    // The skill leaves the root behind the agent's back.
    std::fs::remove_dir_all(&dir).unwrap();
    let snap = ws.scan().unwrap();
    assert!(matches!(
        snap.agent("a").unwrap().entries["printer"],
        EntryState::Broken { .. }
    ));

    // Naming nothing cleans every broken link and leaves the healthy ones
    // out of it; naming a healthy one is answered rather than ignored.
    let plan = deploy::plan_clean(&ws, &snap, "a", &[]).unwrap();
    assert_eq!(
        plan,
        vec![Action::Unlink {
            agent: "a".into(),
            skill: "printer".into(),
            path: f.agent_a.join("printer"),
        }]
    );
    let named = deploy::plan_clean(&ws, &snap, "a", &["bicycle".into()]).unwrap();
    assert!(matches!(&named[0], Action::Skip { skill, .. } if skill == "bicycle"));

    assert_eq!(deploy::apply(&plan).unwrap(), 1);
    assert!(!entry_exists(&f.agent_a.join("printer")));
    assert!(entry_exists(&f.agent_a.join("bicycle")));

    // To the log a clean is an unlink. While the skill is still gone there
    // is nothing undo can do, and it says so instead of failing.
    let intent = Intent::from_actions(&plan).unwrap();
    assert!(matches!(
        &intent,
        Intent::Links { added, removed }
            if added.is_empty() && removed == &[("printer".to_string(), "a".to_string())]
    ));
    let snap = ws.scan().unwrap();
    assert!(matches!(
        history::undo_plan(&ws, &snap, &intent).unwrap(),
        Plan::Nothing(_)
    ));

    // Once the skill is back, undo puts the link back.
    f.add_skill("printer", "prints again");
    let snap = ws.scan().unwrap();
    let Plan::Links(actions) = history::undo_plan(&ws, &snap, &intent).unwrap() else {
        panic!("undo of a clean should plan links");
    };
    assert_eq!(deploy::apply(&actions).unwrap(), 1);
    assert_eq!(
        link_state(&f.agent_a, "printer").unwrap(),
        f.root.join("printer")
    );
}

#[test]
fn relink_replaces_a_same_content_copy_and_refuses_one_that_differs() {
    let f = Fixture::new("relink");
    f.add_skill("etcd", "keys");
    f.add_skill("msgpack", "bytes");
    std::fs::create_dir_all(f.agent_a.join("etcd")).unwrap();
    std::fs::copy(
        f.root.join("etcd/SKILL.md"),
        f.agent_a.join("etcd/SKILL.md"),
    )
    .unwrap();
    std::fs::create_dir_all(f.agent_a.join("msgpack")).unwrap();
    std::fs::write(
        f.agent_a.join("msgpack/SKILL.md"),
        "---\nname: msgpack\ndescription: my own notes\n---\n",
    )
    .unwrap();
    let ws = f.ws();
    let snap = ws.scan().unwrap();
    let entries = &snap.agent("a").unwrap().entries;
    assert_eq!(entries["etcd"], EntryState::Shadow { same_content: true });
    assert_eq!(
        entries["msgpack"],
        EntryState::Shadow {
            same_content: false
        }
    );

    // One copy is relinked, the other named and left alone; and none of it
    // is offered to the log, since the directory deleted cannot come back.
    let plan = deploy::plan_relink(&ws, &snap, "a", &[]).unwrap();
    assert_eq!(plan.len(), 2);
    assert!(matches!(&plan[0], Action::Relink { skill, .. } if skill == "etcd"));
    assert!(matches!(&plan[1], Action::Skip { skill, .. } if skill == "msgpack"));
    assert!(Intent::from_actions(&plan).is_none());

    // A copy edited between plan and apply no longer matches, and is kept.
    std::fs::write(f.agent_a.join("etcd/notes.txt"), "mine").unwrap();
    assert!(deploy::apply(&plan).is_err());
    assert!(f.agent_a.join("etcd/notes.txt").exists());
    std::fs::remove_file(f.agent_a.join("etcd/notes.txt")).unwrap();

    assert_eq!(deploy::apply(&plan).unwrap(), 1);
    assert_eq!(link_state(&f.agent_a, "etcd").unwrap(), f.root.join("etcd"));
    assert!(f.agent_a.join("msgpack").is_dir());
    assert!(link_state(&f.agent_a, "msgpack").is_none());
    let snap = ws.scan().unwrap();
    assert_eq!(snap.get("etcd").unwrap().deploy["a"], DeployState::Deployed);

    // Asked for by name, the copy that differs is still refused.
    let named = deploy::plan_relink(&ws, &snap, "a", &["msgpack".into()]).unwrap();
    assert!(matches!(&named[0], Action::Skip { .. }));
    let unknown = deploy::plan_relink(&ws, &snap, "a", &["yaml-reader".into()]).unwrap();
    assert!(matches!(&unknown[0], Action::Skip { skill, .. } if skill == "yaml-reader"));
}

/// Run the binary against `root` with `--json` and parse what it printed.
fn skills_json(root: &Path, args: &[&str]) -> Result<serde_json::Value, String> {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_skills"))
        .arg("--root")
        .arg(root)
        .arg("--json")
        .args(args)
        .output()
        .unwrap();
    if out.status.success() {
        Ok(serde_json::from_slice(&out.stdout).unwrap())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).into_owned())
    }
}

#[test]
fn cli_repairs_and_preset_edits_round_trip_as_json() {
    let f = Fixture::new("cli");
    let dir = f.add_skill("yaml-reader", "reads");
    f.add_skill("printer", "prints");
    let ws = f.ws();
    let snap = ws.scan().unwrap();
    let plan = deploy::plan_deploy(&ws, &snap, &["yaml-reader".into()], &["a".into()]).unwrap();
    deploy::apply(&plan).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    std::fs::create_dir_all(f.agent_a.join("printer")).unwrap();
    std::fs::copy(
        f.root.join("printer/SKILL.md"),
        f.agent_a.join("printer/SKILL.md"),
    )
    .unwrap();

    // A plan with changes needs --yes; --dry-run shows it and touches nothing.
    let err = skills_json(&f.root, &["agents", "clean", "a"]).unwrap_err();
    assert!(err.contains("--yes"), "{err}");
    let v = skills_json(&f.root, &["agents", "clean", "a", "--dry-run"]).unwrap();
    assert_eq!(v["dry_run"], true);
    assert_eq!(v["applied"], 0);
    assert_eq!(v["actions"][0]["op"], "unlink");
    assert_eq!(v["actions"][0]["skill"], "yaml-reader");
    assert!(entry_exists(&f.agent_a.join("yaml-reader")));
    let v = skills_json(&f.root, &["agents", "clean", "a", "--yes"]).unwrap();
    assert_eq!(v["applied"], 1);
    assert!(!entry_exists(&f.agent_a.join("yaml-reader")));

    let v = skills_json(&f.root, &["agents", "relink", "a", "printer", "--yes"]).unwrap();
    assert_eq!(v["applied"], 1);
    assert_eq!(v["actions"][0]["op"], "relink");
    assert_eq!(
        link_state(&f.agent_a, "printer").unwrap(),
        f.root.join("printer")
    );
    // With nothing left to delete there is nothing to consent to, and the
    // skips are the answer.
    let v = skills_json(&f.root, &["agents", "relink", "a", "printer"]).unwrap();
    assert_eq!(v["applied"], 0);
    assert_eq!(v["actions"][0]["op"], "skip");

    // Presets: describe, clear, rename, each visible through `show` after.
    ws.presets
        .save(&Preset {
            name: "daily".into(),
            description: None,
            skills: vec!["printer".into()],
            agents: vec![],
        })
        .unwrap();
    let v = skills_json(
        &f.root,
        &["preset", "describe", "daily", "what I reach for every day"],
    )
    .unwrap();
    assert_eq!(v["preset"]["description"], "what I reach for every day");
    let v = skills_json(&f.root, &["preset", "show", "daily"]).unwrap();
    assert_eq!(v["description"], "what I reach for every day");
    let v = skills_json(&f.root, &["preset", "describe", "daily", ""]).unwrap();
    assert!(v["preset"]["description"].is_null());
    let v = skills_json(&f.root, &["preset", "rename", "daily", "weekly"]).unwrap();
    assert_eq!(v["renamed"]["from"], "daily");
    assert_eq!(v["renamed"]["to"], "weekly");
    assert!(ws.presets.load("daily").unwrap().is_none());
    let v = skills_json(&f.root, &["preset", "show", "weekly"]).unwrap();
    assert_eq!(v["skills"][0], "printer");
    assert!(skills_json(&f.root, &["preset", "rename", "daily", "weekly"]).is_err());
}

#[test]
fn aliased_roots_use_the_same_identity_for_adopt_and_deployment() {
    let f = Fixture::new("root-alias");
    let skill = f.add_skill("printer", "prints");
    let alias = f.base.join("root-alias");
    std::os::unix::fs::symlink(&f.root, &alias).unwrap();
    let ws = Workspace::open(&alias).unwrap();
    assert_eq!(ws.root, f.root);

    // Adopting an existing central skill must not try to move it onto itself.
    let before = std::fs::read(skill.join("SKILL.md")).unwrap();
    install::adopt(&ws, &alias.join("printer"), None).unwrap();
    assert_eq!(std::fs::read(skill.join("SKILL.md")).unwrap(), before);
    assert!(ws.meta.exists("printer"));

    std::fs::create_dir_all(&f.agent_a).unwrap();
    std::os::unix::fs::symlink(alias.join("printer"), f.agent_a.join("printer")).unwrap();
    std::os::unix::fs::symlink(&alias, &f.agent_b).unwrap();
    std::os::unix::fs::symlink(alias.join("printer"), f.agent_a.join("other-name")).unwrap();
    let foreign = f.base.join("foreign");
    std::fs::create_dir_all(&foreign).unwrap();
    std::os::unix::fs::symlink(&foreign, f.agent_a.join("foreign")).unwrap();
    std::os::unix::fs::symlink(alias.join("missing"), f.agent_a.join("missing")).unwrap();

    // Also exercise the public scan entry point with an unnormalized path.
    let snap = skills::reconcile::scan(&alias, &ws.config).unwrap();
    assert_eq!(snap.root, f.root);
    assert_eq!(snap.agent("b").unwrap().mode, AgentDirMode::DirLinked);
    for agent in ["a", "b"] {
        assert_eq!(
            snap.get("printer").unwrap().deploy[agent],
            DeployState::Deployed
        );
    }
    let entries = &snap.agent("a").unwrap().entries;
    assert!(matches!(entries["other-name"], EntryState::Foreign { .. }));
    assert!(matches!(entries["foreign"], EntryState::Foreign { .. }));
    assert!(matches!(entries["missing"], EntryState::Broken { .. }));
}

#[test]
fn note_editor_only_saves_successful_content_changes() {
    let f = Fixture::new("note-editor");
    f.add_skill("printer", "Print documents");
    let ws = f.ws();
    let run = |editor: &str| {
        std::process::Command::new(env!("CARGO_BIN_EXE_skills"))
            .args([
                "--root",
                f.root.to_str().unwrap(),
                "note",
                "edit",
                "printer",
            ])
            .env("VISUAL", editor)
            .env("EDITOR", "false")
            .output()
            .unwrap()
    };

    // Quitting an untouched buffer must not turn an unmanaged skill into a
    // managed one merely by creating an empty note and baseline.
    let output = run("true");
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("note unchanged"));
    assert!(ws.meta.load("printer").unwrap().is_none());

    edit::note_set(&ws, "printer", Some("Original note\n")).unwrap();
    let meta_path = ws.meta.path("printer");
    let before = std::fs::read(&meta_path).unwrap();
    let output = run("true");
    assert!(output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("note saved"));
    assert_eq!(std::fs::read(&meta_path).unwrap(), before);

    // Even a modified temporary file is discarded when the editor fails.
    let output = run("sh -c 'printf changed > \"$1\"; exit 1' sh");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("note edit abandoned"));
    assert_eq!(std::fs::read(&meta_path).unwrap(), before);

    let output = run("sh -c 'printf updated > \"$1\"' sh");
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("note saved"));
    assert_eq!(
        ws.meta.load("printer").unwrap().unwrap().note.as_deref(),
        Some("updated")
    );
}
