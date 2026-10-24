//! One explicit keep/none decision per conflicting name, staged until Apply.
use super::components::choice_footer::{self, ChoiceEvent, ChoiceFocus};
use super::{
    app::{Action, Ctx, Hints},
    modal::Modal,
    widgets::{ListNav, PopupClass, PopupSurface, fit, popup_size, popup_too_small},
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
    cancel_rect: Rect,
    focus: ChoiceFocus,
    previous_rect: Rect,
    next_rect: Rect,
    rect: Rect,
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
            cancel_rect: Rect::default(),
            focus: ChoiceFocus::List,
            previous_rect: Rect::default(),
            next_rect: Rect::default(),
            rect: Rect::default(),
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
        }).chain(std::iter::once("Excluded agent-owned folders are archived in .skills-manager-backups beside the skills directory; Library source skills are kept. Archive operations are not undoable in this session.".into())).collect();
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
        if let Some(hints) = self.focus.button_hints() {
            return hints;
        }

        &[
            ("↑↓", "option"),
            ("Enter/Space", "choose"),
            ("←→", "conflict"),
            ("Tab/Shift+Tab", "list / buttons"),
            ("Esc", "cancel"),
        ]
    }
    pub fn footer_help(&self) -> crate::tui::footer::Help<'_> {
        self.focus.help(
            self.hints(),
            "Choose one or none for each name; changes are staged until confirmed",
            if self.choices.iter().all(Option::is_some) {
                "Review the selected name choices before applying"
            } else {
                "Choose every conflicting name before applying"
            },
            "Discard the staged choices and close",
        )
    }

    pub fn key(&mut self, key: KeyEvent) -> Vec<Action> {
        if matches!(key.code, KeyCode::Esc | KeyCode::Char('q')) {
            return vec![Action::CloseModal];
        }
        let at_end = self.list.selected() == Some(self.len() - 1);
        if let Some(event) = self.focus.key(key.code, at_end) {
            return match event {
                ChoiceEvent::Apply => {
                    if self.choices.iter().all(Option::is_some) {
                        self.apply()
                    } else {
                        vec![]
                    }
                }
                ChoiceEvent::Cancel => vec![Action::CloseModal],
                ChoiceEvent::Moved => vec![],
            };
        }
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
        match super::widgets::popup_mouse(event, self.rect) {
            super::widgets::PopupMouse::OutsidePress => return vec![Action::CloseModal],
            super::widgets::PopupMouse::Inside => {}
            _ => return vec![],
        }
        if matches!(
            event.kind,
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
        ) && self.list.rows.contains((event.column, event.row).into())
        {
            self.focus = ChoiceFocus::List;
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
                self.focus = ChoiceFocus::List;
                self.move_group(-1);
                return vec![];
            }
            if self.next_rect.contains((event.column, event.row).into()) {
                self.focus = ChoiceFocus::List;
                self.move_group(1);
                return vec![];
            }
            if self.cancel_rect.contains((event.column, event.row).into()) {
                self.focus = ChoiceFocus::Cancel;
                return vec![Action::CloseModal];
            }
            if self.apply_rect.contains((event.column, event.row).into()) {
                self.focus = ChoiceFocus::Apply;
                return if self.choices.iter().all(Option::is_some) {
                    self.apply()
                } else {
                    vec![]
                };
            }
            if self.list.rows.contains((event.column, event.row).into())
                && let Some(index) = self.list.row_at(event.row, self.len())
            {
                self.focus = ChoiceFocus::List;
                self.list.select(Some(index));
                self.choose();
            }
        }
        vec![]
    }
    pub fn draw(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        let height = (self.len().min(17) as u16).saturating_add(7);
        let size = popup_size(area, PopupClass::Choice, height, (12, 3));
        let rect = size.outer;
        self.rect = rect;
        f.render_widget(PopupSurface, rect);
        let group = &self.pending.groups[self.group];
        let block = ctx.settings.theme.block(
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
        self.cancel_rect = Rect::default();
        self.previous_rect = Rect::default();
        self.next_rect = Rect::default();
        if !size.usable {
            popup_too_small(f, size, &ctx.settings.theme);
            return;
        }
        let description_height = if inner.height < 8 { 1 } else { 3 };
        f.render_widget(
            Paragraph::new(format!(
                "Name: {}\nTarget: {}",
                group.name,
                group.directory.display()
            ))
            .wrap(Wrap { trim: false }),
            Rect::new(inner.x, inner.y, inner.width, description_height),
        );
        self.list.rows = Rect::new(
            inner.x,
            inner.y + description_height,
            inner.width,
            inner.height.saturating_sub(description_height + 1),
        );
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
            List::new(rows).highlight_style(if self.focus == ChoiceFocus::List {
                ctx.settings.theme.selected()
            } else {
                ctx.settings.theme.dim()
            }),
            self.list.rows,
            &mut self.list.state,
        );
        let enabled = self.choices.iter().all(Option::is_some);
        let navigation_width = if inner.width >= 56 && self.pending.groups.len() > 1 {
            let rects = crate::tui::widgets::draw_button_row(
                f,
                Rect::new(inner.x, inner.bottom() - 1, 24, 1),
                &[
                    crate::tui::widgets::ButtonSpec::new("Previous"),
                    crate::tui::widgets::ButtonSpec::new("Next"),
                ],
                None,
                &ctx.settings.theme,
            );
            self.previous_rect = rects[0];
            self.next_rect = rects[1];
            26
        } else {
            0
        };
        let rects = choice_footer::draw(
            f,
            Rect::new(
                inner.x + navigation_width,
                inner.bottom() - 1,
                inner.width.saturating_sub(navigation_width),
                1,
            ),
            self.focus,
            enabled,
            "",
            &ctx.settings.theme,
        );
        self.apply_rect = rects[0];
        self.cancel_rect = rects[1];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use skills::ops::name_choices::{Candidate, Group};
    #[test]
    fn responsive_name_choices_keep_selection_and_actions() {
        let tmp = skills::ops::DownloadDir::new("name-choice-size").unwrap();
        skills::config::Config {
            agents: vec![],
            ..Default::default()
        }
        .save(tmp.path())
        .unwrap();
        let ws = skills::Workspace::open(tmp.path()).unwrap();
        let snap = ws.scan().unwrap();
        let settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &settings,
        };
        let mut picker = NameChoices::new(Pending {
            changes: vec![],
            groups: vec![Group {
                name: "duplicate".into(),
                directory: "/synthetic/agent".into(),
                candidates: vec![Candidate {
                    key: Some("example".into()),
                    path: "/synthetic/example".into(),
                    archive_hash: None,
                }],
            }],
        });
        picker.rect = Rect::new(10, 5, 60, 18);
        picker.list.rows = Rect::new(11, 8, 58, 2);
        let event = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 12,
            row: 8,
            modifiers: crossterm::event::KeyModifiers::NONE,
        };
        assert!(picker.mouse(event).is_empty());
        assert_eq!(picker.choices, vec![Some(0)]);
        for button in [MouseButton::Left, MouseButton::Right, MouseButton::Middle] {
            assert!(matches!(
                picker
                    .mouse(MouseEvent {
                        kind: MouseEventKind::Down(button),
                        column: 0,
                        row: 0,
                        ..event
                    })
                    .as_slice(),
                [Action::CloseModal]
            ));
        }
        for (width, height) in [(160, 40), (112, 32), (60, 20), (40, 12), (4, 3), (0, 0)] {
            let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(
                width.max(1),
                height.max(1),
            ))
            .unwrap();
            let area = Rect::new(0, 0, width, height.saturating_sub(2));
            terminal
                .draw(|frame| picker.draw(frame, area, &ctx))
                .unwrap();
            for hit in [picker.list.rows, picker.apply_rect, picker.cancel_rect] {
                assert!(hit.is_empty() || hit.intersection(area) == hit);
            }
            if width == 40 {
                let text: String = terminal
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .map(|cell| cell.symbol())
                    .collect();
                for label in ["Keep example", "Apply", "Cancel"] {
                    assert!(text.contains(label), "{text}");
                }
                assert!(picker.list.rows.height > 0);
            }
            assert_eq!(picker.choices, [Some(0)]);
        }
    }

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
