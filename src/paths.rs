//! Locating the central skills directory and expanding user paths.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

/// Hidden directory inside the skills root that holds all metadata and config.
pub const META_DIR: &str = ".skills-meta";
/// Environment variable naming the skills root.
pub const ROOT_ENV: &str = "SKILLS_HOME";
/// Fallback file containing only the root path.
pub const ROOT_POINTER: &str = "~/.config/skills-tui/root";

pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Expand a leading `~` or `~/` to the home directory. Other paths are returned as-is.
pub fn expand_tilde(p: &str) -> PathBuf {
    if p == "~" {
        return home_dir().unwrap_or_else(|| PathBuf::from("~"));
    }
    if let Some(rest) = p.strip_prefix("~/")
        && let Some(home) = home_dir()
    {
        return home.join(rest);
    }
    PathBuf::from(p)
}

/// Render a path with the home directory replaced by `~` for display and config files.
pub fn contract_tilde(p: &Path) -> String {
    if let Some(home) = home_dir()
        && let Ok(rest) = p.strip_prefix(&home)
    {
        if rest.as_os_str().is_empty() {
            return "~".to_string();
        }
        return format!("~/{}", rest.display());
    }
    p.display().to_string()
}

/// Resolve the skills root: explicit flag, then `SKILLS_HOME`, then the pointer file.
pub fn resolve_root(flag: Option<&Path>) -> Result<PathBuf> {
    let candidate = if let Some(p) = flag {
        p.to_path_buf()
    } else if let Some(v) = std::env::var_os(ROOT_ENV) {
        expand_tilde(&v.to_string_lossy())
    } else {
        let pointer = expand_tilde(ROOT_POINTER);
        match std::fs::read_to_string(&pointer) {
            Ok(s) if !s.trim().is_empty() => expand_tilde(s.trim()),
            _ => bail!(
                "skills root not set: pass --root, export {ROOT_ENV}, or write the path into {ROOT_POINTER}"
            ),
        }
    };
    let root = std::fs::canonicalize(&candidate)
        .with_context(|| format!("skills root does not exist: {}", candidate.display()))?;
    if !root.is_dir() {
        bail!("skills root is not a directory: {}", root.display());
    }
    Ok(root)
}

pub fn meta_dir(root: &Path) -> PathBuf {
    root.join(META_DIR)
}
