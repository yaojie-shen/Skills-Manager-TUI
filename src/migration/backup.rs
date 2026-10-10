//! Byte-exact backup publication for every original changed by a plan.

use super::plan::Plan;
use anyhow::{Context, Result};
use std::{
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
pub(crate) fn create(root: &Path, plan: &Plan, from: u32, to: u32) -> Result<PathBuf> {
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let base = crate::paths::meta_dir(root).join("backups");
    let final_dir = base.join(format!("metadata-before-layout-v{from}-to-v{to}-{stamp}"));
    let staging = base.join(format!(".layout-v{from}-to-v{to}-{stamp}.tmp"));
    std::fs::create_dir_all(&staging)?;
    let result = (|| -> Result<()> {
        for op in &plan.ops {
            if let Some(bytes) = &op.before {
                crate::util::write_atomic(&staging.join(op.path.as_path()), bytes)?;
            }
        }
        std::fs::rename(&staging, &final_dir)?;
        Ok(())
    })();
    if let Err(error) = result {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(error).context("creating layout migration backup");
    }
    Ok(final_dir)
}
