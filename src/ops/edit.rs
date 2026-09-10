//! Metadata edits (tags, notes, baseline), rename and remove.

use crate::Workspace;
use crate::config::Config;
use crate::hash::{HASH_ALGO, hash_directory};
use crate::meta::{Baseline, SkillMeta};
use crate::ops::{deploy, require_key};
use crate::reconcile::{AgentDirMode, EntryState, Snapshot};
use anyhow::{Context, Result, bail};

/// Load metadata for `key`, creating an in-memory default with a fresh baseline
/// when none exists yet (first write records the current hash, §7.2).
pub fn load_or_init(ws: &Workspace, key: &str) -> Result<SkillMeta> {
    require_key(key)?;
    if let Some(m) = ws.meta.load(key)? {
        return Ok(m);
    }
    let path = ws.skill_path(key);
    if !path.is_dir() {
        bail!("no such skill: {key}");
    }
    let mut meta = SkillMeta::default();
    if let Ok(h) = hash_directory(&path) {
        meta.baseline = Some(Baseline {
            hash: h,
            hash_algo: HASH_ALGO,
        });
    }
    if meta.source.is_none() {
        meta.source = Some(crate::meta::Source::Local { path: None });
    }
    Ok(meta)
}

fn normalize_tag(t: &str) -> String {
    t.trim().to_string()
}

pub fn tag_add(ws: &Workspace, key: &str, tags: &[String]) -> Result<SkillMeta> {
    let mut meta = load_or_init(ws, key)?;
    for t in tags {
        let t = normalize_tag(t);
        if t.is_empty() {
            continue;
        }
        if !meta.tags.iter().any(|x| x == &t) {
            meta.tags.push(t);
        }
    }
    ws.meta.save(key, &meta)?;
    Ok(meta)
}

pub fn tag_remove(ws: &Workspace, key: &str, tags: &[String]) -> Result<SkillMeta> {
    let mut meta = load_or_init(ws, key)?;
    let drop: Vec<String> = tags.iter().map(|t| normalize_tag(t)).collect();
    meta.tags.retain(|t| !drop.contains(t));
    ws.meta.save(key, &meta)?;
    Ok(meta)
}

pub fn tag_set(ws: &Workspace, key: &str, tags: &[String]) -> Result<SkillMeta> {
    let mut meta = load_or_init(ws, key)?;
    meta.tags.clear();
    for t in tags {
        let t = normalize_tag(t);
        if !t.is_empty() && !meta.tags.contains(&t) {
            meta.tags.push(t);
        }
    }
    ws.meta.save(key, &meta)?;
    Ok(meta)
}

/// Rename a tag across every skill. Returns the number of skills touched.
///
/// Renaming onto a tag that already exists is a merge: a skill carrying both
/// ends up with one copy of the new name. The tag's `[[tags]]` entry in the
/// config follows it, so a rename does not cost the tag its colour.
pub fn tag_rename(ws: &Workspace, old: &str, new: &str) -> Result<usize> {
    let new = normalize_tag(new);
    if new.is_empty() {
        bail!("new tag name is empty");
    }
    if new == old {
        return Ok(0);
    }
    let mut n = 0;
    for key in ws.meta.list_keys()? {
        if let Some(mut meta) = ws.meta.load(&key)?
            && meta.tags.iter().any(|t| t == old)
        {
            meta.tags.retain(|t| t != old && t != &new);
            meta.tags.push(new.clone());
            ws.meta.save(&key, &meta)?;
            n += 1;
        }
    }
    Config::rename_tag_entry(&ws.root, old, &new)?;
    Ok(n)
}

/// Delete a tag from every skill. Returns the number of skills touched.
pub fn tag_delete(ws: &Workspace, tag: &str) -> Result<usize> {
    let mut n = 0;
    for key in ws.meta.list_keys()? {
        if let Some(mut meta) = ws.meta.load(&key)?
            && meta.tags.iter().any(|t| t == tag)
        {
            meta.tags.retain(|t| t != tag);
            ws.meta.save(&key, &meta)?;
            n += 1;
        }
    }
    Ok(n)
}

pub fn note_set(ws: &Workspace, key: &str, note: Option<&str>) -> Result<SkillMeta> {
    let mut meta = load_or_init(ws, key)?;
    meta.note = note
        .map(|s| s.trim_end().to_string())
        .filter(|s| !s.is_empty());
    ws.meta.save(key, &meta)?;
    Ok(meta)
}

/// Record the current content hash as the new baseline ("accept local changes").
pub fn accept(ws: &Workspace, key: &str) -> Result<SkillMeta> {
    let mut meta = load_or_init(ws, key)?;
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

/// Complete an externally performed move: preserve metadata, repair links that
/// pointed exactly at the old path, and update preset/installation references.
/// Content matching is only a suggestion; the caller explicitly chooses the pair.
pub fn migrate_meta(ws: &Workspace, old: &str, new: &str) -> Result<()> {
    require_key(old)?;
    require_key(new)?;
    let from = ws.skill_path(old);
    let to = ws.skill_path(new);
    match std::fs::symlink_metadata(&from) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
        Ok(_) => bail!("{old} still exists; use rename to move an existing skill"),
    }
    crate::skill::SkillDoc::load(&to).context("migration destination is not a readable skill")?;
    if ws.scan()?.get(new).is_none_or(|r| r.name.is_none()) {
        bail!("migration destination is outside the supported library layout");
    }
    let resolved = std::fs::canonicalize(&to)?;
    if !resolved.starts_with(&ws.root) || crate::util::is_symlink(&to) {
        bail!("migration destination must be a skill directory inside the root");
    }
    ws.meta
        .load(old)?
        .context("old skill has no metadata to migrate")?;
    if ws.meta.exists(new) {
        bail!("metadata for {new} already exists");
    }
    // Validate every destination before changing anything. Canonicalize agent
    // directories so aliases of a shared directory are handled only once.
    let config = ws.load_config()?;
    let mut dirs = std::collections::BTreeSet::new();
    for a in config.agents.iter().chain(&ws.config.agents) {
        if a.skills_path().is_dir() {
            dirs.insert(std::fs::canonicalize(a.skills_path())?);
        }
    }
    dirs.insert(ws.root.clone()); // Root aliases for nested skills are deployments too.
    let mut links = Vec::new();
    for dir in dirs {
        let link = dir.join(crate::repository::default_deploy_name(old));
        if crate::util::link_target_abs(&link).as_ref() != Some(&from) {
            continue;
        }
        let destination = dir.join(crate::repository::default_deploy_name(new));
        let already_linked = std::fs::canonicalize(&destination).ok().as_ref() == Some(&resolved);
        if destination != link && !already_linked {
            match std::fs::symlink_metadata(&destination) {
                Ok(_) => bail!(
                    "migration blocked: {} already exists",
                    destination.display()
                ),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        links.push((link, destination, already_linked));
    }
    let mut presets = ws.presets.list()?;
    for p in &mut presets {
        if p.skills.iter().any(|s| s == old) {
            for key in &mut p.skills {
                if key == old {
                    *key = new.to_string();
                }
            }
            let mut seen = std::collections::BTreeSet::new();
            p.skills.retain(|key| seen.insert(key.clone()));
        }
    }
    // Metadata is moved last so a failed reference write can be retried with the
    // same old/new pair. Already repaired links are left intact on retry.
    for (link, destination, already_linked) in links {
        if crate::util::link_target_abs(&link).as_ref() != Some(&from) {
            bail!("{} changed during migration; retry", link.display());
        }
        if destination == link {
            let temporary = link.with_file_name(format!(
                ".skills-migrate-{}-{}",
                std::process::id(),
                super::nanos()
            ));
            std::os::unix::fs::symlink(&to, &temporary)?;
            let result = std::fs::rename(&temporary, &link);
            if result.is_err() {
                let _ = std::fs::remove_file(&temporary);
            }
            result?;
        } else {
            if !already_linked {
                std::os::unix::fs::symlink(&to, &destination)?;
            }
            std::fs::remove_file(&link)?;
        }
    }
    for p in presets {
        if ws.presets.load(&p.name)?.as_ref() != Some(&p) {
            ws.presets.save(&p)?;
        }
    }
    super::targets::rename_skill_reference(ws, old, Some(new))?;
    ws.meta.rename(old, new)
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
    let mut log = Vec::new();
    // Drop old links first so no agent sees a dangling link longer than necessary.
    let agents: Vec<String> = snap
        .agents
        .iter()
        .filter(|a| {
            (a.mode == AgentDirMode::Real
                || (a.mode == AgentDirMode::SharedRoot && old.contains('/')))
                && matches!(
                    a.entries.get(&crate::repository::default_deploy_name(old)),
                    Some(EntryState::Deployed)
                )
        })
        .map(|a| a.key.clone())
        .collect();
    let unlink = deploy::plan_undeploy(ws, snap, &[old.to_string()], &agents)?;
    deploy::apply(&unlink)?;
    std::fs::create_dir_all(to.parent().context("missing parent")?)?;
    std::fs::rename(&from, &to).with_context(|| format!("renaming {old} -> {new}"))?;
    log.push(format!("renamed directory {old} -> {new}"));
    ws.meta.rename(old, new)?;
    let mut linked = std::collections::BTreeSet::new();
    for a in &agents {
        let cfg = ws.config.agent(a).context("agent vanished")?;
        let link = cfg
            .skills_path()
            .join(crate::repository::default_deploy_name(new));
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
    super::targets::rename_skill_reference(ws, old, Some(new))?;
    Ok(log)
}

/// Remove a skill: undeploy everywhere, delete the directory, and (unless kept) its metadata.
pub fn remove(ws: &Workspace, snap: &Snapshot, key: &str, keep_meta: bool) -> Result<Vec<String>> {
    require_key(key)?;
    let rec = snap
        .get(key)
        .with_context(|| format!("no such skill: {key}"))?;
    let mut log = Vec::new();
    let agents: Vec<String> = snap
        .agents
        .iter()
        .filter(|a| {
            (a.mode == AgentDirMode::Real || a.mode == AgentDirMode::SharedRoot)
                && a.entries.contains_key(&rec.deployment_name())
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
    super::targets::rename_skill_reference(ws, key, None)?;
    Ok(log)
}
