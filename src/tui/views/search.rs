//! Library tab: input, result list, preview.

use super::cards::{self, CARD_H, cols_for, frame, skill_card};
use super::completion::Completion;
use super::preview::{Overlay, highlight_spans, preview_lines};
use super::{View, split_panes, wheel};
use crate::tui::app::{Action, Ctx, Hints};
use crate::tui::event::Task;
use crate::tui::modal::Modal;
use crate::tui::widgets::{CardGrid, Input, ScrollTrack, fit, pad, width};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
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

pub struct SearchView {
    input: Input,
    completion: Completion,
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
    esc_armed: bool,
    /// Session overrides for what `config.toml` set. Flipping these is a way to
    /// try a layout on for size; what the next start looks like stays the file's
    /// business, so neither is written back.
    layout: Option<UiLayout>,
    rendered_layout: UiLayout,
    /// Grid layout has no standing preview pane, so it opens over the results.
    overlay: Overlay,
    searcher: Searcher,
    multi: bool,
    scope_agent: Option<String>,
    panel: Option<(BTreeSet<String>, String)>,
    panel_active: bool,
    preset: Option<String>,
    target: Option<(
        skills::config::AgentConfig,
        Option<std::path::PathBuf>,
        bool,
    )>,
    area: Rect,
    scope: Option<(BTreeSet<String>, String)>,
    checked: BTreeSet<String>,
    batch_buttons: Vec<(Rect, char)>,
    updates: BTreeMap<String, String>,
}

impl Default for SearchView {
    fn default() -> Self {
        Self {
            input: Input::default(),
            completion: Completion::default(),
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
            esc_armed: false,
            layout: None,
            rendered_layout: UiLayout::Grid,
            overlay: Overlay::default(),
            searcher: Searcher::new(),
            multi: false,
            scope_agent: None,
            panel: None,
            panel_active: true,
            preset: None,
            target: None,
            area: Rect::default(),
            scope: None,
            checked: BTreeSet::new(),
            batch_buttons: Vec::new(),
            updates: BTreeMap::new(),
        }
    }
}

impl SearchView {
    fn is_picker(&self) -> bool {
        self.preset.is_some() || self.target.is_some()
    }

    fn includes_record(&self, record: &skills::reconcile::SkillRecord) -> bool {
        (!self.is_picker() || (record.status.is_present() && record.name.is_some()))
            && self
                .panel
                .as_ref()
                .is_none_or(|(keys, _)| keys.contains(&record.key))
            && (self.is_picker()
                || self.scope.is_some()
                || self.panel.is_some()
                || record.status.is_healthy())
    }

    pub fn panel(keys: Vec<String>, title: String, ctx: &Ctx) -> Self {
        let mut view = Self {
            panel: Some((keys.into_iter().collect(), title)),
            layout: Some(UiLayout::Grid),
            ..Self::default()
        };
        view.refresh(ctx);
        view
    }

    pub fn start_multi(&mut self, checked: Option<String>) {
        self.focus_list();
        self.multi = true;
        if let Some(key) = checked {
            self.checked.insert(key);
        }
    }

    pub fn panel_keys(&self, ctx: &Ctx) -> Vec<String> {
        if self.multi {
            self.visible_checked(ctx)
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
                ("a", "add skills"),
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
        self.completion.update(&self.input, ctx);
        if self.panel.is_none() && self.scope.is_none() && !self.is_picker() {
            self.completion.healthy_only();
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
            None => " Preset skills ".into(),
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
        view.layout = Some(UiLayout::Grid);
        view
    }

    pub fn preset_members(preset: &str, ctx: &Ctx) -> Self {
        let mut view = Self::default();
        view.refresh(ctx);
        view.preset = Some(preset.into());
        view.run_search(ctx, false);
        view.multi = true;
        view.layout = Some(UiLayout::Grid);
        if let Ok(Some(p)) = ctx.ws.presets.load(preset) {
            view.checked = p.skills.iter().cloned().collect();
        }
        view
    }

    fn apply_preset(&self, ctx: &Ctx) -> Vec<Action> {
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
                            skills::ops::targets::set_installed(
                                ws,
                                &agent,
                                project.as_deref(),
                                &keys,
                                None,
                                on,
                            )
                        }
                    }),
                    keys,
                ),
            ];
        }
        let Some(preset) = self.preset.clone() else {
            return vec![];
        };
        let visible: BTreeSet<String> = self
            .hits
            .iter()
            .map(|hit| ctx.snap.skills[hit.index].key.clone())
            .collect();
        let desired = self.visible_checked(ctx);
        let keys = visible.iter().cloned().collect();
        vec![Action::BatchMeta(
            Box::new(move |ws| {
                skills::history::preset_edit(ws, &preset, |members| {
                    members.retain(|key| !visible.contains(key));
                    for key in &desired {
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
        self.input.clear();
        self.completion.close();
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
        let keys = self.visible_checked(ctx);
        if keys.is_empty() {
            return vec![Action::Error(
                "No selected skills in the current filter".into(),
            )];
        }
        let modal = match operation {
            't' => Modal::batch_tags(keys, ctx),
            'd' => match &self.scope_agent {
                Some(agent) => Modal::batch_deploy_agent(keys, agent, ctx),
                None => Modal::batch_deploy(keys, ctx),
            },
            'p' => Modal::batch_presets(keys, ctx),
            _ => return vec![],
        };
        vec![Action::OpenModal(Box::new(modal))]
    }

    fn decorate(&self, lines: &mut [Line<'static>], r: &SkillRecord, ctx: &Ctx) {
        if let Some(line) = lines.first_mut()
            && let Some(marker) = line.spans.first_mut()
        {
            if self.multi {
                *marker = cards::checkbox_marker(self.checked.contains(&r.key), ctx.theme);
                if !r.status.is_healthy() {
                    marker.style = ctx.theme.warn();
                }
            } else if r.status.is_healthy() && self.updates.contains_key(&r.key) {
                *marker = Span::styled("↑   ", ctx.theme.accent());
            }
        }
        if self.multi
            && !r.status.is_healthy()
            && let Some(line) = lines.first_mut()
        {
            let warning = format!(" ! {}", r.status.label());
            let available = line.width().saturating_sub(cards::MARKER_W);
            if available > width(&warning) + 8 {
                let name = pad(cards::display_name(r), available - width(&warning));
                line.spans.truncate(1);
                line.spans.push(Span::styled(name, ctx.theme.bold()));
                line.spans.push(Span::styled(warning, ctx.theme.warn()));
            }
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
        self.input.value().to_string()
    }
    pub fn paste(&mut self, text: &str, ctx: &Ctx) -> Vec<Action> {
        if self.focus != Focus::Input || self.overlay.is_open() {
            return vec![];
        }
        match self.input.paste(text) {
            Ok(true) => {
                self.esc_armed = false;
                self.run_search(ctx, false);
                self.update_completion(ctx);
                vec![]
            }
            Ok(false) => vec![],
            Err(error) => vec![Action::Error(error.into())],
        }
    }

    pub fn input_focused(&self) -> bool {
        self.focus == Focus::Input
    }
    pub fn focus_input(&mut self) {
        self.focus = Focus::Input;
    }
    pub fn focus_list(&mut self) {
        self.focus = Focus::List;
    }
    pub fn set_query(&mut self, q: &str, ctx: &Ctx) {
        self.input.set(q);
        self.completion.close();
        self.run_search(ctx, false);
    }

    fn layout(&self, ctx: &Ctx) -> UiLayout {
        self.layout.unwrap_or(ctx.ws.config.ui.layout)
    }

    fn selected<'a>(&self, ctx: &'a Ctx) -> Option<&'a SkillRecord> {
        self.grid
            .selected()
            .and_then(|i| self.hits.get(i))
            .map(|h| &ctx.snap.skills[h.index])
    }

    /// Re-run the query. `keep` preserves the selected skill (after a rescan);
    /// typing always jumps back to the best match.
    fn run_search(&mut self, ctx: &Ctx, keep: bool) {
        let key = if keep {
            self.selected(ctx).map(|r| r.key.clone())
        } else {
            None
        };
        let q = Query::parse(self.input.value());
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
        match self.need_present(ctx, "tag") {
            Ok(r) => vec![Action::OpenModal(Box::new(Modal::tags(&r.key, &r.tags)))],
            Err(a) => vec![a],
        }
    }
    fn act_note(&self, ctx: &Ctx) -> Vec<Action> {
        match self.need_present(ctx, "annotate") {
            Ok(r) => vec![Action::EditNote(r.key.clone())],
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
            SkillStatus::Modified | SkillStatus::Managed { no_baseline: true }
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
    fn act_migrate(&self, ctx: &Ctx) -> Vec<Action> {
        let Some(r) = self.selected(ctx) else {
            return vec![];
        };
        let SkillStatus::Renamed { to } = &r.status else {
            return vec![Action::Error("migrate applies to renamed? skills".into())];
        };
        let (old, new) = (r.key.clone(), to.clone());
        vec![Action::Write(Box::new(move |ws| {
            edit::migrate_meta(ws, &old, &new).map(|_| format!("migrated {old} → {new}"))
        }))]
    }
    fn act_check(&self, ctx: &Ctx) -> Vec<Action> {
        let Some(r) = self.selected(ctx) else {
            return vec![];
        };
        if !matches!(r.source, Some(skills::meta::Source::Git { .. })) {
            return vec![Action::Error(format!("{} has no git source", r.key))];
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
        if !matches!(r.source, Some(skills::meta::Source::Git { .. })) {
            return vec![Action::Error(format!("{} has no git source", r.key))];
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
    fn draw_input(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        let th = ctx.theme;
        let title = Line::from(vec![
            Span::raw(" "),
            Span::styled(
                {
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
                            self.includes_record(r)
                                && skills::repository::alias_of(&r.key).is_none()
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
                },
                th.dim(),
            ),
            Span::raw(" "),
        ]);
        let block = th.block(title, self.panel_active && self.focus == Focus::Input);
        let inner = block.inner(area);
        f.render_widget(block, area);
        self.input_rect = area;
        let prompt = Rect {
            x: inner.x + 1,
            width: 2,
            ..inner
        };
        f.render_widget(Paragraph::new(Span::styled("› ", th.accent())), prompt);
        let field = Rect {
            x: inner.x + 3,
            width: inner.width.saturating_sub(4),
            ..inner
        };
        self.input.render(
            f,
            field,
            self.panel_active && self.focus == Focus::Input,
            "search skills…   repo:owner/repo  tag:x  agent:y  status:managed  untagged",
            th,
        );
    }

    /// The results, as a grid of cells that happens to be one column wide in
    /// the split layout. Drawing cell by cell rather than through `List` is what
    /// lets a card carry its own frame and lets several sit on a row.
    fn draw_results(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        let th = ctx.theme;
        let searching = !self.input.value().trim().is_empty()
            && !Query::parse(self.input.value()).text.is_empty();
        // The layout decides the shape too: a grid is made of cards, and the
        // two splits are lists beside a preview. One column of framed cards
        // would be a list wearing frames, which is the worst of both.
        let legend = vec![Span::raw(
            match self.panel.as_ref().or(self.scope.as_ref()) {
                Some((_, title)) => format!(" {title} "),
                None => " skills ".into(),
            },
        )];
        let block = th.block(
            Line::from(legend),
            self.panel_active && self.focus == Focus::List,
        );
        let inner = block.inner(area);
        f.render_widget(block, area);
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
            UiLayout::Grid => CARD_H,
            UiLayout::List => 4,
            UiLayout::Compact if searching && !short => 2,
            UiLayout::Compact => 1,
        };
        // One column is always kept back for the scrollbar so the column count
        // does not change under the user the moment the list grows past a screen.
        let usable = inner.width.saturating_sub(1);
        let cols = if cards { cols_for(usable) } else { 1 };
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
        for i in self.grid.visible() {
            let Some(cell) = self.grid.cell(i) else {
                continue;
            };
            let h = &self.hits[i];
            let r = &ctx.snap.skills[h.index];
            let on = selected == Some(i);
            if cards {
                let ci = frame(f, cell, on, self.focus == Focus::List, th);
                // While searching the card shows the excerpt around the match
                // and names the fields it matched in; a hit on the name or a
                // tag has no excerpt, so the description stays.
                let body = h
                    .excerpt
                    .as_ref()
                    .filter(|_| searching)
                    .map(|e| e.text.as_str());
                let tail = if searching {
                    h.fields
                        .iter()
                        .map(|f| f.label())
                        .collect::<Vec<_>>()
                        .join("·")
                } else {
                    r.source
                        .as_ref()
                        .map(|s| s.kind().to_string())
                        .unwrap_or_default()
                };
                let mut lines = skill_card(r, ctx, ci.width as usize, body, &tail, &h.terms);
                self.decorate(&mut lines, r, ctx);
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
                    if self.focus == Focus::List {
                        th.selected()
                    } else {
                        th.selected_unfocused()
                    }
                } else {
                    Style::default()
                };
                let tail = r
                    .source
                    .as_ref()
                    .map(|s| s.kind().to_string())
                    .unwrap_or_default();
                let body = h
                    .excerpt
                    .as_ref()
                    .filter(|_| searching)
                    .map(|e| e.text.as_str());
                let mut lines = skill_card(
                    r,
                    ctx,
                    cell.width.saturating_sub(3) as usize,
                    body,
                    &tail,
                    &h.terms,
                );
                self.decorate(&mut lines, r, ctx);
                let lines: Vec<Line> = lines
                    .into_iter()
                    .enumerate()
                    .map(|(i, l)| {
                        // The marker sits on the first line only; the rest of
                        // the entry is told by the background.
                        let mark = if on && i == 0 { "▸ " } else { "  " };
                        let mut spans = vec![Span::styled(mark, th.accent())];
                        spans.extend(l.spans);
                        Line::from(spans)
                    })
                    .collect();
                f.render_widget(Paragraph::new(lines).style(style), cell);
            } else {
                let style = if on {
                    if self.focus == Focus::List {
                        th.selected()
                    } else {
                        th.selected_unfocused()
                    }
                } else {
                    Style::default()
                };
                let mut lines = row_lines(
                    r,
                    h,
                    ctx,
                    cell.width.saturating_sub(2) as usize,
                    searching && !short,
                    on,
                );
                if self.multi
                    && let Some(line) = lines.first_mut()
                {
                    // Compact rows reserve two columns for focus, then the marker.
                    line.spans.splice(
                        1..2,
                        [cards::checkbox_marker(self.checked.contains(&r.key), th)],
                    );
                }
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
        let th = ctx.theme;
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
    fn refresh(&mut self, ctx: &Ctx) {
        self.searcher.configure(
            ctx.ws.config.search.clone(),
            skills::dict::Dictionaries::load(&ctx.ws.root, &ctx.ws.config.search.dictionaries),
        );
        self.searcher.index(&ctx.snap.skills);
        self.run_search(ctx, true);
        self.checked.retain(|key| ctx.snap.get(key).is_some());
        self.updates.retain(|key, remote| ctx.snap.get(key).is_some_and(|r| {
            !matches!(&r.source, Some(skills::meta::Source::Git { revision: Some(revision), .. }) if revision == remote)
        }));
    }

    fn handle_key(&mut self, k: KeyEvent, ctx: &Ctx) -> Vec<Action> {
        if self.overlay.handle_key(k) {
            return vec![];
        }
        if self.focus == Focus::Input && self.completion.active() {
            match k.code {
                KeyCode::Up | KeyCode::Down => {
                    self.completion
                        .move_by(if k.code == KeyCode::Up { -1 } else { 1 });
                    return vec![];
                }
                KeyCode::Enter => {
                    self.completion.accept(&mut self.input);
                    self.run_search(ctx, false);
                    if self.input.value()[..self.input.cursor_byte()].ends_with(':') {
                        self.update_completion(ctx);
                    }
                    return vec![];
                }
                KeyCode::Esc => {
                    self.completion.close();
                    self.esc_armed = false;
                    return vec![];
                }
                _ => {}
            }
        }
        if self.is_picker() {
            if k.code == KeyCode::Esc {
                if self.focus == Focus::Preview {
                    self.focus = Focus::List;
                    return vec![];
                }
                return vec![Action::CloseModal];
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
                KeyCode::Esc if self.panel.is_some() => {
                    self.focus = Focus::List;
                }
                KeyCode::Esc => {
                    if self.input.is_empty() {
                        if self.esc_armed {
                            return vec![Action::Quit];
                        }
                        self.esc_armed = true;
                        return vec![Action::Toast("press Esc again to quit".into())];
                    }
                    self.input.clear();
                    self.run_search(ctx, false);
                }
                // Focus moves even with nothing to select: the list is where
                // the action keys live, and installing the first skill needs them.
                KeyCode::Enter | KeyCode::Down => self.focus = Focus::List,
                KeyCode::Up => self.move_sel(-1),
                KeyCode::Char('n') if ctrl => self.move_sel(1),
                KeyCode::Char('p') if ctrl => self.move_sel(-1),
                _ => {
                    let cursor = self.input.cursor_byte();
                    let changed = self.input.handle_key(k);
                    if changed {
                        self.run_search(ctx, false);
                    }
                    if changed || cursor != self.input.cursor_byte() {
                        self.update_completion(ctx);
                    }
                }
            },
            Focus::List => match k.code {
                KeyCode::Esc => self.focus = Focus::Input,
                KeyCode::Char('q') => self.focus = Focus::Input,
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
                // Along a row when there is a row to walk; otherwise the old
                // meaning, which is to step across into the preview.
                KeyCode::Right | KeyCode::Char('l') if self.grid.cols() > 1 => self.move_sel(1),
                KeyCode::Left | KeyCode::Char('h') if self.grid.cols() > 1 => self.move_sel(-1),
                KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => self.open_preview(ctx),
                KeyCode::Char('t') => {
                    acts = if self.multi {
                        self.batch_action('t', ctx)
                    } else {
                        self.act_tags(ctx)
                    }
                }
                KeyCode::Char('n') => acts = self.act_note(ctx),
                KeyCode::Char('d') => {
                    acts = if self.multi {
                        self.batch_action('d', ctx)
                    } else {
                        self.act_deploy(ctx)
                    }
                }
                KeyCode::Char('r') => acts = self.act_rename(ctx),
                KeyCode::Char('s') => acts = self.act_set_source(ctx),
                KeyCode::Char('a') => acts = self.act_accept(ctx),
                KeyCode::Char('m') => self.multi = true,
                KeyCode::Char('M') => acts = self.act_migrate(ctx),
                KeyCode::Char('u') => acts = self.act_check(ctx),
                KeyCode::Char('U') => acts = self.act_update(ctx),
                KeyCode::Char('x') => acts = self.act_remove(ctx),
                KeyCode::Char('v') | KeyCode::Char('V') => {
                    // Cycle from most to least room per skill.
                    self.layout = Some(match self.layout(ctx) {
                        UiLayout::Grid => UiLayout::List,
                        UiLayout::List => UiLayout::Compact,
                        UiLayout::Compact => UiLayout::Grid,
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
                KeyCode::Char('t') => {
                    acts = if self.multi {
                        self.batch_action('t', ctx)
                    } else {
                        self.act_tags(ctx)
                    }
                }
                KeyCode::Char('n') => acts = self.act_note(ctx),
                KeyCode::Char('d') => {
                    acts = if self.multi {
                        self.batch_action('d', ctx)
                    } else {
                        self.act_deploy(ctx)
                    }
                }
                KeyCode::Char('u') => acts = self.act_check(ctx),
                KeyCode::Char('U') => acts = self.act_update(ctx),
                _ => {}
            },
        }
        self.esc_armed = false;
        acts
    }

    fn handle_mouse(&mut self, m: MouseEvent, ctx: &Ctx) -> Vec<Action> {
        if self.overlay.handle_mouse(m) {
            return vec![];
        }
        if self.is_picker()
            && matches!(m.kind, MouseEventKind::Down(MouseButton::Left))
            && !self.area.contains((m.column, m.row).into())
        {
            return vec![Action::CloseModal];
        }
        if self.focus == Focus::Input {
            let (consumed, accepted) = self.completion.mouse(m, &mut self.input);
            if consumed {
                if accepted {
                    self.run_search(ctx, false);
                    if self.input.value()[..self.input.cursor_byte()].ends_with(':') {
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
                'c' => return self.apply_preset(ctx),
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
        let at = (m.column, m.row).into();
        if let Some(d) = wheel(&m) {
            if self.preview_rect.contains(at) {
                self.scroll_preview(d);
            } else if self.list_rect.contains(at) {
                // A wheel notch is a row of cards, however many are on it.
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
            if self.input_rect.contains(at) {
                self.focus = Focus::Input;
                self.input.click(m.column);
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
                        m.row == y && m.column >= x && m.column < x + cards::MARKER_W as u16
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
        if let MouseEventKind::Down(MouseButton::Right) = m.kind
            && self.list_rect.contains(at)
            && self.grid.click(m.column, m.row).is_some()
        {
            self.focus = Focus::List;
            if self.is_picker() {
                return vec![];
            }
            return if self.multi {
                self.batch_action('d', ctx)
            } else {
                self.act_deploy(ctx)
            };
        }
        vec![]
    }

    fn draw(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        self.area = area;
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(1),
                Constraint::Length(1),
            ])
            .split(area);
        self.draw_input(f, rows[0], ctx);

        // Split keeps a preview open beside the results and so gets one column
        // of cards; grid spends the whole width on cards and puts the preview
        // over them when it is wanted.
        let grid = self.layout(ctx) == UiLayout::Grid;
        let (left, right) = if grid {
            (rows[1], Rect::default())
        } else {
            split_panes(rows[1], 38)
        };
        self.list_rect = left;
        self.draw_results(f, left, ctx);
        if grid {
            self.preview_rect = Rect::default();
        } else {
            self.preview_rect = right;
            self.draw_preview(f, right, ctx);
        }
        self.overlay.draw(f, rows[1], ctx);
        self.batch_buttons.clear();
        let bar = rows[2];
        let mut x = bar.x;
        let selected = self.visible_checked(ctx).len();
        let status = if self.multi {
            let hidden = self.checked.len().saturating_sub(selected);
            format!(
                " Multi-select · {selected} selected{} ",
                if hidden > 0 {
                    format!(" · {hidden} hidden (excluded)")
                } else {
                    String::new()
                }
            )
        } else {
            self.selected(ctx)
                .map(|r| format!(" {} ", r.status.label()))
                .unwrap_or_default()
        };
        let status = fit(&status, bar.width as usize / 2);
        let w = width(&status) as u16;
        f.render_widget(
            Paragraph::new(Span::styled(status, ctx.theme.dim())),
            Rect::new(x, bar.y, w, 1),
        );
        x += w;
        let buttons: &[(&str, char)] = if self.is_picker() {
            &[
                ("[Select all]", 'a'),
                ("[Apply a]", 'c'),
                ("[Cancel Esc]", 'e'),
            ]
        } else if self.multi {
            &[
                ("[Select all]", 'a'),
                ("[Tags t]", 't'),
                ("[Deploy d]", 'd'),
                ("[Preset p]", 'p'),
                ("[Cancel Esc]", 'e'),
            ]
        } else {
            &[("[Multi-select m]", 'm')]
        };
        for (label, command) in buttons {
            let w = width(label) as u16;
            if x + w > bar.right() {
                break;
            }
            let rect = Rect::new(x, bar.y, w, 1);
            let enabled =
                self.is_picker() || !self.multi || selected > 0 || matches!(*command, 'e' | 'a');
            f.render_widget(
                Paragraph::new(Span::styled(
                    *label,
                    if enabled {
                        ctx.theme.accent()
                    } else {
                        ctx.theme.dim()
                    },
                )),
                rect,
            );
            if enabled {
                self.batch_buttons.push((rect, *command));
            }
            x += w + 1;
        }
        if self.panel_active && self.focus == Focus::Input && !self.overlay.is_open() {
            self.completion.draw(f, rows[1], ctx);
        }
    }

    fn hints(&self) -> Hints {
        if let Some(hints) = self.overlay.hints() {
            return hints;
        }
        if self.focus == Focus::Input {
            if self.completion.active() {
                return &[
                    ("↑↓", "suggestions"),
                    ("Enter", "complete"),
                    ("Esc", "close suggestions"),
                ];
            }
            if self.is_picker() {
                return &[
                    ("Enter/↓", "results"),
                    ("Ctrl+Enter", "apply"),
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
                ("Space", "select"),
                ("Ctrl+A", "select all results"),
                ("/", "search"),
                ("v/V", "layout"),
                ("Enter", "preview"),
                ("a/Ctrl+Enter", "apply"),
                ("Esc", "cancel"),
            ];
        }
        if self.multi && self.focus == Focus::List {
            return &[
                ("Space", "select"),
                ("Ctrl+A", "select all results"),
                ("t", "tags"),
                ("d", "deploy"),
                ("p", "preset"),
                ("/", "filter"),
                ("Enter", "preview"),
                ("Esc", "cancel selection"),
            ];
        }
        match self.focus {
            Focus::Input if self.completion.active() => &[
                ("↑↓", "suggestions"),
                ("Enter", "complete"),
                ("Esc", "close suggestions"),
            ],
            Focus::Input => &[
                ("↑↓", "select"),
                ("↓", "list"),
                ("Enter", "list"),
                ("Esc", "clear/quit"),
                ("F1", "help"),
            ],
            Focus::List => &[
                ("m", "multi-select"),
                ("t", "tags"),
                ("n", "note"),
                ("d", "deploy"),
                ("r", "rename"),
                ("s", "source"),
                ("a", "accept"),
                ("u/U", "check/update"),
                ("x", "remove"),
                ("Enter", "preview"),
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

/// The compact density: one line, and a second carrying the excerpt while a
/// query is running. `on` draws the selection marker the list widget used to.
#[allow(clippy::too_many_arguments)]
fn row_lines<'a>(
    r: &'a SkillRecord,
    h: &'a Hit,
    ctx: &'a Ctx,
    inner_w: usize,
    searching: bool,
    on: bool,
) -> Vec<Line<'a>> {
    let th = ctx.theme;
    let badge = cards::repository_badge(r, ctx.ws.config.ui.icons);
    let content_w = inner_w.saturating_sub(2 + cards::MARKER_W);
    let badge_w = badge
        .as_deref()
        .map(|text| width(text).min(content_w / 2))
        .unwrap_or(0);
    let badge_space = badge_w + usize::from(badge_w > 0);
    let key_w = 26.min(content_w.saturating_sub(badge_space));
    let mut spans = vec![
        Span::styled(if on { "▸ " } else { "  " }, th.accent()),
        cards::health_marker(r, th),
    ];
    spans.extend(highlight_spans(
        &pad(cards::display_name(r), key_w),
        &h.terms,
        Style::default(),
        th,
    ));
    if let Some(badge) = badge.filter(|_| badge_w > 0) {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(pad(&badge, badge_w), th.dim()));
    }
    let tags_w = content_w.saturating_sub(key_w + badge_space);
    if !r.tags.is_empty() && tags_w > 3 {
        spans.push(Span::raw(" "));
        spans.extend(highlight_spans(
            &pad(&r.tags.join(","), tags_w - 1),
            &h.terms,
            th.tag(),
            th,
        ));
    } else {
        spans.push(Span::raw(" ".repeat(tags_w)));
    }
    spans.push(Span::raw(" "));
    if !searching {
        return vec![Line::from(spans)];
    }
    let mut sub = vec![Span::raw("    ")];
    let fields: Vec<&str> = h.fields.iter().map(|f| f.label()).collect();
    sub.push(Span::styled(
        format!("{} ", fields.join("·")),
        th.dim().add_modifier(ratatui::style::Modifier::ITALIC),
    ));
    let avail = inner_w.saturating_sub(6 + width(&fields.join("·")));
    if let Some(e) = &h.excerpt {
        sub.extend(highlight_spans(
            &fit(&e.text, avail),
            &h.terms,
            th.dim(),
            th,
        ))
    }
    vec![Line::from(spans), Line::from(sub)]
}

#[cfg(test)]
mod tests {
    use super::*;

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
            .save("missing", &skills::meta::SkillMeta::default())
            .unwrap();
        let text = std::fs::read_to_string(ws.meta.path("missing")).unwrap();
        std::fs::write(
            ws.meta.path("corrupt-missing"),
            format!("{text}\n[skills.corrupt-missing]\ntags = 42\n"),
        )
        .unwrap();
        let snap = ws.scan().unwrap();
        let theme = crate::tui::theme::Theme::default();
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            theme: &theme,
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
        assert!(!picker.hints().iter().any(|(k, _)| *k == "Space"));
        picker.focus_list();
        assert!(picker.hints().iter().any(|(k, _)| *k == "Space"));
        picker.focus = Focus::Preview;
        assert!(!picker.hints().iter().any(|(k, _)| *k == "Space"));
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
            theme: &theme,
        };
        let mut view = SearchView::default();
        view.refresh(&ctx);
        view.layout = Some(UiLayout::List);
        view.set_query("shared", &ctx);
        view.grid.select(Some(3));
        let selected = view.selected(&ctx).unwrap().key.clone();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|f| view.draw(f, Rect::new(0, 1, 80, 22), &ctx))
            .unwrap();
        assert_eq!(view.rendered_layout, UiLayout::Compact);
        assert_eq!(view.layout, Some(UiLayout::List));
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
        let snap = ws.scan().unwrap();
        let theme = crate::tui::theme::Theme::default();
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            theme: &theme,
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
        assert_eq!(view.focus, Focus::Input);
        view.layout = Some(UiLayout::List);
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
    fn multi_select_filters_targets_and_keeps_identity_and_scope() {
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
            theme: &theme,
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
            [Action::Error(_)]
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
        let mut lines = skill_card(record, &ctx, 40, None, "", &[]);
        let before = lines[0].to_string();
        view.decorate(&mut lines, record, &ctx);
        assert!(lines[0].to_string().starts_with("[✓] printer"));
        assert_eq!(lines[0].width(), width(&before));
        assert_eq!(
            width(&before[..before.find("printer").unwrap()]),
            cards::MARKER_W
        );
        let hit = view.hits.first().unwrap();
        let compact = row_lines(record, hit, &ctx, 60, false, true);
        let mut checked_compact = compact[0].clone();
        checked_compact.spans[1] = cards::checkbox_marker(true, ctx.theme);
        let normal = compact[0].to_string();
        let selected = checked_compact.to_string();
        assert!(selected.starts_with("▸ [✓] printer"));
        assert_eq!(
            width(&normal[..normal.find("printer").unwrap()]),
            2 + cards::MARKER_W
        );
        assert_eq!(
            width(&selected[..selected.find("printer").unwrap()]),
            2 + cards::MARKER_W
        );
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
    fn filter_completion_keeps_query_on_escape_and_enter_enters_results() {
        let root =
            std::env::temp_dir().join(format!("skills-search-completion-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
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
            theme: &theme,
        };
        let mut view = SearchView::default();
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        for c in "status:mana".chars() {
            view.handle_key(key(KeyCode::Char(c)), &ctx);
        }
        assert!(view.completion.active());
        view.handle_key(key(KeyCode::Down), &ctx);
        assert_eq!(view.focus, Focus::Input);
        view.handle_key(key(KeyCode::Esc), &ctx);
        assert_eq!(view.query(), "status:mana");
        assert!(!view.completion.active());
        view.handle_key(key(KeyCode::Char('g')), &ctx);
        view.handle_key(key(KeyCode::Enter), &ctx);
        assert_eq!(view.query(), "status:managed ");
        assert_eq!(view.focus, Focus::Input);
        assert!(!view.completion.active());
        view.handle_key(key(KeyCode::Enter), &ctx);
        assert_eq!(view.focus, Focus::List);
        view.focus_input();
        view.set_query("status:unknown", &ctx);
        view.handle_key(key(KeyCode::Enter), &ctx);
        assert_eq!(view.focus, Focus::List);
        view.focus_input();
        view.set_query("status:mana 中文", &ctx);
        for _ in 0..3 {
            view.handle_key(key(KeyCode::Left), &ctx);
        }
        assert!(view.completion.active());
        view.handle_key(key(KeyCode::Enter), &ctx);
        assert_eq!(view.query(), "status:managed 中文");
        assert!(!view.completion.active());
        view.focus_input();
        view.set_query("status:invalid", &ctx);
        view.update_completion(&ctx);
        assert!(
            !view.completion.active(),
            "Library must not suggest hidden problem statuses"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
