//! Remote source metadata and shared skill discovery.
use crate::{
    Workspace,
    meta::{Source, SourceKind},
    ops::{DownloadDir, fresh_staging},
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
    /// Stable storage key used in skill identities and paths.
    pub alias: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub url: String,
    #[serde(default)]
    pub kind: SourceKind,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub branch: String,
}

impl Repository {
    pub fn display_name(&self) -> String {
        self.name
            .as_deref()
            .map(str::trim)
            .map(str::to_string)
            .unwrap_or_else(|| default_display_name(self.kind, &self.url))
    }

    pub fn set_name(&mut self, name: &str) -> Result<()> {
        validate_name(name)?;
        self.name = Some(name.trim().to_string());
        Ok(())
    }

    pub fn same_identity(&self, other: &Self) -> bool {
        self.alias == other.alias
            && self.kind == other.kind
            && self.url == other.url
            && self.branch == other.branch
    }

    /// Change only the display label; skill keys and deployed links retain their identities.
    pub fn rename(ws: &Workspace, alias: &str, name: &str) -> Result<Self> {
        validate_name(name)?;
        anyhow::ensure!(
            valid_skill_key(alias),
            "invalid repository alias: {alias:?}"
        );
        let _lock = ws.meta.lock()?;
        let mut repository = Self::list(&ws.root)?
            .into_iter()
            .find(|repository| repository.alias == alias)
            .with_context(|| format!("source not found: {alias}"))?;
        repository.set_name(name)?;
        let path = Self::path(&ws.root, alias);
        let mut doc = crate::meta::MetaStore::read(&path)?;
        doc["name"] = toml_edit::value(repository.name.as_deref().unwrap());
        write_atomic(&path, doc.to_string().as_bytes())?;
        Ok(repository)
    }

    pub fn source(&self, path: &str, revision: Option<&str>) -> Source {
        let subpath = Some(path.to_string());
        let revision = revision.map(str::to_string);
        match self.kind {
            SourceKind::Git => Source::Git {
                url: self.url.clone(),
                branch: (!self.branch.is_empty()).then(|| self.branch.clone()),
                subpath,
                revision,
            },
            SourceKind::Archive => Source::Archive {
                url: self.url.clone(),
                subpath,
                revision,
            },
        }
    }
    pub fn matches_source(&self, source: &Source, path: &str) -> bool {
        self.source(path, None).same_location(source)
    }
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
                if let Some(name) = &repo.name {
                    validate_name(name)?;
                }
                out.push(repo);
            }
        }
        out.sort_by(|a, b| a.alias.cmp(&b.alias));
        Ok(out)
    }
    pub fn validate(&self, ws: &Workspace) -> Result<()> {
        if let Some(name) = &self.name {
            validate_name(name)?;
        }
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
            if !existing.same_identity(self) {
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
        doc["kind"] = toml_edit::value(self.kind.as_str());
        if let Some(name) = &self.name {
            doc["name"] = toml_edit::value(name.trim());
        } else if doc.get("name").is_none() && self.kind == SourceKind::Git {
            doc["name"] = toml_edit::value(self.display_name());
        }
        if self.kind == SourceKind::Git && !self.branch.is_empty() {
            doc["branch"] = toml_edit::value(self.branch.as_str());
        } else {
            doc.remove("branch");
        }
        write_atomic(&path, doc.to_string().as_bytes())
    }
}

pub fn validate_name(name: &str) -> Result<()> {
    anyhow::ensure!(!name.trim().is_empty(), "source name must not be empty");
    anyhow::ensure!(
        !name.chars().any(char::is_control),
        "source name must not contain control characters"
    );
    Ok(())
}

pub fn default_display_name(kind: SourceKind, url: &str) -> String {
    match kind {
        SourceKind::Git => source_name(url).unwrap_or_else(|| "Repository".into()),
        SourceKind::Archive => "Unnamed package".into(),
    }
}

/// User-facing repository identity, independent of the chosen storage alias.
pub fn source_name(url: &str) -> Option<String> {
    if url.chars().any(char::is_whitespace) {
        return None;
    }
    let url = url
        .split(['?', '#'])
        .next()?
        .trim_end_matches('/')
        .trim_end_matches(".git");
    if let Some((_, rest)) = url.split_once("://") {
        let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
        let host = authority.rsplit('@').next().unwrap_or(authority);
        let mut parts = path.rsplit('/').filter(|s| !s.is_empty());
        return match (parts.next(), parts.next()) {
            (Some(name), Some(parent)) => Some(format!("{parent}/{name}")),
            (Some(name), None) => Some(format!("{host}/{name}")),
            _ => (!host.is_empty()).then(|| host.to_string()),
        };
    }
    let parts: Vec<_> = url
        .rsplit(['/', ':'])
        .filter(|s| !s.is_empty())
        .take(2)
        .collect();
    (parts.len() == 2).then(|| format!("{}/{}", parts[1], parts[0]))
}

pub fn default_alias(url: &str) -> String {
    let name = source_name(url).unwrap_or_else(|| "repository".into());
    let name = name.replace('/', "--");
    let alias: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let alias = alias.trim_matches(['.', '-']);
    if alias.is_empty() {
        "repository".into()
    } else {
        alias.into()
    }
}

/// Relative identities preserve the upstream path; old flat keys remain valid.
pub fn valid_id(key: &str) -> bool {
    if !key.contains('/') {
        return valid_skill_key(key);
    }
    key.split('/').all(valid_skill_key)
        && ((key.starts_with("repos/") && key.split('/').count() == 3)
            || (key.starts_with("local/") && key.split('/').count() >= 2))
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
        anyhow::ensure!(reference.is_remote(), "expected a remote source");
        let subpath = reference.subpath();
        let download = DownloadDir::new("repository")?;
        let workdir = download.path().to_path_buf();
        let result = (|| {
            let acquired = crate::ops::source::acquire(reference, &workdir, progress)?;
            let url = &acquired.url;
            let revision = acquired.revision;
            let branch = acquired.branch.unwrap_or_default();
            progress("Scan: looking for SKILL.md files…");
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
            let repositories = Repository::list(&ws.root)?;
            let existing = repositories
                .iter()
                .find(|r| {
                    r.url == *url
                        && r.branch == branch
                        && r.kind == acquired.kind
                        && alias.is_none_or(|alias| r.alias == alias)
                })
                .or_else(|| {
                    repositories
                        .iter()
                        .find(|r| r.url == *url && r.branch == branch && r.kind == acquired.kind)
                });
            let alias = match alias {
                Some(a) => a.to_string(),
                None => existing
                    .map(|r| r.alias.clone())
                    .unwrap_or_else(|| default_alias(url)),
            };
            Ok(Self {
                repository: Repository {
                    alias,
                    name: existing.and_then(|r| r.name.clone()),
                    url: url.to_string(),
                    kind: acquired.kind,
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

        if self.repository.kind == SourceKind::Archive {
            let name = self.repository.name.as_deref().context(
                "URL packages require a name before installation; enter a package name or use --source-name",
            )?;
            validate_name(name)?;
        }

        if paths.is_empty() {
            bail!("select at least one skill")
        }
        let existing = ws.scan()?;
        let paths: Vec<String> = paths
            .iter()
            .filter(|path| {
                let installed = existing.skills.iter().any(|r| {
                    r.source
                        .as_ref()
                        .is_some_and(|source| self.repository.matches_source(source, path))
                });
                if installed {
                    progress(&format!("Already installed: {path}; skipped"));
                }
                !installed
            })
            .cloned()
            .collect();
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
                    source: Some(self.repository.source(path, Some(&self.revision))),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_names_are_independent_of_storage_and_archive_urls() {
        let git: Repository = toml::from_str(
            "alias = 'larksuite--cli'\nurl = 'https://github.com/larksuite/cli.git'\nbranch = 'main'",
        ).unwrap();
        assert_eq!(git.display_name(), "larksuite/cli");
        let mut archive: Repository = toml::from_str(
            "alias = 'latest--skills.tar'\nurl = 'https://example.com/latest/skills.tar'\nkind = 'archive'",
        ).unwrap();
        assert_eq!(archive.display_name(), "Unnamed package");
        let unnamed = archive.clone();
        archive.set_name("  Merlin Skills / 开发  ").unwrap();
        assert_eq!(archive.display_name(), "Merlin Skills / 开发");
        assert!(archive.same_identity(&unnamed));
        let mut other = archive.clone();
        other.url.push_str("?other");
        assert!(!archive.same_identity(&other));
        for name in [
            "",
            " \t",
            "line\nbreak",
            "tab\there",
            "\u{1b}[31mred",
            "null\0",
        ] {
            assert!(archive.set_name(name).is_err(), "accepted {name:?}");
            assert_eq!(archive.display_name(), "Merlin Skills / 开发");
        }
    }

    #[test]
    fn renaming_a_source_preserves_skill_metadata_and_storage() {
        let temp = DownloadDir::new("repository-display-name").unwrap();
        let ws = Workspace::open(temp.path()).unwrap();
        let repo = Repository {
            alias: "unchanged-alias".into(),
            name: None,
            url: "https://example.com/latest/skills.tar".into(),
            kind: SourceKind::Archive,
            branch: String::new(),
        };
        repo.save(&ws).unwrap();
        let key = "repos/unchanged-alias/review";
        let meta = crate::meta::SkillMeta {
            note: Some("keep this note".into()),
            source: Some(repo.source("skills/review", Some("sha256:original"))),
            baseline: Some(crate::meta::Baseline {
                hash: "content hash".into(),
                hash_algo: crate::hash::HASH_ALGO,
            }),
            ..Default::default()
        };
        ws.meta.save(key, &meta).unwrap();
        std::fs::create_dir_all(ws.skill_path(key)).unwrap();
        std::fs::write(ws.skill_path(key).join("file.txt"), "unchanged content").unwrap();
        let path = Repository::path(&ws.root, &repo.alias);
        let mut original: toml::Value =
            toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();

        let renamed = Repository::rename(&ws, &repo.alias, "Merlin skills").unwrap();
        assert_eq!(renamed.display_name(), "Merlin skills");
        assert!(renamed.same_identity(&repo));
        original
            .as_table_mut()
            .unwrap()
            .insert("name".into(), "Merlin skills".into());
        let actual: toml::Value = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(actual, original);
        assert_eq!(ws.meta.load(key).unwrap().unwrap(), meta);
        assert_eq!(
            std::fs::read_to_string(ws.skill_path(key).join("file.txt")).unwrap(),
            "unchanged content"
        );
        assert_eq!(ws.meta.list_keys().unwrap(), [key]);

        repo.save(&ws).unwrap();
        let mut edited = meta;
        edited.note = Some("updated note".into());
        ws.meta.save(key, &edited).unwrap();
        assert_eq!(
            Repository::list(&ws.root).unwrap()[0].display_name(),
            "Merlin skills"
        );
        let before = std::fs::read(&path).unwrap();
        assert!(Repository::rename(&ws, &repo.alias, "bad\nname").is_err());
        assert!(Repository::rename(&ws, "not-found", "valid").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn archive_install_requires_a_name_but_discovery_can_remain_unnamed() {
        let root = DownloadDir::new("unnamed-archive-library").unwrap();
        let source = DownloadDir::new("unnamed-archive-content").unwrap();
        let ws = Workspace::open(root.path()).unwrap();
        std::fs::write(
            source.path().join("SKILL.md"),
            "---\nname: review\ndescription: Review code\n---\n# Review",
        )
        .unwrap();
        let mut fetched = FetchedRepository {
            repository: Repository {
                alias: "package".into(),
                name: None,
                url: "https://example.com/skills.tar".into(),
                kind: SourceKind::Archive,
                branch: String::new(),
            },
            revision: "sha256:fixture".into(),
            workdir: source.path().to_path_buf(),
            choices: vec![String::new()],
            invalid: BTreeMap::new(),
        };
        assert_eq!(fetched.local_name(""), "review");
        assert!(
            fetched
                .resolved_names(&ws, &[String::new()], &BTreeMap::new())
                .is_ok()
        );
        let error = fetched
            .install(&ws, &[String::new()], &BTreeMap::new())
            .unwrap_err();
        assert!(error.to_string().contains("--source-name"));
        assert!(Repository::list(&ws.root).unwrap().is_empty());
        assert!(ws.meta.list_keys().unwrap().is_empty());
        assert!(!ws.root.join("repos").exists());
        fetched.repository.set_name("Review tools").unwrap();
        assert_eq!(
            fetched
                .install(&ws, &[String::new()], &BTreeMap::new())
                .unwrap(),
            ["repos/package/review"]
        );
        assert_eq!(
            Repository::list(&ws.root).unwrap()[0].display_name(),
            "Review tools"
        );
        fetched.repository.set_name("Another name").unwrap();
        assert!(
            fetched
                .install(&ws, &[String::new()], &BTreeMap::new())
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            Repository::list(&ws.root).unwrap()[0].display_name(),
            "Review tools",
            "an already-installed selection remains a no-op; renaming has its own operation"
        );
    }

    #[test]
    fn git_save_records_the_default_display_name() {
        let temp = DownloadDir::new("repository-default-name").unwrap();
        let ws = Workspace::open(temp.path()).unwrap();
        let repo = Repository {
            alias: "larksuite--cli".into(),
            name: None,
            url: "https://github.com/larksuite/cli.git".into(),
            kind: SourceKind::Git,
            branch: "main".into(),
        };
        repo.save(&ws).unwrap();
        let stored = Repository::list(&ws.root).unwrap().remove(0);
        assert_eq!(stored.name.as_deref(), Some("larksuite/cli"));
        assert!(stored.same_identity(&repo));
        repo.validate(&ws).unwrap();
    }

    #[test]
    fn download_labels_omit_query_credentials_and_keep_host_ports() {
        let url = "https://user:password@files.example:8443/bundle.zip?token=a@b";
        assert_eq!(
            source_name(url).as_deref(),
            Some("files.example:8443/bundle.zip")
        );
        assert_eq!(default_alias(url), "files.example-8443--bundle.zip");
        assert_eq!(
            source_name("https://files.example/?token=x").as_deref(),
            Some("files.example")
        );
        assert_eq!(
            default_alias("https://github.com/owner/repo.git"),
            "owner--repo"
        );
    }
}
