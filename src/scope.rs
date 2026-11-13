//! The signed scope (DESIGN §4.3 and §5): what the human signs, twice.
//!
//! A `scope` entry (prefix `S`) stores the summary the human read
//! (`fields.summary`, with its sha256 in `fields.summary_hash`) and what that
//! signature covers (`fields.covers`: the charter, goals, epics and features
//! in force, plus the decisions in force at that moment). A `change` also
//! stores its diff against the previous signed scope (`adds`, `removes`).
//!
//! Everything else is derived, never written:
//! - the **signed scope** is the `covers` of the last `scope` entry that
//!   counts (an owner, or the pm with `approved_by`, and a summary whose hash
//!   matches);
//! - the **current scope** is the charter, goals, epics and features in force
//!   now (decisions are not scope: they never make a scope change);
//! - once a plan is signed, any difference between the two is the **pending
//!   scope change**.
//!
//! A project with no `scope` entry behaves exactly as before: nothing here is
//! looked at until one counts.

use std::collections::BTreeSet;

use anyhow::Result;
use serde::Serialize;
use serde_json::{Value, json};

use crate::ideas::{Ideas, NotPlaced};
use crate::model::{Actor, Entry, RawEntry, sha256_hex};
use crate::plan;
use crate::state::{State, Status, string_list};
use crate::store::{Refused, Store};
use crate::tasks::{self, TaskStatus};
use crate::views::{field, one_line};

/// The kinds that make up the scope (decisions are covered, but they are not
/// scope: superseding one is never a scope change).
pub const SCOPE_KINDS: &[&str] = &["charter", "goal", "epic", "feature"];

/// The shortest `--hash` prefix accepted.
pub const MIN_HASH_PREFIX: usize = 8;

fn refused<T>(msg: impl Into<String>) -> Result<T> {
    Err(Refused(msg.into()).into())
}

/// sha256 of the summary text, as stored in `fields.summary_hash`.
pub fn hash_of(summary: &str) -> String {
    sha256_hex(summary.as_bytes())
}

/// Why a `scope` entry's content does not hold (its hash does not match its
/// summary, or its type is unknown), or `None` if it does.
pub fn content_problem(e: &Entry) -> Option<String> {
    let summary = e
        .fields
        .get("summary")
        .and_then(Value::as_str)
        .unwrap_or("");
    let hash = e
        .fields
        .get("summary_hash")
        .and_then(Value::as_str)
        .unwrap_or("");
    if hash != hash_of(summary) {
        return Some("its summary_hash does not match its summary (the text was changed)".into());
    }
    match field(e, "type") {
        Some("plan" | "change") => None,
        other => Some(format!("unknown scope type {:?}", other.unwrap_or(""))),
    }
}

/// Is there a signed plan? The switch for everything this module adds.
pub fn plan_signed(st: &State) -> bool {
    !st.scopes.is_empty()
}

/// The last `scope` entry that counts.
pub fn last(st: &State) -> Option<&Entry> {
    st.scopes.last().and_then(|id| st.get(id))
}

/// What the last signature covers (every id in `covers`).
pub fn signed_covers(st: &State) -> BTreeSet<String> {
    last(st)
        .map(|s| string_list(s.fields.get("covers")).into_iter().collect())
        .unwrap_or_default()
}

/// The ids of the scope kinds in a set of covers.
fn scope_part(st: &State, covers: &BTreeSet<String>) -> BTreeSet<String> {
    covers
        .iter()
        .filter(|id| st.kind_of(id).is_some_and(|k| SCOPE_KINDS.contains(&k)))
        .cloned()
        .collect()
}

/// The features in force (not superseded, dropped or not counted).
pub fn features_in_force(st: &State) -> Vec<&Entry> {
    st.of_kind("feature")
        .filter(|f| st.feature(&f.id).is_some_and(|i| i.status.in_force()))
        .collect()
}

/// The current scope, in this order: the charter (the last one not
/// superseded, signed or not), the goals, the epics and the features in force.
pub fn current(st: &State) -> Vec<String> {
    let mut out: Vec<String> = st
        .latest_charter()
        .map(|c| c.id.clone())
        .into_iter()
        .collect();
    out.extend(st.goals().iter().map(|g| g.id.clone()));
    out.extend(st.epics().iter().map(|e| e.id.clone()));
    out.extend(features_in_force(st).iter().map(|f| f.id.clone()));
    out
}

/// What a signature would cover now: the current scope and the decisions in
/// force.
pub fn covers_now(st: &State) -> Vec<String> {
    let mut out = current(st);
    out.extend(st.law().iter().map(|d| d.id.clone()));
    out
}

/// The difference between the current scope and the signed one.
#[derive(Serialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct Diff {
    pub adds: Vec<String>,
    pub removes: Vec<String>,
}

impl Diff {
    pub fn is_empty(&self) -> bool {
        self.adds.is_empty() && self.removes.is_empty()
    }
}

/// The diff of the current scope against `signed` (in ledger order).
fn diff_against(st: &State, signed: &BTreeSet<String>) -> Diff {
    let now = current(st);
    let now_set: BTreeSet<&String> = now.iter().collect();
    let signed = scope_part(st, signed);
    let mut removes: Vec<String> = signed
        .iter()
        .filter(|id| !now_set.contains(id))
        .cloned()
        .collect();
    removes.sort_by_key(|id| position(st, id));
    Diff {
        adds: now.into_iter().filter(|id| !signed.contains(id)).collect(),
        removes,
    }
}

fn position(st: &State, id: &str) -> usize {
    st.order.iter().position(|x| x == id).unwrap_or(usize::MAX)
}

/// The pending scope change: with a signed plan, what entered or left the
/// scope since the last signature. `None` without a signed plan or when
/// nothing changed.
pub fn pending(st: &State) -> Option<Diff> {
    if !plan_signed(st) {
        return None;
    }
    let d = diff_against(st, &signed_covers(st));
    (!d.is_empty()).then_some(d)
}

/// Is this feature leaving the signed scope, waiting for the owner to sign
/// it out? It is in the last signature's `covers` and no longer in force. A
/// feature dropped that no signature covered never entered the scope: it
/// waits for nothing.
pub fn leaving(st: &State, id: &str) -> bool {
    plan_signed(st) && signed_covers(st).contains(id) && !current(st).iter().any(|x| x == id)
}

/// "U-… Refunds" for the views.
pub fn label(st: &State, id: &str) -> String {
    match st.get(id) {
        Some(e) => format!("{} {}", e.id, one_line(&e.title, 60)),
        None => id.to_string(),
    }
}

/// "enters U-… Refunds, E-… Catalog; leaves U-… Invoices" in one line.
pub fn diff_line(st: &State, d: &Diff) -> String {
    let mut parts = Vec::new();
    let list = |ids: &[String]| -> String {
        let shown: Vec<String> = ids.iter().take(3).map(|id| label(st, id)).collect();
        let more = if ids.len() > 3 {
            format!(" and {} more", ids.len() - 3)
        } else {
            String::new()
        };
        format!("{}{more}", shown.join(", "))
    };
    if !d.adds.is_empty() {
        parts.push(format!("enters {}", list(&d.adds)));
    }
    if !d.removes.is_empty() {
        parts.push(format!("leaves {}", list(&d.removes)));
    }
    parts.join("; ")
}

// ------------------------------------------------------------ summary

/// What `kontext scope summary` would have the human sign now.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SummaryKind {
    /// No signed scope yet.
    Plan,
    /// A plan is signed and the scope changed since.
    Change,
    /// Nothing to sign.
    Nothing,
}

#[derive(Serialize, Clone, Debug)]
pub struct Summary {
    #[serde(rename = "type")]
    pub kind: SummaryKind,
    pub project: String,
    /// The text the human reads and signs (without the hash line).
    pub summary: String,
    /// sha256 of `summary` (empty when there is nothing to sign).
    pub hash: String,
    /// What the signature would cover.
    pub covers: Vec<String>,
    pub adds: Vec<String>,
    pub removes: Vec<String>,
    /// The user's ideas not placed (a plan with any is refused).
    pub not_placed: Vec<NotPlaced>,
    /// The last signed scope, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signed: Option<String>,
}

impl Summary {
    /// The text with the line that carries its hash (what the command prints).
    pub fn text(&self) -> String {
        if self.kind == SummaryKind::Nothing {
            return format!("{}\n", self.summary);
        }
        format!(
            "{}\n\nHash: {} (to sign: `kontext scope sign --hash {}`)\n",
            self.summary,
            self.hash,
            &self.hash[..12]
        )
    }

    pub fn json(&self) -> Value {
        let mut v = serde_json::to_value(self).unwrap_or(Value::Null);
        v["text"] = json!(self.text());
        v
    }
}

/// "Nov 12" from an `hlc`.
fn day(hlc: &str) -> String {
    crate::ideas::month_day(hlc.get(..10).unwrap_or(hlc))
}

/// A text the signature binds, on one line and whole: never cut.
fn whole(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A body the signature binds, line by line under `indent` (nothing if empty).
fn body_lines(body: &str, indent: &str, l: &mut Vec<String>) {
    if body.trim().is_empty() {
        return;
    }
    l.extend(
        body.trim()
            .lines()
            .map(|x| format!("{indent}{}", x.trim_end()).trim_end().to_string()),
    );
}

fn acceptance_line(f: &Entry) -> String {
    match crate::features::acceptance(f) {
        Some(a) => format!("when it works, the human sees: {}", whole(a)),
        None => "no acceptance written".into(),
    }
}

/// The epics in dependency order counted as if every one were open, so that
/// closing work does not move it: what a change compares, before and after.
/// It is not `kontext plan` (that one moves closed epics out of the waves).
fn epic_order(st: &State, keep: &dyn Fn(&str) -> bool) -> plan::Plan {
    let nodes: Vec<plan::Node> = plan::nodes(st)
        .into_iter()
        .map(|mut n| {
            n.active = keep(&n.id);
            n.tasks = 1;
            n.closed_tasks = 0;
            n.features = 0;
            n.done_features = 0;
            n.open_loose = 1;
            n
        })
        .collect();
    plan::compute(&nodes)
}

/// The epic order in one line: "1. Foundations; 2. Payments, Catalog".
fn order_line(p: &plan::Plan) -> String {
    let mut parts: Vec<String> = p
        .waves
        .iter()
        .map(|w| {
            let names: Vec<String> = w.epics.iter().map(|e| whole(&e.title)).collect();
            format!("{}. {}", w.n, names.join(", "))
        })
        .collect();
    if !p.blocked.is_empty() {
        let names: Vec<String> = p.blocked.iter().map(|b| whole(&b.epic.title)).collect();
        parts.push(format!("no place: {}", names.join(", ")));
    }
    if parts.is_empty() {
        "no epics".into()
    } else {
        parts.join("; ")
    }
}

fn feature_lines(st: &State, epic: &str, indent: &str, l: &mut Vec<String>) {
    let fs: Vec<&Entry> = features_in_force(st)
        .into_iter()
        .filter(|f| field(f, "epic") == Some(epic))
        .collect();
    if fs.is_empty() {
        l.push(format!("{indent}- (no features yet)"));
    }
    for f in fs {
        l.push(format!(
            "{indent}- {} {}: {}",
            f.id,
            whole(&f.title),
            acceptance_line(f)
        ));
    }
}

fn epic_line(e: &Entry) -> String {
    format!(
        "- {} {} (coordinator {})",
        e.id,
        whole(&e.title),
        tasks::coordinator_of(e)
    )
}

fn charter_lines(c: &Entry, l: &mut Vec<String>) {
    let objective = field(c, "objective").unwrap_or(&c.title);
    if whole(&c.title) != whole(objective) {
        l.push(format!("Charter: {}", whole(&c.title)));
    }
    l.push(format!("Objective: {}", whole(objective)));
    let non = string_list(c.fields.get("non_goals"));
    if non.is_empty() {
        l.push("Not doing: (nothing written)".into());
    } else {
        l.push("Not doing:".into());
        for n in non {
            l.push(format!("- {}", whole(&n)));
        }
    }
    if let Some(d) = field(c, "done_when") {
        l.push(format!("Done when: {}", whole(d)));
    }
    body_lines(&c.body, "> ", l);
}

fn goal_lines(g: &Entry, l: &mut Vec<String>) {
    l.push(match field(g, "done_when") {
        Some(d) => format!("- {} {}. Done when: {}", g.id, whole(&g.title), whole(d)),
        None => format!("- {} {}", g.id, whole(&g.title)),
    });
    body_lines(&g.body, "  > ", l);
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn not_placed_line(np: &NotPlaced) -> String {
    let words = np.quote.as_deref().unwrap_or(&np.idea);
    format!("- {} \"{}\"", np.id, one_line(words, 100))
}

/// What the summary knows of the user's ideas: what `kontext ideas` reads
/// from `.pm/items.md`, or that there was none to read from where it ran.
pub enum IdeasCheck {
    Read(Ideas),
    /// No `.pm/items.md` found from this folder (nor in the mapped repos).
    NotFound(String),
}

impl IdeasCheck {
    fn ideas(&self) -> Option<&Ideas> {
        match self {
            IdeasCheck::Read(i) => Some(i),
            IdeasCheck::NotFound(_) => None,
        }
    }

    fn not_placed(&self) -> Vec<NotPlaced> {
        self.ideas()
            .map(|i| i.not_placed.clone())
            .unwrap_or_default()
    }
}

/// The line (and list) about the user's ideas that every summary carries:
/// it is part of the signed text, so a summary shown where `.pm/` was read
/// and a signature run where it was not never match.
fn ideas_lines(ic: &IdeasCheck, plan: bool, l: &mut Vec<String>) {
    match ic {
        IdeasCheck::NotFound(dir) => l.push(format!(
            "Ideas not checked: no .pm/items.md found from {dir}."
        )),
        IdeasCheck::Read(i) if i.not_placed.is_empty() => {
            l.push("Not placed: 0 of your ideas.".into())
        }
        IdeasCheck::Read(i) => {
            l.push(format!(
                "Not placed: {} of your ideas.{}",
                i.not_placed.len(),
                if plan {
                    " Place or discard them before signing:"
                } else {
                    ""
                }
            ));
            l.extend(i.not_placed.iter().map(not_placed_line));
        }
    }
}

fn plan_text(st: &State, project: &str, ideas: &IdeasCheck) -> String {
    let mut l = vec![format!("# Plan for {project}"), String::new()];
    if let Some(c) = st.latest_charter() {
        charter_lines(c, &mut l);
    }
    let goals = st.goals();
    l.push(String::new());
    l.push(format!("## Goals ({})", goals.len()));
    if goals.is_empty() {
        l.push("(no goals yet)".into());
    }
    for g in &goals {
        goal_lines(g, &mut l);
    }
    // The same waves as `kontext plan` and `kontext tree`.
    let plan = plan::of_state(st);
    let epics = st.epics();
    let features = features_in_force(st).len();
    if epics.is_empty() {
        l.push(String::new());
        l.push("## Epics".into());
        l.push("(no epics yet: each one added later is a scope change)".into());
    }
    if !plan.closed.is_empty() {
        l.push(String::new());
        l.push(format!("## Done ({})", plan.closed.len()));
        for pe in &plan.closed {
            if let Some(e) = st.get(&pe.id) {
                l.push(epic_line(e));
                feature_lines(st, &e.id, "  ", &mut l);
            }
        }
    }
    for w in &plan.waves {
        l.push(String::new());
        if w.parallel {
            l.push(format!(
                "## Wave {} (in parallel: {} epics)",
                w.n,
                w.epics.len()
            ));
        } else {
            l.push(format!("## Wave {} (in series)", w.n));
        }
        for pe in &w.epics {
            if let Some(e) = st.get(&pe.id) {
                l.push(epic_line(e));
                feature_lines(st, &e.id, "  ", &mut l);
            }
        }
    }
    if !plan.blocked.is_empty() {
        l.push(String::new());
        l.push(format!("## No wave ({})", plan.blocked.len()));
        for b in &plan.blocked {
            if let Some(e) = st.get(&b.epic.id) {
                l.push(format!("{}: {}", epic_line(e), b.reason));
                feature_lines(st, &e.id, "  ", &mut l);
            }
        }
    }
    l.push(String::new());
    l.push("## In numbers".into());
    l.push(format!(
        "{}, {}, {}; one coordinator per epic ({}).",
        plural(epics.len(), "epic", "epics"),
        plural(features, "feature", "features"),
        plural(plan.waves.len(), "wave", "waves"),
        epics.len()
    ));
    let law = st.law().len();
    if law > 0 {
        l.push(format!(
            "It also covers the {} in force today (`kontext resume --as pm` lists them).",
            plural(law, "decision", "decisions")
        ));
    }
    ideas_lines(ideas, true, &mut l);
    l.join("\n")
}

/// The epics a change touches: the ones that enter or leave, those of the
/// features that enter or leave, and those that cite a goal that does.
fn touched_epics(st: &State, d: &Diff) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut add = |id: String| {
        if !out.contains(&id) {
            out.push(id);
        }
    };
    for id in d.adds.iter().chain(&d.removes) {
        let Some(e) = st.get(id) else { continue };
        match e.kind.as_str() {
            "epic" => add(e.id.clone()),
            "feature" => {
                if let Some(ep) = field(e, "epic") {
                    add(ep.to_string());
                }
            }
            "goal" => {
                for ep in st.of_kind("epic") {
                    if string_list(ep.fields.get("goals")).contains(&e.id) {
                        add(ep.id.clone());
                    }
                }
            }
            _ => {}
        }
    }
    out.sort_by_key(|id| position(st, id));
    out
}

fn change_text(st: &State, project: &str, d: &Diff, ideas: &IdeasCheck) -> String {
    let signed = last(st);
    let mut l = vec![format!("# Scope change for {project}")];
    if let Some(s) = signed {
        l.push(format!("Since {} (signed {}).", s.id, day(&s.hlc)));
    }
    let adds: BTreeSet<&String> = d.adds.iter().collect();
    l.push(String::new());
    l.push(format!("## Enters ({})", d.adds.len()));
    if d.adds.is_empty() {
        l.push("(nothing)".into());
    }
    for id in &d.adds {
        let Some(e) = st.get(id) else { continue };
        match e.kind.as_str() {
            "charter" => {
                l.push(format!("- Charter {}:", e.id));
                let mut c = Vec::new();
                charter_lines(e, &mut c);
                l.extend(c.into_iter().map(|x| format!("  {x}")));
            }
            "goal" => {
                let mut g = Vec::new();
                goal_lines(e, &mut g);
                g[0] = format!("- Goal {}", &g[0][2..]);
                l.extend(g);
            }
            "epic" => {
                l.push(format!("- Epic {}", &epic_line(e)[2..]));
                feature_lines(st, &e.id, "  ", &mut l);
            }
            "feature" => {
                // A feature of an epic that enters is listed under it.
                if field(e, "epic").is_some_and(|ep| adds.contains(&ep.to_string())) {
                    continue;
                }
                let epic = field(e, "epic")
                    .and_then(|ep| st.get(ep))
                    .map(|ep| whole(&ep.title))
                    .unwrap_or_else(|| "?".into());
                l.push(format!(
                    "- Feature {} {}, in epic {epic}: {}",
                    e.id,
                    whole(&e.title),
                    acceptance_line(e)
                ));
            }
            _ => {}
        }
    }
    l.push(String::new());
    l.push(format!("## Leaves ({})", d.removes.len()));
    if d.removes.is_empty() {
        l.push("(nothing)".into());
    }
    for id in &d.removes {
        let Some(e) = st.get(id) else { continue };
        let why = match st.superseded_by.get(id) {
            Some(by) => format!(" (replaced by {by})"),
            None if st.dropped.contains_key(id) => " (dropped)".into(),
            None => String::new(),
        };
        let kind = match e.kind.as_str() {
            "charter" => "Charter",
            "goal" => "Goal",
            "epic" => "Epic",
            _ => "Feature",
        };
        l.push(format!("- {kind} {} {}{why}", e.id, whole(&e.title)));
        if e.kind == "feature" {
            let closed: Vec<String> = crate::features::task_ids(st, id)
                .into_iter()
                .filter(|t| st.task_status(t) == Some(TaskStatus::Closed))
                .collect();
            if closed.is_empty() {
                l.push("  No closed work is thrown away.".into());
            } else {
                l.push(format!(
                    "  Work thrown away: {} ({})",
                    plural(closed.len(), "closed task", "closed tasks"),
                    closed.join(", ")
                ));
            }
            if let Some(i) = ideas.ideas() {
                let theirs: Vec<String> = i
                    .ideas
                    .iter()
                    .filter(|x| x.refs.iter().any(|r| &r.id == id))
                    .map(|x| {
                        format!(
                            "{} \"{}\"",
                            x.id,
                            one_line(x.quote.as_deref().unwrap_or(&x.idea), 80)
                        )
                    })
                    .collect();
                if !theirs.is_empty() {
                    l.push(format!(
                        "  Your ideas that pointed to it, which count as discarded once you sign: {}",
                        theirs.join("; ")
                    ));
                }
            }
        }
    }
    let touched = touched_epics(st, d);
    if !touched.is_empty() {
        l.push(String::new());
        l.push("## Epics it touches".into());
        let names: Vec<String> = touched
            .iter()
            .filter_map(|id| st.get(id))
            .map(|e| whole(&e.title))
            .collect();
        l.push(names.join(", "));
    }
    l.push(String::new());
    // Not `kontext plan`'s waves (those move as work closes): the order the
    // epics' dependencies give, counting every one as open.
    l.push("## Epic order (by their dependencies, every epic counted as open)".into());
    let covers = signed_covers(st);
    let before = epic_order(st, &|id| covers.contains(id));
    let now = epic_order(st, &|id| st.status_of(id) == Some(Status::Active));
    let (b, n) = (order_line(&before), order_line(&now));
    if b == n {
        l.push(format!("Unchanged: {n}."));
    } else {
        l.push(format!("Now: {n}."));
        l.push(format!("Before: {b}."));
    }
    l.push(String::new());
    l.push("## Your ideas".into());
    ideas_lines(ideas, false, &mut l);
    l.join("\n")
}

/// What would be signed now. `ideas`: what `kontext ideas` reads from
/// `.pm/items.md`, or where none was found (the summary says which).
pub fn summary(st: &State, project: &str, ideas: &IdeasCheck) -> Summary {
    let not_placed = ideas.not_placed();
    let signed = last(st).map(|s| s.id.clone());
    let nothing = |text: String| Summary {
        kind: SummaryKind::Nothing,
        project: project.to_string(),
        summary: text,
        hash: String::new(),
        covers: Vec::new(),
        adds: Vec::new(),
        removes: Vec::new(),
        not_placed: not_placed.clone(),
        signed: signed.clone(),
    };
    if !plan_signed(st) {
        if st.latest_charter().is_none() {
            return nothing(
                "Nothing to sign: there is no charter yet. Write it with `kontext charter --objective …`, \
                 then the goals, epics and features, and show the plan again."
                    .into(),
            );
        }
        let text = plan_text(st, project, ideas);
        return Summary {
            kind: SummaryKind::Plan,
            project: project.to_string(),
            hash: hash_of(&text),
            summary: text,
            covers: covers_now(st),
            adds: Vec::new(),
            removes: Vec::new(),
            not_placed,
            signed: None,
        };
    }
    let Some(d) = pending(st) else {
        let s = last(st).expect("a signed plan");
        return nothing(format!(
            "Nothing to sign: the scope signed in {} ({}) is the current one.",
            s.id,
            day(&s.hlc)
        ));
    };
    let text = change_text(st, project, &d, ideas);
    Summary {
        kind: SummaryKind::Change,
        project: project.to_string(),
        hash: hash_of(&text),
        summary: text,
        covers: covers_now(st),
        adds: d.adds,
        removes: d.removes,
        not_placed,
        signed,
    }
}

// ------------------------------------------------------------ sign

/// What `kontext scope sign` asks for.
#[derive(Debug, Clone, Default)]
pub struct SignArgs {
    pub hash: String,
    pub said: Option<String>,
    pub approved_by: Option<String>,
}

/// Where the ideas come from, read before taking the lock: `.pm/` and its
/// `items.md` (or the folder where none was found), so the summary under the
/// lock is the one `kontext scope summary` showed from the same place.
pub enum IdeasSource {
    Found {
        pm: std::path::PathBuf,
        items_md: String,
        people: Vec<String>,
    },
    /// The folder it ran from.
    NotFound(String),
}

impl IdeasSource {
    pub fn check(&self, st: &State, project: &str) -> IdeasCheck {
        match self {
            IdeasSource::Found {
                pm,
                items_md,
                people,
            } => IdeasCheck::Read(crate::ideas::build(st, project, people, pm, items_md)),
            IdeasSource::NotFound(dir) => IdeasCheck::NotFound(dir.clone()),
        }
    }
}

/// `kontext scope sign --hash <h> [--said …]`: an owner, or the pm with
/// `--approved-by <owner>`. The summary is computed again under the lock;
/// if its hash is not `<h>`, nothing is written.
pub fn sign(
    store: &mut Store,
    actor: &Actor,
    project: &str,
    ideas: &IdeasSource,
    a: &SignArgs,
) -> Result<Vec<Entry>> {
    let h = a.hash.trim().to_lowercase();
    if h.len() < MIN_HASH_PREFIX || !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return refused(format!(
            "--hash takes the hash that `kontext scope summary` printed (at least {MIN_HASH_PREFIX} hex characters)"
        ));
    }
    if let Err(reason) = store.policy().check(actor, a.approved_by.as_deref()) {
        return refused(format!(
            "only an owner signs the scope ({reason}). The pm shows the summary to the owner and, \
             on their yes, signs for them with --approved-by <owner> --said \"<their words>\""
        ));
    }
    let ((), written) = store.transact(|b| {
        let st = b.state()?;
        let ideas = ideas.check(st, project);
        let s = summary(st, project, &ideas);
        if s.kind == SummaryKind::Nothing {
            return refused(s.summary.clone());
        }
        if !s.hash.starts_with(&h) {
            return refused(
                "the scope changed since that summary; show it again (`kontext scope summary`)",
            );
        }
        if s.kind == SummaryKind::Plan && !s.not_placed.is_empty() {
            let list: Vec<String> = s.not_placed.iter().map(not_placed_line).collect();
            return refused(format!(
                "{} of the user's ideas are not placed: nothing they said may get lost silently. \
                 Give each one a feature, task or decision in `.pm/items.md`, or ask them whether to \
                 discard it, then show the summary again:\n{}",
                s.not_placed.len(),
                list.join("\n")
            ));
        }
        let title = match s.kind {
            SummaryKind::Plan => "Plan signed".to_string(),
            _ => format!(
                "Scope change signed (+{} −{})",
                s.adds.len(),
                s.removes.len()
            ),
        };
        let mut r = RawEntry::new("scope", &title);
        r.actor = Some(actor.clone());
        r.refs = s.adds.iter().chain(&s.removes).cloned().collect();
        let kind = if s.kind == SummaryKind::Plan {
            "plan"
        } else {
            "change"
        };
        r.fields.insert("type".into(), json!(kind));
        r.fields.insert("covers".into(), json!(s.covers));
        r.fields.insert("adds".into(), json!(s.adds));
        r.fields.insert("removes".into(), json!(s.removes));
        r.fields.insert("summary".into(), json!(s.summary));
        r.fields.insert("summary_hash".into(), json!(s.hash));
        if let Some(said) = a.said.as_deref().filter(|x| !x.trim().is_empty()) {
            r.fields.insert("said".into(), json!(said));
        }
        if let Some(o) = a.approved_by.as_deref().map(str::trim).filter(|x| !x.is_empty()) {
            r.fields.insert("approved_by".into(), json!(o));
        }
        b.push(r)?;
        Ok(())
    })?;
    Ok(written)
}

/// The lines `kontext show <S>` adds: what it covers and the summary as read.
pub fn show_lines(st: &State, e: &Entry) -> Vec<String> {
    let mut l = Vec::new();
    let kind = field(e, "type").unwrap_or("?");
    let covers = string_list(e.fields.get("covers"));
    let n = |k: &str| covers.iter().filter(|id| st.kind_of(id) == Some(k)).count();
    l.push(format!(
        "- **Type:** {}",
        if kind == "plan" {
            "plan"
        } else {
            "scope change"
        }
    ));
    l.push(format!(
        "- **Covers:** {} charter, {}, {}, {}, {}",
        n("charter"),
        plural(n("goal"), "goal", "goals"),
        plural(n("epic"), "epic", "epics"),
        plural(n("feature"), "feature", "features"),
        plural(
            covers
                .iter()
                .filter(|id| matches!(st.kind_of(id), Some("decision" | "proposal")))
                .count(),
            "decision",
            "decisions"
        )
    ));
    for (k, what) in [("adds", "Enters"), ("removes", "Leaves")] {
        let ids = string_list(e.fields.get(k));
        if !ids.is_empty() {
            let labels: Vec<String> = ids.iter().map(|id| label(st, id)).collect();
            l.push(format!("- **{what}:** {}", labels.join("; ")));
        }
    }
    if let Some(s) = field(e, "said") {
        l.push(format!("- **The owner said:** {s}"));
    }
    l.push(format!(
        "- **Summary hash:** {}",
        field(e, "summary_hash").unwrap_or("?")
    ));
    l.push(String::new());
    l.push("**The summary that was signed**".into());
    l.push(String::new());
    l.push(field(e, "summary").unwrap_or("").to_string());
    l
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_sha256_of_the_text() {
        assert_eq!(
            hash_of("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
