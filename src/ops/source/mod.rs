//! Source providers acquire a directory tree. Discovery, selection and publication
//! belong to the shared installation flow, independent of the transport.

mod archive;
mod git;
mod reference;
mod validate;

pub use reference::{InstallRef, parse_ref};

use crate::meta::{Source, SourceKind};
use anyhow::{Result, bail};
use std::path::Path;

pub struct Acquired {
    pub url: String,
    pub kind: SourceKind,
    pub branch: Option<String>,
    pub revision: String,
}

impl Acquired {
    pub fn source(&self, subpath: Option<&str>) -> Source {
        let subpath = subpath.filter(|s| !s.is_empty()).map(str::to_string);
        match self.kind {
            SourceKind::Git => Source::Git {
                url: self.url.clone(),
                branch: self.branch.clone(),
                subpath,
                revision: Some(self.revision.clone()),
            },
            SourceKind::Archive => Source::Archive {
                url: self.url.clone(),
                subpath,
                revision: Some(self.revision.clone()),
            },
        }
    }
}

pub fn acquire(
    reference: &InstallRef,
    destination: &Path,
    progress: &mut dyn FnMut(&str),
) -> Result<Acquired> {
    if let Some(path) = reference.subpath() {
        crate::repository::validate_subpath(path)?;
    }
    match reference {
        InstallRef::Local(_) => bail!("local directories do not need a remote source provider"),
        InstallRef::Git { url, branch, .. } => {
            git::fetch(url, branch.as_deref(), destination, progress)
        }
        InstallRef::Archive { url, .. } => fetch_archive(url, destination, progress),
        InstallRef::Url { url, branch, .. } => {
            progress("Fetch: identifying URL source…");
            if git::is_http_repository(url) {
                git::fetch(url, branch.as_deref(), destination, progress)
            } else if !url.contains(['?', '#'])
                && let Some((base, inline_branch)) = url.rsplit_once('@')
                && !inline_branch.is_empty()
                && !inline_branch.contains(['/', ':'])
                && git::is_http_repository(base)
            {
                git::fetch(
                    base,
                    branch.as_deref().or(Some(inline_branch)),
                    destination,
                    progress,
                )
            } else {
                anyhow::ensure!(
                    branch.is_none(),
                    "--branch applies only to Git repositories; use the archive URL for the desired version"
                );
                fetch_archive(url, destination, progress)
            }
        }
    }
}

fn fetch_archive(
    url: &str,
    destination: &Path,
    progress: &mut dyn FnMut(&str),
) -> Result<Acquired> {
    progress("Download: fetching and extracting archive…");
    archive::download(url, destination)?;
    Ok(Acquired {
        url: url.into(),
        kind: SourceKind::Archive,
        branch: None,
        revision: crate::hash::hash_directory(destination)?,
    })
}

/// Check a selected remote skill before it is published, whatever transport
/// fetched it.
pub(crate) fn validate_skill(path: &Path) -> Result<()> {
    validate::skill_tree(path)
}

pub fn latest_revision(source: &Source) -> Result<String> {
    let reference = InstallRef::from_source(source)?;
    match &reference {
        InstallRef::Git { url, branch, .. } => git::latest_revision(url, branch.as_deref()),
        _ => {
            let download = crate::ops::DownloadDir::new("check")?;
            Ok(acquire(&reference, download.path(), &mut |_| {})?.revision)
        }
    }
}

/// Only providers with addressable history can supply a three-way diff base.
pub fn checkout_revision(reference: &InstallRef, tree: &Path, revision: &str) -> Result<bool> {
    Ok(match reference {
        InstallRef::Git { .. } => git::checkout_revision(tree, revision),
        _ => false,
    })
}
