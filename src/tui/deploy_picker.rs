//! Staged agent deployment with an explicit destination scope.
use super::components::choice_footer::{self, ChoiceEvent, ChoiceFocus};
use super::{
    app::{Action, Ctx, Hints},
    widgets::{Input, ListNav, OverlayClear, fit},
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Position, Rect},
    text::{Line, Span},
    widgets::{List, ListItem, Paragraph, Wrap},
};
use skills::{config::AgentConfig, ops::targets, reconcile::DeployState};
use std::path::PathBuf;

pub struct DeployPicker {
    keys: Vec<String>,
    home: PathBuf,
    local_keys: std::collections::BTreeSet<String>,
    project: Input,
    resolved_project: Option<PathBuf>,
    rows: Vec<(AgentConfig, usize, Option<bool>)>,
    list: ListNav,
    editing: bool,
    focus: ChoiceFocus,
    error: Option<String>,
    rect: Rect,
    project_rect: Rect,
    buttons: [Rect; 2],
}
impl DeployPicker {
    pub fn new(keys: Vec<String>, ctx: &Ctx) -> Self {
        Self::with_home(keys, ctx, skills::paths::expand_tilde("~"))
    }
    fn with_home(keys: Vec<String>, ctx: &Ctx, home: PathBuf) -> Self {
        let project = ctx
            .ws
            .project
            .clone()
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default();
        let mut picker = Self {
            keys,
            home,
            local_keys: Default::default(),
            project: Input::with_value(&project.to_string_lossy()),
            resolved_project: None,
            rows: vec![],
            list: ListNav::default(),
            editing: false,
            focus: ChoiceFocus::List,
            error: None,
            rect: Rect::default(),
            project_rect: Rect::default(),
            buttons: [Rect::default(); 2],
        };
        picker.reload(ctx);
        picker
    }
    fn reload(&mut self, ctx: &Ctx) {
        self.rows.clear();
        self.error = None;
        self.resolved_project = None;
        let result = (|| -> anyhow::Result<()> {
            {
                let path = skills::paths::expand_tilde(self.project.value().trim());
                let path = std::fs::canonicalize(path)?;
                anyhow::ensure!(path.is_dir(), "project must be an existing directory");
                self.resolved_project = Some(path);
            }
            let local =
                targets::all_candidates_in(ctx.ws, self.resolved_project.as_deref(), &self.home)?;
            self.local_keys = local.iter().map(|a| a.key.clone()).collect();
            let mut agents = targets::all_candidates_in(ctx.ws, None, &self.home)?;
            agents.extend(local);
            agents.sort_by_key(|a| {
                (
                    targets::product_key(a).to_string(),
                    self.local_keys.contains(&a.key),
                    a.skills_path(),
                )
            });
            agents.retain(|agent| {
                ctx.ws
                    .inventory_products
                    .as_ref()
                    .is_none_or(|products| products.contains(targets::product_key(agent)))
            });
            let snap = skills::reconcile::rescope(ctx.snap, &agents)?;
            agents.retain(|agent| {
                !matches!(
                    snap.agent(&agent.key).map(|report| &report.mode),
                    Some(skills::reconcile::AgentDirMode::ReadOnly { .. })
                )
            });
            self.rows = agents
                .into_iter()
                .map(|a| {
                    let count = self
                        .keys
                        .iter()
                        .filter(|key| {
                            snap.get(key).is_some_and(|s| {
                                s.deploy.get(&a.key) == Some(&DeployState::Deployed)
                            })
                        })
                        .count();
                    (a, count, None)
                })
                .collect();
            Ok(())
        })();
        if let Err(e) = result {
            self.error = Some(format!("{e:#}"));
        }
        self.list.first(self.rows.len());
        if self.rows.is_empty() {
            self.focus = ChoiceFocus::Apply;
        }
    }
    fn toggle(&mut self) {
        if let Some((agent, count, desired)) = self.list.selected().and_then(|i| self.rows.get(i)) {
            let path = agent.skills_path();
            let on = !desired.unwrap_or(*count == self.keys.len());
            for (agent, _, desired) in &mut self.rows {
                if agent.skills_path() == path {
                    *desired = Some(on);
                }
            }
        }
    }
    fn pending_count(&self) -> usize {
        self.rows
            .iter()
            .filter(|(_, count, desired)| {
                desired.is_some_and(|on| {
                    if on {
                        *count < self.keys.len()
                    } else {
                        *count > 0
                    }
                })
            })
            .map(|(a, _, _)| a.skills_path())
            .collect::<std::collections::BTreeSet<_>>()
            .len()
    }
    fn apply(&mut self) -> Vec<Action> {
        if self.editing {
            self.error = Some("Press Enter to confirm the project path first".into());
            return vec![];
        }
        if self.pending_count() == 0 {
            return vec![];
        }
        let changes: Vec<_> = self
            .rows
            .iter()
            .filter_map(|(a, count, desired)| {
                desired
                    .filter(|on| {
                        if *on {
                            *count < self.keys.len()
                        } else {
                            *count > 0
                        }
                    })
                    .map(|on| {
                        (
                            a.clone(),
                            on,
                            if self.local_keys.contains(&a.key) {
                                self.resolved_project.clone()
                            } else {
                                None
                            },
                        )
                    })
            })
            .collect();
        if changes.is_empty() {
            self.error = Some("Select agents before applying".into());
            return vec![];
        }
        let keys = self.keys.clone();
        vec![
            Action::CloseModal,
            Action::deployment(Action::BatchMeta(
                Box::new(move |ws| targets::apply_scoped(ws, &keys, &changes)),
                self.keys.clone(),
            )),
        ]
    }
    pub fn hints(&self) -> Hints {
        &[
            ("Tab/Shift+Tab", "list / buttons"),
            ("Enter/Space", "select / activate"),
            ("↑↓", "move"),
            ("p", "project path"),
            ("Esc", "cancel"),
        ]
    }
    pub fn paste(&mut self, text: &str) -> Vec<Action> {
        if self.editing
            && let Err(e) = self.project.paste(text)
        {
            self.error = Some(e.into());
        }
        vec![]
    }
    pub fn key(&mut self, k: KeyEvent, ctx: &Ctx) -> Vec<Action> {
        if self.editing && k.code == KeyCode::Esc {
            self.editing = false;
            return vec![];
        }
        if k.code == KeyCode::Esc
            || (!self.editing && k.code == KeyCode::Char('q') && k.modifiers.is_empty())
        {
            return vec![Action::CloseModal];
        }
        if self.editing {
            if matches!(k.code, KeyCode::Enter | KeyCode::Tab) {
                self.editing = false;
                self.reload(ctx);
            } else {
                self.project.handle_key(k);
            }
            return vec![];
        }
        let at_end = self.rows.is_empty() || self.list.selected() == Some(self.rows.len() - 1);
        if let Some(event) = self.focus.key(k.code, at_end) {
            return match event {
                ChoiceEvent::Apply => self.apply(),
                ChoiceEvent::Cancel => vec![Action::CloseModal],
                ChoiceEvent::Moved => vec![],
            };
        }
        match k.code {
            KeyCode::Char('p') => self.editing = true,
            KeyCode::Up => self.list.move_by(-1, self.rows.len()),
            KeyCode::Down => self.list.move_by(1, self.rows.len()),
            KeyCode::Enter if k.modifiers.contains(KeyModifiers::CONTROL) => return self.apply(),
            KeyCode::Char(' ') | KeyCode::Enter => self.toggle(),
            _ => {}
        }
        vec![]
    }
    pub fn mouse(&mut self, m: MouseEvent, ctx: &Ctx) -> Vec<Action> {
        let at = Position::new(m.column, m.row);
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if self.project_rect.contains(at) {
                    self.editing = true;
                    self.project.click(m.column);
                } else if self.buttons[0].contains(at) {
                    self.focus = ChoiceFocus::Apply;
                    return self.apply();
                } else if self.buttons[1].contains(at) {
                    self.focus = ChoiceFocus::Cancel;
                    return vec![Action::CloseModal];
                } else if self.list.rows.contains(at) {
                    if self.editing {
                        self.editing = false;
                        self.reload(ctx);
                    }
                    if let Some(i) = self.list.row_at(m.row, self.rows.len()) {
                        self.focus = ChoiceFocus::List;
                        self.list.select(Some(i));
                        self.toggle();
                    }
                }
            }
            MouseEventKind::ScrollDown => self.list.move_by(3, self.rows.len()),
            MouseEventKind::ScrollUp => self.list.move_by(-3, self.rows.len()),
            _ => {}
        }
        vec![]
    }
    pub fn draw(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        let w = area.width.saturating_sub(2).min(100);
        let h = area.height.saturating_sub(2).min(26);
        self.rect = Rect::new(
            area.x + (area.width - w) / 2,
            area.y + (area.height - h) / 2,
            w,
            h,
        );
        self.buttons = [Rect::default(); 2];
        self.list.rows = Rect::default();
        self.project_rect = Rect::default();
        f.render_widget(OverlayClear, self.rect);
        let block = ctx.settings.theme.block(
            format!(
                " Install to agents · {} {} ",
                self.keys.len(),
                if self.keys.len() == 1 {
                    "skill"
                } else {
                    "skills"
                }
            ),
            true,
        );
        let inner = block.inner(self.rect);
        f.render_widget(block, self.rect);
        if inner.height < 12 || inner.width < 26 {
            let footer_height = inner.height.min(2);
            self.list.rows = Rect::new(
                inner.x,
                inner.y,
                inner.width,
                inner.height.saturating_sub(footer_height),
            );
            let rows: Vec<_> = self
                .rows
                .iter()
                .map(|(a, count, desired)| {
                    ListItem::new(format!(
                        "{} {} {count}/{}",
                        if desired.unwrap_or(*count == self.keys.len()) {
                            "[✓]"
                        } else {
                            "[ ]"
                        },
                        a.display_name(),
                        self.keys.len()
                    ))
                })
                .collect();
            f.render_stateful_widget(
                List::new(rows).highlight_style(if self.focus == ChoiceFocus::List {
                    ctx.settings.theme.selected()
                } else {
                    ctx.settings.theme.dim()
                }),
                self.list.rows,
                &mut self.list.state,
            );
            self.buttons = choice_footer::draw(
                f,
                Rect::new(
                    inner.x,
                    inner.bottom() - footer_height,
                    inner.width,
                    footer_height,
                ),
                self.focus,
                self.pending_count() > 0,
                "",
                &ctx.settings.theme,
            );
            return;
        }
        f.render_widget(
            Paragraph::new("Select directories under each product · Global & Local")
                .style(ctx.settings.theme.bold()),
            Rect::new(inner.x, inner.y, inner.width, 1),
        );
        self.project_rect = Rect::new(inner.x, inner.y + 2, inner.width, 1);
        {
            f.render_widget(
                Paragraph::new("Project:"),
                Rect::new(inner.x, inner.y + 2, 8, 1),
            );
            self.project_rect.x += 9;
            self.project_rect.width = self.project_rect.width.saturating_sub(9);
            self.project.render(
                f,
                self.project_rect,
                self.editing,
                "Project directory",
                &ctx.settings.theme,
            );
        }
        f.render_widget(
            Paragraph::new(fit(
                &format!("Source: {} (symlink deployment)", ctx.ws.root.display()),
                inner.width as usize,
            ))
            .style(ctx.settings.theme.dim()),
            Rect::new(inner.x, inner.y + 3, inner.width, 1),
        );
        self.list.rows = Rect::new(
            inner.x,
            inner.y + 5,
            inner.width,
            inner.height.saturating_sub(11).max(1),
        );
        let mut previous_product = String::new();
        let rows: Vec<_> = self
            .rows
            .iter()
            .map(|(a, count, desired)| {
                let product = targets::product_key(a);
                let label = if product == previous_product {
                    ""
                } else {
                    targets::product_name(a)
                };
                previous_product = product.to_string();
                let mark = match desired {
                    Some(true) => "[✓]",
                    Some(false) => "[ ]",
                    None if *count == self.keys.len() => "[✓]",
                    None if *count > 0 => "[−]",
                    None => "[ ]",
                };
                let th = &ctx.settings.theme;
                let location = targets::location_label(
                    a,
                    self.resolved_project
                        .as_deref()
                        .unwrap_or(std::path::Path::new("")),
                );
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!("{mark} "),
                        if desired.is_some() {
                            th.accent()
                        } else if *count > 0 {
                            th.ok()
                        } else {
                            th.dim()
                        },
                    ),
                    Span::styled(format!("{label:<15} "), th.bold()),
                    Span::styled(
                        fit(
                            &format!("{location}  {count}/{}", self.keys.len()),
                            inner.width.saturating_sub(20) as usize,
                        ),
                        th.dim(),
                    ),
                ]))
            })
            .collect();
        f.render_stateful_widget(
            List::new(rows).highlight_style(if self.focus == ChoiceFocus::List && !self.editing {
                ctx.settings.theme.selected()
            } else {
                ctx.settings.theme.dim()
            }),
            self.list.rows,
            &mut self.list.state,
        );
        let destination = self
            .list
            .selected()
            .and_then(|i| self.rows.get(i))
            .map(|(a, _, _)| {
                format!(
                    "Target: {}\n{}",
                    targets::product_name(a),
                    super::app::middle_ellipsis(
                        &skills::paths::contract_tilde(&a.skills_path()),
                        inner.width as usize
                    )
                )
            })
            .unwrap_or_default();
        f.render_widget(
            Paragraph::new(destination).wrap(Wrap { trim: false }),
            Rect::new(inner.x, inner.bottom() - 6, inner.width, 2),
        );
        let info = self
            .error
            .clone()
            .unwrap_or_else(|| "Shared directories affect every agent reading them.".into());
        f.render_widget(
            Paragraph::new(if self.error.is_some() {
                format!(" {info}")
            } else {
                info
            })
            .wrap(Wrap { trim: false })
            .style(if self.error.is_some() {
                ctx.settings.theme.err()
            } else {
                ctx.settings.theme.dim()
            }),
            Rect::new(inner.x, inner.bottom() - 4, inner.width, 2),
        );
        let pending = self.pending_count();
        let summary = if pending == 0 {
            "No pending changes".into()
        } else {
            format!("{pending} pending destinations")
        };
        self.buttons = choice_footer::draw(
            f,
            Rect::new(inner.x, inner.bottom() - 2, inner.width, 2),
            self.focus,
            pending > 0,
            &summary,
            &ctx.settings.theme,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};
    #[test]
    fn picker_stages_shared_agent_choices_and_supports_mouse_cancel() {
        let project = std::env::temp_dir().join(format!("skills-picker-{}", std::process::id()));
        std::fs::create_dir_all(project.join(".agents/skills/sample")).unwrap();
        std::fs::write(
            project.join(".agents/skills/sample/SKILL.md"),
            "---\nname: sample\ndescription: sample\n---\n",
        )
        .unwrap();
        let ws = skills::Workspace::open_local(&project, false).unwrap();
        let snap = ws.scan().unwrap();
        let theme = super::super::theme::Theme::default();
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &{
                let mut settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
                settings.theme = theme;
                settings
            },
        };
        let mut picker =
            DeployPicker::with_home(vec!["sample".into()], &ctx, project.with_extension("home"));
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        assert_eq!(picker.pending_count(), 0);
        picker.key(key(KeyCode::Tab), &ctx);
        assert_eq!(picker.focus, ChoiceFocus::Apply);
        assert!(picker.key(key(KeyCode::Enter), &ctx).is_empty());
        picker.key(key(KeyCode::Tab), &ctx);
        assert_eq!(picker.focus, ChoiceFocus::Cancel);
        picker.key(key(KeyCode::Up), &ctx);
        let last = picker.rows.len() - 1;
        picker.list.select(Some(last));
        picker.key(key(KeyCode::Down), &ctx);
        assert_eq!(picker.focus, ChoiceFocus::Apply);
        picker.key(key(KeyCode::Up), &ctx);
        assert_eq!(picker.list.selected(), Some(last));
        picker.key(key(KeyCode::Enter), &ctx);
        assert_eq!(picker.focus, ChoiceFocus::List);
        assert!(picker.pending_count() > 0);
        picker.key(key(KeyCode::Enter), &ctx);
        assert_eq!(picker.pending_count(), 0);
        let cursor = picker
            .rows
            .iter()
            .position(|(a, _, _)| a.display_name().starts_with("Cursor"))
            .unwrap();
        picker.list.select(Some(cursor));
        picker.toggle();
        assert!(!project.join(".cursor").exists());
        assert_eq!(picker.rows[cursor].2, Some(true));
        assert!(
            picker
                .rows
                .iter()
                .all(|(a, _, _)| a.skills_path() != ws.root)
        );
        for (w, h) in [(100, 30), (80, 24), (50, 16), (40, 12)] {
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            term.draw(|f| picker.draw(f, f.area(), &ctx)).unwrap();
            let buf = term.backend().buffer();
            let text = (0..h)
                .map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect::<String>())
                .collect::<Vec<_>>()
                .join("\n");
            if h >= 16 {
                if w >= 80 {
                    assert!(text.contains("Global & Local"));
                }
                assert!(!text.contains("{pwd}"));
                if h >= 24 {
                    assert!(text.contains("Local"));
                }
                assert!(text.contains("Target:"));
                let path = picker.rows[cursor].0.skills_path();
                let suffix = path
                    .parent()
                    .unwrap()
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .to_string()
                    + "/"
                    + &path.file_name().unwrap().to_string_lossy();
                assert!(
                    text.contains(&suffix),
                    "target directory tail must remain visible"
                );
            } else {
                assert!(text.contains("Apply") && text.contains("Cancel"));
            }
        }
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| picker.draw(f, f.area(), &ctx)).unwrap();
        if let Ok(path) = std::env::var("SKILLS_TUI_CAPTURE") {
            for (agent, _, _) in &mut picker.rows {
                if let Ok(relative) = agent
                    .skills_path()
                    .strip_prefix(project.with_extension("home"))
                {
                    agent.skills_dir = format!("~/{}", relative.display());
                }
            }
            term.draw(|f| picker.draw(f, f.area(), &ctx)).unwrap();
            let buffer = term.backend().buffer();
            let cells: Vec<Vec<_>> = (0..30)
                .map(|y| {
                    (0..100)
                        .map(|x| {
                            let cell = &buffer[(x, y)];
                            (
                                cell.symbol().to_string(),
                                format!("{:?}", cell.fg),
                                format!("{:?}", cell.bg),
                            )
                        })
                        .collect()
                })
                .collect();
            std::fs::write(path, serde_json::to_vec(&cells).unwrap()).unwrap();
        }
        let cancel = picker.buttons[1];
        assert!(matches!(
            picker
                .mouse(
                    MouseEvent {
                        kind: MouseEventKind::Down(MouseButton::Left),
                        column: cancel.x,
                        row: cancel.y,
                        modifiers: KeyModifiers::NONE
                    },
                    &ctx
                )
                .as_slice(),
            [Action::CloseModal]
        ));
        assert!(!project.join(".cursor").exists());
        assert!(
            !ws.root
                .join(".skills-meta/deployment-targets.toml")
                .exists()
        );
        std::fs::remove_dir_all(project).unwrap();
    }
}
