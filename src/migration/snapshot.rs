//! Bounded, read-only capture of supported Skill Home metadata documents.

use anyhow::{Context, Result, ensure};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

// Includes format.toml, config.toml, and every supported store document.
const MAX_FILES: usize = 10_000;
const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RelPath(PathBuf);
impl RelPath {
    pub fn new(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        ensure!(
            !path.is_absolute()
                && path
                    .components()
                    .all(|component| matches!(component, std::path::Component::Normal(_))),
            "invalid metadata relative path: {}",
            path.display()
        );
        Ok(Self(path))
    }
    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

#[derive(Clone, Debug, Default)]
pub struct HomeSnapshot {
    pub(crate) files: BTreeMap<RelPath, Vec<u8>>,
}
#[derive(Clone, Debug, Default)]
pub struct HomeView {
    pub(crate) files: BTreeMap<RelPath, Vec<u8>>,
}

impl HomeSnapshot {
    pub fn read(root: &Path) -> Result<Self> {
        let meta = crate::paths::meta_dir(root);
        let mut files = BTreeMap::new();
        let config = meta.join("config.toml");
        match std::fs::symlink_metadata(&config) {
            Ok(metadata) => {
                ensure!(
                    metadata.file_type().is_file() && !metadata.file_type().is_symlink(),
                    "invalid metadata entry: {}",
                    config.display()
                );
                read_one(&config, RelPath::new("config.toml")?, &mut files)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        for name in ["tags", "presets", "repos"] {
            let dir = meta.join(name);
            let rd = match std::fs::read_dir(&dir) {
                Ok(rd) => rd,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            let mut entries = rd.collect::<std::io::Result<Vec<_>>>()?;
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                let ty = entry.file_type()?;
                crate::util::reject_interrupted_transaction(&entry.path(), &ty)?;
                // Stray files are never read, moved, or deleted by a migration.
                let document = crate::util::is_store_document_name(&entry.file_name())
                    || (name == "repos" && entry.file_name() == ".root.toml");
                if !document {
                    continue;
                }
                ensure!(
                    ty.is_file() && !ty.is_symlink(),
                    "invalid {} store entry: {}",
                    store_label(name),
                    entry.path().display()
                );
                let filename = entry.file_name();
                let filename = filename.to_str().with_context(|| {
                    format!(
                        "metadata filename is not valid UTF-8: {}",
                        entry.path().display()
                    )
                })?;
                read_one(
                    &entry.path(),
                    RelPath::new(PathBuf::from(name).join(filename))?,
                    &mut files,
                )?;
            }
        }
        Ok(Self { files })
    }
    pub fn view(&self) -> HomeView {
        HomeView {
            files: self.files.clone(),
        }
    }
    pub fn files(&self) -> &BTreeMap<RelPath, Vec<u8>> {
        &self.files
    }
}
fn store_label(name: &str) -> &str {
    match name {
        "tags" => "Tag",
        "presets" => "preset",
        "repos" => "repository metadata",
        _ => "metadata",
    }
}
fn read_one(path: &Path, rel: RelPath, files: &mut BTreeMap<RelPath, Vec<u8>>) -> Result<()> {
    ensure!(files.len() < MAX_FILES, "metadata contains too many files");
    ensure!(
        std::fs::metadata(path)?.len() <= MAX_FILE_BYTES,
        "metadata file is too large: {}",
        path.display()
    );
    let bytes = std::fs::read(path)?;
    let total = files
        .values()
        .try_fold(bytes.len(), |total, current| {
            total.checked_add(current.len())
        })
        .context("metadata snapshot byte count overflow")?;
    ensure!(
        total <= MAX_TOTAL_BYTES,
        "metadata snapshot exceeds the 64 MiB total byte limit"
    );
    files.insert(rel, bytes);
    Ok(())
}
impl HomeView {
    pub fn get(&self, path: &str) -> Option<&[u8]> {
        self.files
            .get(&RelPath(PathBuf::from(path)))
            .map(Vec::as_slice)
    }
    pub fn insert(&mut self, path: impl Into<PathBuf>, bytes: Vec<u8>) -> Result<()> {
        self.files.insert(RelPath::new(path)?, bytes);
        Ok(())
    }
    pub fn remove(&mut self, path: &RelPath) -> Option<Vec<u8>> {
        self.files.remove(path)
    }
    pub fn files(&self) -> &BTreeMap<RelPath, Vec<u8>> {
        &self.files
    }
}
