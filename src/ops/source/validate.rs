//! Validation shared by source transports before untrusted content is published.

use anyhow::{Context, Result, bail};
use std::path::{Component, Path};

pub(super) const MAX_EXPANDED: u64 = 512 * 1024 * 1024;
pub(super) const MAX_ENTRIES: usize = 20_000;

pub(super) fn skill_tree(root: &Path) -> Result<()> {
    resources(root, MAX_EXPANDED, MAX_ENTRIES)?;
    safety(root)
}

fn resources(root: &Path, byte_limit: u64, entry_limit: usize) -> Result<()> {
    let mut entries = 0usize;
    let mut bytes = 0u64;
    let walker = walkdir::WalkDir::new(root)
        .follow_links(false)
        .min_depth(1)
        .into_iter()
        .filter_entry(|entry| entry.depth() != 1 || entry.file_name() != ".git");
    for entry in walker {
        let entry = entry.context("walking skill tree")?;
        let relative = entry.path().strip_prefix(root)?;
        entries += 1;
        if entries > entry_limit {
            bail!(
                "skill contains too many entries at {} (limit {entry_limit})",
                relative.display()
            );
        }
        let kind = entry.file_type();
        if kind.is_file() {
            bytes = bytes
                .checked_add(entry.metadata()?.len())
                .context("skill file size overflow")?;
            if bytes > byte_limit {
                bail!(
                    "skill exceeds {byte_limit} regular-file bytes at {}",
                    relative.display()
                );
            }
        }
    }
    Ok(())
}

fn safety(root: &Path) -> Result<()> {
    let walker = walkdir::WalkDir::new(root)
        .follow_links(false)
        .min_depth(1)
        .into_iter()
        .filter_entry(|entry| entry.depth() != 1 || entry.file_name() != ".git");
    for entry in walker {
        let entry = entry.context("walking skill tree")?;
        let relative = entry.path().strip_prefix(root)?;
        let kind = entry.file_type();
        if kind.is_symlink() {
            let target = std::fs::read_link(entry.path())?;
            if link_escapes(relative, &target) {
                bail!(
                    "skill symlink {} escapes the skill directory (target {})",
                    relative.display(),
                    target.display()
                );
            }
        } else if !kind.is_file() && !kind.is_dir() {
            bail!(
                "skill contains unsupported special file: {}",
                relative.display()
            );
        }
    }
    Ok(())
}

fn link_escapes(link: &Path, target: &Path) -> bool {
    let mut depth = link
        .parent()
        .map_or(0, |parent| parent.components().count());
    for component in target.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(_) => depth += 1,
            Component::ParentDir if depth > 0 => depth -= 1,
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return true,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::DownloadDir;
    use std::fs;

    #[cfg(unix)]
    #[test]
    fn validates_links_relative_to_the_selected_skill() {
        let temp = DownloadDir::new("source-validation").unwrap();
        let root = temp.path();
        fs::create_dir(root.join("resources")).unwrap();
        fs::write(root.join("resources/data"), "data").unwrap();
        std::os::unix::fs::symlink("resources/data", root.join("inside")).unwrap();
        std::os::unix::fs::symlink("../inside", root.join("resources/also-inside")).unwrap();
        safety(root).unwrap();

        std::os::unix::fs::symlink("../outside", root.join("escape")).unwrap();
        let error = safety(root).unwrap_err().to_string();
        assert!(error.contains("escape") && error.contains("escapes"));
        fs::remove_file(root.join("escape")).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", root.join("absolute")).unwrap();
        let error = safety(root).unwrap_err().to_string();
        assert!(error.contains("absolute") && error.contains("escapes"));
    }

    #[test]
    fn bounds_entries_and_regular_file_bytes() {
        let temp = DownloadDir::new("source-validation-limits").unwrap();
        fs::write(temp.path().join("one"), "1234").unwrap();
        fs::write(temp.path().join("two"), "5").unwrap();
        assert!(
            resources(temp.path(), 4, 10)
                .unwrap_err()
                .to_string()
                .contains("bytes")
        );
        assert!(
            resources(temp.path(), 100, 1)
                .unwrap_err()
                .to_string()
                .contains("entries")
        );
    }
}
