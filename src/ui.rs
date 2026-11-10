//! `kontext ui` (phase 6, DESIGN §10): a local read-only page with the ledger,
//! the cursors of each reader, the alerts and the boards of the epics.
//!
//! A minimal HTTP/1.1 server over `std::net::TcpListener` (no runtime or
//! dependencies): one thread per connection, `Connection: close`. Nothing writes.
//!
//! - `GET /`: the page, with the CSS and the JS of `templates/ui/` inside (it
//!   asks nothing of the internet).
//! - `GET /api/state[?before=<hlc>&limit=<n>]`: everything the page draws,
//!   built from what the views already compute (`State`, `standup`,
//!   `epic_line`, `status`), and the ledger from the newest to the oldest, in
//!   pages of `limit` (100 by default).
//! - `GET /api/entry/<id>`: the same as `kontext show --json`, plus its line.
//!
//! It listens on `127.0.0.1` by default. With a loopback address it only
//! answers requests whose `Host` is loopback (against DNS rebinding: a page on
//! the internet cannot read the ledger through the browser).

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{Map, Value, json};

use crate::app;
use crate::board;
use crate::config::{self, Project};
use crate::cursor::{self, Frontier};
use crate::facts::{self, Freshness};
use crate::model::{Entry, is_record_kind};
use crate::state::{Alert, State, Status, string_list};
use crate::store::Store;
use crate::tasks::{self, TaskStatus, epic_name};
use crate::views::{self, field, kind_label, one_line};

pub const DEFAULT_PORT: u16 = 4700;
pub const DEFAULT_LIMIT: usize = 100;
pub const MAX_LIMIT: usize = 1000;

const INDEX_HTML: &str = include_str!("../templates/ui/index.html");
const STYLE_CSS: &str = include_str!("../templates/ui/style.css");
const APP_JS: &str = include_str!("../templates/ui/app.js");

/// The page with its CSS and JS inside.
pub fn page() -> String {
    INDEX_HTML
        .replace("/*KONTEXT_STYLE*/", STYLE_CSS)
        .replace("/*KONTEXT_SCRIPT*/", APP_JS)
}

// ------------------------------------------------------------ the state

/// What is needed to build `/api/state`.
pub struct Source {
    pub home: PathBuf,
    pub writer: String,
    /// Who looks at the page: "Signed by you" and the "For you" card.
    pub you: String,
    /// Where `kontext ui` was started: `/report` looks for `.pm/` from here
    /// on every request, as `kontext report` does.
    pub cwd: Option<PathBuf>,
}

impl Source {
    fn open(&self) -> Result<(Store, Project)> {
        let project = Project::load(&self.home)?;
        let store = Store::open(&self.home, &self.writer)?;
        Ok((store, project))
    }
}

/// A page of the ledger: the entries with an `hlc` lower than `before`.
#[derive(Debug, Clone, Default)]
pub struct Page {
    pub before: Option<String>,
    pub limit: usize,
}

/// Which family an entry falls in: the color of its square.
pub fn category(st: &State, e: &Entry) -> &'static str {
    category_of(st, e, 0)
}

fn category_of(st: &State, e: &Entry, depth: usize) -> &'static str {
    match e.kind.as_str() {
        "charter" | "goal" | "sign" => "charter",
        "decision" | "proposal" | "accept" | "reject" => "decision",
        "question" | "answer" => "question",
        "fact" | "confirm" => "fact",
        "epic" | "task" | "assign" | "claim" | "release" | "done" | "reopen" => "task",
        "journal" => "journal",
        "supersede" => match e.refs.first().and_then(|t| st.get(t)) {
            Some(t) if depth < 4 => category_of(st, t, depth + 1),
            _ => "decision",
        },
        _ => "other",
    }
}

fn category_label(c: &str) -> &'static str {
    match c {
        "charter" => "Charter",
        "decision" => "Decision",
        "question" => "Question",
        "fact" => "Fact",
        "task" => "Task",
        "journal" => "Journal",
        _ => "Other",
    }
}

fn title_of(st: &State, id: &str) -> String {
    st.get(id)
        .map(|t| one_line(&t.title, 90))
        .unwrap_or_default()
}

fn with_title(st: &State, id: &str) -> String {
    let t = title_of(st, id);
    if t.is_empty() {
        id.to_string()
    } else {
        format!("{id} · {t}")
    }
}

/// The title of the line: the entry's, or what a record did.
fn row_title(st: &State, e: &Entry) -> String {
    if !is_record_kind(&e.kind) {
        let t = one_line(&e.title, 160);
        return match e.kind.as_str() {
            "goal" => format!("Meta: {t}"),
            "epic" => format!("Epic: {t}"),
            _ => t,
        };
    }
    let target = e.refs.first().map(String::as_str).unwrap_or("?");
    match e.kind.as_str() {
        "sign" => format!("Signature of {}", with_title(st, target)),
        "accept" => format!("Accepts {}", with_title(st, target)),
        "reject" => format!("Rejects {}", with_title(st, target)),
        "answer" => format!("Answer to {target}: {}", one_line(&e.body, 140)),
        "supersede" => format!(
            "Supersedes {} with {}",
            with_title(st, target),
            e.refs.get(1).map(String::as_str).unwrap_or("?")
        ),
        "assign" => format!(
            "Assigns {} to {}",
            with_title(st, target),
            field(e, "to").unwrap_or("?")
        ),
        "claim" => format!("Claims {}", with_title(st, target)),
        "release" => format!("Releases {}", with_title(st, target)),
        "done" => format!("Closes {}", with_title(st, target)),
        "reopen" => format!("Reopens {}", with_title(st, target)),
        "confirm" if st.kind_of(target) == Some("question") => {
            format!("Asks the same as {}", with_title(st, target))
        }
        "confirm" => format!("Confirms {}", with_title(st, target)),
        "dismiss" => format!("Dismisses from the inbox {}", with_title(st, target)),
        k => format!("{} of {}", kind_label(k), with_title(st, target)),
    }
}

/// The note of the line (in gray) and the warnings (in the warning color).
fn row_note(st: &State, e: &Entry, alerts: &[&Alert]) -> (String, String) {
    let mut warns: Vec<String> = Vec::new();
    let mut parts: Vec<String> = Vec::new();
    if let Some(r) = st.not_counting.get(&e.id) {
        return (String::new(), format!("Does not count: {r}"));
    }
    let status = st.status_of(&e.id);
    match e.kind.as_str() {
        "charter" => parts.push(match status {
            Some(Status::Signed) if st.active_charter.as_deref() == Some(&e.id) => {
                "Charter in force".into()
            }
            Some(Status::Signed) => "Signed".into(),
            Some(Status::Unsigned) => {
                warns.push("Unsigned: waiting for an owner's signature".into());
                String::new()
            }
            Some(Status::Superseded) => format!(
                "Replaced by {}",
                st.superseded_by
                    .get(&e.id)
                    .map(String::as_str)
                    .unwrap_or("?")
            ),
            _ => String::new(),
        }),
        "goal" => parts.push(match status {
            Some(Status::Signed) => "Signed".into(),
            Some(Status::Unsigned) => {
                warns.push("Unsigned".into());
                String::new()
            }
            Some(Status::Superseded) => format!(
                "Replaced by {}",
                st.superseded_by
                    .get(&e.id)
                    .map(String::as_str)
                    .unwrap_or("?")
            ),
            _ => String::new(),
        }),
        "decision" => {
            if let Some(by) = st.superseded_by.get(&e.id) {
                parts.push(format!("Replaced by {by}"));
            } else if !e.why.trim().is_empty() {
                parts.push(format!("Why: {}", one_line(&e.why, 120)));
            }
            if let Some(cmd) = e
                .fields
                .get("guard")
                .and_then(|g| g.get("cmd"))
                .and_then(Value::as_str)
            {
                parts.push(format!("Guard: {cmd}"));
            }
        }
        "proposal" => parts.push(match status {
            Some(Status::Pending) if views::unreviewed(e) => {
                "Unreviewed proposal · written by the distill".into()
            }
            Some(Status::Pending) => "Unreviewed proposal".into(),
            Some(Status::Accepted) => "Accepted".into(),
            Some(Status::Rejected) => "Rejected".into(),
            Some(Status::Superseded) => "Replaced".into(),
            _ => String::new(),
        }),
        "question" => {
            let to = field(e, "to").unwrap_or("owner");
            parts.push(match status {
                Some(Status::Answered) => format!("For {to} · answered"),
                _ => format!("For {to} · unanswered"),
            });
        }
        "answer" => {}
        "fact" | "confirm" => {
            let fps = facts::fingerprints_of(e);
            match st.freshness.get(&e.id) {
                Some(Freshness::Stale {
                    path,
                    head,
                    missing,
                    ..
                }) => {
                    warns.push(format!(
                        "Expired: {path} {} in {}",
                        if *missing {
                            "no longer exists"
                        } else {
                            "changed"
                        },
                        crate::git::short(head)
                    ));
                }
                Some(Freshness::Unchecked { repos }) => parts.push(format!(
                    "Unreviewed fingerprint: {} is not on this machine",
                    repos.join(", ")
                )),
                _ if !fps.is_empty() => {
                    let list: Vec<String> = fps
                        .iter()
                        .map(|f| format!("{} in {}", f.path, crate::git::short(&f.commit)))
                        .collect();
                    parts.push(format!("Fingerprint: {}", list.join(", ")));
                }
                _ => parts.push("No fingerprint".into()),
            }
            if status == Some(Status::Superseded) {
                parts.push(format!(
                    "Replaced by {}",
                    st.superseded_by
                        .get(&e.id)
                        .map(String::as_str)
                        .unwrap_or("?")
                ));
            }
        }
        "epic" => {
            let goals = string_list(e.fields.get("goals"));
            parts.push(format!(
                "Serves {} · coordinates {}",
                goals.join(", "),
                tasks::coordinator_of(e)
            ));
        }
        "task" => {
            let goals = string_list(e.fields.get("goals"));
            let epic = st
                .epic_of(e)
                .map(|x| format!("epic {}", epic_name(x)))
                .unwrap_or_default();
            if !goals.is_empty() {
                parts.push(format!("Serves {}", goals.join(", ")));
            }
            if !epic.is_empty() {
                parts.push(epic);
            }
            if let Some(s) = board::task_status_text(st, &e.id) {
                parts.push(s);
            }
        }
        "journal" => {
            if let Some(t) = field(e, "task") {
                parts.push(format!("Journal of {t}"));
            }
            if let Some(n) = field(e, "next") {
                parts.push(format!("Next: {}", one_line(n, 80)));
            }
        }
        "done" => parts.push(format!(
            "Closed with evidence: {}",
            tasks::evidence_summary(e.fields.get("evidence"))
        )),
        "reject" | "reopen" | "release" | "supersede" if !e.why.trim().is_empty() => {
            parts.push(format!("Why: {}", one_line(&e.why, 120)))
        }
        _ => {}
    }
    // The alerts that touch it, with their own phrase.
    for a in alerts {
        let text = match a {
            Alert::Drift { .. } | Alert::UnsignedGoal { .. } => drift_text(a),
            Alert::FactStale { .. } => continue,
            Alert::ProposalSupersedes { decision, .. } => {
                format!("Proposes to replace {decision}, which is in force")
            }
            other => capitalize(&other.to_string()),
        };
        warns.push(text);
    }
    parts.retain(|p| !p.is_empty());
    (parts.join(" · "), warns.join(" · "))
}

/// How a drift is said next to the task or epic.
fn drift_text(a: &Alert) -> String {
    match a {
        Alert::Drift { reason, .. } if reason == crate::state::NO_GOAL_REASON => {
            "Serves no goal: possible drift".to_string()
        }
        Alert::Drift { reason, .. } => format!("{}: possible drift", capitalize(reason)),
        Alert::UnsignedGoal { goal, .. } => format!("Cites {goal}, which nobody has signed"),
        other => capitalize(&other.to_string()),
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// Is it struck through? What was replaced, rejected, does not count, and the
/// facts that expired.
fn struck(st: &State, e: &Entry) -> bool {
    matches!(
        st.status_of(&e.id),
        Some(Status::Superseded | Status::Rejected | Status::Ignored)
    ) || matches!(st.freshness.get(&e.id), Some(Freshness::Stale { .. }))
}

/// Who signed a charter: the owner of the `sign` (or the pm's `approved_by`).
fn signer_of(st: &State, id: &str) -> Option<String> {
    let rec = st.get(st.signed_by.get(id)?)?;
    Some(
        field(rec, "approved_by")
            .map(str::to_string)
            .unwrap_or_else(|| rec.actor.name.clone()),
    )
}

/// The line of an entry in the ledger.
pub fn row(st: &State, e: &Entry, you: &str) -> Value {
    let alerts: Vec<&Alert> = st
        .alerts
        .iter()
        .filter(|a| a.ids().first() == Some(&e.id.as_str()))
        .collect();
    let (note, warning) = row_note(st, e, &alerts);
    let cat = category(st, e);
    let stamp = (e.kind == "charter" && st.status_of(&e.id) == Some(Status::Signed))
        .then(|| signer_of(st, &e.id))
        .flatten()
        .map(|who| {
            if who == you {
                "Signed by you".to_string()
            } else {
                format!("Signed by {who}")
            }
        });
    json!({
        "id": e.id,
        "writer": e.writer,
        "n": e.n,
        "hash": e.hash.get(..8).unwrap_or(&e.hash),
        "prev": if e.prev == crate::model::GENESIS { "inicio".to_string() } else { e.prev.get(..8).unwrap_or(&e.prev).to_string() },
        "hlc": e.hlc,
        "when": views::when(&e.hlc),
        "kind": e.kind,
        "kind_label": kind_label(&e.kind),
        "category": cat,
        "category_label": category_label(cat),
        "hollow": e.kind == "proposal" && st.status_of(&e.id) == Some(Status::Pending),
        "title": row_title(st, e),
        "note": note,
        "warning": warning,
        "struck": struck(st, e),
        "actor": e.actor.short(),
        "actor_name": e.actor.name,
        "role": e.actor.role,
        "human": e.actor.is_human(),
        "signed": e.sig.is_some(),
        "stamp": stamp,
    })
}

fn alert_category(a: &Alert) -> (&'static str, &'static str) {
    match a {
        Alert::Drift { .. } | Alert::UnsignedGoal { .. } => ("drift", "Drift"),
        Alert::FactStale { .. } => ("stale", "Stale fact"),
        Alert::Conflict { .. } | Alert::CharterConflict { .. } => ("conflict", "Conflict"),
        Alert::ClaimLost { .. } => ("claim", "Claim lost"),
        Alert::ClaimExpired { .. } => ("claim", "Claim expired"),
        Alert::BadSignature { .. } => ("signature", "Invalid signature"),
        Alert::NotCounting { .. } | Alert::TaskRecordIgnored { .. } => {
            ("ignored", "Does not count")
        }
        Alert::ProposalSupersedes { .. } => ("conflict", "Proposal against a decision"),
    }
}

/// The text of the alert without repeating its category ("drift: …").
fn alert_text(a: &Alert, label: &str) -> String {
    let text = a.to_string();
    let prefix = format!("{}: ", label.to_lowercase());
    capitalize(text.strip_prefix(&prefix).unwrap_or(&text))
}

fn alert_json(a: &Alert) -> Value {
    let (cat, label) = alert_category(a);
    json!({
        "type": serde_json::to_value(a).ok().and_then(|v| v.get("type").cloned()),
        "category": cat,
        "label": label,
        "text": alert_text(a, label),
        "ids": a.ids(),
    })
}

fn entry_brief(st: &State, e: &Entry) -> Value {
    let mut v = views::entry_json(st, e);
    v["writer"] = json!(e.writer);
    v["n"] = json!(e.n);
    v
}

fn task_json(st: &State, t: &Entry) -> Value {
    let info = st.task(&t.id);
    json!({
        "id": t.id,
        "title": t.title,
        "epic": t.fields.get("epic"),
        "goals": string_list(t.fields.get("goals")),
        "status": info.map(|i| i.status.label()),
        "holder": info.and_then(|i| i.holder.as_ref()).map(|h| h.actor.clone()),
        "blocked_by": info.map(|i| i.blocked_by.clone()).unwrap_or_default(),
        "done": info.and_then(|i| i.done.clone()),
        "line": board::task_status_text(st, &t.id),
        "drift": st.alerts.iter().find(|a| a.is_drift() && a.ids().first() == Some(&t.id.as_str())).map(drift_text),
    })
}

fn epic_json(st: &State, e: &Entry) -> Value {
    let counts: Map<String, Value> = board::counts(st, &e.id)
        .into_iter()
        .map(|(s, n)| (s.label().to_string(), json!(n)))
        .collect();
    let tasks: Vec<Value> = st
        .tasks_of(&e.id)
        .iter()
        .map(|t| task_json(st, t))
        .collect();
    json!({
        "id": e.id,
        "name": epic_name(e),
        "title": e.title,
        "goals": string_list(e.fields.get("goals")),
        "coordinator": tasks::coordinator_of(e),
        "line": board::epic_line(st, e),
        "counts": counts,
        "columns": TaskStatus::ALL.iter().map(|s| json!({"status": s.label(), "label": capitalize(s.label())})).collect::<Vec<_>>(),
        "tasks": tasks,
    })
}

/// The readers with a cursor: the folders in `cursors/`.
fn reader_names(home: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(home.join("cursors")) {
        for ent in rd.flatten() {
            let name = ent.file_name().to_string_lossy().into_owned();
            if ent.path().is_dir() && cursor::validate_reader(&name).is_ok() {
                out.push(name);
            }
        }
    }
    out.sort();
    out
}

/// A reader's cursor, where it is drawn and what they are due to see.
fn reader_json(
    st: &State,
    store: &Store,
    project: &Project,
    reader: &str,
    you: &str,
    since: &Frontier,
) -> Value {
    let new = cursor::new_since(st.entries.values(), since);
    // Where the line goes: the oldest entry the cursor does not cover, in the
    // order in which the ledger is shown.
    let first_new = st
        .order
        .iter()
        .filter_map(|id| st.get(id))
        .find(|e| !since.covers(e))
        .map(|e| e.id.clone());
    let last = cursor::last_advanced(store.home(), reader)
        .map(|t| t.strftime("%Y-%m-%d %H:%M UTC").to_string());
    let role = if reader == you || st.policy.is_owner(reader) {
        "owner"
    } else if reader == "pm" {
        "pm"
    } else if reader.starts_with("epic:") {
        "epic"
    } else {
        "other"
    };
    let mut card = json!({});
    match role {
        "pm" => {
            card["epics"] = json!(
                st.epics()
                    .iter()
                    .map(|e| board::epic_line(st, e))
                    .collect::<Vec<_>>()
            );
            card["questions"] = json!(
                views::questions_for(st, "pm")
                    .iter()
                    .map(|q| json!({"id": q.id, "text": one_line(&q.title, 140)}))
                    .collect::<Vec<_>>()
            );
        }
        "epic" => {
            let name = reader.trim_start_matches("epic:");
            if let Some(e) = st.epics().into_iter().find(|e| epic_name(e) == name) {
                card["epic"] = json!(e.id);
                card["line"] = json!(board::epic_line(st, e));
                card["counts"] = json!(
                    board::counts(st, &e.id)
                        .into_iter()
                        .map(|(s, n)| json!({"status": s.label(), "label": capitalize(s.label()), "n": n}))
                        .collect::<Vec<_>>()
                );
            }
            card["questions"] = json!(
                views::questions_for(st, reader)
                    .iter()
                    .map(|q| json!({"id": q.id, "text": one_line(&q.title, 140)}))
                    .collect::<Vec<_>>()
            );
        }
        _ => {
            let owned: Vec<Entry> = new.iter().map(|e| (*e).clone()).collect();
            let s = views::standup(st, &owned, &project.name, reader, last.clone());
            card["summary"] = json!(s.summary());
            card["waiting"] = json!(
                s.waiting
                    .iter()
                    .map(|i| json!({"id": i.id, "kind": i.kind, "text": strip_command(&i.text)}))
                    .collect::<Vec<_>>()
            );
            card["decisions"] = json!(s.decisions.len());
            card["closed"] = json!(s.closed.len());
            card["alerts"] = json!(s.alerts.len());
        }
    }
    json!({
        "name": reader,
        "role": role,
        "is_you": reader == you,
        "cursor": since.0,
        "has_cursor": !since.is_empty(),
        "new": new.len(),
        "first_new": first_new,
        "last_read": last,
        "card": card,
    })
}

/// "question from human:ana: … → `kontext answer …`" without the command.
fn strip_command(s: &str) -> String {
    match s.split_once(" → `") {
        Some((a, _)) => a.to_string(),
        None => s.to_string(),
    }
}

/// `/api/state`.
pub fn state_json(src: &Source, page: &Page) -> Result<Value> {
    let (mut store, project) = src.open()?;
    let status = crate::sync::status(&mut store, &project)?;
    let st = app::state(&mut store, &project)?;
    let frontier = Frontier(store.index().frontier()?);
    let you = src.you.as_str();

    let mut readers: Vec<Value> = Vec::new();
    let mut names = reader_names(store.home());
    if !names.iter().any(|n| n == you) {
        names.insert(0, you.to_string());
    } else {
        names.retain(|n| n != you);
        names.insert(0, you.to_string());
    }
    for r in &names {
        let since = store.cursor(r).unwrap_or_default();
        readers.push(reader_json(&st, &store, &project, r, you, &since));
    }

    // The ledger, from the newest to the oldest.
    let limit = page.limit.clamp(1, MAX_LIMIT);
    let mut older: Vec<&Entry> = st
        .order
        .iter()
        .rev()
        .filter_map(|id| st.get(id))
        .filter(|e| page.before.as_deref().is_none_or(|b| e.hlc.as_str() < b))
        .collect();
    let remaining = older.len();
    let mut cut = limit.min(older.len());
    // Do not split a group of entries with the same hlc between two pages.
    while cut > 0 && cut < older.len() && older[cut].hlc == older[cut - 1].hlc {
        cut += 1;
    }
    let more = cut < older.len();
    older.truncate(cut);
    let next_before = if more {
        older.last().map(|e| e.hlc.clone())
    } else {
        None
    };
    let rows: Vec<Value> = older.iter().map(|e| row(&st, e, you)).collect();

    let writers: Vec<Value> = frontier
        .0
        .iter()
        .map(|(w, n)| json!({"name": w, "entries": n}))
        .collect();
    let tasks: Vec<Value> = st.of_kind("task").map(|t| task_json(&st, t)).collect();
    let facts: Vec<Value> = st
        .facts()
        .into_iter()
        .map(|f| entry_brief(&st, f))
        .collect();
    let alerts: Vec<Value> = st.alerts.iter().map(alert_json).collect();
    let ids =
        |v: Vec<&Entry>| -> Vec<Value> { v.into_iter().map(|e| entry_brief(&st, e)).collect() };
    let charter = st.charter().or_else(|| st.latest_charter());

    Ok(json!({
        "project": project.name,
        "generated": app::now_text(),
        "you": you,
        "version": format!("{}|{}|{}", frontier.encode(), st.alerts.len(), readers.iter().map(|r| serde_json::to_string(&r["cursor"]).unwrap_or_default()).collect::<Vec<_>>().join(";")),
        "status": status,
        "writers": writers,
        "charter": charter.map(|c| {
            let mut v = entry_brief(&st, c);
            v["signed_by"] = json!(signer_of(&st, &c.id));
            v
        }),
        "goals": ids(st.goals()),
        "decisions": ids(st.law()),
        "epics": st.epics().iter().map(|e| epic_json(&st, e)).collect::<Vec<_>>(),
        "tasks": tasks,
        "questions": ids(st.open_questions()),
        "proposals": ids(st.pending_proposals()),
        "facts": facts,
        "alerts": alerts,
        "readers": readers,
        "ledger": {
            "total": st.order.len(),
            "before": page.before,
            "limit": limit,
            "remaining": remaining,
            "next_before": next_before,
            "entries": rows,
        },
    }))
}

/// `/api/entry/<id>`: `None` if there is no entry with that id (or unique
/// prefix).
pub fn entry_json(src: &Source, id: &str) -> Result<Option<Value>> {
    let (mut store, project) = src.open()?;
    let st = app::state(&mut store, &project)?;
    let Ok(id) = st.resolve(id) else {
        return Ok(None);
    };
    let e = st.get(&id).expect("resolve returns ids that exist");
    let mut v = views::show_json(&st, &id);
    v["row"] = row(&st, e, &src.you);
    v["status_label"] = json!(st.status_of(&id).map(Status::label));
    v["refs_rows"] = json!(
        e.refs
            .iter()
            .chain(st.referrers(&id).iter().map(|r| &r.id))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .filter_map(|r| st.get(r))
            .map(|r| row(&st, r, &src.you))
            .collect::<Vec<_>>()
    );
    Ok(Some(v))
}

/// `/report`: the same report as `kontext report --html`, with the screenshots
/// through `/media/` instead of embedded (those of the ideas, embedded). The
/// ideas are read on each request, so an edited `items.md` shows on reload.
/// The second value says whether it carries "Your ideas" (for its CSP).
pub fn report_html(src: &Source) -> Result<(String, bool)> {
    let (mut store, project) = src.open()?;
    let st = app::state(&mut store, &project)?;
    let mut r = crate::report::build(&st, &project, &src.you);
    r.ideas = src
        .cwd
        .as_deref()
        .and_then(|cwd| crate::report::ideas_for(&st, &project, cwd));
    let html = crate::report::html(&r, &crate::report::Images::url("/media/"))?;
    Ok((html, r.has_ideas()))
}

// ------------------------------------------------------------ HTTP

/// The options of `kontext ui`.
#[derive(Debug, Clone)]
pub struct Options {
    pub host: String,
    pub port: u16,
    pub open: bool,
}

/// Is it a loopback address?
pub fn is_loopback_host(host: &str) -> bool {
    let h = host.trim().trim_start_matches('[').trim_end_matches(']');
    if h.eq_ignore_ascii_case("localhost") {
        return true;
    }
    h.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// Opens the port. A non-loopback address only gets here if it was asked for
/// with `--host`; it warns because the page has no password.
pub fn bind(opts: &Options) -> Result<(TcpListener, bool)> {
    let loopback = is_loopback_host(&opts.host);
    if !loopback {
        eprintln!(
            "kontext: warning: listening on {} (not loopback): the page has no password and \
             anyone who reaches that address can read the ledger",
            opts.host
        );
    }
    let host = opts.host.trim_start_matches('[').trim_end_matches(']');
    let addrs: Vec<SocketAddr> = (host, opts.port)
        .to_socket_addrs()
        .with_context(|| format!("could not resolve {}", opts.host))?
        .collect();
    let listener = TcpListener::bind(&addrs[..]).with_context(|| {
        format!(
            "could not listen on {}:{} (is another kontext ui running? try --port)",
            opts.host, opts.port
        )
    })?;
    Ok((listener, loopback))
}

/// Serves the page until the process is killed.
pub fn serve(src: Source, opts: &Options) -> Result<()> {
    let (listener, loopback) = bind(opts)?;
    let addr = listener.local_addr()?;
    let url = match addr {
        SocketAddr::V6(a) => format!("http://[{}]:{}/", a.ip(), a.port()),
        SocketAddr::V4(a) => format!("http://{}:{}/", a.ip(), a.port()),
    };
    // The two lines in a single write and without println!: if whoever reads
    // the output closes the pipe after the first line, println! would panic
    // with EPIPE and bring down the server with open connections.
    let mut out = std::io::stdout().lock();
    let _ = write!(out, "kontext ui: {url}\nRead-only. Ctrl-C to stop.\n");
    let _ = out.flush();
    drop(out);
    if opts.open {
        open_browser(&url);
    }
    let shared = Arc::new(Server {
        src,
        loopback,
        port: addr.port(),
        lock: Mutex::new(()),
        page: page(),
    });
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let s = Arc::clone(&shared);
        std::thread::spawn(move || {
            let _ = s.handle(stream);
        });
    }
    Ok(())
}

fn open_browser(url: &str) {
    let cmd = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let ok = std::process::Command::new(cmd)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok();
    if !ok {
        eprintln!("kontext: warning: could not open the browser; open {url}");
    }
}

struct Server {
    src: Source,
    loopback: bool,
    port: u16,
    /// One request at a time builds the state (they share the index).
    lock: Mutex<()>,
    page: String,
}

pub struct Response {
    pub status: u16,
    pub content_type: &'static str,
    pub body: Vec<u8>,
    /// The CSP, if it is not the usual one ([`CSP`]).
    pub csp: Option<String>,
}

/// The CSP of the page and of the API: nothing from outside.
pub const CSP: &str = "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; \
                       connect-src 'self'; img-src 'self' data:; base-uri 'none'; form-action 'none'; \
                       frame-ancestors 'none'";

impl Response {
    fn json(status: u16, v: &Value) -> Response {
        Response {
            status,
            content_type: "application/json; charset=utf-8",
            body: serde_json::to_vec(v).unwrap_or_default(),
            csp: None,
        }
    }

    fn text(status: u16, s: &str) -> Response {
        Response {
            status,
            content_type: "text/plain; charset=utf-8",
            body: s.as_bytes().to_vec(),
            csp: None,
        }
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Internal Server Error",
    }
}

/// Removes the `%xx` of a part of the URL.
pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                let hex = std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(v) => {
                        out.push(v);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn query_map(q: &str) -> BTreeMap<String, String> {
    q.split('&')
        .filter(|p| !p.is_empty())
        .map(|p| match p.split_once('=') {
            Some((k, v)) => (percent_decode(k), percent_decode(v)),
            None => (percent_decode(p), String::new()),
        })
        .collect()
}

impl Server {
    fn handle(&self, mut stream: TcpStream) -> Result<()> {
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        stream.set_write_timeout(Some(Duration::from_secs(10)))?;
        let (method, target, host) = {
            let mut reader = BufReader::new((&stream).take(32 * 1024));
            let mut line = String::new();
            reader.read_line(&mut line)?;
            let mut parts = line.split_whitespace();
            let method = parts.next().unwrap_or("").to_string();
            let target = parts.next().unwrap_or("").to_string();
            let mut host = None;
            loop {
                let mut h = String::new();
                if reader.read_line(&mut h)? == 0 || h.trim().is_empty() {
                    break;
                }
                if let Some((k, v)) = h.split_once(':')
                    && k.trim().eq_ignore_ascii_case("host")
                {
                    host = Some(v.trim().to_string());
                }
            }
            (method, target, host)
        };
        let head = method == "HEAD";
        let resp = if method.is_empty() || target.is_empty() {
            Response::text(400, "invalid request\n")
        } else if method != "GET" && !head {
            Response::text(405, "kontext ui is read-only: GET only\n")
        } else if self.loopback && !host.as_deref().is_some_and(|h| self.host_ok(h)) {
            Response::text(
                403,
                "Host not allowed: kontext ui only answers to 127.0.0.1, localhost or [::1]\n",
            )
        } else {
            self.route(&target)
        };
        write_response(&mut stream, &resp, head)?;
        // Close calmly: if something was left unread, closing abruptly sends an
        // RST and the client may lose the response.
        let _ = stream.shutdown(std::net::Shutdown::Write);
        stream.set_read_timeout(Some(Duration::from_millis(500)))?;
        let _ = std::io::copy(&mut (&stream).take(64 * 1024), &mut std::io::sink());
        Ok(())
    }

    /// The `Host` has to be loopback and with the page's port
    /// (contra DNS rebinding).
    fn host_ok(&self, host: &str) -> bool {
        let (name, port) = match host.strip_prefix('[') {
            Some(rest) => match rest.split_once(']') {
                Some((ip, tail)) => (ip, tail.strip_prefix(':')),
                None => return false,
            },
            None => match host.rsplit_once(':') {
                Some((n, p)) => (n, Some(p)),
                None => (host, None),
            },
        };
        if port.is_some_and(|p| p.parse::<u16>().ok() != Some(self.port)) {
            return false;
        }
        is_loopback_host(name)
    }

    pub fn route(&self, target: &str) -> Response {
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        let q = query_map(query);
        match path {
            "/" | "/index.html" => Response {
                status: 200,
                content_type: "text/html; charset=utf-8",
                body: self.page.as_bytes().to_vec(),
                csp: None,
            },
            "/api/state" => {
                let limit = match q.get("limit").map(|l| l.parse::<usize>()) {
                    None => DEFAULT_LIMIT,
                    Some(Ok(n)) if n > 0 => n,
                    Some(_) => {
                        return Response::json(
                            400,
                            &json!({"error": "limit must be a number greater than 0"}),
                        );
                    }
                };
                let page = Page {
                    before: q.get("before").filter(|b| !b.is_empty()).cloned(),
                    limit,
                };
                let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
                match state_json(&self.src, &page) {
                    Ok(v) => Response::json(200, &v),
                    Err(e) => Response::json(500, &json!({"error": format!("{e:#}")})),
                }
            }
            "/report" => {
                let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
                match report_html(&self.src) {
                    Ok((body, with_ideas)) => Response {
                        status: 200,
                        content_type: "text/html; charset=utf-8",
                        body: body.into_bytes(),
                        // Tighter than the page's: only its style and its script
                        // (by hash) and the embedded font run.
                        csp: Some(crate::report::csp_for(with_ideas)),
                    },
                    Err(e) => Response::text(500, &format!("{e:#}\n")),
                }
            }
            p if p.starts_with("/media/") => {
                let name = percent_decode(&p["/media/".len()..]);
                match crate::media::valid_media_name(&name) {
                    Some((_, ext)) if ext != "html" => {
                        match std::fs::read(self.src.home.join("media").join(&name)) {
                            Ok(body) => Response {
                                status: 200,
                                content_type: crate::media::content_type(ext),
                                body,
                                csp: None,
                            },
                            Err(_) => Response::text(404, "not found\n"),
                        }
                    }
                    _ => Response::text(404, "not found\n"),
                }
            }
            p if p.starts_with("/api/entry/") => {
                let id = percent_decode(&p["/api/entry/".len()..]);
                let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
                match entry_json(&self.src, &id) {
                    Ok(Some(v)) => Response::json(200, &v),
                    Ok(None) => Response::json(
                        404,
                        &json!({"error": format!("there is no entry with id {id}")}),
                    ),
                    Err(e) => Response::json(500, &json!({"error": format!("{e:#}")})),
                }
            }
            _ => Response::text(404, "not found\n"),
        }
    }
}

fn write_response(stream: &mut TcpStream, r: &Response, head: bool) -> Result<()> {
    let mut out = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store\r\n\
         X-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\n\
         Content-Security-Policy: {}\r\n\
         Connection: close\r\n\r\n",
        r.status,
        reason(r.status),
        r.content_type,
        r.body.len(),
        r.csp.as_deref().unwrap_or(CSP)
    )
    .into_bytes();
    if !head {
        out.extend_from_slice(&r.body);
    }
    stream.write_all(&out)?;
    stream.flush()?;
    Ok(())
}

/// Who looks at the page by default: the human of `--actor`/`KONTEXT_ACTOR`, or
/// this machine's (as `standup`).
pub fn default_viewer(actor: Option<&str>, project: &Project, home: &std::path::Path) -> String {
    match crate::model::Actor::resolve(actor) {
        Ok(a) if a.is_human() => a.name,
        _ => config::human_name_in(project, home),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_hosts() {
        assert!(is_loopback_host("127.0.0.1"));
        assert!(is_loopback_host("localhost"));
        assert!(is_loopback_host("[::1]"));
        assert!(is_loopback_host("::1"));
        assert!(!is_loopback_host("0.0.0.0"));
        assert!(!is_loopback_host("192.168.1.4"));
        assert!(!is_loopback_host("example.com"));
    }

    #[test]
    fn decodes_query() {
        assert_eq!(
            percent_decode("2026-11-03T18%3A22%3A10.123Z.0004"),
            "2026-11-03T18:22:10.123Z.0004"
        );
        assert_eq!(percent_decode("a%2"), "a%2");
        assert_eq!(percent_decode("%zz"), "%zz");
        let q = query_map("before=x%3Ay&limit=5");
        assert_eq!(q["before"], "x:y");
        assert_eq!(q["limit"], "5");
    }

    #[test]
    fn page_has_no_placeholders_and_no_remote_urls() {
        let p = page();
        assert!(!p.contains("/*KONTEXT_STYLE*/") && !p.contains("/*KONTEXT_SCRIPT*/"));
        assert!(
            !p.contains("http"),
            "the page must not ask anything of the internet"
        );
    }
}
