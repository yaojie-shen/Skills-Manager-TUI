//! Process-local read state shared by a Workspace and its worker clones.
//!
//! Only display/inventory reads opt in. Mutation preflight, content verification
//! and store writes keep their fresh-read APIs. Entries are reusable only while
//! file identity, size, timestamps, permissions and symlink targets still match.
//! A failed read evicts its previous value; a read racing a change is not cached.
//! No content hashes or on-disk cache are stored here. Polling still performs
//! metadata I/O, including on network filesystems; it does not reread unchanged
//! document contents. Explicit refresh clears the cache, including in-flight
//! publication, and unused entries are pruned after subsequent inventories.

use anyhow::Result;
use std::{
    any::{Any, TypeId},
    collections::HashMap,
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Identity {
    device: u64,
    inode: u64,
    mode: u32,
    size: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

impl Identity {
    fn new(meta: &fs::Metadata) -> Self {
        Self {
            device: meta.dev(),
            inode: meta.ino(),
            mode: meta.mode(),
            size: meta.len(),
            modified: (meta.mtime(), meta.mtime_nsec()),
            changed: (meta.ctime(), meta.ctime_nsec()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Stamp {
    entry: Identity,
    target: Option<Identity>,
    pub(crate) link: Option<PathBuf>,
}

pub(crate) fn stamp(path: &Path) -> std::io::Result<Option<Stamp>> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    stamp_with_metadata(path, &meta).map(Some)
}

pub(crate) fn stamp_with_metadata(path: &Path, meta: &fs::Metadata) -> std::io::Result<Stamp> {
    let (target, link) = if meta.file_type().is_symlink() {
        let target = match fs::metadata(path) {
            Ok(meta) => Some(Identity::new(&meta)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        (target, Some(fs::read_link(path)?))
    } else {
        (None, None)
    };
    Ok(Stamp {
        entry: Identity::new(meta),
        target,
        link,
    })
}

/// A broken/unreadable symlink target must not stop polling other documents.
/// Cache reads still use the fallible signature above and never reuse a value
/// when the target cannot be inspected.
pub(crate) fn watch_stamp(path: &Path, meta: &fs::Metadata) -> Stamp {
    stamp_with_metadata(path, meta).unwrap_or_else(|_| Stamp {
        entry: Identity::new(meta),
        target: None,
        link: fs::read_link(path).ok(),
    })
}

struct Entry {
    stamp: Option<Stamp>,
    value: Arc<dyn Any + Send + Sync>,
    generation: u64,
}

/// Cumulative diagnostics for actual loader calls and reused values.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub loads: u64,
    pub hits: u64,
}

#[derive(Default)]
pub struct ReadCache {
    entries: Mutex<HashMap<(PathBuf, TypeId), Entry>>,
    epoch: AtomicU64,
    generation: AtomicU64,
    loads: AtomicU64,
    hits: AtomicU64,
}

impl std::fmt::Debug for ReadCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReadCache")
            .field("stats", &self.stats())
            .finish_non_exhaustive()
    }
}

impl ReadCache {
    pub fn clear(&self) {
        let mut entries = self.entries.lock().unwrap();
        self.epoch.fetch_add(1, Ordering::SeqCst);
        entries.clear();
    }

    pub fn stats(&self) -> Stats {
        Stats {
            loads: self.loads.load(Ordering::Relaxed),
            hits: self.hits.load(Ordering::Relaxed),
        }
    }

    pub(crate) fn begin_inventory(&self) {
        let generation = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        self.entries
            .lock()
            .unwrap()
            .retain(|_, entry| entry.generation + 2 >= generation);
    }

    pub(crate) fn load<T: Clone + Send + Sync + 'static>(
        &self,
        path: &Path,
        read: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        let key = (path.to_path_buf(), TypeId::of::<T>());
        let epoch = self.epoch.load(Ordering::SeqCst);
        let generation = self.generation.load(Ordering::Relaxed);
        let before = stamp(path);
        let cached = {
            let mut entries = self.entries.lock().unwrap();
            if let Ok(before) = &before
                && let Some(entry) = entries.get_mut(&key)
                && entry.stamp == *before
            {
                entry.generation = generation;
                Some(entry.value.clone())
            } else {
                entries.remove(&key);
                None
            }
        };
        // Parsed documents can be large. Never serialize their cloning behind
        // the map lock while the bounded reader pool is active.
        if let Some(value) = cached {
            self.hits.fetch_add(1, Ordering::Relaxed);
            return Ok(value.downcast_ref::<T>().expect("cache type key").clone());
        }
        self.loads.fetch_add(1, Ordering::Relaxed);
        let value = read()?;
        if let (Ok(before), Ok(after)) = (before, stamp(path))
            && before == after
        {
            let cached = Arc::new(value.clone());
            let mut entries = self.entries.lock().unwrap();
            if self.epoch.load(Ordering::SeqCst) == epoch {
                entries.insert(
                    key,
                    Entry {
                        stamp: after,
                        value: cached,
                        generation,
                    },
                );
            }
        }
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_racing_an_edit_is_not_reused_or_allowed_to_replace_a_newer_read() {
        let temp = crate::ops::DownloadDir::new("cache-edit-race").unwrap();
        let path = temp.path().join("document");
        fs::write(&path, "old").unwrap();
        let cache = Arc::new(ReadCache::default());
        let (started, waiting) = std::sync::mpsc::channel();
        let (resume, go) = std::sync::mpsc::channel();
        let worker_cache = cache.clone();
        let worker_path = path.clone();
        let worker = std::thread::spawn(move || {
            worker_cache
                .load(&worker_path, || {
                    let old = fs::read_to_string(&worker_path)?;
                    started.send(()).unwrap();
                    go.recv().unwrap();
                    Ok(old)
                })
                .unwrap()
        });
        waiting.recv().unwrap();
        fs::write(&path, "new contents").unwrap();
        assert_eq!(
            cache
                .load(&path, || Ok(fs::read_to_string(&path)?))
                .unwrap(),
            "new contents"
        );
        resume.send(()).unwrap();
        // An already running display read may finish with the old snapshot;
        // it must not poison future reads after the file has changed.
        assert_eq!(worker.join().unwrap(), "old");
        assert_eq!(
            cache
                .load::<String>(&path, || panic!("newer value should be cached"))
                .unwrap(),
            "new contents"
        );
    }

    #[test]
    fn deletion_during_read_is_retried_and_recreation_is_observed() {
        let temp = crate::ops::DownloadDir::new("cache-delete-race").unwrap();
        let path = temp.path().join("document");
        fs::write(&path, "old").unwrap();
        let cache = ReadCache::default();
        assert_eq!(
            cache
                .load(&path, || {
                    let value = fs::read_to_string(&path)?;
                    fs::remove_file(&path)?;
                    Ok(value)
                })
                .unwrap(),
            "old"
        );
        assert!(
            cache
                .load(&path, || Ok(fs::read_to_string(&path)?))
                .is_err()
        );
        fs::write(&path, "replacement").unwrap();
        assert_eq!(
            cache
                .load(&path, || Ok(fs::read_to_string(&path)?))
                .unwrap(),
            "replacement"
        );
        assert_eq!(cache.stats(), Stats { loads: 3, hits: 0 });
    }

    #[test]
    fn explicit_clear_during_a_read_prevents_old_publication() {
        let temp = crate::ops::DownloadDir::new("cache-race").unwrap();
        let path = temp.path().join("document");
        fs::write(&path, "value").unwrap();
        let cache = Arc::new(ReadCache::default());
        let (started, waiting) = std::sync::mpsc::channel();
        let (resume, go) = std::sync::mpsc::channel();
        let worker_cache = cache.clone();
        let worker_path = path.clone();
        let worker = std::thread::spawn(move || {
            worker_cache
                .load(&worker_path, || {
                    started.send(()).unwrap();
                    go.recv().unwrap();
                    Ok("previous read".to_string())
                })
                .unwrap()
        });
        waiting.recv().unwrap();
        cache.clear();
        resume.send(()).unwrap();
        worker.join().unwrap();
        assert_eq!(
            cache.load(&path, || Ok("new read".to_string())).unwrap(),
            "new read"
        );
        assert_eq!(cache.stats().hits, 0);
    }

    #[test]
    fn failed_loads_retry_and_unused_values_are_pruned() {
        let temp = crate::ops::DownloadDir::new("cache-errors").unwrap();
        let path = temp.path().join("document");
        fs::write(&path, "value").unwrap();
        let cache = ReadCache::default();
        cache.load(&path, || Ok("first".to_string())).unwrap();
        fs::write(&path, "changed").unwrap();
        assert!(
            cache
                .load::<String>(&path, || anyhow::bail!("read failed"))
                .is_err()
        );
        assert_eq!(
            cache.load(&path, || Ok("recovered".to_string())).unwrap(),
            "recovered"
        );
        for _ in 0..3 {
            cache.begin_inventory();
        }
        assert!(cache.entries.lock().unwrap().is_empty());
    }
}
