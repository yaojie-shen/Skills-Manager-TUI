//! Plain-text navigation search, backed by the shared panel and text index.
use crate::tui::{
    app::{Action, Ctx},
    components::search_panel::{PanelLayout, PanelStyle, SearchEvent, SearchPanel},
};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{Frame, layout::Rect, text::Line};
use skills::search::{TextDocument, TextSearcher};

#[derive(Default)]
pub struct Filter {
    panel: SearchPanel,
    searcher: TextSearcher,
    dictionary: Option<(
        std::path::PathBuf,
        skills::config::SearchConfig,
        skills::dict::Dictionaries,
    )>,
}
// Existing page focus code addresses the panel's input and editing state directly.
impl std::ops::Deref for Filter {
    type Target = SearchPanel;
    fn deref(&self) -> &Self::Target {
        &self.panel
    }
}
impl std::ops::DerefMut for Filter {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.panel
    }
}
impl Filter {
    pub fn rank(&mut self, documents: &[TextDocument], ctx: &Ctx) -> Vec<usize> {
        if self
            .dictionary
            .as_ref()
            .is_none_or(|(root, cfg, _)| root != &ctx.ws.root || cfg != &ctx.settings.search)
        {
            self.dictionary = Some((
                ctx.ws.root.clone(),
                ctx.settings.search.clone(),
                skills::dict::Dictionaries::load(&ctx.ws.root, &ctx.settings.search.dictionaries),
            ));
        }
        self.searcher.search(
            documents,
            self.panel.input.value(),
            &ctx.settings.search,
            &self.dictionary.as_ref().unwrap().2,
        )
    }
    pub fn key(&mut self, key: KeyEvent) -> bool {
        let key =
            if !self.panel.editing && key.code == KeyCode::Char('q') && key.modifiers.is_empty() {
                KeyEvent::new(KeyCode::Esc, key.modifiers)
            } else {
                key
            };
        if !self.panel.editing {
            if key.code == KeyCode::Char('/') {
                self.panel.editing = true;
                return true;
            }
            if key.code == KeyCode::Esc && !self.panel.input.is_empty() {
                self.panel.input.clear();
                return true;
            }
            return false;
        }
        match self.panel.key(key) {
            SearchEvent::Up => return false,
            SearchEvent::Results => self.panel.editing = false,
            SearchEvent::Escape => {
                self.panel.editing = false;
                return true;
            }
            _ => {}
        }
        true
    }
    pub fn paste(&mut self, text: &str) -> Vec<Action> {
        match self.panel.paste(text) {
            Err(error) => vec![Action::Error(error)],
            _ => vec![],
        }
    }
    pub fn draw(
        &mut self,
        f: &mut Frame,
        area: Rect,
        label: &str,
        results: &str,
        active: bool,
        ctx: &Ctx,
    ) -> Rect {
        self.panel
            .draw(
                f,
                area,
                PanelStyle {
                    layout: PanelLayout::Separate,
                    input_title: Line::from(format!(" {label} ")),
                    results_title: Line::from(format!(" {results} ")),
                    hint: ("filter…", ""),
                    input_active: self.panel.editing,
                    results_active: active && !self.panel.editing,
                    header_height: 0,
                },
                &ctx.settings.theme,
            )
            .results
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    #[test]
    fn plain_filter_has_no_completion_and_returns_empty_escape_and_up_to_page() {
        let mut filter = Filter::default();
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        assert!(filter.key(key(KeyCode::Char('/'))));
        assert!(filter.paste("tag:work").is_empty());
        assert_eq!(filter.input.value(), "tag:work");
        assert!(!filter.completion.active());
        assert!(!filter.key(key(KeyCode::Up)));
        assert!(filter.editing);
        assert!(filter.key(key(KeyCode::Esc)));
        assert!(filter.input.is_empty());
        assert!(filter.editing);
        assert!(filter.key(key(KeyCode::Esc)));
        assert!(!filter.editing);
        filter.key(key(KeyCode::Char('/')));
        filter.paste("文档");
        assert!(filter.key(key(KeyCode::Down)));
        assert!(!filter.editing);
        assert_eq!(filter.input.value(), "文档");
    }
}
