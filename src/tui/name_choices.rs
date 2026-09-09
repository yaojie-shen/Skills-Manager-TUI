//! One explicit keep/none decision per conflicting name, staged until Apply.
use super::{
    app::{Action, Ctx, Hints},
    modal::Modal,
    widgets::{ListNav, OverlayClear, fit},
};
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    widgets::{List, ListItem, Paragraph, Wrap},
};
use skills::ops::name_choices::Pending;

pub struct NameChoices {
    pending: Pending,
    choices: Vec<Option<usize>>,
    group: usize,
    list: ListNav,
    apply_rect: Rect,
    previous_rect: Rect,
    next_rect: Rect,
}
impl NameChoices {
    pub fn new(pending: Pending) -> Self {
        let choices = vec![None; pending.groups.len()];
        let mut out = Self {
            pending,
            choices,
            group: 0,
            list: ListNav::default(),
            apply_rect: Rect::default(),
            previous_rect: Rect::default(),
            next_rect: Rect::default(),
        };
        out.list.first(out.len());
        out
    }
    fn len(&self) -> usize {
        self.pending.groups[self.group].candidates.len() + 1
    }
    fn choose(&mut self) {
        self.choices[self.group] = self.list.selected();
    }
    fn move_group(&mut self, delta: i32) {
        self.group =
            (self.group as i32 + delta).rem_euclid(self.pending.groups.len() as i32) as usize;
        self.list
            .select(Some(self.choices[self.group].unwrap_or(0)));
    }
    fn apply(&self) -> Vec<Action> {
        if self.choices.iter().any(Option::is_none) {
            return vec![Action::Error(
                "Choose every conflicting name before applying".into(),
            )];
        }
        let pending = self.pending.clone();
        let choices: Vec<_> = self
            .choices
            .iter()
            .zip(&pending.groups)
            .map(|(choice, group)| choice.filter(|i| *i < group.candidates.len()))
            .collect();
        let summary = pending.groups.iter().zip(&choices).map(|(group, choice)| {
            format!("{}: {}", group.name, choice.map(|i| format!("keep {}", group.candidates[i].path.display())).unwrap_or_else(|| "keep none".into()))
        }).chain(std::iter::once("Excluded agent-owned folders are archived in .skills-manager-backups beside the skills directory; managed source skills are kept. Archive operations are not undoable in this session.".into())).collect();
        let keys = pending.keys();
        vec![
            Action::OpenModal(Box::new(
                Modal::confirm_meta(
                    "Apply name choices".into(),
                    summary,
                    Box::new(move |ws| pending.apply(ws, &choices)),
                )
                .in_background(keys.clone()),
            )),
            Action::Toast(format!("{} selected skills await confirmation", keys.len())),
        ]
    }
    pub fn hints(&self) -> Hints {
        &[
            ("↑↓", "option"),
            ("Enter/Space", "choose"),
            ("←→", "conflict"),
            ("a", "review / apply"),
            ("Esc", "cancel"),
        ]
    }
    pub fn key(&mut self, key: KeyEvent) -> Vec<Action> {
        match key.code {
            KeyCode::Esc => return vec![Action::CloseModal],
            KeyCode::Up => self.list.move_by(-1, self.len()),
            KeyCode::Down => self.list.move_by(1, self.len()),
            KeyCode::Left => self.move_group(-1),
            KeyCode::Right => self.move_group(1),
            KeyCode::Enter | KeyCode::Char(' ') => self.choose(),
            KeyCode::Char('a') => return self.apply(),
            _ => {}
        }
        vec![]
    }
    pub fn mouse(&mut self, event: MouseEvent) -> Vec<Action> {
        if matches!(
            event.kind,
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
        ) && self.list.rows.contains((event.column, event.row).into())
        {
            self.list.move_by(
                if event.kind == MouseEventKind::ScrollUp {
                    -1
                } else {
                    1
                },
                self.len(),
            );
        }
        if event.kind == MouseEventKind::Down(MouseButton::Left) {
            if self
                .previous_rect
                .contains((event.column, event.row).into())
            {
                self.move_group(-1);
                return vec![];
            }
            if self.next_rect.contains((event.column, event.row).into()) {
                self.move_group(1);
                return vec![];
            }
            if self.apply_rect.contains((event.column, event.row).into()) {
                return self.apply();
            }
            if self.list.rows.contains((event.column, event.row).into())
                && let Some(index) = self.list.row_at(event.row, self.len())
            {
                self.list.select(Some(index));
                self.choose();
            }
        }
        vec![]
    }
    pub fn draw(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        let width = area.width.saturating_sub(2).min(110);
        let height = area.height.saturating_sub(2).min(24);
        let rect = Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 2,
            width,
            height,
        );
        f.render_widget(OverlayClear, rect);
        let group = &self.pending.groups[self.group];
        let block = ctx.theme.block(
            format!(
                " Name conflict · {}/{} ",
                self.group + 1,
                self.pending.groups.len()
            ),
            true,
        );
        let inner = block.inner(rect);
        f.render_widget(block, rect);
        self.list.rows = Rect::default();
        self.apply_rect = Rect::default();
        self.previous_rect = Rect::default();
        self.next_rect = Rect::default();
        if inner.height < 8 {
            f.render_widget(Paragraph::new("Enlarge terminal · Esc cancel"), inner);
            return;
        }
        f.render_widget(
            Paragraph::new(format!(
                "Name: {}\nTarget: {}",
                group.name,
                group.directory.display()
            ))
            .wrap(Wrap { trim: false }),
            Rect::new(inner.x, inner.y, inner.width, 3),
        );
        self.list.rows = Rect::new(inner.x, inner.y + 3, inner.width, inner.height - 6);
        let rows = group
            .candidates
            .iter()
            .enumerate()
            .map(|(i, candidate)| {
                ListItem::new(fit(
                    &format!(
                        "[{}] Keep {} · {}{}",
                        if self.choices[self.group] == Some(i) {
                            "✓"
                        } else {
                            " "
                        },
                        candidate.key.as_deref().unwrap_or("agent-owned entry"),
                        candidate.path.display(),
                        if candidate.archive_hash.is_some() {
                            " (others may be archived)"
                        } else {
                            ""
                        }
                    ),
                    inner.width as usize,
                ))
            })
            .chain(std::iter::once(ListItem::new(format!(
                "[{}] Keep none",
                if self.choices[self.group] == Some(group.candidates.len()) {
                    "✓"
                } else {
                    " "
                }
            ))))
            .collect::<Vec<_>>();
        f.render_stateful_widget(
            List::new(rows).highlight_style(ctx.theme.selected()),
            self.list.rows,
            &mut self.list.state,
        );
        f.render_widget(
            Paragraph::new("Choose one or none for each name. Nothing changes until confirmed.")
                .style(ctx.theme.dim()),
            Rect::new(inner.x, inner.bottom() - 2, inner.width, 1),
        );
        self.apply_rect = Rect::new(inner.x, inner.bottom() - 1, 18.min(inner.width), 1);
        if inner.width >= 48 && self.pending.groups.len() > 1 {
            self.previous_rect = Rect::new(inner.x + 20, inner.bottom() - 1, 12, 1);
            self.next_rect = Rect::new(inner.x + 34, inner.bottom() - 1, 10, 1);
            f.render_widget(
                Paragraph::new("[ Previous ]").style(ctx.theme.accent()),
                self.previous_rect,
            );
            f.render_widget(
                Paragraph::new("[ Next ]").style(ctx.theme.accent()),
                self.next_rect,
            );
        }
        f.render_widget(
            Paragraph::new("[ Review / Apply ]").style(ctx.theme.accent()),
            self.apply_rect,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use skills::ops::name_choices::{Candidate, Group};
    #[test]
    fn every_group_requires_an_explicit_choice_before_confirmation() {
        let group = Group {
            name: "duplicate".into(),
            directory: "/tmp/example/skills".into(),
            candidates: vec![Candidate {
                key: Some("first".into()),
                path: "/tmp/example/skills/first".into(),
                archive_hash: None,
            }],
        };
        let mut picker = NameChoices::new(Pending {
            changes: vec![],
            groups: vec![group.clone(), group],
        });
        assert!(matches!(picker.apply().as_slice(), [Action::Error(_)]));
        picker.choose();
        picker.move_group(1);
        picker.list.move_by(1, picker.len());
        picker.choose();
        assert_eq!(picker.choices, vec![Some(0), Some(1)]);
        let actions = picker.apply();
        assert!(matches!(actions.first(), Some(Action::OpenModal(_))));
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, Action::BatchMeta(..) | Action::WriteMeta(..)))
        );
        assert!(matches!(
            picker
                .key(KeyEvent::new(
                    KeyCode::Esc,
                    crossterm::event::KeyModifiers::NONE
                ))
                .as_slice(),
            [Action::CloseModal]
        ));
    }
}
