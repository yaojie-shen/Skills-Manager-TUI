//! Named skill groups stored independently under `<root>/.skills-meta/tags`.

use crate::{group_filename, meta::MetaStore, paths::meta_dir};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};
use toml_edit::{DocumentMut, value};

pub const TAG_DIR: &str = "tags";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tag {
    #[serde(default)]
    pub skills: Vec<String>,
    pub name: String,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

impl Tag {
    fn normalize(&mut self) -> Result<()> {
        self.name = group_filename::normalize_name(&self.name)?;
        self.skills.sort();
        self.skills.dedup();
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Entry {
    pub path: PathBuf,
    pub tag: Tag,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct TagStore {
    pub dir: PathBuf,
    root: PathBuf,
}

impl TagStore {
    pub fn new(root: &Path) -> Self {
        Self {
            dir: meta_dir(root).join(TAG_DIR),
            root: root.to_path_buf(),
        }
    }

    pub(crate) fn path_for_stem(&self, stem: &str) -> PathBuf {
        self.dir.join(format!("{stem}.toml"))
    }

    pub(crate) fn serialize(tag: &Tag) -> Result<Vec<u8>> {
        let mut doc = toml::to_string_pretty(tag)?.parse::<DocumentMut>()?;
        crate::schema::set(&mut doc, crate::schema::TAG);
        Ok(doc.to_string().into_bytes())
    }

    pub(crate) fn entries(&self) -> Result<Vec<Entry>> {
        let files = crate::file_set::read_store(
            &meta_dir(&self.root),
            TAG_DIR,
            "Tag",
            &crate::util::is_store_document_name,
            &mut |_, _, _| Ok(()),
        )?;
        self.parse_entries(files)
    }

    /// Validate Tag documents produced by a migration, which must also use
    /// the canonical filenames for the whole set.
    pub(crate) fn validate_documents(&self, files: Vec<(PathBuf, Vec<u8>)>) -> Result<()> {
        let entries = self.parse_entries(files)?;
        let allocated =
            group_filename::allocate(entries.iter().map(|entry| entry.tag.name.as_str()))?;
        for entry in &entries {
            let expected = self.path_for_stem(&allocated[&entry.tag.name]);
            ensure!(
                entry.path == expected,
                "noncanonical Tag filename {} (expected {})",
                entry.path.display(),
                expected.display()
            );
        }
        Ok(())
    }

    /// Parse and validate Tag documents already read from this store's
    /// directory. Filenames need not be canonical: root sync can merge
    /// definitions created on different machines, and the next write to the
    /// store renames them.
    pub(crate) fn parse_entries(&self, files: Vec<(PathBuf, Vec<u8>)>) -> Result<Vec<Entry>> {
        let mut out = Vec::new();
        let mut names: BTreeMap<String, PathBuf> = BTreeMap::new();
        for (path, bytes) in files {
            let text = std::str::from_utf8(&bytes)
                .with_context(|| format!("invalid UTF-8 in {}", path.display()))?;
            let mut doc = text
                .parse::<DocumentMut>()
                .with_context(|| format!("invalid Tag: {}", path.display()))?;
            let schema = crate::schema::version(&doc, &path, "Tag", crate::schema::TAG, 0)?;
            if schema < crate::schema::TAG {
                bail!(
                    "{} uses legacy Tag schema {schema}, but this Skills Manager expects Tag schema {}; {}",
                    path.display(),
                    crate::schema::TAG,
                    crate::schema::legacy_advice()
                );
            }
            doc.remove("schema");
            let mut tag: Tag = toml::from_str(&doc.to_string())
                .with_context(|| format!("invalid Tag: {}", path.display()))?;
            tag.normalize()?;
            if let Some(first) = names.insert(tag.name.clone(), path.clone()) {
                bail!(
                    "duplicate Tag name {} in {} and {}; merge or rename one of them",
                    tag.name,
                    first.display(),
                    path.display()
                );
            }
            out.push(Entry { path, tag, bytes });
        }
        Ok(out)
    }

    pub(crate) fn tags(entries: &[Entry]) -> Vec<Tag> {
        let mut tags: Vec<_> = entries.iter().map(|entry| entry.tag.clone()).collect();
        tags.sort_by(|a, b| a.name.cmp(&b.name));
        tags
    }

    pub fn list(&self) -> Result<Vec<Tag>> {
        Ok(Self::tags(&self.entries()?))
    }

    pub fn load(&self, name: &str) -> Result<Option<Tag>> {
        let name = group_filename::normalize_name(name)?;
        Ok(self
            .entries()?
            .into_iter()
            .find(|entry| entry.tag.name == name)
            .map(|entry| entry.tag))
    }

    pub(crate) fn choose_entries(
        &self,
        before: &[Entry],
        mut tags: Vec<Tag>,
    ) -> Result<Vec<Entry>> {
        for tag in &mut tags {
            tag.normalize()?;
        }
        tags.sort_by(|a, b| a.name.cmp(&b.name));
        ensure!(
            tags.windows(2).all(|pair| pair[0].name != pair[1].name),
            "duplicate Tag name"
        );
        let allocation = group_filename::allocate(tags.iter().map(|tag| tag.name.as_str()))?;
        let existing: BTreeMap<_, _> = before
            .iter()
            .map(|entry| (entry.tag.name.clone(), entry))
            .collect();
        let mut desired = Vec::new();
        for tag in tags {
            let path = self.path_for_stem(&allocation[&tag.name]);
            let bytes = existing
                .get(&tag.name)
                .filter(|old| old.tag == tag)
                .map(|old| old.bytes.clone())
                .unwrap_or(Self::serialize(&tag)?);
            desired.push(Entry { path, tag, bytes });
        }
        Ok(desired)
    }

    pub(crate) fn apply_entries(&self, before: &[Entry], desired: &[Entry]) -> Result<()> {
        for entry in before {
            ensure!(
                std::fs::read(&entry.path)? == entry.bytes,
                "Tag {} changed during this operation",
                entry.tag.name
            );
        }
        std::fs::create_dir_all(&self.dir)?;
        let sources: BTreeSet<_> = before.iter().map(|entry| entry.path.clone()).collect();
        let physical: Vec<_> = std::fs::read_dir(&self.dir)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<std::io::Result<_>>()?;
        for entry in desired {
            crate::util::reject_ignored_occupant(&physical, &sources, &entry.path, "Tag")?;
            let destination_key = entry
                .path
                .file_name()
                .and_then(|name| name.to_str())
                .map(group_filename::collision_key);
            let external_collision = physical.iter().any(|path| {
                !sources.contains(path)
                    && path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .map(group_filename::collision_key)
                        == destination_key
            });
            let source_collision = before.iter().any(|source| {
                source
                    .path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(group_filename::collision_key)
                    == destination_key
            });
            ensure!(
                !external_collision
                    && (sources.contains(&entry.path) || source_collision || !entry.path.exists()),
                "Tag destination {} appeared during this operation",
                entry.path.display()
            );
        }
        crate::file_set::publish_changes(
            &crate::file_set::Target::store(&meta_dir(&self.root), TAG_DIR),
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
        .context("publishing Tag files")
    }

    pub fn edit(&self, edit: impl FnOnce(&mut Vec<Tag>)) -> Result<()> {
        let _lock = MetaStore::new(&self.root).lock()?;
        let before = self.entries()?;
        let mut tags = Self::tags(&before);
        edit(&mut tags);
        let desired = self.choose_entries(&before, tags)?;
        self.apply_entries(&before, &desired)
    }

    pub fn save(&self, tag: &Tag) -> Result<()> {
        let mut tag = tag.clone();
        tag.normalize()?;
        self.edit(
            |tags| match tags.iter_mut().find(|current| current.name == tag.name) {
                Some(current) => *current = tag,
                None => tags.push(tag),
            },
        )
    }

    pub fn remove(&self, name: &str) -> Result<()> {
        let name = group_filename::normalize_name(name)?;
        let mut found = false;
        self.edit(|tags| {
            tags.retain(|tag| {
                found |= tag.name == name;
                tag.name != name
            })
        })?;
        if !found {
            bail!("no such Tag: {name}");
        }
        Ok(())
    }

    pub fn rename(&self, old: &str, new: &str) -> Result<()> {
        let old = group_filename::normalize_name(old)?;
        let new = group_filename::normalize_name(new)?;
        if old == new {
            return Ok(());
        }
        let _lock = MetaStore::new(&self.root).lock()?;
        let before = self.entries()?;
        let Some(source) = before.iter().find(|entry| entry.tag.name == old) else {
            return Ok(());
        };
        let mut tags = Self::tags(&before);
        if let Some(target) = tags.iter_mut().find(|tag| tag.name == new) {
            target.skills.extend(source.tag.skills.clone());
            target.skills.sort();
            target.skills.dedup();
            if target.color.is_none() {
                target.color = source.tag.color.clone();
            }
            if target.description.is_none() {
                target.description = source.tag.description.clone();
            }
            tags.retain(|tag| tag.name != old);
        } else {
            tags.iter_mut().find(|tag| tag.name == old).unwrap().name = new.clone();
        }
        let mut desired = self.choose_entries(&before, tags)?;
        if let Some(renamed) = desired.iter_mut().find(|entry| entry.tag.name == new)
            && before.iter().all(|entry| entry.tag.name != new)
        {
            let mut doc = std::str::from_utf8(&source.bytes)?.parse::<DocumentMut>()?;
            doc["name"] = value(&new);
            renamed.bytes = doc.to_string().into_bytes();
        }
        self.apply_entries(&before, &desired)
    }

    pub fn remove_skill(&self, old: &str, new: Option<&str>) -> Result<()> {
        self.edit(|tags| {
            for tag in tags {
                for skill in &mut tag.skills {
                    if skill == old
                        && let Some(new) = new
                    {
                        *skill = new.to_owned();
                    }
                }
                if new.is_none() {
                    tag.skills.retain(|skill| skill != old);
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::DownloadDir;

    #[test]
    fn stores_readable_names_and_renames_files_preserving_comments() {
        let temp = DownloadDir::new("tag-store").unwrap();
        let store = TagStore::new(temp.path());
        store
            .save(&Tag {
                name: "工作 / Rust".into(),
                skills: vec!["b".into(), "a".into()],
                color: None,
                description: None,
            })
            .unwrap();
        let old = store.dir.join("工作 - Rust.toml");
        assert!(old.exists());
        let mut text = std::fs::read_to_string(&old).unwrap();
        text.insert_str(0, "# keep me\n");
        std::fs::write(&old, text).unwrap();
        store.rename("工作 / Rust", "工作 / 系统").unwrap();
        let new = store.dir.join("工作 - 系统.toml");
        assert!(new.exists());
        assert!(
            std::fs::read_to_string(new)
                .unwrap()
                .starts_with("# keep me")
        );
    }

    #[test]
    fn same_name_rename_preserves_definition_bytes() {
        let temp = DownloadDir::new("tag-rename-same").unwrap();
        let store = TagStore::new(temp.path());
        store
            .save(&Tag {
                name: "work".into(),
                skills: vec!["demo".into()],
                color: None,
                description: None,
            })
            .unwrap();
        let path = std::fs::read_dir(&store.dir)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let bytes = std::fs::read(&path).unwrap();
        store.rename("work", " work ").unwrap();
        assert_eq!(std::fs::read(path).unwrap(), bytes);
        assert_eq!(store.list().unwrap()[0].name, "work");
    }

    const STRAYS: [&str; 4] = [".DS_Store", "work.toml~", "README", ".work.toml.swp"];

    #[test]
    fn stray_store_files_are_ignored_and_left_alone() {
        let temp = DownloadDir::new("tag-strays").unwrap();
        let store = TagStore::new(temp.path());
        std::fs::create_dir_all(store.dir.join("notes")).unwrap();
        for name in STRAYS {
            std::fs::write(store.dir.join(name), b"stray").unwrap();
        }
        store
            .save(&Tag {
                name: "work".into(),
                skills: vec!["one".into()],
                color: None,
                description: None,
            })
            .unwrap();
        assert_eq!(store.list().unwrap().len(), 1);
        assert!(store.load("work").unwrap().is_some());
        store.rename("work", "play").unwrap();
        store.remove("play").unwrap();
        assert!(store.list().unwrap().is_empty());
        for name in STRAYS {
            assert_eq!(std::fs::read(store.dir.join(name)).unwrap(), b"stray");
        }
        assert!(store.dir.join("notes").is_dir());
    }

    #[test]
    fn an_ignored_file_holding_the_destination_name_is_named() {
        let temp = DownloadDir::new("tag-ignored-occupant").unwrap();
        let store = TagStore::new(temp.path());
        std::fs::create_dir_all(&store.dir).unwrap();
        let occupant = store.dir.join("Zzz.TOML");
        std::fs::write(&occupant, b"foreign").unwrap();
        assert!(store.list().unwrap().is_empty());
        let error = format!(
            "{:#}",
            store
                .save(&Tag {
                    name: "zzz".into(),
                    skills: vec![],
                    color: None,
                    description: None,
                })
                .unwrap_err()
        );
        assert!(
            error.contains(&format!("{} already uses that name", occupant.display())),
            "{error}"
        );
        assert!(error.contains("rename or remove"), "{error}");
        assert_eq!(std::fs::read(&occupant).unwrap(), b"foreign");
        // Compare exact names: on a case-insensitive filesystem `zzz.toml`
        // resolves to `Zzz.TOML`, so an existence check cannot tell them apart.
        let names: Vec<_> = std::fs::read_dir(&store.dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, ["Zzz.TOML"]);
    }

    #[test]
    fn toml_symlink_and_directory_entries_are_still_rejected() {
        let temp = DownloadDir::new("tag-invalid-entries").unwrap();
        let store = TagStore::new(temp.path());
        std::fs::create_dir_all(&store.dir).unwrap();
        let target = temp.path().join("elsewhere.toml");
        std::fs::write(&target, "schema = 1\nname = 'linked'\n").unwrap();
        std::os::unix::fs::symlink(&target, store.dir.join("linked.toml")).unwrap();
        let error = store.list().unwrap_err();
        assert!(format!("{error:#}").contains("invalid Tag store entry"));
        std::fs::remove_file(store.dir.join("linked.toml")).unwrap();
        std::fs::create_dir(store.dir.join("folder.toml")).unwrap();
        let error = store.list().unwrap_err();
        assert!(format!("{error:#}").contains("invalid Tag store entry"));
    }

    fn file_names(dir: &Path) -> BTreeSet<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn merged_noncanonical_filenames_open_and_the_next_write_canonicalizes() {
        // Two machines each created one of these; root sync merged both files.
        for (first, second) in [("Work", "work"), ("A/B", "A-B")] {
            let temp = DownloadDir::new("tag-merged-noncanonical").unwrap();
            let store = TagStore::new(temp.path());
            std::fs::create_dir_all(&store.dir).unwrap();
            for (file, name) in [("one.toml", first), ("two.toml", second)] {
                std::fs::write(
                    store.dir.join(file),
                    format!("schema = 1\nname = '{name}'\nskills = ['{file}']\n"),
                )
                .unwrap();
            }
            let names: Vec<_> = store.list().unwrap().into_iter().map(|t| t.name).collect();
            assert_eq!(names.len(), 2);
            store
                .save(&Tag {
                    name: "extra".into(),
                    skills: vec![],
                    color: None,
                    description: None,
                })
                .unwrap();
            let entries = store.entries().unwrap();
            let allocation =
                group_filename::allocate(entries.iter().map(|e| e.tag.name.as_str())).unwrap();
            for entry in &entries {
                assert_eq!(
                    entry.path,
                    store.path_for_stem(&allocation[&entry.tag.name])
                );
            }
            assert_eq!(file_names(&store.dir).len(), 3);
            assert_eq!(store.load(first).unwrap().unwrap().skills, ["one.toml"]);
            assert_eq!(store.load(second).unwrap().unwrap().skills, ["two.toml"]);
        }
    }

    #[test]
    fn duplicate_names_name_both_files() {
        let temp = DownloadDir::new("tag-duplicate-files").unwrap();
        let store = TagStore::new(temp.path());
        std::fs::create_dir_all(&store.dir).unwrap();
        for file in ["one.toml", "two.toml"] {
            std::fs::write(store.dir.join(file), "schema = 1\nname = 'same'\n").unwrap();
        }
        let error = format!("{:#}", store.list().unwrap_err());
        assert!(error.contains("duplicate Tag name same"), "{error}");
        assert!(
            error.contains("one.toml") && error.contains("two.toml"),
            "{error}"
        );
    }

    #[test]
    fn collision_group_all_get_digests() {
        let temp = DownloadDir::new("tag-collision").unwrap();
        let store = TagStore::new(temp.path());
        for name in ["A/B", "A-B"] {
            store
                .save(&Tag {
                    name: name.into(),
                    skills: vec![],
                    color: None,
                    description: None,
                })
                .unwrap();
        }
        let names: Vec<_> = std::fs::read_dir(&store.dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 2);
        assert!(names.iter().all(|name| name.starts_with("A-B--")));
    }
}
