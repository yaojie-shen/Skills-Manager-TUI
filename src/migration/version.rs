//! The `format.toml` declaration defines the current Skill Home layout contract.
//!
//! It is not migration history: it contains no per-step flags or timestamps. Adding
//! any key requires bumping `layout`. To restore a backup, copy its originals back
//! and delete `format.toml` so layout detection runs again.

use anyhow::{Result, bail, ensure};
use std::path::{Path, PathBuf};
use toml_edit::{DocumentMut, Item};

pub fn content() -> Vec<u8> {
    format!(
        "# Skill Home layout, managed by Skills Manager. Do not edit.\nlayout = {}\n",
        super::CURRENT_LAYOUT
    )
    .into_bytes()
}

pub fn path(root: &Path) -> PathBuf {
    crate::paths::meta_dir(root).join("format.toml")
}

pub fn read(root: &Path) -> Result<Option<u32>> {
    let path = path(root);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    parse(&path, &bytes).map(Some)
}

pub fn parse(path: &Path, bytes: &[u8]) -> Result<u32> {
    let text = std::str::from_utf8(bytes)?;
    let document = text.parse::<DocumentMut>()?;
    ensure!(
        document.len() == 1 && document.get("layout").is_some(),
        "invalid Skill Home declaration in {}: unknown or missing keys",
        path.display()
    );
    match document.get("layout") {
        Some(Item::Value(value)) => match value.as_integer() {
            Some(layout) if layout > 0 && layout <= u32::MAX as i64 => Ok(layout as u32),
            _ => bail!(
                "invalid Skill Home layout in {}: layout must be a positive integer",
                path.display()
            ),
        },
        _ => bail!(
            "invalid Skill Home layout in {}: layout must be a positive integer",
            path.display()
        ),
    }
}
