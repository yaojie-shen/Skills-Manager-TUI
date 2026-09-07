//! The preset × agent table, as a window over a page.
//!
//! The Presets page defines what a preset holds and the Agents page switches
//! presets for one agent at a time; neither shows the whole picture at once.
//! This table does, in one row per preset, and each cell is the same switch
//! the pills are. It is a window rather than a tab because it adds nothing to
//! define or configure — it only lets the switches be seen and thrown together.

use crate::tui::app::{Action, Ctx};
use crate::tui::widgets::{pad, width};
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use skills::ops::deploy::{
    PresetState, plan_preset_activate, plan_preset_deactivate, preset_status,
};
use skills::preset::Preset;

#[derive(Default)]
pub struct Matrix {
    open: bool,
    presets: Vec<Preset>,
    row: usize,
    col: usize,
    rect: Rect,
    /// Where each cell was drawn, for the mouse: `(preset row, agent column, rect)`.
    cells: Vec<(usize, usize, Rect)>,
}

impl Matrix {
    pub fn open(&mut self, ctx: &Ctx) {
        self.presets = ctx.ws.presets.list().unwrap_or_default();
        self.open = true;
        self.row = self.row.min(self.presets.len().saturating_sub(1));
        self.col = self.col.min(ctx.ws.config.agents.len().saturating_sub(1));
    }
    pub fn close(&mut self) {
        self.open = false;
    }

    /// Throw the switch in one cell: on unless the preset is already fully on
    /// there. Runs straight away and records itself, exactly like a pill.
    fn toggle(&self, ctx: &Ctx, row: usize, col: usize) -> Vec<Action> {
        let (Some(p), Some(a)) = (self.presets.get(row), ctx.ws.config.agents.get(col)) else {
            return vec![];
        };
        let scope = [a.key.clone()];
        let on = preset_status(ctx.snap, p, &scope).state() != PresetState::Active;
        let plan = if on {
            plan_preset_activate(ctx.ws, ctx.snap, p, &scope)
        } else {
            plan_preset_deactivate(ctx.ws, ctx.snap, p, &scope)
        };
        match plan {
            Ok(actions) => vec![Action::ApplyLinks {
                title: format!(
                    "{} {} · {}",
                    if on { "deploy" } else { "undeploy" },
                    p.name,
                    a.key
                ),
                actions,
            }],
            Err(e) => vec![Action::Error(format!("{e:#}"))],
        }
    }

    /// Whole row at once: on everywhere unless it is already on everywhere.
    fn toggle_row(&self, ctx: &Ctx, row: usize) -> Vec<Action> {
        let Some(p) = self.presets.get(row) else {
            return vec![];
        };
        let keys = ctx.ws.config.agent_keys();
        let on = keys.iter().any(|k| {
            preset_status(ctx.snap, p, std::slice::from_ref(k)).state() != PresetState::Active
        });
        let plan = if on {
            plan_preset_activate(ctx.ws, ctx.snap, p, &keys)
        } else {
            plan_preset_deactivate(ctx.ws, ctx.snap, p, &keys)
        };
        match plan {
            Ok(actions) => vec![Action::ApplyLinks {
                title: format!(
                    "{} {} everywhere",
                    if on { "deploy" } else { "undeploy" },
                    p.name
                ),
                actions,
            }],
            Err(e) => vec![Action::Error(format!("{e:#}"))],
        }
    }

    /// `None` when the window is closed and the key is the page's to handle.
    pub fn handle_key(&mut self, k: KeyEvent, ctx: &Ctx) -> Option<Vec<Action>> {
        if !self.open {
            return None;
        }
        let rows = self.presets.len();
        let cols = ctx.ws.config.agents.len();
        Some(match k.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('M') => {
                self.close();
                vec![]
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.row = (self.row + 1).min(rows.saturating_sub(1));
                vec![]
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.row = self.row.saturating_sub(1);
                vec![]
            }
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Tab => {
                self.col = (self.col + 1).min(cols.saturating_sub(1));
                vec![]
            }
            KeyCode::Left | KeyCode::Char('h') | KeyCode::BackTab => {
                self.col = self.col.saturating_sub(1);
                vec![]
            }
            KeyCode::Char(' ') | KeyCode::Enter => self.toggle(ctx, self.row, self.col),
            KeyCode::Char('A') => self.toggle_row(ctx, self.row),
            _ => vec![],
        })
    }

    /// A click on a cell throws it; a click outside the window closes it.
    pub fn handle_mouse(&mut self, m: MouseEvent, ctx: &Ctx) -> Option<Vec<Action>> {
        if !self.open {
            return None;
        }
        if let MouseEventKind::Down(MouseButton::Left) = m.kind {
            let at = (m.column, m.row).into();
            if !self.rect.contains(at) {
                self.close();
                return Some(vec![]);
            }
            if let Some((r, c, _)) = self.cells.iter().find(|(_, _, rect)| rect.contains(at)) {
                self.row = *r;
                self.col = *c;
                return Some(self.toggle(ctx, *r, *c));
            }
        }
        Some(vec![])
    }

    pub fn draw(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        self.cells.clear();
        if !self.open {
            self.rect = Rect::default();
            return;
        }
        let th = ctx.theme;
        let agents = &ctx.ws.config.agents;
        let name_w = self
            .presets
            .iter()
            .map(|p| width(&p.name))
            .max()
            .unwrap_or(6)
            .clamp(6, 24);
        // Every column is wide enough for `◐ 12/34`, so the table never shifts
        // as counts change under it.
        let col_w = agents
            .iter()
            .map(|a| width(&a.key))
            .max()
            .unwrap_or(4)
            .max(8);
        // The key help is the widest thing in a small table, so it sets the floor.
        let help = " Space toggle · A whole row · Esc";
        let inner_w = (name_w + 2 + agents.len() * (col_w + 2)).max(width(help)) as u16;
        let inner_h = (self.presets.len() as u16 + 4).max(5);
        let w = (inner_w + 4).min(area.width);
        let h = (inner_h + 2).min(area.height);
        let rect = Rect {
            x: area.x + (area.width - w) / 2,
            y: area.y + (area.height - h) / 2,
            width: w,
            height: h,
        };
        self.rect = rect;
        f.render_widget(Clear, rect);
        let block = th.block(" presets × agents ", true);
        let inner = block.inner(rect);
        f.render_widget(block, rect);

        let mut lines: Vec<Line> = Vec::new();
        let mut head = vec![Span::raw(format!(" {} ", pad("", name_w)))];
        for a in agents {
            head.push(Span::styled(
                format!(" {} ", pad(&a.key, col_w)),
                th.dim().add_modifier(Modifier::BOLD),
            ));
        }
        lines.push(Line::from(head));
        lines.push(Line::from(""));
        for (ri, p) in self.presets.iter().enumerate() {
            let mut spans = vec![Span::styled(
                format!(" {} ", pad(&p.name, name_w)),
                if ri == self.row {
                    th.bold()
                } else {
                    Style::default()
                },
            )];
            for (ci, a) in agents.iter().enumerate() {
                let st = preset_status(ctx.snap, p, std::slice::from_ref(&a.key));
                let (mark, style) = match st.state() {
                    PresetState::Active => ("✓", th.ok()),
                    PresetState::Partial => ("◐", th.warn()),
                    PresetState::Inactive => ("◌", th.dim()),
                    PresetState::Empty => ("◦", th.dim()),
                };
                let text = match st.progress() {
                    Some(pr) => format!("{mark} {pr}"),
                    None => mark.to_string(),
                };
                let mut cell = Style::default().patch(style);
                if ri == self.row && ci == self.col {
                    cell = cell.bg(th.selection_bg).add_modifier(Modifier::BOLD);
                }
                let x = inner.x + 1 + name_w as u16 + 2 + (ci as u16) * (col_w as u16 + 2);
                let y = inner.y + 2 + ri as u16;
                self.cells
                    .push((ri, ci, Rect::new(x, y, col_w as u16 + 2, 1)));
                spans.push(Span::styled(format!(" {} ", pad(&text, col_w)), cell));
            }
            lines.push(Line::from(spans));
        }
        if self.presets.is_empty() {
            lines.push(Line::from(Span::styled(" no presets yet", th.dim())));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(help, th.dim())));
        f.render_widget(Paragraph::new(lines), inner);
    }
}
