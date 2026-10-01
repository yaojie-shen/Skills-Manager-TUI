//! One-time upgrades discovered while opening an existing Workspace.

use crate::{
    config::Config,
    meta::MetaStore,
    preset::{MigrationReport, PresetStore},
    tag::{Tag, TagStore},
    util::write_atomic,
};
use anyhow::{Context, Result, ensure};
use std::{
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use toml_edit::{Document, DocumentMut, Item};

#[derive(Clone)]
struct PresetChange {
    path: PathBuf,
    original: Vec<u8>,
    updated: Vec<u8>,
    name: String,
}

fn normalize(tag: &Tag) -> Tag {
    let mut tag = tag.clone();
    tag.skills.sort();
    tag.skills.dedup();
    tag
}

fn config_without_tags(text: &str) -> Result<DocumentMut> {
    let parsed = Document::parse(text.to_owned())?;
    let prefix = parsed
        .as_table()
        .key("tags")
        .and_then(|key| key.leaf_decor().prefix())
        .and_then(|raw| raw.span())
        .and_then(|span| text.get(span))
        .unwrap_or("")
        .to_owned();
    let mut doc = parsed.into_mut();
    doc.remove("tags");
    if !prefix.is_empty() {
        let next_key = doc.iter().next().map(|(key, _)| key.to_owned());
        if let Some(key) = next_key {
            match doc.get_mut(&key) {
                Some(Item::Value(_)) => {
                    if let Some(mut key) = doc.as_table_mut().key_mut(&key) {
                        key.leaf_decor_mut().set_prefix(prefix);
                    }
                }
                Some(Item::Table(table)) => table.decor_mut().set_prefix(prefix),
                Some(Item::ArrayOfTables(tables)) => {
                    if let Some(table) = tables.get_mut(0) {
                        table.decor_mut().set_prefix(prefix);
                    }
                }
                _ => {}
            }
        } else {
            doc.as_table_mut().decor_mut().set_prefix(prefix);
        }
    }
    Ok(doc)
}

fn plan_presets(store: &PresetStore, config: &Config) -> Result<Vec<PresetChange>> {
    let rd = match std::fs::read_dir(&store.dir) {
        Ok(rd) => rd,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut paths: Vec<_> = rd
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "toml")
        })
        .collect();
    paths.sort();
    let mut changes = Vec::new();
    for path in paths {
        let original = std::fs::read(&path)?;
        let text = std::str::from_utf8(&original)?;
        let mut value: toml::Value =
            toml::from_str(text).with_context(|| format!("invalid preset: {}", path.display()))?;
        let legacy = value
            .as_table_mut()
            .context("preset must be a TOML table")?
            .remove("tags");
        let mut preset: crate::preset::Preset = value
            .try_into()
            .with_context(|| format!("invalid preset: {}", path.display()))?;
        let Some(legacy) = legacy else { continue };
        let names: Vec<String> = legacy
            .try_into()
            .with_context(|| format!("invalid legacy tags in {}", path.display()))?;
        preset
            .skills
            .extend(crate::preset::tag_members(config, &names).with_context(|| {
                format!(
                    "cannot migrate {}; original presets were left unchanged",
                    path.display()
                )
            })?);
        preset.skills = preset.members();
        changes.push(PresetChange {
            path,
            original,
            updated: toml::to_string_pretty(&preset)?.into_bytes(),
            name: preset.name,
        });
    }
    Ok(changes)
}

/// Run all legacy Tag-dependent upgrades under one metadata lock. Normal
/// startup supplies already loaded Tag entries, avoiding another store scan.
pub(crate) fn migrate_legacy_tags(
    root: &Path,
    config: &Config,
    tags: &TagStore,
    loaded_tags: &[crate::tag::Entry],
    presets: &PresetStore,
) -> Result<Option<MigrationReport>> {
    let config_path = Config::path(root);
    let config_original = match std::fs::read(&config_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    let config_text = std::str::from_utf8(&config_original)?;
    let parsed_config = if config_original.is_empty() {
        DocumentMut::new()
    } else {
        config_text.parse::<DocumentMut>()?
    };
    let has_legacy_tags = parsed_config.get("tags").is_some();
    // Presets were already checked at Workspace startup before this combined
    // migration existed. Keep discovering legacy references even when config
    // Tags were migrated by an earlier run; the same loaded aggregate still
    // supplies their members.
    let preset_changes = plan_presets(presets, config)?;
    if !has_legacy_tags && preset_changes.is_empty() {
        return Ok(None);
    }

    let existing = TagStore::tags(loaded_tags);
    for legacy in &config.tags {
        if let Some(current) = existing.iter().find(|tag| tag.name == legacy.name) {
            ensure!(
                normalize(current) == normalize(legacy),
                "Tag {} differs between config.toml and the Tag store",
                legacy.name
            );
        }
    }
    let mut desired_tags = existing;
    for legacy in &config.tags {
        if !desired_tags.iter().any(|tag| tag.name == legacy.name) {
            desired_tags.push(legacy.clone());
        }
    }
    let desired_entries = tags.choose_entries(loaded_tags, desired_tags)?;
    let config_updated = if has_legacy_tags {
        config_without_tags(config_text)?.to_string().into_bytes()
    } else {
        config_original.clone()
    };

    let _lock = MetaStore::new(root).lock()?;
    if !config_original.is_empty() {
        ensure!(
            std::fs::read(&config_path)? == config_original,
            "config.toml changed during migration"
        );
    }
    for change in &preset_changes {
        ensure!(
            std::fs::read(&change.path)? == change.original,
            "{} changed during migration",
            change.path.display()
        );
    }
    for entry in loaded_tags {
        ensure!(
            std::fs::read(tags.path(&entry.id))? == entry.bytes,
            "Tag {} changed during migration",
            entry.tag.name
        );
    }
    let loaded_ids: std::collections::BTreeSet<_> =
        loaded_tags.iter().map(|entry| entry.id.as_str()).collect();
    for entry in &desired_entries {
        if !loaded_ids.contains(entry.id.as_str()) {
            ensure!(
                !tags.path(&entry.id).exists(),
                "Tag destination {} appeared during migration",
                tags.path(&entry.id).display()
            );
        }
    }

    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let backup_dir = crate::paths::meta_dir(root)
        .join("backups")
        .join(format!("metadata-before-migration-{stamp}"));
    std::fs::create_dir_all(&backup_dir)?;
    if has_legacy_tags {
        write_atomic(&backup_dir.join("config.toml"), &config_original)?;
    }
    for change in &preset_changes {
        write_atomic(
            &backup_dir.join(change.path.file_name().context("preset filename missing")?),
            &change.original,
        )?;
    }

    // Publish definitions and references first; config is the final source marker.
    tags.apply_entries(loaded_tags, &desired_entries)?;
    for change in &preset_changes {
        write_atomic(&change.path, &change.updated)?;
    }
    if has_legacy_tags {
        write_atomic(&config_path, &config_updated)?;
    }

    Ok(Some(MigrationReport {
        backup_dir,
        migrated_names: preset_changes
            .into_iter()
            .map(|change| change.name)
            .collect(),
        migrated_tags: config.tags.iter().map(|tag| tag.name.clone()).collect(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::TagConfig, ops::DownloadDir};

    fn tag(name: &str, skills: &[&str]) -> TagConfig {
        TagConfig {
            name: name.into(),
            skills: skills.iter().map(|s| (*s).into()).collect(),
            color: None,
            description: None,
        }
    }

    #[test]
    fn migrates_inline_tags_and_crash_retry_without_marker() {
        let temp = DownloadDir::new("tag-migration-inline").unwrap();
        std::fs::create_dir_all(crate::paths::meta_dir(temp.path())).unwrap();
        std::fs::write(
            Config::path(temp.path()),
            "# keep\ntags = [{ name = '工作', skills = ['b', 'a'] }]\ntags_enabled = true\n",
        )
        .unwrap();
        let config = Config::load_legacy(temp.path()).unwrap();
        let store = TagStore::new(temp.path());
        let loaded = store.entries().unwrap();
        let presets = PresetStore::new(temp.path());
        let report = migrate_legacy_tags(temp.path(), &config, &store, &loaded, &presets)
            .unwrap()
            .unwrap();
        assert_eq!(report.migrated_tags, ["工作"]);
        let migrated = std::fs::read_to_string(Config::path(temp.path())).unwrap();
        assert!(migrated.starts_with("# keep\n"), "{migrated:?}");
        assert!(
            !migrated
                .lines()
                .any(|line| line.trim_start().starts_with("tags ="))
        );
        assert_eq!(store.list().unwrap()[0].skills, ["a", "b"]);
        let config = Config::load_legacy(temp.path()).unwrap();
        let loaded = store.entries().unwrap();
        assert!(
            migrate_legacy_tags(temp.path(), &config, &store, &loaded, &presets)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn conflicting_store_tag_leaves_legacy_config_untouched() {
        let temp = DownloadDir::new("tag-migration-conflict").unwrap();
        std::fs::create_dir_all(crate::paths::meta_dir(temp.path())).unwrap();
        std::fs::write(
            Config::path(temp.path()),
            "[[tags]]\nname = 'same'\nskills = ['old']\n",
        )
        .unwrap();
        let store = TagStore::new(temp.path());
        store.save(&tag("same", &["new"])).unwrap();
        let before = std::fs::read(Config::path(temp.path())).unwrap();
        let config = Config::load_legacy(temp.path()).unwrap();
        let loaded = store.entries().unwrap();
        assert!(
            migrate_legacy_tags(
                temp.path(),
                &config,
                &store,
                &loaded,
                &PresetStore::new(temp.path())
            )
            .is_err()
        );
        assert_eq!(std::fs::read(Config::path(temp.path())).unwrap(), before);
    }
}
