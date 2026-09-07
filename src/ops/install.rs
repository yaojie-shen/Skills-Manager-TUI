//! Installing skills from git or local paths, and adopting existing directories.

use crate::Workspace;
use crate::hash::{HASH_ALGO, hash_directory};
use crate::meta::{Baseline, SkillMeta, Source};
use crate::ops::{fresh_staging, git, require_key, swap_dir};
use crate::skill::{SKILL_FILE, SkillDoc};
use crate::util::copy_dir;
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

/// A parsed install reference.
#[derive(Debug, Clone, PartialEq)]
pub enum InstallRef {
    Local(PathBuf),
    Git {
        url: String,
        branch: Option<String>,
        subpath: Option<String>,
    },
}

/// Parse user input: an existing path, a GitHub `owner/repo[/sub/path]`
/// shorthand, a GitHub tree URL (branch and subpath encoded), or any git URL.
pub fn parse_ref(input: &str, branch: Option<&str>, subpath: Option<&str>) -> Result<InstallRef> {
    let p = crate::paths::expand_tilde(input);
    if p.exists() {
        return Ok(InstallRef::Local(std::fs::canonicalize(p)?));
    }
    let mut url;
    let mut br = branch.map(|s| s.to_string());
    let mut sub = subpath
        .map(|s| s.trim_matches('/').to_string())
        .filter(|s| !s.is_empty());

    if let Some(rest) = input
        .strip_prefix("https://github.com/")
        .or_else(|| input.strip_prefix("http://github.com/"))
    {
        let parts: Vec<&str> = rest.trim_end_matches('/').split('/').collect();
        if parts.len() < 2 {
            bail!("cannot parse GitHub URL: {input}");
        }
        url = format!(
            "https://github.com/{}/{}",
            parts[0],
            parts[1].trim_end_matches(".git")
        );
        if parts.len() >= 4 && parts[2] == "tree" {
            if br.is_none() {
                br = Some(parts[3].to_string());
            }
            if sub.is_none() && parts.len() > 4 {
                sub = Some(parts[4..].join("/"));
            }
        }
    } else if input.starts_with("http://")
        || input.starts_with("https://")
        || input.starts_with("git@")
        || input.starts_with("ssh://")
        || input.starts_with("file://")
        || input.ends_with(".git")
    {
        url = input.to_string();
    } else {
        let parts: Vec<&str> = input.trim_matches('/').split('/').collect();
        if parts.len() < 2 || parts.iter().any(|p| p.is_empty()) {
            bail!("cannot parse reference: {input} (not a path, URL, or owner/repo)");
        }
        url = format!("https://github.com/{}/{}", parts[0], parts[1]);
        if sub.is_none() && parts.len() > 2 {
            sub = Some(parts[2..].join("/"));
        }
    }
    // `url@branch` form, but not `git@host:` prefixes.
    let split = url
        .rsplit_once('@')
        .filter(|(base, b)| {
            !base.is_empty() && !url.starts_with("git@") && !b.contains('/') && !b.contains(':')
        })
        .map(|(base, b)| (base.to_string(), b.to_string()));
    if let Some((base, b)) = split {
        url = base;
        if br.is_none() {
            br = Some(b);
        }
    }
    Ok(InstallRef::Git {
        url,
        branch: br,
        subpath: sub,
    })
}

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
        InstallRef::Git {
            url,
            branch,
            subpath,
        } => {
            let work = fresh_staging(&ws.root, "git")?;
            let mut args = vec!["clone", "--quiet", "--depth", "1"];
            if let Some(b) = branch {
                args.push("--branch");
                args.push(b);
            }
            args.push(url);
            let work_s = work.to_string_lossy().into_owned();
            args.push(&work_s);
            git(&args, None)?;
            let rev = git(&["rev-parse", "HEAD"], Some(&work))?.trim().to_string();
            let resolved_branch = match branch {
                Some(b) => Some(b.clone()),
                None => git(&["rev-parse", "--abbrev-ref", "HEAD"], Some(&work))
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| s != "HEAD"),
            };
            let skill_dir = match subpath {
                Some(s) if !s.is_empty() => work.join(s),
                _ => work.clone(),
            };
            // A repository of many skills is normal; the caller decides which.
            if !skill_dir.join(SKILL_FILE).is_file() {
                let choices = discover(&skill_dir);
                let _ = std::fs::remove_dir_all(&work);
                if choices.is_empty() {
                    bail!(
                        "no {SKILL_FILE} at {} in {url}",
                        subpath.as_deref().unwrap_or("the repository root")
                    );
                }
                return Err(NotOneSkill { choices }.into());
            }
            Ok(Fetched {
                skill_dir,
                workdir: work,
                source: Source::Git {
                    url: url.clone(),
                    subpath: subpath.clone().filter(|s| !s.is_empty()),
                    branch: resolved_branch,
                    revision: Some(rev),
                },
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
        InstallRef::Git { url, subpath, .. } => match subpath {
            Some(s) if !s.is_empty() => Path::new(s)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            _ => {
                let doc = SkillDoc::load(&fetched.skill_dir).ok();
                doc.map(|d| d.name)
                    .filter(|n| crate::util::valid_skill_key(n))
                    .unwrap_or_else(|| {
                        url.trim_end_matches('/')
                            .trim_end_matches(".git")
                            .rsplit('/')
                            .next()
                            .unwrap_or("skill")
                            .to_string()
                    })
            }
        },
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
        // Move the skill dir into place; when it is nested inside workdir, rename works on the same FS.
        std::fs::rename(&fetched.skill_dir, &dest)
            .or_else(|_| copy_dir(&fetched.skill_dir, &dest))
            .with_context(|| format!("placing {}", dest.display()))?;
        let meta = SkillMeta {
            schema: crate::meta::SCHEMA,
            tags: Vec::new(),
            note: None,
            source: Some(fetched.source.clone()),
            baseline: Some(Baseline {
                hash: hash_directory(&dest)?,
                hash_algo: HASH_ALGO,
            }),
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
            schema: crate::meta::SCHEMA,
            source: Some(Source::Local { path: None }),
            baseline: Some(Baseline {
                hash: hash_directory(&dest)?,
                hash_algo: HASH_ALGO,
            }),
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
        schema: crate::meta::SCHEMA,
        source: Some(Source::Local {
            path: Some(crate::paths::contract_tilde(&path)),
        }),
        baseline: Some(Baseline {
            hash: hash_directory(&dest)?,
            hash_algo: HASH_ALGO,
        }),
        ..Default::default()
    };
    ws.meta.save(&key, &meta)?;
    Ok(key)
}

/// Change the recorded source of a skill without touching its content.
pub fn set_source(ws: &Workspace, key: &str, r: &InstallRef) -> Result<SkillMeta> {
    let mut meta = crate::ops::edit::load_or_init(ws, key)?;
    meta.source = Some(match r {
        InstallRef::Local(p) => Source::Local {
            path: Some(crate::paths::contract_tilde(p)),
        },
        InstallRef::Git {
            url,
            branch,
            subpath,
        } => Source::Git {
            url: url.clone(),
            subpath: subpath.clone(),
            branch: branch.clone(),
            revision: None,
        },
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
