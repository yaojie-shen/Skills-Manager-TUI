//! Small filesystem helpers shared by the core.

use anyhow::{Context, Result};
use std::path::Path;

/// Write `data` to `path` via a temporary file in the same directory and an atomic rename.
pub fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    let dir = path.parent().context("path has no parent")?;
    std::fs::create_dir_all(dir)?;
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .context("path has no file name")?;
    let tmp = dir.join(format!(".{file_name}.tmp-{}", std::process::id()));
    std::fs::write(&tmp, data).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("renaming {} to {}", tmp.display(), path.display()))?;
    Ok(())
}

/// Recursively copy a directory. Symlinks are recreated as symlinks.
pub fn copy_dir(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let ft = entry.file_type()?;
        if ft.is_symlink() {
            let target = std::fs::read_link(&from)?;
            std::os::unix::fs::symlink(target, &to)?;
        } else if ft.is_dir() {
            copy_dir(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// Is `path` a symlink (without following it)?
pub fn is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

/// Resolve a symlink target to an absolute, normalized path without requiring it to exist.
pub fn link_target_abs(link: &Path) -> Option<std::path::PathBuf> {
    let target = std::fs::read_link(link).ok()?;
    let abs = if target.is_absolute() {
        target
    } else {
        link.parent()?.join(target)
    };
    Some(normalize(&abs))
}

/// Lexically normalize `.` and `..` components.
pub fn normalize(p: &Path) -> std::path::PathBuf {
    use std::path::Component;
    let mut out = std::path::PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Names that never count as skills or as content.
pub fn is_ignored_name(name: &str) -> bool {
    matches!(name, ".git" | "__pycache__" | ".DS_Store") || name.ends_with(".pyc")
}

/// Directory-name validation for skill keys: one path segment, no hidden names.
pub fn valid_skill_key(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && !name.contains('/')
        && !name.contains('\\')
        && name != ".."
}

/// Whether a Tag or preset store entry name is a document. Hidden names
/// (editor swap files, `.DS_Store`, atomic-write temporaries) and names without
/// a `.toml` extension (`work.toml~`, `README`) are not, and are left alone.
pub(crate) fn is_store_document_name(name: &std::ffi::OsStr) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let name = name.as_bytes();
    !name.starts_with(b".") && name.ends_with(b".toml")
}

/// Reject debris left by an interrupted metadata file-set transaction.
pub(crate) fn reject_interrupted_transaction(
    path: &Path,
    file_type: &std::fs::FileType,
) -> Result<()> {
    if file_type.is_dir()
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(".file-set-"))
    {
        anyhow::bail!(
            "an interrupted metadata transaction left {}; it holds original-N (pre-change files) and stage-N/claimed-N entries; restore what you need (originals are also in .skills-meta/backups when a migration was running), then remove the directory",
            path.display()
        );
    }
    Ok(())
}
