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

/// Detect installed software; configuration and skills directories are not evidence.
pub fn detect_in(
    home: &std::path::Path,
    project: &std::path::Path,
    executable_dirs: &[std::path::PathBuf],
) -> std::collections::BTreeSet<String> {
    detect_with_applications(home, project, executable_dirs, &[])
}

pub fn detect_with_applications(
    home: &std::path::Path,
    _project: &std::path::Path,
    executable_dirs: &[std::path::PathBuf],
    application_dirs: &[std::path::PathBuf],
) -> std::collections::BTreeSet<String> {
    use std::os::unix::fs::PermissionsExt;
    let is_executable = |path: &std::path::Path| {
        std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    };
    let user_bin = home.join(".local/bin");
    let user_apps = home.join("Applications");
    let mut found = std::collections::BTreeSet::new();
    for definition in BUILTINS {
        let command = match definition.key {
            "github-copilot" => "copilot",
            "gemini-cli" => "gemini",
            "qwen-code" => "qwen",
            "kimi-code-cli" => "kimi",
            key => key,
        };
        let apps: &[&str] = match definition.key {
            "codex" => &["Codex.app"],
            "cursor" => &["Cursor.app"],
            "windsurf" => &["Windsurf.app"],
            "trae" => &["Trae.app", "TRAE.app", "TRAE SOLO.app"],
            "trae-cn" => &["Trae CN.app", "TRAE CN.app", "TRAE SOLO CN.app"],
            _ => &[],
        };
        let cli = executable_dirs
            .iter()
            .chain(std::iter::once(&user_bin))
            .any(|dir| is_executable(&dir.join(command)));
        let app = application_dirs
            .iter()
            .chain(std::iter::once(&user_apps))
            .any(|dir| {
                apps.iter().any(|name| {
                    let bundle = dir.join(name).join("Contents");
                    bundle.join("Info.plist").is_file()
                        && std::fs::read_dir(bundle.join("MacOS")).is_ok_and(|entries| {
                            entries
                                .filter_map(Result::ok)
                                .any(|entry| is_executable(&entry.path()))
                        })
                })
            });
        if cli || app {
            found.insert(definition.key.to_string());
        }
    }
    // Use editor installation registries, not leftover extension/config folders.
    for editor in [
        ".vscode",
        ".vscode-insiders",
        ".cursor",
        ".windsurf",
        ".trae",
        ".trae-cn",
    ] {
        let extensions = home.join(editor).join("extensions");
        let installed = std::fs::read(extensions.join("extensions.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Vec<serde_json::Value>>(&bytes).ok())
            .unwrap_or_default();
        for extension in installed {
            let id = extension["identifier"]["id"]
                .as_str()
                .unwrap_or("")
                .to_lowercase();
            let product = match id.as_str() {
                "anthropic.claude-code" => "claude",
                "openai.chatgpt" => "codex",
                "github.copilot-chat" => "github-copilot",
                "saoudrizwan.claude-dev" => "cline",
                "rooveterinaryinc.roo-cline" => "roo",
                "continue.continue" => "continue",
                "kilocode.kilo-code" => "kilo",
                "augment.vscode-augment" => "augment",
                _ => continue,
            };
            let path = extension["relativeLocation"]
                .as_str()
                .map(|s| extensions.join(s))
                .or_else(|| {
                    extension["location"]["path"]
                        .as_str()
                        .map(std::path::PathBuf::from)
                });
            let Some(path) = path else { continue };
            let package = std::fs::read(path.join("package.json"))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
            if let Some(package) = package {
                let identity = format!(
                    "{}.{}",
                    package["publisher"].as_str().unwrap_or(""),
                    package["name"].as_str().unwrap_or("")
                )
                .to_lowercase();
                if identity == id
                    && ["main", "browser"].iter().any(|field| {
                        package[*field]
                            .as_str()
                            .is_some_and(|entry| path.join(entry).is_file())
                    })
                {
                    found.insert(product.to_string());
                }
            }
        }
    }
    found
}
