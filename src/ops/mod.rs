//! Write operations. Every function here is user-initiated; scanning lives in `reconcile`.

pub mod agent_links;
pub mod deploy;
pub mod edit;
pub mod install;
pub mod name_choices;
pub mod repair;
pub mod source;
pub mod sync;
pub mod targets;
pub mod update;

use anyhow::{Result, bail};
use std::path::{Path, PathBuf};

/// Which durable area an operation mutates.
///
/// Library mutations participate in root Git coordination. Deployment
/// mutations only touch Agent directories and never schedule root sync.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MutationScope {
    #[default]
    Library,
    Deployment,
}

impl MutationScope {
    /// Library writes share one cross-process lock and schedule root backup.
    pub const fn changes_library(self) -> bool {
        matches!(self, Self::Library)
    }

    /// Acquire coordination only when this scope can modify the Library.
    pub fn guard(
        self,
        ws: &crate::Workspace,
        operation: &str,
    ) -> Result<Option<sync::MutationGuard>> {
        self.changes_library()
            .then(|| sync::MutationGuard::acquire(ws, operation))
            .transpose()
    }
}

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

pub fn require_existing_keys(ws: &crate::Workspace, keys: &[String]) -> Result<()> {
    if keys.is_empty() {
        return Ok(());
    }
    let snap = ws.inventory()?;
    let missing: std::collections::BTreeSet<_> =
        keys.iter().filter(|key| snap.get(key).is_none()).collect();
    if !missing.is_empty() {
        bail!(
            "no such skill: {}",
            missing.into_iter().cloned().collect::<Vec<_>>().join(", ")
        );
    }
    Ok(())
}

fn git_command() -> std::process::Command {
    git_command_with(|key| std::env::var_os(key))
}

fn git_command_with(
    mut environment: impl FnMut(&str) -> Option<std::ffi::OsString>,
) -> std::process::Command {
    let mut command = std::process::Command::new("git");
    command.env("GIT_TERMINAL_PROMPT", "0");
    for (key, value) in [
        ("GIT_HTTP_LOW_SPEED_LIMIT", "1000"),
        ("GIT_HTTP_LOW_SPEED_TIME", "60"),
    ] {
        if environment(key).is_none() {
            command.env(key, value);
        }
    }
    command
}

/// Run `git` with arguments, returning stdout. Fails with stderr on non-zero exit.
pub fn git(args: &[&str], cwd: Option<&Path>) -> Result<String> {
    let mut cmd = git_command();
    cmd.args(args);
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
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
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    let mut command = git_command();
    let mut child = command
        .args(args)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::{OsStr, OsString};

    fn command_env(command: &std::process::Command, key: &str) -> Option<Option<OsString>> {
        command
            .get_envs()
            .find(|(name, _)| *name == OsStr::new(key))
            .map(|(_, value)| value.map(OsStr::to_os_string))
    }

    #[test]
    fn git_stall_defaults_do_not_override_environment() {
        let defaults = git_command_with(|_| None);
        assert_eq!(
            command_env(&defaults, "GIT_HTTP_LOW_SPEED_LIMIT"),
            Some(Some("1000".into()))
        );
        assert_eq!(
            command_env(&defaults, "GIT_HTTP_LOW_SPEED_TIME"),
            Some(Some("60".into()))
        );

        let inherited = git_command_with(|key| Some(format!("custom-{key}").into()));
        assert_eq!(command_env(&inherited, "GIT_HTTP_LOW_SPEED_LIMIT"), None);
        assert_eq!(command_env(&inherited, "GIT_HTTP_LOW_SPEED_TIME"), None);
    }
}
