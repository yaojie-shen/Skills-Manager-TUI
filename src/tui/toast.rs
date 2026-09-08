//! Notifications that appear in the bottom-right corner and fade on their own.
//!
//! A fixed status marker identifies each result. A thin line below the message
//! shrinks with its remaining lifetime; errors stay longer than ordinary results.
//! Active tasks instead show live details and remain until explicitly finished.

use super::app::Level;
use super::theme::Theme;
use super::widgets::{SPINNER, fit, width};
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
    detail: Option<String>,
}

impl Toast {
    pub fn new(text: impl Into<String>, level: Level) -> Self {
        Self {
            text: text.into(),
            level,
            at: Instant::now(),
            detail: None,
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
        self.detail.is_none() && self.at.elapsed() >= self.lifetime()
    }

    fn marker(&self) -> &'static str {
        if self.detail.is_some() {
            let frame = (self.at.elapsed().as_millis() / 100) % SPINNER.len() as u128;
            return SPINNER[frame as usize];
        }
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
    running: std::collections::BTreeMap<u64, Toast>,
}

impl Toasts {
    pub fn start(&mut self, id: u64, text: String) {
        let mut toast = Toast::new(text, Level::Info);
        toast.detail = Some("Starting…".into());
        self.running.insert(id, toast);
    }

    pub fn progress(&mut self, id: u64, detail: String) {
        if let Some(toast) = self.running.get_mut(&id) {
            toast.detail = Some(detail);
        }
    }

    pub fn running_details(&self) -> Vec<String> {
        self.running
            .values()
            .map(|task| {
                format!(
                    "{} — {}",
                    task.text,
                    task.detail.as_deref().unwrap_or("Running…")
                )
            })
            .collect()
    }

    pub fn finish(&mut self, id: u64) {
        self.running.remove(&id);
    }

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
        if self.items.is_empty() && self.running.is_empty() {
            return None;
        }
        let max_text = (area.width as usize).saturating_sub(12).min(
            if self.running.is_empty() && !self.items.iter().any(|t| t.level == Level::Error) {
                56
            } else {
                100
            },
        );
        let budget = area.height.saturating_sub(6) as usize;
        if budget < 2 || max_text == 0 {
            return None;
        }
        let active = self.running.last_key_value().map(|(_, t)| Toast {
            text: if self.running.len() > 1 {
                format!("{} tasks running · {}", self.running.len(), t.text)
            } else {
                t.text.clone()
            },
            level: t.level,
            at: t.at,
            detail: t.detail.clone(),
        });
        // Reserve space for persistent work, then fit newest results from below.
        let mut remaining = budget.saturating_sub(usize::from(active.is_some()) * 2);
        let mut lines: Vec<(Vec<String>, &Toast)> = Vec::new();
        for toast in self
            .items
            .iter()
            .rev()
            .take(MAX_VISIBLE - usize::from(active.is_some()))
        {
            if remaining < 2 {
                break;
            }
            let mut message = if toast.level == Level::Error {
                wrap_message(&toast.text, max_text)
            } else {
                vec![fit(&toast.text, max_text)]
            };
            if message.len() + 1 > remaining {
                if !lines.is_empty() {
                    break;
                }
                // On tiny terminals keep both the beginning and the final cause.
                let tail = message.pop().unwrap_or_default();
                message.truncate(remaining.saturating_sub(2));
                message.push(tail);
            }
            remaining -= message.len() + 1;
            lines.push((message, toast));
        }
        lines.reverse();
        if let Some(toast) = active.as_ref() {
            lines.push((vec![fit(&toast.text, max_text)], toast));
        }
        if lines.is_empty() {
            return None;
        }
        let inner_w = lines
            .iter()
            .map(|(message, t)| {
                message
                    .iter()
                    .map(|line| width(line))
                    .max()
                    .unwrap_or(0)
                    .max(t.detail.as_ref().map_or(0, |d| width(&fit(d, max_text))))
                    + 4
            })
            .max()
            .unwrap_or(10);
        let w = (inner_w + 2) as u16;
        let h = (lines
            .iter()
            .map(|(message, _)| message.len() + 1)
            .sum::<usize>()
            + 2) as u16;
        if area.width < w + 4 || area.height < h + 4 {
            return None;
        }
        let rect = Rect::new(
            area.right().saturating_sub(w + 2),
            area.bottom().saturating_sub(h + 2),
            w,
            h,
        );
        f.render_widget(Clear, rect);
        let mut body = Vec::new();
        for (message, toast) in &lines {
            let style = match toast.level {
                Level::Info => th.dim(),
                Level::Ok => th.ok(),
                Level::Error => th.err(),
            };
            for (i, line) in message.iter().enumerate() {
                body.push(Line::from(vec![
                    Span::styled(
                        if i == 0 {
                            format!(" {} ", toast.marker())
                        } else {
                            "   ".into()
                        },
                        style,
                    ),
                    Span::raw(line.clone()),
                ]));
            }
            body.push(Line::from(vec![
                Span::raw(" "),
                Span::styled(
                    toast
                        .detail
                        .as_ref()
                        .map(|d| fit(d, max_text))
                        .unwrap_or_else(|| "━".repeat(toast.bar_width(inner_w.saturating_sub(2)))),
                    style,
                ),
            ]));
        }
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

/// Wrap without dropping the final error cause, measuring terminal cells.
fn wrap_message(text: &str, columns: usize) -> Vec<String> {
    use unicode_segmentation::UnicodeSegmentation;
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut used = 0;
    for glyph in text.graphemes(true) {
        if glyph.contains('\n') || glyph.contains('\r') {
            lines.push(std::mem::take(&mut line));
            used = 0;
            continue;
        }
        let cells = width(glyph);
        if used + cells > columns && !line.is_empty() {
            lines.push(std::mem::take(&mut line));
            used = 0;
        }
        line.push_str(glyph);
        used += cells;
    }
    if !line.is_empty() || lines.is_empty() {
        lines.push(line);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_wrap_through_the_final_cause_and_keep_active_progress() {
        use ratatui::{Terminal, backend::TestBackend};
        let message = "install: cannot parse source 'https://example.com/team/tools': 路径解析失败: expected owner/repository (not a valid repository reference)";
        let mut toasts = Toasts::default();
        toasts.start(1, "Check upstream".into());
        toasts.progress(1, "2/3 complete".into());
        toasts.push(message, Level::Error);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|f| {
                toasts.draw(f, f.area(), &Theme::default());
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let rendered: String = buffer.content.iter().map(|c| c.symbol()).collect();
        for line in wrap_message(message, 68) {
            assert!(
                rendered.replace(' ', "").contains(&line.replace(' ', "")),
                "missing error text: {line}"
            );
            assert!(width(&line) <= 68);
        }
        assert!(rendered.contains("2/3 complete"));
        assert!(!rendered.contains('…'));
        let text = "文件系统路径 👩‍💻 malformed value";
        let wrapped = wrap_message(text, 9);
        assert_eq!(wrapped.concat(), text);
        assert!(wrapped.iter().all(|line| width(line) <= 9));
    }

    #[test]
    fn active_tasks_survive_expiry_and_results_until_the_matching_completion() {
        use ratatui::{Terminal, backend::TestBackend};
        let mut ts = Toasts::default();
        ts.start(1, "Clone first".into());
        ts.start(2, "Clone second".into());
        ts.progress(2, "Receiving objects: 42%".into());
        for t in ts.running.values_mut() {
            t.at = Instant::now() - Duration::from_secs(600);
        }
        for i in 0..5 {
            ts.push(format!("saved {i}"), Level::Ok);
        }
        ts.expire();
        let mut terminal = Terminal::new(TestBackend::new(100, 8)).unwrap();
        terminal
            .draw(|f| {
                ts.draw(f, f.area(), &Theme::default());
            })
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("2 tasks running"));
        assert!(text.contains("Receiving objects: 42%"));
        assert!(
            !text.contains('━'),
            "active progress must not look like an expiry timer"
        );
        ts.finish(2);
        assert!(ts.running.contains_key(&1));
        ts.progress(2, "late update".into());
        assert_eq!(ts.running.len(), 1);
        ts.finish(1);
        assert!(ts.running.is_empty());
    }

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
