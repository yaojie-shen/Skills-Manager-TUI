//! Application-owned fix for cursor placement after wide terminal cells.
//!
//! Ratatui 0.30.2 assumes every emitted cell advances the cursor one column.
//! Splitting before a write into a wide cell's continuation column makes the
//! underlying Crossterm backend emit an explicit cursor move. This mirrors
//! upstream ratatui/ratatui#2721 and remains effective in packaged installs.

use std::io::{self, Write};

use ratatui::backend::{Backend, ClearType, CrosstermBackend, WindowSize};
use ratatui::buffer::{Cell, CellWidth};
use ratatui::layout::{Position, Size};

#[derive(Debug, Default, Clone, Eq, PartialEq, Hash)]
pub struct WideCellBackend<W: Write> {
    inner: CrosstermBackend<W>,
}

impl<W: Write> WideCellBackend<W> {
    pub const fn new(writer: W) -> Self {
        Self {
            inner: CrosstermBackend::new(writer),
        }
    }
}

impl<W: Write> Write for WideCellBackend<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Write::flush(&mut self.inner)
    }
}

impl<W: Write> Backend for WideCellBackend<W> {
    type Error = io::Error;

    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        let (lower, _) = content.size_hint();
        let mut batch = Vec::with_capacity(lower);
        let mut previous: Option<(u16, u16, u16)> = None;
        for (x, y, cell) in content {
            let inside_previous = previous
                .is_some_and(|(px, py, width)| y == py && x == px.saturating_add(1) && width != 1);
            if inside_previous {
                self.inner.draw(batch.drain(..))?;
            }
            batch.push((x, y, cell));
            previous = Some((x, y, cell.cell_width()));
        }
        if !batch.is_empty() {
            self.inner.draw(batch.into_iter())?;
        }
        Ok(())
    }

    fn append_lines(&mut self, n: u16) -> io::Result<()> {
        self.inner.append_lines(n)
    }
    fn hide_cursor(&mut self) -> io::Result<()> {
        self.inner.hide_cursor()
    }
    fn show_cursor(&mut self) -> io::Result<()> {
        self.inner.show_cursor()
    }
    fn get_cursor_position(&mut self) -> io::Result<Position> {
        self.inner.get_cursor_position()
    }
    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        self.inner.set_cursor_position(position)
    }
    fn clear(&mut self) -> io::Result<()> {
        self.inner.clear()
    }
    fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
        self.inner.clear_region(clear_type)
    }
    fn size(&self) -> io::Result<Size> {
        self.inner.size()
    }
    fn window_size(&mut self) -> io::Result<WindowSize> {
        self.inner.window_size()
    }
    fn flush(&mut self) -> io::Result<()> {
        Backend::flush(&mut self.inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draw_repositions_before_a_cell_inside_a_wide_symbol() {
        let wide = Cell::new("中");
        let border = Cell::new("│");
        let mut output = Vec::new();
        WideCellBackend::new(&mut output)
            .draw([(0, 0, &wide), (1, 0, &border)].into_iter())
            .unwrap();
        assert!(
            String::from_utf8_lossy(&output).contains(&crossterm::cursor::MoveTo(1, 0).to_string())
        );
    }

    #[test]
    fn draw_keeps_normal_and_truly_contiguous_cells_batched() {
        let wide = Cell::new("中");
        let narrow = Cell::new("a");
        let next = Cell::new("b");
        let mut output = Vec::new();
        WideCellBackend::new(&mut output)
            .draw([(0, 0, &wide), (2, 0, &narrow), (3, 0, &next)].into_iter())
            .unwrap();
        let output = String::from_utf8_lossy(&output);
        assert!(output.contains(&crossterm::cursor::MoveTo(2, 0).to_string()));
        assert!(!output.contains(&crossterm::cursor::MoveTo(3, 0).to_string()));
    }
}
