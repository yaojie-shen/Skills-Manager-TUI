//! Search input, completion and paired result frames. Callers supply data and render results.
use super::completion::Completion;
use crate::tui::{
    app::Ctx,
    theme::Theme,
    widgets::{Input, fit, width},
};
use crossterm::event::{KeyCode, KeyEvent, MouseEvent};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum PanelLayout {
    #[default]
    Separate,
    Unified,
}

pub struct PanelStyle<'a> {
    pub layout: PanelLayout,
    pub input_title: Line<'a>,
    pub results_title: Line<'a>,
    pub hint: (&'a str, &'a str),
    pub input_active: bool,
    pub results_active: bool,
    pub header_height: u16,
}

#[derive(Default, Clone, Copy)]
pub struct PanelAreas {
    pub input: Rect,
    /// Interior of the results frame's top border, when a frame is drawn.
    pub results_header: Rect,
    pub header: Rect,
    pub results: Rect,
    pub footer: Rect,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchEvent {
    Ignored,
    Consumed,
    Changed,
    CursorMoved,
    Accepted,
    Results,
    Up,
    Escape,
}

#[derive(Default)]
pub struct SearchPanel {
    pub input: Input,
    pub completion: Completion,
    pub rect: Rect,
    pub editing: bool,
    areas: PanelAreas,
}

impl SearchPanel {
    /// Completion may be omitted entirely. Providers populate candidates from their own scope.
    pub fn update_completion(&mut self, provider: Option<impl FnOnce(&Input, &mut Completion)>) {
        if let Some(provider) = provider {
            provider(&self.input, &mut self.completion);
        } else {
            self.completion.close();
        }
    }

    pub fn key(&mut self, key: KeyEvent) -> SearchEvent {
        if self.completion.active() {
            match key.code {
                KeyCode::Up | KeyCode::Down => {
                    self.completion
                        .move_by(if key.code == KeyCode::Up { -1 } else { 1 });
                    return SearchEvent::Consumed;
                }
                KeyCode::Enter => {
                    self.completion.accept(&mut self.input);
                    return SearchEvent::Accepted;
                }
                KeyCode::Esc => {
                    self.completion.close();
                    return SearchEvent::Consumed;
                }
                _ => {}
            }
        }
        match key.code {
            KeyCode::Esc if !self.input.is_empty() => {
                self.input.clear();
                SearchEvent::Changed
            }
            KeyCode::Esc => SearchEvent::Escape,
            KeyCode::Enter | KeyCode::Down => SearchEvent::Results,
            KeyCode::Up => SearchEvent::Up,
            _ => {
                let cursor = self.input.cursor_byte();
                if self.input.handle_key(key) {
                    SearchEvent::Changed
                } else if cursor != self.input.cursor_byte() {
                    SearchEvent::CursorMoved
                } else {
                    SearchEvent::Ignored
                }
            }
        }
    }

    pub fn paste(&mut self, text: &str) -> Result<bool, String> {
        self.input.paste(text).map_err(|e| e.to_string())
    }
    pub fn click_input(&mut self, column: u16, row: u16) -> bool {
        if !self.rect.contains((column, row).into()) {
            return false;
        }
        self.editing = true;
        self.input.click(column);
        true
    }
    pub fn mouse_completion(&mut self, event: MouseEvent) -> (bool, bool) {
        self.completion.mouse(event, &mut self.input)
    }
    pub fn draw_completion(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        self.completion.draw(f, area, ctx);
    }

    /// Position belongs to the result frame, leaving every content row available to cards.
    pub fn draw_position(
        &self,
        f: &mut Frame,
        visible: std::ops::Range<usize>,
        total: usize,
        th: &Theme,
    ) {
        let area = self.areas.footer;
        if total == 0 || area.width < 5 || area.height == 0 {
            return;
        }
        let more = if visible.end < total {
            "↓ more · "
        } else {
            ""
        };
        let text = fit(
            &format!(" {more}{}–{} / {total} ", visible.start + 1, visible.end),
            area.width.saturating_sub(2) as usize,
        );
        let w = width(&text) as u16;
        f.render_widget(
            Paragraph::new(Span::styled(text, th.skill_count())),
            Rect::new(area.right() - 1 - w, area.y, w, 1),
        );
    }

    pub fn draw(
        &mut self,
        f: &mut Frame,
        area: Rect,
        style: PanelStyle<'_>,
        th: &Theme,
    ) -> PanelAreas {
        let mut footer = Rect::default();
        let mut results_header = Rect::default();
        let (input, content) = match style.layout {
            PanelLayout::Separate => {
                // In a very short panel preserve a result row instead of spending all rows on borders.
                let height = if area.height >= 6 {
                    3
                } else {
                    u16::from(area.height > 1)
                };
                let top = Rect::new(area.x, area.y, area.width, height);
                let bottom = Rect::new(
                    area.x,
                    area.y + height,
                    area.width,
                    area.height.saturating_sub(height),
                );
                let input = if height >= 3 {
                    let block = th.block(style.input_title, style.input_active);
                    let inner = block.inner(top);
                    f.render_widget(block, top);
                    inner
                } else {
                    top
                };
                let content = if bottom.height >= 3 {
                    results_header =
                        Rect::new(bottom.x + 1, bottom.y, bottom.width.saturating_sub(2), 1);
                    let block = th.block(style.results_title, style.results_active);
                    footer = Rect::new(
                        bottom.x + 1,
                        bottom.bottom() - 1,
                        bottom.width.saturating_sub(2),
                        1,
                    );
                    let inner = block.inner(bottom);
                    f.render_widget(block, bottom);
                    inner
                } else {
                    bottom
                };
                (input, content)
            }
            PanelLayout::Unified => {
                let block = th.block(
                    style.results_title,
                    style.input_active || style.results_active,
                );
                let inner = if area.height >= 3 {
                    footer = Rect::new(
                        area.x + 1,
                        area.bottom() - 1,
                        area.width.saturating_sub(2),
                        1,
                    );
                    let inner = block.inner(area);
                    f.render_widget(block, area);
                    inner
                } else {
                    area
                };
                let ih = u16::from(inner.height > 1);
                let separator = u16::from(inner.height > 2);
                if separator > 0 {
                    f.render_widget(
                        Paragraph::new("─".repeat(inner.width.saturating_sub(2) as usize))
                            .style(Style::default().fg(th.border).add_modifier(Modifier::DIM)),
                        Rect::new(
                            inner.x + inner.width.min(1),
                            inner.y + ih,
                            inner.width.saturating_sub(2),
                            1,
                        ),
                    );
                }
                (
                    Rect::new(inner.x, inner.y, inner.width, ih),
                    Rect::new(
                        inner.x,
                        inner.y + ih + separator,
                        inner.width,
                        inner.height.saturating_sub(ih + separator),
                    ),
                )
            }
        };
        self.rect = input;
        let prefix = if style.layout == PanelLayout::Separate {
            " › "
        } else {
            " "
        };
        let prefix_width = (prefix.chars().count() as u16).min(input.width);
        f.render_widget(
            Paragraph::new(Span::styled(prefix, th.accent())),
            Rect {
                width: prefix_width,
                ..input
            },
        );
        self.input.render_hint(
            f,
            Rect::new(
                input.x + prefix_width,
                input.y,
                input.width.saturating_sub(prefix_width),
                input.height,
            ),
            style.input_active,
            style.hint,
            th,
        );
        let hh = style.header_height.min(content.height.saturating_sub(1));
        self.areas = PanelAreas {
            input,
            results_header,
            footer,
            header: Rect {
                height: hh,
                ..content
            },
            results: Rect::new(
                content.x,
                content.y + hh,
                content.width,
                content.height.saturating_sub(hh),
            ),
        };
        self.areas
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;
    #[test]
    fn escape_and_input_events_do_not_own_page_navigation() {
        let mut panel = SearchPanel::default();
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        assert!(panel.paste("中文 query").unwrap());
        assert_eq!(panel.key(key(KeyCode::Esc)), SearchEvent::Changed);
        assert!(panel.input.is_empty());
        assert_eq!(panel.key(key(KeyCode::Esc)), SearchEvent::Escape);
        assert_eq!(panel.key(key(KeyCode::Enter)), SearchEvent::Results);
        assert_eq!(panel.key(key(KeyCode::Up)), SearchEvent::Up);
        assert_eq!(panel.key(key(KeyCode::Char('x'))), SearchEvent::Changed);
    }
    #[test]
    fn both_frames_keep_input_header_and_results_bounded_and_disjoint() {
        for layout in [PanelLayout::Separate, PanelLayout::Unified] {
            for width in [0, 1, 2, 12, 80] {
                for height in 0..12 {
                    let mut terminal =
                        ratatui::Terminal::new(ratatui::backend::TestBackend::new(90, 20)).unwrap();
                    let area = Rect::new(2, 2, width, height);
                    let mut panel = SearchPanel::default();
                    terminal
                        .draw(|f| {
                            let regions = panel.draw(
                                f,
                                area,
                                PanelStyle {
                                    layout,
                                    input_title: Line::from("Search"),
                                    results_title: Line::from("Results"),
                                    hint: ("search…", ""),
                                    input_active: true,
                                    results_active: false,
                                    header_height: 3,
                                },
                                &Theme::default(),
                            );
                            for rect in [regions.input, regions.header, regions.results] {
                                assert!(rect.x >= area.x && rect.y >= area.y);
                                assert!(
                                    rect.right() <= area.right() && rect.bottom() <= area.bottom()
                                );
                            }
                            assert!(regions.input.bottom() <= regions.header.y);
                            assert!(regions.header.bottom() <= regions.results.y);
                            if height >= 5 && width >= 3 {
                                assert!(regions.results.height > 0);
                            }
                        })
                        .unwrap();
                }
            }
        }
    }
}
