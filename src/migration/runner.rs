use super::{
    MigrationError, Recovery, backup,
    plan::{Phase, Plan},
};
use anyhow::{Context, Result, ensure};
use std::path::{Path, PathBuf};

#[cfg(test)]
type Hook = Box<dyn FnMut(Phase) -> Result<()>>;
#[cfg(test)]
type OperationHook = Box<dyn FnMut(Phase, usize, &Path) -> Result<()>>;
#[cfg(test)]
thread_local! {
    static HOOK: std::cell::RefCell<Option<Hook>> = const { std::cell::RefCell::new(None) };
    static OPERATION_HOOK: std::cell::RefCell<Option<OperationHook>> = const { std::cell::RefCell::new(None) };
}
fn checkpoint(phase: Phase) -> Result<()> {
    #[cfg(test)]
    {
        HOOK.with_borrow_mut(|current| match current {
            Some(hook) => hook(phase),
            None => Ok(()),
        })
    }
    #[cfg(not(test))]
    {
        let _ = phase;
        Ok(())
    }
}

fn operation_checkpoint(phase: Phase, index: usize, path: &Path) -> Result<()> {
    #[cfg(test)]
    {
        OPERATION_HOOK.with_borrow_mut(|current| match current {
            Some(hook) => hook(phase, index, path),
            None => Ok(()),
        })
    }
    #[cfg(not(test))]
    {
        let _ = (phase, index, path);
        Ok(())
    }
}
#[cfg(test)]
pub(crate) fn with_hook<T>(hook: Hook, run: impl FnOnce() -> T) -> T {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            HOOK.with_borrow_mut(|current| *current = None)
        }
    }
    HOOK.with_borrow_mut(|current| {
        assert!(current.is_none());
        *current = Some(hook)
    });
    let _reset = Reset;
    run()
}

#[cfg(test)]
pub(crate) fn with_operation_hook<T>(hook: OperationHook, run: impl FnOnce() -> T) -> T {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            OPERATION_HOOK.with_borrow_mut(|current| *current = None)
        }
    }
    OPERATION_HOOK.with_borrow_mut(|current| {
        assert!(current.is_none());
        *current = Some(hook)
    });
    let _reset = Reset;
    run()
}

fn revalidate(root: &Path, plan: &Plan) -> Result<()> {
    let meta = crate::paths::meta_dir(root);
    for op in &plan.ops {
        let path = meta.join(op.path.as_path());
        match &op.before {
            Some(before) => ensure!(
                std::fs::read(&path).with_context(|| format!("revalidating {}", path.display()))?
                    == *before,
                "{} changed during migration",
                path.display()
            ),
            None => ensure!(
                !path.exists(),
                "migration destination {} appeared",
                path.display()
            ),
        }
    }
    Ok(())
}
#[derive(Debug)]
struct ApplyError {
    source: anyhow::Error,
    wrote: bool,
}

fn apply_phase(root: &Path, plan: &Plan, phase: Phase) -> std::result::Result<bool, ApplyError> {
    let mut wrote = false;
    let result = (|| -> Result<()> {
        let meta = crate::paths::meta_dir(root);
        let operations: Vec<_> = plan.phase(phase).collect();
        if operations.is_empty() {
            return Ok(());
        }
        match phase {
            Phase::Tags | Phase::Presets => {
                let dir = meta.join(match phase {
                    Phase::Tags => "tags",
                    _ => "presets",
                });
                std::fs::create_dir_all(&dir)?;
                let before_paths: Vec<_> = operations
                    .iter()
                    .filter(|operation| operation.before.is_some())
                    .map(|operation| meta.join(operation.path.as_path()))
                    .collect();
                let after_paths: Vec<_> = operations
                    .iter()
                    .filter(|operation| operation.after.is_some())
                    .map(|operation| meta.join(operation.path.as_path()))
                    .collect();
                let before_files: Vec<_> = operations
                    .iter()
                    .filter_map(|operation| operation.before.as_ref())
                    .zip(&before_paths)
                    .map(|(bytes, path)| crate::file_set::File { path, bytes })
                    .collect();
                let after_files: Vec<_> = operations
                    .iter()
                    .filter_map(|operation| operation.after.as_ref())
                    .zip(&after_paths)
                    .map(|(bytes, path)| crate::file_set::File { path, bytes })
                    .collect();
                match crate::file_set::publish(&dir, &before_files, &after_files) {
                    Ok(()) => wrote = true,
                    Err(error) => {
                        wrote =
                            matches!(error.kind, crate::file_set::FailureKind::Committed { .. });
                        return Err(error.into());
                    }
                }
            }
            Phase::Repos | Phase::Config => {
                for (index, operation) in operations.into_iter().enumerate() {
                    let path = meta.join(operation.path.as_path());
                    operation_checkpoint(phase, index, &path)?;
                    match &operation.after {
                        Some(bytes) => {
                            if let Some(parent) = path.parent() {
                                std::fs::create_dir_all(parent)?;
                            }
                            if let Some(before) = &operation.before {
                                ensure!(
                                    std::fs::read(&path)? == *before,
                                    "{} changed during migration",
                                    path.display()
                                );
                            } else {
                                ensure!(
                                    !path.exists(),
                                    "{} appeared during migration",
                                    path.display()
                                );
                            }
                            crate::util::write_atomic(&path, bytes)?;
                        }
                        None => {
                            let before = operation
                                .before
                                .as_ref()
                                .expect("delete operation has original bytes");
                            ensure!(
                                std::fs::read(&path)? == *before,
                                "{} changed during migration",
                                path.display()
                            );
                            std::fs::remove_file(&path)?;
                        }
                    }
                    wrote = true;
                }
            }
        }
        Ok(())
    })();
    result
        .map(|()| wrote)
        .map_err(|source| ApplyError { source, wrote })
}

/// Execute guard → metadata lock → revalidation → backup → ordered phases.
pub(crate) fn execute(
    root: &Path,
    snapshot: &super::snapshot::HomeSnapshot,
    plan: &Plan,
    from: u32,
    to: u32,
) -> std::result::Result<PathBuf, MigrationError> {
    let mut backup_dir = None;
    let mut committed = Vec::new();
    let mut forced_recovery = None;
    let result = (|| -> Result<PathBuf> {
        let _guard = crate::ops::sync::MutationGuard::acquire_root(root, "home layout migration")?;
        let _lock = crate::meta::MetaStore::new(root).lock()?;
        let current = super::snapshot::HomeSnapshot::read(root)?;
        ensure!(
            current.files() == snapshot.files(),
            "Skill Home metadata changed during migration"
        );
        revalidate(root, plan)?;
        let backup = backup::create(root, plan, from, to)?;
        backup_dir = Some(backup.clone());
        for phase in Phase::ORDERED {
            match apply_phase(root, plan, phase) {
                Ok(wrote) => {
                    if wrote {
                        committed.push(phase);
                    }
                }
                Err(error) => {
                    if error.wrote {
                        committed.push(phase);
                    }
                    if let Some(publication) =
                        error.source.downcast_ref::<crate::file_set::PublishError>()
                    {
                        match &publication.kind {
                            crate::file_set::FailureKind::RolledBack => {}
                            crate::file_set::FailureKind::Committed { leftovers } => {
                                forced_recovery = Some(Recovery::Manual(leftovers.clone()));
                            }
                            crate::file_set::FailureKind::ManualRecovery { paths } => {
                                forced_recovery = Some(Recovery::Manual(paths.clone()));
                            }
                        }
                    }
                    return Err(error.source);
                }
            }
            checkpoint(phase)?;
        }
        Ok(backup)
    })();
    result.map_err(|source| {
        let recovery = forced_recovery.unwrap_or({
            if committed.is_empty() {
                Recovery::NothingWritten
            } else {
                Recovery::RetryOnReopen
            }
        });
        MigrationError {
            backup_dir,
            committed,
            recovery,
            source,
        }
    })
}
