//! Installing skills from remote sources or local paths, and adopting existing directories.

use crate::Workspace;
use crate::hash::{HASH_ALGO, hash_directory};
use crate::meta::{Baseline, SkillMeta, Source};
use crate::ops::{DownloadDir, fresh_staging, require_key, swap_dir};
use crate::skill::{SKILL_FILE, SkillDoc};
use crate::util::copy_dir;
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

pub use crate::ops::source::{InstallRef, parse_ref};

pub struct Fetched {
    /// Directory holding the skill content (inside `workdir`).
    pub skill_dir: PathBuf,
    /// Temporary work directory to delete afterwards.
    pub workdir: PathBuf,
    pub source: Source,
}

/// Fetch a reference into a staging area and locate the skill directory.
pub fn fetch(ws: &Workspace, r: &InstallRef) -> Result<Fetched> {
    match r {
        InstallRef::Local(path) => {
            if !path.join(SKILL_FILE).is_file() {
                bail!("{} has no {SKILL_FILE}", path.display());
            }
            let work = fresh_staging(&ws.root, "local")?;
            copy_dir(path, &work)?;
            Ok(Fetched {
                skill_dir: work.clone(),
                workdir: work,
                source: Source::Local {
                    path: Some(crate::paths::contract_tilde(path)),
                },
            })
        }
        _ => {
            let download = DownloadDir::new("source")?;
            let work = download.path();
            let acquired = crate::ops::source::acquire(r, work, &mut |_| {})?;
            let subpath = r.subpath();
            let url = r.url().context("missing source URL")?;
            let skill_dir = work.join(subpath.unwrap_or(""));
            if !skill_dir.join(SKILL_FILE).is_file() {
                let base = subpath.unwrap_or("");
                let choices: Vec<String> = discover(&skill_dir)
                    .into_iter()
                    .map(|c| {
                        if base.is_empty() {
                            c
                        } else {
                            format!("{base}/{c}")
                        }
                    })
                    .collect();
                if choices.is_empty() {
                    bail!(
                        "no {SKILL_FILE} at {} in {url}",
                        subpath.unwrap_or("the source root")
                    );
                }
                return Err(NotOneSkill { choices }.into());
            }
            crate::ops::source::validate_skill(&skill_dir)?;
            Ok(Fetched {
                skill_dir,
                source: acquired.source(subpath),
                workdir: download.keep(),
            })
        }
    }
}

/// Default skill name for a reference.
pub fn default_name(r: &InstallRef, fetched: &Fetched) -> String {
    match r {
        InstallRef::Local(p) => p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        _ => SkillDoc::load(&fetched.skill_dir)
            .map(|doc| doc.name)
            .unwrap_or_default(),
    }
}

/// Raised when a reference resolves to a directory holding several skills
/// rather than one. Carries the subpaths a caller can offer to choose from.
#[derive(Debug)]
pub struct NotOneSkill {
    pub choices: Vec<String>,
}

impl std::fmt::Display for NotOneSkill {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "holds {} skills; pick one", self.choices.len())
    }
}

impl std::error::Error for NotOneSkill {}

/// Subpaths under `dir` that contain a `SKILL.md`, relative and sorted.
pub fn discover(dir: &Path) -> Vec<String> {
    let mut out: Vec<String> = walkdir::WalkDir::new(dir)
        .max_depth(4)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name() == SKILL_FILE && e.file_type().is_file())
        .filter_map(|e| {
            let rel = e.path().parent()?.strip_prefix(dir).ok()?;
            let s = rel.to_string_lossy().into_owned();
            (!s.is_empty() && !s.split('/').any(|p| p.starts_with('.'))).then_some(s)
        })
        .collect();
    out.sort();
    out
}

/// Install: fetch, validate, move into the root, write metadata. Returns the key.
pub fn install(ws: &Workspace, r: &InstallRef, name: Option<&str>) -> Result<String> {
    let fetched = fetch(ws, r)?;
    let key = name
        .map(|s| s.to_string())
        .unwrap_or_else(|| default_name(r, &fetched));
    let result = (|| -> Result<String> {
        require_key(&key)?;
        let dest = ws.skill_path(&key);
        if dest.exists() || crate::util::is_symlink(&dest) {
            bail!("{key} already exists in the skills root");
        }
        SkillDoc::load(&fetched.skill_dir).context("fetched skill is invalid")?;
        // Strip a nested .git when the skill is the repo root.
        let _ = std::fs::remove_dir_all(fetched.skill_dir.join(".git"));
        // Downloads may be on another filesystem. Publish only a complete
        // skill copied into the central root's staging area, never a partial
        // cross-filesystem copy directly into its final destination.
        if r.is_remote() {
            let staged = fresh_staging(&ws.root, "install")?;
            let placed = (|| -> Result<()> {
                copy_dir(&fetched.skill_dir, &staged)?;
                std::fs::rename(&staged, &dest)?;
                Ok(())
            })();
            let _ = std::fs::remove_dir_all(&staged);
            placed.with_context(|| format!("placing {}", dest.display()))?;
        } else {
            std::fs::rename(&fetched.skill_dir, &dest)
                .with_context(|| format!("placing {}", dest.display()))?;
        }
        let meta = SkillMeta {
            note: None,
            installed_name: Some(SkillDoc::load(&dest)?.name),
            source: Some(fetched.source.clone()),
            baseline: if fetched.source.is_remote() {
                Some(Baseline {
                    hash: hash_directory(&dest)?,
                    hash_algo: HASH_ALGO,
                })
            } else {
                None
            },
        };
        ws.meta.save(&key, &meta)?;
        Ok(key.clone())
    })();
    let _ = std::fs::remove_dir_all(&fetched.workdir);
    result
}

/// Adopt an existing directory. Inside the root: just create metadata. Elsewhere:
/// move it into the root (leaving a symlink behind when it lived in an agent dir).
pub fn adopt(ws: &Workspace, path: &Path, name: Option<&str>) -> Result<String> {
    let path = std::fs::canonicalize(path).with_context(|| format!("{}", path.display()))?;
    SkillDoc::load(&path).with_context(|| format!("{} is not a skill", path.display()))?;
    let key = name.map(|s| s.to_string()).unwrap_or_else(|| {
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    require_key(&key)?;
    let dest = ws.skill_path(&key);
    if path.parent() == Some(ws.root.as_path()) {
        if key != path.file_name().unwrap().to_string_lossy() {
            bail!("cannot rename while adopting a skill already in the root; use `rename`");
        }
        if ws.meta.exists(&key) {
            bail!("{key} already has metadata");
        }
        let meta = SkillMeta {
            source: Some(Source::Local { path: None }),
            ..Default::default()
        };
        ws.meta.save(&key, &meta)?;
        return Ok(key);
    }
    if dest.exists() || crate::util::is_symlink(&dest) {
        bail!("{key} already exists in the skills root");
    }
    let in_agent_dir = ws.config.agents.iter().any(|a| {
        std::fs::canonicalize(a.skills_path())
            .map(|d| Some(d.as_path()) == path.parent())
            .unwrap_or(false)
    });
    // Move via staging copy so a cross-filesystem move still ends with an atomic rename into the root.
    let staged = fresh_staging(&ws.root, "adopt")?;
    copy_dir(&path, &staged)?;
    swap_dir(&ws.root, &dest, &staged)?;
    std::fs::remove_dir_all(&path)
        .with_context(|| format!("removing original {}", path.display()))?;
    if in_agent_dir {
        std::os::unix::fs::symlink(&dest, &path)?;
    }
    let meta = SkillMeta {
        source: Some(Source::Local {
            path: Some(crate::paths::contract_tilde(&path)),
        }),
        ..Default::default()
    };
    ws.meta.save(&key, &meta)?;
    Ok(key)
}

/// Change the recorded source of a skill without touching its content.
pub fn set_source(ws: &Workspace, key: &str, r: &InstallRef) -> Result<SkillMeta> {
    anyhow::ensure!(
        r.is_remote(),
        "Only repository upstream sources are recorded"
    );
    let mut meta = crate::ops::edit::load_or_init(ws, key)?;
    // Resolve ambiguous URLs before recording them so future checks always use
    // the same provider. This operation records no installed baseline.
    meta.source = Some(match r {
        InstallRef::Git {
            url,
            branch,
            subpath,
        } => Source::Git {
            url: url.clone(),
            branch: branch.clone(),
            subpath: subpath.clone(),
            revision: None,
        },
        InstallRef::Archive { url, subpath } => Source::Archive {
            url: url.clone(),
            subpath: subpath.clone(),
            revision: None,
        },
        InstallRef::Url { .. } => {
            let download = DownloadDir::new("source-reference")?;
            let acquired = crate::ops::source::acquire(r, download.path(), &mut |_| {})?;
            let mut source = acquired.source(r.subpath());
            match &mut source {
                Source::Git { revision, .. } | Source::Archive { revision, .. } => *revision = None,
                Source::Local { .. } => unreachable!(),
            }
            source
        }
        InstallRef::Local { .. } => unreachable!(),
    });
    ws.meta.save(key, &meta)?;
    Ok(meta)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_shorthand() {
        let r = parse_ref("foo/bar/skills/baz", None, None).unwrap();
        assert_eq!(
            r,
            InstallRef::Git {
                url: "https://github.com/foo/bar".into(),
                branch: None,
                subpath: Some("skills/baz".into())
            }
        );
    }

    #[test]
    fn parses_tree_url() {
        let r = parse_ref(
            "https://github.com/foo/bar/tree/main/skills/baz",
            None,
            None,
        )
        .unwrap();
        assert_eq!(
            r,
            InstallRef::Git {
                url: "https://github.com/foo/bar".into(),
                branch: Some("main".into()),
                subpath: Some("skills/baz".into())
            }
        );
    }

    #[test]
    fn parses_ssh_url() {
        let r = parse_ref("git@github.com:foo/bar.git", None, Some("x")).unwrap();
        assert_eq!(
            r,
            InstallRef::Git {
                url: "git@github.com:foo/bar.git".into(),
                branch: None,
                subpath: Some("x".into())
            }
        );
    }
}
