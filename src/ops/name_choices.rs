//! Reviewable deployment-name decisions. Nothing is written until every group
//! has a choice and the saved selection is checked again against current state.
use super::{deploy, targets};
use crate::{Workspace, config::AgentConfig, reconcile::Snapshot};
use anyhow::{Result, ensure};
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Debug, Clone)]
pub struct Change {
    pub agent: AgentConfig,
    pub project: Option<PathBuf>,
    pub before: targets::Selection,
    pub after: targets::Selection,
}
#[derive(Debug, Clone)]
pub struct Candidate {
    pub key: Option<String>,
    pub path: PathBuf,
    pub archive_hash: Option<String>,
}
#[derive(Debug, Clone)]
pub struct Group {
    pub name: String,
    pub directory: PathBuf,
    pub candidates: Vec<Candidate>,
}
#[derive(Debug, Clone)]
pub struct Pending {
    pub changes: Vec<Change>,
    pub groups: Vec<Group>,
}
impl std::fmt::Display for Pending {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} skill name conflicts require a choice",
            self.groups.len()
        )
    }
}
impl std::error::Error for Pending {}
fn identity(path: &std::path::Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.into())
}
fn fingerprint(path: &std::path::Path) -> Result<String> {
    let link = std::fs::read_link(path).ok();
    Ok(format!("{:?}:{}", link, crate::hash::hash_directory(path)?))
}
impl Pending {
    pub fn for_actions(
        ws: &Workspace,
        snap: &Snapshot,
        actions: &[deploy::Action],
    ) -> Result<Option<Self>> {
        let mut changes = Vec::new();
        for agent in &ws.config.agents {
            let before = targets::selection_from_snapshot(ws, agent, snap)?;
            let mut after = before.clone();
            for action in actions {
                match action {
                    deploy::Action::Link {
                        agent: key, skill, ..
                    } if key == &agent.key => {
                        after.manual.insert(skill.clone());
                    }
                    deploy::Action::Unlink {
                        agent: key, skill, ..
                    } if key == &agent.key => {
                        after.manual.remove(skill);
                        for members in after.presets.values_mut() {
                            members.remove(skill);
                        }
                    }
                    _ => {}
                }
            }
            if before != after {
                changes.push(Change {
                    agent: agent.clone(),
                    project: ws.project.clone(),
                    before,
                    after,
                });
            }
        }
        Self::from_plan(changes, snap, actions)
    }
    pub fn from_plan(
        changes: Vec<Change>,
        snap: &Snapshot,
        actions: &[deploy::Action],
    ) -> Result<Option<Self>> {
        let mut groups: BTreeMap<(PathBuf, String), BTreeMap<PathBuf, Candidate>> = BTreeMap::new();
        for conflict in deploy::name_conflicts(snap, actions) {
            let report = snap
                .agent(&conflict.agent)
                .ok_or_else(|| anyhow::anyhow!("agent no longer exists"))?;
            let directory = identity(&report.skills_dir);
            let options = groups
                .entry((directory.clone(), conflict.name))
                .or_default();
            let path = directory.join(crate::repository::default_deploy_name(&conflict.skill));
            options.entry(path.clone()).or_insert(Candidate {
                key: Some(conflict.skill),
                path,
                archive_hash: None,
            });
            let path = directory.join(
                conflict
                    .other_path
                    .file_name()
                    .ok_or_else(|| anyhow::anyhow!("invalid conflict path"))?,
            );
            let archive_hash = if conflict.other_skill.is_none() {
                Some(fingerprint(&path)?)
            } else {
                None
            };
            options.entry(path.clone()).or_insert(Candidate {
                key: conflict.other_skill,
                path,
                archive_hash,
            });
        }
        if groups.is_empty() {
            return Ok(None);
        }
        let mut unique: BTreeMap<PathBuf, Change> = BTreeMap::new();
        for change in changes {
            let path = identity(&change.agent.skills_path());
            if let Some(existing) = unique.get(&path) {
                ensure!(
                    existing.before == change.before && existing.after == change.after,
                    "shared directory choices disagree"
                );
            } else {
                unique.insert(path, change);
            }
        }
        ensure!(
            groups
                .keys()
                .all(|(directory, _)| unique.contains_key(directory)),
            "conflicting target has no selection plan"
        );
        Ok(Some(Self {
            changes: unique.into_values().collect(),
            groups: groups
                .into_iter()
                .map(|((directory, name), candidates)| Group {
                    directory,
                    name,
                    candidates: candidates.into_values().collect(),
                })
                .collect(),
        }))
    }
    pub fn keys(&self) -> Vec<String> {
        self.changes.iter().flat_map(|c| c.after.skills()).collect()
    }
    /// `None` explicitly means keep none. An agent-owned copy is archived,
    /// never deleted. Managed source directories always remain in the library.
    pub fn apply(
        &self,
        ws: &Workspace,
        choices: &[Option<usize>],
    ) -> Result<(String, Option<crate::history::Intent>)> {
        ensure!(
            choices.len() == self.groups.len(),
            "choose every conflict first"
        );
        for change in &self.changes {
            ensure!(
                targets::selection(ws, &change.agent)? == change.before,
                "deployment selection changed; refresh and retry"
            );
        }
        ensure!(
            self.groups.iter().all(|group| self
                .changes
                .iter()
                .any(|change| identity(&change.agent.skills_path()) == group.directory)),
            "deployment target changed since review; refresh and retry"
        );
        let mut changes = self.changes.clone();
        let mut archives = Vec::new();
        for (group, selected) in self.groups.iter().zip(choices) {
            ensure!(
                selected.is_none_or(|i| i < group.candidates.len()),
                "invalid choice"
            );
            for (index, candidate) in group.candidates.iter().enumerate() {
                if let Some(hash) = &candidate.archive_hash {
                    ensure!(
                        fingerprint(&candidate.path)? == *hash,
                        "{} changed since review; refresh and retry",
                        candidate.path.display()
                    );
                }
                if *selected == Some(index) {
                    continue;
                }
                if let Some(key) = &candidate.key {
                    for change in changes
                        .iter_mut()
                        .filter(|c| identity(&c.agent.skills_path()) == group.directory)
                    {
                        change.after.manual.remove(key);
                        for members in change.after.presets.values_mut() {
                            members.remove(key);
                        }
                    }
                } else {
                    archives.push(candidate.path.clone());
                }
            }
        }
        let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        for path in archives {
            let backup = path
                .parent()
                .and_then(|p| p.parent())
                .ok_or_else(|| anyhow::anyhow!("invalid archive path"))?
                .join(".skills-manager-backups")
                .join(format!("{stamp}-{}", std::process::id()))
                .join(path.file_name().unwrap());
            if let Err(error) = std::fs::create_dir_all(backup.parent().unwrap())
                .and_then(|_| std::fs::rename(&path, &backup))
            {
                for (source, destination) in moved.iter().rev() {
                    let _ = std::fs::rename(destination, source);
                }
                return Err(error.into());
            }
            moved.push((path, backup));
        }
        let mut messages = Vec::new();
        let mut intents = Vec::new();
        let mut applied: Vec<Change> = Vec::new();
        for change in changes {
            match targets::restore_selection(
                ws,
                &change.agent,
                change.project.as_deref(),
                &change.before,
                &change.after,
            ) {
                Ok(message) => {
                    applied.push(change.clone());
                    messages.push(message);
                    if change.before != change.after {
                        intents.push(crate::history::Intent::TargetSelection {
                            agent: change.agent,
                            project: change.project,
                            before: change.before,
                            after: change.after,
                        });
                    }
                }
                Err(error) => {
                    let mut failures = Vec::new();
                    for done in applied.iter().rev() {
                        if let Err(rollback) = targets::restore_selection(
                            ws,
                            &done.agent,
                            done.project.as_deref(),
                            &done.after,
                            &done.before,
                        ) {
                            failures.push(format!("{}: {rollback:#}", done.agent.skills_dir));
                        }
                    }
                    for (source, destination) in moved.iter().rev() {
                        if std::fs::symlink_metadata(source).is_err() {
                            if let Err(rollback) = std::fs::rename(destination, source) {
                                failures.push(format!(
                                    "archive kept at {}: {rollback}",
                                    destination.display()
                                ));
                            }
                        } else {
                            failures.push(format!("archive kept at {}", destination.display()));
                        }
                    }
                    return if failures.is_empty() {
                        Err(error)
                    } else {
                        Err(error
                            .context(format!("Rollback needs attention: {}", failures.join("; "))))
                    };
                }
            }
        }
        for (_, backup) in &moved {
            messages.push(format!(
                "Archived agent-owned entry to {}",
                backup.display()
            ));
        }
        let intent = if moved.is_empty() {
            (!intents.is_empty()).then_some(crate::history::Intent::Group(intents))
        } else {
            Some(crate::history::Intent::OneWay {
                what: messages.join("; "),
            })
        };
        Ok((messages.join("; "), intent))
    }
}
