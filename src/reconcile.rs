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

/// State of a skill relative to its metadata (§7.2).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum SkillStatus {
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
        matches!(self, SkillStatus::Managed { .. } | SkillStatus::Unmanaged)
    }
    /// Skill directory exists with a readable SKILL.md.
    pub fn is_present(&self) -> bool {
        matches!(
            self,
            SkillStatus::Managed { .. }
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
    pub current_hash: Option<String>,
    pub baseline_hash: Option<String>,
    /// agent key -> state
    pub deploy: BTreeMap<String, DeployState>,
    #[serde(skip)]
    pub meta: Option<SkillMeta>,
}

impl SkillRecord {
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
}

impl AgentReport {
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
    // Compare canonical symlink targets with a canonical root, including when
    // this public function is called directly instead of through Workspace.
    let root = crate::paths::resolve_root(Some(root))?;
    let root = root.as_path();
    let store = MetaStore::new(root);
    let mut records: BTreeMap<String, SkillRecord> = BTreeMap::new();

    // 1. Skill directories.
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !valid_skill_key(&name) {
            continue;
        }
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let external = is_symlink(&path);
        let (status, doc, current_hash) = match SkillDoc::load(&path) {
            Ok(doc) => {
                let h = hash_directory(&path).ok();
                (SkillStatus::Unmanaged, Some(doc), h)
            }
            Err(e) => (
                SkillStatus::Invalid {
                    reason: e.to_string(),
                },
                None,
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
                current_hash,
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
                if rec.status == SkillStatus::Unmanaged {
                    rec.status = match (&rec.current_hash, &rec.baseline_hash) {
                        (Some(cur), Some(base)) if cur == base => {
                            SkillStatus::Managed { no_baseline: false }
                        }
                        (Some(_), Some(_)) => SkillStatus::Modified,
                        _ => SkillStatus::Managed { no_baseline: true },
                    };
                }
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

    // 3. Rename detection: missing meta with baseline hash == unmanaged dir hash, unique both ways.
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
        agents.push(scan_agent(root, a)?);
    }
    for rec in records.values_mut() {
        for a in &agents {
            let state = deploy_state(a, &rec.key, rec.current_hash.as_deref(), root);
            rec.deploy.insert(a.key.clone(), state);
        }
    }

    Ok(Snapshot {
        root: root.to_path_buf(),
        skills: records.into_values().collect(),
        agents,
    })
}

fn scan_agent(root: &Path, a: &AgentConfig) -> Result<AgentReport> {
    let dir = a.skills_path();
    let mut report = AgentReport {
        key: a.key.clone(),
        name: a.display_name().to_string(),
        skills_dir: dir.clone(),
        mode: AgentDirMode::Missing,
        entries: BTreeMap::new(),
    };
    let meta = match std::fs::symlink_metadata(&dir) {
        Ok(m) => m,
        Err(_) => return Ok(report),
    };
    if meta.file_type().is_symlink() {
        let target = link_target_abs(&dir).unwrap_or_default();
        let resolved = std::fs::canonicalize(&dir).unwrap_or(target.clone());
        report.mode = if resolved == root {
            AgentDirMode::DirLinked
        } else {
            AgentDirMode::DirForeign { target }
        };
        return Ok(report);
    }
    if !meta.is_dir() {
        return Ok(report);
    }
    report.mode = AgentDirMode::Real;
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
                    if res.parent() == Some(root)
                        && res
                            .file_name()
                            .map(|n| n.to_string_lossy() == name)
                            .unwrap_or(false)
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
        } else if ft.is_dir() {
            let central = root.join(&name);
            if central.is_dir() {
                let same = match (hash_directory(&central), hash_directory(&p)) {
                    (Ok(a), Ok(b)) => a == b,
                    _ => false,
                };
                EntryState::Shadow { same_content: same }
            } else {
                EntryState::AgentOnly
            }
        } else {
            continue;
        };
        report.entries.insert(name, state);
    }
    Ok(report)
}

fn deploy_state(a: &AgentReport, key: &str, _hash: Option<&str>, _root: &Path) -> DeployState {
    match &a.mode {
        AgentDirMode::Missing => DeployState::NoAgentDir,
        AgentDirMode::DirLinked => DeployState::Deployed,
        AgentDirMode::DirForeign { .. } => DeployState::NotDeployed,
        AgentDirMode::Real => match a.entries.get(key) {
            None => DeployState::NotDeployed,
            Some(EntryState::Deployed) => DeployState::Deployed,
            Some(EntryState::Broken { .. }) => DeployState::Broken,
            Some(EntryState::Foreign { .. }) => DeployState::Foreign,
            Some(EntryState::Shadow { same_content }) => DeployState::Shadow {
                same_content: *same_content,
            },
            Some(EntryState::AgentOnly) => DeployState::NotDeployed,
        },
    }
}
