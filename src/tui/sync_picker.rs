//! Configure and run root-wide Git synchronization.
use super::{
    app::{Action, Ctx, Hints},
    event::Task,
    modal::Modal,
    widgets::{Input, OverlayClear},
};
use crossterm::event::{KeyCode, KeyEvent, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    widgets::{Paragraph, Wrap},
};
use skills::ops::sync::{self, Mode, Report, Settings};

#[derive(Debug, Clone)]
pub struct Request {
    pub mode: Mode,
    pub dry_run: bool,
}
pub struct SyncPicker {
    settings: Settings,
    scroll: u16,
    adding: Option<([Input; 2], usize)>,
    preview: Option<(Request, Report)>,
}
impl SyncPicker {
    pub fn new(ctx: &Ctx) -> anyhow::Result<Self> {
        Ok(Self {
            settings: Settings::load(ctx.ws)?,
            scroll: 0,
            adding: None,
            preview: None,
        })
    }
    pub fn preview(ctx: &Ctx, request: Request, report: Report) -> anyhow::Result<Self> {
        Ok(Self {
            preview: Some((request, report)),
            ..Self::new(ctx)?
        })
    }
    pub fn hints(&self) -> Hints {
        if self.adding.is_some() {
            &[
                ("Tab", "next field"),
                ("Enter", "enable"),
                ("Esc", "cancel"),
            ]
        } else if self.preview.is_some() {
            &[("y", "sync root"), ("↑↓", "scroll"), ("Esc", "cancel")]
        } else {
            &[
                ("s", "sync"),
                ("p/P", "push/pull"),
                ("a", "configure"),
                ("d", "disable auto"),
                ("Esc", "close"),
            ]
        }
    }
    pub fn paste(&mut self, text: &str) -> Vec<Action> {
        if let Some((fields, at)) = &mut self.adding
            && let Err(e) = fields[*at].paste(text)
        {
            return vec![Action::Error(e.into())];
        }
        vec![]
    }
    pub fn key(&mut self, key: KeyEvent, ctx: &Ctx) -> Vec<Action> {
        if self.adding.is_none() {
            match key.code {
                KeyCode::Down => self.scroll = self.scroll.saturating_add(1),
                KeyCode::Up => self.scroll = self.scroll.saturating_sub(1),
                KeyCode::PageDown => self.scroll = self.scroll.saturating_add(10),
                KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(10),
                _ => {}
            }
        }
        if let Some((request, _)) = &self.preview {
            return match key.code {
                KeyCode::Esc => vec![Action::CloseModal],
                KeyCode::Char('y') => vec![
                    Action::CloseModal,
                    Action::Spawn(Task::Sync(Request {
                        dry_run: false,
                        ..request.clone()
                    })),
                ],
                _ => vec![],
            };
        }
        if let Some((fields, at)) = &mut self.adding {
            match key.code {
                KeyCode::Esc => self.adding = None,
                KeyCode::Tab | KeyCode::BackTab => *at = 1 - *at,
                KeyCode::Enter => {
                    match sync::configure(
                        ctx.ws,
                        fields[0].value().trim(),
                        fields[1].value().trim(),
                    ) {
                        Ok(()) => {
                            return vec![
                                Action::CloseModal,
                                Action::Rescan,
                                Action::Toast("Root auto-sync enabled".into()),
                            ];
                        }
                        Err(e) => return vec![Action::Error(format!("{e:#}"))],
                    }
                }
                _ => {
                    fields[*at].handle_key(key);
                }
            }
            return vec![];
        }
        match key.code {
            KeyCode::Esc => vec![Action::CloseModal],
            KeyCode::Char('a') => {
                self.scroll = 0;
                self.adding = Some((
                    [
                        Input::with_value(self.settings.url.as_deref().unwrap_or("")),
                        Input::with_value(self.settings.branch.as_deref().unwrap_or("main")),
                    ],
                    0,
                ));
                vec![]
            }
            KeyCode::Char('d') => match sync::disable(ctx.ws) {
                Ok(()) => {
                    self.settings.enabled = false;
                    vec![Action::Toast(
                        "Automatic root sync disabled; Git history retained".into(),
                    )]
                }
                Err(e) => vec![Action::Error(format!("{e:#}"))],
            },
            KeyCode::Char('s' | 'p' | 'P') => vec![
                Action::CloseModal,
                Action::Spawn(Task::Sync(Request {
                    mode: match key.code {
                        KeyCode::Char('p') => Mode::Push,
                        KeyCode::Char('P') => Mode::Pull,
                        _ => Mode::Sync,
                    },
                    dry_run: true,
                })),
            ],
            _ => vec![],
        }
    }
    pub fn mouse(&mut self, event: MouseEvent, _: &Ctx) -> Vec<Action> {
        if self.adding.is_none() {
            match event.kind {
                MouseEventKind::ScrollDown => self.scroll = self.scroll.saturating_add(3),
                MouseEventKind::ScrollUp => self.scroll = self.scroll.saturating_sub(3),
                _ => {}
            }
        }
        vec![]
    }
    pub fn draw(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        let area = Rect::new(
            area.x + 1,
            area.y + 1,
            area.width.saturating_sub(2),
            area.height.saturating_sub(2),
        );
        f.render_widget(OverlayClear, area);
        let block = ctx.settings.theme.block(" Root Git sync · Ctrl-B ", true);
        let inner = block.inner(area);
        f.render_widget(block, area);
        let text = if let Some((fields, at)) = &self.adding {
            format!(
                "Configure whole-root automatic backup\n\n{} URL: {}\n{} Branch: {}\n\nEnter enables automatic commit, pull and push. Skills, notes, presets and configuration (including machine paths) are shared. Deletions propagate. Runtime files stay local. Use an empty remote or a clone of an existing root repository.",
                if *at == 0 { ">" } else { " " },
                fields[0].value(),
                if *at == 1 { ">" } else { " " },
                fields[1].value()
            )
        } else if let Some((request, report)) = &self.preview {
            format!(
                "Root: {}\nMode: {:?}\n\nLocal Git status:\n{}\n\ny: save local changes and execute; remote state is checked again. Conflicts stop sync and preserve the local commit.",
                ctx.ws.root.display(),
                request.mode,
                if report.status.is_empty() {
                    "Clean"
                } else {
                    &report.status
                }
            )
        } else {
            format!(
                "Root: {}\nRemote: {}\nBranch: {}\nAutomatic sync: {}\n\nThe root is the Git working tree. After Library changes, save local work, merge remote updates and push. Startup and Agent-only deployment changes do not sync.\n\nSkills, metadata, tags, notes and presets are included. Runtime locks, staging and backups are excluded.\n\ns: preview sync · p: push · P: pull\na: configure/enable · d: disable automatic sync\n\nConflicts require Git resolution; no force push or reset is performed.",
                ctx.ws.root.display(),
                self.settings.url.as_deref().unwrap_or("Not configured"),
                self.settings.branch.as_deref().unwrap_or("—"),
                self.settings.enabled
            )
        };
        let rows = text
            .lines()
            .map(|line| {
                super::widgets::width(line)
                    .div_ceil(usize::from(inner.width.max(1)))
                    .max(1)
            })
            .sum::<usize>();
        self.scroll = self.scroll.min(
            rows.saturating_sub(usize::from(inner.height))
                .min(u16::MAX as usize) as u16,
        );
        f.render_widget(
            Paragraph::new(text)
                .wrap(Wrap { trim: false })
                .scroll((self.scroll, 0)),
            inner,
        );
    }
}
pub fn open(ctx: &Ctx) -> Modal {
    match SyncPicker::new(ctx) {
        Ok(p) => Modal::Sync(Box::new(p)),
        Err(e) => Modal::message("Root sync", vec![format!("{e:#}")]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn root_sync_preview_requires_confirmation_and_renders_without_skill_bindings() {
        let temp = skills::ops::DownloadDir::new("root-sync-dialog").unwrap();
        let ws = skills::Workspace::open(temp.path()).unwrap();
        let snap = ws.scan().unwrap();
        let settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &settings,
        };
        let mut picker = SyncPicker::new(&ctx).unwrap();
        assert!(matches!(
            picker.key(KeyCode::Char('s').into(), &ctx).as_slice(),
            [
                Action::CloseModal,
                Action::Spawn(Task::Sync(Request { dry_run: true, .. }))
            ]
        ));
        let mut picker = SyncPicker::preview(
            &ctx,
            Request {
                mode: Mode::Sync,
                dry_run: true,
            },
            Report::default(),
        )
        .unwrap();
        assert!(picker.key(KeyCode::Enter.into(), &ctx).is_empty());
        assert!(matches!(
            picker.key(KeyCode::Char('y').into(), &ctx).as_slice(),
            [
                Action::CloseModal,
                Action::Spawn(Task::Sync(Request { dry_run: false, .. }))
            ]
        ));
        for (width, height) in [(80, 24), (48, 12)] {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            terminal.draw(|f| picker.draw(f, f.area(), &ctx)).unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect();
            assert!(text.contains("Root Git sync"));
        }
        assert!(!ws.root.join(".git").exists());
    }
}
