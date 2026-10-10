//! The `format.toml` declaration defines the current Skill Home layout contract.
//!
//! It is not migration history: it contains no per-step flags or timestamps. Adding
//! any key requires bumping `layout`. To restore a backup, copy its originals back,
//! including `format.toml` if the backup has one (otherwise delete `format.toml`),
//! so the next open migrates from the restored layout again.

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
    // A newer layout may add keys, so check `layout` before the key set.
    let layout = match document.get("layout") {
        None => None,
        Some(Item::Value(value)) => match value.as_integer() {
            Some(layout) if layout > 0 && layout <= u32::MAX as i64 => Some(layout as u32),
            _ => bail!(
                "invalid Skill Home layout in {}: layout must be a positive integer",
                path.display()
            ),
        },
        Some(_) => bail!(
            "invalid Skill Home layout in {}: layout must be a positive integer",
            path.display()
        ),
    };
    if let Some(layout) = layout {
        ensure!(
            layout <= super::CURRENT_LAYOUT,
            "Skill Home layout {layout} requires a newer version of Skills Manager"
        );
    }
    match layout {
        Some(layout) if document.len() == 1 => Ok(layout),
        _ => bail!(
            "invalid Skill Home declaration in {}: unknown or missing keys",
            path.display()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_text(text: &str) -> Result<u32> {
        parse(Path::new("format.toml"), text.as_bytes())
    }

    #[test]
    fn newer_layout_is_reported_before_unknown_keys() {
        let error = parse_text(
            "layout = 2
extra = true
",
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "Skill Home layout 2 requires a newer version of Skills Manager"
        );
        let error = parse_text(
            "layout = 1
extra = true
",
        )
        .unwrap_err();
        assert!(error.to_string().contains("unknown or missing keys"));
        let error = parse_text(
            "extra = true
",
        )
        .unwrap_err();
        assert!(error.to_string().contains("unknown or missing keys"));
        let error = parse_text(
            "layout = 0
",
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("layout must be a positive integer")
        );
        assert_eq!(
            parse_text(&String::from_utf8(content()).unwrap()).unwrap(),
            super::super::CURRENT_LAYOUT
        );
    }
}
