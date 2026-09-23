//! Always-expanded repository tree with ancestry-aware multi-selection.
use super::components::search_panel::{PanelLayout, PanelStyle, SearchEvent, SearchPanel};
use super::views::preview::Overlay;
use super::{
    app::{Action, Ctx, Hints},
    event::Task,
    widgets::{Input, ListNav, OverlayClear, fit},
};
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Span},
    widgets::{List, ListItem, Paragraph},
};
use skills::meta::SourceKind;
use skills::reconcile::{SkillRecord, SkillStatus, Snapshot};
use skills::repository::{FetchedRepository, overlaps, related};
use skills::search::{Query, Searcher};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone)]
pub struct InstallSelection {
    pub fetched: FetchedRepository,
    pub paths: Vec<String>,
    pub names: BTreeMap<String, String>,
}

fn needs_source_name(fetched: &FetchedRepository) -> bool {
    fetched.repository.kind == SourceKind::Archive
        && fetched
            .repository
            .name
            .as_deref()
            .is_none_or(|name| name.trim().is_empty())
}

pub struct RepositoryPicker {
    pub selection: InstallSelection,
    source_name: Input,
    alias: Input,
    search_panel: SearchPanel,
    local_name: Input,
    focus: u8, // storage folder, search, list, selected skill name, source name
    list: ListNav,
    shown: Vec<String>,
    rect: Rect,
    fields: [Rect; 4],
    candidates: Snapshot,
    searcher: Searcher,
    matching: BTreeSet<String>,
    configured_search: Option<skills::config::SearchConfig>,
    preview: Overlay,
}
impl RepositoryPicker {
    pub fn new(fetched: FetchedRepository, _ctx: &Ctx) -> Self {
        let mut picker = Self::restore(InstallSelection {
            fetched,
            paths: Vec::new(),
            names: BTreeMap::new(),
        });
        if !needs_source_name(&picker.selection.fetched) {
            picker.focus = 2;
        }
        picker
    }
    pub fn restore(selection: InstallSelection) -> Self {
        let alias = Input::with_value(&selection.fetched.repository.alias);
        let repository = &selection.fetched.repository;
        let needs_name = needs_source_name(&selection.fetched);
        let source_name = Input::with_value(&if needs_name {
            String::new()
        } else {
            repository.display_name()
        });
        let candidates = candidate_snapshot(&selection.fetched);
        let mut this = Self {
            candidates,
            searcher: Searcher::new(),
            matching: BTreeSet::new(),
            configured_search: None,
            preview: Overlay::default(),
            selection,
            source_name,
            alias,
            search_panel: SearchPanel::default(),
            local_name: Input::default(),
            focus: if needs_name { 4 } else { 1 },
            list: ListNav::default(),
            shown: vec![],
            rect: Rect::default(),
            fields: [Rect::default(); 4],
        };
        this.refilter();
        this
    }
    fn tree(&self) -> Vec<String> {
        let mut nodes = BTreeSet::new();
        for path in &self.selection.fetched.choices {
            if path.is_empty() {
                nodes.insert(String::new());
            }
            let mut at = String::new();
            for component in path.split('/').filter(|s| !s.is_empty()) {
                if !at.is_empty() {
                    at.push('/');
                }
                at.push_str(component);
                nodes.insert(at.clone());
            }
        }
        nodes.into_iter().collect()
    }
    fn configure(&mut self, ctx: &Ctx) {
        if self.configured_search.as_ref() != Some(&ctx.settings.search) {
            self.refresh(ctx);
        }
    }

    /// Re-read dictionaries and the resolved settings when the app refreshes.
    /// Preserve staged choices and input while re-evaluating the current query.
    pub fn refresh(&mut self, ctx: &Ctx) {
        self.searcher.configure(
            ctx.settings.search.clone(),
            skills::dict::Dictionaries::load(&ctx.ws.root, &ctx.settings.search.dictionaries),
        );
        self.configured_search = Some(ctx.settings.search.clone());
        self.refilter();
        self.update_completion(ctx);
    }
    fn update_completion(&mut self, ctx: &Ctx) {
        let candidate_ctx = Ctx {
            ws: ctx.ws,
            snap: &self.candidates,
            settings: ctx.settings,
        };
        self.search_panel
            .completion
            .update_install(&self.search_panel.input, &candidate_ctx);
    }
    fn matches_filter(&self, path: &str) -> bool {
        self.matching.contains(path)
    }
    fn refilter(&mut self) {
        let mut query = Query::parse(self.search_panel.input.value());
        // Only free path tokens use root-relative prefix semantics; the slash
        // in repo:owner/repository is part of a normal shared query filter.
        let mut prefixes = Vec::new();
        query.text = query
            .text
            .split_whitespace()
            .filter(|token| {
                if token.contains('/') {
                    prefixes.push(
                        token
                            .strip_prefix("./")
                            .unwrap_or(token)
                            .trim_start_matches('/')
                            .to_lowercase(),
                    );
                    false
                } else {
                    true
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        self.matching = self
            .searcher
            .search(&self.candidates.skills, &query)
            .into_iter()
            .map(|hit| self.candidates.skills[hit.index].key.clone())
            .filter(|path| {
                prefixes
                    .iter()
                    .all(|prefix| path.to_lowercase().starts_with(prefix))
            })
            .collect();
        let matching = &self.matching;
        self.shown = self
            .tree()
            .into_iter()
            .filter(|p| matching.iter().any(|c| c == p || overlaps(p, c)))
            .collect();
        self.list.clamp(self.shown.len());
    }
    fn selected(&self) -> Option<String> {
        self.list
            .selected()
            .and_then(|i| self.shown.get(i))
            .cloned()
    }
    fn resolved_name(&self, path: &str, ctx: &Ctx) -> String {
        let mut paths: Vec<_> = self
            .selection
            .paths
            .iter()
            .filter(|p| self.matches_filter(p))
            .cloned()
            .collect();
        if !paths.iter().any(|p| p == path) {
            paths.push(path.to_string());
        }
        let occupied = ctx
            .snap
            .skills
            .iter()
            .filter(|r| {
                skills::repository::alias_of(&r.key)
                    == Some(self.selection.fetched.repository.alias.as_str())
            })
            .filter_map(|r| r.key.rsplit('/').next().map(str::to_string))
            .collect();
        skills::repository::resolve_local_names(
            &paths,
            &self.selection.names,
            &occupied,
            &paths
                .iter()
                .map(|p| (p.clone(), self.selection.fetched.local_name(p)))
                .collect(),
        )
        .ok()
        .and_then(|names| names.get(path).cloned())
        .or_else(|| self.selection.names.get(path).cloned())
        .unwrap_or_else(|| self.selection.fetched.local_name(path))
    }

    fn disabled(&self, path: &str, ctx: &Ctx) -> Option<String> {
        if let Some(error) = self.selection.fetched.invalid.get(path) {
            return Some(format!("invalid: {error}"));
        }
        if !self.matches_filter(path) {
            return Some("outside current filter".into());
        }
        if self
            .selection
            .paths
            .iter()
            .any(|p| self.matches_filter(p) && related(p, path))
        {
            return Some("ancestor or descendant selected".into());
        }
        if ctx.snap.skills.iter().any(|s| {
            s.status != skills::reconcile::SkillStatus::Missing
                && s.source.as_ref().is_some_and(|source| {
                    self.selection
                        .fetched
                        .repository
                        .matches_source(source, path)
                })
        }) {
            return Some("already installed".into());
        }
        None
    }
    fn restores_missing(&self, path: &str, ctx: &Ctx) -> bool {
        ctx.snap.skills.iter().any(|skill| {
            skill.status == skills::reconcile::SkillStatus::Missing
                && skill.source.as_ref().is_some_and(|source| {
                    self.selection
                        .fetched
                        .repository
                        .matches_source(source, path)
                })
        })
    }

    fn toggle(&mut self, path: &str, ctx: &Ctx) -> Vec<Action> {
        if !self.selection.fetched.choices.iter().any(|p| p == path) {
            return vec![];
        }
        if self.selection.paths.iter().any(|p| p == path) {
            self.selection.paths.retain(|p| p != path);
        } else if let Some(reason) = self.disabled(path, ctx) {
            return vec![Action::Error(reason)];
        } else {
            // An ancestor excluded by the filter no longer blocks this choice.
            // Remove its stored check so clearing the filter cannot select both.
            self.selection.paths.retain(|p| !related(p, path));
            self.selection.paths.push(path.into());
        }
        vec![]
    }
    fn save_source_name(&mut self, ctx: &Ctx) -> Vec<Action> {
        if let Err(error) = self
            .selection
            .fetched
            .repository
            .set_name(self.source_name.value())
        {
            self.focus = 4;
            return vec![Action::Error(format!("{error:#}"))];
        }
        self.candidates = candidate_snapshot(&self.selection.fetched);
        self.refilter();
        self.update_completion(ctx);
        vec![]
    }
    fn save_focused_field(&mut self, ctx: &Ctx) -> Vec<Action> {
        match self.focus {
            0 => {
                let value = self.alias.value().trim();
                if !skills::util::valid_skill_key(value) {
                    return vec![Action::Error("invalid storage folder".into())];
                }
                self.selection.fetched.repository.alias = value.into();
            }
            3 => {
                if let Some(path) = self.selected() {
                    let value = self.local_name.value().trim();
                    if !skills::util::valid_skill_key(value) {
                        return vec![Action::Error("invalid local skill name".into())];
                    }
                    self.selection.names.insert(path, value.into());
                }
            }
            4 => return self.save_source_name(ctx),
            _ => {}
        }
        vec![]
    }
    fn focus_local_name(&mut self, ctx: &Ctx) -> bool {
        if let Some(path) = self.selected()
            && self.selection.fetched.choices.contains(&path)
        {
            self.local_name = Input::with_value(&self.resolved_name(&path, ctx));
            self.focus = 3;
            true
        } else {
            false
        }
    }
    fn next_focus(&mut self, backwards: bool, ctx: &Ctx) -> Vec<Action> {
        let actions = self.save_focused_field(ctx);
        if !actions.is_empty() {
            return actions;
        }
        let order = [4, 0, 1, 2, 3];
        let at = order
            .iter()
            .position(|focus| *focus == self.focus)
            .unwrap_or(0);
        let next = if backwards {
            (at + 4) % 5
        } else {
            (at + 1) % 5
        };
        self.focus = order[next];
        if self.focus == 3 && !self.focus_local_name(ctx) {
            self.focus = if backwards { 2 } else { 4 };
        }
        self.search_panel.completion.close();
        vec![]
    }
    pub fn hints(&self) -> Hints {
        if let Some(hints) = self.preview.hints() {
            return hints;
        }
        if self.focus == 1 && self.search_panel.completion.active() {
            return &[
                ("↑↓", "suggestions"),
                ("Enter", "complete"),
                ("Enter", "list"),
                ("Esc", "close suggestions"),
            ];
        }
        match self.focus {
            2 => &[
                ("Space", "select"),
                ("v", "preview"),
                ("Enter", "install selected"),
                ("n", "source name"),
                ("a", "storage folder"),
                ("e", "skill name"),
                ("/", "filter"),
                ("Esc", "cancel"),
            ],
            _ => &[
                ("type", "edit"),
                ("Enter/↓", "list"),
                ("Tab/⇧Tab", "fields"),
                ("Esc", "cancel"),
            ],
        }
    }
    pub fn paste(&mut self, text: &str, ctx: &Ctx) -> Vec<Action> {
        self.configure(ctx);
        if self.preview.is_open() {
            return vec![];
        }
        let input = match self.focus {
            0 => &mut self.alias,
            1 => &mut self.search_panel.input,
            3 => &mut self.local_name,
            4 => &mut self.source_name,
            _ => return vec![],
        };
        match input.paste(text) {
            Ok(true) if self.focus == 1 => {
                self.refilter();
                self.update_completion(ctx);
                vec![]
            }
            Ok(_) => vec![],
            Err(error) => vec![Action::Error(error.into())],
        }
    }

    pub fn key(&mut self, k: KeyEvent, ctx: &Ctx) -> Vec<Action> {
        let k = if self.focus == 2 && k.code == KeyCode::Char('q') && k.modifiers.is_empty() {
            KeyEvent::new(KeyCode::Esc, k.modifiers)
        } else {
            k
        };
        self.configure(ctx);
        if self.preview.handle_key(k) {
            return vec![];
        }
        if self.focus == 1 && !matches!(k.code, KeyCode::Tab | KeyCode::BackTab) {
            match self.search_panel.key(k) {
                SearchEvent::Changed | SearchEvent::Accepted => {
                    self.refilter();
                    self.update_completion(ctx);
                    return vec![];
                }
                SearchEvent::CursorMoved => {
                    self.update_completion(ctx);
                    return vec![];
                }
                SearchEvent::Results => {
                    self.focus = 2;
                    return vec![];
                }
                SearchEvent::Escape => {
                    self.focus = 2;
                    return vec![];
                }
                _ => return vec![],
            }
        }
        if k.code == KeyCode::Esc {
            self.selection.fetched.cleanup();
            return vec![Action::CloseModal];
        }
        if matches!(k.code, KeyCode::Tab | KeyCode::BackTab) {
            return self.next_focus(k.code == KeyCode::BackTab, ctx);
        }
        if self.focus != 2 {
            if matches!(k.code, KeyCode::Enter | KeyCode::Down) {
                let actions = self.save_focused_field(ctx);
                if !actions.is_empty() {
                    return actions;
                }
                self.focus = 2;
                self.search_panel.completion.close();
                return vec![];
            }
            match self.focus {
                0 => {
                    self.alias.handle_key(k);
                }
                1 => {
                    if self.search_panel.input.handle_key(k) {
                        self.refilter();
                        self.update_completion(ctx);
                    }
                }
                3 => {
                    self.local_name.handle_key(k);
                }
                4 => {
                    self.source_name.handle_key(k);
                }
                _ => {}
            }
            return vec![];
        }
        match k.code {
            KeyCode::Char('v') => {
                if let Some(path) = self
                    .selected()
                    .filter(|path| self.candidates.get(path).is_some())
                {
                    self.preview.open(path);
                }
            }
            KeyCode::Down => self.list.move_by(1, self.shown.len()),
            KeyCode::Up => self.list.move_by(-1, self.shown.len()),
            KeyCode::PageDown => self
                .list
                .move_by(self.list.rows.height as i32, self.shown.len()),
            KeyCode::PageUp => self
                .list
                .move_by(-(self.list.rows.height as i32), self.shown.len()),
            KeyCode::Char('/') => self.focus = 1,
            KeyCode::Char('a') => self.focus = 0,
            KeyCode::Char('n') => self.focus = 4,
            KeyCode::Char('e') => {
                self.focus_local_name(ctx);
            }
            KeyCode::Char(' ') => {
                if let Some(path) = self.selected() {
                    return self.toggle(&path, ctx);
                }
            }
            KeyCode::Enter => {
                let alias = self.alias.value().trim();
                if !skills::util::valid_skill_key(alias) {
                    self.focus = 0;
                    return vec![Action::Error("invalid storage folder".into())];
                }
                self.selection.fetched.repository.alias = alias.into();
                let actions = self.save_source_name(ctx);
                if !actions.is_empty() {
                    return actions;
                }
                let mut selection = self.selection.clone();
                selection.paths.retain(|p| self.matches_filter(p));
                if selection.paths.is_empty() {
                    return vec![Action::Error(
                        "select at least one skill in the current filter".into(),
                    )];
                }
                match selection
                    .fetched
                    .resolved_names(ctx.ws, &selection.paths, &selection.names)
                {
                    Ok(names) => selection.names = names,
                    Err(error) => return vec![Action::Error(format!("{error:#}"))],
                }
                return vec![
                    Action::CloseModal,
                    Action::Spawn(Task::InstallRepository(Box::new(selection))),
                ];
            }
            _ => {}
        }
        vec![]
    }
    pub fn mouse(&mut self, m: MouseEvent, ctx: &Ctx) -> Vec<Action> {
        self.configure(ctx);
        if self.preview.handle_mouse(m, ctx) {
            return vec![];
        }
        if self.focus == 1 {
            let (consumed, accepted) = self.search_panel.mouse_completion(m);
            if accepted {
                self.refilter();
                self.update_completion(ctx);
            }
            if consumed {
                return vec![];
            }
        }
        let at = (m.column, m.row).into();
        match m.kind {
            MouseEventKind::ScrollDown if self.list.rows.contains(at) => self
                .list
                .move_by(ctx.settings.interaction.wheel_rows, self.shown.len()),
            MouseEventKind::ScrollUp if self.list.rows.contains(at) => self
                .list
                .move_by(-ctx.settings.interaction.wheel_rows, self.shown.len()),
            MouseEventKind::Down(MouseButton::Left) => {
                if !self.rect.contains(at) {
                    return self.key(KeyEvent::new(KeyCode::Esc, m.modifiers), ctx);
                }
                let focused_field = match self.focus {
                    0 => Some(0),
                    1 => Some(1),
                    3 => Some(2),
                    4 => Some(3),
                    _ => None,
                };
                if focused_field.is_some_and(|i| !self.fields[i].contains(at)) {
                    let actions = self.save_focused_field(ctx);
                    if !actions.is_empty() {
                        return actions;
                    }
                }
                if self.fields[2].contains(at) {
                    if self.focus != 3 {
                        self.focus_local_name(ctx);
                    }
                    self.local_name.click(m.column);
                    return vec![];
                }
                if self.fields[0].contains(at) {
                    self.focus = 0;
                    self.alias.click(m.column);
                } else if self.search_panel.click_input(m.column, m.row) {
                    self.focus = 1;
                    self.update_completion(ctx);
                } else if self.fields[3].contains(at) {
                    self.focus = 4;
                    self.source_name.click(m.column);
                } else if self.list.rows.contains(at)
                    && let Some((i, _)) = self.list.click(m.row, self.shown.len())
                {
                    self.focus = 2;
                    return self.toggle(&self.shown[i].clone(), ctx);
                }
            }
            _ => {}
        }
        vec![]
    }
    pub fn draw(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        self.configure(ctx);
        let th = &ctx.settings.theme;
        let w = area.width.saturating_sub(4).min(110);
        let h = area.height.saturating_sub(4).min(32);
        let r = Rect::new(
            area.x + (area.width - w) / 2,
            area.y + (area.height - h) / 2,
            w,
            h,
        );
        self.rect = r;
        f.render_widget(OverlayClear, r);
        let block = th.block(
            format!(
                " install · {} selected to install · {} outside filter (excluded) ",
                self.selection
                    .paths
                    .iter()
                    .filter(|p| self.matches_filter(p))
                    .count(),
                self.selection
                    .paths
                    .iter()
                    .filter(|p| !self.matches_filter(p))
                    .count()
            ),
            true,
        );
        let inner = block.inner(r);
        f.render_widget(block, r);
        self.fields = [Rect::default(); 4];
        self.list.rows = Rect::default();
        if inner.height < 6 {
            return;
        }
        let archive = self.selection.fetched.repository.kind == SourceKind::Archive;
        let compact = inner.width < 56;
        let source_label = match (archive, compact) {
            (true, false) => "Package name (required)",
            (true, true) => "Package name *",
            (false, _) => "Source name",
        };
        let label_width = if compact { 16 } else { 24 }.min(inner.width);
        for (i, label) in [
            (3, source_label),
            (0, "Storage folder"),
            (2, "Local skill name"),
        ] {
            let y = if i == 2 {
                inner.bottom() - 1
            } else {
                inner.y + if i == 3 { 0 } else { i as u16 + 1 }
            };
            f.render_widget(
                Paragraph::new(Span::styled(label, th.dim())),
                Rect::new(inner.x, y, label_width, 1),
            );
            self.fields[i] = Rect::new(
                inner.x + label_width,
                y,
                inner.width.saturating_sub(label_width),
                1,
            );
        }
        self.source_name.render(
            f,
            self.fields[3],
            self.focus == 4,
            if archive {
                "required · e.g. merlin"
            } else {
                "owner/repository"
            },
            th,
        );
        self.alias
            .render(f, self.fields[0], self.focus == 0, "", th);
        if self.focus == 3 {
            self.local_name.render(f, self.fields[2], true, "", th);
        } else if let Some(path) = self.selected() {
            let name = self.resolved_name(&path, ctx);
            f.render_widget(
                Paragraph::new(fit(&name, self.fields[2].width as usize)),
                self.fields[2],
            );
        }
        let areas = self.search_panel.draw(
            f,
            Rect::new(
                inner.x,
                inner.y + 2,
                inner.width,
                inner.height.saturating_sub(3),
            ),
            PanelStyle {
                layout: PanelLayout::Unified,
                input_title: Line::default(),
                results_title: Line::from(" Candidate skills "),
                hint: (
                    "Filter skills…",
                    " · repo:owner/repo · status:invalid · keywords",
                ),
                input_active: self.focus == 1,
                results_active: self.focus == 2,
                header_height: 0,
            },
            th,
        );
        self.fields[1] = areas.input;
        self.list.rows = areas.results;
        let root_skill = self.selection.fetched.choices.iter().any(String::is_empty);
        let rows: Vec<_> = self
            .shown
            .iter()
            .map(|path| {
                let skill =
                    self.selection.fetched.choices.contains(path) && self.matches_filter(path);
                let selected = skill && self.selection.paths.contains(path);
                let disabled = if skill && !selected {
                    self.disabled(path, ctx)
                } else {
                    None
                };
                let depth = if path.is_empty() {
                    0
                } else {
                    path.matches('/').count() + usize::from(root_skill)
                };
                let mark = if !skill {
                    "    "
                } else if selected {
                    "[x] "
                } else if disabled.is_some() {
                    "[-] "
                } else {
                    "[ ] "
                };
                let label = if path.is_empty() {
                    ". (source root)"
                } else {
                    path.rsplit('/').next().unwrap_or(path)
                };
                let label = self
                    .candidates
                    .get(path)
                    .and_then(|r| r.name.as_deref())
                    .unwrap_or(label);
                let suffix = disabled
                    .map(|s| format!("  ({s})"))
                    .or_else(|| {
                        (skill && self.restores_missing(path, ctx))
                            .then(|| "  (missing locally — restore)".into())
                    })
                    .unwrap_or_default();
                let style = if self.selection.fetched.invalid.contains_key(path) {
                    th.err()
                } else if !skill || !suffix.is_empty() {
                    th.dim()
                } else {
                    th.bold()
                };
                ListItem::new(Line::from(Span::styled(
                    fit(
                        &format!("{}{mark}{label}{suffix}", "  ".repeat(depth)),
                        inner.width.saturating_sub(2) as usize,
                    ),
                    style,
                )))
            })
            .collect();
        f.render_stateful_widget(
            List::new(rows)
                .highlight_symbol("› ")
                .highlight_style(th.selected()),
            self.list.rows,
            &mut self.list.state,
        );
        if self.focus == 1 {
            self.search_panel.completion.draw(f, self.list.rows, ctx);
        }
        let candidate_ctx = Ctx {
            ws: ctx.ws,
            snap: &self.candidates,
            settings: ctx.settings,
        };
        self.preview.draw(f, area, &candidate_ctx);
    }
}

/// Candidate records stay in memory; browsing never creates metadata or copies skills.
fn candidate_snapshot(fetched: &FetchedRepository) -> Snapshot {
    let skills = fetched
        .choices
        .iter()
        .map(|key| {
            let path = fetched.workdir.join(key);
            let doc = skills::skill::SkillDoc::load(&path).ok();
            SkillRecord {
                key: key.clone(),
                path,
                status: fetched
                    .invalid
                    .get(key)
                    .map(|reason| SkillStatus::Invalid {
                        reason: reason.clone(),
                    })
                    .unwrap_or(SkillStatus::Repository),
                name: doc.as_ref().map(|doc| doc.name.clone()),
                description: doc.as_ref().map(|doc| doc.description.clone()),
                body: doc.map(|doc| doc.body),
                external: false,
                tags: vec![],
                presets: vec![],
                note: None,
                source: Some(fetched.repository.source(key, Some(&fetched.revision))),
                source_name: Some(fetched.repository.display_name()),
                current_hash: None,
                baseline_hash: None,
                deploy: BTreeMap::new(),
                meta: None,
            }
        })
        .collect();
    Snapshot {
        root: fetched.workdir.clone(),
        skills,
        agents: vec![],
        presets: Default::default(),
        repositories: [(fetched.repository.alias.clone(), fetched.repository.clone())]
            .into_iter()
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::super::theme::Theme;
    use super::*;
    use crossterm::event::KeyModifiers;
    use ratatui::{Terminal, backend::TestBackend};
    use skills::{Workspace, config::Config, repository::Repository};

    #[test]
    fn archive_picker_preserves_source_kind_and_excludes_only_matching_installs() {
        let temp = skills::ops::DownloadDir::new("archive-picker").unwrap();
        let root = temp.path().join("library");
        Config {
            agents: vec![],
            ..Default::default()
        }
        .save(&root)
        .unwrap();
        let ws = Workspace::open(&root).unwrap();
        let fetched = FetchedRepository {
            repository: Repository {
                name: Some("sample/tools".into()),
                kind: skills::meta::SourceKind::Archive,
                alias: "sample-tools".into(),
                url: "https://example.com/sample/tools".into(),
                branch: String::new(),
            },
            revision: "digest".into(),
            workdir: temp.path().join("download"),
            choices: vec!["bundle/reader".into(), "bundle/writer".into()],
            invalid: BTreeMap::new(),
        };
        for name in ["reader", "writer"] {
            let path = fetched.workdir.join("bundle").join(name);
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(
                path.join("SKILL.md"),
                format!("---\nname: {name}\ndescription: Sample tool\n---\nBody\n"),
            )
            .unwrap();
        }
        let mut snap = candidate_snapshot(&fetched);
        assert!(
            snap.skills
                .iter()
                .all(|skill| matches!(skill.source, Some(skills::meta::Source::Archive { .. })))
        );
        snap.skills[0].key = "repos/previous/reader".into();
        snap.skills[1].key = "repos/git-source/writer".into();
        snap.skills[1].source = Some(skills::meta::Source::Git {
            url: fetched.repository.url.clone(),
            branch: Some("main".into()),
            subpath: Some("bundle/writer".into()),
            revision: Some("commit".into()),
        });
        let settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &settings,
        };
        let mut picker = RepositoryPicker::new(fetched, &ctx);
        picker.configure(&ctx);
        assert!(picker.selection.paths.is_empty());
        assert_eq!(
            picker.disabled("bundle/reader", &ctx).as_deref(),
            Some("already installed")
        );
        picker.toggle("bundle/writer", &ctx);
        assert_eq!(picker.selection.paths, ["bundle/writer"]);
        picker.toggle("bundle/writer", &ctx);
        assert_eq!(picker.disabled("bundle/writer", &ctx), None);
        picker.focus = 1;
        picker.search_panel.input = Input::with_value("repo:sample/");
        picker.update_completion(&ctx);
        assert!(picker.search_panel.completion.active());
        picker.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &ctx);
        assert_eq!(picker.search_panel.input.value(), "repo:sample/tools ");
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|f| picker.draw(f, f.area(), &ctx)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("reader"));
        assert!(text.contains("writer"));
    }

    #[test]
    fn missing_matching_skill_is_selectable_for_restore() {
        let temp = skills::ops::DownloadDir::new("missing-restore-picker").unwrap();
        let root = temp.path().join("library");
        Config {
            agents: vec![],
            ..Default::default()
        }
        .save(&root)
        .unwrap();
        let ws = Workspace::open(&root).unwrap();
        let fetched = FetchedRepository {
            repository: Repository {
                name: None,
                kind: skills::meta::SourceKind::Git,
                alias: "sample".into(),
                url: "https://example.com/sample.git".into(),
                branch: "main".into(),
            },
            revision: "next".into(),
            workdir: temp.path().join("download"),
            choices: vec!["skills/reader".into()],
            invalid: BTreeMap::new(),
        };
        let path = fetched.workdir.join("skills/reader");
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(
            path.join("SKILL.md"),
            "---\nname: reader\ndescription: Sample tool\n---\nBody\n",
        )
        .unwrap();
        let mut snap = candidate_snapshot(&fetched);
        snap.skills[0].key = "repos/sample/custom-reader".into();
        snap.skills[0].status = SkillStatus::Missing;
        let settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &settings,
        };
        let mut picker = RepositoryPicker::new(fetched, &ctx);
        picker.configure(&ctx);
        assert_eq!(picker.disabled("skills/reader", &ctx), None);
        assert!(picker.restores_missing("skills/reader", &ctx));
        assert!(picker.toggle("skills/reader", &ctx).is_empty());
        assert_eq!(picker.selection.paths, ["skills/reader"]);
    }

    #[test]
    fn archive_name_is_required_and_retained_through_browsing_and_install_retry() {
        let temp = skills::ops::DownloadDir::new("archive-name-picker").unwrap();
        let root = temp.path().join("library");
        Config {
            agents: vec![],
            ..Default::default()
        }
        .save(&root)
        .unwrap();
        let ws = Workspace::open(&root).unwrap();
        let snap = ws.scan().unwrap();
        let settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &settings,
        };
        let fetched = FetchedRepository {
            repository: Repository {
                name: None,
                kind: SourceKind::Archive,
                alias: "latest--skills.tar".into(),
                url: "https://example.com/latest/skills.tar".into(),
                branch: String::new(),
            },
            revision: "digest".into(),
            workdir: temp.path().join("download"),
            choices: vec!["reader".into()],
            invalid: BTreeMap::new(),
        };
        std::fs::create_dir_all(fetched.workdir.join("reader")).unwrap();
        std::fs::write(
            fetched.workdir.join("reader/SKILL.md"),
            "---\nname: reader\ndescription: Read a project\n---\nRead code\n",
        )
        .unwrap();
        let mut picker = RepositoryPicker::new(fetched, &ctx);
        let press = |picker: &mut RepositoryPicker, code| {
            picker.key(KeyEvent::new(code, KeyModifiers::NONE), &ctx)
        };
        assert_eq!(picker.focus, 4);
        assert!(picker.source_name.value().is_empty());
        picker.focus = 2;
        assert!(matches!(
            &press(&mut picker, KeyCode::Enter)[0],
            Action::Error(_)
        ));
        assert_eq!(picker.focus, 4);

        assert!(picker.paste("Merlin Tools", &ctx).is_empty());
        assert!(press(&mut picker, KeyCode::Enter).is_empty());
        assert_eq!(picker.focus, 2);
        assert_eq!(
            picker.selection.fetched.repository.name.as_deref(),
            Some("Merlin Tools")
        );
        assert_eq!(
            picker
                .candidates
                .get("reader")
                .unwrap()
                .source_name
                .as_deref(),
            Some("Merlin Tools")
        );
        assert_eq!(
            picker.selection.fetched.repository.alias,
            "latest--skills.tar"
        );

        press(&mut picker, KeyCode::Char('/'));
        picker.paste("repo:\"Merlin Tools\"", &ctx);
        assert!(picker.matches_filter("reader"));
        picker.search_panel.input = Input::default();
        picker.refilter();
        picker.search_panel.completion.close();
        press(&mut picker, KeyCode::Enter);
        press(&mut picker, KeyCode::Char('v'));
        assert!(picker.preview.is_open());
        press(&mut picker, KeyCode::Esc);
        assert_eq!(picker.source_name.value(), "Merlin Tools");

        picker.toggle("reader", &ctx);
        press(&mut picker, KeyCode::Char('e'));
        picker.local_name = Input::with_value("project-reader");
        press(&mut picker, KeyCode::Enter);
        let selected = press(&mut picker, KeyCode::Enter)
            .into_iter()
            .find_map(|action| match action {
                Action::Spawn(Task::InstallRepository(selection)) => Some(*selection),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            selected.names.get("reader").map(String::as_str),
            Some("project-reader")
        );
        let mut restored = RepositoryPicker::restore(selected);
        assert_eq!(restored.source_name.value(), "Merlin Tools");
        assert_eq!(
            restored.selection.fetched.repository.alias,
            "latest--skills.tar"
        );
        assert_eq!(
            restored.selection.names.get("reader").map(String::as_str),
            Some("project-reader")
        );

        for (width, height) in [(120, 35), (80, 24), (44, 20), (24, 12)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|f| restored.draw(f, f.area(), &ctx)).unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            if width >= 80 {
                assert!(text.contains("Package name (required)"));
                assert!(text.contains("Merlin Tools"));
                assert!(text.contains("Storage folder"));
                assert!(text.contains("Local skill name"));
            } else if width >= 44 {
                assert!(text.contains("Package name *"));
                assert!(text.contains("Merlin Tools"));
            }
            assert!(
                restored
                    .fields
                    .iter()
                    .all(|field| field.right() <= width && field.bottom() <= height)
            );
        }
    }

    #[test]
    fn source_name_navigation_is_separate_from_storage_and_skill_names() {
        let temp = skills::ops::DownloadDir::new("source-name-navigation").unwrap();
        let root = temp.path().join("library");
        Config {
            agents: vec![],
            ..Default::default()
        }
        .save(&root)
        .unwrap();
        let ws = Workspace::open(&root).unwrap();
        let snap = ws.scan().unwrap();
        let settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &settings,
        };
        let mut picker = RepositoryPicker::restore(InstallSelection {
            fetched: FetchedRepository {
                repository: Repository {
                    name: None,
                    kind: SourceKind::Git,
                    alias: "sample--tools".into(),
                    url: "https://github.com/sample/tools".into(),
                    branch: "main".into(),
                },
                revision: "revision".into(),
                workdir: temp.path().join("download"),
                choices: vec!["reader".into()],
                invalid: BTreeMap::new(),
            },
            paths: vec!["reader".into()],
            names: BTreeMap::new(),
        });
        assert_eq!(picker.source_name.value(), "sample/tools");
        assert_eq!(picker.focus, 1);
        let press = |picker: &mut RepositoryPicker, code| {
            picker.key(KeyEvent::new(code, KeyModifiers::NONE), &ctx)
        };
        press(&mut picker, KeyCode::BackTab);
        assert_eq!(picker.focus, 0);
        press(&mut picker, KeyCode::BackTab);
        assert_eq!(picker.focus, 4);
        picker.source_name = Input::with_value("Custom tools");
        press(&mut picker, KeyCode::Tab);
        assert_eq!(picker.focus, 0);
        assert_eq!(
            picker.selection.fetched.repository.name.as_deref(),
            Some("Custom tools")
        );
        assert_eq!(picker.selection.fetched.repository.alias, "sample--tools");
        assert!(picker.selection.names.is_empty());

        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| picker.draw(f, f.area(), &ctx)).unwrap();
        picker.mouse(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: picker.fields[3].x,
                row: picker.fields[3].y,
                modifiers: KeyModifiers::NONE,
            },
            &ctx,
        );
        assert_eq!(picker.focus, 4);
        picker.source_name = Input::with_value("Changed tools");
        picker.mouse(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: picker.fields[1].x,
                row: picker.fields[1].y,
                modifiers: KeyModifiers::NONE,
            },
            &ctx,
        );
        assert_eq!(picker.focus, 1);
        assert_eq!(
            picker.selection.fetched.repository.name.as_deref(),
            Some("Changed tools")
        );
    }

    #[test]
    fn candidate_search_reuses_names_body_fuzzy_filters_and_preview() {
        let temp = skills::ops::DownloadDir::new("candidate-search").unwrap();
        let root = temp.path();
        Config {
            agents: vec![],
            ..Default::default()
        }
        .save(root)
        .unwrap();
        for (path, name, body) in [
            ("skills/print", "printer", "Observability workflows"),
            ("internal/print", "internal-printer", "Internal helpers"),
        ] {
            std::fs::create_dir_all(root.join(path)).unwrap();
            std::fs::write(
                root.join(path).join("SKILL.md"),
                format!("---\nname: {name}\ndescription: Document tools\n---\n{body}\n"),
            )
            .unwrap();
        }
        let ws = Workspace::open(root).unwrap();
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
        let mut picker = RepositoryPicker::new(
            FetchedRepository {
                repository: Repository {
                    name: None,
                    kind: Default::default(),
                    alias: "sample--tools".into(),
                    url: "https://github.com/sample/tools".into(),
                    branch: "main".into(),
                },
                invalid: BTreeMap::new(),
                revision: "test".into(),
                workdir: root.to_path_buf(),
                choices: vec!["skills/print".into(), "internal/print".into()],
            },
            &ctx,
        );
        picker.configure(&ctx);
        picker.focus = 1;
        picker.search_panel.input = Input::with_value("repo:sampl");
        picker.update_completion(&ctx);
        assert!(picker.search_panel.completion.active());
        picker.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &ctx);
        assert_eq!(picker.search_panel.input.value(), "repo:sample/tools ");
        for token in ["tag:", "agent:", "status:modified"] {
            picker.search_panel.input = Input::with_value(token);
            picker.update_completion(&ctx);
            assert!(
                !picker.search_panel.completion.active(),
                "installation must not suggest unavailable {token}"
            );
        }
        for query in [
            "observability",
            "skills/ prnter",
            "repo:sample/tools skills/",
            "status:repository skills/",
        ] {
            picker.search_panel.input = Input::with_value(query);
            picker.refilter();
            assert!(picker.matches_filter("skills/print"), "{query}");
            assert!(!picker.matches_filter("internal/print"), "{query}");
        }

        // A still-open picker follows the resolved configuration without
        // clearing its query or the installation choices already staged.
        picker.search_panel.input = Input::with_value("skills/ prnter");
        picker.refilter();
        let staged = picker.selection.paths.clone();
        assert!(picker.matches_filter("skills/print"));
        let mut settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
        settings.search.fuzzy = false;
        let changed_ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &settings,
        };
        picker.configure(&changed_ctx);
        assert!(picker.shown.is_empty());
        assert_eq!(picker.search_panel.input.value(), "skills/ prnter");
        assert_eq!(picker.selection.paths, staged);
        picker.refresh(&ctx);
        assert!(picker.matches_filter("skills/print"));

        picker.search_panel.input = Input::with_value("repo:other/tools");
        picker.refilter();
        assert!(picker.shown.is_empty());
        picker.search_panel.input = Input::with_value("skills/");
        picker.refilter();
        picker.focus = 2;
        picker.list.move_by(1, picker.shown.len());
        assert!(
            picker
                .key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE), &ctx)
                .is_empty()
        );
        assert!(picker.preview.is_open());
        assert!(
            picker
                .key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &ctx)
                .is_empty()
        );
        assert!(!picker.preview.is_open());
        assert!(root.join("skills/print/SKILL.md").exists());
    }

    #[test]
    fn directory_queries_are_root_relative_and_keep_tree_ancestors() {
        let mut picker = RepositoryPicker::restore(InstallSelection {
            fetched: FetchedRepository {
                repository: Repository {
                    name: None,
                    kind: Default::default(),
                    alias: "sampleorg--kit".into(),
                    url: "https://github.com/sampleorg/kit".into(),
                    branch: "main".into(),
                },
                invalid: BTreeMap::new(),
                revision: "test".into(),
                workdir: std::path::PathBuf::new(),
                choices: vec![
                    "skills/mock-real".into(),
                    "skills/group/mock-other".into(),
                    "internal/tests/skills/mock-demo".into(),
                    "skills-extra/demo".into(),
                ],
            },
            paths: vec!["internal/tests/skills/mock-demo".into()],
            names: BTreeMap::new(),
        });
        for query in ["skills/", "./skills/", "/skills/"] {
            picker.search_panel.input = Input::with_value(query);
            picker.refilter();
            assert_eq!(
                picker.shown,
                vec![
                    "skills",
                    "skills/group",
                    "skills/group/mock-other",
                    "skills/mock-real"
                ]
            );
        }
        picker.search_panel.input = Input::with_value("internal/tests/skills/");
        picker.refilter();
        assert!(
            picker
                .shown
                .contains(&"internal/tests/skills/mock-demo".into())
        );
        assert!(!picker.shown.contains(&"skills".into()));
        picker.search_panel.input = Input::with_value("mock-demo");
        picker.refilter();
        assert!(picker.shown.contains(&"internal".into()));
        assert!(
            picker
                .shown
                .contains(&"internal/tests/skills/mock-demo".into())
        );
        picker.search_panel.input = Input::with_value("missing/");
        picker.refilter();
        assert!(picker.shown.is_empty());
        assert_eq!(
            picker.selection.paths,
            vec!["internal/tests/skills/mock-demo"]
        );
    }

    #[test]
    fn tree_selection_disables_only_ancestors_and_descendants_for_keys_and_mouse() {
        let root = std::env::temp_dir().join(format!("skills-tree-picker-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        Config {
            agents: vec![],
            ..Default::default()
        }
        .save(&root)
        .unwrap();
        let ws = Workspace::open(&root).unwrap();
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
        let fetched = FetchedRepository {
            repository: Repository {
                name: None,
                kind: Default::default(),
                alias: "sample--tools".into(),
                url: "https://example.com/sample/tools.git".into(),
                branch: "main".into(),
            },
            invalid: BTreeMap::new(),
            revision: "sample".into(),
            workdir: root.join("staging"),
            choices: vec![
                "tools".into(),
                "tools/reader".into(),
                "tools/reader/formatter".into(),
                "tools/sibling/leaf".into(),
                "other/printer".into(),
            ],
        };
        for path in &fetched.choices {
            let dir = fetched.workdir.join(path);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("SKILL.md"),
                format!("---\nname: {}\n---\nbody", path.rsplit('/').next().unwrap()),
            )
            .unwrap();
        }
        let mut picker = RepositoryPicker::new(fetched, &ctx);
        assert!(picker.selection.paths.is_empty());
        picker.toggle("tools", &ctx);
        assert!(!picker.toggle("tools/reader", &ctx).is_empty());
        picker.toggle("tools", &ctx);
        picker.toggle("tools/reader", &ctx);
        picker.toggle("tools/sibling/leaf", &ctx);
        assert!(
            picker
                .selection
                .paths
                .contains(&"tools/sibling/leaf".into())
        );
        assert!(!picker.toggle("tools/reader/formatter", &ctx).is_empty());
        assert!(!picker.toggle("tools", &ctx).is_empty());
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|f| picker.draw(f, f.area(), &ctx)).unwrap();
        let index = picker
            .shown
            .iter()
            .position(|p| p == "tools/reader/formatter")
            .unwrap();
        let before = picker.selection.paths.clone();
        picker.mouse(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: picker.list.rows.x + 2,
                row: picker.list.rows.y + index as u16,
                modifiers: KeyModifiers::NONE,
            },
            &ctx,
        );
        assert_eq!(picker.selection.paths, before);
        picker.focus = 1;
        for c in "reader formatter".chars() {
            picker.key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE), &ctx);
        }
        assert_eq!(picker.selection.paths, before); // Spaces in search never toggle.
        picker.search_panel.input = Input::with_value("tools/");
        picker.refilter();
        picker.focus = 2;
        let actions = picker.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &ctx);
        let selection = actions
            .into_iter()
            .find_map(|a| match a {
                Action::Spawn(Task::InstallRepository(s)) => Some(s),
                _ => None,
            })
            .unwrap();
        assert_eq!(selection.paths, vec!["tools/reader", "tools/sibling/leaf"]);
        assert!(!picker.selection.paths.contains(&"other/printer".into()));
        picker.selection.paths = vec!["tools".into(), "other/printer".into()];
        // A tree ancestor retained solely for context is not an installation result.
        assert!(picker.shown.contains(&"tools".into()));
        let actions = picker.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &ctx);
        assert!(matches!(&actions[0], Action::Error(_)));
        assert!(picker.toggle("tools/reader", &ctx).is_empty());
        assert!(!picker.selection.paths.contains(&"tools".into()));
        picker.selection.fetched.invalid.insert(
            "tools/sibling/leaf".into(),
            "frontmatter has no `name`".into(),
        );
        assert!(!picker.toggle("tools/sibling/leaf", &ctx).is_empty());
        let fetched = picker.selection.fetched.clone();
        let defaults = RepositoryPicker::new(fetched, &ctx);
        assert!(
            !defaults
                .selection
                .paths
                .contains(&"tools/sibling/leaf".into())
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
