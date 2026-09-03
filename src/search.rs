//! Fuzzy search over skill records with `tag:` / `agent:` / `status:` / `source:` filters.

use crate::reconcile::{DeployState, SkillRecord};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config as NucleoConfig, Matcher, Utf32Str};

#[derive(Debug, Default, Clone)]
pub struct Query {
    pub text: String,
    pub tags: Vec<String>,
    pub agents: Vec<String>,
    pub statuses: Vec<String>,
    pub sources: Vec<String>,
    pub untagged: bool,
}

impl Query {
    /// Parse `tag:foo agent:codex status:modified free text` into a query.
    pub fn parse(input: &str) -> Self {
        let mut q = Query::default();
        let mut free = Vec::new();
        for tok in input.split_whitespace() {
            if let Some(v) = tok.strip_prefix("tag:") {
                if !v.is_empty() {
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
            } else if tok == "untagged" || tok == "tag:" {
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
        if !self.sources.is_empty() {
            let kind = r.source.as_ref().map(|s| s.kind()).unwrap_or("none");
            if !self.sources.iter().any(|s| s == kind) {
                return false;
            }
        }
        true
    }
}

#[derive(Debug, Clone)]
pub struct Hit {
    pub index: usize,
    pub score: u32,
}

pub struct Searcher {
    matcher: Matcher,
}

impl Default for Searcher {
    fn default() -> Self {
        Self::new()
    }
}

impl Searcher {
    pub fn new() -> Self {
        Self {
            matcher: Matcher::new(NucleoConfig::DEFAULT),
        }
    }

    /// Primary text: directory name, frontmatter name, tags. Matches here rank highest.
    pub fn primary(r: &SkillRecord) -> String {
        let mut s = String::new();
        s.push_str(&r.key);
        if let Some(n) = &r.name
            && n != &r.key
        {
            s.push(' ');
            s.push_str(n);
        }
        if !r.tags.is_empty() {
            s.push_str(" [");
            s.push_str(&r.tags.join(" "));
            s.push(']');
        }
        s
    }

    /// Secondary text: description and note.
    pub fn secondary(r: &SkillRecord) -> String {
        let mut s = String::new();
        if let Some(d) = &r.description {
            s.push_str(d);
        }
        if let Some(n) = &r.note {
            s.push(' ');
            s.push_str(n);
        }
        s
    }

    /// Return matching indices into `records`, best first. Empty text keeps all
    /// filter-passing records in their original order.
    pub fn search(&mut self, records: &[SkillRecord], query: &Query) -> Vec<Hit> {
        let candidates: Vec<usize> = records
            .iter()
            .enumerate()
            .filter(|(_, r)| query.filter(r))
            .map(|(i, _)| i)
            .collect();
        if query.text.is_empty() {
            return candidates
                .into_iter()
                .map(|index| Hit { index, score: 0 })
                .collect();
        }
        let pattern = Pattern::parse(&query.text, CaseMatching::Ignore, Normalization::Smart);
        let mut buf = Vec::new();
        let mut hits: Vec<Hit> = candidates
            .into_iter()
            .filter_map(|index| {
                let r = &records[index];
                let p = Self::primary(r);
                let primary = pattern.score(Utf32Str::new(&p, &mut buf), &mut self.matcher);
                let d = Self::secondary(r);
                let secondary = pattern.score(Utf32Str::new(&d, &mut buf), &mut self.matcher);
                // Name/tag matches outrank description matches of any strength.
                let score = match (primary, secondary) {
                    (Some(a), Some(b)) => a * 4 + b,
                    (Some(a), None) => a * 4,
                    (None, Some(b)) => b,
                    (None, None) => return None,
                };
                Some(Hit { index, score })
            })
            .collect();
        hits.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then(records[a.index].key.cmp(&records[b.index].key))
        });
        hits
    }
}
