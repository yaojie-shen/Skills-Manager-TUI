//! Tags tab: the tags themselves, and a look at what carries each one.
//!
//! Each pane filters its own collection. A tag can be edited directly: renamed, merged into another,
//! deleted, given a colour. The left column uses shared group cards; the right
//! pane is the skills under the selected tag, as the cards the search and
//! presets pages use, so a skill reads the same wherever it turns up.

use super::{SplitFocus, View, wheel};
use crate::tui::app::{Action, Ctx, Hints};
use crate::tui::components::context_menu::{Command, Item, Request, Target};
use crate::tui::components::group::{self, tag_fill};
use crate::tui::components::group_prompt::{Ask, Prompt};
use crate::tui::components::layout::frame;
use crate::tui::components::layout::split_panes;
use crate::tui::modal::Modal;
use crate::tui::settings::LayoutScope;
use crate::tui::widgets::{CardGrid, ScrollTrack};
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::widgets::Paragraph;
use skills::config::Config;
use skills::history;
use skills::ops::edit;
use skills::reconcile::Snapshot;

pub const UNTAGGED: &str = "(untagged)";

#[derive(Default)]
pub struct TagsView {
    /// Every tag with how many skills carry it, `(untagged)` last.
    rows: Vec<(String, usize)>,
    all_rows: Vec<(String, usize)>,
    filter: super::filter::Filter,
    skill_search: Option<super::search::SearchView>,
    /// Skills under the selected tag, in snapshot order.
    members: Vec<String>,
    members_tag: Option<String>,
    list: CardGrid,
    focus: SplitFocus,
    left: Rect,
    right: Rect,
    list_track: ScrollTrack,
    list_drag: bool,
    /// The merge target or colour being chosen, over the page.
    prompt: Option<Prompt>,
}

impl TagsView {
    /// Route pasted text to the same control that currently owns keyboard input.
    pub fn paste(&mut self, text: &str, ctx: &Ctx) -> Vec<Action> {
        if self.filter.editing {
            let actions = self.filter.paste(text);
            self.refilter(ctx);
            return actions;
        }
        if self.focus == SplitFocus::Members
            && let Some(view) = self.skill_search.as_mut()
        {
            return view.paste(text, ctx);
        }
        let Some(prompt) = self.prompt.as_mut() else {
            return vec![];
        };
        prompt.paste(text)
    }

    pub fn input_focused(&self) -> bool {
        self.prompt.is_some()
            || self.filter.editing
            || (self.focus == SplitFocus::Members
                && self
                    .skill_search
                    .as_ref()
                    .is_some_and(|v| v.input_focused()))
    }

    pub fn batch_finished(&mut self, failed: &[String]) {
        if let Some(view) = self.skill_search.as_mut() {
            view.batch_finished(failed);
        }
    }

    pub fn dialog_open(&self) -> bool {
        self.prompt.is_some()
    }

    fn refilter(&mut self, ctx: &Ctx) {
        let selected = self.selected_tag().map(str::to_owned);
        let documents = self
            .all_rows
            .iter()
            .map(|(name, _)| skills::search::TextDocument {
                name: name.clone(),
                description: ctx
                    .ws
                    .config
                    .tags
                    .iter()
                    .find(|t| &t.name == name)
                    .and_then(|t| t.description.clone())
                    .unwrap_or_default(),
                body: String::new(),
            })
            .collect::<Vec<_>>();
        self.rows = self
            .filter
            .rank(&documents, ctx)
            .into_iter()
            .map(|i| self.all_rows[i].clone())
            .collect();
        self.list
            .select(selected.and_then(|tag| self.rows.iter().position(|r| r.0 == tag)));
        self.list.clamp(self.rows.len());
        self.sync_members(ctx.snap);
    }

    pub fn select(&mut self, name: &str, snap: &Snapshot) {
        self.filter = super::filter::Filter::default();
        self.rows = self.all_rows.clone();
        self.list
            .select(self.rows.iter().position(|(tag, _)| tag == name));
        self.skill_search = None;
        self.focus = SplitFocus::Groups;
        self.sync_members(snap);
    }

    fn add_members(&self, ctx: &Ctx) -> Vec<Action> {
        match self.actionable_tag() {
            Some(tag) => vec![Action::OpenModal(Box::new(Modal::PresetSkills(Box::new(
                super::search::SearchView::tag_members(&tag, ctx),
            ))))],
            None => vec![],
        }
    }

    fn remove_members(&self, keys: Vec<String>) -> Vec<Action> {
        let Some(tag) = self.actionable_tag() else {
            return vec![];
        };
        vec![Action::WriteMeta(Box::new(move |ws| {
            history::tag_edit(ws, |ws| {
                Config::edit_tags(&ws.root, |tags| {
                    if let Some(group) = tags.iter_mut().find(|t| t.name == tag) {
                        group.skills.retain(|key| !keys.contains(key));
                    }
                })?;
                Ok(format!("removed {} skill(s) from {tag}", keys.len()))
            })
        }))]
    }

    fn ensure_skill_search(&mut self, ctx: &Ctx) {
        if self.skill_search.is_none() {
            let focused = self.focus == SplitFocus::Members;
            self.search_members(ctx);
            self.skill_search.as_mut().unwrap().focus_list();
            self.focus = if focused {
                SplitFocus::Members
            } else {
                SplitFocus::Groups
            };
        }
    }

    fn search_members(&mut self, ctx: &Ctx) {
        self.skill_search = Some(super::search::SearchView::panel(
            super::search::SkillPanelOptions {
                keys: self.members.clone(),
                title: format!("Tag: {}", self.selected_tag().unwrap_or("none")),
                layout_scope: LayoutScope::Tags,
                hidden_group: self.actionable_tag().map(|tag| (group::Kind::Tag, tag)),
            },
            ctx,
        ));
        self.focus = SplitFocus::Members;
    }

    fn selected_tag(&self) -> Option<&str> {
        self.list
            .selected()
            .and_then(|i| self.rows.get(i))
            .map(|(t, _)| t.as_str())
    }

    /// A real tag under the cursor, which the untagged row is not.
    fn actionable_tag(&self) -> Option<String> {
        self.selected_tag()
            .filter(|t| *t != UNTAGGED)
            .map(str::to_string)
    }

    /// Recompute the right pane for the tag under the cursor. Called after
    /// every move of the left one, so the two never disagree.
    fn sync_members(&mut self, snap: &Snapshot) {
        let old_members = self.members.clone();
        let tag = self.selected_tag().map(str::to_string);
        self.members = match tag.as_deref() {
            Some(UNTAGGED) => snap
                .skills
                .iter()
                .filter(|s| s.tags.is_empty() && s.status.is_present())
                .map(|s| s.key.clone())
                .collect(),
            Some(t) => snap
                .skills
                .iter()
                .filter(|s| s.tags.iter().any(|x| x == t))
                .map(|s| s.key.clone())
                .collect(),
            None => Vec::new(),
        };
        if old_members != self.members || self.members_tag != tag {
            self.skill_search = None;
        }
        self.members_tag = tag;
    }

    fn ask_merge(&mut self) -> Vec<Action> {
        let Some(tag) = self.actionable_tag() else {
            return vec![];
        };
        let others: Vec<String> = self
            .rows
            .iter()
            .map(|(t, _)| t.clone())
            .filter(|t| t != &tag && t != UNTAGGED)
            .collect();
        if others.is_empty() {
            return vec![Action::Error("no other tag to merge into".into())];
        }
        self.prompt = Some(Prompt::for_merge(&tag, others));
        vec![]
    }

    fn ask_color(&mut self, ctx: &Ctx) -> Vec<Action> {
        let Some(tag) = self.actionable_tag() else {
            return vec![];
        };
        let config = &ctx.ws.config;
        let current = config
            .tags
            .iter()
            .find(|t| t.name == tag)
            .and_then(|t| t.color.as_deref());
        self.prompt = Some(Prompt::for_color(
            group::Kind::Tag,
            &tag,
            current,
            &ctx.settings.theme,
        ));
        vec![]
    }

    /// Carry out what the prompt has settled on and put it away.
    fn submit_prompt(&mut self) -> Vec<Action> {
        let Some(p) = self.prompt.as_ref() else {
            return vec![];
        };
        let tag = p.name().to_string();
        match p.kind() {
            Ask::Merge => {
                let Some(into) = p.chosen().map(str::to_string) else {
                    return vec![Action::Error("pick a tag to merge into".into())];
                };
                self.prompt = None;
                vec![Action::WriteMeta(Box::new(move |ws| {
                    history::tag_edit(ws, |ws| {
                        edit::tag_rename(ws, &tag, &into)
                            .map(|n| format!("merged {tag} into {into} on {n} skill(s)"))
                    })
                }))]
            }
            Ask::Color => {
                let Some(color) = p.color_text() else {
                    return vec![Action::Error(format!(
                        "{:?} is not a colour: use a name or #rrggbb",
                        p.value().trim()
                    ))];
                };
                self.prompt = None;
                // Tag colour changes are not recorded in session undo.
                vec![Action::Write(Box::new(move |ws| {
                    Config::set_tag_color(&ws.root, &tag, color.as_deref())?;
                    Ok(match color {
                        Some(c) => format!("{tag} is now {c}"),
                        None => format!("{tag} is back to the default colour"),
                    })
                }))]
            }
        }
    }

    fn prompt_key(&mut self, k: KeyEvent) -> Vec<Action> {
        match self.prompt.as_mut().and_then(|p| p.key(k)) {
            Some(true) => return self.submit_prompt(),
            Some(false) => self.prompt = None,
            None => {}
        }
        vec![]
    }

    fn prompt_mouse(&mut self, m: MouseEvent) -> Vec<Action> {
        match self.prompt.as_mut().and_then(|p| p.mouse(m)) {
            Some(true) => return self.submit_prompt(),
            Some(false) => self.prompt = None,
            None => {}
        }
        vec![]
    }

    fn draw_tags(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        let th = &ctx.settings.theme;
        let focused = self.focus == SplitFocus::Groups && self.prompt.is_none();
        let inner = area;
        let content = Rect {
            width: inner.width.saturating_sub(1),
            ..inner
        };
        let descriptions: Vec<_> = self
            .rows
            .iter()
            .map(|(tag, _)| {
                ctx.ws
                    .config
                    .tags
                    .iter()
                    .find(|t| &t.name == tag)
                    .and_then(|t| t.description.as_deref())
            })
            .collect();
        self.list.layout_heights(
            content,
            descriptions
                .iter()
                .map(|description| group::card_height(*description))
                .collect(),
        );
        let selected = self.list.selected();
        for i in self.list.visible() {
            let Some(cell) = self.list.cell(i) else {
                continue;
            };
            let (tag, count) = &self.rows[i];
            let on = selected == Some(i);
            let inner = if cell.height < 3 {
                cell
            } else {
                frame(f, cell, on, focused && !self.filter.editing, th)
            };
            let lines = group::sidebar_card(
                tag,
                *count,
                descriptions[i],
                tag_fill(tag, ctx),
                inner.width as usize,
                ctx,
            );
            f.render_widget(Paragraph::new(lines), inner);
        }
        group::draw_track(f, inner, &self.list, selected, &mut self.list_track, th);
    }

    fn draw_prompt(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        if let Some(p) = self.prompt.as_mut() {
            p.draw(f, area, ctx);
        }
    }
}

impl View for TagsView {
    fn overlay_open(&self) -> bool {
        self.prompt.is_some()
            || (self.focus == SplitFocus::Members
                && self.skill_search.as_ref().is_some_and(View::overlay_open))
    }
    fn handle_control_key(&mut self, key: KeyEvent, ctx: &Ctx) -> Vec<Action> {
        if self.focus == SplitFocus::Members
            && let Some(view) = self.skill_search.as_mut()
        {
            return view.handle_control_key(key, ctx);
        }
        vec![]
    }
    fn actions_menu(&self, ctx: &Ctx) -> Option<Request> {
        if self.prompt.is_some() || self.filter.editing {
            return None;
        }
        if self.focus == SplitFocus::Members {
            let mut request = self.skill_search.as_ref()?.actions_menu(ctx)?;
            request.items.retain(|item| item.command != Command::Accept);
            for item in &mut request.items {
                if item.command == Command::Remove {
                    item.label = "Remove from tag".into();
                }
            }
            if let Target::Batch { all, .. } = &request.target {
                request.items.push(Item::new(
                    Command::Remove,
                    format!("Remove from tag · {} skills", all.len()),
                    KeyCode::Char('x'),
                    !all.is_empty(),
                    "No selected skills",
                    1,
                ));
            }
            return Some(request);
        }
        let tag = self.selected_tag()?.to_string();
        let editable = tag != UNTAGGED;
        Some(Request {
            title: tag.clone(),
            detail: "Tag".into(),
            target: Target::Tag(tag),
            items: vec![
                Item::new(
                    Command::EditMembers,
                    "Edit skills",
                    KeyCode::Char('e'),
                    true,
                    "",
                    0,
                ),
                Item::new(
                    Command::Description,
                    "Edit description",
                    KeyCode::Char('d'),
                    editable,
                    "The untagged group has no metadata",
                    1,
                ),
                Item::new(
                    Command::Rename,
                    "Rename tag",
                    KeyCode::Char('r'),
                    editable,
                    "The untagged group cannot be renamed",
                    1,
                ),
                Item::new(
                    Command::Merge,
                    "Merge into another tag",
                    KeyCode::Char('m'),
                    editable,
                    "The untagged group cannot be merged",
                    1,
                ),
                Item::new(
                    Command::Color,
                    "Change colour",
                    KeyCode::Char('c'),
                    editable,
                    "The untagged group has no colour",
                    1,
                ),
                Item::new(
                    Command::Remove,
                    "Delete tag",
                    KeyCode::Char('D'),
                    editable,
                    "The untagged group cannot be deleted",
                    2,
                ),
            ],
        })
    }
    fn context_menu(&mut self, x: u16, y: u16, ctx: &Ctx) -> Option<Request> {
        if self.prompt.is_some() {
            return None;
        }
        let view = self.skill_search.as_mut()?;
        let mut request = view.context_menu(x, y, ctx)?;
        request.items.retain(|item| item.command != Command::Accept);
        for item in &mut request.items {
            if item.command == Command::Remove {
                item.label = "Remove from tag".into();
            }
        }
        self.focus = SplitFocus::Members;
        self.filter.editing = false;
        Some(request)
    }
    fn context_execute(&mut self, target: &Target, command: Command, ctx: &Ctx) -> Vec<Action> {
        if let Target::Tag(tag) = target {
            if self.selected_tag() != Some(tag.as_str()) {
                return vec![Action::Error(
                    "Target changed; reopen the actions menu".into(),
                )];
            }
            return match command {
                Command::EditMembers => self.add_members(ctx),
                Command::Description => {
                    let description = ctx
                        .ws
                        .config
                        .tags
                        .iter()
                        .find(|item| item.name == *tag)
                        .and_then(|item| item.description.as_deref());
                    vec![Action::OpenModal(Box::new(Modal::tag_description(
                        tag,
                        description,
                    )))]
                }
                Command::Rename => vec![Action::OpenModal(Box::new(Modal::rename_tag(tag)))],
                Command::Merge => self.ask_merge(),
                Command::Color => self.ask_color(ctx),
                Command::Remove => vec![Action::OpenModal(Box::new(Modal::delete_tag(tag)))],
                _ => vec![],
            };
        }
        if command == Command::Remove {
            if let Target::Batch { all, .. } = target
                && self
                    .actions_menu(ctx)
                    .is_some_and(|request| request.target == *target)
            {
                return self.remove_members(all.clone());
            }
            if let Target::Skill(key) = target
                && ctx.snap.get(key).is_some()
            {
                return self.remove_members(vec![key.clone()]);
            }
            return vec![Action::Error(
                "Target changed; reopen the context menu".into(),
            )];
        }
        match self.skill_search.as_mut() {
            Some(view) => view.context_execute(target, command, ctx),
            None => vec![Action::Error(
                "Target changed; reopen the context menu".into(),
            )],
        }
    }

    fn focus_from_above(&mut self) {
        self.focus = SplitFocus::Filter;
        self.filter.editing = true;
    }

    fn focus_root(&mut self) {
        self.focus = SplitFocus::Groups;
        self.filter.editing = false;
    }

    fn status(&self, ctx: &Ctx) -> String {
        self.skill_search
            .as_ref()
            .map_or_else(String::new, |v| v.status(ctx))
    }
    fn refresh(&mut self, ctx: &Ctx) {
        let selected = self.selected_tag().map(str::to_owned);
        let panel = self.skill_search.take();
        let mut counts = ctx.snap.all_tags();
        for tag in &ctx.ws.config.tags {
            counts.entry(tag.name.clone()).or_insert(0);
        }
        self.rows = counts.into_iter().collect();
        let untagged = ctx
            .snap
            .skills
            .iter()
            .filter(|s| s.tags.is_empty() && s.status.is_present())
            .count();
        self.rows.push((UNTAGGED.into(), untagged));
        self.all_rows = self.rows.clone();
        self.refilter(ctx);
        self.list.select(
            selected
                .as_ref()
                .and_then(|tag| self.rows.iter().position(|r| &r.0 == tag)),
        );
        self.list.clamp(self.rows.len());
        self.sync_members(ctx.snap);
        if self.selected_tag() == selected.as_deref()
            && let Some(mut view) = panel
        {
            view.update_panel(self.members.clone(), ctx);
            self.skill_search = Some(view);
        }
    }

    fn handle_key(&mut self, k: KeyEvent, ctx: &Ctx) -> Vec<Action> {
        self.ensure_skill_search(ctx);
        if self.prompt.is_some() {
            return self.prompt_key(k);
        }
        if self.focus != SplitFocus::Members
            && self.filter.editing
            && k.code == KeyCode::Right
            && self.filter.input.cursor_byte() == self.filter.input.value().len()
        {
            if let Some(view) = self.skill_search.as_mut() {
                self.filter.editing = false;
                self.focus = SplitFocus::Members;
                view.focus_input();
            }
            return vec![];
        }
        if self.focus == SplitFocus::Members
            && let Some(view) = self.skill_search.as_mut()
        {
            if k.code == KeyCode::Left && view.input_at_left_edge() {
                view.close_input_completion();
                self.focus = SplitFocus::Filter;
                self.filter.editing = true;
                return vec![];
            }
            if k.code == KeyCode::Left && view.panel_back() {
                self.focus = SplitFocus::Groups;
                self.filter.editing = false;
                return vec![];
            }
            if view.panel_actions_ready() && k.modifiers.is_empty() {
                if matches!(k.code, KeyCode::Char('x') | KeyCode::Delete) {
                    let keys = view.panel_keys(ctx);
                    return self.remove_members(keys);
                }
                if k.code == KeyCode::Char('a') {
                    return self.add_members(ctx);
                }
            }
            let mut actions = view.handle_key(k, ctx);
            if k.code == KeyCode::Up {
                return actions;
            }
            if actions.iter().any(|a| matches!(a, Action::BackToParent)) {
                self.focus = SplitFocus::Groups;
                self.filter.editing = false;
                actions.retain(|a| !matches!(a, Action::BackToParent));
            }
            return actions;
        }
        if self.focus != SplitFocus::Members && self.filter.editing && k.code == KeyCode::Up {
            self.filter.editing = false;
            return vec![Action::BackToParent];
        }
        if self.focus != SplitFocus::Members && self.filter.key(k) {
            self.focus = if self.filter.editing {
                SplitFocus::Filter
            } else {
                SplitFocus::Groups
            };
            self.refilter(ctx);
            return vec![];
        }
        let n = self.rows.len();
        match k.code {
            KeyCode::Char('q') => vec![Action::BackToParent],
            KeyCode::Esc => vec![Action::BackToParent],
            KeyCode::Down | KeyCode::Char('j') => {
                self.list.move_by(1, n);
                self.sync_members(ctx.snap);
                vec![]
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if self.list.selected().unwrap_or(0) == 0 {
                    self.focus = SplitFocus::Filter;
                    self.filter.editing = true;
                    return vec![];
                }
                self.list.move_by(-1, n);
                self.sync_members(ctx.snap);
                vec![]
            }
            KeyCode::Home | KeyCode::Char('g') => {
                self.list.first(n);
                self.sync_members(ctx.snap);
                vec![]
            }
            KeyCode::End | KeyCode::Char('G') => {
                self.list.last(n);
                self.sync_members(ctx.snap);
                vec![]
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                self.focus = SplitFocus::Members;
                if let Some(view) = self.skill_search.as_mut() {
                    view.focus_list();
                }
                vec![]
            }
            KeyCode::Char('r') => match self.actionable_tag() {
                Some(t) => vec![Action::OpenModal(Box::new(Modal::rename_tag(&t)))],
                None => vec![],
            },
            KeyCode::Char('m') => self.ask_merge(),
            KeyCode::Char('c') => vec![Action::OpenModal(Box::new(Modal::new_tag()))],
            KeyCode::Char('a') => self.add_members(ctx),
            KeyCode::Char('e') => match self.actionable_tag() {
                Some(tag) => {
                    let description = ctx
                        .ws
                        .config
                        .tags
                        .iter()
                        .find(|t| t.name == tag)
                        .and_then(|t| t.description.as_deref());
                    vec![Action::OpenModal(Box::new(Modal::tag_description(
                        &tag,
                        description,
                    )))]
                }
                None => vec![],
            },
            KeyCode::Char('C') => self.ask_color(ctx),
            KeyCode::Char('D') => match self.actionable_tag() {
                Some(t) => vec![Action::OpenModal(Box::new(Modal::delete_tag(&t)))],
                None => vec![],
            },
            _ => vec![],
        }
    }

    fn handle_mouse(&mut self, m: MouseEvent, ctx: &Ctx) -> Vec<Action> {
        if self.prompt.is_some() {
            return self.prompt_mouse(m);
        }
        self.ensure_skill_search(ctx);
        let at = (m.column, m.row).into();
        let pressing = m.kind == MouseEventKind::Down(MouseButton::Left);
        let dragging = m.kind == MouseEventKind::Drag(MouseButton::Left);
        if pressing && self.filter.click_input(m.column, m.row) {
            self.focus = SplitFocus::Filter;
            self.filter.editing = true;
            return vec![];
        }
        if self.right.contains(at) {
            if pressing || wheel(&m, ctx).is_some() {
                self.focus = SplitFocus::Members;
                self.filter.editing = false;
            }
            return self.skill_search.as_mut().unwrap().handle_mouse(m, ctx);
        }
        if let Some(d) = wheel(&m, ctx) {
            if self.left.contains(at) {
                self.focus = SplitFocus::Groups;
                self.filter.editing = false;
                self.list.move_by(d.signum(), self.rows.len());
                self.sync_members(ctx.snap);
            }
            return vec![];
        }
        if (pressing && self.list_track.hit(m.column, m.row)) || (dragging && self.list_drag) {
            self.list_drag = true;
            self.focus = SplitFocus::Groups;
            self.filter.editing = false;
            if let Some(row) = self.list_track.index_at(m.row, self.list.grid_rows()) {
                self.list.select_row(row);
                self.sync_members(ctx.snap);
            }
            return vec![];
        }
        if !dragging {
            self.list_drag = false;
        }
        if pressing && self.left.contains(at) {
            self.focus = SplitFocus::Groups;
            self.filter.editing = false;
            if let Some((_, double)) = self.list.click(m.column, m.row) {
                self.sync_members(ctx.snap);
                if double {
                    self.focus = SplitFocus::Members;
                    if let Some(view) = self.skill_search.as_mut() {
                        view.focus_list();
                    }
                }
            }
        }
        vec![]
    }

    fn draw(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        self.ensure_skill_search(ctx);
        let (left, right) = split_panes(area, 38, ctx);
        self.left = left;
        self.right = right;
        let content = self.filter.draw(
            f,
            left,
            "Filter tags",
            "tags",
            self.focus == SplitFocus::Groups,
            ctx,
        );
        self.draw_tags(f, content, ctx);
        if let Some(view) = self.skill_search.as_mut() {
            view.set_panel_active(self.focus == SplitFocus::Members);
            view.draw(f, right, ctx);
        }
        self.draw_prompt(f, area, ctx);
    }

    fn hints(&self) -> Hints {
        if self.filter.editing {
            return &[("Enter/↓", "tags"), ("Esc", "clear filter")];
        }
        if self.focus == SplitFocus::Members
            && let Some(view) = self.skill_search.as_ref()
        {
            return view.hints();
        }
        match self.prompt.as_ref().map(Prompt::kind) {
            Some(Ask::Merge) => &[("↑/↓", "target"), ("Enter", "merge"), ("Esc", "cancel")],
            Some(Ask::Color) => &[
                ("type", "a name or #rrggbb"),
                ("Enter", "apply"),
                ("Esc", "cancel"),
            ],
            None if self.focus == SplitFocus::Members => &[
                ("/", "filter skills"),
                ("Enter", "preview"),
                ("a", "actions"),
                ("x", "remove from tag"),
                ("t", "edit tags"),
                ("m", "multi-select"),
                ("←/Esc", "tags"),
            ],
            None => &[
                ("↑↓/j/k", "select"),
                ("a", "actions"),
                ("c", "create"),
                ("D", "delete tag"),
                ("Enter/→", "skills"),
                ("/", "filter"),
                ("Esc/q", "back"),
            ],
        }
    }
}

#[cfg(test)]
mod snapshot_tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};
    use skills::config::TagConfig;

    fn context_request<V: crate::tui::views::View>(
        view: &mut V,
        ctx: &Ctx,
        width: u16,
        height: u16,
    ) -> Request {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| view.draw(f, f.area(), ctx)).unwrap();
        for y in 0..height {
            for x in 0..width {
                if let Some(request) = view.context_menu(x, y, ctx) {
                    return request;
                }
            }
        }
        panic!("expected a context menu target in the rendered member panel");
    }

    #[test]
    fn tags_draw_and_edit_from_the_supplied_configuration_snapshot() {
        let tmp = skills::ops::DownloadDir::new("tags-config-snapshot").unwrap();
        Config {
            agents: vec![],
            tags: vec![TagConfig {
                name: "sample".into(),
                skills: vec![],
                color: Some("blue".into()),
                description: Some("Original description".into()),
            }],
            ..Default::default()
        }
        .save(tmp.path())
        .unwrap();
        let mut ws = skills::Workspace::open(tmp.path()).unwrap();
        let snap = ws.scan().unwrap();
        let theme = crate::tui::theme::Theme::default();

        // A page must not silently acquire newer settings than other pages.
        Config::edit_tags(&ws.root, |tags| {
            tags[0].color = Some("green".into());
            tags[0].description = Some("Updated description".into());
        })
        .unwrap();
        let mut view = TagsView::default();
        for (expected_color, expected_description) in [
            ("blue", "Original description"),
            ("green", "Updated description"),
        ] {
            if expected_color == "green" {
                ws.config = ws.load_config().unwrap();
            }
            let ctx = Ctx {
                ws: &ws,
                snap: &snap,
                settings: &{
                    let mut settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
                    settings.theme = theme;
                    settings
                },
            };
            view.refresh(&ctx);
            view.select("sample", &snap);
            let mut term =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 24)).unwrap();
            term.draw(|f| view.draw(f, f.area(), &ctx)).unwrap();
            let text: String = term
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(text.contains(expected_description));
            view.ask_color(&ctx);
            assert_eq!(
                view.prompt.as_ref().unwrap().color_text(),
                Some(Some(expected_color.into()))
            );
            view.prompt = None;
        }
    }

    #[test]
    fn tags_context_menu_uses_shared_member_actions_and_tag_specific_remove_label() {
        let tmp = skills::ops::DownloadDir::new("tags-context-menu").unwrap();
        let root = tmp.path();
        Config {
            agents: vec![],
            tags: vec![TagConfig {
                name: "work".into(),
                skills: vec!["sample".into()],
                color: None,
                description: None,
            }],
            ..Default::default()
        }
        .save(root)
        .unwrap();
        std::fs::create_dir_all(root.join("sample")).unwrap();
        std::fs::write(
            root.join("sample/SKILL.md"),
            "---\nname: sample\ndescription: Sample skill\n---\nBody",
        )
        .unwrap();
        let ws = skills::Workspace::open(root).unwrap();
        let snap = ws.scan().unwrap();
        let settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &settings,
        };
        let mut view = TagsView::default();
        view.refresh(&ctx);
        view.select("work", &snap);

        let request = context_request(&mut view, &ctx, 120, 30);
        assert_eq!(request.target, Target::Skill("sample".into()));
        assert!(
            !request
                .items
                .iter()
                .any(|item| item.command == Command::Accept)
        );
        assert_eq!(
            request
                .items
                .iter()
                .find(|item| item.command == Command::Remove)
                .map(|item| item.label.as_str()),
            Some("Remove from tag")
        );
        assert!(request.items.iter().any(|item| item.disabled.is_some()));
        assert!(matches!(
            view.context_execute(&request.target, Command::Remove, &ctx)
                .as_slice(),
            [Action::WriteMeta(_)]
        ));
        view.focus = SplitFocus::Groups;
        let root_actions = view.actions_menu(&ctx).unwrap();
        assert_eq!(
            root_actions
                .items
                .iter()
                .find(|item| item.command == Command::Remove)
                .map(|item| item.shortcut),
            Some(KeyCode::Char('D'))
        );
    }
}
