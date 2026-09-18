//! Explicit, staged edits for a fixed selection of skills.
use super::app::{Action, Ctx, Hints};
use super::components::choice_footer::{self, ChoiceEvent, ChoiceFocus};
use super::widgets::{Input, ListNav, OverlayClear, fit};
use anyhow::{Context, Result, bail};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{List, ListItem, Paragraph},
};
use skills::{history, ops::deploy};
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
    tag_chips: Vec<(Rect, String)>,
    selected_tags: Vec<String>,
    remove_armed: bool,
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
            tag_chips: vec![],
            selected_tags: vec![],
            remove_armed: false,
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
        let mut picker = Self::new(Kind::Tags, keys, rows);
        picker.filter();
        picker
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
        let rows = ctx
            .snap
            .presets
            .by_name
            .values()
            .map(|preset| Row {
                count: keys
                    .iter()
                    .filter(|key| preset.skills.contains(key))
                    .count(),
                label: preset.name.clone(),
                id: preset.name.clone(),
                desired: None,
            })
            .collect();
        Self::new(Kind::Presets, keys, rows)
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
        if self.kind == Kind::Tags {
            return if self
                .list
                .selected()
                .and_then(|i| self.shown.get(i))
                .and_then(|i| self.rows.get(*i))
                .is_some_and(|r| r.count == self.keys.len())
            {
                &[
                    ("Enter", "remove"),
                    ("↑↓", "choose"),
                    ("Tab", "complete"),
                    ("Backspace", "select last · again removes"),
                    ("Esc", "done"),
                ]
            } else {
                &[
                    ("Enter", "add / create"),
                    ("↑↓", "choose"),
                    ("Tab", "complete"),
                    ("Backspace", "select last · again removes"),
                    ("Esc", "done"),
                ]
            };
        }
        if self.focus == 0 {
            &[
                ("type", "filter"),
                ("↓", "list"),
                ("Enter", "results"),
                ("Tab/Shift+Tab", "list / buttons"),
                ("Esc", "cancel"),
            ]
        } else {
            &[
                ("↑↓", "move"),
                ("Space", "toggle"),
                ("←→", "controls"),
                ("Tab/Shift+Tab", "list / buttons"),
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
            .filter(|(_, r)| {
                r.label.to_lowercase().contains(&query)
                    && (self.kind != Kind::Tags || r.count != self.keys.len() || !query.is_empty())
            })
            .map(|(i, _)| i)
            .collect();
        if self.kind == Kind::Tags
            && !query.is_empty()
            && !self.rows.iter().any(|r| r.id.to_lowercase() == query)
        {
            self.shown.push(self.rows.len()); // Inline create option.
        }
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
        self.remove_armed = false;
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
        if self.kind != Kind::Tags && self.focus == 0 && k.code == KeyCode::Esc {
            if self.input.value().is_empty() {
                self.focus = 1;
            } else {
                self.input = Input::default();
                self.filter();
            }
            return vec![];
        }
        if k.code == KeyCode::Esc
            || (self.kind != Kind::Tags
                && self.focus != 0
                && k.code == KeyCode::Char('q')
                && k.modifiers.is_empty())
        {
            return vec![Action::CloseModal];
        }
        if self.kind == Kind::Tags {
            if k.code != KeyCode::Backspace {
                self.remove_armed = false;
            }
            match k.code {
                KeyCode::Down => self.list.move_by(1, self.shown.len()),
                KeyCode::Up => self.list.move_by(-1, self.shown.len()),
                KeyCode::Enter => return self.edit_selected_tag(),
                KeyCode::Backspace if self.input.value().is_empty() => {
                    if let Some(name) = self.selected_tags.last() {
                        if self.remove_armed {
                            self.remove_armed = false;
                            return self.edit_tag(name.clone(), false);
                        }
                        self.remove_armed = true;
                    }
                }
                KeyCode::Tab => {
                    if let Some(row) = self
                        .list
                        .selected()
                        .and_then(|i| self.shown.get(i))
                        .and_then(|i| self.rows.get(*i))
                    {
                        self.input = Input::with_value(&row.id);
                        self.filter();
                    }
                }
                _ if self.input.handle_key(k) => self.filter(),
                _ => {}
            }
            return vec![];
        }
        if self.focus == 0 && matches!(k.code, KeyCode::Tab | KeyCode::BackTab) {
            self.focus = if k.code == KeyCode::Tab { 2 } else { 3 };
            return vec![];
        }
        if self.focus > 0 {
            let mut focus = match self.focus {
                2 => ChoiceFocus::Apply,
                3 => ChoiceFocus::Cancel,
                _ => ChoiceFocus::List,
            };
            let at_end =
                self.shown.is_empty() || self.list.selected() == Some(self.shown.len() - 1);
            if let Some(event) = focus.key(k.code, at_end) {
                self.focus = match focus {
                    ChoiceFocus::List => 1,
                    ChoiceFocus::Apply => 2,
                    ChoiceFocus::Cancel => 3,
                };
                return match event {
                    ChoiceEvent::Apply => {
                        if self.pending_count() > 0 {
                            self.apply(ctx)
                        } else {
                            vec![]
                        }
                    }
                    ChoiceEvent::Cancel => vec![Action::CloseModal],
                    ChoiceEvent::Moved => vec![],
                };
            }
        }
        if k.code == KeyCode::Enter && k.modifiers.contains(KeyModifiers::CONTROL) {
            return self.apply(ctx);
        }
        match k.code {
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
                    || reason.starts_with("agent directory is read-only:")
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
    fn edit_selected_tag(&mut self) -> Vec<Action> {
        let Some(i) = self
            .list
            .selected()
            .and_then(|i| self.shown.get(i))
            .copied()
        else {
            return vec![];
        };
        let name = self
            .rows
            .get(i)
            .map(|r| r.id.clone())
            .unwrap_or_else(|| self.input.value().trim().to_string());
        let add = self.rows.get(i).is_none_or(|r| r.count != self.keys.len());
        self.edit_tag(name, add)
    }
    fn edit_tag(&mut self, name: String, add: bool) -> Vec<Action> {
        let keys = self.keys.clone();
        self.input = Input::default();
        self.filter();
        vec![Action::WriteMeta(Box::new(move |ws| {
            let config = skills::config::Config::load(&ws.root)?;
            anyhow::ensure!(config.tags_enabled, "Tags are disabled");
            anyhow::ensure!(!keys.is_empty(), "No skills selected");
            anyhow::ensure!(
                !name.is_empty() && name != "(untagged)" && !name.contains(','),
                "Invalid tag name"
            );
            for key in &keys {
                anyhow::ensure!(ws.skill_path(key).is_dir(), "no such skill: {key}");
            }
            history::tag_edit(ws, |ws| {
                skills::config::Config::edit_tags(&ws.root, |tags| {
                    if !tags.iter().any(|t| t.name == name) {
                        tags.push(skills::config::TagConfig {
                            name: name.clone(),
                            skills: vec![],
                            color: None,
                            description: None,
                        });
                    }
                    let tag = tags.iter_mut().find(|t| t.name == name).unwrap();
                    tag.skills.retain(|k| !keys.contains(k));
                    if add {
                        tag.skills.extend(keys.clone());
                    }
                })?;
                Ok(format!("Updated tag {name}"))
            })
        }))]
    }
    fn pending_count(&self) -> usize {
        self.rows
            .iter()
            .filter(|r| {
                r.desired == Some(true) && r.count < self.keys.len()
                    || self.kind == Kind::Deploy && r.desired == Some(false) && r.count > 0
            })
            .count()
    }
    fn apply(&mut self, ctx: &Ctx) -> Vec<Action> {
        if self.kind != Kind::Tags && self.pending_count() == 0 {
            return vec![];
        }
        if self.keys.is_empty() {
            self.error = Some("No skills selected.".into());
            return vec![];
        }
        let changes: Vec<(String, bool)> = self
            .rows
            .iter()
            .filter_map(|r| r.desired.map(|on| (r.id.clone(), on)))
            .collect();
        if changes.is_empty() && self.kind != Kind::Tags {
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
            Kind::Tags => self.edit_selected_tag(),
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
                            format!("Added missing skills to {} presets", originals.len()),
                            (!intents.is_empty()).then_some(history::Intent::Meta(intents)),
                        ))
                    }),
                    keys,
                )]
            }
        }
    }
    pub fn mouse(&mut self, m: MouseEvent, ctx: &Ctx) -> Vec<Action> {
        self.remove_armed = false;
        let at = ratatui::layout::Position::new(m.column, m.row);
        if self.kind == Kind::Tags {
            match m.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    if !self.rect.contains(at) {
                        return vec![Action::CloseModal];
                    }
                    if let Some((rect, name)) =
                        self.tag_chips.iter().find(|(rect, _)| rect.contains(at))
                    {
                        let (_, right) = ctx.settings.ui.pill_caps.glyphs();
                        let close_x = rect
                            .right()
                            .saturating_sub(super::widgets::width(right) as u16 + 2);
                        if m.column == close_x {
                            return self.edit_tag(name.clone(), false);
                        }
                        return vec![];
                    }
                    if self.input_rect.contains(at) {
                        self.input.click(m.column);
                    }
                    if self.list.rows.contains(at)
                        && let Some(i) = self.list.row_at(m.row, self.shown.len())
                    {
                        self.list.select(Some(i));
                        return self.edit_selected_tag();
                    }
                }
                MouseEventKind::ScrollDown => self.list.move_by(1, self.shown.len()),
                MouseEventKind::ScrollUp => self.list.move_by(-1, self.shown.len()),
                _ => {}
            }
            return vec![];
        }
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if !self.rect.contains(at) || self.buttons[1].contains(at) {
                    return vec![Action::CloseModal];
                }
                if self.buttons[0].contains(at) {
                    self.focus = 2;
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
        if self.kind == Kind::Tags {
            let selected = self
                .list
                .selected()
                .and_then(|i| self.shown.get(i))
                .map(|i| {
                    self.rows
                        .get(*i)
                        .map(|r| r.id.clone())
                        .unwrap_or_else(|| self.input.value().trim().to_string())
                });
            let fresh = Self::tags(self.keys.clone(), ctx);
            if self
                .rows
                .iter()
                .map(|r| (&r.id, r.count))
                .ne(fresh.rows.iter().map(|r| (&r.id, r.count)))
            {
                self.rows = fresh.rows;
                self.filter();
                if let Some(name) = selected {
                    let index = self
                        .shown
                        .iter()
                        .position(|i| self.rows.get(*i).is_some_and(|r| r.id == name));
                    if index.is_some() {
                        self.list.select(index);
                    }
                }
            } else {
                self.rows = fresh.rows;
            }
            self.selected_tags.retain(|name| {
                self.rows
                    .iter()
                    .any(|r| &r.id == name && r.count == self.keys.len())
            });
            for row in self.rows.iter().filter(|r| r.count == self.keys.len()) {
                if !self.selected_tags.contains(&row.id) {
                    self.selected_tags.push(row.id.clone());
                }
            }
            let w = area.width.saturating_sub(2).min(50);
            // Selected tags form removable tokens before the search field.
            let mut chips = Vec::new();
            let mut x = 0u16;
            let mut y = 0u16;
            let available = w.saturating_sub(4);
            for name in &self.selected_tags {
                let (left, right) = ctx.settings.ui.pill_caps.glyphs();
                let caps = super::widgets::width(left) + super::widgets::width(right);
                let label = fit(name, (available as usize).saturating_sub(caps + 5));
                let body = format!(" {label} × ");
                let width = (super::widgets::width(&body) + caps) as u16;
                if x > 0 && x + width > available {
                    x = 0;
                    y += 1;
                }
                chips.push((Rect::new(x, y, width, 1), name.clone(), body));
                x += width + 1;
            }
            let total_chip_rows = if chips.is_empty() { 0 } else { y + 1 };
            let chip_rows = total_chip_rows.min(area.height.saturating_sub(10) / 2);
            let overflow = total_chip_rows > chip_rows && chip_rows > 0;
            let first_chip_row =
                total_chip_rows.saturating_sub(chip_rows.saturating_sub(u16::from(overflow)));
            let h = area
                .height
                .saturating_sub(2)
                .min(self.shown.len().clamp(1, 7) as u16 + 6 + chip_rows);
            self.rect = Rect::new(
                area.x + (area.width - w) / 2,
                area.y + (area.height - h) / 2,
                w,
                h,
            );
            f.render_widget(OverlayClear, self.rect);
            let block = ctx
                .settings
                .theme
                .block("", false)
                .title(Line::from(Span::styled(
                    format!(
                        " Tags · {} ",
                        if self.keys.len() == 1 {
                            fit(
                                &ctx.snap
                                    .get(&self.keys[0])
                                    .and_then(|s| s.name.clone())
                                    .unwrap_or_else(|| self.keys[0].clone()),
                                w.saturating_sub(12) as usize,
                            )
                        } else {
                            format!("{} skills · tokens shared by all", self.keys.len())
                        }
                    ),
                    ctx.settings.theme.accent(),
                )));
            let inner = block.inner(self.rect);
            f.render_widget(block, self.rect);
            self.tag_chips.clear();
            if inner.height < 4 || inner.width < 6 {
                return;
            }
            if overflow {
                let hidden = chips
                    .iter()
                    .filter(|(rect, _, _)| rect.y < first_chip_row)
                    .count();
                f.render_widget(
                    Paragraph::new(format!("+{hidden} more · search to remove"))
                        .style(ctx.settings.theme.dim()),
                    Rect::new(inner.x + 1, inner.y, inner.width.saturating_sub(2), 1),
                );
            }
            for (mut rect, name, body) in chips {
                if rect.y < first_chip_row {
                    continue;
                }
                rect.x += inner.x + 1;
                rect.y = inner.y + rect.y - first_chip_row + u16::from(overflow);
                f.render_widget(
                    Paragraph::new(Line::from(
                        crate::tui::components::group::TagLabel::new(
                            body.trim(),
                            crate::tui::components::group::tag_fill(&name, ctx),
                        )
                        .render(ctx, rect.width as usize),
                    ))
                    .style(
                        if self.remove_armed && self.selected_tags.last() == Some(&name) {
                            Style::default().add_modifier(ratatui::style::Modifier::UNDERLINED)
                        } else {
                            Style::default()
                        },
                    ),
                    rect,
                );
                self.tag_chips.push((rect, name));
            }
            let input_y = inner.y + chip_rows;
            f.render_widget(
                Paragraph::new("›").style(ctx.settings.theme.accent()),
                Rect::new(inner.x + 1, input_y, 1, 1),
            );
            self.input_rect = Rect::new(inner.x + 3, input_y, inner.width.saturating_sub(4), 1);
            self.input.render(
                f,
                self.input_rect,
                true,
                "Search or create…",
                &ctx.settings.theme,
            );
            f.render_widget(
                Paragraph::new("─".repeat(inner.width as usize)).style(ctx.settings.theme.dim()),
                Rect::new(inner.x, input_y + 1, inner.width, 1),
            );
            self.list.rows = Rect::new(
                inner.x + 1,
                input_y + 2,
                inner.width.saturating_sub(2),
                inner.height.saturating_sub(4 + chip_rows),
            );
            self.list.clamp(self.shown.len());
            let items: Vec<ListItem> = self
                .shown
                .iter()
                .enumerate()
                .map(|(index, i)| {
                    ListItem::new(if let Some(row) = self.rows.get(*i) {
                        let fill = crate::tui::components::group::tag_fill(&row.id, ctx);
                        let (left, right) = ctx.settings.ui.pill_caps.glyphs();
                        let caps_width = super::widgets::width(left) + super::widgets::width(right);
                        let membership = if row.count == self.keys.len() {
                            "✓".to_string()
                        } else if row.count > 0 {
                            format!("{}/{}", row.count, self.keys.len())
                        } else {
                            " ".to_string()
                        };
                        let label = fit(
                            &row.label,
                            (self.list.rows.width as usize).saturating_sub(
                                6 + caps_width + super::widgets::width(&membership),
                            ),
                        );
                        let chip = format!(" {label} ");
                        let gap = self.list.rows.width.saturating_sub(
                            (super::widgets::width(&chip)
                                + caps_width
                                + super::widgets::width(&membership))
                                as u16
                                + 2,
                        );
                        let mut spans = vec![Span::raw(" ")];
                        spans.extend(
                            crate::tui::components::group::TagLabel::new(&label, fill)
                                .render(ctx, usize::MAX),
                        );
                        spans.extend([
                            Span::raw(" ".repeat(gap as usize)),
                            Span::styled(membership, ctx.settings.theme.ok()),
                            Span::raw(" "),
                        ]);
                        Line::from(spans)
                    } else {
                        Line::from(vec![
                            Span::styled(" + ", ctx.settings.theme.accent()),
                            Span::styled("Create ", ctx.settings.theme.dim()),
                            Span::styled(
                                fit(
                                    self.input.value().trim(),
                                    self.list.rows.width.saturating_sub(10) as usize,
                                ),
                                ctx.settings.theme.accent(),
                            ),
                        ])
                    })
                    .style(if self.list.selected() == Some(index) {
                        ctx.settings.theme.selected_unfocused()
                    } else {
                        Style::default()
                    })
                })
                .collect();
            if items.is_empty() {
                f.render_widget(
                    Paragraph::new("Type to add or create a tag").style(ctx.settings.theme.dim()),
                    self.list.rows,
                );
            } else {
                f.render_stateful_widget(List::new(items), self.list.rows, &mut self.list.state);
            }
            f.render_widget(
                Paragraph::new(if self.remove_armed {
                    "Backspace removes last tag · type to cancel"
                } else if self
                    .list
                    .selected()
                    .and_then(|i| self.shown.get(i))
                    .and_then(|i| self.rows.get(*i))
                    .is_some_and(|r| r.count == self.keys.len())
                {
                    "Enter remove · Esc done"
                } else {
                    "Enter add · × remove · Esc done"
                })
                .style(ctx.settings.theme.dim()),
                Rect::new(
                    inner.x + 2,
                    inner.bottom() - 1,
                    inner.width.saturating_sub(4),
                    1,
                ),
            );
            return;
        }
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
            .settings
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
            "Filter…",
            &ctx.settings.theme,
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
                Paragraph::new("No matching entries").style(ctx.settings.theme.dim()),
                self.list.rows,
            );
        } else {
            f.render_stateful_widget(
                List::new(items).highlight_style(if self.focus == 1 {
                    ctx.settings.theme.selected()
                } else {
                    ctx.settings.theme.selected_unfocused()
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
                } else {
                    "Add selected skills to each chosen Preset's fixed member list.".into()
                }
            })
        };
        f.render_widget(
            Paragraph::new(fit(&summary, inner.width as usize)).style(if self.error.is_some() {
                ctx.settings.theme.err()
            } else {
                ctx.settings.theme.dim()
            }),
            Rect::new(inner.x, inner.bottom() - 2, inner.width, 1),
        );
        let focus = match self.focus {
            2 => ChoiceFocus::Apply,
            3 => ChoiceFocus::Cancel,
            _ => ChoiceFocus::List,
        };
        let pending = self.pending_count();
        self.buttons = choice_footer::draw(
            f,
            Rect::new(inner.x, inner.bottom() - 1, inner.width, 1),
            focus,
            pending > 0,
            if pending == 0 {
                "No pending changes"
            } else {
                "Pending changes"
            },
            &ctx.settings.theme,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use skills::Workspace;
    use skills::ops::edit;
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
    fn tag_picker_saves_each_choice_and_stays_open() {
        let fixture = Fixture::new();
        let ws = Workspace::open(&fixture.0).unwrap();
        edit::tag_add(&ws, "beta", &["merlin".into(), "meta".into()]).unwrap();
        // App reloads configuration after writes before publishing a new snapshot.
        let ws = Workspace::open(&fixture.0).unwrap();
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
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let mut batch = Batch::tags(vec!["alpha".into()], &ctx);
        batch.paste("mer");
        assert_eq!(batch.shown.len(), 2); // Existing merlin and Create mer.
        batch.key(key(KeyCode::Tab), &ctx);
        assert_eq!(batch.input.value(), "merlin");
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(90, 25)).unwrap();
        for name in ["merlin", "meta", "new-tag"] {
            batch.input = Input::default();
            batch.paste(name);
            let actions = batch.key(key(KeyCode::Enter), &ctx);
            assert_eq!(actions.len(), 1);
            let Action::WriteMeta(write) = actions.into_iter().next().unwrap() else {
                panic!("immediate write")
            };
            write(&ws).unwrap();
            assert!(
                skills::config::Config::load(&ws.root)
                    .unwrap()
                    .skill_tags("alpha")
                    .contains(&name.to_string())
            );
            let refreshed_ws = Workspace::open(&fixture.0).unwrap();
            let snap = refreshed_ws.scan().unwrap();
            let settings = crate::tui::settings::RuntimeSettings::new(&refreshed_ws.config);
            terminal
                .draw(|f| {
                    batch.draw(
                        f,
                        f.area(),
                        &Ctx {
                            ws: &refreshed_ws,
                            snap: &snap,
                            settings: &settings,
                        },
                    )
                })
                .unwrap();
        }
        let screen: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(!screen.contains("Apply"));
        assert!(!screen.contains("pending"));
        assert!(screen.contains(" new-tag "));
        assert!(screen.contains('×'));
        assert!(batch.shown.is_empty());
        assert_eq!(batch.selected_tags, vec!["merlin", "meta", "new-tag"]);
        let mut styled_ws = Workspace::open(&ws.root).unwrap();
        let styled_snap = styled_ws.scan().unwrap();
        for caps in [
            skills::config::PillCaps::Round,
            skills::config::PillCaps::Block,
            skills::config::PillCaps::None,
        ] {
            styled_ws.config.ui.pill_caps = caps;
            terminal
                .draw(|f| {
                    batch.draw(
                        f,
                        f.area(),
                        &Ctx {
                            ws: &styled_ws,
                            snap: &styled_snap,
                            settings: &crate::tui::settings::RuntimeSettings::new(
                                &styled_ws.config,
                            ),
                        },
                    )
                })
                .unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            let (left, right) = caps.glyphs();
            assert!(text.contains(&format!("{left} new-tag × {right}")));
        }
        // Only the close glyph removes a token.
        let chip = batch
            .tag_chips
            .iter()
            .find(|(_, name)| name == "new-tag")
            .unwrap()
            .0;
        assert!(
            batch
                .mouse(
                    MouseEvent {
                        kind: MouseEventKind::Down(MouseButton::Left),
                        column: chip.x,
                        row: chip.y,
                        modifiers: KeyModifiers::NONE
                    },
                    &ctx
                )
                .is_empty()
        );
        let actions = batch.mouse(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: chip.right()
                    - super::super::widgets::width(ws.config.ui.pill_caps.glyphs().1) as u16
                    - 2,
                row: chip.y,
                modifiers: KeyModifiers::NONE,
            },
            &ctx,
        );
        let Action::WriteMeta(write) = actions.into_iter().next().unwrap() else {
            panic!("click toggles")
        };
        write(&ws).unwrap();
        assert_eq!(
            skills::config::Config::load(&ws.root)
                .unwrap()
                .skill_tags("alpha"),
            vec!["merlin", "meta"]
        );
        assert!(matches!(
            batch.key(key(KeyCode::Esc), &ctx).as_slice(),
            [Action::CloseModal]
        ));
        assert_eq!(
            skills::config::Config::load(&ws.root)
                .unwrap()
                .skill_tags("alpha"),
            vec!["merlin", "meta"]
        );
    }
    #[test]
    fn selected_tokens_wrap_and_backspace_removes_only_when_input_is_empty() {
        let fixture = Fixture::new();
        let ws = Workspace::open(&fixture.0).unwrap();
        edit::tag_add(
            &ws,
            "alpha",
            &[
                "long-first-tag".into(),
                "long-second-tag".into(),
                "third".into(),
            ],
        )
        .unwrap();
        // App reloads configuration after writes before publishing a new snapshot.
        let ws = Workspace::open(&fixture.0).unwrap();
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
        let mut batch = Batch::tags(vec!["alpha".into()], &ctx);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(32, 20)).unwrap();
        terminal.draw(|f| batch.draw(f, f.area(), &ctx)).unwrap();
        assert_eq!(batch.tag_chips.len(), 3);
        assert!(batch.tag_chips[1].0.y > batch.tag_chips[0].0.y);
        assert!(
            batch
                .tag_chips
                .iter()
                .all(|(rect, _)| rect.bottom() <= batch.input_rect.y)
        );
        assert!(batch.shown.is_empty());
        let mut short = ratatui::Terminal::new(ratatui::backend::TestBackend::new(24, 14)).unwrap();
        short.draw(|f| batch.draw(f, f.area(), &ctx)).unwrap();
        assert_eq!(batch.tag_chips.last().unwrap().1, "third");
        let text: String = short
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("more"));
        batch.paste("long-first-tag");
        assert_eq!(batch.rows[batch.shown[0]].id, "long-first-tag");
        batch.input = Input::default();
        batch.filter();
        batch.paste("x");
        let key = KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE);
        assert!(batch.key(key, &ctx).is_empty());
        assert!(batch.input.value().is_empty());
        assert!(batch.key(key, &ctx).is_empty());
        assert!(batch.remove_armed);
        let Action::WriteMeta(write) = batch.key(key, &ctx).remove(0) else {
            panic!("remove last token")
        };
        write(&ws).unwrap();
        assert_eq!(
            skills::config::Config::load(&ws.root)
                .unwrap()
                .skill_tags("alpha"),
            vec!["long-first-tag", "long-second-tag"]
        );
    }
    #[test]
    fn partial_tags_are_preserved_until_toggled_and_batch_has_one_undo() {
        let fixture = Fixture::new();
        let ws = Workspace::open(&fixture.0).unwrap();
        edit::tag_add(&ws, "alpha", &["existing".into()]).unwrap();
        // App reloads configuration after writes before publishing a new snapshot.
        let ws = Workspace::open(&fixture.0).unwrap();
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
        let mut batch = Batch::tags(vec!["alpha".into(), "beta".into()], &ctx);
        assert_eq!(batch.rows[0].marker(2), "[−]");
        batch.paste("new");
        let Action::WriteMeta(write) = batch
            .key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &ctx)
            .remove(0)
        else {
            panic!("expected metadata edit")
        };
        let (_, intent) = write(&ws).unwrap();
        assert!(
            skills::config::Config::load(&ws.root)
                .unwrap()
                .skill_tags("alpha")
                .contains(&"existing".into())
        );
        assert_eq!(
            skills::config::Config::load(&ws.root)
                .unwrap()
                .skill_tags("beta"),
            vec!["new"]
        );
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
        // App reloads configuration after writes before publishing a new snapshot.
        let ws = Workspace::open(&fixture.0).unwrap();
        let snap = ws.scan().unwrap();
        let path = skills::config::Config::path(&ws.root);
        std::fs::write(&path, "tags = 42").unwrap();
        let before = std::fs::read(&path).unwrap();
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
        let mut batch = Batch::tags(vec!["alpha".into(), "beta".into()], &ctx);
        batch.filter();
        let Action::WriteMeta(write) = batch
            .key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &ctx)
            .remove(0)
        else {
            panic!("expected metadata edit")
        };
        assert!(write(&ws).is_err());
        assert_eq!(std::fs::read(path).unwrap(), before);
    }
    #[test]
    fn adding_to_several_presets_preserves_members_and_groups_history() {
        let fixture = Fixture::new();
        let ws = Workspace::open(&fixture.0).unwrap();
        edit::tag_add(&ws, "alpha", &["dev".into()]).unwrap();
        let ws = Workspace::open(&fixture.0).unwrap();
        for name in ["one", "two"] {
            ws.presets
                .save(&skills::preset::Preset {
                    name: name.into(),
                    skills: if name == "one" {
                        vec!["other".into(), "alpha".into()]
                    } else {
                        vec!["other".into()]
                    },
                    ..Default::default()
                })
                .unwrap();
        }
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
        let mut batch = Batch::presets(vec!["alpha".into(), "beta".into()], &ctx);
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        batch.key(key(KeyCode::Enter), &ctx);
        batch.key(key(KeyCode::Tab), &ctx);
        assert_eq!(batch.focus, 2);
        assert!(batch.key(key(KeyCode::Enter), &ctx).is_empty());
        batch.key(key(KeyCode::Tab), &ctx);
        assert_eq!(batch.focus, 3);
        batch.key(key(KeyCode::Up), &ctx);
        batch.list.select(Some(1));
        batch.key(key(KeyCode::Down), &ctx);
        assert_eq!(batch.focus, 2);
        batch.key(key(KeyCode::Up), &ctx);
        assert_eq!(batch.list.selected(), Some(1));
        assert!(batch.key(key(KeyCode::Enter), &ctx).is_empty());
        assert_eq!(batch.rows[1].desired, Some(true));
        batch.key(key(KeyCode::BackTab), &ctx);
        assert_eq!(batch.focus, 3);
        assert!(matches!(
            batch.key(key(KeyCode::Enter), &ctx).as_slice(),
            [Action::CloseModal]
        ));
        assert_eq!(
            ws.presets.load("two").unwrap().unwrap().skills,
            vec!["other"]
        );
        assert_eq!(batch.rows[0].count, 1);
        assert_eq!(batch.rows[1].count, 0);
        for row in &mut batch.rows {
            row.desired = Some(true);
        }
        let Action::BatchMeta(write, _) = batch.apply(&ctx).remove(0) else {
            panic!("expected metadata edit")
        };
        let (_, intent) = write(&ws).unwrap();
        for name in ["one", "two"] {
            let preset = ws.presets.load(name).unwrap().unwrap();
            assert_eq!(preset.members(), vec!["alpha", "beta", "other"]);
        }
        let Some(history::Intent::Meta(changes)) = intent else {
            panic!("expected metadata intent")
        };
        assert_eq!(changes.len(), 2);
        edit::tag_remove(&ws, "alpha", &["dev".into()]).unwrap();
        let preset = ws.presets.load("one").unwrap().unwrap();
        assert_eq!(preset.members(), vec!["alpha", "beta", "other"]);
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
            settings: &{
                let mut settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
                settings.theme = theme;
                settings
            },
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
            settings: &{
                let mut settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
                settings.theme = theme;
                settings
            },
        };
        batch.rows[0].desired = Some(true);
        assert!(batch.plan(&ctx).unwrap().is_empty());
        assert_eq!(
            std::fs::read_link(agent.join("alpha")).unwrap(),
            ws.skill_path("alpha")
        );
    }
}
