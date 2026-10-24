//! Geometry for Library organization rows.
//!
//! Headers occupy one full-width terminal row. Skill rows preserve the normal
//! card/list cell height and column count. Scrolling is measured in terminal
//! rows so rendering, pointer hit tests and the scrollbar share one model.

use std::collections::BTreeSet;

use ratatui::layout::Rect;

use super::organization::{GroupId, Organized};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Entry {
    Header { group: usize },
    Skill { hit: usize, group: Option<GroupId> },
}

#[derive(Debug, Clone)]
struct Row {
    entries: Vec<usize>,
    height: u16,
    top: usize,
}

#[derive(Debug, Clone, Default)]
pub(super) struct GroupedGrid {
    pub entries: Vec<Entry>,
    rows: Vec<Row>,
    entry_rows: Vec<usize>,
    cells: Vec<Option<Rect>>,
    top_clips: Vec<u16>,
    selected: Option<usize>,
    offset: usize,
    area: Rect,
    cols: usize,
    cell_height: u16,
    gap: u16,
    total_height: usize,
    last_click: Option<(std::time::Instant, usize)>,
}

impl GroupedGrid {
    pub(super) fn rebuild(&mut self, organized: &Organized, collapsed: &BTreeSet<GroupId>) {
        self.entries.clear();
        // Rows and cells describe the previous projection. Key input can arrive
        // before the next draw, so make that stale geometry visibly unavailable.
        self.rows.clear();
        self.entry_rows.clear();
        self.cells.clear();
        self.top_clips.clear();
        self.total_height = 0;
        if organized.groups.is_empty() {
            self.entries.extend(
                organized
                    .flat
                    .iter()
                    .copied()
                    .map(|hit| Entry::Skill { hit, group: None }),
            );
        } else {
            for (group, model) in organized.groups.iter().enumerate() {
                self.entries.push(Entry::Header { group });
                if !collapsed.contains(&model.id) {
                    self.entries
                        .extend(model.hits.iter().copied().map(|hit| Entry::Skill {
                            hit,
                            group: Some(model.id.clone()),
                        }));
                }
            }
        }
        self.selected = self.selected.filter(|&index| index < self.entries.len());
    }

    pub(super) fn layout(&mut self, area: Rect, cols: usize, cell_height: u16, gap: u16) {
        self.area = area;
        self.cols = cols.max(1);
        self.cell_height = cell_height.max(1);
        self.gap = gap;
        self.rows.clear();
        self.entry_rows = vec![0; self.entries.len()];

        let mut index = 0;
        let mut top = 0;
        while index < self.entries.len() {
            let row_index = self.rows.len();
            let (entries, height) = match self.entries[index] {
                Entry::Header { .. } => (vec![index], 1),
                Entry::Skill { .. } => {
                    let mut entries = Vec::new();
                    while index < self.entries.len()
                        && entries.len() < self.cols
                        && matches!(self.entries[index], Entry::Skill { .. })
                    {
                        entries.push(index);
                        index += 1;
                    }
                    (entries, self.cell_height)
                }
            };
            if matches!(
                self.entries[index.min(self.entries.len() - 1)],
                Entry::Header { .. }
            ) && entries.len() == 1
                && entries[0] == index
            {
                index += 1;
            }
            for &entry in &entries {
                self.entry_rows[entry] = row_index;
            }
            self.rows.push(Row {
                entries,
                height,
                top,
            });
            top += height as usize;
        }
        self.total_height = top;
        self.ensure_visible();
        self.offset = self
            .offset
            .min(self.total_height.saturating_sub(area.height as usize));
        self.compute_cells();
    }

    fn compute_cells(&mut self) {
        self.cells = vec![None; self.entries.len()];
        self.top_clips = vec![0; self.entries.len()];
        let cell_width = (self.area.width / self.cols as u16).max(1);
        for row in &self.rows {
            let top = row.top as isize - self.offset as isize;
            let bottom = top + row.height as isize;
            if bottom <= 0 || top >= self.area.height as isize {
                continue;
            }
            let y = self.area.y + top.max(0) as u16;
            let clipped_top = top.min(0).unsigned_abs() as u16;
            let height = row
                .height
                .saturating_sub(clipped_top)
                .min(self.area.bottom().saturating_sub(y));
            let header = row
                .entries
                .first()
                .is_some_and(|&entry| matches!(self.entries[entry], Entry::Header { .. }));
            for (column, &entry) in row.entries.iter().enumerate() {
                self.top_clips[entry] = clipped_top;
                self.cells[entry] = Some(if header {
                    Rect::new(self.area.x, y, self.area.width, height)
                } else {
                    Rect::new(
                        self.area.x + column as u16 * cell_width,
                        y,
                        cell_width.saturating_sub(self.gap),
                        height,
                    )
                });
            }
        }
    }

    fn ensure_visible(&mut self) {
        let Some(selected) = self.selected else {
            return;
        };
        let Some(row) = self.rows.get(self.entry_rows[selected]) else {
            return;
        };
        if row.top < self.offset {
            self.offset = row.top;
        } else if row.top + row.height as usize > self.offset + self.area.height as usize {
            self.offset = row
                .top
                .saturating_add(row.height as usize)
                .saturating_sub(self.area.height as usize);
        }
    }

    pub(super) fn selected(&self) -> Option<usize> {
        self.selected
    }

    pub(super) fn select(&mut self, selected: Option<usize>) {
        self.selected = selected.filter(|&index| index < self.entries.len());
    }

    pub(super) fn selected_entry(&self) -> Option<&Entry> {
        self.selected.and_then(|index| self.entries.get(index))
    }

    pub(super) fn selected_hit(&self) -> Option<usize> {
        match self.selected_entry()? {
            Entry::Skill { hit, .. } => Some(*hit),
            Entry::Header { .. } => None,
        }
    }

    pub(super) fn first_skill(&self) -> Option<usize> {
        self.entries
            .iter()
            .position(|entry| matches!(entry, Entry::Skill { .. }))
    }

    pub(super) fn find_skill(&self, hit: usize, preferred: Option<&GroupId>) -> Option<usize> {
        self.entries
            .iter()
            .position(|entry| matches!(entry, Entry::Skill { hit: value, group } if *value == hit && group.as_ref() == preferred))
            .or_else(|| {
                self.entries
                    .iter()
                    .position(|entry| matches!(entry, Entry::Skill { hit: value, .. } if *value == hit))
            })
    }

    pub(super) fn move_linear(&mut self, delta: i32) {
        if self.entries.is_empty() {
            self.selected = None;
            return;
        }
        let current = self.selected.unwrap_or(0) as i32;
        self.selected = Some((current + delta).clamp(0, self.entries.len() as i32 - 1) as usize);
    }

    pub(super) fn move_vertical(&mut self, delta: i32) {
        let Some(selected) = self.selected else {
            self.selected = self
                .first_skill()
                .or((!self.entries.is_empty()).then_some(0));
            return;
        };
        // Before the first draw (or immediately after regrouping), column and
        // row geometry is not known yet. Treat the projection as one column;
        // the next layout preserves this selected display entry and scrolls it
        // into view using the real dimensions.
        if self.entry_rows.len() != self.entries.len() || self.rows.is_empty() {
            self.move_linear(delta);
            return;
        }
        let row_index = self.entry_rows[selected];
        let column = self.rows[row_index]
            .entries
            .iter()
            .position(|&entry| entry == selected)
            .unwrap_or(0);
        let target = (row_index as i32 + delta).clamp(0, self.rows.len() as i32 - 1) as usize;
        self.selected = self.rows[target]
            .entries
            .get(column)
            .or_else(|| self.rows[target].entries.last())
            .copied();
    }

    pub(super) fn first(&mut self) {
        self.selected = (!self.entries.is_empty()).then_some(0);
    }

    pub(super) fn last(&mut self) {
        self.selected = self.entries.len().checked_sub(1);
    }

    pub(super) fn page(&self) -> i32 {
        self.rows
            .iter()
            .filter(|row| {
                row.top + row.height as usize > self.offset
                    && row.top < self.offset + self.area.height as usize
            })
            .count()
            .max(1) as i32
    }

    pub(super) fn cell(&self, entry: usize) -> Option<Rect> {
        self.cells.get(entry).copied().flatten()
    }

    pub(super) fn top_clip(&self, entry: usize) -> u16 {
        self.top_clips.get(entry).copied().unwrap_or(0)
    }

    pub(super) fn visible_entries(&self) -> impl Iterator<Item = usize> + '_ {
        self.cells
            .iter()
            .enumerate()
            .filter_map(|(index, cell)| cell.map(|_| index))
    }

    pub(super) fn hit(&self, x: u16, y: u16) -> Option<usize> {
        self.visible_entries()
            .find(|&entry| self.cells[entry].is_some_and(|rect| rect.contains((x, y).into())))
    }

    pub(super) fn click(&mut self, x: u16, y: u16) -> Option<(usize, bool)> {
        let entry = self.hit(x, y)?;
        let now = std::time::Instant::now();
        let double = matches!(self.last_click, Some((time, previous)) if previous == entry && now.duration_since(time).as_millis() < 400);
        self.last_click = Some((now, entry));
        self.selected = Some(entry);
        Some((entry, double))
    }

    pub(super) fn total_height(&self) -> usize {
        self.total_height
    }

    pub(super) fn visible_height(&self) -> usize {
        self.area.height as usize
    }

    pub(super) fn offset(&self) -> usize {
        self.offset
    }

    pub(super) fn scroll_to(&mut self, offset: usize) {
        self.offset = offset.min(self.total_height.saturating_sub(self.area.height as usize));
        if let Some((index, _)) = self
            .rows
            .iter()
            .enumerate()
            .find(|(_, row)| row.top + row.height as usize > self.offset)
            && let Some(entry) = self.rows[index].entries.first()
        {
            self.selected = Some(*entry);
        }
        self.compute_cells();
    }

    pub(super) fn cols(&self) -> usize {
        self.cols
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::views::search::organization::{Group, Organized};

    #[test]
    fn headers_are_full_width_and_cards_keep_columns_and_height() {
        let model = Organized {
            flat: vec![],
            groups: vec![Group {
                id: GroupId::Local,
                title: "Local".into(),
                hits: vec![0, 1, 2],
                total: 3,
            }],
        };
        let mut grid = GroupedGrid::default();
        grid.rebuild(&model, &BTreeSet::new());
        grid.select(grid.first_skill());
        grid.layout(Rect::new(2, 3, 30, 8), 2, 4, 1);
        assert_eq!(grid.cell(0), Some(Rect::new(2, 3, 30, 1)));
        assert_eq!(grid.cell(1), Some(Rect::new(2, 4, 14, 4)));
        assert_eq!(grid.cell(2), Some(Rect::new(17, 4, 14, 4)));
        assert_eq!(grid.total_height(), 9);
    }

    #[test]
    fn vertical_navigation_is_safe_before_draw_and_after_projection_rebuild() {
        let first = Organized {
            flat: vec![],
            groups: vec![Group {
                id: GroupId::Local,
                title: "Local".into(),
                hits: vec![0, 1],
                total: 2,
            }],
        };
        let mut grid = GroupedGrid::default();
        grid.rebuild(&first, &BTreeSet::new());
        grid.select(grid.first_skill());
        grid.move_vertical(1);
        assert!(matches!(
            grid.selected_entry(),
            Some(Entry::Skill { hit: 1, .. })
        ));

        grid.layout(Rect::new(0, 0, 40, 10), 2, 4, 1);
        let collapsed = BTreeSet::from([GroupId::Local]);
        grid.rebuild(&first, &collapsed);
        grid.select(Some(0));
        grid.move_vertical(1);
        assert!(matches!(grid.selected_entry(), Some(Entry::Header { .. })));
    }
}
