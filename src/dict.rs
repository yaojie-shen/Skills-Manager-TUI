//! Bilingual term dictionaries used for query expansion (see `data/dict/SOURCES.md`).
//!
//! Each source is a separate table with its own weight, because the same
//! English word can carry an unrelated sense in each: `tee` is 受信任执行环境
//! in the technical table and 发球区 in the common one. Weights live in
//! `config.toml`, never in the data.
//!
//! Built-in tables are embedded gzip-compressed and decoded on first use; a
//! user table at `<root>/.skills-meta/dictionary.tsv` is loaded on top.

use crate::config::DictionaryWeights;
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::sync::{Arc, OnceLock};

const TECH_GZ: &[u8] = include_bytes!("../data/dict/tech.tsv.gz");
const COMMON_GZ: &[u8] = include_bytes!("../data/dict/common.tsv.gz");
pub const USER_FILE: &str = "dictionary.tsv";

/// Which table an expansion came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Microsoft Terminology Collection: software vocabulary, mostly one sense per word.
    Tech,
    /// ECDICT mid-frequency band: general vocabulary, weaker signal.
    Common,
    /// The user's own table, trusted most.
    User,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Tech => "tech",
            Source::Common => "common",
            Source::User => "user",
        }
    }
}

/// One lookup table, both directions.
#[derive(Debug, Default, Clone)]
pub struct Table {
    en2zh: HashMap<String, Vec<String>>,
    zh2en: HashMap<String, Vec<String>>,
    /// Longest Chinese key in chars, bounds substring lookups.
    max_zh_chars: usize,
}

impl Table {
    pub fn parse(text: &str) -> Table {
        let mut t = Table::default();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((en, zhs)) = line.split_once('\t') else {
                continue;
            };
            let en = en.trim().to_lowercase();
            if en.is_empty() {
                continue;
            }
            for zh in zhs.split('|').map(str::trim).filter(|z| !z.is_empty()) {
                t.insert(&en, zh);
            }
        }
        t
    }

    fn insert(&mut self, en: &str, zh: &str) {
        let e = self.en2zh.entry(en.to_string()).or_default();
        if !e.iter().any(|z| z == zh) {
            e.push(zh.to_string());
        }
        let z = self.zh2en.entry(zh.to_string()).or_default();
        if !z.iter().any(|x| x == en) {
            z.push(en.to_string());
        }
        self.max_zh_chars = self.max_zh_chars.max(zh.chars().count());
    }

    pub fn len(&self) -> usize {
        self.en2zh.len()
    }

    pub fn is_empty(&self) -> bool {
        self.en2zh.is_empty()
    }

    pub fn en_to_zh(&self, en: &str) -> &[String] {
        self.en2zh
            .get(&en.to_lowercase())
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    pub fn zh_to_en(&self, zh: &str) -> &[String] {
        self.zh2en.get(zh).map(|v| v.as_slice()).unwrap_or(&[])
    }
}

fn decode(gz: &[u8]) -> Table {
    let mut text = String::new();
    if flate2::read::GzDecoder::new(gz)
        .read_to_string(&mut text)
        .is_err()
    {
        return Table::default();
    }
    Table::parse(&text)
}

fn builtin(source: Source) -> &'static Arc<Table> {
    static TECH: OnceLock<Arc<Table>> = OnceLock::new();
    static COMMON: OnceLock<Arc<Table>> = OnceLock::new();
    match source {
        Source::Tech => TECH.get_or_init(|| Arc::new(decode(TECH_GZ))),
        Source::Common => COMMON.get_or_init(|| Arc::new(decode(COMMON_GZ))),
        Source::User => unreachable!("the user table is not built in"),
    }
}

/// The weighted set of tables a search runs against.
#[derive(Debug, Default, Clone)]
pub struct Dictionaries {
    entries: Vec<(Source, f32, Arc<Table>)>,
}

impl Dictionaries {
    /// Tables for a skills root: the built-ins with their configured weights,
    /// plus the user table when the file exists. A weight of zero disables a source.
    pub fn load(root: &Path, w: &DictionaryWeights) -> Dictionaries {
        let mut d = Dictionaries::default();
        for (source, weight) in [(Source::Tech, w.tech), (Source::Common, w.common)] {
            if weight > 0.0 {
                d.entries.push((source, weight, builtin(source).clone()));
            }
        }
        if w.user > 0.0
            && let Ok(text) = std::fs::read_to_string(crate::paths::meta_dir(root).join(USER_FILE))
        {
            let table = Table::parse(&text);
            if !table.is_empty() {
                d.entries.push((Source::User, w.user, Arc::new(table)));
            }
        }
        d
    }

    /// Build directly from tables, for tests and callers with their own data.
    pub fn from_tables(entries: Vec<(Source, f32, Table)>) -> Dictionaries {
        Dictionaries {
            entries: entries
                .into_iter()
                .map(|(source, weight, table)| (source, weight, Arc::new(table)))
                .collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.iter().all(|(_, _, t)| t.is_empty())
    }

    /// Total entries across all tables.
    pub fn len(&self) -> usize {
        self.entries.iter().map(|(_, _, t)| t.len()).sum()
    }

    pub fn sources(&self) -> impl Iterator<Item = (Source, f32, usize)> + '_ {
        self.entries.iter().map(|(s, w, t)| (*s, *w, t.len()))
    }

    /// Chinese equivalents of an English word, with the weight of the table
    /// they came from. The strongest table wins when several agree.
    pub fn en_to_zh(&self, en: &str) -> Vec<(&str, f32)> {
        let mut out: Vec<(&str, f32)> = Vec::new();
        for (_, weight, table) in &self.entries {
            for zh in table.en_to_zh(en) {
                match out.iter_mut().find(|(z, _)| *z == zh.as_str()) {
                    Some(slot) if slot.1 < *weight => slot.1 = *weight,
                    Some(_) => {}
                    None => out.push((zh.as_str(), *weight)),
                }
            }
        }
        out
    }

    /// Every dictionary term occurring inside a run of CJK characters, as
    /// `(matched substring, English equivalent, weight)`. Longest match first,
    /// so `打印机驱动` expands through 打印机 before 打印.
    pub fn zh_terms_in(&self, run: &str) -> Vec<(String, &str, f32)> {
        let chars: Vec<char> = run.chars().collect();
        let max_len = self
            .entries
            .iter()
            .map(|(_, _, t)| t.max_zh_chars)
            .max()
            .unwrap_or(0)
            .min(chars.len());
        let mut out: Vec<(String, &str, f32)> = Vec::new();
        for len in (2..=max_len).rev() {
            for start in 0..=chars.len() - len {
                let sub: String = chars[start..start + len].iter().collect();
                for (_, weight, table) in &self.entries {
                    for en in table.zh_to_en(&sub) {
                        match out
                            .iter_mut()
                            .find(|(s, e, _)| *s == sub && *e == en.as_str())
                        {
                            Some(slot) if slot.2 < *weight => slot.2 = *weight,
                            Some(_) => {}
                            None => out.push((sub.clone(), en.as_str(), *weight)),
                        }
                    }
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn weights() -> DictionaryWeights {
        DictionaryWeights::default()
    }

    #[test]
    fn reload_shares_builtins_but_reads_updated_user_entries() {
        let root = std::env::temp_dir().join(format!("skills-dict-reload-{}", std::process::id()));
        let meta = crate::paths::meta_dir(&root);
        std::fs::create_dir_all(&meta).unwrap();
        let path = meta.join(USER_FILE);
        std::fs::write(&path, "fixture-term\t旧词\n").unwrap();
        let weights = DictionaryWeights {
            user: 1.0,
            ..Default::default()
        };
        let before = Dictionaries::load(&root, &weights);
        std::fs::write(&path, "fixture-term\t新词\n").unwrap();
        let after = Dictionaries::load(&root, &weights);
        let table = |d: &Dictionaries, source| {
            d.entries
                .iter()
                .find(|(s, _, _)| *s == source)
                .unwrap()
                .2
                .clone()
        };
        assert!(Arc::ptr_eq(
            &table(&before, Source::Tech),
            &table(&after, Source::Tech)
        ));
        assert!(Arc::ptr_eq(
            &table(&before, Source::Common),
            &table(&after, Source::Common)
        ));
        assert_eq!(
            table(&before, Source::User).en_to_zh("fixture-term"),
            &["旧词"]
        );
        assert_eq!(
            table(&after, Source::User).en_to_zh("fixture-term"),
            &["新词"]
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn builtin_tables_load_and_cover_both_registers() {
        let tech = builtin(Source::Tech);
        let common = builtin(Source::Common);
        assert!(tech.len() > 20_000, "tech has {}", tech.len());
        assert!(common.len() > 20_000, "common has {}", common.len());
        // Technical vocabulary comes from the terminology table.
        assert!(tech.en_to_zh("printer").iter().any(|z| z == "打印机"));
        assert!(tech.zh_to_en("打印机").iter().any(|e| e == "printer"));
        // Everyday vocabulary that a terminology list would not carry.
        assert!(!common.en_to_zh("poem").is_empty());
        assert!(tech.en_to_zh("poem").is_empty());
        // ...and technical vocabulary a general dictionary would not.
        assert!(!tech.en_to_zh("symlink").is_empty());
        assert!(common.en_to_zh("symlink").is_empty());
    }

    #[test]
    fn stronger_source_wins_and_unrelated_senses_coexist() {
        let d = Dictionaries::from_tables(vec![
            (
                Source::Tech,
                0.7,
                Table::parse("tee\t受信任执行环境\nprinter\t打印机\n"),
            ),
            (
                Source::Common,
                0.4,
                Table::parse("tee\t发球区\nprinter\t打印机\n"),
            ),
        ]);
        let mut hits = d.en_to_zh("tee");
        hits.sort_by(|a, b| b.1.total_cmp(&a.1));
        assert_eq!(hits, [("受信任执行环境", 0.7), ("发球区", 0.4)]);
        // The same sense in both tables keeps the stronger weight, listed once.
        assert_eq!(d.en_to_zh("printer"), [("打印机", 0.7)]);
    }

    #[test]
    fn substring_lookup_prefers_longer_terms() {
        let d = Dictionaries::from_tables(vec![(
            Source::Tech,
            0.7,
            Table::parse("printer\t打印机\nprint\t打印\ndriver\t驱动\n"),
        )]);
        let found: Vec<&str> = d
            .zh_terms_in("打印机驱动")
            .into_iter()
            .map(|(_, en, _)| en)
            .collect();
        assert_eq!(found, ["printer", "print", "driver"]);
    }

    #[test]
    fn zero_weight_disables_a_source() {
        let root = std::env::temp_dir().join("skills-no-user-dict");
        let w = DictionaryWeights {
            tech: 0.7,
            common: 0.0,
            user: 0.0,
        };
        let d = Dictionaries::load(&root, &w);
        assert_eq!(d.sources().count(), 1);
        assert!(!d.en_to_zh("symlink").is_empty());
        assert!(d.en_to_zh("poem").is_empty(), "common table is off");
        let all = Dictionaries::load(&root, &weights());
        assert!(all.len() > d.len());
    }
}
