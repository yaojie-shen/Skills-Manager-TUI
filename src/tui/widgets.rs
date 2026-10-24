//! Small reusable pieces: a unicode-aware single-line input, list navigation
//! with mouse hit-testing, text fitting, spinner frames and overlay surfaces.
//! Focus-taking popups start with `PopupSurface`, after drawing their background.
//! Passive notifications and clipping use `OverlayClear` alone.
//! Dialog buttons share cell-aware clipped geometry and explicit semantic intent.
//! Popup sizing uses content budgets within the already footer-excluded area.
//! Mouse classification leaves dismissal and cancellation cleanup to the owner.

use super::theme::Theme;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::buffer::CellWidth;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    ListState, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Widget,
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PopupMouse {
    Inside,
    OutsidePress,
    OutsideWheel,
    Inert,
}

/// Classify against the outer border after routing any child popup first.
pub fn popup_mouse(event: MouseEvent, outer: Rect) -> PopupMouse {
    if outer.contains((event.column, event.row).into()) {
        PopupMouse::Inside
    } else {
        match event.kind {
            MouseEventKind::Down(_) => PopupMouse::OutsidePress,
            MouseEventKind::ScrollUp
            | MouseEventKind::ScrollDown
            | MouseEventKind::ScrollLeft
            | MouseEventKind::ScrollRight => PopupMouse::OutsideWheel,
            _ => PopupMouse::Inert,
        }
    }
}

/// De-emphasize inactive content using the terminal's own foreground/background.
/// Remove colored fills and emphasis so this works on both light and dark themes.
/// Filled badge caps become outline delimiters when their adjoining fill is removed.
/// Applying it again for a nested popup has no cumulative effect.
pub fn dim_inactive(area: Rect, buf: &mut ratatui::buffer::Buffer) {
    let area = area.intersection(*buf.area());
    // Inspect the original colors before resetting any cells: a cap is part of
    // a filled badge only when its ink matches the adjacent body's background.
    // The same glyph in ordinary content must remain untouched.
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let cell = &buf[(x, y)];
            let (outline, body_x) = match cell.symbol() {
                "\u{e0b6}" => ("(", x.checked_add(1)),
                "\u{e0b4}" => (")", x.checked_sub(1)),
                "\u{258c}" => ("[", x.checked_add(1)),
                "\u{2590}" => ("]", x.checked_sub(1)),
                "\u{e0ba}" => ("/", x.checked_add(1)),
                "\u{e0bc}" => ("/", x.checked_sub(1)),
                _ => continue,
            };
            if cell.fg != ratatui::style::Color::Reset
                && body_x.is_some_and(|body_x| {
                    area.contains((body_x, y).into()) && buf[(body_x, y)].bg == cell.fg
                })
            {
                buf[(x, y)].set_symbol(outline);
            }
        }
    }
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let cell = &mut buf[(x, y)];
            let hidden = cell.modifier & Modifier::HIDDEN;
            cell.set_style(Style::reset().add_modifier(Modifier::DIM | hidden));
        }
    }
}

/// Start a focus-taking popup: dim everything drawn beneath it, then clear its
/// surface with wide-character edge repair. Draw popup content after this widget
/// and the app-owned shortcut footer after all popups, keeping both fully legible.
pub struct PopupSurface;

impl Widget for PopupSurface {
    fn render(self, area: Rect, buf: &mut ratatui::buffer::Buffer) {
        let area = area.intersection(*buf.area());
        if area.is_empty() {
            return;
        }
        dim_inactive(*buf.area(), buf);
        OverlayClear.render(area, buf);
    }
}

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
        // This is the one permitted native Clear use: edge repair must happen first.
        #[expect(
            clippy::disallowed_types,
            reason = "OverlayClear repairs wide-glyph edges before delegating to native Clear"
        )]
        use ratatui::widgets::Clear;
        Clear.render(area, buf);
    }
}

/// Copy a clipped temporary buffer without leaving either half of a wide symbol.
pub fn blit_clipped(
    source: &ratatui::buffer::Buffer,
    destination: &mut ratatui::buffer::Buffer,
    clip: Rect,
) {
    let clip = clip
        .intersection(*source.area())
        .intersection(*destination.area());
    if clip.is_empty() {
        return;
    }
    OverlayClear.render(clip, destination);
    for y in clip.top()..clip.bottom() {
        let mut x = source.area().left();
        while x < clip.right() {
            let end = x
                .saturating_add(source[(x, y)].cell_width().max(1))
                .min(source.area().right());
            if x >= clip.left() && end <= clip.right() {
                for col in x..end {
                    destination[(col, y)] = source[(col, y)].clone();
                }
            }
            x = end;
        }
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
    for grapheme in s.graphemes(true) {
        let cw = width(grapheme);
        if w + cw > max - 1 {
            break;
        }
        out.push_str(grapheme);
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

/// Center a dialog within the available screen while retaining a one-cell
/// margin whenever the terminal is large enough.
pub fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let w = width
        .min(area.width.saturating_sub(2))
        .max(u16::from(area.width > 0));
    let h = height
        .min(area.height.saturating_sub(2))
        .max(u16::from(area.height > 0));
    Rect::new(
        area.x + area.width.saturating_sub(w) / 2,
        area.y + area.height.saturating_sub(h) / 2,
        w,
        h,
    )
}

#[derive(Clone, Copy, Debug)]
pub enum PopupClass {
    Input,
    Choice,
    Preview,
}

impl PopupClass {
    pub fn width(self) -> u16 {
        match self {
            Self::Input | Self::Choice => 72,
            Self::Preview => 112,
        }
    }
}

/// Borders are included in desired height; minimum dimensions describe content.
/// Margins yield to the caller's minimum usable budget, independently per axis.
#[derive(Clone, Copy, Debug)]
pub struct PopupSize {
    pub outer: Rect,
    pub inner: Rect,
    pub usable: bool,
}

pub fn popup_size(area: Rect, class: PopupClass, height: u16, minimum: (u16, u16)) -> PopupSize {
    let fit_axis = |available: u16, desired: u16, minimum: u16| {
        let margin = if available >= minimum.saturating_add(4) {
            2
        } else {
            0
        };
        desired
            .max(minimum.saturating_add(2))
            .min(available.saturating_sub(margin))
    };
    let w = fit_axis(area.width, class.width(), minimum.0);
    let h = fit_axis(area.height, height, minimum.1);
    let outer = Rect::new(
        area.x + area.width.saturating_sub(w) / 2,
        area.y + area.height.saturating_sub(h) / 2,
        w,
        h,
    );
    let inner = Rect::new(
        outer.x + u16::from(w > 0),
        outer.y + u16::from(h > 0),
        w.saturating_sub(2),
        h.saturating_sub(2),
    );
    PopupSize {
        outer,
        inner,
        usable: inner.width >= minimum.0 && inner.height >= minimum.1,
    }
}

/// Render on the surface itself when even a bordered content row cannot fit.
/// Escape remains in the app-owned footer; this never takes that space away.
pub fn popup_too_small(f: &mut Frame, size: PopupSize, theme: &Theme) {
    let text = if size.outer.width >= 27 {
        "Too small · resize terminal"
    } else {
        "Resize terminal"
    };
    let area = if size.inner.is_empty() {
        size.outer
    } else {
        size.inner
    };
    f.render_widget(Paragraph::new(text).style(theme.dim()), area);
}

/// Reserve the rightmost column for a scrollbar so content never shares its cells.
pub fn scrollbar_gutter(area: Rect) -> (Rect, Rect) {
    let track_width = u16::from(area.width > 0);
    (
        Rect {
            width: area.width.saturating_sub(track_width),
            ..area
        },
        Rect::new(
            area.right().saturating_sub(track_width),
            area.y,
            track_width,
            area.height,
        ),
    )
}

/// Render a vertical scrollbar with total-content semantics in a dedicated gutter.
pub fn render_vertical_scrollbar(
    frame: &mut Frame,
    track: Rect,
    content_len: usize,
    viewport_len: usize,
    position: usize,
    style: Style,
) {
    if track.is_empty() {
        return;
    }
    OverlayClear.render(track, frame.buffer_mut());
    if content_len <= viewport_len {
        return;
    }
    let mut state = ScrollbarState::new(content_len)
        .position(position)
        .viewport_content_length(viewport_len);
    frame.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .style(style),
        track,
        &mut state,
    );
}

pub const SPINNER: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠇"];

// ---- input ----------------------------------------------------------------

/// Single-line text input with a grapheme cursor and horizontal scrolling.
#[derive(Debug, Default, Clone)]
pub struct Input {
    value: String,
    cursor: usize,
    scroll: usize,
    /// Last rendered inner area, used for click-to-position.
    area: Rect,
}

impl Input {
    pub fn clear_area(&mut self) {
        self.area = Rect::default();
    }

    pub fn with_value(v: &str) -> Self {
        Self {
            value: v.to_string(),
            cursor: v.graphemes(true).count(),
            ..Default::default()
        }
    }
    pub fn value(&self) -> &str {
        &self.value
    }
    pub fn set(&mut self, v: &str) {
        self.value = v.to_string();
        self.cursor = self.value.graphemes(true).count();
    }
    /// Cursor position as a byte boundary, for token-aware completion.
    pub fn cursor_byte(&self) -> usize {
        self.byte_at(self.cursor)
    }

    /// Replace a token while retaining the text and cursor after it.
    pub fn replace_range(&mut self, range: std::ops::Range<usize>, replacement: &str) {
        let cursor_byte = range.start + replacement.len();
        self.value.replace_range(range, replacement);
        self.cursor = self.cursor_after(cursor_byte);
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
        self.value.graphemes(true).count()
    }
    // Use boundaries in the complete edited string: an edit can join graphemes
    // on either side. Snap forward when the byte endpoint lies inside one.
    fn cursor_after(&self, byte: usize) -> usize {
        self.value
            .grapheme_indices(true)
            .take_while(|(i, _)| *i < byte)
            .count()
    }

    fn byte_at(&self, gi: usize) -> usize {
        self.value
            .grapheme_indices(true)
            .nth(gi)
            .map(|(i, _)| i)
            .unwrap_or(self.value.len())
    }

    /// Insert one paste atomically. Single-line fields reject the entire payload
    /// when it contains controls; pasted newlines must never become Enter keys.
    pub fn paste(&mut self, text: &str) -> Result<bool, &'static str> {
        if text
            .chars()
            .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
        {
            return Err("Paste rejected: this field accepts one line without control characters.");
        }
        let at = self.cursor_byte();
        self.value.insert_str(at, text);
        self.cursor = self.cursor_after(at + text.len());
        Ok(!text.is_empty())
    }

    /// Returns true when the value changed.
    pub fn handle_key(&mut self, k: KeyEvent) -> bool {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        match k.code {
            KeyCode::Char(c) if !ctrl && !alt && !c.is_control() => {
                let i = self.byte_at(self.cursor);
                self.value.insert(i, c);
                self.cursor = self.cursor_after(i + c.len_utf8());
                true
            }
            KeyCode::Backspace => {
                if self.cursor > 0 {
                    let end = self.byte_at(self.cursor);
                    self.cursor -= 1;
                    let start = self.byte_at(self.cursor);
                    self.value.drain(start..end);
                    self.cursor = self.cursor_after(start);
                    true
                } else {
                    false
                }
            }
            KeyCode::Delete => {
                if self.cursor < self.len() {
                    let start = self.byte_at(self.cursor);
                    let end = self.byte_at(self.cursor + 1);
                    self.value.drain(start..end);
                    self.cursor = self.cursor_after(start);
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
                self.cursor = self.cursor_after(a);
                true
            }
            _ => false,
        }
    }

    fn prev_word(&self) -> usize {
        let graphemes: Vec<&str> = self.value.graphemes(true).collect();
        let mut i = self.cursor;
        while i > 0 && graphemes[i - 1].chars().all(char::is_whitespace) {
            i -= 1;
        }
        while i > 0 && !graphemes[i - 1].chars().all(char::is_whitespace) {
            i -= 1;
        }
        i
    }
    fn next_word(&self) -> usize {
        let graphemes: Vec<&str> = self.value.graphemes(true).collect();
        let mut i = self.cursor;
        while i < graphemes.len() && !graphemes[i].chars().all(char::is_whitespace) {
            i += 1;
        }
        while i < graphemes.len() && graphemes[i].chars().all(char::is_whitespace) {
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
        for grapheme in self.value.graphemes(true).skip(self.scroll) {
            let cw = width(grapheme);
            if col + cw > target {
                break;
            }
            col += cw;
            idx += 1;
        }
        self.cursor = idx.min(self.len());
    }

    /// Empty search fields distinguish the prompt from usage examples while
    /// retaining the same cursor, scrolling and hit geometry as other inputs.
    pub fn render_hint(
        &mut self,
        f: &mut Frame,
        area: Rect,
        focused: bool,
        hint: (&str, &str),
        theme: &Theme,
    ) {
        self.render(f, area, focused, "", theme);
        if self.is_empty() {
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(hint.0, Style::default().fg(theme.placeholder)),
                    Span::styled(hint.1, Style::default().fg(theme.usage)),
                ])),
                area,
            );
        }
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
        if area.is_empty() {
            return;
        }
        let w = area.width as usize;
        // Keep the cursor visible.
        let cursor_col = |from: usize, to: usize| -> usize {
            self.value
                .graphemes(true)
                .skip(from)
                .take(to.saturating_sub(from))
                .map(width)
                .sum()
        };
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        }
        while cursor_col(self.scroll, self.cursor) >= w.max(1) {
            self.scroll += 1;
        }
        let visible: String = self.value.graphemes(true).skip(self.scroll).collect();
        let line = if self.value.is_empty() && !placeholder.is_empty() {
            Line::from(Span::styled(
                fit(placeholder, w),
                Style::default().fg(theme.placeholder),
            ))
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
    /// A recognized pair is consumed, so a third press starts a new pair.
    pub fn click(&mut self, y: u16, len: usize) -> Option<(usize, bool)> {
        let i = self.row_at(y, len)?;
        let now = std::time::Instant::now();
        let double = matches!(self.last_click, Some((t, j)) if j == i && now.duration_since(t).as_millis() < 400);
        self.last_click = (!double).then_some((now, i));
        self.state.select(Some(i));
        Some((i, double))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ButtonIntent {
    #[default]
    Normal,
    Danger,
}

pub struct ButtonSpec<'a> {
    pub label: &'a str,
    pub enabled: bool,
    pub intent: ButtonIntent,
}

impl<'a> ButtonSpec<'a> {
    pub fn new(label: &'a str) -> Self {
        Self {
            label,
            enabled: true,
            intent: ButtonIntent::Normal,
        }
    }
}

/// Right-align controls on the area's last row. Render each label into exactly
/// its returned hit rectangle, with two cells between present buttons when space
/// permits. Empty labels remain absent; narrow areas shrink the widest controls
/// first. Owners retain focus, enabled checks and input dispatch.
pub fn draw_button_row(
    f: &mut Frame,
    area: Rect,
    buttons: &[ButtonSpec<'_>],
    focused: Option<usize>,
    theme: &Theme,
) -> Vec<Rect> {
    let area = area.intersection(f.area());
    if area.is_empty() {
        return vec![Rect::default(); buttons.len()];
    }
    let labels: Vec<String> = buttons
        .iter()
        .map(|b| {
            if b.label.is_empty() {
                String::new()
            } else {
                format!("[ {} ]", b.label)
            }
        })
        .collect();
    let mut widths: Vec<usize> = labels
        .iter()
        .map(|l| width(l).min(area.width as usize))
        .collect();
    let present = widths.iter().filter(|w| **w > 0).count();
    let gap = if area.width as usize >= present + present.saturating_sub(1) * 2 {
        2
    } else {
        0
    };
    let budget = area.width as usize - gap * present.saturating_sub(1);
    let mut total: usize = widths.iter().sum();
    while total > budget {
        // Shrink later buttons first on ties so a one-cell row still has a hit.
        let i = (0..widths.len()).max_by_key(|i| widths[*i]).unwrap();
        widths[i] -= 1;
        total -= 1;
    }
    let mut x = area.right() - (total + gap * present.saturating_sub(1)) as u16;
    let mut previous = false;
    buttons
        .iter()
        .enumerate()
        .map(|(i, b)| {
            if widths[i] == 0 {
                return Rect::new(x, area.bottom() - 1, 0, 1);
            }
            if previous {
                x += gap as u16;
            }
            previous = true;
            let rect = Rect::new(x, area.bottom() - 1, widths[i] as u16, 1);
            x = rect.right();
            let mut style = if focused == Some(i) {
                theme.selected().add_modifier(Modifier::BOLD)
            } else {
                theme.dim()
            };
            if !b.enabled {
                style = style.fg(theme.placeholder).add_modifier(Modifier::DIM);
            } else if b.intent == ButtonIntent::Danger {
                style = style.fg(theme.err);
            }
            f.render_widget(
                Paragraph::new(fit(&labels[i], widths[i])).style(style),
                rect,
            );
            rect
        })
        .collect()
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
    fn mouse_double_click_consumes_pairs_and_popup_classifies_all_buttons() {
        use super::*;
        use crossterm::event::MouseButton;
        let outer = Rect::new(10, 5, 30, 10);
        let mut nav = ListNav {
            rows: outer,
            ..Default::default()
        };
        assert_eq!(nav.click(5, 3), Some((0, false)));
        assert_eq!(nav.click(5, 3), Some((0, true)));
        assert_eq!(nav.click(5, 3), Some((0, false)));
        for button in [MouseButton::Left, MouseButton::Right, MouseButton::Middle] {
            let event = MouseEvent {
                kind: MouseEventKind::Down(button),
                column: 0,
                row: 0,
                modifiers: KeyModifiers::NONE,
            };
            assert_eq!(popup_mouse(event, outer), PopupMouse::OutsidePress);
            assert_eq!(
                popup_mouse(
                    MouseEvent {
                        column: 10,
                        row: 5,
                        ..event
                    },
                    outer
                ),
                PopupMouse::Inside
            );
            assert_eq!(
                popup_mouse(
                    MouseEvent {
                        kind: MouseEventKind::ScrollDown,
                        ..event
                    },
                    outer
                ),
                PopupMouse::OutsideWheel
            );
            assert_eq!(
                popup_mouse(
                    MouseEvent {
                        kind: MouseEventKind::Up(button),
                        ..event
                    },
                    outer
                ),
                PopupMouse::Inert
            );
        }
    }

    #[test]
    fn popup_sizing_caps_content_and_yields_margins_without_leaving_bounds() {
        use super::*;
        for (w, h) in [
            (40, 10),
            (60, 18),
            (112, 30),
            (160, 38),
            (0, 0),
            (1, 1),
            (4, 2),
        ] {
            let available = Rect::new(3, 5, w, h);
            for class in [PopupClass::Input, PopupClass::Choice, PopupClass::Preview] {
                for desired in [3, 7, 32, u16::MAX] {
                    let size = popup_size(available, class, desired, (4, 3));
                    assert_eq!(size.outer.intersection(available), size.outer);
                    assert!(size.outer.width <= class.width());
                    assert_eq!(size.usable, size.inner.width >= 4 && size.inner.height >= 3);
                }
            }
        }
        let size = popup_size(Rect::new(0, 0, 40, 7), PopupClass::Choice, 12, (4, 5));
        assert_eq!(size.outer.height, 7);
        assert!(size.usable);
        assert_eq!(
            popup_size(Rect::new(0, 0, 160, 40), PopupClass::Choice, 6, (4, 3))
                .outer
                .height,
            6
        );
        assert_eq!(centered(Rect::default(), 72, 3), Rect::default());
    }

    #[test]
    fn button_rows_share_clipped_cell_geometry_and_semantic_styles() {
        use super::*;
        use ratatui::{Terminal, backend::TestBackend};
        let th = Theme::default();
        for w in [1, 2, 3, 7, 12, 24, 80] {
            for focused in [None, Some(0), Some(1), Some(2)] {
                let mut terminal = Terminal::new(TestBackend::new(w + 4, 3)).unwrap();
                let area = Rect::new(2, 1, w, 1);
                let specs = [
                    ButtonSpec {
                        label: "删除 👩‍💻",
                        enabled: true,
                        intent: ButtonIntent::Danger,
                    },
                    ButtonSpec {
                        label: "Apply",
                        enabled: false,
                        intent: ButtonIntent::Normal,
                    },
                    ButtonSpec::new("Cancel"),
                    ButtonSpec::new(""),
                ];
                let mut rects = vec![];
                terminal
                    .draw(|f| rects = draw_button_row(f, area, &specs, focused, &th))
                    .unwrap();
                assert_eq!(rects[3].width, 0, "absent buttons have no hit target");
                for (i, r) in rects.iter().enumerate() {
                    assert!(r.x >= area.x && r.right() <= area.right());
                    assert_eq!(r.y, area.y);
                    for x in r.x..r.right() {
                        assert!(r.contains((x, r.y).into()));
                        for (j, other) in rects.iter().enumerate() {
                            if i != j {
                                assert!(!other.contains((x, r.y).into()));
                            }
                        }
                        let cell = &terminal.backend().buffer()[(x, r.y)];
                        if (r.x..x).any(|start| {
                            start + terminal.backend().buffer()[(start, r.y)].cell_width() > x
                        }) {
                            continue;
                        }
                        assert_eq!(cell.modifier.contains(Modifier::BOLD), focused == Some(i));
                        if focused == Some(i) {
                            assert_eq!(cell.bg, th.selection_bg);
                        }
                        if i == 0 {
                            assert_eq!(cell.fg, th.err);
                        } else if i == 1 {
                            assert_eq!(cell.fg, th.placeholder);
                            assert!(cell.modifier.contains(Modifier::DIM));
                        }
                    }
                    if r.width > 0 {
                        assert_eq!(
                            terminal.backend().buffer()[(r.x, r.y)].symbol(),
                            if r.width == 1 { "…" } else { "[" }
                        );
                    }
                }
                if w == 80 {
                    assert_eq!(rects[0].width as usize, width("[ 删除 👩‍💻 ]"));
                    assert_eq!(rects[1].x - rects[0].right(), 2);
                    assert_eq!(rects[2].x - rects[1].right(), 2);
                }
            }
        }
        let mut terminal = Terminal::new(TestBackend::new(10, 2)).unwrap();
        terminal
            .draw(|f| {
                for area in [Rect::new(0, 0, 0, 1), Rect::new(0, 0, 10, 0)] {
                    assert!(
                        draw_button_row(f, area, &[ButtonSpec::new("Done")], Some(0), &th)[0]
                            .is_empty()
                    );
                }
                assert!(draw_button_row(f, f.area(), &[], None, &th).is_empty());
            })
            .unwrap();
    }
    #[test]
    fn typed_scalars_keep_a_grapheme_cursor() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        for text in ["e\u{301}", "👩‍💻", "❤️", "🇨🇳"] {
            for suffix in ["", "tail"] {
                let mut input = super::Input::with_value(suffix);
                input.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
                for c in text.chars() {
                    input.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
                    assert!(input.cursor <= input.len());
                }
                assert_eq!(input.cursor, 1);
                input.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::ALT));
                assert_eq!(input.cursor, 0);
                input.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
                input.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
                assert_eq!(input.value(), suffix);
                for c in text.chars() {
                    input.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
                }
                input.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
                assert_eq!(input.value(), suffix);
            }
        }
    }

    #[test]
    fn fit_keeps_extended_graphemes_intact() {
        use unicode_segmentation::UnicodeSegmentation;

        for text in ["e\u{301}clair", "👩‍💻tools", "❤️health", "🇨🇳中文"] {
            for columns in 0..=super::width(text) {
                let fitted = super::fit(text, columns);
                assert!(super::width(&fitted) <= columns);
                let source = text.graphemes(true).collect::<Vec<_>>();
                let body = fitted.strip_suffix('…').unwrap_or(&fitted);
                assert!(
                    source
                        .iter()
                        .scan(String::new(), |prefix, grapheme| {
                            prefix.push_str(grapheme);
                            Some(prefix.clone())
                        })
                        .any(|prefix| prefix == body)
                        || body.is_empty()
                        || fitted == text,
                    "text={text:?}, columns={columns}, fitted={fitted:?}"
                );
            }
        }
    }

    #[test]
    fn scrollbar_gutter_isolates_wide_content_from_vertical_track() {
        use ratatui::widgets::{Block, Borders};

        let theme = super::Theme::default();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(16, 8)).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                let block = Block::default().borders(Borders::ALL);
                let inner = block.inner(area);
                let (content, track) = super::scrollbar_gutter(inner);
                frame.render_widget(block, area);
                frame.render_widget(
                    ratatui::widgets::Paragraph::new("中文❤️中文❤️".repeat(6))
                        .wrap(ratatui::widgets::Wrap { trim: false }),
                    content,
                );
                super::render_vertical_scrollbar(frame, track, 24, 6, 0, theme.dim());
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        for y in 1..7 {
            assert_eq!(buffer[(15, y)].symbol(), "│");
            assert!(matches!(buffer[(14, y)].symbol(), "║" | "█" | " "));
        }
    }

    #[test]
    fn clipped_blit_drops_partial_wide_symbols_at_both_edges() {
        use ratatui::buffer::Buffer;

        let area = Rect::new(0, 0, 8, 1);
        let mut source = Buffer::empty(area);
        source.set_string(0, 0, "a中文b", Style::default());
        let mut destination = Buffer::empty(area);
        destination.set_string(0, 0, "🙂🙂🙂🙂", Style::default());

        super::blit_clipped(&source, &mut destination, Rect::new(2, 0, 3, 1));
        assert_eq!(destination[(1, 0)].symbol(), " ");
        assert_eq!(destination[(2, 0)].symbol(), " ");
        assert_eq!(destination[(3, 0)].symbol(), "文");
        assert_eq!(destination[(4, 0)].symbol(), " ");
        assert_eq!(destination[(5, 0)].symbol(), " ");
    }

    #[test]
    fn variable_cards_keep_selection_visible_and_hit_the_right_item() {
        use super::*;
        let mut grid = CardGrid::default();
        for height in 1..15 {
            for selected in 0..4 {
                grid.select(Some(selected));
                grid.layout_heights(Rect::new(2, 3, 30, height), vec![3, 4, 3, 4]);
                let cell = grid.cell(selected).unwrap();
                assert_eq!(cell.height, [3, 4, 3, 4][selected].min(height));
                assert!(cell.bottom() <= 3 + height);
                assert_eq!(grid.hit(cell.x, cell.y), Some(selected));
                assert!(grid.visible().contains(&selected));
            }
        }
        grid.select(Some(0));
        grid.layout_heights(Rect::new(0, 0, 30, 14), vec![3, 4, 3, 4]);
        assert_eq!(grid.cell(1).unwrap().y, 3);
        assert_eq!(grid.cell(2).unwrap().y, 7);
    }
    #[test]
    fn filter_hint_colors_do_not_leak_into_typed_text() {
        let theme = super::Theme::default();
        let mut input = super::Input::default();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(24, 1)).unwrap();
        terminal
            .draw(|f| input.render_hint(f, f.area(), true, ("filter", " · Enter results"), &theme))
            .unwrap();
        assert_eq!(terminal.backend().buffer()[(0, 0)].fg, theme.placeholder);
        assert_eq!(terminal.backend().buffer()[(9, 0)].fg, theme.usage);
        input = super::Input::with_value("lark");
        terminal
            .draw(|f| input.render_hint(f, f.area(), true, ("filter", " · Enter results"), &theme))
            .unwrap();
        assert_eq!(
            terminal.backend().buffer()[(0, 0)].fg,
            ratatui::style::Color::Reset
        );
        assert_eq!(terminal.backend().buffer()[(9, 0)].symbol(), " ");
    }
    #[test]
    fn input_edits_and_moves_by_extended_grapheme() {
        let key =
            |code| crossterm::event::KeyEvent::new(code, crossterm::event::KeyModifiers::NONE);
        for grapheme in ["e\u{301}", "👩‍💻", "❤️", "🇨🇳"] {
            let mut input = super::Input::with_value(&format!("a{grapheme}z"));
            input.handle_key(key(crossterm::event::KeyCode::Left));
            input.handle_key(key(crossterm::event::KeyCode::Backspace));
            assert_eq!(input.value(), "az", "backspace split {grapheme:?}");

            let mut input = super::Input::with_value(&format!("a{grapheme}z"));
            input.handle_key(key(crossterm::event::KeyCode::Home));
            input.handle_key(key(crossterm::event::KeyCode::Right));
            input.handle_key(key(crossterm::event::KeyCode::Delete));
            assert_eq!(input.value(), "az", "delete split {grapheme:?}");
            assert_eq!(input.cursor_byte(), 1);
        }
    }

    #[test]
    fn paste_is_atomic_and_respects_unicode_cursor() {
        let mut input = super::Input::with_value("a尾");
        input.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Left,
            crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(input.paste("中文🙂"), Ok(true));
        assert_eq!(input.value(), "a中文🙂尾");
        let cursor = input.cursor_byte();
        for rejected in [
            "first\nsecond",
            "x\ry",
            "x\ty",
            "\u{1b}[31m",
            "x\u{7f}",
            "x\u{2028}y",
            "x\u{2029}y",
        ] {
            assert!(input.paste(rejected).is_err());
            assert_eq!(input.value(), "a中文🙂尾");
            assert_eq!(input.cursor_byte(), cursor);
        }
        assert_eq!(input.paste(""), Ok(false));
        input.paste("!").unwrap();
        assert_eq!(input.value(), "a中文🙂!尾");
    }

    #[test]
    fn popup_surface_dims_nested_background_without_changing_symbols() {
        use ratatui::{buffer::Buffer, style::Color};

        for (fg, bg) in [(Color::White, Color::Black), (Color::Black, Color::White)] {
            let mut background = Buffer::empty(Rect::new(4, 2, 30, 10));
            background.set_style(*background.area(), Style::default().fg(fg).bg(bg));
            background.set_string(4, 2, "Library", Theme::default().selected());
            background.set_string(4, 3, "colored tag", Theme::default().match_hit());
            let mut buffer = background.clone();
            let outer = Rect::new(8, 4, 20, 6);
            PopupSurface.render(outer, &mut buffer);
            for position in [(4, 2), (4, 3)] {
                assert_eq!(buffer[position].symbol(), background[position].symbol());
                assert_eq!(buffer[position].fg, Color::Reset);
                assert_eq!(buffer[position].bg, Color::Reset);
                assert_eq!(buffer[position].modifier, Modifier::DIM);
            }
            assert_eq!(buffer[(8, 4)].fg, Color::Reset);
            assert_eq!(buffer[(8, 4)].bg, Color::Reset);
            assert!(buffer[(8, 4)].modifier.is_empty());
            buffer.set_string(8, 4, "Parent", Theme::default().accent());
            let inactive = buffer[(4, 2)].clone();
            PopupSurface.render(Rect::new(10, 6, 10, 2), &mut buffer);
            assert_eq!(buffer[(4, 2)], inactive, "nested dimming is idempotent");
            assert_eq!(buffer[(8, 4)].symbol(), "P");
            assert_eq!(buffer[(8, 4)].modifier, Modifier::DIM);
            buffer.set_string(10, 6, "Child", Theme::default().accent());
            assert_eq!(buffer[(10, 6)].fg, Theme::default().accent);
            assert!(!buffer[(10, 6)].modifier.contains(Modifier::DIM));

            let mut plain_clear = background.clone();
            OverlayClear.render(outer, &mut plain_clear);
            assert_eq!(plain_clear[(4, 2)], background[(4, 2)]);
            PopupSurface.render(Rect::default(), &mut plain_clear);
            assert_eq!(plain_clear[(4, 2)], background[(4, 2)]);
        }
    }

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
    heights: Vec<u16>,
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
        if !self.heights.is_empty() {
            let mut used = 0;
            return self
                .heights
                .iter()
                .skip(self.offset)
                .take_while(|&&h| {
                    used += h;
                    used <= self.rows.height
                })
                .count();
        }
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
        self.heights.clear();
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

    /// A one-column list whose cards grow by one row when they have a description.
    pub fn layout_heights(&mut self, inner: Rect, heights: Vec<u16>) {
        self.rows = inner;
        self.cols = 1;
        self.cell_w = inner.width;
        self.gap = 0;
        self.heights = heights
            .into_iter()
            .map(|h| h.min(inner.height).max(1))
            .collect();
        self.clamp(self.heights.len());
        self.offset = self.offset.min(self.sel.unwrap_or(0));
        if let Some(i) = self.sel {
            while self.offset < i
                && self.heights[self.offset..=i]
                    .iter()
                    .map(|&h| h as usize)
                    .sum::<usize>()
                    > inner.height as usize
            {
                self.offset += 1;
            }
        }
    }

    /// Where item `i` is drawn, clipped at the viewport's bottom, or `None`
    /// when it is scrolled out of sight. `visible()` includes full rows only;
    /// callers can request the following row to show a continuation. The
    /// last row of a short grid is left-aligned, which falls out of laying every
    /// row out from the left rather than centring a ragged one.
    pub fn cell(&self, i: usize) -> Option<Rect> {
        if !self.heights.is_empty() {
            if i < self.offset || i >= self.len {
                return None;
            }
            let y: usize = self.heights[self.offset..i]
                .iter()
                .map(|&h| h as usize)
                .sum();
            if y >= self.rows.height as usize {
                return None;
            }
            return Some(Rect::new(
                self.rows.x,
                self.rows.y + y as u16,
                self.rows.width,
                self.heights[i].min(self.rows.height - y as u16),
            ));
        }
        let cols = self.cols();
        let (r, c) = (i / cols, i % cols);
        let drawn_rows = self.rows.height.div_ceil(self.cell_h.max(1)) as usize;
        if i >= self.len || r < self.offset || r >= self.offset + drawn_rows {
            return None;
        }
        Some(Rect {
            x: self.rows.x + c as u16 * self.cell_w,
            y: self.rows.y + (r - self.offset) as u16 * self.cell_h,
            width: self.cell_w.saturating_sub(self.gap),
            height: self
                .cell_h
                .min(self.rows.height - (r - self.offset) as u16 * self.cell_h),
        })
    }

    /// The range of items worth drawing this frame.
    pub fn visible(&self) -> std::ops::Range<usize> {
        let cols = self.cols();
        let start = self.offset * cols;
        let end = ((self.offset + self.visible_rows()) * cols).min(self.len);
        start..end.max(start)
    }

    /// Include the clipped next row so a card grid visibly continues below the frame.
    pub fn visible_with_partial(&self) -> std::ops::Range<usize> {
        let full = self.visible();
        let end = if self.cell(full.end).is_some() {
            (full.end + self.cols()).min(self.len)
        } else {
            full.end
        };
        full.start..end
    }

    /// Item under the pointer, if the pointer is over one at all.
    pub fn hit(&self, x: u16, y: u16) -> Option<usize> {
        if !self.heights.is_empty() {
            return (self.offset..self.len)
                .find(|&i| self.cell(i).is_some_and(|r| r.contains((x, y).into())));
        }
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
    /// A recognized pair is consumed, so a third press starts a new pair.
    pub fn click(&mut self, x: u16, y: u16) -> Option<(usize, bool)> {
        let i = self.hit(x, y)?;
        let now = std::time::Instant::now();
        let double = matches!(self.last_click, Some((t, j)) if j == i && now.duration_since(t).as_millis() < 400);
        self.last_click = (!double).then_some((now, i));
        self.sel = Some(i);
        Some((i, double))
    }
}
