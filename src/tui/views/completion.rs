//! Local query-filter completion. Repository names come from source URLs.
use std::{collections::BTreeSet, ops::Range};

use crate::tui::{
    app::Ctx,
    widgets::{Input, OverlayClear, fit},
};
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Frame, layout::Rect, text::Line, widgets::Paragraph};

#[derive(Default)]
pub(crate) struct Completion {
    choices: Vec<String>,
    token: Range<usize>,
    selected: usize,
    offset: usize,
    rect: Rect,
}

impl Completion {
    pub fn active(&self) -> bool {
        !self.choices.is_empty()
    }
    pub fn close(&mut self) {
        self.choices.clear();
        self.rect = Rect::default();
    }

    pub fn update(&mut self, input: &Input, ctx: &Ctx) {
        let range = token_range(input.value(), input.cursor_byte());
        let token = &input.value()[range.clone()];
        let values = match token.split_once(':') {
            Some(("repo", _)) => ctx
                .snap
                .skills
                .iter()
                .filter_map(|r| {
                    if let Some(skills::meta::Source::Git { url, .. }) = &r.source {
                        skills::repository::source_name(url)
                    } else {
                        None
                    }
                })
                .collect(),
            Some(("tag", _)) => ctx
                .snap
                .skills
                .iter()
                .flat_map(|r| r.tags.iter().cloned())
                .chain(ctx.ws.config.tags.iter().map(|t| t.name.clone()))
                .collect(),
            Some(("agent", _)) => ctx.snap.agents.iter().map(|a| a.key.clone()).collect(),
            Some(("status", _)) => [
                "managed",
                "modified",
                "unmanaged",
                "missing",
                "renamed",
                "invalid",
                "corrupt-meta",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            _ => BTreeSet::new(),
        };
        self.choices = candidates(token, values);
        self.token = range;
        self.selected = 0;
        self.offset = 0;
        self.rect = Rect::default();
    }

    /// Installation candidates have repository and health data, but no tags or deployment yet.
    pub fn update_install(&mut self, input: &Input, ctx: &Ctx) {
        self.update(input, ctx);
        self.choices.retain(|choice| {
            choice.starts_with("repo:")
                || (choice.starts_with("status:")
                    && (choice == "status:"
                        || ctx
                            .snap
                            .skills
                            .iter()
                            .any(|r| choice == &format!("status:{}", r.status.label()))))
        });
    }

    pub fn healthy_only(&mut self) {
        self.choices.retain(|choice| {
            !choice.starts_with("status:")
                || matches!(
                    choice.as_str(),
                    "status:" | "status:managed" | "status:unmanaged"
                )
        });
        self.selected = self.selected.min(self.choices.len().saturating_sub(1));
    }

    pub fn move_by(&mut self, delta: i32) {
        if self.active() {
            self.selected =
                (self.selected as i32 + delta).clamp(0, self.choices.len() as i32 - 1) as usize;
        }
    }

    pub fn accept(&mut self, input: &mut Input) {
        if let Some(choice) = self.choices.get(self.selected) {
            // A completed value gets a space; completing a prefix continues into its values.
            let suffix = if choice.ends_with(':')
                || input.value()[self.token.end..].starts_with(char::is_whitespace)
            {
                ""
            } else {
                " "
            };
            input.replace_range(self.token.clone(), &format!("{choice}{suffix}"));
        }
        self.close();
    }

    /// Returns whether the popup consumed the event and whether it accepted a value.
    pub fn mouse(&mut self, m: MouseEvent, input: &mut Input) -> (bool, bool) {
        if !self.active() || !self.rect.contains((m.column, m.row).into()) {
            return (false, false);
        }
        match m.kind {
            MouseEventKind::ScrollDown => self.move_by(1),
            MouseEventKind::ScrollUp => self.move_by(-1),
            MouseEventKind::Down(MouseButton::Left)
                if m.row > self.rect.y && m.row < self.rect.bottom() - 1 =>
            {
                let index = self.offset + (m.row - self.rect.y - 1) as usize;
                if index < self.choices.len() {
                    self.selected = index;
                    self.accept(input);
                    return (true, true);
                }
            }
            _ => {}
        }
        (true, false)
    }

    pub fn draw(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        self.rect = Rect::default();
        if !self.active() || area.height < 3 || area.width < 8 {
            return;
        }
        let rows = self
            .choices
            .len()
            .min(6)
            .min(area.height.saturating_sub(2) as usize);
        if self.selected < self.offset {
            self.offset = self.selected;
        }
        if self.selected >= self.offset + rows {
            self.offset = self.selected + 1 - rows;
        }
        self.rect = Rect {
            width: area.width.min(64),
            height: rows as u16 + 2,
            ..area
        };
        f.render_widget(OverlayClear, self.rect);
        let block = ctx.theme.block(" filters · Tab accepts ", true);
        let inner = block.inner(self.rect);
        f.render_widget(block, self.rect);
        let lines: Vec<Line> = self
            .choices
            .iter()
            .enumerate()
            .skip(self.offset)
            .take(rows)
            .map(|(i, s)| {
                Line::from(fit(
                    &format!(" {} {s}", if i == self.selected { "›" } else { " " }),
                    inner.width as usize,
                ))
                .style(if i == self.selected {
                    ctx.theme.selected()
                } else {
                    ctx.theme.dim()
                })
            })
            .collect();
        f.render_widget(Paragraph::new(lines), inner);
    }
}

fn token_range(text: &str, cursor: usize) -> Range<usize> {
    let start = text[..cursor]
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_whitespace())
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0);
    let end = text[cursor..]
        .char_indices()
        .find(|(_, c)| c.is_whitespace())
        .map(|(i, _)| cursor + i)
        .unwrap_or(text.len());
    start..end
}

fn candidates(token: &str, values: BTreeSet<String>) -> Vec<String> {
    if token.is_empty() {
        return vec![];
    }
    let (prefix, query, choices): (&str, &str, Vec<String>) =
        if let Some((prefix, query)) = token.split_once(':') {
            (prefix, query, values.into_iter().collect())
        } else {
            (
                "",
                token,
                ["repo:", "tag:", "agent:", "status:"]
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
            )
        };
    let mut ranked: Vec<_> = choices
        .into_iter()
        .filter(|value| !value.chars().any(char::is_whitespace))
        .filter_map(|value| score(query, &value).map(|score| (score, value)))
        .collect();
    ranked.sort();
    ranked
        .into_iter()
        .map(|(_, value)| {
            if prefix.is_empty() {
                value
            } else {
                format!("{prefix}:{value}")
            }
        })
        .collect()
}

/// Prefer exact/prefix, then substring, then subsequence, then a small typo.
fn score(query: &str, value: &str) -> Option<(usize, usize)> {
    let q = query.to_lowercase();
    let v = value.to_lowercase();
    if q == v {
        return Some((0, 0));
    }
    if v.starts_with(&q) {
        return Some((1, v.len()));
    }
    if let Some(at) = v.find(&q) {
        return Some((2, at));
    }
    let mut chars = v.chars();
    if q.chars().all(|c| chars.by_ref().any(|x| x == c)) {
        return Some((3, v.len()));
    }
    if q.chars().count() < 3 {
        return None;
    }
    let a: Vec<_> = q.chars().collect();
    let b: Vec<_> = v.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, x) in a.iter().enumerate() {
        let mut row = vec![i + 1; b.len() + 1];
        for (j, y) in b.iter().enumerate() {
            row[j + 1] = (prev[j + 1] + 1)
                .min(row[j] + 1)
                .min(prev[j] + usize::from(x != y));
        }
        prev = row;
    }
    let distance = prev[b.len()];
    (distance <= if a.len() >= 6 { 2 } else { 1 }).then_some((4, distance))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn completes_source_names_with_substrings_subsequences_and_typos() {
        let values = ["sampleorg/kit", "other/tools"]
            .into_iter()
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        for query in ["repo:samp", "repo:kit", "repo:smplkt", "repo:sampleor/kit"] {
            assert_eq!(
                candidates(query, values.clone()),
                vec!["repo:sampleorg/kit"]
            );
        }
        assert!(candidates("repo:unknown", values).is_empty());
        assert_eq!(candidates("rep", BTreeSet::new()), vec!["repo:"]);
    }
    #[test]
    fn token_replacement_preserves_other_filters_and_unicode() {
        let mut input = Input::with_value("中文 repo:smp tag:工作");
        for _ in 0..8 {
            input.handle_key(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Left,
                crossterm::event::KeyModifiers::NONE,
            ));
        }
        let range = token_range(input.value(), input.cursor_byte());
        assert_eq!(&input.value()[range.clone()], "repo:smp");
        let mut popup = Completion {
            token: range,
            choices: vec!["repo:sampleorg/kit".into()],
            ..Default::default()
        };
        popup.accept(&mut input);
        assert_eq!(input.value(), "中文 repo:sampleorg/kit tag:工作");
        assert_eq!(&input.value()[input.cursor_byte()..], " tag:工作");
        assert!(!popup.active());
    }
    #[test]
    fn mouse_scrolls_and_accepts_only_within_the_popup() {
        let mut input = Input::with_value("tag:");
        let mut popup = Completion {
            token: 0..4,
            choices: vec!["tag:alpha".into(), "tag:beta".into()],
            rect: Rect::new(3, 3, 20, 4),
            ..Default::default()
        };
        let event = |kind, row| MouseEvent {
            kind,
            column: 4,
            row,
            modifiers: crossterm::event::KeyModifiers::NONE,
        };
        assert_eq!(
            popup.mouse(event(MouseEventKind::ScrollDown, 4), &mut input),
            (true, false)
        );
        assert_eq!(popup.selected, 1);
        assert_eq!(
            popup.mouse(
                event(MouseEventKind::Down(MouseButton::Left), 0),
                &mut input
            ),
            (false, false)
        );
        assert_eq!(
            popup.mouse(
                event(MouseEventKind::Down(MouseButton::Left), 5),
                &mut input
            ),
            (true, true)
        );
        assert_eq!(input.value(), "tag:beta ");
        assert!(!popup.active());
        assert!(candidates("tag:", ["two words".into()].into_iter().collect()).is_empty());
    }
}
