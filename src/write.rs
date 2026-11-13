//! Commands that write (DESIGN §10, phase 2): charter, goals, signing,
//! decisions, proposals, questions and the journal.
//!
//! Each one checks its references under the writer's lock (inside
//! [`Store::transact`]), so the check sees the state at that moment. What only
//! an owner can do is rejected before writing, with a [`Refused`] that explains
//! what to do instead. The state still applies the rule when reading, because
//! an entry can arrive from another machine.

use anyhow::{Result, bail};
use serde_json::{Map, Value, json};

use crate::cursor::validate_reader;
use crate::model::{Actor, Entry, RawEntry};
use crate::state::{State, Status};
use crate::store::{Batch, Refused, Store};

fn refused<T>(msg: impl Into<String>) -> Result<T> {
    Err(Refused(msg.into()).into())
}

/// What `decide` and `propose` share.
#[derive(Debug, Clone, Default)]
pub struct DecisionArgs {
    pub title: String,
    pub why: String,
    pub body: String,
    pub tags: Vec<String>,
    pub supersedes: Vec<String>,
    pub epic: Option<String>,
    pub guard_cmd: Option<String>,
    /// Attached images or pages, already saved in `media/` (the phase 0 mock-up
    /// as the reference).
    pub attachments: Vec<Value>,
}

fn raw(kind: &str, actor: &Actor, title: &str) -> RawEntry {
    let mut r = RawEntry::new(kind, title);
    r.actor = Some(actor.clone());
    r
}

fn set(fields: &mut Map<String, Value>, k: &str, v: Option<&str>) {
    if let Some(v) = v.map(str::trim).filter(|v| !v.is_empty()) {
        fields.insert(k.into(), json!(v));
    }
}

/// Checks the owner rule before writing.
fn require_owner(
    store: &Store,
    actor: &Actor,
    approved_by: Option<&str>,
    what: &str,
    instead: &str,
) -> Result<()> {
    if let Err(reason) = store.policy().check(actor, approved_by) {
        return refused(format!(
            "only an owner can {what} ({reason}).\n{instead}\n\
             A human owner writes with --actor human:<name> (or KONTEXT_ACTOR); the pm can do it \
             with KONTEXT_ROLE=pm and --approved-by <owner> once an owner has approved it."
        ));
    }
    Ok(())
}

const PROPOSE_INSTEAD: &str =
    "Agents propose, they do not decide. Use: kontext propose \"<title>\" --why \"…\" --body \"…\"";

/// Runs `f` under the lock and writes what it returns. Returns what was written.
fn write(store: &mut Store, f: impl FnOnce(&mut Batch) -> Result<()>) -> Result<Vec<Entry>> {
    let ((), written) = store.transact(f)?;
    Ok(written)
}

fn resolve_kind(st: &State, id: &str, kinds: &[&str], what: &str) -> Result<String> {
    let full = st.resolve(id)?;
    let kind = st.kind_of(&full).unwrap_or_default();
    if !kinds.contains(&kind) {
        return refused(format!("{full} is {kind}, not {what}"));
    }
    Ok(full)
}

/// What a decision or proposal can supersede: something that is law today.
fn resolve_law(st: &State, ids: &[String]) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for s in ids {
        let id = st.resolve(s)?;
        if !st.is_law(&id) {
            let why = st.status_of(&id).map(|x| x.label()).unwrap_or("unknown");
            return refused(format!(
                "--supersedes {id}: not a decision in force ({} {why})",
                st.kind_of(&id).unwrap_or("?")
            ));
        }
        if !out.contains(&id) {
            out.push(id);
        }
    }
    Ok(out)
}

/// `kontext charter`: a new, unsigned charter. If there are already signed
/// charters in force, the new one says it supersedes them (it counts when signed).
pub fn charter(
    store: &mut Store,
    actor: &Actor,
    objective: &str,
    non_goals: &[String],
    done_when: Option<&str>,
    title: Option<&str>,
) -> Result<Vec<Entry>> {
    if objective.trim().is_empty() {
        bail!("the charter needs --objective");
    }
    let title = match title.map(str::trim).filter(|t| !t.is_empty()) {
        Some(t) => t.to_string(),
        None => objective.trim().to_string(),
    };
    write(store, |b| {
        let olds: Vec<String> = b
            .state()?
            .signed_charters()
            .iter()
            .map(|c| c.id.clone())
            .collect();
        let mut r = raw("charter", actor, &title);
        r.fields.insert("objective".into(), json!(objective.trim()));
        r.fields.insert("non_goals".into(), json!(non_goals));
        r.fields
            .insert("done_when".into(), json!(done_when.unwrap_or("").trim()));
        if !olds.is_empty() {
            r.fields.insert("supersedes".into(), json!(olds));
            r.refs = olds;
        }
        b.push(r)?;
        Ok(())
    })
}

/// `kontext goal add`: a goal of the charter in force (or of the latest unsigned one).
pub fn goal_add(
    store: &mut Store,
    actor: &Actor,
    title: &str,
    done_when: Option<&str>,
) -> Result<Vec<Entry>> {
    write(store, |b| {
        let st = b.state()?;
        let Some(charter) = st.charter().or_else(|| st.latest_charter()) else {
            return refused(
                "there is no charter: create one with `kontext charter --objective …` before adding goals",
            );
        };
        let charter = charter.id.clone();
        let mut r = raw("goal", actor, title);
        r.fields.insert("charter".into(), json!(charter));
        r.fields
            .insert("done_when".into(), json!(done_when.unwrap_or("").trim()));
        r.refs = vec![charter];
        b.push(r)?;
        Ok(())
    })
}

/// `kontext sign`: signs a charter or a goal.
pub fn sign(
    store: &mut Store,
    actor: &Actor,
    id: &str,
    approved_by: Option<&str>,
) -> Result<Vec<Entry>> {
    require_owner(
        store,
        actor,
        approved_by,
        "sign a charter or a goal",
        "Ask an owner to sign it: kontext ask \"Sign <id>\" --to owner",
    )?;
    write(store, |b| {
        let st = b.state()?;
        let id = resolve_kind(st, id, &["charter", "goal"], "a charter or a goal")?;
        match st.status_of(&id) {
            Some(Status::Unsigned) => {}
            Some(Status::Signed) => return refused(format!("{id} is already signed")),
            Some(s) => return refused(format!("{id} cannot be signed: it is {}", s.label())),
            None => bail!("{id} has no status"),
        }
        let mut r = raw("sign", actor, &format!("Signature of {id}"));
        r.refs = vec![id];
        set(&mut r.fields, "approved_by", approved_by);
        b.push(r)?;
        Ok(())
    })
}

fn decision_raw(kind: &str, actor: &Actor, a: &DecisionArgs, sup: Vec<String>) -> RawEntry {
    let mut r = raw(kind, actor, a.title.trim());
    r.why = a.why.clone();
    r.body = a.body.clone();
    r.tags = a.tags.clone();
    r.fields.insert("supersedes".into(), json!(sup));
    set(&mut r.fields, "epic", a.epic.as_deref());
    if let Some(cmd) = a.guard_cmd.as_deref().filter(|c| !c.trim().is_empty()) {
        r.fields
            .insert("guard".into(), json!({"cmd": cmd, "expect": "exit0"}));
    }
    if !a.attachments.is_empty() {
        r.fields
            .insert("attachments".into(), Value::Array(a.attachments.clone()));
    }
    r.refs = sup;
    r
}

/// `kontext decide`: a decision. Only an owner (or the pm with `--approved-by`).
pub fn decide(
    store: &mut Store,
    actor: &Actor,
    a: &DecisionArgs,
    approved_by: Option<&str>,
) -> Result<Vec<Entry>> {
    if a.title.trim().is_empty() {
        bail!("the decision needs a title");
    }
    require_owner(
        store,
        actor,
        approved_by,
        "record a decision",
        PROPOSE_INSTEAD,
    )?;
    write(store, |b| {
        let sup = resolve_law(b.state()?, &a.supersedes)?;
        let mut r = decision_raw("decision", actor, a, sup);
        set(&mut r.fields, "approved_by", approved_by);
        b.push(r)?;
        Ok(())
    })
}

/// `kontext propose`: what anyone can write instead of deciding.
pub fn propose(store: &mut Store, actor: &Actor, a: &DecisionArgs) -> Result<Vec<Entry>> {
    if a.title.trim().is_empty() {
        bail!("the proposal needs a title");
    }
    write(store, |b| {
        let sup = resolve_law(b.state()?, &a.supersedes)?;
        b.push(decision_raw("proposal", actor, a, sup))?;
        Ok(())
    })
}

fn pending_proposal(st: &State, id: &str) -> Result<String> {
    let id = resolve_kind(st, id, &["proposal"], "a proposal")?;
    match st.status_of(&id) {
        Some(Status::Pending) => Ok(id),
        Some(s) => refused(format!("{id} is already {}", s.label())),
        None => bail!("{id} has no status"),
    }
}

/// `kontext accept`: the proposal becomes law. With `as_decision`, a decision
/// with its content is also written (and the `accept` points to it in
/// `fields.decision_id`), as in v0.
pub fn accept(
    store: &mut Store,
    actor: &Actor,
    id: &str,
    as_decision: bool,
    approved_by: Option<&str>,
    why: &str,
) -> Result<Vec<Entry>> {
    require_owner(
        store,
        actor,
        approved_by,
        "accept a proposal",
        "If you think it should be accepted, ask an owner: kontext ask \"Should we accept <id>?\" --to owner",
    )?;
    write(store, |b| {
        let st = b.state()?;
        let pid = pending_proposal(st, id)?;
        let p = st.get(&pid).unwrap().clone();
        let sup: Vec<String> = crate::state::string_list(p.fields.get("supersedes"))
            .into_iter()
            .filter(|x| st.is_law(x))
            .collect();
        let mut decision_id = None;
        if as_decision {
            let mut d = raw("decision", actor, &p.title);
            d.why = p.why.clone();
            d.body = p.body.clone();
            d.tags = p.tags.clone();
            d.refs = std::iter::once(pid.clone()).chain(sup.clone()).collect();
            d.fields.insert("supersedes".into(), json!(sup));
            d.fields.insert("from_proposal".into(), json!(pid));
            for k in ["epic", "guard"] {
                if let Some(v) = p.fields.get(k) {
                    d.fields.insert(k.into(), v.clone());
                }
            }
            set(&mut d.fields, "approved_by", approved_by);
            decision_id = Some(b.push(d)?.entry.id);
        }
        let mut r = raw("accept", actor, &format!("Accepts {pid}"));
        r.why = why.to_string();
        r.refs = vec![pid];
        set(&mut r.fields, "approved_by", approved_by);
        set(&mut r.fields, "decision_id", decision_id.as_deref());
        b.push(r)?;
        Ok(())
    })
}

/// `kontext reject`: the proposal is dismissed, with its reason.
pub fn reject(
    store: &mut Store,
    actor: &Actor,
    id: &str,
    why: &str,
    approved_by: Option<&str>,
) -> Result<Vec<Entry>> {
    if why.trim().is_empty() {
        bail!("rejecting needs --why");
    }
    require_owner(
        store,
        actor,
        approved_by,
        "reject a proposal",
        "If you think it should be rejected, say so in a question: kontext ask \"Should we reject <id>?\" --to owner",
    )?;
    write(store, |b| {
        let pid = pending_proposal(b.state()?, id)?;
        let mut r = raw("reject", actor, &format!("Rejects {pid}"));
        r.why = why.trim().to_string();
        r.refs = vec![pid];
        set(&mut r.fields, "approved_by", approved_by);
        b.push(r)?;
        Ok(())
    })
}

/// Which kinds each kind can be replaced with.
fn replacement_kinds(kind: &str) -> Option<&'static [&'static str]> {
    match kind {
        "charter" => Some(&["charter"]),
        "goal" => Some(&["goal"]),
        "decision" | "proposal" => Some(&["decision", "proposal"]),
        "fact" => Some(&["fact"]),
        "feature" => Some(&["feature"]),
        _ => None,
    }
}

/// `kontext supersede`: `old` is superseded by `new`. For charters, goals and
/// decisions, only an owner.
pub fn supersede(
    store: &mut Store,
    actor: &Actor,
    old: &str,
    new: &str,
    why: &str,
    approved_by: Option<&str>,
) -> Result<Vec<Entry>> {
    // The owner rule depends on the kind, which is known under the lock.
    let policy = store.policy().clone();
    write(store, |b| {
        let st = b.state()?;
        let old = st.resolve(old)?;
        let new = st.resolve(new)?;
        let ok_kinds = replacement_kinds(st.kind_of(&old).unwrap_or_default());
        let (Some(ok_kinds), Some(new_kind)) = (ok_kinds, st.kind_of(&new)) else {
            return refused(format!(
                "{old} is {}: only charters, goals, decisions and facts can be superseded",
                st.kind_of(&old).unwrap_or("?")
            ));
        };
        if !ok_kinds.contains(&new_kind) {
            return refused(format!(
                "{old} ({}) cannot be superseded by {new} ({new_kind})",
                st.kind_of(&old).unwrap_or("?")
            ));
        }
        if old == new {
            return refused("an entry cannot supersede itself");
        }
        match st.status_of(&old) {
            Some(Status::Superseded) => {
                return refused(format!(
                    "{old} is already superseded by {}",
                    st.superseded_by
                        .get(&old)
                        .map(String::as_str)
                        .unwrap_or("?")
                ));
            }
            Some(s @ (Status::Rejected | Status::Ignored | Status::Pending)) => {
                return refused(format!("{old} is not in force: it is {}", s.label()));
            }
            _ => {}
        }
        if st.kind_of(&old) == Some("feature") {
            if let Err(reason) = crate::features::may_write(st, actor) {
                return refused(reason);
            }
            crate::features::in_force(st, &new)?;
        } else if st.kind_of(&old) != Some("fact")
            && let Err(reason) = policy.check(actor, approved_by)
        {
            return refused(format!(
                "only an owner can supersede charters, goals or decisions ({reason}).\n{PROPOSE_INSTEAD} --supersedes {old}"
            ));
        }
        let mut r = raw("supersede", actor, &format!("Supersedes {old} with {new}"));
        r.why = why.trim().to_string();
        r.refs = vec![old, new];
        set(&mut r.fields, "approved_by", approved_by);
        b.push(r)?;
        Ok(())
    })
}

/// What `kontext ask` asks for.
#[derive(Debug, Clone, Default)]
pub struct AskArgs {
    pub title: String,
    pub to: Option<String>,
    pub task: Option<String>,
    /// `--feature`: the question is about a feature (`fields.feature`).
    pub feature: Option<String>,
    pub body: String,
    /// The epic of the question (`--epic`). Without it, the task's epic or, if
    /// a coordinator asks (`agent:epic:<x>`), its own.
    pub epic: Option<String>,
    /// `--blocking`: the answer stops the work of whoever asks.
    pub blocking: bool,
    /// `--no-merge`: write the question even if a similar one is already open.
    pub no_merge: bool,
    /// When the human has this many pending items, an agent's question is not
    /// urgent unless it says `--blocking` (`[inbox] max_pending`).
    pub max_pending: usize,
}

/// What `kontext ask` did.
#[derive(Debug, Clone)]
pub enum Asked {
    /// A new question. `brake` says how many pending items the human had if the
    /// question was left as not urgent because of that.
    New {
        entries: Vec<Entry>,
        brake: Option<usize>,
    },
    /// An open question already says the same: a `confirm` was written that
    /// adds to it (with the task of whoever asks).
    Joined {
        question: Box<Entry>,
        record: Box<Entry>,
    },
}

/// `kontext ask`: a question for a reader (`owner`, `pm`, `epic:<x>` or a
/// person). With `task`, the question belongs to that task (`fields.task`),
/// and if no recipient is given it goes to its epic.
///
/// If an open question already exists for the same reader, the same epic and
/// similar text ([`crate::inbox::similar`]), it is not repeated: a `confirm`
/// that adds to it is written instead. If an agent's question reaches the
/// human and he already has `max_pending` pending items or more, it is left as
/// not urgent (`fields.blocking = false`) unless it says `--blocking`.
pub fn ask(store: &mut Store, actor: &Actor, a: &AskArgs) -> Result<Asked> {
    let title = a.title.trim();
    if title.is_empty() {
        bail!("the question needs a title");
    }
    if let Some(t) = &a.to {
        validate_reader(t.trim())?;
    }
    let ((joined, brake), written) = store.transact(|b| {
        let st = b.state()?;
        let mut fields = crate::tasks::question_fields(
            st,
            a.to.as_deref(),
            a.task.as_deref(),
            a.feature.as_deref(),
        )?;
        if !fields.contains_key("epic") {
            let explicit = a.epic.as_deref().map(str::trim).filter(|e| !e.is_empty());
            let epic = match explicit {
                Some(e) => Some(st.find_epic(e)?),
                None => actor
                    .name
                    .strip_prefix("epic:")
                    .filter(|_| actor.is_agent())
                    .and_then(|n| st.find_epic(n).ok()),
            };
            if let Some(id) = epic {
                let name = crate::tasks::epic_name(st.get(&id).unwrap());
                fields.insert("epic".into(), json!(name));
            }
        }
        let to = fields
            .get("to")
            .and_then(Value::as_str)
            .unwrap_or("owner")
            .to_string();
        let epic = fields
            .get("epic")
            .and_then(Value::as_str)
            .map(str::to_string);
        if !a.no_merge
            && let Some(q) = crate::inbox::duplicate_of(st, &to, epic.as_deref(), title)
        {
            let question = q.clone();
            let mut r = raw("confirm", actor, title);
            r.body = a.body.clone();
            r.refs = vec![question.id.clone()];
            r.fields.insert("reason".into(), json!("same question"));
            if let Some(t) = fields.get("task") {
                r.fields.insert("task".into(), t.clone());
            }
            if let Some(u) = fields.get("feature") {
                r.fields.insert("feature".into(), u.clone());
            }
            if a.blocking {
                r.fields.insert("blocking".into(), json!(true));
            }
            b.push(r)?;
            return Ok((Some(question), None));
        }
        let mut brake = None;
        if a.blocking {
            fields.insert("blocking".into(), json!(true));
        } else if actor.is_agent()
            && crate::inbox::to_human(st, &to)
            && let Some(human) = crate::inbox::human_for(st, &to)
        {
            let pending = crate::inbox::build(st, "", &human).items.len();
            if pending >= a.max_pending {
                fields.insert("blocking".into(), json!(false));
                brake = Some(pending);
            }
        }
        let mut r = raw("question", actor, title);
        r.body = a.body.to_string();
        r.fields = fields;
        if let Some(t) = r.fields.get("task").and_then(Value::as_str) {
            r.refs = vec![t.to_string()];
        } else if let Some(u) = r.fields.get("feature").and_then(Value::as_str) {
            r.refs = vec![u.to_string()];
        }
        b.push(r)?;
        Ok((None, brake))
    })?;
    match joined {
        Some(question) => Ok(Asked::Joined {
            question: Box::new(question),
            record: Box::new(written.into_iter().next().expect("the confirm was written")),
        }),
        None => Ok(Asked::New {
            entries: written,
            brake,
        }),
    }
}

/// `kontext answer`: answers a question.
pub fn answer(store: &mut Store, actor: &Actor, id: &str, text: &str) -> Result<Vec<Entry>> {
    if text.trim().is_empty() {
        bail!("the answer is empty");
    }
    write(store, |b| {
        let qid = resolve_kind(b.state()?, id, &["question"], "a question")?;
        let mut r = raw("answer", actor, &format!("Answer to {qid}"));
        r.body = text.trim().to_string();
        r.refs = vec![qid];
        b.push(r)?;
        Ok(())
    })
}

/// The tag of the status paragraph in the report.
pub const STATUS_TAG: &str = "status";

/// `kontext log`: a journal entry. With `status`, it is the "where we are"
/// paragraph that `kontext report` takes (tag `status`): the last one counts.
pub fn log(
    store: &mut Store,
    actor: &Actor,
    text: &str,
    next: Option<&str>,
    task: Option<&str>,
    tags: &[String],
    status: bool,
) -> Result<Vec<Entry>> {
    if text.trim().is_empty() {
        bail!("the journal entry is empty");
    }
    write(store, |b| {
        // If the task exists, its full id is stored (this renews the claim).
        let task = task.map(str::trim).filter(|t| !t.is_empty()).map(|t| {
            let st = b.state();
            st.ok()
                .and_then(|st| {
                    st.resolve(t)
                        .ok()
                        .filter(|id| st.kind_of(id) == Some("task"))
                })
                .unwrap_or_else(|| t.to_string())
        });
        let mut r = raw("journal", actor, text.trim());
        r.tags = tags.to_vec();
        if status && !r.tags.iter().any(|t| t == STATUS_TAG) {
            r.tags.push(STATUS_TAG.to_string());
        }
        set(&mut r.fields, "next", next);
        set(&mut r.fields, "task", task.as_deref());
        b.push(r)?;
        Ok(())
    })
}
