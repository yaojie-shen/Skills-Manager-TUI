//! Root-wide Git backup. Configuration is local to `.git/config`.
use crate::{Workspace, ops::git};
use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;
use std::io::Write;
use std::path::{Path, PathBuf};

const EXCLUDES: &[&str] = &[
    ".skills-meta/.sync",
    ".skills-meta/.staging",
    ".skills-meta/.repair-backups",
    ".skills-meta/.metadata.lock",
    ".skills-meta/backups",
];

#[derive(Debug, Clone, Default, Serialize)]
pub struct Settings {
    pub url: Option<String>,
    pub branch: Option<String>,
    pub enabled: bool,
}
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub enum Mode {
    #[default]
    Sync,
    Pull,
    Push,
}
#[derive(Debug, Default, Serialize)]
pub struct Report {
    pub committed: bool,
    pub pulled: bool,
    pub pushed: bool,
    pub preview: bool,
    pub status: String,
}
fn repository(root: &Path) -> Result<()> {
    let meta = std::fs::symlink_metadata(root.join(".git"))?;
    ensure!(
        meta.is_dir() && !meta.file_type().is_symlink(),
        "root must own its .git directory (not a parent repository or linked worktree)"
    );
    let top = git(&["rev-parse", "--show-toplevel"], Some(root))?;
    ensure!(
        std::fs::canonicalize(top.trim())? == std::fs::canonicalize(root)?,
        "Git repository must be the skills root"
    );
    Ok(())
}
fn config(root: &Path, key: &str) -> Option<String> {
    git(&["config", "--local", "--get", key], Some(root))
        .ok()
        .map(|s| s.trim().to_owned())
}
impl Settings {
    pub fn load(ws: &Workspace) -> Result<Self> {
        if !ws.root.join(".git").exists() {
            return Ok(Self::default());
        }
        repository(&ws.root)?;
        Ok(Self {
            url: config(&ws.root, "remote.origin.url"),
            branch: config(&ws.root, "skills.sync-branch"),
            enabled: config(&ws.root, "skills.autosync").as_deref() == Some("true"),
        })
    }
}
/// Explicit opt-in: never infer a destination from old per-skill bindings.
pub fn configure(ws: &Workspace, url: &str, branch: &str) -> Result<()> {
    ensure!(
        !url.is_empty() && !url.starts_with('-'),
        "invalid remote URL"
    );
    git(&["check-ref-format", &format!("refs/heads/{branch}")], None)?;
    if !ws.root.join(".git").exists() {
        std::fs::create_dir_all(&ws.root)?;
        git(
            &["init", "--quiet", "--initial-branch", branch],
            Some(&ws.root),
        )?;
    }
    repository(&ws.root)?;
    let _lock = MutationGuard::acquire(ws, "configure root sync")?;
    ensure_ready(ws, branch)?;
    if let Some(existing) = config(&ws.root, "remote.origin.url") {
        ensure!(
            existing == url,
            "origin already points to another URL; change it explicitly with Git first"
        );
    } else {
        git(&["remote", "add", "origin", url], Some(&ws.root))?;
    }
    install_excludes(ws)?;
    git(
        &["config", "--local", "skills.sync-branch", branch],
        Some(&ws.root),
    )?;
    git(
        &["config", "--local", "skills.autosync", "true"],
        Some(&ws.root),
    )?;
    Ok(())
}
pub fn disable(ws: &Workspace) -> Result<()> {
    repository(&ws.root)?;
    let _lock = MutationGuard::acquire(ws, "disable root sync")?;
    git(
        &["config", "--local", "skills.autosync", "false"],
        Some(&ws.root),
    )?;
    Ok(())
}
fn excluded(path: &str) -> bool {
    EXCLUDES
        .iter()
        .any(|p| path == *p || path.starts_with(&format!("{p}/")))
}
fn install_excludes(ws: &Workspace) -> Result<()> {
    let path = ws.root.join(".git/info/exclude");
    crate::paths::ensure_local_path(&ws.root, &path)?;
    let mut text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    for entry in EXCLUDES {
        let pattern = format!("/{entry}");
        if !text.lines().any(|line| line == pattern) {
            text.push('\n');
            text.push_str(&pattern);
            text.push('\n');
        }
    }
    crate::util::write_atomic(&path, text.as_bytes())
}
fn ensure_ready(ws: &Workspace, branch: &str) -> Result<()> {
    let current = git(&["symbolic-ref", "--short", "HEAD"], Some(&ws.root))?;
    ensure!(
        current.trim() == branch,
        "root is on {}; expected {branch}; switch branches explicitly",
        current.trim()
    );
    for marker in [
        "MERGE_HEAD",
        "rebase-merge",
        "rebase-apply",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "index.lock",
    ] {
        ensure!(
            !ws.root.join(".git").join(marker).exists(),
            "Git operation in progress: {marker}; finish it before syncing"
        );
    }
    Ok(())
}
fn validate_tree(ws: &Workspace, revision: Option<&str>) -> Result<()> {
    let listing = if let Some(rev) = revision {
        git(&["ls-tree", "-r", "-z", rev], Some(&ws.root))?
    } else {
        git(&["ls-files", "--stage", "-z"], Some(&ws.root))?
    };
    for item in listing.split('\0').filter(|s| !s.is_empty()) {
        let (mode, path) = item.split_once('\t').context("invalid Git tree entry")?;
        ensure!(
            path != ".skills-sync.json",
            "legacy per-skill backup layout is not a root repository; use a new root remote or migrate it explicitly"
        );
        ensure!(
            !mode.starts_with("160000"),
            "nested Git repositories/submodules are not supported: {path}"
        );
        ensure!(
            !excluded(path),
            "runtime file is tracked in Git: {path}; remove it from the index before syncing"
        );
        ensure!(
            !(mode.starts_with("120000")
                && (path == ".skills-meta" || path.starts_with(".skills-meta/"))),
            "metadata cannot be a symlink: {path}"
        );
    }
    Ok(())
}
fn commit(ws: &Workspace, message: &str) -> Result<()> {
    git(
        &[
            "-c",
            "user.name=skills sync",
            "-c",
            "user.email=skills-sync@localhost",
            "-c",
            "commit.gpgSign=false",
            "commit",
            "--quiet",
            "-m",
            message,
        ],
        Some(&ws.root),
    )?;
    Ok(())
}
/// Saves local work first. Conflicting merges are aborted; the backup commit remains.
pub fn run(
    ws: &Workspace,
    mode: Mode,
    preview: bool,
    progress: &mut dyn FnMut(&str),
) -> Result<Report> {
    let settings = Settings::load(ws)?;
    let branch = settings
        .branch
        .context("root sync is not configured; use skills sync configure URL")?;
    repository(&ws.root)?;
    let _lock = MutationGuard::acquire(ws, "root sync")?;
    ensure_ready(ws, &branch)?;
    validate_tree(ws, None)?;
    let mut report = Report {
        preview,
        status: git(&["status", "--short"], Some(&ws.root))?,
        ..Default::default()
    };
    if preview {
        return Ok(report);
    }
    install_excludes(ws)?;
    // Refuse nested repos before git add can turn them into gitlinks.
    for entry in walkdir::WalkDir::new(&ws.root)
        .min_depth(1)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            let rel = e.path().strip_prefix(&ws.root).unwrap();
            rel != Path::new(".git") && !excluded(&rel.to_string_lossy())
        })
    {
        let entry = entry?;
        ensure!(
            entry.file_name() != ".git",
            "nested Git repository: {}",
            entry.path().display()
        );
    }
    let has_head = git(&["rev-parse", "--verify", "HEAD"], Some(&ws.root)).is_ok();
    // A fresh empty root can attach directly; Git refuses to overwrite local files.
    if !has_head && !matches!(mode, Mode::Push) {
        fetch(ws, &branch)?;
        if git(
            &[
                "rev-parse",
                "--verify",
                "refs/remotes/origin/skills-root-sync",
            ],
            Some(&ws.root),
        )
        .is_ok()
        {
            validate_tree(ws, Some("refs/remotes/origin/skills-root-sync"))?;
            git(
                &[
                    "checkout",
                    "--no-overwrite-ignore",
                    "-B",
                    &branch,
                    "refs/remotes/origin/skills-root-sync",
                ],
                Some(&ws.root),
            )?;
            report.pulled = true;
        }
    }
    progress("Saving root changes …");
    git(&["add", "--all", "--", "."], Some(&ws.root))?;
    validate_tree(ws, None)?;
    if !git(&["diff", "--cached", "--name-only"], Some(&ws.root))?
        .trim()
        .is_empty()
    {
        commit(ws, "Back up skills root")?;
        report.committed = true;
    }
    if !matches!(mode, Mode::Push) {
        progress("Fetching root updates …");
        fetch(ws, &branch)?;
        let remote_ref = "refs/remotes/origin/skills-root-sync";
        if let Ok(remote) = git(&["rev-parse", "--verify", remote_ref], Some(&ws.root)) {
            validate_tree(ws, Some(remote_ref))?;
            let before = git(&["rev-parse", "HEAD"], Some(&ws.root))?;
            if before != remote {
                let merge = git(
                    &[
                        "-c",
                        "user.name=skills sync",
                        "-c",
                        "user.email=skills-sync@localhost",
                        "-c",
                        "commit.gpgSign=false",
                        "merge",
                        "--no-overwrite-ignore",
                        "--no-edit",
                        "--no-commit",
                        remote_ref,
                    ],
                    Some(&ws.root),
                );
                if let Err(error) = merge {
                    if ws.root.join(".git/MERGE_HEAD").exists() {
                        git(&["merge", "--abort"], Some(&ws.root))
                            .context("merge failed and could not be aborted; inspect Git status")?;
                    }
                    bail!(
                        "root sync stopped; local backup retained. Resolve with Git, then retry: {error:#}"
                    );
                }
                if ws.root.join(".git/MERGE_HEAD").exists() {
                    commit(ws, "Merge skills root updates")?;
                }
                report.pulled |= git(&["rev-parse", "HEAD"], Some(&ws.root))? != before;
            }
        }
    }
    if !matches!(mode, Mode::Pull)
        && git(&["rev-parse", "--verify", "HEAD"], Some(&ws.root)).is_ok()
    {
        progress("Pushing root backup …");
        git(
            &["push", "origin", &format!("HEAD:refs/heads/{branch}")],
            Some(&ws.root),
        )?;
        report.pushed = true;
    }
    report.status = git(&["status", "--short"], Some(&ws.root))?;
    Ok(report)
}
fn fetch(ws: &Workspace, branch: &str) -> Result<()> {
    let found = git(
        &[
            "ls-remote",
            "--heads",
            "origin",
            &format!("refs/heads/{branch}"),
        ],
        Some(&ws.root),
    )?;
    if found.trim().is_empty() {
        // Do not merge stale tracking data after a remote branch deletion.
        git(
            &["update-ref", "-d", "refs/remotes/origin/skills-root-sync"],
            Some(&ws.root),
        )?;
        ensure!(
            git(&["ls-remote", "origin"], Some(&ws.root))?
                .trim()
                .is_empty(),
            "configured branch is missing from a nonempty remote"
        );
    } else {
        git(
            &[
                "fetch",
                "--no-tags",
                "origin",
                &format!("+refs/heads/{branch}:refs/remotes/origin/skills-root-sync"),
            ],
            Some(&ws.root),
        )?;
    }
    Ok(())
}
pub fn automatic(ws: &Workspace) -> Result<Option<Report>> {
    if !Settings::load(ws)?.enabled {
        return Ok(None);
    }
    run(ws, Mode::Sync, false, &mut |_| {}).map(Some)
}
/// Serializes root Git operations with Library writes across processes.
///
/// Agent-only deployment operations deliberately do not acquire this guard.
pub struct MutationGuard(Option<(PathBuf, String)>);

impl MutationGuard {
    pub fn acquire(ws: &Workspace, operation: &str) -> Result<Self> {
        let git_dir = ws.root.join(".git");
        if !git_dir.is_dir() {
            return Ok(Self(None));
        }
        let path = git_dir.join("skills-sync.lock");
        for attempt in 0..2 {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut file) => {
                    let owner = format!("{}\t{operation}\n", std::process::id());
                    if let Err(error) = file
                        .write_all(owner.as_bytes())
                        .and_then(|_| file.sync_all())
                    {
                        let _ = std::fs::remove_file(&path);
                        return Err(error).context("initialize Skills coordination lock");
                    }
                    return Ok(Self(Some((path, owner))));
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if attempt == 0 && stale_lock(&path) {
                        std::fs::remove_file(&path)
                            .context("remove stale Skills coordination lock")?;
                        continue;
                    }
                    let owner = std::fs::read_to_string(&path)
                        .ok()
                        .filter(|text| !text.trim().is_empty())
                        .map(|text| format!(" ({})", text.trim()))
                        .unwrap_or_default();
                    bail!("another Skills Library operation is still in progress{owner}");
                }
                Err(error) => return Err(error).context("create Skills coordination lock"),
            }
        }
        unreachable!()
    }
}

fn stale_lock(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    let Some(pid) = text
        .split_whitespace()
        .next()
        .and_then(|value| value.parse::<i32>().ok())
    else {
        return false;
    };
    #[cfg(unix)]
    {
        // Signal 0 checks process existence without delivering a signal.
        let result = unsafe { libc::kill(pid, 0) };
        result != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

impl Drop for MutationGuard {
    fn drop(&mut self) {
        if let Some((path, owner)) = &self.0
            && std::fs::read_to_string(path).is_ok_and(|current| current == *owner)
        {
            let _ = std::fs::remove_file(path);
        }
    }
}
