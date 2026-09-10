//! Local metadata in local.toml; repository identity and skills in repos/<alias>.toml.
//!
//! Reading uses serde; writing goes through `toml_edit` so that comments and
//! key order written by hand survive round trips.

use crate::paths::meta_dir;
use crate::util::write_atomic;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use toml_edit::{DocumentMut, Item, Table, value};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SkillMeta {
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub note: Option<String>,
    /// Declared name captured at installation; directory aliases are independent.
    #[serde(default)]
    pub installed_name: Option<String>,
    #[serde(default)]
    pub source: Option<Source>,
    #[serde(default)]
    pub baseline: Option<Baseline>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Source {
    Git {
        url: String,
        #[serde(default)]
        subpath: Option<String>,
        #[serde(default)]
        branch: Option<String>,
        #[serde(default)]
        revision: Option<String>,
    },
    Local {
        #[serde(default)]
        path: Option<String>,
    },
}

impl Source {
    pub fn kind(&self) -> &'static str {
        match self {
            Source::Git { .. } => "git",
            Source::Local { .. } => "local",
        }
    }
    pub fn summary(&self) -> String {
        match self {
            Source::Git {
                url,
                subpath,
                branch,
                revision,
            } => {
                let mut s = url.clone();
                if let Some(p) = subpath
                    && !p.is_empty()
                {
                    s.push('/');
                    s.push_str(p);
                }
                if let Some(b) = branch {
                    s.push('@');
                    s.push_str(b);
                }
                if let Some(r) = revision {
                    s.push_str(&format!(" ({})", short_rev(r)));
                }
                s
            }
            Source::Local { path } => match path {
                Some(p) => format!("local {p}"),
                None => "local".into(),
            },
        }
    }
}

pub fn short_rev(r: &str) -> &str {
    if r.len() > 12 { &r[..12] } else { r }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Baseline {
    pub hash: String,
    #[serde(default = "crate::hash::default_algo")]
    pub hash_algo: u32,
}

/// Access to the metadata directory.
#[derive(Debug, Clone)]
pub struct MetaStore {
    pub dir: PathBuf,
}

impl MetaStore {
    pub fn new(root: &Path) -> Self {
        Self {
            dir: meta_dir(root),
        }
    }

    pub fn path(&self, key: &str) -> PathBuf {
        match crate::repository::alias_of(key) {
            Some(alias) => self.dir.join("repos").join(format!("{alias}.toml")),
            None => self.dir.join("local.toml"),
        }
    }
    fn entry(key: &str) -> &str {
        if crate::repository::alias_of(key).is_some() {
            key.rsplit('/').next().unwrap()
        } else {
            key
        }
    }
    pub(crate) fn lock(&self) -> Result<std::fs::File> {
        std::fs::create_dir_all(&self.dir)?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.dir.join(".metadata.lock"))?;
        file.lock()?;
        Ok(file)
    }
    pub(crate) fn read(path: &Path) -> Result<DocumentMut> {
        match std::fs::read_to_string(path) {
            Ok(text) => text
                .parse()
                .with_context(|| format!("invalid metadata: {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(DocumentMut::new()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }
    pub fn exists(&self, key: &str) -> bool {
        self.load(key).map(|m| m.is_some()).unwrap_or(true)
    }
    pub fn load(&self, key: &str) -> Result<Option<SkillMeta>> {
        let doc = Self::read(&self.path(key))?;
        let document: toml::Value = toml::from_str(&doc.to_string())?;
        let Some(mut value) = document
            .get("skills")
            .and_then(|s| s.get(Self::entry(key)))
            .cloned()
        else {
            return Ok(None);
        };
        if let Some(source) = value.get_mut("source").and_then(toml::Value::as_table_mut)
            && source.get("type").and_then(toml::Value::as_str) == Some("git")
            && crate::repository::alias_of(key).is_some()
        {
            source.insert(
                "url".into(),
                doc.get("url")
                    .and_then(Item::as_str)
                    .context("repository URL missing")?
                    .into(),
            );
            if let Some(branch) = doc.get("branch").and_then(Item::as_str) {
                source.insert("branch".into(), branch.into());
            }
        }
        Ok(Some(value.try_into()?))
    }
    pub fn list_keys(&self) -> Result<Vec<String>> {
        let mut files = vec![(self.dir.join("local.toml"), String::new())];
        let repos = self.dir.join("repos");
        if repos.is_dir() {
            for entry in std::fs::read_dir(repos)? {
                let path = entry?.path();
                if path.is_file() && path.extension().is_some_and(|e| e == "toml") {
                    let alias = path.file_stem().unwrap().to_string_lossy();
                    anyhow::ensure!(
                        crate::util::valid_skill_key(&alias),
                        "invalid repository alias"
                    );
                    files.push((path.clone(), format!("repos/{alias}/")));
                }
            }
        }
        let mut keys = Vec::new();
        for (path, prefix) in files {
            let doc = Self::read(&path)?;
            if let Some(skills) = doc.get("skills") {
                let skills = skills.as_table().context("skills must be a table")?;
                for (name, _) in skills {
                    let key = format!("{prefix}{name}");
                    anyhow::ensure!(
                        crate::repository::valid_id(&key),
                        "invalid skill identity: {key}"
                    );
                    keys.push(key);
                }
            }
        }
        keys.sort();
        Ok(keys)
    }
    fn put(doc: &mut DocumentMut, key: &str, meta: &SkillMeta) -> Result<()> {
        let mut item = toml::to_string(meta)?
            .parse::<DocumentMut>()?
            .as_table()
            .clone();
        if let Some(alias) = crate::repository::alias_of(key)
            && let Some(Source::Git { url, branch, .. }) = &meta.source
        {
            anyhow::ensure!(
                doc.get("url")
                    .and_then(Item::as_str)
                    .is_none_or(|u| u == url),
                "repository URL differs from skill source"
            );
            anyhow::ensure!(
                doc.get("branch")
                    .and_then(Item::as_str)
                    .is_none_or(|b| Some(b) == branch.as_deref()),
                "repository branch differs from skill source"
            );
            doc["alias"] = value(alias);
            doc["url"] = value(url.as_str());
            if let Some(branch) = branch {
                doc["branch"] = value(branch.as_str());
            }
            if let Some(source) = item.get_mut("source").and_then(Item::as_table_mut) {
                source.remove("url");
                source.remove("branch");
            }
        }
        if doc.get("skills").is_none() {
            doc["skills"] = Item::Table(Table::new());
        }
        let skills = doc["skills"]
            .as_table_mut()
            .context("skills must be a table")?;
        let name = Self::entry(key);
        // Retain unknown fields and comments in the selected entry and its siblings.
        if let Some(existing) = skills.get_mut(name).and_then(Item::as_table_mut) {
            for field in ["tags", "note", "installed_name", "source", "baseline"] {
                if let Some(new) = item.remove(field) {
                    existing[field] = new;
                } else {
                    existing.remove(field);
                }
            }
        } else {
            skills[name] = Item::Table(item);
        }
        Ok(())
    }
    pub fn save(&self, key: &str, meta: &SkillMeta) -> Result<()> {
        crate::ops::require_key(key)?;
        let _lock = self.lock()?;
        let path = self.path(key);
        let mut doc = Self::read(&path)?;
        Self::put(&mut doc, key, meta)?;
        write_atomic(&path, doc.to_string().as_bytes())
    }
    pub fn remove(&self, key: &str) -> Result<()> {
        let _lock = self.lock()?;
        let path = self.path(key);
        let mut doc = Self::read(&path)?;
        if let Some(skills) = doc.get_mut("skills").and_then(Item::as_table_mut)
            && skills.remove(Self::entry(key)).is_some()
        {
            write_atomic(&path, doc.to_string().as_bytes())?;
        }
        Ok(())
    }
    pub fn rename(&self, old: &str, new: &str) -> Result<()> {
        crate::ops::require_key(new)?;
        if old == new {
            return Ok(());
        }
        let _lock = self.lock()?;
        let Some(meta) = self.load(old)? else {
            return Ok(());
        };
        anyhow::ensure!(
            self.load(new)?.is_none(),
            "destination metadata already exists"
        );
        let from = self.path(old);
        let to = self.path(new);
        let mut source = Self::read(&from)?;
        if from == to {
            let entry = source["skills"]
                .as_table_mut()
                .unwrap()
                .remove(Self::entry(old))
                .unwrap();
            source["skills"][Self::entry(new)] = entry;
            return write_atomic(&from, source.to_string().as_bytes());
        }
        let original = Self::read(&to)?;
        let mut target = original.clone();
        if target.get("skills").is_none() {
            target["skills"] = Item::Table(Table::new());
        }
        target["skills"][Self::entry(new)] = source["skills"][Self::entry(old)].clone();
        Self::put(&mut target, new, &meta)?;
        write_atomic(&to, target.to_string().as_bytes())?;
        source["skills"]
            .as_table_mut()
            .unwrap()
            .remove(Self::entry(old));
        if let Err(error) = write_atomic(&from, source.to_string().as_bytes()) {
            write_atomic(&to, original.to_string().as_bytes())?;
            return Err(error);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_file_preserves_siblings_and_serializes_concurrent_writes() {
        let temp = crate::ops::DownloadDir::new("metadata-concurrent").unwrap();
        let store = MetaStore::new(temp.path());
        std::thread::scope(|scope| {
            for i in 0..12 {
                let store = store.clone();
                scope.spawn(move || {
                    store
                        .save(
                            &format!("skill-{i}"),
                            &SkillMeta {
                                note: Some(format!("note-{i}")),
                                ..Default::default()
                            },
                        )
                        .unwrap()
                });
            }
        });
        assert_eq!(store.list_keys().unwrap().len(), 12);
        store.rename("skill-0", "renamed").unwrap();
        store.remove("skill-1").unwrap();
        assert!(store.load("skill-0").unwrap().is_none());
        assert_eq!(
            store.load("renamed").unwrap().unwrap().note.as_deref(),
            Some("note-0")
        );
        assert_eq!(store.list_keys().unwrap().len(), 11);
    }

    #[test]
    fn repository_identity_and_skill_versions_share_one_file() {
        let temp = crate::ops::DownloadDir::new("metadata-repo").unwrap();
        let ws = crate::Workspace::open(temp.path()).unwrap();
        let repo = crate::repository::Repository {
            alias: "owner--repo".into(),
            url: "https://example.com/owner/repo".into(),
            branch: "main".into(),
        };
        repo.save(&ws).unwrap();
        for (key, revision) in [("review", "one"), ("print", "two")] {
            ws.meta
                .save(
                    &format!("repos/{}/{key}", repo.alias),
                    &SkillMeta {
                        source: Some(Source::Git {
                            url: repo.url.clone(),
                            branch: Some(repo.branch.clone()),
                            subpath: Some(format!("skills/{key}")),
                            revision: Some(revision.into()),
                        }),
                        ..Default::default()
                    },
                )
                .unwrap();
        }
        repo.save(&ws).unwrap();
        let text = std::fs::read_to_string(ws.meta.path("repos/owner--repo/review")).unwrap();
        assert_eq!(text.matches(&repo.url).count(), 1);
        assert_eq!(ws.meta.list_keys().unwrap().len(), 2);
        ws.meta.remove("repos/owner--repo/review").unwrap();
        assert!(ws.meta.load("repos/owner--repo/print").unwrap().is_some());
        assert_eq!(
            crate::repository::Repository::list(temp.path()).unwrap(),
            vec![repo]
        );
    }

    #[test]
    fn old_per_skill_and_repository_files_are_not_read() {
        let temp = crate::ops::DownloadDir::new("metadata-no-legacy").unwrap();
        let store = MetaStore::new(temp.path());
        std::fs::create_dir_all(store.dir.join(".repositories")).unwrap();
        std::fs::write(store.dir.join("old.toml"), "tags = ['old']").unwrap();
        std::fs::write(
            store.dir.join(".repositories/old.toml"),
            "alias = 'old'\nurl = 'old'\nbranch = 'main'",
        )
        .unwrap();
        assert!(store.list_keys().unwrap().is_empty());
        assert!(
            crate::repository::Repository::list(temp.path())
                .unwrap()
                .is_empty()
        );
        assert!(store.load("old").unwrap().is_none());
    }

    #[test]
    fn deployment_registry_is_not_a_skill() {
        let tmp = std::env::temp_dir().join(format!("skills-meta-registry-{}", std::process::id()));
        std::fs::create_dir_all(tmp.join(".skills-meta")).unwrap();
        let store = MetaStore::new(&tmp);
        std::fs::write(store.dir.join("deployment-targets.toml"), "agents = []\n").unwrap();
        store.save("example", &SkillMeta::default()).unwrap();
        assert_eq!(store.list_keys().unwrap(), vec!["example"]);
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn roundtrip_preserves_comments() {
        let tmp = std::env::temp_dir().join(format!("skills-meta-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join(".skills-meta")).unwrap();
        let store = MetaStore::new(&tmp);
        std::fs::write(
            store.path("foo"),
            "# hand written comment\n[skills.foo]\ntags = [\"a\"]\nnote = \"hi\"\n",
        )
        .unwrap();
        let mut meta = store.load("foo").unwrap().unwrap();
        meta.tags.push("b".into());
        store.save("foo", &meta).unwrap();
        let text = std::fs::read_to_string(store.path("foo")).unwrap();
        assert!(text.contains("# hand written comment"));
        assert!(text.contains("\"b\""));
        let again = store.load("foo").unwrap().unwrap();
        assert_eq!(again.tags, vec!["a", "b"]);
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
