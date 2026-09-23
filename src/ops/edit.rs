//! Metadata edits (tags, notes, baseline), rename and remove.

use crate::Workspace;
use crate::config::Config;
use crate::hash::{HASH_ALGO, hash_directory};
use crate::meta::{Baseline, SkillMeta};
use crate::ops::{deploy, require_key};
use crate::reconcile::{AgentDirMode, EntryState, SkillStatus, Snapshot};
use anyhow::{Context, Result, bail};

/// Load metadata without registering a content baseline for local skills.
pub fn load_or_init(ws: &Workspace, key: &str) -> Result<SkillMeta> {
    require_key(key)?;
    if let Some(m) = ws.meta.load(key)? {
        return Ok(m);
    }
    if !ws.skill_path(key).is_dir() {
        bail!("no such skill: {key}");
    }
    Ok(SkillMeta {
        source: Some(crate::meta::Source::Local { path: None }),
        ..Default::default()
    })
}

pub fn tag_add(ws: &Workspace, key: &str, tags: &[String]) -> Result<Vec<String>> {
    let mut current = Config::load(&ws.root)?.skill_tags(key);
    current.extend_from_slice(tags);
    tag_set(ws, key, &current)
}

pub fn tag_remove(ws: &Workspace, key: &str, tags: &[String]) -> Result<Vec<String>> {
    let mut current = Config::load(&ws.root)?.skill_tags(key);
    current.retain(|t| !tags.iter().any(|remove| remove.trim() == t));
    tag_set(ws, key, &current)
}

pub fn tag_set(ws: &Workspace, key: &str, tags: &[String]) -> Result<Vec<String>> {
    require_key(key)?;
    let config = Config::load(&ws.root)?;
    anyhow::ensure!(config.tags_enabled, "Tags are disabled in settings");
    anyhow::ensure!(
        ws.skill_path(key).is_dir() || !config.skill_tags(key).is_empty(),
        "no such skill: {key}"
    );
    let names: std::collections::BTreeSet<_> = tags
        .iter()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect();
    Config::edit_tags(&ws.root, |groups| {
        for tag in groups.iter_mut() {
            tag.skills.retain(|s| s != key);
        }
        for name in &names {
            if let Some(tag) = groups.iter_mut().find(|t| &t.name == name) {
                tag.skills.push(key.into());
            } else {
                groups.push(crate::config::TagConfig {
                    name: name.clone(),
                    skills: vec![key.into()],
                    color: None,
                    description: None,
                });
            }
        }
    })?;
    Ok(names.into_iter().collect())
}

pub fn tag_rename(ws: &Workspace, old: &str, new: &str) -> Result<usize> {
    let new = new.trim();
    anyhow::ensure!(!new.is_empty(), "new tag name is empty");
    let mut count = 0;
    Config::edit_tags(&ws.root, |tags| {
        if let Some(i) = tags.iter().position(|t| t.name == old) {
            let mut tag = tags.remove(i);
            count = tag.skills.len();
            if let Some(target) = tags.iter_mut().find(|t| t.name == new) {
                target.skills.extend(tag.skills);
                if target.color.is_none() {
                    target.color = tag.color;
                }
                if target.description.is_none() {
                    target.description = tag.description;
                }
            } else {
                tag.name = new.into();
                tags.push(tag);
            }
        }
    })?;
    Ok(count)
}

pub fn tag_delete(ws: &Workspace, tag: &str) -> Result<usize> {
    let mut count = 0;
    Config::edit_tags(&ws.root, |tags| {
        tags.retain(|t| {
            if t.name == tag {
                count = t.skills.len();
                false
            } else {
                true
            }
        });
    })?;
    Ok(count)
}

pub fn note_set(ws: &Workspace, key: &str, note: Option<&str>) -> Result<SkillMeta> {
    let mut meta = load_or_init(ws, key)?;
    anyhow::ensure!(
        meta.source
            .as_ref()
            .is_some_and(crate::meta::Source::is_remote),
        "Local skills do not store notes"
    );
    meta.note = note
        .map(|s| s.trim_end().to_string())
        .filter(|s| !s.is_empty());
    ws.meta.save(key, &meta)?;
    Ok(meta)
}

/// Record the current content hash as the new baseline ("accept local changes").
pub fn accept(ws: &Workspace, key: &str) -> Result<SkillMeta> {
    let mut meta = load_or_init(ws, key)?;
    if !meta
        .source
        .as_ref()
        .is_some_and(crate::meta::Source::is_remote)
    {
        bail!("local skills do not track a baseline");
    }
    let path = ws.skill_path(key);
    if !path.is_dir() {
        bail!("no such skill: {key}");
    }
    meta.baseline = Some(Baseline {
        hash: hash_directory(&path)?,
        hash_algo: HASH_ALGO,
    });
    ws.meta.save(key, &meta)?;
    Ok(meta)
}

/// Rename a skill: directory, metadata, and every agent link.
pub fn rename(ws: &Workspace, snap: &Snapshot, old: &str, new: &str) -> Result<Vec<String>> {
    require_key(old)?;
    require_key(new)?;
    let from = ws.skill_path(old);
    let to = ws.skill_path(new);
    if !from.is_dir() {
        bail!("no such skill: {old}");
    }
    if to.exists() || ws.meta.exists(new) {
        bail!("{new} already exists");
    }
    let deployment_name = snap
        .get(old)
        .and_then(|record| record.deployment_name())
        .context("skill has no valid declared name")?
        .to_string();
    let mut log = Vec::new();
    // Drop old links first so no agent sees a dangling link longer than necessary.
    let agents: Vec<String> = snap
        .agents
        .iter()
        .filter(|a| {
            a.mode == AgentDirMode::Real
                && matches!(a.entries.get(&deployment_name), Some(EntryState::Deployed))
        })
        .map(|a| a.key.clone())
        .collect();
    let unlink = deploy::plan_undeploy(ws, snap, &[old.to_string()], &agents)?;
    deploy::apply(&unlink)?;
    std::fs::create_dir_all(to.parent().context("missing parent")?)?;
    std::fs::rename(&from, &to).with_context(|| format!("renaming {old} -> {new}"))?;
    log.push(format!("renamed directory {old} -> {new}"));
    Config::rename_tag_skill(&ws.root, old, Some(new))?;
    ws.meta.rename(old, new)?;
    let mut linked = std::collections::BTreeSet::new();
    for a in &agents {
        let cfg = ws.config.agent(a).context("agent vanished")?;
        let link = cfg.skills_path().join(&deployment_name);
        if link == to || !linked.insert(link.clone()) {
            continue;
        }
        std::os::unix::fs::symlink(&to, &link)?;
        log.push(format!("relinked {a}/{new}"));
    }
    // Presets refer to skills by key.
    for mut p in ws.presets.list()? {
        if p.skills.iter().any(|s| s == old) {
            for s in p.skills.iter_mut() {
                if s == old {
                    *s = new.to_string();
                }
            }
            ws.presets.save(&p)?;
            log.push(format!("updated preset {}", p.name));
        }
    }
    Ok(log)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryRemoveSummary {
    pub alias: String,
    pub keys: Vec<String>,
    pub modified: usize,
    pub missing: usize,
    pub attention: usize,
}

impl RepositoryRemoveSummary {
    pub fn from_snapshot(snap: &Snapshot, alias: &str) -> Result<Self> {
        anyhow::ensure!(
            crate::util::valid_skill_key(alias),
            "invalid repository alias: {alias:?}"
        );
        anyhow::ensure!(
            snap.repositories.contains_key(alias),
            "source not found: {alias}"
        );
        let mut keys = Vec::new();
        let mut modified = 0;
        let mut missing = 0;
        let mut attention = 0;
        for record in snap
            .skills
            .iter()
            .filter(|record| crate::repository::alias_of(&record.key) == Some(alias))
        {
            keys.push(record.key.clone());
            modified += usize::from(record.status == SkillStatus::Modified);
            missing += usize::from(record.status == SkillStatus::Missing);
            attention += usize::from(!matches!(
                record.status,
                SkillStatus::Repository | SkillStatus::Missing | SkillStatus::Modified
            ));
        }
        keys.sort();
        Ok(Self {
            alias: alias.to_string(),
            keys,
            modified,
            missing,
            attention,
        })
    }
}

pub fn remove_repository(ws: &Workspace, alias: &str) -> Result<RepositoryRemoveSummary> {
    let mut current = ws.clone();
    current.config = current.load_config()?;
    let snap = current.scan()?;
    let summary = RepositoryRemoveSummary::from_snapshot(&snap, alias)?;

    let storage = current.root.join("repos").join(alias);
    anyhow::ensure!(
        !crate::util::is_symlink(&storage),
        "repository storage is a symlink; refusing to remove source"
    );
    let storage_root = current.root.join("repos");
    if storage_root.exists() || crate::util::is_symlink(&storage_root) {
        let metadata = std::fs::symlink_metadata(&storage_root)?;
        anyhow::ensure!(
            metadata.file_type().is_dir() && !metadata.file_type().is_symlink(),
            "repository storage is not a real directory; refusing to remove source"
        );
    }

    for key in &summary.keys {
        remove_preserving_deployments(&current, &snap, key, false)
            .with_context(|| format!("removing source {alias} skill {key}"))?;
    }
    crate::repository::Repository::remove(&current, alias)?;
    Ok(summary)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeploymentRemoval {
    Undeploy,
    Preserve,
}

/// Remove a skill: undeploy everywhere, delete the directory, and (unless kept) its metadata.
pub fn remove(ws: &Workspace, snap: &Snapshot, key: &str, keep_meta: bool) -> Result<Vec<String>> {
    remove_with_policy(ws, snap, key, keep_meta, DeploymentRemoval::Undeploy)
}

fn remove_preserving_deployments(
    ws: &Workspace,
    snap: &Snapshot,
    key: &str,
    keep_meta: bool,
) -> Result<Vec<String>> {
    remove_with_policy(ws, snap, key, keep_meta, DeploymentRemoval::Preserve)
}

fn remove_with_policy(
    ws: &Workspace,
    snap: &Snapshot,
    key: &str,
    keep_meta: bool,
    deployment: DeploymentRemoval,
) -> Result<Vec<String>> {
    require_key(key)?;
    let rec = snap
        .get(key)
        .with_context(|| format!("no such skill: {key}"))?;
    let mut log = Vec::new();
    if deployment == DeploymentRemoval::Undeploy {
        let agents: Vec<String> = snap
            .agents
            .iter()
            .filter(|a| {
                a.mode == AgentDirMode::Real
                    && rec
                        .deployment_name()
                        .is_some_and(|name| a.entries.contains_key(name))
            })
            .map(|a| a.key.clone())
            .collect();
        if !agents.is_empty() {
            let unlink = deploy::plan_undeploy(ws, snap, &[key.to_string()], &agents)?;
            for a in &unlink {
                if a.is_change() {
                    log.push(a.describe());
                }
            }
            deploy::apply(&unlink)?;
        }
    }
    if rec.status.is_present() || rec.path.exists() {
        if crate::util::is_symlink(&rec.path) {
            std::fs::remove_file(&rec.path)?;
        } else {
            std::fs::remove_dir_all(&rec.path)
                .with_context(|| format!("removing {}", rec.path.display()))?;
        }
        log.push(format!("removed directory {}", rec.path.display()));
    }
    if !keep_meta {
        ws.meta.remove(key)?;
        log.push("removed metadata".into());
    }
    if !keep_meta {
        Config::rename_tag_skill(&ws.root, key, None)?;
        // A kept metadata record represents a deliberately restorable Missing
        // skill, so keep its Tags and Preset memberships as well.
        for p in ws.presets.list()? {
            if ws.presets.remove_skill(&p.name, key)? {
                log.push(format!("updated preset {}", p.name));
            }
        }
    }
    Ok(log)
}
