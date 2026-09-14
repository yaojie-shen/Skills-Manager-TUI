//! Explicit, root-scoped backup destinations, independent of installation sources.
//! Only bound skills are published. Agent paths, notes and root configuration stay local.
use crate::{
    Workspace,
    hash::hash_directory,
    meta::{Baseline, Source},
    ops::{DownloadDir, git, require_key},
    util::write_atomic,
};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Remote {
    pub url: String,
    pub branch: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Binding {
    pub remote: String,
    pub baseline: Option<String>,
}
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Settings {
    pub remotes: BTreeMap<String, Remote>,
    pub bindings: BTreeMap<String, Binding>,
    #[serde(default)]
    pub excluded: BTreeSet<String>,
    #[serde(default)]
    pub checkpoints: BTreeMap<String, BTreeMap<String, String>>,
}
#[derive(Debug, Default, Serialize, Deserialize)]
struct Manifest {
    skills: BTreeMap<String, Export>,
}
#[derive(Debug, Serialize, Deserialize)]
struct Export {
    hash: String,
    source: Option<Source>,
    #[serde(default)]
    baseline: Option<Baseline>,
}
#[derive(Debug, Serialize)]
pub struct Change {
    pub skill: String,
    pub action: String,
}

fn settings_path(ws: &Workspace) -> PathBuf {
    ws.meta.dir.join(".sync/settings.json")
}
impl Settings {
    pub fn load(ws: &Workspace) -> Result<Self> {
        let p = settings_path(ws);
        safe_path(&ws.root, &p)?;
        match std::fs::read(&p) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }
    fn save(&self, ws: &Workspace) -> Result<()> {
        safe_path(&ws.root, &settings_path(ws))?;
        write_atomic(&settings_path(ws), &serde_json::to_vec_pretty(self)?)
    }
    pub fn add(&mut self, ws: &Workspace, name: &str, url: &str, branch: &str) -> Result<()> {
        let _lock = Lock::workspace(ws)?;
        *self = Self::load(ws)?;
        ensure!(crate::util::valid_skill_key(name), "invalid remote name");
        ensure!(
            !url.is_empty() && !url.starts_with('-'),
            "invalid remote URL"
        );
        git(&["check-ref-format", &format!("refs/heads/{branch}")], None)?;
        ensure!(
            !self.remotes.contains_key(name),
            "remote already exists; use a new name"
        );
        self.remotes.insert(
            name.into(),
            Remote {
                url: url.into(),
                branch: branch.into(),
            },
        );
        self.save(ws)
    }
    pub fn remove(&mut self, ws: &Workspace, name: &str) -> Result<()> {
        let _lock = Lock::workspace(ws)?;
        *self = Self::load(ws)?;
        ensure!(
            !self.bindings.values().any(|b| b.remote == name),
            "remote still has bound skills; unbind or switch them first"
        );
        ensure!(
            self.remotes.remove(name).is_some(),
            "unknown remote: {name}"
        );
        self.checkpoints.remove(name);
        self.save(ws)
    }
    pub fn bind(&mut self, ws: &Workspace, keys: &[String], remote: Option<&str>) -> Result<()> {
        let _lock = Lock::workspace(ws)?;
        *self = Self::load(ws)?;
        if let Some(remote) = remote {
            ensure!(
                self.remotes.contains_key(remote),
                "unknown remote: {remote}"
            );
        }
        for key in keys {
            require_key(key)?;
            if remote.is_some() {
                validate_skill(&ws.root, &ws.skill_path(key))?;
            }
        }
        for key in keys {
            match remote {
                Some(remote) if self.bindings.get(key).is_some_and(|b| b.remote == remote) => {}
                Some(remote) => {
                    self.excluded.remove(key);
                    self.bindings.insert(
                        key.clone(),
                        Binding {
                            remote: remote.into(),
                            baseline: self
                                .checkpoints
                                .get(remote)
                                .and_then(|entries| entries.get(key))
                                .cloned(),
                        },
                    );
                }
                None => {
                    self.bindings.remove(key);
                    self.excluded.insert(key.clone());
                }
            }
        }
        self.save(ws)
    }
}

/// Reject symlinks at every component, including dangling links and directory ancestors.
fn safe_path(root: &Path, path: &Path) -> Result<()> {
    let rel = path.strip_prefix(root).context("path outside sync root")?;
    let mut current = root.to_path_buf();
    for component in rel.components() {
        ensure!(
            matches!(component, std::path::Component::Normal(_)),
            "invalid sync path"
        );
        current.push(component);
        match std::fs::symlink_metadata(&current) {
            Ok(m) => ensure!(
                !m.file_type().is_symlink(),
                "symlink is not allowed in sync path: {}",
                current.display()
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
fn validate_skill(root: &Path, path: &Path) -> Result<()> {
    safe_path(root, path)?;
    ensure!(
        path.join("SKILL.md").is_file(),
        "missing SKILL.md: {}",
        path.display()
    );
    for entry in walkdir::WalkDir::new(path).follow_links(false) {
        let entry = entry?;
        ensure!(
            !entry.file_type().is_symlink(),
            "sync refuses symlinks: {}",
            entry.path().display()
        );
        ensure!(
            entry.file_type().is_file() || entry.file_type().is_dir(),
            "unsupported sync file"
        );
    }
    Ok(())
}
fn copy_content(src: &Path, dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        if crate::util::is_ignored_name(&entry.file_name().to_string_lossy()) {
            continue;
        }
        ensure!(!entry.file_type()?.is_symlink(), "sync refuses symlinks");
        let to = dest.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_content(&entry.path(), &to)?;
        } else {
            std::fs::copy(entry.path(), to)?;
        }
    }
    Ok(())
}
fn checkout(remote: &Remote, progress: &mut dyn FnMut(&str)) -> Result<(DownloadDir, PathBuf)> {
    let tmp = DownloadDir::new("sync")?;
    let dir = tmp.path().join("repo");
    progress("Connecting to sync remote …");
    let refs = git(
        &[
            "ls-remote",
            "--heads",
            &remote.url,
            &format!("refs/heads/{}", remote.branch),
        ],
        None,
    )?;
    if refs.trim().is_empty() {
        let all = git(&["ls-remote", &remote.url], None)?;
        ensure!(
            all.trim().is_empty(),
            "branch {} is missing in a nonempty remote",
            remote.branch
        );
        git(
            &[
                "init",
                "--quiet",
                "--initial-branch",
                &remote.branch,
                dir.to_str().context("non-UTF8 path")?,
            ],
            None,
        )?;
        git(&["remote", "add", "origin", &remote.url], Some(&dir))?;
    } else {
        crate::ops::git_progress(
            &[
                "clone",
                "--progress",
                "--depth",
                "1",
                "--single-branch",
                "--branch",
                &remote.branch,
                "--",
                &remote.url,
                dir.to_str().context("non-UTF8 path")?,
            ],
            progress,
        )?;
    }
    Ok((tmp, dir))
}

/// Push or pull whole skills. Conflicts stop the operation before any skill is changed.
/// Remote deletions never implicitly delete local skills, or vice versa.
pub fn run(
    ws: &Workspace,
    name: &str,
    push: bool,
    selected: &[String],
    dry_run: bool,
    progress: &mut dyn FnMut(&str),
) -> Result<Vec<Change>> {
    // Serialize writers in this root. The lock is separate from persistent settings.
    let _lock = Lock::workspace(ws)?;
    let mut settings = Settings::load(ws)?;
    let remote = settings
        .remotes
        .get(name)
        .with_context(|| format!("unknown sync remote: {name}"))?;
    let (_tmp, dir) = checkout(remote, progress)?;
    let manifest_path = dir.join(".skills-sync.json");
    safe_path(&dir, &manifest_path)?;
    let mut manifest: Manifest = if manifest_path.exists() {
        serde_json::from_slice(&std::fs::read(&manifest_path)?)?
    } else {
        Manifest::default()
    };
    // The manifest carries provenance, but files are authoritative: people can
    // edit skills or add new SKILL.md directories directly in the backup repo.
    for (key, export) in &mut manifest.skills {
        require_key(key)?;
        match export.source.as_mut() {
            Some(Source::Git {
                url,
                subpath,
                branch,
                revision,
            }) => {
                ensure!(
                    !url.is_empty() && !url.starts_with('-'),
                    "invalid Git source URL for {key}"
                );
                if let Some(path) = subpath {
                    crate::repository::validate_subpath(path)?;
                }
                if let Some(branch) = branch {
                    git(&["check-ref-format", &format!("refs/heads/{branch}")], None)?;
                }
                if let Some(revision) = revision {
                    ensure!(
                        matches!(revision.len(), 40 | 64)
                            && revision.bytes().all(|b| b.is_ascii_hexdigit()),
                        "invalid source revision for {key}"
                    );
                }
            }
            Some(Source::Archive {
                url,
                subpath,
                revision,
            }) => {
                ensure!(
                    url.starts_with("https://") || url.starts_with("http://"),
                    "invalid archive source URL for {key}"
                );
                if let Some(path) = subpath {
                    crate::repository::validate_subpath(path)?;
                }
                if let Some(revision) = revision {
                    ensure!(
                        revision.len() == 64 && revision.bytes().all(|b| b.is_ascii_hexdigit()),
                        "invalid archive revision for {key}"
                    );
                }
            }
            Some(Source::Local { path }) => *path = None,
            None => {}
        }
    }
    let content = dir.join("skills");
    safe_path(&dir, &content)?;
    let mut found = BTreeMap::new();
    if content.exists() {
        for entry in walkdir::WalkDir::new(&content).follow_links(false) {
            let entry = entry?;
            ensure!(
                !entry.file_type().is_symlink(),
                "remote sync content contains a symlink"
            );
            if entry.file_name() != "SKILL.md" || !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path().parent().context("skill parent missing")?;
            let key = path
                .strip_prefix(&content)?
                .to_str()
                .context("non-UTF8 skill name")?
                .to_string();
            require_key(&key)?;
            validate_skill(&dir, path)?;
            let mut export = manifest.skills.remove(&key).unwrap_or(Export {
                hash: String::new(),
                source: None,
                baseline: None,
            });
            export.hash = hash_directory(path)?;
            found.insert(key, export);
        }
    }
    manifest.skills = found;
    let remote_keys: Vec<_> = manifest.skills.keys().collect();
    for (i, key) in remote_keys.iter().enumerate() {
        for other in remote_keys.iter().skip(i + 1) {
            ensure!(
                !other.starts_with(&format!("{key}/")),
                "overlapping remote skills: {key}, {other}"
            );
        }
    }
    let keys: Vec<String> = if !selected.is_empty() {
        selected.to_vec()
    } else if push {
        settings
            .bindings
            .iter()
            .filter(|(_, b)| b.remote == name)
            .map(|(k, _)| k.clone())
            .collect()
    } else {
        manifest.skills.keys().cloned().collect()
    };
    let mut changes = Vec::new();
    let mut local_hashes = BTreeMap::new();
    for key in &keys {
        require_key(key)?;
        if !push && settings.excluded.contains(key) {
            if selected.is_empty() {
                continue;
            }
            bail!("{key} was explicitly unbound; bind it again before pulling");
        }
        let binding = settings.bindings.get(key);
        if push {
            ensure!(
                binding.is_some_and(|b| b.remote == name),
                "{key} is not bound to {name}; bind it explicitly first"
            );
        } else if binding.is_some_and(|b| b.remote != name) {
            if selected.is_empty() {
                continue;
            }
            bail!("{key} is bound to another remote; switch its binding first");
        }
        let local = ws.skill_path(key);
        safe_path(&ws.root, &local)?;
        for parent in local.ancestors().skip(1).take_while(|p| *p != ws.root) {
            ensure!(
                !parent.join("SKILL.md").exists(),
                "sync skill would be nested in another local skill: {key}"
            );
        }
        safe_path(&ws.root, &ws.meta.path(key))?;
        let local_hash = if local.exists() {
            validate_skill(&ws.root, &local)?;
            Some(hash_directory(&local)?)
        } else {
            None
        };
        local_hashes.insert(key.clone(), local_hash.clone());
        let _ = ws.meta.load(key)?;
        let remote_hash = manifest.skills.get(key).map(|s| &s.hash);
        let baseline = binding.and_then(|b| b.baseline.as_ref());
        let action = if push {
            ensure!(local_hash.is_some(), "local skill missing: {key}");
            ensure!(
                remote_hash == local_hash.as_ref()
                    || remote_hash.is_none()
                    || remote_hash == baseline,
                "sync conflict for {key}: remote changed; pull or reconcile the copies before pushing"
            );
            if remote_hash == local_hash.as_ref() {
                "unchanged"
            } else {
                "push"
            }
        } else {
            ensure!(remote_hash.is_some(), "remote skill missing: {key}");
            ensure!(
                local_hash.is_none()
                    || local_hash.as_ref() == remote_hash
                    || local_hash.as_ref() == baseline,
                "sync conflict for {key}: local changes would be overwritten; push or reconcile the copies first"
            );
            if local_hash.as_ref() == remote_hash {
                "unchanged"
            } else {
                "pull"
            }
        };
        // Refuse colliding unmanifested remote paths, even in an existing ordinary repo.
        let remote_path = dir.join("skills").join(key);
        safe_path(&dir, &remote_path)?;
        if push && remote_hash.is_none() {
            ensure!(
                !remote_path.exists(),
                "unmanaged remote path already exists: {key}"
            );
        }
        changes.push(Change {
            skill: key.clone(),
            action: action.into(),
        });
    }
    for (i, key) in keys.iter().enumerate() {
        for other in keys.iter().skip(i + 1) {
            ensure!(
                key != other
                    && !key.starts_with(&format!("{other}/"))
                    && !other.starts_with(&format!("{key}/")),
                "overlapping sync skills: {key}, {other}"
            );
        }
    }
    if dry_run {
        return Ok(changes);
    }
    for change in &changes {
        let key = &change.skill;
        progress(&format!("{} {key}", change.action));
        let local = ws.skill_path(key);
        let current = if local.exists() {
            Some(hash_directory(&local)?)
        } else {
            None
        };
        ensure!(
            current == local_hashes[key],
            "local skill changed during sync: {key}; retry"
        );
        if push {
            let path = dir.join("skills").join(key);
            if change.action == "push" {
                if path.exists() {
                    std::fs::remove_dir_all(&path)?;
                }
                copy_content(&local, &path)?;
            }
            let meta = ws.meta.load(key)?.unwrap_or_default();
            let source = meta.source.map(|s| match s {
                Source::Local { .. } => Source::Local { path: None },
                other => other,
            });
            let hash = hash_directory(&path)?;
            ensure!(
                Some(&hash) == local_hashes[key].as_ref(),
                "local skill changed during copy: {key}; retry"
            );
            manifest.skills.insert(
                key.clone(),
                Export {
                    hash,
                    source,
                    baseline: meta.baseline,
                },
            );
        } else if change.action == "pull" {
            let export = &manifest.skills[key];
            safe_path(&ws.root, &ws.meta.dir.join(".staging"))?;
            let staging = crate::ops::fresh_staging(&ws.root, "sync-pull")?;
            copy_content(&dir.join("skills").join(key), &staging)?;
            std::fs::create_dir_all(local.parent().unwrap())?;
            let mut meta = ws.meta.load(key)?.unwrap_or_default();
            // Preserve existing installation provenance and its modification baseline.
            if !local.exists() {
                meta.source = export.source.clone().or(Some(Source::Local { path: None }));
                meta.baseline = export.baseline.clone();
                if !meta.source.as_ref().is_some_and(Source::is_remote) {
                    meta.baseline = Some(Baseline {
                        hash: export.hash.clone(),
                        hash_algo: crate::hash::HASH_ALGO,
                    });
                }
            }
            crate::ops::swap_dir(&ws.root, &local, &staging)?;
            ws.meta.save(key, &meta)?;
        }
    }
    if push && !changes.is_empty() {
        write_atomic(&manifest_path, &serde_json::to_vec_pretty(&manifest)?)?;
        git(
            &["add", "--force", "--", "skills", ".skills-sync.json"],
            Some(&dir),
        )?;
        if !git(&["diff", "--cached", "--name-only"], Some(&dir))?
            .trim()
            .is_empty()
        {
            git(
                &[
                    "-c",
                    "user.name=skills sync",
                    "-c",
                    "user.email=skills-sync@localhost",
                    "commit",
                    "--quiet",
                    "-m",
                    "Sync bound skills",
                ],
                Some(&dir),
            )?;
            progress("Pushing sync commit …");
            git(
                &[
                    "push",
                    "origin",
                    &format!("HEAD:refs/heads/{}", remote.branch),
                ],
                Some(&dir),
            )?;
        }
    }
    for change in &changes {
        settings.checkpoints.entry(name.into()).or_default().insert(
            change.skill.clone(),
            manifest.skills[&change.skill].hash.clone(),
        );
        settings.bindings.insert(
            change.skill.clone(),
            Binding {
                remote: name.into(),
                baseline: Some(manifest.skills[&change.skill].hash.clone()),
            },
        );
    }
    settings.save(ws)?;
    Ok(changes)
}
struct Lock(PathBuf);
impl Lock {
    fn workspace(ws: &Workspace) -> Result<Self> {
        let path = ws.meta.dir.join(".sync/lock");
        safe_path(&ws.root, &path)?;
        std::fs::create_dir_all(path.parent().context("lock parent missing")?)?;
        Self::new(path)
    }
    fn new(path: PathBuf) -> Result<Self> {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .context("another sync is running (if interrupted, remove .skills-meta/.sync/lock)")?;
        Ok(Self(path))
    }
}
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
