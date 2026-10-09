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
    fn adversarial_digest_shaped_names_exhaust_prefixes_without_panicking() {
        let hash = digest("A/B", 64);
        let mut names = vec!["A-B".to_owned(), "A/B".to_owned()];
        names.extend(
            (8..=64)
                .step_by(2)
                .map(|digits| format!("A-B--{}", &hash[..digits])),
        );

        let first = allocate(names.iter().map(String::as_str)).unwrap();
        let second = allocate(names.iter().rev().map(String::as_str)).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.len(), names.len());
        assert!(first.values().all(|stem| stem.len() <= MAX_STEM_BYTES));
        assert_eq!(
            first
                .values()
                .map(|stem| collision_key(stem))
                .collect::<BTreeSet<_>>()
                .len(),
            names.len()
        );
    }

    #[test]
    fn common_allocations_remain_canonical() {
        assert_eq!(
            allocate(["Work", "A/B", "A-B"]).unwrap(),
            BTreeMap::from([
                ("A-B".to_owned(), "A-B--77101aaa".to_owned()),
                ("A/B".to_owned(), "A-B--998d3ed8".to_owned()),
                ("Work".to_owned(), "Work".to_owned()),
            ])
        );
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
