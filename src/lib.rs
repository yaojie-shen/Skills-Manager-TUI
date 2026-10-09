//! Core library for `skills`: scanning a central skills directory, keeping
//! per-skill metadata as TOML files, and deploying skills to agents via
//! symlinks. Both the CLI and the TUI are thin layers over this crate.

pub mod agents;
pub mod config;
pub mod dict;
mod file_set;
pub(crate) mod group_filename;
pub mod hash;
pub mod history;
pub mod meta;
pub mod migration;
pub mod ops;
pub mod paths;
pub mod preset;
pub mod reconcile;
pub mod repository;
pub(crate) mod schema;
pub mod search;
pub mod skill;
pub mod tag;
pub mod util;

use anyhow::Result;
use std::path::{Path, PathBuf};

/// Everything an operation needs: the root, the loaded config, and stores.
#[derive(Debug, Clone)]
pub struct Workspace {
    pub root: PathBuf,
    pub project: Option<PathBuf>,
    /// Project associated with the transient directory inventory.
    pub inventory_project: Option<PathBuf>,
    pub inventory_products: Option<std::collections::BTreeSet<String>>,
    pub config: config::Config,
    pub meta: meta::MetaStore,
    pub tags: tag::TagStore,
    pub presets: preset::PresetStore,
    pub migration: Option<migration::MigrationReport>,
}

impl Workspace {
    pub fn open(root: &Path) -> Result<Self> {
        let root = paths::resolve_root(Some(root))?;
        let migration = migration::ensure_current(&root)?;
        Self::load_current(root, None, migration)
    }

    /// Open an isolated project store; never consult the global root pointer.
    pub fn open_local(project: &Path, create: bool) -> Result<Self> {
        use anyhow::Context;
        let project = std::fs::canonicalize(project).context("project directory does not exist")?;
        anyhow::ensure!(project.is_dir(), "project is not a directory");
        let root = project.join(".agents/skills");
        paths::ensure_local_path(&project, &root)?;
        if create {
            std::fs::create_dir_all(&root)?;
        }
        let root = paths::resolve_root(Some(&root))?;
        let migration = migration::ensure_current(&root)?;
        Self::load_current(root, Some(project), migration)
    }

    fn load_current(
        root: PathBuf,
        project: Option<PathBuf>,
        migration: Option<migration::MigrationReport>,
    ) -> Result<Self> {
        let mut ws = Self {
            meta: meta::MetaStore::new(&root),
            tags: tag::TagStore::new(&root),
            presets: preset::PresetStore::new(&root),
            migration,
            root,
            project,
            inventory_project: None,
            inventory_products: None,
            config: config::Config::default(),
        };
        ws.config = ws.load_config()?;
        Ok(ws)
    }

    pub fn load_config(&self) -> Result<config::Config> {
        if let Some(layout) = migration::version::read(&self.root)? {
            anyhow::ensure!(
                layout <= migration::CURRENT_LAYOUT,
                "Skill Home layout {layout} requires a newer version of Skills Manager"
            );
        }
        let Some(project) = &self.project else {
            return config::Config::load(&self.root);
        };
        let exists = config::Config::exists(&self.root);
        let mut config = if exists {
            config::Config::load(&self.root)?
        } else {
            let mut config = config::Config::local_default();
            config.tags = self.tags.list()?;
            config
        };
        // An omitted agents table also means local defaults.
        if exists {
            let text = std::fs::read_to_string(config::Config::path(&self.root))?;
            let doc: toml::Value = toml::from_str(&text)?;
            if doc.get("agents").is_none() {
                config.agents = agents::defaults(true);
            }
        }
        for agent in &mut config.agents {
            let path = project.join(agent.skills_path());
            paths::ensure_local_path(project, &path)?;
            agent.skills_dir = path.to_string_lossy().into_owned();
        }
        Ok(config)
    }

    /// Read the current filesystem using this workspace's configuration
    /// snapshot. Callers explicitly load and validate configuration before
    /// replacing `config`; scanning never discards in-memory scope overrides.
    pub fn scan(&self) -> Result<reconcile::Snapshot> {
        reconcile::scan(&self.root, &self.config)
    }

    /// Fresh source/destination inventory for link planning, without unrelated
    /// baseline verification. Never use this snapshot to display content health.
    /// As with `scan`, configuration comes from `self.config`, not another file
    /// read. Write entry points remain responsible for fresh-state validation.
    pub fn scan_for_links(&self) -> Result<reconcile::Snapshot> {
        reconcile::scan_for_links(&self.root, &self.config)
    }

    pub fn skill_path(&self, key: &str) -> PathBuf {
        self.root.join(key)
    }
}
