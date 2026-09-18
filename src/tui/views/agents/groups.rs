//! Compact preset and tag coverage for the selected scope.

use super::*;

fn emphasize_body(spans: &mut [Span<'static>], modifier: ratatui::style::Modifier) {
    let index = usize::from(spans.len() == 3);
    if let Some(body) = spans.get_mut(index) {
        body.style = body.style.add_modifier(modifier);
    }
}

#[derive(Clone, Copy)]
enum GroupIndex {
    Preset(usize),
    Tag(usize),
}

impl AgentsView {
    pub(super) fn has_visible_groups(&self) -> bool {
        self.presets.iter().any(|(_, s)| s.installed > 0)
            || self.tags.iter().any(|t| t.included > 0)
    }

    pub(super) fn draw_groups(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        self.preset_rects.clear();
        self.tag_rects.clear();
        if area.is_empty() {
            return;
        }
        if self.presets.is_empty() && self.tags.is_empty() {
            f.render_widget(
                Paragraph::new("No presets or tags").style(ctx.settings.theme.dim()),
                area,
            );
            return;
        }

        let budget = area.width.saturating_sub(3) as usize;
        let mut identities = Vec::with_capacity(self.presets.len() + self.tags.len());
        let mut rendered = Vec::with_capacity(identities.capacity());
        for (i, (preset, status)) in self
            .presets
            .iter()
            .enumerate()
            .filter(|(_, (_, s))| s.installed > 0)
        {
            identities.push(GroupIndex::Preset(i));
            rendered.push(
                group::Badge {
                    kind: group::Kind::Preset,
                    name: &preset.name,
                    fill: group::preset_fill(preset, ctx),
                    coverage: Some((status.installed, status.total)),
                    selected: false,
                    focused: false,
                }
                .render(ctx, budget),
            );
        }
        for (i, tag) in self.tags.iter().enumerate().filter(|(_, t)| t.included > 0) {
            identities.push(GroupIndex::Tag(i));
            rendered.push(
                group::Badge {
                    coverage: Some((tag.included, tag.total)),
                    selected: false,
                    focused: false,
                    ..group::Badge::new(&tag.name, group::tag_fill(&tag.name, ctx))
                }
                .render(ctx, budget),
            );
        }

        self.filter_cursor = self.filter_cursor.min(identities.len().saturating_sub(1));
        let selected = self.filter_cursor;
        let widths: Vec<_> = rendered
            .iter()
            .map(|spans| spans.iter().map(Span::width).sum::<usize>() + 1)
            .collect();
        let visible = pill_window(&widths, selected, &mut self.preset_offset, budget);
        let mut spans = vec![Span::styled(
            if visible.start > 0 { "‹" } else { " " },
            ctx.settings.theme.dim(),
        )];
        let mut x = area.x + 1;
        for index in visible.clone() {
            let width = widths[index].saturating_sub(1) as u16;
            let rect = Rect::new(x, area.y, width, 1);
            match identities[index] {
                GroupIndex::Preset(i) => self.preset_rects.push((i, rect)),
                GroupIndex::Tag(i) => self.tag_rects.push((i, rect)),
            }
            let query = skills::search::Query::parse(self.search_panel.input.value());
            let active = match identities[index] {
                GroupIndex::Preset(i) => query
                    .presets
                    .iter()
                    .any(|n| n.eq_ignore_ascii_case(&self.presets[i].0.name)),
                GroupIndex::Tag(i) => query
                    .tags
                    .iter()
                    .any(|n| n.eq_ignore_ascii_case(&self.tags[i].name)),
            };
            let mut badge = rendered[index].clone();
            if active {
                emphasize_body(
                    &mut badge,
                    ratatui::style::Modifier::BOLD | ratatui::style::Modifier::UNDERLINED,
                );
            }
            if self.focus() == Focus::Filters && index == self.filter_cursor {
                emphasize_body(
                    &mut badge,
                    ratatui::style::Modifier::BOLD | ratatui::style::Modifier::UNDERLINED,
                );
            }
            spans.extend(badge);

            spans.push(Span::raw(" "));
            x += width + 1;
        }
        if visible.end < identities.len() {
            spans.push(Span::styled("›", ctx.settings.theme.dim()));
        }
        f.render_widget(Paragraph::new(Line::from(spans)), area);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct GroupKey {
    pub preset: bool,
    pub name: String,
}
#[derive(Default)]
pub(super) struct QuickGroups {
    rect: Rect,
    hits: Vec<(Rect, Vec<GroupKey>, bool)>,
    selected: usize,
    pub(super) popup: Option<Vec<GroupKey>>,
    popup_hits: Vec<(Rect, usize)>,
    popup_cursor: usize,
}

/// Keep other field filters and free text; group pills form one exclusive filter.
fn toggle_filter(input: &str, key: &GroupKey) -> String {
    let encoded = skills::search::source_query_token(&key.name);
    let token = format!(
        "{}:{}",
        if key.preset { "preset" } else { "tag" },
        &encoded[5..]
    );
    let tokens: Vec<_> = skills::search::query_token_ranges(input)
        .into_iter()
        .map(|r| &input[r])
        .collect();
    let query = skills::search::Query::parse(input);
    let active = if key.preset {
        &query.presets
    } else {
        &query.tags
    }
    .iter()
    .any(|name| name.eq_ignore_ascii_case(&key.name));
    let mut kept: Vec<_> = tokens
        .into_iter()
        .filter(|t| !t.starts_with("preset:") && !t.starts_with("tag:") && *t != "untagged")
        .map(str::to_owned)
        .collect();
    if !active {
        kept.push(token);
    }
    kept.join(" ")
}

/// Natural wrapping when everything fits; otherwise reserve one line per kind.
fn quick_rows(items: &[(GroupKey, usize)], width: usize) -> Vec<Vec<Vec<GroupKey>>> {
    if items.is_empty() || width == 0 {
        return vec![];
    }
    let mut rows = vec![vec![]];
    let mut used = 0;
    for (key, size) in items {
        let size = (*size).min(width);
        if used > 0 && used + 1 + size > width {
            rows.push(vec![]);
            used = 0;
        }
        rows.last_mut().unwrap().push(vec![key.clone()]);
        used += size + usize::from(used > 0);
    }
    if rows.len() <= 2 {
        return rows;
    }
    [true, false]
        .into_iter()
        .filter_map(|preset| {
            let group: Vec<_> = items.iter().filter(|(k, _)| k.preset == preset).collect();
            if group.is_empty() {
                return None;
            }
            let mut row = vec![];
            let mut used = 0;
            for (i, (key, size)) in group.iter().enumerate() {
                let remaining = group.len() - i - 1;
                let more = if remaining > 0 {
                    format!("+{remaining} {}", if preset { "presets" } else { "tags" }).len() + 1
                } else {
                    0
                };
                let gap = usize::from(used > 0);
                if used + gap + *size + more > width {
                    row.push(group[i..].iter().map(|(k, _)| (*k).clone()).collect());
                    break;
                }
                row.push(vec![(*key).clone()]);
                used += gap + *size;
            }
            Some(row)
        })
        .collect()
}

impl QuickGroups {
    pub(super) fn reset_for_scope(&mut self) {
        let rect = self.rect;
        *self = Self::default();
        self.rect = rect;
    }
}

impl AgentsView {
    fn group_badge(&self, key: &GroupKey, ctx: &Ctx, budget: usize) -> Vec<Span<'static>> {
        if key.preset {
            let Some((p, s)) = self.presets.iter().find(|(p, _)| p.name == key.name) else {
                return vec![];
            };
            group::Badge {
                kind: group::Kind::Preset,
                name: &p.name,
                fill: group::preset_fill(p, ctx),
                coverage: Some((s.installed, s.total)),
                selected: false,
                focused: false,
            }
            .render(ctx, budget)
        } else {
            let Some(t) = self.tags.iter().find(|t| t.name == key.name) else {
                return vec![];
            };
            group::Badge {
                coverage: Some((t.included, t.total)),
                ..group::Badge::new(&t.name, group::tag_fill(&t.name, ctx))
            }
            .render(ctx, budget)
        }
    }
    fn quick_items(&self, ctx: &Ctx, width: usize) -> Vec<(GroupKey, usize)> {
        self.presets
            .iter()
            .map(|(p, _)| GroupKey {
                preset: true,
                name: p.name.clone(),
            })
            .chain(self.tags.iter().map(|t| GroupKey {
                preset: false,
                name: t.name.clone(),
            }))
            .map(|k| {
                let size = self
                    .group_badge(&k, ctx, width)
                    .iter()
                    .map(Span::width)
                    .sum();
                (k, size)
            })
            .collect()
    }
    pub(super) fn quick_visible(&self) -> bool {
        self.quick.rect.height >= 3 && (!self.presets.is_empty() || !self.tags.is_empty())
    }
    pub(super) fn filter_keys(&self) -> Vec<GroupKey> {
        self.presets
            .iter()
            .filter(|(_, s)| s.installed > 0)
            .map(|(p, _)| GroupKey {
                preset: true,
                name: p.name.clone(),
            })
            .chain(
                self.tags
                    .iter()
                    .filter(|t| t.included > 0)
                    .map(|t| GroupKey {
                        preset: false,
                        name: t.name.clone(),
                    }),
            )
            .collect()
    }
    pub(super) fn focus_skill_search(&mut self) {
        self.set_focus(Focus::Entries);
        self.filter_editing = true;
        self.search_panel.completion.close();
    }
    pub(super) fn focus_skill_filters(&mut self) {
        self.set_focus(if self.has_visible_groups() {
            Focus::Filters
        } else {
            Focus::Entries
        });
        self.filter_editing = false;
    }
    pub(super) fn filter_key(&mut self, k: KeyEvent) -> Option<Vec<Action>> {
        if self.focus() != Focus::Filters {
            return None;
        }
        let keys = self.filter_keys();
        self.filter_cursor = self.filter_cursor.min(keys.len().saturating_sub(1));
        match k.code {
            KeyCode::Left => self.filter_cursor = self.filter_cursor.saturating_sub(1),
            KeyCode::Right => {
                self.filter_cursor = (self.filter_cursor + 1).min(keys.len().saturating_sub(1))
            }
            KeyCode::Home => self.filter_cursor = 0,
            KeyCode::End => self.filter_cursor = keys.len().saturating_sub(1),
            KeyCode::Up | KeyCode::Esc | KeyCode::Char('q' | '/') => self.focus_skill_search(),
            KeyCode::Down => self.set_focus(Focus::Entries),
            KeyCode::Enter | KeyCode::Char(' ') => {
                if let Some(key) = keys.get(self.filter_cursor) {
                    self.search_panel
                        .input
                        .set(&toggle_filter(self.search_panel.input.value(), key));
                    self.search_panel.completion.close();
                    self.entries.first(0);
                }
            }
            _ => {}
        }
        Some(vec![])
    }
    pub(super) fn quick_height(&self, ctx: &Ctx, width: u16) -> u16 {
        let rows = quick_rows(
            &self.quick_items(ctx, width.saturating_sub(4) as usize),
            width.saturating_sub(4) as usize,
        );
        if rows.is_empty() {
            0
        } else {
            rows.len() as u16 + 2
        }
    }
    pub(super) fn draw_quick(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        self.quick.rect = area;
        self.quick.hits.clear();
        if area.height < 3 {
            return;
        }
        let block = ctx
            .settings
            .theme
            .block(" Presets + Tags ", self.focus() == Focus::Groups);
        let inner = block.inner(area).inner(ratatui::layout::Margin {
            horizontal: 1,
            vertical: 0,
        });
        f.render_widget(block, area);
        let rows = quick_rows(
            &self.quick_items(ctx, inner.width as usize),
            inner.width as usize,
        );
        for (y, row) in rows.iter().take(inner.height as usize).enumerate() {
            let mut x = inner.x;
            for keys in row {
                let hidden = keys.len() > 1
                    || self
                        .group_badge(&keys[0], ctx, inner.width as usize)
                        .iter()
                        .map(Span::width)
                        .sum::<usize>()
                        > inner.right().saturating_sub(x) as usize;
                let mut spans = if hidden {
                    vec![Span::styled(
                        format!(
                            "+{} {}",
                            keys.len(),
                            if keys[0].preset { "presets" } else { "tags" }
                        ),
                        ctx.settings.theme.dim(),
                    )]
                } else {
                    self.group_badge(&keys[0], ctx, inner.width as usize)
                };
                if self.focus() == Focus::Groups && self.quick.selected == self.quick.hits.len() {
                    emphasize_body(
                        &mut spans,
                        ratatui::style::Modifier::BOLD | ratatui::style::Modifier::UNDERLINED,
                    );
                }
                let w = (spans.iter().map(Span::width).sum::<usize>() as u16)
                    .min(inner.right().saturating_sub(x));
                if hidden {
                    x = inner.right().saturating_sub(w);
                }
                let rect = Rect::new(x, inner.y + y as u16, w, 1);
                f.render_widget(Paragraph::new(Line::from(spans)), rect);
                self.quick.hits.push((rect, keys.clone(), hidden));
                x = x.saturating_add(w + 1);
            }
        }
        self.quick.selected = self
            .quick
            .selected
            .min(self.quick.hits.len().saturating_sub(1));
    }
    pub(super) fn toggle_group(&self, key: &GroupKey, ctx: &Ctx) -> Vec<Action> {
        let keys = if key.preset {
            self.presets
                .iter()
                .find(|(p, _)| p.name == key.name)
                .map(|(p, _)| p.members())
                .unwrap_or_default()
        } else {
            match skills::preset::tag_members(&ctx.ws.config, std::slice::from_ref(&key.name)) {
                Ok(k) => k,
                Err(e) => return vec![Action::Error(e.to_string())],
            }
        };
        if keys.is_empty() {
            return vec![];
        }
        let full = if key.preset {
            self.presets
                .iter()
                .find(|(p, _)| p.name == key.name)
                .is_some_and(|(_, s)| s.total > 0 && s.installed == s.total)
        } else {
            self.tags
                .iter()
                .find(|t| t.name == key.name)
                .is_some_and(|t| t.total > 0 && t.included == t.total)
        };
        let Some(agent) = ctx.ws.config.agent(&self.scope).cloned() else {
            return vec![];
        };
        let project = self.project();
        vec![Action::deployment(Action::BatchMeta(
            Box::new({
                let keys = keys.clone();
                move |ws| {
                    skills::ops::targets::set_deployed(ws, &agent, project.as_deref(), &keys, !full)
                }
            }),
            keys,
        ))]
    }
    fn activate_quick(&mut self, index: usize, ctx: &Ctx) -> Vec<Action> {
        let Some((_, keys, hidden)) = self.quick.hits.get(index).cloned() else {
            return vec![];
        };
        if hidden {
            self.quick.popup = Some(keys);
            self.quick.popup_cursor = 0;
            vec![]
        } else {
            self.toggle_group(&keys[0], ctx)
        }
    }
    pub(super) fn quick_key(&mut self, k: KeyEvent, ctx: &Ctx) -> Option<Vec<Action>> {
        if let Some(keys) = self.quick.popup.clone() {
            match k.code {
                KeyCode::Esc | KeyCode::Char('q') => self.quick.popup = None,
                KeyCode::Up => self.quick.popup_cursor = self.quick.popup_cursor.saturating_sub(1),
                KeyCode::Down => {
                    self.quick.popup_cursor = (self.quick.popup_cursor + 1).min(keys.len() - 1)
                }
                KeyCode::Enter => {
                    let key = keys[self.quick.popup_cursor].clone();
                    self.quick.popup = None;
                    return Some(self.toggle_group(&key, ctx));
                }
                _ => {}
            }
            return Some(vec![]);
        }
        if self.focus() != Focus::Groups {
            return None;
        }
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') => self.set_focus(if self.destinations.is_empty() {
                Focus::Agents
            } else {
                Focus::Scopes
            }),
            KeyCode::Up => {
                let i = self.quick.selected;
                let y = self.quick.hits.get(i).map(|(r, _, _)| r.y);
                if let Some(p) = self
                    .quick
                    .hits
                    .iter()
                    .enumerate()
                    .filter(|(_, (r, _, _))| Some(r.y) < y)
                    .min_by_key(|(_, (r, _, _))| r.x.abs_diff(self.quick.hits[i].0.x))
                    .map(|(i, _)| i)
                {
                    self.quick.selected = p
                } else {
                    self.set_focus(Focus::Scopes)
                }
            }
            KeyCode::Down => {
                let y = self
                    .quick
                    .hits
                    .get(self.quick.selected)
                    .map(|(r, _, _)| r.y);
                if let Some(p) = self
                    .quick
                    .hits
                    .iter()
                    .enumerate()
                    .filter(|(_, (r, _, _))| Some(r.y) > y)
                    .min_by_key(|(_, (r, _, _))| {
                        r.x.abs_diff(self.quick.hits[self.quick.selected].0.x)
                    })
                    .map(|(i, _)| i)
                {
                    self.quick.selected = p
                } else {
                    self.focus_skill_search();
                }
            }
            KeyCode::Char('/') => self.focus_skill_search(),
            KeyCode::Left => self.quick.selected = self.quick.selected.saturating_sub(1),
            KeyCode::Right => {
                self.quick.selected =
                    (self.quick.selected + 1).min(self.quick.hits.len().saturating_sub(1))
            }
            KeyCode::Enter => return Some(self.activate_quick(self.quick.selected, ctx)),
            _ => {}
        }
        Some(vec![])
    }
    pub(super) fn group_mouse(&mut self, m: MouseEvent, ctx: &Ctx) -> Option<Vec<Action>> {
        let at = (m.column, m.row).into();
        if self.quick.popup.is_some() {
            if let Some(delta) = wheel(&m, ctx) {
                let n = self.quick.popup.as_ref().unwrap().len();
                self.quick.popup_cursor = (self.quick.popup_cursor as i32 + delta)
                    .clamp(0, n.saturating_sub(1) as i32)
                    as usize;
                return Some(vec![]);
            }
            if m.kind == MouseEventKind::Down(MouseButton::Left)
                && let Some((_, i)) = self.quick.popup_hits.iter().find(|(r, _)| r.contains(at))
            {
                self.quick.popup_cursor = *i;
                return self.quick_key(KeyEvent::new(KeyCode::Enter, m.modifiers), ctx);
            }
            return Some(vec![]);
        }
        if m.kind != MouseEventKind::Down(MouseButton::Left) {
            return None;
        }
        if let Some(i) = self.quick.hits.iter().position(|(r, _, _)| r.contains(at)) {
            self.set_focus(Focus::Groups);
            self.quick.selected = i;
            return Some(self.activate_quick(i, ctx));
        }
        if self.quick.rect.contains(at) && self.quick_visible() {
            self.set_focus(Focus::Groups);
            return Some(vec![]);
        }
        let key = self
            .preset_rects
            .iter()
            .find(|(_, r)| r.contains(at))
            .map(|(i, _)| GroupKey {
                preset: true,
                name: self.presets[*i].0.name.clone(),
            })
            .or_else(|| {
                self.tag_rects
                    .iter()
                    .find(|(_, r)| r.contains(at))
                    .map(|(i, _)| GroupKey {
                        preset: false,
                        name: self.tags[*i].name.clone(),
                    })
            });
        if let Some(key) = key {
            self.search_panel
                .input
                .set(&toggle_filter(self.search_panel.input.value(), &key));
            self.search_panel.completion.close();
            self.filter_cursor = self
                .filter_keys()
                .iter()
                .position(|k| *k == key)
                .unwrap_or(0);
            self.set_focus(Focus::Filters);
            self.entries.first(0);
            return Some(vec![]);
        }
        None
    }
    pub(super) fn draw_group_popup(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        self.quick.popup_hits.clear();
        let Some(keys) = self.quick.popup.clone() else {
            return;
        };
        let w = area.width.min(64);
        let h = area.height.min(keys.len() as u16 + 3);
        let rect = Rect::new(
            area.x + (area.width - w) / 2,
            area.y + (area.height - h) / 2,
            w,
            h,
        );
        f.render_widget(crate::tui::widgets::OverlayClear, rect);
        let block = ctx
            .settings
            .theme
            .block(" Group deployment · Enter toggle · Esc back ", true);
        let inner = block.inner(rect);
        f.render_widget(block, rect);
        let count = inner.height as usize;
        let start = self
            .quick
            .popup_cursor
            .saturating_sub(count.saturating_sub(1));
        for (i, key) in keys.iter().enumerate().skip(start).take(count) {
            let r = Rect::new(inner.x, inner.y + (i - start) as u16, inner.width, 1);
            let mut spans = self.group_badge(key, ctx, r.width as usize);
            if i == self.quick.popup_cursor {
                emphasize_body(
                    &mut spans,
                    ratatui::style::Modifier::BOLD | ratatui::style::Modifier::UNDERLINED,
                );
            }
            f.render_widget(Paragraph::new(Line::from(spans)), r);
            self.quick.popup_hits.push((r, i));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key(preset: bool, name: &str) -> GroupKey {
        GroupKey {
            preset,
            name: name.into(),
        }
    }
    #[test]
    fn filters_toggle_exclusively_and_keep_text_and_other_fields() {
        let p = key(true, "global");
        let t = key(false, "lark");
        assert_eq!(
            toggle_filter(
                "hello 世界 tag:old preset:other repo:\"My Skills\" agent:codex",
                &p
            ),
            "hello 世界 repo:\"My Skills\" agent:codex preset:global"
        );
        assert_eq!(toggle_filter("hello preset:global", &p), "hello");
        assert_eq!(
            toggle_filter("hello preset:global tag:other untagged", &p),
            "hello"
        );
        assert_eq!(toggle_filter("hello preset:global", &t), "hello tag:lark");
    }
    #[test]
    fn quoted_group_names_keep_matching_and_toggle_as_one_condition() {
        let p = key(true, "My Tools");
        let result = toggle_filter("hello tag:old", &p);
        assert_eq!(skills::search::Query::parse(&result).presets, ["my tools"]);
        assert_eq!(toggle_filter(&result, &p), "hello");
        let t = key(false, "中文 工具");
        let result = toggle_filter(&result, &t);
        assert_eq!(skills::search::Query::parse(&result).tags, ["中文 工具"]);
        assert_eq!(toggle_filter(&result, &t), "hello");
    }
    #[test]
    fn group_rows_wrap_and_reserve_each_kind_when_overflowing() {
        let items = vec![
            (key(true, "p"), 8),
            (key(false, "t1"), 8),
            (key(false, "t2"), 8),
        ];
        let rows = quick_rows(&items, 18);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].len(), 2);
        assert_eq!(rows[1].len(), 1);
        let items: Vec<_> = (0..4)
            .map(|i| (key(true, &format!("p{i}")), 12))
            .chain((0..8).map(|i| (key(false, &format!("t{i}")), 10)))
            .collect();
        let rows = quick_rows(&items, 30);
        assert_eq!(rows.len(), 2);
        assert!(rows[0].iter().flatten().all(|k| k.preset));
        assert!(rows[1].iter().flatten().all(|k| !k.preset));
        assert_eq!(rows[0].last().unwrap().len(), 3);
        assert_eq!(rows[1].last().unwrap().len(), 6);
        assert_eq!(rows.iter().flatten().flatten().count(), items.len());
        for w in 0..60 {
            assert!(quick_rows(&items, w).len() <= 2);
        }
    }
}

#[cfg(test)]
mod interaction_tests {
    use super::*;
    use crossterm::event::KeyModifiers;
    use ratatui::{Terminal, backend::TestBackend};
    #[test]
    fn overflow_expands_without_installing_and_chosen_group_installs_with_counts() {
        let tmp = skills::ops::DownloadDir::new("quick-groups").unwrap();
        let root = tmp.path().join("library");
        let target = tmp.path().join("target");
        std::fs::create_dir_all(root.join("one")).unwrap();
        std::fs::write(
            root.join("one/SKILL.md"),
            "---\nname: one\ndescription: sample\n---\nbody",
        )
        .unwrap();
        let mut ws = skills::Workspace::open(&root).unwrap();
        ws.config.agents = vec![skills::config::AgentConfig {
            key: "sample".into(),
            name: "Sample".into(),
            skills_dir: target.display().to_string(),
        }];
        for i in 0..9 {
            ws.presets
                .save(&Preset {
                    name: format!("preset-{i}"),
                    skills: vec!["one".into()],
                    ..Default::default()
                })
                .unwrap();
        }
        let snap = ws.scan().unwrap();
        let settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &settings,
        };
        let mut view = AgentsView::default();
        view.refresh_current(&ctx);
        let mut term = Terminal::new(TestBackend::new(60, 24)).unwrap();
        term.draw(|f| view.draw_current(f, f.area(), &ctx)).unwrap();
        let text: String = term
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("0/1"));
        assert!(text.contains("presets"));
        assert!(
            view.preset_rects.is_empty(),
            "uninstalled groups appear only above"
        );
        let (rect, hidden, _) = view
            .quick
            .hits
            .iter()
            .find(|(_, _, hidden)| *hidden)
            .unwrap()
            .clone();
        let mouse = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: rect.x,
            row: rect.y,
            modifiers: KeyModifiers::NONE,
        };
        assert!(view.handle_mouse_current(mouse, &ctx).is_empty());
        assert_eq!(view.quick.popup.as_ref(), Some(&hidden));
        assert!(!target.join("one").exists());
        for (w, h) in [(60, 24), (8, 4), (1, 1)] {
            let mut small = Terminal::new(TestBackend::new(w, h)).unwrap();
            small
                .draw(|f| view.draw_group_popup(f, f.area(), &ctx))
                .unwrap();
        }
        view.quick_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &ctx);
        assert!(view.quick.popup.is_none());
        assert!(!target.join("one").exists());
        view.handle_mouse_current(mouse, &ctx);
        let actions = view
            .quick_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &ctx)
            .unwrap();
        let (_, Action::BatchMeta(write, keys)) = actions.into_iter().next().unwrap().into_scoped()
        else {
            panic!("install group")
        };
        assert_eq!(keys, ["one"]);
        write(&ws).unwrap();
        assert!(target.join("one").is_symlink());
        assert!(root.join("one/SKILL.md").is_file());
        ws.presets
            .save(&Preset {
                name: "empty".into(),
                ..Default::default()
            })
            .unwrap();
        ws.config.tags_enabled = true;
        ws.config.tags = vec![
            skills::config::TagConfig {
                name: "all".into(),
                skills: vec!["one".into()],
                color: None,
                description: None,
            },
            skills::config::TagConfig {
                name: "empty".into(),
                skills: vec![],
                color: None,
                description: None,
            },
        ];
        let snap = ws.scan().unwrap();
        let settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &settings,
        };
        view.refresh_current(&ctx);
        for preset in [true, false] {
            assert!(
                view.toggle_group(
                    &GroupKey {
                        preset,
                        name: "empty".into()
                    },
                    &ctx
                )
                .is_empty()
            );
        }
        let actions = view.toggle_group(
            &GroupKey {
                preset: false,
                name: "all".into(),
            },
            &ctx,
        );
        let (_, Action::BatchMeta(write, _)) = actions.into_iter().next().unwrap().into_scoped()
        else {
            panic!("uninstall full tag")
        };
        write(&ws).unwrap();
        assert!(!target.join("one").exists());
        assert!(root.join("one/SKILL.md").is_file());
    }
}
