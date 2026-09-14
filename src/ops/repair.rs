//! Read-only repair plans shared by CLI and TUI. Apply revalidates every item.
use crate::{
    Workspace,
    meta::Source,
    reconcile::{AgentDirMode, EntryState, HealthClass, SkillStatus, Snapshot},
};
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Debug, Clone, Default)]
pub struct Options {
    pub restore_missing: bool,
    pub forget_missing: bool,
    pub clean_links: bool,
    pub keys: Vec<String>,
    pub moves: BTreeMap<String, String>,
}
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Repair {
    Migrate {
        to: String,
        explicit: bool,
    },
    Restore {
        source: PathBuf,
        hash: String,
    },
    Forget,
    CleanLink {
        agent: String,
        path: PathBuf,
        target: PathBuf,
    },
}
#[derive(Debug, Default, Clone, Serialize)]
pub struct Report {
    pub applied: bool,
    pub planned: usize,
    pub repaired: usize,
    pub skipped: usize,
    pub review: usize,
    pub independent: usize,
    pub failed: usize,
    pub backup: Option<PathBuf>,
    pub remaining_issues: Option<usize>,
    pub items: Vec<Item>,
}
#[derive(Debug, Clone, Serialize)]
pub struct Item {
    pub skill: String,
    pub outcome: String,
    pub detail: String,
    pub action: Option<Repair>,
    /// Original skill metadata: do not apply an old dialog to changed records.
    #[serde(skip)]
    metadata: Option<crate::meta::SkillMeta>,
}
impl Report {
    fn push(
        &mut self,
        skill: &str,
        outcome: &str,
        detail: impl Into<String>,
        action: Option<Repair>,
        metadata: Option<crate::meta::SkillMeta>,
    ) {
        match outcome {
            "planned" => self.planned += 1,
            "repaired" => self.repaired += 1,
            "failed" => self.failed += 1,
            "review" => self.review += 1,
            "independent" | "no-op" => self.independent += 1,
            _ => self.skipped += 1,
        }
        self.items.push(Item {
            skill: skill.into(),
            outcome: outcome.into(),
            detail: detail.into(),
            action,
            metadata,
        });
    }
    pub fn lines(&self) -> Vec<String> {
        let mut lines = vec![format!(
            "{} planned · {} repaired · {} blocked · {} review · {} independent/no-op · {} failed",
            self.planned, self.repaired, self.skipped, self.review, self.independent, self.failed
        )];
        if let Some(count) = self.remaining_issues {
            lines.push(format!(
                "{count} faults remain after a fresh scan; review/independent items are listed separately in Health"
            ));
        }
        if let Some(path) = &self.backup {
            lines.push(format!("Metadata backup: {}", path.display()));
        }
        if self.planned == 0 && !self.applied {
            lines.push("No automatic changes available for these options. --apply would not change anything.".into());
        }
        lines.extend(self.items.iter().map(|i| {
            format!(
                "{} {}: {}",
                if i.outcome == "skipped" {
                    "blocked"
                } else {
                    &i.outcome
                },
                i.skill,
                i.detail
            )
        }));
        lines
    }
}
fn unbound(ws: &Workspace, old: &str, new: Option<&str>) -> Result<()> {
    let settings = super::sync::Settings::load(ws)?;
    for key in std::iter::once(old).chain(new) {
        ensure!(
            !settings.bindings.contains_key(key) && !settings.excluded.contains(key),
            "sync binding/exclusion needs manual migration"
        );
    }
    Ok(())
}
fn validate(ws: &Workspace, item: &Item, snap: &Snapshot) -> Result<()> {
    let Some(action) = &item.action else {
        return Ok(());
    };
    if !matches!(action, Repair::CleanLink { .. }) {
        super::require_key(&item.skill)?;
        crate::paths::ensure_local_path(&ws.root, &ws.meta.path(&item.skill))?;
        ensure!(
            ws.meta.load(&item.skill)? == item.metadata,
            "metadata changed since preview; scan again"
        );
    }
    match action {
        Repair::Migrate { to, explicit } => {
            unbound(ws, &item.skill, Some(to))?;
            if !explicit {
                ensure!(
                    matches!(snap.get(&item.skill).map(|s| &s.status), Some(SkillStatus::Renamed { to: current }) if current == to),
                    "move is no longer a unique content match; rescan and review"
                );
            }
            crate::paths::ensure_local_path(&ws.root, &ws.meta.path(to))?;
            super::edit::migrate_meta_checked(ws, &item.skill, to, false, snap)?;
        }
        Repair::Restore { source, hash } => {
            ensure!(
                matches!(
                    snap.get(&item.skill).map(|s| &s.status),
                    Some(SkillStatus::Missing)
                ),
                "skill is no longer missing; scan again"
            );
            crate::paths::ensure_local_path(&ws.root, &ws.skill_path(&item.skill))?;
            ensure!(
                std::fs::symlink_metadata(ws.skill_path(&item.skill))
                    .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound),
                "destination exists; will not overwrite it"
            );
            crate::skill::SkillDoc::load(source)?;
            ensure!(
                crate::hash::hash_directory(source)? == *hash,
                "source changed since the recorded baseline; review before restoring"
            );
        }
        Repair::Forget => {
            crate::paths::ensure_local_path(&ws.root, &ws.skill_path(&item.skill))?;
            ensure!(
                matches!(
                    snap.get(&item.skill).map(|s| &s.status),
                    Some(SkillStatus::Missing)
                ),
                "skill may have moved or returned; refusing to forget it"
            );
            ensure!(
                std::fs::symlink_metadata(ws.skill_path(&item.skill))
                    .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound),
                "skill path exists; refusing to forget it"
            );
        }
        Repair::CleanLink {
            agent,
            path,
            target,
        } => {
            let a = snap.agent(agent).context("agent no longer exists")?;
            ensure!(
                a.mode == AgentDirMode::Real,
                "agent directory is no longer independently managed"
            );
            ensure!(
                path.parent() == Some(a.skills_dir.as_path()),
                "agent path changed"
            );
            ensure!(
                crate::util::link_target_abs(path).as_ref() == Some(target) && !path.exists(),
                "link changed or was restored; will not remove it"
            );
        }
    }
    Ok(())
}

fn local_source(source: &Source) -> Option<PathBuf> {
    match source {
        Source::Local { path: Some(path) } => Some(crate::paths::expand_tilde(path)),
        Source::Git { url, subpath, .. } => {
            let path = PathBuf::from(url.strip_prefix("file://").unwrap_or(url));
            if !path.is_absolute() || !path.is_dir() {
                return None;
            }
            let sub = subpath.as_deref().unwrap_or("");
            if !sub.is_empty() {
                crate::repository::validate_subpath(sub).ok()?;
            }
            Some(path.join(sub))
        }
        _ => None,
    }
}

pub fn plan(ws: &Workspace, options: &Options) -> Result<Report> {
    ensure!(
        !(options.restore_missing && options.forget_missing),
        "choose restoration or forgetting, not both"
    );
    let snap = ws.scan()?;
    for key in options.keys.iter().chain(options.moves.keys()) {
        ensure!(snap.get(key).is_some(), "unknown skill: {key}");
    }
    let mut report = Report::default();
    for s in &snap.skills {
        if !options.keys.is_empty()
            && !options.keys.contains(&s.key)
            && !options.moves.contains_key(&s.key)
        {
            continue;
        }
        let mut reason = String::new();
        let action = if let Some(to) = options.moves.get(&s.key) {
            Some(Repair::Migrate {
                to: to.clone(),
                explicit: true,
            })
        } else if let SkillStatus::Renamed { to } = &s.status {
            Some(Repair::Migrate {
                to: to.clone(),
                explicit: false,
            })
        } else if s.status == SkillStatus::Missing && options.forget_missing {
            Some(Repair::Forget)
        } else if s.status == SkillStatus::Missing && options.restore_missing {
            // Same-named replacements may be edited moves: never recreate duplicates automatically.
            let candidates: Vec<_> = snap
                .skills
                .iter()
                .filter(|r| {
                    r.status.is_present() && r.key.rsplit('/').next() == s.key.rsplit('/').next()
                })
                .map(|r| r.key.as_str())
                .collect();
            if !candidates.is_empty() {
                reason = format!(
                    "possible replacement: {}; compare versions; use --move only for an unmanaged destination, or --forget-missing for the obsolete record",
                    candidates.join(", ")
                );
                None
            } else if let (Some(source), Some(hash)) =
                (s.source.as_ref().and_then(local_source), &s.baseline_hash)
            {
                Some(Repair::Restore {
                    source,
                    hash: hash.clone(),
                })
            } else {
                reason = "no baseline-matching local source: choose a source and reinstall, or archive the obsolete record".into();
                None
            }
        } else {
            None
        };
        if action.is_none() && s.status.is_healthy() {
            continue;
        }
        if let Some(action) = action {
            let detail = match &action {
                Repair::Migrate { to, .. } => format!("migrate metadata and references -> {to}"),
                Repair::Restore { source, .. } => {
                    format!("restore from {} (preserve metadata)", source.display())
                }
                Repair::Forget => {
                    "archive metadata, then remove obsolete record and preset/tag references".into()
                }
                Repair::CleanLink { .. } => unreachable!(),
            };
            let item = Item {
                skill: s.key.clone(),
                outcome: "planned".into(),
                detail,
                action: Some(action),
                metadata: ws.meta.load(&s.key)?,
            };
            match validate(ws, &item, &snap) {
                Ok(()) => {
                    report.planned += 1;
                    report.items.push(item);
                }
                Err(e) => report.push(&s.key, "skipped", format!("{e:#}"), None, None),
            }
        } else {
            if reason.is_empty() {
                reason = match &s.status {
                    SkillStatus::Missing => "missing: choose restore, archive obsolete record, or --move OLD=NEW for an edited move".into(),
                    SkillStatus::Modified => "modified: content is preserved; review changes before accepting a baseline".into(),
                    SkillStatus::Invalid { reason } => format!("invalid: {reason}; fix SKILL.md before deployment"),
                    SkillStatus::CorruptMeta { error } => format!("corrupt metadata: {error}; repair the metadata file"),
                    _ => "review this record".into(),
                };
            }
            report.push(
                &s.key,
                if s.status.health_class() == Some(HealthClass::Review) {
                    "review"
                } else {
                    "skipped"
                },
                reason,
                None,
                None,
            );
        }
    }
    for a in &snap.agents {
        match &a.mode {
            AgentDirMode::Missing => report.push(&a.key, "review", format!("skills directory does not exist: {}. Deploy from Agents if needed; an unused agent does not need a directory.", a.skills_dir.display()), None, None),
            AgentDirMode::DirForeign { target } => report.push(&a.key, "independent", format!("skills directory {} links to external directory {}; preserved, no repair required unless central management is intended", a.skills_dir.display(), target.display()), None, None),
            _ => {}
        }
        for (name, state) in &a.entries {
            if matches!(state, EntryState::Deployed) {
                continue;
            }
            let key = format!("{}/{name}", a.key);
            if let EntryState::Broken { target } = state
                && options.clean_links
                && a.mode == AgentDirMode::Real
                && options.keys.is_empty()
            {
                report.push(
                    &key,
                    "planned",
                    "remove only if still broken after migration/restoration",
                    Some(Repair::CleanLink {
                        agent: a.key.clone(),
                        path: a.skills_dir.join(name),
                        target: target.clone(),
                    }),
                    None,
                );
            } else {
                let reason = match state {
                    EntryState::Broken { .. } => {
                        "broken link: repair moves/restore first, then select clean broken links"
                    }
                    EntryState::Foreign { .. } => {
                        "external link: preserved; adopt explicitly if central management is intended"
                    }
                    EntryState::Shadow { .. } => {
                        "agent copy: compare before relinking; local extras are preserved"
                    }
                    _ => {
                        "agent-only: preserved; adopt explicitly if central management is intended"
                    }
                };
                report.push(
                    &key,
                    match state.health_class() {
                        Some(HealthClass::Independent) => "independent",
                        Some(HealthClass::Review) => "review",
                        _ => "skipped",
                    },
                    reason,
                    None,
                    None,
                );
            }
        }
    }
    Ok(report)
}

fn backup(ws: &Workspace) -> Result<PathBuf> {
    let dest = ws.meta.dir.join(".repair-backups").join(format!(
        "{}-{}",
        std::process::id(),
        super::nanos()
    ));
    crate::paths::ensure_local_path(&ws.root, &dest)?;
    std::fs::create_dir_all(&dest)?;
    // Preserve all metadata, presets and the deployment registry; never recurse into caches/backups.
    for e in walkdir::WalkDir::new(&ws.meta.dir)
        .min_depth(1)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            e.file_type().is_file() || !e.file_name().to_string_lossy().starts_with('.')
        })
    {
        let e = e?;
        if e.file_type().is_file() && e.path().extension().is_some_and(|s| s == "toml") {
            let to = dest.join(e.path().strip_prefix(&ws.meta.dir)?);
            std::fs::create_dir_all(to.parent().unwrap())?;
            std::fs::copy(e.path(), to)?;
        }
    }
    Ok(dest)
}
fn execute(ws: &Workspace, item: &Item, snap: &Snapshot) -> Result<()> {
    validate(ws, item, snap)?;
    match item.action.as_ref().context("no action")? {
        Repair::Migrate { to, .. } => {
            super::edit::migrate_meta_checked(ws, &item.skill, to, true, snap)
        }
        Repair::Restore { source, hash } => {
            crate::paths::ensure_local_path(&ws.root, &ws.meta.dir.join(".staging"))?;
            let staged = super::fresh_staging(&ws.root, "restore")?;
            let result = (|| {
                crate::util::copy_dir(source, &staged)?;
                ensure!(
                    crate::hash::hash_directory(&staged)? == *hash,
                    "source changed during copy"
                );
                validate(ws, item, snap)?;
                let target = ws.skill_path(&item.skill);
                std::fs::create_dir_all(target.parent().unwrap())?;
                std::fs::rename(&staged, target)?;
                Ok(())
            })();
            let _ = std::fs::remove_dir_all(&staged);
            result
        }
        Repair::Forget => {
            let mut presets = ws.presets.list()?;
            for p in &mut presets {
                if p.skills.contains(&item.skill) {
                    p.skills.retain(|key| key != &item.skill);
                    ws.presets.save(p)?;
                }
            }
            crate::config::Config::rename_tag_skill(&ws.root, &item.skill, None)?;
            ws.meta.remove(&item.skill)
        }
        Repair::CleanLink { path, .. } => {
            std::fs::remove_file(path)?;
            Ok(())
        }
    }
}
/// Apply exactly the previewed items. Never expand the batch using a new scan.
pub fn apply(ws: &Workspace, preview: &Report) -> Result<Report> {
    let mut report = Report {
        applied: true,
        ..Default::default()
    };
    if preview.planned > 0 {
        report.backup = Some(backup(ws)?);
    }
    for item in &preview.items {
        if item.action.is_none() {
            report.push(&item.skill, &item.outcome, &item.detail, None, None);
            continue;
        }
        let fresh = if matches!(item.action, Some(Repair::CleanLink { .. })) {
            ws.scan_for_links()?
        } else {
            ws.scan()?
        };
        if let Some(Repair::CleanLink { path, .. }) = &item.action
            && (path.exists() || std::fs::symlink_metadata(path).is_err())
        {
            report.push(
                &item.skill,
                "no-op",
                "link already repaired or removed by an earlier operation",
                None,
                None,
            );
            continue;
        }
        match execute(ws, item, &fresh) {
            Ok(()) => report.push(&item.skill, "repaired", &item.detail, None, None),
            Err(e) => report.push(
                &item.skill,
                "failed",
                format!("{e:#}; inspect status before retrying; metadata backup retained"),
                None,
                None,
            ),
        }
    }
    let fresh = ws.scan()?;
    report.remaining_issues = Some(
        fresh
            .skills
            .iter()
            .filter(|s| s.status.health_class() == Some(HealthClass::Fault))
            .count()
            + fresh
                .agents
                .iter()
                .map(|a| {
                    a.entries
                        .values()
                        .filter(|s| s.health_class() == Some(HealthClass::Fault))
                        .count()
                })
                .sum::<usize>(),
    );
    Ok(report)
}
pub fn run(ws: &Workspace, apply_changes: bool) -> Result<Report> {
    run_with_options(ws, apply_changes, &Options::default())
}
pub fn run_with_options(ws: &Workspace, apply_changes: bool, options: &Options) -> Result<Report> {
    let preview = plan(ws, options)?;
    if apply_changes {
        apply(ws, &preview)
    } else {
        Ok(preview)
    }
}

/// Reconcile on interactive startup: migrate unique moves, then archive absent records.
/// Preserve skill contents, sync bindings, and external/broken links for explicit review.
pub fn startup(ws: &Workspace) -> Result<Report> {
    let preview = plan(
        ws,
        &Options {
            forget_missing: true,
            ..Default::default()
        },
    )?;
    if preview.planned == 0 {
        return Ok(preview);
    }
    apply(ws, &preview)
}
