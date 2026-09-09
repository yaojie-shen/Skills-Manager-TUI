//! The preset × agent table, as a window over a page.
//!
//! The Presets page defines what a preset holds and the Agents page switches
//! presets for one agent at a time; neither shows the whole picture at once.
//! This table does, in one row per preset, and each cell is the same switch
//! the pills are. It is a window rather than a tab because it adds nothing to
//! define or configure — it only lets the switches be seen and thrown together.

use crate::tui::app::{Action, Ctx};
use crate::tui::widgets::OverlayClear as Clear;
use crate::tui::widgets::{pad, width};
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
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
    row_offset: usize,
    col_offset: usize,
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

    pub fn hints(&self) -> Option<crate::tui::app::Hints> {
        self.open.then_some(&[
            ("↑↓←→", "cell"),
            ("Enter/Space", "toggle"),
            ("A", "toggle row"),
            ("Esc", "close"),
        ])
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
            KeyCode::Right | KeyCode::Char('l') => {
                self.col = (self.col + 1).min(cols.saturating_sub(1));
                vec![]
            }
            KeyCode::Left | KeyCode::Char('h') => {
                self.col = self.col.saturating_sub(1);
                vec![]
            }
            KeyCode::Char(' ') | KeyCode::Enter if self.selection_visible() => {
                self.toggle(ctx, self.row, self.col)
            }
            KeyCode::Char('A') if self.selection_visible() => self.toggle_row(ctx, self.row),
            _ => vec![],
        })
    }

    fn selection_visible(&self) -> bool {
        self.cells
            .iter()
            .any(|(r, c, _)| *r == self.row && *c == self.col)
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
            .clamp(8, 20);
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

        let name_w = name_w.min((inner.width as usize).saturating_sub(12));
        let col_w = col_w.min((inner.width as usize).saturating_sub(name_w + 4));
        let visible_cols = (inner.width as usize).saturating_sub(name_w + 2) / (col_w + 2);
        let visible_rows = inner.height.saturating_sub(4) as usize;
        let rows = viewport(
            self.presets.len(),
            self.row,
            &mut self.row_offset,
            visible_rows,
        );
        let cols = viewport(agents.len(), self.col, &mut self.col_offset, visible_cols);
        let mut lines: Vec<Line> = Vec::new();
        let mut head = vec![Span::raw(format!(" {} ", pad("", name_w)))];
        for a in &agents[cols.clone()] {
            head.push(Span::styled(
                format!(" {} ", pad(&a.key, col_w)),
                th.dim().add_modifier(Modifier::BOLD),
            ));
        }
        lines.push(Line::from(head));
        lines.push(Line::from(""));
        for ri in rows.clone() {
            let p = &self.presets[ri];
            let mut spans = vec![Span::styled(
                format!(" {} ", pad(&p.name, name_w)),
                if ri == self.row {
                    th.bold()
                } else {
                    Style::default()
                },
            )];
            for ci in cols.clone() {
                let a = &agents[ci];
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
                let x =
                    inner.x + name_w as u16 + 2 + ((ci - cols.start) as u16) * (col_w as u16 + 2);
                let y = inner.y + 2 + (ri - rows.start) as u16;
                self.cells
                    .push((ri, ci, Rect::new(x, y, col_w as u16 + 2, 1)));
                spans.push(Span::styled(format!(" {} ", pad(&text, col_w)), cell));
            }
            lines.push(Line::from(spans));
        }
        if self.presets.is_empty() {
            lines.push(Line::from(Span::styled(" no presets yet", th.dim())));
        }
        let position = format!(
            " ↑↓ rows {}–{}/{} · ←→ cols {}–{}/{}",
            if rows.is_empty() { 0 } else { rows.start + 1 },
            rows.end,
            self.presets.len(),
            if cols.is_empty() { 0 } else { cols.start + 1 },
            cols.end,
            agents.len()
        );
        lines.push(Line::from(Span::styled(position, th.dim())));
        lines.push(Line::from(Span::styled(help, th.dim())));
        f.render_widget(Paragraph::new(lines), inner);
    }
}

fn viewport(
    total: usize,
    selected: usize,
    offset: &mut usize,
    capacity: usize,
) -> std::ops::Range<usize> {
    if total == 0 || capacity == 0 {
        *offset = 0;
        return 0..0;
    }
    let selected = selected.min(total - 1);
    *offset = (*offset).min(selected).min(total.saturating_sub(capacity));
    if selected >= *offset + capacity {
        *offset = selected + 1 - capacity;
    }
    *offset..(*offset + capacity).min(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::theme::Theme;
    use crossterm::event::KeyModifiers;
    use ratatui::{Terminal, backend::TestBackend};
    use skills::{Workspace, config::AgentConfig};

    #[test]
    fn selection_and_mouse_targets_stay_inside_scrolling_matrix() {
        let root = std::env::temp_dir().join(format!("skills-matrix-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let mut ws = Workspace::open(&root).unwrap();
        ws.config.agents = (0..12)
            .map(|i| AgentConfig {
                key: format!("agent-{i:02}-long-name"),
                name: format!("Agent {i}"),
                skills_dir: format!("../agent-{i}"),
            })
            .collect();
        let snap = ws.scan().unwrap();
        let theme = Theme::default();
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            theme: &theme,
        };
        let mut matrix = Matrix {
            open: true,
            presets: (0..40)
                .map(|i| Preset {
                    name: format!("group-{i:02}-long-name-文件系统"),
                    ..Preset::default()
                })
                .collect(),
            ..Matrix::default()
        };
        matrix.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE), &ctx);
        assert_eq!(matrix.col, 1);
        matrix.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE), &ctx);
        assert_eq!(matrix.col, 0);

        for (w, h) in [(100, 30), (80, 24), (120, 40)] {
            let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
            for (r, c) in [(0, 0), (39, 11), (20, 5), (0, 0)] {
                matrix.row = r;
                matrix.col = c;
                terminal.draw(|f| matrix.draw(f, f.area(), &ctx)).unwrap();
                assert!(matrix.selection_visible());
                let inner = theme.block("", true).inner(matrix.rect);
                for (_, _, cell) in &matrix.cells {
                    assert!(inner.contains((cell.x, cell.y).into()));
                    assert!(cell.right() <= inner.right());
                    assert!(cell.bottom() <= inner.bottom() - 2);
                }
                let (_, _, selected) = matrix
                    .cells
                    .iter()
                    .find(|(ri, ci, _)| *ri == r && *ci == c)
                    .unwrap();
                assert_eq!(
                    terminal.backend().buffer()[(selected.x, selected.y)].bg,
                    theme.selection_bg
                );
            }
            // A queued navigation key cannot activate a not-yet-painted cell.
            matrix.row = 39;
            matrix.col = 11;
            assert!(
                matrix
                    .handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &ctx)
                    .unwrap()
                    .is_empty()
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
