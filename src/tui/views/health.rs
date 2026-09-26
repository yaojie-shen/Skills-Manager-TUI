//! Health tab: skill problems and update checks.
//!
//! The list on the left says *that* something is wrong; the pane on the right
//! says *why* the selected item is here and which keys can do something about
//! it. The footer shows only those keys, so the user never has to guess which
//! of `a`, `m`, `x` and `U` fits the row under the cursor.

mod context;
use super::preview::{Overlay, kv};
use super::{View, wheel};
use crate::tui::app::{Action, Ctx, Hints, Tab};
use crate::tui::components::choice_footer::{self, ChoiceEvent, ChoiceFocus};
use crate::tui::components::context_menu::{Command, Item, Request, Target};
use crate::tui::components::layout::split_panes;
use crate::tui::components::skill::{status_glyph, status_text};
use crate::tui::event::Task;
use crate::tui::modal::Modal;
use crate::tui::widgets::{ListNav, fit, pad, width};
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, Paragraph, Wrap};
use skills::meta::{Source, short_rev};
use skills::ops::update::CheckResult;
use skills::ops::{deploy, edit};
use skills::reconcile::{AgentDirMode, EntryState, HealthClass, SkillRecord, SkillStatus};
use std::collections::{BTreeMap, BTreeSet};

/// Which of the page's actions apply to one row. Decided once per rebuild
/// from the record and its check result, so both the key handler and the
/// footer read the same answer.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Caps {
    accept: bool,
    clean: bool,
    update: bool,
}

impl Caps {
    fn of(r: &SkillRecord, check: Option<&Result<CheckResult, String>>) -> Self {
        let remote = r.source.as_ref().is_some_and(Source::is_remote);
        let update_available = matches!(check, Some(Ok(c)) if c.update_available);
        Caps {
            accept: matches!(
                r.status,
                SkillStatus::Modified | SkillStatus::MissingBaseline
            ),
            clean: matches!(r.status, SkillStatus::Invalid { .. }),
            // `update::prepare` requires usable repository content, so
            // offering `U` for a missing or invalid skill would only produce
            // an error toast.
            update: remote
                && update_available
                && matches!(
                    r.status,
                    SkillStatus::Repository | SkillStatus::MissingBaseline | SkillStatus::Modified
                ),
        }
    }
}

struct Row {
    class: Option<HealthClass>,
    key: String,
    caps: Caps,
    agent: Option<String>,
    state: Option<EntryState>,
    heading: Option<String>,
    explanation: Option<String>,
}

#[derive(Default)]
pub struct HealthView {
    rows: Vec<Row>,
    filter: super::filter::Filter,
    total_issues: usize,
    list: ListNav,
    left: Rect,
    right: Rect,
    detail_scroll: u16,
    detail_rows: usize,
    detail_height: u16,
    preview: Overlay,
    /// key -> last check result
    checks: BTreeMap<String, Result<CheckResult, String>>,
}

/// The content hash without its algorithm prefix, cut to the same length as
/// a short git revision. Twelve hex characters tell two hashes apart on
/// screen just as well as the full digest does.
fn short_hash(h: &str) -> &str {
    short_rev(h.strip_prefix("sha256:").unwrap_or(h))
}

impl HealthView {
    pub fn focus_input(&mut self) {
        self.filter.editing = true;
    }

    pub fn input_focused(&self) -> bool {
        self.filter.editing
    }
    pub fn paste(&mut self, text: &str, ctx: &Ctx) -> Vec<Action> {
        if self.preview.is_open() {
            return vec![];
        }
        let actions = self.filter.paste(text);
        self.rebuild(ctx);
        actions
    }

    fn select_skills(&self, checked: Option<String>) -> Vec<Action> {
        if !self
            .rows
            .iter()
            .any(|row| row.agent.is_none() && row.heading.is_none())
        {
            return vec![];
        }
        vec![Action::SelectSkills {
            keys: self
                .rows
                .iter()
                .filter(|row| row.agent.is_none() && row.heading.is_none())
                .map(|row| row.key.clone())
                .collect(),
            title: "Health".into(),
            checked,
        }]
    }

    fn selected_row(&self) -> Option<&Row> {
        self.list.selected().and_then(|i| self.rows.get(i))
    }

    fn selected<'a>(&self, ctx: &'a Ctx) -> Option<&'a SkillRecord> {
        self.selected_row()
            .filter(|r| r.agent.is_none() && r.heading.is_none())
            .and_then(|r| ctx.snap.get(&r.key))
    }

    fn select_by(&mut self, delta: i32) {
        self.list.move_by(delta, self.rows.len());
        while self.selected_row().is_some_and(|r| r.heading.is_some()) {
            let previous = self.list.selected();
            self.list.move_by(delta.signum(), self.rows.len());
            if self.list.selected() == previous {
                break;
            }
        }
        // The pane describes a different item now, so start it from the top.
        self.detail_scroll = 0;
    }

    fn open_selected(&mut self, ctx: &Ctx) -> Vec<Action> {
        if let Some(actions) = self.agent_action(KeyCode::Enter, ctx) {
            return actions;
        }
        if let Some(r) = self.selected(ctx) {
            self.preview.open(r.key.clone());
        }
        vec![]
    }

    pub fn on_check(
        &mut self,
        results: &[(String, anyhow::Result<CheckResult>)],
        ctx: &Ctx,
    ) -> Vec<Action> {
        for (k, r) in results {
            self.checks
                .insert(k.clone(), r.as_ref().cloned().map_err(|e| format!("{e:#}")));
        }
        self.rebuild(ctx);
        let n = results
            .iter()
            .filter(|(_, r)| r.as_ref().map(|c| c.update_available).unwrap_or(false))
            .count();
        vec![Action::Toast(format!(
            "checked {} skill(s), {n} with updates",
            results.len()
        ))]
    }

    fn rebuild(&mut self, ctx: &Ctx) {
        let selected = self
            .selected_row()
            .map(|r| (r.agent.clone(), r.key.clone()));
        self.rows = ctx
            .snap
            .skills
            .iter()
            .filter(|s| {
                !s.status.is_healthy()
                    || self
                        .checks
                        .get(&s.key)
                        .map(|c| c.as_ref().map(|c| c.update_available).unwrap_or(true))
                        .unwrap_or(false)
            })
            .map(|s| Row {
                class: s.status.health_class().or(Some(HealthClass::Review)),
                key: s.key.clone(),
                caps: Caps::of(s, self.checks.get(&s.key)),
                agent: None,
                state: None,
                heading: None,
                explanation: None,
            })
            .collect();
        if !self.rows.is_empty() {
            self.rows.insert(0, Row::heading("skills".into()));
        }
        for agent in &ctx.snap.agents {
            let issues: Vec<_> = agent
                .entries
                .iter()
                .filter(|(_, state)| !matches!(state, EntryState::Deployed))
                .collect();
            let mode_issue = match &agent.mode {
                AgentDirMode::Missing => Some(
                    "Review: agent directory does not exist. Deploy from Agents if needed; unused agents do not need a directory."
                        .to_string(),
                ),
                AgentDirMode::ReadOnly { reason, resolved } => Some(format!(
                    "Read-only: {reason}. Skills Manager will not modify this directory{}.",
                    resolved
                        .as_ref()
                        .map(|path| format!(" → {}", path.display()))
                        .unwrap_or_default()
                )),
                _ => None,
            };
            self.rows.push(Row::heading(format!(
                "{}  {}{}",
                agent.key,
                agent.skills_dir.display(),
                if issues.is_empty() && mode_issue.is_none() {
                    " · no issues"
                } else {
                    ""
                }
            )));
            if let Some(explanation) = mode_issue {
                self.rows.push(Row {
                    class: agent.mode.health_class(),
                    key: "directory".into(),
                    caps: Caps::default(),
                    agent: Some(agent.key.clone()),
                    state: None,
                    heading: None,
                    explanation: Some(explanation),
                });
            }
            for (name, state) in issues {
                self.rows.push(Row {
                    class: state.health_class(),
                    key: name.clone(),
                    caps: Caps::default(),
                    agent: Some(agent.key.clone()),
                    state: Some(state.clone()),
                    heading: None,
                    explanation: None,
                });
            }
        }
        self.total_issues = self.rows.iter().filter(|r| r.heading.is_none()).count();
        let documents = self
            .rows
            .iter()
            .map(|row| skills::search::TextDocument {
                name: row.key.clone(),
                description: row.explanation.clone().unwrap_or_default(),
                body: format!(
                    "{} {}",
                    row.agent.as_deref().unwrap_or(""),
                    ctx.snap
                        .get(&row.key)
                        .map(|r| status_text(&r.status))
                        .unwrap_or_default()
                ),
            })
            .collect::<Vec<_>>();
        let order = self
            .filter
            .rank(&documents, ctx)
            .into_iter()
            .enumerate()
            .map(|(rank, i)| (i, rank))
            .collect::<std::collections::BTreeMap<_, _>>();
        let mut group = 0;
        let mut ranked = std::mem::take(&mut self.rows)
            .into_iter()
            .enumerate()
            .filter_map(|(i, row)| {
                if row.heading.is_some() {
                    group += 1;
                    Some((group, 0, row))
                } else {
                    order.get(&i).map(|rank| (group, rank + 1, row))
                }
            })
            .collect::<Vec<_>>();
        ranked.sort_by_key(|(group, rank, _)| (*group, *rank));
        self.rows = ranked.into_iter().map(|(_, _, row)| row).collect();
        // Drop empty section headings after filtering.
        let mut has_child = false;
        let mut keep = vec![false; self.rows.len()];
        for (i, row) in self.rows.iter().enumerate().rev() {
            if row.heading.is_some() {
                keep[i] = has_child;
                has_child = false;
            } else {
                keep[i] = true;
                has_child = true;
            }
        }
        let mut i = 0;
        self.rows.retain(|_| {
            let yes = keep[i];
            i += 1;
            yes
        });
        let selection = selected
            .and_then(|(agent, key)| {
                self.rows
                    .iter()
                    .position(|r| r.heading.is_none() && r.agent == agent && r.key == key)
            })
            .or_else(|| self.rows.iter().position(|r| r.heading.is_none()));
        self.list.select(selection);
        self.list.clamp(self.rows.len());
    }

    fn issue_count(&self) -> usize {
        self.rows
            .iter()
            .filter(|r| r.class == Some(HealthClass::Fault))
            .count()
    }

    fn agent_command(&self, code: Command, ctx: &Ctx) -> Option<Vec<Action>> {
        let row = self.selected_row()?;
        let agent = row.agent.as_ref()?;
        let actions = match (&row.state, code) {
            (Some(EntryState::Foreign { .. }), Command::Remove | Command::Adopt) => {
                use skills::ops::agent_links::{self, Repair};
                let operation = if code == Command::Adopt {
                    Repair::Adopt
                } else {
                    Repair::Remove
                };
                let plan = match agent_links::plan(ctx.ws, agent, &row.key, operation) {
                    Ok(plan) => plan,
                    Err(error) => return Some(vec![Action::Error(format!("{error:#}"))]),
                };
                let lines = vec![
                    format!("{} → {}", plan.path.display(), plan.target.display()),
                    match operation {
                        Repair::Remove => "Remove only this symlink? Its external target will be preserved.".into(),
                        Repair::Adopt => "Copy this skill into the root and repoint the agent link? Its external target will be preserved.".into(),
                    },
                    "Undo does not cover this repair.".into(),
                ];
                let modal = Modal::confirm_write(
                    format!("repair {agent}/{}", row.key),
                    lines,
                    Box::new(move |ws| plan.apply(ws)),
                );
                let modal = if matches!(operation, Repair::Remove) {
                    modal.deployment_only()
                } else {
                    modal
                };
                return Some(vec![Action::OpenModal(Box::new(modal))]);
            }
            (Some(EntryState::Broken { .. }), Command::Remove | Command::Open) => {
                deploy::plan_clean(ctx.ws, ctx.snap, agent, std::slice::from_ref(&row.key))
            }
            (Some(EntryState::Shadow { same_content: true }), Command::Relink | Command::Open) => {
                deploy::plan_relink(ctx.ws, ctx.snap, agent, std::slice::from_ref(&row.key))
            }
            (Some(EntryState::AgentOnly), Command::Adopt | Command::Open) => {
                let report = ctx.snap.agent(agent)?;
                return Some(vec![Action::OpenModal(Box::new(Modal::adopt(
                    agent,
                    &row.key,
                    report.skills_dir.join(&row.key),
                )))]);
            }
            (_, Command::Open) => return Some(vec![Action::SwitchTab(Tab::Agents)]),
            _ => return None,
        };
        Some(match actions {
            Ok(actions) => vec![Action::ConfirmLinks {
                title: format!("repair {agent}/{}", row.key),
                actions,
            }],
            Err(e) => vec![Action::Error(format!("{e:#}"))],
        })
    }

    /// The right pane: what the record actually carries for this status and
    /// which keys act on it. Nothing here is guessed; where the record
    /// cannot answer a question, the text says so.
    fn agent_action(&self, code: KeyCode, ctx: &Ctx) -> Option<Vec<Action>> {
        let command = match code {
            KeyCode::Enter => Command::Open,
            KeyCode::Char('x') => Command::Remove,
            KeyCode::Char('a') => Command::Adopt,
            KeyCode::Char('r') => Command::Relink,
            _ => return None,
        };
        self.agent_command(command, ctx)
    }

    fn detail_lines(&self, r: &SkillRecord, caps: Caps, ctx: &Ctx) -> Vec<Line<'static>> {
        let th = &ctx.settings.theme;
        let key = r.key.clone();
        let heading = |s: &'static str| Line::from(Span::styled(s, th.bold().fg(th.accent)));
        let text = |s: String| Line::from(Span::raw(s));
        let action = |k: &'static str, what: String| {
            Line::from(vec![
                Span::styled(format!("{k:<7}"), th.key_hint()),
                Span::raw(what),
            ])
        };
        let source_line = |lines: &mut Vec<Line<'static>>| {
            lines.push(kv(
                "source",
                r.source
                    .as_ref()
                    .map(|s| s.summary())
                    .unwrap_or_else(|| "none".into()),
                th,
            ));
        };
        let meta_lines = |lines: &mut Vec<Line<'static>>| {
            let tags = if r.tags.is_empty() {
                "none".to_string()
            } else {
                r.tags.join(", ")
            };
            lines.push(kv("tags", tags, th));
            source_line(lines);
            if let Some(n) = &r.note {
                lines.push(kv("note", n.lines().next().unwrap_or("").to_string(), th));
                for l in n.lines().skip(1) {
                    lines.push(kv("", l.to_string(), th));
                }
            } else {
                lines.push(kv("note", "none", th));
            }
        };

        let mut lines = vec![Line::from(vec![
            Span::styled(key.clone(), th.bold().fg(th.accent)),
            Span::raw("  "),
            status_glyph(&r.status, th),
            Span::raw(" "),
            Span::styled(status_text(&r.status), th.dim()),
        ])];
        lines.push(Line::from(""));
        lines.push(heading("why it is here"));
        let mut actions: Vec<Line<'static>> = Vec::new();
        match &r.status {
            SkillStatus::Modified => {
                lines.push(text(
                    "The directory's content hash differs from the baseline recorded when \
                     the skill was installed, adopted or last accepted, so something inside \
                     it changed since then. The hash covers the whole directory; which files \
                     changed is not recorded."
                        .into(),
                ));
                lines.push(kv(
                    "baseline",
                    r.baseline_hash.as_deref().map(short_hash).unwrap_or("-"),
                    th,
                ));
                lines.push(kv(
                    "current",
                    r.current_hash.as_deref().map(short_hash).unwrap_or("-"),
                    th,
                ));
                source_line(&mut lines);
                if let Some(source) = &r.source
                    && source.is_remote()
                {
                    lines.push(kv(
                        "installed",
                        source.revision().map(short_rev).unwrap_or("-"),
                        th,
                    ));
                }
                actions.push(action(
                    "a",
                    "accept the current content as the new baseline".into(),
                ));
            }
            SkillStatus::MissingBaseline => {
                lines.push(text(
                    "The metadata has no baseline hash, which happens when it was written by \
                     hand, so local changes cannot be detected until one is recorded."
                        .into(),
                ));
                lines.push(kv(
                    "current",
                    r.current_hash.as_deref().map(short_hash).unwrap_or("-"),
                    th,
                ));
                source_line(&mut lines);
                actions.push(action(
                    "a",
                    "record the current content hash as the baseline".into(),
                ));
            }
            SkillStatus::Local | SkillStatus::Repository => {
                lines.push(text(
                    "The skill itself is fine; it is listed because of the last update check."
                        .into(),
                ));
                source_line(&mut lines);
            }
            SkillStatus::MissingSource => {
                lines.push(text("This repository skill has no upstream source record. Restore its source metadata before checking for updates.".into()));
            }
            SkillStatus::Missing => {
                lines.push(text(format!(
                    "There is a metadata file for this name but no directory at {}. The \
                     metadata is kept until you decide what to do with it.",
                    r.path.display()
                )));
                lines.push(kv("meta", ctx.ws.meta.path(&key).display().to_string(), th));
                meta_lines(&mut lines);
                match &r.source {
                    Some(Source::Git { .. } | Source::Archive { .. }) => actions.push(action(
                        "",
                        "or reinstall it from the Library tab with i, from the source above".into(),
                    )),
                    Some(Source::Local { path: Some(p) }) => actions.push(action(
                        "",
                        format!("or reinstall it from the Library tab with i, from {p}"),
                    )),
                    Some(Source::Local { path: None }) => actions.push(action(
                        "",
                        "the local source has no path recorded, so the tool cannot reinstall it"
                            .into(),
                    )),
                    None => actions.push(action(
                        "",
                        "no source is recorded, so the tool cannot reinstall it".into(),
                    )),
                }
            }
            SkillStatus::Renamed { to } => {
                lines.push(text(format!(
                    "The baseline hash in this metadata equals the current content of the \
                     unidentified directory \"{to}\", and neither hash matches anything else, so \
                     the directory was most likely renamed from \"{key}\" to \"{to}\"."
                )));
                lines.push(kv(
                    "now at",
                    ctx.snap.root.join(to).display().to_string(),
                    th,
                ));
                lines.push(kv(
                    "hash",
                    r.baseline_hash.as_deref().map(short_hash).unwrap_or("-"),
                    th,
                ));
                meta_lines(&mut lines);
                actions.push(action(
                    "",
                    "This possible move is informational only; no automatic migration is available."
                        .into(),
                ));
            }
            SkillStatus::Invalid { reason } => {
                lines.push(kv("error", reason.clone(), th));
                lines.push(text(
                    "A directory without a readable SKILL.md is not a skill: it cannot be \
                     deployed or previewed."
                        .into(),
                ));
                if r.meta.is_some() {
                    lines.push(text(
                        "Its metadata is kept for as long as the directory stays.".into(),
                    ));
                    meta_lines(&mut lines);
                }
                actions.push(action(
                    "x",
                    "discard: delete the directory and its metadata".into(),
                ));
            }
            SkillStatus::CorruptMeta { error } => {
                lines.push(kv("error", error.clone(), th));
                lines.push(text(format!(
                    "The metadata file could not be parsed, so its metadata is unavailable. \
                     The file is never overwritten automatically; fix or delete {} \
                     by hand.",
                    ctx.ws.meta.path(&key).display()
                )));
            }
        }

        match self.checks.get(&key) {
            Some(Ok(c)) if c.update_available => {
                lines.push(Line::from(""));
                lines.push(heading("update available"));
                lines.push(kv(
                    "revision",
                    format!(
                        "{} → {}",
                        c.installed.as_deref().map(short_rev).unwrap_or("-"),
                        short_rev(&c.remote)
                    ),
                    th,
                ));
                if let Some(b) = &c.branch {
                    lines.push(kv("branch", b.clone(), th));
                }
                if caps.update {
                    let what = if r.status == SkillStatus::Modified {
                        "update from upstream; choose local or upstream for the whole skill"
                    } else {
                        "update to the upstream revision"
                    };
                    actions.push(action("U", what.into()));
                } else {
                    actions.push(action(
                        "",
                        format!(
                            "updating requires an existing skill with upstream information; it is {}",
                            r.status.label()
                        ),
                    ));
                }
            }
            Some(Ok(c)) => {
                lines.push(Line::from(""));
                lines.push(heading("update check"));
                lines.push(kv(
                    "upstream",
                    format!("up to date at {}", short_rev(&c.remote)),
                    th,
                ));
            }
            Some(Err(e)) => {
                lines.push(Line::from(""));
                lines.push(heading("update check"));
                lines.push(kv("failed", e.clone(), th));
            }
            None => {}
        }

        lines.push(Line::from(""));
        lines.push(heading("what you can do"));
        lines.extend(actions);
        lines.push(action(
            "Enter",
            "open the skill in a window over this page".into(),
        ));
        lines
    }

    fn draw_detail(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        let th = &ctx.settings.theme;
        let block = th.block(" detail ", false);
        let inner = block.inner(area);
        f.render_widget(block, area);
        let Some(row) = self.selected_row() else {
            return;
        };
        let lines = if let Some(heading) = &row.heading {
            vec![
                Line::from(heading.clone()),
                Line::from("Select an issue to see its details and repair actions."),
            ]
        } else if let Some(agent) = &row.agent {
            let (description, action) = agent_description(row);
            vec![
                kv("agent", agent.clone(), th),
                kv("entry", row.key.clone(), th),
                Line::from(""),
                Line::from(description),
                Line::from(""),
                Line::from(action),
            ]
        } else if let Some(r) = ctx.snap.get(&row.key) {
            self.detail_lines(r, row.caps, ctx)
        } else {
            return;
        };
        // Count wrapped rows so the wheel cannot scroll past the end.
        let wrap_w = inner.width.max(1) as usize;
        self.detail_rows = lines
            .iter()
            .map(|l| width(&l.to_string()).max(1).div_ceil(wrap_w))
            .sum();
        self.detail_height = inner.height;
        let max = self.detail_rows.saturating_sub(inner.height as usize) as u16;
        self.detail_scroll = self.detail_scroll.min(max);
        f.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .scroll((self.detail_scroll, 0)),
            inner,
        );
    }
}

impl View for HealthView {
    fn overlay_open(&self) -> bool {
        self.preview.is_open()
    }
    fn actions_menu(&self, ctx: &Ctx) -> Option<Request> {
        if self.preview.is_open() || self.filter.editing {
            return None;
        }
        self.issue_menu(ctx)
    }
    fn context_menu(&mut self, x: u16, y: u16, ctx: &Ctx) -> Option<Request> {
        if self.preview.is_open() || !self.left.contains((x, y).into()) {
            return None;
        }
        let index = self.list.row_at(y, self.rows.len())?;
        if self.rows.get(index)?.heading.is_some() {
            return None;
        }
        self.list.select(Some(index));
        self.filter.editing = false;
        self.detail_scroll = 0;
        self.issue_menu(ctx)
    }
    fn context_execute(&mut self, target: &Target, command: Command, ctx: &Ctx) -> Vec<Action> {
        if !self
            .issue_menu(ctx)
            .is_some_and(|r| &r.target == target && r.allows(command))
        {
            return vec![Action::Error(
                "Target changed; reopen the context menu".into(),
            )];
        }
        if self.selected_row().is_some_and(|r| r.agent.is_some()) {
            return self.agent_command(command, ctx).unwrap_or_default();
        }
        self.issue_command(command, ctx)
    }

    fn focus_root(&mut self) {
        self.filter.editing = false;
    }

    fn refresh(&mut self, ctx: &Ctx) {
        self.checks.retain(|key, result| {
            let Some(record) = ctx.snap.get(key) else {
                return false;
            };
            match result {
                Ok(check) => record.source.as_ref().is_some_and(|source| {
                    source.same_location(&check.source)
                        && source.revision() == check.source.revision()
                }),
                Err(_) => false,
            }
        });
        self.rebuild(ctx);
    }

    fn handle_key(&mut self, k: KeyEvent, ctx: &Ctx) -> Vec<Action> {
        if self.preview.handle_key(k) {
            return vec![];
        }
        if self.filter.editing && k.code == KeyCode::Up {
            self.filter.editing = false;
            return vec![Action::BackToParent];
        }
        if self.filter.key(k) {
            self.rebuild(ctx);
            return vec![];
        }
        if let Some(actions) = self.agent_action(k.code, ctx) {
            return actions;
        }
        let command = match k.code {
            KeyCode::Char('U') => Some(Command::Update),
            KeyCode::Char('a') => Some(Command::Accept),
            KeyCode::Char('x') => Some(Command::Remove),
            _ => None,
        };
        if let Some(command) = command {
            return self.issue_command(command, ctx);
        }

        match k.code {
            KeyCode::Char('M') => self.select_skills(None),
            KeyCode::Char('q') => vec![Action::BackToParent],
            KeyCode::Esc => vec![Action::BackToParent],
            KeyCode::Down | KeyCode::Char('j') => {
                self.select_by(1);
                vec![]
            }
            KeyCode::Up | KeyCode::Char('k') => {
                let first = self
                    .rows
                    .iter()
                    .position(|r| r.heading.is_none())
                    .unwrap_or(0);
                if self.list.selected().unwrap_or(0) <= first {
                    self.filter.editing = true;
                } else {
                    self.select_by(-1);
                }
                vec![]
            }
            KeyCode::Enter => self.open_selected(ctx),
            KeyCode::Char('c') => {
                let keys: Vec<String> = ctx
                    .snap
                    .skills
                    .iter()
                    .filter(|s| {
                        s.source
                            .as_ref()
                            .is_some_and(skills::meta::Source::is_remote)
                    })
                    .map(|s| s.key.clone())
                    .collect();
                if keys.is_empty() {
                    vec![Action::Error("no remote-sourced skills to check".into())]
                } else {
                    vec![
                        Action::Toast(format!("checking {} skill(s)…", keys.len())),
                        Action::Spawn(Task::Check(keys)),
                    ]
                }
            }
            _ => vec![],
        }
    }

    fn handle_mouse(&mut self, m: MouseEvent, ctx: &Ctx) -> Vec<Action> {
        if self.preview.handle_mouse(m, ctx) {
            return vec![];
        }
        let at = (m.column, m.row).into();
        if m.kind == MouseEventKind::Down(MouseButton::Left)
            && self.filter.click_input(m.column, m.row)
        {
            self.filter.editing = true;
            return vec![];
        }
        if let Some(d) = wheel(&m, ctx) {
            if self.left.contains(at) {
                self.filter.editing = false;
                self.select_by(d);
            } else if self.right.contains(at) {
                self.filter.editing = false;
                let max = (self.detail_rows as i32 - self.detail_height as i32).max(0);
                self.detail_scroll = (self.detail_scroll as i32 + d).clamp(0, max) as u16;
            }
            return vec![];
        }
        if let MouseEventKind::Down(MouseButton::Left) = m.kind
            && self.left.contains(at)
            && let Some((_, double)) = self.list.click(m.row, self.rows.len())
        {
            self.filter.editing = false;
            self.detail_scroll = 0;
            if double {
                return self.open_selected(ctx);
            }
        } else if m.kind == MouseEventKind::Down(MouseButton::Left) && self.right.contains(at) {
            self.filter.editing = false;
        }
        vec![]
    }

    fn draw(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        let area = self.filter.draw(
            f,
            area,
            "Ctrl-R rescan · : commands · Filter health entries",
            &format!(
                "health · {} faults · {} review · {} independent",
                self.issue_count(),
                self.rows
                    .iter()
                    .filter(|r| r.class == Some(HealthClass::Review))
                    .count(),
                self.rows
                    .iter()
                    .filter(|r| r.class == Some(HealthClass::Independent))
                    .count()
            ),
            true,
            ctx,
        );
        let th = &ctx.settings.theme;
        // With nothing to show there is nothing to explain either, so the
        // message gets the whole width instead of being squeezed beside an
        // empty pane.
        if !self.rows.iter().any(|r| r.heading.is_none()) {
            self.left = area;
            self.right = Rect::default();
            let inner = area;
            let msg = if self.total_issues > 0 {
                format!(
                    "No matching entries · {} entries in total",
                    self.total_issues
                )
            } else {
                format!(
                    "everything is healthy\n{} skills · {} agents checked\nPress c to check remote sources for updates.",
                    ctx.snap.skills.len(),
                    ctx.snap.agents.len()
                )
            };
            f.render_widget(
                Paragraph::new(msg)
                    .style(th.dim())
                    .wrap(Wrap { trim: false }),
                Rect {
                    x: inner.x + 1,
                    width: inner.width.saturating_sub(2),
                    ..inner
                },
            );
            self.preview.draw(f, area, ctx);
            return;
        }
        // The list carries the name and the status text side by side, so it
        // needs a little more than the usual share of the width.
        let (left, right) = split_panes(area, 45, ctx);
        self.left = left;
        self.right = right;
        let items: Vec<ListItem> = self
            .rows
            .iter()
            .map(|row| {
                if let Some(heading) = &row.heading {
                    return ListItem::new(Line::from(Span::styled(
                        fit(heading, left.width.saturating_sub(4) as usize),
                        th.bold().fg(th.accent),
                    )));
                }
                if row.agent.is_some() {
                    let (description, _) = agent_description(row);
                    return ListItem::new(Line::from(Span::styled(
                        fit(
                            &format!(
                                "[{}] {} {}  {description}",
                                match row.class {
                                    Some(HealthClass::Independent) => "independent",
                                    Some(HealthClass::Review) => "review",
                                    _ => "fault",
                                },
                                agent_marker(row),
                                row.key
                            ),
                            left.width.saturating_sub(4) as usize,
                        ),
                        if row.class == Some(HealthClass::Independent) {
                            th.dim()
                        } else {
                            th.warn()
                        },
                    )));
                }
                let r = ctx
                    .snap
                    .get(&row.key)
                    .expect("skill row comes from snapshot");
                let mut spans = vec![
                    status_glyph(&r.status, th),
                    Span::raw(format!(" {}", pad(&r.key, 26))),
                ];
                if !r.status.is_healthy() {
                    spans.push(Span::styled(status_text(&r.status), th.warn()));
                }
                match self.checks.get(&r.key) {
                    Some(Ok(c)) if c.update_available => spans.push(Span::styled(
                        format!(
                            "  update {} → {}",
                            c.installed.as_deref().map(short_rev).unwrap_or("-"),
                            short_rev(&c.remote)
                        ),
                        th.accent(),
                    )),
                    Some(Err(e)) => {
                        spans.push(Span::styled(format!("  check failed: {e}"), th.err()))
                    }
                    _ => {}
                }
                ListItem::new(Line::from(spans))
            })
            .collect();

        self.list.rows = left;
        let list = List::new(items)
            .highlight_style(th.selected())
            .highlight_symbol("▸ ");
        f.render_stateful_widget(list, left, &mut self.list.state);
        self.draw_detail(f, right, ctx);
        self.preview.draw(f, area, ctx);
    }

    fn hints(&self) -> Hints {
        if self.filter.editing {
            return &[("Enter/↓", "issues"), ("Esc", "clear filter")];
        }
        if let Some(hints) = self.preview.hints() {
            return hints;
        }
        if self.selected_row().is_none_or(|row| row.heading.is_some()) {
            return &[
                ("c", "check updates"),
                ("/", "filter"),
                ("Esc/q", "clear/back"),
            ];
        }
        &[
            ("Enter", "open / resolve"),
            ("a", "actions"),
            ("/", "filter"),
            ("c", "check updates"),
            ("Esc/q", "clear/back"),
        ]
    }
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

impl Row {
    fn heading(text: String) -> Self {
        Self {
            class: None,
            key: String::new(),
            caps: Caps::default(),
            agent: None,
            state: None,
            heading: Some(text),
            explanation: None,
        }
    }
}

fn agent_marker(row: &Row) -> &'static str {
    match row.state {
        Some(EntryState::Broken { .. }) => "✗",
        Some(EntryState::Foreign { .. }) => "→",
        Some(EntryState::Shadow { .. } | EntryState::AgentOnly) => "▪",
        _ => "!",
    }
}

fn agent_description(row: &Row) -> (String, &'static str) {
    match &row.state {
        Some(EntryState::Broken { target }) => (
            format!("broken link → {}", target.display()),
            "x / Enter: remove this broken link (confirmation required).",
        ),
        Some(EntryState::Foreign { target }) => (
            format!("links outside the root → {}", target.display()),
            "x: remove only the symlink. a: copy a valid skill into the root and repoint this link. Both require confirmation and preserve the external target. Enter opens Agents.",
        ),
        Some(EntryState::Shadow { same_content: true }) => (
            "the agent's own copy, same content as root".into(),
            "r / Enter: replace the matching copy with a root link (confirmation required).",
        ),
        Some(EntryState::Shadow {
            same_content: false,
        }) => (
            "the agent's own copy differs from root".into(),
            "Review both copies manually; relink is disabled to preserve different content. Enter opens Agents.",
        ),
        Some(EntryState::AgentOnly) => (
            "the agent's own directory, absent from root".into(),
            "a / Enter: adopt into the root (confirmation required; only valid skills can be adopted).",
        ),
        _ => (
            row.explanation.clone().unwrap_or_default(),
            "Enter: review agent configuration in Agents.",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::theme::Theme;
    use ratatui::{Terminal, backend::TestBackend};
    use skills::{Workspace, config::AgentConfig};

    fn context_request_for_row(view: &mut HealthView, index: usize, ctx: &Ctx) -> Request {
        let x = view.left.x.saturating_add(1);
        for y in view.list.rows.y..view.list.rows.bottom() {
            if view.list.row_at(y, view.rows.len()) == Some(index)
                && let Some(request) = crate::tui::views::View::context_menu(view, x, y, ctx)
            {
                return request;
            }
        }
        panic!("expected a context menu target for health row {index}");
    }

    #[test]
    fn health_lists_agent_issues_and_only_offers_safe_repairs() {
        let root =
            std::env::temp_dir().join(format!("skills-health-parity-{}", std::process::id()));
        let central = root.join("central");
        let agent_dir = root.join("agent");
        std::fs::create_dir_all(central.join("printer")).unwrap();
        std::fs::create_dir_all(central.join("invalid-item")).unwrap();
        std::fs::create_dir_all(central.join("repos")).unwrap();
        std::fs::create_dir_all(agent_dir.join("printer")).unwrap();
        let skill = "---\nname: printer\ndescription: Print documents\n---\nPrint documents.\n";
        std::fs::write(central.join("printer/SKILL.md"), skill).unwrap();
        std::fs::write(agent_dir.join("printer/SKILL.md"), skill).unwrap();
        std::os::unix::fs::symlink(root.join("gone"), agent_dir.join("broken-item")).unwrap();
        std::os::unix::fs::symlink(&root, agent_dir.join("foreign-item")).unwrap();
        let mut ws = Workspace::open(&central).unwrap();
        ws.config.agents = vec![AgentConfig {
            key: "sample".into(),
            name: "Sample".into(),
            skills_dir: agent_dir.display().to_string(),
        }];
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
        let mut view = HealthView::default();
        view.refresh(&ctx);
        assert!(
            snap.get("repos").is_none(),
            "repos is a storage container, not an invalid skill"
        );
        assert_eq!(view.issue_count(), 2);
        assert_eq!(view.rows.iter().filter(|r| r.heading.is_some()).count(), 2);
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|f| view.draw(f, f.area(), &ctx)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("health · 2 faults"));
        for name in ["invalid-item", "broken-item", "foreign-item", "printer"] {
            assert!(text.contains(name), "missing {name}");
        }
        assert!(!text.contains("everything is healthy"));

        let broken_index = view
            .rows
            .iter()
            .position(|row| row.agent.as_deref() == Some("sample") && row.key == "broken-item")
            .unwrap();
        let broken_request = context_request_for_row(&mut view, broken_index, &ctx);
        assert!(matches!(
            broken_request.target,
            Target::Entry {
                ref key,
                ref scope,
                ..
            } if key == "broken-item" && scope == "sample"
        ));
        assert!(broken_request.allows(Command::Remove));
        assert!(!broken_request.allows(Command::Relink));
        assert_eq!(
            broken_request
                .items
                .iter()
                .find(|item| item.command == Command::Relink)
                .and_then(|item| item.disabled.as_deref()),
            Some("Requires an identical unmanaged copy")
        );

        let invalid_index = view
            .rows
            .iter()
            .position(|row| row.agent.is_none() && row.key == "invalid-item")
            .unwrap();
        let invalid_request = context_request_for_row(&mut view, invalid_index, &ctx);
        assert!(matches!(
            invalid_request.target,
            Target::Entry {
                ref key,
                ref scope,
                ..
            } if key == "invalid-item" && scope.is_empty()
        ));
        assert!(invalid_request.allows(Command::Remove));
        assert!(!invalid_request.allows(Command::Update));

        for (name, key) in [("broken-item", 'x'), ("printer", 'r')] {
            view.list
                .select(view.rows.iter().position(|r| r.key == name));
            assert!(matches!(
                view.agent_action(KeyCode::Char(key), &ctx)
                    .unwrap()
                    .as_slice(),
                [Action::ConfirmLinks { .. }]
            ));
        }
        view.list
            .select(view.rows.iter().position(|r| r.key == "broken-item"));
        let request = view.issue_menu(&ctx).unwrap();
        assert!(!request.items.iter().any(|i| i.command == Command::Check));
        assert!(matches!(
            view.context_execute(&request.target, Command::Remove, &ctx)
                .as_slice(),
            [Action::ConfirmLinks { .. }]
        ));
        view.list
            .select(view.rows.iter().position(|r| r.key == "printer"));
        assert!(matches!(
            view.context_execute(&request.target, Command::Remove, &ctx)
                .as_slice(),
            [Action::Error(_)]
        ));
        view.list
            .select(view.rows.iter().position(|r| r.key == "foreign-item"));
        assert!(matches!(
            view.agent_action(KeyCode::Char('x'), &ctx)
                .unwrap()
                .as_slice(),
            [Action::OpenModal(_)]
        ));
        assert!(matches!(
            view.agent_action(KeyCode::Char('a'), &ctx)
                .unwrap()
                .as_slice(),
            [Action::Error(_)]
        ));
        let mut confirmation = view.agent_action(KeyCode::Char('x'), &ctx).unwrap();
        let Action::OpenModal(modal) = &mut confirmation[0] else {
            panic!("expected confirmation");
        };
        let mut small = Terminal::new(TestBackend::new(80, 24)).unwrap();
        small.draw(|f| modal.draw(f, f.area(), &ctx)).unwrap();
        let text: String = small
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(
            text.contains("Undo does not cover this repair."),
            "wrapped confirmation must show its final warning"
        );

        assert!(
            std::fs::symlink_metadata(agent_dir.join("broken-item"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(agent_dir.join("printer").is_dir());
        assert!(!central.join(".skills-meta/printer.toml").exists());
        assert!(matches!(
            view.handle_key(KeyEvent::from(KeyCode::Char('q')), &ctx)
                .as_slice(),
            [Action::BackToParent]
        ));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn healthy_message_reports_the_full_checked_scope() {
        let root = std::env::temp_dir().join(format!("skills-health-empty-{}", std::process::id()));
        std::fs::create_dir_all(root.join("agent")).unwrap();
        std::fs::create_dir_all(root.join("central")).unwrap();
        let mut ws = Workspace::open(&root.join("central")).unwrap();
        ws.config.agents = vec![AgentConfig {
            key: "sample".into(),
            name: "Sample".into(),
            skills_dir: root.join("agent").display().to_string(),
        }];
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
        let mut view = HealthView::default();
        view.refresh(&ctx);
        assert_eq!(view.issue_count(), 0);
        let mut terminal = Terminal::new(TestBackend::new(90, 20)).unwrap();
        terminal.draw(|f| view.draw(f, f.area(), &ctx)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("everything is healthy"));
        assert!(text.contains("0 skills · 1 agents checked"));
        std::fs::create_dir_all(root.join("agent/own-skill")).unwrap();
        let independent_snap = ws.scan().unwrap();
        let independent_ctx = Ctx {
            ws: &ws,
            snap: &independent_snap,
            settings: ctx.settings,
        };
        view.refresh(&independent_ctx);
        assert_eq!(view.issue_count(), 0);
        terminal
            .draw(|f| view.draw(f, f.area(), &independent_ctx))
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("[independent]"));
        assert!(!text.contains("everything is healthy"));

        std::fs::remove_dir_all(root).unwrap();
    }
}

/// A staged deployment-link repair review. Nothing is written until Apply.
#[derive(Default)]
pub struct RepairDialog {
    options: skills::ops::repair::Options,
    preview: Option<skills::ops::repair::RepairPlan>,
    resolve: Option<RepairResolve>,
    skipped: BTreeSet<String>,
    list: ListNav,
    focus: ChoiceFocus,
    rect: Rect,
    buttons: [Rect; 2],
}

#[derive(Default)]
struct RepairResolve {
    issue: usize,
    input: crate::tui::widgets::Input,
    shown: Vec<String>,
    list: ListNav,
}

impl RepairDialog {
    pub fn preview(
        options: skills::ops::repair::Options,
        report: skills::ops::repair::RepairPlan,
    ) -> Self {
        let mut dialog = Self {
            options,
            preview: Some(report),
            ..Default::default()
        };
        dialog.list.first(dialog.row_count());
        dialog
    }

    pub fn hints(&self) -> Hints {
        if self.resolve.is_some() {
            return &[
                ("type", "search Library"),
                ("↑↓", "choose"),
                ("Enter", "stage"),
                ("Esc", "back"),
            ];
        }
        if self.preview.is_some() {
            &[
                ("↑↓", "review"),
                ("Enter", "resolve"),
                ("Space", "stage / skip"),
                ("Tab", "Apply"),
                ("Esc", "cancel"),
            ]
        } else {
            &[("Enter", "analyze"), ("Esc", "cancel")]
        }
    }

    fn row_count(&self) -> usize {
        self.preview
            .as_ref()
            .map_or(1, |plan| plan.analysis.issues.len())
    }

    fn staged_plan(&self) -> Option<skills::ops::repair::RepairPlan> {
        let plan = self.preview.as_ref()?;
        let mut staged = plan.clone();
        staged
            .actions
            .retain(|action| !self.skipped.contains(action_id(action)));
        Some(staged)
    }

    fn primary(&mut self) -> Vec<Action> {
        if let Some(plan) = self.staged_plan() {
            if plan.actions.is_empty() {
                return vec![Action::CloseModal];
            }
            return vec![Action::CloseModal, Action::Spawn(Task::RepairApply(plan))];
        }
        vec![
            Action::CloseModal,
            Action::Spawn(Task::RepairPlan(self.options.clone())),
        ]
    }

    fn secondary(&mut self) -> Vec<Action> {
        if self.preview.take().is_some() {
            self.skipped.clear();
            self.focus = ChoiceFocus::List;
            self.list.first(1);
            vec![]
        } else {
            vec![Action::CloseModal]
        }
    }

    fn refilter_resolve(&mut self, ctx: &Ctx) {
        let Some(resolve) = &mut self.resolve else {
            return;
        };
        let query = resolve.input.value().to_lowercase();
        let Some(skills::ops::repair::RepairIssue::Broken { link_name, .. }) = self
            .preview
            .as_ref()
            .and_then(|plan| plan.analysis.issues.get(resolve.issue))
        else {
            return;
        };
        resolve.shown = ctx
            .snap
            .skills
            .iter()
            .filter(|skill| {
                skill.status.is_present()
                    && skill.deployment_name() == Some(link_name.as_str())
                    && (skill.key.to_lowercase().contains(&query)
                        || skill
                            .name
                            .as_deref()
                            .is_some_and(|name| name.contains(&query)))
            })
            .map(|skill| skill.key.clone())
            .collect();
        resolve.list.first(resolve.shown.len());
    }

    fn open_resolve(&mut self, ctx: &Ctx) {
        let Some(issue) = self
            .preview
            .as_ref()
            .and_then(|plan| plan.analysis.issues.get(self.list.selected()?))
        else {
            return;
        };
        if !matches!(
            issue,
            skills::ops::repair::RepairIssue::Broken { candidates, .. }
                if candidates.len() != 1
        ) {
            return;
        }
        self.resolve = Some(RepairResolve {
            issue: self.list.selected().unwrap_or(0),
            ..Default::default()
        });
        self.refilter_resolve(ctx);
    }

    fn accept_resolve(&mut self) -> bool {
        let Some(resolve) = &self.resolve else {
            return false;
        };
        let Some(chosen) = resolve
            .list
            .selected()
            .and_then(|index| resolve.shown.get(index))
            .cloned()
        else {
            return false;
        };
        let Some(issue) = self
            .preview
            .as_ref()
            .and_then(|plan| plan.analysis.issues.get(resolve.issue))
        else {
            return false;
        };
        self.options
            .deployments
            .insert(issue.id().to_string(), chosen);
        self.resolve = None;
        true
    }

    pub fn paste(&mut self, text: &str, ctx: &Ctx) -> Vec<Action> {
        let Some(resolve) = &mut self.resolve else {
            return vec![];
        };
        match resolve.input.paste(text) {
            Ok(true) => {
                self.refilter_resolve(ctx);
                vec![]
            }
            Ok(false) => vec![],
            Err(error) => vec![Action::Error(error.into())],
        }
    }

    pub fn key(&mut self, key: KeyEvent, ctx: &Ctx) -> Vec<Action> {
        if let Some(resolve) = &mut self.resolve {
            match key.code {
                KeyCode::Esc => self.resolve = None,
                KeyCode::Down => resolve.list.move_by(1, resolve.shown.len()),
                KeyCode::Up => resolve.list.move_by(-1, resolve.shown.len()),
                KeyCode::Enter => {
                    if self.accept_resolve() {
                        return vec![
                            Action::CloseModal,
                            Action::Spawn(Task::RepairPlan(self.options.clone())),
                        ];
                    }
                }
                _ if resolve.input.handle_key(key) => self.refilter_resolve(ctx),
                _ => {}
            }
            return vec![];
        }
        if key.code == KeyCode::Esc {
            return vec![Action::CloseModal];
        }
        let at_end = self.row_count() == 0
            || self.list.selected() == Some(self.row_count().saturating_sub(1));
        if let Some(event) = self.focus.key(key.code, at_end) {
            return match event {
                ChoiceEvent::Apply => self.primary(),
                ChoiceEvent::Cancel => self.secondary(),
                ChoiceEvent::Moved => vec![],
            };
        }
        match key.code {
            KeyCode::Down => self.list.move_by(1, self.row_count()),
            KeyCode::Up => self.list.move_by(-1, self.row_count()),
            KeyCode::PageDown => self.list.move_by(10, self.row_count()),
            KeyCode::PageUp => self.list.move_by(-10, self.row_count()),
            KeyCode::Home => self.list.first(self.row_count()),
            KeyCode::End => self.list.last(self.row_count()),
            KeyCode::Enter if self.preview.is_none() => return self.primary(),
            KeyCode::Enter if self.focus == ChoiceFocus::List => self.open_resolve(ctx),
            KeyCode::Char(' ') if self.preview.is_some() => {
                if let Some(id) = self
                    .preview
                    .as_ref()
                    .and_then(|plan| plan.analysis.issues.get(self.list.selected()?))
                    .map(|issue| issue.id().to_string())
                    && !self.skipped.remove(&id)
                {
                    self.skipped.insert(id);
                }
            }
            KeyCode::Enter => self.focus = ChoiceFocus::Apply,
            _ => {}
        }
        vec![]
    }

    pub fn mouse(&mut self, mouse: MouseEvent) -> Vec<Action> {
        let position = ratatui::layout::Position::new(mouse.column, mouse.row);
        match mouse.kind {
            MouseEventKind::ScrollDown => self.list.move_by(3, self.row_count()),
            MouseEventKind::ScrollUp => self.list.move_by(-3, self.row_count()),
            MouseEventKind::Down(MouseButton::Left) if !self.rect.contains(position) => {
                return vec![Action::CloseModal];
            }
            MouseEventKind::Down(MouseButton::Left) if self.buttons[0].contains(position) => {
                return self.primary();
            }
            MouseEventKind::Down(MouseButton::Left) if self.buttons[1].contains(position) => {
                return self.secondary();
            }
            MouseEventKind::Down(MouseButton::Left)
                if self.list.click(mouse.row, self.row_count()).is_some() =>
            {
                self.focus = ChoiceFocus::List;
            }
            _ => {}
        }
        vec![]
    }

    pub fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) {
        let preview = self.preview.is_some();
        let area = centered(
            area,
            area.width.saturating_sub(8).min(112),
            if preview {
                area.height.saturating_sub(4).min(56)
            } else {
                15.min(area.height.saturating_sub(4))
            },
        );
        self.rect = area;
        frame.render_widget(crate::tui::widgets::OverlayClear, area);
        let theme = &ctx.settings.theme;
        let block = theme.block(
            if preview {
                " Repair deployments · preview "
            } else {
                " Repair deployments "
            },
            true,
        );
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if inner.height < 6 || inner.width < 30 {
            frame.render_widget(Paragraph::new("Enlarge terminal · Esc cancel"), inner);
            return;
        }
        let footer_height = 2.min(inner.height.saturating_sub(2));
        let summary_area = Rect::new(inner.x + 1, inner.y, inner.width.saturating_sub(2), 2);
        self.list.rows = Rect::new(
            inner.x + 1,
            inner.y + 2,
            inner.width.saturating_sub(2),
            inner.height.saturating_sub(2 + footer_height),
        );
        self.list.item_height = 3;
        self.list.clamp(self.row_count());
        let (items, summary, primary, secondary, enabled) = if let Some(plan) = &self.preview {
            let items = plan
                .analysis
                .issues
                .iter()
                .map(|issue| {
                    let staged = plan
                        .actions
                        .iter()
                        .any(|action| action_id(action) == issue.id())
                        && !self.skipped.contains(issue.id());
                    let (label, style) = if staged {
                        ("STAGED", theme.ok())
                    } else {
                        ("UNRESOLVED", theme.warn())
                    };
                    let detail = match issue {
                        skills::ops::repair::RepairIssue::Broken {
                            link_name,
                            target,
                            candidates,
                            ..
                        } => {
                            if let Some(action) = plan
                                .actions
                                .iter()
                                .find(|action| action_id(action) == issue.id())
                            {
                                let (skill, library_target) = action_target(action);
                                format!(
                                    "broken: {} · target: {} · {}",
                                    target.display(),
                                    skill,
                                    candidates
                                        .iter()
                                        .find(|candidate| candidate.key == skill)
                                        .map(|candidate| candidate.path.display().to_string())
                                        .unwrap_or_else(|| library_target.display().to_string())
                                )
                            } else if candidates.is_empty() {
                                format!(
                                    "broken: {} · no Library skill named {link_name}",
                                    target.display()
                                )
                            } else {
                                format!(
                                    "broken: {} · {} Library matches; Enter to choose",
                                    target.display(),
                                    candidates.len()
                                )
                            }
                        }
                        skills::ops::repair::RepairIssue::OutdatedName {
                            link_name,
                            name,
                            skill,
                            blocked,
                            ..
                        } => blocked
                            .clone()
                            .unwrap_or_else(|| format!("{link_name} → {name} · {skill}")),
                    };
                    ListItem::new(vec![
                        Line::from(vec![
                            Span::styled(format!("{label:<11}"), style),
                            Span::styled(issue.id(), theme.bold()),
                        ]),
                        Line::from(Span::styled(
                            format!("  {}", fit(&detail, inner.width.saturating_sub(4) as usize)),
                            theme.description(),
                        )),
                        Line::default(),
                    ])
                })
                .collect::<Vec<_>>();
            let staged = plan
                .actions
                .iter()
                .filter(|action| !self.skipped.contains(action_id(action)))
                .count();
            (
                items,
                format!(
                    "{staged} staged · {} unresolved",
                    plan.analysis.issues.len().saturating_sub(staged)
                ),
                if staged > 0 { "✓ Apply" } else { "Done" },
                "Back",
                true,
            )
        } else {
            (
                vec![ListItem::new(vec![
                    Line::from(
                        "Find broken deployments and deployments using an outdated skill name.",
                    ),
                    Line::from(Span::styled(
                        "Only independent, managed Agent directories are scanned.",
                        theme.description(),
                    )),
                ])],
                "Deployment links".into(),
                "Analyze",
                "Cancel",
                true,
            )
        };
        frame.render_widget(
            Paragraph::new(fit(&summary, summary_area.width as usize)).style(theme.dim()),
            summary_area,
        );
        frame.render_stateful_widget(
            List::new(items).highlight_style(if self.focus == ChoiceFocus::List {
                theme.selected()
            } else {
                theme.selected_unfocused()
            }),
            self.list.rows,
            &mut self.list.state,
        );
        self.buttons = choice_footer::draw_with_labels(
            frame,
            Rect::new(
                inner.x,
                inner.bottom().saturating_sub(footer_height),
                inner.width,
                footer_height,
            ),
            self.focus,
            enabled,
            if preview {
                "Nothing is written until Apply"
            } else {
                "Enter analyzes current managed Agent directories"
            },
            (primary, secondary),
            theme,
        );
        if let Some(resolve) = &mut self.resolve {
            let picker = centered(area, 72, 18);
            frame.render_widget(crate::tui::widgets::OverlayClear, picker);
            let block = theme.block(" Choose Library skill ", true);
            let inside = block.inner(picker);
            frame.render_widget(block, picker);
            frame.render_widget(
                Paragraph::new(format!("Search: {}", resolve.input.value())).style(theme.accent()),
                Rect::new(inside.x + 1, inside.y, inside.width.saturating_sub(2), 1),
            );
            resolve.list.rows = Rect::new(
                inside.x + 1,
                inside.y + 2,
                inside.width.saturating_sub(2),
                inside.height.saturating_sub(3),
            );
            let rows = resolve
                .shown
                .iter()
                .map(|key| ListItem::new(key.clone()))
                .collect::<Vec<_>>();
            frame.render_stateful_widget(
                List::new(rows)
                    .highlight_style(theme.selected())
                    .highlight_symbol("▸ "),
                resolve.list.rows,
                &mut resolve.list.state,
            );
        }
    }
}

fn action_id(action: &skills::ops::repair::RepairAction) -> &str {
    match action {
        skills::ops::repair::RepairAction::Relink { id, .. }
        | skills::ops::repair::RepairAction::Rename { id, .. } => id,
    }
}

fn action_target(action: &skills::ops::repair::RepairAction) -> (&str, &std::path::Path) {
    match action {
        skills::ops::repair::RepairAction::Relink { skill, target, .. }
        | skills::ops::repair::RepairAction::Rename { skill, target, .. } => {
            (skill, target.as_path())
        }
    }
}

#[cfg(test)]
mod deployment_repair_dialog_tests {
    use super::*;
    use skills::ops::repair::{Candidate, RepairAction, RepairAnalysis, RepairIssue, RepairPlan};
    use std::path::PathBuf;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::from(code)
    }

    fn plan(candidates: Vec<Candidate>) -> RepairPlan {
        let id = "test/shared".to_string();
        let issue = RepairIssue::Broken {
            id: id.clone(),
            agent: "test".into(),
            link_name: "shared".into(),
            target: PathBuf::from("/gone/shared"),
            candidates,
        };
        let actions = match &issue {
            RepairIssue::Broken { candidates, .. } if candidates.len() == 1 => {
                vec![RepairAction::Relink {
                    id,
                    agent: "test".into(),
                    path: PathBuf::from("/agent/shared"),
                    old_target: PathBuf::from("/gone/shared"),
                    skill: candidates[0].key.clone(),
                    target: candidates[0].path.clone(),
                    name: "shared".into(),
                    parent_identity: (1, 1),
                    link_identity: (2, 2),
                }]
            }
            _ => vec![],
        };
        RepairPlan {
            analysis: RepairAnalysis {
                ready: usize::from(actions.len() == 1),
                unresolved: usize::from(actions.is_empty()),
                issues: vec![issue],
            },
            actions,
        }
    }

    #[test]
    fn staged_repair_can_be_skipped_without_applying() {
        let mut dialog = RepairDialog::preview(
            Default::default(),
            plan(vec![Candidate {
                key: "repos/example/folder".into(),
                name: "shared".into(),
                path: PathBuf::from("/library/repos/example/folder"),
            }]),
        );
        assert_eq!(dialog.staged_plan().unwrap().actions.len(), 1);
        dialog.key(key(KeyCode::Char(' ')), &test_ctx());
        assert!(dialog.staged_plan().unwrap().actions.is_empty());
    }

    #[test]
    fn duplicate_name_opens_library_key_resolver() {
        let candidates = ["repos/one/folder", "repos/two/folder"]
            .into_iter()
            .map(|key| Candidate {
                key: key.into(),
                name: "shared".into(),
                path: PathBuf::from("/library").join(key),
            })
            .collect();
        let mut dialog = RepairDialog::preview(Default::default(), plan(candidates));
        let ctx = test_ctx();
        dialog.key(key(KeyCode::Enter), &ctx);
        assert!(dialog.resolve.is_some());
    }

    fn test_ctx() -> Ctx<'static> {
        let tmp = Box::leak(Box::new(
            skills::ops::DownloadDir::new("repair-dialog").unwrap(),
        ));
        let root = tmp.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        let ws = Box::leak(Box::new(skills::Workspace::open(&root).unwrap()));
        let snap = Box::leak(Box::new(ws.scan().unwrap()));
        let settings = Box::leak(Box::new(crate::tui::settings::RuntimeSettings::new(
            &ws.config,
        )));
        Ctx { ws, snap, settings }
    }
}
