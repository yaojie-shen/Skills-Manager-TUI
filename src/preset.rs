//! Skill packages with fixed members, stored in
//! `<root>/.skills-meta/presets/<name>.toml`.

use crate::config::Config;
use crate::paths::meta_dir;
use crate::util::write_atomic;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use toml_edit::DocumentMut;

pub const PRESET_DIR: &str = "presets";

mod migration;
pub use migration::MigrationReport;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Preset {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default)]
    pub skills: Vec<String>,
    /// Agents this preset targets; empty means every configured agent.
    #[serde(default)]
    pub agents: Vec<String>,
}

impl Preset {
    pub fn members(&self) -> Vec<String> {
        self.skills
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct PresetIndex {
    pub by_name: BTreeMap<String, Preset>,
    pub by_skill: BTreeMap<String, Vec<String>>,
}

impl PresetIndex {
    pub fn new(presets: Vec<Preset>) -> Self {
        let by_name: BTreeMap<_, _> = presets
            .into_iter()
            .map(|mut preset| {
                preset.skills = preset.members();
                (preset.name.clone(), preset)
            })
            .collect();
        let mut by_skill: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for preset in by_name.values() {
            for key in &preset.skills {
                by_skill
                    .entry(key.clone())
                    .or_default()
                    .push(preset.name.clone());
            }
        }
        Self { by_name, by_skill }
    }

    pub fn get(&self, name: &str) -> Option<&Preset> {
        self.by_name.get(name)
    }

    pub fn for_skill(&self, key: &str) -> Vec<&Preset> {
        self.by_skill
            .get(key)
            .into_iter()
            .flatten()
            .filter_map(|name| self.get(name))
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagCoverage {
    pub name: String,
    pub included: usize,
    pub total: usize,
}

/// Expand tags once for an explicit membership operation, independently of UI visibility.
pub fn tag_members(config: &Config, names: &[String]) -> Result<Vec<String>> {
    let mut members = BTreeSet::new();
    for name in names {
        let mut found = false;
        for tag in config.tags.iter().filter(|tag| &tag.name == name) {
            found = true;
            members.extend(tag.skills.iter().cloned());
        }
        anyhow::ensure!(found, "no such tag: {name}");
    }
    Ok(members.into_iter().collect())
}

pub fn tag_coverages(config: &Config, skills: &BTreeSet<String>) -> Vec<TagCoverage> {
    let mut groups: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for tag in &config.tags {
        groups
            .entry(tag.name.clone())
            .or_default()
            .extend(tag.skills.iter().cloned());
    }
    groups
        .into_iter()
        .map(|(name, members)| TagCoverage {
            name,
            included: members.intersection(skills).count(),
            total: members.len(),
        })
        .collect()
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

    /// Check the stored spelling, even on a case-insensitive filesystem.
    pub fn contains_name(&self, name: &str) -> Result<bool> {
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e.into()),
        };
        let filename = format!("{name}.toml");
        for entry in entries {
            if entry?.file_name() == filename.as_str() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub fn load(&self, name: &str) -> Result<Option<Preset>> {
        let path = self.path(name);
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let value: toml::Value = toml::from_str(&text)?;
                anyhow::ensure!(
                    value.get("tags").is_none(),
                    "legacy Tag references in {}; reopen the workspace to migrate this preset",
                    path.display()
                );
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
        let mut preset = preset.clone();
        preset.skills = preset.members();
        let text = toml::to_string_pretty(&preset)?;
        write_atomic(&self.path(&preset.name), text.as_bytes())
    }

    /// Remove a fixed skill key without rewriting unrelated fields or comments.
    pub fn remove_skill(&self, name: &str, key: &str) -> Result<bool> {
        let path = self.path(name);
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let mut doc = text
            .parse::<DocumentMut>()
            .with_context(|| format!("invalid preset: {}", path.display()))?;
        let Some(skills) = doc.get_mut("skills").and_then(|item| item.as_array_mut()) else {
            return Ok(false);
        };
        let old_len = skills.len();
        skills.retain(|value| value.as_str() != Some(key));
        if skills.len() == old_len {
            return Ok(false);
        }
        write_atomic(&path, doc.to_string().as_bytes())?;
        Ok(true)
    }

    pub fn remove(&self, name: &str) -> Result<()> {
        let path = self.path(name);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => bail!("no such preset: {name}"),
            Err(e) => Err(e).with_context(|| format!("removing {}", path.display())),
        }
    }

    /// Move a preset to a new name. The name is inside the file as well as on
    /// it, so this is a save under the new name followed by removing the old
    /// file rather than a rename of the file itself. That order also means a
    /// crash between the two leaves both copies, which is easy to see and fix,
    /// where the other order could leave none. Only the preset file moves;
    /// deployment state is not stored in the preset.
    pub fn rename(&self, old: &str, new: &str) -> Result<Preset> {
        let mut p = self
            .load(old)?
            .with_context(|| format!("no such preset: {old}"))?;
        let aliases_old = old != new
            && old.eq_ignore_ascii_case(new)
            && self.path(new).exists()
            && !self.contains_name(new)?;
        if self.path(new).exists() && !aliases_old {
            bail!("preset {new} already exists");
        }
        p.name = new.to_string();
        // `save` refuses an invalid name, and does so before the old file
        // goes, so a bad name loses nothing.
        self.save(&p)?;
        // Atomic replacement can retain the existing filename's spelling.
        // Rename that entry explicitly; removing `old` would delete the result.
        if aliases_old {
            std::fs::rename(self.path(old), self.path(new))?;
        } else {
            self.remove(old)?;
        }
        Ok(p)
    }
}
