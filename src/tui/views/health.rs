//! Health tab: everything that is not managed/unmanaged, plus update checks.

use super::{View, status_glyph, status_text, wheel};
use crate::tui::app::{Action, Ctx, Hints};
use crate::tui::event::Task;
use crate::tui::widgets::{ListNav, pad};
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, Paragraph};
use skills::ops::edit;
use skills::ops::update::CheckResult;
use skills::reconcile::SkillStatus;
use std::collections::BTreeMap;

#[derive(Default)]
pub struct HealthView {
    keys: Vec<String>,
    list: ListNav,
    area: Rect,
    /// key -> last check result
    checks: BTreeMap<String, Result<CheckResult, String>>,
}

impl HealthView {
    fn selected<'a>(&self, ctx: &'a Ctx) -> Option<&'a skills::reconcile::SkillRecord> {
        self.list
            .selected()
            .and_then(|i| self.keys.get(i))
            .and_then(|k| ctx.snap.get(k))
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
        self.keys = ctx
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
            .map(|s| s.key.clone())
            .collect();
        self.list.clamp(self.keys.len());
    }
}

impl View for HealthView {
    fn refresh(&mut self, ctx: &Ctx) {
        self.rebuild(ctx);
    }

    fn handle_key(&mut self, k: KeyEvent, ctx: &Ctx) -> Vec<Action> {
        let n = self.keys.len();
        match k.code {
            KeyCode::Char('q') | KeyCode::Esc => vec![Action::Quit],
            KeyCode::Down | KeyCode::Char('j') => {
                self.list.move_by(1, n);
                vec![]
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.list.move_by(-1, n);
                vec![]
            }
            KeyCode::Enter => match self.selected(ctx) {
                Some(r) => vec![Action::Search {
                    query: r.key.clone(),
                    focus_list: true,
                }],
                None => vec![],
            },
            KeyCode::Char('c') => {
                let keys: Vec<String> = ctx
                    .snap
                    .skills
                    .iter()
                    .filter(|s| matches!(s.source, Some(skills::meta::Source::Git { .. })))
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
            KeyCode::Char('U') => match self.selected(ctx) {
                Some(r) if matches!(r.source, Some(skills::meta::Source::Git { .. })) => vec![
                    Action::Spawn(Task::Prepare(r.key.clone())),
                    Action::Toast(format!("fetching {}…", r.key)),
                ],
                Some(r) => vec![Action::Error(format!("{} has no git source", r.key))],
                None => vec![],
            },
            KeyCode::Char('a') => match self.selected(ctx) {
                Some(r)
                    if matches!(
                        r.status,
                        SkillStatus::Modified | SkillStatus::Managed { no_baseline: true }
                    ) =>
                {
                    let key = r.key.clone();
                    vec![Action::Write(Box::new(move |ws| {
                        edit::accept(ws, &key).map(|_| format!("baseline updated for {key}"))
                    }))]
                }
                _ => vec![Action::Error("accept applies to modified skills".into())],
            },
            KeyCode::Char('m') => match self.selected(ctx) {
                Some(r) => match &r.status {
                    SkillStatus::Renamed { to } => {
                        let (old, new) = (r.key.clone(), to.clone());
                        vec![Action::Write(Box::new(move |ws| {
                            edit::migrate_meta(ws, &old, &new)
                                .map(|_| format!("metadata moved {old} → {new}"))
                        }))]
                    }
                    _ => vec![Action::Error("migrate applies to renamed? skills".into())],
                },
                None => vec![],
            },
            KeyCode::Char('x') => match self.selected(ctx) {
                Some(r) if r.status == SkillStatus::Missing => {
                    let key = r.key.clone();
                    vec![Action::Write(Box::new(move |ws| {
                        ws.meta
                            .remove(&key)
                            .map(|_| format!("dropped metadata of {key}"))
                    }))]
                }
                _ => vec![Action::Error("drop applies to missing skills".into())],
            },
            _ => vec![],
        }
    }

    fn handle_mouse(&mut self, m: MouseEvent, ctx: &Ctx) -> Vec<Action> {
        let at = (m.column, m.row).into();
        if let Some(d) = wheel(&m) {
            if self.area.contains(at) {
                self.list.move_by(d, self.keys.len());
            }
            return vec![];
        }
        if let MouseEventKind::Down(MouseButton::Left) = m.kind
            && self.area.contains(at)
            && let Some((_, double)) = self.list.click(m.row, self.keys.len())
            && double
            && let Some(r) = self.selected(ctx)
        {
            return vec![Action::Search {
                query: r.key.clone(),
                focus_list: true,
            }];
        }
        vec![]
    }

    fn draw(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        let th = ctx.theme;
        self.area = area;
        let items: Vec<ListItem> = self
            .keys
            .iter()
            .filter_map(|k| ctx.snap.get(k))
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
                            c.installed
                                .as_deref()
                                .map(skills::meta::short_rev)
                                .unwrap_or("-"),
                            skills::meta::short_rev(&c.remote)
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
        let title = if self.keys.is_empty() {
            " health ".to_string()
        } else {
            format!(" health · {} item(s) ", self.keys.len())
        };
        self.list.set_area_from_block(area);
        let list = List::new(items)
            .block(th.block(title, true))
            .highlight_style(th.selected())
            .highlight_symbol("▸ ");
        f.render_stateful_widget(list, area, &mut self.list.state);
        if self.keys.is_empty() {
            let msg =
                "everything is managed or unmanaged — press c to check git sources for updates";
            f.render_widget(
                Paragraph::new(Span::styled(msg, th.dim())),
                Rect {
                    x: area.x + 2,
                    y: area.y + 1,
                    width: area.width.saturating_sub(4),
                    height: 1,
                },
            );
        }
    }

    fn hints(&self) -> Hints {
        &[
            ("c", "check updates"),
            ("U", "update"),
            ("a", "accept"),
            ("m", "migrate"),
            ("x", "drop meta"),
            ("Enter", "open"),
            ("q", "quit"),
        ]
    }
}
