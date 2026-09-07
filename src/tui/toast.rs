//! Notifications that appear in the bottom-right corner and fade on their own.
//!
//! A fixed status marker identifies each result. A thin line below the message
//! shrinks with its remaining lifetime; errors stay longer than ordinary results.

use super::app::Level;
use super::theme::Theme;
use super::widgets::{fit, width};
use crate::tui::widgets::OverlayClear as Clear;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use std::time::{Duration, Instant};

/// How long a notification stays before it goes. An error gets longer: it has
/// to be read, but it should not sit in the corner forever either.
const LIFETIME: Duration = Duration::from_secs(5);
const ERROR_LIFETIME: Duration = Duration::from_secs(20);
/// Most shown at once; older ones are dropped rather than pushed off screen.
const MAX_VISIBLE: usize = 3;

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

    fn marker(&self) -> &'static str {
        match self.level {
            Level::Info => "i",
            Level::Ok => "✓",
            Level::Error => "!",
        }
    }

    /// Keep a sliver visible until expiry, with the full line at creation.
    fn bar_width(&self, width: usize) -> usize {
        let remaining = 1.0 - self.at.elapsed().as_secs_f64() / self.lifetime().as_secs_f64();
        (remaining.clamp(0.0, 1.0) * width as f64).ceil() as usize
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
        // Keep the newest messages when the terminal cannot fit the whole stack.
        let visible = self
            .items
            .len()
            .min(area.height.saturating_sub(6) as usize / 2);
        if visible == 0 {
            return None;
        }
        let lines: Vec<_> = self.items[self.items.len() - visible..]
            .iter()
            .map(|t| (fit(&t.text, max_text), t))
            .collect();
        let inner_w = lines
            .iter()
            .map(|(text, ..)| width(text) + 4)
            .max()
            .unwrap_or(10);
        let w = (inner_w + 2) as u16;
        let h = lines.len() as u16 * 2 + 2;
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
            .flat_map(|(text, toast)| {
                let style = match toast.level {
                    Level::Info => th.dim(),
                    Level::Ok => th.ok(),
                    Level::Error => th.err(),
                };
                [
                    Line::from(vec![
                        Span::styled(format!(" {} ", toast.marker()), style),
                        Span::raw(text.clone()),
                    ]),
                    Line::from(vec![
                        Span::raw(" "),
                        Span::styled(
                            "━".repeat(toast.bar_width(inner_w.saturating_sub(2))),
                            style,
                        ),
                    ]),
                ]
            })
            .collect();
        let border = match lines.last().map(|(_, t)| t.level) {
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
    fn the_line_shrinks_without_changing_the_status_marker() {
        let mut toast = Toast::new("saved", Level::Ok);
        assert_eq!(toast.bar_width(20), 20);
        for (elapsed_ms, want) in [(1000, 16), (2500, 10), (4900, 1), (5000, 0)] {
            toast.at = Instant::now() - Duration::from_millis(elapsed_ms);
            assert_eq!(toast.bar_width(20), want);
            assert_eq!(toast.marker(), "✓");
        }
        assert!(toast.done());
    }

    #[test]
    fn errors_use_their_longer_lifetime() {
        let mut toast = Toast::new("failed", Level::Error);
        toast.at = Instant::now() - Duration::from_secs(10);
        assert_eq!(toast.bar_width(20), 10);
        assert_eq!(toast.marker(), "!");
        assert!(!toast.done());
        toast.at = Instant::now() - ERROR_LIFETIME;
        assert_eq!(toast.bar_width(20), 0);
        assert!(toast.done());
    }

    #[test]
    fn each_message_has_its_own_line_and_small_windows_keep_the_newest() {
        use ratatui::{Terminal, backend::TestBackend};
        let mut toasts = Toasts::default();
        toasts.push("old", Level::Info);
        toasts.push("saved", Level::Ok);
        toasts.push("failed", Level::Error);
        toasts.items[1].at = Instant::now() - Duration::from_millis(2500);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let mut rect = Rect::default();
        terminal
            .draw(|f| rect = toasts.draw(f, f.area(), &Theme::default()).unwrap())
            .unwrap();
        let buffer = terminal.backend().buffer();
        let bars: Vec<_> = (rect.y..rect.bottom())
            .map(|y| {
                (rect.x..rect.right())
                    .filter(|&x| buffer[(x, y)].symbol() == "━")
                    .count()
            })
            .filter(|&n| n > 0)
            .collect();
        assert_eq!(bars.len(), 3);
        assert!(bars[1] < bars[0]);
        assert_eq!(bars[0], bars[2]);

        terminal.resize(Rect::new(0, 0, 80, 10)).unwrap();
        terminal.backend_mut().resize(80, 10);
        terminal
            .draw(|f| rect = toasts.draw(f, f.area(), &Theme::default()).unwrap())
            .unwrap();
        let buffer = terminal.backend().buffer();
        let text: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
        assert!(!text.contains("old"));
        assert!(text.contains("saved"));
        assert!(text.contains("failed"));
        assert_eq!(rect.height, 6);
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
