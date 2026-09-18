//! Symlink deployment: deploy, undeploy, convert dir-linked agents.

use crate::Workspace;
use crate::hash::hash_directory;
use crate::preset::Preset;
use crate::reconcile::{AgentDirMode, DeployState, EntryState, Snapshot};
use crate::util::is_symlink;
use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum Action {
    Mkdir {
        agent: String,
        path: PathBuf,
    },
    Link {
        agent: String,
        skill: String,
        path: PathBuf,
        target: PathBuf,
    },
    Unlink {
        agent: String,
        skill: String,
        path: PathBuf,
    },
    /// Remove only the exact broken link that was previewed.
    #[serde(rename = "unlink")]
    Clean {
        agent: String,
        skill: String,
        path: PathBuf,
        target: PathBuf,
        #[serde(skip)]
        parent_identity: (u64, u64),
        #[serde(skip)]
        link_identity: (u64, u64, i64, i64),
    },
    /// Delete a real directory the agent holds and put a link to the root in
    /// its place. Only planned for a copy whose content matches the root, and
    /// checked again on apply, since deleting is the part that cannot be undone.
    Relink {
        agent: String,
        skill: String,
        path: PathBuf,
        target: PathBuf,
    },
    Skip {
        agent: String,
        skill: String,
        reason: String,
    },
}

impl Action {
    pub fn describe(&self) -> String {
        match self {
            Action::Mkdir { agent, path } => format!("mkdir  {agent}: {}", path.display()),
            Action::Link {
                agent,
                skill,
                target,
                ..
            } => {
                format!(
                    "link   {agent}/{skill} -> {}",
                    crate::paths::contract_tilde(target)
                )
            }
            Action::Unlink { agent, skill, path } => {
                format!("unlink {agent}/{skill} ({})", path.display())
            }
            Action::Clean {
                agent, skill, path, ..
            } => {
                format!("clean  {agent}/{skill} ({})", path.display())
            }
            Action::Relink {
                agent,
                skill,
                target,
                ..
            } => format!(
                "relink {agent}/{skill} -> {} (deleting the agent's own copy)",
                crate::paths::contract_tilde(target)
            ),
            Action::Skip {
                agent,
                skill,
                reason,
            } => format!("skip   {agent}/{skill}: {reason}"),
        }
    }
    pub fn is_change(&self) -> bool {
        !matches!(self, Action::Skip { .. })
    }
}

fn agent_dirs(ws: &Workspace, agents: &[String]) -> Result<Vec<(String, PathBuf)>> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for a in agents {
        let cfg = ws
            .config
            .agent(a)
            .with_context(|| format!("unknown agent: {a}"))?;
        let path = cfg.skills_path();
        let identity = std::fs::canonicalize(&path).unwrap_or(path.clone());
        if seen.insert(identity) {
            out.push((cfg.key.clone(), path));
        }
    }
    Ok(out)
}

/// Plan linking `skills` into each of `agents`.
pub fn plan_deploy(
    ws: &Workspace,
    snap: &Snapshot,
    skills: &[String],
    agents: &[String],
) -> Result<Vec<Action>> {
    let mut actions = Vec::new();
    let mut mkdir_done = BTreeSet::new();
    for (agent, dir) in agent_dirs(ws, agents)? {
        let report = snap.agent(&agent).context("agent not scanned")?;
        for skill in skills {
            let rec = match snap.get(skill) {
                Some(r) if r.status.is_present() && r.deployment_name().is_some() => r,
                Some(r) => {
                    actions.push(Action::Skip {
                        agent: agent.clone(),
                        skill: skill.clone(),
                        reason: if r.status.is_present() {
                            "skill has no valid declared name".into()
                        } else {
                            format!("skill is {}", r.status.label())
                        },
                    });
                    continue;
                }
                None => bail!("no such skill: {skill}"),
            };
            let name = rec.deployment_name().expect("checked above");
            match &report.mode {
                AgentDirMode::ReadOnly { reason, .. } => actions.push(Action::Skip {
                    agent: agent.clone(),
                    skill: skill.clone(),
                    reason: format!("agent directory is read-only: {reason}"),
                }),
                AgentDirMode::Missing => {
                    if mkdir_done.insert(agent.clone()) {
                        actions.push(Action::Mkdir {
                            agent: agent.clone(),
                            path: dir.clone(),
                        });
                    }
                    actions.push(Action::Link {
                        agent: agent.clone(),
                        skill: skill.clone(),
                        path: dir.join(name),
                        target: rec.path.clone(),
                    });
                }
                AgentDirMode::Real => match report.entries.get(name) {
                    None => actions.push(Action::Link {
                        agent: agent.clone(),
                        skill: skill.clone(),
                        path: dir.join(name),
                        target: rec.path.clone(),
                    }),
                    Some(EntryState::Deployed)
                        if rec.deploy.get(&agent) == Some(&DeployState::Deployed) =>
                    {
                        actions.push(Action::Skip {
                            agent: agent.clone(),
                            skill: skill.clone(),
                            reason: "already deployed".into(),
                        })
                    }
                    Some(EntryState::Broken { .. }) => actions.push(Action::Skip {
                        agent: agent.clone(),
                        skill: skill.clone(),
                        reason: "broken link; explicitly clean it before deploying".into(),
                    }),
                    Some(EntryState::Deployed) => actions.push(Action::Link {
                        agent: agent.clone(),
                        skill: skill.clone(),
                        path: dir.join(name),
                        target: rec.path.clone(),
                    }),
                    Some(_) => actions.push(Action::Link {
                        agent: agent.clone(),
                        skill: skill.clone(),
                        path: dir.join(name),
                        target: rec.path.clone(),
                    }),
                },
            }
        }
    }
    Ok(actions)
}

/// Plan removing links for `skills` from each of `agents`. Only links into the root are removed.
pub fn plan_undeploy(
    ws: &Workspace,
    snap: &Snapshot,
    skills: &[String],
    agents: &[String],
) -> Result<Vec<Action>> {
    let mut actions = Vec::new();
    for (agent, dir) in agent_dirs(ws, agents)? {
        let report = snap.agent(&agent).context("agent not scanned")?;
        for skill in skills {
            let Some(name) = snap.get(skill).and_then(|record| record.deployment_name()) else {
                actions.push(Action::Skip {
                    agent: agent.clone(),
                    skill: skill.clone(),
                    reason: "skill has no valid declared name".into(),
                });
                continue;
            };
            match &report.mode {
                AgentDirMode::ReadOnly { reason, .. } => actions.push(Action::Skip {
                    agent: agent.clone(),
                    skill: skill.clone(),
                    reason: format!("agent directory is read-only: {reason}"),
                }),
                AgentDirMode::Real => match report.entries.get(name) {
                    Some(EntryState::Deployed)
                        if snap.get(skill).is_some_and(|r| {
                            r.deploy.get(&agent) != Some(&DeployState::Deployed)
                        }) =>
                    {
                        actions.push(Action::Skip {
                            agent: agent.clone(),
                            skill: skill.clone(),
                            reason: "not deployed".into(),
                        });
                    }
                    Some(EntryState::Deployed) | Some(EntryState::Broken { .. }) => {
                        actions.push(Action::Unlink {
                            agent: agent.clone(),
                            skill: skill.clone(),
                            path: dir.join(name),
                        })
                    }
                    None => actions.push(Action::Skip {
                        agent: agent.clone(),
                        skill: skill.clone(),
                        reason: "not deployed".into(),
                    }),
                    Some(other) => actions.push(Action::Skip {
                        agent: agent.clone(),
                        skill: skill.clone(),
                        reason: format!("entry is {}; not touching it", other.label()),
                    }),
                },
                _ => actions.push(Action::Skip {
                    agent: agent.clone(),
                    skill: skill.clone(),
                    reason: "agent dir missing or foreign".into(),
                }),
            }
        }
    }
    Ok(actions)
}

/// The entries of `agent` that `skills` names, or every entry when it names
/// none. A name with no entry behind it is reported rather than dropped, so a
/// typo in a CLI argument does not pass as "nothing to do".
fn entries_of<'a>(
    report: &'a crate::reconcile::AgentReport,
    agent: &str,
    skills: &[String],
) -> (Vec<(&'a str, &'a EntryState)>, Vec<Action>) {
    if skills.is_empty() {
        return (
            report
                .entries
                .iter()
                .map(|(k, v)| (k.as_str(), v))
                .collect(),
            Vec::new(),
        );
    }
    let mut found = Vec::new();
    let mut skips = Vec::new();
    for s in skills {
        match report.entries.get_key_value(s) {
            Some((k, v)) => found.push((k.as_str(), v)),
            None => skips.push(Action::Skip {
                agent: agent.into(),
                skill: s.clone(),
                reason: format!("not in {agent}"),
            }),
        }
    }
    (found, skips)
}

/// Whether `agent` has a directory of its own to repair entries in. Anything
/// else has no per-skill entries, and the skip says which shape it is in.
fn repairable(report: &crate::reconcile::AgentReport, agent: &str) -> Option<Action> {
    let reason = match &report.mode {
        AgentDirMode::Real => return None,
        AgentDirMode::ReadOnly { reason, .. } => {
            format!("agent directory is read-only: {reason}")
        }
        AgentDirMode::Missing => "agent dir does not exist".to_string(),
    };
    Some(Action::Skip {
        agent: agent.into(),
        skill: "*".into(),
        reason,
    })
}

/// Plan removing the broken links of `agent`: those of `skills`, or all of
/// them when none is named. A broken link is one whose target is gone, which
/// is nearly always a skill deleted from the root; the link itself carries no
/// content, so removing it loses nothing. Recorded as an ordinary unlink, so
/// undo puts the link back if the skill has returned and says why not otherwise.
pub fn plan_clean(
    ws: &Workspace,
    snap: &Snapshot,
    agent: &str,
    skills: &[String],
) -> Result<Vec<Action>> {
    let (agent, dir) = agent_dirs(ws, std::slice::from_ref(&agent.to_string()))?.remove(0);
    let report = snap.agent(&agent).context("agent not scanned")?;
    if let Some(skip) = repairable(report, &agent) {
        return Ok(vec![skip]);
    }
    let (entries, mut actions) = entries_of(report, &agent, skills);
    for (name, state) in entries {
        match state {
            EntryState::Broken { target } => {
                let path = dir.join(name);
                actions.push(Action::Clean {
                    agent: agent.clone(),
                    skill: snap
                        .skills
                        .iter()
                        .find(|s| s.deployment_name() == Some(name))
                        .map(|s| s.key.clone())
                        .unwrap_or_else(|| name.into()),
                    target: target.clone(),
                    parent_identity: identity(&fs::symlink_metadata(&dir)?),
                    link_identity: symlink_identity(&fs::symlink_metadata(&path)?),
                    path,
                });
            }
            // With nothing named, the healthy entries are simply not the
            // subject; with a name given, the answer is why it does not apply.
            _ if skills.is_empty() => {}
            other => actions.push(Action::Skip {
                agent: agent.clone(),
                skill: name.into(),
                reason: format!("entry is {}; only broken links are cleaned", other.label()),
            }),
        }
    }
    Ok(actions)
}

/// Plan replacing the agent's own copies of `skills` (or of every skill, when
/// none is named) with links to the root. Only a copy whose content matches the
/// root byte for byte is replaced: that one is a link in all but form, and the
/// root has everything it holds. A copy that differs is the agent's to keep,
/// and the plan says so rather than choosing a side.
pub fn plan_relink(
    ws: &Workspace,
    snap: &Snapshot,
    agent: &str,
    skills: &[String],
) -> Result<Vec<Action>> {
    let (agent, dir) = agent_dirs(ws, std::slice::from_ref(&agent.to_string()))?.remove(0);
    let report = snap.agent(&agent).context("agent not scanned")?;
    if let Some(skip) = repairable(report, &agent) {
        return Ok(vec![skip]);
    }
    let (entries, mut actions) = entries_of(report, &agent, skills);
    for (name, state) in entries {
        match state {
            EntryState::Shadow { same_content: true } => actions.push(Action::Relink {
                agent: agent.clone(),
                skill: snap
                    .skills
                    .iter()
                    .find(|s| s.deployment_name() == Some(name))
                    .map(|s| s.key.clone())
                    .unwrap_or_else(|| name.into()),
                path: dir.join(name),
                target: snap
                    .skills
                    .iter()
                    .find(|s| s.deployment_name() == Some(name))
                    .map(|s| s.path.clone())
                    .unwrap_or_else(|| ws.skill_path(name)),
            }),
            EntryState::Shadow {
                same_content: false,
            } => actions.push(Action::Skip {
                agent: agent.clone(),
                skill: name.into(),
                reason: "the agent's copy differs from the root; not touching it".into(),
            }),
            _ if skills.is_empty() => {}
            other => actions.push(Action::Skip {
                agent: agent.clone(),
                skill: name.into(),
                reason: format!(
                    "entry is {}; only same-content copies are relinked",
                    other.label()
                ),
            }),
        }
    }
    Ok(actions)
}

/// Execute planned actions in order. Stops at the first failure.
pub fn apply(actions: &[Action]) -> Result<usize> {
    // A clean preview is permission to remove only the exact broken links it
    // observed. Preflight the whole batch before any mutations, and validate
    // again at the individual removal to narrow the race window.
    for action in actions {
        if matches!(action, Action::Clean { .. }) {
            validate_clean(action)?;
        }
    }
    // Validate the entire batch before changing even the first directory.
    let removed: BTreeSet<_> = actions
        .iter()
        .filter_map(|a| match a {
            Action::Unlink { path, .. } | Action::Clean { path, .. } => Some(path.clone()),
            _ => None,
        })
        .collect();
    let mut planned = Vec::new();
    for action in actions {
        if let Action::Link { path, target, .. } = action {
            let name = crate::skill::SkillDoc::load(target)?.name;
            let parent = path.parent().context("invalid deployment path")?;
            let directory = std::fs::canonicalize(parent).unwrap_or(parent.to_path_buf());
            for (other_dir, other_path, other_name, other_target) in &planned {
                anyhow::ensure!(
                    other_dir != &directory
                        || other_target == target
                        || (other_path != path && other_name != &name),
                    "deployment conflict: choose only one skill per folder/name"
                );
            }
            if parent.is_dir() {
                for entry in std::fs::read_dir(parent)? {
                    let other = entry?.path();
                    if removed.contains(&other)
                        || other
                            .file_name()
                            .is_some_and(|n| n.to_string_lossy().starts_with('.'))
                    {
                        continue;
                    }
                    if std::fs::canonicalize(&other).ok() == std::fs::canonicalize(target).ok() {
                        continue;
                    }
                    anyhow::ensure!(
                        other != *path
                            && crate::skill::SkillDoc::load(&other)
                                .map_or(true, |d| d.name != name),
                        "deployment conflict at {}; choose one skill before applying",
                        other.display()
                    );
                }
            }
            planned.push((directory, path.clone(), name, target.clone()));
        }
    }

    let mut done = 0;
    for a in actions {
        match a {
            Action::Skip { .. } => {}
            Action::Mkdir { path, .. } => {
                // Only a directory that was not there counts as a change.
                let fresh = !path.is_dir();
                std::fs::create_dir_all(path)
                    .with_context(|| format!("creating {}", path.display()))?;
                if fresh {
                    done += 1;
                }
            }
            Action::Link { path, target, .. } => {
                if is_symlink(path) {
                    // Already pointing where it should; nothing to redo.
                    if crate::util::link_target_abs(path).as_deref() == Some(target.as_path()) {
                        continue;
                    }
                    std::fs::remove_file(path)?;
                } else if path.exists() {
                    bail!("refusing to replace existing entry {}", path.display());
                }
                std::os::unix::fs::symlink(target, path).with_context(|| {
                    format!("linking {} -> {}", path.display(), target.display())
                })?;
                done += 1;
            }
            Action::Relink { path, target, .. } => {
                // The plan was drawn from a snapshot, and deleting is the one
                // step here that cannot be taken back, so the copy is compared
                // with the root again now rather than trusted to still match.
                match std::fs::symlink_metadata(path) {
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        bail!("{} is gone; nothing to relink", path.display())
                    }
                    Ok(m) if !m.file_type().is_dir() => {
                        bail!("{} is not a directory; nothing to relink", path.display())
                    }
                    _ => {}
                }
                if !target.is_dir() {
                    bail!(
                        "{} is not in the root; nothing to relink to",
                        target.display()
                    );
                }
                if hash_directory(path)? != hash_directory(target)? {
                    bail!(
                        "{} no longer matches the root; refusing to delete it",
                        path.display()
                    );
                }
                std::fs::remove_dir_all(path)
                    .with_context(|| format!("removing {}", path.display()))?;
                std::os::unix::fs::symlink(target, path).with_context(|| {
                    format!("linking {} -> {}", path.display(), target.display())
                })?;
                done += 1;
            }
            Action::Unlink { path, .. } => {
                match std::fs::symlink_metadata(path) {
                    // Already gone, by another session or by hand. The state
                    // asked for is the state there is, so carry on rather than
                    // abandoning the rest of the batch.
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    // Something is there that we did not put there.
                    Ok(m) if !m.file_type().is_symlink() => {
                        bail!("refusing to remove non-symlink {}", path.display())
                    }
                    _ => {
                        std::fs::remove_file(path)
                            .with_context(|| format!("removing {}", path.display()))?;
                        done += 1;
                    }
                }
            }
            Action::Clean { path, .. } => {
                if validate_clean(a)? == CleanState::Remove {
                    fs::remove_file(path)
                        .with_context(|| format!("removing {}", path.display()))?;
                    done += 1;
                }
            }
        }
    }
    Ok(done)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CleanState {
    Gone,
    Repaired,
    Remove,
}

fn identity(metadata: &fs::Metadata) -> (u64, u64) {
    (metadata.dev(), metadata.ino())
}

fn symlink_identity(metadata: &fs::Metadata) -> (u64, u64, i64, i64) {
    (
        metadata.dev(),
        metadata.ino(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    )
}

fn validate_clean(action: &Action) -> Result<CleanState> {
    let Action::Clean {
        path,
        target,
        parent_identity,
        link_identity,
        ..
    } = action
    else {
        unreachable!("clean validation called for another action")
    };

    let parent = path.parent().context("invalid clean path")?;
    let parent_metadata = fs::symlink_metadata(parent)
        .with_context(|| format!("checking agent directory {}", parent.display()))?;
    if !parent_metadata.is_dir() || identity(&parent_metadata) != *parent_identity {
        bail!(
            "agent directory changed; refusing to clean {}",
            path.display()
        );
    }
    let link_metadata = match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(CleanState::Gone);
        }
        Err(error) => return Err(error).with_context(|| format!("checking {}", path.display())),
        Ok(metadata) => metadata,
    };
    if !link_metadata.file_type().is_symlink() {
        bail!("refusing to remove non-symlink {}", path.display());
    }
    if symlink_identity(&link_metadata) != *link_identity
        || crate::util::link_target_abs(path).as_ref() != Some(target)
    {
        bail!("broken link changed; refusing to clean {}", path.display());
    }
    if fs::canonicalize(path).is_ok() {
        Ok(CleanState::Repaired)
    } else {
        Ok(CleanState::Remove)
    }
}

// ---- presets ---------------------------------------------------------------

/// Where a preset stands within an agent scope. Counted in skill-agent pairs,
/// so a preset of 3 skills over 2 agents has a total of 6.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PresetStatus {
    pub installed: usize,
    pub total: usize,
    /// Members with no directory in the skills root; they cannot be deployed.
    pub absent: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PresetState {
    /// No members or agents in scope.
    Empty,
    /// Every pair is deployed.
    Active,
    /// Some pairs are deployed.
    Partial,
    /// No pair is deployed.
    Inactive,
}

impl PresetStatus {
    pub fn state(&self) -> PresetState {
        if self.total == 0 {
            PresetState::Empty
        } else if self.installed == self.total {
            PresetState::Active
        } else if self.installed == 0 {
            PresetState::Inactive
        } else {
            PresetState::Partial
        }
    }

    /// Optional compact status text: a fraction for partial coverage, `empty`
    /// for no members or agents in scope, and no suffix for active/inactive states.
    pub fn progress(&self) -> Option<String> {
        match self.state() {
            PresetState::Partial => Some(format!("{}/{}", self.installed, self.total)),
            PresetState::Empty => Some("empty".into()),
            PresetState::Active | PresetState::Inactive => None,
        }
    }
}

/// Count every member/agent pair across the explicit `scope`. Missing
/// library members stay in the denominator so a partial package never toggles off
/// merely because its unavailable members were hidden from the count.
pub fn preset_status(snap: &Snapshot, preset: &Preset, scope: &[String]) -> PresetStatus {
    let members = preset.members();
    let mut installed = 0;
    let total = members.len() * scope.len();
    let mut absent = Vec::new();
    for skill in &members {
        match snap.get(skill) {
            Some(rec) if rec.status.is_present() => {
                for agent in scope {
                    if matches!(rec.deploy.get(agent), Some(DeployState::Deployed)) {
                        installed += 1;
                    }
                }
            }
            _ => absent.push(skill.clone()),
        }
    }
    PresetStatus {
        installed,
        total,
        absent,
    }
}

/// Deploy whatever of `preset` is still missing in `scope`. Members already in
/// place, and entries the tool does not own, are skipped rather than replaced.
pub fn plan_preset_activate(
    ws: &Workspace,
    snap: &Snapshot,
    preset: &Preset,
    scope: &[String],
) -> Result<Vec<Action>> {
    let members = preset.members();
    let agents = scope.to_vec();
    let present: Vec<String> = members
        .iter()
        .filter(|s| snap.get(s).is_some_and(|r| r.status.is_present()))
        .cloned()
        .collect();
    let mut actions = plan_deploy(ws, snap, &present, &agents)?;
    for skill in members.iter().filter(|s| !present.contains(s)) {
        actions.push(Action::Skip {
            agent: "*".into(),
            skill: skill.clone(),
            reason: "not in the skills root".into(),
        });
    }
    Ok(actions)
}

/// Undeploy every member of `preset` from `scope`. Overlap with other presets
/// is deliberately ignored: a preset is applied as a one-time selection, not a live
/// membership, so deactivating removes all of its skills.
pub fn plan_preset_deactivate(
    ws: &Workspace,
    snap: &Snapshot,
    preset: &Preset,
    scope: &[String],
) -> Result<Vec<Action>> {
    let members = preset.members();
    let agents = scope.to_vec();
    let present: Vec<String> = members
        .iter()
        .filter(|s| snap.get(s).is_some())
        .cloned()
        .collect();
    plan_undeploy(ws, snap, &present, &agents)
}

// ---- describing what happened ----------------------------------------------

/// Distinct values in order of first appearance, rendered as a phrase: up to
/// `max` are named, beyond that they are counted.
fn phrase(items: &[String], max: usize, plural: &str) -> String {
    let mut seen: Vec<&str> = Vec::new();
    for i in items {
        if !seen.contains(&i.as_str()) {
            seen.push(i);
        }
    }
    match seen.len() {
        0 => String::new(),
        n if n > max => format!("{n} {plural}"),
        1 => seen[0].to_string(),
        2 => format!("{} and {}", seen[0], seen[1]),
        _ => {
            let (last, rest) = seen.split_last().unwrap();
            format!("{} and {last}", rest.join(", "))
        }
    }
}

/// A sentence saying what a batch of actions did, for a notification.
/// Names the skills while there are few enough to be worth naming.
pub fn summarize(actions: &[Action]) -> String {
    let mut added = (Vec::new(), Vec::new());
    let mut removed = (Vec::new(), Vec::new());
    let mut relinked = (Vec::new(), Vec::new());
    let mut skipped: Vec<String> = Vec::new();
    for a in actions {
        match a {
            Action::Link { skill, agent, .. } => {
                added.0.push(skill.clone());
                added.1.push(agent.clone());
            }
            Action::Unlink { skill, agent, .. } | Action::Clean { skill, agent, .. } => {
                removed.0.push(skill.clone());
                removed.1.push(agent.clone());
            }
            Action::Relink { skill, agent, .. } => {
                relinked.0.push(skill.clone());
                relinked.1.push(agent.clone());
            }
            Action::Skip { skill, reason, .. } => {
                let s = format!("{skill} ({reason})");
                if !skipped.contains(&s) {
                    skipped.push(s);
                }
            }
            Action::Mkdir { .. } => {}
        }
    }
    // A count of skips tells nobody what to do about them; the first couple
    // named with their reason does.
    let skips = match skipped.len() {
        0 => String::new(),
        n if n <= 2 => format!("skipped {}", skipped.join(", ")),
        n => format!("skipped {}, {} more", skipped[..2].join(", "), n - 2),
    };
    let mut parts = Vec::new();
    if !added.0.is_empty() {
        parts.push(format!(
            "added {} to {}",
            phrase(&added.0, 3, "skills"),
            phrase(&added.1, 2, "agents")
        ));
    }
    if !removed.0.is_empty() {
        parts.push(format!(
            "removed {} from {}",
            phrase(&removed.0, 3, "skills"),
            phrase(&removed.1, 2, "agents")
        ));
    }
    if !relinked.0.is_empty() {
        parts.push(format!(
            "relinked {} in {}",
            phrase(&relinked.0, 3, "skills"),
            phrase(&relinked.1, 2, "agents")
        ));
    }
    if parts.is_empty() {
        return if skips.is_empty() {
            "nothing to do".into()
        } else {
            format!("nothing to do; {skips}")
        };
    }
    let mut out = parts.join("; ");
    if !skips.is_empty() {
        out.push_str(&format!("; {skips}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn link(skill: &str, agent: &str) -> Action {
        Action::Link {
            agent: agent.into(),
            skill: skill.into(),
            path: PathBuf::new(),
            target: PathBuf::new(),
        }
    }
    fn unlink(skill: &str, agent: &str) -> Action {
        Action::Unlink {
            agent: agent.into(),
            skill: skill.into(),
            path: PathBuf::new(),
        }
    }
    fn skip() -> Action {
        Action::Skip {
            agent: "a".into(),
            skill: "s".into(),
            reason: "because".into(),
        }
    }

    #[test]
    fn names_a_few_and_counts_many() {
        assert_eq!(summarize(&[link("one", "claude")]), "added one to claude");
        assert_eq!(
            summarize(&[link("one", "claude"), link("two", "claude")]),
            "added one and two to claude"
        );
        // The same skill on two agents is still one skill.
        assert_eq!(
            summarize(&[link("one", "claude"), link("one", "codex")]),
            "added one to claude and codex"
        );
        assert_eq!(
            summarize(&[
                link("a", "claude"),
                link("b", "claude"),
                link("c", "claude"),
                link("d", "claude")
            ]),
            "added 4 skills to claude"
        );
    }

    #[test]
    fn separates_the_two_directions() {
        assert_eq!(
            summarize(&[link("one", "claude"), unlink("two", "codex")]),
            "added one to claude; removed two from codex"
        );
    }

    #[test]
    fn reports_when_there_was_nothing_to_do() {
        assert_eq!(summarize(&[]), "nothing to do");
        assert_eq!(summarize(&[skip()]), "nothing to do; skipped s (because)");
        assert_eq!(
            summarize(&[link("one", "claude"), skip()]),
            "added one to claude; skipped s (because)"
        );
    }
}

/// A filesystem alias does not necessarily disambiguate an agent's skill name.
#[derive(Debug, Clone, Serialize)]
pub struct NameConflict {
    pub agent: String,
    pub skill: String,
    pub name: String,
    pub other_path: PathBuf,
    pub other_skill: Option<String>,
}

pub fn name_conflicts(snap: &Snapshot, actions: &[Action]) -> Vec<NameConflict> {
    let mut conflicts = Vec::new();
    for action in actions {
        let Action::Link {
            agent, skill, path, ..
        } = action
        else {
            continue;
        };
        let Some(record) = snap.get(skill) else {
            continue;
        };
        let Some(name) = record.name.as_ref() else {
            continue;
        };
        let Some(report) = snap.agent(agent) else {
            continue;
        };
        for (alias, state) in &report.entries {
            let other_path = report.skills_dir.join(alias);
            if actions
                .iter()
                .any(|a| matches!(a, Action::Unlink {path,..} if *path == other_path))
            {
                continue;
            }
            let resolved = std::fs::canonicalize(&other_path).ok();
            let other = snap
                .skills
                .iter()
                .find(|s| resolved.is_some() && std::fs::canonicalize(&s.path).ok() == resolved);
            if other.is_some_and(|s| &s.key == skill) {
                continue;
            }

            let actual_name = report.documents.get(alias).map(|d| &d.name);
            if actual_name == Some(name) || &other_path == path {
                conflicts.push(NameConflict {
                    agent: agent.clone(),
                    skill: skill.clone(),
                    name: name.clone(),
                    other_path,
                    other_skill: if matches!(state, EntryState::Deployed) {
                        other.map(|s| s.key.clone())
                    } else {
                        None
                    },
                });
            }
        }
        for other in actions {
            if let Action::Link {
                agent: oa,
                skill: os,
                path: op,
                ..
            } = other
                && oa == agent
                && os != skill
                && (op == path || snap.get(os).and_then(|r| r.name.as_ref()) == Some(name))
            {
                conflicts.push(NameConflict {
                    agent: agent.clone(),
                    skill: skill.clone(),
                    name: name.clone(),
                    other_path: op.clone(),
                    other_skill: Some(os.clone()),
                });
            }
        }
    }
    conflicts
}

/// None requires an explicit decision; replace only unlinks tracked entries.
pub fn resolve_names(
    snap: &Snapshot,
    actions: &[Action],
    policy: Option<&str>,
) -> Result<Vec<Action>> {
    let conflicts = name_conflicts(snap, actions);
    if conflicts.is_empty() {
        return Ok(actions.to_vec());
    }
    match policy {
        Some("coexist") => {
            bail!("same-directory conflicts require choosing one skill; coexist is not supported")
        }
        Some("replace") => {
            let mut resolved = Vec::new();
            let mut seen = BTreeSet::new();
            for conflict in conflicts {
                let skill = conflict.other_skill.context(
                    "cannot replace an agent-owned or foreign entry; manage it explicitly",
                )?;
                if actions.iter().any(|a| matches!(a,Action::Link {skill:s,agent,..} if *s == skill && *agent == conflict.agent)) { bail!("cannot replace when the batch selects multiple skills with the same name; choose one") }
                if seen.insert(conflict.other_path.clone()) {
                    resolved.push(Action::Unlink {
                        agent: conflict.agent,
                        skill,
                        path: conflict.other_path,
                    });
                }
            }
            resolved.extend_from_slice(actions);
            Ok(resolved)
        }
        _ => bail!(
            "skill name conflict: {}; choose one skill or --same-name replace",
            conflicts
                .iter()
                .map(|c| format!(
                    "{}: {} and {} both declare {}",
                    c.agent,
                    c.skill,
                    c.other_path.display(),
                    c.name
                ))
                .collect::<Vec<_>>()
                .join("; ")
        ),
    }
}
