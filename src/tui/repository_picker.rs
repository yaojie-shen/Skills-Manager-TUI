//! Always-expanded repository tree with ancestry-aware multi-selection.
use super::views::{completion::Completion, preview::Overlay};
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

pub struct RepositoryPicker {
    pub selection: InstallSelection,
    alias: Input,
    search: Input,
    name: Input,
    focus: u8, // alias, search, list, selected local name
    list: ListNav,
    shown: Vec<String>,
    rect: Rect,
    fields: [Rect; 3],
    candidates: Snapshot,
    searcher: Searcher,
    matching: BTreeSet<String>,
    configured: bool,
    completion: Completion,
    preview: Overlay,
}
impl RepositoryPicker {
    pub fn new(fetched: FetchedRepository, ctx: &Ctx) -> Self {
        let installed = |path: &str| {
            ctx.snap.skills.iter().any(|s| match &s.source {
                Some(skills::meta::Source::Git {
                    url,
                    branch,
                    subpath,
                    ..
                }) => {
                    *url == fetched.repository.url
                        && branch.as_deref() == Some(&fetched.repository.branch)
                        && subpath.as_deref().unwrap_or("") == path
                }
                _ => false,
            })
        };
        let paths = fetched
            .choices
            .iter()
            .filter(|p| {
                !installed(p)
                    && !fetched.invalid.contains_key(*p)
                    && !fetched
                        .choices
                        .iter()
                        .any(|a| !fetched.invalid.contains_key(a) && overlaps(a, p))
            })
            .cloned()
            .collect();
        Self::restore(InstallSelection {
            fetched,
            paths,
            names: BTreeMap::new(),
        })
    }
    pub fn restore(selection: InstallSelection) -> Self {
        let alias = Input::with_value(&selection.fetched.repository.alias);
        let candidates = candidate_snapshot(&selection.fetched);
        let mut this = Self {
            candidates,
            searcher: Searcher::new(),
            matching: BTreeSet::new(),
            configured: false,
            completion: Completion::default(),
            preview: Overlay::default(),
            selection,
            alias,
            search: Input::default(),
            name: Input::default(),
            focus: 1,
            list: ListNav::default(),
            shown: vec![],
            rect: Rect::default(),
            fields: [Rect::default(); 3],
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
        if !self.configured {
            self.searcher = Searcher::for_workspace(ctx.ws);
            self.configured = true;
            self.refilter();
        }
    }
    fn update_completion(&mut self, ctx: &Ctx) {
        let candidate_ctx = Ctx {
            ws: ctx.ws,
            snap: &self.candidates,
            theme: ctx.theme,
        };
        self.completion.update_install(&self.search, &candidate_ctx);
    }
    fn matches_filter(&self, path: &str) -> bool {
        self.matching.contains(path)
    }
    fn refilter(&mut self) {
        let mut query = Query::parse(self.search.value());
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
        if ctx.snap.skills.iter().any(|s| match &s.source {
            Some(skills::meta::Source::Git { url, subpath, .. }) => {
                *url == self.selection.fetched.repository.url
                    && subpath.as_deref().unwrap_or("") == path
            }
            _ => false,
        }) {
            return Some("already installed".into());
        }
        None
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
    pub fn hints(&self) -> Hints {
        if let Some(hints) = self.preview.hints() {
            return hints;
        }
        if self.focus == 1 && self.completion.active() {
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
                ("Enter", "install"),
                ("a", "repo alias"),
                ("e", "local name"),
                ("/", "filter"),
                ("Esc", "cancel"),
            ],
            _ => &[
                ("type", "edit"),
                ("Enter/↓", "list"),
                ("↓", "results"),
                ("Esc", "cancel"),
            ],
        }
    }
    pub fn paste(&mut self, text: &str, ctx: &Ctx) -> Vec<Action> {
        if self.preview.is_open() {
            return vec![];
        }
        let input = match self.focus {
            0 => &mut self.alias,
            1 => &mut self.search,
            3 => &mut self.name,
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
        self.configure(ctx);
        if self.preview.handle_key(k) {
            return vec![];
        }
        if self.focus == 1 && self.completion.active() {
            match k.code {
                KeyCode::Up => {
                    self.completion.move_by(-1);
                    return vec![];
                }
                KeyCode::Down => {
                    self.completion.move_by(1);
                    return vec![];
                }
                KeyCode::Enter => {
                    self.completion.accept(&mut self.search);
                    self.refilter();
                    self.update_completion(ctx);
                    return vec![];
                }
                KeyCode::Esc => {
                    self.completion.close();
                    return vec![];
                }
                _ => {}
            }
        }
        if k.code == KeyCode::Esc {
            self.selection.fetched.cleanup();
            return vec![Action::CloseModal];
        }
        if self.focus != 2 {
            if matches!(k.code, KeyCode::Enter | KeyCode::Down) {
                if self.focus == 0 {
                    let value = self.alias.value().trim();
                    if !skills::util::valid_skill_key(value) {
                        return vec![Action::Error("invalid repository alias".into())];
                    }
                    self.selection.fetched.repository.alias = value.into();
                } else if self.focus == 3
                    && let Some(path) = self.selected()
                {
                    let value = self.name.value().trim();
                    if !skills::util::valid_skill_key(value) {
                        return vec![Action::Error("invalid local name".into())];
                    }
                    self.selection.names.insert(path, value.into());
                }
                self.focus = 2;
                self.completion.close();
                return vec![];
            }
            match self.focus {
                0 => {
                    self.alias.handle_key(k);
                }
                1 => {
                    if self.search.handle_key(k) {
                        self.refilter();
                        self.update_completion(ctx);
                    }
                }
                3 => {
                    self.name.handle_key(k);
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
            KeyCode::Char('e') => {
                if let Some(path) = self.selected()
                    && self.selection.fetched.choices.contains(&path)
                {
                    self.name = Input::with_value(&self.resolved_name(&path, ctx));
                    self.focus = 3;
                }
            }
            KeyCode::Char(' ') => {
                if let Some(path) = self.selected() {
                    return self.toggle(&path, ctx);
                }
            }
            KeyCode::Enter => {
                let alias = self.alias.value().trim();
                if !skills::util::valid_skill_key(alias) {
                    return vec![Action::Error("invalid repository alias".into())];
                }
                self.selection.fetched.repository.alias = alias.into();
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
        if self.preview.handle_mouse(m) {
            return vec![];
        }
        if self.focus == 1 {
            let (consumed, accepted) = self.completion.mouse(m, &mut self.search);
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
            MouseEventKind::ScrollDown if self.list.rows.contains(at) => {
                self.list.move_by(3, self.shown.len())
            }
            MouseEventKind::ScrollUp if self.list.rows.contains(at) => {
                self.list.move_by(-3, self.shown.len())
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if !self.rect.contains(at) {
                    return self.key(KeyEvent::new(KeyCode::Esc, m.modifiers), ctx);
                }
                if self.focus == 3 && !self.fields[2].contains(at) {
                    let actions = self.key(KeyEvent::new(KeyCode::Enter, m.modifiers), ctx);
                    if !actions.is_empty() {
                        return actions;
                    }
                }
                if self.fields[2].contains(at) {
                    self.focus = 2;
                    return self.key(KeyEvent::new(KeyCode::Char('e'), m.modifiers), ctx);
                }
                if self.fields[0].contains(at) {
                    self.focus = 0;
                    self.alias.click(m.column);
                } else if self.fields[1].contains(at) {
                    self.focus = 1;
                    self.search.click(m.column);
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
        let th = ctx.theme;
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
        if inner.height < 6 {
            return;
        }
        for (i, label) in [(0, "repo alias"), (1, "filter"), (2, "local name")] {
            let y = if i == 2 {
                inner.bottom() - 1
            } else {
                inner.y + i as u16
            };
            f.render_widget(
                Paragraph::new(Span::styled(label, th.dim())),
                Rect::new(inner.x, y, 12.min(inner.width), 1),
            );
            self.fields[i] = Rect::new(
                inner.x + 12.min(inner.width),
                y,
                inner.width.saturating_sub(12),
                1,
            );
        }
        self.alias
            .render(f, self.fields[0], self.focus == 0, "", th);
        self.search.render(
            f,
            self.fields[1],
            self.focus == 1,
            "Filter skills/ · repo:owner/repo · status:invalid · keywords",
            th,
        );
        if self.focus == 3 {
            self.name.render(f, self.fields[2], true, "", th);
        } else if let Some(path) = self.selected() {
            let name = self.resolved_name(&path, ctx);
            f.render_widget(
                Paragraph::new(fit(&name, self.fields[2].width as usize)),
                self.fields[2],
            );
        }
        self.list.rows = Rect::new(
            inner.x,
            inner.y + 3,
            inner.width,
            inner.height.saturating_sub(4),
        );
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
                    ". (repository root)"
                } else {
                    path.rsplit('/').next().unwrap_or(path)
                };
                let label = self
                    .candidates
                    .get(path)
                    .and_then(|r| r.name.as_deref())
                    .unwrap_or(label);
                let suffix = disabled.map(|s| format!("  ({s})")).unwrap_or_default();
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
            self.completion.draw(f, self.list.rows, ctx);
        }
        let candidate_ctx = Ctx {
            ws: ctx.ws,
            snap: &self.candidates,
            theme: ctx.theme,
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
                name_mismatch: false,
                tags: vec![],
                note: None,
                source: Some(skills::meta::Source::Git {
                    url: fetched.repository.url.clone(),
                    branch: Some(fetched.repository.branch.clone()),
                    subpath: Some(key.clone()),
                    revision: Some(fetched.revision.clone()),
                }),
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
            theme: &theme,
        };
        let mut picker = RepositoryPicker::new(
            FetchedRepository {
                repository: Repository {
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
        picker.search = Input::with_value("repo:sampl");
        picker.update_completion(&ctx);
        assert!(picker.completion.active());
        picker.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &ctx);
        assert_eq!(picker.search.value(), "repo:sample/tools ");
        for token in ["tag:", "agent:", "status:modified"] {
            picker.search = Input::with_value(token);
            picker.update_completion(&ctx);
            assert!(
                !picker.completion.active(),
                "installation must not suggest unavailable {token}"
            );
        }
        for query in [
            "observability",
            "skills/ prnter",
            "repo:sample/tools skills/",
            "status:repository skills/",
        ] {
            picker.search = Input::with_value(query);
            picker.refilter();
            assert!(picker.matches_filter("skills/print"), "{query}");
            assert!(!picker.matches_filter("internal/print"), "{query}");
        }
        picker.search = Input::with_value("repo:other/tools");
        picker.refilter();
        assert!(picker.shown.is_empty());
        picker.search = Input::with_value("skills/");
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
            picker.search = Input::with_value(query);
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
        picker.search = Input::with_value("internal/tests/skills/");
        picker.refilter();
        assert!(
            picker
                .shown
                .contains(&"internal/tests/skills/mock-demo".into())
        );
        assert!(!picker.shown.contains(&"skills".into()));
        picker.search = Input::with_value("mock-demo");
        picker.refilter();
        assert!(picker.shown.contains(&"internal".into()));
        assert!(
            picker
                .shown
                .contains(&"internal/tests/skills/mock-demo".into())
        );
        picker.search = Input::with_value("missing/");
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
            theme: &theme,
        };
        let fetched = FetchedRepository {
            repository: Repository {
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
        assert_eq!(picker.selection.paths, vec!["tools", "other/printer"]);
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
        picker.search = Input::with_value("tools/");
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
        assert!(picker.selection.paths.contains(&"other/printer".into()));
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
