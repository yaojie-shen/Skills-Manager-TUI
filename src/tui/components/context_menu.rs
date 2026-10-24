//! Context menu presentation and input. Commands are dispatched by the owning view.
//! Shortcuts and selected action/disabled explanations belong to the app footer.
use crate::tui::app::Hints;
use crate::tui::theme::Theme;
use crate::tui::widgets::{PopupSurface, fit, width};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Frame, layout::Rect, style::Style, text::Line, widgets::Paragraph};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Open,
    EditMembers,
    Description,
    Color,
    Merge,
    Matrix,
    Tags,
    Presets,
    Deploy,
    Note,
    Rename,
    Source,
    Check,
    Refresh,
    Update,
    Accept,
    Remove,
    Relink,
    Adopt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Skill(String),
    Tag(String),
    Preset(String),
    Repository(String),
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
    pub fn keyboard(request: Request) -> Self {
        Self::new(request, u16::MAX, u16::MAX)
    }
    pub fn hints(&self) -> Hints {
        if self
            .request
            .items
            .get(self.selected)
            .is_none_or(|item| item.disabled.is_some())
        {
            &[("↑↓", "choose"), ("Esc", "close")]
        } else {
            &[("↑↓", "choose"), ("Enter", "run"), ("Esc", "close")]
        }
    }
    pub fn footer_help(&self) -> crate::tui::footer::Help<'_> {
        let item = self.request.items.get(self.selected);
        crate::tui::footer::Help::new(self.hints())
            .detail(item.map_or("", |item| item.disabled.as_deref().unwrap_or(&item.label)))
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
        match crate::tui::widgets::popup_mouse(event, self.area) {
            crate::tui::widgets::PopupMouse::OutsidePress => return MenuEvent::Close,
            crate::tui::widgets::PopupMouse::Inside => {}
            _ => return MenuEvent::Stay,
        }
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
        self.hits.clear();
        let wanted = self
            .request
            .items
            .iter()
            .map(|i| width(&i.label) + 4)
            .chain([
                width(&self.request.title) + 4,
                width(&self.request.detail) + 4,
            ])
            .max()
            .unwrap_or(36)
            .clamp(36, 72) as u16;
        let w = wanted.min(bounds.width);
        let mut lines = Vec::new();
        for (index, item) in self.request.items.iter().enumerate() {
            if index > 0 && self.request.items[index - 1].group != item.group {
                lines.push(None);
            }
            lines.push(Some(index));
        }
        let has_detail = !self.request.detail.is_empty();
        let h = (lines.len() as u16 + 2 + u16::from(has_detail)).min(bounds.height);

        self.area = if self.anchor == (u16::MAX, u16::MAX) {
            crate::tui::widgets::centered(bounds, w, h)
        } else {
            Rect::new(
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
            )
        };
        f.render_widget(PopupSurface, self.area);
        let block = th.block(fit(&self.request.title, w.saturating_sub(4) as usize), true);
        let inner = block.inner(self.area);
        f.render_widget(block, self.area);
        if inner.width == 0 || inner.height == 0 {
            return;
        }
        let show_detail = has_detail && inner.height >= 3;
        let rows = inner.height.saturating_sub(u16::from(show_detail)) as usize;
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
        // Keep overflow discoverable without a second, local help row.
        for (visible, y, marker) in [
            (self.offset > 0, self.area.y, "↑"),
            (
                self.offset + rows < lines.len(),
                self.area.bottom() - 1,
                "↓",
            ),
        ] {
            if visible {
                f.render_widget(
                    Paragraph::new(marker).style(th.dim()),
                    Rect::new(inner.right() - 1, y, 1, 1),
                );
            }
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
        m.selected = 0;
        assert!(draw(&mut m, 30, 5).contains('↓'));
        m.selected = 3;
        assert!(draw(&mut m, 30, 5).contains('↑'));
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
        assert!(!rendered.contains("Click or shown keys run"));
        assert!(!rendered.contains("↑↓ choose"));
        assert!(!rendered.contains("shortcut run"));
        assert!(!rendered.contains(" u "));
        assert!(!rendered.contains(" U "));
        assert!(matches!(
            m.key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)),
            MenuEvent::Stay
        ));
        assert!(!draw(&mut m, 80, 24).contains("Unavailable now"));
        assert_eq!(m.footer_help().detail, "Unavailable now");
        assert!(!m.hints().iter().any(|(key, _)| *key == "Enter"));
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
        assert!(!draw(&mut m, 80, 24).contains("Unavailable now"));
        assert_eq!(m.footer_help().detail, "Unavailable now");
        assert!(!m.hints().iter().any(|(key, _)| *key == "Enter"));
    }

    #[test]
    fn context_menu_without_disabled_items_has_no_local_footer() {
        let mut m = menu();
        m.request.items.pop();
        let rendered = draw(&mut m, 80, 24);
        assert!(!rendered.contains("Click or shown keys run"));
        assert!(!rendered.contains("Unavailable now"));
    }

    #[test]
    fn keyboard_menu_is_centered_and_exposes_keys_only_as_hints() {
        let mut m = ContextMenu::keyboard(menu().request);
        let rendered = draw(&mut m, 80, 24);
        assert_eq!(m.area.x, (80 - m.area.width) / 2);
        assert_eq!(
            m.hints(),
            &[("↑↓", "choose"), ("Enter", "run"), ("Esc", "close")]
        );
        assert!(!rendered.contains("↑↓ choose"));
        assert!(rendered.contains("View"));
        assert!(rendered.contains("Check"));
        assert!(!rendered.contains("shown keys"));
    }
}
