//! Best-effort file-set transactions under the caller's metadata lock.
//!
//! Stage everything before moving originals. The journal records only successful
//! filesystem operations, so a failed hold must never delete an untouched source.
//! This is not crash-atomic and does not coordinate unrelated metadata stores.

use anyhow::{Context, Result, ensure};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

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
    Restore,
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
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("creating transaction directory {}", path.display()));
            }
        }
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
pub(crate) fn publish(dir: &Path, before: &[File<'_>], desired: &[File<'_>]) -> Result<()> {
    let work = transaction_dir(dir)?;
    let mut stages = Vec::new();
    let mut held: Vec<(PathBuf, &Path)> = Vec::new();
    let mut published = Vec::new();
    let result = (|| -> Result<()> {
        for (index, file) in desired.iter().enumerate() {
            let stage = work.join(format!("stage-{index}"));
            checkpoint(Step::Create, index, &stage)?;
            let mut writer = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&stage)
                .with_context(|| format!("creating stage {}", stage.display()))?;
            // Journal before writing: even a partially written stage is ours.
            stages.push(stage.clone());
            checkpoint(Step::Write, index, &stage)?;
            let split = file.bytes.len() / 2;
            writer.write_all(&file.bytes[..split])?;
            checkpoint(Step::WriteRemainder, index, &stage)?;
            writer.write_all(&file.bytes[split..])?;
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
            match fs::symlink_metadata(file.path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
                Ok(_) => anyhow::bail!(
                    "destination {} appeared during publication",
                    file.path.display()
                ),
            }
            fs::rename(stage, file.path)
                .with_context(|| format!("publishing {}", file.path.display()))?;
            published.push(file.path);
        }
        Ok(())
    })();

    if let Err(error) = result {
        let mut recovery = Vec::new();
        for (index, path) in published.iter().enumerate().rev() {
            if let Err(error) =
                checkpoint(Step::RemovePublished, index, path).and_then(|()| fs::remove_file(path))
            {
                recovery.push(format!("remove published {}: {error}", path.display()));
            }
        }
        for (index, (hold, original)) in held.iter().enumerate().rev() {
            let restore = (|| -> std::io::Result<()> {
                checkpoint(Step::Restore, index, hold)?;
                // Do not overwrite a new external file or a failed removal.
                match fs::symlink_metadata(original) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                    Ok(_) => return Err(std::io::Error::other("restore destination is occupied")),
                }
                fs::rename(hold, original)
            })();
            if let Err(error) = restore {
                recovery.push(format!(
                    "restore {} from {}: {error}",
                    original.display(),
                    hold.display()
                ));
            }
        }
        for (index, stage) in stages.iter().enumerate() {
            if let Err(error) =
                checkpoint(Step::RemoveStage, index, stage).and_then(|()| fs::remove_file(stage))
                && error.kind() != std::io::ErrorKind::NotFound
            {
                recovery.push(format!("remove stage {}: {error}", stage.display()));
            }
        }
        if let Err(error) = fs::remove_dir(&work) {
            recovery.push(format!(
                "transaction files retained at {}: {error}",
                work.display()
            ));
        }
        return Err(error).with_context(|| {
            if recovery.is_empty() {
                "file-set publication failed; held originals restored and stages removed".to_owned()
            } else {
                format!(
                    "file-set publication failed; manual recovery required: {}",
                    recovery.join("; ")
                )
            }
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
    ensure!(
        cleanup.is_empty(),
        "file-set outputs were published, but old transaction files could not be removed (remove manually): {}",
        cleanup.join("; ")
    );
    Ok(())
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
                assert!(format!("{error:#}").contains("injected"));
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
        assert!(format!("{error:#}").contains("manual recovery"));
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
            assert!(format!("{error:#}").contains("outputs were published"));
            assert!(format!("{error:#}").contains(&work.display().to_string()));
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
        let message = format!("{error:#}");
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
        assert!(format!("{error:#}").contains(&stage.display().to_string()));
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
        assert!(format!("{error:#}").contains("changed during"));
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
                            Caller::Migration => fs::write(
                                dir.join(format!("{name}.toml")),
                                format!("# original {name}\nname = '{name}'\n"),
                            )
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
                            Caller::Migration => {
                                let config =
                                    crate::config::Config::load_legacy(temp.path()).unwrap();
                                crate::migration::migrate_metadata(
                                    temp.path(),
                                    &config,
                                    &tags,
                                    &[],
                                    &presets,
                                )
                                .map(|_| ())
                            }
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
        assert!(format!("{error:#}").contains("appeared during publication"));
        assert_eq!(fs::read(&source).unwrap(), b"old");
        assert_eq!(fs::read(&external).unwrap(), b"external");
        assert!(!first.exists());
        assert_eq!(snapshot(temp.path()).len(), 2);
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
        assert!(format!("{error:#}").contains("restore destination is occupied"));
        assert!(format!("{error:#}").contains(&work.join("original-0").display().to_string()));
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
        assert!(format!("{error:#}").contains("changed during"));
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
