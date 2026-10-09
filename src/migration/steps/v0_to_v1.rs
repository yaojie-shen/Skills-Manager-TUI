//! Frozen and idempotent layout-0 to layout-1 metadata transformation.

use super::StepNotes;
use crate::{
    config::Config,
    migration::snapshot::{HomeView, RelPath},
    tag::Tag,
};
use anyhow::{Context, Result, ensure};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};
use toml_edit::{Document, DocumentMut, Item};

fn normalize(mut tag: Tag) -> Result<Tag> {
    tag.name = crate::group_filename::normalize_name(&tag.name)?;
    tag.skills.sort();
    tag.skills.dedup();
    Ok(tag)
}
fn parse_tag(path: &Path, bytes: &[u8]) -> Result<(Tag, DocumentMut)> {
    let text = std::str::from_utf8(bytes)
        .with_context(|| format!("invalid UTF-8 in {}", path.display()))?;
    let mut doc = text
        .parse::<DocumentMut>()
        .with_context(|| format!("invalid Tag: {}", path.display()))?;
    crate::schema::version(&doc, path, "Tag", crate::schema::TAG, 0)?;
    let domain = doc.clone();
    doc.remove("schema");
    Ok((
        normalize(
            toml::from_str(&doc.to_string())
                .with_context(|| format!("invalid Tag: {}", path.display()))?,
        )?,
        domain,
    ))
}
type StoredTag = (RelPath, Vec<u8>, Tag, DocumentMut);
type DesiredTag = (Option<RelPath>, Vec<u8>, Tag);

fn merge_tags(stored: Vec<StoredTag>, inline: &[Tag]) -> Result<Vec<DesiredTag>> {
    let mut out = Vec::new();
    let mut names = BTreeSet::new();
    let mut stored_names = BTreeSet::new();
    for (path, bytes, tag, mut original) in stored {
        ensure!(
            names.insert(tag.name.clone()),
            "duplicate Tag name: {}",
            tag.name
        );
        stored_names.insert(tag.name.clone());
        if original.get("schema").is_none() {
            crate::schema::set(&mut original, crate::schema::TAG);
            out.push((Some(path), original.to_string().into_bytes(), tag));
        } else {
            out.push((Some(path), bytes, tag));
        }
    }
    for tag in inline {
        let tag = normalize(tag.clone())?;
        if stored_names.contains(&tag.name) {
            let current = &out
                .iter()
                .find(|(_, _, current)| current.name == tag.name)
                .unwrap()
                .2;
            ensure!(
                current == &tag,
                "Tag {} differs between config.toml and the Tag store",
                tag.name
            );
        } else if names.insert(tag.name.clone()) {
            out.push((None, crate::tag::TagStore::serialize(&tag)?, tag));
        }
    }
    Ok(out)
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
        let next = doc.iter().next().map(|(k, _)| k.to_owned());
        if let Some(key) = next {
            match doc.get_mut(&key) {
                Some(Item::Value(_)) => {
                    if let Some(mut k) = doc.as_table_mut().key_mut(&key) {
                        k.leaf_decor_mut().set_prefix(prefix)
                    }
                }
                Some(Item::Table(t)) => t.decor_mut().set_prefix(prefix),
                Some(Item::ArrayOfTables(ts)) => {
                    if let Some(t) = ts.get_mut(0) {
                        t.decor_mut().set_prefix(prefix)
                    }
                }
                _ => {}
            }
        } else {
            doc.as_table_mut().decor_mut().set_prefix(prefix)
        }
    }
    Ok(doc)
}
fn paths(view: &HomeView, prefix: &str) -> Vec<RelPath> {
    view.files()
        .keys()
        .filter(|p| p.as_path().parent() == Some(Path::new(prefix)))
        .cloned()
        .collect()
}

pub fn upgrade(view: &mut HomeView) -> Result<StepNotes> {
    let mut notes = StepNotes::default();
    let config_bytes = view.get("config.toml").map(<[u8]>::to_vec);
    let mut config = Config::default();
    let mut config_change = None;
    if let Some(bytes) = &config_bytes {
        let text = std::str::from_utf8(bytes)?;
        let mut doc = text.parse::<DocumentMut>()?;
        let schema = crate::schema::version(
            &doc,
            Path::new("config.toml"),
            "config",
            crate::schema::CONFIG,
            1,
        )?;
        let has = doc.get("tags").is_some();
        if schema == 1 {
            doc.remove("deploy");
        }
        let mut parse = doc.clone();
        parse["schema"] = toml_edit::value(schema as i64);
        config = toml::from_str(&parse.to_string())?;
        if schema < crate::schema::CONFIG || has {
            config_change = Some(config_without_tags(text)?.to_string().into_bytes());
            notes.config_migrated = true;
        }
    }
    let mut stored = Vec::new();
    for rel in paths(view, "tags") {
        let bytes = view.files().get(&rel).unwrap().clone();
        let (tag, doc) = parse_tag(rel.as_path(), &bytes)?;
        stored.push((rel, bytes, tag, doc));
    }
    let desired = merge_tags(stored.clone(), &config.tags)?;
    let allocation =
        crate::group_filename::allocate(desired.iter().map(|(_, _, t)| t.name.as_str()))?;
    for (old, _, _, _) in &stored {
        view.remove(old);
    }
    for (old, bytes, tag) in desired {
        let target = PathBuf::from("tags").join(format!("{}.toml", allocation[&tag.name]));
        let changed = old.as_ref().is_none_or(|path| path.as_path() != target)
            || old
                .as_ref()
                .and_then(|path| stored.iter().find(|entry| &entry.0 == path))
                .is_some_and(|x| x.1 != bytes);
        if changed {
            notes.migrated_tags.push(tag.name.clone());
        }
        view.insert(target, bytes)?;
    }
    // Reconstruct desired tags from the virtual tree, avoiding current-only store parsing.
    config.tags.clear();
    for rel in paths(view, "tags") {
        let (tag, _) = parse_tag(rel.as_path(), view.files().get(&rel).unwrap())?;
        config.tags.push(tag)
    }
    let preset_paths = paths(view, "presets");
    let mut planned = Vec::new();
    for rel in &preset_paths {
        let original = view.files().get(rel).unwrap();
        let text = std::str::from_utf8(original)?;
        let mut doc = text
            .parse::<DocumentMut>()
            .with_context(|| format!("invalid preset: {}", rel.as_path().display()))?;
        let schema =
            crate::schema::version(&doc, rel.as_path(), "preset", crate::schema::PRESET, 0)?;
        let legacy = doc.remove("tags");
        let had = legacy.is_some();
        let mut domain = doc.clone();
        domain.remove("schema");
        let preset: crate::preset::Preset = toml::from_str(&domain.to_string())?;
        let name = crate::group_filename::normalize_name(&preset.name)?;
        if let Some(legacy) = legacy {
            let names = legacy
                .as_array()
                .with_context(|| {
                    format!(
                        "legacy preset tags must be an array in {}",
                        rel.as_path().display()
                    )
                })?
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .map(str::to_owned)
                        .context("legacy preset tag must be a string")
                })
                .collect::<Result<Vec<_>>>()?;
            let expanded = crate::preset::tag_members(&config, &names).with_context(|| {
                format!(
                    "cannot migrate {}; original presets were left unchanged",
                    rel.as_path().display()
                )
            })?;
            if doc.get("skills").is_none() {
                doc["skills"] = toml_edit::value(toml_edit::Array::new())
            }
            let skills = doc
                .get_mut("skills")
                .and_then(Item::as_array_mut)
                .context("preset skills must be an array")?;
            let mut seen = BTreeSet::new();
            skills.retain(|value| value.as_str().is_some_and(|s| seen.insert(s.to_owned())));
            for skill in expanded {
                if seen.insert(skill.clone()) {
                    skills.push(skill)
                }
            }
        }
        if schema < crate::schema::PRESET {
            crate::schema::set(&mut doc, crate::schema::PRESET)
        }
        planned.push((rel.clone(), doc.to_string().into_bytes(), name, had, schema));
    }
    let alloc = crate::group_filename::allocate(planned.iter().map(|entry| entry.2.as_str()))?;
    for rel in &preset_paths {
        view.remove(rel);
    }
    for (old, bytes, name, had, schema) in planned {
        let target = PathBuf::from("presets").join(format!("{}.toml", alloc[&name]));
        if schema < crate::schema::PRESET || had || old.as_path() != target {
            notes.migrated_names.push(name);
        }
        view.insert(target, bytes)?;
    }
    for rel in paths(view, "repos") {
        let bytes = view.files().get(&rel).unwrap().clone();
        let text = std::str::from_utf8(&bytes)?;
        let mut doc = text
            .parse::<DocumentMut>()
            .with_context(|| format!("invalid repository metadata: {}", rel.as_path().display()))?;
        let schema = crate::schema::version(
            &doc,
            rel.as_path(),
            "repository metadata",
            crate::schema::REPOSITORY,
            0,
        )?;
        crate::meta::validate_repository_document(rel.as_path(), &doc)?;
        if schema < crate::schema::REPOSITORY {
            crate::schema::set(&mut doc, crate::schema::REPOSITORY);
            view.insert(rel.as_path(), doc.to_string().into_bytes())?;
            notes.migrated_repositories.push(
                rel.as_path()
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }
    if let Some(bytes) = config_change {
        view.insert("config.toml", bytes)?;
    }
    notes.migrated_tags.sort();
    notes.migrated_tags.dedup();
    Ok(notes)
}
