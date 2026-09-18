//! Explicit, guarded repair of managed per-skill deployment links.

use crate::{
    Workspace,
    reconcile::{AgentDirMode, EntryState, Snapshot},
};
use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Deployment identity (`agent/link`) -> selected Library key.
    pub deployments: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Candidate {
    pub key: String,
    pub name: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum RepairIssue {
    Broken {
        id: String,
        agent: String,
        link_name: String,
        target: PathBuf,
        candidates: Vec<Candidate>,
    },
    OutdatedName {
        id: String,
        agent: String,
        link_name: String,
        target: PathBuf,
        skill: String,
        name: String,
        blocked: Option<String>,
    },
}

impl RepairIssue {
    pub fn id(&self) -> &str {
        match self {
            Self::Broken { id, .. } | Self::OutdatedName { id, .. } => id,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct RepairAnalysis {
    pub ready: usize,
    pub unresolved: usize,
    pub issues: Vec<RepairIssue>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum RepairAction {
    Relink {
        id: String,
        agent: String,
        path: PathBuf,
        old_target: PathBuf,
        skill: String,
        target: PathBuf,
        name: String,
        #[serde(skip)]
        parent_identity: (u64, u64),
        #[serde(skip)]
        link_identity: (u64, u64),
    },
    Rename {
        id: String,
        agent: String,
        path: PathBuf,
        destination: PathBuf,
        old_target: PathBuf,
        target: PathBuf,
        skill: String,
        name: String,
        #[serde(skip)]
        parent_identity: (u64, u64),
        #[serde(skip)]
        link_identity: (u64, u64),
    },
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct RepairPlan {
    pub analysis: RepairAnalysis,
    pub actions: Vec<RepairAction>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Item {
    pub skill: String,
    pub outcome: String,
    pub detail: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct RepairResult {
    pub applied: bool,
    pub planned: usize,
    pub repaired: usize,
    pub skipped: usize,
    pub failed: usize,
    pub items: Vec<Item>,
}
pub type Report = RepairResult;

impl RepairResult {
    pub fn lines(&self) -> Vec<String> {
        let mut lines = vec![if self.applied {
            format!(
                "{} repaired · {} skipped · {} failed",
                self.repaired, self.skipped, self.failed
            )
        } else {
            format!("{} ready · {} unresolved", self.planned, self.skipped)
        }];
        lines.extend(self.items.iter().map(|item| {
            format!(
                "{:<10} {} · {}",
                item.outcome.to_uppercase(),
                item.skill,
                item.detail
            )
        }));
        lines
    }
}

fn identity(metadata: &fs::Metadata) -> (u64, u64) {
    (metadata.dev(), metadata.ino())
}

fn issue_id(agent: &str, name: &str) -> String {
    format!("{agent}/{name}")
}

fn candidates(snap: &Snapshot, name: &str) -> Vec<Candidate> {
    snap.skills
        .iter()
        .filter(|skill| skill.status.is_present() && skill.deployment_name() == Some(name))
        .map(|skill| Candidate {
            key: skill.key.clone(),
            name: name.to_string(),
            path: skill.path.clone(),
        })
        .collect()
}

fn managed_target<'a>(
    snap: &'a Snapshot,
    target: &Path,
) -> Option<&'a crate::reconcile::SkillRecord> {
    let target = fs::canonicalize(target).ok()?;
    snap.skills.iter().find(|skill| {
        skill.status.is_present() && fs::canonicalize(&skill.path).ok().as_ref() == Some(&target)
    })
}

pub fn analyze(ws: &Workspace) -> Result<RepairAnalysis> {
    analyze_snapshot(&ws.scan()?)
}

fn analyze_snapshot(snap: &Snapshot) -> Result<RepairAnalysis> {
    let mut analysis = RepairAnalysis::default();
    let mut seen_directories = BTreeSet::new();
    for agent in &snap.agents {
        if agent.mode != AgentDirMode::Real {
            continue;
        }
        let directory =
            fs::canonicalize(&agent.skills_dir).unwrap_or_else(|_| agent.skills_dir.clone());
        if !seen_directories.insert(directory) {
            continue;
        }
        for (link_name, state) in &agent.entries {
            let path = agent.skills_dir.join(link_name);
            match state {
                EntryState::Broken { target } => {
                    let found = candidates(snap, link_name);
                    if found.len() == 1 {
                        analysis.ready += 1;
                    } else {
                        analysis.unresolved += 1;
                    }
                    analysis.issues.push(RepairIssue::Broken {
                        id: issue_id(&agent.key, link_name),
                        agent: agent.key.clone(),
                        link_name: link_name.clone(),
                        target: target.clone(),
                        candidates: found,
                    });
                }
                EntryState::Foreign { target } => {
                    let Some(skill) = managed_target(snap, &path) else {
                        continue;
                    };
                    let Some(name) = skill.deployment_name() else {
                        continue;
                    };
                    if name == link_name {
                        continue;
                    }
                    let destination = agent.skills_dir.join(name);
                    let blocked = match fs::symlink_metadata(&destination) {
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                        Err(error) => Some(error.to_string()),
                        Ok(_) => Some(format!("{} already exists", destination.display())),
                    };
                    if blocked.is_none() {
                        analysis.ready += 1;
                    } else {
                        analysis.unresolved += 1;
                    }
                    analysis.issues.push(RepairIssue::OutdatedName {
                        id: issue_id(&agent.key, link_name),
                        agent: agent.key.clone(),
                        link_name: link_name.clone(),
                        target: target.clone(),
                        skill: skill.key.clone(),
                        name: name.to_string(),
                        blocked,
                    });
                }
                _ => {}
            }
        }
    }
    Ok(analysis)
}

fn observed(path: &Path) -> Result<((u64, u64), (u64, u64))> {
    let parent = path.parent().context("deployment has no parent")?;
    let parent_meta = fs::symlink_metadata(parent)
        .with_context(|| format!("checking Agent directory {}", parent.display()))?;
    let link_meta =
        fs::symlink_metadata(path).with_context(|| format!("checking {}", path.display()))?;
    ensure!(
        parent_meta.is_dir(),
        "Agent directory is not a real directory"
    );
    ensure!(
        link_meta.file_type().is_symlink(),
        "deployment is not a symlink"
    );
    Ok((identity(&parent_meta), identity(&link_meta)))
}

pub fn build_plan(ws: &Workspace, options: &Options) -> Result<RepairPlan> {
    let snap = ws.scan()?;
    let analysis = analyze_snapshot(&snap)?;
    let mut actions = Vec::new();
    for issue in &analysis.issues {
        match issue {
            RepairIssue::Broken {
                id,
                agent,
                link_name,
                target: old_target,
                candidates,
            } => {
                let selected = options
                    .deployments
                    .get(id)
                    .and_then(|key| snap.get(key))
                    .or_else(|| {
                        (candidates.len() == 1)
                            .then(|| snap.get(&candidates[0].key))
                            .flatten()
                    });
                let Some(skill) = selected else {
                    continue;
                };
                ensure!(
                    skill.status.is_present()
                        && skill.deployment_name() == Some(link_name.as_str()),
                    "selected Library skill no longer declares {link_name}"
                );
                let report = snap.agent(agent).context("Agent disappeared")?;
                let path = report.skills_dir.join(link_name);
                let (parent_identity, link_identity) = observed(&path)?;
                actions.push(RepairAction::Relink {
                    id: id.clone(),
                    agent: agent.clone(),
                    path,
                    old_target: old_target.clone(),
                    skill: skill.key.clone(),
                    target: skill.path.clone(),
                    name: link_name.clone(),
                    parent_identity,
                    link_identity,
                });
            }
            RepairIssue::OutdatedName {
                id,
                agent,
                link_name,
                skill,
                name,
                blocked,
                target: old_target,
            } if blocked.is_none() => {
                let report = snap.agent(agent).context("Agent disappeared")?;
                let path = report.skills_dir.join(link_name);
                let destination = report.skills_dir.join(name);
                let record = snap.get(skill).context("Library skill disappeared")?;
                let (parent_identity, link_identity) = observed(&path)?;
                actions.push(RepairAction::Rename {
                    id: id.clone(),
                    agent: agent.clone(),
                    path,
                    destination,
                    old_target: old_target.clone(),
                    target: record.path.clone(),
                    skill: skill.clone(),
                    name: name.clone(),
                    parent_identity,
                    link_identity,
                });
            }
            RepairIssue::OutdatedName { .. } => {}
        }
    }
    Ok(RepairPlan { analysis, actions })
}

fn validate_link(
    path: &Path,
    old_target: &Path,
    parent_identity: (u64, u64),
    link_identity: (u64, u64),
) -> Result<()> {
    let (current_parent, current_link) = observed(path)?;
    ensure!(
        current_parent == parent_identity,
        "Agent directory changed since preview"
    );
    ensure!(
        current_link == link_identity,
        "deployment changed since preview"
    );
    ensure!(
        crate::util::link_target_abs(path).as_deref() == Some(old_target),
        "deployment target changed since preview"
    );
    Ok(())
}

fn apply_action(ws: &Workspace, action: &RepairAction) -> Result<String> {
    match action {
        RepairAction::Relink {
            path,
            old_target,
            target,
            skill: _,
            name,
            parent_identity,
            link_identity,
            ..
        } => {
            validate_link(path, old_target, *parent_identity, *link_identity)?;
            let doc = crate::skill::SkillDoc::load(target)?;
            ensure!(
                doc.name == *name,
                "declared skill name changed since preview"
            );
            ensure!(
                fs::canonicalize(target)?.starts_with(&ws.root),
                "repair target is outside the Library"
            );
            let temporary = path.with_file_name(format!(
                ".skills-repair-{}-{}",
                std::process::id(),
                super::nanos()
            ));
            std::os::unix::fs::symlink(target, &temporary)?;
            let result = fs::rename(&temporary, path);
            if result.is_err() {
                let _ = fs::remove_file(&temporary);
            }
            result?;
            Ok(format!("{name} -> {}", target.display()))
        }
        RepairAction::Rename {
            path,
            destination,
            old_target,
            target,
            skill: _,
            name,
            parent_identity,
            link_identity,
            ..
        } => {
            validate_link(path, old_target, *parent_identity, *link_identity)?;
            ensure!(
                fs::canonicalize(path).ok() == fs::canonicalize(target).ok(),
                "deployment target changed since preview"
            );
            let doc = crate::skill::SkillDoc::load(target)?;
            ensure!(
                doc.name == *name,
                "declared skill name changed since preview"
            );
            match fs::symlink_metadata(destination) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
                Ok(_) => bail!("{} is now occupied", destination.display()),
            }
            fs::rename(path, destination)?;
            Ok(format!(
                "{} -> {}",
                path.file_name().unwrap_or_default().to_string_lossy(),
                name
            ))
        }
    }
}

pub fn apply_plan(ws: &Workspace, plan: &RepairPlan) -> Result<RepairResult> {
    let mut report = RepairResult {
        applied: true,
        planned: plan.actions.len(),
        skipped: plan
            .analysis
            .issues
            .len()
            .saturating_sub(plan.actions.len()),
        ..Default::default()
    };
    for action in &plan.actions {
        let (skill, id) = match action {
            RepairAction::Relink { skill, id, .. } | RepairAction::Rename { skill, id, .. } => {
                (skill.clone(), id.clone())
            }
        };
        match apply_action(ws, action) {
            Ok(detail) => {
                report.repaired += 1;
                report.items.push(Item {
                    skill,
                    outcome: "repaired".into(),
                    detail,
                });
            }
            Err(error) => {
                report.failed += 1;
                report.items.push(Item {
                    skill,
                    outcome: "failed".into(),
                    detail: format!("{id}: {error:#}"),
                });
            }
        }
    }
    Ok(report)
}

pub fn run_with_options(ws: &Workspace, apply_changes: bool, options: &Options) -> Result<Report> {
    let plan = build_plan(ws, options)?;
    if apply_changes {
        apply_plan(ws, &plan)
    } else {
        let mut report = RepairResult {
            planned: plan.actions.len(),
            skipped: plan
                .analysis
                .issues
                .len()
                .saturating_sub(plan.actions.len()),
            ..Default::default()
        };
        for action in plan.actions {
            let (skill, detail) = match action {
                RepairAction::Relink {
                    skill,
                    name,
                    target,
                    ..
                } => (skill, format!("relink {name} -> {}", target.display())),
                RepairAction::Rename {
                    skill, path, name, ..
                } => (
                    skill,
                    format!(
                        "rename {} -> {name}",
                        path.file_name().unwrap_or_default().to_string_lossy()
                    ),
                ),
            };
            report.items.push(Item {
                skill,
                outcome: "ready".into(),
                detail,
            });
        }
        Ok(report)
    }
}

pub fn run(ws: &Workspace, apply_changes: bool) -> Result<Report> {
    run_with_options(ws, apply_changes, &Options::default())
}
