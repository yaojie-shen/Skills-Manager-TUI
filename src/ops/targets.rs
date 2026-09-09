//! Explicit deployment destinations, independent of the source store's scope.
use crate::{Workspace, config::AgentConfig, paths};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Registry {
    #[serde(default)]
    agents: Vec<AgentConfig>,
    #[serde(default)]
    selections: std::collections::BTreeMap<String, Selection>,
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
/// A deployment location discovered from the launch directory, never a source store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    pub project: Option<PathBuf>,
    pub repository: bool,
    pub directory: Option<PathBuf>,
    pub label: Option<String>,
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
        },
        Scope {
            repository: start.join(".git").exists(),
            project: Some(start),
            directory: None,
            label: None,
        },
    ])
}

/// Physical skill directories read by one configured agent. A shared root is
/// offered only to agents documented to discover it. Explicit config survives.
pub fn locations(ws: &Workspace, configured: &AgentConfig, start: &Path) -> Result<Vec<Scope>> {
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
                            paths::expand_tilde(p)
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
            });
        }
    }
    Ok(out)
}

/// Keep destination IDs compatible with previously registered targets.
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
    let start = project
        .map(Path::to_path_buf)
        .unwrap_or(std::env::current_dir()?);
    let registry = decoded(&ws.root)?;
    let mut out = Vec::new();
    for definition in crate::agents::BUILTINS {
        let configured = ws
            .config
            .agent(definition.key)
            .cloned()
            .unwrap_or_else(|| definition.config(false));
        for scope in locations(ws, &configured, &start)?
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
        let matches_scope = if registry.agents.iter().any(|a| a.key == configured.key) {
            registry.projects.get(&configured.key).map(PathBuf::as_path) == project
        } else {
            ws.project.as_deref() == project
        };
        if matches_scope {
            out.push(configured.clone());
        }
    }
    Ok(out)
}

/// Register a target only when an operation actually writes to it.
pub fn register(ws: &Workspace, agent: &AgentConfig, project: Option<&Path>) -> Result<()> {
    let mut registry = decoded(&ws.root)?;
    if let Some(project) = project {
        paths::ensure_local_path(project, &agent.skills_path())?;
    }
    if !registry.agents.iter().any(|a| a.key == agent.key) {
        registry.agents.push(agent.clone());
        if let Some(project) = project {
            registry.projects.insert(agent.key.clone(), project.into());
        }
        save(ws, &registry)?;
    }
    Ok(())
}

/// Explicit picker destinations retain their manual deployment state during sync.
pub fn registered_keys(root: &Path) -> Result<std::collections::BTreeSet<String>> {
    Ok(load(root)?.agents.into_iter().map(|a| a.key).collect())
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

fn decoded(root: &Path) -> Result<Registry> {
    let mut registry = load(root)?;
    for project in registry.projects.values_mut() {
        *project = resolve(root, project);
    }
    for agent in &mut registry.agents {
        let stored = Path::new(&agent.skills_dir);
        let path = if let Some(project) = registry.projects.get(&agent.key) {
            // Legacy records used absolute targets. New records are project-relative.
            ensure!(
                !stored
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir)),
                "deployment target must stay inside its project"
            );
            let path = resolve(project, stored);
            ensure!(
                path.starts_with(project),
                "deployment target must stay inside its project"
            );
            if project.exists() {
                paths::ensure_local_path(project, &path)?;
            }
            path
        } else {
            resolve(root, stored)
        };
        agent.skills_dir = path.to_string_lossy().into_owned();
    }
    Ok(registry)
}

fn save(ws: &Workspace, registry: &Registry) -> Result<()> {
    let mut stored = registry.clone();
    for agent in &mut stored.agents {
        let path = agent.skills_path();
        agent.skills_dir = match registry.projects.get(&agent.key) {
            Some(project) => {
                paths::ensure_local_path(project, &path)?;
                path.strip_prefix(project)?.to_string_lossy().into_owned()
            }
            None => portable(&ws.root, &path, true)
                .to_string_lossy()
                .into_owned(),
        };
    }
    for project in stored.projects.values_mut() {
        *project = portable(&ws.root, project, ws.project.is_none());
    }
    crate::util::write_atomic(
        &registry_path(&ws.root),
        toml::to_string_pretty(&stored)?.as_bytes(),
    )
}

/// Resolve portable target paths at runtime. Reading never migrates files.
pub fn extend(root: &Path, agents: &mut Vec<AgentConfig>) -> Result<()> {
    for agent in decoded(root)?.agents {
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
    let registry = decoded(&ws.root)?;
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
        if let Some(existing) = ws.config.agents.iter().find(|a| {
            a.skills_path() == path
                && (a.key == agent.key
                    || a.key == id
                    || registry.agents.iter().any(|r| {
                        r.key == a.key
                            && r.key.starts_with(&format!(
                                "{}-{}-",
                                definition.key,
                                if local { "local" } else { "global" }
                            ))
                    }))
        }) {
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
    for (agent, on) in changes {
        if !on {
            let selection = selection(ws, agent)?;
            for key in keys {
                ensure!(
                    !selection
                        .presets
                        .values()
                        .any(|members| members.contains(key)),
                    "{key} is required by an installed preset"
                );
            }
        }
    }
    let mut messages = Vec::new();
    let mut intents = Vec::new();
    let mut applied = std::collections::BTreeSet::new();
    for (agent, on) in changes {
        if !applied.insert(agent.skills_path()) {
            continue;
        }
        let (message, intent) = set_installed(ws, agent, project, keys, None, *on)?;
        messages.push(message);
        if let Some(intent) = intent {
            intents.push(intent);
        }
    }
    let intent = match intents.len() {
        0 => None,
        1 => intents.pop(),
        _ => Some(crate::history::Intent::Group(intents)),
    };
    Ok((messages.join("; "), intent))
}

/// Installation reasons belong to a destination, independently of its links.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    #[serde(default)]
    pub manual: std::collections::BTreeSet<String>,
    #[serde(default)]
    pub presets: std::collections::BTreeMap<String, std::collections::BTreeSet<String>>,
}
impl Selection {
    pub fn skills(&self) -> std::collections::BTreeSet<String> {
        self.manual
            .iter()
            .chain(self.presets.values().flatten())
            .cloned()
            .collect()
    }
}

fn recorded_selection(ws: &Workspace, agent: &AgentConfig) -> Result<Option<Selection>> {
    let registry = decoded(&ws.root)?;
    if let Some(selection) = registry.selections.get(&agent.key) {
        return Ok(Some(selection.clone()));
    }
    for other in &registry.agents {
        if other.skills_path() == agent.skills_path()
            && let Some(selection) = registry.selections.get(&other.key)
        {
            return Ok(Some(selection.clone()));
        }
    }
    Ok(None)
}

pub fn selection(ws: &Workspace, agent: &AgentConfig) -> Result<Selection> {
    if let Some(selection) = recorded_selection(ws, agent)? {
        return Ok(selection);
    }
    let mut scoped = ws.clone();
    scoped.config.agents = vec![agent.clone()];
    Ok(inferred_selection(&scoped.scan()?, agent))
}

/// UI callers already have a matching snapshot; never scan again just to infer
/// installation reasons. Mutation callers continue to obtain a fresh snapshot.
pub fn selection_from_snapshot(
    ws: &Workspace,
    agent: &AgentConfig,
    snap: &crate::reconcile::Snapshot,
) -> Result<Selection> {
    Ok(recorded_selection(ws, agent)?.unwrap_or_else(|| inferred_selection(snap, agent)))
}

fn inferred_selection(snap: &crate::reconcile::Snapshot, agent: &AgentConfig) -> Selection {
    Selection {
        manual: snap
            .skills
            .iter()
            .filter(|s| s.deploy.get(&agent.key) == Some(&crate::reconcile::DeployState::Deployed))
            .map(|s| s.key.clone())
            .collect(),
        presets: Default::default(),
    }
}

pub fn desired(
    root: &Path,
) -> Result<std::collections::BTreeMap<String, std::collections::BTreeSet<String>>> {
    Ok(decoded(root)?
        .selections
        .into_iter()
        .map(|(key, s)| (key, s.skills()))
        .collect())
}

/// Write only this destination. Other scopes, manual installs, and overlapping
/// presets keep their independent installation reasons.
pub fn set_installed(
    ws: &Workspace,
    agent: &AgentConfig,
    project: Option<&Path>,
    keys: &[String],
    preset: Option<&str>,
    on: bool,
) -> Result<(String, Option<crate::history::Intent>)> {
    let snap = scan_target(ws, agent)?;
    let before = selection_from_snapshot(ws, agent, &snap)?;
    let mut after = before.clone();
    if let Some(preset) = preset {
        if on {
            after
                .presets
                .insert(preset.into(), keys.iter().cloned().collect());
        } else {
            after.presets.remove(preset);
        }
    } else if on {
        after.manual.extend(keys.iter().cloned());
    } else {
        for key in keys {
            ensure!(
                !after.presets.values().any(|members| members.contains(key)),
                "{key} is required by an installed preset; uninstall that preset first"
            );
            after.manual.remove(key);
        }
    }
    let message = restore_scanned_selection(ws, agent, project, &before, &after, &snap)?;
    let intent = (before != after).then(|| crate::history::Intent::TargetSelection {
        agent: agent.clone(),
        project: project.map(Path::to_path_buf),
        before,
        after,
    });
    Ok((message, intent))
}

pub fn restore_selection(
    ws: &Workspace,
    agent: &AgentConfig,
    project: Option<&Path>,
    expected: &Selection,
    desired: &Selection,
) -> Result<String> {
    let snap = scan_target(ws, agent)?;
    restore_scanned_selection(ws, agent, project, expected, desired, &snap)
}

fn scan_target(ws: &Workspace, agent: &AgentConfig) -> Result<crate::reconcile::Snapshot> {
    let mut scoped = ws.clone();
    scoped.config.agents = vec![agent.clone()];
    scoped.scan()
}

/// Share one fresh scan across inference, validation, and planning. Filesystem
/// writes still validate their destination immediately before changing it.
fn restore_scanned_selection(
    ws: &Workspace,
    agent: &AgentConfig,
    project: Option<&Path>,
    expected: &Selection,
    desired: &Selection,
    snap: &crate::reconcile::Snapshot,
) -> Result<String> {
    ensure!(
        &selection_from_snapshot(ws, agent, snap)? == expected,
        "deployment selection changed since this operation; refresh and retry"
    );
    let mut scoped = ws.clone();
    scoped.config.agents = vec![agent.clone()];
    let desired_keys = desired.skills();
    let before_keys = expected.skills();
    let removed = before_keys
        .difference(&desired_keys)
        .cloned()
        .collect::<Vec<_>>();
    let mut actions = super::deploy::plan_deploy(
        &scoped,
        snap,
        &desired_keys.iter().cloned().collect::<Vec<_>>(),
        std::slice::from_ref(&agent.key),
    )?;
    actions.extend(super::deploy::plan_undeploy(
        &scoped,
        snap,
        &removed,
        std::slice::from_ref(&agent.key),
    )?);
    let actions = super::deploy::resolve_names(snap, &actions, None)?;
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
    register(ws, agent, project)?;
    // Persist the prior selection before touching links, so a failed or partial
    // operation can be retried without turning its new links into manual installs.
    let mut registry = decoded(&ws.root)?;
    if save_shared_selection(&mut registry, agent, expected) {
        save(ws, &registry)?;
    }
    super::deploy::apply(&actions)?;
    let mut registry = decoded(&ws.root)?;
    if save_shared_selection(&mut registry, agent, desired) {
        save(ws, &registry)?;
    }
    Ok(format!(
        "{} · {}",
        agent.display_name(),
        super::deploy::summarize(&actions)
    ))
}

/// Keep persisted installation references aligned with central library edits.
pub fn rename_skill_reference(ws: &Workspace, old: &str, new: Option<&str>) -> Result<()> {
    let mut registry = decoded(&ws.root)?;
    let mut changed = false;
    for selection in registry.selections.values_mut() {
        for members in std::iter::once(&mut selection.manual).chain(selection.presets.values_mut())
        {
            if members.remove(old) {
                changed = true;
                if let Some(new) = new {
                    members.insert(new.into());
                }
            }
        }
    }
    if changed {
        save(ws, &registry)?;
    }
    Ok(())
}

pub fn rename_preset_reference(ws: &Workspace, old: &str, new: &str) -> Result<()> {
    let mut registry = decoded(&ws.root)?;
    let mut changed = false;
    for selection in registry.selections.values_mut() {
        if let Some(members) = selection.presets.remove(old) {
            selection.presets.insert(new.into(), members);
            changed = true;
        }
    }
    if changed {
        save(ws, &registry)?;
    }
    Ok(())
}

fn save_shared_selection(
    registry: &mut Registry,
    agent: &AgentConfig,
    selection: &Selection,
) -> bool {
    let mut changed = false;
    for reader in &registry.agents {
        if reader.skills_path() == agent.skills_path()
            && registry.selections.get(&reader.key) != Some(selection)
        {
            changed = true;
            registry
                .selections
                .insert(reader.key.clone(), selection.clone());
        }
    }
    changed
}
