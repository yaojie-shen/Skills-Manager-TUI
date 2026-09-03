//! Write operations. Every function here is user-initiated; scanning lives in `reconcile`.

pub mod deploy;
pub mod edit;
pub mod install;
pub mod update;

use anyhow::{Result, bail};
use std::path::{Path, PathBuf};

/// Staging area on the same filesystem as the root so directory swaps are atomic renames.
pub fn staging_dir(root: &Path) -> Result<PathBuf> {
    let dir = crate::paths::meta_dir(root).join(".staging");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub fn fresh_staging(root: &Path, label: &str) -> Result<PathBuf> {
    let base = staging_dir(root)?;
    let p = base.join(format!("{label}-{}-{}", std::process::id(), nanos()));
    if p.exists() {
        std::fs::remove_dir_all(&p)?;
    }
    Ok(p)
}

fn nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// Replace `dest` with `new_dir` atomically from the point of view of readers:
/// the old directory is renamed aside first, then the new one renamed into place.
pub fn swap_dir(root: &Path, dest: &Path, new_dir: &Path) -> Result<()> {
    let aside = fresh_staging(root, "old")?;
    let had_old = dest.exists() || crate::util::is_symlink(dest);
    if had_old {
        std::fs::rename(dest, &aside)?;
    }
    if let Err(e) = std::fs::rename(new_dir, dest) {
        if had_old {
            let _ = std::fs::rename(&aside, dest);
        }
        bail!("moving new directory into place: {e}");
    }
    if had_old {
        let _ = std::fs::remove_dir_all(&aside);
    }
    Ok(())
}

pub fn require_key(key: &str) -> Result<()> {
    if !crate::util::valid_skill_key(key) {
        bail!("invalid skill name: {key:?}");
    }
    Ok(())
}

/// Run `git` with arguments, returning stdout. Fails with stderr on non-zero exit.
pub fn git(args: &[&str], cwd: Option<&Path>) -> Result<String> {
    let mut cmd = std::process::Command::new("git");
    cmd.args(args);
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    let out = cmd
        .output()
        .map_err(|e| anyhow::anyhow!("running git: {e}"))?;
    if !out.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}
