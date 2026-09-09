//! Repository grouping of the existing inventory, including flat local skills.
use super::{View, split_panes, wheel};
use crate::tui::app::{Action, Ctx, Hints, Tab};
use crate::tui::widgets::ListNav;
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    text::Line,
    widgets::{List, ListItem, Paragraph, Wrap},
};
use skills::repository::{Repository, alias_of};

#[derive(Clone)]
struct Project {
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
    skill_search: Option<super::search::SearchView>,
    members: Vec<String>,
    project: Option<usize>,
    nav: ListNav,
    saved_project: usize,
    error: Option<String>,
    scroll: u16,
    reading: bool,
    right: Rect,
}

impl ReposView {
    pub fn batch_finished(&mut self, failed: &[String]) {
        if let Some(view) = self.skill_search.as_mut() {
            view.batch_finished(failed);
        }
    }

    pub fn input_focused(&self) -> bool {
        self.filter.editing
            || self
                .skill_search
                .as_ref()
                .is_some_and(|v| v.input_focused())
    }
    pub fn paste(&mut self, text: &str, ctx: &Ctx) -> Vec<Action> {
        if let Some(view) = self.skill_search.as_mut() {
            return view.paste(text, ctx);
        }
        let actions = self.filter.paste(text);
        self.refilter();
        actions
    }
    fn refilter(&mut self) {
        self.projects = self
            .all_projects
            .iter()
            .filter(|p| self.filter.matches(&format!("{} {}", p.name, p.source)))
            .cloned()
            .collect();
        self.nav.clamp(self.projects.len());
    }

    fn len(&self) -> usize {
        if self.project.is_some() {
            self.members.len()
        } else {
            self.projects.len()
        }
    }

    fn open(&mut self) {
        if self.project.is_some() {
            self.reading = true;
            return;
        }
        let Some(i) = self.nav.selected() else { return };
        self.saved_project = i;
        self.members = self.projects[i].keys.clone();
        self.project = Some(i);
        self.nav.first(self.members.len());
        self.scroll = 0;
    }

    fn back(&mut self) -> Vec<Action> {
        if self.reading {
            self.reading = false;
        } else if self.project.take().is_some() {
            self.members.clear();
            self.nav.select(Some(self.saved_project));
            self.nav.clamp(self.projects.len());
        } else {
            return vec![Action::SwitchTab(Tab::Search)];
        }
        vec![]
    }

    fn move_by(&mut self, delta: i32) {
        if self.reading {
            self.scroll = (i32::from(self.scroll) + delta).clamp(0, i32::from(u16::MAX)) as u16;
        } else {
            self.nav.move_by(delta, self.len());
            self.scroll = 0;
        }
    }
}

impl View for ReposView {
    fn refresh(&mut self, ctx: &Ctx) {
        let panel = self.skill_search.take();
        let selected_project = self
            .project
            .and_then(|i| self.projects.get(i))
            .map(|p| (p.local, p.name.clone()));
        let selected_skill = self
            .nav
            .selected()
            .and_then(|i| self.members.get(i))
            .cloned();
        let mut groups = std::collections::BTreeMap::<Option<String>, Project>::new();
        self.error = None;
        match Repository::list(&ctx.ws.root) {
            Ok(repos) => {
                for repo in repos {
                    groups.insert(
                        Some(repo.alias.clone()),
                        Project {
                            name: repo.alias,
                            local: false,
                            source: format!("{} · {}", repo.url, repo.branch),
                            keys: vec![],
                        },
                    );
                }
            }
            Err(error) => self.error = Some(format!("{error:#}")),
        }
        for record in &ctx.snap.skills {
            if let Some(alias) = alias_of(&record.key) {
                groups
                    .entry(Some(alias.into()))
                    .or_insert_with(|| Project {
                        name: alias.into(),
                        local: false,
                        source: "Unregistered repository".into(),
                        keys: vec![],
                    })
                    .keys
                    .push(record.key.clone());
            } else {
                groups
                    .entry(None)
                    .or_insert_with(|| Project {
                        name: "Local skills".into(),
                        local: true,
                        source: "Skills outside repository directories".into(),
                        keys: vec![],
                    })
                    .keys
                    .push(record.key.clone());
            }
        }
        self.all_projects = groups.into_values().collect();
        self.refilter();
        self.skill_search = None;
        self.project = None;
        self.members.clear();
        self.reading = false;
        self.scroll = 0;
        self.nav.clamp(self.projects.len());
        if let Some(i) = self
            .projects
            .iter()
            .position(|p| Some(&(p.local, p.name.clone())) == selected_project.as_ref())
        {
            self.nav.select(Some(i));
            self.open();
            if let Some(j) = self
                .members
                .iter()
                .position(|key| Some(key) == selected_skill.as_ref())
            {
                self.nav.select(Some(j));
            }
        }
        if self.project.is_some()
            && let Some(mut view) = panel
        {
            view.update_panel(self.members.clone(), ctx);
            self.skill_search = Some(view);
        }
    }

    fn handle_key(&mut self, k: KeyEvent, ctx: &Ctx) -> Vec<Action> {
        if let Some(view) = self.skill_search.as_mut() {
            if k.code == KeyCode::Left && view.panel_back() {
                self.skill_search = None;
                return vec![];
            }
            return view.handle_key(k, ctx);
        }
        if self.project.is_none() && self.filter.key(k) {
            self.refilter();
            return vec![];
        }
        if self.project.is_some() && matches!(k.code, KeyCode::Char('/' | 'm')) {
            let mut view = super::search::SearchView::panel(
                self.members.clone(),
                "Repository skills".into(),
                ctx,
            );
            if k.code == KeyCode::Char('m') {
                view.focus_list();
                view.handle_key(k, ctx);
            }
            self.skill_search = Some(view);
            return vec![];
        }
        match k.code {
            KeyCode::Down | KeyCode::Char('j') => self.move_by(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_by(-1),
            KeyCode::PageDown => self.move_by(10),
            KeyCode::PageUp => self.move_by(-10),
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => self.open(),
            KeyCode::Esc | KeyCode::Backspace | KeyCode::Left | KeyCode::Char('h') => {
                return self.back();
            }
            KeyCode::Char('q') => return vec![Action::SwitchTab(Tab::Search)],
            _ => {}
        }
        vec![]
    }

    fn handle_mouse(&mut self, m: MouseEvent, ctx: &Ctx) -> Vec<Action> {
        if let Some(view) = self.skill_search.as_mut() {
            return view.handle_mouse(m, ctx);
        }
        let point = (m.column, m.row).into();
        if m.kind == MouseEventKind::Down(MouseButton::Left) && self.filter.rect.contains(point) {
            self.filter.editing = true;
            return vec![];
        }
        if let Some(delta) = wheel(&m) {
            if self.nav.rows.contains(point) {
                self.reading = false;
                self.move_by(delta);
            } else if self.right.contains(point) && self.project.is_some() {
                self.reading = true;
                self.move_by(delta);
            }
        } else if m.kind == MouseEventKind::Down(MouseButton::Left) {
            if self.nav.rows.contains(point) {
                self.reading = false;
                if let Some((_, double)) = self.nav.click(m.row, self.len()) {
                    self.scroll = 0;
                    if double {
                        self.open();
                    }
                }
            } else if self.right.contains(point) && self.project.is_some() {
                self.reading = true;
            }
        }
        vec![]
    }

    fn draw(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        if let Some(view) = self.skill_search.as_mut() {
            view.draw(f, area, ctx);
            return;
        }
        let area = if self.project.is_none() {
            self.filter.draw(f, area, "Filter repositories", ctx)
        } else {
            area
        };
        let (left, right) = split_panes(area, 35);
        self.right = right;
        let path = self
            .project
            .map(|i| {
                if self.projects[i].local {
                    ctx.ws.root.clone()
                } else {
                    ctx.ws.root.join("repos").join(&self.projects[i].name)
                }
            })
            .unwrap_or_else(|| ctx.ws.root.join("repos"));
        let labels: Vec<String> = if self.project.is_some() {
            self.members
                .iter()
                .map(|key| {
                    ctx.snap
                        .get(key)
                        .map(|r| {
                            format!("{}  [{}]", super::cards::display_name(r), r.status.label())
                        })
                        .unwrap_or_else(|| key.clone())
                })
                .collect()
        } else {
            self.projects
                .iter()
                .map(|p| format!("{}/  ({} skills)", p.name, p.keys.len()))
                .collect()
        };
        let title = if self.project.is_some() {
            " skills "
        } else {
            " repositories "
        };
        let block = ctx
            .theme
            .block(format!("{title}({}) ", labels.len()), !self.reading);
        self.nav.rows = block.inner(left);
        let items: Vec<_> = labels.into_iter().map(ListItem::new).collect();
        f.render_stateful_widget(
            List::new(items)
                .block(block)
                .highlight_style(ctx.theme.selected()),
            left,
            &mut self.nav.state,
        );
        let block = ctx.theme.block(" preview · read only ", self.reading);
        let inner = block.inner(right);
        f.render_widget(block, right);
        let mut lines = vec![
            Line::styled(path.display().to_string(), ctx.theme.dim()),
            Line::raw(""),
        ];
        if let Some(error) = &self.error {
            lines.push(Line::styled(error.clone(), ctx.theme.err()));
        }
        if let Some(i) = self.project {
            lines.push(Line::raw(self.projects[i].source.clone()));
            if let Some(record) = self
                .nav
                .selected()
                .and_then(|j| self.members.get(j))
                .and_then(|key| ctx.snap.get(key))
            {
                lines.extend(super::preview::preview_lines(
                    record,
                    ctx,
                    &[],
                    inner.width as usize,
                ));
            } else {
                lines.push(Line::raw("No installed skills in this repository."));
            }
        } else {
            lines.push(Line::raw(
                "Select a repository and press Enter to browse its installed skills.",
            ));
            if let Some(project) = self.nav.selected().and_then(|i| self.projects.get(i)) {
                lines.push(Line::raw(project.source.clone()));
            }
            if self.projects.is_empty() {
                lines.push(Line::raw(
                    "No skills or repositories found. Install from Library first.",
                ));
            }
        }
        let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
        self.scroll = self.scroll.min(
            paragraph
                .line_count(inner.width)
                .saturating_sub(inner.height as usize)
                .min(u16::MAX as usize) as u16,
        );
        f.render_widget(paragraph.scroll((self.scroll, 0)), inner);
    }

    fn hints(&self) -> Hints {
        if self.filter.editing {
            return &[("Enter/↓", "repositories"), ("Esc", "finish filter")];
        }
        if let Some(view) = self.skill_search.as_ref() {
            return view.hints();
        }
        &[
            ("↑↓", "navigate / scroll"),
            ("Enter/→", "open"),
            ("Esc/←", "back"),
            ("Ctrl+r", "refresh"),
            ("/", "filter current list"),
            ("q", "library"),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn navigate_preview_refresh_and_render_small_terminals() {
        let tmp = skills::ops::DownloadDir::new("repos-view-test").unwrap();
        let root = tmp.path();
        let skill = root.join("repos/demo/example");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            "---\nname: Example\ndescription: Sample skill\n---\n# Usage\nHello world",
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
            theme: &theme,
        };
        let mut view = ReposView::default();
        view.refresh(&ctx);
        assert_eq!(view.projects.len(), 1);
        view.open();
        assert_eq!(view.members.len(), 1);

        view.open();
        assert!(view.reading);
        assert!(view.back().is_empty());
        assert!(!view.reading);
        view.refresh(&ctx);
        assert_eq!(view.project, Some(0));
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
                assert!(screen.contains("Hello world"));
            }
        }
        std::fs::remove_dir_all(root.join("repos/demo")).unwrap();
        let snap = ws.scan().unwrap();
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            theme: &theme,
        };
        view.refresh(&ctx);
        assert!(view.project.is_none());

        assert!(matches!(
            view.back().as_slice(),
            [Action::SwitchTab(Tab::Search)]
        ));
    }
    #[test]
    fn grouping_reuses_inventory_and_keeps_empty_repositories_and_local_skills() {
        let tmp = skills::ops::DownloadDir::new("repo-groups-test").unwrap();
        let ws = skills::Workspace::open(tmp.path()).unwrap();
        for alias in ["demo", "empty"] {
            Repository {
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
            theme: &theme,
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
                .any(|p| p.name == "empty" && p.keys.is_empty())
        );
        let mut keys: Vec<_> = view.projects.iter().flat_map(|p| p.keys.clone()).collect();
        keys.sort();
        assert_eq!(keys, ["local-example", "repos/demo/example"]);
    }
}
