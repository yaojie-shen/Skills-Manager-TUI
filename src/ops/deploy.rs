//! Symlink deployment: deploy, undeploy, sync to desired state, convert dir-linked agents.

use crate::Workspace;
use crate::reconcile::{AgentDirMode, EntryState, Snapshot};
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
    for a in agents {
        let cfg = ws
            .config
            .agent(a)
            .with_context(|| format!("unknown agent: {a}"))?;
        out.push((cfg.key.clone(), cfg.skills_path()));
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
                    reason: "agent dir is a whole-directory link to the root; already deployed"
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
                        path: dir.join(skill),
                        target: rec.path.clone(),
                    });
                }
                AgentDirMode::Real => match report.entries.get(skill) {
                    None => actions.push(Action::Link {
                        agent: agent.clone(),
                        skill: skill.clone(),
                        path: dir.join(skill),
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
                            path: dir.join(skill),
                        });
                        actions.push(Action::Link {
                            agent: agent.clone(),
                            skill: skill.clone(),
                            path: dir.join(skill),
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
                AgentDirMode::DirLinked => actions.push(Action::Skip {
                    agent: agent.clone(),
                    skill: skill.clone(),
                    reason: "agent dir is a whole-directory link; run `agents convert` first"
                        .into(),
                }),
                AgentDirMode::Real => match report.entries.get(skill) {
                    Some(EntryState::Deployed) | Some(EntryState::Broken { .. }) => {
                        actions.push(Action::Unlink {
                            agent: agent.clone(),
                            skill: skill.clone(),
                            path: dir.join(skill),
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

/// Desired (skill, agent) pairs from config: all-to-all and/or auto-deployed presets.
pub fn desired_pairs(ws: &Workspace, snap: &Snapshot) -> Result<BTreeSet<(String, String)>> {
    let mut pairs = BTreeSet::new();
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
    for a in &ws.config.agents {
        let report = snap.agent(&a.key).context("agent not scanned")?;
        let dir = a.skills_path();
        let wanted: Vec<String> = desired
            .iter()
            .filter(|(_, ag)| ag == &a.key)
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
                        path: dir.join(s),
                        target: ws.skill_path(s),
                    });
                }
            }
            AgentDirMode::Real => {
                for s in &wanted {
                    match report.entries.get(s) {
                        None => actions.push(Action::Link {
                            agent: a.key.clone(),
                            skill: s.clone(),
                            path: dir.join(s),
                            target: ws.skill_path(s),
                        }),
                        Some(EntryState::Deployed) => {}
                        Some(EntryState::Broken { .. }) => {
                            actions.push(Action::Unlink {
                                agent: a.key.clone(),
                                skill: s.clone(),
                                path: dir.join(s),
                            });
                            actions.push(Action::Link {
                                agent: a.key.clone(),
                                skill: s.clone(),
                                path: dir.join(s),
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
                    let is_wanted = wanted.iter().any(|w| w == name);
                    match state {
                        EntryState::Deployed if !is_wanted => actions.push(Action::Unlink {
                            agent: a.key.clone(),
                            skill: name.clone(),
                            path: dir.join(name),
                        }),
                        EntryState::Broken { .. } if !is_wanted => actions.push(Action::Unlink {
                            agent: a.key.clone(),
                            skill: name.clone(),
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
            path: dir.join(&s.key),
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
                std::fs::create_dir_all(path)
                    .with_context(|| format!("creating {}", path.display()))?;
                done += 1;
            }
            Action::Link { path, target, .. } => {
                if is_symlink(path) {
                    std::fs::remove_file(path)?;
                } else if path.exists() {
                    bail!("refusing to replace existing entry {}", path.display());
                }
                std::os::unix::fs::symlink(target, path).with_context(|| {
                    format!("linking {} -> {}", path.display(), target.display())
                })?;
                done += 1;
            }
            Action::Unlink { path, .. } => {
                if !is_symlink(path) {
                    bail!("refusing to remove non-symlink {}", path.display());
                }
                std::fs::remove_file(path)
                    .with_context(|| format!("removing {}", path.display()))?;
                done += 1;
            }
        }
    }
    Ok(done)
}
