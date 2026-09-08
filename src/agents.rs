//! Built-in skill locations. Sources and compatibility notes: docs/agents.md.
use crate::config::AgentConfig;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct AgentDefinition {
    pub key: &'static str,
    pub name: &'static str,
    pub global_dir: &'static str,
    pub local_dir: &'static str,
}

macro_rules! agents {
    ($(($key:literal, $name:literal, $global:literal, $local:literal)),* $(,)?) => {
        pub const BUILTINS: &[AgentDefinition] = &[$(AgentDefinition {
            key: $key, name: $name, global_dir: $global, local_dir: $local,
        }),*];
    };
}

agents![
    (
        "claude",
        "Claude Code",
        "~/.claude/skills",
        ".claude/skills"
    ),
    ("codex", "Codex", "~/.codex/skills", ".agents/skills"),
    ("cursor", "Cursor", "~/.cursor/skills", ".cursor/skills"),
    (
        "github-copilot",
        "GitHub Copilot",
        "~/.copilot/skills",
        ".github/skills"
    ),
    (
        "gemini-cli",
        "Gemini CLI",
        "~/.gemini/skills",
        ".agents/skills"
    ),
    (
        "opencode",
        "OpenCode",
        "~/.config/opencode/skills",
        ".opencode/skills"
    ),
    (
        "windsurf",
        "Windsurf",
        "~/.codeium/windsurf/skills",
        ".windsurf/skills"
    ),
    ("trae", "Trae", "~/.trae/skills", ".trae/skills"),
    ("trae-cn", "Trae CN", "~/.trae-cn/skills", ".trae/skills"),
    ("cline", "Cline", "~/.agents/skills", ".agents/skills"),
    ("roo", "Roo Code", "~/.roo/skills", ".roo/skills"),
    (
        "continue",
        "Continue",
        "~/.continue/skills",
        ".continue/skills"
    ),
    (
        "kilo",
        "Kilo Code",
        "~/.kilocode/skills",
        ".kilocode/skills"
    ),
    ("amp", "Amp", "~/.config/agents/skills", ".agents/skills"),
    ("qwen-code", "Qwen Code", "~/.qwen/skills", ".qwen/skills"),
    (
        "kimi-code-cli",
        "Kimi Code CLI",
        "~/.agents/skills",
        ".agents/skills"
    ),
    ("kiro-cli", "Kiro CLI", "~/.kiro/skills", ".kiro/skills"),
    ("droid", "Droid", "~/.factory/skills", ".factory/skills"),
    ("augment", "Augment", "~/.augment/skills", ".augment/skills"),
];

impl AgentDefinition {
    pub fn config(&self, local: bool) -> AgentConfig {
        AgentConfig {
            key: self.key.into(),
            name: self.name.into(),
            skills_dir: if local {
                self.local_dir
            } else {
                self.global_dir
            }
            .into(),
        }
    }
}

pub fn defaults(local: bool) -> Vec<AgentConfig> {
    BUILTINS[..2].iter().map(|a| a.config(local)).collect()
}
