//! One-time upgrades discovered while opening an existing Workspace.

use crate::{
    config::Config,
    meta::MetaStore,
    preset::PresetStore,
    tag::{Tag, TagStore},
    util::write_atomic,
};
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use std::{
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use toml_edit::{Document, DocumentMut, Item};

#[derive(Debug, Clone, Serialize)]
pub struct MigrationReport {
    pub backup_dir: PathBuf,
    pub migrated_names: Vec<String>,
    pub migrated_tags: Vec<String>,
    pub config_migrated: bool,
    pub migrated_repositories: Vec<String>,
}

#[derive(Clone)]
struct PresetChange {
    path: PathBuf,
    target: PathBuf,
    original: Vec<u8>,
    updated: Vec<u8>,
    name: Option<String>,
}

#[derive(Clone)]
struct SchemaChange {
    path: PathBuf,
    original: Vec<u8>,
    updated: Vec<u8>,
}

fn validate_repository_document(path: &Path, doc: &DocumentMut) -> Result<()> {
    let filename_alias = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .context("repository metadata filename is not valid UTF-8")?;
    let mut value: toml::Value = toml::from_str(&doc.to_string())?;
    let table = value
        .as_table_mut()
        .context("repository metadata must be a TOML table")?;
    table.remove("schema");

    let is_root = filename_alias == ".root";
    if is_root {
        ensure!(
            table.get("alias").is_none() && table.get("url").is_none(),
            ".root.toml must not define repository identity"
        );
    } else {
        let repository: crate::repository::Repository = toml::Value::Table(table.clone())
            .try_into()
            .with_context(|| format!("invalid repository identity in {}", path.display()))?;
        ensure!(
            crate::util::valid_skill_key(&repository.alias),
            "invalid repository alias: {:?}",
            repository.alias
        );
        ensure!(
            repository.alias == filename_alias,
            "repository metadata filename {filename_alias:?} does not match alias {:?}",
            repository.alias
        );
        if let Some(name) = &repository.name {
            crate::repository::validate_name(name)?;
        }
    }

    let top_kind = table
        .get("kind")
        .and_then(toml::Value::as_str)
        .unwrap_or("git")
        .to_owned();
    let top_url = table
        .get("url")
        .and_then(toml::Value::as_str)
        .map(str::to_owned);
    let top_branch = table
        .get("branch")
        .and_then(toml::Value::as_str)
        .map(str::to_owned);
    if let Some(skills) = table.get_mut("skills") {
        let skills = skills.as_table_mut().context("skills must be a table")?;
        for (name, metadata) in skills.iter_mut() {
            let key = if is_root {
                name.to_owned()
            } else {
                format!("repos/{filename_alias}/{name}")
            };
            ensure!(
                crate::repository::valid_id(&key),
                "invalid skill identity: {key}"
            );
            if let Some(source) = metadata
                .get_mut("source")
                .and_then(toml::Value::as_table_mut)
                && matches!(
                    source.get("type").and_then(toml::Value::as_str),
                    Some("git" | "archive")
                )
                && !is_root
            {
                ensure!(
                    source.get("type").and_then(toml::Value::as_str) == Some(top_kind.as_str()),
                    "repository kind differs from skill source"
                );
                source.insert(
                    "url".into(),
                    top_url.as_deref().context("repository URL missing")?.into(),
                );
                if top_kind == "git"
                    && let Some(branch) = &top_branch
                {
                    source.insert("branch".into(), branch.as_str().into());
                }
            }
            let _: crate::meta::SkillMeta = metadata
                .clone()
                .try_into()
                .with_context(|| format!("invalid metadata for skill {key}"))?;
        }
    }
    Ok(())
}

fn plan_repository_documents(root: &Path) -> Result<Vec<SchemaChange>> {
    let dir = crate::paths::meta_dir(root).join("repos");
    let rd = match std::fs::read_dir(&dir) {
        Ok(rd) => rd,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut paths = Vec::new();
    for entry in rd {
        let entry = entry?;
        let ty = entry.file_type()?;
        ensure!(
            ty.is_file() && !ty.is_symlink(),
            "invalid repository metadata entry: {}",
            entry.path().display()
        );
        let path = entry.path();
        ensure!(
            path.extension()
                .is_some_and(|extension| extension == "toml"),
            "invalid repository metadata entry: {}",
            path.display()
        );
        paths.push(path);
    }
    paths.sort();
    let mut changes = Vec::new();
    for path in paths {
        let original = std::fs::read(&path)?;
        let text = std::str::from_utf8(&original)
            .with_context(|| format!("invalid UTF-8 in {}", path.display()))?;
        let mut doc = text
            .parse::<DocumentMut>()
            .with_context(|| format!("invalid repository metadata: {}", path.display()))?;
        let schema = crate::schema::version(
            &doc,
            &path,
            "repository metadata",
            crate::schema::REPOSITORY,
            0,
        )?;
        validate_repository_document(&path, &doc)?;
        if schema == crate::schema::REPOSITORY {
            continue;
        }
        crate::schema::set(&mut doc, crate::schema::REPOSITORY);
        changes.push(SchemaChange {
            path,
            original,
            updated: doc.to_string().into_bytes(),
        });
    }
    Ok(changes)
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
    doc.remove("deploy");
    crate::schema::set(&mut doc, crate::schema::CONFIG);
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
    let mut paths = Vec::new();
    for entry in rd {
        let entry = entry?;
        let ty = entry.file_type()?;
        ensure!(
            ty.is_file() && !ty.is_symlink(),
            "invalid preset store entry: {}",
            entry.path().display()
        );
        let path = entry.path();
        ensure!(
            path.extension()
                .is_some_and(|extension| extension == "toml"),
            "invalid preset store entry: {}",
            path.display()
        );
        paths.push(path);
    }
    paths.sort();
    let mut planned = Vec::new();
    for path in paths {
        let original = std::fs::read(&path)?;
        let text = std::str::from_utf8(&original)
            .with_context(|| format!("invalid UTF-8 in {}", path.display()))?;
        let mut doc = text
            .parse::<DocumentMut>()
            .with_context(|| format!("invalid preset: {}", path.display()))?;
        let schema = crate::schema::version(&doc, &path, "preset", crate::schema::PRESET, 0)?;
        let legacy = doc.remove("tags");
        let had_legacy = legacy.is_some();
        let mut domain = doc.clone();
        domain.remove("schema");
        let mut preset: crate::preset::Preset = toml::from_str(&domain.to_string())
            .with_context(|| format!("invalid preset: {}", path.display()))?;
        preset.name = crate::group_filename::normalize_name(&preset.name)?;
        if let Some(legacy) = legacy {
            let names: Vec<String> = legacy
                .as_array()
                .with_context(|| {
                    format!("legacy preset tags must be an array in {}", path.display())
                })?
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .map(str::to_owned)
                        .context("legacy preset tag must be a string")
                })
                .collect::<Result<_>>()
                .with_context(|| format!("invalid legacy tags in {}", path.display()))?;
            let expanded = crate::preset::tag_members(config, &names).with_context(|| {
                format!(
                    "cannot migrate {}; original presets were left unchanged",
                    path.display()
                )
            })?;
            if doc.get("skills").is_none() {
                doc["skills"] = toml_edit::value(toml_edit::Array::new());
            }
            let skills = doc
                .get_mut("skills")
                .and_then(Item::as_array_mut)
                .context("preset skills must be an array")?;
            let mut seen = std::collections::BTreeSet::new();
            skills.retain(|value| {
                value
                    .as_str()
                    .is_some_and(|skill| seen.insert(skill.to_owned()))
            });
            for skill in expanded {
                if seen.insert(skill.clone()) {
                    skills.push(skill);
                }
            }
        }
        if schema < crate::schema::PRESET {
            crate::schema::set(&mut doc, crate::schema::PRESET);
        }
        planned.push((path, original, doc, preset.name, had_legacy, schema));
    }
    let allocation = crate::group_filename::allocate(planned.iter().map(|item| item.3.as_str()))?;
    let mut changes = Vec::new();
    for (path, original, doc, name, had_legacy, schema) in planned {
        let target = store.dir.join(format!("{}.toml", allocation[&name]));
        if schema == crate::schema::PRESET && !had_legacy && path == target {
            continue;
        }
        changes.push(PresetChange {
            path,
            target,
            original,
            updated: doc.to_string().into_bytes(),
            name: Some(name),
        });
    }
    Ok(changes)
}

/// Run all legacy Tag-dependent upgrades under one metadata lock. Normal
/// startup supplies already loaded Tag entries, avoiding another store scan.
pub(crate) fn migrate_metadata(
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
    let config_schema = if config_original.is_empty() {
        crate::schema::CONFIG
    } else {
        crate::schema::version(
            &parsed_config,
            &config_path,
            "config",
            crate::schema::CONFIG,
            1,
        )?
    };
    let has_legacy_config = !config_original.is_empty() && config_schema < crate::schema::CONFIG;
    let has_legacy_tags = parsed_config.get("tags").is_some();
    // Presets were already checked at Workspace startup before this combined
    // migration existed. Keep discovering legacy references even when config
    // Tags were migrated by an earlier run; the same loaded aggregate still
    // supplies their members.
    let preset_changes = plan_presets(presets, config)?;
    let repository_changes = plan_repository_documents(root)?;
    let tag_allocation =
        crate::group_filename::allocate(loaded_tags.iter().map(|entry| entry.tag.name.as_str()))?;
    let tags_need_schema = loaded_tags.iter().any(|entry| {
        let schema_missing = std::str::from_utf8(&entry.bytes)
            .ok()
            .and_then(|text| text.parse::<DocumentMut>().ok())
            .is_some_and(|doc| doc.get("schema").is_none());
        let expected = tags.path_for_stem(&tag_allocation[&entry.tag.name]);
        schema_missing || entry.path != expected
    });
    if !has_legacy_config
        && !has_legacy_tags
        && !tags_need_schema
        && preset_changes.is_empty()
        && repository_changes.is_empty()
    {
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
    let mut desired_entries = tags.choose_entries(loaded_tags, desired_tags)?;
    for entry in &mut desired_entries {
        let doc = std::str::from_utf8(&entry.bytes)?.parse::<DocumentMut>()?;
        if doc.get("schema").is_none() {
            entry.bytes = TagStore::serialize(&entry.tag)?;
        }
    }
    let config_updated = if has_legacy_config || has_legacy_tags {
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
    for change in &repository_changes {
        ensure!(
            std::fs::read(&change.path)? == change.original,
            "{} changed during migration",
            change.path.display()
        );
    }
    for entry in loaded_tags {
        ensure!(
            std::fs::read(&entry.path)? == entry.bytes,
            "Tag {} changed during migration",
            entry.tag.name
        );
    }
    let source_paths: std::collections::BTreeSet<_> =
        loaded_tags.iter().map(|entry| entry.path.clone()).collect();
    for entry in &desired_entries {
        if !source_paths.contains(&entry.path) {
            ensure!(
                !entry.path.exists(),
                "Tag destination {} appeared during migration",
                entry.path.display()
            );
        }
    }

    let changed_tag_paths: Vec<_> = loaded_tags
        .iter()
        .filter(|old| {
            desired_entries
                .iter()
                .find(|desired| desired.tag.name == old.tag.name)
                .is_none_or(|desired| desired.path != old.path || desired.bytes != old.bytes)
        })
        .map(|entry| entry.path.clone())
        .collect();
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let backups_dir = crate::paths::meta_dir(root).join("backups");
    let backup_dir = backups_dir.join(format!("metadata-before-schema-migration-{stamp}"));
    let backup_staging = backups_dir.join(format!(".schema-migration-{stamp}.tmp"));
    std::fs::create_dir_all(&backup_staging)?;
    let backup_result = (|| -> Result<()> {
        if has_legacy_config || has_legacy_tags {
            write_atomic(&backup_staging.join("config.toml"), &config_original)?;
        }
        if !preset_changes.is_empty() {
            std::fs::create_dir_all(backup_staging.join("presets"))?;
            for change in &preset_changes {
                write_atomic(
                    &backup_staging
                        .join("presets")
                        .join(change.path.file_name().context("preset filename missing")?),
                    &change.original,
                )?;
            }
        }
        if !changed_tag_paths.is_empty() {
            std::fs::create_dir_all(backup_staging.join("tags"))?;
            for path in &changed_tag_paths {
                let old = loaded_tags
                    .iter()
                    .find(|entry| &entry.path == path)
                    .unwrap();
                write_atomic(
                    &backup_staging
                        .join("tags")
                        .join(path.file_name().context("Tag filename missing")?),
                    &old.bytes,
                )?;
            }
        }
        if !repository_changes.is_empty() {
            std::fs::create_dir_all(backup_staging.join("repos"))?;
            for change in &repository_changes {
                write_atomic(
                    &backup_staging.join("repos").join(
                        change
                            .path
                            .file_name()
                            .context("repository metadata filename missing")?,
                    ),
                    &change.original,
                )?;
            }
        }
        std::fs::rename(&backup_staging, &backup_dir)?;
        Ok(())
    })();
    if let Err(error) = backup_result {
        let _ = std::fs::remove_dir_all(&backup_staging);
        return Err(error).context("creating schema migration backup");
    }

    // Publish definitions and references first; config is the final source marker.
    tags.apply_entries(loaded_tags, &desired_entries)?;
    if !preset_changes.is_empty() {
        let source_paths: std::collections::BTreeSet<_> = preset_changes
            .iter()
            .map(|change| change.path.clone())
            .collect();
        for change in &preset_changes {
            ensure!(
                source_paths.contains(&change.target) || !change.target.exists(),
                "preset destination {} appeared during migration",
                change.target.display()
            );
        }
        crate::file_set::publish(
            &presets.dir,
            &preset_changes
                .iter()
                .map(|change| crate::file_set::File {
                    path: &change.path,
                    bytes: &change.original,
                })
                .collect::<Vec<_>>(),
            &preset_changes
                .iter()
                .map(|change| crate::file_set::File {
                    path: &change.target,
                    bytes: &change.updated,
                })
                .collect::<Vec<_>>(),
        )
        .context("publishing migrated preset files")?;
    }

    for change in &repository_changes {
        ensure!(
            std::fs::read(&change.path)? == change.original,
            "{} changed during migration",
            change.path.display()
        );
        write_atomic(&change.path, &change.updated)?;
    }
    if has_legacy_config || has_legacy_tags {
        ensure!(
            std::fs::read(&config_path)? == config_original,
            "config.toml changed during migration"
        );
        write_atomic(&config_path, &config_updated)?;
    }

    let migrated_repositories = repository_changes
        .iter()
        .filter_map(|change| {
            change
                .path
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned)
        })
        .collect();
    let mut migrated_tags: std::collections::BTreeSet<_> = config
        .tags
        .iter()
        .filter(|tag| {
            has_legacy_tags && !loaded_tags.iter().any(|entry| entry.tag.name == tag.name)
        })
        .map(|tag| tag.name.clone())
        .collect();
    migrated_tags.extend(
        desired_entries
            .iter()
            .filter(|entry| {
                loaded_tags.iter().any(|old| {
                    old.tag.name == entry.tag.name
                        && (old.path != entry.path || old.bytes != entry.bytes)
                })
            })
            .map(|entry| entry.tag.name.clone()),
    );
    Ok(Some(MigrationReport {
        backup_dir,
        migrated_names: preset_changes
            .into_iter()
            .filter_map(|change| change.name)
            .collect(),
        migrated_tags: migrated_tags.into_iter().collect(),
        config_migrated: has_legacy_config || has_legacy_tags,
        migrated_repositories,
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
        let report = migrate_metadata(temp.path(), &config, &store, &loaded, &presets)
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
            migrate_metadata(temp.path(), &config, &store, &loaded, &presets)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn schema_validation_rejects_invalid_and_future_versions() {
        let path = Path::new("fixture.toml");
        for invalid in [
            "schema = 0",
            "schema = -1",
            "schema = 'one'",
            "schema = 1.5",
        ] {
            let doc = invalid.parse::<DocumentMut>().unwrap();
            assert!(crate::schema::version(&doc, path, "Tag", 1, 0).is_err());
        }
        let doc = "schema = 2".parse::<DocumentMut>().unwrap();
        let error = crate::schema::version(&doc, path, "Tag", 1, 0).unwrap_err();
        assert!(format!("{error:#}").contains("newer version"));
    }

    #[test]
    fn migrates_all_document_schemas_with_exact_typed_backups_and_is_idempotent() {
        let temp = DownloadDir::new("all-schema-migration").unwrap();
        let meta = crate::paths::meta_dir(temp.path());
        std::fs::create_dir_all(meta.join("tags")).unwrap();
        std::fs::create_dir_all(meta.join("presets")).unwrap();
        std::fs::create_dir_all(meta.join("repos")).unwrap();
        let config = "# config\nschema = 1\ntags = [{ name = '工作', skills = ['one'] }]\n";
        let preset = "# preset\nname = 'daily'\ntags = ['工作']\n";
        let tag_id = meta.join("tags/tag-existing.toml");
        let tag = "# tag\nname = '现有'\nskills = ['two']\n";
        let repo =
            "# repo\nalias = 'demo'\nurl = 'https://example.com/demo'\n[skills.one]\nnote = 'x'\n";
        let root_repo =
            "# root\n[skills.local.source]\ntype = 'git'\nurl = 'https://example.com/root'\n";
        std::fs::write(Config::path(temp.path()), config).unwrap();
        std::fs::write(meta.join("presets/daily.toml"), preset).unwrap();
        std::fs::write(&tag_id, tag).unwrap();
        std::fs::write(meta.join("repos/demo.toml"), repo).unwrap();
        std::fs::write(meta.join("repos/.root.toml"), root_repo).unwrap();

        let config_loaded = Config::load_legacy(temp.path()).unwrap();
        let store = TagStore::new(temp.path());
        let loaded = store.entries_for_migration().unwrap();
        let report = migrate_metadata(
            temp.path(),
            &config_loaded,
            &store,
            &loaded,
            &PresetStore::new(temp.path()),
        )
        .unwrap()
        .unwrap();
        assert!(report.config_migrated);
        assert_eq!(report.migrated_names, ["daily"]);
        assert_eq!(report.migrated_repositories, [".root.toml", "demo.toml"]);
        assert!(report.migrated_tags.contains(&"现有".to_string()));
        assert_eq!(
            std::fs::read(report.backup_dir.join("config.toml")).unwrap(),
            config.as_bytes()
        );
        assert_eq!(
            std::fs::read(report.backup_dir.join("presets/daily.toml")).unwrap(),
            preset.as_bytes()
        );
        assert_eq!(
            std::fs::read(report.backup_dir.join("tags/tag-existing.toml")).unwrap(),
            tag.as_bytes()
        );
        assert_eq!(
            std::fs::read(report.backup_dir.join("repos/demo.toml")).unwrap(),
            repo.as_bytes()
        );
        assert_eq!(
            std::fs::read(report.backup_dir.join("repos/.root.toml")).unwrap(),
            root_repo.as_bytes()
        );
        for path in [
            Config::path(temp.path()),
            meta.join("presets/daily.toml"),
            meta.join("tags/现有.toml"),
            meta.join("repos/demo.toml"),
            meta.join("repos/.root.toml"),
        ] {
            let text = std::fs::read_to_string(path).unwrap();
            assert!(text.contains("schema = "));
        }
        let config_loaded = Config::load_legacy(temp.path()).unwrap();
        let loaded = store.entries_for_migration().unwrap();
        assert!(
            migrate_metadata(
                temp.path(),
                &config_loaded,
                &store,
                &loaded,
                &PresetStore::new(temp.path()),
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn future_schema_preflight_writes_nothing() {
        let temp = DownloadDir::new("future-schema-preflight").unwrap();
        let meta = crate::paths::meta_dir(temp.path());
        std::fs::create_dir_all(meta.join("presets")).unwrap();
        std::fs::write(Config::path(temp.path()), "schema = 1\n").unwrap();
        let preset = "schema = 2\nname = 'future'\n";
        std::fs::write(meta.join("presets/future.toml"), preset).unwrap();
        let config = Config::load_legacy(temp.path()).unwrap();
        let store = TagStore::new(temp.path());
        let error = migrate_metadata(
            temp.path(),
            &config,
            &store,
            &[],
            &PresetStore::new(temp.path()),
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("unsupported preset schema 2"));
        assert_eq!(
            std::fs::read(meta.join("presets/future.toml")).unwrap(),
            preset.as_bytes()
        );
        assert!(!meta.join("backups").exists());
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
            migrate_metadata(
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
