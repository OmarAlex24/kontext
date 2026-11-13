//! `kontext verify` (DESIGN §10): checks the chain of each file of the ledger,
//! and that there are no repeated `id`s with different content across files.
//!
//! Per file: `n` runs from 1, `prev` equals the `hash` of the previous line,
//! `hash` and `id` are recomputed, `writer` equals the file name, and no line is
//! left half-written at the end. The first problem of each file is reported.
//!
//! An entry of a kind this version does not know (written by a newer kontext)
//! is checked like the rest, except that its `id` only has to be
//! `<uppercase letters>-<6 hex>` with the hex taken from its content: its
//! prefix can't be checked. It is not a problem: it goes in
//! [`Report::newer`], and the state leaves it out.

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::Path;

use anyhow::Result;
use serde_json::Value;

use crate::hlc::Hlc;
use crate::model::{
    FORMAT_VERSION, GENESIS, compute_hash, compute_id, content_digest, is_valid_id, prefix_for,
};
use crate::store::ledger_files;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Problem {
    /// Path relative to the kontext-home, for example `ledger/omar@mac.jsonl`.
    pub file: String,
    pub line: usize,
    pub message: String,
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: {}", self.file, self.line, self.message)
    }
}

#[derive(Debug, Default)]
pub struct Report {
    /// Entries that passed the check.
    pub entries: usize,
    pub writers: usize,
    pub problems: Vec<Problem>,
    /// Cited images that are missing from `media/` or whose content does not
    /// match their hash. They are kept apart: the chain can be fine without them.
    pub media_problems: Vec<Problem>,
    /// How many cited images were checked.
    pub media: usize,
    /// Entries of a kind this version does not know (from a newer kontext):
    /// their chain and hash are fine, they are not a problem, and the state
    /// leaves them out.
    pub newer: Vec<Problem>,
    /// `scope` entries whose `summary_hash` does not match their `summary`:
    /// the chain is fine, but what was signed is not what is stored.
    pub scope_problems: Vec<Problem>,
}

impl Report {
    pub fn ok(&self) -> bool {
        self.problems.is_empty() && self.media_problems.is_empty() && self.scope_problems.is_empty()
    }

    pub fn summary(&self) -> String {
        let e = plural(self.entries, "entry", "entries");
        let w = plural(self.writers, "writer", "writers");
        let m = if self.media > 0 {
            format!(", {}", plural(self.media, "image", "images"))
        } else {
            String::new()
        };
        if self.ok() {
            format!("OK: {e} in {w}{m}")
        } else {
            format!(
                "FAILED: {} in {e} checked across {w}{m}",
                plural(
                    self.problems.len() + self.media_problems.len() + self.scope_problems.len(),
                    "problem",
                    "problems"
                )
            )
        }
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

struct Seen {
    file: String,
    line: usize,
    digest: String,
}

/// The chains of the ledger (what the views and `sync` use).
pub fn run(home: &Path) -> Result<Report> {
    run_inner(home, false)
}

/// `kontext verify`: the chains and also the cited images (it reads and hashes
/// each one, so it is not used in the views).
pub fn run_with_media(home: &Path) -> Result<Report> {
    // With a kontext server, a cited file this copy lacks comes down now: the
    // automatic pull brings no media (DESIGN §14.1).
    for w in crate::remote::ensure_media(home, cited_media(home)?) {
        eprintln!("kontext: warning: {w}");
    }
    run_inner(home, true)
}

/// The names (`<sha256>.<ext>`) of the media every entry cites.
fn cited_media(home: &Path) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for (_, path) in ledger_files(&home.join("ledger"))? {
        let bytes = fs::read(&path)?;
        for raw in bytes.split(|b| *b == b'\n') {
            let Ok(v) = serde_json::from_slice::<Value>(raw) else {
                continue;
            };
            let Some(fields) = v.get("fields").and_then(Value::as_object) else {
                continue;
            };
            for item in crate::media::refs_of(fields) {
                if let Some(name) = item
                    .get("value")
                    .and_then(Value::as_str)
                    .and_then(|v| v.strip_prefix("media/"))
                {
                    out.push(name.to_string());
                }
            }
        }
    }
    Ok(out)
}

fn run_inner(home: &Path, media: bool) -> Result<Report> {
    let files = ledger_files(&home.join("ledger"))?;
    let mut report = Report {
        writers: files.len(),
        ..Report::default()
    };
    let mut ids: HashMap<String, Seen> = HashMap::new();
    for (writer, path) in &files {
        let rel = relative(home, path);
        let bytes = fs::read(path)?;
        if let Err(p) = check_file(
            writer,
            &rel,
            &bytes,
            &mut ids,
            &mut report.entries,
            &mut report.newer,
        ) {
            report.problems.push(p);
        }
        check_scopes(&rel, &bytes, &mut report);
        if media {
            check_media(home, &rel, &bytes, &mut report);
        }
    }
    Ok(report)
}

/// The images the entries cite (`fields.evidence` and `fields.attachments`):
/// they must be in `media/` and their hash must match.
fn check_media(home: &Path, file: &str, bytes: &[u8], report: &mut Report) {
    for (i, raw) in bytes.split(|b| *b == b'\n').enumerate() {
        let Ok(v) = serde_json::from_slice::<Value>(raw) else {
            continue;
        };
        let Some(fields) = v.get("fields").and_then(Value::as_object) else {
            continue;
        };
        for item in crate::media::refs_of(fields) {
            report.media += 1;
            if let Err(message) = crate::media::check(home, item) {
                report.media_problems.push(Problem {
                    file: file.to_string(),
                    line: i + 1,
                    message,
                });
            }
        }
    }
}

/// Each `scope` entry's `summary_hash` must be the sha256 of its `summary`:
/// what is stored has to be exactly what the owner read.
fn check_scopes(file: &str, bytes: &[u8], report: &mut Report) {
    for (i, raw) in bytes.split(|b| *b == b'\n').enumerate() {
        let Ok(v) = serde_json::from_slice::<Value>(raw) else {
            continue;
        };
        if v.get("kind").and_then(Value::as_str) != Some("scope") {
            continue;
        }
        let f = |k: &str| {
            v.get("fields")
                .and_then(|x| x.get(k))
                .and_then(Value::as_str)
                .unwrap_or_default()
        };
        if f("summary_hash") != crate::scope::hash_of(f("summary")) {
            report.scope_problems.push(Problem {
                file: file.to_string(),
                line: i + 1,
                message: format!(
                    "{}: the summary_hash of the scope signature does not match its summary (the text was changed after it was signed)",
                    v.get("id").and_then(Value::as_str).unwrap_or("?")
                ),
            });
        }
    }
}

fn relative(home: &Path, p: &Path) -> String {
    p.strip_prefix(home).unwrap_or(p).display().to_string()
}

fn check_file(
    writer: &str,
    file: &str,
    bytes: &[u8],
    ids: &mut HashMap<String, Seen>,
    count: &mut usize,
    newer: &mut Vec<Problem>,
) -> Result<(), Problem> {
    let problem = |line: usize, message: String| Problem {
        file: file.to_string(),
        line,
        message,
    };
    let mut lines: Vec<&[u8]> = bytes.split(|b| *b == b'\n').collect();
    // What follows the last `\n`: empty if the file ends properly.
    let tail = lines.pop().unwrap_or_default();
    let mut prev = GENESIS.to_string();
    for (i, raw) in lines.iter().enumerate() {
        let line = i + 1;
        let expected_n = line as u64;
        let v: Value = serde_json::from_slice(raw)
            .map_err(|e| problem(line, format!("is not valid JSON ({e})")))?;
        if !v.is_object() {
            return Err(problem(line, "is not a JSON object".into()));
        }
        let get_str = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default();
        let version = v.get("v").and_then(Value::as_u64).unwrap_or(0);
        if version != FORMAT_VERSION {
            return Err(problem(
                line,
                format!(
                    "format version {version}; this version of kontext reads version {FORMAT_VERSION}"
                ),
            ));
        }
        if get_str("writer") != writer {
            return Err(problem(
                line,
                format!(
                    "the entry says writer {:?} but it is in the file of {writer:?}",
                    get_str("writer")
                ),
            ));
        }
        match v.get("n").and_then(Value::as_u64) {
            Some(n) if n == expected_n => {}
            Some(n) => {
                return Err(problem(
                    line,
                    format!(
                        "expected n={expected_n} but it says n={n} (was a line deleted or moved?)"
                    ),
                ));
            }
            None => return Err(problem(line, "n is missing".into())),
        }
        if get_str("prev") != prev {
            return Err(problem(
                line,
                "prev does not match the hash of the previous line".into(),
            ));
        }
        let hash = compute_hash(&v);
        if get_str("hash") != hash {
            return Err(problem(
                line,
                "the hash does not match the content (the line was edited)".into(),
            ));
        }
        let digest = content_digest(&v);
        let kind = get_str("kind");
        let id = if prefix_for(kind).is_some() {
            compute_id(&v).map_err(|e| problem(line, e.to_string()))?
        } else {
            // A kind from a newer version: the prefix is whatever it says, the
            // hex part still comes from the content.
            let id = get_str("id");
            let hex = id.split_once('-').map(|(_, h)| h).unwrap_or_default();
            if !is_valid_id(id) || hex != &digest[..6] {
                return Err(problem(
                    line,
                    format!(
                        "the id {id:?} of an entry of kind {kind:?} does not match its content \
                         (it should be <PREFIX>-{})",
                        &digest[..6]
                    ),
                ));
            }
            newer.push(problem(
                line,
                format!("{id} is from a newer kontext version (kind {kind}): upgrade to read it"),
            ));
            id.to_string()
        };
        if get_str("id") != id {
            return Err(problem(
                line,
                format!(
                    "the id {:?} does not match the content (it should be {id})",
                    get_str("id")
                ),
            ));
        }
        if let Err(e) = get_str("hlc").parse::<Hlc>() {
            return Err(problem(line, e.to_string()));
        }
        if let Some(seen) = ids.get(&id) {
            let what = if seen.digest == digest {
                "repeated entry"
            } else {
                "repeated id with different content"
            };
            return Err(problem(
                line,
                format!("{what}: {id} is already in {}:{}", seen.file, seen.line),
            ));
        }
        ids.insert(
            id,
            Seen {
                file: file.to_string(),
                line,
                digest,
            },
        );
        prev = hash;
        *count += 1;
    }
    if !tail.is_empty() {
        return Err(problem(
            lines.len() + 1,
            "the last line is incomplete (was a write cut off?)".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Actor, RawEntry};
    use crate::store::Store;
    use std::path::PathBuf;

    /// A ledger with 4 entries from `omar@mac`. Returns the folder and the path
    /// of the file.
    fn ledger() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Store::open(dir.path(), "omar@mac").unwrap();
        for t in ["one", "two", "three", "four"] {
            let mut r = RawEntry::new("fact", t);
            r.actor = Some(Actor::human("omar"));
            s.append(r).unwrap();
        }
        let p = s.writer_file();
        (dir, p)
    }

    fn lines(p: &Path) -> Vec<String> {
        fs::read_to_string(p)
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn write_lines(p: &Path, ls: &[String]) {
        fs::write(p, ls.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
    }

    fn first_problem(dir: &Path) -> Problem {
        let r = run(dir).unwrap();
        assert!(!r.ok());
        r.problems[0].clone()
    }

    #[test]
    fn clean_ledger_verifies() {
        let (dir, _) = ledger();
        let r = run(dir.path()).unwrap();
        assert!(r.ok(), "{:?}", r.problems);
        assert_eq!(r.summary(), "OK: 4 entries in 1 writer");
    }

    #[test]
    fn detects_edited_line() {
        let (dir, p) = ledger();
        let mut ls = lines(&p);
        ls[1] = ls[1].replace("\"two\"", "\"TWO\"");
        write_lines(&p, &ls);
        let pr = first_problem(dir.path());
        assert_eq!(pr.line, 2);
        assert!(pr.message.contains("hash"), "{pr}");
        assert_eq!(
            pr.to_string().split(':').next(),
            Some("ledger/omar@mac.jsonl")
        );
    }

    #[test]
    fn detects_edited_line_with_recomputed_hash() {
        let (dir, p) = ledger();
        let mut ls = lines(&p);
        let mut v: Value = serde_json::from_str(&ls[1]).unwrap();
        v["title"] = "TWO".into();
        v["hash"] = compute_hash(&v).into();
        ls[1] = crate::model::canonical(&v);
        write_lines(&p, &ls);
        let pr = first_problem(dir.path());
        assert_eq!(pr.line, 2);
        assert!(pr.message.contains("id"), "{pr}");
    }

    #[test]
    fn detects_deleted_line() {
        let (dir, p) = ledger();
        let mut ls = lines(&p);
        ls.remove(1);
        write_lines(&p, &ls);
        let pr = first_problem(dir.path());
        assert_eq!(pr.line, 2);
        assert!(pr.message.contains("expected n=2 but it says n=3"), "{pr}");
    }

    #[test]
    fn detects_deleted_last_line_only_by_count() {
        // Deleting the last line leaves a valid and shorter chain: that is only
        // noticed by comparing with another copy (git). It is documented here.
        let (dir, p) = ledger();
        let mut ls = lines(&p);
        ls.pop();
        write_lines(&p, &ls);
        assert!(run(dir.path()).unwrap().ok());
    }

    #[test]
    fn detects_reordered_lines() {
        let (dir, p) = ledger();
        let mut ls = lines(&p);
        ls.swap(1, 2);
        write_lines(&p, &ls);
        let pr = first_problem(dir.path());
        assert_eq!(pr.line, 2);
        assert!(pr.message.contains("expected n=2 but it says n=3"), "{pr}");
    }

    #[test]
    fn reports_truncated_last_line_and_index_ignores_it() {
        let (dir, p) = ledger();
        let mut bytes = fs::read(&p).unwrap();
        // Cut the last line in half (without its `\n`).
        let last_start = bytes[..bytes.len() - 1]
            .iter()
            .rposition(|b| *b == b'\n')
            .unwrap()
            + 1;
        bytes.truncate(last_start + 20);
        fs::write(&p, &bytes).unwrap();
        let pr = first_problem(dir.path());
        assert_eq!(pr.line, 4);
        assert!(pr.message.contains("incomplete"), "{pr}");
        // The index only takes complete lines.
        let mut idx = crate::index::Index::open(dir.path()).unwrap();
        idx.rebuild().unwrap();
        assert_eq!(idx.count().unwrap(), 3);
    }

    /// A line of kind `kind` with id prefix `prefix` after `prev_line`, with
    /// its chain and hash right (what a newer kontext would write).
    fn newer_line(prev_line: &str, kind: &str, prefix: &str) -> String {
        let last: Value = serde_json::from_str(prev_line).unwrap();
        let mut v = serde_json::json!({
            "v": 1, "writer": "omar@mac", "n": last["n"].as_u64().unwrap() + 1,
            "prev": last["hash"], "hlc": "2026-11-20T10:00:00.000Z.0000",
            "actor": {"type": "human", "name": "omar"}, "kind": kind,
            "title": "Checkout scope", "body": "", "why": "", "tags": [], "refs": [],
            "fields": {"epic": "x"}, "sig": null,
        });
        v["id"] = format!("{prefix}-{}", &content_digest(&v)[..6]).into();
        v["hash"] = compute_hash(&v).into();
        crate::model::canonical(&v)
    }

    #[test]
    fn an_entry_from_a_newer_version_is_listed_not_a_problem() {
        let (dir, p) = ledger();
        let mut ls = lines(&p);
        ls.push(newer_line(ls.last().unwrap(), "roadmap", "W"));
        write_lines(&p, &ls);
        let r = run(dir.path()).unwrap();
        assert!(r.ok(), "{:?}", r.problems);
        assert_eq!(r.entries, 5);
        assert_eq!(r.newer.len(), 1);
        let n = r.newer[0].to_string();
        assert!(n.starts_with("ledger/omar@mac.jsonl:5: W-"), "{n}");
        assert!(
            n.ends_with("is from a newer kontext version (kind roadmap): upgrade to read it"),
            "{n}"
        );
        // Its id still has to be `<UPPERCASE>-<6 hex>` with the hex from its
        // content (the hash stays right, so only the id is wrong).
        let lowercase = newer_line(&ls[3], "roadmap", "w");
        let mut v: Value = serde_json::from_str(&ls[4]).unwrap();
        let id = v["id"].as_str().unwrap().to_string();
        let flipped = if id.ends_with('0') { '1' } else { '0' };
        v["id"] = format!("{}{flipped}", &id[..id.len() - 1]).into();
        v["hash"] = compute_hash(&v).into();
        for bad in [lowercase, crate::model::canonical(&v)] {
            let mut ls = ls.clone();
            ls[4] = bad;
            write_lines(&p, &ls);
            let pr = first_problem(dir.path());
            assert_eq!(pr.line, 5);
            assert!(
                pr.message.contains("of kind \"roadmap\" does not match"),
                "{pr}"
            );
        }
        // A known kind with another prefix is still wrong.
        let mut ls = ls.clone();
        ls[4] = newer_line(&ls[3], "fact", "S");
        write_lines(&p, &ls);
        assert!(first_problem(dir.path()).message.contains("should be F-"));
    }

    #[test]
    fn detects_same_id_with_different_content_across_files() {
        // Look for two entries from different writers whose ids collide in the
        // 24 bits (birthday paradox: a few thousand are enough).
        use crate::model::{Entry, canonical};
        let make = |writer: &str, i: usize| {
            let mut e = Entry {
                v: 1,
                id: String::new(),
                writer: writer.into(),
                n: 1,
                prev: GENESIS.into(),
                hash: String::new(),
                hlc: "2026-11-03T18:22:10.123Z.0000".into(),
                actor: Actor::human("omar"),
                kind: "fact".into(),
                title: format!("fact {i}"),
                body: String::new(),
                why: String::new(),
                tags: vec![],
                refs: vec![],
                fields: Default::default(),
                sig: None,
                extra: Default::default(),
            }
            .to_value();
            e["id"] = compute_id(&e).unwrap().into();
            e["hash"] = compute_hash(&e).into();
            e
        };
        let mut seen: HashMap<String, Value> = HashMap::new();
        let mut pair = None;
        for i in 0..200_000 {
            let a = make("omar@mac", i);
            let b = make("ana@laptop", i);
            if let Some(other) = seen.get(b["id"].as_str().unwrap()) {
                pair = Some((other.clone(), b));
                break;
            }
            seen.insert(a["id"].as_str().unwrap().to_string(), a);
        }
        let (a, b) = pair.expect("with 200 thousand attempts a collision always shows up");
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("ledger")).unwrap();
        fs::write(
            dir.path().join("ledger/omar@mac.jsonl"),
            canonical(&a) + "\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("ledger/ana@laptop.jsonl"),
            canonical(&b) + "\n",
        )
        .unwrap();
        let r = run(dir.path()).unwrap();
        assert_eq!(r.problems.len(), 1, "{:?}", r.problems);
        let pr = &r.problems[0];
        assert!(
            pr.message.contains("repeated id with different content"),
            "{pr}"
        );
        assert!(pr.message.contains("ledger/"), "{pr}");
    }

    #[test]
    fn detects_line_in_wrong_writer_file() {
        let (dir, p) = ledger();
        fs::copy(&p, dir.path().join("ledger/ana@laptop.jsonl")).unwrap();
        let r = run(dir.path()).unwrap();
        assert_eq!(r.problems.len(), 1);
        assert!(r.problems[0].message.contains("writer"));
        assert_eq!(r.problems[0].file, "ledger/ana@laptop.jsonl");
    }
}
