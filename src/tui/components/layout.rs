//! Shared geometry and frames; pages supply their content and focus.
use crate::tui::{app::Ctx, theme::Theme};
use ratatui::{
    Frame,
    layout::{Margin, Rect},
    style::{Modifier, Style},
    text::Span,
    widgets::{Block, BorderType, Borders, Paragraph},
};

/// Columns that fit in `width`, always at least one.
pub fn cols_for(width: u16, ctx: &Ctx) -> usize {
    ((width / ctx.settings.layout.min_card_width.max(1)) as usize).max(1)
}

/// Draw the frame of one card and hand back the padded area inside it.
/// `on` marks selection; `focused` gives it the theme's accent border.
/// An inactive selection retains a neutral bold border.
pub fn frame(f: &mut Frame, cell: Rect, on: bool, focused: bool, th: &Theme) -> Rect {
    draw_frame(f, cell, Borders::ALL, frame_style(on, focused, th))
}

/// A clipped skill card keeps an open lower edge to show that it continues.
pub fn skill_frame(f: &mut Frame, cell: Rect, on: bool, focused: bool, ctx: &Ctx) -> Rect {
    let borders = if cell.height < ctx.settings.layout.card_height {
        Borders::TOP | Borders::LEFT | Borders::RIGHT
    } else {
        Borders::ALL
    };
    draw_frame(
        f,
        cell,
        borders,
        frame_style(on, focused, &ctx.settings.theme),
    )
}

/// Draw a subdued divider inside a list item's reserved spacer row.
/// The inset keeps the rule clear of the item marker and the scrollbar.
pub fn skill_list_separator(f: &mut Frame, cell: Rect, th: &Theme) {
    if cell.height < 5 || cell.width <= 4 {
        return;
    }
    let separator = Rect::new(cell.x + 2, cell.y + 4, cell.width - 4, 1);
    f.render_widget(
        Paragraph::new(Span::styled(
            "─".repeat(separator.width as usize),
            th.dim().add_modifier(Modifier::DIM),
        )),
        separator,
    );
}

fn frame_style(on: bool, focused: bool, th: &Theme) -> Style {
    if on && focused {
        th.accent().add_modifier(Modifier::BOLD)
    } else if on {
        th.bold()
    } else {
        th.dim().add_modifier(Modifier::DIM)
    }
}

fn draw_frame(f: &mut Frame, cell: Rect, borders: Borders, border: Style) -> Rect {
    let b = Block::default()
        .borders(borders)
        .border_type(BorderType::Rounded)
        .border_style(border);
    let inner = b.inner(cell).inner(Margin {
        horizontal: 1,
        vertical: 0,
    });
    f.render_widget(b, cell);
    inner
}

/// Keep navigation lists compact and give remaining space to details.
/// Narrow terminals stack the list above the content.
pub fn split_panes(area: Rect, left_width: u16, ctx: &Ctx) -> (Rect, Rect) {
    use ratatui::layout::{Constraint, Direction, Layout};
    if area.width < ctx.settings.layout.pane_breakpoint {
        let r = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(
                    (area.height * ctx.settings.layout.stacked_pane_percent / 100)
                        .min(ctx.settings.layout.stacked_pane_max_height),
                ),
                Constraint::Min(0),
            ])
            .split(area);
        (r[0], r[1])
    } else {
        let r = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(left_width.min(area.width / 2)),
                Constraint::Min(0),
            ])
            .split(area);
        (r[0], r[1])
    }
}
