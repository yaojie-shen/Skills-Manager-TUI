//! Best-effort file-set transactions under the caller's metadata lock.
//!
//! Stage everything before moving originals. The journal records only successful
//! filesystem operations, so a failed hold must never delete an untouched source.
//! This is not crash-atomic and does not coordinate unrelated metadata stores.
//! Publication retains a check-then-rename race; cooperating writers are serialized
//! by the metadata lock. Rollback claims entries before checking inode ownership.

use anyhow::{Context, Result, ensure};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{DirBuilderExt, MetadataExt},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FailureKind {
    RolledBack,
    Committed { leftovers: Vec<PathBuf> },
    ManualRecovery { paths: Vec<PathBuf> },
}

#[derive(Debug)]
#[allow(dead_code)]
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

fn transaction_dir(dir: &Path) -> Result<PathBuf> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    loop {
        let path = dir.join(format!(
            ".file-set-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700);
        match builder.create(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("creating transaction directory {}", path.display()));
            }
        }
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

/// Callers validate names/destinations and create `dir` before entering here.
/// Originals are revalidated after staging, then again immediately before each
/// hold. A cleanup error means the new files ARE committed; never roll them back.
///
/// The transaction directory is mode 0700. Same-user processes that can still
/// write there are detected by inode/type identity checks where publication or
/// cleanup would otherwise trust a transaction pathname.
pub(crate) fn publish(
    dir: &Path,
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

    let work = transaction_dir(dir).map_err(|source| PublishError {
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
        checkpoint(Step::Revalidate, 0, &work)?;
        for file in before {
            unchanged(file)?;
        }
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
        if let Err(error) = fs::remove_dir(&work) {
            recovery.push(format!(
                "transaction files retained at {}: {error}",
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

    // Commit has completed. Failures below leave outputs in place and identify
    // the old copies to remove manually; they must never enter rollback.
    let mut cleanup = Vec::new();
    for (index, (hold, _)) in held.iter().enumerate() {
        if let Err(error) =
            checkpoint(Step::Cleanup, index, hold).and_then(|()| fs::remove_file(hold))
        {
            cleanup.push(format!("{}: {error}", hold.display()));
        }
    }
    if let Err(error) = fs::remove_dir(&work) {
        cleanup.push(format!("{}: {error}", work.display()));
    }
    if cleanup.is_empty() {
        Ok(())
    } else {
        Err(PublishError {
            kind: FailureKind::Committed {
                leftovers: vec![work.clone()],
            },
            message: format!(
                "file-set outputs were published, but old transaction files could not be removed (remove manually): {}",
                cleanup.join("; ")
            ),
            source: anyhow::anyhow!("transaction cleanup failed"),
        })
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
        let destination = temp.path().join("destination");
        let error = with_hook(failure(Step::Create, 0), || {
            publish(
                temp.path(),
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
                let paths: Vec<_> = ["a", "b", "c"].map(|name| temp.path().join(name)).into();
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
                fs::write(temp.path().join("untouched"), b"external").unwrap();
                let original = snapshot(temp.path());
                // Overlapping targets recreate the original partial-hold data-loss bug.
                let error = with_hook(failure(step, index), || {
                    publish(temp.path(), &files(&paths, &old), &files(&paths, &new))
                })
                .unwrap_err();
                assert!(error_chain(&error).contains("injected"));
                assert_eq!(snapshot(temp.path()), original, "{step:?} at {index}");
            }
        }
    }

    #[test]
    fn create_new_collision_never_claims_or_deletes_foreign_stage() {
        let temp = DownloadDir::new("file-set-collision").unwrap();
        let destination = temp.path().join("new");
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
                    temp.path(),
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
            .map(|name| temp.path().join(name))
            .into();
        let bytes: Vec<_> = [b"first".to_vec(), b"second".to_vec(), b"third".to_vec()].into();
        for (path, bytes) in paths.iter().zip(&bytes) {
            fs::write(path, bytes).unwrap();
        }
        let targets = vec![paths[1].clone(), paths[2].clone(), paths[0].clone()];
        publish(
            temp.path(),
            &files(&paths, &bytes),
            &files(&targets, &bytes),
        )
        .unwrap();
        for (path, bytes) in targets.iter().zip(&bytes) {
            assert_eq!(fs::read(path).unwrap(), *bytes);
        }
        let upper = temp.path().join("ONE");
        publish(
            temp.path(),
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
        let state = snapshot(temp.path());
        assert_eq!(state.len(), 3);
        assert!(state.contains_key(Path::new("ONE")));
        assert!(!state.contains_key(Path::new("one")));
    }

    #[test]
    fn cleanup_failure_at_each_hold_preserves_committed_outputs() {
        for index in 0..3 {
            let temp = DownloadDir::new("file-set-cleanup").unwrap();
            let paths: Vec<_> = ["a", "b", "c"].map(|name| temp.path().join(name)).into();
            let old = vec![b"old".to_vec(); 3];
            let new = vec![b"committed".to_vec(); 3];
            for path in &paths {
                fs::write(path, b"old").unwrap();
            }
            let error = with_hook(failure(Step::Cleanup, index), || {
                publish(temp.path(), &files(&paths, &old), &files(&paths, &new))
            })
            .unwrap_err();
            for path in &paths {
                assert_eq!(fs::read(path).unwrap(), b"committed");
            }
            let work = fs::read_dir(temp.path())
                .unwrap()
                .map(|e| e.unwrap().path())
                .find(|p| p.is_dir())
                .unwrap();
            assert_eq!(
                snapshot(&work),
                BTreeMap::from([(format!("original-{index}").into(), b"old".to_vec())])
            );
            assert!(error_chain(&error).contains("outputs were published"));
            assert!(error_chain(&error).contains(&work.display().to_string()));
        }
    }

    #[test]
    fn failed_recovery_reports_original_and_hold_paths_and_restores_in_reverse() {
        let temp = DownloadDir::new("file-set-recovery").unwrap();
        let paths: Vec<_> = ["a", "b", "c"].map(|name| temp.path().join(name)).into();
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
            || publish(temp.path(), &files(&paths, &old), &files(&paths, &new)),
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
        let target = temp.path().join("new");
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
                    temp.path(),
                    &[],
                    &[File {
                        path: &target,
                        bytes: b"partial!",
                    }],
                )
            },
        )
        .unwrap_err();
        let work = fs::read_dir(temp.path())
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let stage = work.join("stage-0");
        assert_eq!(fs::read(&stage).unwrap(), b"part");
        assert!(error_chain(&error).contains(&stage.display().to_string()));
        assert!(!target.exists());
    }

    #[test]
    fn changed_source_before_later_hold_preserves_external_edit() {
        let temp = DownloadDir::new("file-set-revalidation").unwrap();
        let paths: Vec<_> = ["a", "b", "c"].map(|name| temp.path().join(name)).into();
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
            || publish(temp.path(), &files(&paths, &old), &files(&paths, &old)),
        )
        .unwrap_err();
        assert!(error_chain(&error).contains("changed during"));
        assert_eq!(fs::read(&paths[0]).unwrap(), b"old");
        assert_eq!(fs::read(&paths[1]).unwrap(), b"external");
        assert_eq!(fs::read(&paths[2]).unwrap(), b"old");
        assert_eq!(snapshot(temp.path()).len(), 3);
    }

    #[derive(Clone, Copy, Debug)]
    enum Caller {
        Tags,
        Presets,
        Rename,
    }

    #[test]
    fn all_four_callers_use_transaction_for_every_failure_position() {
        for caller in [Caller::Tags, Caller::Presets, Caller::Rename] {
            for step in [
                Step::Create,
                Step::Write,
                Step::WriteRemainder,
                Step::Hold,
                Step::Publish,
            ] {
                for index in 0..3 {
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
                        }
                    }
                    let original = snapshot(dir);
                    let scope = dir.clone();
                    let mut fired = false;
                    let error = with_hook(
                        Box::new(move |actual, position, path| {
                            if path.starts_with(&scope) && (actual, position) == (step, index) {
                                assert!(!fired);
                                fired = true;
                                return Err(std::io::Error::other(format!(
                                    "injected {step:?} {index}"
                                )));
                            }
                            Ok(())
                        }),
                        || match caller {
                            Caller::Tags => tags.save(&Tag {
                                name: "a".into(),
                                skills: vec!["changed".into()],
                                color: None,
                                description: None,
                            }),
                            Caller::Presets => presets.save(&Preset {
                                name: "a".into(),
                                skills: vec!["changed".into()],
                                ..Preset::default()
                            }),
                            Caller::Rename => presets.rename("a", "d").map(|_| ()),
                        },
                    )
                    .unwrap_err();
                    assert!(
                        format!("{error:#}").contains("injected"),
                        "{caller:?}: {error:#}"
                    );
                    assert_eq!(snapshot(dir), original, "{caller:?} {step:?} {index}");
                }
            }
        }
    }

    #[test]
    fn unowned_publish_destination_is_preserved_and_earlier_publish_rolled_back() {
        let temp = DownloadDir::new("file-set-target").unwrap();
        let source = temp.path().join("original");
        let first = temp.path().join("first");
        let external = temp.path().join("external");
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
                    temp.path(),
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
        assert_eq!(snapshot(temp.path()).len(), 2);
    }

    #[test]
    fn replacement_of_new_published_destination_survives_rollback() {
        let temp = DownloadDir::new("file-set-foreign-new").unwrap();
        let paths = [temp.path().join("first"), temp.path().join("second")];
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
                    temp.path(),
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
        assert_eq!(snapshot(temp.path()).len(), 1);
    }

    #[test]
    fn replacement_of_existing_destination_preserves_foreign_and_held_original() {
        let temp = DownloadDir::new("file-set-foreign-existing").unwrap();
        let paths = [temp.path().join("first"), temp.path().join("second")];
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
                    temp.path(),
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
        let work = fs::read_dir(temp.path())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.is_dir())
            .unwrap();
        assert_eq!(fs::read(work.join("original-0")).unwrap(), b"original");
        let message = error_chain(&error);
        assert!(message.contains("manual recovery required"));
        assert!(message.contains(&paths[0].display().to_string()));
        assert!(message.contains(&work.join("original-0").display().to_string()));
    }

    #[test]
    fn identical_byte_replacement_is_foreign_by_inode() {
        let temp = DownloadDir::new("file-set-foreign-inode").unwrap();
        let paths = [temp.path().join("first"), temp.path().join("second")];
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
                    temp.path(),
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
        assert_eq!(snapshot(temp.path()).len(), 1);
    }

    #[test]
    fn symlink_and_directory_replacements_are_preserved() {
        use std::os::unix::fs::symlink;

        for directory in [false, true] {
            let temp = DownloadDir::new("file-set-foreign-kind").unwrap();
            let paths = [temp.path().join("first"), temp.path().join("second")];
            let link_target = temp.path().join("link-target");
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
                        temp.path(),
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
        let paths = [temp.path().join("first"), temp.path().join("second")];
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
                    temp.path(),
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
        assert!(snapshot(temp.path()).is_empty());
    }

    #[test]
    fn foreign_precreated_claim_name_is_preserved() {
        let temp = DownloadDir::new("file-set-foreign-claim").unwrap();
        let paths = [temp.path().join("first"), temp.path().join("second")];
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
                    temp.path(),
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
        let destination = temp.path().join("destination");
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
                    temp.path(),
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
        let paths = [temp.path().join("first"), temp.path().join("second")];
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
                    temp.path(),
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
        let work = fs::read_dir(temp.path())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.is_dir())
            .unwrap();
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
        let paths = [temp.path().join("first"), temp.path().join("second")];
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
                    temp.path(),
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
        assert_eq!(snapshot(temp.path()).len(), 1);
    }

    #[test]
    fn failed_published_removal_retains_original_for_manual_recovery() {
        let temp = DownloadDir::new("file-set-remove-output").unwrap();
        let paths = vec![temp.path().join("a"), temp.path().join("b")];
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
            || publish(temp.path(), &files(&paths, &old), &files(&paths, &new)),
        )
        .unwrap_err();
        let work = fs::read_dir(temp.path())
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| p.is_dir())
            .unwrap();
        assert_eq!(fs::read(work.join("original-0")).unwrap(), b"old");
        assert_eq!(fs::read(&paths[0]).unwrap(), b"new");
        assert_eq!(fs::read(&paths[1]).unwrap(), b"old");
        assert!(error_chain(&error).contains("restore destination is occupied"));
        assert!(error_chain(&error).contains(&work.join("original-0").display().to_string()));
    }

    #[test]
    fn byte_revalidation_before_holds_leaves_changed_sources_untouched() {
        let temp = DownloadDir::new("file-set-early-revalidation").unwrap();
        let source = temp.path().join("source");
        fs::write(&source, b"external").unwrap();
        let error = publish(
            temp.path(),
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
        assert_eq!(snapshot(temp.path()).len(), 1);
    }

    #[test]
    fn empty_creation_and_removal_file_sets_commit_cleanly() {
        let temp = DownloadDir::new("file-set-empty").unwrap();
        publish(temp.path(), &[], &[]).unwrap();
        assert!(snapshot(temp.path()).is_empty());
        let path = temp.path().join("new");
        let desired = [File {
            path: &path,
            bytes: b"created",
        }];
        publish(temp.path(), &[], &desired).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"created");
        publish(temp.path(), &desired, &[]).unwrap();
        assert!(snapshot(temp.path()).is_empty());
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
