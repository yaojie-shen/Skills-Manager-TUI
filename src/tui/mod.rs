//! Terminal UI (search-first).
//!
//! Structure: `event` feeds a single `Msg` channel from the input thread,
//! a ticker and background tasks; `app` owns the state and reduces messages
//! into view changes; `views` render and handle input per tab; `modal`
//! implements overlays. All writes go through `skills::ops`, like the CLI.

mod app;
mod batch;
mod event;
mod icons;
mod markdown;
mod modal;
mod repository_picker;
mod theme;
mod toast;
mod views;
mod widgets;

use anyhow::Result;
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use std::path::Path;
use std::sync::mpsc;

type Term = ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>;

pub fn run(root: Option<&Path>) -> Result<()> {
    let root = skills::paths::resolve_root(root)?;
    let ws = skills::Workspace::open(&root)?;
    let (tx, rx) = mpsc::channel();
    let mut app = app::App::new(ws, tx.clone())?;

    install_panic_hook();
    let mut terminal = enter()?;
    let gate = std::sync::Arc::new(event::InputGate::default());
    event::spawn_input(tx.clone(), gate.clone());
    event::spawn_ticker(tx);

    let result = (|| -> Result<()> {
        loop {
            terminal.draw(|f| app.draw(f))?;
            if let Some(req) = app.take_external() {
                gate.hold();
                leave(&mut terminal)?;
                let outcome = app.run_external(req);
                terminal = enter()?;
                gate.release();
                app.finish_external(outcome);
                continue;
            }
            let msg = rx.recv()?;
            app.handle(msg);
            // Drain anything else queued so bursts of ticks/keys coalesce into one frame.
            while let Ok(m) = rx.try_recv() {
                app.handle(m);
                if app.should_quit() {
                    break;
                }
            }
            if app.should_quit() {
                return Ok(());
            }
        }
    })();

    leave(&mut terminal)?;
    result
}

fn enter() -> Result<Term> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = ratatui::Terminal::new(backend)?;
    terminal.clear()?;
    Ok(terminal)
}

fn leave(terminal: &mut Term) -> Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;
    Ok(())
}

/// Restore the terminal before printing a panic so the message is readable.
fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(std::io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
        default(info);
    }));
}
