//! Stable content hash of a skill directory.
//!
//! Algorithm version 1: walk the directory, sort entries by relative path,
//! skip `.git`, `__pycache__`, `.DS_Store` and `*.pyc`; for every file feed
//! `rel_path\0` followed by the bytes and `\0`; symlinks contribute their
//! target path instead of being followed; directories themselves contribute
//! nothing. Permissions and timestamps are ignored.
//!
//! [`hash_directories`] reuses one fixed worker pool for discovery and file
//! reads across the whole input. Discovery response channels and file batches
//! use global windows capped by the worker count. Sorted entries are divided
//! into batches of four. Each worker retains one 64 KiB read buffer; each
//! in-flight batch has a bounded one-message output channel and coalesces
//! encoded bytes into 64 KiB chunks. At the 32-worker cap, read, encode, queued
//! and blocked-send buffers therefore remain around 8 MiB, in addition to the
//! sorted path entries for all inputs.

use crate::util::is_ignored_name;
use anyhow::{Context, Result, anyhow};
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::fs::FileType;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use walkdir::WalkDir;

pub const HASH_ALGO: u32 = 1;

const BLOCK_SIZE: usize = 64 * 1024;
const BATCH_SIZE: usize = 4;
const MAX_WORKERS: usize = 32;

pub fn hash_directory(dir: &Path) -> Result<String> {
    let mut entries: Vec<(String, walkdir::DirEntry)> = Vec::new();
    let walker = WalkDir::new(dir)
        .follow_links(false)
        .min_depth(1)
        .into_iter()
        .filter_entry(|e| !is_ignored_name(&e.file_name().to_string_lossy()));
    for entry in walker {
        let entry = entry?;
        let rel = entry
            .path()
            .strip_prefix(dir)
            .unwrap_or(entry.path())
            .to_string_lossy()
            .into_owned();
        entries.push((rel, entry));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; BLOCK_SIZE];
    for (rel, entry) in entries {
        let ft = entry.file_type();
        if ft.is_symlink() {
            let target = std::fs::read_link(entry.path())?;
            hasher.update(rel.as_bytes());
            hasher.update(b"\0->");
            hasher.update(target.to_string_lossy().as_bytes());
            hasher.update(b"\0");
        } else if ft.is_file() {
            hasher.update(rel.as_bytes());
            hasher.update(b"\0");
            let mut file = std::fs::File::open(entry.path())?;
            loop {
                let read = file.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                hasher.update(&buffer[..read]);
            }
            hasher.update(b"\0");
        }
    }
    Ok(format_digest(hasher))
}

/// Hash several directories with one reusable, bounded worker pool.
///
/// Results retain input order, including duplicate paths. One failed directory
/// does not prevent the other directories from completing.
pub fn hash_directories(paths: &[PathBuf], workers: usize) -> Vec<Result<String>> {
    hash_directories_with_observer(paths, workers, &|_, _, _| {})
}

#[derive(Clone, Copy)]
enum PoolPhase {
    Discover,
    Batch,
}

struct HashEntry {
    relative: String,
    path: PathBuf,
    file_type: FileType,
}

enum Job {
    Discover {
        directory: usize,
        path: PathBuf,
        output: mpsc::Sender<Result<Vec<HashEntry>>>,
    },
    Batch {
        directory: usize,
        entries: Vec<HashEntry>,
        output: mpsc::SyncSender<BatchMessage>,
    },
}

enum BatchMessage {
    Data(Vec<u8>),
    Done,
    Error(anyhow::Error),
}

struct BatchSpec {
    directory: usize,
    entries: Vec<HashEntry>,
}

enum DirectoryState {
    Hashing(Sha256),
    Failed(anyhow::Error),
}

struct Encoder<'a> {
    output: &'a mpsc::SyncSender<BatchMessage>,
    buffer: Vec<u8>,
}

impl Encoder<'_> {
    fn append(&mut self, mut bytes: &[u8]) -> Result<()> {
        while !bytes.is_empty() {
            let count = (BLOCK_SIZE - self.buffer.len()).min(bytes.len());
            self.buffer.extend_from_slice(&bytes[..count]);
            bytes = &bytes[count..];
            if self.buffer.len() == BLOCK_SIZE {
                self.flush()?;
            }
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        let bytes = std::mem::replace(&mut self.buffer, Vec::with_capacity(BLOCK_SIZE));
        self.output
            .send(BatchMessage::Data(bytes))
            .map_err(|_| anyhow!("hash batch consumer closed"))
    }
}

fn hash_directories_with_observer(
    paths: &[PathBuf],
    workers: usize,
    observer: &(impl Fn(PoolPhase, usize, usize) + Sync),
) -> Vec<Result<String>> {
    if paths.is_empty() {
        return Vec::new();
    }
    let workers = workers.clamp(1, MAX_WORKERS);
    let (tasks, task_input) = mpsc::sync_channel::<Job>(MAX_WORKERS);
    let task_input = Arc::new(Mutex::new(task_input));

    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                let task_input = Arc::clone(&task_input);
                scope.spawn(move || worker(&task_input, observer))
            })
            .collect();
        // Only workers should keep the receiving side alive. If they all panic,
        // producers observe a disconnect instead of blocking on a full queue.
        drop(task_input);

        let mut discoveries: Vec<Option<VecDeque<HashEntry>>> =
            std::iter::repeat_with(|| None).take(paths.len()).collect();
        let mut states: Vec<Option<DirectoryState>> =
            std::iter::repeat_with(|| None).take(paths.len()).collect();
        let mut discovery_outputs = VecDeque::new();
        let mut next_discovery = 0;
        let mut pool_stopped = false;
        while next_discovery < paths.len() || !discovery_outputs.is_empty() {
            while next_discovery < paths.len() && discovery_outputs.len() < workers {
                let directory = next_discovery;
                let (output, input) = mpsc::channel();
                let job = Job::Discover {
                    directory,
                    path: paths[directory].clone(),
                    output,
                };
                if tasks.send(job).is_err() {
                    for failed in directory..paths.len() {
                        states[failed] = Some(DirectoryState::Failed(anyhow!(
                            "hash worker pool stopped before discovering {}",
                            paths[failed].display()
                        )));
                    }
                    next_discovery = paths.len();
                    pool_stopped = true;
                    break;
                }
                discovery_outputs.push_back((directory, input));
                next_discovery += 1;
            }
            let Some((directory, input)) = discovery_outputs.pop_front() else {
                break;
            };
            match input.recv() {
                Ok(Ok(entries)) => {
                    discoveries[directory] = Some(entries.into());
                    states[directory] = Some(DirectoryState::Hashing(Sha256::new()));
                }
                Ok(Err(error)) => states[directory] = Some(DirectoryState::Failed(error)),
                Err(_) => {
                    states[directory] = Some(DirectoryState::Failed(anyhow!(
                        "hash discovery worker disconnected for {}",
                        paths[directory].display()
                    )))
                }
            }
        }

        if pool_stopped {
            for (directory, state) in states.iter_mut().enumerate() {
                if matches!(state, Some(DirectoryState::Hashing(_))) {
                    *state = Some(DirectoryState::Failed(anyhow!(
                        "hash worker pool stopped before reading {}",
                        paths[directory].display()
                    )));
                }
            }
        }

        let mut pending = VecDeque::new();
        let mut next_batch_directory = 0;
        loop {
            while !pool_stopped && pending.len() < workers {
                let Some(batch) =
                    take_next_batch(&mut discoveries, &states, &mut next_batch_directory)
                else {
                    break;
                };
                let (output, input) = mpsc::sync_channel(1);
                let directory = batch.directory;
                if tasks
                    .send(Job::Batch {
                        directory,
                        entries: batch.entries,
                        output,
                    })
                    .is_err()
                {
                    states[directory] = Some(DirectoryState::Failed(anyhow!(
                        "hash worker pool stopped while reading {}",
                        paths[directory].display()
                    )));
                    pool_stopped = true;
                    break;
                }
                pending.push_back((directory, input));
            }

            let Some((directory, input)) = pending.pop_front() else {
                break;
            };
            let Some(DirectoryState::Hashing(hasher)) = states[directory].as_mut() else {
                drop(input);
                continue;
            };
            if let Err(error) = consume_batch(hasher, input) {
                states[directory] = Some(DirectoryState::Failed(error));
            }
        }

        if pool_stopped {
            for (directory, state) in states.iter_mut().enumerate() {
                if matches!(state, Some(DirectoryState::Hashing(_))) {
                    *state = Some(DirectoryState::Failed(anyhow!(
                        "hash worker pool stopped while reading {}",
                        paths[directory].display()
                    )));
                }
            }
        }

        // Closing the job queue lets every healthy worker exit. Join all of
        // them before propagating the first panic so no scoped thread is left
        // blocked and all peer directories have had a chance to finish.
        drop(tasks);
        let mut worker_panic = None;
        for handle in handles {
            if let Err(panic) = handle.join()
                && worker_panic.is_none()
            {
                worker_panic = Some(panic);
            }
        }
        if let Some(panic) = worker_panic {
            std::panic::resume_unwind(panic);
        }

        states
            .into_iter()
            .enumerate()
            .map(|(directory, state)| match state {
                Some(DirectoryState::Hashing(hasher)) => Ok(format_digest(hasher)),
                Some(DirectoryState::Failed(error)) => Err(error),
                None => Err(anyhow!(
                    "hash worker pool produced no result for {}",
                    paths[directory].display()
                )),
            })
            .collect()
    })
}

fn take_next_batch(
    discoveries: &mut [Option<VecDeque<HashEntry>>],
    states: &[Option<DirectoryState>],
    next_directory: &mut usize,
) -> Option<BatchSpec> {
    while *next_directory < discoveries.len() {
        let directory = *next_directory;
        if !matches!(states[directory], Some(DirectoryState::Hashing(_))) {
            discoveries[directory] = None;
            *next_directory += 1;
            continue;
        }
        let entries = discoveries[directory]
            .as_mut()
            .expect("successful discovery must retain its entries");
        let mut batch = Vec::with_capacity(BATCH_SIZE);
        for _ in 0..BATCH_SIZE {
            let Some(entry) = entries.pop_front() else {
                break;
            };
            batch.push(entry);
        }
        if entries.is_empty() {
            discoveries[directory] = None;
            *next_directory += 1;
        }
        if !batch.is_empty() {
            return Some(BatchSpec {
                directory,
                entries: batch,
            });
        }
    }
    None
}

fn worker(
    tasks: &Arc<Mutex<mpsc::Receiver<Job>>>,
    observer: &(impl Fn(PoolPhase, usize, usize) + Sync),
) {
    let mut scratch = vec![0; BLOCK_SIZE];
    loop {
        let job = {
            let input = tasks
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            input.recv()
        };
        let Ok(job) = job else { break };
        match job {
            Job::Discover {
                directory,
                path,
                output,
            } => {
                observer(PoolPhase::Discover, directory, 0);
                let result = discover(&path)
                    .with_context(|| format!("failed to discover {}", path.display()));
                let _ = output.send(result);
            }
            Job::Batch {
                directory,
                entries,
                output,
            } => {
                observer(PoolPhase::Batch, directory, entries.len());
                read_batch(entries, output, &mut scratch);
            }
        }
    }
}

fn discover(dir: &Path) -> Result<Vec<HashEntry>> {
    let mut entries = Vec::new();
    let walker = WalkDir::new(dir)
        .follow_links(false)
        .min_depth(1)
        .into_iter()
        .filter_entry(|entry| !is_ignored_name(&entry.file_name().to_string_lossy()));
    for entry in walker {
        let entry = entry?;
        let relative = entry
            .path()
            .strip_prefix(dir)
            .unwrap_or(entry.path())
            .to_string_lossy()
            .into_owned();
        entries.push(HashEntry {
            relative,
            path: entry.path().to_path_buf(),
            file_type: entry.file_type(),
        });
    }
    entries.sort_by(|a, b| a.relative.cmp(&b.relative));
    Ok(entries)
}

fn read_batch(entries: Vec<HashEntry>, output: mpsc::SyncSender<BatchMessage>, scratch: &mut [u8]) {
    let result = (|| -> Result<()> {
        let mut encoder = Encoder {
            output: &output,
            buffer: Vec::with_capacity(BLOCK_SIZE),
        };
        for entry in entries {
            if entry.file_type.is_symlink() {
                let target = std::fs::read_link(&entry.path)?;
                encoder.append(entry.relative.as_bytes())?;
                encoder.append(b"\0->")?;
                encoder.append(target.to_string_lossy().as_bytes())?;
                encoder.append(b"\0")?;
            } else if entry.file_type.is_file() {
                encoder.append(entry.relative.as_bytes())?;
                encoder.append(b"\0")?;
                let mut file = std::fs::File::open(&entry.path)?;
                loop {
                    let read = file.read(scratch)?;
                    if read == 0 {
                        break;
                    }
                    encoder.append(&scratch[..read])?;
                }
                encoder.append(b"\0")?;
            }
        }
        encoder.flush()
    })();
    let _ = output.send(match result {
        Ok(()) => BatchMessage::Done,
        Err(error) => BatchMessage::Error(error),
    });
}

fn consume_batch(hasher: &mut Sha256, input: mpsc::Receiver<BatchMessage>) -> Result<()> {
    loop {
        match input
            .recv()
            .context("hash batch worker disconnected before completion")?
        {
            BatchMessage::Data(bytes) => hasher.update(bytes),
            BatchMessage::Done => return Ok(()),
            BatchMessage::Error(error) => return Err(error),
        }
    }
}

fn format_digest(hasher: Sha256) -> String {
    let hex: String = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("sha256:{hex}")
}

pub fn default_algo() -> u32 {
    HASH_ALGO
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn streaming_keeps_v1_hash_format_across_buffer_boundaries() {
        let tmp = crate::ops::DownloadDir::new("stream-hash").unwrap();
        let data = vec![b'x'; 150_000];
        std::fs::write(tmp.path().join("a"), &data).unwrap();
        std::fs::write(tmp.path().join("empty"), []).unwrap();
        std::fs::write(tmp.path().join("ignored.pyc"), "ignore me").unwrap();
        std::os::unix::fs::symlink("a", tmp.path().join("link")).unwrap();
        let mut bytes = b"a\0".to_vec();
        bytes.extend(&data);
        bytes.extend(b"\0empty\0\0link\0->a\0");
        let hex: String = Sha256::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let expected = format!("sha256:{hex}");
        assert_eq!(hash_directory(tmp.path()).unwrap(), expected);
    }

    #[test]
    fn pool_matches_serial_for_mixed_directories_and_preserves_order() {
        let tmp = crate::ops::DownloadDir::new("parallel-hash-golden").unwrap();
        let mut paths = Vec::new();
        for index in 0..36 {
            let dir = tmp.path().join(format!("dir-{index:02}"));
            std::fs::create_dir(&dir).unwrap();
            std::fs::write(dir.join("empty"), []).unwrap();
            std::fs::write(dir.join("ignored.pyc"), b"ignored").unwrap();
            std::fs::write(dir.join("small"), vec![index as u8; 17]).unwrap();
            std::fs::write(dir.join("block"), vec![index as u8; 65_536]).unwrap();
            std::fs::write(dir.join("large"), vec![index as u8; 150_001]).unwrap();
            std::fs::create_dir(dir.join(".git")).unwrap();
            std::fs::write(dir.join(".git/ignored"), b"ignored").unwrap();
            std::fs::create_dir(dir.join("中文")).unwrap();
            std::fs::write(dir.join("中文/你好"), b"hello").unwrap();
            std::os::unix::fs::symlink("small", dir.join("link")).unwrap();
            paths.push(dir);
        }
        use std::os::unix::ffi::OsStrExt;
        #[cfg(target_os = "linux")]
        std::fs::write(
            paths[0].join(std::ffi::OsStr::from_bytes(b"raw-\xff")),
            b"raw",
        )
        .unwrap();
        std::os::unix::fs::symlink(
            std::ffi::OsStr::from_bytes(b"target-\xff"),
            paths[0].join("raw-link"),
        )
        .unwrap();
        for size in [65_535, 65_537] {
            std::fs::write(paths[0].join(format!("boundary-{size}")), vec![b'z'; size]).unwrap();
        }
        let empty = tmp.path().join("empty-directory");
        std::fs::create_dir(&empty).unwrap();
        paths.push(empty);
        paths.push(paths[3].clone());

        let expected: Vec<_> = paths
            .iter()
            .map(|path| hash_directory(path).unwrap())
            .collect();
        for workers in [0, 1, 2, 32, 64] {
            let actual: Vec<_> = hash_directories(&paths, workers)
                .into_iter()
                .map(Result::unwrap)
                .collect();
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn pool_caps_workers_and_processes_contiguous_batches_once() {
        use std::sync::{Condvar, Mutex, mpsc};
        use std::time::Duration;

        let tmp = crate::ops::DownloadDir::new("parallel-hash-cap").unwrap();
        for index in 0..128 {
            std::fs::write(tmp.path().join(format!("{index:03}")), b"x").unwrap();
        }
        let active = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let batches = AtomicUsize::new(0);
        let entries = AtomicUsize::new(0);
        let (started, ready) = mpsc::channel();
        let release = (Mutex::new(false), Condvar::new());
        let result = std::thread::scope(|scope| {
            let job = scope.spawn(|| {
                hash_directories_with_observer(
                    &[tmp.path().to_path_buf()],
                    64,
                    &|phase, directory, count| {
                        if matches!(phase, PoolPhase::Discover) {
                            assert_eq!(directory, 0);
                        }
                        if matches!(phase, PoolPhase::Batch) {
                            assert_eq!(directory, 0);
                            entries.fetch_add(count, Ordering::Relaxed);
                            let ordinal = batches.fetch_add(1, Ordering::Relaxed);
                            let running = active.fetch_add(1, Ordering::SeqCst) + 1;
                            peak.fetch_max(running, Ordering::SeqCst);
                            if ordinal < 32 {
                                started.send(()).unwrap();
                                drop(
                                    release
                                        .1
                                        .wait_while(release.0.lock().unwrap(), |ready| !*ready)
                                        .unwrap(),
                                );
                            }
                            active.fetch_sub(1, Ordering::SeqCst);
                        }
                    },
                )
            });
            let all_started = (0..32).all(|_| ready.recv_timeout(Duration::from_secs(10)).is_ok());
            *release.0.lock().unwrap() = true;
            release.1.notify_all();
            assert!(all_started, "32 batch jobs should run concurrently");
            job.join().unwrap()
        });
        assert_eq!(
            result[0].as_ref().unwrap(),
            &hash_directory(tmp.path()).unwrap()
        );
        assert_eq!(peak.load(Ordering::SeqCst), 32);
        assert_eq!(batches.load(Ordering::Relaxed), 32);
        assert_eq!(entries.load(Ordering::Relaxed), 128);
    }

    #[test]
    fn directory_errors_do_not_shift_or_cancel_other_results() {
        let tmp = crate::ops::DownloadDir::new("parallel-hash-errors").unwrap();
        let first = tmp.path().join("first");
        let last = tmp.path().join("last");
        std::fs::create_dir(&first).unwrap();
        std::fs::create_dir(&last).unwrap();
        std::fs::write(first.join("one"), b"first").unwrap();
        std::fs::write(last.join("one"), b"last").unwrap();
        let missing = tmp.path().join("missing");
        let results = hash_directories(&[first.clone(), missing, last.clone()], 4);
        assert_eq!(results.len(), 3);
        assert_eq!(
            results[0].as_ref().unwrap(),
            &hash_directory(&first).unwrap()
        );
        assert!(results[1].is_err());
        assert_eq!(
            results[2].as_ref().unwrap(),
            &hash_directory(&last).unwrap()
        );
    }

    #[test]
    fn file_open_error_cancels_only_its_directory() {
        use std::sync::atomic::AtomicBool;

        let tmp = crate::ops::DownloadDir::new("parallel-hash-open-error").unwrap();
        let bad = tmp.path().join("bad");
        let good = tmp.path().join("good");
        std::fs::create_dir(&bad).unwrap();
        std::fs::create_dir(&good).unwrap();
        let removed = bad.join("removed-after-discovery");
        std::fs::write(&removed, b"gone").unwrap();
        std::fs::write(good.join("stable"), b"good").unwrap();
        let deleted = AtomicBool::new(false);
        let results =
            hash_directories_with_observer(&[bad, good.clone()], 2, &|phase, directory, _| {
                if matches!(phase, PoolPhase::Batch)
                    && directory == 0
                    && !deleted.swap(true, Ordering::SeqCst)
                {
                    std::fs::remove_file(&removed).unwrap();
                }
            });
        assert!(results[0].is_err());
        assert_eq!(
            results[1].as_ref().unwrap(),
            &hash_directory(&good).unwrap()
        );
    }

    #[test]
    fn dropped_batch_consumer_cancels_without_blocking_worker() {
        let tmp = crate::ops::DownloadDir::new("parallel-hash-cancel").unwrap();
        let path = tmp.path().join("large");
        std::fs::write(&path, vec![b'x'; BLOCK_SIZE * 3]).unwrap();
        let entries = discover(tmp.path()).unwrap();
        let (output, input) = mpsc::sync_channel(1);
        drop(input);
        read_batch(entries, output, &mut vec![0; BLOCK_SIZE]);
    }

    #[test]
    fn incomplete_batch_stream_is_an_error() {
        let (output, input) = mpsc::sync_channel(1);
        output.send(BatchMessage::Data(vec![b'x'])).unwrap();
        drop(output);
        assert!(consume_batch(&mut Sha256::new(), input).is_err());
    }

    #[test]
    fn worker_panic_joins_the_pool_without_hanging() {
        let tmp = crate::ops::DownloadDir::new("parallel-hash-panic").unwrap();
        for index in 0..12 {
            std::fs::write(tmp.path().join(format!("{index:02}")), b"x").unwrap();
        }
        let panicked = AtomicUsize::new(0);
        let result = std::panic::catch_unwind(|| {
            hash_directories_with_observer(&[tmp.path().to_path_buf()], 2, &|phase, _, _| {
                if matches!(phase, PoolPhase::Batch) && panicked.fetch_add(1, Ordering::SeqCst) == 0
                {
                    panic!("injected hash worker panic");
                }
            })
        });
        assert!(result.is_err());
        assert!(panicked.load(Ordering::SeqCst) >= 2);
    }

    #[test]
    fn empty_input_does_not_start_workers() {
        let calls = AtomicUsize::new(0);
        let results = hash_directories_with_observer(&[], 32, &|_, _, _| {
            calls.fetch_add(1, Ordering::Relaxed);
        });
        assert!(results.is_empty());
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }
}
