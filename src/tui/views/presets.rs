//! Presets tab: what each preset holds, and adding to or taking from it.
//!
//! Turning a preset on or off is the Agents page's job, one agent at a time.
//! This page defines each package's fixed skill list. The
//! cards summarize each preset's purpose and members; deployment lives on Agents.

use super::matrix::Matrix;
use super::{View, wheel};
use crate::tui::app::{Action, Ctx, Hints};
use crate::tui::components::context_menu::{Command, Item, Request, Target};
use crate::tui::components::group;
use crate::tui::components::group_prompt::Prompt;
use crate::tui::components::layout::{frame, split_panes};
use crate::tui::modal::Modal;
use crate::tui::settings::LayoutScope;
use crate::tui::widgets::{CardGrid, ScrollTrack};
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use skills::history;
use skills::preset::{Preset, TagCoverage, tag_coverages};
use std::collections::BTreeSet;

#[derive(Default)]
pub struct PresetsView {
    presets: Vec<Preset>,
    tag_groups: Vec<TagCoverage>,
    tag_area: Rect,
    visible_members: Vec<String>,

    all_presets: Vec<Preset>,
    filter: super::filter::Filter,
    skill_search: Option<(String, super::search::SearchView)>,
    list: CardGrid,
    focus_members: bool,
    left: Rect,
    right: Rect,
    list_track: ScrollTrack,
    list_drag: bool,
    /// The whole preset × agent picture, over the page.
    matrix: Matrix,
    /// A preset to land on when the list next reloads, by name, because the
    /// list is sorted and a preset just created or renamed can appear
    /// anywhere in it.
    pending: Option<String>,
    color_prompt: Option<Prompt>,
}

impl PresetsView {
    fn finish_color(&mut self, result: Option<bool>) -> Vec<Action> {
        match result {
            Some(false) => self.color_prompt = None,
            Some(true) => {
                let Some(prompt) = self.color_prompt.as_ref() else {
                    return vec![];
                };
                let Some(color) = prompt.color_text() else {
                    return vec![Action::Error("Use a colour name or #rrggbb".into())];
                };
                let name = prompt.name().to_string();
                self.color_prompt = None;
                return vec![Action::Write(Box::new(move |ws| {
                    let mut preset = ws
                        .presets
                        .load(&name)?
                        .ok_or_else(|| anyhow::anyhow!("no such preset: {name}"))?;
                    preset.color = color;
                    ws.presets.save(&preset)?;
                    Ok(format!("Updated colour of {name}"))
                }))];
            }
            None => {}
        }
        vec![]
    }

    pub fn batch_finished(&mut self, failed: &[String]) {
        if let Some((_, view)) = self.skill_search.as_mut() {
            view.batch_finished(failed);
        }
    }

    pub fn input_focused(&self) -> bool {
        self.color_prompt.is_some()
            || self.filter.editing
            || (self.focus_members
                && self
                    .skill_search
                    .as_ref()
                    .is_some_and(|(_, v)| v.input_focused()))
    }
    pub fn paste(&mut self, text: &str, ctx: &Ctx) -> Vec<Action> {
        if let Some(prompt) = self.color_prompt.as_mut() {
            return prompt.paste(text);
        }
        if self.filter.editing {
            let actions = self.filter.paste(text);
            self.refilter(ctx);
            return actions;
        }
        if self.focus_members
            && let Some((_, view)) = self.skill_search.as_mut()
        {
            return view.paste(text, ctx);
        }
        vec![]
    }
    fn refilter(&mut self, ctx: &Ctx) {
        let selected = self.selected().map(|p| p.name.clone());
        let documents = self
            .all_presets
            .iter()
            .map(|p| skills::search::TextDocument {
                name: p.name.clone(),
                description: p.description.clone().unwrap_or_default(),
                body: String::new(),
            })
            .collect::<Vec<_>>();
        self.presets = self
            .filter
            .rank(&documents, ctx)
            .into_iter()
            .map(|i| self.all_presets[i].clone())
            .collect();
        self.list
            .select(selected.and_then(|name| self.presets.iter().position(|p| p.name == name)));
        self.list.clamp(self.presets.len());
        if self
            .skill_search
            .as_ref()
            .is_some_and(|(name, _)| self.selected().is_none_or(|p| &p.name != name))
        {
            self.skill_search = None;
        }
    }

    fn selected(&self) -> Option<&Preset> {
        self.list.selected().and_then(|i| self.presets.get(i))
    }

    /// Land on `name` once it shows up in the list.
    pub fn select(&mut self, name: &str) {
        self.pending = Some(name.to_string());
    }

    /// Prompts that need a preset under the cursor, or say what to do
    /// instead.
    fn with_selected(&self, open: impl FnOnce(&Preset) -> Modal) -> Vec<Action> {
        match self.selected() {
            Some(p) => vec![Action::OpenModal(Box::new(open(p)))],
            None => vec![Action::Error(
                "no preset selected; press c to create one".into(),
            )],
        }
    }

    fn ensure_skill_search(&mut self, ctx: &Ctx) {
        let name = self.selected().map(|p| p.name.clone()).unwrap_or_default();
        if self
            .skill_search
            .as_ref()
            .is_none_or(|(current, _)| current != &name)
        {
            let mut view = super::search::SearchView::panel(
                super::search::SkillPanelOptions::new(
                    self.visible_members.clone(),
                    format!("Preset: {name}"),
                    LayoutScope::Presets,
                )
                .hide_group(group::Kind::Preset, name.clone()),
                ctx,
            );
            view.focus_list();
            self.skill_search = Some((name, view));
        }
    }

    fn refresh_groups(&mut self, ctx: &Ctx) {
        self.visible_members = self.selected().map(Preset::members).unwrap_or_default();
        self.tag_groups = if ctx.settings.tags_enabled {
            let members: BTreeSet<_> = self.visible_members.iter().cloned().collect();
            tag_coverages(&ctx.ws.config, &members)
                .into_iter()
                .filter(|tag| tag.included > 0)
                .collect()
        } else {
            vec![]
        };
    }

    fn remove_members(&self, keys: Vec<String>) -> Vec<Action> {
        let Some(preset) = self.selected() else {
            return vec![];
        };
        let name = preset.name.clone();
        vec![Action::WriteMeta(Box::new(move |ws| {
            history::preset_edit(ws, &name, |members| {
                members.retain(|key| !keys.contains(key))
            })
        }))]
    }

    fn draw_tags(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) -> Rect {
        self.tag_area = Rect::default();
        if self.tag_groups.is_empty() || area.height < 3 {
            return area;
        }
        self.tag_area = Rect { height: 3, ..area };
        let spans =
            group::tag_coverage_pills(&self.tag_groups, ctx, area.width.saturating_sub(4) as usize);
        f.render_widget(
            Paragraph::new(Line::from(spans)),
            Rect::new(area.x + 2, area.y + 1, area.width.saturating_sub(4), 1),
        );
        Rect {
            y: area.y + 3,
            height: area.height - 3,
            ..area
        }
    }

    fn add_members(&self, ctx: &Ctx) -> Vec<Action> {
        // Membership is edited by picking from the library, never by typing
        // a name from memory.
        self.with_selected(|p| Modal::preset_members(&p.name, ctx))
    }

    /// Match the Tags sidebar while keeping package membership counts.
    fn preset_card(&self, p: &Preset, ctx: &Ctx, inner_w: usize) -> Vec<Line<'static>> {
        group::sidebar_card(
            &p.name,
            p.members().len(),
            p.description.as_deref(),
            group::preset_fill(p, ctx),
            inner_w,
            ctx,
        )
    }

    fn draw_presets(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        let th = &ctx.settings.theme;
        let inner = area;
        let content = Rect {
            width: inner.width.saturating_sub(1),
            ..inner
        };
        let heights = self.presets.iter().map(group::preset_card_height).collect();
        self.list.layout_heights(content, heights);
        if self.presets.is_empty() {
            f.render_widget(
                Paragraph::new(Span::styled(
                    if self.all_presets.is_empty() {
                        "no presets yet — press c to create one"
                    } else {
                        "No matching presets · / to edit · Esc to clear"
                    },
                    th.dim(),
                )),
                Rect {
                    height: 1,
                    ..content
                },
            );
            self.list_track.clear();
            return;
        }
        let selected = self.list.selected();
        for i in self.list.visible() {
            let Some(cell) = self.list.cell(i) else {
                continue;
            };
            let on = selected == Some(i);
            let ci = if cell.height < 3 {
                cell
            } else {
                frame(f, cell, on, !self.focus_members && !self.filter.editing, th)
            };
            let lines = group::fit_preset_card(
                self.preset_card(&self.presets[i], ctx, ci.width as usize),
                ci.height,
            );
            f.render_widget(Paragraph::new(lines), ci);
        }
        group::draw_track(f, inner, &self.list, selected, &mut self.list_track, th);
    }
}

impl View for PresetsView {
    fn overlay_open(&self) -> bool {
        self.color_prompt.is_some()
            || self.matrix.hints().is_some()
            || (self.focus_members
                && self
                    .skill_search
                    .as_ref()
                    .is_some_and(|(_, view)| view.overlay_open()))
    }
    fn handle_control_key(&mut self, key: KeyEvent, ctx: &Ctx) -> Vec<Action> {
        if self.focus_members
            && let Some((_, view)) = self.skill_search.as_mut()
        {
            return view.handle_control_key(key, ctx);
        }
        vec![]
    }
    fn actions_menu(&self, ctx: &Ctx) -> Option<Request> {
        if self.color_prompt.is_some() || self.matrix.hints().is_some() || self.filter.editing {
            return None;
        }
        if self.focus_members {
            let mut request = self.skill_search.as_ref()?.1.actions_menu(ctx)?;
            request.items.retain(|item| item.command != Command::Accept);
            for item in &mut request.items {
                if item.command == Command::Remove {
                    item.label = "Remove from preset".into();
                }
            }
            if let Target::Batch { all, .. } = &request.target {
                request.items.push(Item::new(
                    Command::Remove,
                    format!("Remove from preset · {} skills", all.len()),
                    KeyCode::Char('x'),
                    !all.is_empty(),
                    "No selected skills",
                    1,
                ));
            }
            return Some(request);
        }
        let preset = self.selected()?.name.clone();
        Some(Request {
            title: preset.clone(),
            detail: "Preset".into(),
            target: Target::Preset(preset),
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
                    Command::Matrix,
                    "Deployment matrix",
                    KeyCode::Char('m'),
                    true,
                    "",
                    0,
                ),
                Item::new(
                    Command::Description,
                    "Edit description",
                    KeyCode::Char('d'),
                    true,
                    "",
                    1,
                ),
                Item::new(
                    Command::Rename,
                    "Rename preset",
                    KeyCode::Char('r'),
                    true,
                    "",
                    1,
                ),
                Item::new(
                    Command::Color,
                    "Change colour",
                    KeyCode::Char('c'),
                    true,
                    "",
                    1,
                ),
                Item::new(
                    Command::Remove,
                    "Delete preset",
                    KeyCode::Char('x'),
                    true,
                    "",
                    2,
                ),
            ],
        })
    }
    fn context_menu(&mut self, x: u16, y: u16, ctx: &Ctx) -> Option<Request> {
        if self.color_prompt.is_some() || self.matrix.hints().is_some() {
            return None;
        }
        let view = &mut self.skill_search.as_mut()?.1;
        let mut request = view.context_menu(x, y, ctx)?;
        request.items.retain(|item| item.command != Command::Accept);
        for item in &mut request.items {
            if item.command == Command::Remove {
                item.label = "Remove from preset".into();
            }
        }
        self.focus_members = true;
        self.filter.editing = false;
        Some(request)
    }
    fn context_execute(&mut self, target: &Target, command: Command, ctx: &Ctx) -> Vec<Action> {
        if let Target::Preset(name) = target {
            if self.selected().map(|preset| preset.name.as_str()) != Some(name.as_str()) {
                return vec![Action::Error(
                    "Target changed; reopen the actions menu".into(),
                )];
            }
            return match command {
                Command::EditMembers => self.add_members(ctx),
                Command::Matrix => {
                    self.matrix.open(ctx);
                    vec![]
                }
                Command::Description => self.with_selected(|preset| {
                    Modal::preset_description(&preset.name, preset.description.as_deref())
                }),
                Command::Rename => self.with_selected(|preset| Modal::rename_preset(&preset.name)),
                Command::Color => {
                    if let Some(preset) = self.selected() {
                        self.color_prompt =
                            Some(Prompt::for_preset_color(preset, &ctx.settings.theme));
                    }
                    vec![]
                }
                Command::Remove => self.with_selected(|preset| Modal::delete_preset(&preset.name)),
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
        match self.skill_search.as_mut().map(|(_, v)| v) {
            Some(view) => view.context_execute(target, command, ctx),
            None => vec![Action::Error(
                "Target changed; reopen the context menu".into(),
            )],
        }
    }

    fn focus_from_above(&mut self) {
        self.focus_members = false;
        self.filter.editing = true;
    }

    fn focus_root(&mut self) {
        self.focus_members = false;
        self.filter.editing = false;
    }

    fn status(&self, ctx: &Ctx) -> String {
        self.skill_search
            .as_ref()
            .map_or_else(String::new, |(_, v)| v.status(ctx))
    }
    fn refresh(&mut self, ctx: &Ctx) {
        // The list is sorted by name, so a preset keeps its place only by
        // name: one created or deleted above the cursor would otherwise move
        // the selection onto a neighbour.
        let keep = self
            .pending
            .clone()
            .or_else(|| self.selected().map(|p| p.name.clone()));
        self.all_presets = ctx.snap.presets.by_name.values().cloned().collect();
        self.refilter(ctx);
        if let Some(i) = keep.and_then(|k| self.presets.iter().position(|p| p.name == k)) {
            self.list.select(Some(i));
            self.pending = None;
        }
        self.list.clamp(self.presets.len());
        self.refresh_groups(ctx);
        if let Some((_, view)) = self.skill_search.as_mut() {
            let keys = self.visible_members.clone();
            view.update_panel(keys, ctx);
        }
    }

    fn handle_key(&mut self, k: KeyEvent, ctx: &Ctx) -> Vec<Action> {
        if let Some(prompt) = self.color_prompt.as_mut() {
            let result = prompt.key(k);
            return self.finish_color(result);
        }
        if let Some(actions) = self.matrix.handle_key(k, ctx) {
            return actions;
        }
        self.refresh_groups(ctx);
        self.ensure_skill_search(ctx);
        if !self.focus_members
            && self.filter.editing
            && k.code == KeyCode::Right
            && self.filter.input.cursor_byte() == self.filter.input.value().len()
        {
            if let Some(view) = self.skill_search.as_mut().map(|(_, view)| view) {
                self.filter.editing = false;
                self.focus_members = true;
                view.focus_input();
            }
            return vec![];
        }
        if !self.focus_members && self.filter.editing && k.code == KeyCode::Up {
            self.filter.editing = false;
            return vec![Action::BackToParent];
        }
        if !self.focus_members && self.filter.key(k) {
            self.refilter(ctx);
            return vec![];
        }
        if self.focus_members
            && let Some((_, view)) = self.skill_search.as_mut()
        {
            if k.code == KeyCode::Left && view.input_at_left_edge() {
                view.close_input_completion();
                self.focus_members = false;
                self.filter.editing = true;
                return vec![];
            }
            if k.code == KeyCode::Left && view.panel_back() {
                self.focus_members = false;
                self.filter.editing = false;
                return vec![];
            }
            if view.panel_actions_ready() {
                if matches!(k.code, KeyCode::Char('x') | KeyCode::Delete) && k.modifiers.is_empty()
                {
                    let keys = view.panel_keys(ctx);
                    return self.remove_members(keys);
                }
                if k.code == KeyCode::Char('a') && k.modifiers.is_empty() {
                    return self.add_members(ctx);
                }
            }
            let mut actions = view.handle_key(k, ctx);
            if k.code == KeyCode::Up {
                return actions;
            }
            if actions.iter().any(|a| matches!(a, Action::BackToParent)) {
                self.focus_members = false;
                self.filter.editing = false;
                actions.retain(|a| !matches!(a, Action::BackToParent));
            }
            return actions;
        }
        if k.code == KeyCode::Char('M') {
            self.matrix.open(ctx);
            return vec![];
        }
        let n = self.presets.len();
        match k.code {
            KeyCode::Char('q') => vec![Action::BackToParent],
            KeyCode::Esc => vec![Action::BackToParent],
            KeyCode::Down | KeyCode::Char('j') => {
                self.skill_search = None;
                self.list.move_by(1, n);
                vec![]
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if self.list.selected().unwrap_or(0) == 0 {
                    self.filter.editing = true;
                    return vec![];
                }
                self.skill_search = None;
                self.list.move_by(-1, n);
                vec![]
            }
            KeyCode::Home | KeyCode::Char('g') => {
                self.skill_search = None;
                self.list.first(n);
                vec![]
            }
            KeyCode::End | KeyCode::Char('G') => {
                self.skill_search = None;
                self.list.last(n);
                vec![]
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                if self.selected().is_some() {
                    self.focus_members = true;
                    if let Some(view) = self.skill_search.as_mut().map(|(_, view)| view) {
                        view.focus_list();
                    }
                }
                vec![]
            }
            KeyCode::Char('c') => vec![Action::OpenModal(Box::new(Modal::new_preset()))],
            KeyCode::Char('C') => {
                if let Some(p) = self.selected() {
                    self.color_prompt = Some(Prompt::for_preset_color(p, &ctx.settings.theme));
                }
                vec![]
            }
            KeyCode::Char('a') => self.add_members(ctx),
            KeyCode::Char('e') => {
                self.with_selected(|p| Modal::preset_description(&p.name, p.description.as_deref()))
            }
            KeyCode::Char('r') => self.with_selected(|p| Modal::rename_preset(&p.name)),
            // Deleting a whole preset is the one destructive key here, and it
            // is the capital so a slip on `x` in the member list cannot reach it.
            KeyCode::Char('D') => match self.selected() {
                Some(p) => vec![Action::OpenModal(Box::new(Modal::delete_preset(&p.name)))],
                None => vec![],
            },
            _ => vec![],
        }
    }

    fn handle_mouse(&mut self, m: MouseEvent, ctx: &Ctx) -> Vec<Action> {
        if let Some(prompt) = self.color_prompt.as_mut() {
            let result = prompt.mouse(m);
            return self.finish_color(result);
        }
        if let Some(actions) = self.matrix.handle_mouse(m, ctx) {
            return actions;
        }
        self.refresh_groups(ctx);
        self.ensure_skill_search(ctx);
        let at = (m.column, m.row).into();
        let pressing = m.kind == MouseEventKind::Down(MouseButton::Left);
        let dragging = m.kind == MouseEventKind::Drag(MouseButton::Left);
        if self.tag_area.contains(at) {
            return vec![];
        }
        if pressing && self.filter.click_input(m.column, m.row) {
            self.focus_members = false;
            self.filter.editing = true;
            return vec![];
        }
        if self.right.contains(at) {
            if pressing || wheel(&m, ctx).is_some() {
                self.focus_members = true;
                self.filter.editing = false;
            }
            return self.skill_search.as_mut().unwrap().1.handle_mouse(m, ctx);
        }
        if let Some(d) = wheel(&m, ctx) {
            if self.left.contains(at) {
                self.focus_members = false;
                self.filter.editing = false;
                self.list.move_by(d.signum(), self.presets.len());
            }
            return vec![];
        }
        if (pressing && self.list_track.hit(m.column, m.row)) || (dragging && self.list_drag) {
            self.list_drag = true;
            self.focus_members = false;
            self.filter.editing = false;
            if let Some(row) = self.list_track.index_at(m.row, self.list.grid_rows()) {
                self.list.select_row(row);
            }
            return vec![];
        }
        if !dragging {
            self.list_drag = false;
        }
        if pressing && self.left.contains(at) {
            self.focus_members = false;
            self.filter.editing = false;
            self.list.click(m.column, m.row);
        }
        vec![]
    }

    fn draw(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        self.refresh_groups(ctx);
        let (left, right) = split_panes(area, 38, ctx);
        self.left = left;
        self.right = right;
        let content = self.filter.draw(
            f,
            left,
            "Filter presets",
            "presets",
            !self.focus_members,
            ctx,
        );
        self.draw_presets(f, content, ctx);
        self.ensure_skill_search(ctx);
        if let Some((name, mut view)) = self.skill_search.take() {
            view.set_panel_active(self.focus_members);
            view.draw_with_content_header(
                f,
                right,
                ctx,
                if self.tag_groups.is_empty() { 0 } else { 3 },
                |f, content| {
                    self.draw_tags(f, content, ctx);
                },
            );
            self.skill_search = Some((name, view));
        }
        self.matrix.draw(f, area, ctx);
        if let Some(prompt) = self.color_prompt.as_mut() {
            prompt.draw(f, area, ctx);
        }
    }

    fn hints(&self) -> Hints {
        if self.color_prompt.is_some() {
            return &[("↑↓", "colour"), ("Enter", "apply"), ("Esc", "cancel")];
        }
        if self.filter.editing {
            return &[("Enter/↓", "presets"), ("Esc", "clear filter")];
        }
        if self.focus_members
            && let Some((_, view)) = self.skill_search.as_ref()
        {
            return view.preset_panel_hints();
        }
        if let Some(hints) = self.matrix.hints() {
            return hints;
        }
        if self.focus_members {
            &[
                ("/", "filter skills"),
                ("a", "actions"),
                ("x", "remove"),
                ("m", "multi-select"),
                ("Enter", "preview"),
                ("←/Esc", "presets"),
            ]
        } else {
            &[
                ("Enter/→", "members"),
                ("/", "filter presets"),
                ("c", "create"),
                ("a", "actions"),
                ("Esc/q", "clear/back"),
            ]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::theme::Theme;
    use crossterm::event::KeyModifiers;
    use ratatui::{Terminal, backend::TestBackend};
    use skills::{Workspace, config::Config};

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
    fn preset_colour_picker_saves_resets_and_cancels_without_changing_members() {
        let root = skills::ops::DownloadDir::new("preset-colour-test").unwrap();
        Config {
            agents: vec![],
            ..Default::default()
        }
        .save(root.path())
        .unwrap();
        let ws = Workspace::open(root.path()).unwrap();
        // Old presets have no colour field.
        let preset: Preset = toml::from_str("name = 'Office'\nskills = ['document']").unwrap();
        assert_eq!(preset.color, None);
        ws.presets.save(&preset).unwrap();
        let snap = ws.scan().unwrap();
        let theme = Theme::default();
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &{
                let mut settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
                settings.theme = theme;
                settings
            },
        };
        let mut view = PresetsView::default();
        view.refresh(&ctx);
        for (typed, expected) in [("#b87e54", Some("#b87e54")), ("none", None)] {
            view.handle_key(KeyEvent::new(KeyCode::Char('C'), KeyModifiers::SHIFT), &ctx);
            assert!(view.input_focused());
            assert!(view.paste(typed, &ctx).is_empty());
            let mut actions =
                view.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &ctx);
            let Action::Write(write) = actions.remove(0) else {
                panic!("expected write")
            };
            write(&ws).unwrap();
            let saved = ws.presets.load("Office").unwrap().unwrap();
            assert_eq!(saved.color.as_deref(), expected);
            assert_eq!(saved.skills, preset.skills);
            view.refresh(&ctx);
            let lines = view.preset_card(&saved, &ctx, 40);
            assert_eq!(
                lines[0].spans[0].style.fg,
                Some(expected.map_or(theme.tag, |_| ratatui::style::Color::Rgb(184, 126, 84)))
            );
        }
        view.handle_key(KeyEvent::new(KeyCode::Char('C'), KeyModifiers::SHIFT), &ctx);
        view.paste("invalid-colour", &ctx);
        assert!(matches!(
            view.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &ctx)
                .as_slice(),
            [Action::Error(_)]
        ));
        assert!(view.color_prompt.is_some());
        view.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &ctx);
        assert!(view.color_prompt.is_none());
        assert_eq!(ws.presets.load("Office").unwrap().unwrap(), preset);
    }

    #[test]
    fn presets_context_menu_uses_shared_member_actions_and_preset_specific_remove_label() {
        let tmp = skills::ops::DownloadDir::new("presets-context-menu").unwrap();
        let root = tmp.path();
        Config {
            agents: vec![],
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
        let ws = Workspace::open(root).unwrap();
        ws.presets
            .save(&Preset {
                name: "Office".into(),
                skills: vec!["sample".into()],
                ..Preset::default()
            })
            .unwrap();
        let snap = ws.scan().unwrap();
        let settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &settings,
        };
        let mut view = PresetsView::default();
        view.select("Office");
        view.refresh(&ctx);

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
            Some("Remove from preset")
        );
        assert!(request.items.iter().any(|item| item.disabled.is_some()));
        assert!(matches!(
            view.context_execute(&request.target, Command::Remove, &ctx)
                .as_slice(),
            [Action::WriteMeta(_)]
        ));
    }

    #[test]
    fn preset_sidebar_cards_share_tag_style_and_fit_unicode_without_agent_status() {
        let root = skills::ops::DownloadDir::new("preset-card-test").unwrap();
        Config {
            agents: vec![],
            ..Default::default()
        }
        .save(root.path())
        .unwrap();
        let path = root.path().join("document");
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(
            path.join("SKILL.md"),
            "---\nname: document-tools\ndescription: Document tools\n---\nBody",
        )
        .unwrap();
        let ws = Workspace::open(root.path()).unwrap();
        let snap = ws.scan().unwrap();
        let theme = Theme::default();
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &{
                let mut settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
                settings.theme = theme;
                settings
            },
        };
        let preset = Preset {
            name: "Office".into(),
            color: Some("#b87e54".into()),
            description: Some("**Document tools** with 中文说明".into()),
            skills: vec!["document".into(), "missing".into()],
            agents: vec!["SampleAgent".into()],
        };
        let view = PresetsView::default();
        let lines = view.preset_card(&preset, &ctx, 65);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].to_string().starts_with("● Office"));
        assert_eq!(lines[0].spans[1].style, theme.bold());
        assert!(lines[0].spans.iter().all(|span| span.style.bg.is_none()));
        let mut compact = preset.clone();
        compact.description = None;
        let compact_lines = view.preset_card(&compact, &ctx, 65);
        assert_eq!(compact_lines.len(), 1);
        assert!(lines[0].to_string().ends_with("2 skills"));
        assert!(lines[1].to_string().starts_with("Document tools"));
        assert!(
            !lines
                .iter()
                .any(|line| line.to_string().contains("SampleAgent"))
        );
        let mut rendered = PresetsView {
            presets: vec![preset.clone()],
            ..Default::default()
        };
        for height in [3, 4, 5, 6, 8] {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, height)).unwrap();
            terminal
                .draw(|f| rendered.draw_presets(f, f.area(), &ctx))
                .unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect();
            assert!(
                text.contains("Office"),
                "preset must remain visible at height {height}"
            );
            if height >= 5 {
                assert_eq!(terminal.backend().buffer()[(0, 0)].symbol(), "╭");
            }
            if height >= 6 {
                assert_eq!(terminal.backend().buffer()[(0, 3)].symbol(), "╰");
            }
        }
        for width in 0..80 {
            assert!(
                view.preset_card(&preset, &ctx, width)
                    .iter()
                    .all(|line| line.width() <= width)
            );
        }
    }

    #[test]
    fn preview_blocks_member_removal_until_closed() {
        let root = std::env::temp_dir().join(format!(
            "skills-preview-keys-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let config = Config {
            agents: vec![],
            ..Config::default()
        };
        config.save(&root).unwrap();
        let ws = Workspace::open(&root).unwrap();
        ws.presets
            .save(&Preset {
                name: "reading".into(),
                skills: vec!["printer".into()],
                ..Preset::default()
            })
            .unwrap();
        let before = std::fs::read(ws.presets.path("reading")).unwrap();
        let snap = ws.scan().unwrap();
        let theme = Theme::default();
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &{
                let mut settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
                settings.theme = theme;
                settings
            },
        };
        let mut view = PresetsView::default();
        view.refresh(&ctx);
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        assert!(view.handle_key(key(KeyCode::Char('m')), &ctx).is_empty());
        assert!(view.handle_key(key(KeyCode::Right), &ctx).is_empty());
        let actions = view.handle_key(key(KeyCode::Char('m')), &ctx);
        assert!(actions.is_empty());
        assert!(view.skill_search.is_some());
        view.handle_key(key(KeyCode::Esc), &ctx); // Leave multi-select.
        assert!(view.handle_key(key(KeyCode::Enter), &ctx).is_empty());
        assert!(!view.skill_search.as_ref().unwrap().1.panel_actions_ready());

        for code in [KeyCode::Char('x'), KeyCode::Delete, KeyCode::Char('a')] {
            assert!(view.handle_key(key(code), &ctx).is_empty());
            assert!(!view.skill_search.as_ref().unwrap().1.panel_actions_ready());
            assert!(view.focus_members);
            assert_eq!(
                view.skill_search.as_ref().unwrap().1.panel_keys(&ctx),
                vec!["printer"]
            );
            assert_eq!(std::fs::read(ws.presets.path("reading")).unwrap(), before);
        }

        assert!(view.handle_key(key(KeyCode::Esc), &ctx).is_empty());
        assert!(view.skill_search.as_ref().unwrap().1.panel_actions_ready());
        assert!(view.focus_members);
        let mut actions = view.handle_key(key(KeyCode::Char('x')), &ctx);
        assert_eq!(actions.len(), 1);
        let Action::WriteMeta(write) = actions.remove(0) else {
            panic!("member removal should resume after closing the preview");
        };
        write(&ws).unwrap();
        assert!(
            ws.presets
                .load("reading")
                .unwrap()
                .unwrap()
                .skills
                .is_empty()
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod tag_group_tests {
    use super::*;
    use crossterm::event::KeyModifiers;
    use ratatui::{Terminal, backend::TestBackend};
    use skills::{
        Workspace,
        config::{Config, TagConfig},
    };

    fn fixture() -> (skills::ops::DownloadDir, Workspace) {
        let temp = skills::ops::DownloadDir::new("preset-tag-coverage").unwrap();
        let root = temp.path();
        let tag = |name: &str, members: &[&str]| TagConfig {
            name: name.into(),
            skills: members.iter().map(|name| (*name).into()).collect(),
            color: Some("blue".into()),
            description: None,
        };
        Config {
            agents: vec![],
            tags: vec![
                tag("all", &["alpha", "beta", "beta"]),
                tag("overlap", &["alpha", "gamma"]),
                tag("zero", &["gamma"]),
                tag("empty", &[]),
            ],
            ..Default::default()
        }
        .save(root)
        .unwrap();
        for key in ["alpha", "beta", "gamma"] {
            std::fs::create_dir(root.join(key)).unwrap();
            std::fs::write(
                root.join(key).join("SKILL.md"),
                format!("---\nname: {key}\ndescription: example\n---\n"),
            )
            .unwrap();
        }
        let ws = Workspace::open(root).unwrap();
        ws.presets
            .save(&Preset {
                name: "work".into(),
                skills: vec!["alpha".into(), "beta".into(), "alpha".into()],
                ..Default::default()
            })
            .unwrap();
        (temp, ws)
    }

    #[test]
    fn composition_counts_overlap_without_creating_an_editable_tag_selector() {
        let (_temp, ws) = fixture();
        let snap = ws.scan().unwrap();
        let settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &settings,
        };
        let mut view = PresetsView::default();
        view.refresh(&ctx);
        assert_eq!(view.visible_members, ["alpha", "beta"]);
        assert_eq!(
            view.tag_groups,
            [
                TagCoverage {
                    name: "all".into(),
                    included: 2,
                    total: 2
                },
                TagCoverage {
                    name: "overlap".into(),
                    included: 1,
                    total: 2
                },
            ]
        );
        let before = std::fs::read(ws.presets.path("work")).unwrap();
        let mut terminal = Terminal::new(TestBackend::new(130, 30)).unwrap();
        terminal.draw(|f| view.draw(f, f.area(), &ctx)).unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(screen.contains("all"));
        assert!(screen.contains("2/2"));
        assert!(screen.contains("1/2"));
        assert!(screen.contains('✓') && screen.contains('◐'));
        assert!(!screen.contains("[✓]") && !screen.contains("[ ]"));
        assert!(!screen.contains("zero") && !screen.contains("empty"));

        for kind in [
            MouseEventKind::Down(MouseButton::Left),
            MouseEventKind::ScrollDown,
        ] {
            let actions = view.handle_mouse(
                MouseEvent {
                    kind,
                    column: view.tag_area.x + 3,
                    row: view.tag_area.y + 1,
                    modifiers: KeyModifiers::NONE,
                },
                &ctx,
            );
            assert!(actions.is_empty());
            assert!(
                !view.focus_members,
                "read-only composition must not change focus"
            );
            assert!(!view.filter.editing);
        }
        assert!(
            view.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE), &ctx)
                .is_empty()
        );
        assert_eq!(std::fs::read(ws.presets.path("work")).unwrap(), before);
        assert_eq!(view.visible_members, ["alpha", "beta"]);
    }

    #[test]
    fn removing_a_member_does_not_modify_its_tags() {
        let (_temp, ws) = fixture();
        let snap = ws.scan().unwrap();
        let settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &settings,
        };
        let mut view = PresetsView::default();
        view.refresh(&ctx);
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        view.handle_key(key(KeyCode::Right), &ctx);
        let Action::WriteMeta(remove) = view.handle_key(key(KeyCode::Char('x')), &ctx).remove(0)
        else {
            panic!("member removal");
        };
        remove(&ws).unwrap();
        assert_eq!(
            ws.presets.load("work").unwrap().unwrap().members(),
            ["beta"]
        );
        assert_eq!(
            Config::load(&ws.root).unwrap().skill_tags("alpha"),
            ["all", "overlap"]
        );
        assert!(ws.root.join("alpha/SKILL.md").is_file());
    }

    #[test]
    fn changing_or_hiding_tags_only_changes_the_composition_summary() {
        let (_temp, mut ws) = fixture();
        let mut view = PresetsView::default();
        for (hide, expected_groups) in [(false, 2), (false, 1), (true, 0)] {
            if expected_groups == 1 {
                Config::edit_tags(&ws.root, |tags| {
                    tags.iter_mut()
                        .find(|tag| tag.name == "all")
                        .unwrap()
                        .skills
                        .push("gamma".into());
                    tags.iter_mut()
                        .find(|tag| tag.name == "overlap")
                        .unwrap()
                        .skills = vec!["gamma".into()];
                })
                .unwrap();
            }
            Config::set_tags_enabled(&ws.root, !hide).unwrap();
            ws.config = ws.load_config().unwrap();
            let snap = ws.scan().unwrap();
            let settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
            let ctx = Ctx {
                ws: &ws,
                snap: &snap,
                settings: &settings,
            };
            view.refresh(&ctx);
            assert_eq!(view.visible_members, ["alpha", "beta"]);
            assert_eq!(view.tag_groups.len(), expected_groups);
            if expected_groups == 1 {
                assert_eq!(
                    (view.tag_groups[0].included, view.tag_groups[0].total),
                    (2, 3)
                );
            }
            assert_eq!(
                ws.presets.load("work").unwrap().unwrap().members(),
                ["alpha", "beta"]
            );
        }
    }
}
