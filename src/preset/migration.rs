use super::{Preset, PresetStore, tag_members};
use crate::{config::Config, util::write_atomic};
use anyhow::{Context, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize)]
pub struct MigrationReport {
    pub backup_dir: PathBuf,
    /// Presets converted from legacy Tag references.
    pub migrated_names: Vec<String>,
    /// Tag definitions moved out of config.toml.
    #[serde(default)]
    pub migrated_tags: Vec<String>,
}

struct MigrationFile {
    path: PathBuf,
    original: Vec<u8>,
    updated: Vec<u8>,
    name: String,
}

fn replace_files(
    changes: &[MigrationFile],
    backup_dir: &Path,
    mut write: impl FnMut(&Path, &[u8]) -> Result<()>,
) -> Result<()> {
    for (index, change) in changes.iter().enumerate() {
        let result = (|| {
            anyhow::ensure!(
                std::fs::read(&change.path)
                    .with_context(|| format!("reading {}", change.path.display()))?
                    == change.original,
                "{} changed during migration",
                change.path.display()
            );
            write(&change.path, &change.updated)
        })();
        if let Err(error) = result {
            let mut unrestored = Vec::new();
            for previous in changes[..index].iter().rev() {
                match std::fs::read(&previous.path) {
                    Ok(current) if current == previous.updated => {
                        if let Err(error) = write(&previous.path, &previous.original) {
                            unrestored.push(format!("{}: {error}", previous.path.display()));
                        }
                    }
                    Ok(current) if current == previous.original => {}
                    _ => unrestored.push(format!(
                        "{} changed again and was left untouched",
                        previous.path.display()
                    )),
                }
            }
            return Err(error).with_context(|| {
                let extra = if unrestored.is_empty() {
                    String::new()
                } else {
                    format!("; {}", unrestored.join("; "))
                };
                format!(
                    "preset migration failed; originals backed up in {}{extra}",
                    backup_dir.display()
                )
            });
        }
    }
    Ok(())
}

impl PresetStore {
    /// Validate the entire upgrade before backing up or replacing any definition.
    pub fn migrate_legacy_tags(&self, config: &Config) -> Result<Option<MigrationReport>> {
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let mut paths = Vec::new();
        for entry in entries {
            let path = entry?.path();
            if path
                .extension()
                .is_some_and(|extension| extension == "toml")
            {
                paths.push(path);
            }
        }
        paths.sort();
        let mut changes = Vec::new();
        for path in paths {
            let original =
                std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
            let text = std::str::from_utf8(&original)
                .with_context(|| format!("invalid UTF-8 in {}", path.display()))?;
            let mut value: toml::Value = toml::from_str(text)
                .with_context(|| format!("invalid preset: {}", path.display()))?;
            let tags = value
                .as_table_mut()
                .context("preset must be a TOML table")?
                .remove("tags");
            let mut preset: Preset = value
                .try_into()
                .with_context(|| format!("invalid preset: {}", path.display()))?;
            let Some(tags) = tags else {
                continue;
            };
            anyhow::ensure!(
                crate::util::valid_skill_key(&preset.name),
                "invalid preset name in {}",
                path.display()
            );
            let tags: Vec<String> = tags
                .try_into()
                .with_context(|| format!("invalid legacy tags in {}", path.display()))?;
            preset
                .skills
                .extend(tag_members(config, &tags).with_context(|| {
                    format!(
                        "cannot migrate {}; original presets were left unchanged",
                        path.display()
                    )
                })?);
            preset.skills = preset.members();
            let updated = toml::to_string_pretty(&preset)?.into_bytes();
            changes.push(MigrationFile {
                path,
                original,
                updated,
                name: preset.name,
            });
        }
        if changes.is_empty() {
            return Ok(None);
        }
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let backup_dir = self
            .dir
            .parent()
            .context("preset directory has no parent")?
            .join("backups")
            .join(format!("presets-before-fixed-members-{stamp}"));
        std::fs::create_dir_all(&backup_dir)?;
        for change in &changes {
            write_atomic(
                &backup_dir.join(change.path.file_name().context("preset filename missing")?),
                &change.original,
            )?;
        }
        // Another writer must not be overwritten by the configuration captured at startup.
        for change in &changes {
            anyhow::ensure!(
                std::fs::read(&change.path).with_context(|| format!(
                    "reading {}; originals backed up in {}",
                    change.path.display(),
                    backup_dir.display()
                ))? == change.original,
                "{} changed during migration; originals backed up in {}",
                change.path.display(),
                backup_dir.display()
            );
        }
        replace_files(&changes, &backup_dir, write_atomic)?;
        Ok(Some(MigrationReport {
            backup_dir,
            migrated_names: changes.into_iter().map(|change| change.name).collect(),
            migrated_tags: Vec::new(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        root: PathBuf,
        backup: PathBuf,
        files: Vec<MigrationFile>,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir()
                .join(format!("skills-migration-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            let backup = root.join("backup");
            std::fs::create_dir_all(&backup).unwrap();
            let files = ["one", "two", "three"]
                .into_iter()
                .map(|name| {
                    let file = MigrationFile {
                        path: root.join(format!("{name}.toml")),
                        original: format!("original {name}").into_bytes(),
                        updated: format!("updated {name}").into_bytes(),
                        name: name.into(),
                    };
                    std::fs::write(&file.path, &file.original).unwrap();
                    std::fs::write(backup.join(format!("{name}.toml")), &file.original).unwrap();
                    file
                })
                .collect();
            Self {
                root,
                backup,
                files,
            }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn migration_rechecks_later_files_and_preserves_external_edits() {
        let f = Fixture::new("later-edit");
        let mut writes = 0;
        let error = replace_files(&f.files, &f.backup, |path, bytes| {
            write_atomic(path, bytes)?;
            writes += 1;
            if writes == 1 {
                std::fs::write(&f.files[1].path, b"external edit")?;
            }
            Ok(())
        })
        .unwrap_err();
        assert!(format!("{error:#}").contains("changed during migration"));
        assert_eq!(
            std::fs::read(&f.files[0].path).unwrap(),
            f.files[0].original
        );
        assert_eq!(std::fs::read(&f.files[1].path).unwrap(), b"external edit");
        assert_eq!(
            std::fs::read(&f.files[2].path).unwrap(),
            f.files[2].original
        );
    }

    #[test]
    fn rollback_keeps_a_file_edited_after_migration_and_reports_its_backup() {
        let f = Fixture::new("rollback-edit");
        let mut writes = 0;
        let error = replace_files(&f.files, &f.backup, |path, bytes| {
            writes += 1;
            if writes == 2 {
                std::fs::write(&f.files[0].path, b"new user contents")?;
                anyhow::bail!("simulated write failure");
            }
            write_atomic(path, bytes)
        })
        .unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("left untouched"));
        assert!(message.contains(f.backup.to_str().unwrap()));
        assert_eq!(
            std::fs::read(&f.files[0].path).unwrap(),
            b"new user contents"
        );
        assert_eq!(
            std::fs::read(&f.files[1].path).unwrap(),
            f.files[1].original
        );
        assert_eq!(
            std::fs::read(f.backup.join("one.toml")).unwrap(),
            f.files[0].original
        );
    }

    #[test]
    fn write_failure_rolls_back_all_previously_replaced_files() {
        let f = Fixture::new("rollback");
        let mut writes = 0;
        let error = replace_files(&f.files, &f.backup, |path, bytes| {
            writes += 1;
            anyhow::ensure!(writes != 3, "simulated write failure");
            write_atomic(path, bytes)
        })
        .unwrap_err();
        assert!(format!("{error:#}").contains("simulated write failure"));
        for file in &f.files {
            assert_eq!(std::fs::read(&file.path).unwrap(), file.original);
        }
    }
}
