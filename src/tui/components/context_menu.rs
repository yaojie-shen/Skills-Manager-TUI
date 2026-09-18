//! Context menu presentation and input. Commands are dispatched by the owning view.
use crate::tui::theme::Theme;
use crate::tui::widgets::{OverlayClear as Clear, fit, width};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Frame, layout::Rect, style::Style, text::Line, widgets::Paragraph};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Open,
    Tags,
    Presets,
    Deploy,
    Note,
    Rename,
    Source,
    Check,
    Update,
    Accept,
    Remove,
    Relink,
    Adopt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Skill(String),
    Batch {
        all: Vec<String>,
        visible: Vec<String>,
    },
    Entry {
        key: String,
        scope: String,
        path: std::path::PathBuf,
        state: String,
    },
}

#[derive(Debug, Clone)]
pub struct Item {
    pub command: Command,
    pub label: String,
    pub shortcut: KeyCode,
    pub disabled: Option<String>,
    pub group: u8,
    pub danger: bool,
}
impl Item {
    pub fn new(
        command: Command,
        label: impl Into<String>,
        shortcut: KeyCode,
        enabled: bool,
        reason: &str,
        group: u8,
    ) -> Self {
        Self {
            command,
            label: label.into(),
            shortcut,
            disabled: (!enabled).then(|| reason.into()),
            group,
            danger: command == Command::Remove,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Request {
    pub title: String,
    pub detail: String,
    pub target: Target,
    pub items: Vec<Item>,
}
impl Request {
    pub fn allows(&self, command: Command) -> bool {
        self.items
            .iter()
            .any(|i| i.command == command && i.disabled.is_none())
    }
}

pub enum MenuEvent {
    Stay,
    Close,
    Execute(Command),
}
pub struct ContextMenu {
    pub request: Request,
    anchor: (u16, u16),
    selected: usize,
    offset: usize,
    area: Rect,
    hits: Vec<(Rect, usize)>,
}
impl ContextMenu {
    pub fn new(request: Request, x: u16, y: u16) -> Self {
        Self {
            request,
            anchor: (x, y),
            selected: 0,
            offset: 0,
            area: Rect::default(),
            hits: vec![],
        }
    }
    fn activate(&self) -> MenuEvent {
        self.request
            .items
            .get(self.selected)
            .filter(|i| i.disabled.is_none())
            .map_or(MenuEvent::Stay, |i| MenuEvent::Execute(i.command))
    }
    fn move_by(&mut self, delta: i32) {
        self.selected = (self.selected as i32 + delta)
            .clamp(0, self.request.items.len().saturating_sub(1) as i32)
            as usize;
    }
    pub fn key(&mut self, key: KeyEvent) -> MenuEvent {
        match key.code {
            KeyCode::Esc => return MenuEvent::Close,
            KeyCode::Up => self.move_by(-1),
            KeyCode::Down => self.move_by(1),
            KeyCode::Enter => return self.activate(),
            _ => {
                if (key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT)
                    && let Some(index) = self.request.items.iter().position(|i| {
                        i.shortcut == key.code && matches!(key.code, KeyCode::Char(_))
                    })
                {
                    self.selected = index;
                    return self.activate();
                }
            }
        }
        MenuEvent::Stay
    }
    pub fn mouse(&mut self, event: MouseEvent) -> MenuEvent {
        let point = (event.column, event.row).into();
        match event.kind {
            MouseEventKind::Down(_) if !self.area.contains(point) => return MenuEvent::Close,
            MouseEventKind::Moved | MouseEventKind::Down(MouseButton::Left) => {
                if let Some((_, index)) = self.hits.iter().find(|(r, _)| r.contains(point)) {
                    self.selected = *index;
                    if event.kind == MouseEventKind::Down(MouseButton::Left) {
                        return self.activate();
                    }
                }
            }
            MouseEventKind::ScrollUp => self.move_by(-1),
            MouseEventKind::ScrollDown => self.move_by(1),
            _ => {}
        }
        MenuEvent::Stay
    }
    pub fn draw(&mut self, f: &mut Frame, bounds: Rect, th: &Theme) {
        const MOUSE_FOOTER: &str = "Left click to run · right click outside to close";
        self.hits.clear();
        let has_footer = self
            .request
            .items
            .iter()
            .any(|item| item.disabled.is_some());
        let footer_width = if has_footer {
            self.request
                .items
                .iter()
                .filter_map(|item| item.disabled.as_deref())
                .map(width)
                .chain([width(MOUSE_FOOTER)])
                .max()
                .unwrap_or(0)
                + 2
        } else {
            0
        };
        let wanted = self
            .request
            .items
            .iter()
            .map(|i| {
                (width(&i.label) + 4)
                    .max(i.disabled.as_deref().map_or(0, |reason| width(reason) + 4))
            })
            .chain([
                width(&self.request.title) + 4,
                width(&self.request.detail) + 4,
                footer_width,
            ])
            .max()
            .unwrap_or(36)
            .max(36)
            .clamp(24, 72) as u16;
        let w = wanted.min(bounds.width);
        let mut lines = Vec::new();
        for (index, item) in self.request.items.iter().enumerate() {
            if index > 0 && self.request.items[index - 1].group != item.group {
                lines.push(None);
            }
            lines.push(Some(index));
        }
        let has_detail = !self.request.detail.is_empty();
        let h = (lines.len() as u16 + 2 + u16::from(has_detail) + 2 * u16::from(has_footer))
            .min(bounds.height);

        self.area = Rect::new(
            self.anchor
                .0
                .min(bounds.right().saturating_sub(w))
                .max(bounds.x),
            self.anchor
                .1
                .min(bounds.bottom().saturating_sub(h))
                .max(bounds.y),
            w,
            h,
        );
        f.render_widget(Clear, self.area);
        let block = th.block(fit(&self.request.title, w.saturating_sub(4) as usize), true);
        let inner = block.inner(self.area);
        f.render_widget(block, self.area);
        if inner.width == 0 || inner.height == 0 {
            return;
        }
        let show_detail = has_detail && inner.height >= 3;
        let footer = has_footer && inner.height >= 2;
        let footer_gap = footer && inner.height > u16::from(show_detail) + 2;
        let rows = inner
            .height
            .saturating_sub(u16::from(show_detail) + u16::from(footer) + u16::from(footer_gap))
            as usize;
        if show_detail {
            f.render_widget(
                Paragraph::new(fit(&self.request.detail, inner.width as usize)).style(th.dim()),
                Rect::new(inner.x, inner.y, inner.width, 1),
            );
        }
        let selected_line = lines
            .iter()
            .position(|i| *i == Some(self.selected))
            .unwrap_or(0);
        if selected_line < self.offset {
            self.offset = selected_line;
        }
        if rows > 0 && selected_line >= self.offset + rows {
            self.offset = selected_line + 1 - rows;
        }
        for (line, index) in lines.iter().enumerate().skip(self.offset).take(rows) {
            let rect = Rect::new(
                inner.x,
                inner.y + u16::from(show_detail) + (line - self.offset) as u16,
                inner.width,
                1,
            );
            let Some(index) = *index else {
                f.render_widget(
                    Paragraph::new("─".repeat(inner.width.saturating_sub(2) as usize)).style(
                        Style::default()
                            .fg(th.border)
                            .add_modifier(ratatui::style::Modifier::DIM),
                    ),
                    rect.inner(ratatui::layout::Margin {
                        horizontal: 1,
                        vertical: 0,
                    }),
                );
                continue;
            };
            let item = &self.request.items[index];
            let marker = if index == self.selected { "›" } else { " " };
            let budget = (inner.width as usize).saturating_sub(2);
            let label = fit(&item.label, budget);
            let text = format!("{marker} {label}");
            let mut style = Style::default();
            if item.danger {
                style = style.fg(th.err);
            }
            if item.disabled.is_some() {
                style = style.fg(th.placeholder);
            }
            if index == self.selected {
                style = th.selected();
                if item.disabled.is_some() {
                    style = style.add_modifier(ratatui::style::Modifier::DIM);
                }
            }
            f.render_widget(
                Paragraph::new(Line::raw(fit(&text, inner.width as usize))).style(style),
                rect,
            );
            self.hits.push((rect, index));
        }
        if footer {
            let reason = self
                .request
                .items
                .get(self.selected)
                .and_then(|i| i.disabled.as_deref())
                .unwrap_or(MOUSE_FOOTER);
            let message = if self.offset > 0 || self.offset + rows < lines.len() {
                format!("↕ {reason}")
            } else {
                reason.into()
            };
            f.render_widget(
                Paragraph::new(fit(&message, inner.width as usize))
                    .style(Style::default().fg(th.placeholder)),
                Rect::new(inner.x, inner.bottom() - 1, inner.width, 1),
            );
        }
    }

    #[cfg(test)]
    pub(crate) fn hit_rect_for(&self, command: Command) -> Option<Rect> {
        self.hits.iter().find_map(|(rect, index)| {
            (self.request.items.get(*index)?.command == command).then_some(*rect)
        })
    }

    #[cfg(test)]
    pub(crate) fn menu_area(&self) -> Rect {
        self.area
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};
    fn menu() -> ContextMenu {
        ContextMenu::new(
            Request {
                title: "Selected skill".into(),
                detail: "Single skill".into(),
                target: Target::Skill("alpha".into()),
                items: vec![
                    Item::new(Command::Open, "View", KeyCode::Enter, true, "", 0),
                    Item::new(Command::Check, "Check", KeyCode::Char('u'), true, "", 1),
                    Item::new(Command::Update, "Update", KeyCode::Char('U'), true, "", 1),
                    Item::new(
                        Command::Remove,
                        "Delete",
                        KeyCode::Char('x'),
                        false,
                        "Unavailable now",
                        2,
                    ),
                ],
            },
            79,
            23,
        )
    }
    fn draw(menu: &mut ContextMenu, w: u16, h: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal
            .draw(|f| menu.draw(f, f.area(), &Theme::default()))
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect()
    }
    #[test]
    fn context_menu_clamps_and_scrolls_on_small_screens() {
        let mut m = menu();
        for (w, h) in [(80, 24), (18, 5), (1, 1), (4, 3)] {
            draw(&mut m, w, h);
            assert!(m.area.right() <= w && m.area.bottom() <= h);
        }
        m.selected = 3;
        draw(&mut m, 30, 5);
        assert!(m.offset > 0);
        assert!(m.hits.iter().any(|(_, i)| *i == 3));
    }
    #[test]
    fn context_menu_enter_shortcuts_and_disabled_reasons() {
        let mut m = menu();
        assert!(matches!(
            m.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE)),
            MenuEvent::Execute(Command::Check)
        ));
        assert!(matches!(
            m.key(KeyEvent::new(KeyCode::Char('U'), KeyModifiers::SHIFT)),
            MenuEvent::Execute(Command::Update)
        ));
        assert!(matches!(
            m.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            MenuEvent::Execute(Command::Update)
        ));
        assert!(matches!(
            m.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL)),
            MenuEvent::Stay
        ));
        let rendered = draw(&mut m, 80, 24);
        assert!(rendered.contains("Left click to run"));
        assert!(!rendered.contains("↑↓ choose"));
        assert!(!rendered.contains("shortcut run"));
        assert!(matches!(
            m.key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)),
            MenuEvent::Stay
        ));
        assert!(draw(&mut m, 80, 24).contains("Unavailable now"));
        for _ in 0..10 {
            m.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        assert_eq!(m.selected, 3);
        for _ in 0..10 {
            m.key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        }
        assert_eq!(m.selected, 0);
    }
    #[test]
    fn context_menu_outside_click_consumed_inside_right_click_ignored() {
        let mut m = menu();
        draw(&mut m, 80, 24);
        let mut event = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Right),
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        assert!(matches!(m.mouse(event), MenuEvent::Close));
        event.column = m.hits[1].0.x;
        event.row = m.hits[1].0.y;
        assert!(matches!(m.mouse(event), MenuEvent::Stay));
        event.kind = MouseEventKind::Moved;
        m.mouse(event);
        assert_eq!(m.selected, 1);
        event.kind = MouseEventKind::Down(MouseButton::Left);
        assert!(matches!(m.mouse(event), MenuEvent::Execute(Command::Check)));
    }

    #[test]
    fn context_menu_mouse_focus_updates_footer_message() {
        let mut m = menu();
        draw(&mut m, 80, 24);
        let rect = m.hits.iter().find(|(_, i)| *i == 3).unwrap().0;
        assert!(matches!(
            m.mouse(MouseEvent {
                kind: MouseEventKind::Moved,
                column: rect.x,
                row: rect.y,
                modifiers: KeyModifiers::NONE,
            }),
            MenuEvent::Stay
        ));
        assert_eq!(m.selected, 3);
        assert!(draw(&mut m, 80, 24).contains("Unavailable now"));
    }

    #[test]
    fn context_menu_without_disabled_items_has_no_footer_row() {
        let mut m = menu();
        m.request.items.pop();
        let rendered = draw(&mut m, 80, 24);
        assert!(!rendered.contains("Left click to run"));
        assert!(!rendered.contains("Unavailable now"));
    }
}
