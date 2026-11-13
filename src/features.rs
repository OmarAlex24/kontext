//! Features: the level between an epic and its tasks (DESIGN §4.3 and §5).
//!
//! A feature is what the human would see working ("Refunds"), with its
//! `acceptance`. It belongs to one epic (`fields.epic`, the epic's id), serves
//! goals like a task (`fields.goals`; without `--goal`, the epic's), and its
//! tasks point to it with `fields.feature`.
//!
//! Its state is derived, never written:
//! - **pending**: no tasks, or none assigned, claimed, closed or reopened,
//!   and never validated;
//! - **in progress**: some task assigned, claimed, closed or reopened, not
//!   all closed, or a validation lost (a task reopened or added after it);
//! - **to validate**: at least one task and all closed, no `done` that counts
//!   on the feature;
//! - **done**: a `done` on the feature that counts, and none of its tasks
//!   reopened or added after it (otherwise it is in progress or to validate
//!   again, with a note: no record undoes the validation);
//! - **superseded** (replaced by another feature) or **not counted** (written
//!   by someone who is not the pm or an owner).
//!
//! Who counts: only the pm or an owner adds or supersedes a feature; only the
//! epic's coordinator, the pm or an owner validates one (`kontext done <U>`),
//! and only once every one of its tasks is closed at that point of the ledger.
//! Anyone can reopen a validation, with its reason. An epic with no features
//! behaves as before: nothing here touches it.

use anyhow::{Result, bail};
use serde::Serialize;
use serde_json::{Value, json};

use crate::model::{Actor, Entry, RawEntry};
use crate::state::{State, Status, TaskEvent, string_list};
use crate::store::{Refused, Store};
use crate::tasks::{self, DoneArgs, Stale, TaskStatus, Written};

/// The state of a feature.
#[derive(Serialize, Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum FeatureStatus {
    #[default]
    #[serde(rename = "pending")]
    Pending,
    #[serde(rename = "in progress")]
    InProgress,
    #[serde(rename = "to validate")]
    ToValidate,
    #[serde(rename = "done")]
    Done,
    #[serde(rename = "superseded")]
    Superseded,
    /// Taken out of the scope with no replacement (`kontext feature drop`).
    #[serde(rename = "dropped")]
    Dropped,
    #[serde(rename = "not counted")]
    NotCounted,
}

impl FeatureStatus {
    pub fn label(self) -> &'static str {
        match self {
            FeatureStatus::Pending => "pending",
            FeatureStatus::InProgress => "in progress",
            FeatureStatus::ToValidate => "to validate",
            FeatureStatus::Done => "done",
            FeatureStatus::Superseded => "superseded",
            FeatureStatus::Dropped => "dropped",
            FeatureStatus::NotCounted => "not counted",
        }
    }

    /// Still part of its epic (not superseded, and counted).
    pub fn in_force(self) -> bool {
        !matches!(
            self,
            FeatureStatus::Superseded | FeatureStatus::Dropped | FeatureStatus::NotCounted
        )
    }
}

impl std::fmt::Display for FeatureStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// What the walk knows about a feature.
#[derive(Serialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct FeatureInfo {
    pub status: FeatureStatus,
    /// Its tasks, in ledger order.
    pub tasks: Vec<String>,
    pub closed_tasks: usize,
    /// The `done` that validated it, while it holds.
    pub validated: Option<String>,
    /// Why an earlier validation no longer holds (a task reopened or added
    /// after it). Cleared by the next validation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Its validations and reopenings, counted or not.
    pub history: Vec<TaskEvent>,
}

/// The feature a task hangs from (`fields.feature`), if any.
pub fn feature_of(task: &Entry) -> Option<&str> {
    task.fields
        .get("feature")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// The feature a task counts toward: its `fields.feature`, if that is a
/// feature of the task's own epic that counts (in force or superseded). A
/// task whose feature is in another epic (only from another machine: the CLI
/// refuses it), missing, or not counted has no feature: it is one of its
/// epic's tasks without a feature, as before features existed.
pub fn counted_feature<'a>(st: &State, task: &'a Entry) -> Option<&'a str> {
    let id = feature_of(task)?;
    let f = st.get(id)?;
    (f.kind == "feature"
        && f.fields.get("epic").and_then(Value::as_str)
            == task.fields.get("epic").and_then(Value::as_str)
        && st.status_of(id) != Some(Status::Ignored))
    .then_some(id)
}

/// A task whose `fields.feature` names a feature of another epic: that
/// feature, and the reason it does not count toward it.
pub fn foreign_feature(st: &State, task: &Entry) -> Option<(String, String)> {
    let id = feature_of(task)?;
    let f = st.get(id).filter(|f| f.kind == "feature")?;
    let theirs = f.fields.get("epic").and_then(Value::as_str).unwrap_or("?");
    let ours = task
        .fields
        .get("epic")
        .and_then(Value::as_str)
        .unwrap_or("?");
    (theirs != ours).then(|| {
        (
            id.to_string(),
            format!(
                "it is in the epic {ours} and {id} in {theirs}, so it does not count toward {id} \
                 (in {ours} it is a task without a feature)"
            ),
        )
    })
}

/// The features of an epic that are in force, in ledger order.
pub fn of_epic<'a>(st: &'a State, epic: &str) -> Vec<&'a Entry> {
    st.of_kind("feature")
        .filter(|f| f.fields.get("epic").and_then(Value::as_str) == Some(epic))
        .filter(|f| st.feature(&f.id).is_some_and(|i| i.status.in_force()))
        .collect()
}

/// Does the epic have at least one feature in force? If not, every view
/// shows it as before features existed.
pub fn has_features(st: &State, epic: &str) -> bool {
    !of_epic(st, epic).is_empty()
}

/// `(done, total, to validate)` among an epic's features in force.
pub fn counts(st: &State, epic: &str) -> (usize, usize, usize) {
    let fs = of_epic(st, epic);
    let n = |s: FeatureStatus| {
        fs.iter()
            .filter(|f| st.feature(&f.id).is_some_and(|i| i.status == s))
            .count()
    };
    (
        n(FeatureStatus::Done),
        fs.len(),
        n(FeatureStatus::ToValidate),
    )
}

/// "2/3 features done, 1 to validate".
pub fn counts_text(st: &State, epic: &str) -> String {
    let (done, total, to_validate) = counts(st, epic);
    let mut s = format!("{done}/{total} features done");
    if to_validate > 0 {
        s.push_str(&format!(", {to_validate} to validate"));
    }
    s
}

/// The tasks of a feature, in ledger order (the ones already walked, during
/// the walk).
pub fn task_ids(st: &State, feature: &str) -> Vec<String> {
    st.order
        .iter()
        .filter_map(|id| st.get(id))
        .filter(|t| t.kind == "task" && counted_feature(st, t) == Some(feature))
        .map(|t| t.id.clone())
        .collect()
}

/// Why a feature can't be validated now, or `None` if it can: it needs at
/// least one task, and every one of them closed. Used by the walk (with the
/// ledger seen so far) and by `kontext done` (with all of it), so the refusal
/// and the alert give the same reason.
pub fn unready(st: &State, feature: &str) -> Option<String> {
    let ts = task_ids(st, feature);
    if ts.is_empty() {
        return Some("it has no tasks yet".into());
    }
    let open: Vec<&String> = ts
        .iter()
        .filter(|t| st.task(t).is_none_or(|i| i.done.is_none()))
        .collect();
    if open.is_empty() {
        return None;
    }
    let list: Vec<&str> = open.iter().take(5).map(|s| s.as_str()).collect();
    let more = if open.len() > 5 {
        format!(" and {} more", open.len() - 5)
    } else {
        String::new()
    };
    Some(format!(
        "not all its tasks are closed ({} still open: {}{more})",
        open.len(),
        list.join(", ")
    ))
}

/// The final state of a feature, from its tasks (called at the end of the walk).
pub fn derive(st: &State, id: &str, info: &FeatureInfo) -> FeatureStatus {
    match st.status_of(id) {
        Some(Status::Superseded) if st.dropped.contains_key(id) => return FeatureStatus::Dropped,
        Some(Status::Superseded) => return FeatureStatus::Superseded,
        Some(Status::Ignored) => return FeatureStatus::NotCounted,
        _ => {}
    }
    let n = info.tasks.len();
    let closed = info.closed_tasks;
    if info.validated.is_some() && n > 0 && closed == n {
        return FeatureStatus::Done;
    }
    if n > 0 && closed == n {
        return FeatureStatus::ToValidate;
    }
    // A reopened task counts as started, and a feature that lost its
    // validation is never pending again.
    let started = info.note.is_some()
        || info.tasks.iter().any(|t| {
            matches!(
                st.task_status(t),
                Some(TaskStatus::Assigned | TaskStatus::Claimed | TaskStatus::Closed)
            ) || st
                .task(t)
                .is_some_and(|i| i.history.iter().any(|h| h.counted && h.kind == "reopen"))
        });
    if started {
        FeatureStatus::InProgress
    } else {
        FeatureStatus::Pending
    }
}

/// "to validate (3/3 tasks closed)", "done (R-…)", with the note if any.
pub fn status_text(st: &State, id: &str) -> Option<String> {
    let info = st.feature(id)?;
    let mut s = info.status.label().to_string();
    match info.status {
        FeatureStatus::Done => {
            if let Some(v) = &info.validated {
                s.push_str(&format!(" (validated in {v})"));
            }
        }
        FeatureStatus::Superseded => {
            if let Some(by) = st.superseded_by.get(id) {
                s.push_str(&format!(" by {by}"));
            }
        }
        FeatureStatus::Dropped => {
            if let Some(r) = st.dropped.get(id) {
                s.push_str(&format!(" ({r})"));
            }
            match st.drop_signed.get(id) {
                Some(sc) => s.push_str(&format!("; the owner signed it in {sc}")),
                None if crate::scope::leaving(st, id) => {
                    s.push_str("; waiting for the owner's signature (scope change)")
                }
                None => {}
            }
        }
        FeatureStatus::NotCounted => {}
        _ if info.tasks.is_empty() => s.push_str(" (no tasks yet)"),
        _ => s.push_str(&format!(
            " ({}/{} tasks closed)",
            info.closed_tasks,
            info.tasks.len()
        )),
    }
    if let Some(n) = &info.note {
        s.push_str(&format!("; {n}"));
    }
    Some(s)
}

/// The feature's acceptance: what the human would see when it works.
pub fn acceptance(f: &Entry) -> Option<&str> {
    f.fields
        .get("acceptance")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// A feature for JSON views: the entry, its state and its tasks.
pub fn json(st: &State, f: &Entry) -> Value {
    let mut v = crate::views::entry_json(st, f);
    if let Some(i) = st.feature(&f.id) {
        v["feature_status"] = json!(i.status);
        v["tasks"] = json!(i.tasks);
        v["closed_tasks"] = json!(i.closed_tasks);
        v["validated"] = json!(i.validated);
        if let Some(n) = &i.note {
            v["note"] = json!(n);
        }
    }
    v
}

// ---------------------------------------------------------------- commands

fn refused<T>(msg: impl Into<String>) -> Result<T> {
    Err(Refused(msg.into()).into())
}

/// What `kontext feature add` asks for.
#[derive(Debug, Clone, Default)]
pub struct FeatureArgs {
    pub title: String,
    pub epic: String,
    pub acceptance: Option<String>,
    pub goals: Vec<String>,
    pub body: String,
    pub tags: Vec<String>,
}

/// The rule for adding or superseding a feature: the pm or an owner.
pub fn may_write(st: &State, actor: &Actor) -> std::result::Result<(), String> {
    if st.policy.manages(actor, None) {
        Ok(())
    } else {
        Err(format!(
            "only the pm or an owner can add or supersede features ({} is neither)",
            actor.short()
        ))
    }
}

/// `kontext feature add`. Without `--goal`, the epic's goals.
pub fn add(store: &mut Store, actor: &Actor, a: &FeatureArgs) -> Result<Written> {
    if a.title.trim().is_empty() {
        bail!("the feature needs a title");
    }
    let mut warnings = Vec::new();
    let ((), entries) = store.transact(|b| {
        let st = b.state()?;
        if let Err(why) = may_write(st, actor) {
            return refused(format!(
                "{why}. A coordinator who needs one asks the pm: `kontext ask --to pm`"
            ));
        }
        let epic_id = st.find_epic(&a.epic)?;
        if st.status_of(&epic_id) != Some(Status::Active) {
            return refused(format!(
                "the epic {epic_id} is not in force ({})",
                st.status_of(&epic_id).map(|s| s.label()).unwrap_or("?")
            ));
        }
        let epic = st.get(&epic_id).unwrap();
        let goals = if a.goals.is_empty() {
            string_list(epic.fields.get("goals"))
        } else {
            tasks::resolve_goals(st, &a.goals)?
        };
        warnings = tasks::goal_warnings(st, &goals);
        let mut r = RawEntry::new("feature", a.title.trim());
        r.actor = Some(actor.clone());
        r.body = a.body.clone();
        r.tags = a.tags.clone();
        r.fields.insert("epic".into(), json!(epic_id));
        r.fields.insert("goals".into(), json!(goals));
        r.fields.insert(
            "acceptance".into(),
            json!(a.acceptance.as_deref().unwrap_or("").trim()),
        );
        r.refs = vec![epic_id];
        b.push(r)?;
        Ok(())
    })?;
    Ok(Written { entries, warnings })
}

/// The feature named by `s` (`U-…` or a prefix), if it is one.
pub fn resolve(st: &State, s: &str) -> Result<String> {
    let id = st.resolve(s)?;
    if st.kind_of(&id) != Some("feature") {
        return refused(format!(
            "{id} is {}, not a feature",
            st.kind_of(&id).unwrap_or("?")
        ));
    }
    Ok(id)
}

/// A feature tasks can still hang from: in force.
pub fn in_force(st: &State, s: &str) -> Result<String> {
    let id = resolve(st, s)?;
    match st.feature(&id).map(|i| i.status) {
        Some(FeatureStatus::Superseded) => refused(format!(
            "{id} was superseded by {}",
            st.superseded_by.get(&id).map(String::as_str).unwrap_or("?")
        )),
        Some(FeatureStatus::Dropped) => refused(format!(
            "{id} was dropped ({}); put it back with `kontext reopen {id} --why …`",
            st.dropped.get(&id).map(String::as_str).unwrap_or("?")
        )),
        Some(FeatureStatus::NotCounted) | None => refused(format!(
            "{id} does not count (it was not written by the pm or an owner)"
        )),
        _ => Ok(id),
    }
}

/// What `kontext done <U>` checks before writing (and before `--run`): the
/// feature is in force and not validated, whoever validates is the epic's
/// coordinator, the pm or an owner, every task is closed, and, with `--since`
/// (optional here), nothing touching it came in after the mark.
pub fn check_validate(st: &State, actor: &Actor, a: &DoneArgs) -> Result<String> {
    let id = in_force(st, &a.task)?;
    let info = st.feature(&id).cloned().unwrap_or_default();
    if let Some(v) = &info.validated {
        return refused(format!(
            "{id} is already validated ({v}); to validate it again, reopen it with `kontext reopen {id} --why …`"
        ));
    }
    let epic = st.get(&id).and_then(|f| st.epic_of(f));
    if !st.policy.manages(actor, epic) {
        return refused(format!(
            "only the coordinator of its epic, the pm or an owner can validate {id} ({} is none of them)",
            actor.short()
        ));
    }
    if let Some(why) = unready(st, &id) {
        return refused(format!("{id} can't be validated yet: {why}"));
    }
    if let Some(since) = &a.since {
        let stale = tasks::stale_entries(st, &id, since);
        if !stale.is_empty() {
            let now = tasks::frontier_of(st);
            return Err(Stale(tasks::stale_message(&id, &stale, &now)).into());
        }
    }
    Ok(id)
}

/// What `kontext reopen <U>` checks: it was validated, or it was dropped
/// (then the pm or an owner puts it back).
pub fn check_reopen(st: &State, actor: &Actor, id: &str) -> Result<()> {
    if st.dropped.contains_key(id) {
        if let Err(why) = may_write(st, actor) {
            return refused(why.replace("add or supersede", "put back"));
        }
        return Ok(());
    }
    match st.feature(id) {
        Some(i) if i.validated.is_some() => Ok(()),
        Some(i) => refused(format!("{id} is not validated ({})", i.status)),
        None => refused(format!("{id} is not a feature")),
    }
}

/// `kontext feature drop <U> --why`: takes a feature out of the current scope
/// with no replacement (the pm or an owner). It is a `supersede` with only
/// the feature in `refs`, which binaries from before scopes already read as
/// superseded. With a signed plan it is a scope change until the owner signs it.
pub fn drop(store: &mut Store, actor: &Actor, feature: &str, why: &str) -> Result<Vec<Entry>> {
    if why.trim().is_empty() {
        bail!("dropping a feature needs --why");
    }
    let ((), written) = store.transact(|b| {
        let st = b.state()?;
        if let Err(why) = may_write(st, actor) {
            return refused(format!(
                "{}. A coordinator who thinks a feature should go asks the pm: `kontext ask --to pm`",
                why.replace("add or supersede", "drop")
            ));
        }
        let id = in_force(st, feature)?;
        let mut r = RawEntry::new("supersede", &format!("Drops {id}"));
        r.actor = Some(actor.clone());
        r.why = why.trim().to_string();
        r.refs = vec![id];
        r.fields.insert("drop".into(), json!(true));
        b.push(r)?;
        Ok(())
    })?;
    Ok(written)
}
