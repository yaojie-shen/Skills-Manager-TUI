//! A local filter for lists of tags, presets, repositories, or issues.
use crate::tui::app::{Action, Ctx};
use crate::tui::widgets::Input;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{Frame, layout::Rect};

#[derive(Default)]
pub struct Filter {
    pub input: Input,
    pub editing: bool,
    pub rect: Rect,
}

impl Filter {
    pub fn matches(&self, text: &str) -> bool {
        let text = text.to_lowercase();
        self.input.value().split_whitespace().all(|word| {
            let word = word.to_lowercase();
            let mut chars = text.chars();
            word.chars()
                .all(|c| chars.by_ref().any(|candidate| candidate == c))
        })
    }

    pub fn key(&mut self, key: KeyEvent) -> bool {
        if !self.editing {
            if key.code == KeyCode::Char('/') {
                self.editing = true;
                return true;
            }
            return false;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Down => self.editing = false,
            _ => {
                self.input.handle_key(key);
            }
        }
        true
    }

    pub fn paste(&mut self, text: &str) -> Vec<Action> {
        match self.input.paste(text) {
            Err(error) => vec![Action::Error(error.into())],
            _ => vec![],
        }
    }

    pub fn draw(&mut self, f: &mut Frame, area: Rect, label: &str, ctx: &Ctx) -> Rect {
        let height = area.height.min(3);
        self.rect = Rect::new(area.x, area.y, area.width, height);
        let block = ctx.theme.block(format!(" {label} "), self.editing);
        let inner = block.inner(self.rect);
        f.render_widget(block, self.rect);
        self.input.render(
            f,
            inner,
            self.editing,
            " / filter · Enter results",
            ctx.theme,
        );
        Rect::new(area.x, area.y + height, area.width, area.height - height)
    }
}
