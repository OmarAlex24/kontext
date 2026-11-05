//! Section ranking: BM25 per field (body, headings, path and "Read when"),
//! with weights, synonyms at a lower weight, without accents and with a light
//! plural.
//!
//! The sums are always added in the same order (terms in query order, fields
//! in [`FIELDS`] order) so that the scores, and the ties, are reproducible.

use std::collections::HashMap;

use indexmap::IndexMap;
use serde::Deserialize;
use unicode_normalization::UnicodeNormalization;

use super::diacritic::is_diacritic;
use super::parse::Status;

/// Words that are not searched for, in Spanish and English.
const STOPWORDS: &[&str] = &[
    "a", "al", "an", "and", "como", "con", "de", "del", "el", "en", "for", "how", "is", "la",
    "las", "los", "of", "on", "or", "para", "por", "que", "se", "the", "to", "un", "una", "what",
    "y",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Body,
    Heading,
    Path,
    ReadWhen,
}

pub const FIELDS: [Field; 4] = [Field::Body, Field::Heading, Field::Path, Field::ReadWhen];

fn weight(f: Field) -> f64 {
    match f {
        Field::Body => 1.0,
        Field::Heading => 3.0,
        Field::Path => 2.0,
        Field::ReadWhen => 3.0,
    }
}

pub fn status_multiplier(s: Status) -> f64 {
    match s {
        Status::Current => 1.0,
        Status::Historical => 0.4,
        Status::Proposed => 0.6,
    }
}

pub const SYNONYM_WEIGHT: f64 = 0.7;
const BM25_K1: f64 = 1.2;
const BM25_B: f64 = 0.75;

fn stem(token: &str) -> String {
    let n = token.len();
    if n > 5 && token.ends_with("es") {
        return token[..n - 2].to_string();
    }
    if n > 3 && token.ends_with('s') {
        return token[..n - 1].to_string();
    }
    token.to_string()
}

fn push_token(out: &mut Vec<String>, cur: &mut String) {
    if cur.len() > 1 && !STOPWORDS.contains(&cur.as_str()) {
        out.push(stem(cur));
    }
    cur.clear();
}

/// Lowercased, without accents, split on anything that is not `[a-z0-9]`,
/// without stopwords, and with a light plural stemmed.
pub fn tokenize(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in text.nfd() {
        if is_diacritic(c) {
            continue;
        }
        if c.is_ascii() {
            let l = c.to_ascii_lowercase();
            if l.is_ascii_lowercase() || l.is_ascii_digit() {
                cur.push(l);
            } else {
                push_token(&mut out, &mut cur);
            }
            continue;
        }
        for l in c.to_lowercase() {
            if l.is_ascii_lowercase() || l.is_ascii_digit() {
                cur.push(l);
            } else {
                push_token(&mut out, &mut cur);
            }
        }
    }
    push_token(&mut out, &mut cur);
    out
}

/// `<kontext-home>/synonyms.toml`: groups of equivalent terms.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SynonymsFile {
    #[serde(default)]
    pub groups: Vec<Vec<String>>,
}

/// Each term (already normalized) with its equivalents, in order.
#[derive(Debug, Clone, Default)]
pub struct Synonyms(IndexMap<String, Vec<String>>);

impl Synonyms {
    pub fn new(groups: &[Vec<String>]) -> Synonyms {
        let mut map: IndexMap<String, Vec<String>> = IndexMap::new();
        for group in groups {
            let mut stems: Vec<String> = Vec::new();
            for t in group.iter().flat_map(|g| tokenize(g)) {
                if !stems.contains(&t) {
                    stems.push(t);
                }
            }
            for term in &stems {
                let others = stems.iter().filter(|s| *s != term).cloned();
                map.entry(term.clone()).or_default().extend(others);
            }
        }
        Synonyms(map)
    }

    /// The query terms with their weight: 1 for what was typed, less for the
    /// synonyms.
    pub fn expand(&self, query: &str) -> IndexMap<String, f64> {
        let mut terms = IndexMap::new();
        let tokens = tokenize(query);
        for t in &tokens {
            terms.insert(t.clone(), 1.0);
        }
        for t in &tokens {
            for s in self.0.get(t).into_iter().flatten() {
                if !terms.contains_key(s) {
                    terms.insert(s.clone(), SYNONYM_WEIGHT);
                }
            }
        }
        terms
    }
}

/// What BM25 needs from the whole corpus.
#[derive(Debug, Clone, Default)]
pub struct Stats {
    pub n: usize,
    pub df: HashMap<String, usize>,
    pub avg: [f64; 4],
}

/// A candidate section: how many times each query term appears in each field,
/// and the length of each field.
#[derive(Debug, Clone, Default)]
pub struct Counts {
    pub tf: [HashMap<String, u32>; 4],
    pub len: [usize; 4],
}

impl Counts {
    /// Counts in texts that are already normalized (terms separated by spaces),
    /// only the terms in `terms`.
    pub fn from_tokens(fields: [&str; 4], terms: &IndexMap<String, f64>) -> Counts {
        let mut c = Counts::default();
        for (i, text) in fields.iter().enumerate() {
            let mut n = 0;
            for tok in text.split(' ').filter(|t| !t.is_empty()) {
                n += 1;
                if terms.contains_key(tok) {
                    *c.tf[i].entry(tok.to_string()).or_default() += 1;
                }
            }
            c.len[i] = n;
        }
        c
    }
}

/// The score of a section (without its status multiplier).
pub fn score(stats: &Stats, c: &Counts, terms: &IndexMap<String, f64>) -> f64 {
    let n = stats.n as f64;
    let mut score = 0.0;
    for (term, w) in terms {
        let df = stats.df.get(term).copied().unwrap_or(0);
        if df == 0 {
            continue;
        }
        let df = df as f64;
        let idf = (1.0 + (n - df + 0.5) / (df + 0.5)).ln();
        for (i, f) in FIELDS.iter().enumerate() {
            let tf = c.tf[i].get(term).copied().unwrap_or(0);
            if tf == 0 {
                continue;
            }
            let tf = tf as f64;
            let norm = 1.0 - BM25_B + BM25_B * (c.len[i] as f64 / stats.avg[i]);
            score += w * weight(*f) * idf * ((tf * (BM25_K1 + 1.0)) / (tf + BM25_K1 * norm));
        }
    }
    score
}

/// The average length of a field (`total / n || 1`).
pub fn average(total: usize, n: usize) -> f64 {
    let n = n.max(1) as f64;
    let a = total as f64 / n;
    if a == 0.0 || a.is_nan() { 1.0 } else { a }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example() -> Synonyms {
        Synonyms::new(&[
            vec!["inventory".into(), "stock".into(), "warehouse".into()],
            vec!["real-time".into(), "realtime".into()],
        ])
    }

    #[test]
    fn folds_accents_and_plurals() {
        assert_eq!(
            tokenize("Geocercas Facturación"),
            vec!["geocerca", "facturacion"]
        );
        assert!(tokenize("  de la ").is_empty());
        assert_eq!(tokenize("`api-core`/NIÑOS"), vec!["api", "core", "nino"]);
        // `^` and the backtick are diacritics: they are removed, not split on.
        assert_eq!(tokenize("foo`bar"), vec!["foobar"]);
    }

    #[test]
    fn expands_synonyms_at_lower_weight() {
        let s = example();
        let t = s.expand("inventory");
        assert_eq!(t.get("inventory"), Some(&1.0));
        assert!(t["stock"] < 1.0);
        let t = s.expand("real time");
        assert_eq!(
            t.keys().collect::<Vec<_>>(),
            vec!["real", "time", "realtime"]
        );
    }
}
