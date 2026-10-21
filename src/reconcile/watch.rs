//! Cheap polling for library topology and document changes. Never hashes skill trees.

use crate::config::Config;
use anyhow::Result;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stamp(BTreeMap<PathBuf, (u64, Option<SystemTime>, Option<PathBuf>)>);

/// Inspect the supported library containers, SKILL.md files and metadata. Do not
/// walk scripts, repository caches or external symlink trees on every UI tick.
pub fn stamp(root: &Path, config: &Config) -> Result<Stamp> {
    let mut out = Stamp(BTreeMap::new());
    containers(root, 0, &mut out)?;
    let meta = root.join(".skills-meta");
    if meta.is_dir() {
        for entry in walkdir::WalkDir::new(&meta)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| e.file_name() != ".staging" && e.file_name() != ".repair-backups")
        {
            let entry = entry?;
            record(entry.path(), &mut out)?;
        }
    }
    for agent in &config.agents {
        let dir = agent.skills_path();
        let file_type = record(&dir, &mut out)?;
        if recorded_is_dir(&dir, file_type) {
            for entry in std::fs::read_dir(&dir)? {
                record(&entry?.path(), &mut out)?;
            }
        }
    }
    Ok(out)
}

fn record(path: &Path, out: &mut Stamp) -> Result<Option<std::fs::FileType>> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => {
            let file_type = meta.file_type();
            let target = if file_type.is_symlink() {
                std::fs::read_link(path).ok()
            } else {
                None
            };
            out.0.insert(
                path.to_path_buf(),
                (meta.len(), meta.modified().ok(), target),
            );
            return Ok(Some(file_type));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    Ok(None)
}

fn containers(path: &Path, depth: usize, out: &mut Stamp) -> Result<()> {
    let file_type = record(path, out)?;
    containers_recorded(path, depth, file_type, out)
}

fn containers_recorded(
    path: &Path,
    depth: usize,
    file_type: Option<std::fs::FileType>,
    out: &mut Stamp,
) -> Result<()> {
    let skill = path.join("SKILL.md");
    let skill_type = record(&skill, out)?;
    if depth > 0
        && (recorded_is_file(&skill, skill_type)
            || file_type.is_some_and(|file_type| file_type.is_symlink()))
    {
        return Ok(());
    }
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !crate::util::valid_skill_key(&name) {
            continue;
        }
        let child = entry.path();
        let child_type = record(&child, out)?;
        if recorded_is_dir(&child, child_type) {
            if depth > 0 || name == "local" || name == "repos" {
                containers_recorded(&child, depth + 1, child_type, out)?;
            } else {
                record(&child.join("SKILL.md"), out)?;
            }
        }
    }
    Ok(())
}

fn recorded_is_dir(path: &Path, file_type: Option<std::fs::FileType>) -> bool {
    file_type.is_some_and(|file_type| {
        if file_type.is_symlink() {
            path.is_dir()
        } else {
            file_type.is_dir()
        }
    })
}

fn recorded_is_file(path: &Path, file_type: Option<std::fs::FileType>) -> bool {
    file_type.is_some_and(|file_type| {
        if file_type.is_symlink() {
            path.is_file()
        } else {
            file_type.is_file()
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn watched_paths(stamp: &Stamp) -> std::collections::BTreeSet<PathBuf> {
        stamp.0.keys().cloned().collect()
    }

    #[test]
    fn detects_moves_deletes_and_document_edits_without_reading_payloads() {
        let tmp = crate::ops::DownloadDir::new("watch").unwrap();
        let root = tmp.path();
        let config = Config {
            agents: vec![],
            ..Default::default()
        };
        let path = root.join("local/group/one");
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("SKILL.md"), "one").unwrap();
        std::fs::create_dir(path.join("scripts")).unwrap();
        let before = stamp(root, &config).unwrap();
        std::fs::write(path.join("scripts/payload"), "not watched").unwrap();
        assert_eq!(before, stamp(root, &config).unwrap());
        std::fs::write(path.join("SKILL.md"), "changed document").unwrap();
        let edited = stamp(root, &config).unwrap();
        assert_ne!(before, edited);
        std::fs::rename(&path, root.join("local/group/two")).unwrap();
        let moved = stamp(root, &config).unwrap();
        assert_ne!(edited, moved);
        std::fs::remove_dir_all(root.join("local/group/two")).unwrap();
        assert_ne!(moved, stamp(root, &config).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn watch_contract_covers_real_and_symlink_topologies() {
        use std::os::unix::fs::symlink;

        let tmp = crate::ops::DownloadDir::new("watch-file-types").unwrap();
        let root = tmp.path().join("root");
        for path in [
            root.join("flat"),
            root.join("local/group/one"),
            root.join("local/empty"),
            root.join("local/group/linked-document"),
            root.join("repos/demo/one"),
        ] {
            std::fs::create_dir_all(path).unwrap();
        }
        for path in [
            root.join("flat/SKILL.md"),
            root.join("local/group/one/SKILL.md"),
            root.join("repos/demo/one/SKILL.md"),
        ] {
            std::fs::write(path, "document").unwrap();
        }
        std::fs::create_dir_all(root.join("flat/scripts")).unwrap();
        std::fs::write(root.join("flat/scripts/payload"), "ignored").unwrap();

        let external = tmp.path().join("external");
        std::fs::create_dir_all(&external).unwrap();
        std::fs::write(external.join("SKILL.md"), "external document").unwrap();
        symlink(&external, root.join("linked")).unwrap();
        symlink(
            external.join("SKILL.md"),
            root.join("local/group/linked-document/SKILL.md"),
        )
        .unwrap();
        symlink(tmp.path().join("missing"), root.join("dangling")).unwrap();

        let agent = tmp.path().join("agent");
        std::fs::create_dir_all(agent.join("one")).unwrap();
        std::fs::write(agent.join("one/SKILL.md"), "agent document").unwrap();
        let agent_link = tmp.path().join("agent-link");
        symlink(&agent, &agent_link).unwrap();
        let config = Config {
            agents: vec![
                crate::config::AgentConfig {
                    key: "real".into(),
                    name: "Real".into(),
                    skills_dir: agent.display().to_string(),
                },
                crate::config::AgentConfig {
                    key: "linked".into(),
                    name: "Linked".into(),
                    skills_dir: agent_link.display().to_string(),
                },
            ],
            ..Default::default()
        };

        let before = stamp(&root, &config).unwrap();
        let expected: std::collections::BTreeSet<_> = [
            root.clone(),
            root.join("flat"),
            root.join("flat/SKILL.md"),
            root.join("local"),
            root.join("local/group"),
            root.join("local/group/one"),
            root.join("local/group/one/SKILL.md"),
            root.join("local/group/linked-document"),
            root.join("local/group/linked-document/SKILL.md"),
            root.join("local/empty"),
            root.join("repos"),
            root.join("repos/demo"),
            root.join("repos/demo/one"),
            root.join("repos/demo/one/SKILL.md"),
            root.join("linked"),
            root.join("linked/SKILL.md"),
            root.join("dangling"),
            agent.clone(),
            agent.join("one"),
            agent_link.clone(),
            agent_link.join("one"),
        ]
        .into_iter()
        .collect();
        assert_eq!(watched_paths(&before), expected);
        assert_eq!(before.0[&root.join("linked")].2.as_ref(), Some(&external));
        assert_eq!(
            before.0[&root.join("local/group/linked-document/SKILL.md")]
                .2
                .as_ref(),
            Some(&external.join("SKILL.md"))
        );
        assert_eq!(
            before.0[&root.join("dangling")].2.as_ref(),
            Some(&tmp.path().join("missing"))
        );
        assert_eq!(before.0[&agent_link].2.as_ref(), Some(&agent));
        assert!(!before.0.contains_key(&root.join("flat/scripts")));
        assert!(!before.0.contains_key(&root.join("flat/scripts/payload")));

        std::fs::write(external.join("SKILL.md"), "changed external document").unwrap();
        assert_ne!(before, stamp(&root, &config).unwrap());

        let before_retarget = stamp(&root, &config).unwrap();
        let replacement = tmp.path().join("replacement");
        std::fs::create_dir_all(&replacement).unwrap();
        std::fs::write(replacement.join("SKILL.md"), "replacement").unwrap();
        std::fs::remove_file(root.join("linked")).unwrap();
        symlink(&replacement, root.join("linked")).unwrap();
        let retargeted = stamp(&root, &config).unwrap();
        assert_ne!(before_retarget, retargeted);
        assert_eq!(watched_paths(&retargeted), expected);
        assert_eq!(
            retargeted.0[&root.join("linked")].2.as_ref(),
            Some(&replacement)
        );
    }
}
