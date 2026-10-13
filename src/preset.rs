//! Skill packages with fixed members, stored in
//! `<root>/.skills-meta/presets/<name>.toml`.

use crate::config::Config;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub const PRESET_DIR: &str = "presets";

pub use crate::migration::MigrationReport;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Preset {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default)]
    pub skills: Vec<String>,
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

impl crate::group_store::Group for Preset {
    const DIR: &'static str = PRESET_DIR;
    const KIND: &'static str = "preset";
    const SCHEMA: u32 = crate::schema::PRESET;
    const FIELDS: &'static [&'static str] = &["name", "description", "color", "skills"];
    fn name(&self) -> &str {
        &self.name
    }
    fn name_mut(&mut self) -> &mut String {
        &mut self.name
    }

    fn skills_mut(&mut self) -> &mut Vec<String> {
        &mut self.skills
    }
    fn check_document(doc: &toml_edit::DocumentMut, path: &Path) -> Result<()> {
        anyhow::ensure!(
            doc.get("tags").is_none(),
            "{} uses legacy preset Tag references, but this Skills Manager expects fixed skill members; {}",
            path.display(),
            crate::schema::legacy_advice()
        );
        Ok(())
    }
    fn missing_rename(name: &str) -> Result<()> {
        anyhow::bail!("no such preset: {name}")
    }
}

pub type PresetStore = crate::group_store::GroupStore<Preset>;
impl crate::group_store::GroupStore<Preset> {
    pub fn remove_skill(&self, name: &str, key: &str) -> Result<bool> {
        let mut changed = false;
        self.edit(|presets| {
            if let Some(preset) = presets.iter_mut().find(|p| p.name == name) {
                let old = preset.skills.len();
                preset.skills.retain(|skill| skill != key);
                changed = old != preset.skills.len();
            }
        })?;
        Ok(changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ops::DownloadDir, paths::meta_dir};

    #[test]
    fn missing_remove_skill_is_a_physical_noop() {
        let temp = DownloadDir::new("preset-remove-skill-noop").unwrap();
        let store = PresetStore::new(temp.path());
        std::fs::create_dir_all(&store.dir).unwrap();
        let path = store.dir.join("odd.toml");
        let bytes = b"schema = 1\nname = 'daily'\nskills = ['one']\n";
        std::fs::write(&path, bytes).unwrap();
        assert!(!store.remove_skill("daily", "missing").unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(std::fs::read_dir(&store.dir).unwrap().count(), 1);
        assert!(!meta_dir(temp.path()).join(".staging").exists());
    }
}
