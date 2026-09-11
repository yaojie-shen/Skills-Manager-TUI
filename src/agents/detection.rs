//! Read-only installation detection. Each probe checks one installation source.
use super::BUILTINS;
use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// Product-specific identities live together; products without an override use
/// their built-in key as the CLI command and have no desktop/extension probe.
struct ProductIdentity {
    key: &'static str,
    command: &'static str,
    applications: &'static [&'static str],
    extension: Option<&'static str>,
}

const IDENTITIES: &[ProductIdentity] = &[
    ProductIdentity {
        key: "claude",
        command: "claude",
        applications: &[],
        extension: Some("anthropic.claude-code"),
    },
    ProductIdentity {
        key: "codex",
        command: "codex",
        applications: &["Codex.app"],
        extension: Some("openai.chatgpt"),
    },
    ProductIdentity {
        key: "cursor",
        command: "cursor",
        applications: &["Cursor.app"],
        extension: None,
    },
    ProductIdentity {
        key: "github-copilot",
        command: "copilot",
        applications: &[],
        extension: Some("github.copilot-chat"),
    },
    ProductIdentity {
        key: "gemini-cli",
        command: "gemini",
        applications: &[],
        extension: None,
    },
    ProductIdentity {
        key: "windsurf",
        command: "windsurf",
        applications: &["Windsurf.app"],
        extension: None,
    },
    ProductIdentity {
        key: "trae",
        command: "trae",
        applications: &["Trae.app", "TRAE.app"],
        extension: None,
    },
    ProductIdentity {
        key: "trae-cn",
        command: "trae-cn",
        applications: &["Trae CN.app", "TRAE CN.app"],
        extension: None,
    },
    ProductIdentity {
        key: "cline",
        command: "cline",
        applications: &[],
        extension: Some("saoudrizwan.claude-dev"),
    },
    ProductIdentity {
        key: "roo",
        command: "roo",
        applications: &[],
        extension: Some("rooveterinaryinc.roo-cline"),
    },
    ProductIdentity {
        key: "continue",
        command: "continue",
        applications: &[],
        extension: Some("continue.continue"),
    },
    ProductIdentity {
        key: "kilo",
        command: "kilo",
        applications: &[],
        extension: Some("kilocode.kilo-code"),
    },
    ProductIdentity {
        key: "qwen-code",
        command: "qwen",
        applications: &[],
        extension: None,
    },
    ProductIdentity {
        key: "kimi-code-cli",
        command: "kimi",
        applications: &[],
        extension: None,
    },
    ProductIdentity {
        key: "augment",
        command: "augment",
        applications: &[],
        extension: Some("augment.vscode-augment"),
    },
];

const EDITORS: &[&str] = &[
    ".vscode",
    ".vscode-insiders",
    ".cursor",
    ".windsurf",
    ".trae",
    ".trae-cn",
];

/// Detect installed software; configuration and skills directories are not evidence.
pub fn detect_in(home: &Path, project: &Path, executable_dirs: &[PathBuf]) -> BTreeSet<String> {
    detect_with_applications(home, project, executable_dirs, &[])
}

/// Keep the project argument for compatibility with existing callers. Installed
/// product identity is independent of the selected deployment project.
pub fn detect_with_applications(
    home: &Path,
    _project: &Path,
    executable_dirs: &[PathBuf],
    application_dirs: &[PathBuf],
) -> BTreeSet<String> {
    let user_bin = home.join(".local/bin");
    let user_apps = home.join("Applications");
    let mut found = BTreeSet::new();
    for definition in BUILTINS {
        // These generations share a launcher but read different skill roots.
        if matches!(definition.key, "trae-cli" | "trae-cli-v1") {
            continue;
        }
        let identity = IDENTITIES.iter().find(|rule| rule.key == definition.key);
        let command = identity.map_or(definition.key, |rule| rule.command);
        let applications = identity.map_or(&[][..], |rule| rule.applications);
        if detect_cli(command, executable_dirs, &user_bin)
            || detect_macos_applications(applications, application_dirs, &user_apps)
        {
            found.insert(definition.key.to_string());
        }
    }
    // Public installers use traecli -> trae-cli (1.x) or traecli -> traex (2.0).
    // The standalone trae-cli command also belongs to the unrelated open-source
    // Trae Agent, so it is not evidence without the public traecli launcher.
    // Inspect paths only; discovery never executes an installed program.
    for dir in executable_dirs.iter().chain(std::iter::once(&user_bin)) {
        for command in ["traecli", "traex"] {
            let path = dir.join(command);
            if !is_executable(&path) {
                continue;
            }
            let Ok(resolved) = path.canonicalize() else {
                continue;
            };
            match resolved.file_name().and_then(|name| name.to_str()) {
                Some("traex") => {
                    found.insert("trae-cli".into());
                }
                Some("trae-cli") if command == "traecli" => {
                    found.insert("trae-cli-v1".into());
                }
                _ => {}
            }
        }
    }
    found.extend(detect_editor_extensions(home));
    found
}

fn is_executable(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

fn detect_cli(command: &str, directories: &[PathBuf], user_bin: &Path) -> bool {
    directories
        .iter()
        .map(PathBuf::as_path)
        .chain(std::iter::once(user_bin))
        .any(|dir| is_executable(&dir.join(command)))
}

fn detect_macos_applications(names: &[&str], directories: &[PathBuf], user_apps: &Path) -> bool {
    directories
        .iter()
        .map(PathBuf::as_path)
        .chain(std::iter::once(user_apps))
        .any(|dir| {
            names.iter().any(|name| {
                let bundle = dir.join(name).join("Contents");
                bundle.join("Info.plist").is_file()
                    && std::fs::read_dir(bundle.join("MacOS")).is_ok_and(|entries| {
                        entries
                            .filter_map(Result::ok)
                            .any(|entry| is_executable(&entry.path()))
                    })
            })
        })
}

fn detect_editor_extensions(home: &Path) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for editor in EDITORS {
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
            let Some(identity) = IDENTITIES
                .iter()
                .find(|rule| rule.extension == Some(id.as_str()))
            else {
                continue;
            };
            let path = extension["relativeLocation"]
                .as_str()
                .map(|s| extensions.join(s))
                .or_else(|| extension["location"]["path"].as_str().map(PathBuf::from));
            if let Some(path) = path
                && extension_payload_matches(&path, &id)
            {
                found.insert(identity.key.to_string());
            }
        }
    }
    found
}

fn extension_payload_matches(path: &Path, id: &str) -> bool {
    let Some(package) = std::fs::read(path.join("package.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
    else {
        return false;
    };
    let identity = format!(
        "{}.{}",
        package["publisher"].as_str().unwrap_or(""),
        package["name"].as_str().unwrap_or("")
    )
    .to_lowercase();
    identity == id
        && ["main", "browser"].iter().any(|field| {
            package[*field]
                .as_str()
                .is_some_and(|entry| path.join(entry).is_file())
        })
}
