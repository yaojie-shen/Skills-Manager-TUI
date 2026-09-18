//! Repository metadata only; local skills do not have metadata records.
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
    Archive {
        url: String,
        #[serde(default)]
        subpath: Option<String>,
        #[serde(default)]
        revision: Option<String>,
    },
    Local {
        #[serde(default)]
        path: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SourceKind {
    #[default]
    Git,
    Archive,
}

impl SourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Git => "git",
            Self::Archive => "archive",
        }
    }
}

impl Source {
    pub fn kind(&self) -> &'static str {
        match self {
            Source::Git { .. } => "git",
            Source::Archive { .. } => "archive",
            Source::Local { .. } => "local",
        }
    }

    pub fn is_remote(&self) -> bool {
        matches!(self, Self::Git { .. } | Self::Archive { .. })
    }

    pub fn url(&self) -> Option<&str> {
        match self {
            Self::Git { url, .. } | Self::Archive { url, .. } => Some(url),
            Self::Local { .. } => None,
        }
    }

    pub fn subpath(&self) -> Option<&str> {
        match self {
            Self::Git { subpath, .. } | Self::Archive { subpath, .. } => subpath.as_deref(),
            Self::Local { .. } => None,
        }
    }

    pub fn branch(&self) -> Option<&str> {
        match self {
            Self::Git { branch, .. } => branch.as_deref(),
            _ => None,
        }
    }

    pub fn revision(&self) -> Option<&str> {
        match self {
            Self::Git { revision, .. } | Self::Archive { revision, .. } => revision.as_deref(),
            Self::Local { .. } => None,
        }
    }

    pub fn set_revision(&mut self, value: String) {
        match self {
            Self::Git { revision, .. } | Self::Archive { revision, .. } => *revision = Some(value),
            Self::Local { .. } => {}
        }
    }

    pub fn same_location(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Local { path }, Self::Local { path: other }) => path == other,
            _ => {
                self.kind() == other.kind()
                    && self.url() == other.url()
                    && self.branch() == other.branch()
                    && self.subpath().unwrap_or("") == other.subpath().unwrap_or("")
            }
        }
    }

    pub fn summary(&self) -> String {
        if let Source::Local { path } = self {
            return match path {
                Some(p) => format!("local {p}"),
                None => "local".into(),
            };
        }
        let mut s = self.url().unwrap().to_owned();
        if let Some(p) = self.subpath().filter(|p| !p.is_empty()) {
            s.push('/');
            s.push_str(p);
        }
        if let Some(b) = self.branch() {
            s.push('@');
            s.push_str(b);
        }
        if let Some(r) = self.revision() {
            s.push_str(&format!(" ({})", short_rev(r)));
        }
        s
    }
}

pub fn short_rev(r: &str) -> &str {
    let r = r.strip_prefix("sha256:").unwrap_or(r);
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
            None => self.dir.join("repos/.root.toml"),
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
            && matches!(
                source.get("type").and_then(toml::Value::as_str),
                Some("git" | "archive")
            )
            && crate::repository::alias_of(key).is_some()
        {
            let kind = doc.get("kind").and_then(Item::as_str).unwrap_or("git");
            anyhow::ensure!(
                source.get("type").and_then(toml::Value::as_str) == Some(kind),
                "repository kind differs from skill source"
            );
            source.insert(
                "url".into(),
                doc.get("url")
                    .and_then(Item::as_str)
                    .context("repository URL missing")?
                    .into(),
            );
            if kind == "git"
                && let Some(branch) = doc.get("branch").and_then(Item::as_str)
            {
                source.insert("branch".into(), branch.into());
            }
        }
        let mut meta: SkillMeta = value.try_into()?;
        if !meta.source.as_ref().is_some_and(Source::is_remote) {
            meta.baseline = None;
        }
        if crate::repository::alias_of(key).is_none()
            && !meta.source.as_ref().is_some_and(Source::is_remote)
        {
            return Ok(None);
        }
        Ok(Some(meta))
    }
    pub fn list_keys(&self) -> Result<Vec<String>> {
        let mut files = Vec::new();
        let repos = self.dir.join("repos");
        if repos.is_dir() {
            for entry in std::fs::read_dir(repos)? {
                let path = entry?.path();
                if path.is_file() && path.extension().is_some_and(|e| e == "toml") {
                    let alias = path.file_stem().unwrap().to_string_lossy();
                    if alias == ".root" {
                        files.push((path.clone(), String::new()));
                        continue;
                    }
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
        if !meta.source.as_ref().is_some_and(Source::is_remote) {
            item.remove("baseline");
        }
        if let Some(alias) = crate::repository::alias_of(key)
            && let Some(source) = &meta.source
            && let Some(url) = source.url()
        {
            anyhow::ensure!(
                doc.get("url").is_none()
                    || doc.get("kind").and_then(Item::as_str).unwrap_or("git") == source.kind(),
                "repository kind differs from skill source"
            );
            anyhow::ensure!(
                doc.get("url")
                    .and_then(Item::as_str)
                    .is_none_or(|u| u == url),
                "repository URL differs from skill source"
            );
            anyhow::ensure!(
                doc.get("branch")
                    .and_then(Item::as_str)
                    .is_none_or(|b| Some(b) == source.branch()),
                "repository branch differs from skill source"
            );
            doc["alias"] = value(alias);
            doc["kind"] = value(source.kind());
            doc["url"] = value(url);
            if let Some(branch) = source.branch() {
                doc["branch"] = value(branch);
            } else {
                doc.remove("branch");
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
        if crate::repository::alias_of(key).is_none()
            && !meta.source.as_ref().is_some_and(Source::is_remote)
        {
            return self.remove(key);
        }
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
        let entry = source["skills"]
            .as_table_mut()
            .context("source metadata entry missing")?
            .remove(Self::entry(old))
            .context("source metadata entry missing")?;
        if from == to {
            source["skills"][Self::entry(new)] = entry;
            return write_atomic(&from, source.to_string().as_bytes());
        }
        let original = Self::read(&to)?;
        let mut target = original.clone();
        if target.get("skills").is_none() {
            target["skills"] = Item::Table(Table::new());
        }
        target["skills"][Self::entry(new)] = entry;
        Self::put(&mut target, new, &meta)?;
        write_atomic(&to, target.to_string().as_bytes())?;
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
                                source: Some(Source::Git {
                                    url: "https://example.com/repo".into(),
                                    branch: None,
                                    subpath: None,
                                    revision: None,
                                }),
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
            name: Some("Owner tools".into()),
            kind: SourceKind::Git,
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
    fn archive_metadata_shares_identity_and_keeps_per_skill_baselines() {
        let temp = crate::ops::DownloadDir::new("archive-metadata").unwrap();
        let store = MetaStore::new(temp.path());
        for key in ["one", "two"] {
            let meta = SkillMeta {
                source: Some(Source::Archive {
                    url: "https://example.com/skills.zip?version=latest".into(),
                    subpath: Some(format!("skills/{key}")),
                    revision: Some(format!("sha256:{key}")),
                }),
                baseline: Some(Baseline {
                    hash: format!("baseline-{key}"),
                    hash_algo: crate::hash::HASH_ALGO,
                }),
                ..Default::default()
            };
            let id = format!("repos/archive/{key}");
            store.save(&id, &meta).unwrap();
            assert_eq!(store.load(&id).unwrap().unwrap(), meta);
        }
        let text = std::fs::read_to_string(store.path("repos/archive/one")).unwrap();
        assert_eq!(text.matches("https://example.com/skills.zip").count(), 1);
        assert!(text.contains("kind = \"archive\""));
        assert!(!text.contains("branch"));
        let incompatible = SkillMeta {
            source: Some(Source::Git {
                url: "https://example.com/skills.zip?version=latest".into(),
                subpath: Some("skills/three".into()),
                branch: None,
                revision: None,
            }),
            ..Default::default()
        };
        assert!(store.save("repos/archive/three", &incompatible).is_err());
        assert_eq!(store.list_keys().unwrap().len(), 2);
    }

    #[test]
    fn repository_without_kind_remains_git() {
        let temp = crate::ops::DownloadDir::new("git-metadata-default-kind").unwrap();
        let store = MetaStore::new(temp.path());
        let file = store.path("repos/demo/one");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "alias = 'demo'\nurl = 'https://example.com/repo'\nbranch = 'main'\n[skills.one.source]\ntype = 'git'\nsubpath = 'one'\nrevision = 'abcdef'\n").unwrap();
        let source = store
            .load("repos/demo/one")
            .unwrap()
            .unwrap()
            .source
            .unwrap();
        assert_eq!(source.kind(), "git");
        assert_eq!(source.url(), Some("https://example.com/repo"));
        assert_eq!(source.branch(), Some("main"));
        assert_eq!(source.revision(), Some("abcdef"));
        assert_eq!(
            crate::repository::Repository::list(temp.path()).unwrap()[0].kind,
            SourceKind::Git
        );
    }

    #[test]
    fn source_location_excludes_versions_but_includes_transport() {
        let archive = Source::Archive {
            url: "https://example.com/source".into(),
            subpath: None,
            revision: Some("one".into()),
        };
        let another_version = Source::Archive {
            url: archive.url().unwrap().into(),
            subpath: Some(String::new()),
            revision: Some("two".into()),
        };
        let git = Source::Git {
            url: archive.url().unwrap().into(),
            subpath: None,
            branch: None,
            revision: None,
        };
        assert!(archive.same_location(&another_version));
        assert!(!archive.same_location(&git));
    }

    #[test]
    fn short_revision_uses_hash_digits_for_git_and_archives() {
        assert_eq!(short_rev("0123456789abcdef"), "0123456789ab");
        assert_eq!(short_rev("sha256:0123456789abcdef"), "0123456789ab");
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
        std::fs::create_dir_all(tmp.join(".skills-meta/repos")).unwrap();
        let store = MetaStore::new(&tmp);
        std::fs::write(store.dir.join("deployment-targets.toml"), "agents = []\n").unwrap();
        store
            .save("repos/demo/example", &SkillMeta::default())
            .unwrap();
        assert_eq!(store.list_keys().unwrap(), vec!["repos/demo/example"]);
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn roundtrip_preserves_comments() {
        let tmp = std::env::temp_dir().join(format!("skills-meta-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join(".skills-meta/repos")).unwrap();
        let store = MetaStore::new(&tmp);
        std::fs::write(
            store.path("foo"),
            "# hand written comment\n[skills.foo]\nnote = \"hi\"\n",
        )
        .unwrap();
        let mut doc = MetaStore::read(&store.path("foo")).unwrap();
        doc["skills"]["foo"]["source"] = toml_edit::Item::Table({
            let mut t = Table::new();
            t["type"] = value("git");
            t["url"] = value("https://example.com/repo");
            t
        });
        std::fs::write(store.path("foo"), doc.to_string()).unwrap();
        let mut meta = store.load("foo").unwrap().unwrap();
        meta.note = Some("updated".into());
        store.save("foo", &meta).unwrap();
        let text = std::fs::read_to_string(store.path("foo")).unwrap();
        assert!(text.contains("# hand written comment"));
        assert!(text.contains("updated"));
        let again = store.load("foo").unwrap().unwrap();
        assert_eq!(again.note.as_deref(), Some("updated"));
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
