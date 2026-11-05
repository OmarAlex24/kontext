//! `kontext recall <text>`: searches the ledger (FTS5) and the Claude Code
//! transcripts, as `recall` did in v0.
//!
//! The transcripts come from `~/.claude/projects/*/*.jsonl`, or from the
//! patterns in `$KONTEXT_TRANSCRIPTS_GLOB` (separated by `:`). The patterns only
//! understand `*` and `?` within each part of the path.

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use serde::Serialize;
use serde_json::Value;

use crate::config::expand_tilde;
use crate::index::Index;
use crate::state::{State, Status};
use crate::views::{kind_label, one_line};

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    /// ISO date (or `hlc`), for sorting.
    pub when: String,
    /// `ledger` or `transcript`.
    pub source: String,
    /// The entry `id`, or the session and who spoke.
    pub label: String,
    pub snippet: String,
}

pub fn transcript_globs() -> Vec<String> {
    match std::env::var("KONTEXT_TRANSCRIPTS_GLOB") {
        Ok(v) if !v.trim().is_empty() => v
            .split(':')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
        _ => vec!["~/.claude/projects/*/*.jsonl".to_string()],
    }
}

/// `*` and `?` in one part of the path.
fn wildcard(pat: &[char], s: &[char]) -> bool {
    match (pat.first(), s.first()) {
        (None, None) => true,
        (Some('*'), _) => wildcard(&pat[1..], s) || (!s.is_empty() && wildcard(pat, &s[1..])),
        (Some('?'), Some(_)) => wildcard(&pat[1..], &s[1..]),
        (Some(p), Some(c)) if p == c => wildcard(&pat[1..], &s[1..]),
        _ => false,
    }
}

/// The paths that match a pattern.
pub fn glob(pattern: &str) -> Vec<PathBuf> {
    let Ok(path) = expand_tilde(pattern) else {
        return Vec::new();
    };
    let mut current = vec![if path.is_absolute() {
        PathBuf::from("/")
    } else {
        PathBuf::from(".")
    }];
    for comp in path.components() {
        let part = comp.as_os_str().to_string_lossy().into_owned();
        if matches!(comp, std::path::Component::RootDir) {
            continue;
        }
        let mut next = Vec::new();
        if !part.contains(['*', '?']) {
            for c in &current {
                let p = c.join(&part);
                if p.exists() {
                    next.push(p);
                }
            }
        } else {
            let pat: Vec<char> = part.chars().collect();
            for c in &current {
                let Ok(rd) = fs::read_dir(c) else { continue };
                for ent in rd.flatten() {
                    let name: Vec<char> = ent.file_name().to_string_lossy().chars().collect();
                    if name.first() != Some(&'.') && wildcard(&pat, &name) {
                        next.push(ent.path());
                    }
                }
            }
        }
        current = next;
    }
    current.sort();
    current
}

fn snippet(text: &str, terms: &[String], width: usize) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let chars: Vec<char> = flat.chars().collect();
    let low: Vec<char> = flat.to_lowercase().chars().collect();
    let pos = terms
        .iter()
        .filter_map(|t| {
            let t: Vec<char> = t.chars().collect();
            low.windows(t.len().max(1)).position(|w| w == t.as_slice())
        })
        .min()
        .unwrap_or(0);
    let start = pos.saturating_sub(width / 3).min(chars.len());
    let end = (start + width).min(chars.len());
    let mut s: String = chars[start..end].iter().collect();
    if start > 0 {
        s.insert(0, '…');
    }
    if end < chars.len() {
        s.push('…');
    }
    s
}

/// Removes `<system-reminder>…</system-reminder>`.
fn strip_reminders(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find("<system-reminder>") {
        out.push_str(&rest[..i]);
        match rest[i..].find("</system-reminder>") {
            Some(j) => rest = &rest[i + j + "</system-reminder>".len()..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

/// `(who, text)` of a transcript record, if it is a message from the user or
/// the assistant. Tools and reasoning are ignored.
pub fn extract_message(rec: &Value) -> Option<(String, String)> {
    let kind = rec.get("type")?.as_str()?;
    if kind != "user" && kind != "assistant" {
        return None;
    }
    let content = rec.get("message")?.get("content")?;
    let mut parts = Vec::new();
    match content {
        Value::String(s) => parts.push(s.clone()),
        Value::Array(blocks) => {
            for b in blocks {
                if b.get("type").and_then(Value::as_str) == Some("text")
                    && let Some(t) = b.get("text").and_then(Value::as_str)
                {
                    parts.push(t.to_string());
                }
            }
        }
        _ => return None,
    }
    let text = strip_reminders(&parts.join("\n")).trim().to_string();
    if text.is_empty() {
        return None;
    }
    Some((kind.to_string(), text))
}

/// A `YYYY-MM-DD` or ISO-8601 date, as text comparable with `hlc` and with
/// the `timestamp`s of the transcripts.
pub fn parse_since(s: &str) -> Result<String> {
    let s = s.trim();
    let ts: jiff::Timestamp = if s.len() == 10 {
        format!("{s}T00:00:00Z").parse()
    } else {
        s.parse()
    }
    .map_err(|_| anyhow::anyhow!("invalid date {s:?}: use YYYY-MM-DD or an ISO-8601 date"))?;
    Ok(ts.strftime("%Y-%m-%dT%H:%M:%S").to_string())
}

/// Each word as a prefix (`"cone"*` finds «Conekta»), all at once.
fn fts_query(terms: &[String]) -> String {
    terms
        .iter()
        .map(|t| format!("\"{}\"*", t.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" ")
}

fn transcript_hits(path: &Path, terms: &[String], since: Option<&str>, hits: &mut Vec<Hit>) {
    let Ok(f) = fs::File::open(path) else { return };
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    for line in BufReader::new(f).lines().map_while(Result::ok) {
        let low = line.to_lowercase();
        // Cheap filter: the ASCII terms must be on the raw line.
        if !terms
            .iter()
            .filter(|t| t.is_ascii())
            .all(|t| low.contains(t.as_str()))
        {
            continue;
        }
        let Ok(rec) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some((who, text)) = extract_message(&rec) else {
            continue;
        };
        let lt = text.to_lowercase();
        if !terms.iter().all(|t| lt.contains(t.as_str())) {
            continue;
        }
        let ts = rec
            .get("timestamp")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if let Some(s) = since
            && !ts.is_empty()
            && ts.as_str() < s
        {
            continue;
        }
        let sid = rec
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| stem.clone());
        hits.push(Hit {
            when: ts,
            source: "transcript".into(),
            label: format!("{sid} {who}"),
            snippet: snippet(&text, terms, 220),
        });
    }
}

/// Searches for `query` (all the words). Returns the first `limit`, from newest
/// to oldest, and how many there were in total.
pub fn recall(
    index: &Index,
    st: &State,
    query: &str,
    limit: usize,
    since: Option<&str>,
) -> Result<(Vec<Hit>, usize)> {
    let terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if terms.is_empty() {
        bail!("the search is empty");
    }
    let since = since.map(parse_since).transpose()?;
    let mut hits = Vec::new();
    for id in index.search(&fts_query(&terms))? {
        let Some(e) = st.get(&id) else { continue };
        if since.as_deref().is_some_and(|s| e.hlc.as_str() < s) {
            continue;
        }
        let text = [&e.title, &e.why, &e.body]
            .iter()
            .filter(|t| !t.trim().is_empty())
            .map(|t| t.as_str())
            .collect::<Vec<_>>()
            .join(" · ");
        let status = match (st.status_of(&id), st.superseded_by.get(&id)) {
            (Some(s), Some(by)) => format!(", {} by {by} (no longer applies)", s.label()),
            // A user rule applies even when it is not signed.
            (Some(Status::Pending), None) if crate::views::user_rule(e) => {
                ", user rule, in force (unsigned)".to_string()
            }
            (Some(s), None) => format!(", {}", s.label()),
            _ => String::new(),
        };
        hits.push(Hit {
            when: e.hlc.clone(),
            source: "ledger".into(),
            label: format!("{id} {}{status}", kind_label(&e.kind)),
            snippet: snippet(&text, &terms, 220),
        });
    }
    let mut files = Vec::new();
    for g in transcript_globs() {
        files.extend(glob(&g));
    }
    files.sort();
    files.dedup();
    for f in files.iter().filter(|f| f.is_file()) {
        transcript_hits(f, &terms, since.as_deref(), &mut hits);
    }
    hits.sort_by(|a, b| b.when.cmp(&a.when));
    let total = hits.len();
    hits.truncate(limit);
    Ok((hits, total))
}

pub fn markdown(query: &str, hits: &[Hit], total: usize) -> String {
    if hits.is_empty() {
        return format!("Nothing for «{query}» in the ledger or in the transcripts.\n");
    }
    let mut l = vec![format!(
        "# kontext recall «{query}»: {} of {total}",
        hits.len()
    )];
    for h in hits {
        let when = h.when.get(..16).unwrap_or(&h.when).replace('T', " ");
        let src = if h.source == "ledger" {
            "ledger"
        } else {
            "transcript"
        };
        l.push(format!(
            "- {when} [{src}] {}: {}",
            h.label,
            one_line(&h.snippet, 400)
        ));
    }
    l.join("\n") + "\n"
}
