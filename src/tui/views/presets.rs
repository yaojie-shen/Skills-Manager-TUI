//! Presets tab.

use super::{View, split_panes, wheel};
use crate::tui::app::{Action, Ctx, Hints};
use crate::tui::modal::Modal;
use crate::tui::widgets::{ListNav, pad};
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, Paragraph, Wrap};
use skills::ops::deploy;
use skills::preset::Preset;

#[derive(Default)]
pub struct PresetsView {
    presets: Vec<Preset>,
    list: ListNav,
    members: ListNav,
    focus_members: bool,
    left: Rect,
    right: Rect,
}

impl PresetsView {
    fn selected(&self) -> Option<&Preset> {
        self.list.selected().and_then(|i| self.presets.get(i))
    }

    fn plan(&self, ctx: &Ctx, on: bool) -> Vec<Action> {
        let Some(p) = self.selected() else {
            return vec![];
        };
        let targets = if p.agents.is_empty() {
            ctx.ws.config.agent_keys()
        } else {
            p.agents.clone()
        };
        let present: Vec<String> = p
            .skills
            .iter()
            .filter(|s| ctx.snap.get(s).is_some())
            .cloned()
            .collect();
        let plan = if on {
            deploy::plan_deploy(ctx.ws, ctx.snap, &present, &targets)
        } else {
            deploy::plan_undeploy(ctx.ws, ctx.snap, &present, &targets)
        };
        match plan {
            Ok(mut actions) => {
                for s in p.skills.iter().filter(|s| ctx.snap.get(s).is_none()) {
                    actions.push(deploy::Action::Skip {
                        agent: "*".into(),
                        skill: s.clone(),
                        reason: "not in skills root".into(),
                    });
                }
                // Toggling a preset goes through without asking, the same as
                // on the Agents page: it is one gesture, it says what it did,
                // and undo takes it back.
                vec![Action::ApplyLinks {
                    title: format!(
                        "{} preset {}",
                        if on { "deploy" } else { "undeploy" },
                        p.name
                    ),
                    actions,
                }]
            }
            Err(e) => vec![Action::Error(format!("{e:#}"))],
        }
    }

    fn remove_member(&self) -> Vec<Action> {
        let Some(p) = self.selected() else {
            return vec![];
        };
        let Some(i) = self.members.selected() else {
            return vec![];
        };
        let Some(skill) = p.skills.get(i).cloned() else {
            return vec![];
        };
        let name = p.name.clone();
        vec![Action::Write(Box::new(move |ws| {
            let mut p = ws
                .presets
                .load(&name)?
                .ok_or_else(|| anyhow::anyhow!("no such preset: {name}"))?;
            p.skills.retain(|s| s != &skill);
            ws.presets.save(&p)?;
            Ok(format!("{name}: removed {skill}"))
        }))]
    }
}

impl View for PresetsView {
    fn refresh(&mut self, ctx: &Ctx) {
        self.presets = ctx.ws.presets.list().unwrap_or_default();
        self.list.clamp(self.presets.len());
        let n = self.selected().map(|p| p.skills.len()).unwrap_or(0);
        self.members.clamp(n);
    }

    fn handle_key(&mut self, k: KeyEvent, ctx: &Ctx) -> Vec<Action> {
        let n = self.presets.len();
        let mcount = self.selected().map(|p| p.skills.len()).unwrap_or(0);
        match k.code {
            KeyCode::Char('q') | KeyCode::Esc if !self.focus_members => vec![Action::Quit],
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('h') => {
                self.focus_members = false;
                vec![]
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') if !self.focus_members => {
                if mcount > 0 {
                    self.focus_members = true;
                    self.members.clamp(mcount);
                }
                vec![]
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.focus_members {
                    self.members.move_by(1, mcount);
                } else {
                    self.list.move_by(1, n);
                    self.members
                        .first(self.selected().map(|p| p.skills.len()).unwrap_or(0));
                }
                vec![]
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if self.focus_members {
                    self.members.move_by(-1, mcount);
                } else {
                    self.list.move_by(-1, n);
                    self.members
                        .first(self.selected().map(|p| p.skills.len()).unwrap_or(0));
                }
                vec![]
            }
            KeyCode::Char('c') => vec![Action::OpenModal(Box::new(Modal::new_preset()))],
            // Membership is edited by picking from the library, never by typing
            // a name from memory.
            KeyCode::Char('a') | KeyCode::Char(' ') => match self.selected() {
                Some(p) => vec![Action::OpenModal(Box::new(Modal::preset_members(
                    p, ctx.snap,
                )))],
                None => vec![Action::Error(
                    "no preset selected; press c to create one".into(),
                )],
            },
            KeyCode::Char('x') | KeyCode::Delete if self.focus_members => self.remove_member(),
            KeyCode::Char('X') | KeyCode::Char('x') => match self.selected() {
                Some(p) => vec![Action::OpenModal(Box::new(Modal::delete_preset(&p.name)))],
                None => vec![],
            },
            KeyCode::Char('d') => self.plan(ctx, true),
            KeyCode::Char('u') => self.plan(ctx, false),
            KeyCode::Enter if self.focus_members => {
                let key = self
                    .selected()
                    .and_then(|p| self.members.selected().and_then(|i| p.skills.get(i)))
                    .cloned();
                match key {
                    Some(k) => vec![Action::Search {
                        query: k,
                        focus_list: true,
                    }],
                    None => vec![],
                }
            }
            _ => vec![],
        }
    }

    fn handle_mouse(&mut self, m: MouseEvent, _ctx: &Ctx) -> Vec<Action> {
        let at = (m.column, m.row).into();
        let mcount = self.selected().map(|p| p.skills.len()).unwrap_or(0);
        if let Some(d) = wheel(&m) {
            if self.left.contains(at) {
                self.list.move_by(d, self.presets.len());
            } else if self.right.contains(at) {
                self.members.move_by(d, mcount);
            }
            return vec![];
        }
        if let MouseEventKind::Down(MouseButton::Left) = m.kind {
            if self.left.contains(at) {
                self.focus_members = false;
                self.list.click(m.row, self.presets.len());
            } else if self.right.contains(at) && self.members.click(m.row, mcount).is_some() {
                self.focus_members = true;
            }
        }
        vec![]
    }

    fn draw(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        let th = ctx.theme;
        let (left, right) = split_panes(area, 38);
        self.left = left;
        self.right = right;
        let w = left.width.saturating_sub(4) as usize;
        let items: Vec<ListItem> = self
            .presets
            .iter()
            .map(|p| {
                let auto = ctx.ws.config.deploy.presets.contains(&p.name);
                ListItem::new(Line::from(vec![
                    Span::raw(pad(&p.name, w.saturating_sub(12))),
                    Span::styled(format!("{:>3} ", p.skills.len()), th.dim()),
                    Span::styled(if auto { "auto" } else { "    " }, th.ok()),
                ]))
            })
            .collect();
        self.list.set_area_from_block(left);
        let list = List::new(items)
            .block(th.block(" presets ", !self.focus_members))
            .highlight_style(if self.focus_members {
                th.selected_unfocused()
            } else {
                th.selected()
            })
            .highlight_symbol("▸ ");
        f.render_stateful_widget(list, left, &mut self.list.state);
        if self.presets.is_empty() {
            f.render_widget(
                Paragraph::new(Span::styled(
                    "no presets yet — press c to create one",
                    th.dim(),
                ))
                .wrap(Wrap { trim: true }),
                Rect {
                    x: left.x + 2,
                    y: left.y + 1,
                    width: left.width.saturating_sub(4),
                    height: 2,
                },
            );
        }

        let block = th.block(" members ", self.focus_members);
        match self.selected().cloned() {
            Some(p) => {
                let mut header: Vec<Line> = vec![Line::from(vec![
                    Span::styled(p.name.as_str(), th.bold().fg(th.accent)),
                    Span::styled(
                        format!(
                            "   agents: {}",
                            if p.agents.is_empty() {
                                "all".to_string()
                            } else {
                                p.agents.join(", ")
                            }
                        ),
                        th.dim(),
                    ),
                ])];
                if let Some(d) = &p.description {
                    header.push(Line::from(Span::styled(d.as_str(), th.dim())));
                }
                let inner = block.inner(right);
                f.render_widget(block, right);
                let hh = header.len() as u16;
                f.render_widget(
                    Paragraph::new(header),
                    Rect {
                        height: hh.min(inner.height),
                        ..inner
                    },
                );
                let list_area = Rect {
                    y: inner.y + hh + 1,
                    height: inner.height.saturating_sub(hh + 1),
                    ..inner
                };
                self.members.rows = list_area;
                let items: Vec<ListItem> = p
                    .skills
                    .iter()
                    .map(|s| {
                        let (mark, style) = match ctx.snap.get(s) {
                            Some(r) if r.status.is_present() => ("●", th.ok()),
                            Some(_) => ("✗", th.err()),
                            None => ("?", th.err()),
                        };
                        ListItem::new(Line::from(vec![
                            Span::styled(mark, style),
                            Span::raw(format!(" {s}")),
                        ]))
                    })
                    .collect();
                let list = List::new(items)
                    .highlight_style(if self.focus_members {
                        th.selected()
                    } else {
                        th.selected_unfocused()
                    })
                    .highlight_symbol("▸ ");
                f.render_stateful_widget(list, list_area, &mut self.members.state);
                if p.skills.is_empty() {
                    f.render_widget(
                        Paragraph::new(Span::styled("empty — press a to add a skill", th.dim())),
                        list_area,
                    );
                }
            }
            None => f.render_widget(block, right),
        }
    }

    fn hints(&self) -> Hints {
        if self.focus_members {
            &[("Enter", "open"), ("x", "remove member"), ("Esc", "back")]
        } else {
            &[
                ("c", "create"),
                ("a", "choose skills"),
                ("d/u", "deploy/undeploy"),
                ("X", "delete"),
                ("Enter", "members"),
                ("q", "quit"),
            ]
        }
    }
}
