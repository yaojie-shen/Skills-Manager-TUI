//! Help for the surface receiving keyboard input. The app alone renders it in
//! fixed bottom row: a clickable message on the left and shortcuts on the right.
//! ContextHelp is an independent, read-only snapshot above every input owner;
//! opening it never replaces the editing modal or its nested overlays.
//! Controls return descriptions and disabled reasons here instead of drawing
//! their own help rows. Data previews, errors and confirmation content stay in
//! their owning panel. Derive help from live focus, never a cached draw result.
//! Fit active actions and Escape before navigation/focus, other panel hints,
//! and optional globals. Keep original order and spelling whenever all fit;
//! narrow forms only remove supported key alternatives or shorten labels.

use std::borrow::Cow;

use super::theme::Theme;
use super::widgets::{
    PopupClass, PopupMouse, PopupSurface, popup_mouse, popup_size, popup_too_small, width,
};
use crossterm::event::{KeyCode, KeyEvent, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    text::Line,
    widgets::{Paragraph, Wrap},
};

/// Shared single-line editing bindings, also available under completion popovers.
pub const INPUT_KEYS: Hints = &[
    ("type / paste", "insert text"),
    ("←/→", "move cursor"),
    ("Ctrl/Alt+←/→", "previous / next word"),
    ("Home/Ctrl-A", "start of line"),
    ("End/Ctrl-E", "end of line"),
    ("Backspace/Delete", "delete previous / next character"),
    ("Ctrl-U", "delete to start"),
    ("Ctrl-K", "delete to end"),
    ("Ctrl-W", "delete previous word"),
];

pub type Hints = &'static [(&'static str, &'static str)];

type Hint = (&'static str, &'static str);

pub fn hint_width((key, desc): Hint) -> usize {
    width(key) + if desc.is_empty() { 0 } else { width(desc) + 3 }
}

fn compact((key, desc): Hint) -> Hint {
    let key = match key {
        "Enter/Space" => "Space",
        "Enter/↓" | "Enter/y" => "Enter",
        "Enter/Esc" | "Enter/Esc/n" | "Esc/q" | "Esc/n" | "Esc/F2" => "Esc",
        "Tab/Shift+Tab" => "Tab",
        "↑↓/j/k" => "↑↓",
        "←→/Tab" => "←→",
        _ => key,
    };
    let desc = match desc {
        "clear/back" | "back to selection" | "back/cancel" => "back",
        "cancel selection" => "cancel",
        "close suggestions" => "close",
        "collapse / expand" => "toggle",
        "fold / unfold all" => "fold/unfold",
        "stage / skip" => "stage/skip",
        "apply / cancel" | "list / buttons" => "buttons",
        "next / previous" => "focus",
        "filter commands" => "filter",
        _ => desc,
    };
    (key, desc)
}

fn priority((key, _): Hint) -> u8 {
    if key.contains("Space") {
        0
    } else if key.contains("Enter") {
        1
    } else if key.contains(['↑', '↓', '←', '→'])
        || key.contains("Tab")
        || key.contains("PgUp")
        || key == "j/k"
        || key == "/"
    {
        2
    } else if matches!(key, ":" | "?" | "Ctrl-G") {
        4
    } else {
        3
    }
}

/// Select hints by importance, then display in their original panel order.
/// An oversized item never prevents a later, shorter item from being shown.
pub fn fit_hints(hints: Hints, globals: &[Hint], tags_enabled: bool, budget: usize) -> Vec<Hint> {
    let escape = hints.iter().copied().find(|(key, _)| key.contains("Esc"));
    let mut candidates: Vec<_> = hints
        .iter()
        .copied()
        .filter(|(key, desc)| {
            (tags_enabled || *key != "t")
                && !key.contains("Esc")
                && !globals.contains(&(*key, *desc))
        })
        .collect();
    let panel_count = candidates.len();
    candidates.extend_from_slice(globals);
    let total =
        candidates.iter().copied().map(hint_width).sum::<usize>() + escape.map_or(0, hint_width);
    if total <= budget {
        if let Some(escape) = escape {
            candidates.insert(panel_count, escape);
        }
        return candidates;
    }
    let panel_width = candidates[..panel_count]
        .iter()
        .copied()
        .map(hint_width)
        .sum::<usize>()
        + escape.map_or(0, hint_width);
    if panel_width <= budget {
        let mut result = candidates[..panel_count].to_vec();
        result.extend(escape);
        let mut remaining = budget - panel_width;
        for &hint in globals {
            if hint_width(hint) <= remaining {
                remaining -= hint_width(hint);
                result.push(hint);
            }
        }
        return result;
    }

    let mut order: Vec<_> = (0..panel_count).collect();
    order.sort_by_key(|&index| priority(candidates[index]));
    order.extend(panel_count..candidates.len());
    let primary_width = order
        .first()
        .filter(|&&index| priority(candidates[index]) <= 1)
        .map_or(0, |&index| hint_width(compact(candidates[index])));
    let mut fitted_escape = escape.and_then(|hint| {
        [compact(hint), ("Esc", "")]
            .into_iter()
            .find(|&form| hint_width(form) + primary_width <= budget)
            .or_else(|| {
                [compact(hint), ("Esc", "")]
                    .into_iter()
                    .find(|&form| hint_width(form) <= budget)
            })
    });
    let mut remaining = budget.saturating_sub(fitted_escape.map_or(0, hint_width));
    let mut selected = vec![None; candidates.len()];
    for index in order {
        let form = compact(candidates[index]);
        if hint_width(form) <= remaining {
            remaining -= hint_width(form);
            selected[index] = Some(form);
        }
    }
    // Spend spare columns on original labels only after selecting controls.
    for (index, selected) in selected.iter_mut().enumerate() {
        if let Some(form) = selected {
            let extra = hint_width(candidates[index]).saturating_sub(hint_width(*form));
            if extra <= remaining {
                remaining -= extra;
                *form = candidates[index];
            }
        }
    }
    if let (Some(original), Some(form)) = (escape, fitted_escape) {
        let extra = hint_width(original).saturating_sub(hint_width(form));
        if extra <= remaining {
            fitted_escape = Some(original);
        }
    }
    let mut result: Vec<_> = selected[..panel_count].iter().flatten().copied().collect();
    result.extend(fitted_escape);
    result.extend(selected[panel_count..].iter().flatten().copied());
    result
}

pub struct Help<'a> {
    pub keys: Hints,
    pub detail: Cow<'a, str>,
}

impl<'a> Help<'a> {
    pub fn new(keys: Hints) -> Self {
        Self {
            keys,
            detail: Cow::Borrowed(""),
        }
    }

    pub fn detail(mut self, detail: impl Into<Cow<'a, str>>) -> Self {
        self.detail = detail.into();
        self
    }
}

/// An immutable snapshot of the owner that was focused when help was opened.
/// Only viewport state changes while background work continues independently.
pub struct ContextHelp {
    pub lines: Vec<String>,
    pub scroll: u16,
    rect: Rect,
    visible: u16,
    total: u16,
}

pub const CONTEXT_KEYS: Hints = &[
    ("↑↓/j/k", "scroll"),
    ("PgUp/PgDn", "page"),
    ("Home/End", "first/last"),
    ("Enter/Esc", "close"),
];

impl ContextHelp {
    pub fn new(lines: Vec<String>) -> Self {
        Self {
            lines,
            scroll: 0,
            rect: Rect::default(),
            visible: 1,
            total: 0,
        }
    }

    /// Returns true to close. Every other key remains owned by this layer.
    pub fn key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Esc | KeyCode::Enter => return true,
            KeyCode::Down | KeyCode::Char('j') => self.scroll = self.scroll.saturating_add(1),
            KeyCode::Up | KeyCode::Char('k') => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::PageDown => self.scroll = self.scroll.saturating_add(self.visible.max(1)),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(self.visible.max(1)),
            KeyCode::Home => self.scroll = 0,
            KeyCode::End => self.scroll = self.total,
            _ => {}
        }
        false
    }

    pub fn mouse(&mut self, event: MouseEvent, wheel_rows: i32) -> bool {
        match popup_mouse(event, self.rect) {
            PopupMouse::OutsidePress => return true,
            PopupMouse::Inside => match event.kind {
                MouseEventKind::ScrollDown => {
                    self.scroll = self.scroll.saturating_add(wheel_rows.max(0) as u16)
                }
                MouseEventKind::ScrollUp => {
                    self.scroll = self.scroll.saturating_sub(wheel_rows.max(0) as u16)
                }
                _ => {}
            },
            _ => {}
        }
        false
    }

    pub fn draw(&mut self, f: &mut Frame, area: Rect, theme: &Theme) {
        let content_width = popup_size(area, PopupClass::Preview, 3, (1, 1)).inner.width;
        let paragraph = Paragraph::new(
            self.lines
                .iter()
                .map(|line| Line::from(line.as_str()))
                .collect::<Vec<_>>(),
        )
        .wrap(Wrap { trim: false });
        self.total = paragraph
            .line_count(content_width.max(1))
            .min(u16::MAX as usize) as u16;
        let size = popup_size(
            area,
            PopupClass::Preview,
            self.total.saturating_add(2),
            (1, 1),
        );
        self.rect = size.outer;
        self.visible = size.inner.height;
        self.scroll = self.scroll.min(self.total.saturating_sub(self.visible));
        f.render_widget(PopupSurface, size.outer);
        f.render_widget(
            paragraph.scroll((self.scroll, 0)).block(theme.block(
                format!(
                    " context help · {}–{}/{} ",
                    self.scroll.saturating_add(1).min(self.total),
                    self.scroll.saturating_add(self.visible).min(self.total),
                    self.total
                ),
                true,
            )),
            size.outer,
        );
        if !size.usable {
            popup_too_small(f, size, theme);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_items_do_not_hide_later_shorter_controls_or_globals() {
        let fitted = fit_hints(
            &[
                ("Enter", "run"),
                ("type", "a very long filter description"),
                ("↑↓", "move"),
                ("Esc", "close"),
            ],
            &[(":", "commands"), ("?", "help")],
            true,
            40,
        );
        assert_eq!(
            fitted,
            [
                ("Enter", "run"),
                ("↑↓", "move"),
                ("Esc", "close"),
                ("?", "help"),
            ]
        );
    }

    #[test]
    fn stage_action_outranks_resolve_and_keeps_navigation_before_globals() {
        let hints: Hints = &[
            ("↑↓", "review"),
            ("Enter", "resolve"),
            ("Space", "stage / skip"),
            ("Tab/Shift+Tab", "buttons"),
            ("Esc", "cancel"),
        ];
        let fitted = fit_hints(hints, &[("?", "help")], true, 44);
        assert!(
            fitted.contains(&("Space", "stage/skip"))
                || fitted.contains(&("Space", "stage / skip"))
        );
        assert!(fitted.contains(&("Esc", "cancel")));
        assert!(fitted.contains(&("↑↓", "review")));
        assert!(!fitted.contains(&("Enter", "resolve")));
        assert!(!fitted.contains(&("?", "help")));
        assert!(fitted.iter().copied().map(hint_width).sum::<usize>() <= 44);
        assert_eq!(fit_hints(hints, &[], true, 120), hints);
    }

    #[test]
    fn tiny_budgets_show_only_complete_truthful_keys() {
        for budget in 0..16 {
            let fitted = fit_hints(&[("Enter/Esc/n", "cancel")], &[], true, budget);
            assert!(fitted.iter().copied().map(hint_width).sum::<usize>() <= budget);
            if budget < 3 {
                assert!(fitted.is_empty());
            } else {
                assert!(fitted.iter().any(|(key, _)| key.contains("Esc")));
            }
        }
    }

    #[test]
    fn compact_alternatives_allow_primary_and_escape_to_fit_together() {
        assert_eq!(
            fit_hints(
                &[("Enter/Space", "toggle"), ("Esc/q", "clear/back")],
                &[],
                true,
                24,
            ),
            [("Space", "toggle"), ("Esc", "back")]
        );
    }
}
