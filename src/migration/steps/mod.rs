//! Frozen, ordered, idempotent Home layout transformations.

pub mod v0_to_v1;

use super::snapshot::HomeView;
use anyhow::Result;

#[derive(Clone, Debug, Default)]
pub struct StepNotes {
    pub migrated_names: Vec<String>,
    pub migrated_tags: Vec<String>,
    pub config_migrated: bool,
    pub migrated_repositories: Vec<String>,
}
pub type Upgrade = fn(&mut HomeView) -> Result<StepNotes>;
pub const STEPS: &[(u32, Upgrade)] = &[(0, v0_to_v1::upgrade)];
