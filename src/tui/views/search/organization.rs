//! Pure Library organization model.
//!
//! Search hits remain the unique source set. Grouping only projects those hit
//! indices into display groups, so tag and preset membership may show one skill
//! more than once without changing result counts or bulk-action semantics.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use skills::reconcile::SkillRecord;
use skills::repository::{Repository, alias_of};
use skills::search::Hit;

use crate::tui::components::skill::display_name;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GroupMode {
    Auto,
    None,
    Repository,
    Tag,
    Preset,
}

impl GroupMode {
    pub(super) const ALL: [Self; 5] = [
        Self::Auto,
        Self::None,
        Self::Repository,
        Self::Tag,
        Self::Preset,
    ];

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::None => "None",
            Self::Repository => "Repository",
            Self::Tag => "Tag",
            Self::Preset => "Preset",
        }
    }

    pub(super) fn effective(self, searching: bool, tags_enabled: bool) -> Self {
        match self {
            Self::Auto if searching => Self::None,
            Self::Auto => Self::Repository,
            Self::Tag if !tags_enabled => Self::Repository,
            other => other,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SortMode {
    Auto,
    NameAsc,
    NameDesc,
    PathAsc,
    PathDesc,
    Relevance,
}

impl SortMode {
    pub(super) const ALL: [Self; 6] = [
        Self::Auto,
        Self::NameAsc,
        Self::NameDesc,
        Self::PathAsc,
        Self::PathDesc,
        Self::Relevance,
    ];

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::NameAsc => "Name ↑",
            Self::NameDesc => "Name ↓",
            Self::PathAsc => "Path ↑",
            Self::PathDesc => "Path ↓",
            Self::Relevance => "Relevance",
        }
    }

    pub(super) fn effective(self, searching: bool) -> Self {
        match self {
            Self::Auto if searching => Self::Relevance,
            Self::Auto | Self::Relevance if !searching => Self::NameAsc,
            other => other,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GroupOrder {
    Name,
    Count,
}

impl GroupOrder {
    pub(super) const ALL: [Self; 2] = [Self::Name, Self::Count];

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::Count => "Count ↓",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) enum GroupId {
    Repository(String),
    Local,
    Tag(String),
    Untagged,
    Preset(String),
    NoPreset,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Group {
    pub id: GroupId,
    pub title: String,
    pub hits: Vec<usize>,
    pub total: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Organized {
    /// Used when grouping is disabled. Values index the unique `SearchView::hits`.
    pub flat: Vec<usize>,
    pub groups: Vec<Group>,
}

impl Organized {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn build(
        hits: &[Hit],
        records: &[SkillRecord],
        repositories: &BTreeMap<String, Repository>,
        grouping: GroupMode,
        sorting: SortMode,
        group_order: GroupOrder,
        searching: bool,
        tags_enabled: bool,
    ) -> Self {
        let grouping = grouping.effective(searching, tags_enabled);
        let sorting = sorting.effective(searching);
        if grouping == GroupMode::None {
            let mut flat: Vec<_> = (0..hits.len()).collect();
            sort_hits(&mut flat, hits, records, sorting);
            return Self {
                flat,
                groups: Vec::new(),
            };
        }

        let mut totals: BTreeMap<GroupId, usize> = BTreeMap::new();
        for record in records.iter().filter(|record| record.status.is_browsable()) {
            for (id, _) in memberships(record, repositories, grouping) {
                *totals.entry(id).or_default() += 1;
            }
        }

        let mut grouped: BTreeMap<GroupId, (String, Vec<usize>)> = BTreeMap::new();
        for (hit_index, hit) in hits.iter().enumerate() {
            let record = &records[hit.index];
            for (id, title) in memberships(record, repositories, grouping) {
                grouped
                    .entry(id)
                    .or_insert_with(|| (title, Vec::new()))
                    .1
                    .push(hit_index);
            }
        }

        let mut groups: Vec<_> = grouped
            .into_iter()
            .map(|(id, (title, mut group_hits))| {
                sort_hits(&mut group_hits, hits, records, sorting);
                Group {
                    total: totals.get(&id).copied().unwrap_or(group_hits.len()),
                    id,
                    title,
                    hits: group_hits,
                }
            })
            .collect();
        let mut display_counts = BTreeMap::new();
        for group in &groups {
            *display_counts.entry(group.title.clone()).or_insert(0usize) += 1;
        }
        for group in &mut groups {
            if display_counts.get(&group.title).copied().unwrap_or(0) > 1 {
                group.title = match &group.id {
                    GroupId::Repository(alias) => format!("{} ({alias})", group.title),
                    GroupId::Tag(_) => format!("{} (tag)", group.title),
                    GroupId::Preset(_) => format!("{} (preset)", group.title),
                    _ => group.title.clone(),
                };
            }
        }
        groups.sort_by(|a, b| match group_order {
            GroupOrder::Name => match (&a.id, &b.id) {
                (GroupId::Local, GroupId::Local) => Ordering::Equal,
                (GroupId::Local, _) => Ordering::Less,
                (_, GroupId::Local) => Ordering::Greater,
                _ => natural_cmp(&a.title, &b.title).then_with(|| a.id.cmp(&b.id)),
            },
            GroupOrder::Count => b
                .hits
                .len()
                .cmp(&a.hits.len())
                .then_with(|| natural_cmp(&a.title, &b.title))
                .then_with(|| a.id.cmp(&b.id)),
        });
        Self {
            flat: Vec::new(),
            groups,
        }
    }
}

fn memberships(
    record: &SkillRecord,
    repositories: &BTreeMap<String, Repository>,
    grouping: GroupMode,
) -> Vec<(GroupId, String)> {
    match grouping {
        GroupMode::Repository => match alias_of(&record.key) {
            Some(alias) => vec![(
                GroupId::Repository(alias.to_string()),
                repositories
                    .get(alias)
                    .map(Repository::display_name)
                    .or_else(|| record.source_display_name().map(str::to_owned))
                    .unwrap_or_else(|| alias.to_string()),
            )],
            None => vec![(GroupId::Local, "Local".into())],
        },
        GroupMode::Tag => {
            let names: BTreeSet<_> = record.tags.iter().cloned().collect();
            if names.is_empty() {
                vec![(GroupId::Untagged, "Untagged (no tags)".into())]
            } else {
                names
                    .into_iter()
                    .map(|name| (GroupId::Tag(name.clone()), name))
                    .collect()
            }
        }
        GroupMode::Preset => {
            let names: BTreeSet<_> = record.presets.iter().cloned().collect();
            if names.is_empty() {
                vec![(GroupId::NoPreset, "No preset (unassigned)".into())]
            } else {
                names
                    .into_iter()
                    .map(|name| (GroupId::Preset(name.clone()), name))
                    .collect()
            }
        }
        GroupMode::Auto | GroupMode::None => Vec::new(),
    }
}

fn sort_hits(indices: &mut [usize], hits: &[Hit], records: &[SkillRecord], sorting: SortMode) {
    indices.sort_by(|&a, &b| {
        let ah = &hits[a];
        let bh = &hits[b];
        let ar = &records[ah.index];
        let br = &records[bh.index];
        let order = match sorting {
            SortMode::NameAsc => natural_cmp(display_name(ar), display_name(br)),
            SortMode::NameDesc => natural_cmp(display_name(br), display_name(ar)),
            SortMode::PathAsc => natural_cmp(&ar.key, &br.key),
            SortMode::PathDesc => natural_cmp(&br.key, &ar.key),
            SortMode::Relevance => bh.score.total_cmp(&ah.score),
            SortMode::Auto => Ordering::Equal,
        };
        order.then_with(|| ar.key.cmp(&br.key))
    });
}

/// Case-insensitive natural ordering. Digit runs compare by magnitude using
/// their normalized lengths, so arbitrarily long values never need parsing.
pub(super) fn natural_cmp(left: &str, right: &str) -> Ordering {
    let left = left.to_lowercase();
    let right = right.to_lowercase();
    let (mut li, mut ri) = (0, 0);
    let (lb, rb) = (left.as_bytes(), right.as_bytes());
    while li < lb.len() && ri < rb.len() {
        if lb[li].is_ascii_digit() && rb[ri].is_ascii_digit() {
            let (ls, rs) = (li, ri);
            while li < lb.len() && lb[li].is_ascii_digit() {
                li += 1;
            }
            while ri < rb.len() && rb[ri].is_ascii_digit() {
                ri += 1;
            }
            let ln = &left[ls..li];
            let rn = &right[rs..ri];
            let ltrim = ln.trim_start_matches('0');
            let rtrim = rn.trim_start_matches('0');
            let lnum = if ltrim.is_empty() { "0" } else { ltrim };
            let rnum = if rtrim.is_empty() { "0" } else { rtrim };
            let order = lnum
                .len()
                .cmp(&rnum.len())
                .then_with(|| lnum.cmp(rnum))
                .then_with(|| ln.len().cmp(&rn.len()));
            if order != Ordering::Equal {
                return order;
            }
            continue;
        }
        let lc = left[li..].chars().next().expect("byte is in bounds");
        let rc = right[ri..].chars().next().expect("byte is in bounds");
        let order = lc.cmp(&rc);
        if order != Ordering::Equal {
            return order;
        }
        li += lc.len_utf8();
        ri += rc.len_utf8();
    }
    lb.len().cmp(&rb.len()).then_with(|| left.cmp(&right))
}

#[cfg(test)]
mod tests {
    use super::natural_cmp;
    use std::cmp::Ordering;

    #[test]
    fn natural_order_handles_case_and_unbounded_numeric_runs() {
        assert_eq!(natural_cmp("Skill 2", "skill 10"), Ordering::Less);
        assert_eq!(natural_cmp("x0002", "x2"), Ordering::Greater);
        assert_eq!(
            natural_cmp(
                "x999999999999999999999999999999999999",
                "x1000000000000000000000000000000000000"
            ),
            Ordering::Less
        );
    }
}
