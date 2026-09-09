//! Symlink deployment: deploy, undeploy, sync to desired state, convert dir-linked agents.

use crate::Workspace;
use crate::hash::hash_directory;
use crate::preset::Preset;
use crate::reconcile::{AgentDirMode, DeployState, EntryState, Snapshot};
use crate::util::is_symlink;
use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::collections::BTreeSet;
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
                Some(r) if r.status.is_present() => r,
                Some(r) => {
                    actions.push(Action::Skip {
                        agent: agent.clone(),
                        skill: skill.clone(),
                        reason: format!("skill is {}", r.status.label()),
                    });
                    continue;
                }
                None => bail!("no such skill: {skill}"),
            };
            match &report.mode {
                AgentDirMode::DirLinked => actions.push(Action::Skip {
                    agent: agent.clone(),
                    skill: skill.clone(),
                    reason: if skill.contains('/') {
                        "repository skill requires a separate per-skill deployment directory"
                    } else {
                        "agent reads the skills root directly; already deployed"
                    }
                    .into(),
                }),
                AgentDirMode::DirForeign { target } => actions.push(Action::Skip {
                    agent: agent.clone(),
                    skill: skill.clone(),
                    reason: format!("agent dir is a symlink to {}", target.display()),
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
                        path: dir.join(crate::repository::default_deploy_name(skill)),
                        target: rec.path.clone(),
                    });
                }
                AgentDirMode::Real | AgentDirMode::SharedRoot => match report
                    .entries
                    .get(&crate::repository::default_deploy_name(skill))
                {
                    None => actions.push(Action::Link {
                        agent: agent.clone(),
                        skill: skill.clone(),
                        path: dir.join(crate::repository::default_deploy_name(skill)),
                        target: rec.path.clone(),
                    }),
                    Some(EntryState::Deployed) => actions.push(Action::Skip {
                        agent: agent.clone(),
                        skill: skill.clone(),
                        reason: "already deployed".into(),
                    }),
                    Some(EntryState::Broken { .. }) => {
                        actions.push(Action::Unlink {
                            agent: agent.clone(),
                            skill: skill.clone(),
                            path: dir.join(crate::repository::default_deploy_name(skill)),
                        });
                        actions.push(Action::Link {
                            agent: agent.clone(),
                            skill: skill.clone(),
                            path: dir.join(crate::repository::default_deploy_name(skill)),
                            target: rec.path.clone(),
                        });
                    }
                    Some(other) => actions.push(Action::Skip {
                        agent: agent.clone(),
                        skill: skill.clone(),
                        reason: format!("entry is {}; not touching it", other.label()),
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
            match &report.mode {
                AgentDirMode::SharedRoot if !skill.contains('/') => actions.push(Action::Skip {
                    agent: agent.clone(),
                    skill: skill.clone(),
                    reason:
                        "skill lives in the shared root; use remove to delete it for every reader"
                            .into(),
                }),
                AgentDirMode::DirLinked => actions.push(Action::Skip {
                    agent: agent.clone(),
                    skill: skill.clone(),
                    reason: "agent dir is a whole-directory link; run `agents convert` first"
                        .into(),
                }),
                AgentDirMode::Real | AgentDirMode::SharedRoot => match report
                    .entries
                    .get(&crate::repository::default_deploy_name(skill))
                {
                    Some(EntryState::Deployed) | Some(EntryState::Broken { .. }) => {
                        actions.push(Action::Unlink {
                            agent: agent.clone(),
                            skill: skill.clone(),
                            path: dir.join(crate::repository::default_deploy_name(skill)),
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
        AgentDirMode::SharedRoot => {
            "shared root contains source skills; per-agent repair is unavailable".to_string()
        }
        AgentDirMode::DirLinked => {
            "agent dir is a whole-directory link; run `agents convert` first".to_string()
        }
        AgentDirMode::DirForeign { target } => {
            format!("agent dir is a symlink to {}", target.display())
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
            EntryState::Broken { .. } => actions.push(Action::Unlink {
                agent: agent.clone(),
                skill: snap
                    .skills
                    .iter()
                    .find(|s| s.deployment_name() == name)
                    .map(|s| s.key.clone())
                    .unwrap_or_else(|| name.into()),
                path: dir.join(name),
            }),
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
                    .find(|s| s.deployment_name() == name)
                    .map(|s| s.key.clone())
                    .unwrap_or_else(|| name.into()),
                path: dir.join(name),
                target: snap
                    .skills
                    .iter()
                    .find(|s| s.deployment_name() == name)
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

/// Desired (skill, agent) pairs from config: all-to-all and/or auto-deployed presets.
pub fn desired_pairs(ws: &Workspace, snap: &Snapshot) -> Result<BTreeSet<(String, String)>> {
    let mut pairs = BTreeSet::new();
    let explicit = super::targets::registered_keys(&ws.root)?;
    let desired = super::targets::desired(&ws.root)?;
    for record in snap.skills.iter().filter(|s| s.status.is_present()) {
        for agent in snap.agents.iter().filter(|a| explicit.contains(&a.key)) {
            if desired.get(&agent.key).map_or_else(
                || {
                    matches!(
                        agent.entries.get(&record.deployment_name()),
                        Some(EntryState::Deployed | EntryState::Broken { .. })
                    )
                },
                |keys| keys.contains(&record.key),
            ) {
                pairs.insert((record.key.clone(), agent.key.clone()));
            }
        }
    }
    let agents = ws.config.agent_keys();
    let present: Vec<&str> = snap
        .skills
        .iter()
        .filter(|s| s.status.is_present())
        .map(|s| s.key.as_str())
        .collect();
    if ws.config.deploy.all_to_all {
        for s in &present {
            for a in &agents {
                if explicit.contains(a) {
                    continue;
                }
                pairs.insert((s.to_string(), a.clone()));
            }
        }
    }
    for name in &ws.config.deploy.presets {
        let preset = ws
            .presets
            .load(name)?
            .with_context(|| format!("deploy.presets names unknown preset {name}"))?;
        let targets = if preset.agents.is_empty() {
            agents.clone()
        } else {
            preset.agents.clone()
        };
        for s in &preset.skills {
            if present.contains(&s.as_str()) {
                for a in &targets {
                    if explicit.contains(a) {
                        continue;
                    }
                    pairs.insert((s.clone(), a.clone()));
                }
            }
        }
    }
    Ok(pairs)
}

/// Plan making reality match the desired state: create missing links, remove
/// links into the root that are no longer desired, clean broken links.
pub fn plan_sync(ws: &Workspace, snap: &Snapshot) -> Result<Vec<Action>> {
    let desired = desired_pairs(ws, snap)?;
    let mut actions = Vec::new();
    let mut seen = BTreeSet::new();
    for a in &ws.config.agents {
        let report = snap.agent(&a.key).context("agent not scanned")?;
        let dir = a.skills_path();
        let identity = std::fs::canonicalize(&dir).unwrap_or(dir.clone());
        if !seen.insert(identity.clone()) {
            continue;
        }
        let readers: BTreeSet<_> = ws
            .config
            .agents
            .iter()
            .filter(|other| {
                let path = other.skills_path();
                std::fs::canonicalize(&path).unwrap_or(path) == identity
            })
            .map(|other| other.key.as_str())
            .collect();
        let wanted: BTreeSet<String> = desired
            .iter()
            .filter(|(_, ag)| readers.contains(ag.as_str()))
            .map(|(s, _)| s.clone())
            .collect();
        match &report.mode {
            AgentDirMode::DirLinked => {
                if !wanted.is_empty() {
                    actions.push(Action::Skip {
                        agent: a.key.clone(),
                        skill: "*".into(),
                        reason: "whole-directory link; everything is deployed".into(),
                    });
                }
                continue;
            }
            AgentDirMode::DirForeign { target } => {
                actions.push(Action::Skip {
                    agent: a.key.clone(),
                    skill: "*".into(),
                    reason: format!("agent dir is a symlink to {}", target.display()),
                });
                continue;
            }
            AgentDirMode::Missing => {
                if wanted.is_empty() {
                    continue;
                }
                actions.push(Action::Mkdir {
                    agent: a.key.clone(),
                    path: dir.clone(),
                });
                for s in &wanted {
                    actions.push(Action::Link {
                        agent: a.key.clone(),
                        skill: s.clone(),
                        path: dir.join(crate::repository::default_deploy_name(s)),
                        target: ws.skill_path(s),
                    });
                }
            }
            AgentDirMode::Real | AgentDirMode::SharedRoot => {
                for s in &wanted {
                    match report
                        .entries
                        .get(&crate::repository::default_deploy_name(s))
                    {
                        None => actions.push(Action::Link {
                            agent: a.key.clone(),
                            skill: s.clone(),
                            path: dir.join(crate::repository::default_deploy_name(s)),
                            target: ws.skill_path(s),
                        }),
                        Some(EntryState::Deployed) => {}
                        Some(EntryState::Broken { .. }) => {
                            actions.push(Action::Unlink {
                                agent: a.key.clone(),
                                skill: s.clone(),
                                path: dir.join(crate::repository::default_deploy_name(s)),
                            });
                            actions.push(Action::Link {
                                agent: a.key.clone(),
                                skill: s.clone(),
                                path: dir.join(crate::repository::default_deploy_name(s)),
                                target: ws.skill_path(s),
                            });
                        }
                        Some(other) => actions.push(Action::Skip {
                            agent: a.key.clone(),
                            skill: s.clone(),
                            reason: format!("entry is {}", other.label()),
                        }),
                    }
                }
                for (name, state) in &report.entries {
                    if report.mode == AgentDirMode::SharedRoot && snap.get(name).is_some() {
                        continue; // Source entries cannot be disabled for only one reader.
                    }
                    let is_wanted = wanted
                        .iter()
                        .any(|w| crate::repository::default_deploy_name(w) == *name);
                    match state {
                        EntryState::Deployed if !is_wanted => actions.push(Action::Unlink {
                            agent: a.key.clone(),
                            skill: snap
                                .skills
                                .iter()
                                .find(|s| s.deployment_name() == *name)
                                .map(|s| s.key.clone())
                                .unwrap_or_else(|| name.clone()),
                            path: dir.join(name),
                        }),
                        EntryState::Broken { .. } if !is_wanted => actions.push(Action::Unlink {
                            agent: a.key.clone(),
                            skill: snap
                                .skills
                                .iter()
                                .find(|s| s.deployment_name() == *name)
                                .map(|s| s.key.clone())
                                .unwrap_or_else(|| name.clone()),
                            path: dir.join(name),
                        }),
                        _ => {}
                    }
                }
            }
        }
    }
    Ok(actions)
}

/// Plan turning a whole-directory link into a real directory with per-skill links.
pub fn plan_convert(ws: &Workspace, snap: &Snapshot, agent: &str) -> Result<Vec<Action>> {
    let cfg = ws
        .config
        .agent(agent)
        .with_context(|| format!("unknown agent: {agent}"))?;
    let report = snap.agent(agent).context("agent not scanned")?;
    if report.mode != AgentDirMode::DirLinked {
        bail!(
            "agent {agent} is not a whole-directory link (mode: {:?})",
            report.mode
        );
    }
    let dir = cfg.skills_path();
    let mut actions = vec![
        Action::Unlink {
            agent: agent.into(),
            skill: "*".into(),
            path: dir.clone(),
        },
        Action::Mkdir {
            agent: agent.into(),
            path: dir.clone(),
        },
    ];
    for s in snap.skills.iter().filter(|s| s.status.is_present()) {
        actions.push(Action::Link {
            agent: agent.into(),
            skill: s.key.clone(),
            path: dir.join(s.deployment_name()),
            target: s.path.clone(),
        });
    }
    Ok(actions)
}

/// Execute planned actions in order. Stops at the first failure.
pub fn apply(actions: &[Action]) -> Result<usize> {
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
        }
    }
    Ok(done)
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
    /// No deployable member, or no agent in scope.
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

    /// How far along a preset is that is neither on nor off, `5/12`. All the
    /// way on or all the way off is already told by the colour and the mark, so
    /// a count there says nothing and only makes the pills harder to read.
    /// A preset with nothing deployable is worth a word of its own.
    pub fn progress(&self) -> Option<String> {
        match self.state() {
            PresetState::Partial => Some(format!("{}/{}", self.installed, self.total)),
            PresetState::Empty => Some("empty".into()),
            PresetState::Active | PresetState::Inactive => None,
        }
    }
}

/// Agents a preset applies to inside `scope`: its own list narrowed to the
/// scope, or the whole scope when the preset targets everything.
pub fn preset_agents(preset: &Preset, scope: &[String]) -> Vec<String> {
    if preset.agents.is_empty() {
        scope.to_vec()
    } else {
        scope
            .iter()
            .filter(|a| preset.agents.contains(a))
            .cloned()
            .collect()
    }
}

/// Count how much of `preset` is deployed across `scope`. Only members that
/// exist in the skills root count; a member whose directory is gone is listed
/// in `absent` and excluded from the total, so a preset referring to a deleted
/// skill can still read as complete.
pub fn preset_status(snap: &Snapshot, preset: &Preset, scope: &[String]) -> PresetStatus {
    let agents = preset_agents(preset, scope);
    let mut installed = 0;
    let mut total = 0;
    let mut absent = Vec::new();
    for skill in &preset.skills {
        match snap.get(skill) {
            Some(rec) if rec.status.is_present() => {
                for agent in &agents {
                    total += 1;
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
    let agents = preset_agents(preset, scope);
    let present: Vec<String> = preset
        .skills
        .iter()
        .filter(|s| snap.get(s).is_some_and(|r| r.status.is_present()))
        .cloned()
        .collect();
    let mut actions = plan_deploy(ws, snap, &present, &agents)?;
    for skill in preset.skills.iter().filter(|s| !present.contains(s)) {
        actions.push(Action::Skip {
            agent: "*".into(),
            skill: skill.clone(),
            reason: "not in the skills root".into(),
        });
    }
    Ok(actions)
}

/// Undeploy every member of `preset` from `scope`. Overlap with other presets
/// is deliberately ignored: a preset is applied as a one-time copy, not a live
/// membership, so deactivating removes all of its skills.
pub fn plan_preset_deactivate(
    ws: &Workspace,
    snap: &Snapshot,
    preset: &Preset,
    scope: &[String],
) -> Result<Vec<Action>> {
    let agents = preset_agents(preset, scope);
    let present: Vec<String> = preset
        .skills
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
            Action::Unlink { skill, agent, .. } => {
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
            if &other_path == path
                || actions
                    .iter()
                    .any(|a| matches!(a, Action::Unlink {path,..} if *path == other_path))
            {
                continue;
            }
            let other = snap.skills.iter().find(|s| s.deployment_name() == *alias);
            let actual_name = crate::skill::SkillDoc::load(&other_path)
                .ok()
                .map(|d| d.name);
            if actual_name.as_ref() == Some(name) {
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
                && snap.get(os).and_then(|r| r.name.as_ref()) == Some(name)
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

/// None requires an explicit decision; replace only unlinks managed entries.
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
        Some("coexist") => Ok(actions.to_vec()),
        Some("replace") => {
            let mut resolved = Vec::new();
            let mut seen = BTreeSet::new();
            for conflict in conflicts {
                let skill = conflict.other_skill.context("cannot replace an agent-owned or foreign entry; choose coexist or manage it explicitly")?;
                if actions.iter().any(|a| matches!(a,Action::Link {skill:s,agent,..} if *s == skill && *agent == conflict.agent)) { bail!("cannot replace when the batch selects multiple skills with the same name; choose one or coexist") }
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
            "skill name conflict: {}; choose --same-name coexist or --same-name replace",
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
