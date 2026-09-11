//! Git repository identities and discovery. Aliases are labels, never URL encodings.
use crate::{
    Workspace,
    meta::Source,
    ops::{DownloadDir, fresh_staging, git},
    util::{valid_skill_key, write_atomic},
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Assign predictable directory names without changing an explicit user alias.
/// Sorted paths claim their declared name first; later collisions use the full path,
/// then numeric suffixes. Reserve overrides before allocating any defaults.
pub fn resolve_local_names(
    paths: &[String],
    overrides: &BTreeMap<String, String>,
    occupied: &BTreeSet<String>,
    declared_names: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>> {
    let sorted: BTreeSet<_> = paths.iter().collect();
    if sorted.len() != paths.len() {
        bail!("duplicate skill selection")
    }
    let mut used = occupied.clone();
    let mut result = BTreeMap::new();
    for path in &sorted {
        validate_subpath(path)?;
        if let Some(name) = overrides.get(*path) {
            if !valid_skill_key(name) {
                bail!("invalid local skill name for {path:?}: {name:?}")
            }
            if !used.insert(name.clone()) {
                bail!(
                    "local skill name {name:?} for {path:?} is already occupied; choose another name"
                )
            }
            result.insert((*path).clone(), name.clone());
        }
    }
    for path in sorted {
        if result.contains_key(path) {
            continue;
        }
        let base = declared_names
            .get(path)
            .context("missing declared skill name")?;
        if !valid_skill_key(base) {
            bail!("invalid default local skill name for {path:?}: {base:?}; choose a local name")
        }
        let mut name = base.to_string();
        if used.contains(&name) {
            let fallback = if path.is_empty() {
                base.to_string()
            } else {
                path.replace('/', "--")
            };
            name = fallback.clone();
            let mut suffix = 2;
            while used.contains(&name) {
                name = format!("{fallback}--{suffix}");
                suffix += 1;
            }
        }
        used.insert(name.clone());
        result.insert(path.clone(), name);
    }
    Ok(result)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Repository {
    pub alias: String,
    pub url: String,
    pub branch: String,
}

impl Repository {
    pub fn path(root: &Path, alias: &str) -> PathBuf {
        crate::paths::meta_dir(root)
            .join("repos")
            .join(format!("{alias}.toml"))
    }
    pub fn list(root: &Path) -> Result<Vec<Self>> {
        let dir = crate::paths::meta_dir(root).join("repos");
        if !dir.exists() {
            return Ok(vec![]);
        }
        let mut out = Vec::new();
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            if entry.path().extension().is_some_and(|e| e == "toml") {
                let text = std::fs::read_to_string(entry.path())?;
                let doc: toml::Value = toml::from_str(&text)?;
                if doc.get("url").is_none() {
                    continue;
                }
                let repo: Self = doc.try_into()?;
                if !valid_skill_key(&repo.alias) {
                    bail!("invalid repository alias")
                }
                out.push(repo);
            }
        }
        out.sort_by(|a, b| a.alias.cmp(&b.alias));
        Ok(out)
    }
    pub fn validate(&self, ws: &Workspace) -> Result<()> {
        if !valid_skill_key(&self.alias) {
            bail!("invalid repository alias: {:?}", self.alias)
        }
        if ws.root.join("repos/SKILL.md").exists()
            || crate::util::is_symlink(&ws.root.join("repos"))
        {
            bail!("repos is already a local skill or symlink; cannot use it for repository storage")
        }
        if let Some(existing) = Self::list(&ws.root)?
            .into_iter()
            .find(|r| r.alias == self.alias)
        {
            if existing != *self {
                bail!(
                    "repository alias {} is already assigned to {} on {}; choose another alias",
                    self.alias,
                    existing.url,
                    existing.branch
                )
            }
        } else if ws.root.join("repos").join(&self.alias).exists() {
            bail!(
                "repository directory {} already exists without a matching source record",
                self.alias
            )
        }
        Ok(())
    }
    pub fn save(&self, ws: &Workspace) -> Result<()> {
        let _lock = ws.meta.lock()?;
        self.validate(ws)?;
        let path = Self::path(&ws.root, &self.alias);
        let mut doc = crate::meta::MetaStore::read(&path)?;
        doc["alias"] = toml_edit::value(self.alias.as_str());
        doc["url"] = toml_edit::value(self.url.as_str());
        doc["branch"] = toml_edit::value(self.branch.as_str());
        write_atomic(&path, doc.to_string().as_bytes())
    }
}

/// User-facing repository identity, independent of the chosen storage alias.
pub fn source_name(url: &str) -> Option<String> {
    if url.chars().any(char::is_whitespace) {
        return None;
    }
    let url = url.trim_end_matches('/').trim_end_matches(".git");
    let parts: Vec<_> = url
        .rsplit(['/', ':'])
        .filter(|s| !s.is_empty())
        .take(2)
        .collect();
    (parts.len() == 2).then(|| format!("{}/{}", parts[1], parts[0]))
}

pub fn default_alias(url: &str) -> String {
    let url = url.trim_end_matches('/').trim_end_matches(".git");
    let pieces: Vec<_> = url.split(['/', ':']).filter(|s| !s.is_empty()).collect();
    let n = pieces.len();
    if n >= 2 {
        format!("{}--{}", pieces[n - 2], pieces[n - 1])
    } else {
        "repository".into()
    }
}

/// Relative identities preserve the upstream path; old flat keys remain valid.
pub fn valid_id(key: &str) -> bool {
    if !key.contains('/') {
        return valid_skill_key(key);
    }
    key.split('/').all(valid_skill_key)
        && ((key.starts_with("repos/") && key.split('/').count() == 3)
            || (key.starts_with("local/") && matches!(key.split('/').count(), 2 | 3)))
}
pub fn alias_of(key: &str) -> Option<&str> {
    key.strip_prefix("repos/")?.split('/').next()
}
pub fn default_deploy_name(key: &str) -> String {
    // Repository aliases identify sources in storage, not the skill's name
    // in an agent directory. Preserve explicit local names verbatim.
    if let Some(path) = key.strip_prefix("repos/") {
        return path.rsplit('/').next().unwrap_or(path).to_string();
    }
    key.strip_prefix("local/").unwrap_or(key).replace('/', "--")
}

#[derive(Debug, Clone)]
pub struct FetchedRepository {
    pub repository: Repository,
    pub revision: String,
    pub workdir: PathBuf,
    pub choices: Vec<String>,
    /// Discovered files that cannot be installed, keyed by upstream path.
    pub invalid: std::collections::BTreeMap<String, String>,
}
impl FetchedRepository {
    pub fn cleanup(&self) {
        let _ = std::fs::remove_dir_all(&self.workdir);
    }
    pub fn fetch(
        ws: &Workspace,
        reference: &crate::ops::install::InstallRef,
        alias: Option<&str>,
    ) -> Result<Self> {
        Self::fetch_with_progress(ws, reference, alias, &mut |_| {})
    }
    pub fn fetch_with_progress(
        ws: &Workspace,
        reference: &crate::ops::install::InstallRef,
        alias: Option<&str>,
        progress: &mut dyn FnMut(&str),
    ) -> Result<Self> {
        let crate::ops::install::InstallRef::Git {
            url,
            branch,
            subpath,
        } = reference
        else {
            bail!("expected a Git repository")
        };
        if let Some(path) = subpath {
            validate_subpath(path)?;
        }
        let download = DownloadDir::new("repository")?;
        let workdir = download.path().to_path_buf();
        let result = (|| {
            let mut args = vec!["clone", "--progress", "--depth", "1"];
            if let Some(branch) = branch {
                args.extend(["--branch", branch]);
            }
            args.extend([url, workdir.to_str().context("invalid staging path")?]);
            progress("Clone: connecting to remote…");
            crate::ops::git_progress(&args, progress)?;
            progress("Scan: looking for SKILL.md…");
            let revision = git(&["rev-parse", "HEAD"], Some(&workdir))?
                .trim()
                .to_string();
            let branch = branch.clone().unwrap_or(
                git(&["rev-parse", "--abbrev-ref", "HEAD"], Some(&workdir))?
                    .trim()
                    .to_string(),
            );
            let mut choices = Vec::new();
            let mut invalid = std::collections::BTreeMap::new();
            for entry in walkdir::WalkDir::new(&workdir)
                .follow_links(false)
                .into_iter()
                .filter_entry(|e| {
                    e.depth() == 0 || !e.file_name().to_string_lossy().starts_with('.')
                })
            {
                let entry = entry?;
                if entry.file_type().is_file() && entry.file_name() == "SKILL.md" {
                    let rel = entry
                        .path()
                        .parent()
                        .unwrap()
                        .strip_prefix(&workdir)?
                        .to_string_lossy()
                        .to_string();
                    if subpath
                        .as_ref()
                        .is_none_or(|s| rel == *s || rel.starts_with(&format!("{s}/")))
                    {
                        progress(&format!(
                            "Scan: found {} — {} skills",
                            if rel.is_empty() { "." } else { &rel },
                            choices.len() + 1
                        ));
                        if let Err(error) =
                            crate::skill::SkillDoc::load(entry.path().parent().unwrap())
                        {
                            invalid.insert(rel.clone(), format!("{error:#}"));
                        }
                        choices.push(rel);
                    }
                }
            }
            choices.sort();
            if choices.is_empty() {
                bail!("no SKILL.md found at the requested repository path")
            }
            let alias = match alias {
                Some(a) => a.to_string(),
                None => Repository::list(&ws.root)?
                    .into_iter()
                    .find(|r| r.url == *url && r.branch == branch)
                    .map(|r| r.alias)
                    .unwrap_or_else(|| default_alias(url)),
            };
            Ok(Self {
                repository: Repository {
                    alias,
                    url: url.clone(),
                    branch,
                },
                revision,
                workdir: workdir.clone(),
                choices,
                invalid,
            })
        })();
        if result.is_ok() {
            download.keep();
        }
        result
    }
    pub fn local_name(&self, path: &str) -> String {
        crate::skill::SkillDoc::load(&self.workdir.join(path))
            .map(|doc| doc.name)
            .unwrap_or_default()
    }

    /// Preview the same collision resolution used when publishing the install.
    pub fn resolved_names(
        &self,
        ws: &Workspace,
        paths: &[String],
        overrides: &BTreeMap<String, String>,
    ) -> Result<BTreeMap<String, String>> {
        self.repository.validate(ws)?;
        let prefix = format!("repos/{}/", self.repository.alias);
        let mut occupied: BTreeSet<String> = ws
            .meta
            .list_keys()?
            .into_iter()
            .filter_map(|key| key.strip_prefix(&prefix).map(str::to_string))
            .collect();
        let dir = ws.root.join("repos").join(&self.repository.alias);
        if dir.is_dir() {
            for entry in std::fs::read_dir(dir)? {
                occupied.insert(entry?.file_name().to_string_lossy().into_owned());
            }
        }
        resolve_local_names(
            paths,
            overrides,
            &occupied,
            &paths
                .iter()
                .map(|p| (p.clone(), self.local_name(p)))
                .collect(),
        )
    }

    pub fn install(
        &self,
        ws: &Workspace,
        paths: &[String],
        names: &std::collections::BTreeMap<String, String>,
    ) -> Result<Vec<String>> {
        self.install_with_progress(ws, paths, names, &mut |_| {})
    }
    pub fn install_with_progress(
        &self,
        ws: &Workspace,
        paths: &[String],
        names: &std::collections::BTreeMap<String, String>,
        progress: &mut dyn FnMut(&str),
    ) -> Result<Vec<String>> {
        progress("Install: validating selection…");

        if paths.is_empty() {
            bail!("select at least one skill")
        }
        let existing = ws.scan()?;
        let paths: Vec<String> = paths.iter().filter(|path| {
            let installed = existing.skills.iter().any(|r| {
                matches!(&r.source, Some(Source::Git { url, subpath, .. })
                    if url == &self.repository.url && subpath.as_deref().unwrap_or("") == path.as_str())
            });
            if installed {
                progress(&format!("Already installed: {path}; skipped"));
            }
            !installed
        }).cloned().collect();
        if paths.is_empty() {
            return Ok(vec![]);
        }
        let paths = paths.as_slice();
        for path in paths {
            validate_subpath(path)?;
            crate::skill::SkillDoc::load(&self.workdir.join(path))
                .with_context(|| format!("invalid skill at {path}/SKILL.md"))?;
        }
        let names = self.resolved_names(ws, paths, names)?;

        let mut keys = Vec::new();
        for path in paths {
            validate_subpath(path)?;
            if !self.choices.contains(path) {
                bail!("skill path was not discovered: {path}")
            }
            let name = names.get(path).expect("resolved selected path");
            if *name != self.local_name(path) {
                progress(&format!(
                    "Warning: {path} declares {}; stored as {name} (name unchanged)",
                    self.local_name(path)
                ));
            }

            if !valid_skill_key(name) {
                bail!("invalid local skill name: {name:?}")
            }
            let key = format!("repos/{}/{}", self.repository.alias, name);
            if keys.contains(&key) {
                bail!("duplicate selection: {path}")
            }
            let dest = ws.skill_path(&key);
            if dest.exists() || ws.meta.exists(&key) {
                bail!("{key} is already installed")
            }
            for parent in dest.ancestors().skip(1).take_while(|p| *p != ws.root) {
                if parent.join("SKILL.md").exists() {
                    bail!(
                        "cannot install inside another installed skill: {}",
                        parent.display()
                    )
                }
                if crate::util::is_symlink(parent) {
                    bail!("repository storage must not traverse a symlink")
                }
            }
            crate::skill::SkillDoc::load(&self.workdir.join(path)).with_context(|| {
                format!(
                    "invalid skill at {}/SKILL.md",
                    if path.is_empty() { "." } else { path }
                )
            })?;
            keys.push(key);
        }
        for a in paths {
            for b in paths {
                if overlaps(a, b) {
                    bail!(
                        "overlapping skill selections: {a:?} and {b:?}; select either the ancestor or descendants"
                    )
                }
            }
        }
        // Prepare every copy before publishing any skill directory.
        let transaction = fresh_staging(&ws.root, "repository-install")?;
        std::fs::create_dir_all(&transaction)?;
        let result = (|| {
            let mut metas = Vec::new();
            for (i, path) in paths.iter().enumerate() {
                progress(&format!("Copy: {}/{} — {}", i + 1, paths.len(), path));
                let staged = transaction.join(i.to_string());
                crate::util::copy_dir(&self.workdir.join(path), &staged)?;
                let _ = std::fs::remove_dir_all(staged.join(".git"));
                metas.push(crate::meta::SkillMeta {
                    installed_name: Some(crate::skill::SkillDoc::load(&staged)?.name),
                    source: Some(Source::Git {
                        url: self.repository.url.clone(),
                        branch: Some(self.repository.branch.clone()),
                        subpath: Some(path.clone()),
                        revision: Some(self.revision.clone()),
                    }),
                    baseline: Some(crate::meta::Baseline {
                        hash: crate::hash::hash_directory(&staged)?,
                        hash_algo: crate::hash::HASH_ALGO,
                    }),
                    ..Default::default()
                });
            }
            self.repository.save(ws)?;
            let mut published = Vec::new();
            let publish = (|| {
                for (i, key) in keys.iter().enumerate() {
                    progress(&format!("Save: {}/{} — {key}", i + 1, keys.len()));
                    let dest = ws.skill_path(key);
                    if dest.exists() || crate::util::is_symlink(&dest) || ws.meta.exists(key) {
                        bail!("{key} appeared during installation; refusing to overwrite")
                    }
                    std::fs::create_dir_all(dest.parent().context("skill parent missing")?)?;
                    std::fs::rename(transaction.join(i.to_string()), &dest)?;
                    published.push(key);
                    ws.meta.save(key, &metas[i])?;
                }
                Ok(())
            })();
            if publish.is_err() {
                for key in published {
                    let _ = std::fs::remove_dir_all(ws.skill_path(key));
                    let _ = ws.meta.remove(key);
                }
            }
            publish
        })();
        let _ = std::fs::remove_dir_all(transaction);
        result?;
        Ok(keys)
    }
}

pub fn validate_subpath(path: &str) -> Result<()> {
    if !path.is_empty() && !path.split('/').all(valid_skill_key) {
        bail!("invalid repository skill path: {path:?}")
    }
    Ok(())
}

/// Strict ancestry only: siblings and descendants of siblings are independent.
pub fn overlaps(a: &str, b: &str) -> bool {
    a != b && (a.is_empty() || b.starts_with(&format!("{a}/")))
}
pub fn related(a: &str, b: &str) -> bool {
    overlaps(a, b) || overlaps(b, a)
}
