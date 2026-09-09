//! Explicit repairs for foreign agent links. The external target is never moved or deleted.

use crate::{
    Workspace,
    ops::install::{self, InstallRef},
    reconcile::{AgentDirMode, EntryState},
    skill::SkillDoc,
};
use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::os::unix::fs::MetadataExt;
use std::{fs, path::PathBuf};

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Repair {
    Remove,
    Adopt,
}

#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    pub operation: Repair,
    pub agent: String,
    pub name: String,
    pub path: PathBuf,
    pub target: PathBuf,
    pub destination: Option<PathBuf>,
    agent_directory: PathBuf,
    raw_target: PathBuf,
    root: PathBuf,
    #[serde(skip)]
    directory_identity: (u64, u64),
    #[serde(skip)]
    link_identity: (u64, u64),
}

pub fn plan(ws: &Workspace, agent: &str, name: &str, operation: Repair) -> Result<Plan> {
    if !crate::util::valid_skill_key(name) {
        bail!("invalid agent entry name");
    }
    let snap = ws.scan_for_links()?;
    let report = snap.agent(agent).context("unknown agent")?;
    if report.mode != AgentDirMode::Real {
        bail!("agent directory must be a real directory");
    }
    if !matches!(report.entries.get(name), Some(EntryState::Foreign { .. })) {
        bail!("{agent}/{name} is not a foreign link");
    }
    let path = report.skills_dir.join(name);
    let target = fs::canonicalize(&path).context("foreign target is unavailable")?;
    let destination = matches!(operation, Repair::Adopt).then(|| ws.skill_path(name));
    let plan = Plan {
        operation,
        agent: agent.into(),
        name: name.into(),
        raw_target: fs::read_link(&path)?,
        target,
        destination,
        agent_directory: fs::canonicalize(&report.skills_dir)?,
        path,
        root: ws.root.clone(),
        directory_identity: identity(&fs::symlink_metadata(&report.skills_dir)?),
        link_identity: identity(&fs::symlink_metadata(report.skills_dir.join(name))?),
    };
    plan.validate(ws)?;
    Ok(plan)
}

impl Plan {
    fn validate_link(&self, ws: &Workspace) -> Result<()> {
        if ws.root != self.root {
            bail!("skills root changed; review the repair again");
        }
        let config = ws
            .config
            .agents
            .iter()
            .find(|a| a.key == self.agent)
            .context("agent configuration changed")?;
        let dir = config.skills_path();
        if dir.join(&self.name) != self.path
            || !fs::symlink_metadata(&dir)?.is_dir()
            || fs::canonicalize(&dir)? != self.agent_directory
            || identity(&fs::symlink_metadata(&dir)?) != self.directory_identity
        {
            bail!("agent directory changed; review the repair again");
        }
        if fs::symlink_metadata(&self.path).ok().as_ref().map(identity) != Some(self.link_identity)
            || fs::read_link(&self.path).ok().as_ref() != Some(&self.raw_target)
            || fs::canonicalize(&self.path).ok().as_ref() != Some(&self.target)
        {
            bail!("agent link changed; review the repair again");
        }
        Ok(())
    }

    fn validate(&self, ws: &Workspace) -> Result<()> {
        self.validate_link(ws)?;
        if let Some(destination) = &self.destination {
            SkillDoc::load(&self.target)
                .context("foreign target is not a valid skill; it cannot be adopted")?;
            if fs::symlink_metadata(destination).is_ok() || ws.meta.exists(&self.name) {
                bail!(
                    "{} already exists in the skills root or metadata",
                    self.name
                );
            }
        }
        Ok(())
    }

    pub fn apply(&self, ws: &Workspace) -> Result<String> {
        self.validate(ws)?;
        match self.operation {
            Repair::Remove => {
                fs::remove_file(&self.path).context("removing foreign symlink")?;
                Ok(format!(
                    "removed link {}/{}; external target preserved",
                    self.agent, self.name
                ))
            }
            Repair::Adopt => {
                install::install(
                    ws,
                    &InstallRef::Local(self.target.clone()),
                    Some(&self.name),
                )?;
                // A concurrent edit must never be replaced just because copying took time.
                self.validate_link(ws).with_context(|| format!("{} was copied into the root, but its agent link changed; review it before deploying", self.name))?;
                let destination = self
                    .destination
                    .as_ref()
                    .expect("adopt plan has a destination");
                let temp = self.agent_directory.join(format!(
                    ".skills-link-{}-{}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)?
                        .as_nanos()
                ));
                std::os::unix::fs::symlink(destination, &temp)?;
                let result = self
                    .validate_link(ws)
                    .and_then(|()| fs::rename(&temp, &self.path).map_err(Into::into));
                let _ = fs::remove_file(&temp);
                result.with_context(|| format!("{} was copied into the root, but linking failed; external target preserved", self.name))?;
                Ok(format!(
                    "adopted {} by copying; {}/{} now links to the root; external target preserved",
                    self.name, self.agent, self.name
                ))
            }
        }
    }
}

fn identity(metadata: &fs::Metadata) -> (u64, u64) {
    (metadata.dev(), metadata.ino())
}
