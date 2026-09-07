//! Notifications that appear in the bottom-right corner and fade on their own.
//!
//! Each carries a countdown drawn as a pie that empties clockwise, the way a
//! desktop notification shows its remaining time. Errors have no countdown:
//! something went wrong and the message waits to be read.

use super::app::Level;
use super::theme::Theme;
use super::widgets::{fit, width};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use std::time::{Duration, Instant};

/// How long a notification stays before it goes. An error gets longer: it has
/// to be read, but it should not sit in the corner forever either.
const LIFETIME: Duration = Duration::from_secs(5);
const ERROR_LIFETIME: Duration = Duration::from_secs(20);
/// Most shown at once; older ones are dropped rather than pushed off screen.
const MAX_VISIBLE: usize = 3;

/// A full circle emptying clockwise. Geometric Shapes has quarters and no more,
/// so the countdown moves in five steps rather than sweeping smoothly.
const PIE: [&str; 5] = ["●", "◕", "◑", "◔", "○"];

pub struct Toast {
    pub text: String,
    pub level: Level,
    at: Instant,
}

impl Toast {
    pub fn new(text: impl Into<String>, level: Level) -> Self {
        Self {
            text: text.into(),
            level,
            at: Instant::now(),
        }
    }

    fn lifetime(&self) -> Duration {
        if self.level == Level::Error {
            ERROR_LIFETIME
        } else {
            LIFETIME
        }
    }

    fn done(&self) -> bool {
        self.at.elapsed() >= self.lifetime()
    }

    /// The countdown glyph, full at first and empty just before it goes.
    fn pie(&self) -> &'static str {
        let ratio = self.at.elapsed().as_secs_f32() / self.lifetime().as_secs_f32();
        let step = (ratio * PIE.len() as f32) as usize;
        PIE[step.min(PIE.len() - 1)]
    }
}

/// The stack of live notifications.
#[derive(Default)]
pub struct Toasts {
    items: Vec<Toast>,
}

impl Toasts {
    pub fn push(&mut self, text: impl Into<String>, level: Level) {
        self.items.push(Toast::new(text, level));
        // Keep the newest; a burst of writes should not bury the last result.
        let excess = self.items.len().saturating_sub(MAX_VISIBLE);
        self.items.drain(..excess);
    }

    /// Drop whatever has run out. Called on every tick.
    pub fn expire(&mut self) {
        self.items.retain(|t| !t.done());
    }

    /// Draw the stack above the footer, hugging the right edge. Returns the
    /// area covered so callers can avoid drawing under it.
    pub fn draw(&self, f: &mut Frame, area: Rect, th: &Theme) -> Option<Rect> {
        if self.items.is_empty() {
            return None;
        }
        let max_text = (area.width as usize).saturating_sub(12).min(56);
        let lines: Vec<(String, Level, &'static str)> = self
            .items
            .iter()
            .map(|t| (fit(&t.text, max_text), t.level, t.pie()))
            .collect();
        let inner_w = lines
            .iter()
            .map(|(text, ..)| width(text) + 4)
            .max()
            .unwrap_or(10);
        let w = (inner_w + 2) as u16;
        let h = lines.len() as u16 + 2;
        if area.width < w + 4 || area.height < h + 4 {
            return None;
        }
        // Bottom-right, clear of the footer and of the panel border below it,
        // so the two frames do not run into each other.
        let rect = Rect::new(
            area.right().saturating_sub(w + 2),
            area.bottom().saturating_sub(h + 2),
            w,
            h,
        );
        f.render_widget(Clear, rect);
        let body: Vec<Line> = lines
            .iter()
            .map(|(text, level, pie)| {
                let style = match level {
                    Level::Info => th.dim(),
                    Level::Ok => th.ok(),
                    Level::Error => th.err(),
                };
                Line::from(vec![
                    Span::styled(format!(" {pie} "), style),
                    Span::raw(text.clone()),
                ])
            })
            .collect();
        let border = match lines.last().map(|(_, l, _)| l) {
            Some(Level::Error) => th.err(),
            _ => th.dim(),
        };
        f.render_widget(
            Paragraph::new(body).block(
                ratatui::widgets::Block::default()
                    .borders(ratatui::widgets::Borders::ALL)
                    .border_type(ratatui::widgets::BorderType::Rounded)
                    .border_style(border),
            ),
            rect,
        );
        Some(rect)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pie_empties_over_the_lifetime() {
        let fresh = Toast::new("x", Level::Ok);
        assert_eq!(fresh.pie(), "●");
        assert!(!fresh.done());

        // Walk the clock by hand rather than sleeping through five seconds.
        let mut aged = Toast::new("x", Level::Ok);
        for (elapsed, want) in [(0, "●"), (1, "◕"), (2, "◑"), (3, "◔"), (4, "○")] {
            aged.at = Instant::now() - Duration::from_secs(elapsed) - Duration::from_millis(100);
            assert_eq!(aged.pie(), want, "at {elapsed}s");
            assert!(!aged.done(), "at {elapsed}s");
        }
        aged.at = Instant::now() - LIFETIME;
        assert!(aged.done());
    }

    #[test]
    fn errors_linger_but_still_go() {
        let mut e = Toast::new("boom", Level::Error);
        // Well past an ordinary lifetime, and still there to be read.
        e.at = Instant::now() - LIFETIME * 2;
        assert!(!e.done());
        assert_eq!(e.pie(), "◑", "halfway through the longer error lifetime");
        e.at = Instant::now() - ERROR_LIFETIME;
        assert!(e.done());
    }

    #[test]
    fn the_newest_notifications_win() {
        let mut ts = Toasts::default();
        for i in 0..5 {
            ts.push(format!("n{i}"), Level::Ok);
        }
        assert_eq!(ts.items.len(), MAX_VISIBLE);
        assert_eq!(ts.items.last().unwrap().text, "n4");
        assert_eq!(ts.items[0].text, "n2");
    }
}
