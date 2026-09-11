//! Full-text search modelled on Obsidian's Omnisearch (MiniSearch under the
//! hood): tokenized inverted index, BM25F ranking with per-field weights,
//! prefix and typo-tolerant term expansion, CJK-aware tokenization, and
//! highlighted excerpts. The index is tiny (one document per skill) and is
//! refreshed after every scan, retaining the index when searchable text is unchanged.
//!
//! Query syntax: free words plus `tag:x`, `agent:y`, `status:z`, `source:w`
//! and `untagged` filters.

use crate::config::{FieldWeights, SearchConfig};
use crate::dict::Dictionaries;
use crate::reconcile::{DeployState, SkillRecord};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};

// ---- query ------------------------------------------------------------------

#[derive(Debug, Default, Clone)]
pub struct Query {
    pub text: String,
    pub tags: Vec<String>,
    pub agents: Vec<String>,
    pub statuses: Vec<String>,
    pub sources: Vec<String>,
    pub repositories: Vec<String>,
    pub untagged: bool,
}

impl Query {
    /// Parse `tag:foo agent:codex status:modified free text` into a query.
    pub fn parse(input: &str) -> Self {
        let mut q = Query::default();
        let mut free = Vec::new();
        for tok in input.split_whitespace() {
            if let Some(v) = tok.strip_prefix("tag:") {
                if v.is_empty() {
                    q.untagged = true;
                } else {
                    q.tags.push(v.to_lowercase());
                }
            } else if let Some(v) = tok.strip_prefix("agent:") {
                if !v.is_empty() {
                    q.agents.push(v.to_lowercase());
                }
            } else if let Some(v) = tok.strip_prefix("status:") {
                if !v.is_empty() {
                    q.statuses.push(v.to_lowercase());
                }
            } else if let Some(v) = tok.strip_prefix("source:") {
                if !v.is_empty() {
                    q.sources.push(v.to_lowercase());
                }
            } else if let Some(v) = tok.strip_prefix("repo:") {
                q.repositories.push(v.to_string());
            } else if tok == "untagged" {
                q.untagged = true;
            } else {
                free.push(tok);
            }
        }
        q.text = free.join(" ");
        q
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
            && self.tags.is_empty()
            && self.agents.is_empty()
            && self.statuses.is_empty()
            && self.sources.is_empty()
            && self.repositories.is_empty()
            && !self.untagged
    }

    fn filter(&self, r: &SkillRecord) -> bool {
        if self.untagged && !r.tags.is_empty() {
            return false;
        }
        for t in &self.tags {
            if !r.tags.iter().any(|x| x.to_lowercase() == *t) {
                return false;
            }
        }
        for a in &self.agents {
            match r.deploy.get(a) {
                Some(DeployState::Deployed) => {}
                _ => return false,
            }
        }
        if !self.statuses.is_empty() {
            let label = r.status.label().trim_end_matches('?');
            if !self.statuses.iter().any(|s| s == label) {
                return false;
            }
        }
        if !self.repositories.is_empty()
            && !self
                .repositories
                .iter()
                .any(|a| {
                    crate::repository::alias_of(&r.key) == Some(a.as_str())
                        || matches!(&r.source, Some(crate::meta::Source::Git { url, .. })
                            if crate::repository::source_name(url).is_some_and(|name| name.eq_ignore_ascii_case(a)))
                })
        {
            return false;
        }
        if !self.sources.is_empty() {
            let kind = r.source_kind();
            if !self.sources.iter().any(|s| s == kind) {
                return false;
            }
        }
        true
    }
}

// ---- tokenizer ----------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub term: String,
    /// Byte range in the source text.
    pub start: usize,
    pub end: usize,
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF
        | 0xAC00..=0xD7AF | 0x20000..=0x2FA1F)
}

/// Lowercased word tokens. Latin/digit runs become one token; every CJK
/// character becomes a token and every pair of adjacent CJK characters a
/// bigram token, which gives usable recall on Chinese without a dictionary.
pub fn tokenize(text: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut word_start: Option<usize> = None;
    let mut prev_cjk: Option<(char, usize)> = None;
    let flush = |out: &mut Vec<Token>, text: &str, start: usize, end: usize| {
        if end > start {
            out.push(Token {
                term: text[start..end].to_lowercase(),
                start,
                end,
            });
        }
    };
    for (i, c) in text.char_indices() {
        let clen = c.len_utf8();
        if is_cjk(c) {
            if let Some(s) = word_start.take() {
                flush(&mut out, text, s, i);
            }
            out.push(Token {
                term: c.to_lowercase().collect(),
                start: i,
                end: i + clen,
            });
            if let Some((p, ps)) = prev_cjk {
                let mut bigram = String::new();
                bigram.extend(p.to_lowercase());
                bigram.extend(c.to_lowercase());
                out.push(Token {
                    term: bigram,
                    start: ps,
                    end: i + clen,
                });
            }
            prev_cjk = Some((c, i));
        } else if c.is_alphanumeric() {
            prev_cjk = None;
            if word_start.is_none() {
                word_start = Some(i);
            }
        } else {
            prev_cjk = None;
            if let Some(s) = word_start.take() {
                flush(&mut out, text, s, i);
            }
        }
    }
    if let Some(s) = word_start {
        flush(&mut out, text, s, text.len());
    }
    out
}

// ---- index --------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Field {
    Name,
    Tag,
    Description,
    Note,
    Heading,
    Body,
}

impl Field {
    const ALL: [Field; 6] = [
        Field::Name,
        Field::Tag,
        Field::Description,
        Field::Note,
        Field::Heading,
        Field::Body,
    ];
    fn idx(self) -> usize {
        self as usize
    }
    pub fn label(self) -> &'static str {
        match self {
            Field::Name => "name",
            Field::Tag => "tag",
            Field::Description => "description",
            Field::Note => "note",
            Field::Heading => "heading",
            Field::Body => "body",
        }
    }
}

/// Shortest word that typo correction applies to, on both the query and the
/// candidate side. Below this, an edit of one is a different word.
const MIN_FUZZY_CHARS: usize = 3;

const K1: f32 = 1.2;
const B: f32 = 0.75;

/// Term frequency of one term inside one document, per field.
#[derive(Debug, Default, Clone)]
struct Posting {
    doc: usize,
    tf: [u16; 6],
}

#[derive(Debug, Default, Clone)]
struct Doc {
    /// Tokens per field (for length normalization).
    len: [u32; 6],
    names: Vec<String>,
    name_terms: Vec<String>,
}

/// Inverted index over a set of skill records.
#[derive(Debug, Default, Clone)]
pub struct Index {
    docs: Vec<Doc>,
    postings: HashMap<String, Vec<Posting>>,
    avg_len: [f32; 6],
    /// Sorted vocabulary for prefix / fuzzy expansion.
    vocab: Vec<String>,
    weights: [f32; 6],
}

impl Index {
    pub fn build(records: &[SkillRecord], w: &FieldWeights) -> Self {
        let weights = [w.name, w.tag, w.description, w.note, w.heading, w.body];
        let mut docs = Vec::with_capacity(records.len());
        let mut postings: HashMap<String, Vec<Posting>> = HashMap::new();
        let mut totals = [0u64; 6];
        for (doc_id, r) in records.iter().enumerate() {
            let mut doc = Doc::default();
            doc.names
                .push(r.key.rsplit('/').next().unwrap_or(&r.key).to_lowercase());
            if let Some(name) = &r.name {
                doc.names.push(name.to_lowercase());
            }
            doc.name_terms = doc
                .names
                .iter()
                .flat_map(|name| tokenize(name).into_iter().map(|t| t.term))
                .collect();
            let mut tf: HashMap<String, [u16; 6]> = HashMap::new();
            let mut add = |field: Field, text: &str, doc: &mut Doc| {
                for t in tokenize(text) {
                    doc.len[field.idx()] += 1;
                    let e = tf.entry(t.term).or_default();
                    e[field.idx()] = e[field.idx()].saturating_add(1);
                }
            };
            if let Some(name) = &r.name {
                add(Field::Name, name, &mut doc);
                if name != &r.key {
                    add(Field::Body, &r.key, &mut doc);
                }
            } else {
                add(Field::Name, &r.key, &mut doc);
            }
            for t in &r.tags {
                add(Field::Tag, t, &mut doc);
            }
            if let Some(d) = &r.description {
                add(Field::Description, d, &mut doc);
            }
            if let Some(n) = &r.note {
                add(Field::Note, n, &mut doc);
            }
            if let Some(b) = &r.body {
                for line in b.lines() {
                    if let Some(h) = line.trim_start().strip_prefix('#') {
                        add(Field::Heading, h.trim_start_matches('#'), &mut doc);
                    }
                }
                add(Field::Body, b, &mut doc);
            }
            for f in Field::ALL {
                totals[f.idx()] += doc.len[f.idx()] as u64;
            }
            for (term, counts) in tf {
                postings.entry(term).or_default().push(Posting {
                    doc: doc_id,
                    tf: counts,
                });
            }
            docs.push(doc);
        }
        let n = records.len().max(1) as f32;
        let mut avg_len = [0f32; 6];
        for f in Field::ALL {
            avg_len[f.idx()] = (totals[f.idx()] as f32 / n).max(1.0);
        }
        let mut vocab: Vec<String> = postings.keys().cloned().collect();
        vocab.sort();
        Self {
            docs,
            postings,
            avg_len,
            vocab,
            weights,
        }
    }

    fn idf(&self, term: &str) -> f32 {
        let n = self.docs.len() as f32;
        let df = self.postings.get(term).map(|p| p.len()).unwrap_or(0) as f32;
        ((n - df + 0.5) / (df + 0.5) + 1.0).ln()
    }

    /// Expand one Latin query word into `(index term, weight)` pairs: exact,
    /// prefix, and typo-tolerant matches.
    fn expand(&self, q: &str, cfg: &SearchConfig) -> Vec<(String, f32)> {
        let mut out: BTreeMap<String, f32> = BTreeMap::new();
        if self.postings.contains_key(q) {
            out.insert(q.to_string(), 1.0);
        }
        let qchars = q.chars().count();
        // A single character expands too, only more cheaply: `number 0` has to
        // reach `number 01`, and letting the last word fail would fail the
        // whole query, since every unit must match. The vocabulary of a skills
        // root is small enough that the wide fan-out costs nothing.
        if cfg.prefix && qchars >= 1 {
            let start = self.vocab.partition_point(|v| v.as_str() < q);
            for v in self.vocab[start..].iter().take_while(|v| v.starts_with(q)) {
                if v != q {
                    let extra = v.chars().count() - qchars;
                    let base = if qchars == 1 { 0.6 } else { 0.85 };
                    let w = (base - 0.05 * extra as f32).max(0.4);
                    out.entry(v.clone()).or_insert(w);
                }
            }
        }
        // Elasticsearch's AUTO fuzziness, which starts correcting at three
        // characters: `vis` should still reach `viz`.
        let max_dist = if !cfg.fuzzy || q.chars().any(is_cjk) {
            0
        } else if qchars >= 8 {
            2
        } else if qchars >= MIN_FUZZY_CHARS {
            1
        } else {
            0
        };
        if max_dist > 0 {
            for v in &self.vocab {
                if out.contains_key(v) {
                    continue;
                }
                let vc = v.chars().count();
                if vc.abs_diff(qchars) > max_dist || v.chars().any(is_cjk) {
                    continue;
                }
                // A candidate shorter than the floor is a different word, not a
                // typo of one: `git` must not correct to `it`, nor `tos` to `to`.
                if vc < MIN_FUZZY_CHARS {
                    continue;
                }
                let d = damerau_levenshtein(q, v, max_dist);
                if d <= max_dist {
                    out.insert(v.clone(), if d == 1 { 0.35 } else { 0.2 });
                }
            }
        }
        out.into_iter().collect()
    }

    /// BM25F contribution of one index term for every document containing it.
    fn term_scores(&self, term: &str) -> HashMap<usize, (f32, Vec<Field>)> {
        let mut out = HashMap::new();
        let Some(postings) = self.postings.get(term) else {
            return out;
        };
        let idf = self.idf(term);
        for p in postings {
            let doc = &self.docs[p.doc];
            let mut tf = 0f32;
            let mut fields = Vec::new();
            for f in Field::ALL {
                let c = p.tf[f.idx()];
                if c > 0 {
                    let norm = 1.0 - B + B * doc.len[f.idx()] as f32 / self.avg_len[f.idx()];
                    tf += self.weights[f.idx()] * c as f32 / norm;
                    fields.push(f);
                }
            }
            out.insert(p.doc, (idf * tf * (K1 + 1.0) / (tf + K1), fields));
        }
        out
    }

    /// Turn the query into units. Each unit must match; a unit matches a
    /// document through any of its alternatives, each an AND of index terms.
    fn units(&self, text: &str, cfg: &SearchConfig, dict: &Dictionaries) -> Vec<Unit> {
        let mut units = Vec::new();
        for seg in segments(text) {
            let mut alts = Vec::new();
            match &seg {
                Seg::Word(w) => {
                    for (term, weight) in self.expand(w, cfg) {
                        alts.push(Alternative {
                            terms: vec![term],
                            weight,
                        });
                    }
                    if cfg.dictionary {
                        for (zh, weight) in dict.en_to_zh(w) {
                            let terms = phrase_terms(zh);
                            if !terms.is_empty()
                                && terms.iter().all(|t| self.postings.contains_key(t))
                            {
                                alts.push(Alternative { terms, weight });
                            }
                        }
                    }
                }
                Seg::Cjk(run) => {
                    let own = phrase_terms(run);
                    if own.iter().all(|t| self.postings.contains_key(t)) {
                        alts.push(Alternative {
                            terms: own,
                            weight: 1.0,
                        });
                    }
                    if cfg.dictionary {
                        let run_len = run.chars().count() as f32;
                        for (sub, en, weight) in dict.zh_terms_in(run) {
                            let rest: String = run.replacen(&sub, " ", 1);
                            let cover = sub.chars().count() as f32 / run_len;
                            let mut terms = phrase_terms(en);
                            terms.extend(phrase_terms(&rest));
                            if !terms.is_empty()
                                && terms.iter().all(|t| self.postings.contains_key(t))
                            {
                                alts.push(Alternative {
                                    terms,
                                    weight: weight * cover.max(0.5),
                                });
                            }
                        }
                    }
                }
            }
            units.push(Unit { alts });
        }
        units
    }

    /// Search the index. Every query unit must match somewhere; BM25F over
    /// the weighted fields decides the order.
    pub fn query(&self, text: &str, cfg: &SearchConfig, dict: &Dictionaries) -> Vec<RawHit> {
        let units = self.units(text, cfg, dict);
        if units.is_empty() {
            return Vec::new();
        }
        let mut cache: HashMap<String, HashMap<usize, (f32, Vec<Field>)>> = HashMap::new();
        let mut scores: HashMap<usize, RawHit> = HashMap::new();
        for unit in &units {
            // Best alternative per document for this unit.
            let mut best: HashMap<usize, (f32, Vec<Field>, Vec<String>)> = HashMap::new();
            for alt in &unit.alts {
                let mut acc: Option<HashMap<usize, (f32, Vec<Field>)>> = None;
                for term in &alt.terms {
                    let ts = cache
                        .entry(term.clone())
                        .or_insert_with(|| self.term_scores(term));
                    acc = Some(match acc {
                        None => ts.clone(),
                        Some(prev) => prev
                            .into_iter()
                            .filter_map(|(doc, (s, mut f))| {
                                ts.get(&doc).map(|(s2, f2)| {
                                    for x in f2 {
                                        if !f.contains(x) {
                                            f.push(*x);
                                        }
                                    }
                                    (doc, (s + s2, f))
                                })
                            })
                            .collect(),
                    });
                }
                for (doc, (s, fields)) in acc.unwrap_or_default() {
                    let s = s * alt.weight;
                    let e = best.entry(doc).or_insert((0.0, Vec::new(), Vec::new()));
                    if s > e.0 {
                        e.0 = s;
                        e.1 = fields;
                        e.2 = alt.terms.clone();
                    }
                }
            }
            for (doc, (s, fields, terms)) in best {
                let h = scores.entry(doc).or_insert_with(|| RawHit {
                    doc,
                    score: 0.0,
                    matched_words: 0,
                    fields: Vec::new(),
                    terms: Vec::new(),
                });
                h.score += s;
                h.matched_words += 1;
                for f in fields {
                    if !h.fields.contains(&f) {
                        h.fields.push(f);
                    }
                }
                for t in terms {
                    if !h.terms.contains(&t) {
                        h.terms.push(t);
                    }
                }
            }
        }
        let total = units.len();
        let mut hits: Vec<RawHit> = scores
            .into_values()
            .filter(|h| h.matched_words == total)
            .collect();
        // Literal name matches must not lose to rare typo expansions or long
        // bodies. BM25F still ranks results within each relevance tier.
        let literal = phrase_terms(text);
        let normalized = text.trim().to_lowercase();
        let relevance = |hit: &RawHit| {
            let doc = &self.docs[hit.doc];
            let name_count = literal
                .iter()
                .filter(|term| doc.name_terms.contains(term))
                .count();
            let literal_count = literal
                .iter()
                .filter(|term| {
                    self.postings
                        .get(*term)
                        .is_some_and(|ps| ps.binary_search_by_key(&hit.doc, |p| p.doc).is_ok())
                })
                .count();
            (
                doc.names.contains(&normalized),
                name_count == literal.len(),
                literal_count == literal.len(),
                name_count,
                literal_count,
            )
        };
        let relevance: HashMap<_, _> = hits.iter().map(|hit| (hit.doc, relevance(hit))).collect();
        hits.sort_by(|a, b| {
            relevance[&b.doc].cmp(&relevance[&a.doc]).then_with(|| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(a.doc.cmp(&b.doc))
            })
        });
        for h in hits.iter_mut() {
            h.fields.sort();
        }
        hits
    }
}

/// One query word or CJK run.
enum Seg {
    Word(String),
    Cjk(String),
}

/// Split a query into lowercase Latin words and runs of CJK characters.
fn segments(text: &str) -> Vec<Seg> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut run = String::new();
    for c in text.chars() {
        if is_cjk(c) {
            if !word.is_empty() {
                out.push(Seg::Word(std::mem::take(&mut word).to_lowercase()));
            }
            run.push(c);
        } else {
            if !run.is_empty() {
                out.push(Seg::Cjk(std::mem::take(&mut run)));
            }
            if c.is_alphanumeric() {
                word.push(c);
            } else if !word.is_empty() {
                out.push(Seg::Word(std::mem::take(&mut word).to_lowercase()));
            }
        }
    }
    if !word.is_empty() {
        out.push(Seg::Word(word.to_lowercase()));
    }
    if !run.is_empty() {
        out.push(Seg::Cjk(run));
    }
    out
}

/// Index terms a phrase must contribute: Latin words, and for each CJK run its
/// bigrams (or the single character when the run has only one).
fn phrase_terms(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for seg in segments(text) {
        match seg {
            Seg::Word(w) => out.push(w),
            Seg::Cjk(run) => {
                let chars: Vec<char> = run.chars().collect();
                if chars.len() == 1 {
                    out.push(run.to_lowercase());
                } else {
                    for pair in chars.windows(2) {
                        out.push(pair.iter().collect::<String>().to_lowercase());
                    }
                }
            }
        }
    }
    out.dedup();
    out
}

struct Alternative {
    terms: Vec<String>,
    weight: f32,
}

struct Unit {
    alts: Vec<Alternative>,
}

#[derive(Debug, Clone)]
pub struct RawHit {
    pub doc: usize,
    pub score: f32,
    pub matched_words: usize,
    pub fields: Vec<Field>,
    /// Index terms that matched (after expansion), used for highlighting.
    pub terms: Vec<String>,
}

/// Restricted Damerau-Levenshtein distance with early exit above `max`.
fn damerau_levenshtein(a: &str, b: &str, max: usize) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (n, m) = (a.len(), b.len());
    if n.abs_diff(m) > max {
        return max + 1;
    }
    let mut prev2: Vec<usize> = Vec::new();
    let mut prev: Vec<usize> = (0..=m).collect();
    for i in 1..=n {
        let mut cur = vec![i; m + 1];
        let mut row_min = usize::MAX;
        for j in 1..=m {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            let mut v = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                v = v.min(prev2[j - 2] + 1);
            }
            cur[j] = v;
            row_min = row_min.min(v);
        }
        if row_min > max {
            return max + 1;
        }
        prev2 = std::mem::replace(&mut prev, cur);
    }
    prev[m]
}

// ---- public search API ----------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct Hit {
    pub index: usize,
    pub score: f32,
    /// Fields that contained a match, ordered by weight (best first).
    pub fields: Vec<Field>,
    /// Matched index terms for highlighting.
    pub terms: Vec<String>,
    /// Excerpt around the first body/description match, with highlight ranges.
    pub excerpt: Option<Excerpt>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Excerpt {
    pub field: Field,
    pub text: String,
    /// Byte ranges into `text` that matched.
    pub ranges: Vec<(usize, usize)>,
}

/// Owns an index over the last set of records and answers queries against it.
/// Exact copies of indexed fields let deployment-only refreshes retain the
/// inverted index. Filters still read live records, including agent status.
#[derive(Debug, Clone)]
struct IndexedText {
    key: String,
    name: Option<String>,
    tags: Vec<String>,
    description: Option<String>,
    note: Option<String>,
    body: Option<String>,
}
impl IndexedText {
    fn from_record(r: &SkillRecord) -> Self {
        Self {
            key: r.key.clone(),
            name: r.name.clone(),
            tags: r.tags.clone(),
            description: r.description.clone(),
            note: r.note.clone(),
            body: r.body.clone(),
        }
    }
    fn matches(&self, r: &SkillRecord) -> bool {
        self.key == r.key
            && self.name == r.name
            && self.tags == r.tags
            && self.description == r.description
            && self.note == r.note
            && self.body == r.body
    }
}

#[derive(Debug, Default)]
pub struct Searcher {
    index: Index,
    indexed_text: Vec<IndexedText>,
    len: usize,
    cfg: SearchConfig,
    dict: Dictionaries,
}

impl Searcher {
    pub fn new() -> Self {
        Self::default()
    }

    /// Searcher for a workspace: its `[search]` config plus the built-in and user dictionaries.
    pub fn for_workspace(ws: &crate::Workspace) -> Self {
        let mut s = Self::new();
        let dicts = Dictionaries::load(&ws.root, &ws.config.search.dictionaries);
        s.configure(ws.config.search.clone(), dicts);
        s
    }

    pub fn configure(&mut self, cfg: SearchConfig, dict: Dictionaries) {
        let reindex = cfg.weights != self.cfg.weights;
        self.cfg = cfg;
        self.dict = dict;
        if reindex {
            self.len = usize::MAX;
        }
    }

    /// Refresh after a scan. Rebuild only when indexed text, ordering, or weights
    /// changed; deployment and health filters do not require tokenizing again.
    pub fn index(&mut self, records: &[SkillRecord]) {
        if self.len == records.len()
            && self.indexed_text.len() == records.len()
            && self
                .indexed_text
                .iter()
                .zip(records)
                .all(|(text, record)| text.matches(record))
        {
            return;
        }
        self.index = Index::build(records, &self.cfg.weights);
        self.indexed_text = records.iter().map(IndexedText::from_record).collect();
        self.len = records.len();
    }

    /// Search `records` with `query`. Rebuilds the index when it does not
    /// match the given records (CLI use); the TUI indexes once per scan.
    pub fn search(&mut self, records: &[SkillRecord], query: &Query) -> Vec<Hit> {
        if self.len != records.len() || (self.index.docs.is_empty() && !records.is_empty()) {
            self.index(records);
        }
        let allowed: Vec<bool> = records.iter().map(|r| query.filter(r)).collect();
        if query.text.trim().is_empty() {
            return (0..records.len())
                .filter(|i| allowed[*i])
                .map(|index| Hit {
                    index,
                    score: 0.0,
                    fields: Vec::new(),
                    terms: Vec::new(),
                    excerpt: None,
                })
                .collect();
        }
        self.index
            .query(&query.text, &self.cfg, &self.dict)
            .into_iter()
            .filter(|h| allowed[h.doc])
            .map(|h| {
                let excerpt = excerpt_for(&records[h.doc], &h.terms, &h.fields);
                Hit {
                    index: h.doc,
                    score: h.score,
                    fields: h.fields,
                    terms: h.terms,
                    excerpt,
                }
            })
            .collect()
    }
}

/// Byte ranges in `text` whose tokens equal one of `terms` (case-insensitive),
/// merged when overlapping. Used for highlighting.
pub fn highlight_ranges(text: &str, terms: &[String]) -> Vec<(usize, usize)> {
    if terms.is_empty() {
        return Vec::new();
    }
    let set: HashSet<&str> = terms.iter().map(|s| s.as_str()).collect();
    let mut ranges: Vec<(usize, usize)> = tokenize(text)
        .into_iter()
        .filter(|t| set.contains(t.term.as_str()))
        .map(|t| (t.start, t.end))
        .collect();
    ranges.sort();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (s, e) in ranges {
        if let Some(last) = merged.last_mut()
            && s <= last.1
        {
            last.1 = last.1.max(e);
        } else {
            merged.push((s, e));
        }
    }
    merged
}

/// Build a short excerpt around the first match in the body (or description
/// or note when the body has none).
fn excerpt_for(r: &SkillRecord, terms: &[String], fields: &[Field]) -> Option<Excerpt> {
    let candidates: [(Field, Option<&str>); 3] = [
        (Field::Body, r.body.as_deref()),
        (Field::Description, r.description.as_deref()),
        (Field::Note, r.note.as_deref()),
    ];
    for (field, text) in candidates {
        if !fields.contains(&field) {
            continue;
        }
        let Some(text) = text else { continue };
        let ranges = highlight_ranges(text, terms);
        let Some(&(first, first_end)) = ranges.first() else {
            continue;
        };
        let start = excerpt_start(text, first);
        let end = excerpt_end(text, start, first_end);
        let slice = &text[start..end];
        let collapsed: String = slice.split_whitespace().collect::<Vec<_>>().join(" ");
        let local = highlight_ranges(&collapsed, terms);
        let prefix = if start > 0 { "…" } else { "" };
        let suffix = if end < text.len() { "…" } else { "" };
        let shift = prefix.len();
        let text_out = format!("{prefix}{collapsed}{suffix}");
        let ranges = local
            .into_iter()
            .map(|(s, e)| (s + shift, e + shift))
            .collect();
        return Some(Excerpt {
            field,
            text: text_out,
            ranges,
        });
    }
    None
}

// Avoid beginning or ending with a fragment that looks like a different
// word. CJK has no spaces between words, so punctuation is a more useful
// context boundary; without one, keep the hit instead of inventing a cut.
fn excerpt_start(text: &str, first: usize) -> usize {
    let mut start = back_chars(text, first, 40);
    if start == 0 {
        return start;
    }
    let previous = text[..start].chars().next_back().unwrap();
    let current = text[start..].chars().next().unwrap();
    if is_cjk(previous) && is_cjk(current) {
        return text[start..first]
            .char_indices()
            .rev()
            .find(|(_, c)| excerpt_separator(*c))
            .map_or(first, |(i, c)| start + i + c.len_utf8());
    }
    if excerpt_word(previous) && excerpt_word(current) {
        while let Some((i, c)) = text[..start].char_indices().next_back() {
            if !excerpt_word(c) {
                break;
            }
            start = i;
        }
    }
    start
}

fn excerpt_end(text: &str, start: usize, first_end: usize) -> usize {
    let mut end = fwd_chars(text, start, 140).max(first_end);
    if end == text.len() {
        return end;
    }
    let previous = text[..end].chars().next_back().unwrap();
    let current = text[end..].chars().next().unwrap();
    if is_cjk(previous) && is_cjk(current) {
        return text[first_end..end]
            .char_indices()
            .rev()
            .find(|(_, c)| excerpt_separator(*c))
            .map_or(first_end, |(i, c)| first_end + i + c.len_utf8());
    }
    if excerpt_word(previous) && excerpt_word(current) {
        for c in text[end..].chars() {
            if !excerpt_word(c) {
                break;
            }
            end += c.len_utf8();
        }
    }
    end
}

fn excerpt_word(c: char) -> bool {
    !is_cjk(c) && (c.is_alphanumeric() || c == '_' || c == '\'')
}

fn excerpt_separator(c: char) -> bool {
    c.is_whitespace()
        || c.is_ascii_punctuation()
        || matches!(c, '，' | '。' | '；' | '：' | '！' | '？' | '、')
}

fn back_chars(text: &str, from: usize, n: usize) -> usize {
    let mut i = from;
    for _ in 0..n {
        match text[..i].char_indices().next_back() {
            Some((p, _)) => i = p,
            None => break,
        }
    }
    // Prefer starting at a line boundary if one is close.
    if let Some(nl) = text[i..from].rfind('\n') {
        return i + nl + 1;
    }
    i
}

fn fwd_chars(text: &str, from: usize, n: usize) -> usize {
    let mut i = from;
    for (k, (p, c)) in text[from..].char_indices().enumerate() {
        if k >= n {
            break;
        }
        i = from + p + c.len_utf8();
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reconcile::SkillStatus;
    use std::collections::BTreeMap;

    fn rec(key: &str, desc: &str, body: &str, tags: &[&str]) -> SkillRecord {
        SkillRecord {
            key: key.into(),
            path: key.into(),
            status: SkillStatus::MissingSource,
            name: Some(key.into()),
            description: Some(desc.into()),
            body: Some(body.into()),
            external: false,
            name_mismatch: false,
            tags: tags.iter().map(|t| t.to_string()).collect(),
            note: None,
            source: None,
            current_hash: None,
            baseline_hash: None,
            deploy: BTreeMap::new(),
            meta: None,
        }
    }

    #[test]
    fn declared_name_outweighs_directory_alias() {
        let mut records = vec![rec("needle", "", "", &[]), rec("stored-alias", "", "", &[])];
        records[0].name = Some("unrelated".into());
        records[1].name = Some("needle".into());
        let mut searcher = Searcher::new();
        let hits = searcher.search(&records, &Query::parse("needle"));
        assert_eq!(keys(&records, &hits), ["stored-alias", "needle"]);
    }

    #[test]
    fn refreshing_an_index_keeps_deployment_filters_live_and_observes_text_edits() {
        let mut records = vec![
            rec("alpha", "needle", "", &[]),
            rec("beta", "needle", "", &[]),
        ];
        records[0]
            .deploy
            .insert("test-agent".into(), DeployState::Deployed);
        let mut searcher = Searcher::new();
        searcher.index(&records);
        let query = Query::parse("needle agent:test-agent");
        assert_eq!(
            keys(&records, &searcher.search(&records, &query)),
            ["alpha"]
        );
        records[0]
            .deploy
            .insert("test-agent".into(), DeployState::NotDeployed);
        records[1]
            .deploy
            .insert("test-agent".into(), DeployState::Deployed);
        searcher.index(&records);
        assert_eq!(keys(&records, &searcher.search(&records, &query)), ["beta"]);
        records[0].body = Some("uniqueupdatedbody".into());
        records[1].tags = vec!["uniquenewtag".into()];
        searcher.index(&records);
        assert_eq!(
            keys(
                &records,
                &searcher.search(&records, &Query::parse("uniqueupdatedbody"))
            ),
            ["alpha"]
        );
        assert_eq!(
            keys(
                &records,
                &searcher.search(&records, &Query::parse("uniquenewtag"))
            ),
            ["beta"]
        );
        records.reverse();
        searcher.index(&records);
        assert_eq!(
            keys(
                &records,
                &searcher.search(&records, &Query::parse("uniqueupdatedbody"))
            ),
            ["alpha"]
        );
    }

    #[test]
    fn repository_filter_uses_source_identity_not_storage_alias() {
        let mut r = rec("repos/custom-label/skills--approval", "", "", &[]);
        for url in [
            "https://github.com/sampleorg/kit.git",
            "git@github.com:sampleorg/kit.git",
            "ssh://git@github.com/sampleorg/kit",
        ] {
            r.source = Some(crate::meta::Source::Git {
                url: url.into(),
                branch: None,
                subpath: None,
                revision: None,
            });
            assert!(Query::parse("repo:sampleorg/kit").filter(&r));
            assert!(Query::parse("repo:custom-label").filter(&r));
            assert!(!Query::parse("repo:other/cli").filter(&r));
        }
        r.key = "old-flat-install".into();
        assert!(Query::parse("repo:sampleorg/kit").filter(&r));
    }

    #[test]
    fn source_filters_classify_local_and_repository_independently_of_problems() {
        let mut r = rec("review", "", "", &[]);
        assert!(Query::parse("source:local").filter(&r));
        assert!(!Query::parse("source:repository").filter(&r));
        r.source = Some(crate::meta::Source::Git {
            url: "https://github.com/example/repo".into(),
            branch: None,
            subpath: None,
            revision: None,
        });
        for status in [
            SkillStatus::Repository,
            SkillStatus::Modified,
            SkillStatus::MissingBaseline,
        ] {
            r.status = status;
            assert!(Query::parse("source:repository").filter(&r));
            assert!(!Query::parse("source:local").filter(&r));
        }
    }

    /// One technical table at full weight, for tests that only need a mapping.
    fn dict(tsv: &str) -> Dictionaries {
        Dictionaries::from_tables(vec![(
            crate::dict::Source::Tech,
            0.7,
            crate::dict::Table::parse(tsv),
        )])
    }

    fn keys(records: &[SkillRecord], hits: &[Hit]) -> Vec<String> {
        hits.iter().map(|h| records[h.index].key.clone()).collect()
    }

    #[test]
    fn tokenizes_latin_and_cjk() {
        let t: Vec<String> = tokenize("Etcd-Msgpack reader 文件系统")
            .into_iter()
            .map(|t| t.term)
            .collect();
        assert_eq!(
            t,
            [
                "etcd", "msgpack", "reader", "文", "件", "文件", "系", "件系", "统", "系统"
            ]
        );
    }

    #[test]
    fn name_beats_tag_beats_description_beats_body() {
        let records = vec![
            rec("alpha", "about msgpack files", "nothing", &[]),
            rec("msgpack-reader", "reads tables", "nothing", &[]),
            rec(
                "gamma",
                "misc",
                "the body mentions msgpack here and msgpack there",
                &[],
            ),
            rec("delta", "misc", "unrelated", &["msgpack"]),
        ];
        let hits = Searcher::new().search(&records, &Query::parse("msgpack"));
        assert_eq!(
            keys(&records, &hits),
            ["msgpack-reader", "delta", "alpha", "gamma"]
        );
        assert_eq!(hits[3].fields, vec![Field::Body]);
        assert!(hits[3].excerpt.as_ref().unwrap().text.contains("msgpack"));
    }

    #[test]
    fn every_word_must_match_and_prefix_works() {
        let records = vec![
            rec("x", "etcd cluster and msgpack", "", &[]),
            rec("y", "only etcd", "", &[]),
        ];
        let s = &mut Searcher::new();
        assert_eq!(
            keys(&records, &s.search(&records, &Query::parse("etcd msgp"))),
            ["x"]
        );
        assert_eq!(s.search(&records, &Query::parse("etcd")).len(), 2);
        assert!(s.search(&records, &Query::parse("etcd nothing")).is_empty());
    }

    #[test]
    fn a_single_character_still_matches_by_prefix() {
        let recs = vec![
            rec("counter", "number 01 in a long list", "", &[]),
            rec("other", "nothing here", "", &[]),
        ];
        let mut s = Searcher::new();
        s.index(&recs);
        let hits = s.search(&recs, &Query::parse("number 0"));
        assert_eq!(
            hits.len(),
            1,
            "the trailing 0 must reach 01, not sink the query"
        );
        assert_eq!(recs[hits[0].index].key, "counter");
        let hits = s.search(&recs, &Query::parse("0"));
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn paper_name_matches_outrank_body_prefix_and_typo_results() {
        let mut target = rec("repos/author--tools/research-paper-writing", "", "", &[]);
        target.name = Some("research-paper-writing".into());
        let mut records = vec![
            rec("pager", "pager", &"pager ".repeat(100), &[]),
            rec("paperwork", "", "", &[]),
            rec("body-only", "", &"paper ".repeat(100), &[]),
            target,
        ];
        // Common literal terms must still beat rare corrected terms (IDF).
        for i in 0..20 {
            records.push(rec(&format!("other-{i}"), "paper", "", &[]));
        }
        let mut searcher = Searcher::new();
        let hits = searcher.search(&records, &Query::parse("paper"));
        assert_eq!(hits[0].index, 3);
        let body = hits.iter().position(|h| h.index == 2).unwrap();
        let typo = hits.iter().position(|h| h.index == 0).unwrap();
        assert!(body < typo, "literal body match precedes corrected name");
        assert!(hits[0].terms.contains(&"paper".into()));
        assert_eq!(
            searcher.search(&records, &Query::parse("research-paper-writing"))[0].index,
            3
        );
        assert_eq!(
            searcher.search(&records, &Query::parse("reserach paper"))[0].index,
            3
        );
        searcher.cfg.fuzzy = false;
        assert!(
            !searcher
                .search(&records, &Query::parse("paper"))
                .iter()
                .any(|h| h.index == 0)
        );
    }

    #[test]
    fn typo_tolerance() {
        let records = vec![rec("zephyr-job-workflows", "manage jobs", "", &[])];
        let hits = Searcher::new().search(&records, &Query::parse("zepyhr"));
        assert_eq!(hits.len(), 1);
        assert!(hits[0].terms.contains(&"zephyr".to_string()));
    }

    #[test]
    fn typo_tolerance_starts_at_three_chars_and_ignores_shorter_words() {
        let records = vec![
            rec("viz-tool", "renders charts", "", &[]),
            rec("it-helper", "it does it", "", &[]),
        ];
        let mut s = Searcher::new();
        // Three characters is enough to reach a three-character word.
        assert_eq!(
            keys(&records, &s.search(&records, &Query::parse("vis"))),
            ["viz-tool"]
        );
        // ...but not to reach a shorter one: `git` is not a typo of `it`.
        assert!(s.search(&records, &Query::parse("git")).is_empty());
        // Two characters still correct nothing. `vi` would reach `viz` by
        // prefix, so use a query that only an edit could bridge.
        assert!(s.search(&records, &Query::parse("vz")).is_empty());
    }

    #[test]
    fn cjk_query_matches_body_and_highlights() {
        let records = vec![
            rec("x", "d", "用于文件系统的工具", &[]),
            rec("y", "d", "网络设置", &[]),
        ];
        let hits = Searcher::new().search(&records, &Query::parse("文件"));
        assert_eq!(keys(&records, &hits), ["x"]);
        let ex = hits[0].excerpt.as_ref().unwrap();
        assert_eq!(&ex.text[ex.ranges[0].0..ex.ranges[0].1], "文件");
    }

    #[test]
    fn excerpts_keep_latin_words_whole_and_highlight_the_hit() {
        let body = format!(
            "Older context. {}printer {}",
            "configuration ".repeat(5),
            "documentation ".repeat(20),
        );
        let r = rec("tool", "", &body, &[]);
        let ex = excerpt_for(&r, &["printer".into()], &[Field::Body]).unwrap();
        assert!(ex.text.starts_with("…configuration "));
        assert!(ex.text.ends_with("documentation…"));
        assert!(
            ex.text
                .trim_matches('…')
                .split_whitespace()
                .all(|word| matches!(word, "configuration" | "printer" | "documentation"))
        );
        assert_eq!(&ex.text[ex.ranges[0].0..ex.ranges[0].1], "printer");
    }

    #[test]
    fn cjk_excerpts_prefer_punctuation_or_the_match_to_arbitrary_cuts() {
        let body = format!(
            "{}。用于文件系统的工具，{}。{}",
            "背景".repeat(35),
            "说明".repeat(35),
            "后文".repeat(90)
        );
        let r = rec("tool", "", &body, &[]);
        let ex = excerpt_for(&r, &["文件".into()], &[Field::Body]).unwrap();
        assert!(ex.text.starts_with("…用于文件系统"));
        assert!(ex.text.ends_with("说明。…"));
        assert_eq!(&ex.text[ex.ranges[0].0..ex.ranges[0].1], "文件");

        let body = format!("{}文件{}", "背景".repeat(35), "后文".repeat(90));
        let r = rec("tool", "", &body, &[]);
        let ex = excerpt_for(&r, &["文件".into()], &[Field::Body]).unwrap();
        assert_eq!(ex.text, "…文件…");
        assert_eq!(&ex.text[ex.ranges[0].0..ex.ranges[0].1], "文件");
    }

    #[test]
    fn dictionary_bridges_languages_at_lower_weight() {
        let records = vec![
            rec("cn", "配置打印机驱动", "", &[]),
            rec("en", "configure the printer driver", "", &[]),
            rec("other", "unrelated", "", &[]),
        ];
        let mut s = Searcher::new();
        s.configure(
            SearchConfig::default(),
            dict("printer\t打印机\nprinters\t打印机\n"),
        );
        let hits = s.search(&records, &Query::parse("printer"));
        assert_eq!(
            keys(&records, &hits),
            ["en", "cn"],
            "direct match first, translated match second"
        );
        assert!(hits[1].terms.contains(&"打印".to_string()));
        let hits = s.search(&records, &Query::parse("打印机"));
        assert_eq!(keys(&records, &hits), ["cn", "en"]);
        let off = SearchConfig {
            dictionary: false,
            ..Default::default()
        };
        s.configure(off, dict("printer\t打印机\n"));
        assert_eq!(
            keys(&records, &s.search(&records, &Query::parse("printer"))),
            ["en"]
        );
    }

    #[test]
    fn field_weights_are_configurable() {
        let records = vec![
            rec("a", "msgpack msgpack msgpack", "", &[]),
            rec("b", "x", "msgpack", &[]),
        ];
        let mut s = Searcher::new();
        s.index(&records);
        let mut cfg = SearchConfig::default();
        cfg.weights.body = 50.0;
        s.configure(cfg, Dictionaries::default());
        assert_eq!(
            keys(&records, &s.search(&records, &Query::parse("msgpack")))[0],
            "b"
        );
    }

    #[test]
    fn filters_still_apply() {
        let records = vec![
            rec("x", "msgpack", "", &["ops"]),
            rec("y", "msgpack", "", &[]),
        ];
        let hits = Searcher::new().search(&records, &Query::parse("msgpack tag:ops"));
        assert_eq!(keys(&records, &hits), ["x"]);
        let hits = Searcher::new().search(&records, &Query::parse("untagged"));
        assert_eq!(keys(&records, &hits), ["y"]);
    }
}
