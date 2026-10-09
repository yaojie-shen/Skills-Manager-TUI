//! Skill packages with fixed members, stored in
//! `<root>/.skills-meta/presets/<name>.toml`.

use crate::config::Config;
use crate::paths::meta_dir;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use toml_edit::DocumentMut;

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
    root: PathBuf,
}

#[derive(Debug, Clone)]
struct StoredPreset {
    path: PathBuf,
    preset: Preset,
    bytes: Vec<u8>,
}

impl PresetStore {
    pub fn new(root: &Path) -> Self {
        Self {
            dir: meta_dir(root).join(PRESET_DIR),
            root: root.to_path_buf(),
        }
    }

    fn path_for_stem(&self, stem: &str) -> PathBuf {
        self.dir.join(format!("{stem}.toml"))
    }

    pub fn path(&self, name: &str) -> PathBuf {
        let normalized = crate::group_filename::normalize_name(name).ok();
        if let (Some(name), Ok(entries)) = (normalized.as_ref(), self.entries())
            && let Some(entry) = entries.iter().find(|entry| &entry.preset.name == name)
        {
            return entry.path.clone();
        }
        let stem = crate::group_filename::safe_stem(name).unwrap_or_else(|_| "invalid".into());
        self.path_for_stem(&stem)
    }

    fn entries(&self) -> Result<Vec<StoredPreset>> {
        let rd = match std::fs::read_dir(&self.dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut paths = Vec::new();
        for entry in rd {
            let entry = entry?;
            let ty = entry.file_type()?;
            crate::util::reject_interrupted_transaction(&entry.path(), &ty)?;
            if !crate::util::is_store_document_name(&entry.file_name()) {
                continue;
            }
            anyhow::ensure!(
                ty.is_file() && !ty.is_symlink(),
                "invalid preset store entry: {}",
                entry.path().display()
            );
            let path = entry.path();
            anyhow::ensure!(
                path.file_name().and_then(|n| n.to_str()).is_some(),
                "preset filename is not valid UTF-8: {}",
                path.display()
            );
            paths.push(path);
        }
        paths.sort();
        let mut files = Vec::new();
        for path in paths {
            let bytes = std::fs::read(&path)?;
            files.push((path, bytes));
        }
        self.parse_entries(files)
    }

    /// Validate preset documents already read from this store's directory,
    /// including the canonical filename check.
    pub(crate) fn validate_documents(&self, files: Vec<(PathBuf, Vec<u8>)>) -> Result<()> {
        self.parse_entries(files).map(drop)
    }

    fn parse_entries(&self, files: Vec<(PathBuf, Vec<u8>)>) -> Result<Vec<StoredPreset>> {
        let mut out = Vec::new();
        let mut names = BTreeSet::new();
        for (path, bytes) in files {
            let text = std::str::from_utf8(&bytes)?;
            let mut doc: DocumentMut = text
                .parse()
                .with_context(|| format!("invalid preset: {}", path.display()))?;
            crate::schema::require_current(&doc, &path, "preset", crate::schema::PRESET, 0)?;
            anyhow::ensure!(
                doc.get("tags").is_none(),
                "{} uses legacy preset Tag references, but this Skills Manager expects fixed skill members; {}",
                path.display(),
                crate::schema::legacy_advice()
            );
            doc.remove("schema");
            let mut preset: Preset = toml::from_str(&doc.to_string())
                .with_context(|| format!("invalid preset: {}", path.display()))?;
            preset.name = crate::group_filename::normalize_name(&preset.name)?;
            anyhow::ensure!(
                names.insert(preset.name.clone()),
                "duplicate preset name: {}",
                preset.name
            );
            out.push(StoredPreset {
                path,
                preset,
                bytes,
            });
        }
        let allocation =
            crate::group_filename::allocate(out.iter().map(|entry| entry.preset.name.as_str()))?;
        for entry in &out {
            let expected = self.path_for_stem(&allocation[&entry.preset.name]);
            anyhow::ensure!(
                entry.path == expected,
                "noncanonical preset filename {} for layout {} (expected {}); copy the originals you need back from .skills-meta/backups, including format.toml if that backup has one, otherwise delete .skills-meta/format.toml; then reopen to re-run migration",
                entry.path.display(),
                crate::migration::CURRENT_LAYOUT,
                expected.display()
            );
        }
        Ok(out)
    }

    fn serialize(preset: &Preset) -> Result<Vec<u8>> {
        let mut doc = toml::to_string_pretty(preset)?.parse::<DocumentMut>()?;
        crate::schema::set(&mut doc, crate::schema::PRESET);
        Ok(doc.to_string().into_bytes())
    }

    fn apply(&self, before: &[StoredPreset], presets: Vec<Preset>) -> Result<()> {
        let allocation = crate::group_filename::allocate(presets.iter().map(|p| p.name.as_str()))?;
        let previous: BTreeMap<_, _> = before
            .iter()
            .map(|entry| (entry.preset.name.clone(), entry))
            .collect();
        let desired: Vec<_> = presets
            .into_iter()
            .map(|mut preset| {
                preset.name = crate::group_filename::normalize_name(&preset.name)?;
                preset.skills = preset.members();
                let path = self.path_for_stem(&allocation[&preset.name]);
                let bytes = if let Some(old) = previous.get(&preset.name) {
                    if old.preset == preset {
                        old.bytes.clone()
                    } else {
                        let mut doc = std::str::from_utf8(&old.bytes)?.parse::<DocumentMut>()?;
                        let fresh = toml::to_string(&preset)?.parse::<DocumentMut>()?;
                        for field in ["name", "description", "color", "skills", "agents"] {
                            if let Some(value) = fresh.get(field) {
                                doc[field] = value.clone();
                            } else {
                                doc.remove(field);
                            }
                        }
                        crate::schema::set(&mut doc, crate::schema::PRESET);
                        doc.to_string().into_bytes()
                    }
                } else {
                    Self::serialize(&preset)?
                };
                Ok(StoredPreset {
                    path,
                    preset,
                    bytes,
                })
            })
            .collect::<Result<_>>()?;
        self.apply_entries(before, &desired)
    }

    fn apply_entries(&self, before: &[StoredPreset], desired: &[StoredPreset]) -> Result<()> {
        for entry in before {
            anyhow::ensure!(
                std::fs::read(&entry.path)? == entry.bytes,
                "preset {} changed during this operation",
                entry.preset.name
            );
        }
        std::fs::create_dir_all(&self.dir)?;
        let sources: BTreeSet<_> = before.iter().map(|e| e.path.clone()).collect();
        for entry in desired {
            // A case-only rename may resolve to an existing source on macOS.
            let source_collision = before.iter().any(|source| {
                source
                    .path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(crate::group_filename::collision_key)
                    == entry
                        .path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .map(crate::group_filename::collision_key)
            });
            anyhow::ensure!(
                sources.contains(&entry.path) || source_collision || !entry.path.exists(),
                "preset destination {} appeared during this operation",
                entry.path.display()
            );
        }
        crate::file_set::publish(
            &self.dir,
            &before
                .iter()
                .map(|entry| crate::file_set::File {
                    path: &entry.path,
                    bytes: &entry.bytes,
                })
                .collect::<Vec<_>>(),
            &desired
                .iter()
                .map(|entry| crate::file_set::File {
                    path: &entry.path,
                    bytes: &entry.bytes,
                })
                .collect::<Vec<_>>(),
        )
        .context("publishing preset files")
    }

    pub fn contains_name(&self, name: &str) -> Result<bool> {
        let name = crate::group_filename::normalize_name(name)?;
        Ok(self
            .entries()?
            .iter()
            .any(|entry| entry.preset.name == name))
    }

    pub fn load(&self, name: &str) -> Result<Option<Preset>> {
        let name = crate::group_filename::normalize_name(name)?;
        Ok(self
            .entries()?
            .into_iter()
            .find(|entry| entry.preset.name == name)
            .map(|entry| entry.preset))
    }

    pub fn list(&self) -> Result<Vec<Preset>> {
        Ok(self
            .entries()?
            .into_iter()
            .map(|entry| entry.preset)
            .collect())
    }

    pub fn save(&self, preset: &Preset) -> Result<()> {
        let _lock = crate::meta::MetaStore::new(&self.root).lock()?;
        let mut preset = preset.clone();
        preset.name = crate::group_filename::normalize_name(&preset.name)?;
        let before = self.entries()?;
        let mut values: Vec<_> = before.iter().map(|e| e.preset.clone()).collect();
        match values.iter_mut().find(|p| p.name == preset.name) {
            Some(current) => *current = preset,
            None => values.push(preset),
        }
        self.apply(&before, values)
    }

    pub fn remove_skill(&self, name: &str, key: &str) -> Result<bool> {
        let _lock = crate::meta::MetaStore::new(&self.root).lock()?;
        let before = self.entries()?;
        let mut values: Vec<_> = before.iter().map(|e| e.preset.clone()).collect();
        let Some(preset) = values.iter_mut().find(|p| p.name == name) else {
            return Ok(false);
        };
        let old = preset.skills.len();
        preset.skills.retain(|skill| skill != key);
        if old == preset.skills.len() {
            return Ok(false);
        }
        self.apply(&before, values)?;
        Ok(true)
    }

    pub fn remove(&self, name: &str) -> Result<()> {
        let _lock = crate::meta::MetaStore::new(&self.root).lock()?;
        let name = crate::group_filename::normalize_name(name)?;
        let before = self.entries()?;
        let mut found = false;
        let values = before
            .iter()
            .filter_map(|e| {
                found |= e.preset.name == name;
                (e.preset.name != name).then(|| e.preset.clone())
            })
            .collect();
        if !found {
            bail!("no such preset: {name}");
        }
        self.apply(&before, values)
    }

    pub fn rename(&self, old: &str, new: &str) -> Result<Preset> {
        let _lock = crate::meta::MetaStore::new(&self.root).lock()?;
        let old = crate::group_filename::normalize_name(old)?;
        let new = crate::group_filename::normalize_name(new)?;
        let before = self.entries()?;
        if old == new {
            return before
                .into_iter()
                .find(|e| e.preset.name == old)
                .map(|e| e.preset)
                .with_context(|| format!("no such preset: {old}"));
        }
        anyhow::ensure!(
            !before.iter().any(|e| e.preset.name == new),
            "preset {new} already exists"
        );
        let source = before
            .iter()
            .find(|e| e.preset.name == old)
            .with_context(|| format!("no such preset: {old}"))?;
        let mut doc = std::str::from_utf8(&source.bytes)?.parse::<DocumentMut>()?;
        doc["name"] = toml_edit::value(&new);
        let mut values: Vec<_> = before.iter().map(|e| e.preset.clone()).collect();
        let renamed = values.iter_mut().find(|p| p.name == old).unwrap();
        renamed.name = new.clone();
        let result = renamed.clone();
        let allocation = crate::group_filename::allocate(values.iter().map(|p| p.name.as_str()))?;
        let previous: BTreeMap<_, _> = before.iter().map(|e| (e.preset.name.clone(), e)).collect();
        let desired: Vec<_> = values
            .into_iter()
            .map(|p| {
                let path = self.path_for_stem(&allocation[&p.name]);
                let bytes = if p.name == new {
                    doc.to_string().into_bytes()
                } else {
                    previous[&p.name].bytes.clone()
                };
                StoredPreset {
                    path,
                    preset: p,
                    bytes,
                }
            })
            .collect();
        // Keep the prepared document (including hand-edited comments).
        self.apply_entries(&before, &desired)?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::DownloadDir;

    #[test]
    fn stray_store_files_are_ignored_and_left_alone() {
        let temp = DownloadDir::new("preset-strays").unwrap();
        let store = PresetStore::new(temp.path());
        std::fs::create_dir_all(store.dir.join("notes")).unwrap();
        let strays = [".DS_Store", "daily.toml~", "README", ".daily.toml.tmp-1"];
        for name in strays {
            std::fs::write(store.dir.join(name), b"stray").unwrap();
        }
        store
            .save(&Preset {
                name: "daily".into(),
                skills: vec!["one".into()],
                ..Preset::default()
            })
            .unwrap();
        assert_eq!(store.list().unwrap().len(), 1);
        assert!(store.load("daily").unwrap().is_some());
        assert!(store.remove_skill("daily", "one").unwrap());
        store.rename("daily", "weekly").unwrap();
        store.remove("weekly").unwrap();
        assert!(store.list().unwrap().is_empty());
        for name in strays {
            assert_eq!(std::fs::read(store.dir.join(name)).unwrap(), b"stray");
        }
        assert!(store.dir.join("notes").is_dir());
    }

    #[test]
    fn toml_symlink_entries_are_still_rejected() {
        let temp = DownloadDir::new("preset-symlink").unwrap();
        let store = PresetStore::new(temp.path());
        std::fs::create_dir_all(&store.dir).unwrap();
        let target = temp.path().join("elsewhere.toml");
        std::fs::write(&target, "schema = 1\nname = 'linked'\n").unwrap();
        std::os::unix::fs::symlink(&target, store.dir.join("linked.toml")).unwrap();
        let error = store.list().unwrap_err();
        assert!(format!("{error:#}").contains("invalid preset store entry"));
    }
}
