//! Colors and block styles used by every view.

use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, BorderType, Borders};

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub accent: Color,
    pub dim: Color,
    pub selection_bg: Color,
    pub ok: Color,
    pub warn: Color,
    pub err: Color,
    pub tag: Color,
    pub border: Color,
    pub border_focus: Color,
    /// Search matches, painted like a highlighter pen.
    pub match_bg: Color,
    pub match_fg: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            accent: Color::Cyan,
            dim: Color::DarkGray,
            selection_bg: Color::Rgb(44, 50, 60),
            ok: Color::Green,
            warn: Color::Yellow,
            err: Color::Red,
            tag: Color::Magenta,
            border: Color::DarkGray,
            border_focus: Color::Cyan,
            match_bg: Color::Rgb(255, 214, 79),
            match_fg: Color::Rgb(24, 24, 24),
        }
    }
}

impl Theme {
    pub fn block<'a>(&self, title: impl Into<ratatui::text::Line<'a>>, focused: bool) -> Block<'a> {
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(if focused {
                self.border_focus
            } else {
                self.border
            }))
            .title(title)
    }

    pub fn dim(&self) -> Style {
        Style::default().fg(self.dim)
    }
    pub fn accent(&self) -> Style {
        Style::default().fg(self.accent)
    }
    pub fn bold(&self) -> Style {
        Style::default().add_modifier(Modifier::BOLD)
    }
    pub fn selected(&self) -> Style {
        Style::default()
            .bg(self.selection_bg)
            .add_modifier(Modifier::BOLD)
    }
    pub fn selected_unfocused(&self) -> Style {
        Style::default().bg(self.selection_bg)
    }
    pub fn tag(&self) -> Style {
        Style::default().fg(self.tag)
    }
    pub fn ok(&self) -> Style {
        Style::default().fg(self.ok)
    }
    pub fn warn(&self) -> Style {
        Style::default().fg(self.warn)
    }
    pub fn err(&self) -> Style {
        Style::default().fg(self.err)
    }
    /// Marker-pen styling for a search match. Both colors are set so the run
    /// stays readable over a selected row or dimmed excerpt text.
    pub fn match_hit(&self) -> Style {
        Style::default().bg(self.match_bg).fg(self.match_fg)
    }
    pub fn key_hint(&self) -> Style {
        Style::default()
            .fg(self.accent)
            .add_modifier(Modifier::BOLD)
    }
}
