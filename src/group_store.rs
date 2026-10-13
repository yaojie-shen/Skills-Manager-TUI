//! Shared storage for named skill groups, one TOML document per group.
//!
//! Reads tolerate non-canonical filenames produced by merges; the next write
//! canonicalizes the set. Unchanged documents keep their bytes, while changed
//! documents have only the group's declared fields patched.

use crate::{group_filename, meta::MetaStore, paths::meta_dir};
use anyhow::{Context, Result, bail, ensure};
use serde::{Serialize, de::DeserializeOwned};
use std::{
    collections::{BTreeMap, BTreeSet},
    marker::PhantomData,
    path::{Path, PathBuf},
};
use toml_edit::DocumentMut;

pub(crate) trait Group: Clone + PartialEq + Serialize + DeserializeOwned {
    const DIR: &'static str;
    const KIND: &'static str;
    const SCHEMA: u32;
    const FIELDS: &'static [&'static str];
    fn name(&self) -> &str;
    fn name_mut(&mut self) -> &mut String;
    fn skills_mut(&mut self) -> &mut Vec<String>;
    fn check_document(_doc: &DocumentMut, _path: &Path) -> Result<()> {
        Ok(())
    }
    fn missing_rename(_name: &str) -> Result<()> {
        Ok(())
    }
    fn rename_onto(target: &mut Self, _source: &Self) -> Result<()> {
        bail!("{} {} already exists", Self::KIND, target.name())
    }
    fn normalize(&mut self) -> Result<()> {
        *self.name_mut() = group_filename::normalize_name(self.name())?;
        self.skills_mut().sort();
        self.skills_mut().dedup();
        Ok(())
    }
}

struct Entry<G> {
    path: PathBuf,
    group: G,
    bytes: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct GroupStore<G> {
    pub dir: PathBuf,
    root: PathBuf,
    marker: PhantomData<fn() -> G>,
}

#[allow(private_bounds)]
impl<G: Group> GroupStore<G> {
    pub fn new(root: &Path) -> Self {
        Self {
            dir: meta_dir(root).join(G::DIR),
            root: root.to_path_buf(),
            marker: PhantomData,
        }
    }
    fn path_for_stem(&self, stem: &str) -> PathBuf {
        self.dir.join(format!("{stem}.toml"))
    }
    pub fn path(&self, name: &str) -> PathBuf {
        let normalized = group_filename::normalize_name(name).ok();
        if let (Some(name), Ok(entries)) = (normalized.as_ref(), self.entries())
            && let Some(entry) = entries.iter().find(|entry| entry.group.name() == name)
        {
            return entry.path.clone();
        }
        self.path_for_stem(&group_filename::safe_stem(name).unwrap_or_else(|_| "invalid".into()))
    }
    pub(crate) fn serialize(group: &G) -> Result<Vec<u8>> {
        let mut doc = toml::to_string_pretty(group)?.parse::<DocumentMut>()?;
        crate::schema::set(&mut doc, G::SCHEMA);
        Ok(doc.to_string().into_bytes())
    }
    fn entries(&self) -> Result<Vec<Entry<G>>> {
        let files = crate::file_set::read_store(
            &meta_dir(&self.root),
            G::DIR,
            G::KIND,
            &crate::util::is_store_document_name,
            &mut |_, _, _| Ok(()),
        )?;
        self.parse_entries(files)
    }
    fn parse_entries(&self, files: Vec<(PathBuf, Vec<u8>)>) -> Result<Vec<Entry<G>>> {
        let mut out = Vec::new();
        let mut names = BTreeMap::new();
        for (path, bytes) in files {
            let text = std::str::from_utf8(&bytes)
                .with_context(|| format!("invalid UTF-8 in {}", path.display()))?;
            let mut doc = text
                .parse::<DocumentMut>()
                .with_context(|| format!("invalid {}: {}", G::KIND, path.display()))?;
            crate::schema::require_current(&doc, &path, G::KIND, G::SCHEMA, 0)?;
            G::check_document(&doc, &path)?;
            doc.remove("schema");
            let mut group: G = toml::from_str(&doc.to_string())
                .with_context(|| format!("invalid {}: {}", G::KIND, path.display()))?;
            group.normalize()?;
            if let Some(first) = names.insert(group.name().to_owned(), path.clone()) {
                bail!(
                    "duplicate {} name {} in {} and {}; merge or rename one of them",
                    G::KIND,
                    group.name(),
                    first.display(),
                    path.display()
                );
            }
            out.push(Entry { path, group, bytes });
        }
        Ok(out)
    }
    pub(crate) fn validate_documents(&self, files: Vec<(PathBuf, Vec<u8>)>) -> Result<()> {
        let entries = self.parse_entries(files)?;
        let allocation = group_filename::allocate(entries.iter().map(|e| e.group.name()))?;
        for entry in entries {
            let expected = self.path_for_stem(&allocation[entry.group.name()]);
            ensure!(
                entry.path == expected,
                "noncanonical {} filename {} (expected {})",
                G::KIND,
                entry.path.display(),
                expected.display()
            );
        }
        Ok(())
    }
    fn values(entries: &[Entry<G>]) -> Vec<G> {
        let mut values: Vec<_> = entries.iter().map(|e| e.group.clone()).collect();
        values.sort_by(|a, b| a.name().cmp(b.name()));
        values
    }
    pub fn list(&self) -> Result<Vec<G>> {
        Ok(Self::values(&self.entries()?))
    }
    pub fn load(&self, name: &str) -> Result<Option<G>> {
        let name = group_filename::normalize_name(name)?;
        Ok(self
            .entries()?
            .into_iter()
            .find(|e| e.group.name() == name)
            .map(|e| e.group))
    }
    pub fn contains_name(&self, name: &str) -> Result<bool> {
        Ok(self.load(name)?.is_some())
    }
    pub fn edit(&self, edit: impl FnOnce(&mut Vec<G>)) -> Result<()> {
        let _lock = MetaStore::new(&self.root).lock()?;
        let before = self.entries()?;
        let mut values = Self::values(&before);
        edit(&mut values);
        if values == Self::values(&before) {
            return Ok(());
        }
        self.write(&before, values, None)
    }
    pub fn save(&self, group: &G) -> Result<()> {
        let mut group = group.clone();
        group.normalize()?;
        self.edit(
            |values| match values.iter_mut().find(|v| v.name() == group.name()) {
                Some(v) => *v = group,
                None => values.push(group),
            },
        )
    }
    pub fn remove(&self, name: &str) -> Result<()> {
        let name = group_filename::normalize_name(name)?;
        let mut found = false;
        self.edit(|v| {
            v.retain(|g| {
                found |= g.name() == name;
                g.name() != name
            })
        })?;
        ensure!(found, "no such {}: {name}", G::KIND);
        Ok(())
    }
    pub fn rename(&self, old: &str, new: &str) -> Result<()> {
        let old = group_filename::normalize_name(old)?;
        let new = group_filename::normalize_name(new)?;
        let _lock = MetaStore::new(&self.root).lock()?;
        let before = self.entries()?;
        let Some(source) = before
            .iter()
            .find(|e| e.group.name() == old)
            .map(|e| e.group.clone())
        else {
            G::missing_rename(&old)?;
            return Ok(());
        };
        if old == new {
            return Ok(());
        }
        let mut values = Self::values(&before);
        if let Some(target) = values.iter_mut().find(|g| g.name() == new) {
            G::rename_onto(target, &source)?;
            values.retain(|g| g.name() != old);
        } else {
            *values
                .iter_mut()
                .find(|g| g.name() == old)
                .unwrap()
                .name_mut() = new.clone();
        }
        self.write(&before, values, Some((&old, &new)))
    }
    fn write(
        &self,
        before: &[Entry<G>],
        mut desired: Vec<G>,
        renamed: Option<(&str, &str)>,
    ) -> Result<()> {
        for group in &mut desired {
            group.normalize()?;
        }
        desired.sort_by(|a, b| a.name().cmp(b.name()));
        ensure!(
            desired.windows(2).all(|w| w[0].name() != w[1].name()),
            "duplicate {} name",
            G::KIND
        );
        let allocation = group_filename::allocate(desired.iter().map(|g| g.name()))?;
        let previous: BTreeMap<_, _> = before.iter().map(|e| (e.group.name(), e)).collect();
        let mut entries = Vec::new();
        for group in desired {
            let path = self.path_for_stem(&allocation[group.name()]);
            let old = previous.get(group.name()).copied().or_else(|| {
                // A rename carries the source document's comments to its new name.
                renamed
                    .filter(|(_, new)| *new == group.name())
                    .and_then(|(old, _)| previous.get(old).copied())
            });
            let bytes = match old {
                Some(old) if old.group == group => old.bytes.clone(),
                Some(old) => {
                    let mut doc = std::str::from_utf8(&old.bytes)?.parse::<DocumentMut>()?;
                    let fresh =
                        std::str::from_utf8(&Self::serialize(&group)?)?.parse::<DocumentMut>()?;
                    for field in G::FIELDS {
                        if let Some(v) = fresh.get(field) {
                            doc[field] = v.clone();
                        } else {
                            doc.remove(field);
                        }
                    }
                    crate::schema::set(&mut doc, G::SCHEMA);
                    doc.to_string().into_bytes()
                }
                None => Self::serialize(&group)?,
            };
            entries.push(Entry { path, group, bytes });
        }
        for entry in before {
            ensure!(
                std::fs::read(&entry.path)? == entry.bytes,
                "{} {} changed during this operation",
                G::KIND,
                entry.group.name()
            );
        }
        std::fs::create_dir_all(&self.dir)?;
        let sources: BTreeSet<_> = before.iter().map(|e| e.path.clone()).collect();
        let physical: Vec<_> = std::fs::read_dir(&self.dir)?
            .map(|e| e.map(|e| e.path()))
            .collect::<std::io::Result<_>>()?;
        for entry in &entries {
            crate::util::reject_ignored_occupant(&physical, &sources, &entry.path, G::KIND)?;
            let key = entry
                .path
                .file_name()
                .and_then(|n| n.to_str())
                .map(group_filename::collision_key);
            let external = physical.iter().any(|p| {
                !sources.contains(p)
                    && p.file_name()
                        .and_then(|n| n.to_str())
                        .map(group_filename::collision_key)
                        == key
            });
            // A case-only rename may resolve to the source on case-insensitive filesystems.
            let source = before.iter().any(|s| {
                s.path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(group_filename::collision_key)
                    == key
            });
            ensure!(
                !external && (sources.contains(&entry.path) || source || !entry.path.exists()),
                "{} destination {} appeared during this operation",
                G::KIND,
                entry.path.display()
            );
        }
        crate::file_set::publish_changes(
            &crate::file_set::Target::store(&meta_dir(&self.root), G::DIR),
            &before
                .iter()
                .map(|e| crate::file_set::File {
                    path: &e.path,
                    bytes: &e.bytes,
                })
                .collect::<Vec<_>>(),
            &entries
                .iter()
                .map(|e| crate::file_set::File {
                    path: &e.path,
                    bytes: &e.bytes,
                })
                .collect::<Vec<_>>(),
        )
        .with_context(|| format!("publishing {} files", G::KIND))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ops::DownloadDir, preset::Preset, tag::Tag};

    fn store_suite<G: Group + std::fmt::Debug>(label: &str, make: fn(&str, Vec<String>) -> G) {
        let temp = DownloadDir::new(&format!("group-{label}-strays")).unwrap();
        let store = GroupStore::<G>::new(temp.path());
        std::fs::create_dir_all(store.dir.join("notes")).unwrap();
        let strays = [".DS_Store", "work.toml~", "README", ".work.toml.swp"];
        for name in strays {
            std::fs::write(store.dir.join(name), b"stray").unwrap();
        }
        store.save(&make("work", vec!["one".into()])).unwrap();
        assert_eq!(store.list().unwrap().len(), 1);
        store.rename("work", "play").unwrap();
        store.remove("play").unwrap();
        assert!(store.list().unwrap().is_empty());
        assert!(store.dir.join("notes").is_dir());
        for name in strays {
            assert_eq!(std::fs::read(store.dir.join(name)).unwrap(), b"stray");
        }

        let temp = DownloadDir::new(&format!("group-{label}-patch")).unwrap();
        let store = GroupStore::<G>::new(temp.path());
        store.save(&make("work", vec!["one".into()])).unwrap();
        let path = store.path("work");
        let text = format!("# keep me\n{}", std::fs::read_to_string(&path).unwrap());
        std::fs::write(&path, text).unwrap();
        store.save(&make("work", vec!["two".into()])).unwrap();
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .starts_with("# keep me")
        );

        let temp = DownloadDir::new(&format!("group-{label}-normalized")).unwrap();
        let store = GroupStore::<G>::new(temp.path());
        std::fs::create_dir_all(&store.dir).unwrap();
        std::fs::write(
            store.dir.join("z.toml"),
            "schema = 1\nname = 'z'\nskills = ['b', 'a', 'b']\n",
        )
        .unwrap();
        std::fs::write(
            store.dir.join("a.toml"),
            "schema = 1\nname = 'a'\nskills = ['d', 'c', 'c']\n",
        )
        .unwrap();
        let values = store.list().unwrap();
        assert_eq!(
            values.iter().map(|g| g.name()).collect::<Vec<_>>(),
            ["a", "z"]
        );
        assert_eq!(
            values
                .into_iter()
                .map(|mut g| std::mem::take(g.skills_mut()))
                .collect::<Vec<_>>(),
            [vec!["c", "d"], vec!["a", "b"]]
        );

        let temp = DownloadDir::new(&format!("group-{label}-external")).unwrap();
        let store = GroupStore::<G>::new(temp.path());
        store.save(&make("work", vec!["one".into()])).unwrap();
        let dir = store.dir.clone();
        let error = format!(
            "{:#}",
            store
                .edit(|values| {
                    std::fs::write(dir.join("WORK.toml"), b"foreign").unwrap();
                    *values.iter_mut().find(|g| g.name() == "work").unwrap() =
                        make("work", vec!["two".into()]);
                })
                .unwrap_err()
        );
        assert!(error.contains("appeared during this operation"), "{error}");

        for (first, second) in [("A/B", "A-B"), ("Work", "work")] {
            let temp = DownloadDir::new(&format!("group-{label}-merged")).unwrap();
            let store = GroupStore::<G>::new(temp.path());
            std::fs::create_dir_all(&store.dir).unwrap();
            for (file, name) in [("one.toml", first), ("two.toml", second)] {
                std::fs::write(
                    store.dir.join(file),
                    format!("schema = 1\nname = '{name}'\nskills = ['{file}']\n"),
                )
                .unwrap();
            }
            assert_eq!(store.list().unwrap().len(), 2);
            store.save(&make("extra", vec![])).unwrap();
            let entries = store.entries().unwrap();
            let allocation =
                group_filename::allocate(entries.iter().map(|e| e.group.name())).unwrap();
            assert!(
                entries
                    .iter()
                    .all(|e| e.path == store.path_for_stem(&allocation[e.group.name()]))
            );
            assert_eq!(std::fs::read_dir(&store.dir).unwrap().count(), 3);
        }

        let temp = DownloadDir::new(&format!("group-{label}-noop")).unwrap();
        let store = GroupStore::<G>::new(temp.path());
        std::fs::create_dir_all(&store.dir).unwrap();
        std::fs::write(
            store.dir.join("odd.toml"),
            "schema = 1\nname = 'daily'\nskills = []\n",
        )
        .unwrap();
        let before: Vec<_> = std::fs::read_dir(&store.dir)
            .unwrap()
            .map(|e| {
                let p = e.unwrap().path();
                (p.file_name().unwrap().to_owned(), std::fs::read(p).unwrap())
            })
            .collect();
        assert!(
            format!("{:#}", store.remove("missing").unwrap_err())
                .contains(&format!("no such {}", G::KIND))
        );
        let after: Vec<_> = std::fs::read_dir(&store.dir)
            .unwrap()
            .map(|e| {
                let p = e.unwrap().path();
                (p.file_name().unwrap().to_owned(), std::fs::read(p).unwrap())
            })
            .collect();
        assert_eq!(after, before);
        assert!(!meta_dir(temp.path()).join(".staging").exists());

        let temp = DownloadDir::new(&format!("group-{label}-duplicate")).unwrap();
        let store = GroupStore::<G>::new(temp.path());
        std::fs::create_dir_all(&store.dir).unwrap();
        for file in ["one.toml", "two.toml"] {
            std::fs::write(store.dir.join(file), "schema = 1\nname = 'same'\n").unwrap();
        }
        let error = format!("{:#}", store.list().unwrap_err());
        assert!(
            error.contains(&format!("duplicate {} name same", G::KIND))
                && error.contains("one.toml")
                && error.contains("two.toml"),
            "{error}"
        );

        let temp = DownloadDir::new(&format!("group-{label}-occupant")).unwrap();
        let store = GroupStore::<G>::new(temp.path());
        std::fs::create_dir_all(&store.dir).unwrap();
        let occupant = store.dir.join("Zzz.TOML");
        std::fs::write(&occupant, b"foreign").unwrap();
        let error = format!("{:#}", store.save(&make("zzz", vec![])).unwrap_err());
        assert!(
            error.contains(&format!("{} already uses that name", occupant.display())),
            "{error}"
        );
        assert!(error.contains("rename or remove"), "{error}");
        assert_eq!(std::fs::read(&occupant).unwrap(), b"foreign");
        assert_eq!(std::fs::read_dir(&store.dir).unwrap().count(), 1);

        let temp = DownloadDir::new(&format!("group-{label}-invalid")).unwrap();
        let store = GroupStore::<G>::new(temp.path());
        std::fs::create_dir_all(&store.dir).unwrap();
        let target = temp.path().join("elsewhere.toml");
        std::fs::write(&target, "schema = 1\nname = 'linked'\n").unwrap();
        std::os::unix::fs::symlink(&target, store.dir.join("linked.toml")).unwrap();
        assert!(
            format!("{:#}", store.list().unwrap_err())
                .contains(&format!("invalid {} store entry", G::KIND))
        );
        std::fs::remove_file(store.dir.join("linked.toml")).unwrap();
        std::fs::create_dir(store.dir.join("folder.toml")).unwrap();
        assert!(
            format!("{:#}", store.list().unwrap_err())
                .contains(&format!("invalid {} store entry", G::KIND))
        );
    }

    #[test]
    fn tag_store_contract() {
        store_suite::<Tag>("tag", |name, skills| Tag {
            name: name.into(),
            skills,
            color: None,
            description: None,
        });
    }

    #[test]
    fn preset_store_contract() {
        store_suite::<Preset>("preset", |name, skills| Preset {
            name: name.into(),
            skills,
            ..Preset::default()
        });
    }
}
