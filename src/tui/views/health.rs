//! Health tab: skill problems and update checks.
//!
//! The list on the left says *that* something is wrong; the pane on the right
//! says *why* the selected item is here and what can be done about it. The
//! footer shows only the applicable keys, so the user never has to guess which
//! repair fits the row under the cursor.
//! Replacement-skill filtering preserves the eligible choices' bounded viewport.

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
    verifying: bool,
    verification_error: Option<String>,
    filter: super::filter::Filter,
    total_issues: usize,
    class_counts: [usize; 3],
    formatted_width: u16,
    formatted_rows: Vec<ListItem<'static>>,
    #[cfg(test)]
    draw_all_rows: bool,
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
    pub fn set_verifying(&mut self, verifying: bool) {
        self.verifying = verifying;
    }

    pub fn set_verification_error(&mut self, error: Option<String>) {
        self.verification_error = error;
    }

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
        self.class_counts = [
            self.rows
                .iter()
                .filter(|r| r.class == Some(HealthClass::Fault))
                .count(),
            self.rows
                .iter()
                .filter(|r| r.class == Some(HealthClass::Review))
                .count(),
            self.rows
                .iter()
                .filter(|r| r.class == Some(HealthClass::Independent))
                .count(),
        ];
        if self.formatted_width > 0 {
            self.format_rows(ctx, self.formatted_width);
        }
    }

    fn format_rows(&mut self, ctx: &Ctx, width: u16) {
        let th = &ctx.settings.theme;
        self.formatted_width = width;
        self.formatted_rows = self
            .rows
            .iter()
            .map(|row| {
                if let Some(heading) = &row.heading {
                    return ListItem::new(Line::from(Span::styled(
                        fit(heading, width.saturating_sub(4) as usize),
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
                            width.saturating_sub(4) as usize,
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
                    modal.destructive("Remove").deployment_only()
                } else {
                    modal.apply_label("Adopt")
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
    /// which actions apply. Nothing here is guessed; where the record cannot
    /// answer a question, the text says so.
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
        let action = |what: String| Line::from(Span::raw(what));
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
            SkillStatus::Unchecked => {
                lines.push(text(
                    "Local content has not been verified against its recorded baseline. This is \
                     normally deferred while loading the Library; if verification leaves it \
                     unchecked, a file could not be read. Run Verify local content again after \
                     checking file access."
                        .into(),
                ));
                source_line(&mut lines);
            }
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
                    "Accept the current content as the new baseline.".into(),
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
                    "Accept the current content hash as the baseline.".into(),
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
                        "Reinstall it from the source above in the Library tab.".into(),
                    )),
                    Some(Source::Local { path: Some(p) }) => {
                        actions.push(action(format!("Reinstall it from {p} in the Library tab.")))
                    }
                    Some(Source::Local { path: None }) => actions.push(action(
                        "the local source has no path recorded, so the tool cannot reinstall it"
                            .into(),
                    )),
                    None => actions.push(action(
                        "No source is recorded, so the tool cannot reinstall it.".into(),
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
                    "Discard the invalid directory and its metadata.".into(),
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
                    actions.push(action(format!("Update: {what}.")));
                } else {
                    actions.push(action(format!(
                        "Updating requires an existing skill with upstream information; it is {}.",
                        r.status.label()
                    )));
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
        lines.push(action("Open the skill in a window over this page.".into()));
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
    fn preview_overlay(&mut self) -> Option<&mut super::preview::Overlay> {
        Some(&mut self.preview)
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
        let help = if self.verifying {
            "Checking local content… · Filter health entries".to_string()
        } else if let Some(error) = &self.verification_error {
            format!("Verification failed: {error} · Filter health entries")
        } else {
            "Filter health entries".to_string()
        };
        let area = self.filter.draw(
            f,
            area,
            &help,
            &format!(
                "health · {} faults · {} review · {} independent{}",
                self.class_counts[0],
                self.class_counts[1],
                self.class_counts[2],
                if self.verifying {
                    " · checking local content…"
                } else if self.verification_error.is_some() {
                    " · verification failed"
                } else {
                    ""
                }
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
                    "everything is healthy\n{} skills · {} agents checked\nRemote sources have not been checked for updates.",
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
        if self.formatted_width != left.width {
            self.format_rows(ctx, left.width);
        }
        self.list.rows = left;
        #[cfg(test)]
        if self.draw_all_rows {
            self.list.rows = left;
            let list = List::new(self.formatted_rows.clone())
                .highlight_style(th.selected())
                .highlight_symbol("▸ ");
            f.render_stateful_widget(list, left, &mut self.list.state);
            self.draw_detail(f, right, ctx);
            self.preview.draw(f, area, ctx);
            return;
        }
        let height = left.height as usize;
        let selected = self.list.selected();
        let mut offset = self.list.state.offset();
        if height == 0 {
            return;
        }
        if let Some(selected) = selected {
            if selected < offset {
                offset = selected;
            } else if selected >= offset + height {
                offset = selected + 1 - height;
            }
        }
        *self.list.state.offset_mut() = offset;
        let end = (offset + height).min(self.formatted_rows.len());
        let mut visible = self
            .list
            .state
            .with_selected(selected.map(|selected| selected - offset));
        *visible.offset_mut() = 0;
        let list = List::new(self.formatted_rows[offset..end].to_vec())
            .highlight_style(th.selected())
            .highlight_symbol("▸ ");
        f.render_stateful_widget(list, left, &mut visible);
        *self.list.state.offset_mut() = offset + visible.offset();
        self.draw_detail(f, right, ctx);
        self.preview.draw(f, area, ctx);
    }

    fn hints(&self) -> Hints {
        if let Some(hints) = self.preview.hints() {
            return hints;
        }
        if self.filter.editing {
            return &[("Enter/↓", "issues"), ("Esc", "clear filter")];
        }
        if self.selected_row().is_none_or(|row| row.heading.is_some()) {
            return &[
                ("c", "check updates"),
                ("/", "filter"),
                ("Esc/q", "clear/back"),
            ];
        }
        let row = self.selected_row().expect("selected issue checked above");
        if row.agent.is_some() {
            return match row.state {
                Some(EntryState::Broken { .. } | EntryState::Foreign { .. }) => &[
                    ("Enter", "open / resolve"),
                    ("x", "remove link"),
                    ("/", "filter"),
                    ("c", "check updates"),
                    ("Esc/q", "clear/back"),
                ],
                Some(EntryState::Shadow { same_content: true }) => &[
                    ("Enter", "open / resolve"),
                    ("r", "relink"),
                    ("/", "filter"),
                    ("c", "check updates"),
                    ("Esc/q", "clear/back"),
                ],
                _ => &[
                    ("Enter", "open / resolve"),
                    ("/", "filter"),
                    ("c", "check updates"),
                    ("Esc/q", "clear/back"),
                ],
            };
        }
        if row.caps.clean {
            return &[
                ("Enter", "open"),
                ("x", "discard invalid"),
                ("/", "filter"),
                ("c", "check updates"),
                ("Esc/q", "clear/back"),
            ];
        }
        if row.caps.update {
            return &[
                ("Enter", "open"),
                ("U", "update repo"),
                ("/", "filter"),
                ("c", "check updates"),
                ("Esc/q", "clear/back"),
            ];
        }
        &[
            ("Enter", "open"),
            ("/", "filter"),
            ("c", "check updates"),
            ("Esc/q", "clear/back"),
        ]
    }
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
            "Remove this broken link after confirmation.",
        ),
        Some(EntryState::Foreign { target }) => (
            format!("links outside the root → {}", target.display()),
            "Remove only the symlink, or adopt a valid skill into the root and repoint this link. Both require confirmation and preserve the external target.",
        ),
        Some(EntryState::Shadow { same_content: true }) => (
            "the agent's own copy, same content as root".into(),
            "Replace the matching copy with a root link after confirmation.",
        ),
        Some(EntryState::Shadow {
            same_content: false,
        }) => (
            "the agent's own copy differs from root".into(),
            "Review both copies manually; relink is disabled to preserve different content.",
        ),
        Some(EntryState::AgentOnly) => (
            "the agent's own directory, absent from root".into(),
            "Adopt into the root after confirmation; only valid skills can be adopted.",
        ),
        _ => (
            row.explanation.clone().unwrap_or_default(),
            "Review agent configuration in Agents.",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::theme::Theme;
    use ratatui::{Terminal, backend::TestBackend};
    use skills::{Workspace, config::AgentConfig};

    fn render(view: &mut HealthView, ctx: &Ctx, width: u16, height: u16) -> (String, usize, usize) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| view.draw(f, f.area(), ctx)).unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        (
            text,
            view.list.state.offset(),
            view.list.selected().unwrap_or_default(),
        )
    }

    fn assert_virtual_render_matches_full(
        view: &mut HealthView,
        ctx: &Ctx,
        width: u16,
        height: u16,
    ) {
        view.draw_all_rows = true;
        let expected = render(view, ctx, width, height);
        view.draw_all_rows = false;
        let actual = render(view, ctx, width, height);
        assert_eq!(actual, expected);
    }

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
    fn detail_actions_use_names_and_footer_hints_follow_capabilities() {
        let root = skills::ops::DownloadDir::new("health-detail-actions").unwrap();
        std::fs::create_dir_all(root.path().join("invalid-item")).unwrap();
        let ws = Workspace::open(root.path()).unwrap();
        let snap = ws.scan().unwrap();
        let settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &settings,
        };
        let record = snap.get("invalid-item").unwrap();
        let mut view = HealthView::default();
        let detail = view
            .detail_lines(record, Caps::of(record, None), &ctx)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(detail.contains("Discard the invalid directory and its metadata."));
        assert!(detail.contains("Open the skill in a window over this page."));
        for local_legend in ["x      ", "U      ", "a      ", "Enter  "] {
            assert!(
                !detail.contains(local_legend),
                "detail pane must not contain a local key column: {detail:?}"
            );
        }

        view.rows = vec![Row {
            class: Some(HealthClass::Fault),
            key: record.key.clone(),
            caps: Caps {
                clean: true,
                ..Caps::default()
            },
            agent: None,
            state: None,
            heading: None,
            explanation: None,
        }];
        view.list.select(Some(0));
        assert_eq!(
            view.hints(),
            &[
                ("Enter", "open"),
                ("x", "discard invalid"),
                ("/", "filter"),
                ("c", "check updates"),
                ("Esc/q", "clear/back"),
            ]
        );

        view.rows[0].caps = Caps {
            accept: true,
            ..Caps::default()
        };
        assert!(
            view.hints().iter().all(|(key, _)| *key != "a"),
            "accept is reached through the App-owned Actions menu"
        );

        view.rows[0].caps = Caps {
            update: true,
            ..Caps::default()
        };
        assert!(view.hints().contains(&("U", "update repo")));

        view.rows[0].agent = Some("sample".into());
        view.rows[0].caps = Caps::default();
        view.rows[0].state = Some(EntryState::Shadow { same_content: true });
        assert!(view.hints().contains(&("r", "relink")));
        assert!(view.hints().iter().all(|(key, _)| *key != "x"));

        view.rows[0].state = Some(EntryState::Broken {
            target: root.path().join("missing"),
        });
        assert!(view.hints().contains(&("x", "remove link")));
        assert!(view.hints().iter().all(|(key, _)| *key != "r"));
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
        assert_eq!(view.class_counts[0], 2);
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
        for (width, height) in [(120, 30), (50, 12)] {
            view.list.first(view.rows.len());
            assert_virtual_render_matches_full(&mut view, &ctx, width, height);
            view.list.last(view.rows.len());
            assert_virtual_render_matches_full(&mut view, &ctx, width, height);
        }
        view.list.first(view.rows.len());
        render(&mut view, &ctx, 120, 30);

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
        assert_eq!(view.class_counts[0], 0);
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
        assert_eq!(view.class_counts[0], 0);
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

    #[test]
    fn unchecked_content_is_review_only_and_never_claimed_healthy() {
        let root = skills::ops::DownloadDir::new("health-unchecked").unwrap();
        skills::config::Config {
            agents: vec![],
            ..Default::default()
        }
        .save(root.path())
        .unwrap();
        let skill = root.path().join("repos/demo/sample");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            "---\nname: sample\ndescription: sample\n---\nBody",
        )
        .unwrap();
        let ws = Workspace::open(root.path()).unwrap();
        ws.meta
            .save(
                "repos/demo/sample",
                &skills::meta::SkillMeta {
                    source: Some(skills::meta::Source::Archive {
                        url: "https://example.test/demo.tar.gz".into(),
                        subpath: Some("sample".into()),
                        revision: Some("v1".into()),
                    }),
                    baseline: Some(skills::meta::Baseline {
                        hash: "sha256:recorded".into(),
                        hash_algo: skills::hash::HASH_ALGO,
                    }),
                    ..Default::default()
                },
            )
            .unwrap();
        let snap = ws.inventory().unwrap();
        let record = snap.get("repos/demo/sample").unwrap();
        assert_eq!(record.status, SkillStatus::Unchecked);
        assert_eq!(Caps::of(record, None), Caps::default());

        let settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &settings,
        };
        let mut view = HealthView::default();
        view.refresh(&ctx);
        assert_eq!(view.class_counts, [0, 1, 0]);
        let (text, _, _) = render(&mut view, &ctx, 100, 24);
        assert!(text.contains("unchecked"));
        assert!(!text.contains("everything is healthy"));

        view.set_verifying(true);
        let (checking, _, _) = render(&mut view, &ctx, 100, 24);
        assert!(checking.contains("Checking local content"));

        view.set_verifying(false);
        view.set_verification_error(Some("disk offline".into()));
        let (failed, _, _) = render(&mut view, &ctx, 100, 24);
        assert!(failed.contains("Verification failed: disk offline"));
        assert!(failed.contains("verification failed"));
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
    deployment_name: String,
    eligible: usize,
    shown: Vec<String>,
    list: ListNav,
    rect: Rect,
}

impl RepairResolve {
    fn empty_message(&self) -> Option<String> {
        if !self.shown.is_empty() {
            return None;
        }
        Some(if self.eligible == 0 {
            format!(
                "No eligible Library skills. Repairs require a present skill with the same deployment name: {}.",
                self.deployment_name
            )
        } else {
            format!(
                "No search matches. Clear or edit the query to find Library skills with the same deployment name: {}.",
                self.deployment_name
            )
        })
    }
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
        if let Some(resolve) = &self.resolve {
            if resolve.shown.is_empty() {
                return &[("type", "search Library"), ("Esc", "back")];
            }
            return &[
                ("type", "search Library"),
                ("↑↓", "choose"),
                ("Enter", "stage"),
                ("Esc", "back"),
            ];
        }
        if let Some(hints) = self.focus.button_hints() {
            return hints;
        }
        if self.preview.is_some() {
            &[
                ("↑↓", "review"),
                ("Enter", "resolve"),
                ("Space", "stage / skip"),
                ("Tab/Shift+Tab", "buttons"),
                ("Esc", "cancel"),
            ]
        } else {
            &[("Enter", "analyze"), ("Esc", "cancel")]
        }
    }

    pub fn footer_help(&self) -> crate::tui::footer::Help<'_> {
        if let Some(resolve) = &self.resolve {
            return crate::tui::footer::Help::new(self.hints()).detail(
                resolve.empty_message().unwrap_or_else(|| {
                    "Select a Library skill as the replacement deployment target".into()
                }),
            );
        }
        let preview = self.preview.is_some();
        self.focus.help(
            self.hints(),
            if preview {
                "Review staged repairs; nothing is written until Apply"
            } else {
                "Analyze current managed Agent directories"
            },
            if self.preview.as_ref().is_some_and(|plan| {
                plan.actions
                    .iter()
                    .all(|a| self.skipped.contains(action_id(a)))
            }) {
                "No staged repairs; close the review"
            } else if preview {
                "Apply the staged deployment repairs"
            } else {
                "Analyze current managed Agent directories"
            },
            if preview {
                "Return to analysis without applying repairs"
            } else {
                "Close without analyzing"
            },
        )
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
        resolve.eligible = 0;
        resolve.shown = ctx
            .snap
            .skills
            .iter()
            .filter(|skill| {
                skill.status.is_present() && skill.deployment_name() == Some(link_name.as_str())
            })
            .inspect(|_| resolve.eligible += 1)
            .filter(|skill| {
                skill.key.to_lowercase().contains(&query)
                    || skill
                        .name
                        .as_deref()
                        .is_some_and(|name| name.contains(&query))
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
            deployment_name: match issue {
                skills::ops::repair::RepairIssue::Broken { link_name, .. } => link_name.clone(),
                _ => unreachable!(),
            },
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
        if let Some(resolve) = &mut self.resolve {
            match crate::tui::widgets::popup_mouse(mouse, resolve.rect) {
                crate::tui::widgets::PopupMouse::OutsidePress => {
                    self.resolve = None;
                    return vec![];
                }
                crate::tui::widgets::PopupMouse::Inside => {}
                _ => return vec![],
            }
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) if !resolve.rect.contains(position) => {
                    self.resolve = None;
                }
                MouseEventKind::Down(MouseButton::Left) if resolve.list.rows.contains(position) => {
                    resolve.list.click(mouse.row, resolve.shown.len());
                }
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                    if resolve.list.rows.contains(position) =>
                {
                    resolve.list.move_by(
                        if mouse.kind == MouseEventKind::ScrollDown {
                            3
                        } else {
                            -3
                        },
                        resolve.shown.len(),
                    );
                }
                _ => {}
            }
            return vec![];
        }
        match crate::tui::widgets::popup_mouse(mouse, self.rect) {
            crate::tui::widgets::PopupMouse::OutsidePress => return vec![Action::CloseModal],
            crate::tui::widgets::PopupMouse::Inside => {}
            _ => return vec![],
        }
        match mouse.kind {
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp
                if self.list.rows.contains(position) =>
            {
                self.focus = ChoiceFocus::List;
                self.list.move_by(
                    if mouse.kind == MouseEventKind::ScrollDown {
                        3
                    } else {
                        -3
                    },
                    self.row_count(),
                );
            }
            MouseEventKind::Down(MouseButton::Left) if !self.rect.contains(position) => {
                return vec![Action::CloseModal];
            }
            MouseEventKind::Down(MouseButton::Left) if self.buttons[0].contains(position) => {
                self.focus = ChoiceFocus::Apply;
                return self.primary();
            }
            MouseEventKind::Down(MouseButton::Left) if self.buttons[1].contains(position) => {
                self.focus = ChoiceFocus::Cancel;
                return self.secondary();
            }
            MouseEventKind::Down(MouseButton::Left)
                if self.list.rows.contains(position)
                    && self.list.click(mouse.row, self.row_count()).is_some() =>
            {
                self.focus = ChoiceFocus::List;
            }
            _ => {}
        }
        vec![]
    }

    pub fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) {
        use crate::tui::widgets::{PopupClass, popup_size, popup_too_small};
        self.buttons = [Rect::default(); 2];
        self.list.rows = Rect::default();
        let preview = self.preview.is_some();
        let available = area;
        let size = popup_size(
            area,
            PopupClass::Preview,
            if preview {
                (self.row_count().min(16) as u16 * 3).saturating_add(6)
            } else {
                9
            },
            (12, 3),
        );
        let area = size.outer;
        self.rect = area;
        frame.render_widget(crate::tui::widgets::PopupSurface, area);
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
        if !size.usable {
            popup_too_small(frame, size, theme);
            self.draw_resolve(frame, available, ctx);
            return;
        }
        let compact = inner.height < 7 || inner.width < 30;
        let footer_height = if compact { 1 } else { 2 };
        let summary_height = if compact { 1 } else { 2 };
        let summary_area = Rect::new(
            inner.x + 1,
            inner.y,
            inner.width.saturating_sub(2),
            summary_height,
        );
        self.list.rows = Rect::new(
            inner.x + 1,
            inner.y + summary_height,
            inner.width.saturating_sub(2),
            inner.height.saturating_sub(summary_height + footer_height),
        );
        self.list.item_height = if compact { 1 } else { 3 };
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
                                    "broken: {} · {} Library matches; choose a replacement",
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
                    let mut lines = vec![
                        Line::from(vec![
                            Span::styled(format!("{label:<11}"), style),
                            Span::styled(issue.id(), theme.bold()),
                        ]),
                        Line::from(Span::styled(
                            format!("  {}", fit(&detail, inner.width.saturating_sub(4) as usize)),
                            theme.description(),
                        )),
                        Line::default(),
                    ];
                    if compact {
                        lines.truncate(1);
                    }
                    ListItem::new(lines)
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
                vec![ListItem::new(if compact {
                    vec![Line::from("Find broken and outdated deployments.")]
                } else {
                    vec![
                        Line::from(
                            "Find broken deployments and deployments using an outdated skill name.",
                        ),
                        Line::from(Span::styled(
                            "Only independent, managed Agent directories are scanned.",
                            theme.description(),
                        )),
                    ]
                })],
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
            "",
            (primary, secondary),
            theme,
        );
        self.draw_resolve(frame, available, ctx);
    }

    fn draw_resolve(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) {
        use crate::tui::widgets::{PopupClass, popup_size, popup_too_small};
        let theme = &ctx.settings.theme;
        if let Some(resolve) = &mut self.resolve {
            resolve.list.rows = Rect::default();
            resolve.input.clear_area();
            let size = popup_size(
                area,
                PopupClass::Choice,
                resolve.eligible.clamp(1, 13) as u16 + 5,
                (4, 3),
            );
            let picker = size.outer;
            resolve.rect = picker;
            frame.render_widget(crate::tui::widgets::PopupSurface, picker);
            let block = theme.block(" Choose Library skill ", true);
            let inside = block.inner(picker);
            frame.render_widget(block, picker);
            if !size.usable {
                popup_too_small(frame, size, theme);
                return;
            }
            frame.render_widget(
                Paragraph::new(format!("Search: {}", resolve.input.value())).style(theme.accent()),
                Rect::new(inside.x + 1, inside.y, inside.width.saturating_sub(2), 1),
            );
            resolve.list.rows = Rect::new(
                inside.x + 1,
                inside.y + 2,
                inside.width.saturating_sub(2),
                inside.height.saturating_sub(2),
            );
            if let Some(message) = resolve.empty_message() {
                frame.render_widget(
                    Paragraph::new(message)
                        .style(theme.dim())
                        .wrap(Wrap { trim: false }),
                    resolve.list.rows,
                );
            } else {
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

    #[test]
    fn responsive_repair_keeps_actions_and_child_empty_state_and_clears_hits() {
        with_library(&[], |ctx| {
            let mut dialog = RepairDialog::preview(Default::default(), plan(vec![]));
            for (width, height) in [
                (160, 40),
                (112, 32),
                (60, 20),
                (40, 12),
                (4, 3),
                (1, 1),
                (0, 0),
            ] {
                let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(
                    width.max(1),
                    height.max(1),
                ))
                .unwrap();
                let area = Rect::new(0, 0, width, height.saturating_sub(2));
                terminal
                    .draw(|frame| dialog.draw(frame, area, ctx))
                    .unwrap();
                for hit in [dialog.list.rows, dialog.buttons[0], dialog.buttons[1]] {
                    assert!(hit.is_empty() || hit.intersection(area) == hit);
                }
                if width == 40 {
                    assert_eq!(dialog.list.item_height, 1);
                    assert!(dialog.list.rows.height > 0);
                    assert!(!dialog.buttons[0].is_empty());
                    assert!(!dialog.buttons[1].is_empty());
                }
                if width < 4 {
                    assert_eq!(dialog.list.rows, Rect::default());
                    assert_eq!(dialog.buttons, [Rect::default(); 2]);
                }
            }
            dialog.key(key(KeyCode::Enter), ctx);
            assert!(dialog.resolve.is_some());
            for (width, height) in [(40, 12), (1, 1), (160, 40)] {
                let mut terminal =
                    ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))
                        .unwrap();
                let area = Rect::new(0, 0, width, height - 2.min(height));
                terminal
                    .draw(|frame| dialog.draw(frame, area, ctx))
                    .unwrap();
                let resolve = dialog.resolve.as_ref().unwrap();
                assert!(
                    resolve.list.rows.is_empty()
                        || resolve.list.rows.intersection(area) == resolve.list.rows
                );
                if width >= 40 {
                    let text: String = terminal
                        .backend()
                        .buffer()
                        .content
                        .iter()
                        .map(|cell| cell.symbol())
                        .collect();
                    assert!(text.contains("Library"), "{text}");
                    assert!(text.contains("No") || text.contains("no"), "{text}");
                }
            }
        });
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::from(code)
    }

    #[test]
    fn repair_filter_geometry_stays_stable_and_resize_scrolls_selection() {
        let names: Vec<_> = (0..30).map(|i| format!("skill-{i:02}")).collect();
        let skills: Vec<_> = names.iter().map(|name| (name.as_str(), "shared")).collect();
        with_library(&skills, |ctx| {
            let mut dialog = RepairDialog::preview(Default::default(), plan(vec![]));
            dialog.open_resolve(ctx);
            let mut previous = None;
            for (width, height) in [(100, 30), (60, 12), (100, 30)] {
                let mut term =
                    ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))
                        .unwrap();
                let mut geometry = None;
                for (query, count) in [("", 30), ("skill-29", 1), ("absent", 0), ("", 30)] {
                    dialog.resolve.as_mut().unwrap().input =
                        crate::tui::widgets::Input::with_value(query);
                    dialog.refilter_resolve(ctx);
                    let resolve = dialog.resolve.as_mut().unwrap();
                    assert_eq!(resolve.shown.len(), count);
                    resolve.list.last(count);
                    term.draw(|f| dialog.draw_resolve(f, Rect::new(0, 0, width, height - 2), ctx))
                        .unwrap();
                    let resolve = dialog.resolve.as_ref().unwrap();
                    let current = (resolve.rect, resolve.list.rows);
                    if let Some(expected) = geometry {
                        assert_eq!(current, expected);
                    } else {
                        geometry = Some(current);
                    }
                    if let Some(selected) = resolve.list.selected() {
                        let offset = resolve.list.state.offset();
                        assert!(
                            selected >= offset
                                && selected < offset + resolve.list.rows.height as usize
                        );
                        let y = resolve.list.rows.y + (selected - offset) as u16;
                        assert_eq!(
                            term.backend().buffer()[(resolve.list.rows.x, y)].symbol(),
                            "▸"
                        );
                    }
                }
                if let Some(before) = previous {
                    assert_ne!(geometry.unwrap().0, before);
                }
                previous = Some(geometry.unwrap().0);
            }
        });
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
    fn repair_resolver_contains_pointer_events_and_footer_follows_buttons() {
        let mut dialog = RepairDialog {
            rect: Rect::new(0, 0, 100, 30),
            buttons: [Rect::new(80, 28, 8, 1), Rect::new(90, 28, 8, 1)],
            ..Default::default()
        };
        dialog.focus = ChoiceFocus::Apply;
        assert!(dialog.hints().contains(&("Enter/Space", "activate")));
        let mut resolve = RepairResolve {
            rect: Rect::new(20, 5, 60, 18),
            shown: vec!["first".into(), "second".into()],
            ..Default::default()
        };
        resolve.list.rows = Rect::new(21, 8, 58, 12);
        resolve.list.first(2);
        dialog.resolve = Some(resolve);
        assert!(dialog.hints().contains(&("Enter", "stage")));
        let mouse = |column, row, kind| MouseEvent {
            column,
            row,
            kind,
            modifiers: crossterm::event::KeyModifiers::NONE,
        };
        assert!(
            dialog
                .mouse(mouse(22, 9, MouseEventKind::Down(MouseButton::Left)))
                .is_empty()
        );
        assert_eq!(dialog.resolve.as_ref().unwrap().list.selected(), Some(1));
        for button in [MouseButton::Left, MouseButton::Right, MouseButton::Middle] {
            if dialog.resolve.is_none() {
                dialog.resolve = Some(RepairResolve {
                    rect: Rect::new(20, 5, 60, 18),
                    ..Default::default()
                });
            }
            assert!(
                dialog
                    .mouse(mouse(120, 40, MouseEventKind::ScrollDown))
                    .is_empty()
            );
            assert!(dialog.resolve.is_some());
            // Even outside both rectangles, only the child closes.
            assert!(
                dialog
                    .mouse(mouse(120, 40, MouseEventKind::Down(button)))
                    .is_empty()
            );
            assert!(dialog.resolve.is_none());
        }
        assert!(dialog.hints().contains(&("Enter/Space", "activate")));
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

    fn with_library(skills: &[(&str, &str)], test: impl FnOnce(&Ctx)) {
        let tmp = skills::ops::DownloadDir::new("repair-empty-state").unwrap();
        skills::config::Config {
            agents: vec![],
            ..Default::default()
        }
        .save(tmp.path())
        .unwrap();
        for (key, name) in skills {
            let path = tmp.path().join(key);
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(
                path.join("SKILL.md"),
                format!("---\nname: {name}\ndescription: Test skill\n---\nBody\n"),
            )
            .unwrap();
        }
        let ws = skills::Workspace::open(tmp.path()).unwrap();
        let snap = ws.scan().unwrap();
        let settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
        test(&Ctx {
            ws: &ws,
            snap: &snap,
            settings: &settings,
        });
    }

    fn render_dialog(dialog: &mut RepairDialog, ctx: &Ctx) -> String {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        terminal
            .draw(|frame| dialog.draw(frame, frame.area(), ctx))
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    fn assert_empty_inert(dialog: &mut RepairDialog, ctx: &Ctx) {
        assert_eq!(
            dialog.hints(),
            &[("type", "search Library"), ("Esc", "back")]
        );
        for code in [KeyCode::Enter, KeyCode::Down, KeyCode::Up] {
            assert!(dialog.key(key(code), ctx).is_empty());
        }
        let rows = dialog.resolve.as_ref().unwrap().list.rows;
        for kind in [
            MouseEventKind::Down(MouseButton::Left),
            MouseEventKind::Down(MouseButton::Left),
            MouseEventKind::ScrollDown,
            MouseEventKind::ScrollUp,
        ] {
            assert!(
                dialog
                    .mouse(MouseEvent {
                        column: rows.x,
                        row: rows.y,
                        kind,
                        modifiers: crossterm::event::KeyModifiers::NONE,
                    })
                    .is_empty()
            );
        }
        assert!(dialog.resolve.as_ref().unwrap().list.selected().is_none());
        assert!(dialog.options.deployments.is_empty());
        assert!(dialog.staged_plan().unwrap().actions.is_empty());
    }

    #[test]
    fn repair_resolver_explains_no_same_deployment_name_and_keeps_back_working() {
        // A storage key matching the broken link is insufficient: the declared
        // deployment name must match, and the Library skill must be present.
        with_library(&[("shared", "other"), ("invalid", "shared")], |ctx| {
            let mut snap = ctx.snap.clone();
            snap.skills
                .iter_mut()
                .find(|skill| skill.key == "invalid")
                .unwrap()
                .status = SkillStatus::Invalid {
                reason: "Unreadable SKILL.md".into(),
            };
            let ctx = Ctx {
                snap: &snap,
                ..*ctx
            };
            let mut dialog = RepairDialog::preview(Default::default(), plan(vec![]));
            assert!(dialog.key(key(KeyCode::Enter), &ctx).is_empty());
            let text = render_dialog(&mut dialog, &ctx);
            assert!(text.contains("No eligible Library skills."));
            assert!(
                dialog
                    .footer_help()
                    .detail
                    .contains("same deployment name: shared")
            );
            assert_empty_inert(&mut dialog, &ctx);
            assert!(dialog.paste("missing", &ctx).is_empty());
            assert!(render_dialog(&mut dialog, &ctx).contains("No eligible Library skills."));
            assert!(dialog.key(key(KeyCode::Esc), &ctx).is_empty());
            assert!(dialog.resolve.is_none());
            assert!(dialog.options.deployments.is_empty());
        });
    }

    #[test]
    fn repair_resolver_filter_empty_recovers_hints_and_stages_only_selected_skill() {
        with_library(
            &[("alpha", "shared"), ("beta", "shared"), ("shared", "other")],
            |ctx| {
                let mut dialog = RepairDialog::preview(Default::default(), plan(vec![]));
                dialog.key(key(KeyCode::Enter), ctx);
                assert_eq!(dialog.resolve.as_ref().unwrap().shown, ["alpha", "beta"]);
                assert!(dialog.hints().contains(&("Enter", "stage")));
                assert!(dialog.paste("z", ctx).is_empty());
                let text = render_dialog(&mut dialog, ctx);
                assert!(text.contains("No search matches."));
                assert!(!text.contains("No eligible Library skills."));
                assert!(
                    dialog
                        .footer_help()
                        .detail
                        .contains("Clear or edit the query")
                );
                assert!(
                    dialog
                        .footer_help()
                        .detail
                        .contains("same deployment name: shared")
                );
                assert_empty_inert(&mut dialog, ctx);

                assert!(dialog.key(key(KeyCode::Backspace), ctx).is_empty());
                let text = render_dialog(&mut dialog, ctx);
                assert!(!text.contains("No search matches."));
                assert_eq!(dialog.resolve.as_ref().unwrap().shown, ["alpha", "beta"]);
                assert!(dialog.hints().contains(&("↑↓", "choose")));
                assert!(dialog.hints().contains(&("Enter", "stage")));
                assert_eq!(
                    dialog.footer_help().detail,
                    "Select a Library skill as the replacement deployment target"
                );
                assert!(dialog.key(key(KeyCode::Char('b')), ctx).is_empty());
                assert_eq!(dialog.resolve.as_ref().unwrap().shown, ["beta"]);
                assert!(dialog.options.deployments.is_empty());
                let actions = dialog.key(key(KeyCode::Enter), ctx);
                assert!(matches!(
                    actions.as_slice(),
                    [Action::CloseModal, Action::Spawn(Task::RepairPlan(options))]
                        if options.deployments.get("test/shared").map(String::as_str) == Some("beta")
                ));
                assert!(dialog.resolve.is_none());
                assert_eq!(dialog.options.deployments["test/shared"], "beta");
                assert!(dialog.staged_plan().unwrap().actions.is_empty());
            },
        );
    }

    fn test_ctx() -> Ctx<'static> {
        let tmp = Box::leak(Box::new(
            skills::ops::DownloadDir::new("repair-dialog").unwrap(),
        ));
        let root = tmp.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        skills::config::Config {
            agents: vec![],
            ..Default::default()
        }
        .save(&root)
        .unwrap();
        let ws = Box::leak(Box::new(skills::Workspace::open(&root).unwrap()));
        let snap = Box::leak(Box::new(ws.scan().unwrap()));
        let settings = Box::leak(Box::new(crate::tui::settings::RuntimeSettings::new(
            &ws.config,
        )));
        Ctx { ws, snap, settings }
    }
}
