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
use crate::tui::widgets::{ListNav, pad, width};
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, Paragraph, Wrap};
use skills::meta::{Source, short_rev};
use skills::ops::edit;
use skills::ops::update::CheckResult;
use skills::reconcile::{SkillRecord, SkillStatus};
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
}

#[derive(Default)]
pub struct HealthView {
    rows: Vec<Row>,
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
    fn selected_row(&self) -> Option<&Row> {
        self.list.selected().and_then(|i| self.rows.get(i))
    }

    fn selected<'a>(&self, ctx: &'a Ctx) -> Option<&'a SkillRecord> {
        self.selected_row().and_then(|r| ctx.snap.get(&r.key))
    }

    fn select_by(&mut self, delta: i32) {
        self.list.move_by(delta, self.rows.len());
        // The pane describes a different item now, so start it from the top.
        self.detail_scroll = 0;
    }

    fn open_selected(&mut self, ctx: &Ctx) -> Vec<Action> {
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
            })
            .collect();
        self.list.clamp(self.rows.len());
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
                        "or reinstall it from the Search tab with i, from the git source above"
                            .into(),
                    )),
                    Some(Source::Local { path: Some(p) }) => actions.push(action(
                        "",
                        format!("or reinstall it from the Search tab with i, from {p}"),
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
        let Some(r) = ctx.snap.get(&row.key) else {
            return;
        };
        let lines = self.detail_lines(r, row.caps, ctx);
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
        if self.preview.handle_key(k) {
            return vec![];
        }
        let caps = self.selected_row().map(|r| r.caps).unwrap_or_default();
        match k.code {
            KeyCode::Char('q') => vec![Action::Quit],
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
        let th = ctx.theme;
        // With nothing to show there is nothing to explain either, so the
        // message gets the whole width instead of being squeezed beside an
        // empty pane.
        if self.rows.is_empty() {
            self.left = area;
            self.right = Rect::default();
            let block = th.block(" health ", true);
            let inner = block.inner(area);
            f.render_widget(block, area);
            let msg =
                "everything is managed or unmanaged — press c to check git sources for updates";
            f.render_widget(
                Paragraph::new(Span::styled(msg, th.dim())).wrap(Wrap { trim: false }),
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
            .filter_map(|row| ctx.snap.get(&row.key))
            .map(|r| {
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
        let title = format!(" health · {} item(s) ", self.rows.len());
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
        let Some(caps) = self.selected_row().map(|r| r.caps) else {
            return &[("c", "check updates"), ("Esc", "search"), ("q", "quit")];
        };
        match (caps.update, caps.accept, caps.migrate, caps.clean) {
            (true, true, _, _) => &[
                ("c", "check updates"),
                ("U", "update"),
                ("a", "accept"),
                ("Enter", "open"),
                ("Esc", "search"),
                ("q", "quit"),
            ],
            (true, false, true, _) => &[
                ("c", "check updates"),
                ("U", "update"),
                ("m", "migrate"),
                ("Enter", "open"),
                ("Esc", "search"),
                ("q", "quit"),
            ],
            (true, false, false, true) => &[
                ("c", "check updates"),
                ("U", "update"),
                ("x", "clean up"),
                ("Enter", "open"),
                ("Esc", "search"),
                ("q", "quit"),
            ],
            (true, false, false, false) => &[
                ("c", "check updates"),
                ("U", "update"),
                ("Enter", "open"),
                ("Esc", "search"),
                ("q", "quit"),
            ],
            (false, true, _, _) => &[
                ("c", "check updates"),
                ("a", "accept"),
                ("Enter", "open"),
                ("Esc", "search"),
                ("q", "quit"),
            ],
            (false, false, true, _) => &[
                ("c", "check updates"),
                ("m", "migrate"),
                ("Enter", "open"),
                ("Esc", "search"),
                ("q", "quit"),
            ],
            (false, false, false, true) => &[
                ("c", "check updates"),
                ("x", "clean up"),
                ("Enter", "open"),
                ("Esc", "search"),
                ("q", "quit"),
            ],
            (false, false, false, false) => &[
                ("c", "check updates"),
                ("Enter", "open"),
                ("Esc", "search"),
                ("q", "quit"),
            ],
        }
    }
}
