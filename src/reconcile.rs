//! Read-only reconciliation of the skills root, its metadata and every agent
//! directory. Scanning never writes anything.

use crate::config::{AgentConfig, Config};
use crate::hash::hash_directory;
use crate::meta::{MetaStore, SkillMeta};
use crate::skill::SkillDoc;
use crate::util::{is_symlink, link_target_abs, valid_skill_key};
use anyhow::Result;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

pub mod watch;

/// State of a skill relative to its metadata (§7.2).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum SkillStatus {
    Local,
    Managed { no_baseline: bool },
    Modified,
    Unmanaged,
    Missing,
    Renamed { to: String },
    Invalid { reason: String },
    CorruptMeta { error: String },
}

impl SkillStatus {
    pub fn label(&self) -> &'static str {
        match self {
            SkillStatus::Local => "local",
            SkillStatus::Managed { .. } => "managed",
            SkillStatus::Modified => "modified",
            SkillStatus::Unmanaged => "unmanaged",
            SkillStatus::Missing => "missing",
            SkillStatus::Renamed { .. } => "renamed?",
            SkillStatus::Invalid { .. } => "invalid",
            SkillStatus::CorruptMeta { .. } => "corrupt-meta",
        }
    }
    pub fn is_healthy(&self) -> bool {
        matches!(
            self,
            SkillStatus::Local | SkillStatus::Managed { .. } | SkillStatus::Unmanaged
        )
    }
    /// Skill directory exists with a readable SKILL.md.
    pub fn is_present(&self) -> bool {
        matches!(
            self,
            SkillStatus::Local
                | SkillStatus::Managed { .. }
                | SkillStatus::Modified
                | SkillStatus::Unmanaged
                | SkillStatus::CorruptMeta { .. }
        )
    }
}

/// How a skill shows up in one agent's directory.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DeployState {
    /// Symlink into the skills root (or the whole agent dir is linked).
    Deployed,
    NotDeployed,
    /// Real directory or copy in the agent dir with the same name.
    Shadow {
        same_content: bool,
    },
    /// Symlink to somewhere outside the root.
    Foreign,
    /// Symlink whose target no longer exists.
    Broken,
    /// Agent directory does not exist.
    NoAgentDir,
}

#[derive(Debug, Clone, Serialize)]
pub struct SkillRecord {
    pub key: String,
    pub path: PathBuf,
    pub status: SkillStatus,
    pub name: Option<String>,
    pub description: Option<String>,
    #[serde(skip)]
    pub body: Option<String>,
    pub external: bool,
    pub name_mismatch: bool,
    pub tags: Vec<String>,
    pub note: Option<String>,
    pub source: Option<crate::meta::Source>,
    /// Calculated only when needed for baseline, rename, or shadow comparison.
    pub current_hash: Option<String>,
    pub baseline_hash: Option<String>,
    /// agent key -> state
    pub deploy: BTreeMap<String, DeployState>,
    #[serde(skip)]
    pub meta: Option<SkillMeta>,
}

impl SkillRecord {
    pub fn deployment_name(&self) -> String {
        crate::repository::default_deploy_name(&self.key)
    }

    pub fn deployed_to(&self) -> Vec<&str> {
        self.deploy
            .iter()
            .filter(|(_, s)| **s == DeployState::Deployed)
            .map(|(k, _)| k.as_str())
            .collect()
    }
}

/// Shape of an agent's skills directory (§7.4).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentDirMode {
    /// Directory does not exist.
    Missing,
    /// The whole skills dir is a symlink to the skills root.
    DirLinked,
    /// The agent reads the root directly; its real directories must never be relinked.
    SharedRoot,
    /// The skills dir is a symlink to somewhere else.
    DirForeign { target: PathBuf },
    /// A real directory containing per-skill entries.
    Real,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum EntryState {
    Deployed,
    Broken { target: PathBuf },
    Foreign { target: PathBuf },
    Shadow { same_content: bool },
    AgentOnly,
}

impl EntryState {
    pub fn label(&self) -> &'static str {
        match self {
            EntryState::Deployed => "deployed",
            EntryState::Broken { .. } => "broken",
            EntryState::Foreign { .. } => "foreign",
            EntryState::Shadow { .. } => "shadow",
            EntryState::AgentOnly => "agent-only",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentReport {
    pub key: String,
    pub name: String,
    pub skills_dir: PathBuf,
    pub mode: AgentDirMode,
    /// entry name -> state (only for `Real`).
    pub entries: BTreeMap<String, EntryState>,
    /// Successfully parsed skills; invalid entries remain in `entries` for Health.
    #[serde(skip)]
    pub documents: BTreeMap<String, crate::skill::SkillDoc>,
}

impl AgentReport {
    pub fn valid_count(&self, pred: impl Fn(&EntryState) -> bool) -> usize {
        self.documents
            .keys()
            .filter(|key| self.entries.get(*key).is_some_and(&pred))
            .count()
    }
    pub fn count(&self, pred: impl Fn(&EntryState) -> bool) -> usize {
        self.entries.values().filter(|s| pred(s)).count()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    pub root: PathBuf,
    pub skills: Vec<SkillRecord>,
    pub agents: Vec<AgentReport>,
}

impl Snapshot {
    pub fn get(&self, key: &str) -> Option<&SkillRecord> {
        self.skills.iter().find(|s| s.key == key)
    }
    pub fn agent(&self, key: &str) -> Option<&AgentReport> {
        self.agents.iter().find(|a| a.key == key)
    }
    pub fn all_tags(&self) -> BTreeMap<String, usize> {
        let mut m = BTreeMap::new();
        for s in &self.skills {
            for t in &s.tags {
                *m.entry(t.clone()).or_insert(0) += 1;
            }
        }
        m
    }
}

/// Scan the root and every agent. Pure read.
pub fn scan(root: &Path, config: &Config) -> Result<Snapshot> {
    scan_inventory(root, config, &mut hash_directory, true, true)
}

/// Refresh deployment reports against an existing library snapshot. Scope browsing
/// does not need to reread every SKILL.md or hash every managed source. Shadow
/// copies still get fresh comparisons; hashes are shared only within this call.
pub fn rescope(snapshot: &Snapshot, destinations: &[AgentConfig]) -> Result<Snapshot> {
    let mut records: BTreeMap<_, _> = snapshot
        .skills
        .iter()
        .map(|r| (r.key.clone(), r.clone()))
        .collect();
    let mut hashes = HashMap::new();
    let mut agents = Vec::with_capacity(destinations.len());
    for agent in destinations {
        agents.push(scan_agent(
            &snapshot.root,
            agent,
            &records,
            &mut hashes,
            &mut hash_directory,
        )?);
    }
    for record in records.values_mut() {
        record.deploy.clear();
        for agent in &agents {
            record
                .deploy
                .insert(agent.key.clone(), deploy_state(agent, record));
        }
    }
    Ok(Snapshot {
        root: snapshot.root.clone(),
        skills: records.into_values().collect(),
        agents,
    })
}

fn parallel_hashes(paths: &[PathBuf]) -> HashMap<PathBuf, Option<String>> {
    if paths.is_empty() {
        return HashMap::new();
    }
    let workers = std::thread::available_parallelism()
        .map_or(1, usize::from)
        .min(4);
    std::thread::scope(|scope| {
        let jobs: Vec<_> = paths
            .chunks(paths.len().div_ceil(workers))
            .map(|chunk| {
                scope.spawn(move || {
                    chunk
                        .iter()
                        .map(|path| (path.clone(), hash_directory(path).ok()))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        jobs.into_iter()
            .flat_map(|job| job.join().expect("hash worker panicked"))
            .collect()
    })
}

// Cache only within this scan: a later scan must observe every content change.
fn cached_hash(
    path: &Path,
    hashes: &mut HashMap<PathBuf, Option<String>>,
    hash: &mut dyn FnMut(&Path) -> Result<String>,
) -> Option<String> {
    hashes
        .entry(path.to_path_buf())
        .or_insert_with(|| hash(path).ok())
        .clone()
}

/// Fresh inventory for link planning. Baseline/rename statuses are not verified:
/// use `scan` for display, health checks and content updates. Shadow comparisons
/// and actual destination inspection remain fresh, including before mutations.
pub fn scan_for_links(root: &Path, config: &Config) -> Result<Snapshot> {
    scan_inventory(root, config, &mut hash_directory, false, false)
}

#[cfg(test)]
fn scan_with_hash(
    root: &Path,
    config: &Config,
    hash: &mut dyn FnMut(&Path) -> Result<String>,
) -> Result<Snapshot> {
    scan_inventory(root, config, hash, true, false)
}

fn scan_inventory(
    root: &Path,
    config: &Config,
    hash: &mut dyn FnMut(&Path) -> Result<String>,
    verify_content: bool,
    parallel: bool,
) -> Result<Snapshot> {
    let mut hashes = HashMap::new();
    // Compare canonical symlink targets with a canonical root, including when
    // this public function is called directly instead of through Workspace.
    let root = crate::paths::resolve_root(Some(root))?;
    let root = root.as_path();
    let store = MetaStore::new(root);
    let mut records: BTreeMap<String, SkillRecord> = BTreeMap::new();

    // Preserve flat local skills; repository skill identities are relative paths.
    let mut discovered = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !valid_skill_key(&name) || !entry.path().is_dir() {
            continue;
        }
        if name == "local" && !entry.path().join("SKILL.md").exists() && !is_symlink(&entry.path())
        {
            for skill in std::fs::read_dir(entry.path())? {
                let skill = skill?;
                let leaf = skill.file_name().to_string_lossy().into_owned();
                if skill.file_type()?.is_dir() && valid_skill_key(&leaf) {
                    if skill.path().join("SKILL.md").is_file() {
                        discovered.push((format!("local/{leaf}"), skill.path()));
                    } else {
                        for member in std::fs::read_dir(skill.path())? {
                            let member = member?;
                            let name = member.file_name().to_string_lossy().into_owned();
                            if member.file_type()?.is_dir() && valid_skill_key(&name) {
                                discovered.push((format!("local/{leaf}/{name}"), member.path()));
                            }
                        }
                    }
                }
            }
            continue;
        }
        if name == "repos" && !entry.path().join("SKILL.md").exists() && !is_symlink(&entry.path())
        {
            for repo in std::fs::read_dir(entry.path())? {
                let repo = repo?;
                if !repo.file_type()?.is_dir()
                    || !valid_skill_key(&repo.file_name().to_string_lossy())
                {
                    continue;
                }
                for skill in std::fs::read_dir(repo.path())? {
                    let skill = skill?;
                    if skill.path().is_dir()
                        && valid_skill_key(&skill.file_name().to_string_lossy())
                    {
                        discovered.push((
                            skill
                                .path()
                                .strip_prefix(root)?
                                .to_string_lossy()
                                .to_string(),
                            skill.path(),
                        ));
                    }
                }
            }
        } else {
            discovered.push((name, entry.path()));
        }
    }
    // Per-skill aliases into repository storage are deployments, not second skills.
    let aliases: std::collections::BTreeSet<PathBuf> = discovered
        .iter()
        .filter(|(key, _)| key.contains('/'))
        .filter_map(|(key, path)| {
            let alias = root.join(crate::repository::default_deploy_name(key));
            (is_symlink(&alias)
                && std::fs::canonicalize(&alias).ok() == std::fs::canonicalize(path).ok())
            .then_some(alias)
        })
        .collect();
    for (name, path) in discovered {
        if aliases.contains(&path) {
            continue;
        }
        let external = is_symlink(&path);
        let (status, doc) = match SkillDoc::load(&path) {
            Ok(doc) => (SkillStatus::Unmanaged, Some(doc)),
            Err(e) => (
                SkillStatus::Invalid {
                    reason: e.to_string(),
                },
                None,
            ),
        };
        records.insert(
            name.clone(),
            SkillRecord {
                key: name,
                path,
                status,
                name: doc.as_ref().map(|d| d.name.clone()),
                description: doc
                    .as_ref()
                    .map(|d| d.description.trim().to_string())
                    .filter(|d| !d.is_empty()),
                body: doc.as_ref().map(|d| d.body.clone()),
                external,
                name_mismatch: doc.as_ref().map(|d| d.name_mismatch()).unwrap_or(false),
                tags: Vec::new(),
                note: None,
                source: None,
                current_hash: None,
                baseline_hash: None,
                deploy: BTreeMap::new(),
                meta: None,
            },
        );
    }

    // 2. Metadata files.
    for key in store.list_keys()? {
        match store.load(&key) {
            Ok(Some(meta)) => {
                let rec = records.entry(key.clone()).or_insert_with(|| SkillRecord {
                    key: key.clone(),
                    path: root.join(&key),
                    status: SkillStatus::Missing,
                    name: None,
                    description: None,
                    body: None,
                    external: false,
                    name_mismatch: false,
                    tags: Vec::new(),
                    note: None,
                    source: None,
                    current_hash: None,
                    baseline_hash: None,
                    deploy: BTreeMap::new(),
                    meta: None,
                });
                rec.tags = meta.tags.clone();
                rec.note = meta.note.clone();
                rec.source = meta.source.clone();
                rec.baseline_hash = meta.baseline.as_ref().map(|b| b.hash.clone());
                rec.meta = Some(meta);
            }
            Ok(None) => {}
            Err(e) => {
                let rec = records.entry(key.clone()).or_insert_with(|| SkillRecord {
                    key: key.clone(),
                    path: root.join(&key),
                    status: SkillStatus::Missing,
                    name: None,
                    description: None,
                    body: None,
                    external: false,
                    name_mismatch: false,
                    tags: Vec::new(),
                    note: None,
                    source: None,
                    current_hash: None,
                    baseline_hash: None,
                    deploy: BTreeMap::new(),
                    meta: None,
                });
                rec.status = SkillStatus::CorruptMeta {
                    error: format!("{e:#}"),
                };
            }
        }
    }

    for rec in records.values_mut() {
        if rec.status == SkillStatus::Unmanaged
            && crate::repository::alias_of(&rec.key).is_none()
            && !matches!(rec.source, Some(crate::meta::Source::Git { .. }))
        {
            rec.status = SkillStatus::Local;
        }
    }

    // Collect baseline work once, then share completed hashes with agent/shadow
    // comparisons. Limit concurrency to avoid an unbounded disk/FD fan-out.
    if verify_content && parallel {
        let paths: Vec<_> = records
            .values()
            .filter(|r| {
                r.status == SkillStatus::Unmanaged && r.meta.is_some() && r.baseline_hash.is_some()
            })
            .map(|r| r.path.clone())
            .collect();
        hashes.extend(parallel_hashes(&paths));
    }
    for rec in records
        .values_mut()
        .filter(|r| r.status == SkillStatus::Unmanaged && r.meta.is_some())
    {
        if verify_content && rec.baseline_hash.is_some() {
            rec.current_hash = cached_hash(&rec.path, &mut hashes, hash);
        }
        rec.status = match (&rec.current_hash, &rec.baseline_hash) {
            (Some(cur), Some(base)) if cur == base => SkillStatus::Managed { no_baseline: false },
            (Some(_), Some(_)) => SkillStatus::Modified,
            _ => SkillStatus::Managed {
                no_baseline: verify_content || rec.baseline_hash.is_none(),
            },
        };
    }

    // 3. Only hash unmanaged directories when a missing baseline needs rename candidates.
    // The candidates still have to be unique in both directions.
    if verify_content
        && records
            .values()
            .any(|r| r.status == SkillStatus::Missing && r.baseline_hash.is_some())
    {
        for rec in records
            .values_mut()
            .filter(|r| r.status == SkillStatus::Unmanaged)
        {
            rec.current_hash = cached_hash(&rec.path, &mut hashes, hash);
        }
    }
    let mut by_hash_unmanaged: HashMap<String, Vec<String>> = HashMap::new();
    for r in records.values() {
        if r.status == SkillStatus::Unmanaged
            && let Some(h) = &r.current_hash
        {
            by_hash_unmanaged
                .entry(h.clone())
                .or_default()
                .push(r.key.clone());
        }
    }
    let mut by_hash_missing: HashMap<String, Vec<String>> = HashMap::new();
    for r in records.values() {
        if r.status == SkillStatus::Missing
            && let Some(h) = &r.baseline_hash
        {
            by_hash_missing
                .entry(h.clone())
                .or_default()
                .push(r.key.clone());
        }
    }
    let mut renames: Vec<(String, String)> = Vec::new();
    for (h, missing) in &by_hash_missing {
        if let Some(cands) = by_hash_unmanaged.get(h)
            && missing.len() == 1
            && cands.len() == 1
        {
            renames.push((missing[0].clone(), cands[0].clone()));
        }
    }
    for (old, new) in renames {
        if let Some(r) = records.get_mut(&old) {
            r.status = SkillStatus::Renamed { to: new };
        }
    }

    // 4. Agents.
    let mut agents = Vec::new();
    for a in &config.agents {
        agents.push(scan_agent(root, a, &records, &mut hashes, hash)?);
    }
    for rec in records.values_mut() {
        if rec.current_hash.is_none() {
            rec.current_hash = hashes.get(&rec.path).cloned().flatten();
        }
        for a in &agents {
            let state = deploy_state(a, rec);
            rec.deploy.insert(a.key.clone(), state);
        }
    }

    Ok(Snapshot {
        root: root.to_path_buf(),
        skills: records.into_values().collect(),
        agents,
    })
}

fn scan_agent(
    root: &Path,
    a: &AgentConfig,
    records: &BTreeMap<String, SkillRecord>,
    hashes: &mut HashMap<PathBuf, Option<String>>,
    hash: &mut dyn FnMut(&Path) -> Result<String>,
) -> Result<AgentReport> {
    let dir = a.skills_path();
    let mut report = AgentReport {
        key: a.key.clone(),
        name: a.display_name().to_string(),
        skills_dir: dir.clone(),
        mode: AgentDirMode::Missing,
        entries: BTreeMap::new(),
        documents: BTreeMap::new(),
    };
    let meta = match std::fs::symlink_metadata(&dir) {
        Ok(m) => m,
        Err(_) => return Ok(report),
    };
    let shared_link = crate::agents::linked_skill_directory(&dir);
    if meta.file_type().is_symlink() && shared_link.is_none() {
        let target = link_target_abs(&dir).unwrap_or_default();
        let resolved = std::fs::canonicalize(&dir).unwrap_or(target.clone());
        report.mode = if resolved == root {
            AgentDirMode::DirLinked
        } else {
            AgentDirMode::DirForeign { target }
        };
        if report.mode == AgentDirMode::DirLinked {
            for (key, record) in records.iter().filter(|(key, _)| !key.contains('/')) {
                if let Ok(doc) = crate::skill::SkillDoc::load(&record.path) {
                    report.documents.insert(key.clone(), doc);
                }
            }
        }
        return Ok(report);
    }
    if !meta.is_dir() && shared_link.is_none() {
        return Ok(report);
    }
    report.mode = if std::fs::canonicalize(&dir).ok().as_deref() == Some(root) {
        AgentDirMode::SharedRoot
    } else {
        AgentDirMode::Real
    };
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let p = entry.path();
        let ft = entry.file_type()?;
        let state = if ft.is_symlink() {
            let target = link_target_abs(&p).unwrap_or_default();
            let resolved = std::fs::canonicalize(&p).ok();
            match resolved {
                None => EntryState::Broken { target },
                Some(res) => {
                    if records.values().any(|r| {
                        r.key.contains('/')
                            && r.deployment_name() == name
                            && std::fs::canonicalize(&r.path).ok().as_ref() == Some(&res)
                    }) || (res.parent() == Some(root)
                        && res
                            .file_name()
                            .map(|n| n.to_string_lossy() == name)
                            .unwrap_or(false))
                    {
                        EntryState::Deployed
                    } else if res.starts_with(root) {
                        // Link into the root but under a different name: treat as deployed alias.
                        EntryState::Foreign { target }
                    } else {
                        EntryState::Foreign { target }
                    }
                }
            }
        } else if ft.is_dir()
            && report.mode == AgentDirMode::SharedRoot
            && records.contains_key(&name)
        {
            EntryState::Deployed
        } else if ft.is_dir() {
            let central = records
                .values()
                .find(|r| r.deployment_name() == name)
                .map(|r| r.path.clone())
                .unwrap_or_else(|| root.join(&name));
            if central.is_dir() {
                let same = match (
                    cached_hash(&central, hashes, hash),
                    cached_hash(&p, hashes, hash),
                ) {
                    (Some(a), Some(b)) => a == b,
                    _ => false,
                };
                EntryState::Shadow { same_content: same }
            } else {
                EntryState::AgentOnly
            }
        } else {
            continue;
        };
        if let Ok(doc) = crate::skill::SkillDoc::load(&p) {
            report.documents.insert(name.clone(), doc);
        }
        report.entries.insert(name, state);
    }
    Ok(report)
}

fn deploy_state(a: &AgentReport, record: &SkillRecord) -> DeployState {
    let key = &record.key;
    match &a.mode {
        AgentDirMode::Missing => DeployState::NoAgentDir,
        AgentDirMode::DirLinked if key.contains('/') || record.name.is_none() => {
            DeployState::NotDeployed
        }
        AgentDirMode::DirLinked => DeployState::Deployed,
        AgentDirMode::DirForeign { .. } => DeployState::NotDeployed,
        AgentDirMode::Real | AgentDirMode::SharedRoot => {
            match a.entries.get(&crate::repository::default_deploy_name(key)) {
                None => DeployState::NotDeployed,
                Some(EntryState::Deployed) => {
                    let destination = a.skills_dir.join(record.deployment_name());
                    if std::fs::canonicalize(destination).ok()
                        == std::fs::canonicalize(&record.path).ok()
                    {
                        DeployState::Deployed
                    } else {
                        DeployState::NotDeployed
                    }
                }

                Some(EntryState::Broken { .. }) => DeployState::Broken,
                Some(EntryState::Foreign { .. }) => DeployState::Foreign,
                Some(EntryState::Shadow { same_content }) => DeployState::Shadow {
                    same_content: *same_content,
                },
                Some(EntryState::AgentOnly) => DeployState::NotDeployed,
            }
        }
    }
}

#[cfg(test)]
mod scan_cost_tests {
    use super::*;
    use crate::meta::{Baseline, SkillMeta};

    fn skill(root: &Path, key: &str) -> PathBuf {
        let path = root.join(key);
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(
            path.join("SKILL.md"),
            "---\nname: example\ndescription: test\n---\nBody\n",
        )
        .unwrap();
        path
    }

    #[test]
    fn link_planning_skips_unrelated_baselines_but_compares_shadow_contents() {
        let tmp = crate::ops::DownloadDir::new("link-scan-cost").unwrap();
        let root = tmp.path().join("root");
        let path = skill(&root, "one");
        let meta = SkillMeta {
            baseline: Some(Baseline {
                hash: hash_directory(&path).unwrap(),
                hash_algo: crate::hash::HASH_ALGO,
            }),
            ..Default::default()
        };
        MetaStore::new(&root).save("one", &meta).unwrap();
        std::fs::write(path.join("script.py"), "changed").unwrap();
        let mut config = Config {
            agents: vec![],
            ..Default::default()
        };
        let snap = scan_inventory(
            &root,
            &config,
            &mut |_| panic!("link planning must not hash unrelated baselines"),
            false,
            false,
        )
        .unwrap();
        assert!(snap.get("one").unwrap().status.is_present());
        assert!(snap.get("one").unwrap().current_hash.is_none());
        let agent = tmp.path().join("agent");
        skill(&agent, "one");
        config.agents.push(AgentConfig {
            key: "a".into(),
            name: "A".into(),
            skills_dir: agent.display().to_string(),
        });
        assert_eq!(
            scan_for_links(&root, &config)
                .unwrap()
                .get("one")
                .unwrap()
                .deploy["a"],
            DeployState::Shadow {
                same_content: false
            }
        );
        std::fs::write(agent.join("one/script.py"), "changed").unwrap();
        assert_eq!(
            scan_for_links(&root, &config)
                .unwrap()
                .get("one")
                .unwrap()
                .deploy["a"],
            DeployState::Shadow { same_content: true }
        );
        assert_eq!(
            scan(&root, &config).unwrap().get("one").unwrap().status,
            SkillStatus::Local
        );
    }

    #[test]
    fn parallel_scan_matches_serial_health_and_observes_later_content_changes() {
        let tmp = crate::ops::DownloadDir::new("parallel-scan").unwrap();
        let root = tmp.path();
        for i in 0..12 {
            let key = format!("skill-{i}");
            let path = skill(root, &key);
            std::fs::write(path.join("unique"), key.as_bytes()).unwrap();
            MetaStore::new(root)
                .save(
                    &key,
                    &SkillMeta {
                        baseline: Some(Baseline {
                            hash: hash_directory(&path).unwrap(),
                            hash_algo: crate::hash::HASH_ALGO,
                        }),
                        ..Default::default()
                    },
                )
                .unwrap();
        }
        std::fs::rename(root.join("skill-0"), root.join("moved")).unwrap();
        std::fs::remove_dir_all(root.join("skill-1")).unwrap();
        let config = Config {
            agents: vec![],
            ..Default::default()
        };
        for body in ["edit one", "edit two, different bytes"] {
            std::fs::write(root.join("skill-2/unique"), body).unwrap();
            let parallel = scan(root, &config).unwrap();
            let serial = scan_with_hash(root, &config, &mut hash_directory).unwrap();
            assert_eq!(
                serde_json::to_value(&parallel).unwrap(),
                serde_json::to_value(&serial).unwrap()
            );
            assert_eq!(parallel.get("skill-2").unwrap().status, SkillStatus::Local);
        }
    }

    #[test]
    fn browsing_without_comparisons_never_hashes_skill_trees() {
        let tmp = crate::ops::DownloadDir::new("scan-no-hash").unwrap();
        let root = tmp.path();
        skill(root, "repos/demo/one");
        skill(root, "two");
        MetaStore::new(root)
            .save("two", &SkillMeta::default())
            .unwrap();
        let config = Config {
            agents: vec![],
            ..Default::default()
        };
        let snap = scan_with_hash(root, &config, &mut |path| {
            panic!("browsing must not traverse {}", path.display())
        })
        .unwrap();
        assert_eq!(
            snap.get("repos/demo/one").unwrap().status,
            SkillStatus::Unmanaged
        );
        assert_eq!(snap.get("two").unwrap().status, SkillStatus::Local);
        assert!(snap.skills.iter().all(|r| r.current_hash.is_none()));
    }

    #[test]
    fn baseline_changes_and_rename_ambiguity_are_still_detected() {
        let tmp = crate::ops::DownloadDir::new("scan-required-hash").unwrap();
        let root = tmp.path();
        let path = skill(root, "repos/demo/one");
        let meta = SkillMeta {
            source: Some(crate::meta::Source::Git {
                url: "https://example.com/demo".into(),
                branch: None,
                subpath: Some("one".into()),
                revision: None,
            }),
            baseline: Some(Baseline {
                hash: hash_directory(&path).unwrap(),
                hash_algo: crate::hash::HASH_ALGO,
            }),
            ..Default::default()
        };
        MetaStore::new(root).save("repos/demo/one", &meta).unwrap();
        skill(root, "repos/demo/unrelated");
        let config = Config {
            agents: vec![],
            ..Default::default()
        };
        let mut reads = vec![];
        let snap = scan_with_hash(root, &config, &mut |path| {
            reads.push(path.to_path_buf());
            hash_directory(path)
        })
        .unwrap();
        assert_eq!(reads, vec![path.canonicalize().unwrap()]);
        assert_eq!(
            snap.get("repos/demo/one").unwrap().status,
            SkillStatus::Managed { no_baseline: false }
        );
        std::fs::write(path.join("script.py"), "print('changed')").unwrap();
        assert_eq!(
            scan(root, &config)
                .unwrap()
                .get("repos/demo/one")
                .unwrap()
                .status,
            SkillStatus::Modified
        );
        std::fs::remove_file(path.join("script.py")).unwrap();
        std::fs::rename(&path, root.join("repos/demo/renamed")).unwrap();
        // Both unmanaged directories have the same content, so no unique rename.
        assert_eq!(
            scan(root, &config)
                .unwrap()
                .get("repos/demo/one")
                .unwrap()
                .status,
            SkillStatus::Missing
        );
        std::fs::write(root.join("repos/demo/unrelated/extra.txt"), "different").unwrap();
        assert_eq!(
            scan(root, &config)
                .unwrap()
                .get("repos/demo/one")
                .unwrap()
                .status,
            SkillStatus::Renamed {
                to: "repos/demo/renamed".into()
            }
        );
    }

    #[test]
    fn shadow_comparisons_share_hashes_only_within_one_scan() {
        let tmp = crate::ops::DownloadDir::new("scan-shadows").unwrap();
        let root = tmp.path().join("root");
        let central = skill(&root, "one");
        let a = tmp.path().join("agent-a");
        let b = tmp.path().join("agent-b");
        skill(&a, "one");
        skill(&b, "one");
        let config = Config {
            agents: vec![
                AgentConfig {
                    key: "a".into(),
                    name: "A".into(),
                    skills_dir: a.display().to_string(),
                },
                AgentConfig {
                    key: "b".into(),
                    name: "B".into(),
                    skills_dir: b.display().to_string(),
                },
            ],
            ..Default::default()
        };
        let mut reads = vec![];
        let snap = scan_with_hash(&root, &config, &mut |path| {
            reads.push(path.to_path_buf());
            hash_directory(path)
        })
        .unwrap();
        assert_eq!(
            reads
                .iter()
                .filter(|p| **p == central.canonicalize().unwrap())
                .count(),
            1
        );
        assert_eq!(reads.len(), 3);
        assert_eq!(
            snap.get("one").unwrap().deploy["b"],
            DeployState::Shadow { same_content: true }
        );
        std::fs::write(b.join("one/changed.txt"), "changed").unwrap();
        assert_eq!(
            scan(&root, &config).unwrap().get("one").unwrap().deploy["b"],
            DeployState::Shadow {
                same_content: false
            }
        );
    }
}
