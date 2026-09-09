//! Built-in skill locations.
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
    ("cline", "Cline", "~/.cline/skills", ".cline/skills"),
    ("roo", "Roo Code", "~/.roo/skills", ".roo/skills"),
    (
        "continue",
        "Continue",
        "~/.continue/skills",
        ".continue/skills"
    ),
    ("kilo", "Kilo Code", "~/.kilo/skills", ".kilo/skills"),
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
    /// Additional documented discovery roots. These are independent deployment
    /// destinations, not directories to merge into one inventory.
    pub fn search_dirs(&self, local: bool) -> Vec<&'static str> {
        let mut dirs = vec![if local {
            self.local_dir
        } else {
            self.global_dir
        }];
        let extra: &[&str] = match (self.key, local) {
            // https://learn.chatgpt.com/docs/build-skills (legacy user root retained)
            ("codex", false) => &["~/.agents/skills"],
            ("codex", true) => &[".codex/skills"],
            // https://cursor.com/docs/skills
            ("cursor", false) => &["~/.agents/skills", "~/.claude/skills", "~/.codex/skills"],
            ("cursor", true) => &[".agents/skills", ".claude/skills", ".codex/skills"],
            // https://code.visualstudio.com/docs/agent-customization/agent-skills
            ("github-copilot", false) => &["~/.agents/skills", "~/.claude/skills"],
            ("github-copilot", true) => &[".agents/skills", ".claude/skills"],
            // https://geminicli.com/docs/cli/skills/
            ("gemini-cli", false) => &["~/.agents/skills"],
            ("gemini-cli", true) => &[".gemini/skills"],
            // https://opencode.ai/docs/skills/
            ("opencode", false) => &["~/.agents/skills", "~/.claude/skills"],
            ("opencode", true) => &[".agents/skills", ".claude/skills"],
            // https://docs.devin.ai/desktop/cascade/skills
            // Claude paths require opt-in and are not assumed here.
            ("windsurf", false) => &["~/.agents/skills"],
            ("windsurf", true) => &[".agents/skills"],
            // https://roocodeinc.github.io/Roo-Code/features/skills/
            ("roo", false) => &["~/.agents/skills"],
            ("roo", true) => &[".agents/skills"],
            // https://docs.cline.bot/customization/skills
            ("cline", true) => &[".clinerules/skills", ".claude/skills"],
            // https://docs.augmentcode.com/cli/skills
            ("augment" | "kilo", false) => &["~/.agents/skills", "~/.claude/skills"],
            ("augment" | "kilo", true) => &[".agents/skills", ".claude/skills"],
            // https://ampcode.com/docs/cli/settings
            ("amp", false) => &["~/.agents/skills", "~/.config/amp/skills"],
            // https://moonshotai.github.io/kimi-code/en/customization/skills
            ("kimi-code-cli", false) => &["~/.kimi-code/skills"],
            ("kimi-code-cli", true) => &[".kimi-code/skills"],
            _ => &[],
        };
        for path in extra {
            if !dirs.contains(path) {
                dirs.push(path);
            }
        }
        dirs
    }

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

mod detection;
pub use detection::{detect_in, detect_with_applications};
