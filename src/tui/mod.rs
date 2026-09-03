//! Terminal UI (search-first).

use anyhow::Result;
use std::path::Path;

pub fn run(root: Option<&Path>) -> Result<()> {
    let root = skills::paths::resolve_root(root)?;
    let ws = skills::Workspace::open(&root)?;
    let snap = ws.scan()?;
    println!(
        "TUI not implemented yet; {} skills scanned at {}",
        snap.skills.len(),
        root.display()
    );
    Ok(())
}
