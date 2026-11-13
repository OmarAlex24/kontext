//! The human's inbox (`kontext inbox`): what is waiting for an action from them.
//!
//! It is not a separate store: it is derived from the ledger every time. There
//! are four classes:
//!
//! - **sign**: charters and goals not signed yet, proposals from an agent that
//!   wait for an owner, and the user's rules (unsigned) that the distill noted.
//! - **answer**: open questions for the human (`owner` or their name), grouped
//!   by epic; the ones without an epic, by topic.
//! - **review**: what the distill noted from conversations and nobody has
//!   confirmed, in a single group.
//! - **problem**: closes that didn't count, reopened tasks, expired or lost
//!   claims, drift, invalid signatures, what doesn't count, conflicts, and
//!   epics that don't fit in any wave.
//!
//! Rule: seeing it closes nothing (not here, not in the report, not in the
//! hook). A pending item disappears when its cause is resolved in the ledger
//! (someone signs, answers, accepts, rejects or fixes it) or when the human
//! dismisses it (`dismiss`) or confirms it (`confirm`). "New" is what came in
//! after the last entry the human wrote, not after their last view.
//!
//! With a signed plan (`kontext scope sign`) the inbox splits: the owner's
//! holds only the pending scope signature (at most one), the questions for
//! them and the problems only an owner resolves (an invalid signature, two
//! signed charters); proposals, what the distill noted and the other problems
//! go to the pm's inbox (`kontext inbox --as pm`), which the pm resolves
//! alone. Without a signed plan nothing changes.
//!
//! Order: by impact (how many epics and tasks it blocks, directly or
//! transitively), then by age, oldest first.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use serde::Serialize;
use serde_json::{Value, json};
use unicode_normalization::UnicodeNormalization;

use crate::model::{Actor, Entry, RawEntry};
use crate::plan;
use crate::state::{Alert, State, Status, string_list};
use crate::store::{Refused, Store};
use crate::tasks::{self, TaskStatus};
use crate::views::{
    field, from_user, one_line, question_is_for, question_to, unreviewed, user_rule,
};

/// How many items are shown in full without `--all`.
pub const SHOWN: usize = 5;

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Class {
    #[serde(rename = "sign")]
    Sign,
    #[serde(rename = "answer")]
    Answer,
    #[serde(rename = "review")]
    Review,
    #[serde(rename = "problem")]
    Problem,
}

impl Class {
    pub fn label(self) -> &'static str {
        match self {
            Class::Sign => "Sign",
            Class::Answer => "Answer",
            Class::Review => "Review",
            Class::Problem => "Problem",
        }
    }

    fn plural(self, n: usize) -> String {
        let (one, many) = match self {
            Class::Sign => ("to sign", "to sign"),
            Class::Answer => ("to answer", "to answer"),
            Class::Review => ("to review", "to review"),
            Class::Problem => ("problem", "problems"),
        };
        format!("{n} {}", if n == 1 { one } else { many })
    }
}

/// An entry inside a grouped pending item (one question of a group, something
/// the distill noted).
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct Sub {
    pub id: String,
    pub text: String,
    pub by: String,
    pub hlc: String,
    pub new: bool,
}

/// A pending item.
#[derive(Serialize, Clone, Debug)]
pub struct Item {
    /// Its number in the inbox (1, 2…), for `kontext inbox sign 2`.
    pub n: usize,
    /// Stable key: the entry id (`Q-9b2`), `review`, or the problem type and
    /// its entry (`close:R-4c1`).
    pub key: String,
    pub class: Class,
    pub title: String,
    pub subs: Vec<Sub>,
    /// Problems: why it matters, in one line.
    pub why: Option<String>,
    /// The entries it touches.
    pub ids: Vec<String>,
    /// What a `dismiss` or a `confirm` of this item closes.
    pub keys: Vec<String>,
    /// The `hlc` of the oldest and the newest entry that form it.
    pub since: String,
    pub newest: String,
    /// It came in after the human's last action.
    pub new: bool,
    /// Who is waiting: whoever asked or proposed.
    pub waiting: Vec<String>,
    /// The epics it blocks (titles), directly or transitively.
    pub blocks_epics: Vec<String>,
    /// How many tasks it blocks.
    pub blocks_tasks: usize,
    /// The epics of those tasks (titles), unless already in `blocks_epics`.
    pub task_epics: Vec<String>,
    /// The question was marked not urgent (by the brake of `kontext ask`).
    pub not_urgent: bool,
    /// The epics it touches (ids, in ledger order): its own, those of its
    /// tasks and the ones it blocks. The report filters by them.
    #[serde(skip)]
    pub epics: Vec<String>,
    /// "blocks Payments and Notices · waits agent:epic:pagos · 3 days ago".
    pub line: String,
    /// The command that resolves it.
    pub command: String,
    /// Another way to close it (reject, dismiss).
    pub alt: Option<String>,
}

impl Item {
    pub fn score(&self) -> (usize, usize) {
        (self.blocks_epics.len(), self.blocks_tasks)
    }

    pub fn impact_text(&self) -> String {
        impact_text(&self.blocks_epics, self.blocks_tasks, &self.task_epics)
    }
}

/// A human's inbox.
#[derive(Serialize, Clone, Debug)]
pub struct Inbox {
    pub project: String,
    pub human: String,
    /// The `hlc` of the human's last write, if they wrote anything.
    pub last_action: Option<String>,
    pub now_ms: i64,
    pub items: Vec<Item>,
    /// With a signed plan, in an owner's inbox: how many items the pm handles.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pm_items: Option<usize>,
}

/// The reader of the pm's inbox (`kontext inbox --as pm`).
pub const PM: &str = "pm";

impl Inbox {
    pub fn new_count(&self) -> usize {
        self.items.iter().filter(|i| i.new).count()
    }

    pub fn oldest(&self) -> Option<&Item> {
        self.items.iter().min_by(|a, b| a.since.cmp(&b.since))
    }

    pub fn last_action_ago(&self) -> Option<String> {
        self.last_action
            .as_deref()
            .map(|h| tasks::ago(self.now_ms, tasks::hlc_ms(h)))
    }

    pub fn of_class(&self, c: Class) -> Vec<&Item> {
        self.items.iter().filter(|i| i.class == c).collect()
    }
}

// ------------------------------------------------------------ impact

/// What is needed to know what blocks what.
struct Graph {
    /// Task → the open tasks that depend on it.
    task_dependents: BTreeMap<String, Vec<String>>,
    /// Epic → the open epics that come after it.
    epic_dependents: BTreeMap<String, Vec<String>>,
    /// Open epics (in force and without all their tasks closed).
    open_epics: BTreeSet<String>,
}

impl Graph {
    fn new(st: &State) -> Graph {
        let mut task_dependents: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for t in st.of_kind("task") {
            if st.task_status(&t.id) == Some(TaskStatus::Closed) {
                continue;
            }
            for d in string_list(t.fields.get("depends_on")) {
                task_dependents.entry(d).or_default().push(t.id.clone());
            }
        }
        let nodes = plan::nodes(st);
        let open_epics: BTreeSet<String> = nodes
            .iter()
            .filter(|n| n.active && !n.closed())
            .map(|n| n.id.clone())
            .collect();
        let mut epic_dependents: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for n in nodes.iter().filter(|n| open_epics.contains(&n.id)) {
            for dep in &n.after {
                if let Some(d) = plan::resolve(&nodes, dep) {
                    epic_dependents
                        .entry(d.id.clone())
                        .or_default()
                        .push(n.id.clone());
                }
            }
        }
        Graph {
            task_dependents,
            epic_dependents,
            open_epics,
        }
    }

    /// The tasks and epics that something stopped by `tasks` and `epics`
    /// blocks. Returns (blocked epics, blocked tasks, epics of those tasks).
    fn impact(
        &self,
        st: &State,
        tasks: &[String],
        epics: &[String],
    ) -> (Vec<String>, Vec<String>, Vec<String>) {
        let mut blocked_tasks: BTreeSet<String> = BTreeSet::new();
        let mut stack: Vec<String> = tasks
            .iter()
            .filter(|t| st.task_status(t).is_some_and(|s| s != TaskStatus::Closed))
            .cloned()
            .collect();
        while let Some(t) = stack.pop() {
            if !blocked_tasks.insert(t.clone()) {
                continue;
            }
            for d in self.task_dependents.get(&t).into_iter().flatten() {
                stack.push(d.clone());
            }
        }
        let direct: BTreeSet<String> = epics
            .iter()
            .filter(|e| self.open_epics.contains(*e))
            .cloned()
            .collect();
        let task_epics: BTreeSet<String> = blocked_tasks
            .iter()
            .filter_map(|t| st.get(t).and_then(|t| st.epic_of(t)))
            .map(|e| e.id.clone())
            .collect();
        // Epics that come after a stopped epic (or one with a stopped task)
        // can't start either.
        let mut after: BTreeSet<String> = BTreeSet::new();
        let mut stack: Vec<String> = direct.iter().chain(task_epics.iter()).cloned().collect();
        let mut seen: BTreeSet<String> = stack.iter().cloned().collect();
        while let Some(e) = stack.pop() {
            for d in self.epic_dependents.get(&e).into_iter().flatten() {
                if seen.insert(d.clone()) {
                    after.insert(d.clone());
                    stack.push(d.clone());
                }
            }
        }
        let mut blocked: Vec<String> = direct.iter().cloned().collect();
        for e in after {
            if !blocked.contains(&e) {
                blocked.push(e);
            }
        }
        // In ledger order, so the text comes out stable.
        blocked.sort_by_key(|e| position(st, e));
        let mut te: Vec<String> = task_epics
            .into_iter()
            .filter(|e| !blocked.contains(e))
            .collect();
        te.sort_by_key(|e| position(st, e));
        (blocked, blocked_tasks.into_iter().collect(), te)
    }
}

/// `agent:pagos-impl-1`, without the role.
fn who(a: &Actor) -> String {
    format!("{}:{}", a.kind, a.name)
}

fn position(st: &State, id: &str) -> usize {
    st.order.iter().position(|x| x == id).unwrap_or(usize::MAX)
}

fn epic_title(st: &State, id: &str) -> String {
    st.get(id)
        .map(|e| one_line(&e.title, 40))
        .unwrap_or_else(|| id.to_string())
}

fn names_text(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [a] => a.clone(),
        [a, b] => format!("{a} and {b}"),
        [a, b, rest @ ..] => format!("{a}, {b} and {} more", rest.len()),
    }
}

/// "blocks Payments and Notices (and 3 tasks)", "blocks 2 tasks of Payments", or
/// empty if it blocks nothing.
pub fn impact_text(epics: &[String], tasks: usize, task_epics: &[String]) -> String {
    let t = |n: usize| {
        if n == 1 {
            "1 task".to_string()
        } else {
            format!("{n} tasks")
        }
    };
    match (epics.is_empty(), tasks) {
        (true, 0) => String::new(),
        (true, n) if task_epics.is_empty() => format!("blocks {}", t(n)),
        (true, n) => format!("blocks {} of {}", t(n), names_text(task_epics)),
        (false, 0) => format!("blocks {}", names_text(epics)),
        (false, n) => format!("blocks {} (and {})", names_text(epics), t(n)),
    }
}

// ------------------------------------------------------------ assemble

fn closed(st: &State, key: &str) -> bool {
    st.inbox_closed.contains_key(key)
}

/// An open question that waits for a person, as the inbox sees it: not
/// closed for the inbox, and addressed to `owner`, an owner, `human:<x>` or
/// someone in `[people]` (a `--to <name>` that is none of these may be an
/// agent).
pub fn waits_for_a_human(st: &State, q: &Entry, people: &[String]) -> bool {
    if st.status_of(&q.id) != Some(Status::Open) || closed(st, &q.id) {
        return false;
    }
    let raw = field(q, "to").unwrap_or("owner");
    let to = question_to(q);
    raw.starts_with("human:")
        || to == "owner"
        || st.policy.is_owner(to)
        || people.iter().any(|p| p == to)
}

/// The last thing the human wrote (`actor` `human:<name>`).
pub fn last_action(st: &State, human: &str) -> Option<String> {
    st.entries
        .values()
        .filter(|e| e.actor.is_human() && e.actor.name == human)
        .map(|e| e.hlc.clone())
        .max()
}

/// A project's default human: the first owner.
pub fn default_human(st: &State) -> Option<String> {
    st.policy.owners.first().cloned()
}

struct Draft {
    key: String,
    class: Class,
    title: String,
    subs: Vec<Sub>,
    why: Option<String>,
    ids: Vec<String>,
    keys: Vec<String>,
    waiting: Vec<String>,
    tasks: Vec<String>,
    epics: Vec<String>,
    /// Epics it touches without stopping them (a proposal for an epic): only
    /// to know which epic it belongs to.
    related: Vec<String>,
    not_urgent: bool,
    command: String,
    alt: Option<String>,
    /// The entry that dates the item (a problem: the ledger entry that caused
    /// it). Without it, the oldest of `ids`.
    anchor: Option<String>,
}

impl Draft {
    fn new(class: Class, key: &str, title: String, command: String) -> Draft {
        Draft {
            key: key.to_string(),
            class,
            title,
            subs: Vec::new(),
            why: None,
            ids: Vec::new(),
            keys: vec![key.to_string()],
            waiting: Vec::new(),
            tasks: Vec::new(),
            epics: Vec::new(),
            related: Vec::new(),
            not_urgent: false,
            command,
            alt: None,
            anchor: None,
        }
    }
}

/// The inbox of `human` in this state.
pub fn build(st: &State, project: &str, human: &str) -> Inbox {
    let last = last_action(st, human);
    let mut drafts: Vec<Draft> = Vec::new();
    let is_owner = st.policy.owners.is_empty() || st.policy.is_owner(human);
    if crate::scope::plan_signed(st) {
        if is_owner {
            scope_draft(st, project, &mut drafts);
        }
        answer_drafts(st, &|q| question_is_for(st, q, human), &mut drafts);
        owner_problems(st, true, &mut drafts);
        let mut ib = assemble(st, project, human, last, drafts);
        if is_owner {
            ib.pm_items = Some(build_pm(st, project).items.len());
        }
        return ib;
    }
    if is_owner {
        sign_drafts(st, Voice::You, &mut drafts);
    }
    answer_drafts(st, &|q| question_is_for(st, q, human), &mut drafts);
    if is_owner {
        review_drafts(st, Voice::You, &mut drafts);
    }
    problemas(st, &mut drafts);
    assemble(st, project, human, last, drafts)
}

/// The pm's inbox, with a signed plan: the proposals and the user's unsigned
/// rules (the pm accepts or rejects them), what the distill noted (the pm
/// confirms or dismisses it) and the problems that are not the owner's.
/// Empty without a signed plan: then all of that is the owner's.
pub fn build_pm(st: &State, project: &str) -> Inbox {
    let mut drafts: Vec<Draft> = Vec::new();
    if crate::scope::plan_signed(st) {
        proposal_drafts(st, Voice::All, &mut drafts);
        review_drafts(st, Voice::All, &mut drafts);
        owner_problems(st, false, &mut drafts);
    }
    let last = st
        .entries
        .values()
        .filter(|e| is_pm(&e.actor))
        .map(|e| e.hlc.clone())
        .max();
    assemble(st, project, PM, last, drafts)
}

/// The pm agent (`agent:<x>` with role pm).
pub fn is_pm(a: &Actor) -> bool {
    a.is_agent() && a.role.as_deref() == Some("pm")
}

/// The problems, keeping those only an owner resolves (`owner`) or the rest.
fn owner_problems(st: &State, owner: bool, out: &mut Vec<Draft>) {
    let mut all = Vec::new();
    problemas(st, &mut all);
    out.extend(all.into_iter().filter(|d| owner_only(st, &d.key) == owner));
}

/// A problem only an owner resolves: a signature that doesn't verify, or more
/// than one signed charter.
fn owner_only(st: &State, key: &str) -> bool {
    key.starts_with("signature:")
        || key
            .strip_prefix("conflict:")
            .is_some_and(|id| st.kind_of(id) == Some("charter"))
}

/// The scope change that waits for the owner (at most one), with the first
/// lines of its summary. It holds the tasks it covers.
fn scope_draft(st: &State, project: &str, out: &mut Vec<Draft>) {
    let Some(diff) = crate::scope::pending(st) else {
        return;
    };
    // Only a preview: the ideas line is left out below (the owner reads it in
    // the full summary, where it is signed).
    let summary = crate::scope::summary(
        st,
        project,
        &crate::scope::IdeasCheck::NotFound(String::new()),
    );
    let mut d = Draft::new(
        Class::Sign,
        SCOPE_KEY,
        format!("Scope change: {}", crate::scope::diff_line(st, &diff)),
        "kontext scope summary".into(),
    );
    d.alt = Some("say yes to the pm, who signs it with you".into());
    d.ids = diff.adds.iter().chain(&diff.removes).cloned().collect();
    d.keys = Vec::new();
    for line in preview(&summary.summary, SCOPE_LINES) {
        d.subs.push(Sub {
            id: String::new(),
            text: line,
            by: String::new(),
            hlc: String::new(),
            new: false,
        });
    }
    d.tasks = st
        .of_kind("task")
        .filter(|t| st.task_status(&t.id) != Some(TaskStatus::Closed))
        .filter(|t| crate::scope::blocked(st, t).is_some())
        .map(|t| t.id.clone())
        .collect();
    out.push(d);
}

/// The first lines of a scope change's summary, for the inbox: its sections
/// as "Enters (1):" and their lines, without the empty ones ("(nothing)"),
/// the title or the line that says since when. At most `max` lines, never
/// ending on a section's name.
fn preview(summary: &str, max: usize) -> Vec<String> {
    let mut out: Vec<(String, bool)> = Vec::new();
    let mut header: Option<String> = None;
    let mut skip = false;
    for l in summary.lines().map(str::trim) {
        if l.is_empty() || l.starts_with("# ") || l.starts_with("Since ") {
            continue;
        }
        if let Some(h) = l.strip_prefix("## ") {
            skip = h == "Your ideas";
            header = Some(format!("{h}:"));
            continue;
        }
        if skip {
            continue;
        }
        if l == "(nothing)" {
            header = None;
            continue;
        }
        if let Some(h) = header.take() {
            out.push((h, true));
        }
        out.push((l.to_string(), false));
    }
    out.truncate(max);
    while out.last().is_some_and(|(_, h)| *h) {
        out.pop();
    }
    out.into_iter().map(|(l, _)| l).collect()
}

/// The key of the pending scope signature in the inbox.
pub const SCOPE_KEY: &str = "scope";
/// How many lines of the summary the inbox shows.
const SCOPE_LINES: usize = 6;

/// Everyone's inbox (`kontext inbox --everyone`): one section per human with
/// what is theirs alone, and a common one with what is the same for several.
#[derive(Serialize, Clone, Debug)]
pub struct Everyone {
    pub project: String,
    pub now_ms: i64,
    /// The owners in `project.toml`, in order.
    pub owners: Vec<String>,
    /// Humans with pending items of their own: the owners in order, then the
    /// rest alphabetically.
    pub humans: Vec<Inbox>,
    /// Owners with no pending items of their own.
    pub quiet: Vec<String>,
    /// What any owner resolves (sign, review, questions to `owner`) and the
    /// problems: once only. `human` is empty.
    pub common: Inbox,
}

impl Everyone {
    pub fn count(&self) -> usize {
        self.humans.iter().map(|h| h.items.len()).sum::<usize>() + self.common.items.len()
    }

    fn is_owner(&self, human: &str) -> bool {
        self.owners.iter().any(|o| o == human)
    }
}

/// Who appears in everyone's inbox: the owners, and the recipients of open
/// questions who are humans (`--to human:<x>`, or the name of someone in
/// `[people]`). A `--to <name>` that is neither an owner nor in `[people]` may
/// be an agent, so it doesn't count.
fn humans(st: &State, people: &[String]) -> Vec<String> {
    let mut out: Vec<String> = st.policy.owners.clone();
    let mut others: BTreeSet<String> = BTreeSet::new();
    for q in st.open_questions() {
        if closed(st, &q.id) {
            continue;
        }
        let raw = field(q, "to").unwrap_or("owner");
        let to = question_to(q);
        let human = raw.starts_with("human:") || people.iter().any(|p| p == to);
        if human && to != "owner" && !st.policy.is_owner(to) {
            others.insert(to.to_string());
        }
    }
    out.extend(others);
    out
}

/// Everyone's inbox. Each person gets only the questions addressed to them by
/// name; the rest is the same for every owner (or for everyone, the problems)
/// and goes in `common`, which counts what is new since the last action of any
/// owner.
pub fn everyone(st: &State, project: &str, people: &[String]) -> Everyone {
    let mut humans = Vec::new();
    let mut quiet = Vec::new();
    for h in self::humans(st, people) {
        let mut drafts = Vec::new();
        answer_drafts(st, &|q| question_to(q) == h, &mut drafts);
        let ib = assemble(st, project, &h, last_action(st, &h), drafts);
        if ib.items.is_empty() {
            if st.policy.is_owner(&h) {
                quiet.push(h);
            }
        } else {
            humans.push(ib);
        }
    }
    let mut drafts = Vec::new();
    if crate::scope::plan_signed(st) {
        scope_draft(st, project, &mut drafts);
        answer_drafts(st, &|q| question_to(q) == "owner", &mut drafts);
        owner_problems(st, true, &mut drafts);
    } else {
        sign_drafts(st, Voice::All, &mut drafts);
        answer_drafts(st, &|q| question_to(q) == "owner", &mut drafts);
        review_drafts(st, Voice::All, &mut drafts);
        problemas(st, &mut drafts);
    }
    let last = st
        .policy
        .owners
        .iter()
        .filter_map(|o| last_action(st, o))
        .max();
    Everyone {
        project: project.to_string(),
        now_ms: st.now_ms,
        owners: st.policy.owners.clone(),
        humans,
        quiet,
        common: assemble(st, project, "", last, drafts),
    }
}

/// How the reader is addressed: a human in their own inbox, or everyone.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Voice {
    You,
    All,
}

/// Sorts the pending items by impact and numbers them.
fn assemble(
    st: &State,
    project: &str,
    human: &str,
    last: Option<String>,
    drafts: Vec<Draft>,
) -> Inbox {
    let graph = Graph::new(st);
    let now = st.now_ms;
    let mut items: Vec<Item> = drafts
        .into_iter()
        .filter(|d| d.keys.is_empty() || !d.keys.iter().all(|k| closed(st, k)))
        .map(|d| {
            let hlcs: Vec<&str> = d
                .ids
                .iter()
                .filter_map(|id| st.get(id))
                .map(|e| e.hlc.as_str())
                .collect();
            let since = d
                .anchor
                .as_deref()
                .and_then(|a| st.get(a))
                .map(|e| e.hlc.clone())
                .or_else(|| hlcs.iter().min().map(|s| s.to_string()))
                .unwrap_or_default();
            let newest = hlcs.iter().max().map(|s| s.to_string()).unwrap_or_default();
            let new = last.as_deref().is_none_or(|l| newest.as_str() > l);
            let (epics, tasks, task_epics) = if d.not_urgent {
                (Vec::new(), Vec::new(), Vec::new())
            } else {
                graph.impact(st, &d.tasks, &d.epics)
            };
            let touched: BTreeSet<&str> = d
                .epics
                .iter()
                .chain(&d.related)
                .map(String::as_str)
                .chain(
                    d.tasks
                        .iter()
                        .filter_map(|t| st.get(t).and_then(|t| st.epic_of(t)))
                        .map(|e| e.id.as_str()),
                )
                .chain(epics.iter().map(String::as_str))
                .chain(task_epics.iter().map(String::as_str))
                .collect();
            let epic_ids: Vec<String> = st
                .epics()
                .iter()
                .filter(|e| touched.contains(e.id.as_str()))
                .map(|e| e.id.clone())
                .collect();
            let mut subs = d.subs;
            for s in &mut subs {
                s.new = last.as_deref().is_none_or(|l| s.hlc.as_str() > l);
            }
            let blocks_epics: Vec<String> = epics.iter().map(|e| epic_title(st, e)).collect();
            let task_epics: Vec<String> = task_epics.iter().map(|e| epic_title(st, e)).collect();
            let mut parts = Vec::new();
            let impact = impact_text(&blocks_epics, tasks.len(), &task_epics);
            if !impact.is_empty() {
                parts.push(impact);
            } else if d.not_urgent {
                parts.push("not urgent".to_string());
            }
            if !d.waiting.is_empty() {
                let verb = if d.waiting.len() == 1 {
                    "waits"
                } else {
                    "wait"
                };
                parts.push(format!("{verb} {}", names_text(&d.waiting)));
            }
            if !since.is_empty() {
                parts.push(tasks::ago(now, tasks::hlc_ms(&since)));
            }
            Item {
                n: 0,
                key: d.key,
                class: d.class,
                title: d.title,
                subs,
                why: d.why,
                ids: d.ids,
                keys: d.keys.into_iter().filter(|k| !closed(st, k)).collect(),
                since,
                newest,
                new,
                waiting: d.waiting,
                blocks_epics,
                blocks_tasks: tasks.len(),
                task_epics,
                not_urgent: d.not_urgent,
                epics: epic_ids,
                line: parts.join(" · "),
                command: d.command,
                alt: d.alt,
            }
        })
        .collect();
    items.sort_by(|a, b| {
        b.score()
            .cmp(&a.score())
            .then_with(|| a.since.cmp(&b.since))
            .then_with(|| a.class.cmp(&b.class))
            .then_with(|| a.key.cmp(&b.key))
    });
    for (i, it) in items.iter_mut().enumerate() {
        it.n = i + 1;
    }
    Inbox {
        project: project.to_string(),
        human: human.to_string(),
        last_action: last,
        now_ms: now,
        items,
        pm_items: None,
    }
}

fn sign_drafts(st: &State, voice: Voice, out: &mut Vec<Draft>) {
    let all_epics: Vec<String> = st.epics().iter().map(|e| e.id.clone()).collect();
    for c in st.unsigned_charters() {
        let mut d = Draft::new(
            Class::Sign,
            &c.id,
            format!("Charter: {}", one_line(&c.title, 120)),
            format!("kontext sign {}", c.id),
        );
        d.ids.push(c.id.clone());
        // Without a signed charter nothing has a direction: it stops every epic.
        if st.charter().is_none() {
            d.epics = all_epics.clone();
        }
        out.push(d);
    }
    for g in st.with_status("goal", &[Status::Unsigned]) {
        let mut d = Draft::new(
            Class::Sign,
            &g.id,
            format!("Goal: {}", one_line(&g.title, 120)),
            format!("kontext sign {}", g.id),
        );
        d.ids.push(g.id.clone());
        d.epics = st
            .epics()
            .iter()
            .filter(|e| string_list(e.fields.get("goals")).contains(&g.id))
            .map(|e| e.id.clone())
            .collect();
        out.push(d);
    }
    proposal_drafts(st, voice, out);
}

/// The proposals that wait for an answer and the user's unsigned rules.
fn proposal_drafts(st: &State, voice: Voice, out: &mut Vec<Draft>) {
    for p in st.pending_proposals() {
        let rule = user_rule(p);
        if unreviewed(p) && !rule {
            continue; // goes in "review"
        }
        let title = if rule {
            let whose = match voice {
                Voice::You => "Your unsigned rule",
                Voice::All => "Unsigned user rule",
            };
            format!("{whose}: {}", one_line(&p.title, 120))
        } else {
            format!("Proposal: {}", one_line(&p.title, 120))
        };
        let mut d = Draft::new(
            Class::Sign,
            &p.id,
            title,
            format!("kontext accept {}", p.id),
        );
        d.alt = Some(format!("kontext reject {} --why \"…\"", p.id));
        d.ids.push(p.id.clone());
        d.related = field(p, "epic")
            .and_then(|n| st.find_epic(n).ok())
            .into_iter()
            .collect();
        if !rule {
            d.waiting.push(who(&p.actor));
        }
        out.push(d);
    }
}

/// A question's epic: `fields.epic` (name) or the epic of its task.
fn question_epic(st: &State, q: &Entry) -> Option<String> {
    if let Some(n) = field(q, "epic") {
        return st.find_epic(n).ok();
    }
    field(q, "task")
        .and_then(|t| st.get(t))
        .and_then(|t| st.epic_of(t))
        .map(|e| e.id.clone())
}

fn answer_drafts(st: &State, pick: &dyn Fn(&Entry) -> bool, out: &mut Vec<Draft>) {
    let qs: Vec<&Entry> = st
        .open_questions()
        .into_iter()
        .filter(|q| pick(q) && !closed(st, &q.id))
        .collect();
    // Groups: by epic; without an epic, by topic (similar questions).
    let mut groups: Vec<(Option<String>, Vec<&Entry>)> = Vec::new();
    for q in qs {
        let epic = question_epic(st, q);
        let slot = groups.iter_mut().find(|(e, members)| match (&epic, e) {
            (Some(a), Some(b)) => a == b,
            (None, None) => similar(&members[0].title, &q.title),
            _ => false,
        });
        match slot {
            Some((_, members)) => members.push(q),
            None => groups.push((epic, vec![q])),
        }
    }
    for (epic, members) in groups {
        let first = members[0];
        let title = match (&epic, members.len()) {
            (_, 1) => one_line(&first.title, 140),
            (Some(e), n) => format!("{n} questions in {}", epic_title(st, e)),
            (None, n) => format!("{n} questions about {}", one_line(&first.title, 80)),
        };
        let command = if members.len() == 1 {
            format!("kontext answer {} \"…\"", first.id)
        } else {
            "kontext answer <Q> \"…\" (one per question)".to_string()
        };
        let mut d = Draft::new(Class::Answer, &first.id, title, command);
        d.alt = Some(format!("kontext inbox dismiss {} --why \"…\"", first.id));
        d.keys = members.iter().map(|q| q.id.clone()).collect();
        // Which epics they belong to (even if they don't stop them): the
        // group's, and those of each question's task.
        d.related = members
            .iter()
            .filter_map(|q| question_epic(st, q))
            .chain(epic.iter().cloned())
            .collect();
        let mut all_not_urgent = true;
        for q in &members {
            d.ids.push(q.id.clone());
            let mut askers = vec![who(&q.actor)];
            let mut tasks_of_q: Vec<String> =
                field(q, "task").map(str::to_string).into_iter().collect();
            for r in st.joined.get(&q.id).into_iter().flatten() {
                if let Some(c) = st.get(r) {
                    d.ids.push(c.id.clone());
                    let who = who(&c.actor);
                    if !askers.contains(&who) {
                        askers.push(who);
                    }
                    if let Some(t) = field(c, "task") {
                        tasks_of_q.push(t.to_string());
                    }
                }
            }
            for a in &askers {
                if !d.waiting.contains(a) {
                    d.waiting.push(a.clone());
                }
            }
            // The `--blocking` of whoever joined counts too.
            let joined_blocking = st
                .joined
                .get(&q.id)
                .into_iter()
                .flatten()
                .filter_map(|r| st.get(r))
                .any(|c| c.fields.get("blocking").and_then(Value::as_bool) == Some(true));
            let blocking = if joined_blocking {
                Some(true)
            } else {
                q.fields.get("blocking").and_then(Value::as_bool)
            };
            if blocking != Some(false) {
                all_not_urgent = false;
                d.tasks.extend(tasks_of_q.iter().cloned());
                // Without a task, the question stops its epic only if it said
                // --blocking.
                if tasks_of_q.is_empty()
                    && blocking == Some(true)
                    && let Some(e) = question_epic(st, q)
                {
                    d.epics.push(e);
                }
            }
            let joined = askers.len().saturating_sub(1);
            let by = if joined == 0 {
                askers[0].clone()
            } else {
                format!("{}; joined by {}", askers[0], names_text(&askers[1..]))
            };
            d.subs.push(Sub {
                id: q.id.clone(),
                text: one_line(&q.title, 160),
                by,
                hlc: q.hlc.clone(),
                new: false,
            });
        }
        d.not_urgent = all_not_urgent;
        out.push(d);
    }
}

fn review_drafts(st: &State, voice: Voice, out: &mut Vec<Draft>) {
    let pending: Vec<&Entry> = st
        .order
        .iter()
        .filter_map(|id| st.get(id))
        .filter(|e| unreviewed(e) && !closed(st, &e.id))
        .filter(|e| match e.kind.as_str() {
            "fact" => {
                st.status_of(&e.id) == Some(Status::Active) && crate::facts::is_live(st, &e.id)
            }
            "proposal" => st.status_of(&e.id) == Some(Status::Pending) && !user_rule(e),
            _ => false,
        })
        .collect();
    if pending.is_empty() {
        return;
    }
    let n = pending.len();
    let of = match voice {
        Voice::You => "from your conversations",
        Voice::All => "from conversations",
    };
    let title = if n == 1 {
        format!("1 thing the distill noted {of}")
    } else {
        format!("{n} things the distill noted {of}")
    };
    let mut d = Draft::new(
        Class::Review,
        "review",
        title,
        "kontext inbox confirm review".into(),
    );
    d.alt = Some("kontext inbox dismiss review --why \"…\"".into());
    d.keys = pending.iter().map(|e| e.id.clone()).collect();
    for e in pending.iter().rev() {
        d.ids.push(e.id.clone());
        let what = match (e.kind.as_str(), from_user(e), voice) {
            ("proposal", true, Voice::You) => "your proposal",
            ("proposal", true, Voice::All) => "proposal from the user",
            ("proposal", false, _) => "proposal",
            (_, true, Voice::You) => "you said",
            (_, true, Voice::All) => "the user said",
            _ => "fact",
        };
        d.subs.push(Sub {
            id: e.id.clone(),
            text: format!("{what}: {}", one_line(&e.title, 150)),
            by: who(&e.actor),
            hlc: e.hlc.clone(),
            new: false,
        });
    }
    out.push(d);
}

fn task_title(st: &State, id: &str) -> String {
    st.get(id)
        .map(|t| one_line(&t.title, 90))
        .unwrap_or_else(|| id.to_string())
}

fn is_closed_task(st: &State, id: &str) -> bool {
    st.task_status(id) == Some(TaskStatus::Closed)
        || st
            .feature(id)
            .is_some_and(|f| f.status == crate::features::FeatureStatus::Done)
}

fn problemas(st: &State, out: &mut Vec<Draft>) {
    let mut push = |key: String, title: String, why: &str, ids: Vec<String>, tasks: Vec<String>| {
        let mut d = Draft::new(
            Class::Problem,
            &key,
            title,
            format!("kontext inbox dismiss {key} --why \"…\""),
        );
        d.why = Some(why.to_string());
        d.anchor = ids.first().cloned();
        d.ids = ids;
        d.tasks = tasks;
        out.push(d);
    };
    for a in &st.alerts {
        match a {
            Alert::TaskRecordIgnored {
                id,
                task,
                kind,
                actor,
                reason,
            } if !is_closed_task(st, task) => {
                let feature = st.kind_of(task) == Some("feature");
                let (key, title, why) = if kind == "done" && feature {
                    (
                        format!("close:{id}"),
                        format!(
                            "Validation that didn't count: {} ({reason})",
                            task_title(st, task)
                        ),
                        format!("{actor} validated the feature, but it is not done"),
                    )
                } else if kind == "done" {
                    (
                        format!("close:{id}"),
                        format!(
                            "Close that didn't count: {} ({reason})",
                            task_title(st, task)
                        ),
                        format!("{actor} marked it done, but the task is still open"),
                    )
                } else {
                    (
                        format!("not-counted:{id}"),
                        format!(
                            "{} of {} didn't count ({reason})",
                            crate::views::kind_label(kind),
                            task_title(st, task)
                        ),
                        "the board is not what the agent believes".to_string(),
                    )
                };
                push(
                    key,
                    title,
                    &why,
                    vec![id.clone(), task.clone()],
                    vec![task.clone()],
                );
            }
            Alert::ClaimExpired { task, holder, .. } => {
                let record = st
                    .task(task)
                    .and_then(|t| t.expired.as_ref())
                    .map(|c| c.record.clone());
                push(
                    format!("expired:{task}"),
                    format!(
                        "Nobody is working on {}: {holder}'s claim expired",
                        task_title(st, task)
                    ),
                    "if the agent died, it has to be relaunched or reassigned",
                    record.into_iter().chain([task.clone()]).collect(),
                    vec![task.clone()],
                )
            }
            Alert::ClaimLost {
                task,
                record,
                loser,
                winner,
                ..
            } => push(
                format!("claim:{record}"),
                format!(
                    "Two agents on {}: {loser} lost it to {winner}",
                    task_title(st, task)
                ),
                "one of the two is working for nothing",
                vec![record.clone(), task.clone()],
                Vec::new(),
            ),
            Alert::Drift { id, kind, reason } => push(
                format!("drift:{id}"),
                format!(
                    "Outside the charter: {} {} {reason}",
                    if kind == "epic" {
                        "the epic"
                    } else {
                        "the task"
                    },
                    task_title(st, id)
                ),
                "it is work that serves no signed goal",
                vec![id.clone()],
                Vec::new(),
            ),
            Alert::BadSignature {
                id, kind, reason, ..
            } => push(
                format!("signature:{id}"),
                format!(
                    "{} {id} with an invalid signature ({reason})",
                    crate::views::kind_label(kind)
                ),
                "it looks like law, but it doesn't count",
                vec![id.clone()],
                Vec::new(),
            ),
            Alert::NotCounting {
                id, kind, actor, ..
            } if kind != "dismiss" => push(
                format!("not-counted:{id}"),
                format!(
                    "{} {id} from {actor} does not count: not an owner",
                    crate::views::kind_label(kind)
                ),
                "an agent tried to decide for you",
                vec![id.clone()],
                Vec::new(),
            ),
            Alert::Conflict { target, by } => push(
                format!("conflict:{target}"),
                format!(
                    "Conflict: {} replace {target} at the same time",
                    by.join(" and ")
                ),
                "two rules in force say different things; agents don't know which to follow",
                std::iter::once(target.clone())
                    .chain(by.iter().cloned())
                    .collect(),
                Vec::new(),
            ),
            Alert::ScopeInvalid { id, reason } => push(
                format!("scope:{id}"),
                format!("Scope signature {id} does not count: {reason}"),
                "what is stored is not what the owner signed; the scope stands on the last \
                 signature that holds",
                vec![id.clone()],
                Vec::new(),
            ),
            Alert::CharterConflict { active, others } => push(
                format!("conflict:{active}"),
                format!("There is more than one signed charter: {active} applies"),
                "the project's direction is ambiguous",
                std::iter::once(active.clone())
                    .chain(others.iter().cloned())
                    .collect(),
                Vec::new(),
            ),
            _ => {}
        }
    }
    // Conflicts with the epics that those decisions touch.
    for d in out.iter_mut().filter(|d| d.key.starts_with("conflict:")) {
        for e in st.epics() {
            if d.ids
                .iter()
                .filter_map(|id| st.get(id))
                .any(|x| tasks::touches_epic(x, e))
            {
                d.epics.push(e.id.clone());
            }
        }
    }
    // Reopened tasks that are still open.
    for (id, info) in &st.tasks {
        if info.status == TaskStatus::Closed {
            continue;
        }
        let last = info.history.iter().rev().find(|h| h.counted);
        if let Some(h) = last.filter(|h| h.kind == "reopen") {
            let why = st
                .get(&h.record)
                .map(|r| one_line(&r.why, 120))
                .unwrap_or_default();
            let mut d = Draft::new(
                Class::Problem,
                &format!("reopened:{}", h.record),
                format!("Reopened: {} ({why})", task_title(st, id)),
                format!("kontext inbox dismiss reopened:{} --why \"…\"", h.record),
            );
            d.why = Some(format!(
                "it had been marked done and {} says it doesn't meet the bar",
                h.actor.split(" (").next().unwrap_or(&h.actor)
            ));
            d.ids = vec![h.record.clone(), id.clone()];
            d.anchor = Some(h.record.clone());
            d.tasks = vec![id.clone()];
            out.push(d);
        }
        // A dependency that doesn't exist: the task will never be ready.
        for b in &info.blocked_by {
            if st.kind_of(b) != Some("task") {
                let mut d = Draft::new(
                    Class::Problem,
                    &format!("dependency:{id}"),
                    format!("{} waits for {b}, which is not a task", task_title(st, id)),
                    format!("kontext inbox dismiss dependency:{id} --why \"…\""),
                );
                d.why = Some("that task will never be ready".into());
                d.ids = vec![id.clone()];
                d.tasks = vec![id.clone()];
                out.push(d);
            }
        }
    }
    // Epics that don't fit in any wave.
    let p = plan::of_state(st);
    for b in &p.blocked {
        let mut d = Draft::new(
            Class::Problem,
            &format!("plan:{}", b.epic.id),
            format!("{} doesn't fit in any wave: {}", b.epic.title, b.reason),
            format!("kontext inbox dismiss plan:{} --why \"…\"", b.epic.id),
        );
        d.why = Some("that epic can't start; `kontext plan` says what's missing".into());
        d.ids = vec![b.epic.id.clone()];
        d.epics = vec![b.epic.id.clone()];
        out.push(d);
    }
}

// ------------------------------------------------------------ text

/// "3 days ago" for the oldest pending item, etc.
pub fn count_line(ib: &Inbox) -> String {
    let n = ib.items.len();
    if n == 0 {
        return "No pending items.".into();
    }
    let mut parts = vec![if n == 1 {
        "1 pending item".to_string()
    } else {
        format!("{n} pending items")
    }];
    let new = ib.new_count();
    if new > 0 && new < n {
        parts[0].push_str(&format!(" ({new} new)"));
    }
    if let Some(o) = ib.oldest() {
        parts.push(format!(
            "oldest, from {}",
            tasks::ago(ib.now_ms, tasks::hlc_ms(&o.since))
        ));
    }
    if let Some(first) = ib.items.first() {
        let impact = first.impact_text();
        if !impact.is_empty() {
            parts.push(format!("the first {impact}"));
        }
    }
    parts.join(" · ")
}

/// The lines of a pending item. Without `numbered` (everyone's inbox) they get
/// a dash: their numbers are nobody's inbox numbers, so they don't work with
/// `kontext inbox sign 2`.
fn item_lines(it: &Item, numbered: bool, l: &mut Vec<String>) {
    let new = if it.new { " · new" } else { "" };
    if numbered {
        l.push(format!("{}. {} · {}", it.n, it.class.label(), it.title));
    } else {
        l.push(format!("-  {} · {}", it.class.label(), it.title));
    }
    l.push(format!("   {}{new}", it.line));
    if let Some(w) = &it.why {
        l.push(format!("   Why it matters: {w}"));
    }
    let max_subs = if it.class == Class::Review { 3 } else { 8 };
    if it.subs.len() > 1 || it.class == Class::Review {
        for (i, s) in it.subs.iter().take(max_subs).enumerate() {
            let tag = if it.class == Class::Answer && numbered {
                format!("{}.{} {}", it.n, i + 1, s.id)
            } else {
                s.id.clone()
            };
            let who = if it.class == Class::Answer {
                format!(" ({})", s.by)
            } else {
                String::new()
            };
            if s.id.is_empty() {
                l.push(format!("   | {}", s.text));
            } else {
                l.push(format!("   - {tag} {}{who}", s.text));
            }
        }
        if it.subs.len() > max_subs {
            l.push(format!("   - … and {} more", it.subs.len() - max_subs));
        }
    } else if let Some(s) = it.subs.first()
        && it.class == Class::Answer
    {
        l.push(format!("   From {}", s.by));
    }
    match &it.alt {
        Some(a) if it.class != Class::Problem => l.push(format!("   → {}   (or {a})", it.command)),
        _ => l.push(format!("   → {}", it.command)),
    }
}

/// `kontext inbox`: the first 5 in full and "and N more", or all with `all`.
pub fn text(ib: &Inbox, all: bool) -> String {
    let mut l = vec![format!(
        "# Pending for {} in {}: {}",
        ib.human,
        ib.project,
        ib.items.len()
    )];
    let since = match ib.last_action_ago() {
        Some(a) => format!("your last action in the ledger was {a}"),
        None => "you haven't written anything in the ledger yet".into(),
    };
    if ib.items.is_empty() {
        l.push(String::new());
        l.push(format!(
            "Nothing is waiting for an action from you ({since})."
        ));
        l.extend(pm_footer(ib));
        return l.join("\n") + "\n";
    }
    let new = ib.new_count();
    l.push(format!(
        "{} since your last action; {since}. Seeing them doesn't close them: sign, answer, confirm or dismiss.",
        match new {
            0 => "None is new".to_string(),
            1 => "1 is new".to_string(),
            n => format!("{n} are new"),
        }
    ));
    items_lines(&ib.items, all, true, "`kontext inbox --all`", &mut l);
    l.extend(pm_footer(ib));
    l.join("\n") + "\n"
}

/// With a signed plan, at the end of an owner's inbox: what the pm handles.
fn pm_footer(ib: &Inbox) -> Option<String> {
    let n = ib.pm_items.filter(|n| *n > 0)?;
    Some(format!(
        "\nThe pm handles {} inside the signed scope (proposals, what the distill noted, \
         problems): `kontext inbox --as pm`.",
        if n == 1 {
            "1 more item".to_string()
        } else {
            format!("{n} more items")
        }
    ))
}

/// The first 5 (or all with `all`) and "And N more: …".
fn items_lines(items: &[Item], all: bool, numbered: bool, more: &str, l: &mut Vec<String>) {
    let shown = if all { items.len() } else { SHOWN };
    for it in items.iter().take(shown) {
        l.push(String::new());
        item_lines(it, numbered, l);
    }
    let rest = &items[shown.min(items.len())..];
    if !rest.is_empty() {
        let mut by: BTreeMap<Class, usize> = BTreeMap::new();
        for it in rest {
            *by.entry(it.class).or_default() += 1;
        }
        let parts: Vec<String> = by.iter().map(|(c, n)| c.plural(*n)).collect();
        l.push(String::new());
        l.push(format!(
            "And {} more: {}. {more} shows them.",
            rest.len(),
            parts.join(", ")
        ));
    }
}

/// The heading of a human's section in everyone's inbox.
fn person_title(owner: bool, ib: &Inbox) -> String {
    let who = if owner {
        ib.human.clone()
    } else {
        format!("{} (not an owner)", ib.human)
    };
    format!("{who}: {}", count_line(ib))
}

fn last_action_text(ib: &Inbox) -> String {
    match ib.last_action_ago() {
        Some(a) => format!("Their last action in the ledger was {a}."),
        None => "They haven't written anything in the ledger yet.".into(),
    }
}

/// `kontext inbox --everyone`: one section per human, and the common one at the end.
pub fn everyone_text(ev: &Everyone, all: bool) -> String {
    let total = ev.count();
    let mut l = vec![format!("# Pending for everyone in {}: {total}", ev.project)];
    if total == 0 {
        l.push(String::new());
        l.push("Nothing is waiting for anyone's action.".into());
        return l.join("\n") + "\n";
    }
    let parts: Vec<String> = ev
        .humans
        .iter()
        .map(|h| format!("{} {}", h.human, h.items.len()))
        .collect();
    let mut intro = String::new();
    if !parts.is_empty() {
        intro.push_str(&format!("Per person: {}. ", names_list(&parts)));
    }
    if !ev.common.items.is_empty() {
        intro.push_str(&format!("Common: {}. ", ev.common.items.len()));
    }
    intro.push_str(
        "Seeing them doesn't close them: each person signs, answers, confirms or dismisses \
         from their own `kontext inbox`.",
    );
    if !ev.quiet.is_empty() {
        intro.push_str(&format!(
            " No pending items of their own: {}.",
            names_list(&ev.quiet)
        ));
    }
    l.push(intro);
    for ib in &ev.humans {
        l.push(String::new());
        l.push(format!("## {}", person_title(ev.is_owner(&ib.human), ib)));
        l.push(last_action_text(ib));
        items_lines(
            &ib.items,
            all,
            false,
            "`kontext inbox --everyone --all`",
            &mut l,
        );
    }
    if !ev.common.items.is_empty() {
        l.push(String::new());
        l.push(format!("## Common: {}", count_line(&ev.common)));
        l.push(
            "Any owner resolves it (sign, answer `owner`, review); anyone can resolve the \
             problems. What is new is counted from the last action of any owner."
                .into(),
        );
        items_lines(
            &ev.common.items,
            all,
            false,
            "`kontext inbox --everyone --all`",
            &mut l,
        );
    }
    l.join("\n") + "\n"
}

fn names_list(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [a] => a.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

/// `kontext inbox count --everyone`: one line per human (owners without pending
/// items of their own too), and one for the common items.
pub fn everyone_count(ev: &Everyone) -> String {
    let mut l: Vec<String> = Vec::new();
    for o in &ev.owners {
        match ev.humans.iter().find(|h| &h.human == o) {
            Some(ib) => l.push(format!("{o}: {}", count_line(ib))),
            None => l.push(format!("{o}: no pending items of their own.")),
        }
    }
    for ib in ev.humans.iter().filter(|h| !ev.is_owner(&h.human)) {
        l.push(person_title(false, ib));
    }
    l.push(format!("common: {}", count_line(&ev.common)));
    l.join("\n") + "\n"
}

fn section_json(ib: &Inbox, all: bool) -> Value {
    let shown = if all { ib.items.len() } else { SHOWN };
    json!({
        "last_action": ib.last_action,
        "count": ib.items.len(),
        "new": ib.new_count(),
        "line": count_line(ib),
        "items": ib.items.iter().take(shown).collect::<Vec<_>>(),
        "more": ib.items.len().saturating_sub(shown),
    })
}

/// `kontext inbox count --everyone --json`: what `everyone_json` gives, without
/// the items.
pub fn everyone_count_json(ev: &Everyone) -> Value {
    let mut v = everyone_json(ev, false);
    let strip = |s: &mut Value| {
        if let Some(o) = s.as_object_mut() {
            o.remove("items");
            o.remove("more");
        }
    };
    if let Some(hs) = v["humans"].as_array_mut() {
        hs.iter_mut().for_each(strip);
    }
    strip(&mut v["common"]);
    v
}

/// `kontext inbox --everyone --json`: `humans` (each with `human` and `owner`)
/// and `common`, each section like `kontext inbox --json`.
pub fn everyone_json(ev: &Everyone, all: bool) -> Value {
    let humans: Vec<Value> = ev
        .humans
        .iter()
        .map(|ib| {
            let mut v = section_json(ib, all);
            v["human"] = json!(ib.human);
            v["owner"] = json!(ev.is_owner(&ib.human));
            v
        })
        .collect();
    json!({
        "project": ev.project,
        "count": ev.count(),
        "humans": humans,
        "quiet": ev.quiet,
        "common": section_json(&ev.common, all),
    })
}

pub fn json(ib: &Inbox, all: bool) -> Value {
    let mut v = section_json(ib, all);
    v["project"] = json!(ib.project);
    v["human"] = json!(ib.human);
    if let Some(n) = ib.pm_items {
        v["pm_items"] = json!(n);
    }
    v
}

// ------------------------------------------------------------ acciones

/// An item from `3`, `3.2` (the second question of group 3), its key
/// (`review`, `close:R-4c1`) or the id of an entry it touches. Returns the
/// item and, if asked for, the group's entry.
pub fn find<'a>(ib: &'a Inbox, st: &State, s: &str) -> Result<(&'a Item, Option<&'a Sub>)> {
    let s = s.trim();
    let none = |what: &str| -> anyhow::Error {
        Refused(format!(
            "there is no pending item {what}: the inbox has {} (`kontext inbox --all` lists them)",
            ib.items.len()
        ))
        .into()
    };
    let (num, sub) = match s.split_once('.') {
        Some((a, b)) if a.parse::<usize>().is_ok() && b.parse::<usize>().is_ok() => {
            (a.parse::<usize>().ok(), b.parse::<usize>().ok())
        }
        _ => (s.parse::<usize>().ok(), None),
    };
    if let Some(n) = num {
        let it = ib
            .items
            .iter()
            .find(|i| i.n == n)
            .ok_or_else(|| none(&n.to_string()))?;
        return match sub {
            None => Ok((it, None)),
            Some(m) => match it.subs.get(m.wrapping_sub(1)) {
                Some(x) => Ok((it, Some(x))),
                None => Err(none(s)),
            },
        };
    }
    if let Some(it) = ib.items.iter().find(|i| i.key == s) {
        return Ok((it, None));
    }
    let id = st.resolve(s).map_err(|_| none(s))?;
    for it in &ib.items {
        if let Some(x) = it.subs.iter().find(|x| x.id == id) {
            let single = it.subs.len() == 1;
            return Ok((it, if single { None } else { Some(x) }));
        }
        if it.ids.contains(&id) || it.keys.contains(&id) {
            return Ok((it, None));
        }
    }
    Err(none(s))
}

/// Who may close an item of `ib`: the pm closes its own inbox's items, a
/// human closes theirs.
pub fn require_closer(ib: &Inbox, actor: &Actor) -> Result<()> {
    if ib.human == PM {
        if !is_pm(actor) && !actor.is_human() {
            return Err(Refused(format!(
                "only the pm (or a human) closes the pm's pending items ({} is not)",
                actor.short()
            ))
            .into());
        }
        return Ok(());
    }
    require_human(actor)
}

/// Only the human closes their pending items.
pub fn require_human(actor: &Actor) -> Result<()> {
    if !actor.is_human() {
        return Err(Refused(format!(
            "only the human closes their pending items ({} is not). An agent doesn't dismiss \
             or confirm for them: show them the items (skill kontext-show) and keep going with \
             what doesn't depend on them",
            actor.short()
        ))
        .into());
    }
    Ok(())
}

/// `kontext inbox dismiss` (`kind = "dismiss"`) and `kontext inbox confirm`
/// (`kind = "confirm"`): an entry from the human that closes `keys`.
pub fn close(
    store: &mut Store,
    actor: &Actor,
    ib: &Inbox,
    kind: &str,
    item: &Item,
    sub: Option<&Sub>,
    why: &str,
) -> Result<Vec<Entry>> {
    require_closer(ib, actor)?;
    let (keys, ids, title) = match sub {
        Some(s) => (vec![s.id.clone()], vec![s.id.clone()], s.text.clone()),
        None => (item.keys.clone(), item.ids.clone(), item.title.clone()),
    };
    let verb = if kind == "dismiss" {
        "Dismisses"
    } else {
        "Confirms"
    };
    let mut r = RawEntry::new(kind, &format!("{verb}: {}", one_line(&title, 140)));
    r.actor = Some(actor.clone());
    r.why = why.trim().to_string();
    r.refs = ids;
    r.fields.insert("items".into(), json!(keys));
    let ((), written) = store.transact(|b| {
        b.push(r)?;
        Ok(())
    })?;
    Ok(written)
}

// ------------------------------------------------------------ duplicates

/// Words that don't say what a question is about. Spanish and English: the
/// questions are written in the language of the conversation.
const STOPWORDS: &[&str] = &[
    "a", "al", "algo", "como", "con", "cual", "cuales", "cuando", "de", "del", "donde", "e", "el",
    "ella", "en", "entre", "era", "es", "esa", "ese", "eso", "esta", "este", "esto", "estos", "ha",
    "hay", "la", "las", "le", "les", "lo", "los", "mas", "me", "mi", "muy", "no", "nos", "o",
    "para", "pero", "por", "que", "qué", "se", "si", "sin", "sobre", "su", "sus", "te", "tu", "un",
    "una", "unas", "uno", "unos", "y", "ya", "va", "vamos", "debe", "debemos", "deberia", "hace",
    "hacer", "hacemos", "puede", "podemos", "the", "of", "to", "and", "or", "is", "we", "should",
    "do", "an", "are", "at", "be", "by", "can", "for", "from", "how", "in", "it", "on", "our",
    "that", "this", "what", "which", "with", "would",
];

/// The words that matter in a text: without capitals, accents, punctuation or
/// stopwords, and without the final `s` of the plural.
pub fn words(s: &str) -> BTreeSet<String> {
    let folded: String = s
        .nfkd()
        .filter(|c| !unicode_normalization::char::is_combining_mark(*c))
        .collect::<String>()
        .to_lowercase();
    folded
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() > 1 && !STOPWORDS.contains(w))
        .map(|w| {
            if w.chars().count() > 3 && w.ends_with('s') {
                w[..w.len() - 1].to_string()
            } else {
                w.to_string()
            }
        })
        .collect()
}

/// Do two questions say the same thing? Deterministic: they share at least two
/// words that matter and most of them (Jaccard ≥ 0.6), or the shorter one, if
/// it has at least three words, almost entirely fits in the other (≥ 0.8).
pub fn similar(a: &str, b: &str) -> bool {
    let (x, y) = (words(a), words(b));
    if x.is_empty() || y.is_empty() {
        return false;
    }
    let inter = x.intersection(&y).count();
    if inter < 2 {
        return false;
    }
    let union = x.union(&y).count();
    let min = x.len().min(y.len());
    (inter as f64 / union as f64) >= 0.6 || (min >= 3 && inter as f64 / min as f64 >= 0.8)
}

/// The open question that already asks the same thing: the same recipient, the
/// same epic (or neither), similar text, and not dismissed.
pub fn duplicate_of<'a>(
    st: &'a State,
    to: &str,
    epic: Option<&str>,
    title: &str,
) -> Option<&'a Entry> {
    st.open_questions().into_iter().find(|q| {
        question_to(q) == to.strip_prefix("human:").unwrap_or(to)
            && field(q, "epic") == epic
            && !closed(st, &q.id)
            && similar(&q.title, title)
    })
}

/// Does the question reach the human? (`owner` or the name of an owner, with
/// or without `human:`).
pub fn to_human(st: &State, to: &str) -> bool {
    let to = to.strip_prefix("human:").unwrap_or(to);
    to == "owner" || st.policy.is_owner(to)
}

/// The human who receives a question for `to`.
pub fn human_for(st: &State, to: &str) -> Option<String> {
    let to = to.strip_prefix("human:").unwrap_or(to);
    if st.policy.is_owner(to) {
        return Some(to.to_string());
    }
    default_human(st)
}

/// The text the start hook puts in the session that talks to the human: one
/// line, or nothing if they have no pending items.
pub fn hook_line(ib: &Inbox) -> Option<String> {
    if ib.items.is_empty() {
        return None;
    }
    let n = ib.items.len();
    let mut s = format!(
        "{} has {} in kontext",
        ib.human,
        if n == 1 {
            "1 pending item".to_string()
        } else {
            format!("{n} pending items")
        }
    );
    let mut parts = Vec::new();
    if let Some(o) = ib.oldest() {
        parts.push(format!(
            "oldest, from {}",
            tasks::ago(ib.now_ms, tasks::hlc_ms(&o.since))
        ));
    }
    if let Some(first) = ib.items.first() {
        let impact = first.impact_text();
        if !impact.is_empty() {
            parts.push(format!("the first {impact}"));
        }
    }
    if !parts.is_empty() {
        s.push_str(&format!(" ({})", parts.join("; ")));
    }
    s.push('.');
    if let Some(a) = ib.last_action_ago()
        && ib.now_ms - tasks::hlc_ms(ib.last_action.as_deref().unwrap_or_default()) > 24 * 3_600_000
    {
        s.push_str(&format!(" Their last action was {a}."));
    }
    s.push_str(
        " Tell them once, when you start. Don't run `kontext inbox` on your own: it is their \
         inbox, not your task list; show it to them only if they ask or if something blocks \
         work (skill kontext-show). Don't pile new questions on them while they haven't \
         answered.",
    );
    Some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn similar_questions() {
        assert!(similar("¿Cobramos en MXN o USD?", "¿cobramos en usd?"));
        assert!(similar(
            "¿El cobro es mensual o anual?",
            "El cobro, ¿es mensual?"
        ));
        assert!(similar(
            "¿Los reembolsos entran en esta épica?",
            "¿Entran los reembolsos en la épica?"
        ));
        assert!(!similar("¿Cobramos en pesos?", "¿Cobramos en dólares?"));
        assert!(!similar(
            "¿Los reembolsos entran en esta épica?",
            "¿Las cancelaciones entran en esta épica?"
        ));
        assert!(!similar("¿Y?", "¿Y?"));
    }

    #[test]
    fn impact_texts() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(impact_text(&[], 0, &[]), "");
        assert_eq!(impact_text(&s(&["Payments"]), 0, &[]), "blocks Payments");
        assert_eq!(
            impact_text(&s(&["Payments", "Notices", "Catalog"]), 2, &[]),
            "blocks Payments, Notices and 1 more (and 2 tasks)"
        );
        assert_eq!(
            impact_text(&[], 1, &s(&["Payments"])),
            "blocks 1 task of Payments"
        );
    }
}
