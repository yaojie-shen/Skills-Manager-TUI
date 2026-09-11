//! Input thread and background tasks. Everything reaches the main loop as a `Msg`.

use anyhow::Result;
use crossterm::event::{Event, KeyEvent, KeyEventKind, MouseEvent};
use skills::Workspace;
use skills::ops::update::{self, CheckResult, Prepared};
use skills::reconcile::Snapshot;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::time::Duration;

pub enum Msg {
    Key(KeyEvent),
    Paste(String),
    Mouse(MouseEvent),
    Resize,
    Tick,
    Task(u64, Box<TaskOutput>),
    Progress(u64, String),
}

/// Long-running work executed off the UI thread.
#[derive(Debug, Clone)]
pub enum Task {
    DiscoverRepository(String),
    InstallRepository(Box<super::repository_picker::InstallSelection>),
    Scan,
    PollRoot,
    Check(Vec<String>),
    Prepare(String),
    /// Fetch a skill from a git repository or a local path into the root.
    /// Cloning is slow enough that it cannot run on the UI thread. The subpath
    /// stays separate because appending it to a URL would break the clone.
    Install {
        reference: String,
        subpath: Option<String>,
    },
}

pub enum TaskOutput {
    Batch(BatchOutcome),
    RepositoryFetched(String, Result<skills::repository::FetchedRepository>),
    RepositoryInstalled(
        Box<super::repository_picker::InstallSelection>,
        Result<Vec<String>>,
    ),
    Scan(Result<Snapshot>, Option<skills::reconcile::watch::Stamp>),
    RootStamp(Result<skills::reconcile::watch::Stamp>),
    Check(Vec<(String, Result<CheckResult>)>),
    Prepared(String, Result<Prepared>),
    /// The reference asked for, and the key it landed under.
    Installed(String, Result<String>),
}

/// A way to take the input thread off the terminal for a while.
///
/// Handing the terminal to an editor is not just a matter of leaving the
/// alternate screen. The input thread sits inside crossterm's reader, and while
/// it does, two things go wrong: the editor and the thread compete for the same
/// keystrokes, and the cursor-position query that ratatui makes on the way
/// back in cannot get at the reader to collect its answer and times out. So
/// the thread has to be parked, and confirmed parked, before either happens.
#[derive(Default)]
pub struct InputGate {
    pause: AtomicBool,
    idle: AtomicBool,
}

impl InputGate {
    /// Ask the thread to stop reading and wait until it has. The wait is what
    /// matters: a flag alone would leave a window where the thread is still in
    /// `poll`, and a keystroke meant for the editor lands in the TUI instead.
    pub fn hold(&self) {
        self.idle.store(false, Ordering::SeqCst);
        self.pause.store(true, Ordering::SeqCst);
        while !self.idle.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    pub fn release(&self) {
        self.pause.store(false, Ordering::SeqCst);
    }
}

pub fn spawn_input(tx: Sender<Msg>, gate: Arc<InputGate>) {
    std::thread::Builder::new()
        .name("input".into())
        .spawn(move || {
            loop {
                if gate.pause.load(Ordering::SeqCst) {
                    gate.idle.store(true, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(20));
                    continue;
                }
                gate.idle.store(false, Ordering::SeqCst);
                // Polling with a timeout rather than blocking in `read` is what
                // lets the pause flag be noticed at all.
                match crossterm::event::poll(Duration::from_millis(100)) {
                    Ok(true) => {}
                    Ok(false) => continue,
                    Err(_) => return,
                }
                match crossterm::event::read() {
                    Ok(Event::Key(k))
                        if k.kind == KeyEventKind::Press || k.kind == KeyEventKind::Repeat =>
                    {
                        if tx.send(Msg::Key(k)).is_err() {
                            return;
                        }
                    }
                    Ok(Event::Paste(text)) => {
                        if tx.send(Msg::Paste(text)).is_err() {
                            return;
                        }
                    }
                    Ok(Event::Mouse(m)) => {
                        if tx.send(Msg::Mouse(m)).is_err() {
                            return;
                        }
                    }
                    Ok(Event::Resize(_, _)) => {
                        if tx.send(Msg::Resize).is_err() {
                            return;
                        }
                    }
                    Ok(_) => {}
                    Err(_) => return,
                }
            }
        })
        .expect("spawn input thread");
}

pub fn spawn_ticker(tx: Sender<Msg>) {
    std::thread::Builder::new()
        .name("ticker".into())
        .spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(100));
                if tx.send(Msg::Tick).is_err() {
                    return;
                }
            }
        })
        .expect("spawn ticker thread");
}

pub fn spawn_task(ws: Workspace, task: Task, id: u64, tx: Sender<Msg>) {
    std::thread::Builder::new()
        .name("task".into())
        .spawn(move || {
            let mut progress = |text: &str| {
                let _ = tx.send(Msg::Progress(id, text.into()));
            };
            let out = match task {
                Task::DiscoverRepository(reference) => {
                    let result =
                        skills::ops::install::parse_ref(&reference, None, None).and_then(|r| {
                            skills::repository::FetchedRepository::fetch_with_progress(
                                &ws,
                                &r,
                                None,
                                &mut progress,
                            )
                        });
                    TaskOutput::RepositoryFetched(reference, result)
                }
                Task::InstallRepository(selection) => {
                    let result = selection.fetched.install_with_progress(
                        &ws,
                        &selection.paths,
                        &selection.names,
                        &mut progress,
                    );
                    TaskOutput::RepositoryInstalled(selection, result)
                }
                Task::Scan => {
                    // Capture before scanning: changes during the scan must still
                    // invalidate the next poll, while our own writes need no second scan.
                    let stamp = skills::reconcile::watch::stamp(&ws.root, &ws.config).ok();
                    TaskOutput::Scan(ws.scan(), stamp)
                }
                Task::PollRoot => {
                    TaskOutput::RootStamp(skills::reconcile::watch::stamp(&ws.root, &ws.config))
                }
                Task::Check(keys) => {
                    let total = keys.len();
                    TaskOutput::Check(
                        keys.into_iter()
                            .enumerate()
                            .map(|(done, k)| {
                                progress(&format!("{done}/{total} complete · querying {k}…"));
                                let r = update::check(&ws, &k);
                                progress(&format!("{}/{total} complete", done + 1));
                                (k, r)
                            })
                            .collect(),
                    )
                }
                Task::Install { reference, subpath } => {
                    progress("Install: preparing source files…");
                    let out = skills::ops::install::parse_ref(&reference, None, subpath.as_deref())
                        .and_then(|r| skills::ops::install::install(&ws, &r, None));
                    TaskOutput::Installed(reference, out)
                }
                Task::Prepare(key) => {
                    progress("Update: inspecting local files and fetching upstream…");
                    let r = ws.scan().and_then(|snap| update::prepare(&ws, &snap, &key));
                    TaskOutput::Prepared(key, r)
                }
            };
            let _ = tx.send(Msg::Task(id, Box::new(out)));
        })
        .expect("spawn task thread");
}

/// Batch closures are not cloneable, so they use a dedicated worker rather than Task.
pub enum BatchWork {
    Metadata(super::app::MetaFn, Vec<String>),
    Links(Vec<skills::ops::deploy::Action>, Vec<String>),
}
pub struct BatchOutcome {
    pub conflict: Option<skills::ops::name_choices::Pending>,
    pub message: String,
    pub intent: Option<skills::history::Intent>,
    pub failed: Vec<String>,
    pub errors: Vec<String>,
}
impl BatchWork {
    fn keys(&self) -> &[String] {
        match self {
            Self::Metadata(_, keys) | Self::Links(_, keys) => keys,
        }
    }
    fn run(self, ws: &Workspace, progress: &mut dyn FnMut(&str)) -> BatchOutcome {
        match self {
            Self::Metadata(write, keys) => match write(ws) {
                Ok((message, intent)) => BatchOutcome {
                    conflict: None,
                    message,
                    intent,
                    failed: vec![],
                    errors: vec![],
                },
                Err(e)
                    if e.downcast_ref::<skills::ops::name_choices::Pending>()
                        .is_some() =>
                {
                    BatchOutcome {
                        conflict: e
                            .downcast_ref::<skills::ops::name_choices::Pending>()
                            .cloned(),
                        message: "Choose conflicting skills".into(),
                        intent: None,
                        failed: vec![],
                        errors: vec![],
                    }
                }
                Err(e) => BatchOutcome {
                    conflict: None,
                    message: "Batch metadata edit failed".into(),
                    intent: None,
                    failed: keys,
                    errors: vec![format!("{e:#}")],
                },
            },
            Self::Links(actions, keys) => {
                use skills::{history, ops::deploy};
                // Recheck names in the worker against the current filesystem.
                match ws.scan_for_links().and_then(|snap| {
                    skills::ops::name_choices::Pending::for_actions(ws, &snap, &actions)
                }) {
                    Ok(None) => {}
                    Ok(Some(pending)) => {
                        return BatchOutcome {
                            conflict: Some(pending),
                            message: "Choose conflicting skills".into(),
                            intent: None,
                            failed: vec![],
                            errors: vec![],
                        };
                    }
                    Err(error) => {
                        return BatchOutcome {
                            conflict: None,
                            message: "Batch deployment failed".into(),
                            intent: None,
                            failed: keys,
                            errors: vec![format!("{error:#}")],
                        };
                    }
                }
                let mut completed = Vec::new();
                let mut failed = std::collections::BTreeSet::new();
                let mut errors = Vec::new();
                for (i, action) in actions.iter().enumerate() {
                    progress(&format!(
                        "Deploy {}/{}: {}",
                        i + 1,
                        actions.len(),
                        action.describe()
                    ));
                    let key = match action {
                        deploy::Action::Link { skill, .. }
                        | deploy::Action::Unlink { skill, .. }
                        | deploy::Action::Relink { skill, .. }
                        | deploy::Action::Skip { skill, .. } => Some(skill),
                        _ => None,
                    };
                    let result = if let deploy::Action::Skip { reason, .. } = action {
                        Err(anyhow::anyhow!(reason.clone()))
                    } else {
                        apply_batch_link(ws, action)
                    };
                    match result {
                        Ok(changes) if changes > 0 => completed.push(action.clone()),
                        Ok(_) => {}
                        Err(error) => {
                            if let Some(key) = key {
                                failed.insert(key.clone());
                            } else {
                                failed.extend(keys.iter().cloned());
                            }
                            errors.push(format!("{}: {error:#}", action.describe()));
                        }
                    }
                }
                BatchOutcome {
                    conflict: None,
                    message: deploy::summarize(&completed),
                    intent: history::Intent::from_actions(&completed),
                    failed: failed.into_iter().collect(),
                    errors,
                }
            }
        }
    }
}
pub fn spawn_batch(
    ws: Workspace,
    work: BatchWork,
    id: u64,
    tx: Sender<Msg>,
) -> std::io::Result<()> {
    std::thread::Builder::new().name("batch".into()).spawn(move || {
        let keys = work.keys().to_vec();
        let mut progress = |text: &str| { let _ = tx.send(Msg::Progress(id, text.into())); };
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work.run(&ws, &mut progress)))
            .unwrap_or_else(|_| BatchOutcome { conflict: None, message: "Batch worker failed".into(), intent: None, failed: keys, errors: vec!["The worker stopped unexpectedly. Refresh and inspect affected skills before retrying.".into()] });
        let _ = tx.send(Msg::Task(id, Box::new(TaskOutput::Batch(outcome))));
    }).map(|_| ())
}

fn apply_batch_link(ws: &Workspace, action: &skills::ops::deploy::Action) -> Result<usize> {
    use skills::ops::deploy;
    match action {
        deploy::Action::Link { path, target, .. } => {
            if path.parent().is_some_and(|p| p.is_symlink()) {
                anyhow::bail!("Agent directory became a symlink; refresh before deploying");
            }
            if !target.is_dir() {
                anyhow::bail!(
                    "Skill directory is no longer available: {}",
                    target.display()
                );
            }
            if path.is_symlink()
                && skills::util::link_target_abs(path).as_deref() == Some(target.as_path())
            {
                return Ok(0);
            }
            // Creating directly is deliberately exclusive: never unlink an entry
            // that appeared after the plan was made, even if it is a symlink.
            std::os::unix::fs::symlink(target, path)?;
            Ok(1)
        }
        deploy::Action::Unlink { path, skill, .. } => {
            if path.parent().is_some_and(|p| p.is_symlink()) {
                anyhow::bail!("Agent directory became a symlink; refresh before undeploying");
            }
            if path.is_symlink() {
                let expected = ws.skill_path(skill);
                if skills::util::link_target_abs(path).as_deref() != Some(expected.as_path()) {
                    anyhow::bail!("Link target changed; refusing to remove {}", path.display());
                }
            }
            deploy::apply(std::slice::from_ref(action))
        }
        deploy::Action::Mkdir { path, .. } if path.is_symlink() => {
            anyhow::bail!("Agent directory became a symlink; refresh before deploying")
        }
        _ => deploy::apply(std::slice::from_ref(action)),
    }
}

#[cfg(test)]
mod batch_tests {
    use super::*;
    #[test]
    fn batch_keeps_foreign_links_and_records_only_successful_targets() {
        let root = std::env::temp_dir().join(format!("skills-batch-links-{}", std::process::id()));
        for key in ["alpha", "beta"] {
            std::fs::create_dir_all(root.join(key)).unwrap();
            std::fs::write(
                root.join(key).join("SKILL.md"),
                format!("---\nname: {key}\ndescription: test\n---\nBody\n"),
            )
            .unwrap();
        }
        skills::config::Config {
            agents: vec![],
            ..Default::default()
        }
        .save(&root)
        .unwrap();
        let ws = Workspace::open(&root).unwrap();
        let agent = root.join("agent");
        std::fs::create_dir(&agent).unwrap();
        let foreign = root.join("foreign");
        std::fs::create_dir(&foreign).unwrap();
        std::os::unix::fs::symlink(&foreign, agent.join("beta")).unwrap();
        let actions = ["alpha", "beta"]
            .iter()
            .map(|key| skills::ops::deploy::Action::Link {
                agent: "test".into(),
                skill: (*key).into(),
                path: agent.join(key),
                target: ws.skill_path(key),
            })
            .collect();
        let outcome =
            BatchWork::Links(actions, vec!["alpha".into(), "beta".into()]).run(&ws, &mut |_| {});
        assert_eq!(outcome.failed, vec!["beta"]);
        assert_eq!(std::fs::read_link(agent.join("beta")).unwrap(), foreign);
        assert_eq!(
            std::fs::read_link(agent.join("alpha")).unwrap(),
            ws.skill_path("alpha")
        );
        let Some(skills::history::Intent::Links { added, removed }) = outcome.intent else {
            panic!("expected successful link history")
        };
        assert_eq!(added, vec![("alpha".into(), "test".into())]);
        assert!(removed.is_empty());
        assert_eq!(outcome.errors.len(), 1);
        let unlink = skills::ops::deploy::Action::Unlink {
            agent: "test".into(),
            skill: "beta".into(),
            path: agent.join("beta"),
        };
        assert!(apply_batch_link(&ws, &unlink).is_err());
        assert_eq!(std::fs::read_link(agent.join("beta")).unwrap(), foreign);
        std::fs::remove_dir_all(root).unwrap();
    }
}
