//! Checking upstream for new revisions and updating git-sourced skills.

use crate::Workspace;
use crate::hash::{HASH_ALGO, hash_directory};
use crate::meta::{Baseline, Source};
use crate::ops::{DownloadDir, fresh_staging, git};
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
    /// Newly discovered sources are informational; updating never installs them.
    pub new_skills: Vec<String>,
    /// relative path -> classification (only when the skill is modified locally)
    pub files: BTreeMap<String, FileChange>,
    #[serde(skip)]
    pub upstream_dir: PathBuf,
    #[serde(skip)]
    pub workdir: PathBuf,
    #[serde(skip)]
    pub baseline_dir: Option<PathBuf>,
    #[serde(skip)]
    expected_name: String,
    #[serde(skip)]
    expected_meta: Box<crate::meta::SkillMeta>,
    #[serde(skip)]
    expected_hash: String,
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
pub fn prepare(_ws: &Workspace, snap: &Snapshot, key: &str) -> Result<Prepared> {
    let rec = snap
        .get(key)
        .with_context(|| format!("no such skill: {key}"))?;
    let meta = rec
        .meta
        .clone()
        .with_context(|| format!("{key} has no metadata"))?;
    let (url, subpath, branch, revision) = match meta.source.clone() {
        Some(Source::Git {
            url,
            subpath,
            branch,
            revision,
        }) => (url, subpath, branch, revision),
        _ => bail!("{key} is not a git-sourced skill"),
    };
    match rec.status {
        SkillStatus::Repository | SkillStatus::MissingBaseline | SkillStatus::Modified => {}
        ref s => bail!("cannot update a skill in state {}", s.label()),
    }
    let local = crate::skill::SkillDoc::load(&rec.path)?;
    let expected_name = meta
        .installed_name
        .clone()
        .unwrap_or_else(|| local.name.clone());
    anyhow::ensure!(
        local.name == expected_name,
        "local skill name changed; keeping local copy and deployments"
    );
    let expected_hash = hash_directory(&rec.path)?;

    let download = DownloadDir::new("update")?;
    let work = download.path().to_path_buf();
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
        bail!("upstream no longer has a skill at {sub:?}; keeping local copy");
    }
    let upstream = crate::skill::SkillDoc::load(&upstream_src)
        .context("upstream skill is invalid; keeping local copy and deployments")?;
    anyhow::ensure!(
        upstream.name == local.name,
        "upstream skill name changed from {:?} to {:?}; keeping local copy and deployments",
        local.name,
        upstream.name
    );
    let installed_paths: Vec<_> = snap
        .skills
        .iter()
        .filter_map(|s| match &s.source {
            Some(Source::Git {
                url: source_url,
                subpath,
                ..
            }) if source_url == &url => Some(subpath.as_deref().unwrap_or("")),
            _ => None,
        })
        .collect();
    let mut new_skills = Vec::new();
    for entry in walkdir::WalkDir::new(&clone)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| e.depth() == 0 || !e.file_name().to_string_lossy().starts_with('.'))
    {
        let entry = entry?;
        if entry.file_type().is_file() && entry.file_name() == crate::skill::SKILL_FILE {
            let directory = entry.path().parent().context("missing skill parent")?;
            let path = directory.strip_prefix(&clone)?.to_string_lossy();
            if !installed_paths
                .iter()
                .any(|p| *p == path || crate::repository::overlaps(p, &path))
                && crate::skill::SkillDoc::load(directory).is_ok()
            {
                new_skills.push(path.into_owned());
            }
        }
    }
    new_skills.sort();
    let upstream_dir = work.join("upstream");
    copy_dir(&upstream_src, &upstream_dir)?;
    let _ = std::fs::remove_dir_all(upstream_dir.join(".git"));

    let mut prepared = Prepared {
        skill: key.to_string(),
        from_revision: revision.clone(),
        to_revision,
        status: rec.status.label().to_string(),
        new_skills,
        files: BTreeMap::new(),
        upstream_dir,
        workdir: work.clone(),
        baseline_dir: None,
        expected_name,
        expected_meta: Box::new(meta),
        expected_hash,
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
    prepared.workdir = download.keep();
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

/// Apply the complete upstream content, or keep local content and metadata untouched.
pub fn apply(
    ws: &Workspace,
    prepared: &Prepared,
    take: Take,
    per_file: &BTreeMap<String, Take>,
) -> Result<()> {
    anyhow::ensure!(
        per_file.is_empty(),
        "per-file merging is not supported; choose local or upstream for the whole skill"
    );
    if take == Take::Local {
        prepared.cleanup();
        return Ok(());
    }
    let key = &prepared.skill;

    let dest = ws.skill_path(key);
    anyhow::ensure!(
        ws.meta.load(key)?.as_ref() == Some(prepared.expected_meta.as_ref())
            && hash_directory(&dest)? == prepared.expected_hash,
        "local skill or source metadata changed since update was prepared; refresh and retry"
    );
    let result_dir = fresh_staging(&ws.root, "update-result")?;
    let result = (|| -> Result<()> {
        copy_dir(&prepared.upstream_dir, &result_dir)?;
        let document = crate::skill::SkillDoc::load(&result_dir)
            .context("updated skill is invalid; keeping local copy and deployments")?;
        anyhow::ensure!(
            document.name == prepared.expected_name,
            "update would rename the skill; keeping local copy and deployments"
        );

        let mut meta = ws.meta.load(key)?.context("metadata vanished")?;
        if let Some(Source::Git { revision, .. }) = meta.source.as_mut() {
            *revision = Some(prepared.to_revision.clone());
        }
        meta.installed_name = Some(prepared.expected_name.clone());
        meta.baseline = Some(Baseline {
            hash: hash_directory(&result_dir)?,
            hash_algo: HASH_ALGO,
        });
        let backup = fresh_staging(&ws.root, "update-backup")?;
        std::fs::rename(&dest, &backup)?;
        if let Err(error) = std::fs::rename(&result_dir, &dest) {
            std::fs::rename(&backup, &dest).context("could not restore original skill")?;
            return Err(error.into());
        }
        if let Err(error) = ws.meta.save(key, &meta) {
            std::fs::rename(&dest, &result_dir)?;
            std::fs::rename(&backup, &dest).context("could not restore original skill")?;
            return Err(error);
        }
        let _ = std::fs::remove_dir_all(backup);

        prepared.cleanup();
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&result_dir);
    result
}
