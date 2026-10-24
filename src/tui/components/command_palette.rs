//! Command selection; focused command descriptions belong to the app footer.
use crate::tui::app::{Ctx, Hints};
use crate::tui::widgets::{
    Input, ListNav, PopupClass, PopupSurface, fit, popup_size, popup_too_small,
};
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Span},
    widgets::{List, ListItem, Paragraph},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Rescan,
    VerifyContent,
    Install,
    Repositories,
    Repair,
    MigrationReport,
    RootSync,
    ToggleTags,
    Undo,
    Redo,
    Help,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Stay,
    Close,
    Execute(Command),
}

#[derive(Clone, Copy)]
struct Entry {
    command: Command,
    label: &'static str,
    detail: &'static str,
    keywords: &'static str,
}

const ENTRIES: &[Entry] = &[
    Entry {
        command: Command::Rescan,
        label: "Rescan workspace",
        detail: "Refresh skills and deployment state",
        keywords: "refresh reload scan",
    },
    Entry {
        command: Command::VerifyContent,
        label: "Verify local content",
        detail: "Compare repository skills with recorded baselines",
        keywords: "health hash check modified local content",
    },
    Entry {
        command: Command::Install,
        label: "Install skill",
        detail: "Use a repository, archive URL or local path",
        keywords: "add repository repo local",
    },
    Entry {
        command: Command::Repositories,
        label: "Browse repositories",
        detail: "Open installed repository sources",
        keywords: "source repos",
    },
    Entry {
        command: Command::Repair,
        label: "Repair health issues",
        detail: "Preview root repairs before applying",
        keywords: "health fix missing broken",
    },
    Entry {
        command: Command::MigrationReport,
        label: "Show metadata migration report",
        detail: "Review recovery instructions from library opening",
        keywords: "migration report recovery upgrade format",
    },
    Entry {
        command: Command::RootSync,
        label: "Root backup and sync",
        detail: "Configure or run Git synchronization",
        keywords: "git remote push pull backup",
    },
    Entry {
        command: Command::ToggleTags,
        label: "Show or hide tags",
        detail: "Change tag visibility throughout the interface",
        keywords: "settings classification",
    },
    Entry {
        command: Command::Undo,
        label: "Undo last change",
        detail: "Preview the previous session change",
        keywords: "history back",
    },
    Entry {
        command: Command::Redo,
        label: "Redo last change",
        detail: "Preview the next session change",
        keywords: "history forward",
    },
    Entry {
        command: Command::Help,
        label: "Open keyboard help",
        detail: "Show navigation and interaction reference",
        keywords: "shortcuts keys",
    },
];

pub struct CommandPalette {
    input: Input,
    shown: Vec<usize>,
    list: ListNav,
    rect: Rect,
    input_rect: Rect,
}

impl Default for CommandPalette {
    fn default() -> Self {
        let mut palette = Self {
            input: Input::default(),
            shown: (0..ENTRIES.len()).collect(),
            list: ListNav::default(),
            rect: Rect::default(),
            input_rect: Rect::default(),
        };
        palette.list.first(palette.shown.len());
        palette
    }
}

impl CommandPalette {
    pub fn hints(&self) -> Hints {
        if self.shown.is_empty() {
            return &[("type", "filter commands"), ("Esc", "close")];
        }
        &[
            ("type", "filter commands"),
            ("↑↓", "choose"),
            ("Enter", "run"),
            ("Esc", "close"),
        ]
    }

    pub fn footer_help(&self) -> crate::tui::footer::Help<'_> {
        let detail = self
            .list
            .selected()
            .and_then(|index| self.shown.get(index))
            .map(|index| ENTRIES[*index].detail)
            .unwrap_or("");
        crate::tui::footer::Help::new(self.hints()).detail(detail)
    }

    fn refilter(&mut self) {
        let query = self.input.value().trim().to_lowercase();
        let terms = query.split_whitespace().collect::<Vec<_>>();
        self.shown = ENTRIES
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                let haystack =
                    format!("{} {} {}", entry.label, entry.detail, entry.keywords).to_lowercase();
                terms.iter().all(|term| haystack.contains(term))
            })
            .map(|(index, _)| index)
            .collect();
        self.list.first(self.shown.len());
    }

    fn selected(&self) -> Option<Command> {
        self.list
            .selected()
            .and_then(|index| self.shown.get(index))
            .and_then(|index| ENTRIES.get(*index))
            .map(|entry| entry.command)
    }

    pub fn paste(&mut self, text: &str) -> Result<(), String> {
        if self.input.paste(text).map_err(str::to_string)? {
            self.refilter();
        }
        Ok(())
    }

    pub fn key(&mut self, key: KeyEvent) -> Event {
        match key.code {
            KeyCode::Esc => Event::Close,
            KeyCode::Enter => self.selected().map_or(Event::Stay, Event::Execute),
            KeyCode::Down => {
                self.list.move_by(1, self.shown.len());
                Event::Stay
            }
            KeyCode::Up => {
                self.list.move_by(-1, self.shown.len());
                Event::Stay
            }
            KeyCode::PageDown => {
                self.list.move_by(8, self.shown.len());
                Event::Stay
            }
            KeyCode::PageUp => {
                self.list.move_by(-8, self.shown.len());
                Event::Stay
            }
            _ => {
                if self.input.handle_key(key) {
                    self.refilter();
                }
                Event::Stay
            }
        }
    }

    pub fn mouse(&mut self, event: MouseEvent) -> Event {
        match crate::tui::widgets::popup_mouse(event, self.rect) {
            crate::tui::widgets::PopupMouse::OutsidePress => return Event::Close,
            crate::tui::widgets::PopupMouse::Inside => {}
            _ => return Event::Stay,
        }
        let point = (event.column, event.row).into();
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) if self.input_rect.contains(point) => {
                self.input.click(event.column);
                Event::Stay
            }
            MouseEventKind::Down(MouseButton::Left) if self.list.rows.contains(point) => {
                let Some(row) = self.list.row_at(event.row, self.shown.len()) else {
                    return Event::Stay;
                };
                self.list.select(Some(row));
                self.selected().map_or(Event::Stay, Event::Execute)
            }
            MouseEventKind::ScrollDown => {
                self.list.move_by(3, self.shown.len());
                Event::Stay
            }
            MouseEventKind::ScrollUp => {
                self.list.move_by(-3, self.shown.len());
                Event::Stay
            }
            _ => Event::Stay,
        }
    }

    #[cfg(test)]
    pub fn mouse_row_for_test(&self) -> Rect {
        self.list.rows
    }

    pub fn draw(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        self.input_rect = Rect::default();
        self.list.rows = Rect::default();
        let height = (ENTRIES.len() as u16 + 6).clamp(8, 18);
        self.input.clear_area();
        let size = popup_size(area, PopupClass::Choice, height, (4, 3));
        self.rect = size.outer;
        f.render_widget(PopupSurface, self.rect);
        let block = ctx.settings.theme.block(" Commands ", true);
        let inner = block.inner(self.rect);
        f.render_widget(block, self.rect);
        if !size.usable {
            popup_too_small(f, size, &ctx.settings.theme);
            return;
        }
        self.input_rect = Rect::new(inner.x + 1, inner.y, inner.width.saturating_sub(2), 1);
        self.input.render(
            f,
            self.input_rect,
            true,
            "Search commands…",
            &ctx.settings.theme,
        );
        self.list.rows = Rect::new(
            inner.x + 1,
            inner.y + 2,
            inner.width.saturating_sub(2),
            inner.height.saturating_sub(2),
        );
        self.list.item_height = 1;
        self.list.clamp(self.shown.len());
        let rows = self
            .shown
            .iter()
            .filter_map(|index| ENTRIES.get(*index))
            .map(|entry| {
                ListItem::new(Line::from(Span::raw(fit(
                    entry.label,
                    self.list.rows.width as usize,
                ))))
            });
        f.render_stateful_widget(
            List::new(rows).highlight_style(ctx.settings.theme.selected()),
            self.list.rows,
            &mut self.list.state,
        );
        if self.shown.is_empty() {
            f.render_widget(
                Paragraph::new("No matching commands").style(ctx.settings.theme.dim()),
                self.list.rows,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    #[test]
    fn mouse_command_executes_on_first_press_and_outside_wheel_is_inert() {
        let mut palette = CommandPalette {
            rect: Rect::new(10, 5, 40, 15),
            ..Default::default()
        };
        palette.list.rows = Rect::new(11, 8, 38, 10);
        let mut event = MouseEvent {
            kind: MouseEventKind::Moved,
            column: 12,
            row: 8,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(palette.mouse(event), Event::Stay);
        event.kind = MouseEventKind::Down(MouseButton::Left);
        assert_eq!(palette.mouse(event), Event::Execute(Command::Rescan));
        event.column = 0;
        event.kind = MouseEventKind::ScrollDown;
        let selected = palette.list.selected();
        assert_eq!(palette.mouse(event), Event::Stay);
        assert_eq!(palette.list.selected(), selected);
        for button in [MouseButton::Left, MouseButton::Right, MouseButton::Middle] {
            event.kind = MouseEventKind::Down(button);
            assert_eq!(palette.mouse(event), Event::Close);
        }
    }

    #[test]
    fn empty_results_paste_and_small_terminal_geometry() {
        let root = skills::ops::DownloadDir::new("palette").unwrap();
        skills::config::Config {
            agents: vec![],
            ..Default::default()
        }
        .save(root.path())
        .unwrap();
        let ws = skills::Workspace::open(root.path()).unwrap();
        let snap = ws.scan().unwrap();
        let settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &settings,
        };
        let mut palette = CommandPalette::default();
        palette.paste("no-such-command").unwrap();
        assert_eq!(palette.key(KeyCode::Enter.into()), Event::Stay);
        palette.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        palette.paste("BACKUP").unwrap();
        assert_eq!(palette.selected(), Some(Command::RootSync));
        for (w, h) in [(120, 35), (40, 12), (8, 4), (1, 1)] {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
            terminal.draw(|f| palette.draw(f, f.area(), &ctx)).unwrap();
            assert!(palette.rect.right() <= w && palette.rect.bottom() <= h);
            if w < 20 {
                assert_eq!(palette.list.rows, Rect::default());
            }
        }
        assert_eq!(palette.key(KeyCode::Esc.into()), Event::Close);
    }

    #[test]
    fn filters_and_runs_a_command() {
        let mut palette = CommandPalette::default();
        for ch in "backup".chars() {
            assert_eq!(
                palette.key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE)),
                Event::Stay
            );
        }
        assert_eq!(palette.shown.len(), 1);
        assert_eq!(
            palette.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Event::Execute(Command::RootSync)
        );
    }

    #[test]
    fn documented_upgrade_wording_finds_the_migration_report() {
        for query in ["upgrade", "format"] {
            let mut palette = CommandPalette::default();
            palette.paste(query).unwrap();
            assert_eq!(
                palette.selected(),
                Some(Command::MigrationReport),
                "{query}"
            );
        }
    }
}
