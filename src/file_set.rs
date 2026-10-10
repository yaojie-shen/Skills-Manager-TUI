//! Best-effort file-set transactions for one metadata store directory.
//!
//! Every transaction works in its own directory under
//! `.skills-meta/.staging/file-set/`, outside every store, so listing a store
//! never meets transaction files and root sync never commits them. The
//! directory's `manifest.toml` names the target store, the owning process, and
//! the store file behind every `original-N` and `stage-N`. Marker files record
//! the state durably: none (staging), then `publishing` before the first
//! original moves, then `committed` once every output is in place, or `failed`
//! when rollback itself needs manual recovery.
//!
//! Stage everything before moving originals. The journal records only successful
//! filesystem operations, so a failed hold must never delete an untouched source.
//! This is not crash-atomic and does not coordinate unrelated metadata stores.
//! The metadata lock is best effort, so writers are not guaranteed to be
//! serialized: publication revalidates sources and destinations, but retains a
//! check-then-rename race. Rollback claims entries before checking inode ownership.
//!
//! Readers use [`read_store`]: it waits briefly while a live transaction targets
//! the store, removes committed leftovers, refuses interrupted transactions, and
//! accepts a listing only if it is unchanged across the read. A transaction
//! with this process's id is live only while this process is running it, and
//! one whose owner cannot be verified by start time stops being waited for
//! once it is a minute old. Stage files and both directories are synced before
//! the markers that let readers act on them.

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

/// Where transaction directories live, relative to `.skills-meta`.
pub(crate) const TRANSACTION_DIR: &str = ".staging/file-set";
const MANIFEST: &str = "manifest.toml";
const PUBLISHING: &str = "publishing";
const COMMITTED: &str = "committed";
const FAILED: &str = "failed";
/// How long a reader waits for a live transaction on its store.
const READ_WAIT: Duration = Duration::from_secs(2);
const READ_POLL: Duration = Duration::from_millis(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FailureKind {
    RolledBack,
    Committed { leftovers: Vec<PathBuf> },
    ManualRecovery { paths: Vec<PathBuf> },
}

#[derive(Debug)]
pub(crate) struct PublishError {
    pub kind: FailureKind,
    message: String,
    source: anyhow::Error,
}

impl std::fmt::Display for PublishError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for PublishError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

pub(crate) struct File<'a> {
    pub path: &'a Path,
    pub bytes: &'a [u8],
}

/// The store a transaction publishes into.
#[derive(Debug, Clone)]
pub(crate) struct Target {
    /// Directory holding the transaction directories.
    pub transactions: PathBuf,
    /// The store directory every published path lives in.
    pub store: PathBuf,
    /// The store as recorded in manifests, relative to the Library root.
    pub label: String,
}

impl Target {
    pub(crate) fn store(meta: &Path, name: &str) -> Self {
        Self {
            transactions: meta.join(TRANSACTION_DIR),
            store: meta.join(name),
            label: format!("{}/{name}", crate::paths::META_DIR),
        }
    }

    fn relative(&self, path: &Path) -> String {
        path.strip_prefix(&self.store)
            .unwrap_or(path)
            .display()
            .to_string()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    Create,
    Write,
    WriteRemainder,
    Revalidate,
    Hold,
    Publish,
    Cleanup,
    RemovePublished,
    RestoreClaim,
    Restore,
    Link,
    RemoveStage,
    Mark,
    Marked,
    Discard,
    Sync,
}

#[cfg(test)]
type Hook = Box<dyn FnMut(Step, usize, &Path) -> std::io::Result<()>>;
#[cfg(test)]
thread_local! {
    static HOOK: std::cell::RefCell<Option<Hook>> = const { std::cell::RefCell::new(None) };
}

fn checkpoint(step: Step, index: usize, path: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    return HOOK.with_borrow_mut(|hook| match hook {
        Some(hook) => hook(step, index, path),
        None => Ok(()),
    });
    #[cfg(not(test))]
    {
        let _ = (step, index, path);
        Ok(())
    }
}

#[cfg(test)]
pub(crate) fn with_hook<T>(hook: Hook, run: impl FnOnce() -> T) -> T {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            HOOK.with_borrow_mut(|hook| *hook = None);
        }
    }
    HOOK.with_borrow_mut(|current| {
        assert!(current.is_none());
        *current = Some(hook);
    });
    let _reset = Reset;
    run()
}

/// What a transaction directory holds, written once before it becomes visible.
#[derive(Debug, Serialize, Deserialize)]
struct Manifest {
    store: String,
    pid: u32,
    /// Owner start time, to tell a reused process id from the owner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    start: Option<u64>,
    /// `original-N` → the store file whose previous contents it holds.
    #[serde(default)]
    originals: BTreeMap<String, String>,
    /// `stage-N` → the store file it publishes.
    #[serde(default)]
    stages: BTreeMap<String, String>,
}

const MANIFEST_HEADER: &str = "# Skills Manager metadata transaction. original-N holds the previous contents\n# of the listed store file; stage-N holds new contents to publish there.\n";

/// Start time of a process in clock ticks since boot, where the platform
/// exposes it cheaply. Also reports zombies as gone.
fn process_start(pid: u32) -> Option<std::result::Result<u64, ()>> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let fields: Vec<_> = stat
        .get(stat.rfind(')')? + 1..)?
        .split_whitespace()
        .collect();
    if matches!(fields.first(), Some(&"Z" | &"X")) {
        return Some(Err(()));
    }
    fields.get(19)?.parse().ok().map(Ok)
}

fn owner_alive(pid: u32, start: Option<u64>) -> bool {
    let Ok(raw) = libc::pid_t::try_from(pid) else {
        return false;
    };
    if raw <= 0 {
        return false;
    }
    // SAFETY: signal 0 only checks that the process exists.
    let exists = unsafe { libc::kill(raw, 0) } == 0
        || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
    if !exists {
        return false;
    }
    match (start, process_start(pid)) {
        (_, Some(Err(()))) => false,
        (Some(recorded), Some(Ok(current))) => recorded == current,
        _ => true,
    }
}

/// Transactions this process is running, by directory name. A transaction
/// carrying this process's id but missing here was abandoned by this process
/// (for example by a panic), so readers never wait on themselves.
static ACTIVE: std::sync::Mutex<std::collections::BTreeSet<String>> =
    std::sync::Mutex::new(std::collections::BTreeSet::new());

fn active() -> std::sync::MutexGuard<'static, std::collections::BTreeSet<String>> {
    ACTIVE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Registration of one running transaction, removed when publication returns.
struct Running(String);

impl Drop for Running {
    fn drop(&mut self) {
        active().remove(&self.0);
    }
}

/// Whether the owner of transaction `id` may still be writing it.
fn owner_running(id: &str, pid: u32, start: Option<u64>) -> bool {
    if pid == std::process::id() {
        active().contains(id)
    } else {
        owner_alive(pid, start)
    }
}

fn sync_dir(dir: &Path) -> std::io::Result<()> {
    fs::File::open(dir)?.sync_all()
}

fn write_new(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// Record a state durably. Once `committed` exists any reader may remove the
/// whole leftover directory, so its directory sync may find it already gone;
/// everything it guarded was synced before the marker was created.
fn mark(work: &Path, state: &str, index: usize) -> std::result::Result<(), String> {
    let path = work.join(state);
    checkpoint(Step::Mark, index, &path)
        .and_then(|()| write_new(&path, b""))
        .map_err(|error| format!("creating {}: {error}", path.display()))?;
    match checkpoint(Step::Marked, index, &path).and_then(|()| sync_dir(work)) {
        Err(error) if state == COMMITTED && error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other.map_err(|error| format!("syncing {}: {error}", work.display())),
    }
}

fn remove_if_present(path: &Path) -> std::io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

fn nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// Create a transaction directory and its manifest under a hidden name, then
/// rename it into view so readers never see a directory without a manifest.
fn transaction_dir(target: &Target, manifest: &Manifest) -> Result<(PathBuf, Running)> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    fs::create_dir_all(&target.transactions).with_context(|| {
        format!(
            "creating transaction directory {}",
            target.transactions.display()
        )
    })?;
    let text = format!("{MANIFEST_HEADER}{}", toml::to_string(manifest)?);
    loop {
        let id = format!(
            "{}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
            nanos()
        );
        active().insert(id.clone());
        let running = Running(id.clone());
        let pending = target.transactions.join(format!(".new-{id}"));
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700);
        match builder.create(&pending) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("creating transaction directory {}", pending.display())
                });
            }
        }
        let work = target.transactions.join(&id);
        let result = write_new(&pending.join(MANIFEST), text.as_bytes())
            .and_then(|()| fs::rename(&pending, &work))
            .and_then(|()| sync_dir(&target.transactions));
        if let Err(error) = result {
            for directory in [&pending, &work] {
                let _ = remove_if_present(&directory.join(MANIFEST));
                let _ = fs::remove_dir(directory);
            }
            return Err(error)
                .with_context(|| format!("creating transaction directory {}", work.display()));
        }
        return Ok((work, running));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EntryType {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Identity {
    device: u64,
    inode: u64,
    entry_type: EntryType,
}

fn identity(metadata: &fs::Metadata) -> Identity {
    let kind = metadata.file_type();
    Identity {
        device: metadata.dev(),
        inode: metadata.ino(),
        entry_type: if kind.is_file() {
            EntryType::File
        } else if kind.is_dir() {
            EntryType::Directory
        } else if kind.is_symlink() {
            EntryType::Symlink
        } else {
            EntryType::Other
        },
    }
}
fn link_unsupported(error: &std::io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(
            libc::EPERM
                | libc::EACCES
                | libc::ENOTSUP
                | libc::EXDEV
                | libc::ENOSYS
                | libc::EMLINK
                | libc::EROFS
        )
    )
}

/// Move `source` into an absent `destination` without clobbering. Hard links
/// provide the atomic no-clobber operation. Filesystems that reject links use
/// check-then-rename, retaining that unavoidable compatibility race.
fn restore_no_clobber(source: &Path, destination: &Path, index: usize) -> std::io::Result<()> {
    let linked = checkpoint(Step::Link, index, destination)
        .and_then(|()| fs::hard_link(source, destination));
    match linked {
        Ok(()) => fs::remove_file(source),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "restore destination is occupied",
            ))
        }
        Err(error) if link_unsupported(&error) => {
            match fs::symlink_metadata(destination) {
                Err(absent) if absent.kind() == std::io::ErrorKind::NotFound => {}
                Err(other) => return Err(other),
                Ok(_) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::AlreadyExists,
                        "restore destination is occupied",
                    ));
                }
            }
            fs::rename(source, destination)
        }
        Err(error) => Err(error),
    }
}

fn unique_claim_dir(work: &Path, index: usize) -> std::io::Result<PathBuf> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    loop {
        let path = work.join(format!(
            "claim-{index}-{}",
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
}

fn has_identity(path: &Path, expected: Identity) -> std::io::Result<bool> {
    fs::symlink_metadata(path).map(|metadata| identity(&metadata) == expected)
}

fn claim_no_clobber(source: &Path, claim: &Path) -> std::io::Result<()> {
    match fs::hard_link(source, claim) {
        Ok(()) => fs::remove_file(source),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Err(error),
        Err(error) if link_unsupported(&error) => {
            match fs::symlink_metadata(claim) {
                Err(absent) if absent.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
                Ok(_) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::AlreadyExists,
                        "claim destination is occupied",
                    ));
                }
            }
            fs::rename(source, claim)
        }
        Err(error) => Err(error),
    }
}

fn unchanged(file: &File<'_>) -> Result<()> {
    ensure!(
        fs::read(file.path).with_context(|| format!("revalidating {}", file.path.display()))?
            == file.bytes,
        "{} changed during this operation",
        file.path.display()
    );
    Ok(())
}

/// Callers validate names/destinations and create the store directory first.
/// Originals are revalidated after staging, then again immediately before each
/// hold. A cleanup error means the new files ARE committed; never roll them back.
///
/// The transaction directory is mode 0700. Same-user processes that can still
/// write there are detected by inode/type identity checks where publication or
/// cleanup would otherwise trust a transaction pathname.
pub(crate) fn publish(
    target: &Target,
    before: &[File<'_>],
    desired: &[File<'_>],
) -> std::result::Result<(), PublishError> {
    struct Stage {
        path: PathBuf,
        _handle: fs::File,
        identity: Identity,
    }
    struct Published<'a> {
        index: usize,
        path: &'a Path,
        identity: Identity,
    }

    let manifest = Manifest {
        store: target.label.clone(),
        pid: std::process::id(),
        start: process_start(std::process::id()).and_then(Result::ok),
        originals: before
            .iter()
            .enumerate()
            .map(|(index, file)| (format!("original-{index}"), target.relative(file.path)))
            .collect(),
        stages: desired
            .iter()
            .enumerate()
            .map(|(index, file)| (format!("stage-{index}"), target.relative(file.path)))
            .collect(),
    };
    let (work, _running) = transaction_dir(target, &manifest).map_err(|source| PublishError {
        kind: FailureKind::RolledBack,
        message: "file-set publication failed before staging".into(),
        source,
    })?;
    let mut stages = Vec::new();
    let mut held: Vec<(PathBuf, &Path)> = Vec::new();
    let mut published = Vec::new();
    let result = (|| -> Result<()> {
        for (index, file) in desired.iter().enumerate() {
            let stage = work.join(format!("stage-{index}"));
            checkpoint(Step::Create, index, &stage)?;
            let writer = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&stage)
                .with_context(|| format!("creating stage {}", stage.display()))?;
            let stage_identity = identity(&writer.metadata()?);
            // Journal immediately after creation, while retaining the open handle
            // to prevent its inode from being reused during rollback.
            stages.push(Stage {
                path: stage,
                _handle: writer,
                identity: stage_identity,
            });
            let stage = stages.last_mut().expect("stage was just pushed");
            checkpoint(Step::Write, index, &stage.path)?;
            let split = file.bytes.len() / 2;
            stage._handle.write_all(&file.bytes[..split])?;
            checkpoint(Step::WriteRemainder, index, &stage.path)?;
            stage._handle.write_all(&file.bytes[split..])?;
        }
        // New contents must be durable before any original moves.
        for stage in &stages {
            checkpoint(Step::Sync, 0, &stage.path)?;
            stage
                ._handle
                .sync_all()
                .with_context(|| format!("syncing {}", stage.path.display()))?;
        }
        checkpoint(Step::Revalidate, 0, &work)?;
        for file in before {
            unchanged(file)?;
        }
        mark(&work, PUBLISHING, 0)
            .map_err(anyhow::Error::msg)
            .context("recording transaction state")?;
        for (index, file) in before.iter().enumerate() {
            let hold = work.join(format!("original-{index}"));
            checkpoint(Step::Hold, index, file.path)?;
            unchanged(file)?;
            fs::rename(file.path, &hold).with_context(|| {
                format!("holding {} at {}", file.path.display(), hold.display())
            })?;
            held.push((hold, file.path));
        }
        for (index, (stage, file)) in stages.iter().zip(desired).enumerate() {
            checkpoint(Step::Publish, index, file.path)?;
            ensure!(
                has_identity(&stage.path, stage.identity)?,
                "staged file identity changed at {}",
                stage.path.display()
            );
            match fs::symlink_metadata(file.path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
                Ok(_) => anyhow::bail!(
                    "destination {} appeared during publication",
                    file.path.display()
                ),
            }
            fs::rename(&stage.path, file.path)
                .with_context(|| format!("publishing {}", file.path.display()))?;
            published.push(Published {
                index,
                path: file.path,
                identity: stage.identity,
            });
        }
        Ok(())
    })();

    if let Err(error) = result {
        let mut recovery = Vec::new();
        for publication in published.iter().rev() {
            let index = publication.index;
            let path = publication.path;
            let claim = unique_claim_dir(&work, index).map(|directory| directory.join("entry"));
            let claimed = match &claim {
                Ok(claim) => checkpoint(Step::RemovePublished, index, path)
                    .and_then(|()| claim_no_clobber(path, claim)),
                Err(error) => Err(std::io::Error::new(error.kind(), error.to_string())),
            };
            let claim =
                claim.unwrap_or_else(|_| work.join(format!("claim-{index}-unavailable/entry")));
            match claimed {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => recovery.push(format!(
                    "claim published {} at {}: {error}",
                    path.display(),
                    claim.display()
                )),
                Ok(()) => match fs::symlink_metadata(&claim) {
                    Ok(metadata) if identity(&metadata) == publication.identity => {
                        if let Err(error) = fs::remove_file(&claim) {
                            recovery.push(format!(
                                "remove published claim {}: {error}",
                                claim.display()
                            ));
                        }
                    }
                    Ok(_) => {
                        let restored = checkpoint(Step::RestoreClaim, index, path)
                            .and_then(|()| restore_no_clobber(&claim, path, index));
                        if let Err(error) = restored {
                            recovery.push(format!(
                                "restore foreign {} from {}: {error}",
                                path.display(),
                                claim.display()
                            ));
                        }
                    }
                    Err(error) => recovery.push(format!(
                        "inspect published claim {} for {}: {error}",
                        claim.display(),
                        path.display()
                    )),
                },
            }
            if let Some(directory) = claim.parent()
                && let Err(error) = fs::remove_dir(directory)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                recovery.push(format!(
                    "remove claim directory {}: {error}",
                    directory.display()
                ));
            }
        }
        for (index, (hold, original)) in held.iter().enumerate().rev() {
            let restore = checkpoint(Step::Restore, index, hold)
                .and_then(|()| restore_no_clobber(hold, original, index));
            if let Err(error) = restore {
                recovery.push(format!(
                    "restore {} from {}: {error}",
                    original.display(),
                    hold.display()
                ));
            }
        }
        for (index, stage) in stages.iter().enumerate() {
            let removal =
                checkpoint(Step::RemoveStage, index, &stage.path).and_then(
                    |()| match has_identity(&stage.path, stage.identity) {
                        Ok(true) => fs::remove_file(&stage.path),
                        Ok(false) => Err(std::io::Error::other(
                            "staged file identity changed; foreign entry retained",
                        )),
                        Err(error) => Err(error),
                    },
                );
            if let Err(error) = removal
                && error.kind() != std::io::ErrorKind::NotFound
            {
                recovery.push(format!("remove stage {}: {error}", stage.path.display()));
            }
        }
        if recovery.is_empty() {
            let removed = [MANIFEST, PUBLISHING]
                .into_iter()
                .try_for_each(|name| remove_if_present(&work.join(name)))
                .and_then(|()| fs::remove_dir(&work));
            if let Err(error) = removed {
                recovery.push(format!(
                    "transaction files retained at {}: {error}",
                    work.display()
                ));
            }
        }
        if !recovery.is_empty()
            && let Err(error) = mark(&work, FAILED, 2)
        {
            recovery.push(format!(
                "record failed transaction at {}: {error}",
                work.display()
            ));
        }
        let (kind, message) = if recovery.is_empty() {
            (
                FailureKind::RolledBack,
                "file-set publication failed; held originals restored and stages removed"
                    .to_owned(),
            )
        } else {
            (
                FailureKind::ManualRecovery {
                    paths: vec![work.clone()],
                },
                format!(
                    "file-set publication failed; manual recovery required: {}",
                    recovery.join("; ")
                ),
            )
        };
        return Err(PublishError {
            kind,
            message,
            source: error,
        });
    }

    // Commit has completed. Failures below leave outputs in place; readers
    // remove a committed leftover. They must never enter rollback. A reader may
    // already be removing it, so absent entries are not failures.
    let mut cleanup = Vec::new();
    // Every rename into the store and into `work` must be durable before
    // `committed` is: readers delete the held originals of a committed
    // transaction. Without that, keep the originals and report a leftover.
    let moved = !held.is_empty() || !published.is_empty();
    let durable = checkpoint(Step::Sync, 1, &target.store)
        .and_then(|()| {
            if moved {
                sync_dir(&target.store)
            } else {
                Ok(())
            }
        })
        .map_err(|error| format!("syncing {}: {error}", target.store.display()))
        .and_then(|()| {
            sync_dir(&work).map_err(|error| format!("syncing {}: {error}", work.display()))
        })
        .and_then(|()| mark(&work, COMMITTED, 1));
    if let Err(error) = durable {
        let kept = if work.exists() {
            format!("the previous files were kept in {}", work.display())
        } else {
            format!("{} is gone", work.display())
        };
        return Err(PublishError {
            kind: FailureKind::Committed {
                leftovers: vec![work.clone()],
            },
            message: format!(
                "file-set outputs were published, but could not be confirmed on disk ({error}); {kept}"
            ),
            source: anyhow::anyhow!("transaction durability failed"),
        });
    }
    for (index, (hold, _)) in held.iter().enumerate() {
        if let Err(error) =
            checkpoint(Step::Cleanup, index, hold).and_then(|()| remove_if_present(hold))
        {
            cleanup.push(format!("{}: {error}", hold.display()));
        }
    }
    if cleanup.is_empty() {
        let removed = [MANIFEST, PUBLISHING, COMMITTED]
            .into_iter()
            .try_for_each(|name| remove_if_present(&work.join(name)))
            .and_then(|()| match fs::remove_dir(&work) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                other => other,
            });
        if let Err(error) = removed {
            cleanup.push(format!("{}: {error}", work.display()));
        }
    }
    if cleanup.is_empty() {
        Ok(())
    } else {
        Err(PublishError {
            kind: FailureKind::Committed {
                leftovers: vec![work.clone()],
            },
            message: format!(
                "file-set outputs were published, but old transaction files could not be removed: {}",
                cleanup.join("; ")
            ),
            source: anyhow::anyhow!("transaction cleanup failed"),
        })
    }
}

/// Publish only the files whose path or bytes change. A committed publication
/// whose cleanup failed is a success: its outputs are live, so the caller
/// records the change; leftovers are retried here and otherwise removed by the
/// next reader, with a warning.
pub(crate) fn publish_changes<'a>(
    target: &Target,
    before: &[File<'a>],
    desired: &[File<'a>],
) -> std::result::Result<(), PublishError> {
    let same =
        |left: &File<'_>, right: &File<'_>| left.path == right.path && left.bytes == right.bytes;
    let copy = |file: &File<'a>| File {
        path: file.path,
        bytes: file.bytes,
    };
    let changed_before: Vec<_> = before
        .iter()
        .filter(|file| !desired.iter().any(|other| same(file, other)))
        .map(copy)
        .collect();
    let changed_desired: Vec<_> = desired
        .iter()
        .filter(|file| !before.iter().any(|other| same(file, other)))
        .map(copy)
        .collect();
    let (before, desired) = (changed_before, changed_desired);
    if before.is_empty() && desired.is_empty() {
        return Ok(());
    }
    match publish(target, &before, &desired) {
        // Outputs that could not be confirmed on disk keep their originals.
        Err(
            error @ PublishError {
                kind: FailureKind::Committed { .. },
                ..
            },
        ) if matches!(&error.kind, FailureKind::Committed { leftovers }
            if leftovers.iter().any(|work| !work.join(COMMITTED).exists())) =>
        {
            Err(error)
        }
        Err(PublishError {
            kind: FailureKind::Committed { leftovers },
            message,
            ..
        }) => {
            let remaining: Vec<_> = leftovers
                .iter()
                .filter(|work| discard(work).is_err())
                .map(|work| work.display().to_string())
                .collect();
            if !remaining.is_empty() {
                crate::warnings::push(format!(
                    "changes to {} were saved; {message}; the next read removes {}",
                    target.label,
                    remaining.join(", ")
                ));
            }
            Ok(())
        }
        other => other,
    }
}

fn transaction_entry(name: &str) -> bool {
    [MANIFEST, PUBLISHING, COMMITTED, FAILED].contains(&name)
        || ["original-", "stage-"].iter().any(|prefix| {
            name.strip_prefix(prefix)
                .is_some_and(|index| !index.is_empty() && index.bytes().all(|b| b.is_ascii_digit()))
        })
}

/// Remove a transaction directory that no longer guards anything. Only entries
/// a transaction creates are removed, so foreign files keep the directory.
fn discard(work: &Path) -> std::io::Result<()> {
    checkpoint(Step::Discard, 0, work)?;
    let entries = match fs::read_dir(work) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let mut markers = Vec::new();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str().filter(|name| transaction_entry(name)) else {
            continue;
        };
        if [PUBLISHING, COMMITTED, FAILED].contains(&name) {
            // Markers go last so an interrupted discard keeps its state.
            markers.push(entry.path());
        } else {
            remove_if_present(&entry.path())?;
        }
    }
    for marker in [PUBLISHING, FAILED, COMMITTED] {
        if let Some(path) = markers.iter().find(|path| path.ends_with(marker)) {
            remove_if_present(path)?;
        }
    }
    match fs::remove_dir(work) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// Whether any original was moved out of the store into `work`.
fn holds_originals(work: &Path) -> Result<bool> {
    let entries = match fs::read_dir(work) {
        Ok(entries) => entries,
        // Another reader already removed it.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        if entry?
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with("original-"))
        {
            return Ok(true);
        }
    }
    Ok(false)
}

enum Pending {
    Clear,
    Busy(u32, PathBuf),
}

fn unfinished(work: &Path, manifest: &Manifest, failed: bool) -> anyhow::Error {
    let how = if failed {
        format!(
            "process {} could not roll back a metadata transaction",
            manifest.pid
        )
    } else {
        format!(
            "process {} did not finish replacing metadata files",
            manifest.pid
        )
    };
    anyhow::anyhow!(
        "{how} and left {work}; {store} may mix previous and new files. In {work}, {MANIFEST} maps each original-N (previous contents) and stage-N (new contents not yet published) to its file in {store}, and claim-* directories hold files found at published paths during rollback. Move the files you want into {store}, then remove {work}",
        work = work.display(),
        store = manifest.store,
    )
}

/// A live transaction this old whose owner cannot be verified by start time
/// is more likely a crashed writer whose process id was reused.
const UNVERIFIED_OWNER_LIMIT: Duration = Duration::from_secs(60);

/// The age of a transaction whose owner's identity cannot be confirmed (no
/// recorded or readable start time), once it exceeds the limit.
fn unverifiable_age(work: &Path, manifest: &Manifest) -> Option<Duration> {
    if manifest.pid == std::process::id()
        || (manifest.start.is_some() && matches!(process_start(manifest.pid), Some(Ok(_))))
    {
        return None;
    }
    let created = fs::metadata(work.join(MANIFEST)).ok()?.modified().ok()?;
    let age = created.elapsed().ok()?;
    (age > UNVERIFIED_OWNER_LIMIT).then_some(age)
}

fn unverifiable(work: &Path, manifest: &Manifest, age: Duration) -> anyhow::Error {
    anyhow::anyhow!(
        "{store} has a metadata transaction at {work} that process {pid} started {secs} seconds ago; a process with that id is running, but Skills Manager cannot confirm it is the writer. If no Skills Manager is writing this Library, the writer was interrupted: in {work}, {MANIFEST} maps each original-N (previous contents) and stage-N (new contents not yet published) to its file in {store}; move the files you want into {store}, then remove {work}",
        store = manifest.store,
        work = work.display(),
        pid = manifest.pid,
        secs = age.as_secs(),
    )
}

/// Settle the transactions that target `target`'s store: committed leftovers
/// and abandoned staging are removed, live ones reported, interrupted ones
/// refused.
fn settle(target: &Target) -> Result<Pending> {
    let entries = match fs::read_dir(&target.transactions) {
        Ok(entries) => entries,
        // No transaction can exist where the directory cannot.
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(Pending::Clear);
        }
        Err(error) => return Err(error.into()),
    };
    let mut pending = Pending::Clear;
    for entry in entries {
        let entry = entry?;
        let work = entry.path();
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if let Some(id) = name.strip_prefix(".new-") {
            // A directory whose creator died before publishing it.
            let owner = id.split('-').next().and_then(|pid| pid.parse().ok());
            if owner.is_some_and(|pid| !owner_running(id, pid, None)) {
                let _ = discard(&work);
            }
            continue;
        }
        if name.starts_with('.') {
            continue;
        }
        if work.join(COMMITTED).exists() {
            let _ = discard(&work);
            continue;
        }
        let text = match fs::read_to_string(work.join(MANIFEST)) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // Only a cleanup remnant lacks its manifest; anything still
                // holding transaction files cannot be attributed to a store.
                if discard(&work).is_err() && work.exists() {
                    bail!(
                        "an unfinished metadata transaction left {} without its {MANIFEST}; move the files you need back into .skills-meta, then remove the directory",
                        work.display()
                    );
                }
                continue;
            }
            Err(error) => {
                return Err(error).with_context(|| format!("reading {}", work.display()));
            }
        };
        let manifest: Manifest = toml::from_str(&text)
            .with_context(|| format!("invalid transaction manifest in {}", work.display()))?;
        if manifest.store != target.label {
            continue;
        }
        if work.join(FAILED).exists() {
            return Err(unfinished(&work, &manifest, true));
        }
        if owner_running(&name, manifest.pid, manifest.start) {
            if let Some(age) = unverifiable_age(&work, &manifest) {
                return Err(unverifiable(&work, &manifest, age));
            }
            pending = Pending::Busy(manifest.pid, work);
            continue;
        }
        if work.join(PUBLISHING).exists() || holds_originals(&work)? {
            return Err(unfinished(&work, &manifest, false));
        }
        // The owner died while staging, before touching the store.
        if discard(&work).is_err() && work.exists() {
            return Err(unfinished(&work, &manifest, false));
        }
    }
    Ok(pending)
}

type Listing = Vec<(PathBuf, Identity)>;

/// List a store's documents. Stray names are skipped; document names that are
/// not regular files, or not UTF-8, are errors.
fn list_store(
    dir: &Path,
    kind: &str,
    is_document: &dyn Fn(&OsStr) -> bool,
) -> Result<Option<Listing>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Some(Vec::new())),
        Err(error) => return Err(error.into()),
    };
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        crate::util::reject_interrupted_transaction(&path, &file_type)?;
        if !is_document(&entry.file_name()) {
            continue;
        }
        ensure!(
            file_type.is_file() && !file_type.is_symlink(),
            "invalid {kind} store entry: {}",
            path.display()
        );
        ensure!(
            entry.file_name().to_str().is_some(),
            "{kind} filename is not valid UTF-8: {}",
            path.display()
        );
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        out.push((path, identity(&metadata)));
    }
    out.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(Some(out))
}

enum Attempt {
    Done(Vec<(PathBuf, Vec<u8>)>),
    Busy(u32, PathBuf),
    Changed,
}

/// Admission check run before each document is read: its path, its size, and
/// the documents already read in this attempt.
pub(crate) type Admit<'a> = dyn FnMut(&Path, u64, &[(PathBuf, Vec<u8>)]) -> Result<()> + 'a;

fn read_attempt(
    target: &Target,
    kind: &str,
    is_document: &dyn Fn(&OsStr) -> bool,
    admit: &mut Admit<'_>,
) -> Result<Attempt> {
    if let Pending::Busy(pid, work) = settle(target)? {
        return Ok(Attempt::Busy(pid, work));
    }
    let Some(listing) = list_store(&target.store, kind, is_document)? else {
        return Ok(Attempt::Changed);
    };
    let mut files = Vec::with_capacity(listing.len());
    for (path, expected) in &listing {
        let mut file = match fs::File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Attempt::Changed);
            }
            Err(error) => {
                return Err(error).with_context(|| format!("reading {}", path.display()));
            }
        };
        let metadata = file.metadata()?;
        if identity(&metadata) != *expected {
            return Ok(Attempt::Changed);
        }
        admit(path, metadata.len(), &files)?;
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        file.read_to_end(&mut bytes)
            .with_context(|| format!("reading {}", path.display()))?;
        files.push((path.clone(), bytes));
    }
    if let Pending::Busy(pid, work) = settle(target)? {
        return Ok(Attempt::Busy(pid, work));
    }
    if list_store(&target.store, kind, is_document)?.as_ref() != Some(&listing) {
        return Ok(Attempt::Changed);
    }
    Ok(Attempt::Done(files))
}

/// Read every document of the store `name` under `meta` as one consistent set,
/// sorted by path. `is_document` selects document names; other entries are
/// ignored and never touched.
pub(crate) fn read_store(
    meta: &Path,
    name: &str,
    kind: &str,
    is_document: &dyn Fn(&OsStr) -> bool,
    admit: &mut Admit<'_>,
) -> Result<Vec<(PathBuf, Vec<u8>)>> {
    let target = Target::store(meta, name);
    let deadline = Instant::now() + READ_WAIT;
    loop {
        let attempt = read_attempt(&target, kind, is_document, admit)?;
        let expired = Instant::now() >= deadline;
        match attempt {
            Attempt::Done(files) => return Ok(files),
            Attempt::Busy(pid, work) if expired => bail!(
                "{} is being written by process {pid} (transaction {}); retry in a moment",
                target.label,
                work.display()
            ),
            Attempt::Changed if expired => bail!(
                "{} kept changing while it was read; retry in a moment",
                target.label
            ),
            Attempt::Busy(..) | Attempt::Changed => std::thread::sleep(READ_POLL),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ops::DownloadDir,
        preset::{Preset, PresetStore},
        tag::{Tag, TagStore},
    };
    use std::collections::BTreeMap;

    fn error_chain(error: &PublishError) -> String {
        format!("{error}: {:#}", error.source)
    }

    fn store(temp: &DownloadDir) -> PathBuf {
        let dir = temp.path().join("store");
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn target(temp: &DownloadDir) -> Target {
        Target::store(temp.path(), "store")
    }

    fn work_dir(temp: &DownloadDir) -> PathBuf {
        let mut found: Vec<_> = fs::read_dir(temp.path().join(TRANSACTION_DIR))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(found.len(), 1, "{found:?}");
        found.pop().unwrap()
    }

    /// Transaction files in a work directory, without manifest and markers.
    fn transaction_files(work: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        fs::read_dir(work)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                let name = path.file_name().unwrap().to_str().unwrap();
                ![MANIFEST, PUBLISHING, COMMITTED, FAILED].contains(&name)
            })
            .map(|path| (path.file_name().unwrap().into(), fs::read(path).unwrap()))
            .collect()
    }

    fn files<'a>(paths: &'a [PathBuf], bytes: &'a [Vec<u8>]) -> Vec<File<'a>> {
        paths
            .iter()
            .zip(bytes)
            .map(|(path, bytes)| File { path, bytes })
            .collect()
    }

    fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        fs::read_dir(dir)
            .unwrap()
            .map(|entry| {
                let path = entry.unwrap().path();
                assert!(
                    path.is_file(),
                    "unexpected transaction debris: {}",
                    path.display()
                );
                (path.file_name().unwrap().into(), fs::read(path).unwrap())
            })
            .collect()
    }

    fn failure(step: Step, index: usize) -> Hook {
        Box::new(move |actual, position, _| {
            if (actual, position) == (step, index) {
                Err(std::io::Error::other(format!("injected {step:?} {index}")))
            } else {
                Ok(())
            }
        })
    }

    #[test]
    fn publish_error_chain_prints_the_cause_once() {
        let temp = DownloadDir::new("file-set-error-chain").unwrap();
        let destination = store(&temp).join("destination");
        let error = with_hook(failure(Step::Create, 0), || {
            publish(
                &target(&temp),
                &[],
                &[File {
                    path: &destination,
                    bytes: b"new",
                }],
            )
        })
        .unwrap_err();
        let error: anyhow::Error = error.into();
        let error = error.context("publishing test files");
        let text = format!("{error:#}");
        assert_eq!(text.matches("injected Create 0").count(), 1, "{text}");
    }

    #[test]
    fn every_stage_hold_and_publish_failure_restores_exact_file_set() {
        for step in [
            Step::Create,
            Step::Write,
            Step::WriteRemainder,
            Step::Hold,
            Step::Publish,
        ] {
            for index in 0..3 {
                let temp = DownloadDir::new("file-set-failure").unwrap();
                let paths: Vec<_> = ["a", "b", "c"].map(|name| store(&temp).join(name)).into();
                let old: Vec<_> = [
                    b"original a".to_vec(),
                    b"original b".to_vec(),
                    b"original c".to_vec(),
                ]
                .into();
                let new: Vec<_> = [b"new a".to_vec(), b"new b".to_vec(), b"new c".to_vec()].into();
                for (path, bytes) in paths.iter().zip(&old) {
                    fs::write(path, bytes).unwrap();
                }
                fs::write(store(&temp).join("untouched"), b"external").unwrap();
                let original = snapshot(&store(&temp));
                // Overlapping targets recreate the original partial-hold data-loss bug.
                let error = with_hook(failure(step, index), || {
                    publish(&target(&temp), &files(&paths, &old), &files(&paths, &new))
                })
                .unwrap_err();
                assert!(error_chain(&error).contains("injected"));
                assert_eq!(snapshot(&store(&temp)), original, "{step:?} at {index}");
            }
        }
    }

    #[test]
    fn create_new_collision_never_claims_or_deletes_foreign_stage() {
        let temp = DownloadDir::new("file-set-collision").unwrap();
        let destination = store(&temp).join("new");
        let captured = std::rc::Rc::new(std::cell::RefCell::new(PathBuf::new()));
        let output = captured.clone();
        let error = with_hook(
            Box::new(move |step, _, path| {
                if step == Step::Create {
                    fs::write(path, b"foreign")?;
                    *output.borrow_mut() = path.to_path_buf();
                }
                Ok(())
            }),
            || {
                publish(
                    &target(&temp),
                    &[],
                    &[File {
                        path: &destination,
                        bytes: b"ours",
                    }],
                )
            },
        )
        .unwrap_err();
        assert_eq!(fs::read(&*captured.borrow()).unwrap(), b"foreign");
        assert!(!destination.exists());
        assert!(error_chain(&error).contains("manual recovery"));
    }

    #[test]
    fn rename_cycle_and_case_only_name_commit_without_debris() {
        let temp = DownloadDir::new("file-set-cycle").unwrap();
        let paths: Vec<_> = ["one", "two", "three"]
            .map(|name| store(&temp).join(name))
            .into();
        let bytes: Vec<_> = [b"first".to_vec(), b"second".to_vec(), b"third".to_vec()].into();
        for (path, bytes) in paths.iter().zip(&bytes) {
            fs::write(path, bytes).unwrap();
        }
        let targets = vec![paths[1].clone(), paths[2].clone(), paths[0].clone()];
        publish(
            &target(&temp),
            &files(&paths, &bytes),
            &files(&targets, &bytes),
        )
        .unwrap();
        for (path, bytes) in targets.iter().zip(&bytes) {
            assert_eq!(fs::read(path).unwrap(), *bytes);
        }
        let upper = store(&temp).join("ONE");
        publish(
            &target(&temp),
            &[File {
                path: &paths[0],
                bytes: &bytes[2],
            }],
            &[File {
                path: &upper,
                bytes: &bytes[2],
            }],
        )
        .unwrap();
        let state = snapshot(&store(&temp));
        assert_eq!(state.len(), 3);
        assert!(state.contains_key(Path::new("ONE")));
        assert!(!state.contains_key(Path::new("one")));
    }

    #[test]
    fn cleanup_failure_at_each_hold_preserves_committed_outputs() {
        for index in 0..3 {
            let temp = DownloadDir::new("file-set-cleanup").unwrap();
            let paths: Vec<_> = ["a", "b", "c"].map(|name| store(&temp).join(name)).into();
            let old = vec![b"old".to_vec(); 3];
            let new = vec![b"committed".to_vec(); 3];
            for path in &paths {
                fs::write(path, b"old").unwrap();
            }
            let error = with_hook(failure(Step::Cleanup, index), || {
                publish(&target(&temp), &files(&paths, &old), &files(&paths, &new))
            })
            .unwrap_err();
            for path in &paths {
                assert_eq!(fs::read(path).unwrap(), b"committed");
            }
            let work = work_dir(&temp);
            assert_eq!(
                transaction_files(&work),
                BTreeMap::from([(format!("original-{index}").into(), b"old".to_vec())])
            );
            assert!(error_chain(&error).contains("outputs were published"));
            assert!(error_chain(&error).contains(&work.display().to_string()));
        }
    }

    #[test]
    fn failed_recovery_reports_original_and_hold_paths_and_restores_in_reverse() {
        let temp = DownloadDir::new("file-set-recovery").unwrap();
        let paths: Vec<_> = ["a", "b", "c"].map(|name| store(&temp).join(name)).into();
        let old = vec![b"old".to_vec(); 3];
        let new = vec![b"new".to_vec(); 3];
        for path in &paths {
            fs::write(path, b"old").unwrap();
        }
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let observed = seen.clone();
        let error = with_hook(
            Box::new(move |step, index, path| {
                if step == Step::Restore {
                    observed.borrow_mut().push((index, path.to_path_buf()));
                }
                if (step, index) == (Step::Publish, 1) || (step, index) == (Step::Restore, 1) {
                    return Err(std::io::Error::other("injected"));
                }
                Ok(())
            }),
            || publish(&target(&temp), &files(&paths, &old), &files(&paths, &new)),
        )
        .unwrap_err();
        assert_eq!(
            seen.borrow()
                .iter()
                .map(|(index, _)| *index)
                .collect::<Vec<_>>(),
            [2, 1, 0]
        );
        assert_eq!(fs::read(&paths[0]).unwrap(), b"old");
        assert_eq!(fs::read(&paths[2]).unwrap(), b"old");
        let hold = seen.borrow()[1].1.clone();
        assert_eq!(fs::read(&hold).unwrap(), b"old");
        let message = error_chain(&error);
        assert!(message.contains(&hold.display().to_string()));
        assert!(message.contains(&paths[1].display().to_string()));
        assert!(message.contains("manual recovery"));
    }

    #[test]
    fn partial_stage_cleanup_failure_reports_stage_path() {
        let temp = DownloadDir::new("file-set-stage-cleanup").unwrap();
        let destination = store(&temp).join("new");
        let error = with_hook(
            Box::new(|step, _, _| {
                if matches!(step, Step::WriteRemainder | Step::RemoveStage) {
                    Err(std::io::Error::other("injected"))
                } else {
                    Ok(())
                }
            }),
            || {
                publish(
                    &target(&temp),
                    &[],
                    &[File {
                        path: &destination,
                        bytes: b"partial!",
                    }],
                )
            },
        )
        .unwrap_err();
        let work = work_dir(&temp);
        let stage = work.join("stage-0");
        assert_eq!(fs::read(&stage).unwrap(), b"part");
        assert!(error_chain(&error).contains(&stage.display().to_string()));
        assert!(!destination.exists());
    }

    #[test]
    fn changed_source_before_later_hold_preserves_external_edit() {
        let temp = DownloadDir::new("file-set-revalidation").unwrap();
        let paths: Vec<_> = ["a", "b", "c"].map(|name| store(&temp).join(name)).into();
        let old = vec![b"old".to_vec(); 3];
        for path in &paths {
            fs::write(path, b"old").unwrap();
        }
        let error = with_hook(
            Box::new(|step, index, path| {
                if (step, index) == (Step::Hold, 1) {
                    fs::write(path, b"external")?;
                }
                Ok(())
            }),
            || publish(&target(&temp), &files(&paths, &old), &files(&paths, &old)),
        )
        .unwrap_err();
        assert!(error_chain(&error).contains("changed during"));
        assert_eq!(fs::read(&paths[0]).unwrap(), b"old");
        assert_eq!(fs::read(&paths[1]).unwrap(), b"external");
        assert_eq!(fs::read(&paths[2]).unwrap(), b"old");
        assert_eq!(snapshot(&store(&temp)).len(), 3);
    }

    #[derive(Clone, Copy, Debug)]
    enum Caller {
        Tags,
        Presets,
        Rename,
        Migration,
    }

    #[test]
    fn all_four_callers_use_transaction_for_every_failure_position() {
        for caller in [
            Caller::Tags,
            Caller::Presets,
            Caller::Rename,
            Caller::Migration,
        ] {
            for step in [
                Step::Create,
                Step::Write,
                Step::WriteRemainder,
                Step::Hold,
                Step::Publish,
            ] {
                // Only changed files enter a transaction.
                let changed = match caller {
                    Caller::Tags | Caller::Migration => 3,
                    Caller::Presets | Caller::Rename => 1,
                };
                for index in 0..changed {
                    let temp = DownloadDir::new("file-set-callers").unwrap();
                    let tags = TagStore::new(temp.path());
                    let presets = PresetStore::new(temp.path());
                    let dir = if matches!(caller, Caller::Tags) {
                        &tags.dir
                    } else {
                        &presets.dir
                    };
                    fs::create_dir_all(dir).unwrap();
                    for name in ["a", "b", "c"] {
                        match caller {
                            Caller::Tags => tags
                                .save(&Tag {
                                    name: name.into(),
                                    skills: vec![],
                                    color: None,
                                    description: None,
                                })
                                .unwrap(),
                            Caller::Presets | Caller::Rename => presets
                                .save(&Preset {
                                    name: name.into(),
                                    ..Preset::default()
                                })
                                .unwrap(),
                            Caller::Migration => fs::write(
                                dir.join(format!("{name}.toml")),
                                format!("# original {name}\nname = '{name}'\n"),
                            )
                            .unwrap(),
                        }
                    }
                    let original = snapshot(dir);
                    let scope = dir.clone();
                    let transactions = crate::paths::meta_dir(temp.path()).join(TRANSACTION_DIR);
                    let staging = transactions.clone();
                    let mut fired = false;
                    let error = with_hook(
                        Box::new(move |actual, position, path| {
                            let ours = path.starts_with(&scope) || path.starts_with(&staging);
                            if ours && (actual, position) == (step, index) {
                                assert!(!fired);
                                fired = true;
                                return Err(std::io::Error::other(format!(
                                    "injected {step:?} {index}"
                                )));
                            }
                            Ok(())
                        }),
                        || match caller {
                            Caller::Tags => tags.edit(|tags| {
                                for tag in tags {
                                    tag.skills.push("changed".into());
                                }
                            }),
                            Caller::Presets => presets.save(&Preset {
                                name: "a".into(),
                                skills: vec!["changed".into()],
                                ..Preset::default()
                            }),
                            Caller::Rename => presets.rename("a", "d").map(|_| ()),
                            Caller::Migration => crate::migration::ensure_current(temp.path())
                                .map(|_| ())
                                .map_err(anyhow::Error::from),
                        },
                    )
                    .unwrap_err();
                    assert!(
                        format!("{error:#}").contains("injected"),
                        "{caller:?}: {error:#}"
                    );
                    assert_eq!(snapshot(dir), original, "{caller:?} {step:?} {index}");
                    assert_eq!(fs::read_dir(&transactions).unwrap().count(), 0);
                }
            }
        }
    }

    #[test]
    fn unowned_publish_destination_is_preserved_and_earlier_publish_rolled_back() {
        let temp = DownloadDir::new("file-set-target").unwrap();
        let source = store(&temp).join("original");
        let first = store(&temp).join("first");
        let external = store(&temp).join("external");
        fs::write(&source, b"old").unwrap();
        let error = with_hook(
            Box::new(|step, index, path| {
                if (step, index) == (Step::Publish, 1) {
                    fs::write(path, b"external")?;
                }
                Ok(())
            }),
            || {
                publish(
                    &target(&temp),
                    &[File {
                        path: &source,
                        bytes: b"old",
                    }],
                    &[
                        File {
                            path: &first,
                            bytes: b"new first",
                        },
                        File {
                            path: &external,
                            bytes: b"new second",
                        },
                    ],
                )
            },
        )
        .unwrap_err();
        assert!(error_chain(&error).contains("appeared during publication"));
        assert_eq!(fs::read(&source).unwrap(), b"old");
        assert_eq!(fs::read(&external).unwrap(), b"external");
        assert!(!first.exists());
        assert_eq!(snapshot(&store(&temp)).len(), 2);
    }

    #[test]
    fn replacement_of_new_published_destination_survives_rollback() {
        let temp = DownloadDir::new("file-set-foreign-new").unwrap();
        let paths = [store(&temp).join("first"), store(&temp).join("second")];
        let error = with_hook(
            Box::new({
                let first = paths[0].clone();
                move |step, index, _| {
                    if (step, index) == (Step::Publish, 1) {
                        fs::remove_file(&first)?;
                        fs::write(&first, b"external exact bytes")?;
                        return Err(std::io::Error::other("stop after replacement"));
                    }
                    Ok(())
                }
            }),
            || {
                publish(
                    &target(&temp),
                    &[],
                    &[
                        File {
                            path: &paths[0],
                            bytes: b"ours",
                        },
                        File {
                            path: &paths[1],
                            bytes: b"later",
                        },
                    ],
                )
            },
        )
        .unwrap_err();
        assert!(error_chain(&error).contains("stop after replacement"));
        assert_eq!(fs::read(&paths[0]).unwrap(), b"external exact bytes");
        assert!(!paths[1].exists());
        assert_eq!(snapshot(&store(&temp)).len(), 1);
    }

    #[test]
    fn replacement_of_existing_destination_preserves_foreign_and_held_original() {
        let temp = DownloadDir::new("file-set-foreign-existing").unwrap();
        let paths = [store(&temp).join("first"), store(&temp).join("second")];
        fs::write(&paths[0], b"original").unwrap();
        let error = with_hook(
            Box::new({
                let first = paths[0].clone();
                move |step, index, _| {
                    if (step, index) == (Step::Publish, 1) {
                        fs::remove_file(&first)?;
                        fs::write(&first, b"external")?;
                        return Err(std::io::Error::other("stop after replacement"));
                    }
                    Ok(())
                }
            }),
            || {
                publish(
                    &target(&temp),
                    &[File {
                        path: &paths[0],
                        bytes: b"original",
                    }],
                    &[
                        File {
                            path: &paths[0],
                            bytes: b"ours",
                        },
                        File {
                            path: &paths[1],
                            bytes: b"later",
                        },
                    ],
                )
            },
        )
        .unwrap_err();
        assert_eq!(fs::read(&paths[0]).unwrap(), b"external");
        let work = work_dir(&temp);
        assert_eq!(fs::read(work.join("original-0")).unwrap(), b"original");
        let message = error_chain(&error);
        assert!(message.contains("manual recovery required"));
        assert!(message.contains(&paths[0].display().to_string()));
        assert!(message.contains(&work.join("original-0").display().to_string()));
    }

    #[test]
    fn identical_byte_replacement_is_foreign_by_inode() {
        let temp = DownloadDir::new("file-set-foreign-inode").unwrap();
        let paths = [store(&temp).join("first"), store(&temp).join("second")];
        let _error = with_hook(
            Box::new({
                let first = paths[0].clone();
                move |step, index, _| {
                    if (step, index) == (Step::Publish, 1) {
                        fs::remove_file(&first)?;
                        fs::write(&first, b"same")?;
                        return Err(std::io::Error::other("stop"));
                    }
                    Ok(())
                }
            }),
            || {
                publish(
                    &target(&temp),
                    &[],
                    &[
                        File {
                            path: &paths[0],
                            bytes: b"same",
                        },
                        File {
                            path: &paths[1],
                            bytes: b"later",
                        },
                    ],
                )
            },
        )
        .unwrap_err();
        assert_eq!(fs::read(&paths[0]).unwrap(), b"same");
        assert_eq!(snapshot(&store(&temp)).len(), 1);
    }

    #[test]
    fn symlink_and_directory_replacements_are_preserved() {
        use std::os::unix::fs::symlink;

        for directory in [false, true] {
            let temp = DownloadDir::new("file-set-foreign-kind").unwrap();
            let paths = [store(&temp).join("first"), store(&temp).join("second")];
            let link_target = store(&temp).join("link-target");
            fs::write(&link_target, b"target").unwrap();
            with_hook(
                Box::new({
                    let first = paths[0].clone();
                    let link_target = link_target.clone();
                    move |step, index, _| {
                        if (step, index) == (Step::Publish, 1) {
                            fs::remove_file(&first)?;
                            if directory {
                                fs::create_dir(&first)?;
                            } else {
                                symlink(&link_target, &first)?;
                            }
                            return Err(std::io::Error::other("stop"));
                        }
                        Ok(())
                    }
                }),
                || {
                    publish(
                        &target(&temp),
                        &[],
                        &[
                            File {
                                path: &paths[0],
                                bytes: b"ours",
                            },
                            File {
                                path: &paths[1],
                                bytes: b"later",
                            },
                        ],
                    )
                },
            )
            .unwrap_err();
            let metadata = fs::symlink_metadata(&paths[0]).unwrap();
            assert_eq!(metadata.file_type().is_dir(), directory);
            assert_eq!(metadata.file_type().is_symlink(), !directory);
        }
    }

    #[test]
    fn missing_destination_at_claim_time_needs_no_recovery() {
        let temp = DownloadDir::new("file-set-missing-claim").unwrap();
        let paths = [store(&temp).join("first"), store(&temp).join("second")];
        let error = with_hook(
            Box::new(|step, index, path| {
                if (step, index) == (Step::Publish, 1) {
                    return Err(std::io::Error::other("stop"));
                }
                if (step, index) == (Step::RemovePublished, 0) {
                    fs::remove_file(path)?;
                }
                Ok(())
            }),
            || {
                publish(
                    &target(&temp),
                    &[],
                    &[
                        File {
                            path: &paths[0],
                            bytes: b"ours",
                        },
                        File {
                            path: &paths[1],
                            bytes: b"later",
                        },
                    ],
                )
            },
        )
        .unwrap_err();
        assert!(!error_chain(&error).contains("manual recovery required"));
        assert!(snapshot(&store(&temp)).is_empty());
    }

    #[test]
    fn foreign_precreated_claim_name_is_preserved() {
        let temp = DownloadDir::new("file-set-foreign-claim").unwrap();
        let paths = [store(&temp).join("first"), store(&temp).join("second")];
        let foreign = std::rc::Rc::new(std::cell::RefCell::new(None));
        let captured = foreign.clone();
        let error = with_hook(
            Box::new(move |step, index, path| {
                if (step, index) == (Step::Publish, 1) {
                    let work = path.parent().unwrap();
                    let occupied = work.join("claimed-0");
                    fs::write(&occupied, b"foreign claim")?;
                    *captured.borrow_mut() = Some(occupied);
                    return Err(std::io::Error::other("stop"));
                }
                Ok(())
            }),
            || {
                publish(
                    &target(&temp),
                    &[],
                    &[
                        File {
                            path: &paths[0],
                            bytes: b"ours",
                        },
                        File {
                            path: &paths[1],
                            bytes: b"later",
                        },
                    ],
                )
            },
        )
        .unwrap_err();
        let foreign = foreign.borrow().clone().unwrap();
        assert_eq!(fs::read(&foreign).unwrap(), b"foreign claim");
        assert!(error_chain(&error).contains("stop"));
    }

    #[test]
    fn foreign_stage_swap_is_neither_published_nor_deleted() {
        let temp = DownloadDir::new("file-set-stage-swap").unwrap();
        let destination = store(&temp).join("destination");
        let swapped = std::rc::Rc::new(std::cell::RefCell::new(None));
        let captured = swapped.clone();
        let work = std::rc::Rc::new(std::cell::RefCell::new(None::<PathBuf>));
        let observed_work = work.clone();
        let error = with_hook(
            Box::new(move |step, index, path| {
                if step == Step::Revalidate {
                    *observed_work.borrow_mut() = Some(path.to_path_buf());
                }
                if (step, index) == (Step::Publish, 0) {
                    let stage = observed_work.borrow().as_ref().unwrap().join("stage-0");
                    fs::remove_file(&stage)?;
                    fs::write(&stage, b"foreign stage")?;
                    *captured.borrow_mut() = Some(stage);
                }
                Ok(())
            }),
            || {
                publish(
                    &target(&temp),
                    &[],
                    &[File {
                        path: &destination,
                        bytes: b"ours",
                    }],
                )
            },
        )
        .unwrap_err();
        let stage = swapped.borrow().clone().unwrap();
        assert!(!destination.exists());
        assert_eq!(fs::read(&stage).unwrap(), b"foreign stage");
        assert!(error_chain(&error).contains("staged file identity changed"));
    }

    #[test]
    fn destination_reoccupied_after_claim_retains_both_files_and_paths() {
        let temp = DownloadDir::new("file-set-reoccupied").unwrap();
        let paths = [store(&temp).join("first"), store(&temp).join("second")];
        let error = with_hook(
            Box::new({
                let first = paths[0].clone();
                move |step, index, path| {
                    if (step, index) == (Step::Publish, 1) {
                        fs::remove_file(&first)?;
                        fs::write(&first, b"external claimed")?;
                        return Err(std::io::Error::other("stop"));
                    }
                    if (step, index) == (Step::RestoreClaim, 0) {
                        fs::write(path, b"external reoccupied")?;
                    }
                    Ok(())
                }
            }),
            || {
                publish(
                    &target(&temp),
                    &[],
                    &[
                        File {
                            path: &paths[0],
                            bytes: b"ours",
                        },
                        File {
                            path: &paths[1],
                            bytes: b"later",
                        },
                    ],
                )
            },
        )
        .unwrap_err();
        assert_eq!(fs::read(&paths[0]).unwrap(), b"external reoccupied");
        let work = work_dir(&temp);
        let claim = fs::read_dir(&work)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("claim-0-")
            })
            .unwrap()
            .join("entry");
        assert_eq!(fs::read(&claim).unwrap(), b"external claimed");
        let message = error_chain(&error);
        assert!(message.contains(&paths[0].display().to_string()));
        assert!(message.contains(&claim.display().to_string()));
    }

    #[test]
    fn unsupported_hard_links_fall_back_to_restoring_originals() {
        let temp = DownloadDir::new("file-set-link-fallback").unwrap();
        let paths = [store(&temp).join("first"), store(&temp).join("second")];
        fs::write(&paths[0], b"original").unwrap();
        let error = with_hook(
            Box::new(|step, index, _| {
                if (step, index) == (Step::Publish, 1) {
                    return Err(std::io::Error::other("stop"));
                }
                if step == Step::Link {
                    return Err(std::io::Error::from_raw_os_error(libc::EPERM));
                }
                Ok(())
            }),
            || {
                publish(
                    &target(&temp),
                    &[File {
                        path: &paths[0],
                        bytes: b"original",
                    }],
                    &[
                        File {
                            path: &paths[0],
                            bytes: b"new",
                        },
                        File {
                            path: &paths[1],
                            bytes: b"later",
                        },
                    ],
                )
            },
        )
        .unwrap_err();
        assert!(error_chain(&error).contains("stop"));
        assert_eq!(fs::read(&paths[0]).unwrap(), b"original");
        assert_eq!(snapshot(&store(&temp)).len(), 1);
    }

    #[test]
    fn failed_published_removal_retains_original_for_manual_recovery() {
        let temp = DownloadDir::new("file-set-remove-output").unwrap();
        let paths = vec![store(&temp).join("a"), store(&temp).join("b")];
        let old = vec![b"old".to_vec(); 2];
        let new = vec![b"new".to_vec(); 2];
        for path in &paths {
            fs::write(path, b"old").unwrap();
        }
        let error = with_hook(
            Box::new(|step, index, _| {
                if (step, index) == (Step::Publish, 1) || step == Step::RemovePublished {
                    Err(std::io::Error::other("injected"))
                } else {
                    Ok(())
                }
            }),
            || publish(&target(&temp), &files(&paths, &old), &files(&paths, &new)),
        )
        .unwrap_err();
        let work = work_dir(&temp);
        assert_eq!(fs::read(work.join("original-0")).unwrap(), b"old");
        assert_eq!(fs::read(&paths[0]).unwrap(), b"new");
        assert_eq!(fs::read(&paths[1]).unwrap(), b"old");
        assert!(error_chain(&error).contains("restore destination is occupied"));
        assert!(error_chain(&error).contains(&work.join("original-0").display().to_string()));
    }

    #[test]
    fn byte_revalidation_before_holds_leaves_changed_sources_untouched() {
        let temp = DownloadDir::new("file-set-early-revalidation").unwrap();
        let source = store(&temp).join("source");
        fs::write(&source, b"external").unwrap();
        let error = publish(
            &target(&temp),
            &[File {
                path: &source,
                bytes: b"stale",
            }],
            &[File {
                path: &source,
                bytes: b"new",
            }],
        )
        .unwrap_err();
        assert!(error_chain(&error).contains("changed during"));
        assert_eq!(fs::read(&source).unwrap(), b"external");
        assert_eq!(snapshot(&store(&temp)).len(), 1);
    }

    #[test]
    fn empty_creation_and_removal_file_sets_commit_cleanly() {
        let temp = DownloadDir::new("file-set-empty").unwrap();
        publish(&target(&temp), &[], &[]).unwrap();
        assert!(snapshot(&store(&temp)).is_empty());
        let path = store(&temp).join("new");
        let desired = [File {
            path: &path,
            bytes: b"created",
        }];
        publish(&target(&temp), &[], &desired).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"created");
        publish(&target(&temp), &desired, &[]).unwrap();
        assert!(snapshot(&store(&temp)).is_empty());
    }

    fn tag(name: &str, skills: &[&str]) -> Tag {
        Tag {
            name: name.into(),
            skills: skills.iter().map(|skill| (*skill).into()).collect(),
            color: None,
            description: None,
        }
    }

    fn dead_pid() -> u32 {
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        pid
    }

    /// Fabricate a transaction directory as another process would leave it.
    fn fake_transaction(root: &Path, store: &str, pid: u32, markers: &[&str]) -> PathBuf {
        static FAKE: AtomicU64 = AtomicU64::new(0);
        let work = crate::paths::meta_dir(root)
            .join(TRANSACTION_DIR)
            .join(format!(
                "{pid}-{}-0",
                1_000_000 + FAKE.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&work).unwrap();
        fs::write(
            work.join(MANIFEST),
            format!(
                "store = \".skills-meta/{store}\"\npid = {pid}\n[originals]\noriginal-0 = \"a.toml\"\n[stages]\nstage-0 = \"a.toml\"\n"
            ),
        )
        .unwrap();
        fs::write(work.join("original-0"), b"previous").unwrap();
        fs::write(work.join("stage-0"), b"next").unwrap();
        for marker in markers {
            fs::write(work.join(marker), b"").unwrap();
        }
        work
    }

    /// Register a fabricated transaction as one this process is running.
    fn pretend_running(work: &Path) -> Running {
        let id = work.file_name().unwrap().to_str().unwrap().to_owned();
        active().insert(id.clone());
        Running(id)
    }

    #[test]
    fn transactions_never_appear_inside_store_directories() {
        let temp = DownloadDir::new("file-set-outside-store").unwrap();
        let tags = TagStore::new(temp.path());
        let presets = PresetStore::new(temp.path());
        tags.save(&tag("a", &[])).unwrap();
        presets
            .save(&Preset {
                name: "a".into(),
                ..Preset::default()
            })
            .unwrap();
        let dirs = [tags.dir.clone(), presets.dir.clone()];
        let checks = std::rc::Rc::new(std::cell::Cell::new(0));
        let counted = checks.clone();
        with_hook(
            Box::new(move |_, _, _| {
                for dir in &dirs {
                    for entry in fs::read_dir(dir)? {
                        assert!(entry?.file_type()?.is_file());
                    }
                }
                counted.set(counted.get() + 1);
                Ok(())
            }),
            || {
                tags.save(&tag("a", &["changed"])).unwrap();
                tags.rename("a", "b").unwrap();
                presets.rename("a", "b").unwrap();
                presets.remove("b").unwrap();
            },
        );
        assert!(checks.get() > 10);
        let transactions = crate::paths::meta_dir(temp.path()).join(TRANSACTION_DIR);
        assert_eq!(fs::read_dir(transactions).unwrap().count(), 0);
    }

    #[test]
    fn unchanged_files_keep_their_inode_across_an_edit() {
        let temp = DownloadDir::new("file-set-inodes").unwrap();
        let tags = TagStore::new(temp.path());
        for name in ["a", "b", "c"] {
            tags.save(&tag(name, &[])).unwrap();
        }
        let inode = |name: &str| fs::metadata(tags.dir.join(name)).unwrap().ino();
        let before: Vec<_> = ["a.toml", "b.toml", "c.toml"].map(inode).into();
        tags.save(&tag("b", &["changed"])).unwrap();
        assert_eq!(inode("a.toml"), before[0]);
        assert_ne!(inode("b.toml"), before[1]);
        assert_eq!(inode("c.toml"), before[2]);
        // A write that changes nothing opens no transaction.
        with_hook(Box::new(|_, _, _| panic!("no-op edit published")), || {
            tags.edit(|_| {}).unwrap();
            tags.save(&tag("b", &["changed"])).unwrap();
        });
    }

    #[test]
    fn committed_cleanup_failure_is_a_recorded_success_and_reads_clean_up() {
        let temp = DownloadDir::new("file-set-committed-success").unwrap();
        fs::create_dir_all(temp.path().join("demo")).unwrap();
        fs::write(temp.path().join("demo/SKILL.md"), "---\nname: demo\n---\n").unwrap();
        let ws = crate::Workspace::open(temp.path()).unwrap();
        crate::ops::edit::tag_add(&ws, "demo", &["work".into()]).unwrap();
        let transactions = crate::paths::meta_dir(temp.path()).join(TRANSACTION_DIR);
        let (_, intent) = with_hook(
            Box::new(|step, _, _| {
                if matches!(step, Step::Cleanup | Step::Discard) {
                    Err(std::io::Error::other("injected cleanup failure"))
                } else {
                    Ok(())
                }
            }),
            || {
                crate::history::tag_edit(&ws, |ws| {
                    crate::ops::edit::tag_set(ws, "demo", &["play".into()])?;
                    Ok("tagged".into())
                })
            },
        )
        .unwrap();
        assert!(intent.is_some(), "undo must record the committed change");
        let warnings = crate::warnings::take();
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("changes to .skills-meta/tags were saved")),
            "{warnings:?}"
        );
        let work: Vec<_> = fs::read_dir(&transactions).unwrap().collect();
        assert_eq!(work.len(), 1);
        let work = work.into_iter().next().unwrap().unwrap().path();
        assert!(work.join(COMMITTED).exists());
        assert!(work.join("original-0").exists());
        // The next read removes the leftover and sees the committed change.
        assert_eq!(ws.tags.load("play").unwrap().unwrap().skills, ["demo"]);
        assert!(!work.exists());
    }

    #[test]
    fn interrupted_transactions_are_refused_and_abandoned_staging_removed() {
        let temp = DownloadDir::new("file-set-interrupted").unwrap();
        let tags = TagStore::new(temp.path());
        tags.save(&tag("a", &[])).unwrap();
        let dead = dead_pid();

        let work = fake_transaction(temp.path(), "tags", dead, &[PUBLISHING]);
        let error = format!("{:#}", tags.list().unwrap_err());
        assert!(
            error.contains(&format!("process {dead} did not finish")),
            "{error}"
        );
        assert!(error.contains("original-N (previous contents)"), "{error}");
        assert!(error.contains(&work.display().to_string()), "{error}");
        // Another store is unaffected.
        PresetStore::new(temp.path()).list().unwrap();
        assert!(work.join("original-0").exists());
        fs::remove_dir_all(&work).unwrap();

        // A failed rollback is refused even while its owner still runs.
        let work = fake_transaction(temp.path(), "tags", std::process::id(), &[FAILED]);
        let error = format!("{:#}", tags.list().unwrap_err());
        assert!(error.contains("could not roll back"), "{error}");
        fs::remove_dir_all(&work).unwrap();

        // A held original means publication began even if its marker is lost.
        let work = fake_transaction(temp.path(), "tags", dead, &[]);
        let error = format!("{:#}", tags.list().unwrap_err());
        assert!(
            error.contains(&format!("process {dead} did not finish")),
            "{error}"
        );

        // Staging never touches the store, so a dead owner's staging is removed.
        fs::remove_file(work.join("original-0")).unwrap();
        assert_eq!(tags.list().unwrap().len(), 1);
        assert!(!work.exists());

        // Committed leftovers are removed whoever owns them.
        let work = fake_transaction(
            temp.path(),
            "tags",
            std::process::id(),
            &[PUBLISHING, COMMITTED],
        );
        assert_eq!(tags.list().unwrap().len(), 1);
        assert!(!work.exists());
    }

    #[test]
    fn legacy_in_store_debris_is_still_refused() {
        let temp = DownloadDir::new("file-set-legacy-debris").unwrap();
        let tags = TagStore::new(temp.path());
        fs::create_dir_all(tags.dir.join(".file-set-1-0")).unwrap();
        let error = format!("{:#}", tags.list().unwrap_err());
        assert!(
            error.contains("an interrupted metadata transaction left"),
            "{error}"
        );
    }

    #[test]
    fn readers_wait_for_a_live_transaction_and_then_read() {
        let temp = DownloadDir::new("file-set-live-wait").unwrap();
        let tags = TagStore::new(temp.path());
        tags.save(&tag("a", &[])).unwrap();
        let work = fake_transaction(temp.path(), "tags", std::process::id(), &[PUBLISHING]);
        let running = pretend_running(&work);
        let finisher = {
            let work = work.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(300));
                fs::remove_dir_all(work).unwrap();
                drop(running);
            })
        };
        let started = Instant::now();
        assert_eq!(tags.list().unwrap().len(), 1);
        assert!(started.elapsed() >= Duration::from_millis(250));
        finisher.join().unwrap();

        let work = fake_transaction(temp.path(), "tags", std::process::id(), &[PUBLISHING]);
        let running = pretend_running(&work);
        let started = Instant::now();
        let error = format!("{:#}", tags.list().unwrap_err());
        assert!(started.elapsed() >= READ_WAIT);
        assert!(
            error.contains(&format!(
                ".skills-meta/tags is being written by process {} (transaction {})",
                std::process::id(),
                work.display()
            )),
            "{error}"
        );
        drop(running);
        fs::remove_dir_all(work).unwrap();
    }

    #[test]
    fn readers_never_wait_on_a_transaction_this_process_abandoned() {
        let temp = DownloadDir::new("file-set-own-abandoned").unwrap();
        let tags = TagStore::new(temp.path());
        tags.save(&tag("a", &[])).unwrap();
        // Same process id, not running here: abandoned, e.g. by a panic.
        let work = fake_transaction(temp.path(), "tags", std::process::id(), &[PUBLISHING]);
        let started = Instant::now();
        let error = format!("{:#}", tags.list().unwrap_err());
        assert!(started.elapsed() < READ_WAIT, "{:?}", started.elapsed());
        assert!(
            error.contains(&format!("process {} did not finish", std::process::id())),
            "{error}"
        );
        fs::remove_dir_all(&work).unwrap();

        let work = fake_transaction(temp.path(), "tags", std::process::id(), &[]);
        fs::remove_file(work.join("original-0")).unwrap();
        let started = Instant::now();
        assert_eq!(tags.list().unwrap().len(), 1);
        assert!(started.elapsed() < READ_WAIT);
        assert!(!work.exists());
    }

    #[test]
    fn an_old_transaction_with_an_unverifiable_owner_is_reported_at_once() {
        let temp = DownloadDir::new("file-set-unverifiable").unwrap();
        let tags = TagStore::new(temp.path());
        tags.save(&tag("a", &[])).unwrap();
        // Process 1 always exists; the manifest records no start time.
        let work = fake_transaction(temp.path(), "tags", 1, &[PUBLISHING]);
        fs::File::options()
            .write(true)
            .open(work.join(MANIFEST))
            .unwrap()
            .set_modified(std::time::SystemTime::now() - Duration::from_secs(120))
            .unwrap();
        let started = Instant::now();
        let error = format!("{:#}", tags.list().unwrap_err());
        assert!(started.elapsed() < READ_WAIT);
        for expected in [
            work.display().to_string(),
            "process 1 started".into(),
            "cannot confirm it is the writer".into(),
            "then remove".into(),
        ] {
            assert!(error.contains(&expected), "{error}");
        }
    }

    #[test]
    fn unsynced_outputs_keep_their_originals_and_fail_loudly() {
        let temp = DownloadDir::new("file-set-durability").unwrap();
        let tags = TagStore::new(temp.path());
        tags.save(&tag("a", &[])).unwrap();
        let before = fs::read(tags.dir.join("a.toml")).unwrap();
        // A stage that cannot be synced rolls back before any original moves.
        let error = with_hook(failure(Step::Sync, 0), || {
            tags.save(&tag("a", &["first"])).unwrap_err()
        });
        assert!(format!("{error:#}").contains("injected Sync 0"));
        assert_eq!(fs::read(tags.dir.join("a.toml")).unwrap(), before);
        // A store directory that cannot be synced never becomes committed.
        let error = with_hook(failure(Step::Sync, 1), || {
            tags.save(&tag("a", &["second"])).unwrap_err()
        });
        assert!(
            format!("{error:#}").contains("could not be confirmed on disk"),
            "{error:#}"
        );
        let transactions = crate::paths::meta_dir(temp.path()).join(TRANSACTION_DIR);
        let work = fs::read_dir(transactions)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert!(!work.join(COMMITTED).exists());
        assert_eq!(fs::read(work.join("original-0")).unwrap(), before);
    }

    #[test]
    fn readers_see_whole_states_while_a_writer_is_paused_mid_publication() {
        for step in [Step::Hold, Step::Publish] {
            for index in [0, 1] {
                let temp = DownloadDir::new("file-set-paused-writer").unwrap();
                let root = temp.path().to_path_buf();
                let tags = TagStore::new(&root);
                for name in ["a", "b"] {
                    tags.save(&tag(name, &[])).unwrap();
                }
                let (paused, pause) = std::sync::mpsc::channel();
                let (resume, resumed) = std::sync::mpsc::channel::<()>();
                let writer = std::thread::spawn(move || {
                    let tags = TagStore::new(&root);
                    with_hook(
                        Box::new(move |actual, position, _| {
                            if (actual, position) == (step, index) {
                                paused.send(()).unwrap();
                                resumed.recv().unwrap();
                            }
                            Ok(())
                        }),
                        || {
                            tags.edit(|tags| {
                                for tag in tags {
                                    tag.skills.push("changed".into());
                                }
                            })
                        },
                    )
                });
                pause.recv().unwrap();
                let reader = {
                    let tags = tags.clone();
                    std::thread::spawn(move || tags.list())
                };
                std::thread::sleep(Duration::from_millis(100));
                resume.send(()).unwrap();
                writer.join().unwrap().unwrap();
                let seen = reader.join().unwrap().unwrap();
                assert_eq!(seen.len(), 2, "{step:?} {index}");
                assert!(
                    seen.iter().all(|tag| tag.skills == ["changed"]),
                    "{step:?} {index}: {seen:?}"
                );
            }
        }
    }

    #[test]
    fn a_reader_removing_the_committed_leftover_does_not_fail_the_writer() {
        let temp = DownloadDir::new("file-set-committed-race").unwrap();
        let tags = TagStore::new(temp.path());
        tags.save(&tag("a", &[])).unwrap();
        let transactions = crate::paths::meta_dir(temp.path()).join(TRANSACTION_DIR);
        // A reader on another thread sees `committed` as soon as it exists and
        // removes the leftover before the writer syncs the directory.
        with_hook(
            Box::new(|step, index, path| {
                if (step, index) == (Step::Marked, 1) {
                    fs::remove_dir_all(path.parent().unwrap())?;
                }
                Ok(())
            }),
            || tags.save(&tag("a", &["changed"])).unwrap(),
        );
        assert_eq!(tags.load("a").unwrap().unwrap().skills, ["changed"]);
        assert_eq!(fs::read_dir(&transactions).unwrap().count(), 0);
    }

    #[test]
    fn concurrent_writer_and_reader_never_fail() {
        let temp = DownloadDir::new("file-set-concurrent").unwrap();
        let root = temp.path().to_path_buf();
        let tags = TagStore::new(&root);
        tags.save(&tag("seed", &[])).unwrap();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reader = {
            let (tags, stop) = (tags.clone(), stop.clone());
            std::thread::spawn(move || {
                let mut reads = 0;
                while !stop.load(Ordering::Relaxed) {
                    let seen = tags.list().unwrap();
                    assert!(seen.iter().any(|tag| tag.name == "seed"));
                    reads += 1;
                }
                reads
            })
        };
        for round in 0..150 {
            let name = format!("t{}", round % 5);
            tags.edit(|tags| match tags.iter_mut().find(|tag| tag.name == name) {
                Some(tag) => tag.skills.push(format!("s{round}")),
                None => tags.push(tag(&name, &[])),
            })
            .unwrap();
        }
        stop.store(true, Ordering::Relaxed);
        assert!(reader.join().unwrap() > 0);
    }

    #[test]
    fn preset_normalized_noop_and_case_rename_keep_comments() {
        let temp = DownloadDir::new("file-set-preset-rename").unwrap();
        let store = PresetStore::new(temp.path());
        store
            .save(&Preset {
                name: "work".into(),
                ..Preset::default()
            })
            .unwrap();
        let source = store.dir.join("work.toml");
        let bytes = format!("# preserve me\n{}", fs::read_to_string(&source).unwrap()).into_bytes();
        fs::write(&source, &bytes).unwrap();
        with_hook(
            Box::new(|_, _, _| panic!("same-name rename must not publish")),
            || {
                assert_eq!(store.rename(" work ", "work").unwrap().name, "work");
            },
        );
        assert_eq!(fs::read(&source).unwrap(), bytes);
        store.rename("work", "WORK").unwrap();
        assert_eq!(store.list().unwrap()[0].name, "WORK");
        let state = snapshot(&store.dir);
        assert_eq!(state.len(), 1);
        assert!(state[Path::new("WORK.toml")].starts_with(b"# preserve me\n"));
    }
}
