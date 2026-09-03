//! Checking upstream for new revisions and updating git-sourced skills.

use crate::Workspace;
use crate::hash::{HASH_ALGO, hash_directory};
use crate::meta::{Baseline, Source};
use crate::ops::{fresh_staging, git, swap_dir};
use crate::reconcile::{SkillStatus, Snapshot};
use crate::util::{copy_dir, is_ignored_name};
use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize)]
pub struct CheckResult {
    pub skill: String,
    pub url: String,
    pub branch: Option<String>,
    pub installed: Option<String>,
    pub remote: String,
    pub update_available: bool,
}

/// `git ls-remote` the tracked branch and compare with the installed revision.
pub fn check(ws: &Workspace, key: &str) -> Result<CheckResult> {
    let meta = ws
        .meta
        .load(key)?
        .with_context(|| format!("{key} has no metadata"))?;
    let (url, branch, revision) = match meta.source {
        Some(Source::Git {
            url,
            branch,
            revision,
            ..
        }) => (url, branch, revision),
        _ => bail!("{key} is not a git-sourced skill"),
    };
    let refspec = branch.clone().unwrap_or_else(|| "HEAD".into());
    let out = git(&["ls-remote", &url, &refspec], None)?;
    let remote = out
        .lines()
        .find_map(|l| {
            let mut it = l.split_whitespace();
            let sha = it.next()?;
            let name = it.next()?;
            let ok = refspec == "HEAD" && name == "HEAD"
                || name == format!("refs/heads/{refspec}")
                || name == format!("refs/tags/{refspec}");
            ok.then(|| sha.to_string())
        })
        .or_else(|| {
            out.lines()
                .next()
                .and_then(|l| l.split_whitespace().next().map(|s| s.to_string()))
        })
        .with_context(|| format!("no ref {refspec} at {url}"))?;
    Ok(CheckResult {
        skill: key.to_string(),
        url,
        branch,
        update_available: revision.as_deref() != Some(remote.as_str()),
        installed: revision,
        remote,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum FileChange {
    Unchanged,
    UpstreamChanged,
    LocalChanged,
    BothChanged,
    /// No baseline available: only known to differ.
    Differs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Take {
    #[default]
    Upstream,
    Local,
}

impl std::str::FromStr for Take {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "upstream" | "remote" | "theirs" => Ok(Take::Upstream),
            "local" | "mine" | "ours" => Ok(Take::Local),
            _ => bail!("expected `local` or `upstream`, got {s:?}"),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Prepared {
    pub skill: String,
    pub from_revision: Option<String>,
    pub to_revision: String,
    pub status: String,
    /// relative path -> classification (only when the skill is modified locally)
    pub files: BTreeMap<String, FileChange>,
    #[serde(skip)]
    pub upstream_dir: PathBuf,
    #[serde(skip)]
    pub workdir: PathBuf,
    #[serde(skip)]
    pub baseline_dir: Option<PathBuf>,
}

impl Prepared {
    pub fn needs_resolution(&self) -> bool {
        self.status == "modified"
    }
    pub fn cleanup(&self) {
        let _ = std::fs::remove_dir_all(&self.workdir);
    }
}

/// Fetch the upstream (and, when the skill is modified, the baseline revision)
/// into staging and classify differences. Nothing in the root is touched.
pub fn prepare(ws: &Workspace, snap: &Snapshot, key: &str) -> Result<Prepared> {
    let rec = snap
        .get(key)
        .with_context(|| format!("no such skill: {key}"))?;
    let meta = rec
        .meta
        .clone()
        .with_context(|| format!("{key} has no metadata"))?;
    let (url, subpath, branch, revision) = match meta.source {
        Some(Source::Git {
            url,
            subpath,
            branch,
            revision,
        }) => (url, subpath, branch, revision),
        _ => bail!("{key} is not a git-sourced skill"),
    };
    match rec.status {
        SkillStatus::Managed { .. } | SkillStatus::Modified => {}
        ref s => bail!("cannot update a skill in state {}", s.label()),
    }
    let work = fresh_staging(&ws.root, "update")?;
    let clone = work.join("clone");
    let mut args = vec!["clone", "--quiet", "--depth", "1"];
    if let Some(b) = &branch {
        args.push("--branch");
        args.push(b);
    }
    args.push(&url);
    let clone_s = clone.to_string_lossy().into_owned();
    args.push(&clone_s);
    git(&args, None)?;
    let to_revision = git(&["rev-parse", "HEAD"], Some(&clone))?
        .trim()
        .to_string();
    let sub = subpath.clone().unwrap_or_default();
    let upstream_src = if sub.is_empty() {
        clone.clone()
    } else {
        clone.join(&sub)
    };
    if !upstream_src.join(crate::skill::SKILL_FILE).is_file() {
        let _ = std::fs::remove_dir_all(&work);
        bail!("upstream no longer has a skill at {sub:?}; keeping local copy");
    }
    let upstream_dir = work.join("upstream");
    copy_dir(&upstream_src, &upstream_dir)?;
    let _ = std::fs::remove_dir_all(upstream_dir.join(".git"));

    let mut prepared = Prepared {
        skill: key.to_string(),
        from_revision: revision.clone(),
        to_revision,
        status: rec.status.label().to_string(),
        files: BTreeMap::new(),
        upstream_dir,
        workdir: work.clone(),
        baseline_dir: None,
    };

    if rec.status == SkillStatus::Modified {
        // Try to materialize the baseline revision for a three-way classification.
        if let Some(rev) = &revision {
            let fetched = git(
                &["fetch", "--quiet", "--depth", "1", "origin", rev],
                Some(&clone),
            )
            .is_ok();
            if fetched && git(&["checkout", "--quiet", rev], Some(&clone)).is_ok() {
                let base_src = if sub.is_empty() {
                    clone.clone()
                } else {
                    clone.join(&sub)
                };
                if base_src.is_dir() {
                    let base_dir = work.join("baseline");
                    copy_dir(&base_src, &base_dir)?;
                    let _ = std::fs::remove_dir_all(base_dir.join(".git"));
                    prepared.baseline_dir = Some(base_dir);
                }
            }
        }
        prepared.files = classify(
            &rec.path,
            &prepared.upstream_dir,
            prepared.baseline_dir.as_deref(),
        )?;
    }
    Ok(prepared)
}

fn list_files(dir: &Path) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut m = BTreeMap::new();
    if !dir.is_dir() {
        return Ok(m);
    }
    for entry in walkdir::WalkDir::new(dir)
        .min_depth(1)
        .into_iter()
        .filter_entry(|e| !is_ignored_name(&e.file_name().to_string_lossy()))
    {
        let entry = entry?;
        if entry.file_type().is_file() {
            let rel = entry
                .path()
                .strip_prefix(dir)?
                .to_string_lossy()
                .into_owned();
            m.insert(rel, std::fs::read(entry.path())?);
        }
    }
    Ok(m)
}

fn classify(
    local: &Path,
    upstream: &Path,
    baseline: Option<&Path>,
) -> Result<BTreeMap<String, FileChange>> {
    let l = list_files(local)?;
    let u = list_files(upstream)?;
    let b = match baseline {
        Some(p) => Some(list_files(p)?),
        None => None,
    };
    let mut out = BTreeMap::new();
    let mut names: Vec<&String> = l.keys().chain(u.keys()).collect();
    names.sort();
    names.dedup();
    for name in names {
        let lv = l.get(name);
        let uv = u.get(name);
        let change = if lv == uv {
            FileChange::Unchanged
        } else if let Some(b) = &b {
            let bv = b.get(name);
            match (lv == bv, uv == bv) {
                (true, false) => FileChange::UpstreamChanged,
                (false, true) => FileChange::LocalChanged,
                _ => FileChange::BothChanged,
            }
        } else {
            FileChange::Differs
        };
        out.insert(name.clone(), change);
    }
    Ok(out)
}

/// Apply a prepared update. `take` is the default side; `per_file` overrides it per path.
pub fn apply(
    ws: &Workspace,
    prepared: &Prepared,
    take: Take,
    per_file: &BTreeMap<String, Take>,
) -> Result<()> {
    let key = &prepared.skill;
    let dest = ws.skill_path(key);
    let result_dir = prepared.workdir.join("result");
    if prepared.needs_resolution() {
        // Start from the chosen default side, then overlay per-file picks.
        let (base_side, other_side) = match take {
            Take::Upstream => (&prepared.upstream_dir, dest.clone()),
            Take::Local => (&dest, prepared.upstream_dir.clone()),
        };
        copy_dir(base_side, &result_dir)?;
        for (rel, side) in per_file {
            if *side == take {
                continue;
            }
            let src = other_side.join(rel);
            let dst = result_dir.join(rel);
            if src.is_file() {
                if let Some(p) = dst.parent() {
                    std::fs::create_dir_all(p)?;
                }
                std::fs::copy(&src, &dst)?;
            } else {
                let _ = std::fs::remove_file(&dst);
            }
        }
    } else {
        copy_dir(&prepared.upstream_dir, &result_dir)?;
    }
    swap_dir(&ws.root, &dest, &result_dir)?;
    let mut meta = ws.meta.load(key)?.context("metadata vanished")?;
    if let Some(Source::Git { revision, .. }) = meta.source.as_mut() {
        *revision = Some(prepared.to_revision.clone());
    }
    meta.baseline = Some(Baseline {
        hash: hash_directory(&dest)?,
        hash_algo: HASH_ALGO,
    });
    ws.meta.save(key, &meta)?;
    prepared.cleanup();
    Ok(())
}
