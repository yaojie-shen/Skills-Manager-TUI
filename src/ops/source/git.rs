use super::Acquired;
use crate::{meta::SourceKind, ops::git};
use anyhow::{Context, Result};
use std::{
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

pub(super) fn fetch(
    url: &str,
    branch: Option<&str>,
    destination: &Path,
    progress: &mut dyn FnMut(&str),
) -> Result<Acquired> {
    progress("Clone: connecting to repository…");
    let mut args = vec!["clone", "--progress", "--depth", "1"];
    if let Some(branch) = branch {
        args.extend(["--branch", branch]);
    }
    let path = destination.to_string_lossy();
    args.extend(["--", url, &path]);
    crate::ops::git_progress(&args, progress)?;
    let revision = git(&["rev-parse", "HEAD"], Some(destination))?
        .trim()
        .to_string();
    let resolved = git(&["rev-parse", "--abbrev-ref", "HEAD"], Some(destination))?
        .trim()
        .to_string();
    let branch = branch
        .map(str::to_string)
        .or_else(|| (resolved != "HEAD").then_some(resolved));
    Ok(Acquired {
        url: url.into(),
        kind: SourceKind::Git,
        branch,
        revision,
    })
}

pub(super) fn latest_revision(url: &str, branch: Option<&str>) -> Result<String> {
    let refspec = branch.unwrap_or("HEAD");
    let peeled = format!("{refspec}^{{}}");
    let output = git(&["ls-remote", "--", url, refspec, &peeled], None)?;
    let refs: Vec<_> = output
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let sha = fields.next()?;
            let name = fields.next()?;
            Some((name, sha))
        })
        .collect();
    // Clone prefers a branch with this name; annotated tags must be peeled to
    // the commit that checkout records as HEAD, rather than the tag object.
    [
        format!("refs/heads/{refspec}"),
        format!("refs/tags/{refspec}^{{}}"),
        peeled,
        refspec.to_string(),
        format!("refs/tags/{refspec}"),
    ]
    .iter()
    .find_map(|candidate| {
        refs.iter()
            .find_map(|(name, sha)| (*name == candidate).then(|| (*sha).to_string()))
    })
    .or_else(|| refs.first().map(|(_, sha)| (*sha).to_string()))
    .with_context(|| format!("no ref {refspec} at {url}"))
}

pub(super) fn checkout_revision(tree: &Path, revision: &str) -> bool {
    git(
        &["fetch", "--quiet", "--depth", "1", "origin", revision],
        Some(tree),
    )
    .is_ok()
        && git(&["checkout", "--quiet", revision], Some(tree)).is_ok()
}

/// Probe through Git so authentication, URL rewrites and supported HTTP protocols
/// match cloning. Discard the advertisement rather than limiting its total size.
pub(super) fn is_http_repository(url: &str) -> bool {
    let mut command = Command::new("git");
    command
        .args(["ls-remote", "--quiet", "--", url, "HEAD"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    command.process_group(0);
    match command.spawn() {
        Ok(child) => wait_for_probe(child, Duration::from_secs(10)),
        Err(_) => false,
    }
}

fn wait_for_probe(mut child: Child, timeout: Duration) -> bool {
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => {}
            Err(_) => break,
        }
        if started.elapsed() >= timeout {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    // The probe owns a new process group whose id is the child pid. Terminate
    // credential helpers and transport subprocesses as well as the Git process.
    #[cfg(unix)]
    unsafe {
        libc::killpg(child.id() as libc::pid_t, libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latest_revision_returns_checkout_commit_for_branches_and_tags() {
        let temp = crate::ops::DownloadDir::new("git-revision-refs").unwrap();
        let root = temp.path();
        git(&["init", "--quiet", "--initial-branch=main"], Some(root)).unwrap();
        for (key, value) in [
            ("user.name", "Fixture"),
            ("user.email", "fixture@example.invalid"),
            ("commit.gpgsign", "false"),
            ("tag.gpgsign", "false"),
            ("core.hooksPath", "/dev/null"),
        ] {
            git(&["config", key, value], Some(root)).unwrap();
        }
        git(
            &["commit", "--quiet", "--allow-empty", "-m", "fixture"],
            Some(root),
        )
        .unwrap();
        git(&["tag", "-a", "annotated", "-m", "fixture"], Some(root)).unwrap();
        git(&["tag", "lightweight"], Some(root)).unwrap();
        let expected = git(&["rev-parse", "HEAD"], Some(root))
            .unwrap()
            .trim()
            .to_string();
        let tag_object = git(&["rev-parse", "annotated"], Some(root)).unwrap();
        assert_ne!(expected, tag_object.trim());
        let url = format!("file://{}", root.display());
        for reference in [None, Some("main"), Some("annotated"), Some("lightweight")] {
            assert_eq!(latest_revision(&url, reference).unwrap(), expected);
        }
    }

    #[test]
    fn repository_probe_accepts_empty_git_repositories_and_rejects_other_directories() {
        let temp = crate::ops::DownloadDir::new("git-probe").unwrap();
        let bare = temp.path().join("repo.git");
        assert!(
            Command::new("git")
                .args(["init", "--bare", "--quiet"])
                .arg(&bare)
                .status()
                .unwrap()
                .success()
        );
        assert!(is_http_repository(&format!("file://{}", bare.display())));
        assert!(!is_http_repository(&format!(
            "file://{}",
            temp.path().display()
        )));
    }

    #[cfg(unix)]
    #[test]
    fn timed_out_probe_terminates_its_own_subprocess_group() {
        let temp = crate::ops::DownloadDir::new("git-probe-timeout").unwrap();
        let marker = temp.path().join("survived");
        let child = Command::new("sh")
            .args([
                "-c",
                "(sleep 0.6; printf survived > \"$1\") & wait",
                "probe",
            ])
            .arg(&marker)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        assert!(!wait_for_probe(child, Duration::from_millis(100)));
        std::thread::sleep(Duration::from_millis(700));
        assert!(!marker.exists());
    }
}
