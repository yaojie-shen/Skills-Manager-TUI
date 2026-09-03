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
