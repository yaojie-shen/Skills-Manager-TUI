//! Named skill groups stored independently under `<root>/.skills-meta/tags`.
use crate::group_store::{Group, GroupStore};
use anyhow::Result;
use serde::{Deserialize, Serialize};

pub const TAG_DIR: &str = "tags";
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tag {
    #[serde(default)]
    pub skills: Vec<String>,
    pub name: String,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}
impl Group for Tag {
    const DIR: &'static str = TAG_DIR;
    const KIND: &'static str = "Tag";
    const SCHEMA: u32 = crate::schema::TAG;
    const FIELDS: &'static [&'static str] = &["skills", "name", "color", "description"];
    fn name(&self) -> &str {
        &self.name
    }
    fn name_mut(&mut self) -> &mut String {
        &mut self.name
    }

    fn skills_mut(&mut self) -> &mut Vec<String> {
        &mut self.skills
    }
}
pub type TagStore = GroupStore<Tag>;
impl GroupStore<Tag> {
    pub fn remove_skill(&self, old: &str, new: Option<&str>) -> Result<()> {
        self.edit(|tags| {
            for tag in tags {
                for skill in &mut tag.skills {
                    if skill == old
                        && let Some(new) = new
                    {
                        *skill = new.to_owned();
                    }
                }
                if new.is_none() {
                    tag.skills.retain(|skill| skill != old);
                }
            }
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::DownloadDir;

    #[test]
    fn stores_readable_names_and_renames_files_preserving_comments() {
        let temp = DownloadDir::new("tag-store").unwrap();
        let store = TagStore::new(temp.path());
        store
            .save(&Tag {
                name: "工作 / Rust".into(),
                skills: vec!["b".into(), "a".into()],
                color: None,
                description: None,
            })
            .unwrap();
        let old = store.dir.join("工作 - Rust.toml");
        assert!(old.exists());
        let mut text = std::fs::read_to_string(&old).unwrap();
        text.insert_str(0, "# keep me\n");
        std::fs::write(&old, text).unwrap();
        store.rename("工作 / Rust", "工作 / 系统").unwrap();
        let new = store.dir.join("工作 - 系统.toml");
        assert!(new.exists());
        assert!(
            std::fs::read_to_string(new)
                .unwrap()
                .starts_with("# keep me")
        );
    }

    #[test]
    fn same_name_rename_preserves_definition_bytes() {
        let temp = DownloadDir::new("tag-rename-same").unwrap();
        let store = TagStore::new(temp.path());
        store
            .save(&Tag {
                name: "work".into(),
                skills: vec!["demo".into()],
                color: None,
                description: None,
            })
            .unwrap();
        let path = std::fs::read_dir(&store.dir)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let bytes = std::fs::read(&path).unwrap();
        store.rename("work", " work ").unwrap();
        assert_eq!(std::fs::read(path).unwrap(), bytes);
        assert_eq!(store.list().unwrap()[0].name, "work");
    }

    #[test]
    fn collision_group_all_get_digests() {
        let temp = DownloadDir::new("tag-collision").unwrap();
        let store = TagStore::new(temp.path());
        for name in ["A/B", "A-B"] {
            store
                .save(&Tag {
                    name: name.into(),
                    skills: vec![],
                    color: None,
                    description: None,
                })
                .unwrap();
        }
        let names: Vec<_> = std::fs::read_dir(&store.dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 2);
        assert!(names.iter().all(|name| name.starts_with("A-B--")));
    }
}
