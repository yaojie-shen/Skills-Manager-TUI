//! Core library for `skills`: scanning a central skills directory, keeping
//! per-skill metadata as TOML files, and deploying skills to agents via
//! symlinks. Both the CLI and the TUI are thin layers over this crate.

pub mod config;
pub mod dict;
pub mod hash;
pub mod meta;
pub mod ops;
pub mod paths;
pub mod preset;
pub mod reconcile;
pub mod search;
pub mod skill;
pub mod util;

use anyhow::Result;
use std::path::{Path, PathBuf};

/// Everything an operation needs: the root, the loaded config, and stores.
#[derive(Debug, Clone)]
pub struct Workspace {
    pub root: PathBuf,
    pub config: config::Config,
    pub meta: meta::MetaStore,
    pub presets: preset::PresetStore,
}

impl Workspace {
    pub fn open(root: &Path) -> Result<Self> {
        let config = config::Config::load(root)?;
        Ok(Self {
            root: root.to_path_buf(),
            config,
            meta: meta::MetaStore::new(root),
            presets: preset::PresetStore::new(root),
        })
    }

    pub fn scan(&self) -> Result<reconcile::Snapshot> {
        reconcile::scan(&self.root, &self.config)
    }

    pub fn skill_path(&self, key: &str) -> PathBuf {
        self.root.join(key)
    }
}
