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
        for filename in ["format.toml", "config.toml"] {
            let config = meta.join(filename);
            match std::fs::symlink_metadata(&config) {
                Ok(metadata) => {
                    ensure!(
                        metadata.file_type().is_file() && !metadata.file_type().is_symlink(),
                        "invalid metadata entry: {}",
                        config.display()
                    );
                    read_one(&config, RelPath::new(filename)?, &mut files)?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        for name in ["tags", "presets", "repos"] {
            // Stray files are never read, moved, or deleted by a migration.
            let is_document = |file: &std::ffi::OsStr| {
                if name == "repos" {
                    crate::util::is_repository_document_name(file)
                } else {
                    crate::util::is_store_document_name(file)
                }
            };
            let documents = crate::file_set::read_store(
                &meta,
                name,
                store_label(name),
                &is_document,
                &mut |path, len, read| admit(&files, path, len, read),
            )?;
            for (path, bytes) in documents {
                let filename = path
                    .file_name()
                    .and_then(|filename| filename.to_str())
                    .context("metadata filename is not valid UTF-8")?;
                files.insert(RelPath::new(PathBuf::from(name).join(filename))?, bytes);
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
/// Bound the snapshot before reading a file of `len` bytes, counting what is
/// already captured plus `pending` documents read for the current store.
fn admit(
    files: &BTreeMap<RelPath, Vec<u8>>,
    path: &Path,
    len: u64,
    pending: &[(PathBuf, Vec<u8>)],
) -> Result<()> {
    ensure!(
        files.len() + pending.len() < MAX_FILES,
        "metadata contains too many files"
    );
    ensure!(
        len <= MAX_FILE_BYTES,
        "metadata file is too large: {}",
        path.display()
    );
    let total = files
        .values()
        .chain(pending.iter().map(|(_, bytes)| bytes))
        .try_fold(len as usize, |total, current| {
            total.checked_add(current.len())
        })
        .context("metadata snapshot byte count overflow")?;
    ensure!(
        total <= MAX_TOTAL_BYTES,
        "metadata snapshot exceeds the 64 MiB total byte limit"
    );
    Ok(())
}

fn read_one(path: &Path, rel: RelPath, files: &mut BTreeMap<RelPath, Vec<u8>>) -> Result<()> {
    admit(files, path, std::fs::metadata(path)?.len(), &[])?;
    let bytes = std::fs::read(path)?;
    admit(files, path, bytes.len() as u64, &[])?;
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
