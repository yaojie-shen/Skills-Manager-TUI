//! Stable content hash of a skill directory.
//!
//! Algorithm version 1: walk the directory, sort entries by relative path,
//! skip `.git`, `__pycache__`, `.DS_Store` and `*.pyc`; for every file feed
//! `rel_path\0` followed by the bytes and `\0`; symlinks contribute their
//! target path instead of being followed; directories themselves contribute
//! nothing. Permissions and timestamps are ignored.

use crate::util::is_ignored_name;
use anyhow::Result;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;
use walkdir::WalkDir;

pub const HASH_ALGO: u32 = 1;

pub fn hash_directory(dir: &Path) -> Result<String> {
    let mut entries: Vec<(String, walkdir::DirEntry)> = Vec::new();
    let walker = WalkDir::new(dir)
        .follow_links(false)
        .min_depth(1)
        .into_iter()
        .filter_entry(|e| !is_ignored_name(&e.file_name().to_string_lossy()));
    for entry in walker {
        let entry = entry?;
        let rel = entry
            .path()
            .strip_prefix(dir)
            .unwrap_or(entry.path())
            .to_string_lossy()
            .into_owned();
        entries.push((rel, entry));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    for (rel, entry) in entries {
        let ft = entry.file_type();
        if ft.is_symlink() {
            let target = std::fs::read_link(entry.path())?;
            hasher.update(rel.as_bytes());
            hasher.update(b"\0->");
            hasher.update(target.to_string_lossy().as_bytes());
            hasher.update(b"\0");
        } else if ft.is_file() {
            hasher.update(rel.as_bytes());
            hasher.update(b"\0");
            let mut file = std::fs::File::open(entry.path())?;
            loop {
                let read = file.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                hasher.update(&buffer[..read]);
            }
            hasher.update(b"\0");
        }
    }
    let digest = hasher.finalize();
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    Ok(format!("sha256:{hex}"))
}

pub fn default_algo() -> u32 {
    HASH_ALGO
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streaming_keeps_v1_hash_format_across_buffer_boundaries() {
        let tmp = crate::ops::DownloadDir::new("stream-hash").unwrap();
        let data = vec![b'x'; 150_000];
        std::fs::write(tmp.path().join("a"), &data).unwrap();
        std::fs::write(tmp.path().join("empty"), []).unwrap();
        std::fs::write(tmp.path().join("ignored.pyc"), "ignore me").unwrap();
        std::os::unix::fs::symlink("a", tmp.path().join("link")).unwrap();
        let mut bytes = b"a\0".to_vec();
        bytes.extend(&data);
        bytes.extend(b"\0empty\0\0link\0->a\0");
        let hex: String = Sha256::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let expected = format!("sha256:{hex}");
        assert_eq!(hash_directory(tmp.path()).unwrap(), expected);
    }
}
