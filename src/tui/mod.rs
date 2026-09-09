//! Terminal UI (search-first).
//!
//! Structure: `event` feeds a single `Msg` channel from the input thread,
//! a ticker and background tasks; `app` owns the state and reduces messages
//! into view changes; `views` render and handle input per tab; `modal`
//! implements overlays. All writes go through `skills::ops`, like the CLI.

mod app;
mod batch;
mod deploy_picker;
mod event;
mod icons;
#[cfg(test)]
mod latency_bench;
mod markdown;
mod modal;
mod repository_picker;
mod theme;
mod toast;
mod views;
mod widgets;

use anyhow::Result;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use std::sync::mpsc;

type Term = ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>;

pub fn run(ws: skills::Workspace, launch_dir: Option<&std::path::Path>) -> Result<()> {
    let (tx, rx) = mpsc::channel();
    let mut app = app::App::new_with_launch_directory(ws, tx.clone(), launch_dir)?;

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
    let result = (|| -> Result<Term> {
        enable_raw_mode()?;
        let mut stdout = std::io::stdout();
        execute!(
            stdout,
            EnterAlternateScreen,
            EnableMouseCapture,
            EnableBracketedPaste
        )?;
        let backend = ratatui::backend::CrosstermBackend::new(stdout);
        let mut terminal = ratatui::Terminal::new(backend)?;
        terminal.clear()?;
        Ok(terminal)
    })();
    // An error after enabling a mode must not leave the shell in that mode.
    if result.is_err() {
        let _ = restore(&mut std::io::stdout());
    }
    result
}

fn restore(writer: &mut impl std::io::Write) -> Result<()> {
    // Try both cleanup steps even if either fails.
    let modes = execute!(
        writer,
        DisableBracketedPaste,
        DisableMouseCapture,
        LeaveAlternateScreen,
        crossterm::cursor::Show
    );
    let raw = disable_raw_mode();
    modes?;
    raw?;
    Ok(())
}

fn leave(terminal: &mut Term) -> Result<()> {
    restore(terminal.backend_mut())
}

/// Restore the terminal before printing a panic so the message is readable.
fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = restore(&mut std::io::stdout());
        default(info);
    }));
}
