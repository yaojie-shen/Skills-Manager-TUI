//! Frozen Home layout definitions, one module per layout: per-document schemas,
//! document shapes, and filename rules exactly as released. A released layout is
//! never edited; a format change adds a new layout and step (see `steps`).

pub mod v1;

/// Per-document schema versions that make up one layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Schemas {
    pub config: u32,
    pub tag: u32,
    pub preset: u32,
    pub repository: u32,
}

/// Every frozen layout in order; the last entry is the current layout.
#[allow(dead_code)] // Anchors the drift tests below.
pub const LAYOUTS: &[(u32, Schemas)] = &[(v1::LAYOUT, v1::SCHEMAS)];
#[allow(dead_code)] // Anchors the drift tests below.
pub const LATEST: Schemas = v1::SCHEMAS;

/// Drift tests: the live program must still agree with the latest frozen layout.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{group_filename, migration::CURRENT_LAYOUT, preset::Preset, schema, tag::Tag};

    const DRIFT: &str = "live definition diverged from frozen layout 1: bump CURRENT_LAYOUT and add a migration step instead of editing layouts::v1";

    #[test]
    fn latest_layout_schemas_match_live_schemas() {
        assert_eq!(
            LATEST,
            Schemas {
                config: schema::CONFIG,
                tag: schema::TAG,
                preset: schema::PRESET,
                repository: schema::REPOSITORY,
            },
            "{DRIFT}"
        );
    }

    #[test]
    fn layouts_and_steps_cover_current_layout() {
        assert_eq!(LAYOUTS.len(), CURRENT_LAYOUT as usize, "{DRIFT}");
        assert!(
            LAYOUTS
                .iter()
                .enumerate()
                .all(|(index, (layout, _))| *layout == index as u32 + 1),
            "{DRIFT}"
        );
        assert_eq!(LAYOUTS.last().map(|entry| entry.1), Some(LATEST), "{DRIFT}");
        assert_eq!(
            super::super::steps::STEPS.len(),
            CURRENT_LAYOUT as usize,
            "{DRIFT}"
        );
    }

    fn sha256_hex(name: &str) -> String {
        use sha2::{Digest, Sha256};
        Sha256::digest(name.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    #[test]
    fn filename_rules_match_live_group_filename() {
        let long = "x".repeat(200);
        let long_unicode = "工作".repeat(40);
        let mut batteries: Vec<Vec<String>> = vec![
            vec!["work".into(), "Rust tools".into(), " padded ".into()],
            vec!["工作 / Rust".into(), "é".into(), "Ünïcödé".into()],
            vec!["Work".into(), "work".into(), "WORK".into()],
            vec!["A/B".into(), "A-B".into(), "A\\B".into(), "A<B>".into()],
            vec![
                "a:b".into(),
                "q\"uote".into(),
                "p|ipe".into(),
                "what?".into(),
                "star*".into(),
                "///".into(),
            ],
            vec![".hidden".into(), "..".into(), "-dash-".into()],
            vec![
                "CON".into(),
                "con.txt".into(),
                "COM1".into(),
                "LPT9".into(),
                "AUX".into(),
                "NUL".into(),
                "PRN".into(),
                "COM0".into(),
                "CONIN$".into(),
                "CONOUT$".into(),
                "COM¹".into(),
                "LPT1.log".into(),
            ],
            vec![long.clone(), format!("{long}y"), long_unicode],
            // Whitespace exactly at the truncation points for long and colliding stems.
            vec![
                format!("{} {}", "x".repeat(165), "y".repeat(40)),
                format!("{} {}", "z".repeat(169), "w".repeat(40)),
                format!("{}/{}", "z".repeat(169), "w".repeat(40)),
                format!("{}-{}", "z".repeat(169), "w".repeat(40)),
            ],
        ];
        let hash = sha256_hex("A/B");
        let mut adversarial = vec!["A-B".to_owned(), "A/B".to_owned()];
        adversarial.extend(
            (8..=64)
                .step_by(2)
                .map(|digits| format!("A-B--{}", &hash[..digits])),
        );
        batteries.push(adversarial);
        batteries.push(vec![
            "A/B".into(),
            "A-B".into(),
            format!("A-B--{}", &sha256_hex("A-B")[..8]),
        ]);
        batteries.push(vec!["é".into(), "e\u{301}".into()]);
        batteries.push(vec!["".into()]);
        batteries.push(vec!["bad\u{7}".into()]);

        for names in &batteries {
            let frozen = v1::allocate(names.iter().map(String::as_str));
            let live = group_filename::allocate(names.iter().map(String::as_str));
            match (frozen, live) {
                (Ok(frozen), Ok(live)) => assert_eq!(frozen, live, "{DRIFT}: {names:?}"),
                (Err(frozen), Err(live)) => {
                    assert_eq!(frozen.to_string(), live.to_string(), "{DRIFT}: {names:?}")
                }
                (frozen, live) => panic!("{DRIFT}: {names:?}: {frozen:?} vs {live:?}"),
            }
            for name in names {
                assert_eq!(
                    v1::normalize_name(name).map_err(|error| error.to_string()),
                    group_filename::normalize_name(name).map_err(|error| error.to_string()),
                    "{DRIFT}: {name:?}"
                );
            }
        }
    }

    fn sample_tags() -> Vec<Tag> {
        vec![
            Tag {
                skills: vec!["one".into(), "two".into()],
                name: "work".into(),
                color: Some("blue".into()),
                description: Some("Work tools".into()),
            },
            Tag {
                skills: vec![],
                name: "工作 / Rust".into(),
                color: None,
                description: None,
            },
            Tag {
                skills: vec!["x\"y".into()],
                name: "quoted 'name'".into(),
                color: Some(String::new()),
                description: None,
            },
        ]
    }

    fn frozen_tag(tag: &Tag) -> v1::TagV1 {
        v1::TagV1 {
            skills: tag.skills.clone(),
            name: tag.name.clone(),
            color: tag.color.clone(),
            description: tag.description.clone(),
        }
    }

    #[test]
    fn tag_serializer_matches_live_store() {
        for tag in sample_tags() {
            assert_eq!(
                v1::serialize_tag(&frozen_tag(&tag)).unwrap(),
                crate::tag::TagStore::serialize(&tag).unwrap(),
                "{DRIFT}: {tag:?}"
            );
        }
    }

    #[test]
    fn tag_documents_round_trip_like_live_tags() {
        for text in [
            "name = 'work'\nskills = ['b', 'a']\ncolor = 'red'\ndescription = 'd'\n",
            "name = 'bare'\n",
            "skills = ['one']\n",
            "name = 'extra'\nunknown = 1\n",
            "name = 'typed'\nskills = 'one'\n",
        ] {
            let frozen = toml::from_str::<v1::TagV1>(text);
            let live = toml::from_str::<Tag>(text);
            match (frozen, live) {
                (Ok(frozen), Ok(live)) => {
                    assert_eq!(frozen, frozen_tag(&live), "{DRIFT}: {text:?}");
                    assert_eq!(
                        toml::to_string(&frozen).unwrap(),
                        toml::to_string(&live).unwrap(),
                        "{DRIFT}: {text:?}"
                    );
                }
                (Err(frozen), Err(live)) => {
                    assert_eq!(frozen.to_string(), live.to_string(), "{DRIFT}: {text:?}")
                }
                (frozen, live) => panic!("{DRIFT}: {text:?}: {frozen:?} vs {live:?}"),
            }
        }
    }

    #[test]
    fn preset_documents_round_trip_like_live_presets() {
        for text in [
            "name = 'daily'\nskills = ['one', 'two']\n",
            "name = 'full'\ndescription = 'd'\ncolor = 'blue'\nskills = ['a']\nagents = ['claude']\n",
            "name = 'extra'\nunknown = 1\n",
            "skills = ['one']\n",
            "name = 'typed'\nagents = 'claude'\n",
        ] {
            let frozen = toml::from_str::<v1::PresetV1>(text);
            let live = toml::from_str::<Preset>(text);
            match (frozen, live) {
                (Ok(frozen), Ok(live)) => {
                    assert_eq!(
                        (
                            &frozen.name,
                            &frozen.description,
                            &frozen.color,
                            &frozen.skills,
                            &frozen.agents
                        ),
                        (
                            &live.name,
                            &live.description,
                            &live.color,
                            &live.skills,
                            &live.agents
                        ),
                        "{DRIFT}: {text:?}"
                    );
                    assert_eq!(
                        toml::to_string(&frozen).unwrap(),
                        toml::to_string(&live).unwrap(),
                        "{DRIFT}: {text:?}"
                    );
                }
                (Err(frozen), Err(live)) => {
                    assert_eq!(frozen.to_string(), live.to_string(), "{DRIFT}: {text:?}")
                }
                (frozen, live) => panic!("{DRIFT}: {text:?}: {frozen:?} vs {live:?}"),
            }
        }
    }
}
