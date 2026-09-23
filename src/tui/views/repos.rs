//! Installed sources with an embedded, shared Library skill panel.
use super::search::{SearchView, SkillPanelOptions};
use super::{View, wheel};
use crate::tui::app::{Action, Ctx, Hints};
use crate::tui::components::context_menu::{Command, Item, Request, Target};
use crate::tui::components::layout::{frame, split_panes};
use crate::tui::event::Task;
use crate::tui::modal::Modal;
use crate::tui::settings::LayoutScope;
use crate::tui::widgets::{CardGrid, fit, width};
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
};
use skills::repository::{RepositoryInventory, RepositoryInventoryState, alias_of};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone)]
struct Project {
    alias: Option<String>,
    name: String,
    local: bool,
    source: String,
    keys: Vec<String>,
}

#[derive(Default)]
pub struct ReposView {
    projects: Vec<Project>,
    all_projects: Vec<Project>,
    filter: super::filter::Filter,
    skill_search: Option<SearchView>,
    panel_source: Option<Option<String>>,
    nav: CardGrid,
    focus_skills: bool,
    inventories: BTreeMap<String, RepositoryInventory>,
    inventory_errors: BTreeMap<String, String>,
    refreshing: BTreeSet<String>,
    scroll: u16,
    left: Rect,
    details: Rect,
    skills: Rect,
}

impl ReposView {
    pub fn batch_finished(&mut self, failed: &[String]) {
        if let Some(view) = self.skill_search.as_mut() {
            view.batch_finished(failed);
        }
    }

    pub fn remember_checks(
        &mut self,
        results: &[(String, anyhow::Result<skills::ops::update::CheckResult>)],
    ) {
        if let Some(view) = self.skill_search.as_mut() {
            view.remember_checks(results);
        }
    }

    pub fn refreshing(&mut self, alias: &str) {
        self.refreshing.insert(alias.to_string());
        self.inventory_errors.remove(alias);
    }

    pub fn inventory_result(
        &mut self,
        alias: &str,
        result: &anyhow::Result<RepositoryInventory>,
    ) -> bool {
        // A snapshot refresh clears this marker. Ignore a worker that was
        // started against an older Library state instead of republishing stale
        // local-vs-remote classifications.
        if !self.refreshing.remove(alias) {
            return false;
        }
        match result {
            Ok(inventory) => {
                self.inventories
                    .insert(alias.to_string(), inventory.clone());
                self.inventory_errors.remove(alias);
            }
            Err(error) => {
                self.inventory_errors
                    .insert(alias.to_string(), format!("{error:#}"));
            }
        }
        true
    }

    pub fn input_focused(&self) -> bool {
        self.filter.editing
            || (self.focus_skills
                && self
                    .skill_search
                    .as_ref()
                    .is_some_and(|v| v.input_focused()))
    }

    pub fn paste(&mut self, text: &str, ctx: &Ctx) -> Vec<Action> {
        if self.focus_skills
            && let Some(view) = self.skill_search.as_mut()
        {
            return view.paste(text, ctx);
        }
        let actions = self.filter.paste(text);
        self.refilter(ctx);
        actions
    }

    fn selected(&self) -> Option<&Project> {
        self.nav.selected().and_then(|i| self.projects.get(i))
    }

    fn refilter(&mut self, ctx: &Ctx) {
        let previous = self.selected().map(|p| p.alias.clone());
        let documents = self
            .all_projects
            .iter()
            .map(|p| skills::search::TextDocument {
                name: p.name.clone(),
                description: String::new(),
                body: format!("{} {}", p.alias.as_deref().unwrap_or(""), p.source),
            })
            .collect::<Vec<_>>();
        self.projects = self
            .filter
            .rank(&documents, ctx)
            .into_iter()
            .map(|i| self.all_projects[i].clone())
            .collect();
        let index = self
            .projects
            .iter()
            .position(|p| Some(&p.alias) == previous.as_ref());
        self.nav.select(index);
        self.nav.clamp(self.projects.len());
        self.sync_panel(ctx, false);
    }

    fn sync_panel(&mut self, ctx: &Ctx, refresh: bool) {
        let project = self.selected().cloned();
        let identity = project.as_ref().map(|p| p.alias.clone());
        let keys = project.as_ref().map(|p| p.keys.clone()).unwrap_or_default();
        if self.skill_search.is_none() || self.panel_source != identity {
            let mut view = SearchView::panel(
                SkillPanelOptions::new(keys, "Source skills".into(), LayoutScope::Repositories),
                ctx,
            );
            view.focus_list();
            self.skill_search = Some(view);
            self.panel_source = identity;
            self.scroll = 0;
        } else if refresh && let Some(view) = self.skill_search.as_mut() {
            view.update_panel(keys, ctx);
        }
    }

    fn move_by(&mut self, delta: i32, ctx: &Ctx) {
        self.nav.move_by(delta, self.projects.len());
        self.sync_panel(ctx, false);
    }

    fn inventory_counts(&self, alias: &str) -> (usize, usize, usize) {
        let Some(inventory) = self.inventories.get(alias) else {
            return (0, 0, 0);
        };
        let mut available = 0;
        let mut updates = 0;
        let mut attention = 0;
        for entry in &inventory.entries {
            match entry.state {
                RepositoryInventoryState::Available => available += 1,
                RepositoryInventoryState::Update { .. } => updates += 1,
                RepositoryInventoryState::PossibleMove { .. } => {
                    available += 1;
                    attention += 1;
                }
                RepositoryInventoryState::Changed { .. }
                | RepositoryInventoryState::MissingUpstream { .. }
                | RepositoryInventoryState::Invalid { .. } => attention += 1,
                RepositoryInventoryState::Installed { .. } => {}
            }
        }
        (available, updates, attention)
    }

    fn inventory_line(entry: &skills::repository::RepositoryInventoryEntry) -> String {
        let path = if entry.path.is_empty() {
            "."
        } else {
            &entry.path
        };
        let name = entry
            .skill
            .as_ref()
            .map(|skill| skill.name.as_str())
            .filter(|name| *name != path)
            .map(|name| format!(" · {name}"))
            .unwrap_or_default();
        let state = match &entry.state {
            RepositoryInventoryState::Installed {
                key,
                covered: false,
            } => {
                format!("installed · {key}")
            }
            RepositoryInventoryState::Installed { key, covered: true } => {
                format!("covered by {key}")
            }
            RepositoryInventoryState::Available => "available · i to install".into(),
            RepositoryInventoryState::Update { key } => format!("update · {key} · u to check"),
            RepositoryInventoryState::Changed {
                key,
                update_available,
            } => format!(
                "local changes{} · {key} · u to resolve",
                if *update_available {
                    " + upstream update"
                } else {
                    ""
                }
            ),
            RepositoryInventoryState::MissingUpstream { key } => {
                format!("missing upstream · {key} · review/remove locally")
            }
            RepositoryInventoryState::PossibleMove { key, from } => {
                format!("possible move from {from} · {key} · review manually")
            }
            RepositoryInventoryState::Invalid { error } => format!("invalid · {error}"),
        };
        format!(" {path}{name}  [{state}]")
    }

    fn source_icon<'a>(&self, project: &Project, ctx: &'a Ctx) -> &'a str {
        if let Some(repo) = project
            .alias
            .as_ref()
            .and_then(|a| ctx.snap.repositories.get(a))
        {
            return match repo.kind {
                skills::meta::SourceKind::Git => {
                    crate::tui::icons::git(ctx.settings.ui.icons, &repo.url)
                }
                skills::meta::SourceKind::Archive => {
                    crate::tui::icons::package(ctx.settings.ui.icons)
                }
            };
        }
        if ctx.settings.ui.icons == skills::config::Icons::Text {
            if project.local { "local" } else { "?" }
        } else if project.local {
            "󰉋"
        } else {
            ""
        }
    }

    fn detail_lines(&self, ctx: &Ctx) -> Vec<Line<'static>> {
        let th = &ctx.settings.theme;
        let Some(project) = self.selected() else {
            return vec![Line::styled(" No matching sources", th.dim())];
        };
        let mut lines = vec![Line::from(vec![
            Span::styled(
                format!(" {} {}", self.source_icon(project, ctx), project.name),
                th.bold(),
            ),
            Span::styled(
                format!(" · {} skills", project.keys.len()),
                th.skill_count(),
            ),
        ])];
        let mut field = |label: &str, value: String| {
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {label:<8}"),
                    ratatui::style::Style::default().fg(th.placeholder),
                ),
                Span::raw(value),
            ]));
        };
        let repo = project
            .alias
            .as_ref()
            .and_then(|a| ctx.snap.repositories.get(a));
        let kind = if project.local {
            "Local skills"
        } else if let Some(repo) = repo {
            match repo.kind {
                skills::meta::SourceKind::Git => "Git repository",
                skills::meta::SourceKind::Archive => "URL package",
            }
        } else {
            "Unregistered source"
        };
        field("Type", kind.into());
        let path = project
            .alias
            .as_ref()
            .map(|a| ctx.ws.root.join("repos").join(a))
            .unwrap_or_else(|| ctx.ws.root.clone());
        field("Folder", skills::paths::contract_tilde(&path));
        if let Some(repo) = repo {
            field("Alias", repo.alias.clone());
            field("URL", repo.url.clone());
            if !repo.branch.is_empty() {
                field("Branch", repo.branch.clone());
            }
            if self.refreshing.contains(&repo.alias) {
                field("Remote", "refreshing…".into());
            } else if let Some(error) = self.inventory_errors.get(&repo.alias) {
                field("Remote", format!("error: {error}"));
            } else if let Some(inventory) = self.inventories.get(&repo.alias) {
                let (available, updates, attention) = self.inventory_counts(&repo.alias);
                field(
                    "Remote",
                    format!("{available} available · {updates} updates · {attention} attention"),
                );
                lines.push(Line::styled(" Inventory", th.bold()));
                if inventory.entries.is_empty() {
                    lines.push(Line::styled(" (no skill boundaries found)", th.dim()));
                } else {
                    lines.extend(
                        inventory
                            .entries
                            .iter()
                            .map(|entry| Line::raw(Self::inventory_line(entry))),
                    );
                }
            } else {
                field("Remote", "not checked · press f".into());
            }
        }
        lines
    }
}

impl View for ReposView {
    fn overlay_open(&self) -> bool {
        self.focus_skills && self.skill_search.as_ref().is_some_and(View::overlay_open)
    }
    fn handle_control_key(&mut self, key: KeyEvent, ctx: &Ctx) -> Vec<Action> {
        if self.focus_skills
            && let Some(view) = self.skill_search.as_mut()
        {
            return view.handle_control_key(key, ctx);
        }
        vec![]
    }
    fn actions_menu(&self, ctx: &Ctx) -> Option<Request> {
        if self.filter.editing {
            return None;
        }
        if self.focus_skills {
            return self.skill_search.as_ref()?.actions_menu(ctx);
        }
        let project = self.selected()?;
        let alias = project.alias.clone()?;
        let registered = ctx.snap.repositories.contains_key(&alias);
        Some(Request {
            title: project.name.clone(),
            detail: "Repository source".into(),
            target: Target::Repository(alias.clone()),
            items: vec![
                Item::new(
                    Command::Refresh,
                    "Refresh source inventory",
                    KeyCode::Char('f'),
                    registered && !self.refreshing.contains(&alias),
                    if self.refreshing.contains(&alias) {
                        "This source is already refreshing"
                    } else {
                        "This source is not registered"
                    },
                    0,
                ),
                Item::new(
                    Command::Check,
                    "Check installed skills",
                    KeyCode::Char('u'),
                    registered && !project.keys.is_empty(),
                    "This source has no installed skills",
                    0,
                ),
                Item::new(
                    Command::Source,
                    "Install more skills",
                    KeyCode::Char('i'),
                    registered,
                    "This source is not registered",
                    0,
                ),
                Item::new(
                    Command::Rename,
                    "Rename source",
                    KeyCode::Char('r'),
                    registered,
                    "This source is not registered",
                    1,
                ),
                Item::new(
                    Command::Remove,
                    "Remove local source and skills…",
                    KeyCode::Char('D'),
                    registered,
                    "This source is not registered",
                    2,
                ),
            ],
        })
    }
    fn context_menu(&mut self, x: u16, y: u16, ctx: &Ctx) -> Option<Request> {
        let view = self.skill_search.as_mut()?;
        let request = view.context_menu(x, y, ctx)?;
        self.focus_skills = true;
        self.filter.editing = false;
        Some(request)
    }
    fn context_execute(&mut self, target: &Target, command: Command, ctx: &Ctx) -> Vec<Action> {
        if let Target::Repository(alias) = target {
            let Some(project) = self
                .selected()
                .filter(|project| project.alias.as_deref() == Some(alias))
            else {
                return vec![Action::Error(
                    "Target changed; reopen the actions menu".into(),
                )];
            };
            let Some(repository) = ctx.snap.repositories.get(alias) else {
                return vec![Action::Error("This source is not registered".into())];
            };
            return match command {
                Command::Rename => vec![Action::OpenModal(Box::new(Modal::rename_source(
                    alias,
                    &project.name,
                )))],
                Command::Refresh if !self.refreshing.contains(alias) => {
                    vec![Action::Spawn(Task::RefreshRepository(alias.clone()))]
                }
                Command::Check if !project.keys.is_empty() => vec![
                    Action::Toast(format!(
                        "checking {} installed skill(s)…",
                        project.keys.len()
                    )),
                    Action::Spawn(Task::Check(project.keys.clone())),
                ],
                Command::Source => match skills::ops::install::InstallRef::from_source(
                    &repository.source("", None),
                ) {
                    Ok(reference) => vec![Action::Spawn(Task::DiscoverRepository {
                        label: repository.url.clone(),
                        reference,
                    })],
                    Err(error) => vec![Action::Error(format!("discover {alias}: {error:#}"))],
                },
                Command::Remove => {
                    match skills::ops::edit::RepositoryRemoveSummary::from_snapshot(ctx.snap, alias)
                    {
                        Ok(summary) => vec![Action::OpenModal(Box::new(Modal::remove_source(
                            alias,
                            &project.name,
                            &summary,
                        )))],
                        Err(error) => {
                            vec![Action::Error(format!("remove source {alias}: {error:#}"))]
                        }
                    }
                }
                _ => vec![Action::Error(
                    "Action is no longer available; reopen the actions menu".into(),
                )],
            };
        }
        match self.skill_search.as_mut() {
            Some(view) => view.context_execute(target, command, ctx),
            None => vec![Action::Error(
                "Target changed; reopen the context menu".into(),
            )],
        }
    }

    fn focus_root(&mut self) {
        self.focus_skills = false;
        self.filter.editing = false;
    }

    fn focus_from_above(&mut self) {
        self.focus_skills = false;
        self.filter.editing = true;
    }

    fn refresh(&mut self, ctx: &Ctx) {
        // Inventory is computed against one concrete Library snapshot. Any new
        // snapshot can change local hashes, installed paths, or source records,
        // so never present the old comparison as current.
        self.inventories.clear();
        self.inventory_errors.clear();
        self.refreshing.clear();
        let mut groups = std::collections::BTreeMap::<Option<String>, Project>::new();
        for repo in ctx.snap.repositories.values() {
            let kind = match repo.kind {
                skills::meta::SourceKind::Git => "Git repository",
                skills::meta::SourceKind::Archive => "URL package",
            };
            groups.insert(
                Some(repo.alias.clone()),
                Project {
                    alias: Some(repo.alias.clone()),
                    name: repo.display_name(),
                    local: false,
                    source: format!("{kind} {} {}", repo.url, repo.alias),
                    keys: vec![],
                },
            );
        }
        for record in &ctx.snap.skills {
            let alias = alias_of(&record.key).map(str::to_string);
            groups
                .entry(alias.clone())
                .or_insert_with(|| Project {
                    local: alias.is_none(),
                    alias: alias.clone(),
                    name: if alias.is_none() {
                        "Local skills".into()
                    } else {
                        record
                            .source_display_name()
                            .unwrap_or("Unregistered source")
                            .into()
                    },
                    source: if alias.is_none() {
                        "Skills outside repository directories".into()
                    } else {
                        "Unregistered source".into()
                    },
                    keys: vec![],
                })
                .keys
                .push(record.key.clone());
        }
        self.all_projects = groups.into_values().collect();
        self.refilter(ctx);
        self.sync_panel(ctx, true);
    }

    fn handle_key(&mut self, k: KeyEvent, ctx: &Ctx) -> Vec<Action> {
        if !self.focus_skills
            && self.filter.editing
            && k.code == KeyCode::Right
            && self.filter.input.cursor_byte() == self.filter.input.value().len()
            && let Some(view) = self.skill_search.as_mut()
        {
            self.filter.editing = false;
            self.focus_skills = true;
            view.focus_input();
            return vec![];
        }
        if self.focus_skills
            && let Some(view) = self.skill_search.as_mut()
        {
            if k.code == KeyCode::Left && view.input_at_left_edge() {
                view.close_input_completion();
                self.focus_skills = false;
                self.filter.editing = true;
                return vec![];
            }
            if k.code == KeyCode::Left && view.panel_back() {
                self.focus_skills = false;
                return vec![];
            }
            let mut actions = view.handle_key(k, ctx);
            if actions.iter().any(|a| matches!(a, Action::BackToParent)) {
                if k.code == KeyCode::Up {
                    return actions;
                }
                self.focus_skills = false;
                self.filter.editing = false;
                actions.retain(|a| !matches!(a, Action::BackToParent));
            }
            return actions;
        }
        if !self.focus_skills && self.filter.editing && k.code == KeyCode::Up {
            self.filter.editing = false;
            return vec![Action::BackToParent];
        }
        if self.filter.key(k) {
            self.refilter(ctx);
            return vec![];
        }
        match k.code {
            KeyCode::Char('r')
            | KeyCode::Char('f')
            | KeyCode::Char('u')
            | KeyCode::Char('i')
            | KeyCode::Char('D') => {
                if let Some(alias) = self.selected().and_then(|project| project.alias.clone()) {
                    let command = match k.code {
                        KeyCode::Char('r') => Command::Rename,
                        KeyCode::Char('f') => Command::Refresh,
                        KeyCode::Char('u') => Command::Check,
                        KeyCode::Char('D') => Command::Remove,
                        _ => Command::Source,
                    };
                    return self.context_execute(&Target::Repository(alias), command, ctx);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => self.move_by(1, ctx),
            KeyCode::Up | KeyCode::Char('k') => {
                if self.nav.selected().unwrap_or(0) == 0 {
                    self.filter.editing = true;
                } else {
                    self.move_by(-1, ctx);
                }
            }
            KeyCode::PageDown => self.move_by(10, ctx),
            KeyCode::PageUp => self.move_by(-10, ctx),
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                self.focus_skills = true;
                if let Some(view) = self.skill_search.as_mut() {
                    view.focus_list();
                }
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                return vec![Action::BackToParent];
            }
            _ => {}
        }
        vec![]
    }

    fn handle_mouse(&mut self, m: MouseEvent, ctx: &Ctx) -> Vec<Action> {
        let point = (m.column, m.row).into();
        if self.skills.contains(point) {
            if m.kind == MouseEventKind::Down(MouseButton::Left) || wheel(&m, ctx).is_some() {
                self.focus_skills = true;
                self.filter.editing = false;
            }
            if let Some(view) = self.skill_search.as_mut() {
                return view.handle_mouse(m, ctx);
            }
        }
        if let Some(delta) = wheel(&m, ctx) {
            if self.left.contains(point) {
                self.focus_skills = false;
                self.filter.editing = false;
                self.move_by(delta, ctx);
            } else if self.details.contains(point) {
                self.focus_skills = false;
                self.filter.editing = false;
                self.scroll = (i32::from(self.scroll) + delta).clamp(0, u16::MAX as i32) as u16;
            }
        } else if m.kind == MouseEventKind::Down(MouseButton::Left) {
            if self.details.contains(point) {
                self.focus_skills = false;
                self.filter.editing = false;
            } else if self.left.contains(point) {
                self.focus_skills = false;
                self.filter.editing = self.filter.click_input(m.column, m.row);
                if !self.filter.editing {
                    self.nav.click(m.column, m.row);
                    self.sync_panel(ctx, false);
                }
            }
        }
        vec![]
    }

    fn draw(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        self.sync_panel(ctx, false);
        let th = &ctx.settings.theme;
        let (left, right) = split_panes(area, 38, ctx);
        self.left = left;
        let inner = self.filter.draw(
            f,
            left,
            "Filter sources",
            &format!("sources ({})", self.projects.len()),
            !self.focus_skills,
            ctx,
        );
        self.nav.layout(
            inner,
            1,
            crate::tui::components::group::card_height(None),
            0,
            self.projects.len(),
        );
        for i in self.nav.visible() {
            let Some(cell) = self.nav.cell(i) else {
                continue;
            };
            let project = &self.projects[i];
            let ci = frame(
                f,
                cell,
                self.nav.selected() == Some(i),
                !self.focus_skills && !self.filter.editing,
                th,
            );
            let columns = ci.width as usize;
            let count = fit(&format!("{} skills", project.keys.len()), columns);
            let room = columns.saturating_sub(width(&count) + 1);
            let icon = fit(&format!("{} ", self.source_icon(project, ctx)), room);
            let name = fit(&project.name, room.saturating_sub(width(&icon)));
            let gap = columns.saturating_sub(width(&icon) + width(&name) + width(&count));
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(icon, th.source()),
                    Span::styled(name, th.bold()),
                    Span::raw(" ".repeat(gap)),
                    Span::styled(count, th.skill_count()),
                ])),
                ci,
            );
        }
        if self.projects.is_empty() {
            f.render_widget(
                Paragraph::new(" No matching sources").style(th.dim()),
                inner,
            );
        }
        let paragraph = Paragraph::new(self.detail_lines(ctx)).wrap(Wrap { trim: false });
        let needed = paragraph
            .line_count(right.width.saturating_sub(2).max(1))
            .saturating_add(2)
            .min(u16::MAX as usize) as u16;
        let height = needed.min(right.height / 3);
        self.details = Rect::new(right.x, right.y, right.width, height);
        self.skills = Rect::new(
            right.x,
            right.y + height,
            right.width,
            right.height.saturating_sub(height),
        );
        if height > 0 {
            let block = th.block(" source details ", false);
            let inner = block.inner(self.details);
            f.render_widget(block, self.details);
            self.scroll = self.scroll.min(
                paragraph
                    .line_count(inner.width.max(1))
                    .saturating_sub(inner.height as usize)
                    .min(u16::MAX as usize) as u16,
            );
            f.render_widget(paragraph.scroll((self.scroll, 0)), inner);
        }
        if let Some(view) = self.skill_search.as_mut() {
            view.set_panel_active(self.focus_skills);
            view.draw(f, self.skills, ctx);
        }
    }

    fn hints(&self) -> Hints {
        if self.filter.editing {
            return &[("Enter/↓", "sources"), ("Esc", "clear filter")];
        }
        if self.focus_skills
            && let Some(view) = self.skill_search.as_ref()
        {
            return view.hints();
        }
        &[
            ("↑↓", "sources"),
            ("Enter/→", "skills"),
            ("a", "actions"),
            ("f", "refresh source"),
            ("u", "check"),
            ("i", "install more"),
            ("D", "remove source"),
            ("/", "filter sources"),
            ("Esc/q", "clear/back"),
        ]
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};
    use skills::{Workspace, config::Config, repository::Repository};

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
        panic!("expected a context menu target in the rendered repository panel");
    }

    #[test]
    fn named_packages_use_shared_metadata_and_keep_storage_paths_when_renamed() {
        let tmp = skills::ops::DownloadDir::new("named-package-display").unwrap();
        let mut ws = skills::Workspace::open(tmp.path()).unwrap();
        ws.config.agents.clear();
        let repo = Repository {
            alias: "stable-folder".into(),
            name: Some("Merlin Skills".into()),
            kind: skills::meta::SourceKind::Archive,
            url: "https://example.test/latest/skills.tar".into(),
            branch: String::new(),
        };
        repo.save(&ws).unwrap();
        let key = "repos/stable-folder/review";
        std::fs::create_dir_all(ws.root.join(key)).unwrap();
        std::fs::write(
            ws.root.join(key).join("SKILL.md"),
            "---\nname: review\ndescription: test\n---\nBody",
        )
        .unwrap();
        ws.meta
            .save(
                key,
                &skills::meta::SkillMeta {
                    source: Some(repo.source("review", None)),
                    ..Default::default()
                },
            )
            .unwrap();
        let snap = ws.scan().unwrap();
        let mut settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
        settings.ui.icons = skills::config::Icons::Text;
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &settings,
        };
        let mut view = ReposView::default();
        view.refresh(&ctx);
        assert_eq!(view.projects[0].name, "Merlin Skills");
        assert!(view.projects[0].source.starts_with("URL package"));
        let action = view.handle_key(
            KeyEvent::new(KeyCode::Char('r'), crossterm::event::KeyModifiers::NONE),
            &ctx,
        );
        assert!(matches!(action.as_slice(), [Action::OpenModal(_)]));
        let record = snap.get(key).unwrap();
        let badge =
            crate::tui::components::skill::repository_badge(record, settings.ui.icons).unwrap();
        assert_eq!(badge, "archive Merlin Skills");
        let preview = super::super::preview::preview_lines(record, &ctx, &[], 180);
        assert!(
            preview
                .iter()
                .any(|line| line.to_string().contains("source   archive Merlin Skills"))
        );
        assert!(preview.iter().any(|line| {
            line.to_string()
                .contains("https://example.test/latest/skills.tar")
        }));
        let mut completion = crate::tui::components::completion::Completion::default();
        let mut input = crate::tui::widgets::Input::with_value("repo:Merl");
        completion.update(&input, &ctx);
        completion.accept(&mut input);
        assert_eq!(input.value(), "repo:\"Merlin Skills\" ");
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(180, 20)).unwrap();
        terminal
            .draw(|frame| view.draw(frame, frame.area(), &ctx))
            .unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(screen.contains("Merlin Skills · 1 skills"));
        assert!(screen.contains("repos/stable-folder"));
        assert!(!screen.contains("repos/Merlin Skills"));
        view.focus_skills = true;
        Repository::rename(&ws, "stable-folder", "Training Tools").unwrap();
        // A view refresh consistently uses the supplied snapshot, even if metadata changed afterward.
        view.refresh(&ctx);
        assert_eq!(view.projects[0].name, "Merlin Skills");
        let snap = ws.scan().unwrap();
        let ctx = Ctx { snap: &snap, ..ctx };
        view.refresh(&ctx);
        assert_eq!(view.nav.selected(), Some(0));
        assert_eq!(view.projects[0].name, "Training Tools");
        assert_eq!(view.selected().unwrap().keys, [key]);
        assert_eq!(
            crate::tui::components::skill::repository_badge(
                snap.get(key).unwrap(),
                settings.ui.icons
            )
            .as_deref(),
            Some("archive Training Tools")
        );
    }

    #[test]
    fn source_remove_is_enabled_with_installed_skills_and_opens_confirmation() {
        let tmp = skills::ops::DownloadDir::new("repos-source-remove").unwrap();
        let ws = Workspace::open(tmp.path()).unwrap();
        let repository = Repository {
            alias: "demo".into(),
            name: Some("Demo skills".into()),
            kind: skills::meta::SourceKind::Git,
            url: "https://example.test/demo.git".into(),
            branch: "main".into(),
        };
        repository.save(&ws).unwrap();
        let key = "repos/demo/example";
        std::fs::create_dir_all(ws.root.join(key)).unwrap();
        std::fs::write(
            ws.root.join(key).join("SKILL.md"),
            "---\nname: example\ndescription: test\n---\nBody",
        )
        .unwrap();
        ws.meta
            .save(
                key,
                &skills::meta::SkillMeta {
                    source: Some(repository.source("example", None)),
                    ..Default::default()
                },
            )
            .unwrap();
        let snap = ws.scan().unwrap();
        let settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &settings,
        };
        let mut view = ReposView::default();
        view.refresh(&ctx);
        let request = view.actions_menu(&ctx).unwrap();
        let remove = request
            .items
            .iter()
            .find(|item| item.command == Command::Remove)
            .unwrap();
        assert_eq!(remove.label, "Remove local source and skills…");
        assert!(remove.disabled.is_none());
        assert!(matches!(
            view.context_execute(&request.target, Command::Remove, &ctx)
                .as_slice(),
            [Action::OpenModal(_)]
        ));
    }

    #[test]
    fn repositories_context_menu_delegates_to_the_shared_skill_panel() {
        let tmp = skills::ops::DownloadDir::new("repos-context-menu").unwrap();
        let root = tmp.path();
        Config {
            agents: vec![],
            ..Default::default()
        }
        .save(root)
        .unwrap();
        let ws = Workspace::open(root).unwrap();
        Repository {
            alias: "demo".into(),
            name: Some("Demo skills".into()),
            kind: skills::meta::SourceKind::Git,
            url: "https://example.test/demo.git".into(),
            branch: "main".into(),
        }
        .save(&ws)
        .unwrap();
        let key = "repos/demo/example";
        std::fs::create_dir_all(root.join(key)).unwrap();
        std::fs::write(
            root.join(key).join("SKILL.md"),
            "---\nname: example\ndescription: Example skill\n---\nBody",
        )
        .unwrap();
        let snap = ws.scan().unwrap();
        let settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &settings,
        };
        let mut view = ReposView::default();
        view.refresh(&ctx);

        let request = context_request(&mut view, &ctx, 140, 30);
        assert_eq!(request.target, Target::Skill(key.into()));
        assert!(view.focus_skills);
        assert!(
            request
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
            Some("Delete skill")
        );
        assert!(matches!(
            view.context_execute(&request.target, Command::Open, &ctx)
                .as_slice(),
            []
        ));
    }

    #[test]
    fn navigate_preview_refresh_and_render_small_terminals() {
        let tmp = skills::ops::DownloadDir::new("repos-view-test").unwrap();
        let root = tmp.path();
        let skill = root.join("repos/demo/example");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            "---\nname: example\ndescription: Sample skill\n---\n# Usage\nHello world",
        )
        .unwrap();
        skills::config::Config {
            agents: vec![],
            ..Default::default()
        }
        .save(root)
        .unwrap();
        let ws = skills::Workspace::open(root).unwrap();
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
        let mut view = ReposView::default();
        view.refresh(&ctx);
        assert_eq!(view.projects.len(), 1);
        assert_eq!(view.selected().unwrap().keys.len(), 1);
        assert!(
            view.skill_search.is_some(),
            "skills are available without Enter"
        );
        let enter = KeyEvent::new(KeyCode::Enter, crossterm::event::KeyModifiers::NONE);
        view.handle_key(enter, &ctx);
        assert!(view.focus_skills);
        view.handle_key(
            KeyEvent::new(KeyCode::Left, crossterm::event::KeyModifiers::NONE),
            &ctx,
        );
        assert!(!view.focus_skills);
        view.refresh(&ctx);
        assert_eq!(view.nav.selected(), Some(0));
        for (w, h) in [(120, 30), (60, 20), (12, 5), (1, 1)] {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
            terminal.draw(|f| view.draw(f, f.area(), &ctx)).unwrap();
            if w == 120 {
                let screen = terminal
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .map(|cell| cell.symbol())
                    .collect::<String>();
                assert!(screen.contains("Sample skill"));
                assert!(view.filter.rect.right() <= view.left.right());
                assert!(view.details.bottom() <= view.skills.y);
            }
        }
        std::fs::remove_dir_all(root.join("repos/demo")).unwrap();
        let snap = ws.scan().unwrap();
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
        assert!(view.selected().is_none());

        assert!(matches!(
            view.handle_key(
                KeyEvent::new(KeyCode::Esc, crossterm::event::KeyModifiers::NONE),
                &ctx
            )
            .as_slice(),
            [Action::BackToParent]
        ));
    }
    #[test]
    fn grouping_reuses_inventory_and_keeps_empty_repositories_and_local_skills() {
        let tmp = skills::ops::DownloadDir::new("repo-groups-test").unwrap();
        let ws = skills::Workspace::open(tmp.path()).unwrap();
        for alias in ["demo", "empty"] {
            Repository {
                name: None,
                kind: Default::default(),
                alias: alias.into(),
                url: "https://example.com/demo.git".into(),
                branch: "main".into(),
            }
            .save(&ws)
            .unwrap();
        }
        for key in ["repos/demo/example", "local-example"] {
            let dir = ws.root.join(key);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("SKILL.md"),
                "---\nname: example\ndescription: test\n---\nHello",
            )
            .unwrap();
        }
        let snap = skills::reconcile::scan(
            &ws.root,
            &skills::config::Config {
                agents: vec![],
                ..Default::default()
            },
        )
        .unwrap();
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
        let mut view = ReposView::default();
        view.refresh(&ctx);
        assert_eq!(view.projects.len(), 3);
        assert!(
            view.projects
                .iter()
                .any(|p| p.local && p.keys == ["local-example"])
        );
        assert!(
            view.projects
                .iter()
                .any(|p| p.alias.as_deref() == Some("empty") && p.keys.is_empty())
        );
        let mut keys: Vec<_> = view.projects.iter().flat_map(|p| p.keys.clone()).collect();
        keys.sort();
        assert_eq!(keys, ["local-example", "repos/demo/example"]);

        let key = |code| KeyEvent::new(code, crossterm::event::KeyModifiers::NONE);
        let panel = view.skill_search.as_mut().unwrap();
        panel.handle_key(key(KeyCode::Char('m')), &ctx);
        panel.handle_key(key(KeyCode::Char(' ')), &ctx);
        panel.set_query("example", &ctx);
        view.refresh(&ctx);
        assert_eq!(view.skill_search.as_ref().unwrap().query(), "example");
        assert_eq!(
            view.skill_search.as_ref().unwrap().panel_keys(&ctx),
            ["local-example"]
        );

        view.move_by(1, &ctx);
        assert_eq!(view.selected().unwrap().alias.as_deref(), Some("demo"));
        assert_eq!(view.skill_search.as_ref().unwrap().query(), "");
        assert_eq!(
            view.skill_search.as_ref().unwrap().panel_keys(&ctx),
            ["repos/demo/example"]
        );
        view.move_by(1, &ctx);
        assert!(
            view.skill_search
                .as_ref()
                .unwrap()
                .panel_keys(&ctx)
                .is_empty()
        );
        assert!(
            view.detail_lines(&ctx)
                .iter()
                .any(|line| line.to_string().contains("0 skills"))
        );

        view.filter.input = crate::tui::widgets::Input::with_value("absent-source");
        view.refilter(&ctx);
        assert!(view.selected().is_none());
        assert!(
            view.skill_search
                .as_ref()
                .unwrap()
                .panel_keys(&ctx)
                .is_empty()
        );
    }

    #[test]
    fn inventory_details_expose_paths_states_and_actions() {
        let tmp = skills::ops::DownloadDir::new("repo-inventory-lines").unwrap();
        let ws = skills::Workspace::open(tmp.path()).unwrap();
        let repository = Repository {
            name: None,
            kind: skills::meta::SourceKind::Git,
            alias: "demo".into(),
            url: "https://example.com/demo.git".into(),
            branch: "topic".into(),
        };
        repository.save(&ws).unwrap();
        let snap = ws.scan().unwrap();
        let settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &settings,
        };
        let mut view = ReposView::default();
        view.refresh(&ctx);
        view.refreshing("demo");
        let skill = skills::skill::SkillDoc {
            key: "new".into(),
            path: std::path::PathBuf::from("skills/new"),
            name: "new".into(),
            description: "New skill".into(),
            body: String::new(),
            external: false,
        };
        assert!(view.inventory_result(
            "demo",
            &Ok(RepositoryInventory {
                entries: vec![skills::repository::RepositoryInventoryEntry {
                    path: "skills/new".into(),
                    skill: Some(skill),
                    state: RepositoryInventoryState::Available,
                }],
            })
        ));
        let details = view
            .detail_lines(&ctx)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(details.contains("skills/new"));
        assert!(details.contains("available · i to install"));

        let actions =
            view.context_execute(&Target::Repository("demo".into()), Command::Source, &ctx);
        assert!(matches!(
            actions.as_slice(),
            [Action::Spawn(Task::DiscoverRepository {
                reference: skills::ops::install::InstallRef::Git {
                    branch: Some(branch),
                    ..
                },
                ..
            })] if branch == "topic"
        ));

        view.refresh(&ctx);
        assert!(!view.inventories.contains_key("demo"));
        assert!(!view.inventory_result("demo", &Ok(RepositoryInventory { entries: vec![] })));
    }
}
