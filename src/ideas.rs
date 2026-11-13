//! The user's ideas and where each one went (`kontext ideas`, DESIGN §12.3).
//!
//! The skill `kontext-pm` keeps them in `.pm/` of the code repo: what the user
//! said, verbatim, in `dumps/<date>.md`, and one row per idea in `items.md`
//! (`| id | idea | source | state | where | evidence |`). The agent writes the
//! `state` column by hand and it goes stale; here it is derived from the
//! ledger through the ids in `where` and `evidence`, first match wins:
//!
//! a. written `discarded` → discarded (only the user discards);
//! b. empty `where` → no destination;
//! c. an open question for a human, or a proposal nobody accepted or
//!    rejected (imported from v0 or not) → waiting for you;
//! d. tasks or epics → their tasks decide: all closed with a `done` that
//!    counts → done; some work claimed, started or closed → in progress;
//!    otherwise pending;
//! e. otherwise the written state: `pending`, `in progress`, `decided` or
//!    `done` (`done` without evidence shows as in progress, with a note; an
//!    unknown state, as pending with a note).
//!
//! In rule d a feature counts as its own state: done → done; to validate or
//! in progress → in progress; pending → pending; a superseded one is followed
//! to its replacement, with a note.
//!
//! Apart from its state, an idea is **placed** when the user discarded it or
//! its `where` cites a feature, a task or a decision: the work has a home. An
//! idea that cites only an epic, a question, text or nothing is not placed,
//! and says what it does cite ("Payments, no feature").
//!
//! The messages of the dumps (each `## <date> <HH:MM>` entry) are audited when
//! the pm runs `kontext ideas --mark-audited` before the coverage audit: it
//! writes `.pm/audited.json` with the time and how many entries each dump had.
//! What is beyond those counts, and any dump not listed, is not audited yet.
//!
//! An id that is not in the ledger fails nothing: it stays as text, with a
//! note. Nothing here prints: [`build`] gives a plain [`Ideas`], and
//! [`text`] and `serde` show it.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::features::FeatureStatus;
use crate::model::{ENTITY_KINDS, Entry, RECORD_PREFIX};
use crate::repos::Repos;
use crate::state::{State, Status};
use crate::tasks::TaskStatus;
use crate::views::{field, one_line, question_to};

/// The folder of the PM's memory, at the root of the code repo.
pub const PM_DIR: &str = ".pm";
/// How many characters of the dump's line go in `quote`.
pub const QUOTE_MAX: usize = 200;

/// The state shown for an idea. The order is the display order.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IdeaState {
    #[serde(rename = "no destination")]
    NoDestination,
    #[serde(rename = "waiting for you")]
    WaitingForYou,
    #[serde(rename = "pending")]
    Pending,
    #[serde(rename = "in progress")]
    InProgress,
    #[serde(rename = "decided")]
    Decided,
    #[serde(rename = "done")]
    Done,
    #[serde(rename = "discarded")]
    Discarded,
}

impl IdeaState {
    pub const ALL: [IdeaState; 7] = [
        IdeaState::NoDestination,
        IdeaState::WaitingForYou,
        IdeaState::Pending,
        IdeaState::InProgress,
        IdeaState::Decided,
        IdeaState::Done,
        IdeaState::Discarded,
    ];

    pub fn label(self) -> &'static str {
        match self {
            IdeaState::NoDestination => "no destination",
            IdeaState::WaitingForYou => "waiting for you",
            IdeaState::Pending => "pending",
            IdeaState::InProgress => "in progress",
            IdeaState::Decided => "decided",
            IdeaState::Done => "done",
            IdeaState::Discarded => "discarded",
        }
    }
}

/// Where the state shown came from.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum StateFrom {
    /// The ledger decided it (rules c and d).
    Ledger,
    /// The table's `state` and `where` columns decided it.
    Table,
}

/// A row of `items.md`, as written (cells trimmed).
#[derive(Serialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct Row {
    /// Its line in `items.md` (from 1).
    pub line: usize,
    pub id: String,
    pub idea: String,
    pub source: String,
    pub state: String,
    #[serde(rename = "where")]
    pub where_: String,
    pub evidence: String,
}

/// Where the user said it: `dumps/2026-11-07.md:14`.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct Source {
    /// Relative to `.pm/`.
    pub file: String,
    pub line: Option<usize>,
}

/// An entry of the ledger that an idea cites.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct Reference {
    /// Its id here.
    pub id: String,
    /// As the table cites it (`D-0030`, `T-4c1`), if it differs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cited: Option<String>,
    pub kind: String,
    pub title: String,
    /// `open`, `pending`, `in force`, `claimed`, `closed`…
    pub status: String,
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EvidenceKind {
    /// A screenshot under `.pm/shots/`.
    Image,
    Text,
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct Evidence {
    pub kind: EvidenceKind,
    /// The text, or the image's path relative to `.pm/` (`shots/home.png`).
    pub value: String,
}

/// An idea with its state derived.
#[derive(Serialize, Clone, Debug)]
pub struct Idea {
    pub id: String,
    pub idea: String,
    /// The date it was said (`2026-11-07`), from the dump's file name.
    pub said: Option<String>,
    pub source: Option<Source>,
    /// The line of the dump the source points to, trimmed and cut.
    pub quote: Option<String>,
    pub state: IdeaState,
    /// The `state` cell, as written.
    pub written_state: String,
    /// Whether the ledger or the table decided `state`.
    pub state_from: StateFrom,
    #[serde(rename = "where")]
    pub where_: String,
    pub refs: Vec<Reference>,
    pub evidence: Vec<Evidence>,
    pub notes: Vec<String>,
    /// Discarded, or its `where` cites a feature, a task or a decision.
    pub placed: bool,
    /// When it is not placed, what its `where` does cite ("Payments, no
    /// feature", "question Q-… open", the text).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cites: Option<String>,
}

/// An idea that is not placed, for the "Not placed" list.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct NotPlaced {
    pub id: String,
    pub idea: String,
    pub said: Option<String>,
    pub quote: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cites: Option<String>,
}

/// The user's messages in the dumps that no coverage audit has seen.
#[derive(Serialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct Unaudited {
    pub count: usize,
    /// When the last audit was marked (ISO-8601); `None`: never.
    pub since: Option<String>,
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct Count {
    pub state: IdeaState,
    pub count: usize,
}

/// Every idea of `items.md`, in the table's order.
#[derive(Serialize, Clone, Debug)]
pub struct Ideas {
    pub project: String,
    /// The `items.md` read.
    pub path: PathBuf,
    pub total: usize,
    /// The seven states in display order, with zeros.
    pub counts: Vec<Count>,
    pub ideas: Vec<Idea>,
    /// What is wrong with the table as a whole (a missing `where` column).
    pub warnings: Vec<String>,
    /// The ideas that are not placed, in the table's order.
    pub not_placed: Vec<NotPlaced>,
    pub unaudited: Unaudited,
}

impl Ideas {
    pub fn count(&self, s: IdeaState) -> usize {
        self.ideas.iter().filter(|i| i.state == s).count()
    }

    /// The ideas in a state, in the table's order.
    pub fn of_state(&self, s: IdeaState) -> Vec<&Idea> {
        self.ideas.iter().filter(|i| i.state == s).collect()
    }
}

// ------------------------------------------------------------ where

/// The `.pm/` folder of the checkout that contains `dir`: `<root>/.pm` in a
/// main checkout, and in a linked git worktree the main checkout's, unless
/// `own_in_worktrees` (`[capture] worktrees = true`) gives the worktree its
/// own. `None` outside a git repo, or in a worktree of a bare repository.
pub fn pm_dir(dir: &Path, own_in_worktrees: bool) -> Option<PathBuf> {
    let (root, linked) = crate::git::checkout(dir)?;
    pm_dir_of(&root, linked, own_in_worktrees)
}

/// [`pm_dir`] for a checkout already found ([`crate::git::checkout`]).
pub fn pm_dir_of(root: &Path, linked: bool, own_in_worktrees: bool) -> Option<PathBuf> {
    if !linked || own_in_worktrees {
        return Some(root.join(PM_DIR));
    }
    crate::git::main_checkout(root).map(|m| m.join(PM_DIR))
}

/// The `.pm/` folders [`locate`] looks in, in order: the one of the git repo
/// the command runs in ([`pm_dir`]), then those of the project repos on this
/// machine.
pub fn candidates(repos: &Repos, own_in_worktrees: bool) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = repos
        .here()
        .and_then(|h| pm_dir(&h.root, own_in_worktrees))
        .into_iter()
        .collect();
    for (_, root) in repos.available() {
        let pm = root.join(PM_DIR);
        if !out.contains(&pm) {
            out.push(pm);
        }
    }
    out
}

/// The `.pm/` folder that has an `items.md`: the first of [`candidates`].
/// `kontext ideas`, `--mark-audited` and `kontext tree` all use it.
pub fn locate(repos: &Repos, own_in_worktrees: bool) -> Option<PathBuf> {
    candidates(repos, own_in_worktrees)
        .into_iter()
        .find(|pm| pm.join("items.md").is_file())
}

// ------------------------------------------------------------ table

/// The cells of a table line, trimmed. `\|` is a `|` inside a cell.
fn cells(line: &str) -> Vec<String> {
    let t = line.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = t.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'|') => {
                cur.push('|');
                chars.next();
            }
            '|' => out.push(std::mem::take(&mut cur).trim().to_string()),
            _ => cur.push(c),
        }
    }
    // What follows the last `|` is a cell only if it has text.
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

fn is_separator(cells: &[String]) -> bool {
    !cells.is_empty()
        && cells.iter().all(|c| {
            let c = c.trim_matches(':');
            !c.is_empty() && c.chars().all(|x| x == '-')
        })
}

/// What `kontext ideas` and the report say when the table has rows but no
/// `where` column.
pub const NO_WHERE_WARNING: &str =
    "items.md has no `where` column: every idea shows as no destination.";

/// The rows of `items.md`. The columns are found by their header name
/// (`id`, `idea`, `source`, `state`, `where`, `evidence`, in any case and
/// order); other columns, lines that are not table rows and rows without an
/// id are ignored. Without an `id` column there are no rows.
pub fn parse(text: &str) -> Vec<Row> {
    parse_table(text).0
}

/// [`parse`], and whether the table's header has a `where` column.
pub fn parse_table(text: &str) -> (Vec<Row>, bool) {
    let mut cols: Option<HashMap<String, usize>> = None;
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if !line.trim_start().starts_with('|') {
            continue;
        }
        let cs = cells(line);
        if is_separator(&cs) {
            continue;
        }
        let Some(map) = &cols else {
            let map: HashMap<String, usize> = cs
                .iter()
                .enumerate()
                .map(|(n, c)| (c.to_lowercase(), n))
                .collect();
            if map.contains_key("id") {
                cols = Some(map);
            }
            continue;
        };
        let get = |name: &str| {
            map.get(name)
                .and_then(|n| cs.get(*n))
                .cloned()
                .unwrap_or_default()
        };
        let id = get("id");
        if id.is_empty() {
            continue;
        }
        out.push(Row {
            line: i + 1,
            id,
            idea: get("idea"),
            source: get("source"),
            state: get("state"),
            where_: get("where"),
            evidence: get("evidence"),
        });
    }
    let has_where = cols.is_some_and(|m| m.contains_key("where"));
    (out, has_where)
}

// ------------------------------------------------------------ sources

/// `dumps/2026-11-07.md:14` → file and line (a `.pm/` in front is dropped).
pub fn parse_source(s: &str) -> Option<Source> {
    let s = s.trim();
    let s = s.strip_prefix(".pm/").unwrap_or(s);
    if s.is_empty() {
        return None;
    }
    match s.rsplit_once(':') {
        Some((file, line)) if line.trim().parse::<usize>().is_ok() => Some(Source {
            file: file.trim().to_string(),
            line: line.trim().parse().ok(),
        }),
        _ => Some(Source {
            file: s.to_string(),
            line: None,
        }),
    }
}

/// The date in a dump's name: `2026-11-07.md` and `2026-11-07-2.md`.
pub fn said_date(file: &str) -> Option<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"(\d{4}-\d{2}-\d{2})(?:-\d+)?\.md$").unwrap());
    re.captures(file).map(|c| c[1].to_string())
}

/// The skill's marker of the quoted coordinator lines in a dump entry.
pub const ASKED_MARKER: &str = "**Coordinator asked:**";
/// The skill's marker of the user's own text in a dump entry.
pub const USER_MARKER: &str = "**User:**";

/// Whether line `i` (0-based) of a dump is the user's own text. An entry's
/// header (`## <date> <HH:MM>`), its markers and the lines under
/// `**Coordinator asked:**` (until `**User:**`) are not; a file without the
/// skill's markers is all the user's, as before.
fn users_own(lines: &[String], i: usize) -> bool {
    let marker = |l: &str| l.trim() == ASKED_MARKER || l.trim() == USER_MARKER;
    let line = &lines[i];
    if is_entry_header(line) || marker(line) {
        return false;
    }
    for l in lines[..i].iter().rev() {
        if is_entry_header(l) || l.trim() == USER_MARKER {
            return true;
        }
        if l.trim() == ASKED_MARKER {
            return false;
        }
    }
    true
}

/// The header of a dump entry: `## 2026-11-07 14:05`. Each one is a message
/// of the user.
pub fn is_entry_header(line: &str) -> bool {
    static HEADER: OnceLock<Regex> = OnceLock::new();
    let header =
        HEADER.get_or_init(|| Regex::new(r"^## \d{4}-\d{2}-\d{2} \d{2}:\d{2}\s*$").unwrap());
    header.is_match(line)
}

/// Reads the dumps once each.
struct Dumps<'a> {
    pm: &'a Path,
    files: HashMap<String, Option<Vec<String>>>,
}

impl Dumps<'_> {
    /// The cited line, when it is the user's own text. A line the coordinator
    /// said (quoted under `**Coordinator asked:**`) or a marker is never shown
    /// as the user's words: no quote, and a note.
    fn quote(&mut self, src: &Source, notes: &mut Vec<String>) -> Option<String> {
        let n = src.line?;
        let pm = self.pm;
        let lines = self.files.entry(src.file.clone()).or_insert_with(|| {
            // Only inside `.pm/`.
            let rel = Path::new(&src.file);
            if rel.is_absolute()
                || src.file.starts_with(['/', '\\'])
                || src.file.split(['/', '\\']).any(|p| p == "..")
            {
                return None;
            }
            fs::read_to_string(pm.join(&src.file))
                .ok()
                .map(|t| t.lines().map(str::to_string).collect())
        });
        let lines = lines.as_ref()?;
        let i = n.checked_sub(1)?;
        let line = lines.get(i)?;
        if !users_own(lines, i) {
            notes.push(format!(
                "{}:{n} is not the user's own text (a marker or a line the coordinator said): not quoted",
                src.file
            ));
            return None;
        }
        let q = one_line(line, QUOTE_MAX);
        (!q.is_empty()).then_some(q)
    }
}

// ------------------------------------------------------------ evidence

pub fn parse_evidence(s: &str) -> Vec<Evidence> {
    s.split(',')
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .map(|x| {
            let path = x.strip_prefix(".pm/").unwrap_or(x);
            if path.starts_with("shots/") && !path.contains(char::is_whitespace) {
                Evidence {
                    kind: EvidenceKind::Image,
                    value: path.to_string(),
                }
            } else {
                Evidence {
                    kind: EvidenceKind::Text,
                    value: x.to_string(),
                }
            }
        })
        .collect()
}

// ------------------------------------------------------------ ledger ids

/// The tokens that look like ledger ids: kontext's (`D-7f3a9c`, or a prefix
/// such as `T-4c1`) and v0's (`D-0030`, `ACC-0003`), in order, once each.
pub fn id_tokens(s: &str) -> Vec<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"\b[A-Z]{1,5}-(?:[0-9a-f]{3,6}|[0-9]{4,})\b").unwrap());
    let mut seen = BTreeSet::new();
    re.find_iter(s)
        .map(|m| m.as_str().to_string())
        .filter(|t| seen.insert(t.clone()))
        .collect()
}

/// The prefixes an id of this ledger can have: kontext's and those of the
/// v0 ids imported. A token with another prefix (`CVE-2024`) is plain text.
fn known_prefixes(st: &State) -> BTreeSet<String> {
    let mut out: BTreeSet<String> = ENTITY_KINDS.iter().map(|(_, p)| p.to_string()).collect();
    out.insert(RECORD_PREFIX.to_string());
    for v0 in st.v0_ids.keys() {
        if let Some((p, _)) = v0.split_once('-') {
            out.insert(p.to_string());
        }
    }
    out
}

fn status_text(st: &State, e: &Entry) -> String {
    if let Some(f) = st.feature(&e.id) {
        return f.status.label().to_string();
    }
    if let Some(t) = st.task_status(&e.id) {
        return t.label().to_string();
    }
    st.status_of(&e.id)
        .map(Status::label)
        .unwrap_or("")
        .to_string()
}

/// A question that waits for a person: open, not dismissed, and addressed to
/// `owner`, an owner, `human:<x>` or someone in `[people]`.
fn waits_question(st: &State, q: &Entry, people: &[String]) -> bool {
    if st.status_of(&q.id) != Some(Status::Open) || st.inbox_closed.contains_key(&q.id) {
        return false;
    }
    let to = question_to(q);
    field(q, "to").is_some_and(|t| t.starts_with("human:"))
        || to == "owner"
        || st.policy.is_owner(to)
        || people.iter().any(|p| p == to)
}

/// A proposal nobody accepted or rejected (and the human did not dismiss).
fn waits_proposal(st: &State, p: &Entry) -> bool {
    st.status_of(&p.id) == Some(Status::Pending) && !st.inbox_closed.contains_key(&p.id)
}

/// The state of a set of tasks (rule d).
fn tasks_state(st: &State, tasks: &[String]) -> IdeaState {
    if tasks.is_empty() {
        return IdeaState::Pending;
    }
    let done = |t: &String| {
        st.task(t)
            .is_some_and(|i| i.status == TaskStatus::Closed && i.done.is_some())
    };
    if tasks.iter().all(done) {
        return IdeaState::Done;
    }
    let worked = |t: &String| {
        st.task(t).is_some_and(|i| {
            matches!(
                i.status,
                TaskStatus::Assigned | TaskStatus::Claimed | TaskStatus::Closed
            ) || i.holder.is_some()
                || i.done.is_some()
                || i.history.iter().any(|h| h.counted)
        })
    };
    if tasks.iter().any(worked) {
        IdeaState::InProgress
    } else {
        IdeaState::Pending
    }
}

/// The state of a cited feature (rule d). A superseded one is followed to
/// its replacement, with a note.
fn feature_state(st: &State, id: &str, notes: &mut Vec<String>) -> IdeaState {
    let cur = replacement(st, id);
    if cur != id {
        notes.push(format!(
            "{id} was superseded by {cur}: its state is {cur}'s"
        ));
    }
    match st.feature(&cur).map(|f| f.status) {
        Some(FeatureStatus::Done) => IdeaState::Done,
        Some(FeatureStatus::ToValidate | FeatureStatus::InProgress) => IdeaState::InProgress,
        Some(FeatureStatus::NotCounted) => {
            notes.push(format!("{cur} does not count: shown as pending"));
            IdeaState::Pending
        }
        _ => IdeaState::Pending,
    }
}

/// Several work states in one: all done → done; all pending → pending;
/// otherwise in progress.
fn combine(parts: &[IdeaState]) -> IdeaState {
    if parts.iter().all(|s| *s == IdeaState::Done) {
        IdeaState::Done
    } else if parts.iter().all(|s| *s == IdeaState::Pending) {
        IdeaState::Pending
    } else {
        IdeaState::InProgress
    }
}

/// Where a superseded entry leads: the last of its replacements.
fn replacement(st: &State, id: &str) -> String {
    let mut cur = id.to_string();
    let mut seen = BTreeSet::from([cur.clone()]);
    while let Some(next) = st.superseded_by.get(&cur) {
        if !seen.insert(next.clone()) {
            break;
        }
        cur = next.clone();
    }
    cur
}

/// Does a citation place an idea? A feature, task or decision in force that
/// counts; a superseded one, if its replacement does.
fn places(st: &State, e: &Entry) -> bool {
    let id = replacement(st, &e.id);
    let Some(x) = st.get(&id) else {
        return false;
    };
    match x.kind.as_str() {
        "feature" => st.feature(&id).is_some_and(|f| f.status.in_force()),
        "task" | "decision" => !matches!(
            st.status_of(&id),
            Some(Status::Ignored | Status::Superseded | Status::Rejected) | None
        ),
        _ => false,
    }
}

/// Discarded, or the `where` cites a feature, a task or a decision that
/// counts ([`places`]).
fn is_placed(st: &State, state: IdeaState, cited: &[&Entry]) -> bool {
    state == IdeaState::Discarded || cited.iter().any(|e| places(st, e))
}

/// What the `where` of an idea that is not placed cites: "Payments, no
/// feature", "question Q-… open", or its text. `None` if it is empty.
fn cites_text(st: &State, cited: &[&Entry], where_: &str) -> Option<String> {
    if cited.is_empty() {
        let t = one_line(where_, 80);
        return (!t.is_empty()).then_some(t);
    }
    let parts: Vec<String> = cited
        .iter()
        .map(|e| match e.kind.as_str() {
            "epic" => format!("{}, no feature", one_line(&e.title, 60)),
            "question" | "proposal" => format!("{} {} {}", e.kind, e.id, status_text(st, e)),
            "feature" | "task" | "decision" => {
                format!("{} {} {}", e.kind, e.id, status_text(st, e))
            }
            k => format!("{k} {}", e.id),
        })
        .collect();
    Some(parts.join("; "))
}

/// The written state, normalized: `In-Progress` → `in progress`.
fn normalize(s: &str) -> String {
    s.trim()
        .to_lowercase()
        .replace(['-', '_'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

// ------------------------------------------------------------ build

/// The ideas of `items.md` (its text) with their state from the ledger. `pm`
/// is the `.pm/` folder (to quote the dumps); `people`, the names in
/// `[people]` (a question to one of them waits for a person).
pub fn build(st: &State, project: &str, people: &[String], pm: &Path, items_md: &str) -> Ideas {
    let prefixes = known_prefixes(st);
    let mut dumps = Dumps {
        pm,
        files: HashMap::new(),
    };
    let (rows, has_where) = parse_table(items_md);
    let ideas: Vec<Idea> = rows
        .into_iter()
        .map(|row| idea(st, people, &prefixes, &mut dumps, row))
        .collect();
    let mut warnings = Vec::new();
    if !ideas.is_empty() && !has_where {
        warnings.push(NO_WHERE_WARNING.to_string());
    }
    let not_placed = ideas
        .iter()
        .filter(|i| !i.placed)
        .map(|i| NotPlaced {
            id: i.id.clone(),
            idea: i.idea.clone(),
            said: i.said.clone(),
            quote: i.quote.clone(),
            cites: i.cites.clone(),
        })
        .collect();
    Ideas {
        project: project.to_string(),
        path: pm.join("items.md"),
        total: ideas.len(),
        counts: IdeaState::ALL
            .iter()
            .map(|s| Count {
                state: *s,
                count: ideas.iter().filter(|i| i.state == *s).count(),
            })
            .collect(),
        ideas,
        warnings,
        not_placed,
        unaudited: unaudited(pm),
    }
}

/// Reads `<pm>/items.md` and builds the ideas.
pub fn load(st: &State, project: &str, people: &[String], pm: &Path) -> Result<Ideas> {
    let path = pm.join("items.md");
    let text =
        fs::read_to_string(&path).with_context(|| format!("could not read {}", path.display()))?;
    Ok(build(st, project, people, pm, &text))
}

fn idea(
    st: &State,
    people: &[String],
    prefixes: &BTreeSet<String>,
    dumps: &mut Dumps,
    row: Row,
) -> Idea {
    let source = parse_source(&row.source);
    let said = source.as_ref().and_then(|s| said_date(&s.file));
    let mut notes = Vec::new();
    let quote = source.as_ref().and_then(|s| dumps.quote(s, &mut notes));
    let evidence = parse_evidence(&row.evidence);
    // The entries cited in `where` and in `evidence`, once each.
    let mut refs: Vec<Reference> = Vec::new();
    let mut entries: Vec<&Entry> = Vec::new();
    // Those cited in `where` (they decide whether it is placed).
    let mut cited: Vec<&Entry> = Vec::new();
    let in_where = id_tokens(&row.where_);
    let tokens = in_where
        .iter()
        .map(|t| (t.clone(), true))
        .chain(id_tokens(&row.evidence).into_iter().map(|t| (t, false)));
    let mut seen = BTreeSet::new();
    for (tok, from_where) in tokens {
        if !seen.insert(tok.clone()) {
            continue;
        }
        let prefix = tok.split_once('-').map(|(p, _)| p).unwrap_or_default();
        if !prefixes.contains(prefix) {
            continue;
        }
        match st.resolve(&tok) {
            Ok(id) => {
                let Some(e) = st.get(&id) else { continue };
                if from_where && !cited.iter().any(|x| x.id == e.id) {
                    cited.push(e);
                }
                if entries.iter().any(|x| x.id == e.id) {
                    continue;
                }
                entries.push(e);
                refs.push(Reference {
                    id: e.id.clone(),
                    cited: (tok != e.id).then_some(tok),
                    kind: e.kind.clone(),
                    title: one_line(&e.title, 120),
                    status: status_text(st, e),
                });
            }
            Err(err) if err.to_string().contains("ambiguous") => notes.push(format!("{err}")),
            Err(_) => notes.push(format!("{tok} is not in the ledger")),
        }
    }
    let written = normalize(&row.state);
    let (state, from) = derive(st, people, &row, &written, &entries, &evidence, &mut notes);
    let placed = is_placed(st, state, &cited);
    let cites = if placed {
        None
    } else {
        cites_text(st, &cited, &row.where_)
    };
    Idea {
        id: row.id,
        idea: row.idea,
        said,
        source,
        quote,
        state,
        written_state: row.state,
        state_from: from,
        where_: row.where_,
        refs,
        evidence,
        notes,
        placed,
        cites,
    }
}

/// Rules a–e (the module's doc).
fn derive(
    st: &State,
    people: &[String],
    row: &Row,
    written: &str,
    entries: &[&Entry],
    evidence: &[Evidence],
    notes: &mut Vec<String>,
) -> (IdeaState, StateFrom) {
    // a. Only the user discards.
    if written == "discarded" {
        return (IdeaState::Discarded, StateFrom::Table);
    }
    // b.
    if row.where_.trim().is_empty() {
        return (IdeaState::NoDestination, StateFrom::Table);
    }
    // c.
    let waits = entries.iter().any(|e| match e.kind.as_str() {
        "question" => waits_question(st, e, people),
        "proposal" => waits_proposal(st, e),
        _ => false,
    });
    if waits {
        return (IdeaState::WaitingForYou, StateFrom::Ledger);
    }
    // d. The tasks cited and those of the epics cited, and the features cited.
    let mut tasks: Vec<String> = Vec::new();
    let mut work = false;
    let mut parts: Vec<IdeaState> = Vec::new();
    for e in entries {
        match e.kind.as_str() {
            "task" => {
                work = true;
                tasks.push(e.id.clone());
            }
            // An epic with features closes as `kontext plan` closes it; one
            // without features is decided by its tasks, as before.
            "epic" if crate::features::has_features(st, &e.id) => {
                let ts: Vec<String> = st.tasks_of(&e.id).iter().map(|t| t.id.clone()).collect();
                parts.push(if crate::plan::node(st, e).closed() {
                    IdeaState::Done
                } else {
                    match tasks_state(st, &ts) {
                        IdeaState::Done => IdeaState::InProgress,
                        s => s,
                    }
                });
            }
            "epic" => {
                work = true;
                tasks.extend(st.tasks_of(&e.id).into_iter().map(|t| t.id.clone()));
            }
            "feature" => parts.push(feature_state(st, &e.id, notes)),
            _ => {}
        }
    }
    if work {
        tasks.sort();
        tasks.dedup();
        parts.push(tasks_state(st, &tasks));
    }
    if !parts.is_empty() {
        return (combine(&parts), StateFrom::Ledger);
    }
    // e.
    let state = match written {
        "pending" | "" => IdeaState::Pending,
        "in progress" => IdeaState::InProgress,
        "decided" => IdeaState::Decided,
        "done" if evidence.is_empty() => {
            notes.push("marked done without evidence".to_string());
            IdeaState::InProgress
        }
        "done" => IdeaState::Done,
        _ => {
            notes.push(format!("unknown state {:?}, shown as pending", row.state));
            IdeaState::Pending
        }
    };
    (state, StateFrom::Table)
}

// ------------------------------------------------------------ audit

/// The file, in `.pm/`, where `kontext ideas --mark-audited` writes the audit.
pub const AUDITED_FILE: &str = "audited.json";

/// What `.pm/audited.json` holds: when the audit was marked, and how many
/// entries each dump had then (`dumps/2026-11-07.md` → 3).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct Audited {
    pub at: String,
    pub files: BTreeMap<String, usize>,
}

/// The entries (`## <date> <HH:MM>`) of each dump in `<pm>/dumps/`, by its
/// path relative to `.pm/`.
pub fn dump_entries(pm: &Path) -> BTreeMap<String, usize> {
    let mut out = BTreeMap::new();
    let Ok(dir) = fs::read_dir(pm.join("dumps")) else {
        return out;
    };
    for f in dir.flatten() {
        let name = f.file_name().to_string_lossy().to_string();
        if !name.ends_with(".md") || !f.path().is_file() {
            continue;
        }
        let n = fs::read_to_string(f.path())
            .map(|t| t.lines().filter(|l| is_entry_header(l)).count())
            .unwrap_or(0);
        out.insert(format!("dumps/{name}"), n);
    }
    out
}

/// The last audit marked, if `.pm/audited.json` exists and reads.
pub fn audited(pm: &Path) -> Option<Audited> {
    let text = fs::read_to_string(pm.join(AUDITED_FILE)).ok()?;
    serde_json::from_str(&text).ok()
}

/// The messages no audit has seen: the entries beyond the counts of the last
/// audit, and all those of a dump it did not list. Without an audit, all.
pub fn unaudited(pm: &Path) -> Unaudited {
    let last = audited(pm);
    let count = dump_entries(pm)
        .iter()
        .map(|(f, n)| {
            let seen = last.as_ref().and_then(|a| a.files.get(f)).copied();
            n.saturating_sub(seen.unwrap_or(0))
        })
        .sum();
    Unaudited {
        count,
        since: last.map(|a| a.at),
    }
}

/// Writes `<pm>/audited.json` with the time (`KONTEXT_NOW` or the clock) and
/// the entries each dump has now.
pub fn mark_audited(pm: &Path) -> Result<Audited> {
    let at = jiff::Timestamp::from_millisecond(crate::tasks::now_ms())?
        .strftime("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    let a = Audited {
        at,
        files: dump_entries(pm),
    };
    fs::create_dir_all(pm).with_context(|| format!("could not create {}", pm.display()))?;
    let path = pm.join(AUDITED_FILE);
    let tmp = pm.join(format!(".{AUDITED_FILE}.tmp"));
    fs::write(&tmp, serde_json::to_string_pretty(&a)? + "\n")
        .with_context(|| format!("could not write {}", tmp.display()))?;
    fs::rename(&tmp, &path).with_context(|| format!("could not write {}", path.display()))?;
    Ok(a)
}

/// `2026-11-12…` → `Nov 12`.
pub fn month_day(date: &str) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let mut it = date.get(..10).unwrap_or(date).split('-');
    let parsed = (|| {
        let (_, m, d) = (it.next()?, it.next()?, it.next()?);
        let m: usize = m.parse().ok()?;
        let d: u32 = d.parse().ok()?;
        Some(format!("{} {d}", MONTHS.get(m.checked_sub(1)?)?))
    })();
    parsed.unwrap_or_else(|| date.to_string())
}

/// "12 of your messages not audited since Nov 12", "3 of your messages never
/// audited"; `None` when there is nothing to audit.
pub fn unaudited_text(u: &Unaudited) -> Option<String> {
    if u.count == 0 {
        return None;
    }
    Some(match &u.since {
        Some(s) => format!(
            "{} of your messages not audited since {}",
            u.count,
            month_day(s)
        ),
        None => format!("{} of your messages never audited", u.count),
    })
}

// ------------------------------------------------------------ text

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// The line with the total and the count per state (only those with ideas).
pub fn summary(ideas: &Ideas) -> String {
    let mut parts = vec![plural(ideas.total, "idea", "ideas")];
    for c in &ideas.counts {
        if c.count > 0 {
            parts.push(format!("{} {}", c.state.label(), c.count));
        }
    }
    parts.join(" · ")
}

/// The terminal version: the total, then the ideas grouped by state.
pub fn text(ideas: &Ideas) -> String {
    let mut l = vec![format!("# Your ideas in {}", ideas.project)];
    l.push(String::new());
    if ideas.total == 0 {
        l.push(format!(
            "{} has no ideas yet.",
            ideas.path.to_string_lossy()
        ));
        if let Some(u) = unaudited_text(&ideas.unaudited) {
            l.push(u + ".");
        }
        return l.join("\n") + "\n";
    }
    l.push(summary(ideas) + ".");
    let ids: Vec<&str> = ideas.not_placed.iter().map(|n| n.id.as_str()).collect();
    if ids.is_empty() {
        l.push("Not placed: 0.".to_string());
    } else {
        l.push(format!("Not placed: {} ({}).", ids.len(), ids.join(", ")));
    }
    if let Some(u) = unaudited_text(&ideas.unaudited) {
        l.push(u + ".");
    }
    for w in &ideas.warnings {
        l.push(format!("warning: {w}"));
    }
    for s in IdeaState::ALL {
        let group = ideas.of_state(s);
        if group.is_empty() {
            continue;
        }
        l.push(String::new());
        l.push(format!("## {} ({})", capitalize(s.label()), group.len()));
        for i in group {
            let said = i
                .said
                .as_deref()
                .map(|d| format!(" · {d}"))
                .unwrap_or_default();
            l.push(format!("- {}{said} · {}", i.id, one_line(&i.idea, 120)));
            if !i.where_.is_empty() {
                l.push(format!("  where: {}", one_line(&i.where_, 160)));
            }
            for r in &i.refs {
                let cited = match &r.cited {
                    Some(c) => format!("{c} = {}", r.id),
                    None => r.id.clone(),
                };
                l.push(format!(
                    "  {cited}: {} {}: {}",
                    r.kind,
                    r.status,
                    one_line(&r.title, 90)
                ));
            }
            if !i.evidence.is_empty() {
                let ev: Vec<String> = i
                    .evidence
                    .iter()
                    .map(|e| match e.kind {
                        EvidenceKind::Image => format!("{} (image)", e.value),
                        EvidenceKind::Text => e.value.clone(),
                    })
                    .collect();
                l.push(format!("  evidence: {}", ev.join(", ")));
            }
            if let Some(c) = &i.cites {
                l.push(format!("  not placed: {c}"));
            }
            for n in &i.notes {
                l.push(format!("  note: {n}"));
            }
        }
    }
    l.join("\n") + "\n"
}

/// What `kontext ideas` says when there is no `.pm/items.md`.
pub const NONE_TEXT: &str = "There is no .pm/items.md in this repo or in the project's repos on this \
     machine: the kontext-pm skill keeps the user's ideas there.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cells_handle_escaped_pipes_and_edges() {
        assert_eq!(cells("| a | b \\| c | d |"), vec!["a", "b | c", "d"]);
        assert_eq!(cells("|a|b"), vec!["a", "b"]);
        assert_eq!(cells("| a |  |"), vec!["a", ""]);
    }

    #[test]
    fn sources_and_dates() {
        let s = parse_source("dumps/2026-11-07-2.md:3").unwrap();
        assert_eq!(s.file, "dumps/2026-11-07-2.md");
        assert_eq!(s.line, Some(3));
        assert_eq!(said_date(&s.file).as_deref(), Some("2026-11-07"));
        let s = parse_source(".pm/dumps/2026-11-07.md").unwrap();
        assert_eq!(s.file, "dumps/2026-11-07.md");
        assert_eq!(s.line, None);
        assert!(parse_source("  ").is_none());
        assert_eq!(said_date("notes.md"), None);
    }

    #[test]
    fn evidence_kinds() {
        let ev = parse_evidence("PR #12, shots/home.png , .pm/shots/b.jpg,, commit abc");
        let kinds: Vec<(&EvidenceKind, &str)> =
            ev.iter().map(|e| (&e.kind, e.value.as_str())).collect();
        assert_eq!(
            kinds,
            vec![
                (&EvidenceKind::Text, "PR #12"),
                (&EvidenceKind::Image, "shots/home.png"),
                (&EvidenceKind::Image, "shots/b.jpg"),
                (&EvidenceKind::Text, "commit abc"),
            ]
        );
    }

    #[test]
    fn id_tokens_find_both_kinds() {
        assert_eq!(
            id_tokens("T-4c1a2b and D-0030, D-0030; ACC-0003 · CVE-2024 PR #12 SHA-256 M7"),
            vec!["T-4c1a2b", "D-0030", "ACC-0003", "CVE-2024", "SHA-256"]
        );
        // `CVE` and `SHA` are not ledger prefixes: `known_prefixes` drops them.
    }

    #[test]
    fn parse_by_header_names() {
        let md = "# Ideas\n\
            \n\
            Some text | with a pipe that is not a row.\n\
            | Where | ID | Idea | Extra | State | Source | Evidence |\n\
            |:---|---|---|---|---:|---|---|\n\
            | D-a1b2c3 | P1 | split \\| merge | x | Done | dumps/2026-11-07.md:3 | PR #4 |\n\
            |  | | no id | | pending | | |\n\
            | | P2 | short row |\n";
        let rows = parse(md);
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert_eq!(rows[0].line, 6);
        assert_eq!(rows[0].id, "P1");
        assert_eq!(rows[0].idea, "split | merge");
        assert_eq!(rows[0].state, "Done");
        assert_eq!(rows[0].where_, "D-a1b2c3");
        assert_eq!(rows[0].source, "dumps/2026-11-07.md:3");
        assert_eq!(rows[0].evidence, "PR #4");
        assert_eq!(rows[1].id, "P2");
        assert_eq!(rows[1].state, "", "missing cells are empty");
    }

    #[test]
    fn parse_with_missing_columns() {
        // No `where` or `evidence` column: they are empty. No `id`: no rows.
        let rows = parse("| id | idea | state |\n|---|---|---|\n| P1 | a | done |\n");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].where_, "");
        assert_eq!(rows[0].evidence, "");
        assert!(parse("| idea | state |\n|---|---|\n| a | done |\n").is_empty());
        assert!(parse("").is_empty());
    }

    #[test]
    fn a_table_without_where_warns_once() {
        let st = state();
        let md = "| id | idea | state |\n|---|---|---|\n| P1 | a | done |\n| P2 | b | pending |\n";
        let ideas = build(&st, "demo", &[], Path::new("/nowhere"), md);
        assert_eq!(ideas.warnings, vec![NO_WHERE_WARNING.to_string()]);
        assert_eq!(ideas.count(IdeaState::NoDestination), 2, "the states stay");
        let t = text(&ideas);
        assert!(t.contains(&format!("warning: {NO_WHERE_WARNING}")), "{t}");
        // With a `where` column, or with no rows, nothing to warn about.
        let md = "| id | idea | where |\n|---|---|---|\n| P1 | a | |\n";
        assert!(
            build(&st, "demo", &[], Path::new("/nowhere"), md)
                .warnings
                .is_empty()
        );
        let md = "| id | idea | state |\n|---|---|---|\n";
        assert!(
            build(&st, "demo", &[], Path::new("/nowhere"), md)
                .warnings
                .is_empty()
        );
    }

    // ------------------------------------------------------------ rules

    use crate::model::{Actor, GENESIS};
    use crate::state::Policy;
    use serde_json::{Map, Value, json};

    fn e(id: &str, n: u32, kind: &str, refs: &[&str]) -> Entry {
        Entry {
            v: 1,
            id: id.into(),
            writer: "w".into(),
            n: 1,
            prev: GENESIS.into(),
            hash: String::new(),
            hlc: format!("2026-11-03T18:22:10.{n:03}Z.0000"),
            actor: Actor::human("omar"),
            kind: kind.into(),
            title: format!("Title of {id}"),
            body: String::new(),
            why: String::new(),
            tags: vec![],
            refs: refs.iter().map(|s| s.to_string()).collect(),
            fields: Map::new(),
            sig: None,
            extra: Map::new(),
        }
    }

    fn with(mut x: Entry, k: &str, v: Value) -> Entry {
        x.fields.insert(k.into(), v);
        x
    }

    fn by(mut x: Entry, actor: Actor) -> Entry {
        x.actor = actor;
        x
    }

    fn state() -> State {
        let entries = vec![
            e("D-000001", 1, "decision", &[]),
            // Open question to the owner, and one to an agent.
            e("Q-000002", 2, "question", &[]),
            with(e("Q-000003", 3, "question", &[]), "to", json!("coder")),
            // A pending proposal from an agent, imported from v0.
            with(
                with(
                    by(
                        e("P-000004", 4, "proposal", &[]),
                        Actor::agent("coordinator"),
                    ),
                    "v0_id",
                    json!("P-0012"),
                ),
                "status",
                json!("pending"),
            ),
            // An accepted proposal.
            e("P-000005", 5, "proposal", &[]),
            e("R-000006", 6, "accept", &["P-000005"]),
            // A question answered.
            e("Q-000007", 7, "question", &[]),
            e("R-000008", 8, "answer", &["Q-000007"]),
            // A question the human dismissed.
            e("Q-000009", 9, "question", &[]),
            with(
                e("R-00000a", 10, "dismiss", &[]),
                "items",
                json!(["Q-000009"]),
            ),
            with(e("D-00000b", 11, "decision", &[]), "v0_id", json!("D-0015")),
            with(e("Q-00000c", 12, "question", &[]), "to", json!("human:ana")),
            with(e("Q-00000d", 13, "question", &[]), "to", json!("lee")),
        ];
        State::build_at(entries, &Policy::new(&["omar"]), 0)
    }

    fn one(st: &State, row: &str) -> Idea {
        let md = format!(
            "| id | idea | source | state | where | evidence |\n|---|---|---|---|---|---|\n{row}\n"
        );
        let ideas = build(st, "demo", &["lee".to_string()], Path::new("/nowhere"), &md);
        assert_eq!(ideas.total, 1);
        ideas.ideas.into_iter().next().unwrap()
    }

    #[test]
    fn rule_a_discarded_wins() {
        let st = state();
        let i = one(&st, "| P1 | x | | Discarded | Q-000002 | dumps/a.md:1 |");
        assert_eq!(i.state, IdeaState::Discarded);
        assert_eq!(i.state_from, StateFrom::Table);
    }

    #[test]
    fn rule_b_no_destination() {
        let st = state();
        let i = one(&st, "| P1 | x | | done |  | PR #1 |");
        assert_eq!(i.state, IdeaState::NoDestination);
    }

    #[test]
    fn rule_c_waiting_for_you() {
        let st = state();
        for (cell, waits) in [
            ("Q-000002", true),  // open, to the owner
            ("Q-000003", false), // open, to an agent
            ("P-0012", true),    // pending, from v0 and from an agent
            ("P-000005", false), // accepted
            ("Q-000007", false), // answered
            ("Q-000009", false), // dismissed by the human
            ("Q-00000c", true),  // human:ana
            ("Q-00000d", true),  // lee is in [people]
            ("D-000001, Q-000002", true),
        ] {
            let i = one(&st, &format!("| P1 | x | | decided | {cell} | |"));
            assert_eq!(
                i.state == IdeaState::WaitingForYou,
                waits,
                "{cell}: {:?}",
                i.state
            );
            if waits {
                assert_eq!(i.state_from, StateFrom::Ledger);
            } else {
                assert_eq!(i.state, IdeaState::Decided);
                assert_eq!(i.state_from, StateFrom::Table);
            }
        }
    }

    #[test]
    fn rule_e_written_state_stands() {
        let st = state();
        let i = one(&st, "| P1 | x | | In-Progress | D-000001 | |");
        assert_eq!(i.state, IdeaState::InProgress);
        let i = one(&st, "| P1 | x | | pending | the roadmap | |");
        assert_eq!(i.state, IdeaState::Pending);
        let i = one(&st, "| P1 | x | | decided | D-0015: invoices | |");
        assert_eq!(i.state, IdeaState::Decided);
        assert_eq!(i.refs[0].id, "D-00000b");
        assert_eq!(i.refs[0].cited.as_deref(), Some("D-0015"));
        assert_eq!(i.refs[0].status, "accepted");
        let i = one(&st, "| P1 | x | | done | D-000001 | PR #12, shots/a.png |");
        assert_eq!(i.state, IdeaState::Done);
        assert!(i.notes.is_empty(), "{:?}", i.notes);
        let i = one(&st, "| P1 | x | | done | D-000001 | |");
        assert_eq!(i.state, IdeaState::InProgress);
        assert_eq!(i.notes, vec!["marked done without evidence"]);
        let i = one(&st, "| P1 | x | | someday | D-000001 | |");
        assert_eq!(i.state, IdeaState::Pending);
        assert!(i.notes[0].contains("\"someday\""), "{:?}", i.notes);
    }

    #[test]
    fn rule_f_unknown_ids_are_text() {
        let st = state();
        let i = one(
            &st,
            "| P1 | x | | decided | D-9999 and D-000001, CVE-2024-1 | PR #3 |",
        );
        assert_eq!(i.state, IdeaState::Decided);
        assert_eq!(i.notes, vec!["D-9999 is not in the ledger"]);
        assert_eq!(i.refs.len(), 1);
        assert_eq!(i.where_, "D-9999 and D-000001, CVE-2024-1");
    }

    #[test]
    fn quotes_come_from_the_dumps() {
        let dir = tempfile::tempdir().unwrap();
        let pm = &dir.path().join("pm");
        fs::create_dir_all(pm.join("dumps")).unwrap();
        // A file outside `.pm/`: never quoted.
        let outside = dir.path().join("secret.md");
        fs::write(&outside, "not a dump\n").unwrap();
        let long = "word ".repeat(80);
        fs::write(
            pm.join("dumps/2026-11-07-2.md"),
            format!("## 2026-11-07 10:42\n\n  export the invoices as CSV  \n{long}\n"),
        )
        .unwrap();
        let md = format!(
            "| id | idea | source | state | where | evidence |\n|---|---|---|---|---|---|\n\
             | P1 | csv | dumps/2026-11-07-2.md:3 | pending | | |\n\
             | P2 | long | dumps/2026-11-07-2.md:4 | pending | | |\n\
             | P3 | gone | dumps/2026-11-07-2.md:99 | pending | | |\n\
             | P4 | no file | dumps/2026-01-01.md:1 | pending | | |\n\
             | P5 | escape | ../secret.md:1 | pending | | |\n\
             | P6 | absolute | {}:1 | pending | | |\n",
            outside.display()
        );
        let ideas = build(&state(), "demo", &[], pm, &md);
        let q: Vec<Option<&str>> = ideas.ideas.iter().map(|i| i.quote.as_deref()).collect();
        assert_eq!(q[0], Some("export the invoices as CSV"));
        assert_eq!(q[1].unwrap().chars().count(), QUOTE_MAX);
        assert!(q[1].unwrap().ends_with('…'));
        assert_eq!(&q[2..], &[None, None, None, None]);
        assert_eq!(ideas.ideas[0].said.as_deref(), Some("2026-11-07"));
        assert_eq!(ideas.ideas[3].said.as_deref(), Some("2026-01-01"));
    }

    #[test]
    fn coordinator_lines_are_never_quoted_as_the_users() {
        let dir = tempfile::tempdir().unwrap();
        let pm = &dir.path().join("pm");
        fs::create_dir_all(pm.join("dumps")).unwrap();
        // Lines: 1 header, 3 asked marker, 5-6 the coordinator's, 8 user
        // marker, 10 the user's, 12 a `> ` the user typed, 14 the next header,
        // 16 user marker, 18 the user's.
        fs::write(
            pm.join("dumps/2026-11-07.md"),
            "## 2026-11-07 10:42\n\n**Coordinator asked:**\n\n\
             > Say go and I merge the PDF export\n> and drop the old one.\n\n\
             **User:**\n\ngo, and add paging\n\n> quoting the old spec: 30 per page\n\n\
             ## 2026-11-07 11:00\n\n**User:**\n\nkeep 50 per page\n",
        )
        .unwrap();
        let rows: String = [1, 3, 5, 6, 8, 10, 12, 14, 18]
            .iter()
            .map(|n| format!("| P{n} | x | dumps/2026-11-07.md:{n} | pending | | |\n"))
            .collect();
        let md = format!(
            "| id | idea | source | state | where | evidence |\n|---|---|---|---|---|---|\n{rows}"
        );
        let ideas = build(&state(), "demo", &[], pm, &md);
        let q: Vec<(&str, Option<&str>)> = ideas
            .ideas
            .iter()
            .map(|i| (i.id.as_str(), i.quote.as_deref()))
            .collect();
        assert_eq!(
            q,
            vec![
                ("P1", None),
                ("P3", None),
                ("P5", None),
                ("P6", None),
                ("P8", None),
                ("P10", Some("go, and add paging")),
                ("P12", Some("> quoting the old spec: 30 per page")),
                ("P14", None),
                ("P18", Some("keep 50 per page")),
            ]
        );
        let p5 = &ideas.ideas[2];
        assert_eq!(
            p5.notes,
            vec![
                "dumps/2026-11-07.md:5 is not the user's own text (a marker or a line the coordinator said): not quoted"
            ]
        );
        assert!(ideas.ideas[5].notes.is_empty());
        // The text view and the JSON never carry the coordinator's line as a quote.
        assert!(!text(&ideas).contains("Say go"));
        let json = serde_json::to_string(&ideas).unwrap();
        assert!(!json.contains("Say go"), "{json}");
    }

    #[test]
    fn counts_keep_the_display_order() {
        let st = state();
        let md = "| id | idea | source | state | where | evidence |\n|---|---|---|---|---|---|\n\
                  | P1 | a | | discarded | | |\n\
                  | P2 | b | | pending | | |\n\
                  | P3 | c | | done | D-000001 | PR #1 |\n";
        let ideas = build(&st, "demo", &[], Path::new("/nowhere"), md);
        let counts: Vec<(&str, usize)> = ideas
            .counts
            .iter()
            .map(|c| (c.state.label(), c.count))
            .collect();
        assert_eq!(
            counts,
            vec![
                ("no destination", 1),
                ("waiting for you", 0),
                ("pending", 0),
                ("in progress", 0),
                ("decided", 0),
                ("done", 1),
                ("discarded", 1),
            ]
        );
        let t = text(&ideas);
        assert!(
            t.contains("3 ideas · no destination 1 · done 1 · discarded 1."),
            "{t}"
        );
        let nd = t.find("## No destination (1)").unwrap();
        let done = t.find("## Done (1)").unwrap();
        let dis = t.find("## Discarded (1)").unwrap();
        assert!(nd < done && done < dis, "{t}");
    }
}
