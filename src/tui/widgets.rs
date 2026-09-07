//! Small reusable pieces: a unicode-aware single-line input, list navigation
//! with mouse hit-testing, text fitting, spinner frames.

use super::theme::Theme;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::buffer::CellWidth;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{ListState, Paragraph};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Clear an overlay without leaving half of a wide background character at its edge.
pub struct OverlayClear;

impl ratatui::widgets::Widget for OverlayClear {
    fn render(self, area: Rect, buf: &mut ratatui::buffer::Buffer) {
        let area = area.intersection(*buf.area());
        if area.is_empty() {
            return;
        }
        for y in area.top()..area.bottom() {
            // Walk whole symbols: continuation cells of a wide glyph look like spaces.
            let mut x = buf.area().left();
            while x < area.right() {
                let end = x
                    .saturating_add(buf[(x, y)].cell_width().max(1))
                    .min(buf.area().right());
                if (x < area.left() && end > area.left()) || end > area.right() {
                    for col in x..end {
                        buf[(col, y)].reset();
                    }
                }
                x = end;
            }
        }
        ratatui::widgets::Clear.render(area, buf);
    }
}

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

// ---- scrollbar track ------------------------------------------------------

/// The clickable track of a vertical scrollbar: the cells it occupies plus the
/// mapping from a terminal row back to an item index. The mapping is in item
/// space, which is what the user aims at, not the terminal rows an item
/// happens to occupy.
#[derive(Debug, Default, Clone, Copy)]
pub struct ScrollTrack {
    rect: Rect,
}

impl ScrollTrack {
    pub fn set(&mut self, rect: Rect) {
        self.rect = rect;
    }
    /// A track with no rect is not on screen and must not swallow clicks.
    pub fn clear(&mut self) {
        self.rect = Rect::default();
    }
    pub fn hit(&self, x: u16, y: u16) -> bool {
        self.rect.width > 0
            && self.rect.height > 0
            && x >= self.rect.x
            && x < self.rect.right()
            && y >= self.rect.y
            && y < self.rect.bottom()
    }
    /// Item index for a row on the track. `y` is clamped to the track's ends so
    /// a drag that runs past either edge pins to the first or last item.
    pub fn index_at(&self, y: u16, len: usize) -> Option<usize> {
        if len == 0 || self.rect.height == 0 {
            return None;
        }
        let span = (self.rect.height - 1) as usize;
        if span == 0 {
            return Some(0);
        }
        let off = (y.clamp(self.rect.y, self.rect.bottom() - 1) - self.rect.y) as usize;
        Some((off * (len - 1) + span / 2) / span)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn overlay_edges_are_emitted_when_background_contains_wide_characters() {
        use ratatui::{
            buffer::Buffer,
            widgets::{Block, Borders, Widget},
        };
        for text in ["中文中文中文中文中文中文", "🙂🙂🙂🙂🙂🙂🙂🙂🙂🙂🙂🙂"]
        {
            let mut background = Buffer::empty(Rect::new(0, 0, 24, 6));
            for y in 0..6 {
                background.set_string(0, y, text, Style::default());
            }
            let mut overlay = background.clone();
            let area = Rect::new(3, 1, 14, 4);
            OverlayClear.render(area, &mut overlay);
            Block::default()
                .borders(Borders::ALL)
                .render(area, &mut overlay);
            let updates = background.diff(&overlay);
            for y in 2..4 {
                assert!(
                    updates
                        .iter()
                        .any(|(x, row, cell)| *x == 3 && *row == y && cell.symbol() == "│"),
                    "left border missing at row {y}"
                );
                assert!(
                    updates
                        .iter()
                        .any(|(x, row, cell)| *x == 16 && *row == y && cell.symbol() == "│")
                );
            }
            assert_eq!(overlay[(2, 2)].symbol(), " ");
            assert_eq!(overlay[(17, 2)].symbol(), " ");
            assert_eq!(overlay[(0, 2)].symbol(), background[(0, 2)].symbol());
            assert_eq!(overlay[(18, 2)].symbol(), background[(18, 2)].symbol());
            // Closing the overlay restores the full background character.
            assert!(
                overlay
                    .diff(&background)
                    .iter()
                    .any(|(x, y, _)| *x == 2 && *y == 2)
            );
        }
    }

    use super::*;

    fn track() -> ScrollTrack {
        let mut t = ScrollTrack::default();
        t.set(Rect {
            x: 40,
            y: 5,
            width: 1,
            height: 10,
        });
        t
    }

    #[test]
    fn track_hit_testing() {
        let t = track();
        assert!(t.hit(40, 5));
        assert!(t.hit(40, 14));
        assert!(!t.hit(40, 15));
        assert!(!t.hit(39, 8));
        assert!(!t.hit(41, 8));
        assert!(!ScrollTrack::default().hit(0, 0));
    }

    #[test]
    fn track_maps_rows_to_item_indices() {
        let t = track();
        assert_eq!(t.index_at(5, 100), Some(0));
        assert_eq!(t.index_at(14, 100), Some(99));
        assert_eq!(t.index_at(10, 100), Some(55));
        // Off the ends of the track a drag pins to the first or last item.
        assert_eq!(t.index_at(0, 100), Some(0));
        assert_eq!(t.index_at(200, 100), Some(99));
        assert_eq!(t.index_at(8, 0), None);
        // Fewer items than rows still spans the whole range.
        assert_eq!(t.index_at(5, 3), Some(0));
        assert_eq!(t.index_at(14, 3), Some(2));
    }
}

// ---- card grid ------------------------------------------------------------

/// Selection and scrolling for a grid of equally sized cells. One column is the
/// ordinary list case, so the split and grid layouts share this and there is no
/// second set of navigation rules to keep in step.
///
/// Everything is in item space rather than terminal rows: the caller says how
/// many columns fit and how tall a cell is, and asks back for the rectangle of
/// each visible item. Scrolling moves by whole grid rows, so a resize never
/// leaves half a card at the top.
#[derive(Debug, Clone, Default)]
pub struct CardGrid {
    sel: Option<usize>,
    /// First visible grid row.
    offset: usize,
    cols: usize,
    cell_w: u16,
    cell_h: u16,
    /// Columns left between cells; the last column of a cell is given up to it.
    gap: u16,
    /// Inner area of the last render, without borders.
    rows: Rect,
    len: usize,
    last_click: Option<(std::time::Instant, usize)>,
}

impl CardGrid {
    pub fn selected(&self) -> Option<usize> {
        self.sel
    }
    pub fn select(&mut self, i: Option<usize>) {
        self.sel = i;
    }
    pub fn cols(&self) -> usize {
        self.cols.max(1)
    }
    /// Grid rows the whole list needs.
    pub fn grid_rows(&self) -> usize {
        self.len.div_ceil(self.cols())
    }
    /// Grid rows that fit on screen.
    pub fn visible_rows(&self) -> usize {
        (self.rows.height / self.cell_h.max(1)) as usize
    }
    pub fn page(&self) -> i32 {
        (self.visible_rows().max(1) * self.cols()) as i32
    }
    pub fn clamp(&mut self, len: usize) {
        self.len = len;
        self.sel = if len == 0 {
            None
        } else {
            Some(self.sel.unwrap_or(0).min(len - 1))
        };
    }
    pub fn first(&mut self, len: usize) {
        self.len = len;
        self.sel = (len > 0).then_some(0);
    }
    pub fn last(&mut self, len: usize) {
        self.len = len;
        self.sel = (len > 0).then_some(len - 1);
    }
    /// Move by whole items: along a row, or through the whole list in one column.
    pub fn move_by(&mut self, delta: i32, len: usize) {
        self.len = len;
        if len == 0 {
            self.sel = None;
            return;
        }
        let cur = self.sel.unwrap_or(0) as i32;
        self.sel = Some((cur + delta).clamp(0, len as i32 - 1) as usize);
    }
    /// Move by whole grid rows, keeping the column where it is.
    pub fn move_rows(&mut self, delta: i32, len: usize) {
        self.move_by(delta * self.cols() as i32, len);
    }
    /// Put the selection on a grid row, keeping its column. Used by the
    /// scrollbar, which points at rows rather than at items.
    pub fn select_row(&mut self, row: usize) {
        if self.len == 0 {
            return;
        }
        let col = self.sel.unwrap_or(0) % self.cols();
        let i = row * self.cols() + col;
        self.sel = Some(i.min(self.len - 1));
    }

    /// Record the geometry of a render and scroll so the selection is on screen.
    /// Called every frame, which is what keeps a resize from losing the cursor.
    pub fn layout(&mut self, inner: Rect, cols: usize, cell_h: u16, gap: u16, len: usize) {
        self.rows = inner;
        self.cols = cols.max(1);
        self.cell_h = cell_h.max(1);
        self.gap = gap;
        self.cell_w = (inner.width / self.cols as u16).max(1);
        self.clamp(len);
        let vis = self.visible_rows().max(1);
        if let Some(i) = self.sel {
            let r = i / self.cols();
            if r < self.offset {
                self.offset = r;
            } else if r >= self.offset + vis {
                self.offset = r + 1 - vis;
            }
        }
        self.offset = self.offset.min(self.grid_rows().saturating_sub(vis));
    }

    /// Where item `i` is drawn, or `None` when it is scrolled out of sight. The
    /// last row of a short grid is left-aligned, which falls out of laying every
    /// row out from the left rather than centring a ragged one.
    pub fn cell(&self, i: usize) -> Option<Rect> {
        let cols = self.cols();
        let (r, c) = (i / cols, i % cols);
        if r < self.offset || r >= self.offset + self.visible_rows() {
            return None;
        }
        Some(Rect {
            x: self.rows.x + c as u16 * self.cell_w,
            y: self.rows.y + (r - self.offset) as u16 * self.cell_h,
            width: self.cell_w.saturating_sub(self.gap),
            height: self.cell_h,
        })
    }

    /// The range of items worth drawing this frame.
    pub fn visible(&self) -> std::ops::Range<usize> {
        let cols = self.cols();
        let start = self.offset * cols;
        let end = ((self.offset + self.visible_rows()) * cols).min(self.len);
        start..end.max(start)
    }

    /// Item under the pointer, if the pointer is over one at all.
    pub fn hit(&self, x: u16, y: u16) -> Option<usize> {
        if x < self.rows.x || x >= self.rows.right() || y < self.rows.y || y >= self.rows.bottom() {
            return None;
        }
        let c = ((x - self.rows.x) / self.cell_w.max(1)) as usize;
        if c >= self.cols() {
            return None;
        }
        let r = self.offset + ((y - self.rows.y) / self.cell_h.max(1)) as usize;
        let i = r * self.cols() + c;
        (i < self.len).then_some(i)
    }

    /// Select what was clicked. Returns `(index, is_double_click)`.
    pub fn click(&mut self, x: u16, y: u16) -> Option<(usize, bool)> {
        let i = self.hit(x, y)?;
        let now = std::time::Instant::now();
        let double = matches!(self.last_click, Some((t, j)) if j == i && now.duration_since(t).as_millis() < 400);
        self.last_click = Some((now, i));
        self.sel = Some(i);
        Some((i, double))
    }
}
