//! Library tab: input, result list, preview.
mod context;
use crate::tui::components::choice_footer::{self, ChoiceEvent, ChoiceFocus};
use crate::tui::components::context_menu::{Command, Request, Target};

use super::preview::{Overlay, preview_lines};
use super::{View, wheel};
use crate::tui::app::{Action, Ctx, Hints};
use crate::tui::components::group::Kind;
use crate::tui::components::layout::split_panes;
use crate::tui::components::layout::{cols_for, skill_frame};
use crate::tui::components::search_panel::{PanelLayout, PanelStyle, SearchEvent, SearchPanel};
use crate::tui::components::skill::{SkillPresentation, SkillRenderState};
use crate::tui::event::Task;
use crate::tui::modal::Modal;
use crate::tui::settings::LayoutScope;
use crate::tui::widgets::{CardGrid, Input, ScrollTrack};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap};
use skills::config::UiLayout;
use skills::ops::edit;
use skills::reconcile::{SkillRecord, SkillStatus};
use skills::search::{Hit, Query, Searcher};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Input,
    List,
    Preview,
}

/// Configuration shared by embedded Library panels. Hosts own navigation and
/// domain actions; this view owns search, cards, selection, preview and deployment.
pub struct SkillPanelOptions {
    pub keys: Vec<String>,
    pub title: String,
    pub layout_scope: LayoutScope,
    pub hidden_group: Option<(Kind, String)>,
}

impl SkillPanelOptions {
    pub fn new(keys: Vec<String>, title: String, layout_scope: LayoutScope) -> Self {
        Self {
            keys,
            title,
            layout_scope,
            hidden_group: None,
        }
    }

    pub fn hide_group(mut self, kind: Kind, name: String) -> Self {
        self.hidden_group = Some((kind, name));
        self
    }
}

pub struct SearchView {
    search_panel: SearchPanel,
    focus: Focus,
    hits: Vec<Hit>,
    grid: CardGrid,
    preview_scroll: u16,
    preview_lines: usize,
    preview_height: u16,
    input_rect: Rect,
    list_rect: Rect,
    list_track: ScrollTrack,
    /// A drag keeps hold of the track even when the pointer wanders off it.
    track_drag: bool,
    preview_rect: Rect,
    /// The owner of the layout preference; selection dialogs constrain their own layout.
    layout_scope: LayoutScope,
    rendered_layout: UiLayout,
    /// Grid layout has no standing preview pane, so it opens over the results.
    overlay: Overlay,
    searcher: Searcher,
    multi: bool,
    scope_agent: Option<String>,
    panel: Option<(BTreeSet<String>, String)>,
    panel_active: bool,
    hidden_group: Option<(Kind, String)>,
    preset: Option<String>,
    /// The starting membership lets saves preserve unrelated concurrent edits.
    preset_original: BTreeSet<String>,
    tag: Option<String>,
    target: Option<(
        skills::config::AgentConfig,
        Option<std::path::PathBuf>,
        bool,
    )>,
    area: Rect,
    scope: Option<(BTreeSet<String>, String)>,
    checked: BTreeSet<String>,
    batch_buttons: Vec<(Rect, char)>,
    choice_focus: ChoiceFocus,
    updates: BTreeMap<String, String>,
}

impl Default for SearchView {
    fn default() -> Self {
        Self {
            search_panel: SearchPanel::default(),
            focus: Focus::Input,
            hits: Vec::new(),
            grid: CardGrid::default(),
            preview_scroll: 0,
            preview_lines: 0,
            preview_height: 0,
            input_rect: Rect::default(),
            list_rect: Rect::default(),
            list_track: ScrollTrack::default(),
            track_drag: false,
            preview_rect: Rect::default(),
            layout_scope: LayoutScope::Library,
            rendered_layout: UiLayout::Grid,
            overlay: Overlay::default(),
            searcher: Searcher::new(),
            multi: false,
            scope_agent: None,
            panel: None,
            panel_active: true,
            hidden_group: None,
            preset: None,
            preset_original: BTreeSet::new(),
            tag: None,
            target: None,
            area: Rect::default(),
            scope: None,
            checked: BTreeSet::new(),
            batch_buttons: Vec::new(),
            choice_focus: ChoiceFocus::List,
            updates: BTreeMap::new(),
        }
    }
}

impl SearchView {
    pub fn draw_with_content_header(
        &mut self,
        f: &mut Frame,
        area: Rect,
        ctx: &Ctx,
        header_height: u16,
        header: impl FnOnce(&mut Frame, Rect),
    ) {
        self.area = area;
        let footer_height = (u16::from(self.is_picker()) * 2).min(area.height);
        let panel_area = Rect {
            height: area.height.saturating_sub(footer_height),
            ..area
        };
        let footer = Rect::new(area.x, panel_area.bottom(), area.width, footer_height);
        let title = match self.panel.as_ref().or(self.scope.as_ref()) {
            Some((_, title)) => format!(" {title} "),
            None => " skills ".into(),
        };
        let input_title = self.input_title(ctx);
        let usage = if ctx.settings.tags_enabled {
            "   repo:owner/repo  tag:x  preset:y  agent:z  source:local  untagged"
        } else {
            "   repo:owner/repo  preset:y  agent:z  source:local"
        };
        let areas = self.search_panel.draw(
            f,
            panel_area,
            PanelStyle {
                layout: PanelLayout::Separate,
                input_title,
                results_title: Line::from(title),
                hint: ("search skills…", usage),
                input_active: self.panel_active && self.focus == Focus::Input,
                results_active: self.panel_active
                    && self.focus == Focus::List
                    && self.choice_focus == ChoiceFocus::List,
                header_height,
            },
            &ctx.settings.theme,
        );
        self.input_rect = areas.input;
        header(f, areas.header);
        let content = areas.results;

        // Split keeps a preview open beside the results and so gets one column
        // of cards; grid spends the whole width on cards and puts the preview
        // over them when it is wanted.
        let grid = self.layout(ctx) == UiLayout::Grid;
        let (left, right) = if grid {
            (content, Rect::default())
        } else {
            split_panes(content, 38, ctx)
        };
        self.list_rect = left;
        self.draw_results(f, left, ctx);
        if self.rendered_layout == UiLayout::Grid {
            self.search_panel.draw_position(
                f,
                self.grid.visible(),
                self.hits.len(),
                &ctx.settings.theme,
            );
        }
        if grid {
            self.preview_rect = Rect::default();
        } else {
            self.preview_rect = right;
            self.draw_preview(f, right, ctx);
        }
        self.overlay.draw(f, content, ctx);
        self.batch_buttons.clear();
        if self.is_picker() {
            if self.hits.is_empty()
                && self.focus == Focus::List
                && self.choice_focus == ChoiceFocus::List
            {
                self.choice_focus = ChoiceFocus::Apply;
            }
            let enabled = self.can_apply(ctx);
            let summary = if enabled {
                format!("{} selected", self.checked.len())
            } else {
                "No pending changes".into()
            };
            let buttons = choice_footer::draw(
                f,
                footer,
                self.choice_focus,
                enabled,
                &summary,
                &ctx.settings.theme,
            );
            self.batch_buttons
                .extend([(buttons[0], 'c'), (buttons[1], 'e')]);
        }
        if self.panel_active && self.focus == Focus::Input && !self.overlay.is_open() {
            self.search_panel.draw_completion(f, areas.results, ctx);
        }
    }

    fn is_picker(&self) -> bool {
        self.preset.is_some() || self.tag.is_some() || self.target.is_some()
    }

    fn includes_record(&self, record: &skills::reconcile::SkillRecord) -> bool {
        (!self.is_picker() || (record.status.is_present() && record.name.is_some()))
            && self
                .panel
                .as_ref()
                .or(self.scope.as_ref())
                .is_none_or(|(keys, _)| keys.contains(&record.key))
            && (self.is_picker()
                || self.scope.is_some()
                || self.panel.is_some()
                || record.status.is_healthy())
    }

    pub fn panel(options: SkillPanelOptions, ctx: &Ctx) -> Self {
        let mut view = Self {
            panel: Some((options.keys.into_iter().collect(), options.title)),
            layout_scope: options.layout_scope,
            hidden_group: options.hidden_group,
            ..Self::default()
        };
        view.refresh(ctx);
        view
    }

    pub fn panel_keys(&self, ctx: &Ctx) -> Vec<String> {
        if self.multi {
            self.checked.iter().cloned().collect()
        } else {
            self.selected(ctx)
                .map(|r| vec![r.key.clone()])
                .unwrap_or_default()
        }
    }

    pub fn update_panel(&mut self, keys: Vec<String>, ctx: &Ctx) {
        if let Some((current, _)) = &mut self.panel {
            *current = keys.into_iter().collect();
        }
        self.refresh(ctx);
    }

    pub fn restore_results(&mut self, ctx: &Ctx) {
        self.run_search(ctx, true);
    }

    pub fn set_panel_active(&mut self, active: bool) {
        self.panel_active = active;
    }

    pub fn preset_panel_hints(&self) -> Hints {
        if self.panel_actions_ready() {
            if self.multi {
                return &[
                    ("Space", "select"),
                    ("Ctrl+A", "select results"),
                    ("x", "remove from preset"),
                    ("t", "tags"),
                    ("/", "filter"),
                    ("Esc", "cancel selection"),
                ];
            }
            return &[
                ("/", "filter skills"),
                ("m", "multi-select"),
                ("a", "edit members"),
                ("x", "remove from preset"),
                ("Enter", "preview"),
                ("←", "presets"),
            ];
        }
        self.hints()
    }

    pub fn panel_actions_ready(&self) -> bool {
        self.focus == Focus::List && !self.overlay.is_open()
    }

    /// Let an owning split page cross to its adjacent search field at the cursor boundary.
    pub fn input_at_left_edge(&self) -> bool {
        self.focus == Focus::Input
            && !self.overlay.is_open()
            && self.search_panel.input.cursor_byte() == 0
    }
    pub fn close_input_completion(&mut self) {
        self.search_panel.completion.close();
    }
    pub fn panel_back(&self) -> bool {
        !self.multi
            && self.focus == Focus::List
            && !self.overlay.is_open()
            && self
                .grid
                .selected()
                .is_none_or(|index| index.is_multiple_of(self.grid.cols()))
    }

    fn update_completion(&mut self, ctx: &Ctx) {
        let keys: std::collections::BTreeSet<_> = ctx
            .snap
            .skills
            .iter()
            .filter(|r| self.includes_record(r))
            .map(|r| r.key.clone())
            .collect();
        self.search_panel.update_completion(Some(
            |input: &Input, completion: &mut crate::tui::components::completion::Completion| {
                completion.update_scoped(input, ctx, Some(&keys))
            },
        ));
        if self.panel.is_none() && self.scope.is_none() && !self.is_picker() {
            self.search_panel.completion.healthy_only();
        }
    }

    pub fn picker_title(&self) -> String {
        match &self.target {
            Some((agent, _, on)) => format!(
                " {} skills · {} · {} ",
                if *on { "Install" } else { "Uninstall" },
                agent.display_name(),
                skills::paths::contract_tilde(&agent.skills_path())
            ),
            None => self.tag.as_ref().map_or_else(
                || {
                    format!(
                        " Preset: {} · members ",
                        self.preset.as_deref().unwrap_or_default()
                    )
                },
                |tag| format!(" Tag: {tag} · skills "),
            ),
        }
    }

    pub fn target_skills(
        agent: skills::config::AgentConfig,
        project: Option<std::path::PathBuf>,
        on: bool,
        keys: Option<Vec<String>>,
        ctx: &Ctx,
    ) -> Self {
        let mut view = Self::default();
        view.refresh(ctx);
        if let Some(keys) = keys {
            view.select_scope(keys, "Deployed skills".into(), None, ctx);
        }
        view.target = Some((agent, project, on));
        view.run_search(ctx, false);
        view.multi = true;
        view
    }

    pub fn preset_members(preset: &str, ctx: &Ctx) -> Self {
        let mut view = Self::default();
        view.refresh(ctx);
        view.preset = Some(preset.into());
        view.hidden_group = Some((Kind::Preset, preset.into()));
        view.run_search(ctx, false);
        view.multi = true;
        if let Ok(Some(p)) = ctx.ws.presets.load(preset) {
            view.checked = p.members().into_iter().collect();
            view.preset_original = view.checked.clone();
        }
        view
    }

    pub fn tag_members(tag: &str, ctx: &Ctx) -> Self {
        let mut view = Self {
            tag: Some(tag.into()),
            multi: true,
            hidden_group: Some((Kind::Tag, tag.into())),
            ..Self::default()
        };
        view.refresh(ctx);
        view.checked = ctx
            .snap
            .skills
            .iter()
            .filter(|r| r.tags.iter().any(|t| t == tag))
            .map(|r| r.key.clone())
            .collect();
        view
    }

    fn can_apply(&self, ctx: &Ctx) -> bool {
        if self.target.is_some() {
            return !self.visible_checked(ctx).is_empty();
        }
        if self.tag.is_some() {
            return self.hits.iter().any(|h| {
                let r = &ctx.snap.skills[h.index];
                self.checked.contains(&r.key) != r.tags.iter().any(|t| Some(t) == self.tag.as_ref())
            });
        }
        self.checked != self.preset_original
    }
    fn apply_preset(&self, ctx: &Ctx) -> Vec<Action> {
        if !self.can_apply(ctx) {
            return vec![];
        }
        if let Some((agent, project, on)) = self.target.clone() {
            let keys = self.visible_checked(ctx);
            if keys.is_empty() {
                return vec![Action::Error(
                    "Select skills in the current filter first".into(),
                )];
            }
            return vec![
                Action::CloseModal,
                Action::BatchMeta(
                    Box::new({
                        let keys = keys.clone();
                        move |ws| {
                            skills::ops::targets::set_deployed(
                                ws,
                                &agent,
                                project.as_deref(),
                                &keys,
                                on,
                            )
                        }
                    }),
                    keys,
                ),
            ];
        }
        if let Some(tag) = self.tag.clone() {
            let visible: BTreeSet<String> = self
                .hits
                .iter()
                .map(|h| ctx.snap.skills[h.index].key.clone())
                .collect();
            let desired = self.visible_checked(ctx);
            let keys = visible.iter().cloned().collect();
            return vec![Action::BatchMeta(
                Box::new(move |ws| {
                    skills::history::tag_edit(ws, |ws| {
                        skills::config::Config::edit_tags(&ws.root, |tags| {
                            if let Some(group) = tags.iter_mut().find(|t| t.name == tag) {
                                group.skills.retain(|k| !visible.contains(k));
                                group.skills.extend(desired);
                            }
                        })?;
                        Ok(format!("updated members of {tag}"))
                    })
                }),
                keys,
            )];
        }
        let Some(preset) = self.preset.clone() else {
            return vec![];
        };
        let added: Vec<_> = self
            .checked
            .difference(&self.preset_original)
            .cloned()
            .collect();
        let removed: BTreeSet<_> = self
            .preset_original
            .difference(&self.checked)
            .cloned()
            .collect();
        let keys = self.checked.union(&self.preset_original).cloned().collect();
        vec![Action::BatchMeta(
            Box::new(move |ws| {
                skills::history::preset_edit(ws, &preset, |members| {
                    members.retain(|key| !removed.contains(key));
                    for key in &added {
                        if !members.contains(key) {
                            members.push(key.clone());
                        }
                    }
                })
            }),
            keys,
        )]
    }

    pub fn clear_selection(&mut self) {
        self.multi = false;
        self.checked.clear();
        self.scope = None;
        self.scope_agent = None;
    }

    pub fn restrict_agent(&mut self, agent: String) {
        self.scope_agent = Some(agent);
    }

    pub fn select_scope(
        &mut self,
        keys: Vec<String>,
        title: String,
        checked: Option<String>,
        ctx: &Ctx,
    ) {
        self.clear_selection();
        self.scope = Some((keys.into_iter().collect(), title));
        self.multi = true;
        self.search_panel.input.clear();
        self.search_panel.completion.close();
        if let Some(key) = checked {
            self.checked.insert(key);
        }
        self.focus = Focus::List;
        self.run_search(ctx, false);
    }

    pub fn batch_finished(&mut self, failed: &[String]) {
        if failed.is_empty() {
            self.clear_selection();
        } else {
            self.multi = true;
            self.checked = failed.iter().cloned().collect();
        }
    }

    fn toggle_current(&mut self, ctx: &Ctx) {
        if let Some(key) = self.selected(ctx).map(|r| r.key.clone()) {
            self.multi = true;
            if !self.checked.remove(&key) {
                self.checked.insert(key);
            }
        }
    }

    fn visible_checked(&self, ctx: &Ctx) -> Vec<String> {
        self.hits
            .iter()
            .map(|h| &ctx.snap.skills[h.index].key)
            .filter(|key| self.checked.contains(*key))
            .cloned()
            .collect()
    }

    fn batch_action(&self, operation: char, ctx: &Ctx) -> Vec<Action> {
        let command = match operation {
            't' => Command::Tags,
            'p' => Command::Presets,
            'd' => Command::Deploy,
            _ => return vec![],
        };
        self.batch_command(command, self.checked.iter().cloned().collect(), ctx)
    }
    fn render_state<'a>(
        &'a self,
        r: &SkillRecord,
        hit: &'a Hit,
        context: &'a str,
        searching: bool,
    ) -> SkillRenderState<'a> {
        SkillRenderState {
            checked: self.multi.then(|| self.checked.contains(&r.key)),
            update_available: self.updates.contains_key(&r.key),
            excerpt: hit
                .excerpt
                .as_ref()
                .filter(|_| searching)
                .map(|e| e.text.as_str()),
            terms: &hit.terms,
            context: searching.then_some(context),
            show_tags: true,
            hidden_group: self
                .hidden_group
                .as_ref()
                .map(|(kind, name)| (*kind, name.as_str())),
            show_match_details: searching,
        }
    }

    pub fn remember_checks(
        &mut self,
        results: &[(String, anyhow::Result<skills::ops::update::CheckResult>)],
    ) {
        for (key, result) in results {
            if let Ok(check) = result {
                if check.update_available {
                    self.updates.insert(key.clone(), check.remote.clone());
                } else {
                    self.updates.remove(key);
                }
            }
        }
    }

    #[cfg(test)]
    pub fn query(&self) -> String {
        self.search_panel.input.value().to_string()
    }
    pub fn paste(&mut self, text: &str, ctx: &Ctx) -> Vec<Action> {
        if self.focus != Focus::Input || self.overlay.is_open() {
            return vec![];
        }
        match self.search_panel.paste(text) {
            Ok(true) => {
                self.run_search(ctx, false);
                self.update_completion(ctx);
                vec![]
            }
            Ok(false) => vec![],
            Err(error) => vec![Action::Error(error)],
        }
    }

    pub fn input_focused(&self) -> bool {
        self.focus == Focus::Input
    }
    pub fn focus_input(&mut self) {
        self.choice_focus = ChoiceFocus::List;
        self.focus = Focus::Input;
    }
    pub fn focus_list(&mut self) {
        self.choice_focus = ChoiceFocus::List;
        self.focus = Focus::List;
    }
    pub fn set_query(&mut self, q: &str, ctx: &Ctx) {
        self.search_panel.input.set(q);
        self.search_panel.completion.close();
        self.run_search(ctx, false);
    }

    fn layout(&self, ctx: &Ctx) -> UiLayout {
        if self.is_picker() {
            UiLayout::Grid
        } else {
            ctx.settings.layout_for(self.layout_scope)
        }
    }

    fn selected<'a>(&self, ctx: &'a Ctx) -> Option<&'a SkillRecord> {
        self.grid
            .selected()
            .and_then(|i| self.hits.get(i))
            .and_then(|h| ctx.snap.get(&h.key))
    }

    /// Re-run the query. `keep` preserves the selected skill (after a rescan);
    /// typing always jumps back to the best match.
    fn run_search(&mut self, ctx: &Ctx, keep: bool) {
        let key = if keep {
            self.grid
                .selected()
                .and_then(|i| self.hits.get(i))
                .map(|h| h.key.clone())
        } else {
            None
        };
        let q = Query::parse(self.search_panel.input.value());
        self.hits = self
            .searcher
            .search(&ctx.snap.skills, &q)
            .into_iter()
            .filter(|hit| self.includes_record(&ctx.snap.skills[hit.index]))
            .collect();
        // Browsing groups sources; text searches preserve relevance across panels.
        if q.text.trim().is_empty() {
            self.hits.sort_by_key(|hit| {
                skills::repository::alias_of(&ctx.snap.skills[hit.index].key).is_some()
            });
        }
        if let Some((keys, _)) = self.panel.as_ref().or(self.scope.as_ref()) {
            self.hits
                .retain(|hit| keys.contains(&ctx.snap.skills[hit.index].key));
        }
        let idx = key.and_then(|k| {
            self.hits
                .iter()
                .position(|h| ctx.snap.skills[h.index].key == k)
        });
        self.grid
            .select(idx.or(if self.hits.is_empty() { None } else { Some(0) }));
        if !keep {
            self.preview_scroll = 0;
        }
    }

    fn selected_terms(&self) -> &[String] {
        self.grid
            .selected()
            .and_then(|i| self.hits.get(i))
            .map(|h| h.terms.as_slice())
            .unwrap_or(&[])
    }

    /// One step along a row, which in a single column is one step down the list.
    fn move_sel(&mut self, delta: i32) {
        self.grid.move_by(delta, self.hits.len());
        self.preview_scroll = 0;
    }

    /// One step down or up, which crosses a whole grid row when there are several.
    fn move_row(&mut self, delta: i32) {
        self.grid.move_rows(delta, self.hits.len());
        self.preview_scroll = 0;
    }

    /// Split layout has a pane to step into; grid layout opens a window over
    /// the results instead, which is the same thing every other page does.
    fn open_preview(&mut self, ctx: &Ctx) {
        if self.hits.is_empty() {
            return;
        }
        if self.layout(ctx) == UiLayout::Grid {
            if let Some(r) = self.selected(ctx) {
                self.overlay.open(r.key.clone());
            }
        } else {
            self.focus = Focus::Preview;
        }
    }

    fn scroll_preview(&mut self, delta: i32) {
        let max = (self.preview_lines as i32 - self.preview_height as i32).max(0);
        self.preview_scroll = (self.preview_scroll as i32 + delta).clamp(0, max) as u16;
    }

    pub fn on_check(
        &mut self,
        results: &[(String, anyhow::Result<skills::ops::update::CheckResult>)],
        _ctx: &Ctx,
    ) -> Vec<Action> {
        let mut acts = Vec::new();
        for (key, r) in results {
            acts.push(match r {
                Ok(c) if c.update_available => Action::Toast(format!(
                    "{key}: update available {} → {}",
                    c.installed
                        .as_deref()
                        .map(skills::meta::short_rev)
                        .unwrap_or("-"),
                    skills::meta::short_rev(&c.remote)
                )),
                Ok(c) => Action::Toast(format!(
                    "{key}: up to date ({})",
                    skills::meta::short_rev(&c.remote)
                )),
                Err(e) => Action::Error(format!("{key}: {e:#}")),
            });
        }
        acts
    }

    // ---- actions on the selected skill ------------------------------------

    #[allow(clippy::result_large_err)]
    fn need_present<'a>(&self, ctx: &'a Ctx, what: &str) -> Result<&'a SkillRecord, Action> {
        match self.selected(ctx) {
            Some(r) if r.status.is_present() => Ok(r),
            Some(r) => Err(Action::Error(format!(
                "{}: cannot {what} a {} skill",
                r.key,
                r.status.label()
            ))),
            None => Err(Action::Error("nothing selected".into())),
        }
    }

    fn act_tags(&self, ctx: &Ctx) -> Vec<Action> {
        if !ctx.settings.tags_enabled {
            return vec![];
        }
        match self.need_present(ctx, "tag") {
            Ok(r) => vec![Action::OpenModal(Box::new(Modal::batch_tags(
                vec![r.key.clone()],
                ctx,
            )))],
            Err(a) => vec![a],
        }
    }
    fn act_note(&self, ctx: &Ctx) -> Vec<Action> {
        match self.need_present(ctx, "annotate") {
            Ok(r) if r.source_kind() == "repository" => vec![Action::EditNote(r.key.clone())],
            Ok(_) => vec![Action::Error("Local skills do not store notes".into())],
            Err(a) => vec![a],
        }
    }
    fn act_deploy(&self, ctx: &Ctx) -> Vec<Action> {
        match self.need_present(ctx, "deploy") {
            Ok(r) => vec![Action::OpenModal(Box::new(Modal::batch_deploy(
                vec![r.key.clone()],
                ctx,
            )))],
            Err(a) => vec![a],
        }
    }
    fn act_rename(&self, ctx: &Ctx) -> Vec<Action> {
        match self.need_present(ctx, "rename") {
            Ok(r) => vec![Action::OpenModal(Box::new(Modal::rename(&r.key)))],
            Err(a) => vec![a],
        }
    }
    fn act_set_source(&self, ctx: &Ctx) -> Vec<Action> {
        match self.need_present(ctx, "set the source of") {
            Ok(r) => vec![Action::OpenModal(Box::new(Modal::set_source(
                &r.key,
                r.source.as_ref(),
            )))],
            Err(a) => vec![a],
        }
    }
    fn act_accept(&self, ctx: &Ctx) -> Vec<Action> {
        let Some(r) = self.selected(ctx) else {
            return vec![];
        };
        if !matches!(
            r.status,
            SkillStatus::Modified | SkillStatus::MissingBaseline
        ) {
            return vec![Action::Error(
                "accept applies to modified skills or skills without a baseline".into(),
            )];
        }
        let key = r.key.clone();
        vec![Action::Write(Box::new(move |ws| {
            edit::accept(ws, &key).map(|_| format!("baseline updated for {key}"))
        }))]
    }
    fn act_check(&self, ctx: &Ctx) -> Vec<Action> {
        let Some(r) = self.selected(ctx) else {
            return vec![];
        };
        if !r
            .source
            .as_ref()
            .is_some_and(skills::meta::Source::is_remote)
        {
            return vec![Action::Error(format!("{} has no remote source", r.key))];
        }
        vec![
            Action::Spawn(Task::Check(vec![r.key.clone()])),
            Action::Toast(format!("checking {}…", r.key)),
        ]
    }
    fn act_update(&self, ctx: &Ctx) -> Vec<Action> {
        let Some(r) = self.selected(ctx) else {
            return vec![];
        };
        if !r
            .source
            .as_ref()
            .is_some_and(skills::meta::Source::is_remote)
        {
            return vec![Action::Error(format!("{} has no remote source", r.key))];
        }
        vec![
            Action::Spawn(Task::Prepare(r.key.clone())),
            Action::Toast(format!("fetching {}…", r.key)),
        ]
    }
    fn act_remove(&self, ctx: &Ctx) -> Vec<Action> {
        let Some(r) = self.selected(ctx) else {
            return vec![];
        };
        vec![Action::OpenModal(Box::new(Modal::remove(&r.key)))]
    }
}

/// Drawing, split by band. `draw` itself only decides which of these run.
impl SearchView {
    fn input_title(&self, ctx: &Ctx) -> Line<'static> {
        Line::from(vec![
            Span::raw(" "),
            Span::raw({
                let total = ctx
                    .snap
                    .skills
                    .iter()
                    .filter(|r| self.includes_record(r))
                    .count();
                let local_total = ctx
                    .snap
                    .skills
                    .iter()
                    .filter(|r| {
                        self.includes_record(r) && skills::repository::alias_of(&r.key).is_none()
                    })
                    .count();
                let local_matches = self
                    .hits
                    .iter()
                    .filter(|h| {
                        skills::repository::alias_of(&ctx.snap.skills[h.index].key).is_none()
                    })
                    .count();
                format!(
                    "{local_matches}/{local_total} local · {}/{} repository installs",
                    self.hits.len() - local_matches,
                    total - local_total
                )
            }),
            Span::raw(" "),
        ])
    }

    /// The results, as a grid of cells that happens to be one column wide in
    /// the split layout. Drawing cell by cell rather than through `List` is what
    /// lets a card carry its own frame and lets several sit on a row.
    fn draw_results(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        let th = &ctx.settings.theme;
        let searching = !self.search_panel.input.value().trim().is_empty()
            && !Query::parse(self.search_panel.input.value())
                .text
                .is_empty();
        let inner = area;
        // Keep the chosen layout as a preference; a short pane needs enough
        // rows to navigate, and returns to that preference after a resize.
        let short = inner.height < 12;
        let layout = if short {
            UiLayout::Compact
        } else {
            self.layout(ctx)
        };
        self.rendered_layout = layout;
        let cards = layout == UiLayout::Grid;

        // A framed card is its content plus the border; the list is the same
        // content bare; a compact row is one line, two while an excerpt has
        // something to say.
        let cell_h = match layout {
            UiLayout::Grid => ctx.settings.layout.card_height,
            UiLayout::List => 4,
            UiLayout::Compact if searching && !short => 2,
            UiLayout::Compact => 1,
        };
        // One column is always kept back for the scrollbar so the column count
        // does not change under the user the moment the list grows past a screen.
        let usable = inner.width.saturating_sub(1);
        let cols = if cards { cols_for(usable, ctx) } else { 1 };
        let gap = if cols > 1 { 1 } else { 0 };
        let content = Rect {
            width: usable,
            ..inner
        };
        self.grid
            .layout(content, cols, cell_h, gap, self.hits.len());

        if self.hits.is_empty() {
            let msg = if ctx.snap.skills.is_empty() {
                "no skills in this root"
            } else {
                "no match"
            };
            f.render_widget(
                Paragraph::new(Span::styled(msg, th.dim())),
                Rect {
                    height: 1,
                    ..content
                },
            );
            self.list_track.clear();
            return;
        }

        let selected = self.grid.selected();
        let visible = if cards {
            self.grid.visible_with_partial()
        } else {
            self.grid.visible()
        };
        for i in visible {
            let Some(cell) = self.grid.cell(i) else {
                continue;
            };
            let h = &self.hits[i];
            let r = &ctx.snap.skills[h.index];
            let on = selected == Some(i);
            let presentation = SkillPresentation::managed(r, ctx);
            let context = h
                .fields
                .iter()
                .map(|field| field.label())
                .collect::<Vec<_>>()
                .join("·");
            let render_state = self.render_state(r, h, &context, searching);
            if cards {
                let ci = skill_frame(
                    f,
                    cell,
                    on,
                    self.focus == Focus::List && self.choice_focus == ChoiceFocus::List,
                    ctx,
                );
                let lines = presentation.card(ctx, ci.width as usize, &render_state);
                let style = if self.multi && self.checked.contains(&r.key) {
                    th.selected_unfocused()
                } else {
                    Style::default()
                };
                f.render_widget(Paragraph::new(lines).style(style), ci);
            } else if layout == UiLayout::List {
                // The card's lines without its frame or rule; the selection is
                // the marker and a background, as in any list.
                let style = if on {
                    if self.focus == Focus::List && self.choice_focus == ChoiceFocus::List {
                        th.selected()
                    } else {
                        th.selected_unfocused()
                    }
                } else {
                    Style::default()
                };
                let lines = presentation.list(
                    ctx,
                    cell.width.saturating_sub(1) as usize,
                    on,
                    &render_state,
                );
                f.render_widget(Paragraph::new(lines).style(style), cell);
            } else {
                let style = if on {
                    if self.focus == Focus::List && self.choice_focus == ChoiceFocus::List {
                        th.selected()
                    } else {
                        th.selected_unfocused()
                    }
                } else {
                    Style::default()
                };
                let lines = presentation.compact(
                    ctx,
                    cell.width.saturating_sub(2) as usize,
                    on,
                    &SkillRenderState {
                        show_match_details: searching && !short,
                        ..render_state
                    },
                );
                f.render_widget(Paragraph::new(lines).style(style), cell);
            }
        }

        // Item space here is grid rows, which is what the thumb is measuring and
        // what a click on the track has to land on.
        let vis = self.grid.visible_rows();
        if self.grid.grid_rows() > vis && inner.height > 0 {
            let track = Rect {
                x: inner.right().saturating_sub(1),
                y: inner.y,
                width: 1,
                height: inner.height,
            };
            self.list_track.set(track);
            let mut sb = ScrollbarState::new(self.grid.grid_rows())
                .position(selected.unwrap_or(0) / self.grid.cols())
                .viewport_content_length(vis);
            f.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .begin_symbol(None)
                    .end_symbol(None),
                track,
                &mut sb,
            );
        } else {
            self.list_track.clear();
        }
    }

    fn draw_preview(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        let th = &ctx.settings.theme;
        let block = th.block(" preview ", self.focus == Focus::Preview);
        let inner = block.inner(area);
        f.render_widget(block, area);
        self.preview_height = inner.height;
        let Some(r) = self.selected(ctx) else {
            f.render_widget(
                Paragraph::new(Span::styled("select a skill to preview", th.dim())),
                inner,
            );
            return;
        };
        let terms: Vec<String> = self.selected_terms().to_vec();
        let lines = preview_lines(r, ctx, &terms, inner.width as usize);
        let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
        self.preview_lines = paragraph.line_count(inner.width);
        let max = self
            .preview_lines
            .saturating_sub(inner.height as usize)
            .min(u16::MAX as usize) as u16;
        self.preview_scroll = self.preview_scroll.min(max);
        f.render_widget(paragraph.scroll((self.preview_scroll, 0)), inner);
        if self.preview_lines > inner.height as usize {
            let mut sb =
                ScrollbarState::new(self.preview_lines.saturating_sub(inner.height as usize))
                    .position(self.preview_scroll as usize);
            f.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .begin_symbol(None)
                    .end_symbol(None),
                area.inner(ratatui::layout::Margin {
                    vertical: 1,
                    horizontal: 0,
                }),
                &mut sb,
            );
        }
    }
}

impl View for SearchView {
    fn context_menu(&mut self, x: u16, y: u16, ctx: &Ctx) -> Option<Request> {
        self.menu_at(x, y, ctx)
    }
    fn context_execute(&mut self, target: &Target, command: Command, ctx: &Ctx) -> Vec<Action> {
        self.run_menu(target, command, ctx)
    }

    fn focus_root(&mut self) {
        self.focus = Focus::List;
    }

    fn status(&self, ctx: &Ctx) -> String {
        if !self.multi {
            return String::new();
        }
        let selected = self.checked.len();
        let hidden = selected.saturating_sub(self.visible_checked(ctx).len());
        if hidden > 0 {
            format!(" {selected} selected · {hidden} outside filter ")
        } else {
            format!(" {selected} selected ")
        }
    }
    fn refresh(&mut self, ctx: &Ctx) {
        self.searcher.configure(
            ctx.settings.search.clone(),
            skills::dict::Dictionaries::load(&ctx.ws.root, &ctx.settings.search.dictionaries),
        );
        self.searcher.index(&ctx.snap.skills);
        self.run_search(ctx, true);
        self.search_panel.completion.close();
        if self.focus == Focus::Input {
            self.update_completion(ctx);
        }
        // Missing fixed members remain selected until the user removes them.
        if self.preset.is_none() {
            self.checked.retain(|key| ctx.snap.get(key).is_some());
        }
        self.updates.retain(|key, remote| {
            ctx.snap.get(key).is_some_and(|r| {
                r.source.as_ref().is_some_and(|source| {
                    source.is_remote() && source.revision() != Some(remote.as_str())
                })
            })
        });
    }

    fn handle_key(&mut self, k: KeyEvent, ctx: &Ctx) -> Vec<Action> {
        if self.overlay.handle_key(k) {
            return vec![];
        }
        let k =
            if self.focus != Focus::Input && k.code == KeyCode::Char('q') && k.modifiers.is_empty()
            {
                KeyEvent::new(KeyCode::Esc, k.modifiers)
            } else {
                k
            };
        if k.code == KeyCode::Esc && self.focus == Focus::Preview {
            self.focus = Focus::List;
            return vec![];
        }
        // Popup handling precedes picker and multi-select shortcuts.
        if self.focus == Focus::Input
            && self.search_panel.completion.active()
            && matches!(
                k.code,
                KeyCode::Up | KeyCode::Down | KeyCode::Enter | KeyCode::Esc
            )
        {
            let event = self.search_panel.key(k);
            if event == SearchEvent::Accepted {
                self.run_search(ctx, false);
                if self.search_panel.input.value()[..self.search_panel.input.cursor_byte()]
                    .ends_with(':')
                {
                    self.update_completion(ctx);
                }
            }
            return vec![];
        }
        if self.focus == Focus::Input
            && k.code == KeyCode::Esc
            && !self.search_panel.input.is_empty()
        {
            self.search_panel.key(k);
            self.run_search(ctx, true);
            return vec![];
        }
        if k.code == KeyCode::Esc && self.focus == Focus::Input {
            self.focus = Focus::List;
            return vec![];
        }
        if self.is_picker() {
            if self.focus == Focus::Input && matches!(k.code, KeyCode::Tab | KeyCode::BackTab) {
                self.search_panel.completion.close();
                self.focus = Focus::List;
                self.choice_focus = if k.code == KeyCode::Tab {
                    ChoiceFocus::Apply
                } else {
                    ChoiceFocus::Cancel
                };
                return vec![];
            }
            if k.code == KeyCode::Esc {
                if self.focus == Focus::Preview {
                    self.focus = Focus::List;
                    return vec![];
                }
                return vec![Action::CloseModal];
            }
            if self.focus == Focus::List {
                let at_end =
                    self.hits.is_empty() || self.grid.selected() == Some(self.hits.len() - 1);
                if let Some(event) = self.choice_focus.key(k.code, at_end) {
                    return match event {
                        ChoiceEvent::Apply => self.apply_preset(ctx),
                        ChoiceEvent::Cancel => vec![Action::CloseModal],
                        ChoiceEvent::Moved => vec![],
                    };
                }
            }
            if self.focus == Focus::List && k.code == KeyCode::Enter && k.modifiers.is_empty() {
                self.toggle_current(ctx);
                return vec![];
            }
            if self.focus == Focus::List && k.code == KeyCode::Char('o') {
                self.open_preview(ctx);
                return vec![];
            }
            if self.focus == Focus::List && k.code == KeyCode::Char('a') && k.modifiers.is_empty() {
                return self.apply_preset(ctx);
            }
            if k.code == KeyCode::Enter && k.modifiers.contains(KeyModifiers::CONTROL) {
                return self.apply_preset(ctx);
            }
            if self.focus != Focus::Input {
                match k.code {
                    KeyCode::Char('/') => {
                        self.focus_input();
                        return vec![];
                    }
                    KeyCode::Char('m' | 't' | 'd' | 'p' | 'q') if k.modifiers.is_empty() => {
                        return vec![];
                    }
                    _ => {}
                }
            }
        }
        if self.multi && k.code == KeyCode::Esc {
            self.clear_selection();
            self.run_search(ctx, true);
            return vec![];
        }
        if self.multi
            && self.focus == Focus::Preview
            && matches!(k.code, KeyCode::Char('n' | 'u' | 'U'))
            && !k.modifiers.contains(KeyModifiers::CONTROL)
        {
            return vec![Action::Toast(
                "Finish or cancel multi-select before a single-skill action".into(),
            )];
        }
        if self.multi && self.focus == Focus::List {
            match k.code {
                KeyCode::Char(' ') => {
                    self.toggle_current(ctx);
                    return vec![];
                }
                KeyCode::Char('a') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.checked.extend(
                        self.hits
                            .iter()
                            .map(|h| ctx.snap.skills[h.index].key.clone()),
                    );
                    return vec![];
                }
                KeyCode::Char(op @ ('t' | 'd' | 'p')) if k.modifiers.is_empty() => {
                    if op == 't' && !ctx.settings.tags_enabled {
                        return vec![];
                    }
                    return self.batch_action(op, ctx);
                }
                KeyCode::Char('m') => {
                    self.clear_selection();
                    self.run_search(ctx, true);
                    return vec![];
                }
                KeyCode::Char('n' | 'r' | 's' | 'a' | 'u' | 'U' | 'x' | 'i' | 'M') => {
                    return vec![Action::Toast(
                        "Finish or cancel multi-select before a single-skill action".into(),
                    )];
                }
                _ => {}
            }
        }
        if k.code == KeyCode::Char('/') && self.focus != Focus::Input {
            self.focus_input();
            return vec![];
        }
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let mut acts = Vec::new();
        match self.focus {
            Focus::Input => match k.code {
                // Focus moves even with nothing to select: the list is where
                // the action keys live, and installing the first skill needs them.
                KeyCode::Enter | KeyCode::Down => self.focus = Focus::List,
                KeyCode::Up if !self.is_picker() => {
                    self.focus = Focus::List;
                    return vec![Action::BackToParent];
                }
                KeyCode::Up => self.move_sel(-1),
                KeyCode::Char('n') if ctrl => self.move_sel(1),
                KeyCode::Char('p') if ctrl => self.move_sel(-1),
                _ => {
                    let event = self.search_panel.key(k);
                    if event == SearchEvent::Changed {
                        self.run_search(ctx, false);
                    }
                    if matches!(event, SearchEvent::Changed | SearchEvent::CursorMoved) {
                        self.update_completion(ctx);
                    }
                }
            },
            Focus::List => match k.code {
                KeyCode::Esc => {
                    if !self.search_panel.input.is_empty() {
                        self.search_panel.input.clear();
                        self.run_search(ctx, true);
                    } else {
                        return vec![Action::BackToParent];
                    }
                }

                KeyCode::Down | KeyCode::Char('j') => self.move_row(1),
                KeyCode::Up
                    if self
                        .grid
                        .selected()
                        .is_none_or(|index| index < self.grid.cols()) =>
                {
                    self.focus_input()
                }
                KeyCode::Up | KeyCode::Char('k') => self.move_row(-1),
                KeyCode::PageDown | KeyCode::Char('f') if k.code == KeyCode::PageDown || ctrl => {
                    self.move_sel(self.grid.page())
                }
                KeyCode::PageUp | KeyCode::Char('b') if k.code == KeyCode::PageUp || ctrl => {
                    self.move_sel(-self.grid.page())
                }
                KeyCode::Home | KeyCode::Char('g') => self.grid.first(self.hits.len()),
                KeyCode::End | KeyCode::Char('G') => self.grid.last(self.hits.len()),
                // In multi-column grids, horizontal keys select neighbors;
                // in a single column, Right opens the selected skill's preview.
                KeyCode::Right | KeyCode::Char('l') if self.grid.cols() > 1 => self.move_sel(1),
                KeyCode::Left | KeyCode::Char('h') if self.grid.cols() > 1 => self.move_sel(-1),
                KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                    acts = self.skill_command(Command::Open, ctx)
                }
                KeyCode::Char('t') if ctx.settings.tags_enabled => {
                    acts = if self.multi {
                        self.batch_action('t', ctx)
                    } else {
                        self.skill_command(Command::Tags, ctx)
                    }
                }
                KeyCode::Char('n') => acts = self.skill_command(Command::Note, ctx),
                KeyCode::Char('d') => {
                    acts = if self.multi {
                        self.batch_action('d', ctx)
                    } else {
                        self.skill_command(Command::Deploy, ctx)
                    }
                }
                KeyCode::Char('p') => acts = self.skill_command(Command::Presets, ctx),
                KeyCode::Char('r') => acts = self.skill_command(Command::Rename, ctx),
                KeyCode::Char('s') => acts = self.skill_command(Command::Source, ctx),
                KeyCode::Char('a') => acts = self.skill_command(Command::Accept, ctx),
                KeyCode::Char('m') => self.multi = true,
                KeyCode::Char('u') => acts = self.skill_command(Command::Check, ctx),
                KeyCode::Char('U') => acts = self.skill_command(Command::Update, ctx),
                KeyCode::Char('x') => acts = self.skill_command(Command::Remove, ctx),
                KeyCode::Char('v') | KeyCode::Char('V') => {
                    // Cycle from most to least room per skill.
                    if self.is_picker() {
                        return vec![];
                    }
                    let layout = self.layout(ctx).next();
                    acts.push(Action::SetLayout {
                        scope: self.layout_scope,
                        layout,
                    });
                    self.overlay.close();
                }
                KeyCode::Char('i') => acts = vec![Action::OpenModal(Box::new(Modal::install()))],
                _ => {}
            },
            Focus::Preview => match k.code {
                KeyCode::Char('e') => {
                    if let Some(record) = self.selected(ctx) {
                        self.overlay.open(record.key.clone());
                        self.overlay.expand_fields();
                    }
                }
                KeyCode::Esc | KeyCode::Left | KeyCode::Char('h') => {
                    self.focus = Focus::List;
                }
                KeyCode::Char('q') => self.focus = Focus::Input,
                KeyCode::Down | KeyCode::Char('j') => self.scroll_preview(1),
                KeyCode::Up | KeyCode::Char('k') => self.scroll_preview(-1),
                KeyCode::PageDown | KeyCode::Char(' ') => {
                    self.scroll_preview(self.preview_height as i32 - 2)
                }
                KeyCode::PageUp => self.scroll_preview(-(self.preview_height as i32 - 2)),
                KeyCode::Char('d') if ctrl => self.scroll_preview(self.preview_height as i32 / 2),
                KeyCode::Char('u') if ctrl => {
                    self.scroll_preview(-(self.preview_height as i32 / 2))
                }
                KeyCode::Home | KeyCode::Char('g') => self.preview_scroll = 0,
                KeyCode::End | KeyCode::Char('G') => self.scroll_preview(i32::MAX / 2),
                KeyCode::Char('t') if ctx.settings.tags_enabled => {
                    acts = if self.multi {
                        self.batch_action('t', ctx)
                    } else {
                        self.skill_command(Command::Tags, ctx)
                    }
                }
                KeyCode::Char('n') => acts = self.skill_command(Command::Note, ctx),
                KeyCode::Char('d') => {
                    acts = if self.multi {
                        self.batch_action('d', ctx)
                    } else {
                        self.skill_command(Command::Deploy, ctx)
                    }
                }
                KeyCode::Char('u') => acts = self.skill_command(Command::Check, ctx),
                KeyCode::Char('U') => acts = self.skill_command(Command::Update, ctx),
                _ => {}
            },
        }
        acts
    }

    fn handle_mouse(&mut self, m: MouseEvent, ctx: &Ctx) -> Vec<Action> {
        if self.overlay.handle_mouse(m, ctx) {
            return vec![];
        }
        if self.is_picker()
            && matches!(m.kind, MouseEventKind::Down(MouseButton::Left))
            && !self.area.contains((m.column, m.row).into())
        {
            return vec![Action::CloseModal];
        }
        if self.focus == Focus::Input {
            let (consumed, accepted) = self.search_panel.mouse_completion(m);
            if consumed {
                if accepted {
                    self.run_search(ctx, false);
                    if self.search_panel.input.value()[..self.search_panel.input.cursor_byte()]
                        .ends_with(':')
                    {
                        self.update_completion(ctx);
                    }
                }
                return vec![];
            }
        }
        if matches!(m.kind, MouseEventKind::Down(MouseButton::Left))
            && let Some((_, command)) = self
                .batch_buttons
                .iter()
                .find(|(rect, _)| rect.contains((m.column, m.row).into()))
        {
            match *command {
                'm' => {
                    self.multi = true;
                    self.focus = Focus::List;
                    return vec![];
                }
                'a' => {
                    self.checked.extend(
                        self.hits
                            .iter()
                            .map(|h| ctx.snap.skills[h.index].key.clone()),
                    );
                    return vec![];
                }
                'c' => {
                    self.focus = Focus::List;
                    self.choice_focus = ChoiceFocus::Apply;
                    return self.apply_preset(ctx);
                }
                'e' => {
                    if self.is_picker() {
                        return vec![Action::CloseModal];
                    }
                    self.clear_selection();
                    self.run_search(ctx, true);
                    return vec![];
                }
                operation => return self.batch_action(operation, ctx),
            }
        }
        if matches!(m.kind, MouseEventKind::Down(MouseButton::Left)) {
            self.choice_focus = ChoiceFocus::List;
        }
        let at = (m.column, m.row).into();
        if let Some(d) = wheel(&m, ctx) {
            if self.preview_rect.contains(at) {
                self.focus = Focus::Preview;
                self.choice_focus = ChoiceFocus::List;
                self.scroll_preview(d);
            } else if self.list_rect.contains(at) {
                // A wheel notch is a row of cards, however many are on it.
                self.focus = Focus::List;
                self.choice_focus = ChoiceFocus::List;
                self.move_row(d.signum());
            }
            return vec![];
        }
        // The track is inside `list_rect`, so it has to claim the event before
        // the cell hit-test below turns it into a click on a card.
        let dragging = matches!(m.kind, MouseEventKind::Drag(MouseButton::Left));
        let pressing = matches!(m.kind, MouseEventKind::Down(MouseButton::Left));
        if (pressing && self.list_track.hit(m.column, m.row)) || (dragging && self.track_drag) {
            self.track_drag = true;
            self.focus = Focus::List;
            if let Some(r) = self.list_track.index_at(m.row, self.grid.grid_rows()) {
                self.grid.select_row(r);
                self.preview_scroll = 0;
            }
            return vec![];
        }
        if !dragging {
            self.track_drag = false;
        }
        if let MouseEventKind::Down(MouseButton::Left) = m.kind {
            if self.search_panel.click_input(m.column, m.row) {
                self.focus = Focus::Input;
                self.update_completion(ctx);
            } else if self.preview_rect.contains(at) {
                self.focus = Focus::Preview;
            } else if self.list_rect.contains(at) {
                self.focus = Focus::List;
                if let Some((index, double)) = self.grid.click(m.column, m.row) {
                    self.preview_scroll = 0;
                    let marker = self.grid.cell(index).is_some_and(|cell| {
                        let (x, y) = if self.rendered_layout == UiLayout::Grid {
                            (cell.x + 2, cell.y + 1)
                        } else {
                            (cell.x + 2, cell.y)
                        };
                        m.row == y
                            && m.column >= x
                            && m.column < x + ctx.settings.layout.marker_width as u16
                    });
                    if self.multi || marker {
                        if !double {
                            self.toggle_current(ctx);
                        }
                    } else if double {
                        self.open_preview(ctx);
                    }
                }
            }
        }
        vec![]
    }

    fn draw(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        self.draw_with_content_header(f, area, ctx, 0, |_, _| {});
    }

    fn hints(&self) -> Hints {
        if self.is_picker()
            && self.focus == Focus::List
            && self.choice_focus != ChoiceFocus::List
            && !self.overlay.is_open()
        {
            return &[
                ("←→", "buttons"),
                ("Tab/Shift+Tab", "next / previous"),
                ("↑", "list"),
                ("Enter/Space", "activate"),
                ("Esc/q", "cancel"),
            ];
        }
        if let Some(hints) = self.overlay.hints() {
            return hints;
        }
        if self.focus == Focus::Input {
            if self.search_panel.completion.active() {
                return &[
                    ("↑↓", "suggestions"),
                    ("Enter", "complete"),
                    ("Esc", "close suggestions"),
                ];
            }
            if self.is_picker() {
                return &[
                    ("Enter/↓", "results"),
                    ("Tab/Shift+Tab", "list / buttons"),
                    ("Esc", "cancel"),
                ];
            }
            if self.multi {
                return &[("Enter/↓", "results"), ("Esc", "cancel selection")];
            }
            if self.panel.is_some() {
                return &[("Enter/↓", "results"), ("Esc", "results")];
            }
        }
        if self.is_picker() && self.focus == Focus::Preview {
            return &[
                ("↑↓/j/k", "scroll"),
                ("e", "expand fields"),
                ("Esc", "results"),
                ("/", "filter"),
            ];
        }
        if self.is_picker() {
            return &[
                ("Enter/Space", "select"),
                ("Ctrl+A", "select all results"),
                ("/", "search"),
                ("o", "preview"),
                ("Tab/Shift+Tab", "list / buttons"),
                ("Esc", "cancel"),
            ];
        }
        if self.multi && self.focus == Focus::List {
            return &[
                ("Space", "select"),
                ("Ctrl+A", "select all results"),
                ("t", "tags"),
                ("d", "deploy"),
                ("p", "preset (all selected)"),
                ("/", "filter"),
                ("Enter", "preview"),
                ("Esc", "cancel selection"),
            ];
        }
        match self.focus {
            Focus::Input if self.search_panel.completion.active() => &[
                ("↑↓", "suggestions"),
                ("Enter", "complete"),
                ("Esc", "close suggestions"),
            ],
            Focus::Input => &[
                ("↑↓", "select"),
                ("↓", "list"),
                ("Enter", "list"),
                ("Esc", "clear/results"),
                ("Ctrl-G", "help"),
            ],
            Focus::List => &[
                ("Enter", "preview"),
                ("Esc/q", "clear/back"),
                ("m", "multi-select"),
                ("t", "tags"),
                ("n", "note"),
                ("d", "deploy"),
                ("p", "add to preset"),
                ("r", "rename"),
                ("s", "source"),
                ("a", "accept repo changes"),
                ("u/U", "check/update"),
                ("x", "remove"),
                ("i", "install"),
                ("R", "repos"),
                ("v/V", "layout"),
            ],
            Focus::Preview => &[
                ("j/k", "scroll"),
                ("e", "expand fields"),
                ("Esc", "back"),
                ("t", "tags"),
                ("n", "note"),
                ("d", "deploy"),
                ("/", "search"),
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::widgets::width;

    #[test]
    fn preset_picker_refreshes_indices_without_dropping_pending_members() {
        let root = skills::ops::DownloadDir::new("preset-picker-refresh").unwrap();
        skills::config::Config {
            agents: vec![],
            ..Default::default()
        }
        .save(root.path())
        .unwrap();
        for name in ["alpha", "beta"] {
            std::fs::create_dir_all(root.path().join(name)).unwrap();
            std::fs::write(
                root.path().join(name).join("SKILL.md"),
                format!("---\nname: {name}\ndescription: {name}\n---\nBody"),
            )
            .unwrap();
        }
        let ws = skills::Workspace::open(root.path()).unwrap();
        let snap = ws.scan().unwrap();
        let settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &settings,
        };
        let mut picker = SearchView::preset_members("daily", &ctx);
        picker.focus_list();
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        picker.handle_key(key(KeyCode::Tab), &ctx);
        assert_eq!(picker.choice_focus, ChoiceFocus::Apply);
        assert!(picker.handle_key(key(KeyCode::Enter), &ctx).is_empty());
        picker.handle_key(key(KeyCode::Tab), &ctx);
        assert_eq!(picker.choice_focus, ChoiceFocus::Cancel);
        picker.handle_key(key(KeyCode::Up), &ctx);
        assert_eq!(picker.choice_focus, ChoiceFocus::List);

        picker.handle_key(key(KeyCode::Enter), &ctx);
        assert!(!picker.overlay.is_open());
        assert_eq!(picker.checked.len(), 1);
        picker.handle_key(key(KeyCode::Enter), &ctx);
        assert!(picker.checked.is_empty());
        picker.checked.insert("alpha".into());
        picker.set_query("beta", &ctx);
        picker.focus_list();
        let mut modal = Modal::PresetSkills(Box::new(picker));
        std::fs::remove_dir_all(root.path().join("alpha")).unwrap();
        let newer = ws.scan().unwrap();
        let ctx = Ctx {
            snap: &newer,
            ..ctx
        };
        modal.refresh(&ctx);
        modal.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE), &ctx);
        let Modal::PresetSkills(picker) = modal else {
            unreachable!()
        };
        assert_eq!(picker.selected(&ctx).unwrap().key, "beta");
        assert_eq!(
            picker.checked,
            BTreeSet::from(["alpha".into(), "beta".into()])
        );
    }

    #[test]
    fn picker_mouse_wheel_returns_focus_from_buttons_to_results() {
        let root = skills::ops::DownloadDir::new("picker-mouse-focus").unwrap();
        skills::config::Config {
            agents: vec![],
            ..Default::default()
        }
        .save(root.path())
        .unwrap();
        std::fs::create_dir_all(root.path().join("alpha")).unwrap();
        std::fs::write(
            root.path().join("alpha/SKILL.md"),
            "---\nname: alpha\ndescription: alpha\n---\nBody",
        )
        .unwrap();
        let ws = skills::Workspace::open(root.path()).unwrap();
        let snap = ws.scan().unwrap();
        let settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &settings,
        };
        let mut picker = SearchView::preset_members("daily", &ctx);
        picker.focus_list();
        picker.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &ctx);
        assert_eq!(picker.choice_focus, ChoiceFocus::Apply);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        terminal.draw(|f| picker.draw(f, f.area(), &ctx)).unwrap();
        picker.handle_mouse(
            MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: picker.list_rect.x + 1,
                row: picker.list_rect.y + 1,
                modifiers: KeyModifiers::NONE,
            },
            &ctx,
        );
        assert_eq!(picker.focus, Focus::List);
        assert_eq!(picker.choice_focus, ChoiceFocus::List);
        assert!(picker.hints().iter().any(|(key, _)| *key == "Enter/Space"));
    }

    #[test]
    fn preset_editor_applies_cross_filter_changes_and_preserves_concurrent_additions() {
        let root = skills::ops::DownloadDir::new("preset-fixed-picker").unwrap();
        skills::config::Config {
            agents: vec![],
            ..Default::default()
        }
        .save(root.path())
        .unwrap();
        for name in ["alpha", "beta", "gamma", "delta"] {
            std::fs::create_dir_all(root.path().join(name)).unwrap();
            std::fs::write(
                root.path().join(name).join("SKILL.md"),
                format!("---\nname: {name}\ndescription: {name}\n---\nBody"),
            )
            .unwrap();
        }
        let ws = skills::Workspace::open(root.path()).unwrap();
        ws.presets
            .save(&skills::preset::Preset {
                name: "daily".into(),
                skills: vec!["alpha".into()],
                ..Default::default()
            })
            .unwrap();
        let snap = ws.scan().unwrap();
        let settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &settings,
        };
        let mut picker = SearchView::preset_members("daily", &ctx);
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        for name in ["alpha", "beta", "gamma"] {
            picker.set_query(name, &ctx);
            picker.focus_list();
            picker.handle_key(key(KeyCode::Char(' ')), &ctx);
        }
        picker.set_query("beta", &ctx);
        picker.focus_list();
        assert_eq!(picker.visible_checked(&ctx), ["beta"]);
        assert!(picker.status(&ctx).contains("2 selected"));
        let mut newer = ws.presets.load("daily").unwrap().unwrap();
        newer.skills.push("delta".into());
        ws.presets.save(&newer).unwrap();
        let Action::BatchMeta(write, keys) =
            picker.handle_key(key(KeyCode::Char('a')), &ctx).remove(0)
        else {
            panic!("expected complete member edit");
        };
        assert_eq!(keys, ["alpha", "beta", "gamma"]);
        let (_, intent) = write(&ws).unwrap();
        assert!(intent.is_some());
        assert_eq!(
            ws.presets.load("daily").unwrap().unwrap().members(),
            ["beta", "delta", "gamma"]
        );

        let mut panel = SearchView::panel(
            SkillPanelOptions::new(
                vec!["alpha".into(), "beta".into()],
                "Preset: daily".into(),
                LayoutScope::Presets,
            ),
            &ctx,
        );
        panel.multi = true;
        panel.checked.extend(["alpha".into(), "beta".into()]);
        panel.set_query("beta", &ctx);
        assert_eq!(panel.panel_keys(&ctx), ["alpha", "beta"]);
    }

    #[test]
    fn shared_panels_read_scoped_layouts_and_emit_session_updates() {
        use crate::tui::settings::{RuntimeSettings, SessionSettings};
        let root = skills::ops::DownloadDir::new("scoped-panel-layout").unwrap();
        let ws = skills::Workspace::open(root.path()).unwrap();
        let snap = ws.scan().unwrap();
        let mut session = SessionSettings::default();
        session.set_layout(LayoutScope::Library, UiLayout::Compact);
        session.set_layout(LayoutScope::Tags, UiLayout::List);
        let settings = RuntimeSettings::resolve(&ws.config, &session);
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &settings,
        };
        let mut panel = SearchView::panel(
            SkillPanelOptions::new(vec![], "Tag skills".into(), LayoutScope::Tags),
            &ctx,
        );
        assert_eq!(panel.layout(&ctx), UiLayout::List);
        assert_eq!(SearchView::default().layout(&ctx), UiLayout::Compact);
        panel.focus_list();
        let actions = panel.handle_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE), &ctx);
        assert!(matches!(
            actions.as_slice(),
            [Action::SetLayout {
                scope: LayoutScope::Tags,
                layout: UiLayout::Compact
            }]
        ));
        // The supplied snapshot is authoritative until App resolves the action.
        assert_eq!(panel.layout(&ctx), UiLayout::List);
        session.set_layout(LayoutScope::Tags, UiLayout::Compact);
        let settings = RuntimeSettings::resolve(&ws.config, &session);
        let ctx = Ctx {
            settings: &settings,
            ..ctx
        };
        assert_eq!(panel.layout(&ctx), UiLayout::Compact);
        let mut picker = SearchView::tag_members("events", &ctx);
        picker.focus_list();
        assert_eq!(picker.layout(&ctx), UiLayout::Grid);
        assert!(
            picker
                .handle_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE), &ctx)
                .is_empty()
        );
    }

    #[test]
    fn library_hides_problem_records_without_removing_them_from_repair_views() {
        let root =
            std::env::temp_dir().join(format!("skills-library-health-{}", std::process::id()));
        std::fs::create_dir_all(root.join("valid")).unwrap();
        std::fs::create_dir_all(root.join("invalid")).unwrap();
        std::fs::write(
            root.join("valid/SKILL.md"),
            "---\nname: valid\ndescription: valid skill\n---\nBody",
        )
        .unwrap();
        skills::config::Config {
            agents: vec![],
            ..Default::default()
        }
        .save(&root)
        .unwrap();
        let ws = skills::Workspace::open(&root).unwrap();
        ws.meta
            .save(
                "missing",
                &skills::meta::SkillMeta {
                    source: Some(skills::meta::Source::Git {
                        url: "https://example.com/repo".into(),
                        branch: None,
                        subpath: None,
                        revision: None,
                    }),
                    ..Default::default()
                },
            )
            .unwrap();
        let text = std::fs::read_to_string(ws.meta.path("missing")).unwrap();
        std::fs::write(
            ws.meta.path("corrupt-missing"),
            format!("{text}\n[skills.corrupt-missing]\nnote = 42\n"),
        )
        .unwrap();
        let snap = ws.scan().unwrap();
        let theme = crate::tui::theme::Theme::default();
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &{
                let mut settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
                settings.theme = theme;
                settings
            },
        };
        let mut view = SearchView::default();
        view.refresh(&ctx);
        assert_eq!(snap.skills.len(), 4);
        assert_eq!(view.hits.len(), 1);
        assert_eq!(view.selected(&ctx).unwrap().key, "valid");
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(110, 34)).unwrap();
        terminal.draw(|f| view.draw(f, f.area(), &ctx)).unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(screen.contains("1/1 local"));
        view.set_query("invalid", &ctx);
        assert!(view.hits.is_empty());
        view.set_query("", &ctx);
        view.select_scope(vec!["invalid".into()], "Repair".into(), None, &ctx);
        assert_eq!(view.selected(&ctx).unwrap().key, "invalid");
        let mut picker = SearchView::preset_members("example", &ctx);
        assert_eq!(picker.hits.len(), 1);
        picker.focus_input();
        assert!(!picker.hints().iter().any(|(k, _)| k.contains("Space")));
        picker.focus_list();
        assert!(picker.hints().iter().any(|(k, _)| k.contains("Space")));
        picker.focus = Focus::Preview;
        assert!(!picker.hints().iter().any(|(k, _)| k.contains("Space")));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn short_result_panes_use_one_line_and_restore_the_chosen_layout() {
        let root = std::env::temp_dir().join(format!("skills-short-pane-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        skills::config::Config {
            agents: vec![],
            ..Default::default()
        }
        .save(&root)
        .unwrap();
        for n in 0..12 {
            let path = root.join(format!("printer-{n:02}"));
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(
                path.join("SKILL.md"),
                format!("---\nname: printer-{n:02}\ndescription: shared tools\n---\nshared body"),
            )
            .unwrap();
        }
        let ws = skills::Workspace::open(&root).unwrap();
        let snap = ws.scan().unwrap();
        let theme = crate::tui::theme::Theme::default();
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &{
                let mut settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
                settings.theme = theme;
                settings
            },
        };
        let mut view = SearchView::default();
        view.refresh(&ctx);
        let mut session = crate::tui::settings::SessionSettings::default();
        session.set_layout(LayoutScope::Library, UiLayout::List);
        let settings = crate::tui::settings::RuntimeSettings::resolve(&ws.config, &session);
        let ctx = Ctx {
            settings: &settings,
            ..ctx
        };
        view.set_query("shared", &ctx);
        view.grid.select(Some(3));
        let selected = view.selected(&ctx).unwrap().key.clone();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|f| view.draw(f, Rect::new(0, 1, 80, 22), &ctx))
            .unwrap();
        assert_eq!(view.rendered_layout, UiLayout::Compact);
        assert_eq!(view.layout(&ctx), UiLayout::List);
        assert!(view.grid.visible().len() >= 4);
        assert_eq!(view.grid.cell(3).unwrap().height, 1);
        assert_eq!(view.selected(&ctx).unwrap().key, selected);
        let shown: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(shown.matches("printer-").count() >= 4);

        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
        terminal
            .draw(|f| view.draw(f, Rect::new(0, 1, 120, 38), &ctx))
            .unwrap();
        assert_eq!(view.rendered_layout, UiLayout::List);
        assert_eq!(view.grid.cell(3).unwrap().height, 4);
        assert_eq!(view.selected(&ctx).unwrap().key, selected);

        session.set_layout(LayoutScope::Library, UiLayout::Grid);
        let settings = crate::tui::settings::RuntimeSettings::resolve(&ws.config, &session);
        let ctx = Ctx {
            settings: &settings,
            ..ctx
        };

        view.set_query("", &ctx);
        view.grid.first(view.hits.len());
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(124, 20)).unwrap();
        let area = Rect::new(0, 0, 124, 20);
        terminal.draw(|f| view.draw(f, area, &ctx)).unwrap();
        assert_eq!(view.grid.visible(), 0..6);
        let peek = view.grid.cell(6).unwrap();
        assert_eq!(peek.height, 3);
        let buffer = terminal.backend().buffer();
        let row = |y| {
            (0..124)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        };
        assert!(row(peek.y + 1).contains("printer-06"));
        assert!(row(area.bottom() - 1).contains("↓ more · 1–6 / 12"));
        assert_eq!(buffer[(peek.x, peek.y + 1)].symbol(), "│");
        assert_eq!(buffer[(0, area.bottom() - 1)].symbol(), "╰");
        assert_eq!(buffer[(2, area.bottom() - 1)].symbol(), "─");
        assert_eq!(buffer[(123, area.bottom() - 1)].symbol(), "╯");
        assert_eq!(area.bottom(), buffer.area.bottom());

        // Clicking the peek scrolls it fully into view, including after resize.
        assert_eq!(view.grid.click(peek.x + 2, peek.y + 1).unwrap().0, 6);
        for height in 14..20 {
            terminal
                .draw(|f| view.draw_results(f, Rect::new(0, 0, 124, height), &ctx))
                .unwrap();
            assert_eq!(
                view.grid.cell(6).unwrap().height,
                ctx.settings.layout.card_height
            );
        }
        view.grid.last(view.hits.len());
        terminal.draw(|f| view.draw(f, area, &ctx)).unwrap();
        let bottom: String = (0..124)
            .map(|x| terminal.backend().buffer()[(x, area.bottom() - 1)].symbol())
            .collect();
        assert!(bottom.contains("7–12 / 12"));
        assert!(!bottom.contains("more"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn text_search_preserves_relevance_across_local_and_repository_results() {
        let root =
            std::env::temp_dir().join(format!("skills-search-groups-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        skills::config::Config {
            agents: vec![],
            ..Default::default()
        }
        .save(&root)
        .unwrap();
        for name in ["printer", "repos/sampleorg--kit/calendar"] {
            let path = root.join(name);
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(
                path.join("SKILL.md"),
                format!(
                    "---\nname: {}\ndescription: shared tools\n---\nBody mentions calendar",
                    name.rsplit('/').next().unwrap()
                ),
            )
            .unwrap();
        }
        let ws = skills::Workspace::open(&root).unwrap();
        let key = "repos/sampleorg--kit/calendar";
        ws.meta
            .save(
                key,
                &skills::meta::SkillMeta {
                    source: Some(skills::meta::Source::Git {
                        url: "https://github.com/sampleorg/kit".into(),
                        branch: None,
                        subpath: Some("calendar".into()),
                        revision: None,
                    }),
                    baseline: Some(skills::meta::Baseline {
                        hash: skills::hash::hash_directory(&root.join(key)).unwrap(),
                        hash_algo: skills::hash::HASH_ALGO,
                    }),
                    ..Default::default()
                },
            )
            .unwrap();
        let snap = ws.scan().unwrap();
        let theme = crate::tui::theme::Theme::default();
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &{
                let mut settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
                settings.theme = theme;
                settings
            },
        };
        let mut view = SearchView::default();
        view.refresh(&ctx);
        for query in ["", "shared"] {
            view.set_query(query, &ctx);
            let keys: Vec<_> = view
                .hits
                .iter()
                .map(|h| snap.skills[h.index].key.as_str())
                .collect();
            assert_eq!(keys, ["printer", "repos/sampleorg--kit/calendar"]);
        }
        view.set_query("calendar", &ctx);
        assert_eq!(view.hits.len(), 2);
        assert_eq!(
            view.selected(&ctx).unwrap().key,
            "repos/sampleorg--kit/calendar"
        );
        view.focus_list();
        assert!(
            view.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE), &ctx)
                .is_empty()
        );
        assert_eq!(view.focus, Focus::List);
        assert!(view.query().is_empty());
        let mut session = crate::tui::settings::SessionSettings::default();
        session.set_layout(LayoutScope::Library, UiLayout::List);
        let settings = crate::tui::settings::RuntimeSettings::resolve(&ws.config, &session);
        let ctx = Ctx {
            settings: &settings,
            ..ctx
        };
        view.focus = Focus::Preview;
        view.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE), &ctx);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| view.draw(f, f.area(), &ctx)).unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(
            screen.contains("e fields · Esc closes"),
            "expanded fields must render over split layouts"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn multi_select_preserves_all_targets_across_filters_and_keeps_scope() {
        let root = std::env::temp_dir().join(format!("skills-multi-select-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        skills::config::Config {
            agents: vec![],
            ..Default::default()
        }
        .save(&root)
        .unwrap();
        for name in ["printer", "calendar", "document"] {
            let path = root.join(name);
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(
                path.join("SKILL.md"),
                format!("---\nname: {name}\ndescription: tools\n---\nBody"),
            )
            .unwrap();
        }
        let ws = skills::Workspace::open(&root).unwrap();
        let snap = ws.scan().unwrap();
        let theme = crate::tui::theme::Theme::default();
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &{
                let mut settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
                settings.theme = theme;
                settings
            },
        };
        let mut view = SearchView::default();
        view.refresh(&ctx);
        view.focus_list();
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        view.handle_key(key(KeyCode::Char('m')), &ctx);
        assert!(view.multi);
        assert!(view.checked.is_empty());
        view.handle_key(
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL),
            &ctx,
        );
        assert_eq!(view.checked.len(), 3);
        view.set_query("printer", &ctx);
        assert_eq!(view.visible_checked(&ctx), vec!["printer"]);
        assert_eq!(view.checked.len(), 3);
        view.set_query("no-match-at-all", &ctx);
        assert!(matches!(
            view.batch_action('t', &ctx).as_slice(),
            [Action::OpenModal(_)]
        ));
        view.set_query("", &ctx);
        assert_eq!(view.visible_checked(&ctx).len(), 3);
        view.handle_key(key(KeyCode::Esc), &ctx);
        assert!(!view.multi);
        assert!(view.checked.is_empty());
        view.select_scope(vec!["printer".into()], "Preset: tools".into(), None, &ctx);
        assert_eq!(view.hits.len(), 1);
        view.handle_key(key(KeyCode::Char(' ')), &ctx);
        assert_eq!(view.visible_checked(&ctx), vec!["printer"]);
        let record = ctx.snap.get("printer").unwrap();
        let presentation = SkillPresentation::managed(record, &ctx);
        let normal = presentation.card(&ctx, 40, &SkillRenderState::default());
        let hit = view.hits.first().unwrap();
        let state = view.render_state(record, hit, "", false);
        let checked = presentation.card(&ctx, 40, &state);
        assert!(checked[0].to_string().starts_with("[✓] printer"));
        assert_eq!(checked[0].width(), normal[0].width());
        assert_eq!(
            width(&normal[0].to_string()[..normal[0].to_string().find("printer").unwrap()]),
            ctx.settings.layout.marker_width
        );
        for state in [SkillRenderState::default(), state] {
            let compact = presentation.compact(&ctx, 60, true, &state);
            let text = compact[0].to_string();
            assert_eq!(
                width(&text[..text.find("printer").unwrap()]),
                2 + ctx.settings.layout.marker_width
            );
            if state.checked == Some(true) {
                assert!(text.starts_with("▸ [✓] printer"));
            }
        }
        view.handle_key(key(KeyCode::Esc), &ctx);
        assert_eq!(view.hits.len(), 3);
        view.batch_finished(&["printer".into()]);
        assert_eq!(view.visible_checked(&ctx), vec!["printer"]);
        view.batch_finished(&[]);
        assert!(!view.multi);
        ws.presets
            .save(&skills::preset::Preset {
                name: "office".into(),
                skills: vec!["document".into()],
                ..Default::default()
            })
            .unwrap();
        let mut picker = SearchView::preset_members("office", &ctx);
        picker.set_query("printer", &ctx);
        picker.focus_list();
        picker.handle_key(key(KeyCode::Char(' ')), &ctx);
        assert_eq!(
            ws.presets.load("office").unwrap().unwrap().skills,
            vec!["document"]
        );
        let mut actions = picker.handle_key(key(KeyCode::Char('a')), &ctx);
        let Action::BatchMeta(write, _) = actions.remove(0) else {
            panic!("not a staged preset apply")
        };
        let (_, intent) = write(&ws).unwrap();
        assert!(intent.is_some());
        assert_eq!(
            ws.presets.load("office").unwrap().unwrap().skills,
            vec!["document", "printer"]
        );
        picker.handle_key(key(KeyCode::Char(' ')), &ctx);
        assert!(matches!(
            picker.handle_key(key(KeyCode::Esc), &ctx).as_slice(),
            [Action::CloseModal]
        ));
        assert_eq!(ws.presets.load("office").unwrap().unwrap().skills.len(), 2);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn completions_follow_panel_inventory_not_other_query_filters() {
        let root = skills::ops::DownloadDir::new("panel-completion-scope").unwrap();
        for name in ["alpha", "beta"] {
            let path = root.path().join(name);
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(
                path.join("SKILL.md"),
                format!("---\nname: {name}\ndescription: sample\n---\nBody"),
            )
            .unwrap();
        }
        let mut config = skills::config::Config {
            agents: vec![],
            ..Default::default()
        };
        config.tags = ["alpha", "beta"]
            .into_iter()
            .map(|name| skills::config::TagConfig {
                name: name.into(),
                skills: vec![name.into()],
                color: None,
                description: None,
            })
            .collect();
        config.save(root.path()).unwrap();
        let ws = skills::Workspace::open(root.path()).unwrap();
        let snap = ws.scan().unwrap();
        let settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &settings,
        };
        let mut view = SearchView::panel(
            SkillPanelOptions::new(vec!["alpha".into()], "test".into(), LayoutScope::Tags),
            &ctx,
        );
        view.set_query("tag:beta", &ctx);
        view.update_completion(&ctx);
        assert!(!view.search_panel.completion.active());
        view.set_query("nomatch tag:al", &ctx);
        view.update_completion(&ctx);
        assert!(view.search_panel.completion.active());
        view.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &ctx);
        assert_eq!(view.query(), "nomatch tag:alpha ");
        assert!(view.hits.is_empty());
        view.set_query("tag:al", &ctx);
        view.update_completion(&ctx);
        view.select_scope(vec!["beta".into()], "new scope".into(), None, &ctx);
        assert!(
            !view.search_panel.completion.active(),
            "scope switch closes stale popup"
        );
    }

    #[test]
    fn filter_completion_keeps_query_on_escape_and_enter_enters_results() {
        let root =
            std::env::temp_dir().join(format!("skills-search-completion-{}", std::process::id()));
        std::fs::create_dir_all(root.join("sample")).unwrap();
        std::fs::write(
            root.join("sample/SKILL.md"),
            "---\nname: sample\ndescription: sample\n---\nBody",
        )
        .unwrap();
        skills::config::Config {
            agents: vec![],
            ..Default::default()
        }
        .save(&root)
        .unwrap();
        let ws = skills::Workspace::open(&root).unwrap();
        let snap = skills::reconcile::scan(&root, &ws.config).unwrap();
        let theme = crate::tui::theme::Theme::default();
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &{
                let mut settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
                settings.theme = theme;
                settings
            },
        };
        let mut view = SearchView::default();
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        for c in "status:loc".chars() {
            view.handle_key(key(KeyCode::Char(c)), &ctx);
        }
        assert!(view.search_panel.completion.active());
        view.handle_key(key(KeyCode::Down), &ctx);
        assert_eq!(view.focus, Focus::Input);
        view.handle_key(key(KeyCode::Esc), &ctx);
        assert_eq!(view.query(), "status:loc");
        assert!(!view.search_panel.completion.active());
        view.handle_key(key(KeyCode::Char('a')), &ctx);
        view.handle_key(key(KeyCode::Enter), &ctx);
        assert_eq!(view.query(), "status:local ");
        assert_eq!(view.focus, Focus::Input);
        assert!(!view.search_panel.completion.active());
        view.handle_key(key(KeyCode::Enter), &ctx);
        assert_eq!(view.focus, Focus::List);
        view.focus_input();
        view.set_query("status:unknown", &ctx);
        view.handle_key(key(KeyCode::Enter), &ctx);
        assert_eq!(view.focus, Focus::List);
        view.focus_input();
        view.set_query("status:loc 中文", &ctx);
        for _ in 0..3 {
            view.handle_key(key(KeyCode::Left), &ctx);
        }
        assert!(view.search_panel.completion.active());
        view.handle_key(key(KeyCode::Enter), &ctx);
        assert_eq!(view.query(), "status:local 中文");
        assert!(!view.search_panel.completion.active());
        view.focus_input();
        view.set_query("status:invalid", &ctx);
        view.update_completion(&ctx);
        assert!(
            !view.search_panel.completion.active(),
            "Library must not suggest hidden problem statuses"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
