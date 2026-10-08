use anyhow::{Result, bail};
use std::path::Path;
use toml_edit::{DocumentMut, Item, value};

pub const CONFIG: u32 = 2;
pub const PRESET: u32 = 1;
pub const TAG: u32 = 1;
pub const REPOSITORY: u32 = 1;

pub fn version(
    doc: &DocumentMut,
    path: &Path,
    kind: &str,
    current: u32,
    missing: u32,
) -> Result<u32> {
    let found = match doc.get("schema") {
        None => missing,
        Some(Item::Value(value)) => match value.as_integer() {
            Some(value) if value > 0 && value <= u32::MAX as i64 => value as u32,
            _ => bail!(
                "invalid {kind} schema in {}: schema must be a positive integer",
                path.display()
            ),
        },
        Some(_) => bail!(
            "invalid {kind} schema in {}: schema must be a positive integer",
            path.display()
        ),
    };
    if found > current {
        bail!(
            "unsupported {kind} schema {found} in {}; this version supports up to {current}; open it with a newer version of Skills Manager",
            path.display()
        );
    }
    Ok(found)
}

pub fn require_current(
    doc: &DocumentMut,
    path: &Path,
    kind: &str,
    current: u32,
    missing: u32,
) -> Result<()> {
    let found = version(doc, path, kind, current, missing)?;
    if found < current {
        bail!(
            "legacy {kind} schema {found} in {}; reopen the workspace to migrate it to schema {current}",
            path.display()
        );
    }
    Ok(())
}

pub fn set(doc: &mut DocumentMut, schema: u32) {
    doc["schema"] = value(schema as i64);
}
