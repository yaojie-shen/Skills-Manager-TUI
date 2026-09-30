//! Repository-level configuration: `<root>/.skills-meta/config.toml`.
//!
//! This module owns the persisted schema and defaults. The TUI resolves these
//! values with its application defaults and session preferences in
//! `tui::settings`; views consume that snapshot. Paths use `~` and are expanded
//! at load time.

use crate::paths::{expand_tilde, meta_dir};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, value};

pub const CONFIG_FILE: &str = "config.toml";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "default_schema")]
    pub schema: u32,
    #[serde(default = "default_agents")]
    pub agents: Vec<AgentConfig>,
    #[serde(default)]
    pub tags: Vec<TagConfig>,
    #[serde(default = "default_true")]
    pub tags_enabled: bool,
    #[serde(default)]
    pub search: SearchConfig,
    #[serde(default)]
    pub ui: UiConfig,
    #[serde(default)]
    pub sync: SyncConfig,
}

/// Waiting policy for automatic root sync in the TUI. Zero disables that wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SyncConfig {
    pub quiet_seconds: u64,
    pub tui_idle_seconds: u64,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            quiet_seconds: 120,
            tui_idle_seconds: 10,
        }
    }
}

/// Skill result density. The file supplies the startup default; page-scoped
/// session choices override it until the process exits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum UiLayout {
    /// Framed cards in as many columns as fit; previews open as overlays.
    #[default]
    Grid,
    /// Four unframed lines per skill: identity, two description lines, and
    /// source/Tag/Preset metadata. Search panels show an adjacent preview;
    /// Agents uses overlays.
    List,
    /// One identity row per skill, with an optional search-excerpt row.
    #[serde(alias = "split")]
    Compact,
}

impl UiLayout {
    /// Shared order for the session layout shortcut on every skills page.
    pub fn next(self) -> Self {
        match self {
            Self::Grid => Self::List,
            Self::List => Self::Compact,
            Self::Compact => Self::Grid,
        }
    }
}

/// What caps the ends of a Tag pill. A terminal cell is taller than it is
/// wide, so a rounded end has to be drawn by a glyph that fills the whole cell:
/// the geometric half-discs (U+25D6/U+25D7) sit at x-height and read as a bead
/// beside the fill, not as the end of a capsule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PillCaps {
    /// Powerline's thick half circles, full height and genuinely round. Needs a
    /// patched font (Nerd Font, Powerline); without one they show as tofu.
    #[default]
    Round,
    /// Half blocks: full height with square ends, present in every font.
    Block,
    /// No caps at all, leaving a plain filled rectangle.
    None,
}

impl PillCaps {
    /// Left and right cap, drawn in the fill colour against the page.
    pub fn glyphs(self) -> (&'static str, &'static str) {
        match self {
            // U+E0B6 and U+E0B4, Powerline Extra's round caps.
            PillCaps::Round => ("\u{e0b6}", "\u{e0b4}"),
            PillCaps::Block => ("\u{258c}", "\u{2590}"),
            PillCaps::None => ("", ""),
        }
    }
}

/// Source decorations; text mode supports terminals without Nerd Fonts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Icons {
    Text,
    #[default]
    Nerd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct UiConfig {
    #[serde(default)]
    pub layout: UiLayout,
    #[serde(default)]
    pub pill_caps: PillCaps,
    #[serde(default)]
    pub icons: Icons,
}

/// Search tuning. Defaults follow Omnisearch-style field boosting; every value
/// can be overridden in `config.toml` under `[search]`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SearchConfig {
    /// Per-field BM25F boosts.
    #[serde(default)]
    pub weights: FieldWeights,
    /// Match on word prefixes (`msgp` finds `msgpack`).
    #[serde(default = "default_true")]
    pub prefix: bool,
    /// Typo tolerance: edit distance 1 from 4 chars, 2 from 8 chars.
    #[serde(default = "default_true")]
    pub fuzzy: bool,
    /// Expand query words through the English/Chinese dictionaries.
    #[serde(default = "default_true")]
    pub dictionary: bool,
    /// Per-source weight of a dictionary-expanded match, relative to a direct
    /// match. Zero disables that source.
    #[serde(default)]
    pub dictionaries: DictionaryWeights,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            weights: FieldWeights::default(),
            prefix: true,
            fuzzy: true,
            dictionary: true,
            dictionaries: DictionaryWeights::default(),
        }
    }
}

/// How much a match found through each dictionary counts. Technical terms are
/// nearly one-to-one and carry a strong signal; general vocabulary is more
/// ambiguous, so it only breaks ties. The user's own table is trusted most.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DictionaryWeights {
    #[serde(default = "w_tech")]
    pub tech: f32,
    #[serde(default = "w_common")]
    pub common: f32,
    #[serde(default = "w_user")]
    pub user: f32,
}

impl Default for DictionaryWeights {
    fn default() -> Self {
        Self {
            tech: w_tech(),
            common: w_common(),
            user: w_user(),
        }
    }
}

fn w_tech() -> f32 {
    0.7
}
fn w_common() -> f32 {
    0.4
}
fn w_user() -> f32 {
    1.0
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FieldWeights {
    #[serde(default = "w_name")]
    pub name: f32,
    #[serde(default = "w_tag")]
    pub tag: f32,
    #[serde(default = "w_description")]
    pub description: f32,
    #[serde(default = "w_note")]
    pub note: f32,
    #[serde(default = "w_heading")]
    pub heading: f32,
    #[serde(default = "w_body")]
    pub body: f32,
}

impl Default for FieldWeights {
    fn default() -> Self {
        Self {
            name: w_name(),
            tag: w_tag(),
            description: w_description(),
            note: w_note(),
            heading: w_heading(),
            body: w_body(),
        }
    }
}

fn w_name() -> f32 {
    6.0
}
fn w_tag() -> f32 {
    4.0
}
fn w_description() -> f32 {
    2.0
}
fn w_note() -> f32 {
    2.0
}
fn w_heading() -> f32 {
    1.5
}
fn w_body() -> f32 {
    1.0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConfig {
    /// Short identifier used on the command line, e.g. `claude`.
    pub key: String,
    /// Human-readable name.
    #[serde(default)]
    pub name: String,
    /// Skills directory of this agent, `~` allowed.
    pub skills_dir: String,
}

impl AgentConfig {
    pub fn skills_path(&self) -> PathBuf {
        expand_tilde(&self.skills_dir)
    }
    pub fn display_name(&self) -> &str {
        if self.name.is_empty() {
            &self.key
        } else {
            &self.name
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TagConfig {
    #[serde(default)]
    pub skills: Vec<String>,
    pub name: String,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

fn default_schema() -> u32 {
    1
}
fn default_true() -> bool {
    true
}

pub fn default_agents() -> Vec<AgentConfig> {
    crate::agents::defaults(false)
}

impl Default for Config {
    fn default() -> Self {
        Self {
            schema: 1,
            agents: default_agents(),
            tags: Vec::new(),
            tags_enabled: true,
            search: SearchConfig::default(),
            ui: UiConfig::default(),
            sync: SyncConfig::default(),
        }
    }
}

impl Config {
    /// Ignore the retired deploy section only at the file boundary. It is not
    /// represented in runtime configuration, and all other fields stay strict.
    fn parse(text: &str) -> Result<Self> {
        let mut doc: DocumentMut = text.parse()?;
        doc.remove("deploy");
        Ok(toml::from_str(&doc.to_string())?)
    }
    pub fn local_default() -> Self {
        Self {
            agents: crate::agents::defaults(true),
            ..Self::default()
        }
    }

    /// Append one agent without rewriting comments or activating other built-ins.
    pub fn add_agent(root: &Path, agent: &AgentConfig, local: bool) -> Result<()> {
        if !crate::util::valid_skill_key(&agent.key) || agent.skills_dir.trim().is_empty() {
            anyhow::bail!("agent key and skills directory must be nonempty and valid");
        }
        let fallback = if local {
            Self::local_default()
        } else {
            Self::default()
        };
        Self::edit_document(root, |doc| {
            let parsed = Self::parse(&doc.to_string())?;
            let existing = if doc.get("agents").is_some() {
                &parsed.agents
            } else {
                &fallback.agents
            };
            if existing.iter().any(|a| a.key == agent.key) {
                anyhow::bail!("agent already configured: {}", agent.key);
            }
            if doc.get("agents").is_none() {
                let mut entries = ArrayOfTables::new();
                for a in existing {
                    entries.push(Self::agent_table(a));
                }
                doc["agents"] = Item::ArrayOfTables(entries);
            }
            doc["agents"]
                .as_array_of_tables_mut()
                .context("agents must use [[agents]] tables")?
                .push(Self::agent_table(agent));
            Ok(true)
        })
    }

    fn agent_table(agent: &AgentConfig) -> Table {
        let mut table = Table::new();
        table["key"] = value(&agent.key);
        table["name"] = value(&agent.name);
        table["skills_dir"] = value(&agent.skills_dir);
        table
    }

    pub fn path(root: &Path) -> PathBuf {
        meta_dir(root).join(CONFIG_FILE)
    }

    /// Load the config, falling back to defaults when the file does not exist.
    pub fn load(root: &Path) -> Result<Self> {
        let path = Self::path(root);
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                Self::parse(&text).with_context(|| format!("invalid config: {}", path.display()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn exists(root: &Path) -> bool {
        Self::path(root).is_file()
    }

    /// Write the config. Used by `init`; everyday edits are expected to be made by hand.
    pub fn save(&self, root: &Path) -> Result<()> {
        let path = Self::path(root);
        std::fs::create_dir_all(path.parent().unwrap())?;
        let text = toml::to_string_pretty(self)?;
        crate::util::write_atomic(&path, text.as_bytes())
    }

    /// Change `config.toml` on disk without disturbing the rest of it. The
    /// file is meant to be edited by hand, so anything the tool writes has to
    /// keep the comments and order the user put there; `save` is a serde
    /// round trip and would drop them. `edit` says whether it changed anything,
    /// so a no-op leaves the file untouched.
    fn edit_document(
        root: &Path,
        edit: impl FnOnce(&mut DocumentMut) -> Result<bool>,
    ) -> Result<()> {
        let path = Self::path(root);
        let mut doc = match std::fs::read_to_string(&path) {
            Ok(text) => text
                .parse::<DocumentMut>()
                .with_context(|| format!("invalid config: {}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => DocumentMut::new(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        if edit(&mut doc)? {
            crate::util::write_atomic(&path, doc.to_string().as_bytes())?;
        }
        Ok(())
    }

    /// The `[[tags]]` entries of a document, created when there are none yet.
    /// `save` writes an empty list as `tags = []`, and a hand-written file may
    /// use inline tables; either is turned into `[[tags]]` tables first.
    fn tag_tables(doc: &mut DocumentMut) -> Result<&mut ArrayOfTables> {
        if let Some(arr) = doc.get("tags").and_then(Item::as_array) {
            let mut tables = ArrayOfTables::new();
            for v in arr.iter() {
                let t = v
                    .as_inline_table()
                    .context("an entry of `tags` in config.toml is not a table")?;
                tables.push(t.clone().into_table());
            }
            doc["tags"] = Item::ArrayOfTables(tables);
        }
        doc.entry("tags")
            .or_insert(Item::ArrayOfTables(ArrayOfTables::new()))
            .as_array_of_tables_mut()
            .context("`tags` in config.toml is not a list of [[tags]] tables")
    }

    fn tag_index(tables: &ArrayOfTables, name: &str) -> Option<usize> {
        tables
            .iter()
            .position(|t| t.get("name").and_then(|v| v.as_str()) == Some(name))
    }

    /// Give a tag a colour, adding its `[[tags]]` entry when it has none, or
    /// take the colour away again with `None`. The value is written as given;
    /// what counts as a colour is the caller's business.
    pub fn set_tag_color(root: &Path, name: &str, color: Option<&str>) -> Result<()> {
        Self::edit_document(root, |doc| {
            let tables = Self::tag_tables(doc)?;
            match (Self::tag_index(tables, name), color) {
                (Some(i), Some(c)) => {
                    tables.get_mut(i).context("tag entry vanished")?["color"] = value(c);
                }
                (Some(i), None) => {
                    let t = tables.get_mut(i).context("tag entry vanished")?;
                    if t.remove("color").is_none() {
                        return Ok(false);
                    }
                    // An entry with nothing left but its name says nothing, so
                    // it goes rather than accumulate.
                    if t.len() == 1 {
                        tables.remove(i);
                    }
                }
                (None, Some(c)) => {
                    let mut t = Table::new();
                    t["name"] = value(name);
                    t["color"] = value(c);
                    tables.push(t);
                }
                (None, None) => return Ok(false),
            }
            Ok(true)
        })
    }

    /// Carry a tag's `[[tags]]` entry over to its new name. When the new name
    /// already has an entry of its own, that one wins and the old is dropped:
    /// the tag is being merged into it, not replacing it.
    pub fn rename_tag_entry(root: &Path, old: &str, new: &str) -> Result<()> {
        Self::edit_document(root, |doc| {
            let tables = Self::tag_tables(doc)?;
            let Some(i) = Self::tag_index(tables, old) else {
                return Ok(false);
            };
            if Self::tag_index(tables, new).is_some() {
                tables.remove(i);
            } else {
                tables.get_mut(i).context("tag entry vanished")?["name"] = value(new);
            }
            Ok(true)
        })
    }

    pub fn skill_tags(&self, key: &str) -> Vec<String> {
        self.tags
            .iter()
            .filter(|t| t.skills.iter().any(|s| s == key))
            .map(|t| t.name.clone())
            .collect()
    }

    pub fn edit_tags(root: &Path, edit: impl FnOnce(&mut Vec<TagConfig>)) -> Result<()> {
        let _lock = crate::meta::MetaStore::new(root).lock()?;
        Self::edit_document(root, |doc| {
            let mut config = Self::parse(&doc.to_string())?;
            edit(&mut config.tags);
            for tag in &mut config.tags {
                tag.skills.sort();
                tag.skills.dedup();
            }
            let updated = toml::to_string(&config)?.parse::<DocumentMut>()?;
            doc["tags"] = updated["tags"].clone();
            Ok(true)
        })
    }

    pub fn set_tags_enabled(root: &Path, enabled: bool) -> Result<()> {
        Self::edit_document(root, |doc| {
            doc["tags_enabled"] = value(enabled);
            Ok(true)
        })
    }

    pub fn rename_tag_skill(root: &Path, old: &str, new: Option<&str>) -> Result<()> {
        Self::edit_tags(root, |tags| {
            for tag in tags {
                if tag.skills.iter().any(|s| s == old) {
                    tag.skills.retain(|s| s != old);
                    if let Some(new) = new {
                        tag.skills.push(new.to_string());
                    }
                }
            }
        })
    }

    pub fn agent(&self, key: &str) -> Option<&AgentConfig> {
        self.agents.iter().find(|a| a.key == key)
    }

    pub fn agent_keys(&self) -> Vec<String> {
        self.agents.iter().map(|a| a.key.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_waits_default_and_accept_nonnegative_seconds() {
        assert_eq!(Config::parse("").unwrap().sync, SyncConfig::default());
        assert_eq!(
            Config::parse("[sync]\nquiet_seconds = 45").unwrap().sync,
            SyncConfig {
                quiet_seconds: 45,
                tui_idle_seconds: 10,
            }
        );
        let configured = Config::parse("[sync]\nquiet_seconds = 0\ntui_idle_seconds = 30").unwrap();
        assert_eq!(configured.sync.quiet_seconds, 0);
        assert_eq!(configured.sync.tui_idle_seconds, 30);
        assert_eq!(
            Config::parse("[sync]\ntui_idle_seconds = 0")
                .unwrap()
                .sync
                .tui_idle_seconds,
            0
        );
        for invalid in [
            "quiet_seconds = -1",
            "tui_idle_seconds = -1",
            "quiet_seconds = 1.5",
            "tui_idle_seconds = 'ten'",
            "unknown_wait = 5",
        ] {
            assert!(Config::parse(&format!("[sync]\n{invalid}")).is_err());
        }
        let tmp = crate::ops::DownloadDir::new("sync-config-roundtrip").unwrap();
        configured.save(tmp.path()).unwrap();
        assert_eq!(Config::load(tmp.path()).unwrap().sync, configured.sync);
    }

    #[test]
    fn local_config_edits_preserve_sync_values_comments_and_order() {
        let tmp = crate::ops::DownloadDir::new("sync-config-preserve").unwrap();
        let path = Config::path(tmp.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let sync_section = "[sync] # automatic backup waits\nquiet_seconds = 42 # content\ntui_idle_seconds = 7 # activity\n\n";
        std::fs::write(
            &path,
            format!("[[agents]]\nkey = 'existing'\nskills_dir = '~/.existing/skills'\n\n{sync_section}[ui]\nlayout = 'list'\n"),
        )
        .unwrap();
        Config::edit_tags(tmp.path(), |tags| {
            tags.push(TagConfig {
                name: "example".into(),
                skills: vec![],
                color: None,
                description: None,
            });
        })
        .unwrap();
        Config::add_agent(
            tmp.path(),
            &AgentConfig {
                key: "example".into(),
                name: "Example".into(),
                skills_dir: "~/.example/skills".into(),
            },
            false,
        )
        .unwrap();
        let text = std::fs::read_to_string(path).unwrap();
        assert!(text.contains(sync_section));
        assert!(text.find("[sync]").unwrap() < text.find("[ui]").unwrap());
        assert_eq!(
            Config::load(tmp.path()).unwrap().sync,
            SyncConfig {
                quiet_seconds: 42,
                tui_idle_seconds: 7,
            }
        );
    }
}
