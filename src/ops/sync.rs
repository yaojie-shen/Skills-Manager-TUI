//! Root-wide Git backup. Configuration is local to `.git/config`.
use crate::{Workspace, ops::git};
use anyhow::{Context, Result, anyhow, ensure};
use serde::Serialize;
use std::fmt;
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
#[derive(Debug, Clone, Default, Serialize)]
pub struct Status {
    pub settings: Settings,
    pub changes: Vec<String>,
    pub ahead: usize,
    pub behind: usize,
    pub remote_checked: bool,
    #[serde(skip)]
    pub local_revision: Option<String>,
    #[serde(skip)]
    pub remote_revision: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoSyncDisposition {
    Transient,
    Fatal,
    WorkingTreeChanged,
}

#[derive(Debug)]
pub struct AutoSyncFailure {
    pub disposition: AutoSyncDisposition,
    source: anyhow::Error,
}

impl AutoSyncFailure {
    fn transient(source: anyhow::Error) -> Self {
        Self {
            disposition: AutoSyncDisposition::Transient,
            source,
        }
    }

    fn fatal(source: anyhow::Error) -> Self {
        let disposition = if source.downcast_ref::<WorkingTreeChanged>().is_some() {
            AutoSyncDisposition::WorkingTreeChanged
        } else {
            AutoSyncDisposition::Fatal
        };
        Self {
            disposition,
            source,
        }
    }
}

impl fmt::Display for AutoSyncFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.source)
    }
}

impl std::error::Error for AutoSyncFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.source()
    }
}

#[derive(Debug)]
pub struct SyncConflict {
    source: anyhow::Error,
}

impl fmt::Display for SyncConflict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("root sync conflict; local backup retained. Resolve with Git, then retry")
    }
}

impl std::error::Error for SyncConflict {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

#[derive(Debug)]
pub struct WorkingTreeChanged;

impl fmt::Display for WorkingTreeChanged {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Library changed outside this operation; automatic sync was skipped")
    }
}

impl std::error::Error for WorkingTreeChanged {}

#[derive(Debug)]
struct CoordinationBusy(String);

impl fmt::Display for CoordinationBusy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "another Skills Library operation is still in progress{}",
            self.0
        )
    }
}

impl std::error::Error for CoordinationBusy {}

#[derive(Debug)]
struct StatusCacheUnavailable(anyhow::Error);

impl fmt::Display for StatusCacheUnavailable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "root sync status cache is unavailable; check for updates and retry: {:#}",
            self.0
        )
    }
}

impl std::error::Error for StatusCacheUnavailable {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref())
    }
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

/// Inspect root backup without changing the working tree or its Git refs.
pub fn status(ws: &Workspace, check_remote: bool) -> Result<Status> {
    let settings = Settings::load(ws)?;
    if settings.url.is_none() || settings.branch.is_none() {
        return Ok(Status {
            settings,
            ..Status::default()
        });
    }
    repository(&ws.root)?;
    let changes = git(&["status", "--short"], Some(&ws.root))?
        .lines()
        .map(str::to_owned)
        .collect();
    let (ahead, behind) = if check_remote {
        compare_remote(ws, &settings)?
    } else {
        compare_ref(ws, "refs/remotes/origin/skills-root-sync").unwrap_or_default()
    };
    let local_revision = git(&["rev-parse", "--verify", "HEAD"], Some(&ws.root))
        .ok()
        .map(|revision| revision.trim().to_owned());
    let remote_revision = if check_remote {
        let cache = ws.root.join(".git/skills-sync-cache");
        git(
            &[
                "--git-dir",
                cache.to_string_lossy().as_ref(),
                "rev-parse",
                "--verify",
                "refs/heads/remote",
            ],
            None,
        )
        .ok()
        .map(|revision| revision.trim().to_owned())
    } else {
        git(
            &[
                "rev-parse",
                "--verify",
                "refs/remotes/origin/skills-root-sync",
            ],
            Some(&ws.root),
        )
        .ok()
        .map(|revision| revision.trim().to_owned())
    };
    Ok(Status {
        settings,
        changes,
        ahead,
        behind,
        remote_checked: check_remote,
        local_revision,
        remote_revision,
    })
}

fn compare_ref(ws: &Workspace, remote_ref: &str) -> Result<(usize, usize)> {
    let local = git(&["rev-parse", "--verify", "HEAD"], Some(&ws.root)).ok();
    let remote = git(&["rev-parse", "--verify", remote_ref], Some(&ws.root)).ok();
    match (local, remote) {
        (Some(_), Some(_)) => {
            let counts = git(
                &[
                    "rev-list",
                    "--left-right",
                    "--count",
                    &format!("HEAD...{remote_ref}"),
                ],
                Some(&ws.root),
            )?;
            parse_counts(&counts)
        }
        (Some(_), None) => Ok((
            git(&["rev-list", "--count", "HEAD"], Some(&ws.root))?
                .trim()
                .parse()?,
            0,
        )),
        (None, Some(_)) => Ok((
            0,
            git(&["rev-list", "--count", remote_ref], Some(&ws.root))?
                .trim()
                .parse()?,
        )),
        (None, None) => Ok((0, 0)),
    }
}

fn compare_remote(ws: &Workspace, settings: &Settings) -> Result<(usize, usize)> {
    let remote = settings.url.as_deref().context("missing remote URL")?;
    let branch = settings.branch.as_deref().context("missing sync branch")?;
    let cache = ws.root.join(".git/skills-sync-cache");
    let _guard = FileGuard::acquire(
        ws.root.join(".git/skills-sync-cache.lock"),
        "refresh root sync status",
    )?;
    if !cache_healthy(&cache, false) {
        rebuild_cache(&cache)?;
    }
    let remote_ref = format!("refs/heads/{branch}");
    let found = git(&["ls-remote", "--heads", remote, &remote_ref], None)?;
    if found.trim().is_empty() {
        ensure!(
            git(&["ls-remote", remote], None)?.trim().is_empty(),
            "configured branch is missing from a nonempty remote"
        );
    }
    for attempt in 0..2 {
        match update_status_cache(ws, &cache, remote, &remote_ref, !found.trim().is_empty())
            .and_then(|local| compare_cache(&cache, local, !found.trim().is_empty()))
        {
            Ok(counts) => return Ok(counts),
            Err(_) if attempt == 0 && !cache_healthy(&cache, true) => {
                rebuild_cache(&cache)?;
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!()
}

fn update_status_cache(
    ws: &Workspace,
    cache: &Path,
    remote: &str,
    remote_ref: &str,
    remote_exists: bool,
) -> Result<bool> {
    let git_dir = cache.to_string_lossy();
    for reference in ["refs/heads/local", "refs/heads/remote"] {
        git(
            &["--git-dir", &git_dir, "update-ref", "-d", reference],
            None,
        )?;
    }
    let local = git(&["rev-parse", "--verify", "HEAD"], Some(&ws.root)).is_ok();
    if local {
        git(
            &[
                "--git-dir",
                &git_dir,
                "fetch",
                "--quiet",
                ws.root.to_string_lossy().as_ref(),
                "+HEAD:refs/heads/local",
            ],
            None,
        )?;
    }
    if remote_exists {
        git(
            &[
                "--git-dir",
                &git_dir,
                "fetch",
                "--quiet",
                remote,
                &format!("+{remote_ref}:refs/heads/remote"),
            ],
            None,
        )?;
    }
    Ok(local)
}

fn compare_cache(cache: &Path, local: bool, remote: bool) -> Result<(usize, usize)> {
    let git_dir = cache.to_string_lossy();
    match (local, remote) {
        (true, true) => parse_counts(&git(
            &[
                "--git-dir",
                &git_dir,
                "rev-list",
                "--left-right",
                "--count",
                "refs/heads/local...refs/heads/remote",
            ],
            None,
        )?),
        (true, false) => Ok((
            git(
                &[
                    "--git-dir",
                    &git_dir,
                    "rev-list",
                    "--count",
                    "refs/heads/local",
                ],
                None,
            )?
            .trim()
            .parse()?,
            0,
        )),
        (false, true) => Ok((
            0,
            git(
                &[
                    "--git-dir",
                    &git_dir,
                    "rev-list",
                    "--count",
                    "refs/heads/remote",
                ],
                None,
            )?
            .trim()
            .parse()?,
        )),
        (false, false) => Ok((0, 0)),
    }
}

fn cache_healthy(cache: &Path, thorough: bool) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(cache) else {
        return false;
    };
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return false;
    }
    let git_dir = cache.to_string_lossy();
    git(
        &["--git-dir", &git_dir, "rev-parse", "--is-bare-repository"],
        None,
    )
    .is_ok_and(|value| value.trim() == "true")
        && (!thorough
            || git(
                &[
                    "--git-dir",
                    &git_dir,
                    "fsck",
                    "--connectivity-only",
                    "--no-dangling",
                ],
                None,
            )
            .is_ok())
}

fn rebuild_cache(cache: &Path) -> Result<()> {
    let parent = cache.parent().context("sync cache has no parent")?;
    let suffix = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let fresh = parent.join(format!("skills-sync-cache.new-{suffix}"));
    let old = parent.join(format!("skills-sync-cache.old-{suffix}"));
    let fresh_arg = fresh.to_string_lossy();
    git(&["init", "--bare", "--quiet", &fresh_arg], None)?;
    let had_old = std::fs::symlink_metadata(cache).is_ok();
    if had_old {
        std::fs::rename(cache, &old).context("retire damaged sync status cache")?;
    }
    if let Err(error) = std::fs::rename(&fresh, cache) {
        if had_old {
            let _ = std::fs::rename(&old, cache);
        }
        let _ = remove_cache_path(&fresh);
        return Err(error).context("publish rebuilt sync status cache");
    }
    if had_old {
        let _ = remove_cache_path(&old);
    }
    Ok(())
}

fn remove_cache_path(path: &Path) -> std::io::Result<()> {
    if std::fs::symlink_metadata(path)?.file_type().is_symlink() || path.is_file() {
        std::fs::remove_file(path)
    } else {
        std::fs::remove_dir_all(path)
    }
}

fn parse_counts(text: &str) -> Result<(usize, usize)> {
    let mut fields = text.split_whitespace();
    let ahead = fields.next().context("missing ahead count")?.parse()?;
    let behind = fields.next().context("missing behind count")?.parse()?;
    Ok((ahead, behind))
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
    run_checked(ws, mode, preview, None, true, false, progress)
}

/// Run an automatic sync only if the working tree still matches the status
/// accepted by the coordinator. The comparison happens under the mutation
/// lock, closing the gap between an asynchronous probe and Git staging.
pub fn run_automatic(
    ws: &Workspace,
    expected_changes: &[String],
    publishing: &mut dyn FnMut(),
    progress: &mut dyn FnMut(&str),
) -> std::result::Result<Report, AutoSyncFailure> {
    let cache_lock = FileGuard::acquire(
        ws.root.join(".git/skills-sync-cache.lock"),
        "consume root sync status",
    )
    .map_err(classify_auto_failure)?;
    let mut report = run_checked(
        ws,
        Mode::Sync,
        false,
        Some(expected_changes),
        false,
        true,
        progress,
    )
    .map_err(classify_auto_failure)?;
    drop(cache_lock);
    publishing();
    if git(&["rev-parse", "--verify", "HEAD"], Some(&ws.root)).is_ok() {
        progress("Pushing root backup …");
        let branch = Settings::load(ws)
            .and_then(|settings| settings.branch.context("root sync branch is missing"))
            .map_err(AutoSyncFailure::fatal)?;
        git(
            &["push", "origin", &format!("HEAD:refs/heads/{branch}")],
            Some(&ws.root),
        )
        .map_err(AutoSyncFailure::transient)?;
        report.pushed = true;
    }
    Ok(report)
}

fn classify_auto_failure(error: anyhow::Error) -> AutoSyncFailure {
    if error.downcast_ref::<CoordinationBusy>().is_some()
        || error.downcast_ref::<StatusCacheUnavailable>().is_some()
    {
        AutoSyncFailure::transient(error)
    } else {
        AutoSyncFailure::fatal(error)
    }
}

fn run_checked(
    ws: &Workspace,
    mode: Mode,
    preview: bool,
    expected_changes: Option<&[String]>,
    push: bool,
    cached_remote: bool,
    progress: &mut dyn FnMut(&str),
) -> Result<Report> {
    let settings = Settings::load(ws)?;
    let branch = settings
        .branch
        .context("root sync is not configured; use skills sync configure URL")?;
    repository(&ws.root)?;
    let _lock = MutationGuard::acquire(ws, "root sync")?;
    ensure_ready(ws, &branch)?;
    if let Some(expected) = expected_changes {
        let current: Vec<String> = git(&["status", "--short"], Some(&ws.root))?
            .lines()
            .map(str::to_owned)
            .collect();
        if current != expected {
            return Err(WorkingTreeChanged.into());
        }
    }
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
        if cached_remote {
            fetch_cached(ws)?;
        } else {
            fetch(ws, &branch)?;
        }
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
        if cached_remote {
            progress("Applying cached root updates …");
            fetch_cached(ws)?;
        } else {
            progress("Fetching root updates …");
            fetch(ws, &branch)?;
        }
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
                    let conflicted = git(&["ls-files", "-u"], Some(&ws.root))
                        .is_ok_and(|output| !output.trim().is_empty());
                    if ws.root.join(".git/MERGE_HEAD").exists() {
                        git(&["merge", "--abort"], Some(&ws.root))
                            .context("merge failed and could not be aborted; inspect Git status")?;
                    }
                    if conflicted {
                        return Err(SyncConflict { source: error }.into());
                    }
                    return Err(error);
                }
                if ws.root.join(".git/MERGE_HEAD").exists() {
                    commit(ws, "Merge skills root updates")?;
                }
                report.pulled |= git(&["rev-parse", "HEAD"], Some(&ws.root))? != before;
            }
        }
    }
    if push
        && !matches!(mode, Mode::Pull)
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

fn fetch_cached(ws: &Workspace) -> Result<()> {
    let cache = ws.root.join(".git/skills-sync-cache");
    if !cache_healthy(&cache, false) {
        return Err(
            StatusCacheUnavailable(anyhow!("root sync status cache is unavailable")).into(),
        );
    }
    let cache_arg = cache.to_string_lossy();
    let imported = if git(
        &[
            "--git-dir",
            &cache_arg,
            "rev-parse",
            "--verify",
            "refs/heads/remote",
        ],
        None,
    )
    .is_ok()
    {
        git(
            &[
                "fetch",
                "--no-tags",
                &cache_arg,
                "+refs/heads/remote:refs/remotes/origin/skills-root-sync",
            ],
            Some(&ws.root),
        )
    } else {
        git(
            &["update-ref", "-d", "refs/remotes/origin/skills-root-sync"],
            Some(&ws.root),
        )
    };
    imported
        .map(|_| ())
        .map_err(|error| StatusCacheUnavailable(error).into())
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
pub struct MutationGuard {
    _guard: FileGuard,
}

impl MutationGuard {
    pub fn acquire(ws: &Workspace, operation: &str) -> Result<Self> {
        let git_dir = ws.root.join(".git");
        if !git_dir.is_dir() {
            return Ok(Self {
                _guard: FileGuard(None),
            });
        }
        FileGuard::acquire(git_dir.join("skills-sync.lock"), operation)
            .map(|guard| Self { _guard: guard })
    }
}

struct FileGuard(Option<(PathBuf, String)>);

impl FileGuard {
    fn acquire(path: PathBuf, operation: &str) -> Result<Self> {
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
                    return Err(CoordinationBusy(owner).into());
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

impl Drop for FileGuard {
    fn drop(&mut self) {
        if let Some((path, owner)) = &self.0
            && std::fs::read_to_string(path).is_ok_and(|current| current == *owner)
        {
            let _ = std::fs::remove_file(path);
        }
    }
}
