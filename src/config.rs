//! Repository-level configuration: `<root>/.skills-meta/config.toml`.
//!
//! Everything that shapes the user experience lives here so that cloning the
//! skills repository reproduces the same setup on another machine. Paths use
//! `~` and are expanded at load time.

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
    pub deploy: DeployConfig,
    #[serde(default)]
    pub tags: Vec<TagConfig>,
    #[serde(default)]
    pub search: SearchConfig,
    #[serde(default)]
    pub ui: UiConfig,
}

/// How the result area is arranged. Both settings can be flipped at runtime for
/// the session; only `config.toml` decides what the next start looks like.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum UiLayout {
    /// Results across the full width in as many columns of framed cards as
    /// fit; the preview opens over them when asked for. The default: cards
    /// only earn their frames once there are several to a row.
    #[default]
    Grid,
    /// A list beside an always-open preview, three lines a skill: identity,
    /// description, tags. The card's content without the frame.
    List,
    /// The same split with one line a skill (two while searching), for when
    /// the names are what matters.
    #[serde(alias = "split")]
    Compact,
}

/// What caps the ends of a preset pill. A terminal cell is taller than it is
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeployConfig {
    /// Deploy every skill to every agent. Matches the "one shared directory" setup.
    #[serde(default = "default_true")]
    pub all_to_all: bool,
    /// Presets whose members are part of the desired deployment state.
    #[serde(default)]
    pub presets: Vec<String>,
}

impl Default for DeployConfig {
    fn default() -> Self {
        Self {
            all_to_all: true,
            presets: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TagConfig {
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
            deploy: DeployConfig::default(),
            tags: Vec::new(),
            search: SearchConfig::default(),
            ui: UiConfig::default(),
        }
    }
}

impl Config {
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
            let parsed: Self = toml::from_str(&doc.to_string())?;
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
                toml::from_str(&text).with_context(|| format!("invalid config: {}", path.display()))
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

    pub fn agent(&self, key: &str) -> Option<&AgentConfig> {
        self.agents.iter().find(|a| a.key == key)
    }

    pub fn agent_keys(&self) -> Vec<String> {
        self.agents.iter().map(|a| a.key.clone()).collect()
    }
}
