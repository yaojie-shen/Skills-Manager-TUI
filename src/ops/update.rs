//! Checking upstream for new revisions and updating remote-sourced skills.

use crate::Workspace;
use crate::hash::{HASH_ALGO, hash_directory};
use crate::meta::{Baseline, Source};
use crate::ops::{DownloadDir, fresh_staging};
use crate::reconcile::{SkillStatus, Snapshot};
use crate::util::{copy_dir, is_ignored_name};
use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize)]
pub struct CheckResult {
    pub skill: String,
    #[serde(skip)]
    pub source: Source,
    pub url: String,
    pub branch: Option<String>,
    pub installed: Option<String>,
    pub remote: String,
    pub update_available: bool,
}

/// Compare the tracked Git revision or archive content hash with the installed version.
pub fn check(ws: &Workspace, key: &str) -> Result<CheckResult> {
    UpdateSession::default().check(ws, key, &mut |_| {})
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
    UpdateSession::default().prepare(snap, key, &mut |_| {})
}

/// Share source downloads and Git head checks within one update run.
#[derive(Default)]
pub struct UpdateSession {
    trees: BTreeMap<(String, String, Option<String>), (DownloadDir, String)>,
    heads: BTreeMap<(String, String, Option<String>), String>,
}
impl UpdateSession {
    /// Check each provider/URL/branch once per batch, using the same provider as updates.
    pub fn check(
        &mut self,
        ws: &Workspace,
        key: &str,
        progress: &mut dyn FnMut(&str),
    ) -> Result<CheckResult> {
        let meta = ws
            .meta
            .load(key)?
            .with_context(|| format!("{key} has no metadata"))?;
        let source = meta
            .source
            .filter(Source::is_remote)
            .with_context(|| format!("{key} is not a remote-sourced skill"))?;
        let url = source.url().unwrap().to_owned();
        let branch = source.branch().map(str::to_owned);
        let revision = source.revision().map(str::to_owned);
        let remote = self.latest(&source, progress)?;
        Ok(CheckResult {
            skill: key.to_string(),
            source,
            url,
            branch,
            update_available: revision.as_deref() != Some(remote.as_str()),
            installed: revision,
            remote,
        })
    }

    fn identity(source: &Source) -> (String, String, Option<String>) {
        (
            source.kind().into(),
            source.url().unwrap().into(),
            source.branch().map(str::to_owned),
        )
    }

    fn acquire(&mut self, source: &Source, progress: &mut dyn FnMut(&str)) -> Result<()> {
        let identity = Self::identity(source);
        if !self.trees.contains_key(&identity) {
            progress(&format!("Downloading {} …", identity.1));
            let reference = crate::ops::install::InstallRef::from_source(source)?;
            let tree = DownloadDir::new("update-source")?;
            let acquired =
                crate::ops::source::acquire(&reference, &tree.path().join("source"), progress)?;
            // The fetched revision is authoritative if the remote moved after checking.
            self.heads
                .insert(identity.clone(), acquired.revision.clone());
            self.trees.insert(identity, (tree, acquired.revision));
        }
        Ok(())
    }

    fn latest(&mut self, source: &Source, progress: &mut dyn FnMut(&str)) -> Result<String> {
        let identity = Self::identity(source);
        if !self.heads.contains_key(&identity) {
            progress(&format!("Checking {} …", identity.1));
            if matches!(source, Source::Git { .. }) {
                self.heads.insert(
                    identity.clone(),
                    crate::ops::source::latest_revision(source)?,
                );
            } else {
                self.acquire(source, progress)?;
            }
        }
        Ok(self.heads[&identity].clone())
    }

    pub fn prepare(
        &mut self,
        snap: &Snapshot,
        key: &str,
        progress: &mut dyn FnMut(&str),
    ) -> Result<Prepared> {
        let rec = snap
            .get(key)
            .with_context(|| format!("no such skill: {key}"))?;
        let meta = rec
            .meta
            .clone()
            .with_context(|| format!("{key} has no metadata"))?;
        let source = meta
            .source
            .clone()
            .filter(Source::is_remote)
            .with_context(|| format!("{key} is not a remote-sourced skill"))?;
        let revision = source.revision().map(str::to_owned);
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
        let reference = crate::ops::install::InstallRef::from_source(&source)?;
        let identity = Self::identity(&source);
        if matches!(source, Source::Git { .. }) {
            self.latest(&source, progress)?;
        }
        if matches!(source, Source::Git { .. })
            && rec.status != SkillStatus::Modified
            && let Some(head) = self.heads.get(&identity)
            && revision.as_ref() == Some(head)
        {
            return Ok(Prepared {
                skill: key.into(),
                from_revision: revision,
                to_revision: head.clone(),
                status: rec.status.label().into(),
                new_skills: Vec::new(),
                files: BTreeMap::new(),
                upstream_dir: work.join("upstream"),
                workdir: download.keep(),
                baseline_dir: None,
                expected_name,
                expected_meta: Box::new(meta),
                expected_hash,
            });
        }
        self.acquire(&source, progress)?;
        let (tree, to_revision) = &self.trees[&identity];
        let to_revision = to_revision.clone();
        let source_tree = tree.path().join("source");
        if matches!(source, Source::Git { .. }) {
            crate::ops::git(
                &["checkout", "--quiet", "--detach", &to_revision],
                Some(&source_tree),
            )?;
        }
        let sub = source.subpath().unwrap_or("");
        let upstream_src = if sub.is_empty() {
            source_tree.clone()
        } else {
            source_tree.join(sub)
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
                Some(installed)
                    if installed.kind() == source.kind()
                        && installed.url() == source.url()
                        && installed.branch() == source.branch() =>
                {
                    Some(installed.subpath().unwrap_or(""))
                }
                _ => None,
            })
            .collect();
        let mut new_skills = Vec::new();
        for entry in walkdir::WalkDir::new(&source_tree)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| e.depth() == 0 || !e.file_name().to_string_lossy().starts_with('.'))
        {
            let entry = entry?;
            if entry.file_type().is_file() && entry.file_name() == crate::skill::SKILL_FILE {
                let directory = entry.path().parent().context("missing skill parent")?;
                let path = directory.strip_prefix(&source_tree)?.to_string_lossy();
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
            // Providers with version history can recover a three-way baseline;
            // otherwise differences remain unclassified.
            if let Some(rev) = &revision
                && crate::ops::source::checkout_revision(&reference, &source_tree, rev)?
            {
                let base_src = if sub.is_empty() {
                    source_tree.clone()
                } else {
                    source_tree.join(sub)
                };
                if base_src.is_dir() {
                    let base_dir = work.join("baseline");
                    copy_dir(&base_src, &base_dir)?;
                    let _ = std::fs::remove_dir_all(base_dir.join(".git"));
                    prepared.baseline_dir = Some(base_dir);
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
    if prepared.from_revision.as_deref() == Some(prepared.to_revision.as_str())
        && !prepared.needs_resolution()
    {
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
        if let Some(source) = meta.source.as_mut() {
            source.set_revision(prepared.to_revision.clone());
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
