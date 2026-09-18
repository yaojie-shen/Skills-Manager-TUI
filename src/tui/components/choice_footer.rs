//! Keyboard focus and footer shared by staged selection dialogs.
use crate::tui::theme::Theme;
use crossterm::event::KeyCode;
use ratatui::{Frame, layout::Rect, style::Modifier, widgets::Paragraph};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ChoiceFocus {
    #[default]
    List,
    Apply,
    Cancel,
}
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ChoiceEvent {
    Moved,
    Apply,
    Cancel,
}
impl ChoiceFocus {
    pub fn key(&mut self, key: KeyCode, at_end: bool) -> Option<ChoiceEvent> {
        use ChoiceFocus::*;
        *self = match (key, *self) {
            (KeyCode::Tab, List) | (KeyCode::BackTab, Cancel) => Apply,
            (KeyCode::Tab, Apply) | (KeyCode::BackTab, List) => Cancel,
            (KeyCode::Tab, Cancel) | (KeyCode::BackTab, Apply) => List,
            (KeyCode::Down, List) if at_end => Apply,
            (KeyCode::Up, Apply | Cancel) => List,
            (KeyCode::Left | KeyCode::Right, Apply) => Cancel,
            (KeyCode::Left | KeyCode::Right, Cancel) => Apply,
            (KeyCode::Enter | KeyCode::Char(' '), Apply) => return Some(ChoiceEvent::Apply),
            (KeyCode::Enter | KeyCode::Char(' '), Cancel) => return Some(ChoiceEvent::Cancel),
            (_, List) => return None,
            _ => return Some(ChoiceEvent::Moved),
        };
        Some(ChoiceEvent::Moved)
    }
}

/// Reserve the footer before sizing the scrollable content. Works with one or two rows.
pub(crate) fn draw(
    f: &mut Frame,
    area: Rect,
    focus: ChoiceFocus,
    enabled: bool,
    summary: &str,
    th: &Theme,
) -> [Rect; 2] {
    draw_with_labels(f, area, focus, enabled, summary, ("✓ Apply", "Cancel"), th)
}

/// Draw a shared footer with caller-provided primary and secondary labels.
pub(crate) fn draw_with_labels(
    f: &mut Frame,
    area: Rect,
    focus: ChoiceFocus,
    enabled: bool,
    summary: &str,
    labels: (&str, &str),
    th: &Theme,
) -> [Rect; 2] {
    if area.height == 0 || area.width == 0 {
        return [Rect::default(); 2];
    }
    if area.height > 1 {
        f.render_widget(
            Paragraph::new("─".repeat(area.width.saturating_sub(2) as usize)).style(th.dim()),
            Rect::new(area.x + 1, area.y, area.width.saturating_sub(2), 1),
        );
    }
    let labels = [format!("[ {} ]", labels.0), format!("[ {} ]", labels.1)];
    let widths = labels
        .each_ref()
        .map(|label| (label.len() as u16).min(area.width / 2));
    let total_width = widths[0].saturating_add(widths[1]);
    let x = area.right().saturating_sub(total_width);
    let y = area.bottom() - 1;
    f.render_widget(
        Paragraph::new(summary).style(th.dim()),
        Rect::new(area.x, y, x - area.x, 1),
    );
    let rects = [
        Rect::new(x, y, widths[0], 1),
        Rect::new(x + widths[0], y, widths[1], 1),
    ];
    for (i, label) in labels.iter().enumerate() {
        let active = focus
            == if i == 0 {
                ChoiceFocus::Apply
            } else {
                ChoiceFocus::Cancel
            };
        let mut style = if active {
            th.selected().add_modifier(Modifier::BOLD)
        } else {
            th.dim()
        };
        if i == 0 && !enabled {
            style = style.fg(th.placeholder);
        }
        f.render_widget(Paragraph::new(label.as_str()).style(style), rects[i]);
    }
    rects
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn footer_clips_to_small_windows_and_highlights_only_focused_button() {
        use ratatui::{Terminal, backend::TestBackend};
        for (w, h) in [(1, 1), (12, 2), (22, 2), (80, 2)] {
            let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
            let mut rects = [Rect::default(); 2];
            terminal
                .draw(|f| {
                    rects = draw(
                        f,
                        f.area(),
                        ChoiceFocus::Apply,
                        false,
                        "No pending changes",
                        &Theme::default(),
                    )
                })
                .unwrap();
            for r in rects {
                assert!(r.right() <= w && r.bottom() <= h);
            }
            if w >= 22 {
                let buf = terminal.backend().buffer();
                assert!(
                    buf[(rects[0].x, rects[0].y)]
                        .modifier
                        .contains(Modifier::BOLD)
                );
                assert!(
                    !buf[(rects[1].x, rects[1].y)]
                        .modifier
                        .contains(Modifier::BOLD)
                );
            }
        }
    }
    #[test]
    fn navigation_never_applies_from_list_and_preserves_cycle() {
        let mut f = ChoiceFocus::List;
        assert_eq!(f.key(KeyCode::Enter, true), None);
        assert_eq!(f.key(KeyCode::Down, false), None);
        assert_eq!(f.key(KeyCode::Down, true), Some(ChoiceEvent::Moved));
        assert_eq!(f, ChoiceFocus::Apply);
        assert_eq!(f.key(KeyCode::Enter, false), Some(ChoiceEvent::Apply));
        f.key(KeyCode::Right, false);
        assert_eq!(f, ChoiceFocus::Cancel);
        assert_eq!(f.key(KeyCode::Char(' '), false), Some(ChoiceEvent::Cancel));
        f.key(KeyCode::Up, false);
        assert_eq!(f, ChoiceFocus::List);
        for expected in [ChoiceFocus::Apply, ChoiceFocus::Cancel, ChoiceFocus::List] {
            f.key(KeyCode::Tab, false);
            assert_eq!(f, expected);
        }
        for expected in [ChoiceFocus::Cancel, ChoiceFocus::Apply, ChoiceFocus::List] {
            f.key(KeyCode::BackTab, false);
            assert_eq!(f, expected);
        }
    }

    #[test]
    fn custom_labels_get_independent_button_widths() {
        use ratatui::{Terminal, backend::TestBackend};
        let mut terminal = Terminal::new(TestBackend::new(80, 2)).unwrap();
        let mut rects = [Rect::default(); 2];
        terminal
            .draw(|f| {
                rects = draw_with_labels(
                    f,
                    f.area(),
                    ChoiceFocus::Apply,
                    true,
                    "Ready",
                    ("Analyze", "Back"),
                    &Theme::default(),
                )
            })
            .unwrap();
        assert_eq!(rects[0].width, "[ Analyze ]".len() as u16);
        assert_eq!(rects[1].width, "[ Back ]".len() as u16);
        assert_eq!(rects[0].right(), rects[1].x);
    }
}
