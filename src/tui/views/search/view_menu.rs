//! Compact, categorized controls for the session-only Library organization view.
//!
//! The popup shows one category at a time. Its state and rows are derived from
//! the current snapshot-backed SearchView model; it never reads or writes disk.
//! Filled tabs identify the category, a pointer and row highlight identify the
//! keyboard choice, and independently coloured markers identify applied settings.
//! Navigating settings applies them immediately without closing the popup;
//! Enter only dismisses the already-applied settings. Shortcut hints belong to
//! the app footer. Tabs lead directly to a subdued divider and choices; only
//! unavailable choices carry an inline explanation when space permits.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use skills::config::{Icons, UiLayout};

use crate::tui::theme::Theme;
use crate::tui::widgets::{OverlayClear, fit, width};

use super::organization::{GroupMode, GroupOrder, SortMode};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) enum Section {
    #[default]
    Layout,
    Group,
    Sort,
    Order,
}

impl Section {
    const ALL: [Self; 4] = [Self::Layout, Self::Group, Self::Sort, Self::Order];
    const MAX_ROWS: u16 = 6;

    fn title(self) -> &'static str {
        match self {
            Self::Layout => "Layout",
            Self::Group => "Group by",
            Self::Sort => "Sort",
            Self::Order => "Group order",
        }
    }

    fn icon(self, icons: Icons) -> &'static str {
        match (self, icons) {
            (Self::Layout, Icons::Nerd) => "\u{f0570}",
            (Self::Group, Icons::Nerd) => "\u{f0253}",
            (Self::Sort, Icons::Nerd) => "\u{f04ba}",
            (Self::Order, Icons::Nerd) => "\u{f04bf}",
            (_, Icons::Text) => "",
        }
    }

    fn tab_width(self, icons: Icons) -> u16 {
        let icon = self.icon(icons);
        let icon_width = width(icon) + usize::from(!icon.is_empty());
        (width(self.title()) + icon_width + 2) as u16
    }

    fn shifted(self, delta: i32, grouping: GroupMode, searching: bool, tags_enabled: bool) -> Self {
        let index = Self::ALL
            .iter()
            .position(|section| *section == self)
            .unwrap_or(0) as i32;
        for distance in 1..=Self::ALL.len() {
            let candidate = Self::ALL
                [(index + delta * distance as i32).rem_euclid(Self::ALL.len() as i32) as usize];
            if candidate.enabled(grouping, searching, tags_enabled) {
                return candidate;
            }
        }
        self
    }

    fn enabled(self, grouping: GroupMode, searching: bool, tags_enabled: bool) -> bool {
        self != Self::Order || grouping.effective(searching, tags_enabled) != GroupMode::None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Choice {
    Layout(UiLayout),
    Group(GroupMode),
    Sort(SortMode),
    Order(GroupOrder),
}

impl Choice {
    fn section(self) -> Section {
        match self {
            Self::Layout(_) => Section::Layout,
            Self::Group(_) => Section::Group,
            Self::Sort(_) => Section::Sort,
            Self::Order(_) => Section::Order,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct MenuRow {
    label: &'static str,
    choice: Choice,
    current: bool,
    enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Event {
    Stay,
    Close,
    Change(Choice),
}

#[derive(Debug, Default)]
pub(super) struct ViewMenu {
    open: bool,
    section: Section,
    selected: usize,
    offset: usize,
    pub(super) rect: Rect,
    tab_rects: Vec<(Rect, Section)>,
    row_rects: Vec<(Rect, usize)>,
}

impl ViewMenu {
    pub(super) fn is_open(&self) -> bool {
        self.open
    }

    pub(super) fn section(&self) -> Section {
        self.section
    }

    #[cfg(test)]
    pub(super) fn first_row_y(&self) -> Option<u16> {
        self.row_rects.first().map(|(rect, _)| rect.y)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn open(
        &mut self,
        section: Section,
        layout: UiLayout,
        grouping: GroupMode,
        sorting: SortMode,
        order: GroupOrder,
        tags_enabled: bool,
        searching: bool,
    ) {
        self.open = true;
        self.section = if section.enabled(grouping, searching, tags_enabled) {
            section
        } else {
            Section::Layout
        };
        self.offset = 0;
        let rows = menu_rows(layout, grouping, sorting, order, tags_enabled, searching);
        self.select_default(&rows);
    }

    pub(super) fn close(&mut self) {
        self.open = false;
        self.rect = Rect::default();
        self.tab_rects.clear();
        self.row_rects.clear();
    }

    pub(super) fn key(&mut self, key: KeyEvent, rows: &[MenuRow]) -> Event {
        let previous = self.selected;
        match key.code {
            KeyCode::Esc | KeyCode::F(2) => {
                self.close();
                return Event::Close;
            }
            KeyCode::Left if key.modifiers.is_empty() => {
                self.switch_section(-1, rows);
                return Event::Stay;
            }
            KeyCode::Right if key.modifiers.is_empty() => {
                self.switch_section(1, rows);
                return Event::Stay;
            }
            KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.switch_section(-1, rows);
                return Event::Stay;
            }
            KeyCode::Tab => {
                self.switch_section(1, rows);
                return Event::Stay;
            }
            KeyCode::BackTab => {
                self.switch_section(-1, rows);
                return Event::Stay;
            }
            KeyCode::Up => {
                self.move_by(-1, rows);
            }
            KeyCode::Down => {
                self.move_by(1, rows);
            }
            KeyCode::Char('k') if key.modifiers.is_empty() => {
                self.move_by(-1, rows);
            }
            KeyCode::Char('j') if key.modifiers.is_empty() => {
                self.move_by(1, rows);
            }
            KeyCode::Home => {
                let first = { self.active_indices(rows).next() };
                if let Some(index) = first {
                    self.selected = index;
                }
            }
            KeyCode::End => {
                let last = { self.active_indices(rows).next_back() };
                if let Some(index) = last {
                    self.selected = index;
                }
            }
            KeyCode::PageUp => {
                for _ in 0..5 {
                    self.move_by(-1, rows);
                }
            }
            KeyCode::PageDown => {
                for _ in 0..5 {
                    self.move_by(1, rows);
                }
            }
            KeyCode::Enter if key.modifiers.is_empty() => {
                self.close();
                return Event::Close;
            }
            _ => return Event::Stay,
        }
        self.selection_change(previous, rows)
    }

    fn selection_change(&self, previous: usize, rows: &[MenuRow]) -> Event {
        if self.selected == previous {
            return Event::Stay;
        }
        rows.get(self.selected)
            .filter(|row| row.enabled && !row.current && row.choice.section() == self.section)
            .map_or(Event::Stay, |row| Event::Change(row.choice))
    }

    fn active_indices<'a>(
        &'a self,
        rows: &'a [MenuRow],
    ) -> impl DoubleEndedIterator<Item = usize> + 'a {
        rows.iter()
            .enumerate()
            .filter(|(_, row)| row.choice.section() == self.section && row.enabled)
            .map(|(index, _)| index)
    }

    fn select_default(&mut self, rows: &[MenuRow]) {
        self.selected = rows
            .iter()
            .position(|row| row.choice.section() == self.section && row.current && row.enabled)
            .or_else(|| self.active_indices(rows).next())
            .unwrap_or(0);
    }

    fn switch_section(&mut self, delta: i32, rows: &[MenuRow]) {
        let grouping = grouping(rows);
        let searching = relevance_enabled(rows);
        let tags_enabled = tag_available(rows);
        self.section = self
            .section
            .shifted(delta, grouping, searching, tags_enabled);
        self.offset = 0;
        self.select_default(rows);
    }

    fn move_by(&mut self, delta: i32, rows: &[MenuRow]) {
        let active: Vec<_> = self.active_indices(rows).collect();
        if active.is_empty() {
            return;
        }
        let position = active
            .iter()
            .position(|index| *index == self.selected)
            .unwrap_or(0) as i32;
        let next = (position + delta).clamp(0, active.len() as i32 - 1) as usize;
        self.selected = active[next];
    }

    pub(super) fn mouse(&mut self, event: MouseEvent, rows: &[MenuRow]) -> Event {
        let point = (event.column, event.row).into();
        let previous = self.selected;
        if matches!(event.kind, MouseEventKind::ScrollUp) {
            if !self.rect.contains(point) {
                return Event::Stay;
            }
            self.move_by(-1, rows);
            return self.selection_change(previous, rows);
        }
        if matches!(event.kind, MouseEventKind::ScrollDown) {
            if !self.rect.contains(point) {
                return Event::Stay;
            }
            self.move_by(1, rows);
            return self.selection_change(previous, rows);
        }
        let MouseEventKind::Down(button) = event.kind else {
            return Event::Stay;
        };
        if !self.rect.contains(point) {
            self.close();
            return Event::Close;
        }
        if button != MouseButton::Left {
            return Event::Stay;
        }
        if let Some((_, section)) = self.tab_rects.iter().find(|(rect, _)| rect.contains(point)) {
            let grouping = grouping(rows);
            let searching = relevance_enabled(rows);
            let tags_enabled = tag_available(rows);
            if !section.enabled(grouping, searching, tags_enabled) {
                return Event::Stay;
            }
            self.section = *section;
            self.offset = 0;
            self.select_default(rows);
            return Event::Stay;
        }
        let Some((_, index)) = self.row_rects.iter().find(|(rect, _)| rect.contains(point)) else {
            return Event::Stay;
        };
        let Some(_) = rows
            .get(*index)
            .filter(|row| row.enabled && row.choice.section() == self.section)
        else {
            return Event::Stay;
        };
        self.selected = *index;
        self.selection_change(previous, rows)
    }

    pub(super) fn draw(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        rows: &[MenuRow],
        theme: &Theme,
        icons: Icons,
    ) {
        if !self.open || area.is_empty() {
            self.rect = Rect::default();
            self.tab_rects.clear();
            self.row_rects.clear();
            return;
        }
        let display = display_rows(rows, self.section);
        let popup_width = if area.width > 66 {
            64
        } else if area.width > 2 {
            area.width - 2
        } else {
            area.width
        }
        .max(1);
        let height = area.height.clamp(1, 11);
        self.rect = Rect::new(
            area.x + area.width.saturating_sub(popup_width) / 2,
            area.y + area.height.saturating_sub(height) / 2,
            popup_width,
            height,
        );
        let block = theme.block(" View ", true);
        let inner = block.inner(self.rect);
        frame.render_widget(OverlayClear, self.rect);
        frame.render_widget(block, self.rect);

        self.tab_rects.clear();
        self.row_rects.clear();
        if inner.is_empty() {
            return;
        }

        let content = if inner.width > 4 {
            Rect::new(inner.x + 2, inner.y, inner.width - 4, inner.height)
        } else {
            inner
        };
        let tabs = Rect::new(content.x, content.y, content.width, 1);
        self.draw_tabs(frame, tabs, rows, theme, icons);

        let divider = content.height >= 2 + Section::MAX_ROWS;
        if divider {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    "─".repeat(content.width as usize),
                    theme.description(),
                )),
                Rect::new(content.x, content.y + 1, content.width, 1),
            );
        }
        let header_height = 1 + u16::from(divider);
        let bottom_pad = u16::from(content.height > header_height + Section::MAX_ROWS);
        let choices = Rect::new(
            content.x,
            content.y + header_height,
            content.width,
            content.height.saturating_sub(header_height + bottom_pad),
        );
        self.draw_choices(frame, choices, rows, &display, theme);
    }

    fn draw_tabs(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        rows: &[MenuRow],
        theme: &Theme,
        icons: Icons,
    ) {
        if area.is_empty() {
            return;
        }
        let required_width = Section::ALL
            .into_iter()
            .map(|section| section.tab_width(icons))
            .sum::<u16>()
            .saturating_add(Section::ALL.len() as u16 - 1);
        if area.width < required_width {
            self.draw_compact_tabs(frame, area, rows, theme, icons);
            return;
        }
        let grouping = grouping(rows);
        let searching = relevance_enabled(rows);
        let tags_enabled = tag_available(rows);
        let mut x = area.x;
        for section in Section::ALL {
            let tab_width = section.tab_width(icons);
            let rect = Rect::new(x, area.y, tab_width, 1);
            if !rect.is_empty() {
                let enabled = section.enabled(grouping, searching, tags_enabled);
                let active = section == self.section;
                let base = if active {
                    theme.selected()
                } else if !enabled {
                    theme.dim().add_modifier(Modifier::DIM)
                } else {
                    theme.description()
                };
                let icon_style = if active {
                    theme.accent().bg(theme.selection_bg)
                } else {
                    base
                };
                let icon = section.icon(icons);
                let mut spans = vec![Span::styled(" ", base)];
                if !icon.is_empty() {
                    spans.push(Span::styled(icon, icon_style));
                    spans.push(Span::styled(" ", base));
                }
                spans.push(Span::styled(section.title(), base));
                spans.push(Span::styled(" ", base));
                frame.render_widget(Paragraph::new(Line::from(spans)).style(base), rect);
                self.tab_rects.push((rect, section));
            }
            x = rect.right().saturating_add(1);
        }
    }

    fn draw_compact_tabs(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        rows: &[MenuRow],
        theme: &Theme,
        icons: Icons,
    ) {
        if area.width < 3 {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    fit(&section_label(self.section, icons), area.width as usize),
                    theme.selected(),
                )),
                area,
            );
            self.tab_rects.push((area, self.section));
            return;
        }
        let arrow_width = if area.width >= 8 { 3 } else { 1 };
        let previous = Rect::new(area.x, area.y, arrow_width, 1);
        let next = Rect::new(area.right() - arrow_width, area.y, arrow_width, 1);
        let active = Rect::new(
            previous.right(),
            area.y,
            area.width.saturating_sub(arrow_width * 2),
            1,
        );
        let grouping = grouping(rows);
        let searching = relevance_enabled(rows);
        let tags_enabled = tag_available(rows);
        let previous_section = self.section.shifted(-1, grouping, searching, tags_enabled);
        let next_section = self.section.shifted(1, grouping, searching, tags_enabled);
        frame.render_widget(
            Paragraph::new(Span::styled(
                if previous.width >= 3 { " ‹ " } else { "‹" },
                theme.dim(),
            )),
            previous,
        );
        frame.render_widget(
            Paragraph::new(compact_active_line(self.section, icons, theme)).style(theme.selected()),
            active,
        );
        frame.render_widget(
            Paragraph::new(Span::styled(
                if next.width >= 3 { " › " } else { "›" },
                theme.dim(),
            )),
            next,
        );
        self.tab_rects.push((previous, previous_section));
        self.tab_rects.push((active, self.section));
        self.tab_rects.push((next, next_section));
    }

    fn draw_choices(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        rows: &[MenuRow],
        display: &[DisplayRow],
        theme: &Theme,
    ) {
        if area.is_empty() || display.is_empty() {
            return;
        }
        let selected = display
            .iter()
            .position(|row| matches!(row, DisplayRow::Choice(index) if *index == self.selected))
            .unwrap_or(0);
        let visible = area.height as usize;
        if selected < self.offset {
            self.offset = selected;
        } else if selected >= self.offset + visible {
            self.offset = selected + 1 - visible;
        }
        self.offset = self.offset.min(display.len().saturating_sub(visible));

        for (line, display_row) in display.iter().skip(self.offset).take(visible).enumerate() {
            let rect = Rect::new(area.x, area.y + line as u16, area.width, 1);
            let DisplayRow::Choice(index) = display_row;
            let row = &rows[*index];
            let focused = row.enabled && *index == self.selected;
            let base = if !row.enabled {
                theme.description().add_modifier(Modifier::DIM)
            } else if focused {
                theme.selected()
            } else {
                Style::default()
            };
            let marker = if row.current { "●" } else { "○" };
            let marker_style = if !row.enabled {
                base
            } else if row.current {
                theme.ok().bg(if focused {
                    theme.selection_bg
                } else {
                    ratatui::style::Color::Reset
                })
            } else {
                theme.description().bg(if focused {
                    theme.selection_bg
                } else {
                    ratatui::style::Color::Reset
                })
            };
            let pointer_style = if focused {
                theme.accent().bg(theme.selection_bg)
            } else {
                base
            };
            let label_style = if focused { theme.selected() } else { base };
            let mut spans = vec![
                Span::styled(if focused { "›" } else { " " }, pointer_style),
                Span::styled(" ", base),
                Span::styled(marker, marker_style),
                Span::styled(" ", base),
                Span::styled(row.label, label_style),
            ];
            let hint = " · requires search";
            if !row.enabled
                && row.choice == Choice::Sort(SortMode::Relevance)
                && 4 + width(row.label) + width(hint) <= rect.width as usize
            {
                spans.push(Span::styled(hint, theme.description()));
            }
            frame.render_widget(Paragraph::new(Line::from(spans)).style(base), rect);
            self.row_rects.push((rect, *index));
        }
        if self.offset > 0 {
            frame.render_widget(
                Paragraph::new(Span::styled("↑", theme.dim())),
                Rect::new(area.right().saturating_sub(1), area.y, 1, 1),
            );
        }
        if self.offset + visible < display.len() {
            frame.render_widget(
                Paragraph::new(Span::styled("↓", theme.dim())),
                Rect::new(
                    area.right().saturating_sub(1),
                    area.bottom().saturating_sub(1),
                    1,
                    1,
                ),
            );
        }
    }
}

fn section_label(section: Section, icons: Icons) -> String {
    let icon = section.icon(icons);
    if icon.is_empty() {
        section.title().into()
    } else {
        format!("{icon} {}", section.title())
    }
}

fn compact_active_line(section: Section, icons: Icons, theme: &Theme) -> Line<'static> {
    let icon = section.icon(icons);
    if icon.is_empty() {
        return Line::from(Span::styled(section.title(), theme.selected()));
    }
    Line::from(vec![
        Span::styled(icon, theme.accent().bg(theme.selection_bg)),
        Span::styled(format!(" {}", section.title()), theme.selected()),
    ])
}

#[derive(Debug, Clone, Copy)]
enum DisplayRow {
    Choice(usize),
}

fn display_rows(rows: &[MenuRow], section: Section) -> Vec<DisplayRow> {
    let mut display = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        if row.choice.section() != section {
            continue;
        }
        display.push(DisplayRow::Choice(index));
    }
    display
}

pub(super) fn menu_rows(
    layout: UiLayout,
    grouping: GroupMode,
    sorting: SortMode,
    order: GroupOrder,
    tags_enabled: bool,
    searching: bool,
) -> Vec<MenuRow> {
    let mut rows = Vec::new();
    for value in [UiLayout::Grid, UiLayout::List, UiLayout::Compact] {
        rows.push(MenuRow {
            label: match value {
                UiLayout::Grid => "Grid",
                UiLayout::List => "List",
                UiLayout::Compact => "Compact",
            },
            choice: Choice::Layout(value),
            current: value == layout,
            enabled: true,
        });
    }
    for value in GroupMode::ALL {
        if value == GroupMode::Tag && !tags_enabled {
            continue;
        }
        rows.push(MenuRow {
            label: value.label(),
            choice: Choice::Group(value),
            current: value == grouping,
            enabled: true,
        });
    }
    for value in SortMode::ALL {
        rows.push(MenuRow {
            label: value.label(),
            choice: Choice::Sort(value),
            current: value == sorting,
            enabled: value != SortMode::Relevance || searching,
        });
    }
    for value in GroupOrder::ALL {
        rows.push(MenuRow {
            label: value.label(),
            choice: Choice::Order(value),
            current: value == order,
            enabled: grouping.effective(searching, tags_enabled) != GroupMode::None,
        });
    }
    rows
}

fn grouping(rows: &[MenuRow]) -> GroupMode {
    rows.iter()
        .find_map(|row| match row.choice {
            Choice::Group(value) if row.current => Some(value),
            _ => None,
        })
        .unwrap_or(GroupMode::Auto)
}

fn relevance_enabled(rows: &[MenuRow]) -> bool {
    rows.iter()
        .any(|row| matches!(row.choice, Choice::Sort(SortMode::Relevance)) && row.enabled)
}

fn tag_available(rows: &[MenuRow]) -> bool {
    rows.iter()
        .any(|row| matches!(row.choice, Choice::Group(GroupMode::Tag)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn rows(grouping: GroupMode) -> Vec<MenuRow> {
        menu_rows(
            UiLayout::Grid,
            grouping,
            SortMode::Auto,
            GroupOrder::Name,
            true,
            false,
        )
    }

    #[test]
    fn category_navigation_stays_within_visible_choices() {
        let rows = rows(GroupMode::Auto);
        let mut menu = ViewMenu::default();
        menu.open(
            Section::Layout,
            UiLayout::Grid,
            GroupMode::Auto,
            SortMode::Auto,
            GroupOrder::Name,
            true,
            false,
        );
        assert_eq!(menu.section(), Section::Layout);
        menu.key(KeyEvent::from(KeyCode::Right), &rows);
        assert_eq!(menu.section(), Section::Group);
        menu.key(KeyEvent::from(KeyCode::End), &rows);
        assert!(matches!(
            rows[menu.selected].choice,
            Choice::Group(GroupMode::Preset)
        ));
        menu.key(KeyEvent::from(KeyCode::Tab), &rows);
        assert_eq!(menu.section(), Section::Sort);
        assert!(matches!(rows[menu.selected].choice, Choice::Sort(_)));
    }

    #[test]
    fn flat_group_order_is_stable_disabled_and_skipped() {
        let flat = rows(GroupMode::None);
        let order: Vec<_> = flat
            .iter()
            .filter(|row| matches!(row.choice, Choice::Order(_)))
            .collect();
        assert_eq!(order.len(), GroupOrder::ALL.len());
        assert!(order.iter().all(|row| !row.enabled));
        assert!(order.iter().any(|row| row.current));

        let mut menu = ViewMenu::default();
        menu.open(
            Section::Sort,
            UiLayout::Grid,
            GroupMode::None,
            SortMode::Auto,
            GroupOrder::Name,
            true,
            false,
        );
        menu.key(KeyEvent::from(KeyCode::Right), &flat);
        assert_eq!(menu.section(), Section::Layout);
    }

    #[test]
    fn draw_shows_only_the_active_category_at_normal_and_tiny_sizes() {
        let mut rows = rows(GroupMode::Auto);
        let mut menu = ViewMenu::default();
        menu.open(
            Section::Layout,
            UiLayout::Grid,
            GroupMode::Auto,
            SortMode::Auto,
            GroupOrder::Name,
            true,
            false,
        );
        let theme = Theme::default();
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| menu.draw(frame, frame.area(), &rows, &theme, Icons::Nerd))
            .unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(screen.contains("Layout"));
        assert!(screen.contains("Grid"));
        assert!(screen.contains("Compact"));
        assert!(!screen.contains("Collapse all"));
        assert!(!screen.contains("Relevance"));
        assert_eq!(menu.rect.width, 64);
        assert_eq!(menu.rect.height, 11);
        assert_eq!(menu.first_row_y(), Some(menu.rect.y + 3));
        assert!(!screen.contains("Choose a skill layout."));

        // Category, keyboard cursor and applied value must remain distinguishable.
        let tab = menu.tab_rects[0].0;
        let grid = menu.row_rects[0].0;
        let list = menu.row_rects[1].0;
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(tab.x + 1, tab.y)].fg, theme.accent);
        assert_eq!(buffer[(tab.x + 1, tab.y)].bg, theme.selection_bg);
        assert_eq!(buffer[(tab.x + 3, tab.y)].bg, theme.selection_bg);
        assert_eq!(buffer[(grid.x, grid.y)].symbol(), "›");
        assert_eq!(buffer[(grid.x, grid.y)].fg, theme.accent);
        assert_eq!(buffer[(grid.x + 2, grid.y)].symbol(), "●");
        assert_eq!(buffer[(grid.x + 2, grid.y)].fg, theme.ok);
        assert_eq!(buffer[(grid.right() - 1, grid.y)].bg, theme.selection_bg);

        assert_eq!(
            menu.key(KeyEvent::from(KeyCode::Down), &rows),
            Event::Change(Choice::Layout(UiLayout::List)),
        );
        rows = menu_rows(
            UiLayout::List,
            GroupMode::Auto,
            SortMode::Auto,
            GroupOrder::Name,
            true,
            false,
        );
        terminal
            .draw(|frame| menu.draw(frame, frame.area(), &rows, &theme, Icons::Nerd))
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(grid.x, grid.y)].symbol(), " ");
        assert_eq!(buffer[(grid.x + 2, grid.y)].symbol(), "○");
        assert_ne!(buffer[(grid.x, grid.y)].bg, theme.selection_bg);
        assert_eq!(buffer[(list.x, list.y)].symbol(), "›");
        assert_eq!(buffer[(list.x + 2, list.y)].symbol(), "●");
        assert_eq!(buffer[(list.x + 2, list.y)].fg, theme.ok);
        assert_eq!(buffer[(list.right() - 1, list.y)].bg, theme.selection_bg);
        assert_eq!(
            menu.key(KeyEvent::from(KeyCode::Enter), &rows),
            Event::Close,
        );
        assert!(!menu.is_open());

        menu.open(
            Section::Layout,
            UiLayout::List,
            GroupMode::Auto,
            SortMode::Auto,
            GroupOrder::Name,
            true,
            false,
        );
        let mut tiny = Terminal::new(TestBackend::new(12, 4)).unwrap();
        tiny.draw(|frame| menu.draw(frame, frame.area(), &rows, &theme, Icons::Nerd))
            .unwrap();
        assert!(menu.rect.right() <= 12);
        assert!(menu.rect.bottom() <= 4);
        assert_eq!(menu.tab_rects.len(), 3);
        assert!(!menu.row_rects.is_empty());
        let active = menu.tab_rects[1].0;
        let buffer = tiny.backend().buffer();
        assert_eq!(buffer[(active.x, active.y)].bg, theme.selection_bg);
    }

    #[test]
    fn tabs_and_markers_follow_icon_mode_and_alternate_theme_colors() {
        let rows = rows(GroupMode::Auto);
        let mut menu = ViewMenu::default();
        menu.open(
            Section::Layout,
            UiLayout::Grid,
            GroupMode::Auto,
            SortMode::Auto,
            GroupOrder::Name,
            true,
            false,
        );
        let theme = Theme {
            accent: ratatui::style::Color::Yellow,
            ok: ratatui::style::Color::LightGreen,
            dim: ratatui::style::Color::Magenta,
            selection_bg: ratatui::style::Color::Blue,
            selection_fg: ratatui::style::Color::White,
            ..Theme::default()
        };

        let mut nerd = Terminal::new(TestBackend::new(80, 24)).unwrap();
        nerd.draw(|frame| menu.draw(frame, frame.area(), &rows, &theme, Icons::Nerd))
            .unwrap();
        let tab = menu.tab_rects[0].0;
        let selected = menu.row_rects[0].0;
        let unapplied = menu.row_rects[1].0;
        let buffer = nerd.backend().buffer();
        assert_eq!(
            buffer[(tab.x + 1, tab.y)].symbol(),
            Section::Layout.icon(Icons::Nerd)
        );
        assert_eq!(buffer[(tab.x + 1, tab.y)].fg, theme.accent);
        assert_eq!(buffer[(tab.x + 1, tab.y)].bg, theme.selection_bg);
        assert_eq!(buffer[(selected.x, selected.y)].fg, theme.accent);
        assert_eq!(buffer[(selected.x + 2, selected.y)].fg, theme.ok);
        assert_eq!(buffer[(selected.x + 2, selected.y)].bg, theme.selection_bg);
        assert_eq!(buffer[(unapplied.x + 2, unapplied.y)].fg, theme.dim);

        let mut text = Terminal::new(TestBackend::new(80, 24)).unwrap();
        text.draw(|frame| menu.draw(frame, frame.area(), &rows, &theme, Icons::Text))
            .unwrap();
        let screen: String = text
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        for section in Section::ALL {
            assert!(screen.contains(section.title()));
            assert!(!screen.contains(section.icon(Icons::Nerd)));
        }
    }

    #[test]
    fn mouse_choices_apply_without_closing_and_outside_wheel_is_ignored() {
        let mut rows = rows(GroupMode::Auto);
        let mut menu = ViewMenu::default();
        menu.open(
            Section::Layout,
            UiLayout::Grid,
            GroupMode::Auto,
            SortMode::Auto,
            GroupOrder::Name,
            true,
            false,
        );
        let theme = Theme::default();
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| menu.draw(frame, frame.area(), &rows, &theme, Icons::Nerd))
            .unwrap();
        let group_tab = menu
            .tab_rects
            .iter()
            .find(|(_, section)| *section == Section::Group)
            .unwrap()
            .0;
        assert_eq!(
            menu.mouse(
                MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: group_tab.x,
                    row: group_tab.y,
                    modifiers: KeyModifiers::NONE,
                },
                &rows,
            ),
            Event::Stay
        );
        assert_eq!(menu.section(), Section::Group);
        terminal
            .draw(|frame| menu.draw(frame, frame.area(), &rows, &theme, Icons::Nerd))
            .unwrap();
        let none_index = rows
            .iter()
            .position(|row| row.choice == Choice::Group(GroupMode::None))
            .unwrap();
        let none = menu
            .row_rects
            .iter()
            .find(|(_, index)| *index == none_index)
            .unwrap()
            .0;
        assert_eq!(
            menu.mouse(
                MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: none.x,
                    row: none.y,
                    modifiers: KeyModifiers::NONE,
                },
                &rows,
            ),
            Event::Change(Choice::Group(GroupMode::None))
        );
        assert!(menu.is_open());
        rows = menu_rows(
            UiLayout::Grid,
            GroupMode::None,
            SortMode::Auto,
            GroupOrder::Name,
            true,
            false,
        );
        let selected = menu.selected;
        assert_eq!(
            menu.mouse(
                MouseEvent {
                    kind: MouseEventKind::ScrollDown,
                    column: 0,
                    row: 0,
                    modifiers: KeyModifiers::NONE,
                },
                &rows,
            ),
            Event::Stay
        );
        assert_eq!(menu.selected, selected);
    }

    #[test]
    fn outside_button_down_closes_while_inside_non_left_buttons_are_inert() {
        let rows = rows(GroupMode::Auto);
        let theme = Theme::default();

        for button in [MouseButton::Right, MouseButton::Middle] {
            let mut menu = ViewMenu::default();
            menu.open(
                Section::Layout,
                UiLayout::Grid,
                GroupMode::Auto,
                SortMode::Auto,
                GroupOrder::Name,
                true,
                false,
            );
            let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
            terminal
                .draw(|frame| menu.draw(frame, frame.area(), &rows, &theme, Icons::Nerd))
                .unwrap();
            let inside = menu.rect;

            assert_eq!(
                menu.mouse(
                    MouseEvent {
                        kind: MouseEventKind::Down(button),
                        column: inside.x,
                        row: inside.y,
                        modifiers: KeyModifiers::NONE,
                    },
                    &rows,
                ),
                Event::Stay
            );
            assert!(menu.is_open());
            assert_eq!(
                menu.mouse(
                    MouseEvent {
                        kind: MouseEventKind::Down(button),
                        column: 0,
                        row: 0,
                        modifiers: KeyModifiers::NONE,
                    },
                    &rows,
                ),
                Event::Close
            );
            assert!(!menu.is_open());
        }
    }

    #[test]
    fn empty_draw_clears_stale_mouse_geometry() {
        let rows = rows(GroupMode::Auto);
        let mut menu = ViewMenu::default();
        menu.open(
            Section::Layout,
            UiLayout::Grid,
            GroupMode::Auto,
            SortMode::Auto,
            GroupOrder::Name,
            true,
            false,
        );
        let theme = Theme::default();
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| menu.draw(frame, frame.area(), &rows, &theme, Icons::Nerd))
            .unwrap();
        assert!(!menu.rect.is_empty());
        terminal
            .draw(|frame| menu.draw(frame, Rect::default(), &rows, &theme, Icons::Nerd))
            .unwrap();
        assert!(menu.rect.is_empty());
        assert!(menu.tab_rects.is_empty());
        assert!(menu.row_rects.is_empty());
    }
}
