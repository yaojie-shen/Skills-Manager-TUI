use anyhow::{Result, ensure};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use unicode_casefold::UnicodeCaseFold;
use unicode_normalization::UnicodeNormalization;

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
    hash.iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()[..digits]
        .to_owned()
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

pub fn safe_stem(name: &str) -> Result<String> {
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

pub fn collision_key(stem: &str) -> String {
    stem.nfc().case_fold().collect()
}

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
        let mut digits = 8;
        loop {
            let suffix = digest(&name, digits);
            let stem = format!(
                "{}--{suffix}",
                truncate_utf8(base, MAX_STEM_BYTES - suffix.len() - 2)
            );
            if used.insert(collision_key(&stem)) {
                out.insert(name, stem);
                break;
            }
            digits += 2;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readable_stems_and_collision_groups_are_stable() {
        assert_eq!(safe_stem(" 工作 / Rust ").unwrap(), "工作 - Rust");
        assert_eq!(safe_stem("CON").unwrap(), "group-CON");
        let names = allocate(["A/B", "A-B", "Work", "work"]).unwrap();
        assert!(names["A/B"].starts_with("A-B--"));
        assert!(names["A-B"].starts_with("A-B--"));
        assert!(names["Work"].starts_with("Work--"));
        assert!(names["work"].starts_with("work--"));
        assert_ne!(collision_key(&names["Work"]), collision_key(&names["work"]));
    }

    #[test]
    fn canonical_equivalents_collide() {
        let names = allocate(["é", "e\u{301}"]).unwrap_err();
        assert!(names.to_string().contains("duplicate group name"));
    }

    #[test]
    fn allocated_stems_are_globally_unique_even_against_digest_shaped_names() {
        let colliding = format!("A-B--{}", digest("A-B", 8));
        let names = allocate(["A/B", "A-B", colliding.as_str()]).unwrap();
        let keys: BTreeSet<_> = names.values().map(|stem| collision_key(stem)).collect();
        assert_eq!(keys.len(), 3);
        assert_ne!(names[&colliding], colliding);
    }
}
