//! Agents tab: per-agent directory state, sync and convert.

use super::{View, split_panes, wheel};
use crate::tui::app::{Action, Ctx, Hints};
use crate::tui::widgets::{ListNav, pad};
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, Paragraph, Wrap};
use skills::ops::deploy;
use skills::reconcile::{AgentDirMode, EntryState};

#[derive(Default)]
pub struct AgentsView {
    list: ListNav,
    left: Rect,
    right: Rect,
    detail_scroll: u16,
}

impl AgentsView {
    fn sync(&self, ctx: &Ctx) -> Vec<Action> {
        match deploy::plan_sync(ctx.ws, ctx.snap) {
            Ok(actions) => vec![Action::ConfirmLinks {
                title: "sync agents to desired state".into(),
                actions,
            }],
            Err(e) => vec![Action::Error(format!("{e:#}"))],
        }
    }
    fn convert(&self, ctx: &Ctx) -> Vec<Action> {
        let Some(a) = self.list.selected().and_then(|i| ctx.snap.agents.get(i)) else {
            return vec![];
        };
        match deploy::plan_convert(ctx.ws, ctx.snap, &a.key) {
            Ok(actions) => vec![Action::ConfirmLinks {
                title: format!("convert {} to per-skill links", a.key),
                actions,
            }],
            Err(e) => vec![Action::Error(format!("{e:#}"))],
        }
    }
}

impl View for AgentsView {
    fn refresh(&mut self, ctx: &Ctx) {
        self.list.clamp(ctx.snap.agents.len());
    }

    fn handle_key(&mut self, k: KeyEvent, ctx: &Ctx) -> Vec<Action> {
        let n = ctx.snap.agents.len();
        match k.code {
            KeyCode::Char('q') | KeyCode::Esc => vec![Action::Quit],
            KeyCode::Down | KeyCode::Char('j') => {
                self.list.move_by(1, n);
                self.detail_scroll = 0;
                vec![]
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.list.move_by(-1, n);
                self.detail_scroll = 0;
                vec![]
            }
            KeyCode::Char('s') => self.sync(ctx),
            KeyCode::Char('c') => self.convert(ctx),
            _ => vec![],
        }
    }

    fn handle_mouse(&mut self, m: MouseEvent, ctx: &Ctx) -> Vec<Action> {
        let at = (m.column, m.row).into();
        if let Some(d) = wheel(&m) {
            if self.left.contains(at) {
                self.list.move_by(d, ctx.snap.agents.len());
            } else if self.right.contains(at) {
                self.detail_scroll = (self.detail_scroll as i32 + d).max(0) as u16;
            }
            return vec![];
        }
        if let MouseEventKind::Down(MouseButton::Left) = m.kind
            && self.left.contains(at)
        {
            self.list.click(m.row, ctx.snap.agents.len());
            self.detail_scroll = 0;
        }
        vec![]
    }

    fn draw(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        let th = ctx.theme;
        let (left, right) = split_panes(area, 38);
        self.left = left;
        self.right = right;
        let items: Vec<ListItem> = ctx
            .snap
            .agents
            .iter()
            .map(|a| {
                let mode = match &a.mode {
                    AgentDirMode::Missing => Span::styled("missing", th.dim()),
                    AgentDirMode::DirLinked => Span::styled("dir-linked", th.warn()),
                    AgentDirMode::DirForeign { .. } => Span::styled("dir-foreign", th.warn()),
                    AgentDirMode::Real => {
                        let d = a.count(|s| matches!(s, EntryState::Deployed));
                        let issues = a.entries.len() - d;
                        if issues > 0 {
                            Span::styled(format!("{d} deployed, {issues} issue(s)"), th.warn())
                        } else {
                            Span::styled(format!("{d} deployed"), th.ok())
                        }
                    }
                };
                ListItem::new(Line::from(vec![Span::raw(pad(&a.key, 10)), mode]))
            })
            .collect();
        self.list.set_area_from_block(left);
        let list = List::new(items)
            .block(th.block(" agents ", true))
            .highlight_style(th.selected())
            .highlight_symbol("▸ ");
        f.render_stateful_widget(list, left, &mut self.list.state);

        let block = th.block(" details ", false);
        let inner = block.inner(right);
        f.render_widget(block, right);
        let mut lines: Vec<Line> = Vec::new();
        if let Some(a) = self.list.selected().and_then(|i| ctx.snap.agents.get(i)) {
            lines.push(Line::from(Span::styled(
                a.name.as_str(),
                th.bold().fg(th.accent),
            )));
            lines.push(Line::from(vec![
                Span::styled("dir   ", th.dim()),
                Span::raw(skills::paths::contract_tilde(&a.skills_dir)),
            ]));
            let mode = match &a.mode {
                AgentDirMode::Missing => "directory does not exist; it is created on first deploy or sync".to_string(),
                AgentDirMode::DirLinked => "whole directory is a symlink to the skills root — every skill is visible; press c to convert to per-skill links".into(),
                AgentDirMode::DirForeign { target } => format!("directory is a symlink to {} — left alone", target.display()),
                AgentDirMode::Real => "real directory with per-skill entries".into(),
            };
            lines.push(Line::from(vec![
                Span::styled("mode  ", th.dim()),
                Span::raw(mode),
            ]));
            lines.push(Line::from(""));
            let mut issues: Vec<(&String, &EntryState)> = a
                .entries
                .iter()
                .filter(|(_, s)| !matches!(s, EntryState::Deployed))
                .collect();
            issues.sort_by_key(|(n, _)| (*n).clone());
            if a.mode == AgentDirMode::Real {
                if issues.is_empty() {
                    lines.push(Line::from(Span::styled(
                        "all entries are links into the root",
                        th.ok(),
                    )));
                } else {
                    lines.push(Line::from(Span::styled(
                        "entries needing attention",
                        th.bold(),
                    )));
                }
            }
            for (name, st) in issues {
                let (txt, style) = match st {
                    EntryState::Broken { target } => {
                        (format!("broken → {}", target.display()), th.err())
                    }
                    EntryState::Foreign { target } => {
                        (format!("foreign → {}", target.display()), th.warn())
                    }
                    EntryState::Shadow { same_content } => (
                        format!(
                            "shadow ({})",
                            if *same_content {
                                "same content; sync can replace it"
                            } else {
                                "differs; resolve by hand"
                            }
                        ),
                        th.warn(),
                    ),
                    EntryState::AgentOnly => (
                        "agent-only; adopt with `skills adopt <path>`".to_string(),
                        th.warn(),
                    ),
                    EntryState::Deployed => unreachable!(),
                };
                lines.push(Line::from(vec![
                    Span::raw(pad(name, 24)),
                    Span::styled(txt, style),
                ]));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(vec![Span::styled("s", th.key_hint()), Span::styled(" sync all agents to the desired state (config + presets), with a preview first", th.dim())]));
        }
        f.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .scroll((self.detail_scroll, 0)),
            inner,
        );
    }

    fn hints(&self) -> Hints {
        &[
            ("s", "sync"),
            ("c", "convert dir-link"),
            ("/", "search"),
            ("q", "quit"),
        ]
    }
}
