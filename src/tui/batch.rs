//! Explicit, staged edits for a fixed selection of skills.
use super::app::{Action, Ctx, Hints};
use super::widgets::{Input, ListNav, OverlayClear, fit};
use anyhow::{Context, Result, bail};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    text::Line,
    widgets::{List, ListItem, Paragraph},
};
use skills::{
    history,
    ops::{deploy, edit},
};
use std::collections::BTreeSet;

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Tags,
    Deploy,
    Presets,
}
struct Row {
    id: String,
    label: String,
    count: usize,
    desired: Option<bool>,
}
impl Row {
    fn enabled(&self, total: usize) -> bool {
        self.desired.unwrap_or(self.count == total)
    }
    fn toggle(&mut self, total: usize) {
        self.desired = Some(!self.enabled(total));
    }
    fn marker(&self, total: usize) -> &'static str {
        match self.desired {
            Some(true) => "[✓]",
            Some(false) => "[ ]",
            None if self.count == total => "[✓]",
            None if self.count > 0 => "[−]",
            None => "[ ]",
        }
    }
}

pub struct Batch {
    busy: bool,
    kind: Kind,
    keys: Vec<String>,
    rows: Vec<Row>,
    input: Input,
    focus: usize,
    list: ListNav,
    shown: Vec<usize>,
    rect: Rect,
    input_rect: Rect,
    buttons: [Rect; 2],
    error: Option<String>,
}
impl Batch {
    fn new(kind: Kind, keys: Vec<String>, rows: Vec<Row>) -> Self {
        let shown = (0..rows.len()).collect();
        Self {
            busy: false,
            kind,
            keys,
            rows,
            input: Input::default(),
            focus: 0,
            list: ListNav::default(),
            shown,
            rect: Rect::default(),
            input_rect: Rect::default(),
            buttons: [Rect::default(); 2],
            error: None,
        }
    }
    pub fn tags(keys: Vec<String>, ctx: &Ctx) -> Self {
        let mut names: BTreeSet<String> = ctx
            .snap
            .skills
            .iter()
            .flat_map(|s| s.tags.clone())
            .collect();
        names.extend(ctx.ws.config.tags.iter().map(|t| t.name.clone()));
        let rows = names
            .into_iter()
            .map(|id| {
                let count = keys
                    .iter()
                    .filter(|k| ctx.snap.get(k).is_some_and(|s| s.tags.contains(&id)))
                    .count();
                Row {
                    label: id.clone(),
                    id,
                    count,
                    desired: None,
                }
            })
            .collect();
        Self::new(Kind::Tags, keys, rows)
    }
    pub fn deploy(keys: Vec<String>, ctx: &Ctx) -> Self {
        let rows = ctx
            .ws
            .config
            .agents
            .iter()
            .map(|a| Row {
                id: a.key.clone(),
                label: a.display_name().to_string(),
                count: keys
                    .iter()
                    .filter(|k| {
                        ctx.snap.get(k).is_some_and(|s| {
                            s.deploy.get(&a.key) == Some(&skills::reconcile::DeployState::Deployed)
                        })
                    })
                    .count(),
                desired: None,
            })
            .collect();
        Self::new(Kind::Deploy, keys, rows)
    }
    pub fn presets(keys: Vec<String>, ctx: &Ctx) -> Self {
        match ctx.ws.presets.list() {
            Ok(presets) => {
                let rows = presets
                    .into_iter()
                    .map(|p| Row {
                        count: keys.iter().filter(|k| p.skills.contains(k)).count(),
                        label: p.name.clone(),
                        id: p.name,
                        desired: None,
                    })
                    .collect();
                Self::new(Kind::Presets, keys, rows)
            }
            Err(e) => {
                let mut b = Self::new(Kind::Presets, keys, vec![]);
                b.error = Some(format!("{e:#}"));
                b
            }
        }
    }
    pub fn set_busy(&mut self, busy: bool) {
        self.busy = busy;
    }
    pub fn deploy_agent(keys: Vec<String>, agent: &str, ctx: &Ctx) -> Self {
        let mut batch = Self::deploy(keys, ctx);
        batch.rows.retain(|r| r.id == agent);
        batch.filter();
        batch
    }
    pub fn hints(&self) -> Hints {
        if self.busy {
            return &[("…", "applying changes")];
        }
        if self.focus == 0 {
            &[
                ("type", "filter"),
                ("↓", "list"),
                ("Enter", "choose/create"),
                ("Ctrl+Enter", "apply"),
                ("Esc", "cancel"),
            ]
        } else {
            &[
                ("↑↓", "move"),
                ("Space", "toggle"),
                ("←→", "controls"),
                ("Ctrl+Enter", "apply"),
                ("Esc", "cancel"),
            ]
        }
    }
    fn filter(&mut self) {
        let query = self.input.value().trim().to_lowercase();
        self.shown = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.label.to_lowercase().contains(&query))
            .map(|(i, _)| i)
            .collect();
        self.list.first(self.shown.len());
    }
    fn toggle(&mut self) {
        if let Some(i) = self
            .list
            .selected()
            .and_then(|i| self.shown.get(i))
            .copied()
        {
            if self.kind == Kind::Presets && self.rows[i].count == self.keys.len() {
                return;
            }
            self.rows[i].toggle(self.keys.len());
            self.error = None;
        }
    }
    pub fn paste(&mut self, text: &str) -> Vec<Action> {
        if self.focus != 0 {
            return vec![];
        }
        match self.input.paste(text) {
            Ok(true) => {
                self.filter();
                vec![]
            }
            Ok(false) => vec![],
            Err(error) => vec![Action::Error(error.into())],
        }
    }

    pub fn key(&mut self, k: KeyEvent, ctx: &Ctx) -> Vec<Action> {
        if k.code == KeyCode::Esc {
            return vec![Action::CloseModal];
        }
        if k.code == KeyCode::Enter && k.modifiers.contains(KeyModifiers::CONTROL) {
            return self.apply(ctx);
        }
        match k.code {
            KeyCode::Right if self.focus > 0 => self.focus = (self.focus + 1).min(3),
            KeyCode::Left if self.focus > 1 => self.focus -= 1,
            KeyCode::Down if self.focus == 0 => {
                self.focus = 1;
                self.list.clamp(self.shown.len());
            }
            KeyCode::Down if self.focus == 1 => self.list.move_by(1, self.shown.len()),
            KeyCode::Up if self.focus == 1 => {
                if self.list.selected() == Some(0) {
                    self.focus = 0;
                } else {
                    self.list.move_by(-1, self.shown.len());
                }
            }
            KeyCode::Char('/') if self.focus != 0 => self.focus = 0,
            KeyCode::Char(' ') if self.focus == 1 => self.toggle(),
            KeyCode::Enter if self.focus == 2 => return self.apply(ctx),
            KeyCode::Enter if self.focus == 3 => return vec![Action::CloseModal],
            KeyCode::Enter if self.focus == 1 => self.toggle(),
            KeyCode::Enter if self.focus == 0 => {
                let name = self.input.value().trim().to_string();
                if self.kind == Kind::Tags
                    && !name.is_empty()
                    && !self.rows.iter().any(|r| r.id == name)
                {
                    self.rows.push(Row {
                        label: name.clone(),
                        id: name,
                        count: 0,
                        desired: Some(true),
                    });
                    self.filter();
                }
                self.focus = 1;
                self.list.clamp(self.shown.len());
            }
            _ if self.focus == 0 && self.input.handle_key(k) => self.filter(),
            _ => {}
        }
        vec![]
    }
    fn plan(&self, ctx: &Ctx) -> Result<Vec<deploy::Action>> {
        let mut actions = Vec::new();
        for row in &self.rows {
            if let Some(on) = row.desired {
                actions.extend(if on {
                    deploy::plan_deploy(
                        ctx.ws,
                        ctx.snap,
                        &self.keys,
                        std::slice::from_ref(&row.id),
                    )?
                } else {
                    deploy::plan_undeploy(
                        ctx.ws,
                        ctx.snap,
                        &self.keys,
                        std::slice::from_ref(&row.id),
                    )?
                });
            }
        }
        actions.retain(|a| match a {
            deploy::Action::Skip { agent, reason, .. } => {
                !(reason == "already deployed"
                    || reason == "not deployed"
                    || reason.ends_with("; already deployed")
                    || (reason == "agent dir missing or foreign"
                        && ctx
                            .snap
                            .agent(agent)
                            .is_some_and(|a| a.mode == skills::reconcile::AgentDirMode::Missing)))
            }
            _ => true,
        });
        Ok(actions)
    }
    fn apply(&mut self, ctx: &Ctx) -> Vec<Action> {
        if self.keys.is_empty() {
            self.error = Some("No skills selected.".into());
            return vec![];
        }
        let changes: Vec<(String, bool)> = self
            .rows
            .iter()
            .filter_map(|r| r.desired.map(|on| (r.id.clone(), on)))
            .collect();
        if changes.is_empty() {
            self.error = Some("No changes selected.".into());
            return vec![];
        }
        let keys = self.keys.clone();
        match self.kind {
            Kind::Deploy => match self.plan(ctx) {
                Ok(actions) => vec![Action::BatchLinks {
                    title: format!("Deploy · {} skills", keys.len()),
                    actions,
                    keys,
                }],
                Err(e) => {
                    self.error = Some(format!("{e:#}"));
                    vec![]
                }
            },
            Kind::Tags => {
                let targets = keys.clone();
                vec![Action::BatchMeta(
                    Box::new(move |ws| {
                        // Validate and prepare every file before any write. Roll back a failed batch.
                        let mut prepared = Vec::new();
                        for key in &targets {
                            let original = ws.meta.load(key)?;
                            let mut meta = edit::load_or_init(ws, key)?;
                            for (tag, on) in &changes {
                                if *on {
                                    if !meta.tags.contains(tag) {
                                        meta.tags.push(tag.clone());
                                    }
                                } else {
                                    meta.tags.retain(|t| t != tag);
                                }
                            }
                            prepared.push((key.clone(), original, meta));
                        }
                        history::tag_edit(ws, |ws| {
                            for (i, (key, _, meta)) in prepared.iter().enumerate() {
                                if let Err(e) = ws.meta.save(key, meta) {
                                    for (key, original, _) in prepared[..i].iter().rev() {
                                        match original {
                                            Some(m) => ws.meta.save(key, m)?,
                                            None => ws.meta.remove(key)?,
                                        }
                                    }
                                    return Err(e);
                                }
                            }
                            Ok(format!("Updated tags for {} skills", targets.len()))
                        })
                    }),
                    keys,
                )]
            }
            Kind::Presets => {
                let targets = keys.clone();
                vec![Action::BatchMeta(
                    Box::new(move |ws| {
                        let originals = changes
                            .iter()
                            .filter(|(_, on)| *on)
                            .map(|(name, _)| {
                                ws.presets
                                    .load(name)?
                                    .with_context(|| format!("No such preset: {name}"))
                            })
                            .collect::<Result<Vec<_>>>()?;
                        if originals.is_empty() {
                            bail!("No presets selected.");
                        }
                        let mut intents = Vec::new();
                        for (i, preset) in originals.iter().enumerate() {
                            match history::preset_edit(ws, &preset.name, |members| {
                                for key in &targets {
                                    if !members.contains(key) {
                                        members.push(key.clone());
                                    }
                                }
                            }) {
                                Ok((_, Some(history::Intent::Meta(mut changes)))) => {
                                    intents.append(&mut changes)
                                }
                                Ok(_) => {}
                                Err(e) => {
                                    for p in originals[..i].iter().rev() {
                                        ws.presets.save(p)?;
                                    }
                                    return Err(e);
                                }
                            }
                        }
                        Ok((
                            format!(
                                "Added {} skills to {} presets",
                                targets.len(),
                                originals.len()
                            ),
                            (!intents.is_empty()).then_some(history::Intent::Meta(intents)),
                        ))
                    }),
                    keys,
                )]
            }
        }
    }
    pub fn mouse(&mut self, m: MouseEvent, ctx: &Ctx) -> Vec<Action> {
        let at = ratatui::layout::Position::new(m.column, m.row);
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if !self.rect.contains(at) || self.buttons[1].contains(at) {
                    return vec![Action::CloseModal];
                }
                if self.buttons[0].contains(at) {
                    return self.apply(ctx);
                }
                if self.input_rect.contains(at) {
                    self.focus = 0;
                    self.input.click(m.column);
                } else if self.list.rows.contains(at)
                    && let Some(i) = self.list.row_at(m.row, self.shown.len())
                {
                    self.list.select(Some(i));
                    self.focus = 1;
                    self.toggle();
                }
            }
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp
                if self.list.rows.contains(at) =>
            {
                self.focus = 1;
                self.list.move_by(
                    if m.kind == MouseEventKind::ScrollDown {
                        1
                    } else {
                        -1
                    },
                    self.shown.len(),
                );
            }
            _ => {}
        }
        vec![]
    }
    pub fn draw(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        let w = area.width.saturating_sub(2).min(76);
        let h = area.height.saturating_sub(2).min(20);
        self.rect = Rect::new(
            area.x + (area.width - w) / 2,
            area.y + (area.height - h) / 2,
            w,
            h,
        );
        f.render_widget(OverlayClear, self.rect);
        let title = match self.kind {
            Kind::Tags => "Tags",
            Kind::Deploy => "Deploy",
            Kind::Presets => "Add to presets",
        };
        let block = ctx
            .theme
            .block(format!(" {title} · {} skills ", self.keys.len()), true);
        let inner = block.inner(self.rect);
        f.render_widget(block, self.rect);
        if inner.height < 5 {
            f.render_widget(Paragraph::new("Enlarge terminal · Esc cancel"), inner);
            return;
        }
        self.input_rect = Rect::new(inner.x, inner.y, inner.width, 1);
        self.input.render(
            f,
            self.input_rect,
            self.focus == 0,
            if self.kind == Kind::Tags {
                "Filter or create a tag…"
            } else {
                "Filter…"
            },
            ctx.theme,
        );
        self.list.rows = Rect::new(inner.x, inner.y + 2, inner.width, inner.height - 5);
        self.list.clamp(self.shown.len());
        let items: Vec<ListItem> = self
            .shown
            .iter()
            .map(|i| {
                let r = &self.rows[*i];
                let detail = if r.desired.is_some() {
                    if self.kind == Kind::Presets {
                        "add"
                    } else if r.desired == Some(true) {
                        "all"
                    } else {
                        "none"
                    }
                    .to_string()
                } else {
                    format!("{}/{}", r.count, self.keys.len())
                };
                ListItem::new(format!(
                    "{} {}  {detail}",
                    r.marker(self.keys.len()),
                    fit(&r.label, inner.width.saturating_sub(14) as usize)
                ))
            })
            .collect();
        if items.is_empty() {
            f.render_widget(
                Paragraph::new(if self.kind == Kind::Tags && !self.input.is_empty() {
                    "Enter creates this tag"
                } else {
                    "No matching entries"
                })
                .style(ctx.theme.dim()),
                self.list.rows,
            );
        } else {
            f.render_stateful_widget(
                List::new(items).highlight_style(if self.focus == 1 {
                    ctx.theme.selected()
                } else {
                    ctx.theme.selected_unfocused()
                }),
                self.list.rows,
                &mut self.list.state,
            );
        }
        let summary = if self.busy {
            "Applying changes… Please wait.".to_string()
        } else {
            self.error.clone().unwrap_or_else(|| {
                if self.kind == Kind::Deploy {
                    match self.plan(ctx) {
                        Ok(plan) => format!(
                            "Apply {} link changes; {} unchanged/skipped",
                            plan.iter().filter(|a| a.is_change()).count(),
                            plan.iter().filter(|a| !a.is_change()).count()
                        ),
                        Err(e) => e.to_string(),
                    }
                } else if self.kind == Kind::Presets {
                    "Selected presets receive all selected skills.".into()
                } else {
                    "Untouched tags keep their existing membership.".into()
                }
            })
        };
        f.render_widget(
            Paragraph::new(fit(&summary, inner.width as usize)).style(if self.error.is_some() {
                ctx.theme.err()
            } else {
                ctx.theme.dim()
            }),
            Rect::new(inner.x, inner.bottom() - 2, inner.width, 1),
        );
        let bw = (inner.width / 2).min(18);
        self.buttons = [
            Rect::new(inner.x, inner.bottom() - 1, bw, 1),
            Rect::new(inner.x + bw, inner.bottom() - 1, bw, 1),
        ];
        for (i, label) in ["Apply changes", "Cancel"].iter().enumerate() {
            f.render_widget(
                Paragraph::new(Line::from(super::widgets::button(
                    label,
                    self.focus == i + 2,
                    ctx.theme,
                ))),
                self.buttons[i],
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use skills::Workspace;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    struct Fixture(std::path::PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "skills-batch-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            for name in ["alpha", "beta"] {
                std::fs::create_dir_all(root.join(name)).unwrap();
                std::fs::write(
                    root.join(name).join("SKILL.md"),
                    format!("---\nname: {name}\ndescription: test\n---\nBody\n"),
                )
                .unwrap();
            }
            Self(root)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn partial_tags_are_preserved_until_toggled_and_batch_has_one_undo() {
        let fixture = Fixture::new();
        let ws = Workspace::open(&fixture.0).unwrap();
        edit::tag_add(&ws, "alpha", &["existing".into()]).unwrap();
        let snap = ws.scan().unwrap();
        let theme = super::super::theme::Theme::default();
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            theme: &theme,
        };
        let mut batch = Batch::tags(vec!["alpha".into(), "beta".into()], &ctx);
        assert_eq!(batch.rows[0].marker(2), "[−]");
        batch.rows.push(Row {
            id: "new".into(),
            label: "new".into(),
            count: 0,
            desired: Some(true),
        });
        let Action::BatchMeta(write, keys) = batch.apply(&ctx).remove(0) else {
            panic!("expected metadata edit")
        };
        assert_eq!(keys.len(), 2);
        let (_, intent) = write(&ws).unwrap();
        assert!(
            ws.meta
                .load("alpha")
                .unwrap()
                .unwrap()
                .tags
                .contains(&"existing".into())
        );
        assert_eq!(ws.meta.load("beta").unwrap().unwrap().tags, vec!["new"]);
        let Some(history::Intent::Meta(changes)) = intent else {
            panic!("expected one metadata intent")
        };
        assert_eq!(changes.len(), 2);
        batch.rows[0].toggle(2);
        assert_eq!(batch.rows[0].marker(2), "[✓]");
        batch.rows[0].toggle(2);
        assert_eq!(batch.rows[0].marker(2), "[ ]");
    }
    #[test]
    fn corrupt_target_prevents_writes_to_other_selected_skills() {
        let fixture = Fixture::new();
        let ws = Workspace::open(&fixture.0).unwrap();
        edit::tag_add(&ws, "alpha", &["existing".into()]).unwrap();
        let text = std::fs::read_to_string(ws.meta.path("alpha")).unwrap();
        std::fs::write(
            ws.meta.path("beta"),
            format!("{text}\n[skills.beta]\ntags = 42\n"),
        )
        .unwrap();
        let before = std::fs::read(ws.meta.path("alpha")).unwrap();
        let snap = ws.scan().unwrap();
        let theme = super::super::theme::Theme::default();
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            theme: &theme,
        };
        let mut batch = Batch::tags(vec!["alpha".into(), "beta".into()], &ctx);
        batch.rows[0].desired = Some(false);
        let Action::BatchMeta(write, _) = batch.apply(&ctx).remove(0) else {
            panic!("expected metadata edit")
        };
        assert!(write(&ws).is_err());
        assert_eq!(std::fs::read(ws.meta.path("alpha")).unwrap(), before);
    }
    #[test]
    fn adding_to_several_presets_preserves_members_and_groups_history() {
        let fixture = Fixture::new();
        let ws = Workspace::open(&fixture.0).unwrap();
        for name in ["one", "two"] {
            ws.presets
                .save(&skills::preset::Preset {
                    name: name.into(),
                    skills: vec!["other".into()],
                    ..Default::default()
                })
                .unwrap();
        }
        let snap = ws.scan().unwrap();
        let theme = super::super::theme::Theme::default();
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            theme: &theme,
        };
        let mut batch = Batch::presets(vec!["alpha".into(), "beta".into()], &ctx);
        for row in &mut batch.rows {
            row.desired = Some(true);
        }
        let Action::BatchMeta(write, _) = batch.apply(&ctx).remove(0) else {
            panic!("expected metadata edit")
        };
        let (_, intent) = write(&ws).unwrap();
        for name in ["one", "two"] {
            assert_eq!(
                ws.presets.load(name).unwrap().unwrap().skills,
                vec!["other", "alpha", "beta"]
            );
        }
        let Some(history::Intent::Meta(changes)) = intent else {
            panic!("expected metadata intent")
        };
        assert_eq!(changes.len(), 2);
    }
    #[test]
    fn deployment_plans_omit_satisfied_targets_without_writing() {
        let fixture = Fixture::new();
        let agent = fixture.0.join("agent-links");
        skills::config::Config {
            agents: vec![skills::config::AgentConfig {
                key: "test".into(),
                name: "Test".into(),
                skills_dir: agent.to_string_lossy().into(),
            }],
            ..Default::default()
        }
        .save(&fixture.0)
        .unwrap();
        let ws = Workspace::open(&fixture.0).unwrap();
        let theme = super::super::theme::Theme::default();
        let snap = ws.scan().unwrap();
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            theme: &theme,
        };
        let mut batch = Batch::deploy(vec!["alpha".into()], &ctx);
        batch.rows[0].desired = Some(false);
        assert!(batch.plan(&ctx).unwrap().is_empty());
        assert!(!agent.exists());
        std::fs::create_dir(&agent).unwrap();
        std::os::unix::fs::symlink(ws.skill_path("alpha"), agent.join("alpha")).unwrap();
        let snap = ws.scan().unwrap();
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            theme: &theme,
        };
        batch.rows[0].desired = Some(true);
        assert!(batch.plan(&ctx).unwrap().is_empty());
        assert_eq!(
            std::fs::read_link(agent.join("alpha")).unwrap(),
            ws.skill_path("alpha")
        );
    }
}
