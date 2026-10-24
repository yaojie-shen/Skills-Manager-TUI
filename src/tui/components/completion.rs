//! Local query-filter completion from the current inventory and source metadata.
//! Active suggestions own input. Draw them after all parent content using the
//! popup surface; higher overlays and the App footer may draw afterwards.
use std::{collections::BTreeSet, ops::Range};

use crate::tui::{
    app::Ctx,
    widgets::{Input, PopupSurface, fit},
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
    #[cfg(test)]
    pub fn rect(&self) -> Rect {
        self.rect
    }
    pub fn clear_area(&mut self) {
        self.rect = Rect::default();
    }
    pub fn close(&mut self) {
        self.choices.clear();
        self.rect = Rect::default();
    }

    pub fn update(&mut self, input: &Input, ctx: &Ctx) {
        self.update_scoped(input, ctx, None);
    }

    pub fn update_scoped(&mut self, input: &Input, ctx: &Ctx, keys: Option<&BTreeSet<String>>) {
        let records: Vec<_> = ctx
            .snap
            .skills
            .iter()
            .filter(|r| keys.is_none_or(|keys| keys.contains(&r.key)))
            .collect();
        self.update_records(input, ctx, &records, keys);
    }

    pub fn update_records(
        &mut self,
        input: &Input,
        ctx: &Ctx,
        records: &[&skills::reconcile::SkillRecord],
        keys: Option<&BTreeSet<String>>,
    ) {
        let range = token_range(input.value(), input.cursor_byte());
        let token = &input.value()[range.clone()];
        let values = match token.split_once(':') {
            Some(("repo", _)) => records
                .iter()
                .filter_map(|r| r.source_display_name().map(str::to_owned))
                .collect(),
            Some(("tag", _)) => records
                .iter()
                .flat_map(|r| r.tags.iter().cloned())
                .chain(
                    ctx.ws
                        .config
                        .tags
                        .iter()
                        .filter(|t| {
                            keys.is_none_or(|keys| t.skills.iter().any(|key| keys.contains(key)))
                        })
                        .map(|t| t.name.clone()),
                )
                .collect(),
            Some(("agent", _)) => ctx
                .snap
                .agents
                .iter()
                .filter(|a| {
                    keys.is_none()
                        || records.iter().any(|r| {
                            r.deploy.get(&a.key).is_some_and(|state| {
                                !matches!(
                                    state,
                                    skills::reconcile::DeployState::NotDeployed
                                        | skills::reconcile::DeployState::NoAgentDir
                                )
                            })
                        })
                })
                .map(|a| a.key.clone())
                .collect(),
            Some(("preset", _)) => ctx
                .snap
                .presets
                .by_name
                .iter()
                .filter(|(_, members)| {
                    keys.is_none_or(|keys| members.skills.iter().any(|key| keys.contains(key)))
                })
                .map(|(name, _)| name.clone())
                .collect(),
            Some(("source", _)) if keys.is_some() => records
                .iter()
                .map(|r| r.source_kind().to_string())
                .collect(),
            Some(("source", _)) => ["local", "repository"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            Some(("status", _)) if keys.is_some() => records
                .iter()
                .map(|r| r.status.label().trim_end_matches('?').to_string())
                .collect(),
            Some(("status", _)) => [
                "local",
                "repository",
                "modified",
                "missing-baseline",
                "missing-source",
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
        if !ctx.settings.tags_enabled {
            self.choices
                .retain(|s| !s.starts_with("tag:") && s != "untagged");
        }
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
                        || ctx.snap.skills.iter().any(|r| {
                            choice == &format!("status:{}", r.status.label().trim_end_matches('?'))
                        })))
        });
    }

    pub fn healthy_only(&mut self) {
        self.choices.retain(|choice| {
            !choice.starts_with("status:")
                || matches!(
                    choice.as_str(),
                    "status:" | "status:local" | "status:repository" | "status:unchecked"
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
        if !self.active() {
            return (false, false);
        }
        match crate::tui::widgets::popup_mouse(m, self.rect) {
            crate::tui::widgets::PopupMouse::OutsidePress => {
                self.close();
                return (true, false);
            }
            crate::tui::widgets::PopupMouse::Inside => {}
            _ => return (true, false),
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
        f.render_widget(PopupSurface, self.rect);
        let block = ctx.settings.theme.block(" filters ", true);
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
                    ctx.settings.theme.selected()
                } else {
                    ctx.settings.theme.dim()
                })
            })
            .collect();
        f.render_widget(Paragraph::new(lines), inner);
    }
}

fn token_range(text: &str, cursor: usize) -> Range<usize> {
    skills::search::query_token_ranges(text)
        .into_iter()
        .find(|range| range.start <= cursor && cursor <= range.end)
        .unwrap_or(cursor..cursor)
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
                ["repo:", "tag:", "preset:", "agent:", "status:"]
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
            )
        };
    let query = if prefix == "repo" {
        skills::search::source_query_value(query)
    } else {
        query.to_owned()
    };
    let mut ranked: Vec<_> = choices
        .into_iter()
        .filter(|value| prefix == "repo" || !value.chars().any(char::is_whitespace))
        .filter_map(|value| score(&query, &value).map(|score| (score, value)))
        .collect();
    ranked.sort();
    ranked
        .into_iter()
        .map(|(_, value)| {
            if prefix.is_empty() {
                value
            } else if prefix == "repo" {
                skills::search::source_query_token(&value)
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
    fn library_completion_keeps_browsable_unchecked_status() {
        let mut popup = Completion {
            choices: [
                "status:local",
                "status:repository",
                "status:unchecked",
                "status:modified",
                "status:invalid",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            ..Default::default()
        };
        popup.healthy_only();
        assert_eq!(
            popup.choices,
            ["status:local", "status:repository", "status:unchecked"]
        );
    }

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
    fn source_name_completion_quotes_spaces_and_replaces_the_whole_value() {
        let values = ["Merlin Skills".into(), "Other Package".into()]
            .into_iter()
            .collect();
        assert_eq!(candidates("repo:merl", values), ["repo:\"Merlin Skills\""]);
        let mut input = Input::with_value("tag:work repo:\"Merlin Sk\"");
        let range = token_range(input.value(), input.cursor_byte());
        assert_eq!(&input.value()[range.clone()], "repo:\"Merlin Sk\"");
        let mut popup = Completion {
            token: range,
            choices: vec!["repo:\"Merlin Skills\"".into()],
            ..Default::default()
        };
        popup.accept(&mut input);
        assert_eq!(input.value(), "tag:work repo:\"Merlin Skills\" ");
        let partial = "repo:\"Merlin Sk";
        assert_eq!(token_range(partial, partial.len()), 0..partial.len());
        assert_eq!(
            candidates(partial, ["Merlin Skills".into()].into_iter().collect()),
            ["repo:\"Merlin Skills\""]
        );
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
    fn outside_mouse_buttons_close_completion_without_accepting() {
        for button in [MouseButton::Left, MouseButton::Right, MouseButton::Middle] {
            let mut input = Input::with_value("tag:");
            let mut popup = Completion {
                token: 0..4,
                choices: vec!["tag:alpha".into()],
                rect: Rect::new(3, 3, 20, 4),
                ..Default::default()
            };
            let outside = MouseEvent {
                kind: MouseEventKind::Down(button),
                column: 0,
                row: 0,
                modifiers: crossterm::event::KeyModifiers::NONE,
            };
            assert_eq!(popup.mouse(outside, &mut input), (true, false));
            assert!(!popup.active());
            assert_eq!(input.value(), "tag:");
            assert_eq!(popup.mouse(outside, &mut input), (false, false));
        }
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
            popup.mouse(event(MouseEventKind::ScrollDown, 0), &mut input),
            (true, false)
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
