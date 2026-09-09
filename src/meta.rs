//! Per-skill metadata files: `<root>/.skills-meta/<key>.toml`.
//!
//! Reading uses serde; writing goes through `toml_edit` so that comments and
//! key order written by hand survive round trips.

use crate::paths::meta_dir;
use crate::util::write_atomic;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use toml_edit::{Array, DocumentMut, Item, Table, value};

pub const SCHEMA: u32 = 1;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SkillMeta {
    #[serde(default = "default_schema")]
    pub schema: u32,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub source: Option<Source>,
    #[serde(default)]
    pub baseline: Option<Baseline>,
}

fn default_schema() -> u32 {
    SCHEMA
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
        self.dir.join(format!("{key}.toml"))
    }

    pub fn exists(&self, key: &str) -> bool {
        self.path(key).is_file()
    }

    /// Load metadata for `key`. `Ok(None)` when no file exists; `Err` when it is unreadable.
    pub fn load(&self, key: &str) -> Result<Option<SkillMeta>> {
        let path = self.path(key);
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let meta: SkillMeta = toml::from_str(&text)
                    .with_context(|| format!("invalid metadata: {}", path.display()))?;
                Ok(Some(meta))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    /// Keys of every metadata file present.
    pub fn list_keys(&self) -> Result<Vec<String>> {
        let mut keys = Vec::new();
        let rd = match std::fs::read_dir(&self.dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(keys),
            Err(e) => return Err(e.into()),
        };
        for entry in rd {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if matches!(name.as_str(), "repos" | "local") && entry.file_type()?.is_dir() {
                for file in walkdir::WalkDir::new(entry.path()).follow_links(false) {
                    let file = file?;
                    if file.file_type().is_file()
                        && file.path().extension().is_some_and(|e| e == "toml")
                    {
                        let relative = file.path().strip_prefix(&self.dir)?.to_string_lossy();
                        let key = relative.trim_end_matches(".toml");
                        if crate::repository::valid_id(key) {
                            keys.push(key.to_string());
                        }
                    }
                }
            } else if entry.file_type()?.is_file()
                && name != crate::config::CONFIG_FILE
                && !name.starts_with('.')
                && let Some(stem) = name.strip_suffix(".toml")
            {
                keys.push(stem.to_string());
            }
        }
        keys.sort();
        Ok(keys)
    }

    /// Write metadata, preserving comments and unknown keys of an existing file.
    pub fn save(&self, key: &str, meta: &SkillMeta) -> Result<()> {
        let path = self.path(key);
        let mut doc = match std::fs::read_to_string(&path) {
            Ok(text) => text
                .parse::<DocumentMut>()
                .with_context(|| format!("invalid metadata: {}", path.display()))?,
            Err(_) => DocumentMut::new(),
        };
        doc["schema"] = value(meta.schema as i64);
        let mut tags = Array::new();
        for t in &meta.tags {
            tags.push(t.as_str());
        }
        doc["tags"] = value(tags);
        match &meta.note {
            Some(n) if !n.is_empty() => {
                doc["note"] = value(n.as_str());
            }
            _ => {
                doc.remove("note");
            }
        }
        match &meta.source {
            Some(src) => {
                let mut t = Table::new();
                match src {
                    Source::Git {
                        url,
                        subpath,
                        branch,
                        revision,
                    } => {
                        t["type"] = value("git");
                        t["url"] = value(url.as_str());
                        if let Some(p) = subpath {
                            t["subpath"] = value(p.as_str());
                        }
                        if let Some(b) = branch {
                            t["branch"] = value(b.as_str());
                        }
                        if let Some(r) = revision {
                            t["revision"] = value(r.as_str());
                        }
                    }
                    Source::Local { path } => {
                        t["type"] = value("local");
                        if let Some(p) = path {
                            t["path"] = value(p.as_str());
                        }
                    }
                }
                doc["source"] = Item::Table(t);
            }
            None => {
                doc.remove("source");
            }
        }
        match &meta.baseline {
            Some(b) => {
                let mut t = Table::new();
                t["hash"] = value(b.hash.as_str());
                t["hash_algo"] = value(b.hash_algo as i64);
                doc["baseline"] = Item::Table(t);
            }
            None => {
                doc.remove("baseline");
            }
        }
        write_atomic(&path, doc.to_string().as_bytes())
    }

    pub fn remove(&self, key: &str) -> Result<()> {
        let path = self.path(key);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| format!("removing {}", path.display())),
        }
    }

    pub fn rename(&self, old: &str, new: &str) -> Result<()> {
        let from = self.path(old);
        if !from.exists() {
            return Ok(());
        }
        std::fs::create_dir_all(self.path(new).parent().context("metadata parent missing")?)?;
        std::fs::rename(&from, self.path(new))
            .with_context(|| format!("renaming metadata {old} -> {new}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_preserves_comments() {
        let tmp = std::env::temp_dir().join(format!("skills-meta-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join(".skills-meta")).unwrap();
        let store = MetaStore::new(&tmp);
        std::fs::write(
            store.path("foo"),
            "# hand written comment\nschema = 1\ntags = [\"a\"]\nnote = \"hi\"\n",
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
