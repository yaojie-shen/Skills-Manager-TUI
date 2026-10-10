//! Frozen definition of Home layout 1: document schemas, Tag/preset shapes,
//! the Tag serializer, and the group filename rules. Never edit after release.

use super::Schemas;
use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};
use toml_edit::{DocumentMut, Item, value};
use unicode_casefold::UnicodeCaseFold;
use unicode_normalization::UnicodeNormalization;

pub const LAYOUT: u32 = 1;
pub const SCHEMAS: Schemas = Schemas {
    config: 2,
    tag: 1,
    preset: 1,
    repository: 1,
};

/// Read a document's `schema` key, using `missing` when it is absent.
pub fn schema(
    doc: &DocumentMut,
    path: &Path,
    kind: &str,
    current: u32,
    missing: u32,
) -> Result<u32> {
    let found = match doc.get("schema") {
        None => missing,
        Some(Item::Value(value)) => match value.as_integer() {
            Some(value) if value > 0 && value <= u32::MAX as i64 => value as u32,
            _ => bail!(
                "invalid {kind} schema in {}: schema must be a positive integer",
                path.display()
            ),
        },
        Some(_) => bail!(
            "invalid {kind} schema in {}: schema must be a positive integer",
            path.display()
        ),
    };
    if found > current {
        bail!(
            "unsupported {kind} schema {found} in {}; this version supports up to {current}; open it with a newer version of Skills Manager",
            path.display()
        );
    }
    Ok(found)
}

pub fn set_schema(doc: &mut DocumentMut, schema: u32) {
    doc["schema"] = value(schema as i64);
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TagV1 {
    #[serde(default)]
    pub skills: Vec<String>,
    pub name: String,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

pub fn serialize_tag(tag: &TagV1) -> Result<Vec<u8>> {
    let mut doc = toml::to_string_pretty(tag)?.parse::<DocumentMut>()?;
    set_schema(&mut doc, SCHEMAS.tag);
    Ok(doc.to_string().into_bytes())
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PresetV1 {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub agents: Vec<String>,
}

const MAX_STEM_BYTES: usize = 180;

pub fn normalize_name(name: &str) -> Result<String> {
    let name: String = name.trim().nfc().collect();
    ensure!(!name.is_empty(), "group name is empty");
    ensure!(
        !name.chars().any(char::is_control),
        "group name must not contain control characters"
    );
    Ok(name)
}

fn digest(name: &str, digits: usize) -> String {
    let hash = Sha256::digest(name.as_bytes());
    let hex = hash
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    hex[..digits.min(hex.len())].to_owned()
}

fn windows_device(stem: &str) -> bool {
    let base = stem.split('.').next().unwrap_or(stem).to_ascii_uppercase();
    matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || base
            .strip_prefix("COM")
            .or_else(|| base.strip_prefix("LPT"))
            .is_some_and(|suffix| {
                matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
            })
}

fn truncate_utf8(value: &str, max: usize) -> String {
    if value.len() <= max {
        return value.to_owned();
    }
    let mut end = max;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].trim_end().to_owned()
}

fn safe_stem(name: &str) -> Result<String> {
    let name = normalize_name(name)?;
    let mut stem = String::new();
    let mut replacing = false;
    for ch in name.chars() {
        let unsafe_char = matches!(ch, '/' | '\\' | '<' | '>' | ':' | '"' | '|' | '?' | '*');
        if unsafe_char {
            if !replacing && !stem.ends_with('-') {
                stem.push('-');
            }
            replacing = true;
        } else {
            stem.push(ch);
            replacing = false;
        }
    }
    let stem = stem.trim_matches(|ch: char| ch.is_whitespace() || ch == '.' || ch == '-');
    let mut stem = if stem.is_empty() {
        "group".to_owned()
    } else {
        stem.to_owned()
    };
    if stem.starts_with('.') || windows_device(&stem) {
        stem = format!("group-{stem}");
    }
    if stem.len() > MAX_STEM_BYTES {
        let suffix = digest(&name, 12);
        stem = format!(
            "{}--{suffix}",
            truncate_utf8(&stem, MAX_STEM_BYTES - suffix.len() - 2)
        );
    }
    Ok(stem)
}

fn collision_key(stem: &str) -> String {
    stem.nfc().case_fold().collect()
}

/// Canonical filename stem for every group name in one store.
pub fn allocate<'a>(names: impl IntoIterator<Item = &'a str>) -> Result<BTreeMap<String, String>> {
    let mut normalized = BTreeSet::new();
    for name in names {
        let name = normalize_name(name)?;
        ensure!(
            normalized.insert(name.clone()),
            "duplicate group name: {name}"
        );
    }
    let bases: BTreeMap<_, _> = normalized
        .iter()
        .map(|name| Ok((name.clone(), safe_stem(name)?)))
        .collect::<Result<_>>()?;
    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (name, stem) in &bases {
        groups
            .entry(collision_key(stem))
            .or_default()
            .push(name.clone());
    }
    let mut candidates = Vec::new();
    for names in groups.values() {
        for name in names {
            candidates.push((name.clone(), names.len() > 1));
        }
    }
    candidates.sort();
    let mut out = BTreeMap::new();
    let mut used = BTreeSet::new();
    for (name, collides) in candidates {
        let base = &bases[&name];
        if !collides && used.insert(collision_key(base)) {
            out.insert(name, base.clone());
            continue;
        }
        let mut allocated = None;
        for digits in (8..=64).step_by(2) {
            let suffix = digest(&name, digits);
            let stem = format!(
                "{}--{suffix}",
                truncate_utf8(base, MAX_STEM_BYTES - suffix.len() - 2)
            );
            if used.insert(collision_key(&stem)) {
                allocated = Some(stem);
                break;
            }
        }
        if allocated.is_none() {
            let suffix = digest(&name, 64);
            for counter in 1..=used.len() + 1 {
                let tail = format!("--{suffix}-{counter}");
                let stem = format!(
                    "{}{}",
                    truncate_utf8(base, MAX_STEM_BYTES - tail.len()),
                    tail
                );
                if used.insert(collision_key(&stem)) {
                    allocated = Some(stem);
                    break;
                }
            }
        }
        out.insert(
            name,
            allocated.expect("used.len() + 1 fallback candidates guarantee a free stem"),
        );
    }
    Ok(out)
}
