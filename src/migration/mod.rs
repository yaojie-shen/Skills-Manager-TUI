//! Ordered, versioned migrations for the filesystem-backed Skill Home.
//!
//! Two version layers: `format.toml` declares the Home `layout`, and each
//! document carries its own `schema` (`crate::schema`). A layout fixes one
//! schema per document kind plus the filename rules.
//!
//! Pipeline: read the declared layout ([`version`]), capture a bounded
//! [`snapshot`], run the frozen `steps` in memory, validate the result with
//! the current program codecs, diff it into a [`plan`], then the runner locks,
//! revalidates, backs up, and publishes by phase, writing `format.toml` last.
//!
//! Adding layout N:
//! 1. add frozen definitions in `layouts/vN.rs` and register them in `layouts::LAYOUTS`;
//! 2. add `steps/v{N-1}_to_vN.rs` and register it in `steps::STEPS`;
//! 3. bump `CURRENT_LAYOUT` and the `crate::schema` constants, then update the live stores;
//! 4. add fixtures under `tests/fixtures/migrations/`;
//! 5. for a new metadata directory, extend `snapshot.rs`, the `plan.rs` phase
//!    mapping, and `validate_current`.
mod backup;
mod layouts;
pub mod plan;
mod runner;
pub mod snapshot;
mod steps;
pub mod version;
use serde::Serialize;
use std::{
    fmt,
    path::{Path, PathBuf},
};
pub const CURRENT_LAYOUT: u32 = 1;
#[derive(Debug, Clone, Serialize)]
pub struct MigrationReport {
    pub backup_dir: PathBuf,
    pub migrated_names: Vec<String>,
    pub migrated_tags: Vec<String>,
    pub config_migrated: bool,
    pub migrated_repositories: Vec<String>,
    pub from_layout: u32,
    pub to_layout: u32,
}
impl MigrationReport {
    /// Human-readable recovery instructions followed by a migration summary.
    pub fn recovery_lines(&self) -> Vec<String> {
        fn changed(label: &str, names: &[String]) -> String {
            if names.is_empty() {
                format!("{label}: 0")
            } else {
                format!("{label}: {} ({})", names.len(), names.join(", "))
            }
        }

        vec![
            "Original files were backed up to:".into(),
            self.backup_dir.display().to_string(),
            "This backup holds only the original files this migration changed; it is not a full snapshot.".into(),
            "To recover, copy the original files you need back into .skills-meta.".into(),
            "Copy format.toml too if this backup has one; otherwise delete .skills-meta/format.toml. Then reopen to re-run migration.".into(),
            format!(
                "Configuration: {}",
                if self.config_migrated { "migrated" } else { "unchanged" }
            ),
            changed("Tags", &self.migrated_tags),
            changed("Presets", &self.migrated_names),
            changed("Repository documents", &self.migrated_repositories),
        ]
    }
}
pub use plan::Phase;
#[derive(Debug)]
pub enum Recovery {
    NothingWritten,
    RetryOnReopen,
    Manual(Vec<PathBuf>),
}
#[derive(Debug)]
pub struct MigrationError {
    pub backup_dir: Option<PathBuf>,
    pub committed: Vec<Phase>,
    pub recovery: Recovery,
    pub source: anyhow::Error,
}
impl fmt::Display for MigrationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.recovery {
            Recovery::NothingWritten => write!(f, "migration wrote nothing; ")?,
            Recovery::RetryOnReopen => write!(
                f,
                "migration can be retried by reopening; backup: {}; ",
                self.backup_dir
                    .as_ref()
                    .map_or_else(|| "unavailable".into(), |path| path.display().to_string())
            )?,
            Recovery::Manual(paths) => write!(
                f,
                "migration needs manual recovery at {}; backup: {}; ",
                paths
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
                self.backup_dir
                    .as_ref()
                    .map_or_else(|| "unavailable".into(), |path| path.display().to_string())
            )?,
        }
        write!(f, "{}", self.source)
    }
}
impl std::error::Error for MigrationError {
    // Display already prints the outermost source message.
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.source()
    }
}
pub fn ensure_current(root: &Path) -> Result<Option<MigrationReport>, MigrationError> {
    // `version::read` already rejects layouts newer than this build.
    let declared = version::read(root).map_err(preflight)?;
    if declared == Some(CURRENT_LAYOUT) {
        return Ok(None);
    }
    let from_layout = declared.unwrap_or(0);
    let snapshot = snapshot::HomeSnapshot::read(root).map_err(preflight)?;
    if snapshot
        .files()
        .keys()
        .all(|path| path.as_path() == Path::new("format.toml"))
    {
        return Ok(None);
    }
    let mut view = snapshot.view();
    let mut notes = steps::StepNotes::default();
    for (_, upgrade) in steps::STEPS
        .iter()
        .filter(|(layout, _)| *layout >= from_layout)
    {
        notes.merge(upgrade(&mut view).map_err(preflight)?);
    }
    view.insert("format.toml", version::content())
        .map_err(preflight)?;
    validate_current(root, &view).map_err(preflight)?;
    let plan = plan::diff(&snapshot, &view);
    let declaration_only = plan.ops.len() == 1 && plan.ops[0].phase == Phase::Declaration;
    if declaration_only {
        // Declaring is best effort, but a declaration published concurrently
        // must still be one this build understands.
        if runner::declare_current(root).is_err() {
            version::read(root).map_err(preflight)?;
        }
        return Ok(None);
    }
    if plan.is_empty() {
        return Ok(None);
    }
    let backup_dir = runner::execute(root, &snapshot, &plan, from_layout, CURRENT_LAYOUT)?;
    Ok(Some(MigrationReport {
        backup_dir,
        migrated_names: notes.migrated_names,
        migrated_tags: notes.migrated_tags,
        config_migrated: notes.config_migrated,
        migrated_repositories: notes.migrated_repositories,
        from_layout,
        to_layout: CURRENT_LAYOUT,
    }))
}
/// Check the migrated tree with the current program codecs before any write.
/// Steps only know frozen layouts; producing a tree this build can load is the
/// engine's job, so the engine alone consults live definitions.
fn validate_current(root: &Path, view: &snapshot::HomeView) -> anyhow::Result<()> {
    use anyhow::Context;
    let meta = crate::paths::meta_dir(root);
    let mut tags = Vec::new();
    let mut presets = Vec::new();
    for (rel, bytes) in view.files() {
        let rel = rel.as_path();
        let path = meta.join(rel);
        let text = || {
            std::str::from_utf8(bytes)
                .with_context(|| format!("invalid UTF-8 in {}", path.display()))
        };
        match rel.parent().and_then(Path::to_str) {
            Some("") if rel == Path::new("format.toml") => {}
            Some("") if rel == Path::new(crate::config::CONFIG_FILE) => {
                crate::config::Config::validate_text(&path, text()?)?
            }
            Some(crate::tag::TAG_DIR) => tags.push((path, bytes.clone())),
            Some(crate::preset::PRESET_DIR) => presets.push((path, bytes.clone())),
            Some("repos") => {
                let doc = crate::meta::MetaStore::parse_document(&path, text()?)?;
                crate::meta::validate_repository_document(&path, &doc)?;
            }
            _ => anyhow::bail!(
                "no current codec validates migrated metadata {}",
                path.display()
            ),
        }
    }
    crate::tag::TagStore::new(root).validate_documents(tags)?;
    crate::preset::PresetStore::new(root).validate_documents(presets)?;
    Ok(())
}
fn preflight(source: anyhow::Error) -> MigrationError {
    MigrationError {
        backup_dir: None,
        committed: Vec::new(),
        recovery: Recovery::NothingWritten,
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::DownloadDir;
    use anyhow::anyhow;
    use std::collections::BTreeMap;

    fn fixture(label: &str) -> DownloadDir {
        let temp = DownloadDir::new(label).unwrap();
        let meta = crate::paths::meta_dir(temp.path());
        for dir in ["tags", "presets", "repos"] {
            std::fs::create_dir_all(meta.join(dir)).unwrap();
        }
        std::fs::write(
            meta.join("config.toml"),
            "# config\nschema = 1\ntags = [{ name = 'work', skills = ['one'] }]\n",
        )
        .unwrap();
        std::fs::write(
            meta.join("tags/tag-0123456789abcdef01234567.toml"),
            "# tag\nname = 'other'\nskills = ['two']\n",
        )
        .unwrap();
        std::fs::write(
            meta.join("presets/daily.toml"),
            "# preset\nname = 'daily'\ntags = ['work']\n",
        )
        .unwrap();
        std::fs::write(
            meta.join("repos/demo.toml"),
            "# repo\nalias = 'demo'\nurl = 'https://example.com/demo'\n[skills.one]\nnote = 'x'\n",
        )
        .unwrap();
        temp
    }

    fn business_tree(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        snapshot::HomeSnapshot::read(root)
            .unwrap()
            .files
            .into_iter()
            .map(|(path, bytes)| (path.as_path().to_path_buf(), bytes))
            .collect()
    }

    #[test]
    fn recovery_lines_label_backup_and_describe_every_category() {
        let report = MigrationReport {
            backup_dir: PathBuf::from("/tmp/home/.skills-meta/backups/originals"),
            migrated_names: vec!["daily".into(), "focus".into()],
            migrated_tags: vec!["work".into()],
            config_migrated: true,
            migrated_repositories: vec!["demo.toml".into(), ".root.toml".into()],
            from_layout: 0,
            to_layout: 1,
        };
        let lines = report.recovery_lines();
        assert_eq!(lines[0], "Original files were backed up to:");
        assert_eq!(lines[1], report.backup_dir.display().to_string());
        let text = lines.join("\n");
        for expected in [
            "only the original files this migration changed",
            "not a full snapshot",
            "Copy format.toml too if this backup has one",
            "delete .skills-meta/format.toml",
            "Configuration: migrated",
            "Tags: 1 (work)",
            "Presets: 2 (daily, focus)",
            "Repository documents: 2 (demo.toml, .root.toml)",
        ] {
            assert!(text.contains(expected), "missing {expected:?} from {text}");
        }
        assert!(!text.contains("true"));
    }

    #[test]
    fn recovery_lines_report_unchanged_and_empty_categories_without_booleans() {
        let report = MigrationReport {
            backup_dir: PathBuf::from("backup"),
            migrated_names: vec![],
            migrated_tags: vec![],
            config_migrated: false,
            migrated_repositories: vec![],
            from_layout: 0,
            to_layout: 1,
        };
        let text = report.recovery_lines().join("\n");
        assert!(text.contains("Configuration: unchanged"));
        assert!(text.contains("Tags: 0"));
        assert!(text.contains("Presets: 0"));
        assert!(text.contains("Repository documents: 0"));
        assert!(!text.contains("false"));
    }

    #[test]
    fn steps_cover_every_layout_contiguously() {
        assert_eq!(steps::STEPS.len(), CURRENT_LAYOUT as usize);
        assert!(
            steps::STEPS
                .iter()
                .enumerate()
                .all(|(index, (layout, _))| *layout == index as u32)
        );
    }

    #[test]
    fn planning_is_read_only_and_creates_no_lock() {
        let temp = fixture("migration-plan-read-only");
        let before = business_tree(temp.path());
        let snapshot = snapshot::HomeSnapshot::read(temp.path()).unwrap();
        let mut view = snapshot.view();
        steps::v0_to_v1::upgrade(&mut view).unwrap();
        assert!(!plan::diff(&snapshot, &view).is_empty());
        assert_eq!(business_tree(temp.path()), before);
        assert!(
            !crate::paths::meta_dir(temp.path())
                .join(".metadata.lock")
                .exists()
        );
    }

    #[test]
    fn empty_plan_returns_none_without_creating_lock() {
        let temp = DownloadDir::new("migration-empty").unwrap();
        assert!(ensure_current(temp.path()).unwrap().is_none());
        assert!(
            !crate::paths::meta_dir(temp.path())
                .join(".metadata.lock")
                .exists()
        );
    }

    #[test]
    fn declaration_publication_ignores_foreign_temp_files() {
        let temp = DownloadDir::new("migration-declaration-temp").unwrap();
        let meta = crate::paths::meta_dir(temp.path());
        std::fs::create_dir_all(&meta).unwrap();
        std::fs::write(meta.join("config.toml"), "schema = 2\n").unwrap();
        std::fs::write(meta.join(".format.toml.tmp-garbage"), "partial").unwrap();

        assert!(ensure_current(temp.path()).unwrap().is_none());
        assert_eq!(
            std::fs::read(version::path(temp.path())).unwrap(),
            version::content()
        );
        assert_eq!(
            std::fs::read(meta.join(".format.toml.tmp-garbage")).unwrap(),
            b"partial"
        );
    }

    #[test]
    fn declaration_race_accepts_current_layout() {
        let temp = DownloadDir::new("migration-declaration-race").unwrap();
        let meta = crate::paths::meta_dir(temp.path());
        std::fs::create_dir_all(&meta).unwrap();
        std::fs::write(meta.join("config.toml"), "schema = 2\n").unwrap();
        std::fs::write(version::path(temp.path()), version::content()).unwrap();

        assert!(!runner::declare_current(temp.path()).unwrap());
        assert_eq!(
            std::fs::read(version::path(temp.path())).unwrap(),
            version::content()
        );
    }

    #[test]
    fn declare_only_lock_error_does_not_fail_open() {
        let temp = DownloadDir::new("migration-declaration-lock-error").unwrap();
        let meta = crate::paths::meta_dir(temp.path());
        std::fs::create_dir_all(meta.join(".metadata.lock")).unwrap();
        std::fs::write(meta.join("config.toml"), "schema = 2\n").unwrap();

        // The metadata lock is best effort, so the declaration is still written.
        assert!(ensure_current(temp.path()).unwrap().is_none());
        assert_eq!(
            std::fs::read(version::path(temp.path())).unwrap(),
            version::content()
        );
    }

    #[test]
    fn committed_tag_cleanup_failure_requires_manual_recovery() {
        let temp = fixture("migration-tag-cleanup");
        let error = crate::file_set::with_hook(
            Box::new(|step, _, _| {
                if step == crate::file_set::Step::Cleanup {
                    Err(std::io::Error::other("injected cleanup failure"))
                } else {
                    Ok(())
                }
            }),
            || ensure_current(temp.path()),
        )
        .unwrap_err();
        assert!(error.committed.contains(&Phase::Tags));
        assert!(matches!(error.recovery, Recovery::Manual(ref paths) if !paths.is_empty()));
    }

    #[test]
    fn every_phase_failure_converges_on_reopen() {
        let expected = fixture("migration-uninterrupted");
        ensure_current(expected.path()).unwrap().unwrap();
        let expected_tree = business_tree(expected.path());
        for failed in Phase::ORDERED {
            let temp = fixture(&format!("migration-fail-{failed:?}"));
            let error = runner::with_hook(
                Box::new(move |phase| {
                    if phase == failed {
                        Err(anyhow!("injected after {phase:?}"))
                    } else {
                        Ok(())
                    }
                }),
                || ensure_current(temp.path()),
            )
            .unwrap_err();
            assert!(format!("{error:#}").contains("injected after"));
            ensure_current(temp.path()).unwrap();
            assert_eq!(business_tree(temp.path()), expected_tree, "{failed:?}");
        }
    }

    #[test]
    fn partial_repo_phase_is_reported_and_reopen_converges() {
        let expected = DownloadDir::new("migration-repo-partial-expected").unwrap();
        let expected_repos = crate::paths::meta_dir(expected.path()).join("repos");
        std::fs::create_dir_all(&expected_repos).unwrap();
        for alias in ["a", "b"] {
            std::fs::write(
                expected_repos.join(format!("{alias}.toml")),
                format!("alias = '{alias}'\nurl = 'https://example.com/{alias}'\n"),
            )
            .unwrap();
        }
        ensure_current(expected.path()).unwrap();
        let expected_tree = business_tree(expected.path());

        let temp = DownloadDir::new("migration-repo-partial").unwrap();
        let repos = crate::paths::meta_dir(temp.path()).join("repos");
        std::fs::create_dir_all(&repos).unwrap();
        for alias in ["a", "b"] {
            std::fs::write(
                repos.join(format!("{alias}.toml")),
                format!("alias = '{alias}'\nurl = 'https://example.com/{alias}'\n"),
            )
            .unwrap();
        }
        let error = runner::with_operation_hook(
            Box::new(|phase, index, _| {
                if (phase, index) == (Phase::Repos, 1) {
                    Err(anyhow!("injected after first repository write"))
                } else {
                    Ok(())
                }
            }),
            || ensure_current(temp.path()),
        )
        .unwrap_err();
        assert!(error.committed.contains(&Phase::Repos));
        assert!(matches!(error.recovery, Recovery::RetryOnReopen));
        ensure_current(temp.path()).unwrap();
        assert_eq!(business_tree(temp.path()), expected_tree);
    }

    #[test]
    fn revalidation_failure_writes_no_business_files() {
        let temp = fixture("migration-revalidation");
        let snapshot = snapshot::HomeSnapshot::read(temp.path()).unwrap();
        let mut view = snapshot.view();
        steps::v0_to_v1::upgrade(&mut view).unwrap();
        let plan = plan::diff(&snapshot, &view);
        let config = crate::config::Config::path(temp.path());
        std::fs::write(&config, b"external change").unwrap();
        let before = business_tree(temp.path());
        let error = runner::execute(temp.path(), &snapshot, &plan, 0, CURRENT_LAYOUT).unwrap_err();
        assert!(format!("{error:#}").contains("changed during migration"));
        assert_eq!(business_tree(temp.path()), before);
        assert!(!crate::paths::meta_dir(temp.path()).join("backups").exists());
    }

    #[test]
    fn full_snapshot_revalidation_rejects_unchanged_input_mutation() {
        let temp = fixture("migration-full-revalidation");
        let tag = crate::paths::meta_dir(temp.path()).join("tags/canonical.toml");
        std::fs::write(&tag, "schema = 1\nname = 'canonical'\nskills = ['one']\n").unwrap();
        let snapshot = snapshot::HomeSnapshot::read(temp.path()).unwrap();
        let mut view = snapshot.view();
        steps::v0_to_v1::upgrade(&mut view).unwrap();
        view.insert("format.toml", version::content()).unwrap();
        let plan = plan::diff(&snapshot, &view);
        std::fs::write(
            &tag,
            "schema = 1\nname = 'canonical'\nskills = ['changed']\n",
        )
        .unwrap();
        let error = runner::execute(temp.path(), &snapshot, &plan, 0, CURRENT_LAYOUT).unwrap_err();
        assert!(format!("{error:#}").contains("metadata changed during migration"));
        assert!(!version::path(temp.path()).exists());
        assert!(!crate::paths::meta_dir(temp.path()).join("backups").exists());
    }

    #[test]
    fn final_validation_rejects_invalid_current_documents_before_any_write() {
        for (config, expected) in [
            (
                "schema = 2\n[[tags]]\nname = 'work'\n[deploy]\nlegacy = true\n",
                "no longer supports top-level tags or deploy",
            ),
            ("schema = 1\nunknown_setting = true\n", "unknown field"),
        ] {
            let temp = DownloadDir::new("migration-final-validation").unwrap();
            let meta = crate::paths::meta_dir(temp.path());
            std::fs::create_dir_all(&meta).unwrap();
            std::fs::write(meta.join("config.toml"), config).unwrap();
            let before = business_tree(temp.path());
            let error = ensure_current(temp.path()).unwrap_err();
            assert!(matches!(error.recovery, Recovery::NothingWritten));
            let message = format!("{:#}", error.source);
            assert!(message.contains(expected), "{message}");
            assert_eq!(business_tree(temp.path()), before);
            assert!(!meta.join(".metadata.lock").exists());
            assert!(!version::path(temp.path()).exists());
        }
    }

    #[test]
    fn final_validation_still_requires_canonical_filenames() {
        let temp = DownloadDir::new("migration-canonical-validation").unwrap();
        for (dir, kind) in [("tags", "Tag"), ("presets", "preset")] {
            let mut view = snapshot::HomeView::default();
            view.insert(
                format!("{dir}/odd.toml"),
                b"schema = 1\nname = 'work'\n".to_vec(),
            )
            .unwrap();
            let error = format!("{:#}", validate_current(temp.path(), &view).unwrap_err());
            assert!(
                error.contains(&format!("noncanonical {kind} filename")),
                "{error}"
            );
        }
    }

    #[test]
    fn interrupted_transaction_has_actionable_errors() {
        let temp = DownloadDir::new("migration-transaction-debris").unwrap();
        let debris = crate::paths::meta_dir(temp.path()).join("tags/.file-set-leftover");
        std::fs::create_dir_all(&debris).unwrap();
        let error = ensure_current(temp.path()).unwrap_err();
        assert!(format!("{error:#}").contains("an interrupted metadata transaction left"));
        let error = crate::tag::TagStore::new(temp.path()).list().unwrap_err();
        assert!(format!("{error:#}").contains("original-N (pre-change files)"));
    }

    #[test]
    fn migration_ignores_and_preserves_stray_store_files() {
        let temp = fixture("migration-strays");
        let meta = crate::paths::meta_dir(temp.path());
        let strays = [
            "tags/.DS_Store",
            "tags/work.toml~",
            "tags/.other.toml.tmp-123",
            "presets/README",
            "presets/.DS_Store",
            "repos/demo.toml~",
            "repos/.DS_Store",
        ];
        for stray in strays {
            std::fs::write(meta.join(stray), b"stray").unwrap();
        }
        std::fs::write(
            meta.join("repos/.root.toml"),
            "[skills.one]\nnote = 'root'\n",
        )
        .unwrap();
        let report = ensure_current(temp.path()).unwrap().unwrap();
        assert!(
            report
                .migrated_repositories
                .contains(&".root.toml".to_owned()),
            "{report:?}"
        );
        for stray in strays {
            assert_eq!(
                std::fs::read(meta.join(stray)).unwrap(),
                b"stray",
                "{stray}"
            );
        }
        assert!(crate::tag::TagStore::new(temp.path()).list().unwrap().len() == 2);
        assert!(ensure_current(temp.path()).unwrap().is_none());
    }

    #[test]
    fn hidden_repository_documents_are_ignored_by_every_reader() {
        let temp = fixture("migration-hidden-repository");
        ensure_current(temp.path()).unwrap().unwrap();
        let meta = crate::paths::meta_dir(temp.path());
        // A hidden legacy document must not trap the Library in a loop of
        // "legacy document" errors that re-declaring cannot clear.
        let hidden = meta.join("repos/.foo.toml");
        std::fs::write(&hidden, "[skills.one]\nnote = 'hidden'\n").unwrap();
        let ws = crate::Workspace::open(temp.path()).unwrap();
        ws.meta.list_keys().unwrap();
        crate::repository::Repository::list(temp.path()).unwrap();
        // Re-declaring (deleting format.toml and reopening) must not loop either.
        let _ = std::fs::remove_file(meta.join("format.toml"));
        assert!(ensure_current(temp.path()).unwrap().is_none());
        crate::Workspace::open(temp.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(&hidden).unwrap(),
            "[skills.one]\nnote = 'hidden'\n"
        );
    }

    #[test]
    fn tag_defined_differently_inline_and_in_the_store_says_what_to_do() {
        let temp = DownloadDir::new("migration-tag-differs").unwrap();
        let meta = crate::paths::meta_dir(temp.path());
        std::fs::create_dir_all(meta.join("tags")).unwrap();
        std::fs::write(
            meta.join("config.toml"),
            "schema = 2\ntags = [{ name = 'work', skills = ['one'] }]\n",
        )
        .unwrap();
        std::fs::write(
            meta.join("tags/work.toml"),
            "schema = 1\nname = 'work'\nskills = ['two']\n",
        )
        .unwrap();
        let error = format!("{:#}", ensure_current(temp.path()).unwrap_err());
        for expected in [
            "Tag work is defined differently in .skills-meta/config.toml",
            "in .skills-meta/tags/work.toml",
            "make the two definitions match, or remove the config.toml tags entry you do not want, then reopen",
        ] {
            assert!(error.contains(expected), "{error}");
        }
    }

    #[test]
    fn migration_still_rejects_a_toml_symlink() {
        let temp = fixture("migration-symlink");
        let meta = crate::paths::meta_dir(temp.path());
        std::os::unix::fs::symlink(meta.join("config.toml"), meta.join("tags/linked.toml"))
            .unwrap();
        let error = ensure_current(temp.path()).unwrap_err();
        assert!(format!("{error:#}").contains("invalid Tag store entry"));
    }

    #[test]
    fn error_chain_prints_each_message_once() {
        let error = anyhow::Error::new(preflight(anyhow!("inner").context("outer")));
        assert_eq!(
            format!("{error:#}"),
            "migration wrote nothing; outer: inner"
        );
    }

    #[test]
    fn snapshot_enforces_cumulative_byte_limit() {
        let temp = DownloadDir::new("migration-total-limit").unwrap();
        let tags = crate::paths::meta_dir(temp.path()).join("tags");
        std::fs::create_dir_all(&tags).unwrap();
        let bytes = vec![b'x'; 9 * 1024 * 1024];
        for index in 0..8 {
            std::fs::write(tags.join(format!("{index}.toml")), &bytes).unwrap();
        }
        let error = snapshot::HomeSnapshot::read(temp.path()).unwrap_err();
        assert!(format!("{error:#}").contains("64 MiB total byte limit"));
    }
}
