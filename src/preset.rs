//! Presets: named groups of skills, `<root>/.skills-meta/presets/<name>.toml`.

use crate::paths::meta_dir;
use crate::util::write_atomic;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const PRESET_DIR: &str = "presets";

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Preset {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub skills: Vec<String>,
    /// Agents this preset targets; empty means every configured agent.
    #[serde(default)]
    pub agents: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct PresetStore {
    pub dir: PathBuf,
}

impl PresetStore {
    pub fn new(root: &Path) -> Self {
        Self {
            dir: meta_dir(root).join(PRESET_DIR),
        }
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}.toml"))
    }

    pub fn load(&self, name: &str) -> Result<Option<Preset>> {
        let path = self.path(name);
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let p: Preset = toml::from_str(&text)
                    .with_context(|| format!("invalid preset: {}", path.display()))?;
                Ok(Some(p))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn list(&self) -> Result<Vec<Preset>> {
        let mut out = Vec::new();
        let rd = match std::fs::read_dir(&self.dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e.into()),
        };
        let mut names: Vec<String> = rd
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let n = e.file_name().to_string_lossy().into_owned();
                n.strip_suffix(".toml").map(|s| s.to_string())
            })
            .collect();
        names.sort();
        for n in names {
            if let Some(p) = self.load(&n)? {
                out.push(p);
            }
        }
        Ok(out)
    }

    pub fn save(&self, preset: &Preset) -> Result<()> {
        if !crate::util::valid_skill_key(&preset.name) {
            bail!("invalid preset name: {}", preset.name);
        }
        let text = toml::to_string_pretty(preset)?;
        write_atomic(&self.path(&preset.name), text.as_bytes())
    }

    pub fn remove(&self, name: &str) -> Result<()> {
        let path = self.path(name);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => bail!("no such preset: {name}"),
            Err(e) => Err(e).with_context(|| format!("removing {}", path.display())),
        }
    }
}
