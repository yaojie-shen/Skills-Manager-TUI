//! Input thread and background tasks. Everything reaches the main loop as a `Msg`.

use anyhow::Result;
use crossterm::event::{Event, KeyEvent, KeyEventKind, MouseEvent};
use skills::Workspace;
use skills::ops::update::{self, CheckResult, Prepared};
use skills::reconcile::Snapshot;
use std::sync::mpsc::Sender;
use std::time::Duration;

pub enum Msg {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Resize,
    Tick,
    Task(Box<TaskOutput>),
}

/// Long-running work executed off the UI thread.
#[derive(Debug, Clone)]
pub enum Task {
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
    Scan(Result<Snapshot>),
    Check(Vec<(String, Result<CheckResult>)>),
    Prepared(String, Result<Prepared>),
    /// The reference asked for, and the key it landed under.
    Installed(String, Result<String>),
}

pub fn spawn_input(tx: Sender<Msg>) {
    std::thread::Builder::new()
        .name("input".into())
        .spawn(move || {
            loop {
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

pub fn spawn_task(ws: Workspace, task: Task, tx: Sender<Msg>) {
    std::thread::Builder::new()
        .name("task".into())
        .spawn(move || {
            let out = match task {
                Task::Scan => TaskOutput::Scan(ws.scan()),
                Task::Check(keys) => TaskOutput::Check(
                    keys.into_iter()
                        .map(|k| {
                            let r = update::check(&ws, &k);
                            (k, r)
                        })
                        .collect(),
                ),
                Task::Install { reference, subpath } => {
                    let out = skills::ops::install::parse_ref(&reference, None, subpath.as_deref())
                        .and_then(|r| skills::ops::install::install(&ws, &r, None));
                    TaskOutput::Installed(reference, out)
                }
                Task::Prepare(key) => {
                    let r = ws.scan().and_then(|snap| update::prepare(&ws, &snap, &key));
                    TaskOutput::Prepared(key, r)
                }
            };
            let _ = tx.send(Msg::Task(Box::new(out)));
        })
        .expect("spawn task thread");
}
