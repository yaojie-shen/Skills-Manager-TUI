//! Explicit deployment destinations, independent of the source store's scope.
use crate::{Workspace, config::AgentConfig, paths};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Registry {
    #[serde(default)]
    agents: Vec<AgentConfig>,
    #[serde(default)]
    projects: std::collections::BTreeMap<String, PathBuf>,
}
fn registry_path(root: &Path) -> PathBuf {
    paths::meta_dir(root).join("deployment-targets.toml")
}
fn load(root: &Path) -> Result<Registry> {
    match std::fs::read_to_string(registry_path(root)) {
        Ok(text) => toml::from_str(&text).context("invalid deployment-targets.toml"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Registry::default()),
        Err(e) => Err(e.into()),
    }
}
/// Explicit picker destinations retain their manual deployment state during sync.
pub fn registered_keys(root: &Path) -> Result<std::collections::BTreeSet<String>> {
    Ok(load(root)?.agents.into_iter().map(|a| a.key).collect())
}

/// Only explicitly registered destinations are added; catalog entries are never auto-enabled.
pub fn extend(root: &Path, agents: &mut Vec<AgentConfig>) -> Result<()> {
    for agent in load(root)?.agents {
        ensure!(
            Path::new(&agent.skills_dir).is_absolute(),
            "deployment target must be absolute"
        );
        if let Some(existing) = agents.iter().find(|a| a.key == agent.key) {
            ensure!(
                existing.skills_path() == agent.skills_path(),
                "conflicting deployment target: {}",
                agent.key
            );
        } else {
            agents.push(agent);
        }
    }
    Ok(())
}

/// Candidate agents in one scope; opening a picker performs no writes.
pub fn candidates(ws: &Workspace, project: Option<&Path>) -> Result<Vec<AgentConfig>> {
    let registry = load(&ws.root)?;
    let local = project.is_some();
    let mut out = Vec::new();
    for definition in crate::agents::BUILTINS {
        if ws.project.as_deref() == project
            && let Some(configured) = ws.config.agent(definition.key)
        {
            out.push(configured.clone());
            continue;
        }
        let mut agent = definition.config(local);
        let path = if let Some(project) = project {
            let path = project.join(&agent.skills_dir);
            paths::ensure_local_path(project, &path)?;
            path
        } else {
            agent.skills_path()
        };
        let digest: String = Sha256::digest(path.as_os_str().as_encoded_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let id = format!(
            "{}-{}-{}",
            agent.key,
            if local { "local" } else { "global" },
            &digest[..10]
        );
        if let Some(existing) = ws
            .config
            .agents
            .iter()
            .find(|a| a.skills_path() == path && (a.key == agent.key || a.key == id))
        {
            agent = existing.clone();
        } else {
            agent.key = id;
            agent.name = format!(
                "{} ({})",
                agent.name,
                if local { "local" } else { "global" }
            );
            agent.skills_dir = path.to_string_lossy().into_owned();
        }
        out.push(agent);
    }
    for agent in &ws.config.agents {
        let matches_scope = if registry.agents.iter().any(|a| a.key == agent.key) {
            registry.projects.get(&agent.key).map(PathBuf::as_path) == project
        } else {
            ws.project.as_deref() == project
        };
        if matches_scope && !out.iter().any(|a| a.key == agent.key) {
            out.push(agent.clone());
        }
    }
    Ok(out)
}

pub fn apply(
    ws: &Workspace,
    keys: &[String],
    changes: &[(AgentConfig, bool)],
    project: Option<&Path>,
) -> Result<(String, Option<crate::history::Intent>)> {
    let mut scoped = ws.clone();
    scoped.config.agents = changes.iter().map(|(a, _)| a.clone()).collect();
    for agent in &scoped.config.agents {
        if let Some(project) = project {
            paths::ensure_local_path(project, &agent.skills_path())?;
        }
    }
    let snap = scoped.scan()?;
    let mut actions = Vec::new();
    let mut visited = std::collections::BTreeMap::new();
    for (agent, on) in changes {
        if let Some(previous) = visited.insert(agent.skills_path(), *on) {
            ensure!(
                previous == *on,
                "agents sharing a directory must have the same selection"
            );
            continue;
        }
        let agents = vec![agent.key.clone()];
        actions.extend(if *on {
            super::deploy::plan_deploy(&scoped, &snap, keys, &agents)?
        } else {
            super::deploy::plan_undeploy(&scoped, &snap, keys, &agents)?
        });
    }
    let actions = super::deploy::resolve_names(&snap, &actions, None)?;
    if actions.iter().any(super::deploy::Action::is_change) {
        let mut registry = load(&ws.root)?;
        for (agent, _) in changes {
            if !ws.config.agents.iter().any(|a| a.key == agent.key)
                && !registry.agents.iter().any(|a| a.key == agent.key)
            {
                registry.agents.push(agent.clone());
                if let Some(project) = project {
                    registry
                        .projects
                        .insert(agent.key.clone(), project.to_path_buf());
                }
            }
        }
        // Record first so even a partially failed filesystem operation remains discoverable.
        crate::util::write_atomic(
            &registry_path(&ws.root),
            toml::to_string_pretty(&registry)?.as_bytes(),
        )?;
    }
    super::deploy::apply(&actions)?;
    Ok((
        super::deploy::summarize(&actions),
        crate::history::Intent::from_actions(&actions),
    ))
}
