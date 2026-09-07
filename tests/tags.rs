//! Managing tags as things in their own right: merging one into another,
//! and giving a tag a colour in `config.toml` without disturbing the rest of
//! the file.

use skills::Workspace;
use skills::config::{AgentConfig, Config, DeployConfig, TagConfig};
use skills::history::{self, History};
use skills::ops::edit;
use std::path::PathBuf;

struct Fixture {
    base: PathBuf,
    root: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let base = std::env::temp_dir().join(format!("skills-tags-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("skills");
        std::fs::create_dir_all(&root).unwrap();
        Config {
            schema: 1,
            agents: vec![AgentConfig {
                key: "a".into(),
                name: "Agent A".into(),
                skills_dir: base.join("agent-a").display().to_string(),
            }],
            deploy: DeployConfig {
                all_to_all: false,
                presets: vec![],
            },
            tags: vec![TagConfig {
                name: "storage".into(),
                color: Some("blue".into()),
                description: Some("keeps bytes".into()),
            }],
            search: Default::default(),
            ui: Default::default(),
        }
        .save(&root)
        .unwrap();
        let fx = Self { base, root };
        for key in ["printer", "bicycle", "etcd"] {
            let dir = fx.root.join(key);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("SKILL.md"),
                format!("---\nname: {key}\ndescription: a {key}\n---\n"),
            )
            .unwrap();
        }
        fx
    }

    fn ws(&self) -> Workspace {
        Workspace::open(&self.root).unwrap()
    }

    fn tags(&self, key: &str) -> Vec<String> {
        self.ws()
            .meta
            .load(key)
            .unwrap()
            .map(|m| m.tags)
            .unwrap_or_default()
    }

    fn config_text(&self) -> String {
        std::fs::read_to_string(Config::path(&self.root)).unwrap()
    }

    fn color_of(&self, tag: &str) -> Option<String> {
        Config::load(&self.root)
            .unwrap()
            .tags
            .into_iter()
            .find(|t| t.name == tag)
            .and_then(|t| t.color)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn set_tags(ws: &Workspace, key: &str, tags: &[&str]) {
    let tags: Vec<String> = tags.iter().map(|t| t.to_string()).collect();
    edit::tag_set(ws, key, &tags).unwrap();
}

#[test]
fn merging_onto_an_existing_tag_leaves_one_copy_and_comes_back_as_one_step() {
    let fx = Fixture::new("merge");
    let ws = fx.ws();
    set_tags(&ws, "printer", &["paper", "office"]);
    set_tags(&ws, "bicycle", &["paper"]);
    set_tags(&ws, "etcd", &["office"]);

    let mut log = History::default();
    let (_, intent) = history::tag_edit(&ws, |ws| {
        edit::tag_rename(ws, "paper", "office").map(|n| n.to_string())
    })
    .unwrap();
    log.record(intent.expect("a merge changes something"));

    // A skill carrying both ends up with the target once, not twice.
    assert_eq!(fx.tags("printer"), vec!["office"]);
    assert_eq!(fx.tags("bicycle"), vec!["office"]);
    assert_eq!(fx.tags("etcd"), vec!["office"]);

    let snap = ws.scan().unwrap();
    match history::undo_plan(&ws, &snap, &log.last().unwrap().intent).unwrap() {
        history::Plan::Write { apply, .. } => {
            apply.apply(&ws).unwrap();
        }
        other => panic!("expected a write plan, got {}", plan_name(&other)),
    }
    assert_eq!(fx.tags("printer"), vec!["office", "paper"]);
    assert_eq!(fx.tags("bicycle"), vec!["paper"]);
    assert_eq!(fx.tags("etcd"), vec!["office"]);
}

fn plan_name(p: &history::Plan) -> &'static str {
    match p {
        history::Plan::Links(_) => "links",
        history::Plan::Write { .. } => "write",
        history::Plan::Nothing(_) => "nothing",
    }
}

#[test]
fn renaming_a_tag_carries_its_colour_and_merging_keeps_the_targets() {
    let fx = Fixture::new("rename-color");
    let ws = fx.ws();
    set_tags(&ws, "printer", &["storage"]);
    set_tags(&ws, "bicycle", &["disk"]);

    assert_eq!(edit::tag_rename(&ws, "storage", "bytes").unwrap(), 1);
    assert_eq!(fx.color_of("bytes").as_deref(), Some("blue"));
    assert_eq!(fx.color_of("storage"), None);

    // Merging a coloured tag into one without a colour brings the colour along.
    assert_eq!(edit::tag_rename(&ws, "bytes", "disk").unwrap(), 1);
    assert_eq!(fx.color_of("disk").as_deref(), Some("blue"));

    // Merging into a tag that has its own colour keeps the target's.
    Config::set_tag_color(&fx.root, "keep", Some("red")).unwrap();
    assert_eq!(edit::tag_rename(&ws, "disk", "keep").unwrap(), 2);
    assert_eq!(fx.color_of("keep").as_deref(), Some("red"));
    assert_eq!(fx.color_of("disk"), None);
    let cfg = Config::load(&fx.root).unwrap();
    assert_eq!(
        cfg.tags.len(),
        1,
        "the merged-away entry is gone: {:?}",
        cfg.tags
    );
}

#[test]
fn setting_a_colour_keeps_the_rest_of_the_file_as_written() {
    let fx = Fixture::new("color");
    let path = Config::path(&fx.root);
    let mut text = fx.config_text();
    text.insert_str(0, "# hand-written header\n");
    text.push_str("\n# trailing note\n");
    std::fs::write(&path, &text).unwrap();

    // A new entry for a tag with none.
    Config::set_tag_color(&fx.root, "paper", Some("#ff8800")).unwrap();
    assert_eq!(fx.color_of("paper").as_deref(), Some("#ff8800"));
    assert_eq!(fx.color_of("storage").as_deref(), Some("blue"));
    let after = fx.config_text();
    assert!(after.starts_with("# hand-written header\n"), "{after}");
    assert!(after.contains("# trailing note"), "{after}");
    assert!(after.contains("keeps bytes"), "{after}");

    // Changing an existing entry edits it in place rather than adding another.
    Config::set_tag_color(&fx.root, "storage", Some("green")).unwrap();
    assert_eq!(fx.color_of("storage").as_deref(), Some("green"));
    assert_eq!(Config::load(&fx.root).unwrap().tags.len(), 2);

    // Taking the colour away leaves the entry, and its description, behind.
    Config::set_tag_color(&fx.root, "storage", None).unwrap();
    assert_eq!(fx.color_of("storage"), None);
    let cfg = Config::load(&fx.root).unwrap();
    let storage = cfg.tags.iter().find(|t| t.name == "storage").unwrap();
    assert_eq!(storage.description.as_deref(), Some("keeps bytes"));

    // An entry that would be left with only its name is dropped instead.
    Config::set_tag_color(&fx.root, "paper", None).unwrap();
    assert!(
        Config::load(&fx.root)
            .unwrap()
            .tags
            .iter()
            .all(|t| t.name != "paper")
    );

    // Clearing a colour that was never set is not a write.
    let before = fx.config_text();
    Config::set_tag_color(&fx.root, "nowhere", None).unwrap();
    assert_eq!(fx.config_text(), before);
    assert!(
        Config::load(&fx.root)
            .unwrap()
            .tags
            .iter()
            .all(|t| t.name != "nowhere")
    );
}

#[test]
fn a_colour_can_be_set_before_there_is_a_config_file() {
    let fx = Fixture::new("no-config");
    std::fs::remove_file(Config::path(&fx.root)).unwrap();
    Config::set_tag_color(&fx.root, "paper", Some("cyan")).unwrap();
    let cfg = Config::load(&fx.root).unwrap();
    assert_eq!(cfg.tags.len(), 1);
    assert_eq!(cfg.tags[0].color.as_deref(), Some("cyan"));
    // The rest of the config is still the default, not an empty one.
    assert_eq!(cfg.agents.len(), 2);
}
