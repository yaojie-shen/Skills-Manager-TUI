//! Reading a skill directory: `SKILL.md` frontmatter and body.

use anyhow::{Result, bail};
use std::path::{Path, PathBuf};

pub const SKILL_FILE: &str = "SKILL.md";

/// A skill as found on disk. `key` is the directory name, the identity used everywhere.
#[derive(Debug, Clone)]
pub struct SkillDoc {
    pub key: String,
    pub path: PathBuf,
    /// `name` from frontmatter, may differ from `key`.
    pub name: String,
    pub description: String,
    /// Markdown body after the frontmatter.
    pub body: String,
    /// The skill directory itself is a symlink pointing elsewhere.
    pub external: bool,
}

impl SkillDoc {
    pub fn load(dir: &Path) -> Result<Self> {
        let key = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let file = dir.join(SKILL_FILE);
        let text = match std::fs::read_to_string(&file) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                bail!("missing {SKILL_FILE}")
            }
            Err(e) => bail!("cannot read {SKILL_FILE}: {e}"),
        };
        let (front, body) = split_frontmatter(&text)?;
        let name = front.get("name").cloned().unwrap_or_default();
        let description = front.get("description").cloned().unwrap_or_default();
        if name.is_empty() {
            bail!("frontmatter has no `name`");
        }
        Ok(Self {
            key,
            path: dir.to_path_buf(),
            name,
            description,
            body: body.to_string(),
            external: crate::util::is_symlink(dir),
        })
    }

    pub fn name_mismatch(&self) -> bool {
        self.name != self.key
    }
}

/// Minimal YAML frontmatter reader. Supports top-level `key: value` scalars,
/// single/double quoted values, and folded/literal block scalars (`>`, `|`)
/// with indented continuation lines. Anything more exotic is ignored.
pub fn split_frontmatter(text: &str) -> Result<(std::collections::BTreeMap<String, String>, &str)> {
    let mut map = std::collections::BTreeMap::new();
    let mut lines = text.lines();
    let first = lines.next().unwrap_or("").trim_end();
    if first.trim() != "---" {
        bail!("no YAML frontmatter");
    }
    let mut consumed = first.len() + 1;
    let mut current: Option<(String, String, bool)> = None; // key, value, block
    let mut closed = false;
    for line in lines {
        consumed += line.len() + 1;
        if line.trim() == "---" {
            closed = true;
            break;
        }
        if let Some((key, val, _)) = current.as_mut() {
            if line.starts_with(' ') || line.starts_with('\t') || line.trim().is_empty() {
                if !val.is_empty() {
                    val.push(' ');
                }
                val.push_str(line.trim());
                continue;
            }
            let key = key.clone();
            let val = val.trim().to_string();
            map.insert(key, val);
            current = None;
        }
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        if let Some((k, v)) = line.split_once(':') {
            let k = k.trim();
            if k.is_empty() || k.starts_with(' ') {
                continue;
            }
            let v = v.trim();
            if v == ">" || v == "|" || v == ">-" || v == "|-" {
                current = Some((k.to_string(), String::new(), true));
            } else if v.is_empty() {
                current = Some((k.to_string(), String::new(), false));
            } else {
                map.insert(k.to_string(), unquote(v));
            }
        }
    }
    if let Some((k, v, _)) = current {
        map.insert(k, v.trim().to_string());
    }
    if !closed {
        bail!("unterminated YAML frontmatter");
    }
    let body = text.get(consumed.min(text.len())..).unwrap_or("");
    Ok((map, body))
}

fn unquote(v: &str) -> String {
    let v = v.trim();
    if v.len() >= 2 {
        let b = v.as_bytes();
        if (b[0] == b'"' && b[b.len() - 1] == b'"') || (b[0] == b'\'' && b[b.len() - 1] == b'\'') {
            return v[1..v.len() - 1].to_string();
        }
    }
    v.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_frontmatter() {
        let t = "---\nname: foo\ndescription: \"bar baz\"\n---\n# body\n";
        let (m, body) = split_frontmatter(t).unwrap();
        assert_eq!(m["name"], "foo");
        assert_eq!(m["description"], "bar baz");
        assert_eq!(body, "# body\n");
    }

    #[test]
    fn parses_folded_description() {
        let t = "---\nname: foo\ndescription: >\n  line one\n  line two\n---\nbody";
        let (m, _) = split_frontmatter(t).unwrap();
        assert_eq!(m["description"], "line one line two");
    }

    #[test]
    fn rejects_missing_frontmatter() {
        assert!(split_frontmatter("# no front\n").is_err());
    }
}
