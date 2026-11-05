//! Cursors (DESIGN §7.5) and assignment frontiers (§7.3).
//!
//! A [`Frontier`] is a vector: the last position `n` of each file in the
//! ledger, for example `{"omar@mac": 412, "ana@laptop": 30}`. Since each file
//! only grows, "what is new" since a frontier is exact even when entries arrive
//! late through sync and with an older `hlc`.
//!
//! The cursor of reader `omar`, written from machine `omar@mac`, lives in
//! `cursors/omar/omar@mac.json`. Only that reader on that machine writes it,
//! and it replaces it in one go (temporary file and `rename`). The effective
//! cursor is the maximum per file across all the files of that reader.
//!
//! In text, a frontier is written `omar@mac:412,ana@laptop:30` (writer names
//! contain no `:` or `,`), and `-` if it is empty. That way it fits in
//! `kontext done --since <frontier>` (phase 3).

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{Context, Result, bail};

use crate::config::{validate_writer, write_atomic};
use crate::model::Entry;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Frontier(pub BTreeMap<String, u64>);

impl Frontier {
    pub fn get(&self, writer: &str) -> u64 {
        self.0.get(writer).copied().unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.0.values().all(|n| *n == 0)
    }

    /// Was the entry already there before this frontier?
    pub fn covers(&self, e: &Entry) -> bool {
        e.n <= self.get(&e.writer)
    }

    /// The maximum per file.
    pub fn merge(&mut self, other: &Frontier) {
        for (w, n) in &other.0 {
            let cur = self.0.entry(w.clone()).or_insert(0);
            *cur = (*cur).max(*n);
        }
    }

    pub fn encode(&self) -> String {
        let parts: Vec<String> = self
            .0
            .iter()
            .filter(|(_, n)| **n > 0)
            .map(|(w, n)| format!("{w}:{n}"))
            .collect();
        if parts.is_empty() {
            "-".into()
        } else {
            parts.join(",")
        }
    }

    pub fn decode(s: &str) -> Result<Frontier> {
        let s = s.trim();
        let mut out = BTreeMap::new();
        if s == "-" || s.is_empty() {
            return Ok(Frontier(out));
        }
        for part in s.split(',') {
            let Some((w, n)) = part.rsplit_once(':') else {
                bail!("invalid frontier {s:?}: expected writer:n,writer:n");
            };
            validate_writer(w)?;
            let n: u64 = n
                .parse()
                .with_context(|| format!("invalid frontier {s:?}: {n:?} is not a number"))?;
            out.insert(w.to_string(), n);
        }
        Ok(Frontier(out))
    }
}

impl fmt::Display for Frontier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.encode())
    }
}

impl FromStr for Frontier {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Frontier> {
        Frontier::decode(s)
    }
}

/// A reader is part of a path: letters, digits and `:._@-`
/// (`omar`, `pm`, `epic:payments`).
pub fn validate_reader(r: &str) -> Result<()> {
    let ok = !r.is_empty()
        && !r.starts_with('.')
        && r.len() <= 128
        && r.chars()
            .all(|c| c.is_alphanumeric() || matches!(c, ':' | '.' | '_' | '@' | '-'));
    if !ok {
        bail!(
            "invalid reader {r:?}: use letters, digits and : . _ @ - (for example omar or epic:payments)"
        );
    }
    Ok(())
}

pub fn reader_dir(home: &Path, reader: &str) -> PathBuf {
    home.join("cursors").join(reader)
}

fn read_file(p: &Path) -> Result<Frontier> {
    let text = fs::read_to_string(p).with_context(|| format!("could not read {}", p.display()))?;
    let map: BTreeMap<String, u64> = serde_json::from_str(&text)
        .with_context(|| format!("{} is not a valid cursor", p.display()))?;
    Ok(Frontier(map))
}

fn files(home: &Path, reader: &str) -> Result<Vec<PathBuf>> {
    let dir = reader_dir(home, reader);
    let mut out = Vec::new();
    match fs::read_dir(&dir) {
        Ok(rd) => {
            for ent in rd {
                let p = ent?.path();
                if p.extension().is_some_and(|x| x == "json") && p.is_file() {
                    out.push(p);
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    out.sort();
    Ok(out)
}

/// The effective cursor of a reader: the maximum per file across all its
/// machines. Empty if it never read anything.
pub fn load(home: &Path, reader: &str) -> Result<Frontier> {
    validate_reader(reader)?;
    let mut out = Frontier::default();
    for p in files(home, reader)? {
        out.merge(&read_file(&p)?);
    }
    Ok(out)
}

/// When the cursor of a reader last advanced (on any machine), according to
/// the date of its files.
pub fn last_advanced(home: &Path, reader: &str) -> Option<jiff::Timestamp> {
    let mut best: Option<jiff::Timestamp> = None;
    for p in files(home, reader).ok()? {
        if let Some(t) = fs::metadata(&p)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| jiff::Timestamp::try_from(t).ok())
        {
            best = best.max(Some(t));
        }
    }
    best
}

/// Advances the cursor of `reader` on this machine (`writer`) to `frontier`.
/// It never goes back: the maximum with what is already there is stored.
pub fn advance(home: &Path, reader: &str, writer: &str, frontier: &Frontier) -> Result<()> {
    validate_reader(reader)?;
    validate_writer(writer)?;
    let mut next = load(home, reader)?;
    next.merge(frontier);
    let dir = reader_dir(home, reader);
    fs::create_dir_all(&dir)?;
    let text = serde_json::to_string_pretty(&next.0)? + "\n";
    write_atomic(&dir.join(format!("{writer}.json")), text.as_bytes())
}

/// The entries that a frontier does not cover yet, in `hlc` order.
pub fn new_since<'a>(
    entries: impl IntoIterator<Item = &'a Entry>,
    since: &Frontier,
) -> Vec<&'a Entry> {
    let mut out: Vec<&Entry> = entries.into_iter().filter(|e| !since.covers(e)).collect();
    out.sort_by(|a, b| (&a.hlc, &a.id).cmp(&(&b.hlc, &b.id)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode() {
        let f = Frontier(BTreeMap::from([
            ("omar@mac".to_string(), 412),
            ("ana@laptop".to_string(), 30),
        ]));
        assert_eq!(f.encode(), "ana@laptop:30,omar@mac:412");
        assert_eq!(Frontier::decode(&f.encode()).unwrap(), f);
        assert_eq!(Frontier::default().encode(), "-");
        assert!(Frontier::decode("-").unwrap().is_empty());
        assert!(Frontier::decode("omar@mac").is_err());
        assert!(Frontier::decode("a/b:1").is_err());
    }

    #[test]
    fn effective_cursor_is_max_per_file_and_never_goes_back() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let f = |pairs: &[(&str, u64)]| {
            Frontier(pairs.iter().map(|(w, n)| (w.to_string(), *n)).collect())
        };
        advance(
            home,
            "omar",
            "omar@mac",
            &f(&[("omar@mac", 5), ("ana@laptop", 1)]),
        )
        .unwrap();
        // Another machine of the same reader, synced through git.
        fs::write(
            reader_dir(home, "omar").join("omar@t3.json"),
            r#"{"ana@laptop": 7}"#,
        )
        .unwrap();
        assert_eq!(
            load(home, "omar").unwrap(),
            f(&[("omar@mac", 5), ("ana@laptop", 7)])
        );
        advance(home, "omar", "omar@mac", &f(&[("omar@mac", 2)])).unwrap();
        assert_eq!(load(home, "omar").unwrap().get("omar@mac"), 5);
        // No stray temporary files.
        let names: Vec<String> = fs::read_dir(reader_dir(home, "omar"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(names.iter().all(|n| n.ends_with(".json")), "{names:?}");
        assert!(validate_reader("epic:pagos").is_ok());
        assert!(validate_reader("../x").is_err());
    }
}
