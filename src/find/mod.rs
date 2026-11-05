//! `kontext find` (DESIGN §10): searches the docs of the project's repos and
//! reads only what matters: BM25 per field over headings, "Read when", path
//! and body, with accents ignored, light plurals and at most two sections per
//! file.
//!
//! - The docs come from `docs = [...]` in each repo of `[[repos]]` that is on
//!   this machine (by default `**/*.md`), respecting `.gitignore` and without
//!   `node_modules`, `target` or `.git`.
//! - Sections are stored in the index (`docs_files`, `docs_sections` and
//!   `docs_fts`). A file is read again only if its date or size changed, or if
//!   the repo's rules changed.
//! - The ranking is in [`search`]; `docs_fts` stores the
//!   already normalized terms and is used to find the candidates and their
//!   frequency without reading the whole corpus.
//! - Synonyms come from `<kontext-home>/synonyms.toml`.

pub mod diacritic;
pub mod parse;
pub mod search;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result, bail};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use indexmap::IndexMap;
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::config::Repo;
use crate::git;
use crate::index::Index;
use parse::{FileStatus, Rules, Status, StatusInfo};
use search::{Counts, Stats, Synonyms, SynonymsFile};

/// Changing this changes how docs are split or normalized: it forces them to
/// be read again.
const PARSER_VERSION: &str = "1";
pub const SHOW_MAX_LINES: usize = 200;
const SNIPPET_CHARS: usize = 160;
/// At most this many sections per file, so that a long doc does not fill the
/// page.
const PER_FILE: usize = 2;
/// Bits of a section's id used for its position within the file.
const SECTION_BITS: i64 = 20;

/// Folders that are never read.
const SKIP_DIRS: &[&str] = &[".git", "node_modules", "target"];

/// A project repo that is on this machine.
#[derive(Debug, Clone)]
pub struct LocalRepo {
    pub cfg: Repo,
    pub root: PathBuf,
    /// Files and folders that git tracks (to know whether a cited path
    /// exists). `None` if it is not a git repo.
    tracked: Option<(BTreeSet<String>, BTreeSet<String>)>,
}

impl LocalRepo {
    pub fn new(cfg: Repo, root: PathBuf) -> LocalRepo {
        let tracked = git::tracked_files(&root).map(|files| {
            let mut dirs = BTreeSet::new();
            for f in &files {
                let mut cur = f.as_str();
                while let Some(i) = cur.rfind('/') {
                    cur = &cur[..i];
                    if !dirs.insert(cur.to_string()) {
                        break;
                    }
                }
            }
            (files.into_iter().collect(), dirs)
        });
        LocalRepo { cfg, root, tracked }
    }

    pub fn name(&self) -> &str {
        &self.cfg.name
    }

    /// Does the path exist? With git, if it tracks it (file or folder); without
    /// git, if it is on disk.
    pub fn exists(&self, r: &str) -> bool {
        match &self.tracked {
            Some((files, dirs)) => files.contains(r) || dirs.contains(r),
            None => self.root.join(r).exists(),
        }
    }

    /// The top-level folders: `roots` from project.toml, or the ones git
    /// tracks, or the ones on disk.
    fn roots(&self) -> Vec<String> {
        if !self.cfg.roots.is_empty() {
            return self.cfg.roots.clone();
        }
        let mut out = BTreeSet::new();
        match &self.tracked {
            Some((files, _)) => {
                for f in files {
                    if let Some((top, _)) = f.split_once('/') {
                        out.insert(top.to_string());
                    }
                }
            }
            None => {
                if let Ok(rd) = fs::read_dir(&self.root) {
                    for e in rd.flatten() {
                        let name = e.file_name().to_string_lossy().into_owned();
                        if e.path().is_dir() && !SKIP_DIRS.contains(&name.as_str()) {
                            out.insert(name);
                        }
                    }
                }
            }
        }
        out.into_iter().collect()
    }

    fn rules(&self) -> Result<(Rules, String)> {
        let roots = self.roots();
        let rules = Rules::new(&self.cfg.historical, &roots)?;
        let mut h = Sha256::new();
        h.update(PARSER_VERSION.as_bytes());
        for part in [&self.cfg.historical, &roots] {
            h.update(b"\0");
            for x in part {
                h.update(x.as_bytes());
                h.update(b"\n");
            }
        }
        let key = h
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        Ok((rules, key))
    }
}

fn globset(patterns: &[String], what: &str) -> Result<GlobSet> {
    let mut b = GlobSetBuilder::new();
    for p in patterns {
        b.add(
            GlobBuilder::new(p)
                .literal_separator(true)
                .build()
                .with_context(|| format!("{what} = {p:?} is not a valid glob"))?,
        );
    }
    Ok(b.build()?)
}

/// The docs of a repo: relative path, date (ns) and size.
fn walk_docs(repo: &LocalRepo) -> Result<Vec<(String, i64, i64)>> {
    let docs = if repo.cfg.docs.is_empty() {
        vec!["**/*.md".to_string()]
    } else {
        repo.cfg.docs.clone()
    };
    let include = globset(&docs, "docs")?;
    let exclude = globset(&repo.cfg.exclude, "exclude")?;
    let mut out = Vec::new();
    let walker = ignore::WalkBuilder::new(&repo.root)
        .hidden(false)
        .follow_links(false)
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            !(e.file_type().is_some_and(|t| t.is_dir()) && SKIP_DIRS.contains(&name.as_ref()))
        })
        .build();
    for ent in walker {
        let Ok(ent) = ent else { continue };
        if !ent.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let Ok(rel) = ent.path().strip_prefix(&repo.root) else {
            continue;
        };
        let rel = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        if !include.is_match(&rel) || exclude.is_match(&rel) {
            continue;
        }
        let Ok(meta) = ent.metadata() else { continue };
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0);
        out.push((rel, mtime, meta.len() as i64));
    }
    out.sort();
    Ok(out)
}

/// What [`sync`] did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Synced {
    pub files: usize,
    pub parsed: usize,
    pub removed: usize,
}

fn delete_file(tx: &rusqlite::Transaction, id: i64) -> Result<()> {
    let lo = id << SECTION_BITS;
    let hi = (id + 1) << SECTION_BITS;
    tx.execute(
        "DELETE FROM docs_fts WHERE rowid >= ?1 AND rowid < ?2",
        params![lo, hi],
    )?;
    tx.execute("DELETE FROM docs_sections WHERE file_id = ?1", [id])?;
    tx.execute("DELETE FROM docs_files WHERE id = ?1", [id])?;
    Ok(())
}

fn joined_tokens(text: &str) -> (String, usize) {
    let toks = search::tokenize(text);
    (toks.join(" "), toks.len())
}

/// Brings the docs index of a repo up to date.
pub fn sync(index: &mut Index, repo: &LocalRepo) -> Result<Synced> {
    let (rules, key) = repo.rules()?;
    let files = walk_docs(repo)?;
    let conn = index.conn_mut();
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let old_rules: Option<String> = tx
        .query_row(
            "SELECT rules FROM docs_repos WHERE repo = ?1",
            [repo.name()],
            |r| r.get(0),
        )
        .optional()?;
    let mut known: HashMap<String, (i64, i64, i64)> = HashMap::new();
    {
        let mut st = tx.prepare("SELECT id, path, mtime, size FROM docs_files WHERE repo = ?1")?;
        let rows = st.query_map([repo.name()], |r| {
            Ok((
                r.get::<_, String>(1)?,
                (
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                ),
            ))
        })?;
        for row in rows {
            let (p, v) = row?;
            known.insert(p, v);
        }
    }
    let mut out = Synced {
        files: files.len(),
        ..Synced::default()
    };
    let force = old_rules.as_deref() != Some(key.as_str());
    let present: BTreeSet<&str> = files.iter().map(|(p, _, _)| p.as_str()).collect();
    for (path, (id, _, _)) in &known {
        if force || !present.contains(path.as_str()) {
            delete_file(&tx, *id)?;
            if !present.contains(path.as_str()) {
                out.removed += 1;
            }
        }
    }
    for (path, mtime, size) in &files {
        if !force && let Some((id, m, s)) = known.get(path) {
            if m == mtime && s == size {
                continue;
            }
            delete_file(&tx, *id)?;
        }
        let bytes = fs::read(repo.root.join(path))
            .with_context(|| format!("could not read {}", repo.root.join(path).display()))?;
        let text = String::from_utf8_lossy(&bytes);
        let parsed = parse::parse_markdown(path, &text, &rules);
        tx.execute(
            "INSERT INTO docs_files (repo, path, mtime, size, status, status_label, supersedes)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                repo.name(),
                path,
                mtime,
                size,
                parsed.status.status.as_str(),
                parsed.status.label,
                serde_json::to_string(&parsed.supersedes)?,
            ],
        )?;
        let file_id = tx.last_insert_rowid();
        let (path_tokens, path_len) = joined_tokens(path);
        let (rw_tokens, rw_len) = joined_tokens(&parsed.read_when);
        for (i, s) in parsed.sections.iter().enumerate() {
            let id = (file_id << SECTION_BITS) + i as i64;
            let (body_tokens, body_len) = joined_tokens(&s.body);
            let (head_tokens, head_len) = joined_tokens(&s.heading_path.join(" "));
            tx.execute(
                "INSERT INTO docs_sections (id, file_id, heading, heading_path, start_line, end_line,
                   body, read_when, refs, len_body, len_heading, len_path, len_read_when)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    id,
                    file_id,
                    s.heading,
                    serde_json::to_string(&s.heading_path)?,
                    s.start_line as i64,
                    s.end_line as i64,
                    s.body,
                    parsed.read_when,
                    serde_json::to_string(&s.refs)?,
                    body_len as i64,
                    head_len as i64,
                    path_len as i64,
                    rw_len as i64,
                ],
            )?;
            tx.execute(
                "INSERT INTO docs_fts (rowid, body, heading, path, read_when) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![id, body_tokens, head_tokens, path_tokens, rw_tokens],
            )?;
        }
        out.parsed += 1;
    }
    tx.execute(
        "INSERT OR REPLACE INTO docs_repos (repo, rules) VALUES (?1, ?2)",
        params![repo.name(), key],
    )?;
    tx.commit()?;
    Ok(out)
}

/// The repos that are searched, already up to date in the index.
pub struct Corpus<'a> {
    index: &'a Index,
    pub repos: Vec<LocalRepo>,
    /// File → (repo, path, final status, after the ADR supersessions).
    files: HashMap<i64, (String, String, StatusInfo)>,
}

impl<'a> Corpus<'a> {
    /// Brings the docs of `repos` up to date and loads the status of each file.
    pub fn open(index: &'a mut Index, repos: Vec<LocalRepo>) -> Result<Corpus<'a>> {
        for r in &repos {
            sync(index, r)?;
        }
        let index: &'a Index = index;
        let names: BTreeSet<&str> = repos.iter().map(LocalRepo::name).collect();
        let mut by_repo: BTreeMap<String, Vec<(i64, FileStatus)>> = BTreeMap::new();
        {
            let mut st = index.conn().prepare(
                "SELECT id, repo, path, status, status_label, supersedes FROM docs_files ORDER BY repo, path",
            )?;
            let rows = st.query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                ))
            })?;
            for row in rows {
                let (id, repo, path, status, label, sup) = row?;
                if !names.contains(repo.as_str()) {
                    continue;
                }
                by_repo.entry(repo).or_default().push((
                    id,
                    FileStatus {
                        path,
                        status: StatusInfo {
                            label,
                            status: Status::parse(&status),
                        },
                        supersedes: serde_json::from_str(&sup).unwrap_or_default(),
                    },
                ));
            }
        }
        let mut files = HashMap::new();
        for (repo, list) in by_repo {
            let (ids, mut statuses): (Vec<i64>, Vec<FileStatus>) = list.into_iter().unzip();
            parse::apply_supersession(&mut statuses);
            for (id, f) in ids.into_iter().zip(statuses) {
                files.insert(id, (repo.clone(), f.path, f.status));
            }
        }
        Ok(Corpus {
            index,
            repos,
            files,
        })
    }

    fn multi(&self) -> bool {
        self.repos.len() > 1
    }

    fn repo(&self, name: &str) -> Option<&LocalRepo> {
        self.repos.iter().find(|r| r.name() == name)
    }

    /// `path:start-end`, with `repo:` in front if there is more than one repo.
    pub fn location(&self, repo: &str, path: &str, start: usize, end: usize) -> String {
        if self.multi() {
            format!("{repo}:{path}:{start}-{end}")
        } else {
            format!("{path}:{start}-{end}")
        }
    }

    fn doc_label(&self, repo: &str, path: &str) -> String {
        if self.multi() {
            format!("{repo}:{path}")
        } else {
            path.to_string()
        }
    }

    fn stats(&self, terms: &IndexMap<String, f64>) -> Result<(Stats, BTreeSet<i64>)> {
        let mut stats = Stats::default();
        let mut totals = [0usize; 4];
        {
            let mut st = self.index.conn().prepare(
                "SELECT file_id, len_body, len_heading, len_path, len_read_when FROM docs_sections",
            )?;
            let rows = st.query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    [
                        r.get::<_, i64>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, i64>(4)?,
                    ],
                ))
            })?;
            for row in rows {
                let (file, lens) = row?;
                if !self.files.contains_key(&file) {
                    continue;
                }
                stats.n += 1;
                for (t, l) in totals.iter_mut().zip(lens) {
                    *t += l as usize;
                }
            }
        }
        for (avg, total) in stats.avg.iter_mut().zip(totals) {
            *avg = search::average(total, stats.n);
        }
        let mut candidates = BTreeSet::new();
        let mut st = self
            .index
            .conn()
            .prepare("SELECT rowid FROM docs_fts WHERE docs_fts MATCH ?1")?;
        for term in terms.keys() {
            let q = format!("\"{term}\"");
            let ids: Vec<i64> = st
                .query_map([q], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            let mut df = 0;
            for id in ids {
                if self.files.contains_key(&(id >> SECTION_BITS)) {
                    df += 1;
                    candidates.insert(id);
                }
            }
            if df > 0 {
                stats.df.insert(term.clone(), df);
            }
        }
        Ok((stats, candidates))
    }

    fn section(&self, id: i64) -> Result<Option<Hit>> {
        let Some((repo, path, status)) = self.files.get(&(id >> SECTION_BITS)) else {
            return Ok(None);
        };
        let row = self
            .index
            .conn()
            .query_row(
                "SELECT heading, start_line, end_line, body, refs FROM docs_sections WHERE id = ?1",
                [id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                    ))
                },
            )
            .optional()?;
        Ok(row.map(|(heading, start, end, body, refs)| Hit {
            id,
            repo: repo.clone(),
            path: path.clone(),
            heading,
            start_line: start as usize,
            end_line: end as usize,
            body,
            refs: serde_json::from_str(&refs).unwrap_or_default(),
            status: status.clone(),
            score: 0.0,
        }))
    }

    /// The sections that best answer `query`.
    pub fn search(
        &self,
        query: &str,
        synonyms: &Synonyms,
        all: bool,
        limit: usize,
    ) -> Result<Vec<Hit>> {
        let terms = synonyms.expand(query);
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        let (stats, candidates) = self.stats(&terms)?;
        // Sections are ordered by file path and then by
        // position; that way ties come out the same.
        let mut ordered: Vec<(String, String, i64)> = candidates
            .into_iter()
            .filter_map(|id| {
                let (repo, path, status) = self.files.get(&(id >> SECTION_BITS))?;
                (all || status.status != Status::Historical)
                    .then(|| (repo.clone(), path.clone(), id))
            })
            .collect();
        ordered.sort();
        let mut tok = self
            .index
            .conn()
            .prepare("SELECT body, heading, path, read_when FROM docs_fts WHERE rowid = ?1")?;
        let mut scored: Vec<(f64, i64)> = Vec::new();
        for (_, _, id) in ordered {
            let fields: Option<[String; 4]> = tok
                .query_row([id], |r| Ok([r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?]))
                .optional()?;
            let Some(fields) = fields else { continue };
            let counts =
                Counts::from_tokens([&fields[0], &fields[1], &fields[2], &fields[3]], &terms);
            let status = &self.files[&(id >> SECTION_BITS)].2;
            let s =
                search::score(&stats, &counts, &terms) * search::status_multiplier(status.status);
            if s > 0.0 {
                scored.push((s, id));
            }
        }
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        let mut per_file: HashMap<i64, usize> = HashMap::new();
        let mut hits = Vec::new();
        for (score, id) in scored {
            let used = per_file.entry(id >> SECTION_BITS).or_default();
            if *used >= PER_FILE {
                continue;
            }
            *used += 1;
            if let Some(mut h) = self.section(id)? {
                h.score = score;
                hits.push(h);
            }
            if hits.len() >= limit {
                break;
            }
        }
        Ok(hits)
    }

    /// The code paths that a section cites and that still exist.
    pub fn code_pointers(&self, h: &Hit) -> Vec<String> {
        let Some(repo) = self.repo(&h.repo) else {
            return Vec::new();
        };
        h.refs
            .iter()
            .filter(|r| !r.ends_with(".md") && repo.exists(r))
            .cloned()
            .collect()
    }

    /// The files (that are not Markdown) that cite a doc: ADRs by their number
    /// and everything else by its path.
    pub fn cited_by(&self, h: &Hit, limit: usize) -> Vec<String> {
        match self.repo(&h.repo) {
            Some(repo) => cited_by(&repo.root, &h.path, limit),
            None => Vec::new(),
        }
    }

    /// The section that contains `[repo:]path:line`.
    pub fn show(&self, target: &str) -> Result<Option<Hit>> {
        let parts: Vec<&str> = target.split(':').collect();
        let (repo, path, line) = match parts.as_slice() {
            [repo, path, line, ..] if self.repo(repo).is_some() => {
                (Some(*repo), *path, Some(*line))
            }
            [path, line, ..] => (None, *path, Some(*line)),
            [path] => (None, *path, None),
            [] => bail!("--show needs <path:line>"),
        };
        // `310-346` (what search prints) counts as line 310.
        let line: usize = line
            .map(|l| {
                l.chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>()
            })
            .and_then(|l| l.parse().ok())
            .filter(|n| *n > 0)
            .unwrap_or(1);
        let mut st = self.index.conn().prepare(
            "SELECT s.id FROM docs_sections s JOIN docs_files f ON f.id = s.file_id
             WHERE f.path = ?1 AND s.start_line <= ?2 AND ?2 <= s.end_line ORDER BY f.repo, s.id",
        )?;
        let ids: Vec<i64> = st
            .query_map(params![path, line as i64], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for id in ids {
            if let Some(h) = self.section(id)?
                && repo.is_none_or(|r| r == h.repo)
            {
                return Ok(Some(h));
            }
        }
        Ok(None)
    }

    /// Paths cited by current (not historical) docs that no longer exist.
    pub fn broken_refs(&self) -> Result<Vec<BrokenRef>> {
        let mut st = self
            .index
            .conn()
            .prepare("SELECT id, start_line, refs FROM docs_sections")?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, start, refs) = row?;
            let Some((repo, path, status)) = self.files.get(&(id >> SECTION_BITS)) else {
                continue;
            };
            if status.status == Status::Historical {
                continue;
            }
            let Some(local) = self.repo(repo) else {
                continue;
            };
            let refs: Vec<String> = serde_json::from_str(&refs).unwrap_or_default();
            for r in refs {
                if !local.exists(&r) {
                    out.push(BrokenRef {
                        doc: self.doc_label(repo, path),
                        line: start as usize,
                        reference: r,
                        order: (repo.clone(), path.clone(), id),
                    });
                }
            }
        }
        out.sort_by(|a, b| a.order.cmp(&b.order));
        Ok(out)
    }
}

/// `git grep -l` for the doc in non-Markdown files.
fn cited_by(root: &Path, doc: &str, limit: usize) -> Vec<String> {
    let needle = citation_needle(doc);
    git::run(
        root,
        &["grep", "-l", "-F", "-e", &needle, "--", ".", ":!*.md"],
    )
    .ok()
    .flatten()
    .unwrap_or_default()
    .lines()
    .filter(|l| !l.is_empty())
    .take(limit)
    .map(str::to_string)
    .collect()
}

/// The ADR number (`ADR-0102`) or the path that code cites a doc by.
pub fn citation_needle(path: &str) -> String {
    static ADR: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"(ADR-[0-9]{4})").unwrap());
    ADR.captures(path)
        .map(|c| c[1].to_string())
        .unwrap_or_else(|| path.to_string())
}

/// A section that was found.
#[derive(Debug, Clone)]
pub struct Hit {
    id: i64,
    pub repo: String,
    pub path: String,
    pub heading: String,
    pub start_line: usize,
    pub end_line: usize,
    pub body: String,
    pub refs: Vec<String>,
    pub status: StatusInfo,
    pub score: f64,
}

impl Hit {
    /// The lines after the heading, without Markdown marks, on one line.
    pub fn snippet(&self) -> String {
        static MARKS: std::sync::LazyLock<regex::Regex> =
            std::sync::LazyLock::new(|| regex::Regex::new(r"[#>*`|_\[\]]+").unwrap());
        let rest = self.body.split('\n').skip(1).collect::<Vec<_>>().join(" ");
        let text = MARKS.replace_all(&rest, " ");
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        // Length is counted as in JavaScript: in UTF-16 units.
        if text.encode_utf16().count() > SNIPPET_CHARS {
            let mut units = 0;
            let cut: String = text
                .chars()
                .take_while(|c| {
                    units += c.len_utf16();
                    units <= SNIPPET_CHARS
                })
                .collect();
            format!("{cut}…")
        } else {
            text
        }
    }

    pub fn section_id(&self) -> i64 {
        self.id
    }
}

/// A cited path that no longer exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokenRef {
    pub doc: String,
    pub line: usize,
    pub reference: String,
    order: (String, String, i64),
}

/// How many broken paths each doc has (what the baseline stores).
pub fn count_by_doc(broken: &[BrokenRef]) -> BTreeMap<String, usize> {
    let mut m = BTreeMap::new();
    for b in broken {
        *m.entry(b.doc.clone()).or_default() += 1;
    }
    m
}

/// The baseline: `<n> <doc>` per line.
pub fn parse_baseline(text: &str) -> BTreeMap<String, usize> {
    let mut m = BTreeMap::new();
    for line in text.lines() {
        let mut it = line.split_whitespace();
        if let (Some(n), Some(doc), None) = (it.next(), it.next(), it.next())
            && let Ok(n) = n.parse()
        {
            m.insert(doc.to_string(), n);
        }
    }
    m
}

pub fn format_baseline(counts: &BTreeMap<String, usize>) -> String {
    let mut s: String = counts.iter().map(|(d, n)| format!("{n} {d}\n")).collect();
    if s.is_empty() {
        s.push('\n');
    }
    s
}

/// Docs with more broken paths than the baseline, and whether any improved.
pub fn compare_baseline(
    current: &BTreeMap<String, usize>,
    baseline: &BTreeMap<String, usize>,
) -> (Vec<(String, usize, usize)>, bool) {
    let regressions = current
        .iter()
        .filter_map(|(doc, found)| {
            let allowed = baseline.get(doc).copied().unwrap_or(0);
            (*found > allowed).then(|| (doc.clone(), *found, allowed))
        })
        .collect();
    let improved = baseline
        .iter()
        .any(|(doc, allowed)| current.get(doc).copied().unwrap_or(0) < *allowed);
    (regressions, improved)
}

/// The synonyms from `<kontext-home>/synonyms.toml` (empty if it does not exist).
pub fn load_synonyms(home: &Path) -> Result<Synonyms> {
    let p = home.join("synonyms.toml");
    match fs::read_to_string(&p) {
        Ok(s) => {
            let f: SynonymsFile =
                toml::from_str(&s).with_context(|| format!("could not read {}", p.display()))?;
            Ok(Synonyms::new(&f.groups))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Synonyms::default()),
        Err(e) => Err(e).with_context(|| format!("could not read {}", p.display())),
    }
}

/// A search result, as `--json` prints it.
#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Found {
    pub cited_by: Vec<String>,
    pub code: Vec<String>,
    pub heading: String,
    pub location: String,
    pub repo: String,
    /// Two decimals; an integer comes out without `.0`, as in JavaScript.
    pub score: serde_json::Number,
    pub snippet: String,
    pub status: Status,
    pub status_label: String,
}

/// The results with their pointers to code and who cites them (each `git
/// grep` on its own thread).
pub fn results(corpus: &Corpus, hits: &[Hit]) -> Vec<Found> {
    let jobs: Vec<Option<(PathBuf, String)>> = hits
        .iter()
        .map(|h| {
            corpus
                .repo(&h.repo)
                .map(|r| (r.root.clone(), h.path.clone()))
        })
        .collect();
    let cited: Vec<Vec<String>> = std::thread::scope(|s| {
        let handles: Vec<_> = jobs
            .iter()
            .map(|j| {
                s.spawn(move || match j {
                    Some((root, path)) => cited_by(root, path, 3),
                    None => Vec::new(),
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_default())
            .collect()
    });
    hits.iter()
        .zip(cited)
        .map(|(h, cited_by)| Found {
            cited_by,
            code: corpus.code_pointers(h).into_iter().take(5).collect(),
            heading: h.heading.clone(),
            location: corpus.location(&h.repo, &h.path, h.start_line, h.end_line),
            repo: h.repo.clone(),
            score: js_number((h.score * 100.0).round() / 100.0),
            snippet: h.snippet(),
            status: h.status.status,
            status_label: h.status.label.clone(),
        })
        .collect()
}

fn js_number(x: f64) -> serde_json::Number {
    if x.fract() == 0.0 && x.abs() < 1e15 {
        serde_json::Number::from(x as i64)
    } else {
        serde_json::Number::from_f64(x).unwrap_or_else(|| 0.into())
    }
}

/// The text output of the search.
pub fn markdown(results: &[Found]) -> String {
    if results.is_empty() {
        return "No results. Try other words or --all.\n".into();
    }
    let mut l = Vec::new();
    for (i, r) in results.iter().enumerate() {
        l.push(format!(
            "{}. {} · {} · {}",
            i + 1,
            r.location,
            r.heading,
            r.status.label()
        ));
        if !r.snippet.is_empty() {
            l.push(format!("   {}", r.snippet));
        }
        if !r.code.is_empty() {
            l.push(format!("   code: {}", r.code.join(", ")));
        }
        if !r.cited_by.is_empty() {
            l.push(format!("   cited by: {}", r.cited_by.join(", ")));
        }
    }
    l.push(String::new());
    l.push("Read a section with: kontext find --show <path:line>".into());
    l.join("\n") + "\n"
}

/// The text of `--show`: the section, up to 200 lines.
pub fn show_text(h: &Hit, location: &str) -> String {
    let lines: Vec<&str> = h.body.split('\n').collect();
    let mut out = lines
        .iter()
        .take(SHOW_MAX_LINES)
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    out.push('\n');
    if lines.len() > SHOW_MAX_LINES {
        out.push_str(&format!(
            "\n… {} more lines ({location})\n",
            lines.len() - SHOW_MAX_LINES
        ));
    }
    out
}

/// For `--json` of `--show`.
pub fn show_json(h: &Hit, location: &str) -> serde_json::Value {
    json!({
        "repo": h.repo,
        "path": h.path,
        "heading": h.heading,
        "location": location,
        "startLine": h.start_line,
        "endLine": h.end_line,
        "status": h.status.status,
        "statusLabel": h.status.label,
        "body": h.body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn needles_and_baseline() {
        assert_eq!(
            citation_needle("docs/01-decisions/ADR-0102-geocercas.md"),
            "ADR-0102"
        );
        assert_eq!(citation_needle("docs/runbooks/x.md"), "docs/runbooks/x.md");
        let counts = BTreeMap::from([("docs/a.md".to_string(), 2), ("docs/b.md".to_string(), 1)]);
        assert_eq!(parse_baseline(&format_baseline(&counts)), counts);
        assert_eq!(
            parse_baseline("junk\n3 docs/c.md\n"),
            BTreeMap::from([("docs/c.md".to_string(), 3)])
        );
        let cur = BTreeMap::from([("docs/guide.md".to_string(), 1)]);
        assert_eq!(
            compare_baseline(&cur, &BTreeMap::from([("docs/guide.md".to_string(), 1)])),
            (vec![], false)
        );
        assert_eq!(
            compare_baseline(&cur, &BTreeMap::new()).0,
            vec![("docs/guide.md".to_string(), 1, 0)]
        );
        assert!(compare_baseline(&BTreeMap::new(), &cur).1);
    }
}
