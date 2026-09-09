//! Staged agent deployment with an explicit destination scope.
use super::{
    app::{Action, Ctx, Hints},
    widgets::{ListNav, OverlayClear, fit},
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Position, Rect},
    text::Line,
    widgets::{List, ListItem, Paragraph, Wrap},
};
use skills::{config::AgentConfig, ops::targets, reconcile::DeployState};
use std::path::PathBuf;

pub struct DeployPicker {
    keys: Vec<String>,
    local: bool,
    project: PathBuf,
    resolved_project: Option<PathBuf>,
    rows: Vec<(AgentConfig, usize, Option<bool>)>,
    list: ListNav,
    error: Option<String>,
    rect: Rect,
    scopes: [Rect; 2],
    project_rect: Rect,
    buttons: [Rect; 2],
}
impl DeployPicker {
    pub fn new(keys: Vec<String>, ctx: &Ctx) -> Self {
        let project = ctx
            .ws
            .project
            .clone()
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default();
        let mut picker = Self {
            keys,
            local: ctx.ws.project.is_some(),
            project,
            resolved_project: None,
            rows: vec![],
            list: ListNav::default(),
            error: None,
            rect: Rect::default(),
            scopes: [Rect::default(); 2],
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
            if self.local {
                let path = &self.project;
                let path = std::fs::canonicalize(path)?;
                anyhow::ensure!(path.is_dir(), "project must be an existing directory");
                self.resolved_project = Some(path);
            }
            let agents = targets::all_candidates(ctx.ws, self.resolved_project.as_deref())?;
            let snap = skills::reconcile::rescope(ctx.snap, &agents)?;
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
    }
    fn scope(&mut self, local: bool, ctx: &Ctx) {
        if self.local != local {
            self.local = local;
            self.reload(ctx);
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
    fn apply(&mut self) -> Vec<Action> {
        let changes: Vec<_> = self
            .rows
            .iter()
            .filter_map(|(a, _, desired)| desired.map(|on| (a.clone(), on)))
            .collect();
        if changes.is_empty() {
            self.error = Some("Select agents before applying".into());
            return vec![];
        }
        let keys = self.keys.clone();
        let project = self.resolved_project.clone();
        vec![
            Action::CloseModal,
            Action::BatchMeta(
                Box::new(move |ws| targets::apply(ws, &keys, &changes, project.as_deref())),
                self.keys.clone(),
            ),
        ]
    }
    pub fn hints(&self) -> Hints {
        &[
            ("g/l", "global/local"),
            ("Space", "toggle target"),
            ("↑↓", "move"),
            ("Ctrl+Enter", "apply"),
            ("Esc", "cancel"),
        ]
    }
    pub fn paste(&mut self, _text: &str) -> Vec<Action> {
        vec![]
    }
    pub fn key(&mut self, k: KeyEvent, ctx: &Ctx) -> Vec<Action> {
        if k.code == KeyCode::Esc {
            return vec![Action::CloseModal];
        }
        match k.code {
            KeyCode::Char('g') => self.scope(false, ctx),
            KeyCode::Char('l') => self.scope(true, ctx),

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
                if self.scopes[0].contains(at) {
                    self.scope(false, ctx);
                } else if self.scopes[1].contains(at) {
                    self.scope(true, ctx);
                } else if self.buttons[0].contains(at) {
                    return self.apply();
                } else if self.buttons[1].contains(at) {
                    return vec![Action::CloseModal];
                } else if self.list.rows.contains(at)
                    && let Some(i) = self.list.row_at(m.row, self.rows.len())
                {
                    self.list.select(Some(i));
                    self.toggle();
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
        self.scopes = [Rect::default(); 2];
        self.buttons = [Rect::default(); 2];
        self.list.rows = Rect::default();
        self.project_rect = Rect::default();
        f.render_widget(OverlayClear, self.rect);
        let block = ctx.theme.block(
            format!(" Install to agents · {} skills ", self.keys.len()),
            true,
        );
        let inner = block.inner(self.rect);
        f.render_widget(block, self.rect);
        if inner.height < 12 || inner.width < 40 {
            f.render_widget(Paragraph::new("Enlarge terminal · Esc cancel"), inner);
            return;
        }
        self.scopes = [
            Rect::new(inner.x, inner.y, 18, 1),
            Rect::new(inner.x + 19, inner.y, 24, 1),
        ];
        for (i, label) in ["Global (home)", "Local (working dir)"].iter().enumerate() {
            f.render_widget(
                Paragraph::new(format!(
                    "[{}] {label}",
                    if (i == 1) == self.local { "✓" } else { " " }
                ))
                .style(if (i == 1) == self.local {
                    ctx.theme.selected()
                } else {
                    ctx.theme.dim()
                }),
                self.scopes[i],
            );
        }
        self.project_rect = Rect::new(inner.x, inner.y + 2, inner.width, 1);
        if self.local {
            f.render_widget(
                Paragraph::new("Project:"),
                Rect::new(inner.x, inner.y + 2, 8, 1),
            );
            self.project_rect.x += 9;
            self.project_rect.width = self.project_rect.width.saturating_sub(9);
            f.render_widget(
                Paragraph::new(fit(
                    &self.project.display().to_string(),
                    self.project_rect.width as usize,
                )),
                self.project_rect,
            );
        } else {
            f.render_widget(
                Paragraph::new("Home-level agent directories"),
                self.project_rect,
            );
        }
        f.render_widget(
            Paragraph::new(fit(
                &format!("Source: {} (symlink deployment)", ctx.ws.root.display()),
                inner.width as usize,
            ))
            .style(ctx.theme.dim()),
            Rect::new(inner.x, inner.y + 3, inner.width, 1),
        );
        self.list.rows = Rect::new(inner.x, inner.y + 5, inner.width, inner.height - 11);
        let rows: Vec<_> = self
            .rows
            .iter()
            .map(|(a, count, desired)| {
                let mark = match desired {
                    Some(true) => "[✓]",
                    Some(false) => "[ ]",
                    None if *count == self.keys.len() => "[✓]",
                    None if *count > 0 => "[−]",
                    None => "[ ]",
                };
                ListItem::new(Line::from(fit(
                    &format!("{mark} {}  {count}/{}", a.display_name(), self.keys.len()),
                    inner.width as usize,
                )))
            })
            .collect();
        f.render_stateful_widget(
            List::new(rows).highlight_style(ctx.theme.selected()),
            self.list.rows,
            &mut self.list.state,
        );
        let destination = self
            .list
            .selected()
            .and_then(|i| self.rows.get(i))
            .map(|(a, _, _)| format!("Target: {}", a.skills_path().display()))
            .unwrap_or_default();
        f.render_widget(
            Paragraph::new(destination).wrap(Wrap { trim: false }),
            Rect::new(inner.x, inner.bottom() - 6, inner.width, 2),
        );
        let info = self.error.clone().unwrap_or_else(|| {
            "Shared directories affect every agent reading them. Space selects; Apply writes."
                .into()
        });
        f.render_widget(
            Paragraph::new(info)
                .wrap(Wrap { trim: false })
                .style(if self.error.is_some() {
                    ctx.theme.err()
                } else {
                    ctx.theme.dim()
                }),
            Rect::new(inner.x, inner.bottom() - 4, inner.width, 2),
        );
        self.buttons = [
            Rect::new(inner.x, inner.bottom() - 1, 12, 1),
            Rect::new(inner.x + 14, inner.bottom() - 1, 12, 1),
        ];
        for (i, label) in ["[ Apply ]", "[ Cancel ]"].iter().enumerate() {
            f.render_widget(
                Paragraph::new(*label).style(ctx.theme.bold()),
                self.buttons[i],
            );
        }
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
            theme: &theme,
        };
        let mut picker = DeployPicker::new(vec!["sample".into()], &ctx);
        let cursor = picker
            .rows
            .iter()
            .position(|(a, _, _)| a.display_name().starts_with("Cursor"))
            .unwrap();
        picker.list.select(Some(cursor));
        picker.toggle();
        assert!(!project.join(".cursor").exists());
        assert_eq!(picker.rows[cursor].2, Some(true));
        let codex = picker
            .rows
            .iter()
            .position(|(a, _, _)| a.key == "codex")
            .unwrap();
        picker.list.select(Some(codex));
        picker.toggle();
        assert!(
            picker
                .rows
                .iter()
                .filter(|(a, _, _)| a.skills_path() == ws.root)
                .all(|(_, _, selected)| *selected == Some(false))
        );
        for (w, h) in [(100, 30), (80, 24), (40, 12)] {
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            term.draw(|f| picker.draw(f, f.area(), &ctx)).unwrap();
            let buf = term.backend().buffer();
            let text = (0..h)
                .map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect::<String>())
                .collect::<Vec<_>>()
                .join("\n");
            if h >= 24 {
                assert!(text.contains("Global (home)"));
                assert!(text.contains("Local (working dir)"));
                assert!(text.contains("Target:"));
            } else {
                assert!(text.contains("Enlarge terminal"));
            }
        }
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| picker.draw(f, f.area(), &ctx)).unwrap();
        if let Ok(path) = std::env::var("SKILLS_TUI_CAPTURE") {
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
