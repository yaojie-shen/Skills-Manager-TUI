//! Explicit deployment destinations, independent of the source store's scope.
use crate::{Workspace, config::AgentConfig, paths};
use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// A deployment location discovered from the launch directory, never a source store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    pub project: Option<PathBuf>,
    pub repository: bool,
    pub directory: Option<PathBuf>,
    pub label: Option<String>,
    pub links: Vec<(PathBuf, PathBuf)>,
}

impl Scope {
    pub fn name(&self) -> String {
        if let Some(label) = &self.label {
            return label.clone();
        }
        self.project
            .as_ref()
            .map(|p| {
                p.file_name()
                    .unwrap_or(p.as_os_str())
                    .to_string_lossy()
                    .into_owned()
            })
            .unwrap_or_else(|| "Global".into())
    }

    pub fn path_label(&self) -> String {
        if let Some(path) = &self.directory {
            return paths::contract_tilde(path);
        }
        self.project
            .as_ref()
            .map(|p| paths::contract_tilde(p))
            .unwrap_or_else(|| "~".into())
    }
}

/// Only the launch directory is a local scope, including inside a monorepo.
/// Discovery never walks ancestors or creates directories.
pub fn discover_scopes(start: &Path) -> Result<Vec<Scope>> {
    let start = std::fs::canonicalize(start).context("resolving launch directory")?;
    ensure!(start.is_dir(), "launch directory must be a directory");
    Ok(vec![
        Scope {
            project: None,
            repository: false,
            directory: None,
            label: None,
            links: vec![],
        },
        Scope {
            repository: start.join(".git").exists(),
            project: Some(start),
            directory: None,
            label: None,
            links: vec![],
        },
    ])
}

/// Physical skill directories read by one configured agent. A shared root is
/// offered only to agents documented to discover it. Explicit config survives.
pub fn locations(ws: &Workspace, configured: &AgentConfig, start: &Path) -> Result<Vec<Scope>> {
    locations_in(ws, configured, start, &paths::expand_tilde("~"))
}

fn locations_in(
    ws: &Workspace,
    configured: &AgentConfig,
    start: &Path,
    home: &Path,
) -> Result<Vec<Scope>> {
    let start = std::fs::canonicalize(start).context("resolving launch directory")?;
    let definition = crate::agents::BUILTINS
        .iter()
        .find(|a| a.key == configured.key);
    let mut out = Vec::new();
    for local in [false, true] {
        let mut dirs: Vec<PathBuf> = definition
            .map(|a| {
                a.search_dirs(local)
                    .into_iter()
                    .map(|p| {
                        if local {
                            start.join(p)
                        } else {
                            home.join(p.trim_start_matches("~/"))
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        if ws.project.is_some() == local {
            let configured_path = configured.skills_path();
            if !local || configured_path.starts_with(&start) {
                dirs.insert(0, configured_path);
            }
        }
        for dir in dirs {
            if out
                .iter()
                .any(|scope: &Scope| scope.directory.as_ref() == Some(&dir))
            {
                continue;
            }
            let folder = dir
                .parent()
                .and_then(|p| p.file_name())
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|| "custom".into());
            let label = format!(
                "{} {}",
                if local { "Local" } else { "Global" },
                if folder == ".agents" {
                    "shared"
                } else {
                    &folder
                }
            );
            out.push(Scope {
                project: local.then(|| start.clone()),
                repository: false,
                directory: Some(dir),
                label: Some(label),
                links: vec![],
            });
        }
    }
    // Only recognize links to another documented skill location. Arbitrary
    // external directory links retain the existing foreign-directory protection.
    for local in [false, true] {
        let mut peers: Vec<PathBuf> = crate::agents::BUILTINS
            .iter()
            .flat_map(|a| a.search_dirs(local))
            .map(|p| {
                if local {
                    start.join(p)
                } else {
                    paths::expand_tilde(p)
                }
            })
            .collect();
        peers.extend(
            out.iter()
                .filter(|s| s.project.is_some() == local)
                .filter_map(|s| s.directory.clone()),
        );
        peers.sort();
        peers.dedup();
        let links: Vec<_> = peers
            .iter()
            .filter_map(|source| {
                let target = std::fs::read_link(source).ok()?;
                let target = if target.is_absolute() {
                    target
                } else {
                    source.parent()?.join(target)
                };
                let resolved = crate::agents::linked_skill_directory(source)?;
                if local && !resolved.starts_with(&start) {
                    return None;
                }
                Some((source.clone(), resolve(&start, &target), resolved))
            })
            .collect();
        if links.is_empty() {
            continue;
        }
        for scope in out.iter_mut().filter(|s| s.project.is_some() == local) {
            let Some(dir) = &scope.directory else {
                continue;
            };
            let physical = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.clone());
            scope.links = links
                .iter()
                .filter(|(_, _, target)| *target == physical)
                .map(|(source, target, _)| (source.clone(), target.clone()))
                .collect();
            if !scope.links.is_empty() {
                scope.directory = Some(physical);
            }
        }
    }
    let mut merged: Vec<Scope> = Vec::new();
    for scope in out {
        if !merged
            .iter()
            .any(|s| s.project == scope.project && s.directory == scope.directory)
        {
            merged.push(scope);
        }
    }
    Ok(merged)
}

/// Identify each product and physical scope separately within the current session.
pub fn scope_agent(ws: &Workspace, configured: &AgentConfig, scope: &Scope) -> Result<AgentConfig> {
    let path = scope
        .directory
        .as_ref()
        .context("scope has no skill directory")?;
    if let Some(project) = &scope.project {
        paths::ensure_local_path(project, path)?;
    }
    if configured.skills_path() == *path {
        return Ok(configured.clone());
    }
    let local = scope.project.is_some();
    let identity = match &scope.project {
        Some(project) => format!(
            "{}:{}",
            portable(&ws.root, project, ws.project.is_none()).display(),
            path.strip_prefix(project)?.display()
        ),
        None => portable(&ws.root, path, true).display().to_string(),
    };
    let digest: String = Sha256::digest(identity.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let key = format!(
        "{}-{}-{}",
        configured.key,
        if local { "local" } else { "global" },
        &digest[..10]
    );
    Ok(AgentConfig {
        key,
        name: configured.display_name().into(),
        skills_dir: path.to_string_lossy().into_owned(),
    })
}

/// Every documented physical destination in the selected global/local tier.
pub fn all_candidates(ws: &Workspace, project: Option<&Path>) -> Result<Vec<AgentConfig>> {
    all_candidates_in(ws, project, &paths::expand_tilde("~"))
}

pub fn all_candidates_in(
    ws: &Workspace,
    project: Option<&Path>,
    home: &Path,
) -> Result<Vec<AgentConfig>> {
    let start = project
        .map(Path::to_path_buf)
        .unwrap_or(std::env::current_dir()?);
    let mut out = Vec::new();
    for definition in crate::agents::BUILTINS {
        let configured = ws.config.agent(definition.key).cloned().unwrap_or_else(|| {
            let mut agent = definition.config(false);
            agent.skills_dir = home
                .join(definition.global_dir.trim_start_matches("~/"))
                .display()
                .to_string();
            agent
        });
        for scope in locations_in(ws, &configured, &start, home)?
            .into_iter()
            .filter(|s| s.project.is_some() == project.is_some())
        {
            let mut agent = scope_agent(ws, &configured, &scope)?;
            agent.name = format!("{} · {}", definition.name, scope.name());
            out.push(agent);
        }
    }
    for configured in &ws.config.agents {
        if crate::agents::BUILTINS
            .iter()
            .any(|a| a.key == configured.key)
            || out.iter().any(|a| a.key == configured.key)
        {
            continue;
        }
        let matches_scope = ws.project.as_deref() == project;
        if matches_scope {
            out.push(configured.clone());
        }
    }
    Ok(out)
}

/// Normalize relative locators without requiring the destination to exist yet.
fn resolve(root: &Path, value: &Path) -> PathBuf {
    let expanded = paths::expand_tilde(&value.to_string_lossy());
    let mut out = PathBuf::new();
    for part in root.join(expanded).components() {
        match part {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn relative(root: &Path, path: &Path) -> PathBuf {
    let left: Vec<_> = root.components().collect();
    let right: Vec<_> = path.components().collect();
    let common = left.iter().zip(&right).take_while(|(a, b)| a == b).count();
    let mut out = PathBuf::new();
    for _ in common..left.len() {
        out.push("..");
    }
    for part in &right[common..] {
        out.push(part.as_os_str());
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

fn portable(root: &Path, path: &Path, home: bool) -> PathBuf {
    if home {
        let contracted = paths::contract_tilde(path);
        if contracted == "~" || contracted.starts_with("~/") {
            return contracted.into();
        }
    }
    relative(root, path)
}

/// Candidate agents in one scope; opening a picker performs no writes.
pub fn candidates(ws: &Workspace, project: Option<&Path>) -> Result<Vec<AgentConfig>> {
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
        let identity = if let Some(project) = project {
            format!(
                "{}:{}",
                portable(&ws.root, project, ws.project.is_none()).display(),
                path.strip_prefix(project)?.display()
            )
        } else {
            portable(&ws.root, &path, true).display().to_string()
        };
        let digest: String = Sha256::digest(identity.as_bytes())
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
        let matches_scope = ws.project.as_deref() == project;
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
    let changes: Vec<_> = changes
        .iter()
        .map(|(a, on)| (a.clone(), *on, project.map(Path::to_path_buf)))
        .collect();
    apply_scoped(ws, keys, &changes)
}

/// Validate all selected current-scope destinations before modifying links.
pub fn apply_scoped(
    ws: &Workspace,
    keys: &[String],
    changes: &[(AgentConfig, bool, Option<PathBuf>)],
) -> Result<(String, Option<crate::history::Intent>)> {
    let mut scoped = ws.clone();
    scoped.config.agents = changes.iter().map(|(a, _, _)| a.clone()).collect();
    for (agent, _, project) in changes {
        if let Some(project) = project {
            paths::ensure_local_path(project, &agent.skills_path())?;
        }
    }
    let snap = scoped.scan_for_links()?;
    for (agent, _, _) in changes {
        if let Some(report) = snap.agent(&agent.key)
            && let crate::reconcile::AgentDirMode::ReadOnly { reason, .. } = &report.mode
        {
            anyhow::bail!(
                "{}: agent directory is read-only: {reason}",
                agent.display_name()
            );
        }
    }
    let mut actions = Vec::new();
    let mut visited = std::collections::BTreeMap::new();
    for (agent, on, _) in changes {
        if let Some(previous) = visited.insert(
            std::fs::canonicalize(agent.skills_path()).unwrap_or_else(|_| agent.skills_path()),
            *on,
        ) {
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
    let mut deployments = Vec::new();
    for (agent, on, project) in changes {
        let before = deployed_in(&snap, agent);
        let mut after = before.clone();
        if *on {
            after.extend(keys.iter().cloned());
        } else {
            for key in keys {
                after.remove(key);
            }
        }
        deployments.push(super::name_choices::Change {
            agent: agent.clone(),
            project: project.clone(),
            before,
            after,
        });
    }
    if let Some(pending) =
        super::name_choices::Pending::from_plan(deployments.clone(), &snap, &actions)?
    {
        return Err(pending.into());
    }
    let actions = super::deploy::resolve_names(&snap, &actions, None)?;
    // Validate the full batch before changing any destination.
    for action in &actions {
        if let super::deploy::Action::Skip { reason, skill, .. } = action {
            ensure!(
                reason == "already deployed"
                    || reason == "not deployed"
                    || reason == "agent dir missing or foreign",
                "{skill}: {reason}"
            );
        }
    }
    super::deploy::apply(&actions)?;
    let mut visited = BTreeSet::new();
    let intents: Vec<_> = deployments
        .into_iter()
        .filter_map(|change| {
            let path = change.agent.skills_path();
            let identity = std::fs::canonicalize(&path).unwrap_or(path);
            if !visited.insert(identity) || change.before == change.after {
                return None;
            }
            Some(crate::history::Intent::TargetDeployment {
                agent: change.agent,
                project: change.project,
                before: change.before,
                after: change.after,
            })
        })
        .collect();
    Ok((
        super::deploy::summarize(&actions),
        (!intents.is_empty()).then_some(crate::history::Intent::Group(intents)),
    ))
}

/// A transient snapshot used only to validate an operation and support session undo.
pub type DeploymentState = BTreeSet<String>;

pub fn scan_deployed(ws: &Workspace, agent: &AgentConfig) -> Result<DeploymentState> {
    Ok(deployed_in(&scan_target(ws, agent)?, agent))
}

pub fn deployed_in(snap: &crate::reconcile::Snapshot, agent: &AgentConfig) -> DeploymentState {
    snap.skills
        .iter()
        .filter(|s| s.deploy.get(&agent.key) == Some(&crate::reconcile::DeployState::Deployed))
        .map(|s| s.key.clone())
        .collect()
}

/// Apply an explicit install/uninstall to the current directory state.
pub fn set_deployed(
    ws: &Workspace,
    agent: &AgentConfig,
    project: Option<&Path>,
    keys: &[String],
    on: bool,
) -> Result<(String, Option<crate::history::Intent>)> {
    apply_scoped(
        ws,
        keys,
        &[(agent.clone(), on, project.map(Path::to_path_buf))],
    )
}

/// Execute reviewed maintenance actions in an explicit scope. Session history
/// carries the target itself, so undo never needs a persistent target registry.
pub fn apply_actions(
    ws: &Workspace,
    agent: &AgentConfig,
    project: Option<&Path>,
    actions: &[super::deploy::Action],
) -> Result<(String, Option<crate::history::Intent>)> {
    use super::deploy::{self, Action};
    use crate::history::Intent;
    if let Some(project) = project {
        paths::ensure_local_path(project, &agent.skills_path())?;
    }
    let mut scoped = ws.clone();
    scoped.config.agents = vec![agent.clone()];
    scoped.project = project.map(Path::to_path_buf);
    let snap = scoped.scan_for_links()?;
    if let Some(pending) = super::name_choices::Pending::for_actions(&scoped, &snap, actions)? {
        return Err(pending.into());
    }
    let before = deployed_in(&snap, agent);
    // Copies, broken links and whole-directory links cannot be restored from a
    // set of installed skill keys. Keep their maintenance out of reversible history.
    let irreversible = actions.iter().any(|a| match a {
        Action::Relink { .. } => true,
        Action::Clean { .. } => true,
        Action::Unlink { skill, .. } => !before.contains(skill),
        _ => false,
    });
    let changed = deploy::apply(actions)?;
    let message = deploy::summarize(actions);
    let intent = if changed == 0 {
        None
    } else if irreversible {
        Some(Intent::OneWay {
            what: message.clone(),
        })
    } else {
        let after = scan_deployed(&scoped, agent)?;
        (before != after).then(|| Intent::TargetDeployment {
            agent: agent.clone(),
            project: project.map(Path::to_path_buf),
            before,
            after,
        })
    };
    Ok((message, intent))
}

pub fn restore_deployed(
    ws: &Workspace,
    agent: &AgentConfig,
    project: Option<&Path>,
    expected: &DeploymentState,
    desired: &DeploymentState,
) -> Result<String> {
    let snap = scan_target(ws, agent)?;
    restore_scanned_deployment(ws, agent, project, expected, desired, &snap)
}

fn scan_target(ws: &Workspace, agent: &AgentConfig) -> Result<crate::reconcile::Snapshot> {
    let mut scoped = ws.clone();
    scoped.config.agents = vec![agent.clone()];
    scoped.scan_for_links()
}

/// Share one fresh scan across validation and planning. Filesystem
/// writes still validate their destination immediately before changing it.
fn restore_scanned_deployment(
    ws: &Workspace,
    agent: &AgentConfig,
    project: Option<&Path>,
    expected: &DeploymentState,
    desired: &DeploymentState,
    snap: &crate::reconcile::Snapshot,
) -> Result<String> {
    if let Some(report) = snap.agent(&agent.key)
        && let crate::reconcile::AgentDirMode::ReadOnly { reason, .. } = &report.mode
    {
        anyhow::bail!(
            "{}: agent directory is read-only: {reason}",
            agent.display_name()
        );
    }
    ensure!(
        &deployed_in(snap, agent) == expected,
        "deployment state changed since this operation; refresh and retry"
    );
    if let Some(project) = project {
        paths::ensure_local_path(project, &agent.skills_path())?;
    }
    let mut scoped = ws.clone();
    scoped.config.agents = vec![agent.clone()];
    let removed = expected.difference(desired).cloned().collect::<Vec<_>>();
    let mut actions = super::deploy::plan_deploy(
        &scoped,
        snap,
        &desired.difference(expected).cloned().collect::<Vec<_>>(),
        std::slice::from_ref(&agent.key),
    )?;
    actions.extend(super::deploy::plan_undeploy(
        &scoped,
        snap,
        &removed,
        std::slice::from_ref(&agent.key),
    )?);
    if let Some(pending) = super::name_choices::Pending::from_plan(
        vec![super::name_choices::Change {
            agent: agent.clone(),
            project: project.map(Path::to_path_buf),
            before: expected.clone(),
            after: desired.clone(),
        }],
        snap,
        &actions,
    )? {
        return Err(pending.into());
    }
    let actions = super::deploy::resolve_names(snap, &actions, None)?;
    let mut actions = actions;
    actions.sort_by_key(|a| {
        !matches!(
            a,
            super::deploy::Action::Unlink { .. } | super::deploy::Action::Clean { .. }
        )
    });
    for action in &actions {
        if let super::deploy::Action::Skip { reason, skill, .. } = action {
            let missing_removal = removed.contains(skill)
                && snap
                    .agent(&agent.key)
                    .is_some_and(|a| a.mode == crate::reconcile::AgentDirMode::Missing);
            ensure!(
                reason == "already deployed" || reason == "not deployed" || missing_removal,
                "{skill}: {reason}"
            );
        }
    }
    super::deploy::apply(&actions)?;
    let message = format!(
        "{} · {}",
        agent.display_name(),
        super::deploy::summarize(&actions)
    );
    Ok(message)
}

/// Product identity stays separate from a destination's stable key.
pub fn product_key(agent: &AgentConfig) -> &str {
    crate::agents::BUILTINS
        .iter()
        .find(|d| {
            agent.key == d.key
                || agent.key.starts_with(&format!("{}-global-", d.key))
                || agent.key.starts_with(&format!("{}-local-", d.key))
        })
        .map(|d| d.key)
        .unwrap_or(&agent.key)
}
pub fn product_name(agent: &AgentConfig) -> &str {
    crate::agents::BUILTINS
        .iter()
        .find(|d| d.key == product_key(agent))
        .map(|d| d.name)
        .unwrap_or(agent.display_name())
}
pub fn location_label(agent: &AgentConfig, project: &Path) -> String {
    let path = agent.skills_path();
    if path.starts_with(project) {
        format!("Local   {}", path.display())
    } else {
        format!("Global  {}", paths::contract_tilde(&path))
    }
}

/// Detect installed agent products and add missing defaults to this workspace.
/// Explicit agent configuration is preserved; scope paths are resolved separately.
pub fn discover(ws: &mut Workspace, project: &Path) -> Result<()> {
    let dirs = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .unwrap_or_default();
    discover_with_paths(
        ws,
        project,
        &paths::expand_tilde("~"),
        &dirs,
        &[PathBuf::from("/Applications")],
    )
}
pub fn discover_in(ws: &mut Workspace, project: &Path, home: &Path) -> Result<()> {
    discover_with_paths(ws, project, home, &[], &[])
}
fn discover_with_paths(
    ws: &mut Workspace,
    project: &Path,
    home: &Path,
    dirs: &[PathBuf],
    applications: &[PathBuf],
) -> Result<()> {
    ws.inventory_project = Some(project.to_path_buf());
    let products = crate::agents::detect_with_applications(home, project, dirs, applications);
    for definition in crate::agents::BUILTINS {
        if products.contains(definition.key) && ws.config.agent(definition.key).is_none() {
            let mut agent = definition.config(ws.project.is_some());
            agent.skills_dir = if let Some(project) = &ws.project {
                project.join(definition.local_dir)
            } else {
                home.join(definition.global_dir.trim_start_matches("~/"))
            }
            .display()
            .to_string();
            ws.config.agents.push(agent);
        }
    }
    ws.inventory_products = Some(products);
    Ok(())
}
pub fn visible_agents(ws: &Workspace) -> impl Iterator<Item = &AgentConfig> {
    ws.config.agents.iter().filter(|a| {
        ws.inventory_products
            .as_ref()
            .is_none_or(|products| products.contains(product_key(a)))
    })
}
