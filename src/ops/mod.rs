//! Write operations. Every function here is user-initiated; scanning lives in `reconcile`.

pub mod agent_links;
pub mod deploy;
pub mod edit;
pub mod install;
pub mod name_choices;
pub mod targets;
pub mod update;

use anyhow::{Result, bail};
use std::path::{Path, PathBuf};

/// Download and inspection workspace outside the tracked root. Keep final
/// installation staging on the root filesystem so publication stays atomic.
pub struct DownloadDir {
    path: Option<std::path::PathBuf>,
}

impl DownloadDir {
    pub fn new(label: &str) -> Result<Self> {
        for _ in 0..10 {
            let path = std::env::temp_dir().join(format!(
                "skills-download-{label}-{}-{}",
                std::process::id(),
                nanos()
            ));
            let mut builder = std::fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            match builder.create(&path) {
                Ok(()) => return Ok(Self { path: Some(path) }),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e.into()),
            }
        }
        bail!("could not allocate a temporary download directory")
    }

    pub fn path(&self) -> &Path {
        self.path.as_deref().expect("download directory owned")
    }

    /// Transfer cleanup responsibility to a fetched/prepared result.
    pub fn keep(mut self) -> std::path::PathBuf {
        self.path.take().expect("download directory owned")
    }
}

impl Drop for DownloadDir {
    fn drop(&mut self) {
        if let Some(path) = &self.path {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

/// Staging area on the same filesystem as the root so directory swaps are atomic renames.
pub fn staging_dir(root: &Path) -> Result<PathBuf> {
    let dir = crate::paths::meta_dir(root).join(".staging");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub fn fresh_staging(root: &Path, label: &str) -> Result<PathBuf> {
    let base = staging_dir(root)?;
    let p = base.join(format!("{label}-{}-{}", std::process::id(), nanos()));
    if p.exists() {
        std::fs::remove_dir_all(&p)?;
    }
    Ok(p)
}

fn nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// Replace `dest` with `new_dir` atomically from the point of view of readers:
/// the old directory is renamed aside first, then the new one renamed into place.
pub fn swap_dir(root: &Path, dest: &Path, new_dir: &Path) -> Result<()> {
    let aside = fresh_staging(root, "old")?;
    let had_old = dest.exists() || crate::util::is_symlink(dest);
    if had_old {
        std::fs::rename(dest, &aside)?;
    }
    if let Err(e) = std::fs::rename(new_dir, dest) {
        if had_old {
            let _ = std::fs::rename(&aside, dest);
        }
        bail!("moving new directory into place: {e}");
    }
    if had_old {
        let _ = std::fs::remove_dir_all(&aside);
    }
    Ok(())
}

pub fn require_key(key: &str) -> Result<()> {
    if !crate::repository::valid_id(key) {
        bail!("invalid skill name: {key:?}");
    }
    Ok(())
}

/// Run `git` with arguments, returning stdout. Fails with stderr on non-zero exit.
pub fn git(args: &[&str], cwd: Option<&Path>) -> Result<String> {
    let mut cmd = std::process::Command::new("git");
    cmd.args(args);
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    let out = cmd
        .output()
        .map_err(|e| anyhow::anyhow!("running git: {e}"))?;
    if !out.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Stream Git's carriage-return progress records without exposing terminal control
/// characters. Clone stdout is unused; stderr is drained before waiting on Git.
pub fn git_progress(args: &[&str], progress: &mut dyn FnMut(&str)) -> Result<()> {
    use std::io::{BufReader, Read};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    let mut child = Command::new("git")
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut record = Vec::new();
    let mut tail = std::collections::VecDeque::new();
    let mut last = Instant::now() - Duration::from_secs(1);
    let mut emit = |record: &mut Vec<u8>| {
        let text: String = String::from_utf8_lossy(record)
            .chars()
            .filter(|c| !c.is_control())
            .collect();
        record.clear();
        let text = text.trim();
        if !text.is_empty() {
            if last.elapsed() >= Duration::from_millis(100) {
                progress(&format!("Clone: {text}"));
                last = Instant::now();
            }
            tail.push_back(text.to_string());
            if tail.len() > 8 {
                tail.pop_front();
            }
        }
    };
    let read_result = (|| -> std::io::Result<()> {
        for byte in BufReader::new(child.stderr.take().expect("piped stderr")).bytes() {
            let byte = byte?;
            if byte == b'\r' || byte == b'\n' {
                emit(&mut record);
            } else if record.len() < 8192 {
                record.push(byte);
            }
        }
        emit(&mut record);
        Ok(())
    })();
    if read_result.is_err() {
        let _ = child.kill();
    }
    let status = child.wait()?;
    read_result?;
    if !status.success() {
        bail!(
            "git clone failed: {}",
            tail.into_iter().collect::<Vec<_>>().join("\n")
        );
    }
    Ok(())
}
