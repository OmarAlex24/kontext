//! `kontext import --from-v0 <path>`: migrates the ledger of `ctx.py` (v0).
//!
//! v0 has a single file `ledger/entries.jsonl` with `seq`, `id` (`D-0001`),
//! `ts`, `kind`, `title`, `body`, `why`, `tags`, `actor` (`human:omar`),
//! `source`, `refs`, `status`, `prev_hash`, `hash` and extra fields. Its `hash`
//! is the sha256 of `json.dumps(sort_keys=True, separators=(",", ":"),
//! ensure_ascii=True)` of the entry without `hash`; here it is ported as is.
//!
//! Each v0 entry becomes a v1 entry of the same kind, in the same order, all
//! written to the current writer's file under a single lock. The v0 `id`s that
//! appear in `refs` and in the fields are rewritten to the new `id`s. What only
//! exists in v0 goes to `fields`, plus `fields.v0_id` and `fields.v0_hash`.
//! Importing twice does not duplicate anything.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value};

use crate::hlc::Hlc;
use crate::model::{Actor, GENESIS, RawEntry, prefix_for, sha256_hex};
use crate::store::Store;

/// v0 fields that have their own place in v1.
const MAPPED: &[&str] = &[
    "id",
    "ts",
    "kind",
    "title",
    "body",
    "why",
    "tags",
    "actor",
    "refs",
    "prev_hash",
    "hash",
];

#[derive(Debug, Default)]
pub struct ImportReport {
    /// Entries read from the v0 ledger.
    pub total: usize,
    /// Imported now, per kind.
    pub imported: BTreeMap<String, usize>,
    /// Already imported before.
    pub already: usize,
    /// Whether the v0 chain was broken (only with `--force`).
    pub chain_problem: Option<String>,
    /// v0 `id`s cited in `refs` that do not appear earlier in the v0 ledger.
    pub unresolved: Vec<String>,
}

impl ImportReport {
    pub fn imported_total(&self) -> usize {
        self.imported.values().sum()
    }
}

/// `json.dumps(obj, sort_keys=True, separators=(",", ":"), ensure_ascii=True)`.
pub fn py_canonical(v: &Value) -> String {
    let mut out = String::new();
    write_py(v, &mut out);
    out
}

fn write_py(v: &Value, out: &mut String) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => {
            if n.is_i64() || n.is_u64() {
                out.push_str(&n.to_string());
            } else {
                out.push_str(&py_float(n.as_f64().unwrap_or(0.0)));
            }
        }
        Value::String(s) => write_py_str(s, out),
        Value::Array(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_py(x, out);
            }
            out.push(']');
        }
        Value::Object(m) => {
            // serde_json's Map is already sorted by UTF-8 bytes, which is the
            // same order as Python's code points.
            out.push('{');
            for (i, (k, x)) in m.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_py_str(k, out);
                out.push(':');
                write_py(x, out);
            }
            out.push('}');
        }
    }
}

fn write_py_str(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            ' '..='~' => out.push(c),
            _ => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
        }
    }
    out.push('"');
}

/// Python's `repr(float)`: the shortest digits, with an exponent outside
/// `1e-4 <= |x| < 1e16`.
fn py_float(f: f64) -> String {
    if f == 0.0 {
        return if f.is_sign_negative() { "-0.0" } else { "0.0" }.into();
    }
    let sci = format!("{f:e}"); // e.g. "1.5e-7", "-1e16"
    let (mant, exp) = sci.split_once('e').unwrap();
    let exp: i32 = exp.parse().unwrap();
    let (sign, mant) = match mant.strip_prefix('-') {
        Some(m) => ("-", m),
        None => ("", mant),
    };
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    if (-4..16).contains(&exp) {
        let point = exp + 1; // position of the point within `digits`
        let s = if point <= 0 {
            format!("0.{}{digits}", "0".repeat((-point) as usize))
        } else if point as usize >= digits.len() {
            format!("{digits}{}.0", "0".repeat(point as usize - digits.len()))
        } else {
            format!(
                "{}.{}",
                &digits[..point as usize],
                &digits[point as usize..]
            )
        };
        format!("{sign}{s}")
    } else {
        let m = if digits.len() > 1 {
            format!("{}.{}", &digits[..1], &digits[1..])
        } else {
            digits
        };
        format!(
            "{sign}{m}e{}{:02}",
            if exp < 0 { '-' } else { '+' },
            exp.abs()
        )
    }
}

/// The `hash` of a v0 entry.
pub fn v0_hash(entry: &Value) -> String {
    let mut m = entry.as_object().cloned().unwrap_or_default();
    m.remove("hash");
    sha256_hex(py_canonical(&Value::Object(m)).as_bytes())
}

/// Checks the v0 chain the way `ctx.py verify` does. Returns the first problem.
pub fn verify_v0(entries: &[(usize, Value)]) -> Option<String> {
    let mut prev = GENESIS.to_string();
    for (count, (line, e)) in entries.iter().enumerate() {
        let count = count + 1;
        let id = e.get("id").and_then(Value::as_str).unwrap_or("?");
        if e.get("seq").and_then(Value::as_u64) != Some(count as u64) {
            return Some(format!(
                "line {line}: expected seq {count} (was a line deleted or moved?)"
            ));
        }
        if e.get("prev_hash").and_then(Value::as_str) != Some(prev.as_str()) {
            return Some(format!(
                "line {line} ({id}): prev_hash does not match the previous entry"
            ));
        }
        let h = v0_hash(e);
        if e.get("hash").and_then(Value::as_str) != Some(h.as_str()) {
            return Some(format!(
                "line {line} ({id}): the hash does not match (the entry was edited)"
            ));
        }
        prev = h;
    }
    None
}

/// Reads `<v0_home>/ledger/entries.jsonl`: (line number, entry).
pub fn read_v0(v0_home: &Path) -> Result<Vec<(usize, Value)>> {
    let p = v0_home.join("ledger").join("entries.jsonl");
    let text = fs::read_to_string(&p).with_context(|| format!("could not read {}", p.display()))?;
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(line)
            .with_context(|| format!("{}:{}: is not valid JSON", p.display(), i + 1))?;
        if !v.is_object() {
            bail!("{}:{}: is not a JSON object", p.display(), i + 1);
        }
        out.push((i + 1, v));
    }
    Ok(out)
}

fn v0_actor(s: &str) -> (Actor, bool) {
    match Actor::parse_short(s) {
        Ok(a) => (a, true),
        Err(_) => (
            Actor::agent(if s.is_empty() { "unknown" } else { s }),
            false,
        ),
    }
}

fn text(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

fn strings(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Array(a)) => a.iter().map(|x| text(Some(x))).collect(),
        Some(Value::String(s)) if !s.is_empty() => vec![s.clone()],
        _ => Vec::new(),
    }
}

/// Rewrites the texts that are exactly a known v0 `id`.
fn rewrite_ids(v: &Value, map: &HashMap<String, String>) -> Value {
    match v {
        Value::String(s) => Value::String(map.get(s).cloned().unwrap_or_else(|| s.clone())),
        Value::Array(a) => Value::Array(a.iter().map(|x| rewrite_ids(x, map)).collect()),
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, x)| (k.clone(), rewrite_ids(x, map)))
                .collect(),
        ),
        other => other.clone(),
    }
}

pub fn import(store: &mut Store, v0_home: &Path, force: bool) -> Result<ImportReport> {
    let entries = read_v0(v0_home)?;
    let mut report = ImportReport {
        total: entries.len(),
        ..ImportReport::default()
    };
    if let Some(p) = verify_v0(&entries) {
        if !force {
            bail!(
                "the v0 ledger chain is broken: {p}. Check it with `ctx.py verify`, \
                 or use --force to import it anyway"
            );
        }
        report.chain_problem = Some(p);
    }
    store.transact(|batch| {
        // What was already imported before: v0_id -> (v1 id, v0 hash, hlc).
        let mut done: HashMap<String, (String, String, String)> = HashMap::new();
        for e in batch.state()?.entries.values() {
            if let Some(v0) = e.fields.get("v0_id").and_then(Value::as_str) {
                let h = text(e.fields.get("v0_hash"));
                done.insert(v0.to_string(), (e.id.clone(), h, e.hlc.clone()));
            }
        }
        let mut map: HashMap<String, String> = HashMap::new();
        let mut last: Option<Hlc> = None;
        for (line, e) in &entries {
            let v0_id = text(e.get("id"));
            if v0_id.is_empty() {
                bail!("line {line} of the v0 ledger: the entry has no id");
            }
            let hash = text(e.get("hash"));
            if let Some((id, h, hlc)) = done.get(&v0_id) {
                if *h != hash {
                    bail!(
                        "{v0_id} was already imported from another v0 ledger (as {id}) and its hash does \
                         not match this one. Is it another project?"
                    );
                }
                map.insert(v0_id, id.clone());
                last = last.max(hlc.parse().ok());
                report.already += 1;
                continue;
            }
            let kind = text(e.get("kind"));
            if prefix_for(&kind).is_none() {
                bail!("line {line} of the v0 ledger ({v0_id}): unknown kind {kind:?}");
            }
            // The v0 order is kept even when several entries share a second.
            let mut hlc = Hlc::from_iso(&text(e.get("ts")))
                .with_context(|| format!("line {line} of the v0 ledger ({v0_id})"))?;
            if let Some(l) = last.filter(|l| hlc <= *l) {
                hlc = l.bump();
            }
            let actor_text = text(e.get("actor"));
            let (actor, standard) = v0_actor(&actor_text);
            let mut refs = Vec::new();
            for r in strings(e.get("refs")) {
                match map.get(&r) {
                    Some(id) => refs.push(id.clone()),
                    None => {
                        report.unresolved.push(r.clone());
                        refs.push(r);
                    }
                }
            }
            let mut fields = Map::new();
            for (k, v) in e.as_object().unwrap() {
                if !MAPPED.contains(&k.as_str()) {
                    fields.insert(k.clone(), rewrite_ids(v, &map));
                }
            }
            fields.insert("v0_id".into(), Value::String(v0_id.clone()));
            fields.insert("v0_hash".into(), Value::String(hash));
            if !standard {
                fields.insert("v0_actor".into(), Value::String(actor_text));
            }
            let raw = RawEntry {
                kind: kind.clone(),
                actor: Some(actor),
                title: text(e.get("title")),
                body: text(e.get("body")),
                why: text(e.get("why")),
                tags: strings(e.get("tags")),
                refs,
                fields,
                hlc: Some(hlc.to_string()),
            };
            let pushed = batch.push(raw)?;
            last = last.max(pushed.entry.hlc.parse().ok());
            map.insert(v0_id, pushed.entry.id.clone());
            if pushed.new {
                *report.imported.entry(kind).or_default() += 1;
            } else {
                report.already += 1;
            }
        }
        Ok(())
    })?;
    report.unresolved.sort();
    report.unresolved.dedup();
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn python_canonical_matches_json_dumps() {
        // Values checked with python3 -c 'json.dumps(..., sort_keys=True,
        // separators=(",", ":"), ensure_ascii=True)'.
        let v = json!({"b": "ñ\n\"x\"\u{7f}", "a": [1, -2, 1.5, 1e16, 0.0001, 1e-5, 123.0], "c": "😀", "d": null, "e": true});
        assert_eq!(
            py_canonical(&v),
            "{\"a\":[1,-2,1.5,1e+16,0.0001,1e-05,123.0],\"b\":\"\\u00f1\\n\\\"x\\\"\\u007f\",\"c\":\"\\ud83d\\ude00\",\"d\":null,\"e\":true}"
        );
    }

    #[test]
    fn v0_hash_matches_reference() {
        // First line of a ledger made with ctx.py. This is real v0 data, so its
        // Spanish text stays: its hash is computed over that text.
        let line = r#"{"seq": 1, "id": "D-0001", "ts": "2026-11-04T01:11:54Z", "kind": "decision", "title": "Usar Rust", "body": "", "why": "binario único", "tags": ["core", "lang"], "actor": "human:omar", "source": "manual", "refs": [], "status": "accepted", "prev_hash": "0000000000000000000000000000000000000000000000000000000000000000", "hash": "23138a2ec66cd17c98e7d9cdd7db6a6471891e547c0104a72dda194c70d7e530"}"#;
        let v: Value = serde_json::from_str(line).unwrap();
        assert_eq!(v0_hash(&v), v["hash"].as_str().unwrap());
    }
}
