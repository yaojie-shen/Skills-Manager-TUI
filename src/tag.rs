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
        let rd = match std::fs::read_dir(&self.dir) {
            Ok(rd) => rd,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut paths = Vec::new();
        for entry in rd {
            let entry = entry?;
            let ty = entry.file_type()?;
            crate::util::reject_interrupted_transaction(&entry.path(), &ty)?;
            if !crate::util::is_store_document_name(&entry.file_name()) {
                continue;
            }
            ensure!(
                ty.is_file() && !ty.is_symlink(),
                "invalid Tag store entry: {}",
                entry.path().display()
            );
            let path = entry.path();
            ensure!(
                path.file_name().and_then(|n| n.to_str()).is_some(),
                "Tag filename is not valid UTF-8: {}",
                path.display()
            );
            paths.push(path);
        }
        paths.sort();
        let mut out = Vec::new();
        let mut names = BTreeSet::new();
        for path in paths {
            let bytes =
                std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
            let text = std::str::from_utf8(&bytes)
                .with_context(|| format!("invalid UTF-8 in {}", path.display()))?;
            let mut doc = text
                .parse::<DocumentMut>()
                .with_context(|| format!("invalid Tag: {}", path.display()))?;
            let schema = crate::schema::version(&doc, &path, "Tag", crate::schema::TAG, 0)?;
            if schema < crate::schema::TAG {
                bail!(
                    "legacy Tag schema {schema} in {}; reopen the workspace to migrate it to schema {}",
                    path.display(),
                    crate::schema::TAG
                );
            }
            doc.remove("schema");
            let mut tag: Tag = toml::from_str(&doc.to_string())
                .with_context(|| format!("invalid Tag: {}", path.display()))?;
            tag.normalize()?;
            ensure!(
                names.insert(tag.name.clone()),
                "duplicate Tag name: {}",
                tag.name
            );
            out.push(Entry { path, tag, bytes });
        }
        let allocated = group_filename::allocate(out.iter().map(|entry| entry.tag.name.as_str()))?;
        for entry in &out {
            let expected = self.path_for_stem(&allocated[&entry.tag.name]);
            ensure!(
                entry.path == expected,
                "noncanonical Tag filename {}; reopen the workspace to migrate it to {}",
                entry.path.display(),
                expected.display()
            );
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
