//! Pure diffing from a captured snapshot to a migrated virtual Home tree.

use super::snapshot::{HomeSnapshot, HomeView, RelPath};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub enum Phase {
    Tags,
    Presets,
    Repos,
    Config,
}
impl Phase {
    pub(crate) const ORDERED: [Self; 4] = [Self::Tags, Self::Presets, Self::Repos, Self::Config];
}
#[derive(Clone, Debug)]
pub struct FileOp {
    pub path: RelPath,
    pub before: Option<Vec<u8>>,
    pub after: Option<Vec<u8>>,
    pub phase: Phase,
}
#[derive(Clone, Debug, Default)]
pub struct Plan {
    pub ops: Vec<FileOp>,
}
impl Plan {
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }
    pub fn phase(&self, phase: Phase) -> impl Iterator<Item = &FileOp> {
        self.ops.iter().filter(move |op| op.phase == phase)
    }
}
fn phase(path: &RelPath) -> Phase {
    match path
        .as_path()
        .components()
        .next()
        .and_then(|component| component.as_os_str().to_str())
    {
        Some("tags") => Phase::Tags,
        Some("presets") => Phase::Presets,
        Some("repos") => Phase::Repos,
        _ => Phase::Config,
    }
}
pub fn diff(snapshot: &HomeSnapshot, view: &HomeView) -> Plan {
    let paths: BTreeSet<_> = snapshot
        .files()
        .keys()
        .chain(view.files().keys())
        .cloned()
        .collect();
    Plan {
        ops: paths
            .into_iter()
            .filter_map(|path| {
                let before = snapshot.files().get(&path).cloned();
                let after = view.files().get(&path).cloned();
                (before != after).then(|| FileOp {
                    phase: phase(&path),
                    path,
                    before,
                    after,
                })
            })
            .collect(),
    }
}
