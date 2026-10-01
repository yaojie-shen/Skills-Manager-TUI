//! Named skill groups stored independently under `<root>/.skills-meta/tags`.

use crate::{meta::MetaStore, paths::meta_dir, util::write_atomic};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

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
    fn normalize(&mut self) {
        self.skills.sort();
        self.skills.dedup();
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Entry {
    pub id: String,
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

    pub(crate) fn path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.toml"))
    }

    fn default_id(name: &str) -> String {
        let digest = Sha256::digest(name.as_bytes());
        format!(
            "tag-{}",
            digest[..12]
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        )
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
            ensure!(
                ty.is_file() && !ty.is_symlink(),
                "invalid Tag store entry: {}",
                entry.path().display()
            );
            let path = entry.path();
            ensure!(
                path.extension()
                    .is_some_and(|extension| extension == "toml"),
                "invalid Tag store entry: {}",
                path.display()
            );
            paths.push(path);
        }
        paths.sort();
        let mut out = Vec::new();
        let mut names = BTreeSet::new();
        for path in paths {
            let id = path
                .file_stem()
                .context("Tag filename missing")?
                .to_string_lossy()
                .into_owned();
            ensure!(
                crate::util::valid_skill_key(&id),
                "invalid Tag storage id: {id}"
            );
            let bytes =
                std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
            let text = std::str::from_utf8(&bytes)
                .with_context(|| format!("invalid UTF-8 in {}", path.display()))?;
            let mut tag: Tag =
                toml::from_str(text).with_context(|| format!("invalid Tag: {}", path.display()))?;
            ensure!(
                !tag.name.trim().is_empty(),
                "empty Tag name in {}",
                path.display()
            );
            tag.normalize();
            ensure!(
                names.insert(tag.name.clone()),
                "duplicate Tag name: {}",
                tag.name
            );
            out.push(Entry { id, tag, bytes });
        }
        Ok(out)
    }

    pub(crate) fn tags(entries: &[Entry]) -> Vec<Tag> {
        let mut tags: Vec<_> = entries.iter().map(|entry| entry.tag.clone()).collect();
        tags.sort_by(|left, right| left.name.cmp(&right.name));
        tags
    }

    pub fn list(&self) -> Result<Vec<Tag>> {
        Ok(Self::tags(&self.entries()?))
    }

    pub fn load(&self, name: &str) -> Result<Option<Tag>> {
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
            tag.name = tag.name.trim().to_owned();
            ensure!(!tag.name.is_empty(), "Tag name is empty");
            tag.normalize();
        }
        tags.sort_by(|left, right| left.name.cmp(&right.name));
        ensure!(
            tags.windows(2).all(|pair| pair[0].name != pair[1].name),
            "duplicate Tag name"
        );
        let existing: BTreeMap<_, _> = before
            .iter()
            .map(|entry| (entry.tag.name.clone(), entry))
            .collect();
        let by_tag: Vec<_> = before.iter().collect();
        let mut used = BTreeSet::new();
        let mut desired = Vec::new();
        for tag in tags {
            let prior = existing.get(&tag.name).copied().or_else(|| {
                // A rename preserves storage identity by matching the one old
                // definition whose non-name content is unchanged.
                let candidates: Vec<_> = by_tag
                    .iter()
                    .copied()
                    .filter(|entry| {
                        !used.contains(&entry.id)
                            && entry.tag.skills == tag.skills
                            && entry.tag.color == tag.color
                            && entry.tag.description == tag.description
                    })
                    .collect();
                if candidates.len() == 1 {
                    Some(candidates[0])
                } else {
                    None
                }
            });
            let base = prior
                .map(|entry| entry.id.clone())
                .unwrap_or_else(|| Self::default_id(&tag.name));
            let mut id = base.clone();
            let mut suffix = 2;
            while !used.insert(id.clone()) || (prior.is_none() && self.path(&id).exists()) {
                id = format!("{base}-{suffix}");
                suffix += 1;
            }
            let bytes = if prior.is_some_and(|entry| entry.tag == tag) {
                prior.unwrap().bytes.clone()
            } else {
                toml::to_string_pretty(&tag)?.into_bytes()
            };
            desired.push(Entry { id, tag, bytes });
        }
        Ok(desired)
    }

    pub(crate) fn apply_entries(&self, before: &[Entry], desired: &[Entry]) -> Result<()> {
        // Revalidate every source before publishing any change.
        for entry in before {
            ensure!(
                std::fs::read(self.path(&entry.id))? == entry.bytes,
                "Tag {} changed during this operation",
                entry.tag.name
            );
        }
        std::fs::create_dir_all(&self.dir)?;
        let before_by_id: BTreeMap<_, _> = before
            .iter()
            .map(|entry| (entry.id.as_str(), entry))
            .collect();
        let mut written: Vec<&Entry> = Vec::new();
        for entry in desired {
            if before_by_id
                .get(entry.id.as_str())
                .is_some_and(|old| old.bytes == entry.bytes)
            {
                continue;
            }
            let path = self.path(&entry.id);
            if !before_by_id.contains_key(entry.id.as_str()) {
                ensure!(
                    !path.exists(),
                    "Tag destination {} appeared during this operation",
                    path.display()
                );
            }
            if let Err(error) = write_atomic(&path, &entry.bytes) {
                for applied in written.into_iter().rev() {
                    if let Some(old) = before_by_id.get(applied.id.as_str()) {
                        let _ = write_atomic(&self.path(&old.id), &old.bytes);
                    } else {
                        let _ = std::fs::remove_file(self.path(&applied.id));
                    }
                }
                return Err(error);
            }
            written.push(entry);
        }
        let keep: BTreeSet<_> = desired.iter().map(|entry| entry.id.as_str()).collect();
        for entry in before {
            if !keep.contains(entry.id.as_str()) {
                std::fs::remove_file(self.path(&entry.id))?;
            }
        }
        Ok(())
    }

    /// Update only changed definitions; untouched TOML bytes and comments remain exact.
    pub fn edit(&self, edit: impl FnOnce(&mut Vec<Tag>)) -> Result<()> {
        let _lock = MetaStore::new(&self.root).lock()?;
        let before = self.entries()?;
        let mut tags = Self::tags(&before);
        edit(&mut tags);
        let desired = self.choose_entries(&before, tags)?;
        self.apply_entries(&before, &desired)
    }

    pub fn save(&self, tag: &Tag) -> Result<()> {
        let tag = tag.clone();
        self.edit(
            |tags| match tags.iter_mut().find(|current| current.name == tag.name) {
                Some(current) => *current = tag,
                None => tags.push(tag),
            },
        )
    }

    pub fn remove(&self, name: &str) -> Result<()> {
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

    /// Move a definition. If the target exists it wins unchanged, matching the
    /// old config-entry history behavior.
    pub fn rename(&self, old: &str, new: &str) -> Result<()> {
        let new = new.trim();
        ensure!(!new.is_empty(), "new Tag name is empty");
        if old == new {
            return Ok(());
        }
        let _lock = MetaStore::new(&self.root).lock()?;
        let before = self.entries()?;
        let Some(source) = before.iter().find(|entry| entry.tag.name == old) else {
            return Ok(());
        };
        if before.iter().any(|entry| entry.tag.name == new) {
            let desired: Vec<_> = before
                .iter()
                .filter(|entry| entry.id != source.id)
                .cloned()
                .collect();
            return self.apply_entries(&before, &desired);
        }
        let mut desired = before.clone();
        let renamed = desired
            .iter_mut()
            .find(|entry| entry.id == source.id)
            .context("Tag vanished during rename planning")?;
        renamed.tag.name = new.to_owned();
        renamed.bytes = toml::to_string_pretty(&renamed.tag)?.into_bytes();
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
    fn stores_unicode_names_under_safe_stable_ids_and_preserves_comments() {
        let temp = DownloadDir::new("tag-store").unwrap();
        let store = TagStore::new(temp.path());
        store
            .save(&Tag {
                name: "工作 / Rust".into(),
                skills: vec!["b".into(), "a".into(), "a".into()],
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
        assert!(!path.to_string_lossy().contains("工作"));
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.insert_str(0, "# keep me\n");
        std::fs::write(&path, &text).unwrap();
        store.edit(|_| {}).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        store.rename("工作 / Rust", "工作 / 系统").unwrap();
        assert_eq!(
            std::fs::read_dir(&store.dir)
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .path(),
            path
        );
        assert_eq!(store.list().unwrap()[0].skills, ["a", "b"]);
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

    #[test]
    fn existing_rename_target_wins_unchanged() {
        let temp = DownloadDir::new("tag-rename-target").unwrap();
        let store = TagStore::new(temp.path());
        store
            .save(&Tag {
                name: "old".into(),
                skills: vec!["a".into()],
                color: Some("blue".into()),
                description: None,
            })
            .unwrap();
        store
            .save(&Tag {
                name: "target".into(),
                skills: vec!["b".into()],
                color: Some("red".into()),
                description: None,
            })
            .unwrap();
        store.rename("old", "target").unwrap();
        assert_eq!(
            store.list().unwrap(),
            [Tag {
                name: "target".into(),
                skills: vec!["b".into()],
                color: Some("red".into()),
                description: None
            }]
        );
    }
}
