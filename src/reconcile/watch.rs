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
            .filter_entry(|e| e.file_name() != ".staging")
        {
            let entry = entry?;
            record(entry.path(), &mut out)?;
        }
    }
    for agent in &config.agents {
        let dir = agent.skills_path();
        record(&dir, &mut out)?;
        if dir.is_dir() {
            for entry in std::fs::read_dir(&dir)? {
                record(&entry?.path(), &mut out)?;
            }
        }
    }
    Ok(out)
}

fn record(path: &Path, out: &mut Stamp) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => {
            out.0.insert(
                path.to_path_buf(),
                (
                    meta.len(),
                    meta.modified().ok(),
                    std::fs::read_link(path).ok(),
                ),
            );
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

fn containers(path: &Path, depth: usize, out: &mut Stamp) -> Result<()> {
    record(path, out)?;
    record(&path.join("SKILL.md"), out)?;
    if depth > 0 && (path.join("SKILL.md").is_file() || crate::util::is_symlink(path) || depth == 3)
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
        record(&child, out)?;
        if child.is_dir() {
            if depth > 0 || name == "local" || name == "repos" {
                containers(&child, depth + 1, out)?;
            } else {
                record(&child.join("SKILL.md"), out)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
