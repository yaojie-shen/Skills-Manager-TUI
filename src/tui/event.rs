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
    RepositoryFetched(String, Result<skills::repository::FetchedRepository>),
    RepositoryInstalled(
        Box<super::repository_picker::InstallSelection>,
        Result<Vec<String>>,
    ),
    Scan(Result<Snapshot>),
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
                Task::Scan => TaskOutput::Scan(ws.scan()),
                Task::Check(keys) => TaskOutput::Check(
                    keys.into_iter()
                        .map(|k| {
                            progress(&format!("Check: querying upstream for {k}…"));
                            let r = update::check(&ws, &k);
                            (k, r)
                        })
                        .collect(),
                ),
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
