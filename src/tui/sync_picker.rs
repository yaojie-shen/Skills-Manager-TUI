//! Root sync destinations and explicit skill bindings.
use super::{
    app::{Action, Ctx, Hints},
    event::Task,
    modal::Modal,
    widgets::{Input, OverlayClear},
};
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    text::Line,
    widgets::{List, ListItem, ListState, Paragraph},
};
use skills::ops::sync::{Change, Settings};
use std::collections::BTreeSet;

#[derive(Debug, Clone)]
pub struct Request {
    pub remote: String,
    pub push: bool,
    pub keys: Vec<String>,
    pub dry_run: bool,
}
pub struct SyncPicker {
    settings: Settings,
    remote: usize,
    filter: Input,
    editing: bool,
    adding: Option<([Input; 3], usize)>,
    keys: Vec<String>,
    selected: BTreeSet<String>,
    cursor: usize,
    list_area: Rect,
    offset: usize,
    preview: Option<(Request, Vec<Change>)>,
}
impl SyncPicker {
    pub fn new(ctx: &Ctx) -> anyhow::Result<Self> {
        Ok(Self {
            settings: Settings::load(ctx.ws)?,
            remote: 0,
            filter: Input::default(),
            editing: false,
            adding: None,
            keys: ctx.snap.skills.iter().map(|s| s.key.clone()).collect(),
            selected: BTreeSet::new(),
            cursor: 0,
            list_area: Rect::default(),
            offset: 0,
            preview: None,
        })
    }
    pub fn preview(ctx: &Ctx, mut request: Request, changes: Vec<Change>) -> anyhow::Result<Self> {
        let mut picker = Self::new(ctx)?;
        request.keys = changes.iter().map(|c| c.skill.clone()).collect();
        picker.preview = Some((request, changes));
        Ok(picker)
    }
    fn remote(&self) -> Option<String> {
        self.settings.remotes.keys().nth(self.remote).cloned()
    }
    fn visible(&self) -> Vec<String> {
        self.keys
            .iter()
            .filter(|k| {
                k.to_lowercase()
                    .contains(&self.filter.value().to_lowercase())
            })
            .cloned()
            .collect()
    }
    fn targets(&self) -> Vec<String> {
        let visible = self.visible();
        if self.selected.is_empty() {
            visible.get(self.cursor).cloned().into_iter().collect()
        } else {
            visible
                .into_iter()
                .filter(|k| self.selected.contains(k))
                .collect()
        }
    }
    pub fn hints(&self) -> Hints {
        if self.preview.is_some() {
            return &[("y", "apply sync"), ("Esc", "cancel")];
        }
        if self.adding.is_some() {
            return &[
                ("Tab", "next field"),
                ("Enter", "register"),
                ("Esc", "cancel"),
            ];
        }
        &[
            ("←→", "remote"),
            ("Space", "select"),
            ("b", "bind"),
            ("x", "unbind"),
            ("p/P", "push/pull"),
            ("a", "add"),
            ("/", "filter"),
            ("Esc", "close"),
        ]
    }
    pub fn paste(&mut self, text: &str) -> Vec<Action> {
        let result = if let Some((fields, at)) = &mut self.adding {
            fields[*at].paste(text)
        } else if self.editing {
            self.filter.paste(text)
        } else {
            return vec![];
        };
        match result {
            Ok(_) => vec![],
            Err(e) => vec![Action::Error(e.into())],
        }
    }
    pub fn key(&mut self, key: KeyEvent, ctx: &Ctx) -> Vec<Action> {
        if let Some((request, changes)) = &self.preview {
            return match key.code {
                KeyCode::Esc => vec![Action::CloseModal],
                KeyCode::Char('y') if !changes.is_empty() => {
                    let mut request = request.clone();
                    request.dry_run = false;
                    vec![Action::CloseModal, Action::Spawn(Task::Sync(request))]
                }
                KeyCode::Down => {
                    self.cursor = self.cursor.saturating_add(1);
                    vec![]
                }
                KeyCode::Up => {
                    self.cursor = self.cursor.saturating_sub(1);
                    vec![]
                }
                _ => vec![],
            };
        }
        if let Some((fields, at)) = &mut self.adding {
            match key.code {
                KeyCode::Esc => self.adding = None,
                KeyCode::Tab => *at = (*at + 1) % 3,
                KeyCode::BackTab => *at = (*at + 2) % 3,
                KeyCode::Enter => {
                    let result = self.settings.add(
                        ctx.ws,
                        fields[0].value().trim(),
                        fields[1].value().trim(),
                        fields[2].value().trim(),
                    );
                    match result {
                        Ok(()) => {
                            self.adding = None;
                            return vec![Action::Toast(
                                "Sync destination registered; nothing uploaded".into(),
                            )];
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
        if self.editing {
            match key.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Down => self.editing = false,
                _ => {
                    self.filter.handle_key(key);
                    self.cursor = 0;
                }
            }
            return vec![];
        }
        let visible = self.visible();
        match key.code {
            KeyCode::Esc => return vec![Action::CloseModal],
            KeyCode::Char('/') => self.editing = true,
            KeyCode::Char('a') => {
                self.adding = Some((
                    [
                        Input::default(),
                        Input::default(),
                        Input::with_value("main"),
                    ],
                    0,
                ))
            }
            KeyCode::Left => self.remote = self.remote.saturating_sub(1),
            KeyCode::Right => {
                self.remote = (self.remote + 1).min(self.settings.remotes.len().saturating_sub(1))
            }
            KeyCode::Up => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Down => self.cursor = (self.cursor + 1).min(visible.len().saturating_sub(1)),
            KeyCode::Char(' ') => {
                if let Some(k) = visible.get(self.cursor)
                    && !self.selected.remove(k)
                {
                    self.selected.insert(k.clone());
                }
            }
            KeyCode::Char('g' | 'l') => {
                let git = key.code == KeyCode::Char('g');
                self.selected = ctx
                    .snap
                    .skills
                    .iter()
                    .filter(|s| {
                        visible.contains(&s.key)
                            && s.source
                                .as_ref()
                                .map(|source| source.kind())
                                .unwrap_or("local")
                                == if git { "git" } else { "local" }
                    })
                    .map(|s| s.key.clone())
                    .collect();
            }
            KeyCode::Char('b' | 'x') => {
                let targets = self.targets();
                if targets.is_empty() {
                    return vec![Action::Error("Select skills first".into())];
                }
                let remote = if key.code == KeyCode::Char('b') {
                    match self.remote() {
                        Some(r) => Some(r),
                        None => return vec![Action::Error("Add a sync destination first".into())],
                    }
                } else {
                    None
                };
                match self.settings.bind(ctx.ws, &targets, remote.as_deref()) {
                    Ok(()) => {
                        return vec![Action::Toast(format!(
                            "{} bindings changed. Previous remote copies/history remain; nothing uploaded.",
                            targets.len()
                        ))];
                    }
                    Err(e) => return vec![Action::Error(format!("{e:#}"))],
                }
            }
            KeyCode::Char('p' | 'P') => {
                let Some(remote) = self.remote() else {
                    return vec![Action::Error("Add a sync destination first".into())];
                };
                let keys = if self.selected.is_empty() {
                    vec![]
                } else {
                    self.targets()
                };
                if !self.selected.is_empty() && keys.is_empty() {
                    return vec![Action::Error(
                        "No selected skills match the current filter".into(),
                    )];
                }
                return vec![
                    Action::CloseModal,
                    Action::Spawn(Task::Sync(Request {
                        remote,
                        push: key.code == KeyCode::Char('p'),
                        keys,
                        dry_run: true,
                    })),
                ];
            }
            _ => {}
        }
        vec![]
    }
    pub fn mouse(&mut self, event: MouseEvent, ctx: &Ctx) -> Vec<Action> {
        if self.list_area.contains((event.column, event.row).into())
            && self.preview.is_none()
            && self.adding.is_none()
        {
            match event.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    self.cursor = self.offset + usize::from(event.row - self.list_area.y);
                    self.cursor = self.cursor.min(self.visible().len().saturating_sub(1));
                }
                MouseEventKind::ScrollDown => {
                    return self.key(
                        KeyEvent::new(KeyCode::Down, crossterm::event::KeyModifiers::NONE),
                        ctx,
                    );
                }
                MouseEventKind::ScrollUp => {
                    return self.key(
                        KeyEvent::new(KeyCode::Up, crossterm::event::KeyModifiers::NONE),
                        ctx,
                    );
                }
                _ => {}
            }
        }
        vec![]
    }
    pub fn draw(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        let rect = Rect::new(
            area.x + 1,
            area.y + 1,
            area.width.saturating_sub(2),
            area.height.saturating_sub(2),
        );
        f.render_widget(OverlayClear, rect);
        let block = ctx.settings.theme.block(" Sync destinations · F7 ", true);
        let inner = block.inner(rect);
        f.render_widget(block, rect);
        if let Some((fields, at)) = &mut self.adding {
            let mut lines = vec![Line::from(
                "Register a destination (does not upload anything)",
            )];
            for (i, label) in ["Name", "Git URL / path", "Branch"].iter().enumerate() {
                lines.push(Line::from(format!(
                    "{} {label}: {}",
                    if i == *at { ">" } else { " " },
                    fields[i].value()
                )));
            }
            lines.push(Line::from(
                "Tab: next field · Enter: register · Esc: cancel",
            ));
            f.render_widget(Paragraph::new(lines), inner);
            return;
        }
        if let Some((request, changes)) = &self.preview {
            let mut lines = vec![
                Line::from(format!(
                    "Preview: {} {}",
                    if request.push { "push to" } else { "pull from" },
                    request.remote
                )),
                Line::from("y: apply · Esc: cancel · ↑↓: scroll"),
            ];
            lines.extend(
                changes
                    .iter()
                    .map(|c| Line::from(format!("{}  {}", c.action, c.skill))),
            );
            if changes.is_empty() {
                lines.push(Line::from("No skills to sync."));
            }
            f.render_widget(
                Paragraph::new(lines).scroll((self.cursor.min(u16::MAX as usize) as u16, 0)),
                inner,
            );
            return;
        }
        let remote = self
            .remote()
            .unwrap_or_else(|| "(none — press a to add)".into());
        let url = self
            .settings
            .remotes
            .get(&remote)
            .map(|r| format!("{} [{}]", r.url, r.branch))
            .unwrap_or_default();
        let header = vec![
            Line::from(format!("←→ Destination: {remote}  {url}")),
            Line::from("Unbound stays local · g/l: select Git/local · b: bind/switch · x: unbind"),
            Line::from("p: preview push · P: preview pull · no selection = all for destination"),
            Line::from(format!(
                "{} Filter: {}",
                if self.editing { ">" } else { "/" },
                self.filter.value()
            )),
        ];
        f.render_widget(
            Paragraph::new(header),
            Rect::new(inner.x, inner.y, inner.width, 4.min(inner.height)),
        );
        self.list_area = Rect::new(
            inner.x,
            inner.y + 4.min(inner.height),
            inner.width,
            inner.height.saturating_sub(4),
        );
        let visible = self.visible();
        let items: Vec<_> = visible
            .iter()
            .map(|k| {
                ListItem::new(format!(
                    "{} {k}  → {}",
                    if self.selected.contains(k) {
                        "[x]"
                    } else {
                        "[ ]"
                    },
                    self.settings
                        .bindings
                        .get(k)
                        .map(|b| b.remote.as_str())
                        .unwrap_or("local only")
                ))
            })
            .collect();
        let mut state = ListState::default()
            .with_selected(Some(self.cursor))
            .with_offset(self.offset);
        f.render_stateful_widget(
            List::new(items)
                .highlight_style(
                    ratatui::style::Style::default().bg(ctx.settings.theme.selection_bg),
                )
                .highlight_symbol("› "),
            self.list_area,
            &mut state,
        );
        self.offset = state.offset();
    }
}
pub fn open(ctx: &Ctx) -> Modal {
    match SyncPicker::new(ctx) {
        Ok(p) => Modal::Sync(Box::new(p)),
        Err(e) => Modal::message("Sync error", vec![format!("{e:#}")]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;
    #[test]
    fn filtered_bindings_and_transfer_preview_require_explicit_execution() {
        let tmp = skills::ops::DownloadDir::new("sync-picker-test").unwrap();
        let mut ws = skills::Workspace::open(tmp.path()).unwrap();
        ws.config.agents.clear();
        for key in ["open", "secret"] {
            std::fs::create_dir_all(ws.skill_path(key)).unwrap();
            std::fs::write(
                ws.skill_path(key).join("SKILL.md"),
                "---\nname: test\ndescription: test\n---\ncontent",
            )
            .unwrap();
        }
        Settings::load(&ws)
            .unwrap()
            .add(&ws, "public", "https://example.com/backup.git", "main")
            .unwrap();
        let snap = ws.scan().unwrap();
        let settings = super::super::settings::RuntimeSettings::new(&ws.config);
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &settings,
        };
        let mut picker = SyncPicker::new(&ctx).unwrap();
        let key = |c| KeyEvent::new(c, KeyModifiers::NONE);
        picker.key(key(KeyCode::Char('l')), &ctx);
        picker.filter.set("open");
        picker.key(key(KeyCode::Char('b')), &ctx);
        let settings = Settings::load(&ws).unwrap();
        assert!(settings.bindings.contains_key("open"));
        assert!(!settings.bindings.contains_key("secret"));
        let actions = picker.key(key(KeyCode::Char('p')), &ctx);
        assert!(actions.iter().any(
            |a| matches!(a, Action::Spawn(Task::Sync(r)) if r.dry_run && r.keys == vec!["open"])
        ));
        picker.filter.set("no matches");
        assert!(
            picker
                .key(key(KeyCode::Char('p')), &ctx)
                .iter()
                .all(|a| !matches!(a, Action::Spawn(_)))
        );
        picker.filter.set("open");
        let request = Request {
            remote: "public".into(),
            push: true,
            keys: vec![],
            dry_run: true,
        };
        let mut preview = SyncPicker::preview(
            &ctx,
            request,
            vec![Change {
                skill: "open".into(),
                action: "push".into(),
            }],
        )
        .unwrap();
        assert!(preview.key(key(KeyCode::Enter), &ctx).is_empty());
        assert!(preview.key(key(KeyCode::Char('y')), &ctx).iter().any(
            |a| matches!(a, Action::Spawn(Task::Sync(r)) if !r.dry_run && r.keys == vec!["open"])
        ));
        for (width, height) in [(100, 26), (40, 12), (12, 5)] {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            terminal.draw(|f| picker.draw(f, f.area(), &ctx)).unwrap();
            terminal.draw(|f| preview.draw(f, f.area(), &ctx)).unwrap();
        }
    }
}
