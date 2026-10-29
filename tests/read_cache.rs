//! Cache correctness and actual loader counts on disposable roots.
use skills::{
    Workspace,
    config::{AgentConfig, Config},
    ops::DownloadDir,
    reconcile,
};
use std::{fs, path::Path};

fn put(root: &Path, key: &str, body: &str) {
    fs::create_dir_all(root.join(key)).unwrap();
    fs::write(
        root.join(key).join("SKILL.md"),
        format!("---\nname: {key}\n---\n{body}"),
    )
    .unwrap();
}

fn fixture() -> (DownloadDir, Workspace) {
    let temp = DownloadDir::new("read-cache").unwrap();
    let root = temp.path().join("library");
    fs::create_dir(&root).unwrap();
    Config {
        agents: vec![],
        ..Default::default()
    }
    .save(&root)
    .unwrap();
    for key in ["alpha", "beta", "gamma"] {
        put(&root, key, "original");
    }
    let ws = Workspace::open(&root).unwrap();
    (temp, ws)
}

fn refresh(ws: &mut Workspace) -> reconcile::Snapshot {
    ws.config = ws.load_config_cached().unwrap();
    let fresh_config = ws.load_config().unwrap();
    assert_eq!(
        serde_json::to_value(&ws.config).unwrap(),
        serde_json::to_value(&fresh_config).unwrap()
    );
    let cached = ws.inventory().unwrap();
    let fresh = reconcile::inventory(&ws.root, &fresh_config).unwrap();
    assert_eq!(
        serde_json::to_value(&cached).unwrap(),
        serde_json::to_value(&fresh).unwrap()
    );
    cached
}

#[test]
fn repeated_mixed_edits_match_fresh_state_and_search_results() {
    use skills::search::{Query, Searcher};
    let (_temp, mut ws) = fixture();
    let mut searcher = Searcher::for_workspace(&ws);
    for step in 0..32 {
        match step % 8 {
            0 => put(&ws.root, "delta", &format!("new token{step}")),
            1 => put(&ws.root, "beta", &format!("replacement token{step}")),
            2 => ws
                .tags
                .save(&skills::tag::Tag {
                    name: "work".into(),
                    skills: vec!["delta".into()],
                    color: None,
                    description: Some(format!("step{step}")),
                })
                .unwrap(),
            3 => ws
                .presets
                .save(&skills::preset::Preset {
                    name: "daily".into(),
                    skills: vec!["beta".into(), "delta".into()],
                    ..Default::default()
                })
                .unwrap(),
            4 => fs::rename(ws.root.join("delta"), ws.root.join("renamed")).unwrap(),
            5 => {
                let mut cfg = ws.load_config().unwrap();
                cfg.tags_enabled = !cfg.tags_enabled;
                cfg.search.weights.body += 0.25;
                cfg.save(&ws.root).unwrap();
            }
            6 => fs::remove_dir_all(ws.root.join("renamed")).unwrap(),
            7 => {
                ws.tags.remove("work").unwrap();
                ws.presets.remove("daily").unwrap();
            }
            _ => unreachable!(),
        }
        let snapshot = refresh(&mut ws);
        searcher.configure(
            ws.config.search.clone(),
            skills::dict::Dictionaries::for_workspace(&ws, &ws.config.search.dictionaries),
        );
        searcher.index(&snapshot.skills);
        let fresh_config = ws.load_config().unwrap();
        let fresh = reconcile::inventory(&ws.root, &fresh_config).unwrap();
        let mut reference = Searcher::new();
        reference.configure(
            fresh_config.search.clone(),
            skills::dict::Dictionaries::load(&ws.root, &fresh_config.search.dictionaries),
        );
        for query in [
            "original",
            "replacement",
            "delta",
            "tag:work",
            "preset:daily",
        ] {
            let query = Query::parse(query);
            let hits = |searcher: &mut Searcher, records: &[skills::reconcile::SkillRecord]| {
                searcher
                    .search(records, &query)
                    .into_iter()
                    .map(|hit| (hit.key, hit.score.to_bits()))
                    .collect::<Vec<_>>()
            };
            assert_eq!(
                hits(&mut searcher, &snapshot.skills),
                hits(&mut reference, &fresh.skills),
                "step {step}"
            );
        }
    }
}

#[test]
fn dictionary_creation_edits_invalid_bytes_deletion_and_recovery_are_fresh() {
    let (_temp, ws) = fixture();
    let path = skills::paths::meta_dir(&ws.root).join(skills::dict::USER_FILE);
    let weights = skills::config::DictionaryWeights {
        tech: 0.0,
        common: 0.0,
        user: 1.0,
    };
    for contents in [
        None,
        Some("amber\t映射\n".as_bytes()),
        Some("cobalt\t映射\n".as_bytes()),
        Some(b"\xff".as_slice()),
        None,
        Some("amber\t映射\n".as_bytes()),
    ] {
        if let Some(contents) = contents {
            fs::write(&path, contents).unwrap();
        } else if path.exists() {
            fs::remove_file(&path).unwrap();
        }
        let cached = skills::dict::Dictionaries::for_workspace(&ws, &weights);
        let fresh = skills::dict::Dictionaries::load(&ws.root, &weights);
        for term in ["amber", "cobalt"] {
            assert_eq!(cached.en_to_zh(term), fresh.en_to_zh(term));
        }
    }
}

#[test]
fn unchanged_reads_are_shared_and_only_changed_documents_reload() {
    let (_temp, mut ws) = fixture();
    refresh(&mut ws);
    let before = ws.reads.stats();
    refresh(&mut ws.clone());
    assert_eq!(ws.reads.stats().loads, before.loads);
    assert!(ws.reads.stats().hits > before.hits);
    put(&ws.root, "beta", "changed");
    let snap = refresh(&mut ws);
    assert_eq!(snap.get("beta").unwrap().body.as_deref(), Some("changed"));
    assert_eq!(ws.reads.stats().loads, before.loads + 1);
    fs::remove_dir_all(ws.root.join("alpha")).unwrap();
    fs::rename(ws.root.join("gamma"), ws.root.join("delta")).unwrap();
    let snap = refresh(&mut ws);
    assert!(snap.get("alpha").is_none());
    assert!(snap.get("gamma").is_none());
    assert!(snap.get("delta").is_some());
    assert_eq!(ws.reads.stats().loads, before.loads + 2);
    ws.reads.clear();
    let before = ws.reads.stats().loads;
    refresh(&mut ws);
    assert_eq!(ws.reads.stats().loads - before, 3); // config + two skills
}

#[test]
fn configuration_and_skill_parse_failures_recover_without_old_values() {
    let (_temp, mut ws) = fixture();
    refresh(&mut ws);
    let config_path = Config::path(&ws.root);
    let original = fs::read(&config_path).unwrap();
    fs::write(&config_path, "[broken").unwrap();
    assert!(ws.load_config_cached().is_err());
    fs::write(&config_path, &original).unwrap();
    refresh(&mut ws);
    fs::write(ws.root.join("beta/SKILL.md"), "---\nname: [\n---\n").unwrap();
    let snapshot = refresh(&mut ws);
    assert!(matches!(
        snapshot.get("beta").unwrap().status,
        reconcile::SkillStatus::Invalid { .. }
    ));
    put(&ws.root, "beta", "recovered");
    assert_eq!(
        refresh(&mut ws).get("beta").unwrap().body.as_deref(),
        Some("recovered")
    );
    fs::remove_file(config_path).unwrap();
    // A missing config must resolve to defaults, not the last cached config.
    assert_eq!(
        serde_json::to_value(ws.load_config_cached().unwrap()).unwrap(),
        serde_json::to_value(ws.load_config().unwrap()).unwrap()
    );
}

#[test]
fn retargeted_document_symlink_reloads_and_broken_target_does_not_linger() {
    let (temp, mut ws) = fixture();
    let first = temp.path().join("first.md");
    let second = temp.path().join("second.md");
    fs::write(&first, "---\nname: alpha\n---\nfirst").unwrap();
    fs::write(&second, "---\nname: alpha\n---\nsecond").unwrap();
    let path = ws.root.join("alpha/SKILL.md");
    fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink(&first, &path).unwrap();
    assert_eq!(
        refresh(&mut ws).get("alpha").unwrap().body.as_deref(),
        Some("first")
    );
    let before = reconcile::watch::stamp(&ws.root, &ws.config).unwrap();
    let link = ws.root.join("replacement-link");
    std::os::unix::fs::symlink(&second, &link).unwrap();
    fs::rename(link, &path).unwrap();
    assert_ne!(
        before,
        reconcile::watch::stamp(&ws.root, &ws.config).unwrap()
    );
    assert_eq!(
        refresh(&mut ws).get("alpha").unwrap().body.as_deref(),
        Some("second")
    );
    fs::remove_file(&second).unwrap();
    let snapshot = refresh(&mut ws);
    assert!(
        snapshot
            .get("alpha")
            .is_none_or(|record| record.body.is_none())
    );
    fs::write(second, "---\nname: alpha\n---\nrestored").unwrap();
    assert_eq!(
        refresh(&mut ws).get("alpha").unwrap().body.as_deref(),
        Some("restored")
    );
}

#[test]
fn store_edits_removals_and_invalid_data_never_return_stale_values() {
    let (_temp, mut ws) = fixture();
    let mut tag = skills::tag::Tag {
        name: "work".into(),
        skills: vec!["alpha".into()],
        color: None,
        description: None,
    };
    ws.tags.save(&tag).unwrap();
    ws.presets
        .save(&skills::preset::Preset {
            name: "daily".into(),
            skills: vec!["beta".into()],
            ..Default::default()
        })
        .unwrap();
    refresh(&mut ws);
    let before = ws.reads.stats().loads;
    refresh(&mut ws);
    assert_eq!(ws.reads.stats().loads, before);
    tag.skills.push("beta".into());
    ws.tags.save(&tag).unwrap();
    let snap = refresh(&mut ws);
    assert_eq!(snap.get("beta").unwrap().tags, ["work"]);
    assert_eq!(ws.reads.stats().loads, before + 1);
    ws.presets.remove("daily").unwrap();
    assert!(refresh(&mut ws).get("beta").unwrap().presets.is_empty());
    let path = ws.tags.path("work");
    fs::write(&path, "not valid TOML [").unwrap();
    assert!(ws.load_config_cached().is_err());
    fs::remove_file(path).unwrap();
    assert!(refresh(&mut ws).get("alpha").unwrap().tags.is_empty());
}

#[test]
fn preserved_mtime_and_atomic_replacement_invalidate_cache_and_watcher() {
    let (_temp, mut ws) = fixture();
    refresh(&mut ws);
    let path = ws.root.join("alpha/SKILL.md");
    let modified = fs::metadata(&path).unwrap().modified().unwrap();
    let stamp = reconcile::watch::stamp(&ws.root, &ws.config).unwrap();
    put(&ws.root, "alpha", "modified"); // same length
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(modified))
        .unwrap();
    assert_ne!(
        stamp,
        reconcile::watch::stamp(&ws.root, &ws.config).unwrap()
    );
    assert_eq!(
        refresh(&mut ws).get("alpha").unwrap().body.as_deref(),
        Some("modified")
    );
    let stamp = reconcile::watch::stamp(&ws.root, &ws.config).unwrap();
    let replacement = ws.root.join("replacement");
    fs::write(&replacement, "---\nname: alpha\n---\nreplaced").unwrap();
    fs::File::options()
        .write(true)
        .open(&replacement)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(modified))
        .unwrap();
    fs::rename(replacement, &path).unwrap();
    assert_ne!(
        stamp,
        reconcile::watch::stamp(&ws.root, &ws.config).unwrap()
    );
    assert_eq!(
        refresh(&mut ws).get("alpha").unwrap().body.as_deref(),
        Some("replaced")
    );
}

#[test]
fn agent_documents_and_symlink_target_edits_are_observed() {
    let (temp, mut ws) = fixture();
    let agent = temp.path().join("agent");
    fs::create_dir(&agent).unwrap();
    let external = temp.path().join("external");
    fs::create_dir(&external).unwrap();
    put(&external, "outside", "original");
    std::os::unix::fs::symlink(external.join("outside"), agent.join("outside")).unwrap();
    ws.config.agents = vec![AgentConfig {
        key: "agent".into(),
        name: "Agent".into(),
        skills_dir: agent.display().to_string(),
    }];
    ws.config.save(&ws.root).unwrap();
    refresh(&mut ws);
    let before = ws.reads.stats().loads;
    let stamp = reconcile::watch::stamp(&ws.root, &ws.config).unwrap();
    put(&external, "outside", "modified");
    assert_ne!(
        stamp,
        reconcile::watch::stamp(&ws.root, &ws.config).unwrap()
    );
    let snapshot = refresh(&mut ws);
    assert_eq!(snapshot.agents[0].documents["outside"].body, "modified");
    assert_eq!(ws.reads.stats().loads, before + 1);
}

#[test]
fn unreadable_symlink_target_does_not_disable_change_detection() {
    let (_temp, ws) = fixture();
    let path = ws.root.join("loop");
    std::os::unix::fs::symlink("loop", &path).unwrap();
    let before = reconcile::watch::stamp(&ws.root, &ws.config).unwrap();
    put(&ws.root, "alpha", "changed");
    assert_ne!(
        before,
        reconcile::watch::stamp(&ws.root, &ws.config).unwrap()
    );
}

#[test]
fn repository_metadata_refreshes_independently_and_bad_edits_are_reported() {
    let (_temp, mut ws) = fixture();
    let repo = skills::repository::Repository {
        alias: "demo".into(),
        name: None,
        kind: skills::meta::SourceKind::Git,
        url: "https://example.test/demo.git".into(),
        branch: "main".into(),
    };
    repo.save(&ws).unwrap();
    let key = "repos/demo/remote";
    let directory = ws.root.join(key);
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("SKILL.md"), "---\nname: remote\n---\nBody").unwrap();
    let mut meta = skills::meta::SkillMeta {
        source: Some(repo.source("remote", None)),
        ..Default::default()
    };
    ws.meta.save(key, &meta).unwrap();
    refresh(&mut ws);
    let before = ws.reads.stats().loads;
    meta.note = Some("changed note".into());
    ws.meta.save(key, &meta).unwrap();
    assert_eq!(refresh(&mut ws).get(key).unwrap().note, meta.note);
    assert_eq!(ws.reads.stats().loads, before + 1);
    let path = ws.meta.path(key);
    let original = fs::read(&path).unwrap();
    fs::write(&path, "[broken").unwrap();
    assert!(ws.inventory().is_err());
    fs::write(path, original).unwrap();
    assert_eq!(refresh(&mut ws).get(key).unwrap().note, meta.note);
}

#[test]
#[ignore = "release-mode inventory benchmark on disposable roots; run with --ignored --nocapture"]
fn inventory_cache_latency() {
    let temp = DownloadDir::new("inventory-cache-bench").unwrap();
    Config {
        agents: vec![],
        ..Default::default()
    }
    .save(temp.path())
    .unwrap();
    let count = 500;
    for i in 0..count {
        put(
            temp.path(),
            &format!("skill-{i}"),
            &"sample text 中文\n".repeat(512),
        );
    }
    let ws = Workspace::open(temp.path()).unwrap();
    ws.inventory().unwrap();
    for scenario in ["cold-cache", "unchanged", "one-document"] {
        for round in 0..3 {
            if scenario == "cold-cache" {
                ws.reads.clear();
            }
            if scenario == "one-document" {
                put(
                    &ws.root,
                    "skill-42",
                    &format!("edited-{round}\n{}", "sample text 中文\n".repeat(512)),
                );
            }
            for cached in if round % 2 == 0 {
                [false, true]
            } else {
                [true, false]
            } {
                let before = ws.reads.stats();
                let start = std::time::Instant::now();
                let snapshot = if cached {
                    ws.inventory()
                } else {
                    reconcile::inventory(&ws.root, &ws.config)
                }
                .unwrap();
                let ms = start.elapsed().as_secs_f64() * 1000.0;
                let stats = ws.reads.stats();
                assert_eq!(snapshot.skills.len(), count);
                println!(
                    "INVENTORY scenario={scenario} cached={cached} round={round} records={count} ms={ms:.3} loads={} hits={}",
                    stats.loads - before.loads,
                    stats.hits - before.hits
                );
            }
        }
    }
}
