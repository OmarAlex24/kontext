//! Facts with fingerprints (DESIGN §4.3, §7.4 and §11, phase 4).
//!
//! A fact records which files it came from: `fields.fingerprints[]` with
//! `{repo, path, commit, blob}`, where `blob` is `git rev-parse HEAD:<path>`
//! when it is written. "stale" is never written: it is computed on read by
//! comparing that blob with the one at `HEAD` in the repo's local checkout. If
//! the repo is not on this machine, the fact stays "unchecked" (it is not
//! known, not stale).
//!
//! The blob for each `(repo, path, HEAD)` is stored in the index (`fact_blobs`):
//! with the same `HEAD` the answer never changes, so the views do not run git
//! unnecessarily. Within a run there is also a memo.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use unicode_normalization::UnicodeNormalization;

use crate::git;
use crate::index::Index;
use crate::model::{Actor, Entry, RawEntry};
use crate::repos::Repos;
use crate::state::{Alert, State, Status};
use crate::store::{Refused, Store};
use crate::tasks::epic_name;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Fingerprint {
    pub repo: String,
    pub path: String,
    pub commit: String,
    pub blob: String,
}

impl Fingerprint {
    /// `shop:src/a.ts@9b2a1c3`.
    pub fn label(&self) -> String {
        format!("{}:{}@{}", self.repo, self.path, git::short(&self.commit))
    }
}

/// The fingerprints of a fact (or of a confirmation).
pub fn fingerprints_of(e: &Entry) -> Vec<Fingerprint> {
    match e.fields.get("fingerprints") {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|v| serde_json::from_value(v.clone()).ok())
            .collect(),
        _ => Vec::new(),
    }
}

/// How fresh a fact is, according to its fingerprints.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Freshness {
    /// All its paths still have the same blob.
    Fresh,
    /// Some repo is not on this machine: it could not be checked.
    Unchecked { repos: Vec<String> },
    /// A path changed (or no longer exists) at `head`.
    Stale {
        repo: String,
        path: String,
        head: String,
        missing: bool,
    },
}

/// The normalized title used to find duplicates: lowercase, without accents,
/// without punctuation, and with the spaces collapsed.
pub fn norm_title(s: &str) -> String {
    let folded: String = s
        .nfkd()
        .filter(|c| !unicode_normalization::char::is_combining_mark(*c))
        .collect::<String>()
        .to_lowercase();
    let spaced: String = folded
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    spaced.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// What `HEAD` of a repo has for a path.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Lookup {
    Unavailable,
    At { head: String, blob: Option<String> },
}

/// Checks fingerprints against the local checkouts, with a memo and the index
/// cache.
pub struct Checker<'a> {
    repos: &'a Repos,
    index: Option<&'a Index>,
    heads: HashMap<String, Option<(PathBuf, String)>>,
    memo: HashMap<(String, String), Lookup>,
}

impl<'a> Checker<'a> {
    pub fn new(repos: &'a Repos, index: Option<&'a Index>) -> Checker<'a> {
        Checker {
            repos,
            index,
            heads: HashMap::new(),
            memo: HashMap::new(),
        }
    }

    fn head_of(&mut self, repo: &str) -> Option<(PathBuf, String)> {
        if let Some(h) = self.heads.get(repo) {
            return h.clone();
        }
        let found = self
            .repos
            .locate(repo)
            .and_then(|root| git::head(&root).map(|h| (root, h)));
        self.heads.insert(repo.to_string(), found.clone());
        found
    }

    /// Puts into the memo whatever these fingerprints need: first the index
    /// cache, and the rest with a single `git cat-file --batch-check` per repo.
    pub fn prefetch(&mut self, fps: &[Fingerprint]) {
        let mut missing: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for fp in fps {
            let key = (fp.repo.clone(), fp.path.clone());
            if self.memo.contains_key(&key) {
                continue;
            }
            let Some((_, head)) = self.head_of(&fp.repo) else {
                self.memo.insert(key, Lookup::Unavailable);
                continue;
            };
            if let Some(idx) = self.index
                && let Ok(Some(blob)) = idx.fact_blob(&fp.repo, &fp.path, &head)
            {
                self.memo.insert(key, Lookup::At { head, blob });
                continue;
            }
            missing
                .entry(fp.repo.clone())
                .or_default()
                .insert(fp.path.clone());
        }
        for (repo, paths) in missing {
            let Some((root, head)) = self.head_of(&repo) else {
                continue;
            };
            let paths: Vec<String> = paths.into_iter().collect();
            let blobs = batch_objects(&root, &head, &paths);
            for (path, blob) in paths.into_iter().zip(blobs) {
                if let Some(idx) = self.index {
                    let _ = idx.put_fact_blob(&repo, &path, &head, blob.as_deref());
                }
                self.memo.insert(
                    (repo.clone(), path),
                    Lookup::At {
                        head: head.clone(),
                        blob,
                    },
                );
            }
        }
    }

    fn lookup(&mut self, fp: &Fingerprint) -> Lookup {
        let key = (fp.repo.clone(), fp.path.clone());
        if !self.memo.contains_key(&key) {
            self.prefetch(std::slice::from_ref(fp));
        }
        self.memo.get(&key).cloned().unwrap_or(Lookup::Unavailable)
    }

    /// `None` if it has no fingerprints.
    pub fn freshness(&mut self, fps: &[Fingerprint]) -> Option<Freshness> {
        if fps.is_empty() {
            return None;
        }
        let mut unchecked = BTreeSet::new();
        for fp in fps {
            match self.lookup(fp) {
                Lookup::Unavailable => {
                    unchecked.insert(fp.repo.clone());
                }
                Lookup::At { head, blob } => {
                    if blob.as_deref() != Some(fp.blob.as_str()) {
                        return Some(Freshness::Stale {
                            repo: fp.repo.clone(),
                            path: fp.path.clone(),
                            head,
                            missing: blob.is_none(),
                        });
                    }
                }
            }
        }
        if unchecked.is_empty() {
            Some(Freshness::Fresh)
        } else {
            Some(Freshness::Unchecked {
                repos: unchecked.into_iter().collect(),
            })
        }
    }
}

/// `git cat-file --batch-check` of `<head>:<path>` for each path: the object,
/// or `None` if it is not there.
fn batch_objects(root: &Path, head: &str, paths: &[String]) -> Vec<Option<String>> {
    let fallback = || {
        paths
            .iter()
            .map(|p| git::object_at(root, head, p))
            .collect()
    };
    let Ok(mut child) = Command::new("git")
        .args(["cat-file", "--batch-check=%(objectname)"])
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return fallback();
    };
    let mut input = String::new();
    for p in paths {
        input.push_str(&format!("{head}:{p}\n"));
    }
    let mut stdin = child.stdin.take().unwrap();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(input.as_bytes());
    });
    let out = child.wait_with_output();
    let _ = writer.join();
    let Ok(out) = out else { return fallback() };
    let text = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() != paths.len() {
        return fallback();
    }
    lines
        .into_iter()
        .map(|l| {
            let l = l.trim();
            let is_sha = l.len() >= 40 && l.chars().all(|c| c.is_ascii_hexdigit());
            is_sha.then(|| l.to_string())
        })
        .collect()
}

/// Computes the freshness of the active facts and adds an alert for each one
/// that is stale. Whatever fails (git, the index) leaves the fact unchecked.
pub fn annotate(st: &mut State, repos: &Repos, index: Option<&Index>) {
    let facts: Vec<(String, Vec<Fingerprint>)> = st
        .facts()
        .into_iter()
        .map(|f| (f.id.clone(), fingerprints_of(f)))
        .filter(|(_, fps)| !fps.is_empty())
        .collect();
    if facts.is_empty() {
        return;
    }
    let mut checker = Checker::new(repos, index);
    let all: Vec<Fingerprint> = facts.iter().flat_map(|(_, f)| f.clone()).collect();
    checker.prefetch(&all);
    for (id, fps) in facts {
        if let Some(fr) = checker.freshness(&fps) {
            if let Freshness::Stale {
                repo,
                path,
                head,
                missing,
            } = &fr
            {
                st.alerts.push(Alert::FactStale {
                    id: id.clone(),
                    repo: repo.clone(),
                    path: path.clone(),
                    head: git::short(head).to_string(),
                    missing: *missing,
                });
            }
            st.freshness.insert(id, fr);
        }
    }
}

/// What `kontext fact` asks for.
#[derive(Debug, Clone, Default)]
pub struct FactArgs {
    pub text: String,
    pub body: String,
    pub epic: Option<String>,
    pub tags: Vec<String>,
    pub paths: Vec<String>,
    pub repo: Option<String>,
}

/// What happened when a fact was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// A new fact.
    New,
    /// There was already one the same (by title or by fingerprints): a `confirm` was written.
    Confirmed { of: String, reason: String },
    /// There was one with the same title that is stale: the new one replaces it.
    Replaced { old: String },
}

#[derive(Debug, Clone)]
pub struct Written {
    pub entries: Vec<Entry>,
    pub outcome: Outcome,
    pub fingerprints: Vec<Fingerprint>,
    pub warnings: Vec<String>,
}

/// Normalizes a path without touching the disk (`a/./b/../c` → `a/c`).
fn lexical(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn canonical_or_lexical(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| lexical(p))
}

/// The fingerprints for `--path`: each path is resolved against the git repo
/// that contains `cwd` (or against `--repo`), and must be in a commit.
pub fn capture(
    repos: &Repos,
    repo_flag: Option<&str>,
    cwd: &Path,
    paths: &[String],
) -> Result<(Vec<Fingerprint>, Vec<String>)> {
    if paths.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    let mut warnings = Vec::new();
    let (name, root) = match repo_flag.map(str::trim).filter(|r| !r.is_empty()) {
        Some(name) => {
            if repos.get(name).is_none() {
                return Err(
                    Refused(format!("--repo {name}: not in [[repos]] of project.toml")).into(),
                );
            }
            match repos.locate(name) {
                Some(root) => (name.to_string(), root),
                None => {
                    return Err(Refused(format!(
                        "repo {name} is not on this machine: run `kontext fact` from its checkout \
                         or tell projects.toml where it is ([projects.<project>.repos] {name} = \"<path>\")"
                    ))
                    .into());
                }
            }
        }
        None => {
            let Some(here) = repos.here() else {
                return Err(Refused(
                    "--path needs to run inside a git repo (or use --repo <name>)".into(),
                )
                .into());
            };
            let (name, known) = repos.name_for(&here.root, here.remote.as_deref());
            if !known {
                warnings.push(format!(
                    "repo {name} is not in [[repos]] of project.toml: its fingerprint can only be checked \
                     from a folder named {name} (run `kontext install` in the repo to add it)"
                ));
            }
            (name, here.root.clone())
        }
    };
    let Some(head) = git::head(&root) else {
        return Err(Refused(format!(
            "{} has no commits yet: make a commit or drop --path",
            root.display()
        ))
        .into());
    };
    let root_c = canonical_or_lexical(&root);
    let base = if repo_flag.is_some() {
        root.clone()
    } else {
        cwd.to_path_buf()
    };
    let mut out: Vec<Fingerprint> = Vec::new();
    for p in paths {
        let p = p.trim();
        if p.is_empty() {
            continue;
        }
        let abs = if Path::new(p).is_absolute() {
            PathBuf::from(p)
        } else {
            base.join(p)
        };
        let abs = canonical_or_lexical(&abs);
        let Ok(rel) = abs.strip_prefix(&root_c) else {
            return Err(Refused(format!(
                "--path {p}: is outside repo {name} ({})",
                root.display()
            ))
            .into());
        };
        let rel = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        if rel.is_empty() {
            return Err(Refused(format!(
                "--path {p}: is the repo root; point to a file or folder"
            ))
            .into());
        }
        let Some(blob) = git::object_at(&root, &head, &rel) else {
            return Err(Refused(format!(
                "--path {p}: {rel} is not in the latest commit of {name}. Make a commit or drop --path"
            ))
            .into());
        };
        if git::dirty(&root, &rel) {
            warnings.push(format!(
                "{rel} has uncommitted changes: the fingerprint is the one from the latest commit ({})",
                git::short(&head)
            ));
        }
        let fp = Fingerprint {
            repo: name.clone(),
            path: rel,
            commit: head.clone(),
            blob,
        };
        if !out.contains(&fp) {
            out.push(fp);
        }
    }
    Ok((out, warnings))
}

/// The key used to compare fingerprints: repo, path and blob (the commit may
/// differ while the content is the same).
fn fp_key(fps: &[Fingerprint]) -> BTreeSet<(String, String, String)> {
    fps.iter()
        .map(|f| (f.repo.clone(), f.path.clone(), f.blob.clone()))
        .collect()
}

/// An active fact equal to this one (§7.4): same normalized title or same
/// fingerprints. Returns the `id` and the reason.
pub fn duplicate_of(st: &State, title: &str, fps: &[Fingerprint]) -> Option<(String, String)> {
    let norm = norm_title(title);
    let key = fp_key(fps);
    for f in st.facts().into_iter().rev() {
        if !norm.is_empty() && norm_title(&f.title) == norm {
            return Some((f.id.clone(), "same title".into()));
        }
        if !key.is_empty() && fp_key(&fingerprints_of(f)) == key {
            return Some((f.id.clone(), "same fingerprints".into()));
        }
    }
    None
}

/// `kontext fact <text> [--path …] [--repo] [--epic] [--tags]`.
///
/// `freshness` is the freshness of the active facts, computed before taking
/// the lock (running git under the lock would make it wait).
pub fn write(
    store: &mut Store,
    actor: &Actor,
    a: &FactArgs,
    fingerprints: Vec<Fingerprint>,
    freshness: &BTreeMap<String, Freshness>,
) -> Result<Written> {
    let text = a.text.trim();
    if text.is_empty() {
        bail!("the fact is empty");
    }
    let (outcome, entries) = store.transact(|b| {
        let st = b.state()?;
        let mut fields = serde_json::Map::new();
        let mut refs = Vec::new();
        if let Some(e) = a.epic.as_deref().map(str::trim).filter(|e| !e.is_empty()) {
            let id = st.find_epic(e)?;
            fields.insert("epic".into(), json!(epic_name(st.get(&id).unwrap())));
            refs.push(id);
        }
        fields.insert("fingerprints".into(), json!(fingerprints));
        let dup = duplicate_of(st, text, &fingerprints);
        let stale = |id: &str| matches!(freshness.get(id), Some(Freshness::Stale { .. }));
        match dup {
            Some((of, reason)) if !stale(&of) => {
                let mut r = RawEntry::new("confirm", text);
                r.actor = Some(actor.clone());
                r.body = a.body.clone();
                r.tags = a.tags.clone();
                r.refs = std::iter::once(of.clone()).chain(refs).collect();
                fields.insert("reason".into(), json!(reason));
                r.fields = fields;
                b.push(r)?;
                Ok(Outcome::Confirmed { of, reason })
            }
            other => {
                let mut r = RawEntry::new("fact", text);
                r.actor = Some(actor.clone());
                r.body = a.body.clone();
                r.tags = a.tags.clone();
                r.refs = refs;
                r.fields = fields;
                let new = b.push(r)?.entry;
                match other {
                    Some((old, _)) => {
                        let mut s = RawEntry::new("supersede", "");
                        s.actor = Some(actor.clone());
                        s.refs = vec![old.clone(), new.id.clone()];
                        s.why =
                            "stale: the code of its fingerprint changed; this one counts it again"
                                .into();
                        b.push(s)?;
                        Ok(Outcome::Replaced { old })
                    }
                    None => Ok(Outcome::New),
                }
            }
        }
    })?;
    Ok(Written {
        entries,
        outcome,
        fingerprints,
        warnings: Vec::new(),
    })
}

/// The mark of a fact in a list: ` [unchecked]` if its repo is not on this
/// machine.
pub fn mark(st: &State, id: &str) -> &'static str {
    match st.freshness.get(id) {
        Some(Freshness::Unchecked { .. }) => " [unchecked]",
        Some(Freshness::Stale { .. }) => " [stale]",
        _ => "",
    }
}

/// Is it still valid? Active, and without a stale fingerprint.
pub fn is_live(st: &State, id: &str) -> bool {
    st.status_of(id) == Some(Status::Active)
        && !matches!(st.freshness.get(id), Some(Freshness::Stale { .. }))
}

/// How the freshness is described in `show`.
pub fn describe(fr: &Freshness) -> String {
    match fr {
        Freshness::Fresh => "fresh: its paths have not changed".into(),
        Freshness::Unchecked { repos } => {
            format!("unchecked: {} is not on this machine", repos.join(", "))
        }
        Freshness::Stale {
            repo,
            path,
            head,
            missing,
        } => format!(
            "stale: {path} ({repo}) {} at {}",
            if *missing {
                "no longer exists"
            } else {
                "changed"
            },
            git::short(head)
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_normalize() {
        assert_eq!(
            norm_title("  The Café  uses  Stripe. "),
            "the cafe uses stripe"
        );
        assert_eq!(norm_title("¿Where is invoice.ts?"), "where is invoice ts");
        assert_eq!(norm_title("Ñandú"), "nandu");
    }

    #[test]
    fn lexical_paths() {
        assert_eq!(lexical(Path::new("/a/./b/../c")), PathBuf::from("/a/c"));
    }
}
