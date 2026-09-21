//! One module per tab. Views keep their own cursor and layout rects; the
//! data always comes from `Ctx`.

pub mod agents;
pub(crate) mod filter;
pub mod health;
pub mod matrix;
pub mod presets;
pub mod preview;
pub mod repos;
pub mod search;
pub mod tags;

use super::app::{Action, Ctx, Hints};
use crossterm::event::{KeyEvent, MouseEvent};
use ratatui::Frame;
use ratatui::layout::Rect;

use crate::tui::components::context_menu::{Command, Request, Target};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) enum SplitFocus {
    Filter,
    #[default]
    Groups,
    Members,
}

pub trait View {
    fn overlay_open(&self) -> bool {
        false
    }
    fn handle_control_key(&mut self, _key: KeyEvent, _ctx: &Ctx) -> Vec<Action> {
        vec![]
    }
    fn actions_menu(&self, _ctx: &Ctx) -> Option<Request> {
        None
    }
    fn context_menu(&mut self, _x: u16, _y: u16, _ctx: &Ctx) -> Option<Request> {
        None
    }
    fn context_execute(&mut self, _target: &Target, _command: Command, _ctx: &Ctx) -> Vec<Action> {
        vec![Action::Error(
            "Target changed; reopen the context menu".into(),
        )]
    }

    fn status(&self, _ctx: &Ctx) -> String {
        String::new()
    }
    /// Called after every new snapshot.
    fn refresh(&mut self, ctx: &Ctx);
    /// Called when the tab becomes the active one after being away, before
    /// any key reaches it. A view that keeps a focus of its own can put it
    /// back where someone returning expects to find it; most have nothing to
    /// reset, so the default does nothing.
    fn enter(&mut self) {}
    /// Enter the page from the tab strip without changing selection or filters.
    fn focus_root(&mut self) {}
    /// Directional entry lands on the topmost interactive region.
    fn focus_from_above(&mut self) {
        self.focus_root();
    }
    fn handle_key(&mut self, k: KeyEvent, ctx: &Ctx) -> Vec<Action>;
    fn handle_mouse(&mut self, m: MouseEvent, ctx: &Ctx) -> Vec<Action>;
    fn draw(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx);
    fn hints(&self) -> Hints;
}

/// Mouse wheel delta for list navigation.
pub fn wheel(m: &MouseEvent, ctx: &Ctx) -> Option<i32> {
    use crossterm::event::MouseEventKind::*;
    match m.kind {
        ScrollUp => Some(-ctx.settings.interaction.wheel_rows),
        ScrollDown => Some(ctx.settings.interaction.wheel_rows),
        _ => None,
    }
}

#[cfg(test)]
mod navigation_tests;

#[cfg(test)]
mod presentation_tests;
