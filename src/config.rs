//! Repository-level configuration: `<root>/.skills-meta/config.toml`.
//!
//! Everything that shapes the user experience lives here so that cloning the
//! skills repository reproduces the same setup on another machine. Paths use
//! `~` and are expanded at load time.

use crate::paths::{expand_tilde, meta_dir};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

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
    vec![
        AgentConfig {
            key: "claude".into(),
            name: "Claude Code".into(),
            skills_dir: "~/.claude/skills".into(),
        },
        AgentConfig {
            key: "codex".into(),
            name: "Codex".into(),
            skills_dir: "~/.codex/skills".into(),
        },
    ]
}

impl Default for Config {
    fn default() -> Self {
        Self {
            schema: 1,
            agents: default_agents(),
            deploy: DeployConfig::default(),
            tags: Vec::new(),
            search: SearchConfig::default(),
        }
    }
}

impl Config {
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

    pub fn agent(&self, key: &str) -> Option<&AgentConfig> {
        self.agents.iter().find(|a| a.key == key)
    }

    pub fn agent_keys(&self) -> Vec<String> {
        self.agents.iter().map(|a| a.key.clone()).collect()
    }
}
