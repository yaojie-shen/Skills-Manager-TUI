//! Health tab: everything that is not managed/unmanaged, plus update checks.
//!
//! The list on the left says *that* something is wrong; the pane on the right
//! says *why* the selected item is here and which keys can do something about
//! it. The footer shows only those keys, so the user never has to guess which
//! of `a`, `m`, `x` and `U` fits the row under the cursor.

use super::preview::{Overlay, kv};
use super::{View, split_panes, status_glyph, status_text, wheel};
use crate::tui::app::{Action, Ctx, Hints, Tab};
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
use skills::reconcile::{AgentDirMode, EntryState, SkillRecord, SkillStatus};
use std::collections::BTreeMap;

/// Which of the page's actions apply to one row. Decided once per rebuild
/// from the record and its check result, so both the key handler and the
/// footer read the same answer.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Caps {
    accept: bool,
    migrate: bool,
    clean: bool,
    update: bool,
}

impl Caps {
    fn of(r: &SkillRecord, check: Option<&Result<CheckResult, String>>) -> Self {
        let git = matches!(r.source, Some(Source::Git { .. }));
        let update_available = matches!(check, Some(Ok(c)) if c.update_available);
        Caps {
            accept: matches!(
                r.status,
                SkillStatus::Modified | SkillStatus::Managed { no_baseline: true }
            ),
            migrate: matches!(r.status, SkillStatus::Renamed { .. }),
            clean: matches!(r.status, SkillStatus::Missing | SkillStatus::Invalid { .. }),
            // `update::prepare` refuses anything but managed and modified, so
            // offering `U` for a missing or invalid skill would only produce
            // an error toast.
            update: git
                && update_available
                && matches!(
                    r.status,
                    SkillStatus::Managed { .. } | SkillStatus::Modified
                ),
        }
    }
}

struct Row {
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
    pub fn input_focused(&self) -> bool {
        self.filter.editing
    }
    pub fn paste(&mut self, text: &str, ctx: &Ctx) -> Vec<Action> {
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
                    "Agent directory does not exist. Deploy or sync from Agents to create it."
                        .to_string(),
                ),
                AgentDirMode::DirForeign { target } => Some(format!(
                    "Agent directory links outside the root → {}. Review this path before changing it.",
                    target.display()
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
                    key: name.clone(),
                    caps: Caps::default(),
                    agent: Some(agent.key.clone()),
                    state: Some(state.clone()),
                    heading: None,
                    explanation: None,
                });
            }
        }
        self.total_issues = self.issue_count();
        self.rows.retain(|row| {
            row.heading.is_some()
                || self.filter.matches(&format!(
                    "{} {} {} {}",
                    row.key,
                    row.agent.as_deref().unwrap_or(""),
                    row.explanation.as_deref().unwrap_or(""),
                    ctx.snap
                        .get(&row.key)
                        .map(|r| status_text(&r.status))
                        .unwrap_or_default()
                ))
        });
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
        self.rows.iter().filter(|r| r.heading.is_none()).count()
    }

    fn agent_action(&self, code: KeyCode, ctx: &Ctx) -> Option<Vec<Action>> {
        let row = self.selected_row()?;
        let agent = row.agent.as_ref()?;
        let actions = match (&row.state, code) {
            (Some(EntryState::Foreign { .. }), KeyCode::Char('x') | KeyCode::Char('a')) => {
                use skills::ops::agent_links::{self, Repair};
                let operation = if code == KeyCode::Char('a') {
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
                return Some(vec![Action::OpenModal(Box::new(Modal::confirm_write(
                    format!("repair {agent}/{}", row.key),
                    lines,
                    Box::new(move |ws| plan.apply(ws)),
                )))]);
            }
            (Some(EntryState::Broken { .. }), KeyCode::Char('x') | KeyCode::Enter) => {
                deploy::plan_clean(ctx.ws, ctx.snap, agent, std::slice::from_ref(&row.key))
            }
            (
                Some(EntryState::Shadow { same_content: true }),
                KeyCode::Char('r') | KeyCode::Enter,
            ) => deploy::plan_relink(ctx.ws, ctx.snap, agent, std::slice::from_ref(&row.key)),
            (Some(EntryState::AgentOnly), KeyCode::Char('a') | KeyCode::Enter) => {
                let report = ctx.snap.agent(agent)?;
                return Some(vec![Action::OpenModal(Box::new(Modal::adopt(
                    agent,
                    &row.key,
                    report.skills_dir.join(&row.key),
                )))]);
            }
            (_, KeyCode::Enter) => return Some(vec![Action::SwitchTab(Tab::Agents)]),
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
    fn detail_lines(&self, r: &SkillRecord, caps: Caps, ctx: &Ctx) -> Vec<Line<'static>> {
        let th = ctx.theme;
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
                if let Some(Source::Git { revision, .. }) = &r.source {
                    lines.push(kv(
                        "installed",
                        revision.as_deref().map(short_rev).unwrap_or("-"),
                        th,
                    ));
                }
                actions.push(action(
                    "a",
                    "accept the current content as the new baseline".into(),
                ));
            }
            SkillStatus::Managed { no_baseline: true } => {
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
            SkillStatus::Managed { no_baseline: false } | SkillStatus::Unmanaged => {
                lines.push(text(
                    "The skill itself is fine; it is listed because of the last update check."
                        .into(),
                ));
                source_line(&mut lines);
            }
            SkillStatus::Missing => {
                lines.push(text(format!(
                    "There is a metadata file for this name but no directory at {}. The \
                     metadata is kept until you decide what to do with it.",
                    r.path.display()
                )));
                lines.push(kv("meta", ctx.ws.meta.path(&key).display().to_string(), th));
                meta_lines(&mut lines);
                actions.push(action(
                    "x",
                    "forget it: delete the metadata file, tags and note included".into(),
                ));
                match &r.source {
                    Some(Source::Git { .. }) => actions.push(action(
                        "",
                        "or reinstall it from the Library tab with i, from the git source above"
                            .into(),
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
                     unmanaged directory \"{to}\", and neither hash matches anything else, so \
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
                    "m",
                    format!("migrate: move the metadata to \"{to}\""),
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
                    "The metadata file could not be parsed, so the skill is treated as \
                     unmanaged. The file is never overwritten automatically; fix or delete {} \
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
                        "update from upstream; you choose local or upstream file by file"
                    } else {
                        "update to the upstream revision"
                    };
                    actions.push(action("U", what.into()));
                } else {
                    actions.push(action(
                        "",
                        format!(
                            "updating needs the skill present and managed; it is {}",
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
        let th = ctx.theme;
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
    fn refresh(&mut self, ctx: &Ctx) {
        self.rebuild(ctx);
    }

    fn handle_key(&mut self, k: KeyEvent, ctx: &Ctx) -> Vec<Action> {
        if self.filter.key(k) {
            self.rebuild(ctx);
            return vec![];
        }
        if self.preview.handle_key(k) {
            return vec![];
        }
        if let Some(actions) = self.agent_action(k.code, ctx) {
            return actions;
        }
        let caps = self.selected_row().map(|r| r.caps).unwrap_or_default();
        match k.code {
            KeyCode::Char('M') => self.select_skills(None),
            KeyCode::Char('q') => vec![Action::SwitchTab(Tab::Search)],
            // Esc means "back" everywhere else in the program, so here it goes
            // back to the search page rather than out of the door.
            KeyCode::Esc => vec![Action::SwitchTab(Tab::Search)],
            KeyCode::Down | KeyCode::Char('j') => {
                self.select_by(1);
                vec![]
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.select_by(-1);
                vec![]
            }
            KeyCode::Enter => self.open_selected(ctx),
            KeyCode::Char('c') => {
                let keys: Vec<String> = ctx
                    .snap
                    .skills
                    .iter()
                    .filter(|s| matches!(s.source, Some(Source::Git { .. })))
                    .map(|s| s.key.clone())
                    .collect();
                if keys.is_empty() {
                    vec![Action::Error("no git-sourced skills to check".into())]
                } else {
                    vec![
                        Action::Toast(format!("checking {} skill(s)…", keys.len())),
                        Action::Spawn(Task::Check(keys)),
                    ]
                }
            }
            // The guards below mirror `Caps::of`: a key the footer does not
            // show is simply ignored, never answered with an error toast.
            KeyCode::Char('U') if caps.update => match self.selected(ctx) {
                Some(r) => vec![
                    Action::Spawn(Task::Prepare(r.key.clone())),
                    Action::Toast(format!("fetching {}…", r.key)),
                ],
                None => vec![],
            },
            KeyCode::Char('a') if caps.accept => match self.selected(ctx) {
                Some(r) => {
                    let key = r.key.clone();
                    vec![Action::Write(Box::new(move |ws| {
                        edit::accept(ws, &key).map(|_| format!("baseline updated for {key}"))
                    }))]
                }
                None => vec![],
            },
            KeyCode::Char('m') if caps.migrate => match self.selected(ctx) {
                Some(r) => match &r.status {
                    SkillStatus::Renamed { to } => {
                        let (old, new) = (r.key.clone(), to.clone());
                        vec![Action::Write(Box::new(move |ws| {
                            edit::migrate_meta(ws, &old, &new)
                                .map(|_| format!("metadata moved {old} → {new}"))
                        }))]
                    }
                    _ => vec![],
                },
                None => vec![],
            },
            // Clean up an entry that is not a working skill. What that means
            // depends on which half is missing: the directory or the files in it.
            KeyCode::Char('x') if caps.clean => match self
                .selected(ctx)
                .map(|r| (r.key.clone(), r.status.clone()))
            {
                Some((key, SkillStatus::Missing)) => {
                    vec![Action::OpenModal(Box::new(Modal::forget_missing(&key)))]
                }
                Some((key, SkillStatus::Invalid { reason })) => {
                    vec![Action::OpenModal(Box::new(Modal::discard_invalid(
                        &key, &reason,
                    )))]
                }
                _ => vec![],
            },
            _ => vec![],
        }
    }

    fn handle_mouse(&mut self, m: MouseEvent, ctx: &Ctx) -> Vec<Action> {
        if self.preview.handle_mouse(m) {
            return vec![];
        }
        let at = (m.column, m.row).into();
        if m.kind == MouseEventKind::Down(MouseButton::Left) && self.filter.rect.contains(at) {
            self.filter.editing = true;
            return vec![];
        }
        if let Some(d) = wheel(&m) {
            if self.left.contains(at) {
                self.select_by(d);
            } else if self.right.contains(at) {
                let max = (self.detail_rows as i32 - self.detail_height as i32).max(0);
                self.detail_scroll = (self.detail_scroll as i32 + d).clamp(0, max) as u16;
            }
            return vec![];
        }
        if let MouseEventKind::Down(MouseButton::Left) = m.kind
            && self.left.contains(at)
            && let Some((_, double)) = self.list.click(m.row, self.rows.len())
        {
            self.detail_scroll = 0;
            if double {
                return self.open_selected(ctx);
            }
        }
        vec![]
    }

    fn draw(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        let area = self.filter.draw(f, area, "Filter health issues", ctx);
        let th = ctx.theme;
        // With nothing to show there is nothing to explain either, so the
        // message gets the whole width instead of being squeezed beside an
        // empty pane.
        if self.issue_count() == 0 {
            self.left = area;
            self.right = Rect::default();
            let block = th.block(" health ", true);
            let inner = block.inner(area);
            f.render_widget(block, area);
            let msg = if self.total_issues > 0 {
                format!("No matching issues · {} issues in total", self.total_issues)
            } else {
                format!(
                    "everything is healthy\n{} skills · {} agents checked\nPress c to check git sources for updates.",
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
        let (left, right) = split_panes(area, 45);
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
                            &format!("{} {}  {description}", agent_marker(row), row.key),
                            left.width.saturating_sub(4) as usize,
                        ),
                        th.warn(),
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
        let title = format!(" health · {} issues ", self.issue_count());
        self.list.set_area_from_block(left);
        let list = List::new(items)
            .block(th.block(title, true))
            .highlight_style(th.selected())
            .highlight_symbol("▸ ");
        f.render_stateful_widget(list, left, &mut self.list.state);
        self.draw_detail(f, right, ctx);
        self.preview.draw(f, area, ctx);
    }

    /// Only the keys that do something for the selected row. The statuses
    /// are mutually exclusive, so at most one of `a`, `m`, `x` applies, with
    /// or without `U`.
    fn hints(&self) -> Hints {
        if self.filter.editing {
            return &[("Enter/↓", "issues"), ("Esc", "finish filter")];
        }
        if let Some(hints) = self.preview.hints() {
            return hints;
        }
        if let Some(row) = self.selected_row() {
            if row.heading.is_some() {
                return &[("c", "check updates"), ("Esc/q", "library")];
            }
            if row.agent.is_some() {
                return match row.state {
                    Some(EntryState::Foreign { .. }) => &[
                        ("x", "remove link"),
                        ("a", "adopt copy"),
                        ("Enter", "agents"),
                        ("c", "check updates"),
                        ("Esc/q", "library"),
                    ],
                    Some(EntryState::Broken { .. }) => &[
                        ("x", "remove link"),
                        ("Enter", "act"),
                        ("c", "check updates"),
                        ("Esc/q", "library"),
                    ],
                    Some(EntryState::Shadow { same_content: true }) => &[
                        ("r", "relink"),
                        ("Enter", "act"),
                        ("c", "check updates"),
                        ("Esc/q", "library"),
                    ],
                    Some(EntryState::AgentOnly) => &[
                        ("a", "adopt"),
                        ("Enter", "act"),
                        ("c", "check updates"),
                        ("Esc/q", "library"),
                    ],
                    _ => &[
                        ("Enter", "agents"),
                        ("c", "check updates"),
                        ("Esc/q", "library"),
                    ],
                };
            }
        }
        let Some(caps) = self.selected_row().map(|r| r.caps) else {
            return &[("c", "check updates"), ("Esc", "library"), ("q", "library")];
        };
        match (caps.update, caps.accept, caps.migrate, caps.clean) {
            (true, true, _, _) => &[
                ("c", "check updates"),
                ("U", "update"),
                ("a", "accept"),
                ("Enter", "open"),
                ("M", "multi-select"),
                ("Esc", "library"),
                ("q", "library"),
            ],
            (true, false, true, _) => &[
                ("c", "check updates"),
                ("U", "update"),
                ("m", "migrate"),
                ("Enter", "open"),
                ("M", "multi-select"),
                ("Esc", "library"),
                ("q", "library"),
            ],
            (true, false, false, true) => &[
                ("c", "check updates"),
                ("U", "update"),
                ("x", "clean up"),
                ("Enter", "open"),
                ("M", "multi-select"),
                ("Esc", "library"),
                ("q", "library"),
            ],
            (true, false, false, false) => &[
                ("c", "check updates"),
                ("U", "update"),
                ("Enter", "open"),
                ("M", "multi-select"),
                ("Esc", "library"),
                ("q", "library"),
            ],
            (false, true, _, _) => &[
                ("c", "check updates"),
                ("a", "accept"),
                ("Enter", "open"),
                ("M", "multi-select"),
                ("Esc", "library"),
                ("q", "library"),
            ],
            (false, false, true, _) => &[
                ("c", "check updates"),
                ("m", "migrate"),
                ("Enter", "open"),
                ("M", "multi-select"),
                ("Esc", "library"),
                ("q", "library"),
            ],
            (false, false, false, true) => &[
                ("c", "check updates"),
                ("x", "clean up"),
                ("Enter", "open"),
                ("M", "multi-select"),
                ("Esc", "library"),
                ("q", "library"),
            ],
            (false, false, false, false) => &[
                ("c", "check updates"),
                ("Enter", "open"),
                ("M", "multi-select"),
                ("Esc", "library"),
                ("q", "library"),
            ],
        }
    }
}

impl Row {
    fn heading(text: String) -> Self {
        Self {
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
            theme: &theme,
        };
        let mut view = HealthView::default();
        view.refresh(&ctx);
        assert!(
            snap.get("repos").is_none(),
            "repos is a storage container, not an invalid skill"
        );
        assert_eq!(view.issue_count(), 4);
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
        assert!(text.contains("health · 4 issues"));
        for name in ["invalid-item", "broken-item", "foreign-item", "printer"] {
            assert!(text.contains(name), "missing {name}");
        }
        assert!(!text.contains("everything is healthy"));
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
            [Action::SwitchTab(Tab::Search)]
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
            theme: &theme,
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
        std::fs::remove_dir_all(root).unwrap();
    }
}
