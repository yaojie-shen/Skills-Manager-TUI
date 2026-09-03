//! Tags tab: browse by tag, rename/delete.

use super::{View, split_panes, status_glyph, wheel};
use crate::tui::app::{Action, Ctx, Hints};
use crate::tui::modal::Modal;
use crate::tui::widgets::{ListNav, pad};
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, Paragraph, Wrap};

#[derive(Default)]
pub struct TagsView {
    rows: Vec<(String, usize)>,
    list: ListNav,
    left: Rect,
    right: Rect,
}

pub const UNTAGGED: &str = "(untagged)";

impl TagsView {
    fn selected_tag(&self) -> Option<&str> {
        self.list
            .selected()
            .and_then(|i| self.rows.get(i))
            .map(|(t, _)| t.as_str())
    }
    fn open(&self) -> Vec<Action> {
        match self.selected_tag() {
            Some(UNTAGGED) => vec![Action::Search {
                query: "untagged".into(),
                focus_list: true,
            }],
            Some(t) => vec![Action::Search {
                query: format!("tag:{t} "),
                focus_list: true,
            }],
            None => vec![],
        }
    }
}

impl View for TagsView {
    fn refresh(&mut self, ctx: &Ctx) {
        self.rows = ctx.snap.all_tags().into_iter().collect();
        let untagged = ctx
            .snap
            .skills
            .iter()
            .filter(|s| s.tags.is_empty() && s.status.is_present())
            .count();
        self.rows.push((UNTAGGED.into(), untagged));
        self.list.clamp(self.rows.len());
    }

    fn handle_key(&mut self, k: KeyEvent, _ctx: &Ctx) -> Vec<Action> {
        let n = self.rows.len();
        match k.code {
            KeyCode::Char('q') | KeyCode::Esc => vec![Action::Quit],
            KeyCode::Down | KeyCode::Char('j') => {
                self.list.move_by(1, n);
                vec![]
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.list.move_by(-1, n);
                vec![]
            }
            KeyCode::Home | KeyCode::Char('g') => {
                self.list.first(n);
                vec![]
            }
            KeyCode::End | KeyCode::Char('G') => {
                self.list.last(n);
                vec![]
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => self.open(),
            KeyCode::Char('r') => match self.selected_tag() {
                Some(t) if t != UNTAGGED => vec![Action::OpenModal(Box::new(Modal::rename_tag(t)))],
                _ => vec![],
            },
            KeyCode::Char('x') => match self.selected_tag() {
                Some(t) if t != UNTAGGED => vec![Action::OpenModal(Box::new(Modal::delete_tag(t)))],
                _ => vec![],
            },
            _ => vec![],
        }
    }

    fn handle_mouse(&mut self, m: MouseEvent, _ctx: &Ctx) -> Vec<Action> {
        let at = (m.column, m.row).into();
        if let Some(d) = wheel(&m) {
            if self.left.contains(at) {
                self.list.move_by(d, self.rows.len());
            }
            return vec![];
        }
        if let MouseEventKind::Down(MouseButton::Left) = m.kind
            && self.left.contains(at)
            && let Some((_, double)) = self.list.click(m.row, self.rows.len())
            && double
        {
            return self.open();
        }
        vec![]
    }

    fn draw(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        let th = ctx.theme;
        let (left, right) = split_panes(area, 38);
        self.left = left;
        self.right = right;
        let w = left.width.saturating_sub(4) as usize;
        let items: Vec<ListItem> = self
            .rows
            .iter()
            .map(|(t, n)| {
                let style = if t == UNTAGGED { th.dim() } else { th.tag() };
                ListItem::new(Line::from(vec![
                    Span::styled(pad(t, w.saturating_sub(5)), style),
                    Span::styled(format!("{n:>4}"), th.dim()),
                ]))
            })
            .collect();
        self.list.set_area_from_block(left);
        let list = List::new(items)
            .block(th.block(" tags ", true))
            .highlight_style(th.selected())
            .highlight_symbol("▸ ");
        f.render_stateful_widget(list, left, &mut self.list.state);

        let block = th.block(" skills with this tag ", false);
        let inner = block.inner(right);
        f.render_widget(block, right);
        let mut lines: Vec<Line> = Vec::new();
        if let Some(t) = self.selected_tag() {
            let members: Vec<_> = ctx
                .snap
                .skills
                .iter()
                .filter(|s| {
                    if t == UNTAGGED {
                        s.tags.is_empty() && s.status.is_present()
                    } else {
                        s.tags.iter().any(|x| x == t)
                    }
                })
                .collect();
            for s in members {
                lines.push(Line::from(vec![
                    status_glyph(&s.status, th),
                    Span::raw(format!(" {}", pad(&s.key, 26))),
                    Span::styled(s.description.as_deref().unwrap_or("").to_string(), th.dim()),
                ]));
            }
            if lines.is_empty() {
                lines.push(Line::from(Span::styled("no skills", th.dim())));
            }
        }
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
    }

    fn hints(&self) -> Hints {
        &[
            ("Enter", "search by tag"),
            ("r", "rename"),
            ("x", "delete"),
            ("/", "search"),
            ("q", "quit"),
        ]
    }
}
