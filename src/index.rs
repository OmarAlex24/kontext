//! SQLite index (DESIGN §6). It is not the truth: it is built from the ledger
//! files and can be deleted at any time.
//!
//! It catches up on open: for each file, if it grew since what was ingested, it
//! reads the new complete lines (a final half-written line is ignored until it
//! ends) in a single transaction.

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::Value;

use crate::model::Entry;
use crate::store::ledger_files;

/// Bump this when the tables or the text they cache change: an older index is
/// then dropped and rebuilt from the ledger files.
const SCHEMA_VERSION: i64 = 4;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS entries (
    id TEXT PRIMARY KEY,
    writer TEXT NOT NULL,
    n INTEGER NOT NULL,
    hlc TEXT NOT NULL,
    kind TEXT NOT NULL,
    title TEXT NOT NULL,
    actor_name TEXT NOT NULL,
    json TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS entries_hlc ON entries(hlc);
CREATE INDEX IF NOT EXISTS entries_kind ON entries(kind);
CREATE TABLE IF NOT EXISTS refs (
    from_id TEXT NOT NULL,
    to_id TEXT NOT NULL,
    pos INTEGER NOT NULL,
    PRIMARY KEY (from_id, pos)
);
CREATE INDEX IF NOT EXISTS refs_to ON refs(to_id);
CREATE TABLE IF NOT EXISTS files (
    writer TEXT PRIMARY KEY,
    bytes_ingested INTEGER NOT NULL,
    last_n INTEGER NOT NULL
);
CREATE VIRTUAL TABLE IF NOT EXISTS entries_fts USING fts5(id UNINDEXED, title, body, why, tags);
-- Phase 4: the blob of each path at each HEAD, to know quickly whether a fact
-- has expired (NULL blob: the path does not exist at that HEAD).
CREATE TABLE IF NOT EXISTS fact_blobs (
    repo TEXT NOT NULL,
    path TEXT NOT NULL,
    head TEXT NOT NULL,
    blob TEXT,
    PRIMARY KEY (repo, path, head)
);
-- Phase 4: the docs of `kontext find`. A file is read again only if its date or
-- its size changed.
CREATE TABLE IF NOT EXISTS docs_files (
    id INTEGER PRIMARY KEY,
    repo TEXT NOT NULL,
    path TEXT NOT NULL,
    mtime INTEGER NOT NULL,
    size INTEGER NOT NULL,
    status TEXT NOT NULL,
    status_label TEXT NOT NULL,
    supersedes TEXT NOT NULL,
    UNIQUE (repo, path)
);
CREATE TABLE IF NOT EXISTS docs_sections (
    id INTEGER PRIMARY KEY,
    file_id INTEGER NOT NULL,
    heading TEXT NOT NULL,
    heading_path TEXT NOT NULL,
    start_line INTEGER NOT NULL,
    end_line INTEGER NOT NULL,
    body TEXT NOT NULL,
    read_when TEXT NOT NULL,
    refs TEXT NOT NULL,
    len_body INTEGER NOT NULL,
    len_heading INTEGER NOT NULL,
    len_path INTEGER NOT NULL,
    len_read_when INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS docs_sections_file ON docs_sections(file_id);
-- Which rules (top-level folders, historical paths) each repo was read with: if
-- they change, its docs are read again.
CREATE TABLE IF NOT EXISTS docs_repos (repo TEXT PRIMARY KEY, rules TEXT NOT NULL);
-- The terms already normalized (no accents, singular, no stop words) of each
-- section; the rowid is the id in docs_sections.
CREATE VIRTUAL TABLE IF NOT EXISTS docs_fts USING fts5(body, heading, path, read_when);
-- Phase 5: the result of verifying each signature. The key is the sha256 of
-- what decides the result (id, hash, signature, person and [people] keys), so
-- it is never invalidated: if anything changes, the key changes.
CREATE TABLE IF NOT EXISTS sig_checks (
    key TEXT PRIMARY KEY,
    ok INTEGER NOT NULL,
    reason TEXT NOT NULL
);
"#;

const DROP: &str = r#"
DROP TABLE IF EXISTS entries;
DROP TABLE IF EXISTS refs;
DROP TABLE IF EXISTS files;
DROP TABLE IF EXISTS entries_fts;
DROP TABLE IF EXISTS fact_blobs;
DROP TABLE IF EXISTS docs_files;
DROP TABLE IF EXISTS docs_sections;
DROP TABLE IF EXISTS docs_fts;
DROP TABLE IF EXISTS docs_repos;
DROP TABLE IF EXISTS sig_checks;
"#;

pub struct Index {
    conn: Connection,
    ledger_dir: PathBuf,
}

/// How many new entries were ingested.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CatchUp {
    pub entries: u64,
    pub rebuilt: bool,
}

impl Index {
    /// Opens (or creates) `<home>/.kontext/index.sqlite` and catches it up.
    pub fn open(home: &Path) -> Result<Index> {
        let dir = crate::config::ensure_internal_dir(home)?;
        let path = dir.join("index.sqlite");
        let conn = Connection::open(&path)
            .with_context(|| format!("could not open the index {}", path.display()))?;
        conn.busy_timeout(Duration::from_secs(5))?;
        // Switching to WAL needs a lock; if another process holds it, it is retried.
        let mut tries = 0;
        loop {
            match conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get::<_, String>(0)) {
                Ok(_) => break,
                Err(e) if tries < 50 && is_busy(&e) => {
                    tries += 1;
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => return Err(e.into()),
            }
        }
        conn.execute_batch("PRAGMA synchronous=NORMAL;")?;
        let mut idx = Index {
            conn,
            ledger_dir: home.join("ledger"),
        };
        idx.migrate()?;
        idx.catch_up()?;
        Ok(idx)
    }

    fn migrate(&mut self) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let version: i64 = tx.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version != SCHEMA_VERSION {
            if version != 0 {
                tx.execute_batch(DROP)?;
            }
            tx.execute_batch(SCHEMA)?;
            tx.execute_batch(&format!("PRAGMA user_version={SCHEMA_VERSION}"))?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Ingests what is new in each ledger file.
    pub fn catch_up(&mut self) -> Result<CatchUp> {
        self.ingest(false)
    }

    /// Deletes everything and rebuilds it from the files. It also deletes what
    /// is stored for docs and fingerprints: it is computed again on read.
    pub fn rebuild(&mut self) -> Result<CatchUp> {
        let r = self.ingest(true)?;
        self.conn.execute_batch(
            "DELETE FROM fact_blobs; DELETE FROM docs_files; DELETE FROM docs_sections; DELETE FROM docs_fts; DELETE FROM docs_repos; DELETE FROM sig_checks;",
        )?;
        Ok(r)
    }

    /// The connection, for the phase 4 tables (docs and fingerprints).
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    pub fn conn_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }

    /// The stored blob of `path` at the `head` of `repo`: `None` if it has not
    /// been seen, `Some(None)` if the path did not exist.
    pub fn fact_blob(&self, repo: &str, path: &str, head: &str) -> Result<Option<Option<String>>> {
        Ok(self
            .conn
            .query_row(
                "SELECT blob FROM fact_blobs WHERE repo = ?1 AND path = ?2 AND head = ?3",
                params![repo, path, head],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?)
    }

    pub fn put_fact_blob(
        &self,
        repo: &str,
        path: &str,
        head: &str,
        blob: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO fact_blobs (repo, path, head, blob) VALUES (?1, ?2, ?3, ?4)",
            params![repo, path, head, blob],
        )?;
        Ok(())
    }

    /// The signatures already verified (phase 5).
    pub fn sig_checks(&self) -> Result<std::collections::HashMap<String, crate::sign::Verdict>> {
        let mut st = self
            .conn
            .prepare("SELECT key, ok, reason FROM sig_checks")?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let mut out = std::collections::HashMap::new();
        for row in rows {
            let (k, ok, reason) = row?;
            let v = if ok != 0 {
                crate::sign::Verdict::Valid
            } else {
                crate::sign::Verdict::Invalid(reason)
            };
            out.insert(k, v);
        }
        Ok(out)
    }

    pub fn put_sig_check(&self, key: &str, v: &crate::sign::Verdict) -> Result<()> {
        let (ok, reason) = match v {
            crate::sign::Verdict::Valid => (1, ""),
            crate::sign::Verdict::Invalid(r) => (0, r.as_str()),
        };
        self.conn.execute(
            "INSERT OR REPLACE INTO sig_checks (key, ok, reason) VALUES (?1, ?2, ?3)",
            params![key, ok, reason],
        )?;
        Ok(())
    }

    fn ingest(&mut self, mut full: bool) -> Result<CatchUp> {
        let files = ledger_files(&self.ledger_dir)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if !full {
            // If a file shrank or disappeared (git reset, another branch), what
            // was ingested no longer matches: everything is built again.
            let known: Vec<(String, i64)> = {
                let mut st = tx.prepare("SELECT writer, bytes_ingested FROM files")?;
                st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<rusqlite::Result<_>>()?
            };
            for (writer, bytes) in known {
                let size = files
                    .iter()
                    .find(|(w, _)| *w == writer)
                    .map(|(_, p)| fs::metadata(p).map(|m| m.len()).unwrap_or(0));
                if size.is_none_or(|s| (s as i64) < bytes) {
                    full = true;
                    break;
                }
            }
        }
        if full {
            tx.execute_batch(
                "DELETE FROM entries; DELETE FROM refs; DELETE FROM files; DELETE FROM entries_fts;",
            )?;
        }
        let mut total = 0;
        for (writer, path) in &files {
            let (ingested, last_n): (i64, i64) = tx
                .query_row(
                    "SELECT bytes_ingested, last_n FROM files WHERE writer = ?1",
                    [writer],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?
                .unwrap_or((0, 0));
            let (ingested, last_n) = (ingested as u64, last_n as u64);
            let size = fs::metadata(path)?.len();
            if size <= ingested {
                continue;
            }
            let mut f = File::open(path)?;
            f.seek(SeekFrom::Start(ingested))?;
            let mut buf = Vec::with_capacity((size - ingested) as usize);
            f.read_to_end(&mut buf)?;
            let Some(end) = buf.iter().rposition(|b| *b == b'\n').map(|i| i + 1) else {
                continue;
            };
            let mut last = last_n;
            for line in buf[..end].split(|b| *b == b'\n') {
                if let Some(e) = parse_line(line) {
                    last = last.max(e.n);
                    if insert(&tx, &e, line)? {
                        total += 1;
                    }
                }
            }
            tx.execute(
                "INSERT INTO files (writer, bytes_ingested, last_n) VALUES (?1, ?2, ?3)
                 ON CONFLICT(writer) DO UPDATE SET bytes_ingested = ?2, last_n = ?3",
                params![writer, (ingested + end as u64) as i64, last as i64],
            )?;
        }
        tx.commit()?;
        Ok(CatchUp {
            entries: total,
            rebuilt: full,
        })
    }

    pub fn count(&self) -> Result<u64> {
        let n: i64 = self
            .conn
            .query_row("SELECT count(*) FROM entries", [], |r| r.get(0))?;
        Ok(n as u64)
    }

    /// The stored JSON of an entry.
    pub fn get_value(&self, id: &str) -> Result<Option<Value>> {
        let json: Option<String> = self
            .conn
            .query_row("SELECT json FROM entries WHERE id = ?1", [id], |r| r.get(0))
            .optional()?;
        Ok(json.and_then(|j| serde_json::from_str(&j).ok()))
    }

    /// The highest `hlc` of the whole project (clock receive rule).
    pub fn max_hlc(&self) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT max(hlc) FROM entries", [], |r| r.get(0))?)
    }

    /// All entries, in `hlc` order and then `id`.
    pub fn all_entries(&self) -> Result<Vec<Entry>> {
        let mut st = self
            .conn
            .prepare("SELECT json FROM entries ORDER BY hlc, id")?;
        let rows = st.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for json in rows {
            let json = json?;
            if let Some(e) = parse_line(json.as_bytes()) {
                out.push(e);
            }
        }
        Ok(out)
    }

    /// Full-text search (FTS5). The later phases use it; here it serves to check
    /// that the text index gets filled.
    pub fn search(&self, query: &str) -> Result<Vec<String>> {
        let mut st = self
            .conn
            .prepare("SELECT id FROM entries_fts WHERE entries_fts MATCH ?1 ORDER BY rank")?;
        let ids = st
            .query_map([query], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(ids)
    }

    /// How far each ledger file was ingested: the current "frontier"
    /// (DESIGN §7.3 and §7.5).
    pub fn frontier(&self) -> Result<std::collections::BTreeMap<String, u64>> {
        let mut st = self
            .conn
            .prepare("SELECT writer, last_n FROM files WHERE last_n > 0")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        let mut out = std::collections::BTreeMap::new();
        for row in rows {
            let (w, n) = row?;
            out.insert(w, n as u64);
        }
        Ok(out)
    }

    /// The entries that cite `id`.
    pub fn referrers(&self, id: &str) -> Result<Vec<String>> {
        let mut st = self
            .conn
            .prepare("SELECT from_id FROM refs WHERE to_id = ?1 ORDER BY from_id")?;
        let ids = st
            .query_map([id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(ids)
    }
}

fn is_busy(e: &rusqlite::Error) -> bool {
    matches!(
        e.sqlite_error_code(),
        Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
    )
}

fn parse_line(line: &[u8]) -> Option<Entry> {
    if line.iter().all(u8::is_ascii_whitespace) {
        return None;
    }
    serde_json::from_slice(line).ok()
}

/// Inserts an entry. If the `id` was already there, the first one stays
/// (`verify` reports the collision).
fn insert(tx: &rusqlite::Transaction<'_>, e: &Entry, line: &[u8]) -> Result<bool> {
    let json = String::from_utf8_lossy(line);
    let added = tx.execute(
        "INSERT OR IGNORE INTO entries (id, writer, n, hlc, kind, title, actor_name, json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            e.id,
            e.writer,
            e.n as i64,
            e.hlc,
            e.kind,
            e.title,
            e.actor.name,
            json
        ],
    )?;
    if added == 0 {
        return Ok(false);
    }
    for (pos, to) in e.refs.iter().enumerate() {
        tx.execute(
            "INSERT OR IGNORE INTO refs (from_id, to_id, pos) VALUES (?1, ?2, ?3)",
            params![e.id, to, pos as i64],
        )?;
    }
    tx.execute(
        "INSERT INTO entries_fts (id, title, body, why, tags) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![e.id, e.title, e.body, e.why, e.tags.join(" ")],
    )?;
    Ok(true)
}
