//! One module per tab. Views keep their own cursor and layout rects; the
//! data always comes from `Ctx`.

pub mod agents;
pub mod cards;
pub(crate) mod completion;
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

pub trait View {
    /// Called after every new snapshot.
    fn refresh(&mut self, ctx: &Ctx);
    /// Called when the tab becomes the active one after being away, before
    /// any key reaches it. A view that keeps a focus of its own can put it
    /// back where someone returning expects to find it; most have nothing to
    /// reset, so the default does nothing.
    fn enter(&mut self) {}
    fn handle_key(&mut self, k: KeyEvent, ctx: &Ctx) -> Vec<Action>;
    fn handle_mouse(&mut self, m: MouseEvent, ctx: &Ctx) -> Vec<Action>;
    fn draw(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx);
    fn hints(&self) -> Hints;
}

/// Shared rendering of a skill status glyph.
pub fn status_glyph(
    s: &skills::reconcile::SkillStatus,
    th: &super::theme::Theme,
) -> ratatui::text::Span<'static> {
    use ratatui::text::Span;
    use skills::reconcile::SkillStatus::*;
    match s {
        Managed { no_baseline: false } => Span::styled("●", th.ok()),
        // Same shape as managed, since it is managed; the colour carries the
        // caveat. A half-filled circle here would read as a partial preset,
        // which is what that glyph means everywhere else.
        Managed { no_baseline: true } => Span::styled("●", th.warn()),
        Unmanaged => Span::styled("○", th.dim()),
        Modified => Span::styled("✎", th.warn()),
        Missing => Span::styled("✗", th.err()),
        Renamed { .. } => Span::styled("↪", th.warn()),
        Invalid { .. } | CorruptMeta { .. } => Span::styled("!", th.err()),
    }
}

pub fn status_text(s: &skills::reconcile::SkillStatus) -> String {
    use skills::reconcile::SkillStatus::*;
    match s {
        Managed { no_baseline: true } => "managed, no baseline".into(),
        Renamed { to } => format!("renamed? → {to}"),
        Invalid { reason } => format!("invalid: {reason}"),
        CorruptMeta { error } => format!("corrupt metadata: {error}"),
        other => other.label().into(),
    }
}

/// Split an area into a left list and a right detail pane, stacking vertically on narrow terminals.
pub fn split_panes(area: Rect, left_pct: u16) -> (Rect, Rect) {
    use ratatui::layout::{Constraint, Direction, Layout};
    if area.width < 90 {
        let r = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
            .split(area);
        (r[0], r[1])
    } else {
        let r = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(left_pct),
                Constraint::Percentage(100 - left_pct),
            ])
            .split(area);
        (r[0], r[1])
    }
}

/// Mouse wheel delta for list navigation.
pub fn wheel(m: &MouseEvent) -> Option<i32> {
    use crossterm::event::MouseEventKind::*;
    match m.kind {
        ScrollUp => Some(-3),
        ScrollDown => Some(3),
        _ => None,
    }
}
