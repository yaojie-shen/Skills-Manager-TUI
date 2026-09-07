//! Small reusable pieces: a unicode-aware single-line input, list navigation
//! with mouse hit-testing, text fitting, spinner frames.

use super::theme::Theme;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{ListState, Paragraph};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

// ---- text helpers ---------------------------------------------------------

pub fn width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// Truncate `s` to at most `max` display columns, appending `…` when cut.
pub fn fit(s: &str, max: usize) -> String {
    if width(s) <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut w = 0;
    for c in s.chars() {
        let cw = UnicodeWidthChar::width(c).unwrap_or(0);
        if w + cw > max - 1 {
            break;
        }
        out.push(c);
        w += cw;
    }
    out.push('…');
    out
}

/// Pad or truncate to exactly `cols` display columns.
pub fn pad(s: &str, cols: usize) -> String {
    let mut t = fit(s, cols);
    let w = width(&t);
    if w < cols {
        t.extend(std::iter::repeat_n(' ', cols - w));
    }
    t
}

pub const SPINNER: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠇"];

// ---- input ----------------------------------------------------------------

/// Single-line text input with a char cursor and horizontal scrolling.
#[derive(Debug, Default, Clone)]
pub struct Input {
    value: String,
    cursor: usize,
    scroll: usize,
    /// Last rendered inner area, used for click-to-position.
    area: Rect,
}

impl Input {
    pub fn with_value(v: &str) -> Self {
        Self {
            value: v.to_string(),
            cursor: v.chars().count(),
            ..Default::default()
        }
    }
    pub fn value(&self) -> &str {
        &self.value
    }
    pub fn set(&mut self, v: &str) {
        self.value = v.to_string();
        self.cursor = self.value.chars().count();
    }
    pub fn clear(&mut self) {
        self.value.clear();
        self.cursor = 0;
        self.scroll = 0;
    }
    pub fn is_empty(&self) -> bool {
        self.value.is_empty()
    }
    fn len(&self) -> usize {
        self.value.chars().count()
    }
    fn byte_at(&self, ci: usize) -> usize {
        self.value
            .char_indices()
            .nth(ci)
            .map(|(i, _)| i)
            .unwrap_or(self.value.len())
    }

    /// Returns true when the value changed.
    pub fn handle_key(&mut self, k: KeyEvent) -> bool {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        match k.code {
            KeyCode::Char(c) if !ctrl && !alt => {
                let i = self.byte_at(self.cursor);
                self.value.insert(i, c);
                self.cursor += 1;
                true
            }
            KeyCode::Backspace => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    let i = self.byte_at(self.cursor);
                    self.value.remove(i);
                    true
                } else {
                    false
                }
            }
            KeyCode::Delete => {
                if self.cursor < self.len() {
                    let i = self.byte_at(self.cursor);
                    self.value.remove(i);
                    true
                } else {
                    false
                }
            }
            KeyCode::Left => {
                if ctrl || alt {
                    self.cursor = self.prev_word();
                } else {
                    self.cursor = self.cursor.saturating_sub(1);
                }
                false
            }
            KeyCode::Right => {
                if ctrl || alt {
                    self.cursor = self.next_word();
                } else {
                    self.cursor = (self.cursor + 1).min(self.len());
                }
                false
            }
            KeyCode::Home => {
                self.cursor = 0;
                false
            }
            KeyCode::End => {
                self.cursor = self.len();
                false
            }
            KeyCode::Char('a') if ctrl => {
                self.cursor = 0;
                false
            }
            KeyCode::Char('e') if ctrl => {
                self.cursor = self.len();
                false
            }
            KeyCode::Char('u') if ctrl => {
                let i = self.byte_at(self.cursor);
                self.value.drain(..i);
                self.cursor = 0;
                true
            }
            KeyCode::Char('k') if ctrl => {
                let i = self.byte_at(self.cursor);
                self.value.truncate(i);
                true
            }
            KeyCode::Char('w') if ctrl => {
                let start = self.prev_word();
                let (a, b) = (self.byte_at(start), self.byte_at(self.cursor));
                self.value.drain(a..b);
                self.cursor = start;
                true
            }
            _ => false,
        }
    }

    fn prev_word(&self) -> usize {
        let chars: Vec<char> = self.value.chars().collect();
        let mut i = self.cursor;
        while i > 0 && chars[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && !chars[i - 1].is_whitespace() {
            i -= 1;
        }
        i
    }
    fn next_word(&self) -> usize {
        let chars: Vec<char> = self.value.chars().collect();
        let mut i = self.cursor;
        while i < chars.len() && !chars[i].is_whitespace() {
            i += 1;
        }
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        i
    }

    /// Place the cursor at the clicked column.
    pub fn click(&mut self, x: u16) {
        if x < self.area.x {
            self.cursor = self.scroll;
            return;
        }
        let target = (x - self.area.x) as usize;
        let mut col = 0;
        let mut idx = self.scroll;
        for c in self.value.chars().skip(self.scroll) {
            let cw = UnicodeWidthChar::width(c).unwrap_or(0);
            if col + cw > target {
                break;
            }
            col += cw;
            idx += 1;
        }
        self.cursor = idx.min(self.len());
    }

    /// Render inside `area` (already the inner area of a block). Sets the terminal cursor when focused.
    pub fn render(
        &mut self,
        f: &mut Frame,
        area: Rect,
        focused: bool,
        placeholder: &str,
        theme: &Theme,
    ) {
        self.area = area;
        let w = area.width as usize;
        // Keep the cursor visible.
        let cursor_col = |from: usize, to: usize| -> usize {
            self.value
                .chars()
                .skip(from)
                .take(to.saturating_sub(from))
                .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
                .sum()
        };
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        }
        while cursor_col(self.scroll, self.cursor) >= w.max(1) {
            self.scroll += 1;
        }
        let visible: String = self.value.chars().skip(self.scroll).collect();
        let line = if self.value.is_empty() && !placeholder.is_empty() {
            Line::from(Span::styled(fit(placeholder, w), theme.dim()))
        } else {
            Line::from(Span::raw(fit(&visible, w + 1)))
        };
        f.render_widget(Paragraph::new(line), area);
        if focused {
            let x = area.x + cursor_col(self.scroll, self.cursor) as u16;
            f.set_cursor_position((x.min(area.right().saturating_sub(1)), area.y));
        }
    }
}

// ---- list navigation ------------------------------------------------------

/// Selection + offset for a list, with mouse helpers.
#[derive(Debug, Clone)]
pub struct ListNav {
    pub state: ListState,
    /// Inner rows area from the last render (without borders).
    pub rows: Rect,
    /// Terminal rows per item (2 when items carry an excerpt line).
    pub item_height: u16,
    last_click: Option<(std::time::Instant, usize)>,
}

impl Default for ListNav {
    fn default() -> Self {
        Self {
            state: ListState::default(),
            rows: Rect::default(),
            item_height: 1,
            last_click: None,
        }
    }
}

impl ListNav {
    pub fn selected(&self) -> Option<usize> {
        self.state.selected()
    }
    pub fn select(&mut self, i: Option<usize>) {
        self.state.select(i);
    }
    pub fn clamp(&mut self, len: usize) {
        if len == 0 {
            self.state.select(None);
        } else {
            let i = self.state.selected().unwrap_or(0).min(len - 1);
            self.state.select(Some(i));
        }
    }
    pub fn move_by(&mut self, delta: i32, len: usize) {
        if len == 0 {
            self.state.select(None);
            return;
        }
        let cur = self.state.selected().unwrap_or(0) as i32;
        let next = (cur + delta).clamp(0, len as i32 - 1);
        self.state.select(Some(next as usize));
    }
    pub fn first(&mut self, len: usize) {
        self.state.select(if len == 0 { None } else { Some(0) });
    }
    pub fn last(&mut self, len: usize) {
        self.state
            .select(if len == 0 { None } else { Some(len - 1) });
    }
    pub fn page(&self) -> i32 {
        (self.rows.height as i32 / self.item_height.max(1) as i32).max(1)
    }
    /// Row index under the pointer, if any.
    pub fn row_at(&self, y: u16, len: usize) -> Option<usize> {
        if y < self.rows.y || y >= self.rows.bottom() {
            return None;
        }
        let i = self.state.offset() + ((y - self.rows.y) / self.item_height.max(1)) as usize;
        (i < len).then_some(i)
    }
    /// Select the clicked row. Returns `(index, is_double_click)`.
    pub fn click(&mut self, y: u16, len: usize) -> Option<(usize, bool)> {
        let i = self.row_at(y, len)?;
        let now = std::time::Instant::now();
        let double = matches!(self.last_click, Some((t, j)) if j == i && now.duration_since(t).as_millis() < 400);
        self.last_click = Some((now, i));
        self.state.select(Some(i));
        Some((i, double))
    }
    /// Remember the rows area of a list rendered with a bordered block.
    pub fn set_area_from_block(&mut self, outer: Rect) {
        self.rows = Rect {
            x: outer.x + 1,
            y: outer.y + 1,
            width: outer.width.saturating_sub(2),
            height: outer.height.saturating_sub(2),
        };
    }
}

/// A clickable button label like `[ Apply ]`.
pub fn button(label: &str, active: bool, theme: &Theme) -> Span<'static> {
    let text = format!("[ {label} ]");
    let style = if active {
        Style::default()
            .fg(theme.accent)
            .add_modifier(ratatui::style::Modifier::BOLD)
    } else {
        theme.dim()
    };
    Span::styled(text, style)
}
