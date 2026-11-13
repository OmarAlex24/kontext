//! Team views (phase 3, DESIGN §8): `brief` for an epic's coordinator, `next`
//! for the implementer or tester, and the per-epic line for the pm.
//!
//! Like `resume`, they write nothing. They take the state, what is new since
//! the reader's cursor, and the assignment mark at that same moment.

use serde_json::{Value, json};

use crate::cursor::Frontier;
use crate::features::{self, FeatureStatus};
use crate::model::{Entry, is_record_kind};
use crate::state::{State, Status, string_list};
use crate::tasks::{self, TaskStatus, epic_name, touches_epic};
use crate::views::{
    capped, charter_lines, entry_json, field, largest_fitting, new_line, one_line, partial_notice,
    plural, question_is_for, when,
};

/// The last activity on an epic (ms): the epic itself, its tasks, their
/// ledger entries and journal, and the decisions and questions that touch it.
pub fn last_activity(st: &State, epic: &Entry) -> i64 {
    let tasks: Vec<&str> = st
        .tasks_of(&epic.id)
        .iter()
        .map(|t| t.id.as_str())
        .collect();
    let mut last = tasks::hlc_ms(&epic.hlc);
    for e in st.entries.values() {
        let task_ref = e.refs.first().map(String::as_str);
        let journal_task = e.fields.get("task").and_then(Value::as_str);
        let hit = (e.kind == "task" && tasks.contains(&e.id.as_str()))
            || (is_record_kind(&e.kind) && task_ref.is_some_and(|t| tasks.contains(&t)))
            || journal_task.is_some_and(|t| tasks.contains(&t))
            || (matches!(e.kind.as_str(), "decision" | "question" | "fact")
                && touches_epic(e, epic));
        if hit {
            last = last.max(tasks::hlc_ms(&e.hlc));
        }
    }
    last
}

/// How many tasks are in each status.
pub fn counts(st: &State, epic: &str) -> Vec<(TaskStatus, usize)> {
    let tasks = st.tasks_of(epic);
    TaskStatus::ALL
        .iter()
        .map(|s| {
            (
                *s,
                tasks
                    .iter()
                    .filter(|t| st.task_status(&t.id) == Some(*s))
                    .count(),
            )
        })
        .collect()
}

fn count_text(n: usize, s: TaskStatus) -> String {
    format!("{n} {}", if n == 1 { s.label() } else { s.plural() })
}

/// "Pagos: 3/5 closed, 1 claimed, 1 blocked; last activity 2 h ago". An epic
/// with features starts with them: "Pagos: 2/3 features done, 1 to validate;
/// 3/5 closed, …".
pub fn epic_line(st: &State, epic: &Entry) -> String {
    let c = counts(st, &epic.id);
    let total: usize = c.iter().map(|(_, n)| n).sum();
    let get = |s: TaskStatus| c.iter().find(|(x, _)| *x == s).map(|x| x.1).unwrap_or(0);
    let mut parts = vec![format!("{}/{total} closed", get(TaskStatus::Closed))];
    for s in [
        TaskStatus::Claimed,
        TaskStatus::Assigned,
        TaskStatus::Ready,
        TaskStatus::Blocked,
    ] {
        let n = get(s);
        if n > 0 {
            parts.push(count_text(n, s));
        }
    }
    let drift = st
        .alerts
        .iter()
        .filter(|a| a.is_drift())
        .filter(|a| {
            a.ids().iter().any(|id| {
                *id == epic.id
                    || st
                        .get(id)
                        .and_then(|t| st.epic_of(t))
                        .is_some_and(|e| e.id == epic.id)
            })
        })
        .count();
    let drift = if drift > 0 {
        format!("; {}", plural(drift, "drift", "drift"))
    } else {
        String::new()
    };
    let features = if features::has_features(st, &epic.id) {
        format!("{}; ", features::counts_text(st, &epic.id))
    } else {
        String::new()
    };
    format!(
        "{}: {features}{}; last activity {}{drift}",
        epic.title.trim(),
        parts.join(", "),
        tasks::ago(st.now_ms, last_activity(st, epic))
    )
}

/// The epics section of `resume`: one line per epic, no tasks.
pub fn epic_lines(st: &State) -> Vec<String> {
    let epics = st.epics();
    let mut l = vec![format!("## Epics ({})", epics.len())];
    if epics.is_empty() {
        l.push("- (none)".into());
    }
    for e in epics {
        l.push(format!("- {} {}", e.id, epic_line(st, e)));
    }
    l
}

/// Decisions in force that touch an epic (or one of its tasks).
fn epic_decisions<'a>(st: &'a State, epic: &Entry, task: Option<&str>) -> Vec<&'a Entry> {
    st.law()
        .into_iter()
        .filter(|d| touches_epic(d, epic) || task.is_some_and(|t| d.refs.iter().any(|r| r == t)))
        .collect()
}

/// Facts in force for an epic (by tag, `fields.epic`, or reference), without
/// the ones that went stale (those come out as alerts).
fn epic_facts<'a>(st: &'a State, epic: &Entry, task: Option<&str>) -> Vec<&'a Entry> {
    st.live_facts()
        .into_iter()
        .filter(|f| touches_epic(f, epic) || task.is_some_and(|t| f.refs.iter().any(|r| r == t)))
        .collect()
}

fn goal_label(st: &State, g: &str) -> String {
    match st.get(g) {
        Some(e) => {
            let s = st.status_of(g).map(|s| s.label()).unwrap_or("?");
            format!("{g} [{s}] {}", one_line(&e.title, 100))
        }
        None => format!("{g} (does not exist)"),
    }
}

fn holder_text(st: &State, task: &str) -> String {
    let Some(info) = st.task(task) else {
        return String::new();
    };
    match &info.holder {
        Some(h) => format!(
            "{} {} (activity {})",
            if h.kind == "assign" {
                "assigned to"
            } else {
                "claimed by"
            },
            h.actor,
            tasks::ago(st.now_ms, info.last_activity)
        ),
        None => String::new(),
    }
}

/// One board line.
fn task_line(st: &State, t: &Entry, detail: bool) -> String {
    let info = st.task(&t.id).cloned().unwrap_or_default();
    let mut s = format!("- {} {}", t.id, one_line(&t.title, 120));
    match info.status {
        TaskStatus::Blocked => {
            let by: Vec<String> = info
                .blocked_by
                .iter()
                .map(|d| match st.task_status(d) {
                    Some(x) => format!("{d} ({x})"),
                    None => format!("{d} (does not exist)"),
                })
                .collect();
            s.push_str(&format!(" → waits for {}", by.join(", ")));
        }
        TaskStatus::Assigned | TaskStatus::Claimed => {
            s.push_str(&format!(" → {}", holder_text(st, &t.id)));
        }
        TaskStatus::Closed => {
            let ev = info
                .done
                .as_ref()
                .and_then(|d| st.get(d))
                .map(|d| {
                    format!(
                        " · closed by {} · evidence: {}",
                        d.actor.short(),
                        tasks::evidence_summary(d.fields.get("evidence"))
                    )
                })
                .unwrap_or_default();
            s.push_str(&ev);
        }
        TaskStatus::Ready => {
            if let Some(exp) = &info.expired {
                s.push_str(&format!(" (the claim by {} expired)", exp.actor));
            }
        }
    }
    if detail
        && info.status != TaskStatus::Closed
        && let Some(a) = field(t, "acceptance")
    {
        s.push_str(&format!(" · acceptance: {}", one_line(a, 120)));
    }
    s
}

/// What `brief` needs besides the state.
#[derive(Debug, Clone)]
pub struct BriefOpts {
    pub project: String,
    pub epic: String,
    pub max_chars: usize,
    pub generated: String,
    pub frontier: Frontier,
}

#[derive(Debug, Clone, Copy)]
struct Level {
    journal: usize,
    detail: bool,
    facts: bool,
    list_cap: Option<usize>,
    /// The last cuts, only when the ones above are not enough: how many of
    /// the other decisions in force, and then of the epic's, are listed (the
    /// newest; `None`: all of them).
    others: Option<usize>,
    epic_decisions: Option<usize>,
}

const JOURNAL_N: usize = 10;
const LIST_CAP: usize = 5;

fn epic_journal<'a>(st: &'a State, epic: &Entry) -> Vec<&'a Entry> {
    let tasks: Vec<String> = st.tasks_of(&epic.id).iter().map(|t| t.id.clone()).collect();
    st.journal()
        .into_iter()
        .filter(|j| {
            j.fields
                .get("task")
                .and_then(Value::as_str)
                .is_some_and(|t| tasks.iter().any(|x| x == t))
                || touches_epic(j, epic)
        })
        .collect()
}

fn epic_alerts(st: &State, epic: &Entry) -> Vec<String> {
    st.alerts
        .iter()
        .filter(|a| {
            // A stale fact matters to the coordinator if it belongs to their epic
            // or to the whole project (no epic).
            if let crate::state::Alert::FactStale { id, .. } = a {
                return st.get(id).is_some_and(|f| {
                    touches_epic(f, epic) || (field(f, "epic").is_none() && f.refs.is_empty())
                });
            }
            a.ids().iter().any(|id| {
                *id == epic.id
                    || st
                        .get(id)
                        .and_then(|t| st.epic_of(t))
                        .is_some_and(|e| e.id == epic.id)
            })
        })
        .map(|a| format!("- {a}"))
        .collect()
}

/// Does it matter to the epic's coordinator? Anything in their epic (tasks,
/// their ledger entries and journal, questions for them, the epic's facts and
/// decisions), and anything that changes the project-wide rules (charter,
/// goals, decisions).
pub fn concerns_epic(st: &State, e: &Entry, epic: &Entry) -> bool {
    let of_epic_task = |id: &str| {
        st.get(id)
            .and_then(|t| st.epic_of(t))
            .is_some_and(|x| x.id == epic.id)
    };
    match e.kind.as_str() {
        "charter" | "goal" | "decision" | "sign" | "supersede" | "accept" | "reject" => true,
        "task" => of_epic_task(&e.id),
        "assign" | "claim" | "release" | "done" | "reopen" => {
            e.refs.first().is_some_and(|t| of_epic_task(t))
        }
        "question" => {
            question_is_for(st, e, &format!("epic:{}", epic_name(epic)))
                || field(e, "task").is_some_and(of_epic_task)
        }
        "answer" => e
            .refs
            .first()
            .and_then(|q| st.get(q))
            .is_some_and(|q| concerns_epic(st, q, epic)),
        "journal" => field(e, "task").is_some_and(of_epic_task) || touches_epic(e, epic),
        _ => touches_epic(e, epic),
    }
}

fn build_brief(st: &State, new: &[Entry], o: &BriefOpts, lv: Level) -> String {
    let epic = st.get(&o.epic).expect("the epic exists");
    let name = epic_name(epic);
    let reader = format!("epic:{name}");
    let mark = o.frontier.encode();
    let mut l = vec![format!(
        "# Brief for epic {} ({reader}) in {}",
        epic.title.trim(),
        o.project
    )];
    let status = st.status_of(&epic.id).map(|s| s.label()).unwrap_or("?");
    l.push(format!(
        "Generated {}. {} [{status}], coordinator {}. Assignment mark: `{mark}`.",
        o.generated,
        epic.id,
        tasks::coordinator_of(epic)
    ));
    l.push(String::new());
    l.push(
        "You coordinate this epic: hand out the ready tasks, answer what the charter \
         and the decisions already answer, and escalate with `kontext ask --to pm`. \
         Don't decide: propose (`kontext propose`). Details for any id: `kontext show <id>`."
            .into(),
    );
    l.push(String::new());
    l.extend(charter_lines(st));
    l.push(String::new());
    let goals = string_list(epic.fields.get("goals"));
    l.push(format!("## Epic goals ({})", goals.len()));
    if goals.is_empty() {
        l.push("- (none: drift; ask the pm to link it to a goal)".into());
    }
    for g in &goals {
        l.push(format!("- {}", goal_label(st, g)));
    }

    let ds = epic_decisions(st, epic, None);
    l.push(String::new());
    l.push(format!("## Epic decisions ({})", ds.len()));
    if ds.is_empty() {
        l.push("- (none)".into());
    }
    // The newest stay: the decisions are in ledger order.
    let from = ds
        .len()
        .saturating_sub(lv.epic_decisions.unwrap_or(usize::MAX));
    if from > 0 {
        l.push(partial_notice(from, ""));
    }
    for d in &ds[from..] {
        let why = if d.why.trim().is_empty() || !lv.detail {
            String::new()
        } else {
            format!(" (why: {})", one_line(&d.why, 160))
        };
        l.push(format!("- {} {}{why}", d.id, d.title.trim()));
    }
    let others: Vec<&Entry> = st
        .law()
        .into_iter()
        .filter(|d| !ds.iter().any(|x| x.id == d.id))
        .collect();
    if !others.is_empty() {
        l.push(String::new());
        l.push(format!("## Other decisions in force ({})", others.len()));
        let from = others.len().saturating_sub(lv.others.unwrap_or(usize::MAX));
        if from > 0 {
            l.push(partial_notice(from, ""));
        }
        let titles: Vec<String> = others[from..]
            .iter()
            .map(|d| format!("{} {}", d.id, d.title.trim()))
            .collect();
        if !titles.is_empty() {
            l.push(format!("- {}", titles.join("; ")));
        }
    }

    let ts = st.tasks_of(&epic.id);
    let c = counts(st, &epic.id);
    let summary: Vec<String> = c
        .iter()
        .filter(|(_, n)| *n > 0)
        .map(|(s, n)| count_text(*n, *s))
        .collect();
    l.push(String::new());
    l.push(format!(
        "## Board ({}{})",
        plural(ts.len(), "task", "tasks"),
        if summary.is_empty() {
            String::new()
        } else {
            format!(": {}", summary.join(", "))
        }
    ));
    if features::has_features(st, &epic.id) {
        feature_board(st, epic, &ts, lv, &mut l);
    } else if ts.is_empty() {
        l.push(format!(
            "- (none) Add tasks with `kontext task add \"<title>\" --epic {name} --acceptance \"…\"`"
        ));
    }
    for s in [
        TaskStatus::Ready,
        TaskStatus::Claimed,
        TaskStatus::Assigned,
        TaskStatus::Blocked,
        TaskStatus::Closed,
    ] {
        let of: Vec<&&Entry> = ts
            .iter()
            .filter(|t| st.task_status(&t.id) == Some(s))
            .collect();
        // With features, the board is grouped by feature instead.
        if of.is_empty() || features::has_features(st, &epic.id) {
            continue;
        }
        let mut title = s.plural().to_string();
        title[..1].make_ascii_uppercase();
        l.push(format!("### {title} ({})", of.len()));
        let items: Vec<String> = of.iter().map(|t| task_line(st, t, lv.detail)).collect();
        // Closed tasks are the only part of the board that gets cut.
        let cap = if s == TaskStatus::Closed {
            lv.list_cap
        } else {
            None
        };
        capped(&mut l, items, cap, "ver `kontext brief --json`");
    }
    if ts
        .iter()
        .any(|t| st.task_status(&t.id) == Some(TaskStatus::Ready))
    {
        l.push(format!(
            "To hand this to an implementer, run `kontext --actor agent:<name> --role implementer next --epic {name} --claim` and paste the output into their assignment."
        ));
    }

    let qs: Vec<&Entry> = st
        .open_questions()
        .into_iter()
        .filter(|q| question_is_for(st, q, &reader))
        .collect();
    l.push(String::new());
    l.push(format!("## Questions for {reader} ({})", qs.len()));
    if qs.is_empty() {
        l.push("- (none)".into());
    }
    let items = qs
        .iter()
        .map(|q| {
            let task = field(q, "task")
                .map(|t| format!(" [task {t}]"))
                .unwrap_or_default();
            format!(
                "- {} {}{task} (from {}) → `kontext answer {} \"…\"`",
                q.id,
                one_line(&q.title, 140),
                q.actor.short(),
                q.id
            )
        })
        .collect();
    capped(&mut l, items, lv.list_cap, "ver `kontext brief --json`");

    if lv.facts {
        let fs = epic_facts(st, epic, None);
        l.push(String::new());
        l.push(format!("## Epic facts ({})", fs.len()));
        if fs.is_empty() {
            l.push("- (none)".into());
        }
        let items = fs
            .iter()
            .rev()
            .map(|f| {
                format!(
                    "- {}{} {}",
                    f.id,
                    crate::facts::mark(st, &f.id),
                    one_line(&f.title, 160)
                )
            })
            .collect();
        capped(&mut l, items, lv.list_cap, "see `kontext recall <topic>`");
    }

    let journal = epic_journal(st, epic);
    let from = journal.len().saturating_sub(lv.journal);
    l.push(String::new());
    l.push(format!(
        "## Epic journal (last {} of {})",
        journal.len() - from,
        journal.len()
    ));
    if journal.len() == from {
        l.push("- (none)".into());
    }
    for j in &journal[from..] {
        let task = field(j, "task")
            .map(|t| format!(" [{t}]"))
            .unwrap_or_default();
        l.push(format!(
            "- {} {}{task} {}",
            when(&j.hlc),
            j.actor.short(),
            one_line(&j.title, 160)
        ));
    }

    let alerts = epic_alerts(st, epic);
    if !alerts.is_empty() {
        l.push(String::new());
        l.push(format!("## Epic alerts ({})", alerts.len()));
        capped(&mut l, alerts, lv.list_cap, "see `kontext standup`");
    }

    l.push(String::new());
    l.push(format!(
        "## What's new since your last visit ({})",
        new.len()
    ));
    if new.is_empty() {
        l.push("- (none)".into());
    }
    let items = new.iter().map(|e| new_line(st, e)).collect();
    capped(
        &mut l,
        items,
        Some(lv.list_cap.unwrap_or(30)),
        &format!("see `kontext brief {name} --peek --json`"),
    );

    l.push(String::new());
    l.push("## Assignment mark".into());
    l.push(format!("`{mark}`"));
    l.push(
        "It goes in every assignment and in `kontext done <T> --since <mark>`, so `done` knows whether anything arrived that changes the task."
            .into(),
    );
    l.join("\n") + "\n"
}

/// The board of an epic with features: each feature in force with its state
/// and acceptance, its tasks under it (open first), and the tasks with no
/// feature (or whose feature is no longer in force) in a last group.
fn feature_board(st: &State, epic: &Entry, ts: &[&Entry], lv: Level, l: &mut Vec<String>) {
    let fs = features::of_epic(st, &epic.id);
    l.push(format!("{}.", features::counts_text(st, &epic.id)));
    let line = |t: &Entry| {
        let status = st.task_status(&t.id).map(|s| s.label()).unwrap_or("?");
        let s = task_line(st, t, lv.detail);
        format!("- [{status}] {}", s.strip_prefix("- ").unwrap_or(&s))
    };
    // Open tasks are never cut; closed ones are, like the rest of the board.
    let group = |l: &mut Vec<String>, of: Vec<&&Entry>| {
        let (closed, open): (Vec<&&Entry>, Vec<&&Entry>) = of
            .into_iter()
            .partition(|t| st.task_status(&t.id) == Some(TaskStatus::Closed));
        l.extend(open.iter().map(|t| line(t)));
        let items = closed.iter().map(|t| line(t)).collect();
        capped(l, items, lv.list_cap, "see `kontext brief --json`");
    };
    for f in &fs {
        let status = features::status_text(st, &f.id).unwrap_or_default();
        l.push(format!(
            "### Feature {} {} [{status}]",
            f.id,
            one_line(&f.title, 120)
        ));
        match features::acceptance(f) {
            Some(a) => l.push(format!(
                "When it works, the human sees: {}",
                one_line(a, 240)
            )),
            None => l.push("When it works, the human sees: (not stated)".into()),
        }
        let of: Vec<&&Entry> = ts
            .iter()
            .filter(|t| features::counted_feature(st, t) == Some(f.id.as_str()))
            .collect();
        if of.is_empty() {
            l.push(format!(
                "- (no tasks yet) Add them with `kontext task add \"<title>\" --feature {} --acceptance \"…\"`",
                f.id
            ));
        }
        group(l, of);
        if st.feature(&f.id).map(|i| i.status) == Some(FeatureStatus::ToValidate) {
            l.push(format!(
                "All its tasks are closed: check it against what the human should see (yourself or with a \
                 tester) and validate it with `kontext done {} --evidence …` (or `--run \"<command>\"`).",
                f.id
            ));
        }
    }
    let in_force: Vec<&str> = fs.iter().map(|f| f.id.as_str()).collect();
    let loose: Vec<&&Entry> = ts
        .iter()
        .filter(|t| features::counted_feature(st, t).is_none_or(|f| !in_force.contains(&f)))
        .collect();
    if !loose.is_empty() {
        l.push(format!("### No feature ({})", loose.len()));
        group(l, loose);
    }
}

/// `kontext brief <epic>`, cut to the budget: the journal first, then the why
/// of decisions and criteria, facts, and then the lists. If that is not
/// enough, the titles of the other decisions in force and then the epic's get
/// what is left (the newest), with a line that says how many are not shown.
/// The charter, the goals, the open tasks, and the mark are never cut.
pub fn brief(st: &State, new: &[Entry], o: &BriefOpts) -> String {
    let epic = st.get(&o.epic).expect("the epic exists");
    let new: Vec<Entry> = new
        .iter()
        .filter(|e| concerns_epic(st, e, epic))
        .cloned()
        .collect();
    let new = &new[..];
    let mut lv = Level {
        journal: JOURNAL_N,
        detail: true,
        facts: true,
        list_cap: None,
        others: None,
        epic_decisions: None,
    };
    let mut text = build_brief(st, new, o, lv);
    while text.chars().count() > o.max_chars && lv.journal > 0 {
        lv.journal -= 1;
        text = build_brief(st, new, o, lv);
    }
    for step in 0..3 {
        if text.chars().count() <= o.max_chars {
            break;
        }
        match step {
            0 => lv.detail = false,
            1 => lv.facts = false,
            _ => lv.list_cap = Some(LIST_CAP),
        }
        text = build_brief(st, new, o, lv);
    }
    if text.chars().count() <= o.max_chars {
        return text;
    }
    // A long ledger of decisions: first the other decisions get what is
    // left, then the epic's own.
    let others = st.law().len() - epic_decisions(st, epic, None).len();
    if let Some(t) = largest_fitting(others, o.max_chars, |n| {
        lv.others = Some(n);
        build_brief(st, new, o, lv)
    }) {
        return t;
    }
    lv.others = Some(0);
    let own = epic_decisions(st, epic, None).len();
    if let Some(t) = largest_fitting(own, o.max_chars, |n| {
        lv.epic_decisions = Some(n);
        build_brief(st, new, o, lv)
    }) {
        return t;
    }
    // Not even the rest fits: the shorter of all the titles and none.
    lv.epic_decisions = Some(0);
    let none = build_brief(st, new, o, lv);
    let without = if none.chars().count() < text.chars().count() {
        text = none;
        " even without the titles of the decisions in force"
    } else {
        ""
    };
    text.push_str(&format!(
        "\n(kontext: this is over {} characters{without}; the charter, the goals, the open tasks, and the mark are never cut)\n",
        o.max_chars
    ));
    text
}

fn task_json(st: &State, t: &Entry) -> Value {
    let info = st.task(&t.id).cloned().unwrap_or_default();
    let mut v = entry_json(st, t);
    v["task_status"] = json!(info.status);
    v["holder"] = json!(info.holder);
    v["blocked_by"] = json!(info.blocked_by);
    v["done"] = json!(info.done);
    if let Some(d) = info.done.as_ref().and_then(|d| st.get(d)) {
        v["evidence"] = d.fields.get("evidence").cloned().unwrap_or(Value::Null);
    }
    v
}

/// `kontext brief --json`: the same, without cuts.
pub fn brief_json(st: &State, new: &[Entry], o: &BriefOpts) -> Value {
    let epic = st.get(&o.epic).expect("the epic exists");
    let name = epic_name(epic);
    let reader = format!("epic:{name}");
    let list =
        |v: Vec<&Entry>| -> Vec<Value> { v.into_iter().map(|e| entry_json(st, e)).collect() };
    let mut v = json!({
        "project": o.project,
        "epic": entry_json(st, epic),
        "name": name,
        "reader": reader,
        "coordinator": tasks::coordinator_of(epic),
        "generated": o.generated,
        "mark": o.frontier.encode(),
        "charter": st.charter().or_else(|| st.latest_charter()).map(|c| entry_json(st, c)),
        "goals": string_list(epic.fields.get("goals")).iter().filter_map(|g| st.get(g)).map(|g| entry_json(st, g)).collect::<Vec<_>>(),
        "decisions": list(epic_decisions(st, epic, None)),
        "tasks": st.tasks_of(&epic.id).iter().map(|t| task_json(st, t)).collect::<Vec<_>>(),
        "questions": list(st.open_questions().into_iter().filter(|q| question_is_for(st, q, &reader)).collect()),
        "facts": list(epic_facts(st, epic, None)),
        "journal": list(epic_journal(st, epic)),
        "alerts": epic_alerts(st, epic),
        "new": new.iter().filter(|e| concerns_epic(st, e, epic)).map(|e| entry_json(st, e)).collect::<Vec<_>>(),
    });
    if features::has_features(st, &epic.id) {
        v["features"] = features::of_epic(st, &epic.id)
            .into_iter()
            .map(|f| features::json(st, f))
            .collect();
    }
    v
}

/// What `next` needs.
#[derive(Debug, Clone)]
pub struct NextOpts {
    pub project: String,
    pub frontier: Frontier,
    /// The `claim` just written, if any.
    pub claimed: Option<String>,
    /// The actor that asked for the task (`agent:impl-1`).
    pub actor: String,
    /// Character budget.
    pub max_chars: usize,
}

/// Answers to the task's questions.
fn task_answers<'a>(st: &'a State, task: &str) -> Vec<(&'a Entry, Vec<&'a Entry>)> {
    st.of_kind("question")
        .filter(|q| field(q, "task") == Some(task))
        .map(|q| {
            let answers = st
                .answers
                .get(&q.id)
                .map(|a| a.iter().filter_map(|x| st.get(x)).collect())
                .unwrap_or_default();
            (q, answers)
        })
        .collect()
}

/// How much `next` shows at each cut level.
#[derive(Debug, Clone, Copy)]
struct NextLevel {
    /// The why of the decisions.
    detail: bool,
    /// How many facts.
    facts: usize,
    /// Cap for the other lists (decisions, questions).
    list_cap: Option<usize>,
}

/// `kontext next`: the assignment for one task, cut to `max_chars` (DESIGN §8):
/// first the why of the decisions, then the facts, then the lists. The task,
/// its acceptance, the mark, and how to close it are never cut.
pub fn next(st: &State, task: &str, o: &NextOpts) -> String {
    let mut lv = NextLevel {
        detail: true,
        facts: 15,
        list_cap: None,
    };
    let mut text = build_next(st, task, o, lv);
    for step in 0..3 {
        if text.chars().count() <= o.max_chars {
            break;
        }
        match step {
            0 => lv.detail = false,
            1 => lv.facts = 0,
            _ => lv.list_cap = Some(LIST_CAP),
        }
        text = build_next(st, task, o, lv);
    }
    if text.chars().count() > o.max_chars {
        text.push_str(&format!(
            "\n(kontext: this is over {} characters; the task, its acceptance, the mark, and how to close it are never cut)\n",
            o.max_chars
        ));
    }
    text
}

fn build_next(st: &State, task: &str, o: &NextOpts, lv: NextLevel) -> String {
    let t = st.get(task).expect("the task exists");
    let epic = st.epic_of(t);
    let name = epic.map(epic_name).unwrap_or_else(|| "?".into());
    let info = st.task(task).cloned().unwrap_or_default();
    let mark = o.frontier.encode();
    let mut l = vec![format!("# Assignment: {} {}", t.id, t.title.trim())];
    let status = match &info.holder {
        Some(_) => holder_text(st, task),
        None => info.status.label().to_string(),
    };
    l.push(format!(
        "Epic {} (epic:{name}, {}) in {}. Status: {status}.",
        epic.map(|e| e.title.trim()).unwrap_or("?"),
        epic.map(|e| e.id.as_str()).unwrap_or("?"),
        o.project
    ));
    match &o.claimed {
        Some(c) => l.push(format!("You claimed it just now ({c}) as {}.", o.actor)),
        None if info.holder.is_none() => l.push(format!(
            "No one holds it yet: claim it with `kontext next --epic {name} --claim` before you start."
        )),
        None => {}
    }
    l.push(format!("Assignment mark: `{mark}`"));
    // Phase 5: one of your claims that lost to another machine.
    for a in lost_claims(st, &o.actor) {
        l.push(format!("Warning: {a}"));
    }
    l.push(String::new());
    l.push("## What to do".into());
    l.push(t.title.trim().to_string());
    if !t.body.trim().is_empty() {
        l.push(String::new());
        l.push(t.body.trim().to_string());
    }
    l.push(String::new());
    match field(t, "acceptance") {
        Some(a) => l.push(format!("- Acceptance: {}", a.trim())),
        None => l.push(
            "- Acceptance: (not stated; ask with `kontext ask … --task` if it isn't obvious)"
                .into(),
        ),
    }
    if let Some(f) = features::counted_feature(st, t).and_then(|f| st.get(f)) {
        let status = st.feature(&f.id).map(|i| i.status.label()).unwrap_or("?");
        let sees = features::acceptance(f).unwrap_or("(not stated)");
        l.push(format!(
            "- Feature: {} {} ({status}). When it works, the human sees: {sees}",
            f.id,
            f.title.trim()
        ));
    }
    let goals = string_list(t.fields.get("goals"));
    if goals.is_empty() {
        l.push("- Goals: none (drift: ask the coordinator which goal this serves)".into());
    }
    for g in &goals {
        l.push(format!("- Goal: {}", goal_label(st, g)));
    }
    for d in string_list(t.fields.get("depends_on")) {
        let s = st
            .task_status(&d)
            .map(|s| s.label())
            .unwrap_or("does not exist");
        let title = st
            .get(&d)
            .map(|x| one_line(&x.title, 80))
            .unwrap_or_default();
        l.push(format!("- Depends on: {d} {title} ({s})"));
    }

    if let Some(c) = st.charter().or_else(|| st.latest_charter()) {
        l.push(String::new());
        l.push(format!("## Charter ({})", c.id));
        if let Some(o) = field(c, "objective") {
            l.push(format!("- Objective: {}", o.trim()));
        }
        let non = string_list(c.fields.get("non_goals"));
        if !non.is_empty() {
            l.push(format!("- Non-goals: {}", non.join("; ")));
        }
    }

    if let Some(epic) = epic {
        let ds = epic_decisions(st, epic, Some(task));
        l.push(String::new());
        l.push(format!("## Decisions that touch it ({})", ds.len()));
        if ds.is_empty() {
            l.push("- (none)".into());
        }
        let items = ds
            .iter()
            .map(|d| {
                let why = if d.why.trim().is_empty() || !lv.detail {
                    String::new()
                } else {
                    format!(" (why: {})", one_line(&d.why, 160))
                };
                format!("- {} {}{why}", d.id, d.title.trim())
            })
            .collect();
        capped(&mut l, items, lv.list_cap, "ver `kontext next --json`");
        let fs = epic_facts(st, epic, Some(task));
        l.push(String::new());
        l.push(format!("## Epic facts ({})", fs.len()));
        if fs.is_empty() {
            l.push("- (none)".into());
        }
        let items = fs
            .iter()
            .rev()
            .map(|f| {
                format!(
                    "- {}{} {}",
                    f.id,
                    crate::facts::mark(st, &f.id),
                    one_line(&f.title, 160)
                )
            })
            .collect();
        capped(&mut l, items, Some(lv.facts), "ver `kontext next --json`");
    }

    let qa = task_answers(st, task);
    if !qa.is_empty() {
        l.push(String::new());
        l.push(format!("## Questions on this task ({})", qa.len()));
        let skip = lv.list_cap.map_or(0, |c| qa.len().saturating_sub(c));
        if skip > 0 {
            l.push(format!("- … {skip} older, see `kontext show {task}`"));
        }
        for (q, answers) in qa.into_iter().skip(skip) {
            let a = if answers.is_empty() {
                "no answer yet".to_string()
            } else {
                answers
                    .iter()
                    .map(|a| one_line(&a.body, 160))
                    .collect::<Vec<_>>()
                    .join(" / ")
            };
            l.push(format!("- {} {} → {a}", q.id, one_line(&q.title, 120)));
        }
    }

    let lease = tasks::duration_text(st.policy.lease_ms);
    l.push(String::new());
    l.push("## How to work".into());
    l.push(format!(
        "- Always run it as the task's holder, with arguments: `kontext --actor {} --role implementer <command>` (or tester). Put nothing before or around `kontext`: a `Bash(kontext:*)` permission doesn't cover variables in front, `cd … &&`, or pipes.",
        info.holder
            .as_ref()
            .map(|h| h.actor.clone())
            .unwrap_or_else(|| o.actor.clone())
    ));
    l.push(format!(
        "- Progress: `kontext log \"…\" --task {task}`. Renew your claim: with no journal entry or close within {lease}, it expires and the task goes back to ready."
    ));
    l.push(format!(
        "- Questions: if the charter or a decision answers it, keep going. If not, `kontext ask \"…\" --task {task}` (it goes to epic:{name}) and move on with what you can."
    ));
    l.push("- Don't decide: if something should change, `kontext propose`.".into());
    l.push(format!(
        "- Close: `kontext done {task} --run \"<command that tests the acceptance>\" --since {mark}`. Without evidence it doesn't close."
    ));
    l.push(
        "- If `done` rejects it for stale context (exit code 3), read what it lists (`kontext show <id>`), adjust, and retry with the new mark it gives you."
            .into(),
    );
    l.join("\n") + "\n"
}

/// `kontext next --json`.
pub fn next_json(st: &State, task: &str, o: &NextOpts) -> Value {
    let t = st.get(task).expect("the task exists");
    let epic = st.epic_of(t);
    let list =
        |v: Vec<&Entry>| -> Vec<Value> { v.into_iter().map(|e| entry_json(st, e)).collect() };
    let mut v = json!({
        "project": o.project,
        "task": task_json(st, t),
        "epic": epic.map(|e| entry_json(st, e)),
        "goals": string_list(t.fields.get("goals")).iter().filter_map(|g| st.get(g)).map(|g| entry_json(st, g)).collect::<Vec<_>>(),
        "decisions": epic.map(|e| list(epic_decisions(st, e, Some(task)))).unwrap_or_default(),
        "facts": epic.map(|e| list(epic_facts(st, e, Some(task)))).unwrap_or_default(),
        "mark": o.frontier.encode(),
        "claimed": o.claimed,
        "actor": o.actor,
        "lost_claims": lost_claims(st, &o.actor),
    });
    if let Some(f) = features::counted_feature(st, t).and_then(|f| st.get(f)) {
        v["feature"] = features::json(st, f);
    }
    v
}

/// The claims of `actor` that lost to an earlier claim from another machine.
fn lost_claims<'a>(st: &'a State, actor: &str) -> Vec<&'a crate::state::Alert> {
    st.alerts
        .iter()
        .filter(|a| matches!(a, crate::state::Alert::ClaimLost { loser, .. } if loser == actor))
        .collect()
}

/// Is this day-to-day task work that the pm doesn't see (DESIGN §8)?
pub fn task_level(e: &Entry) -> bool {
    match e.kind.as_str() {
        "task" | "fact" | "confirm" => true,
        // A feature's validation (or its reopening) is news for the pm.
        "done" | "reopen" if e.refs.first().is_some_and(|r| r.starts_with("U-")) => false,
        "assign" | "claim" | "release" | "done" | "reopen" => true,
        "journal" => field(e, "task").is_some(),
        // The coordinator sees questions on a task, or addressed to a coordinator.
        "question" => {
            field(e, "task").is_some() || field(e, "to").is_some_and(|t| t.starts_with("epic:"))
        }
        _ => false,
    }
}

/// Status of a task, for `show`.
pub fn task_status_text(st: &State, id: &str) -> Option<String> {
    let info = st.task(id)?;
    let mut s = info.status.label().to_string();
    match info.status {
        TaskStatus::Assigned | TaskStatus::Claimed => {
            s = holder_text(st, id);
        }
        TaskStatus::Blocked => s.push_str(&format!(" (waits for {})", info.blocked_by.join(", "))),
        TaskStatus::Closed => {
            if let Some(d) = &info.done {
                s.push_str(&format!(" ({d})"));
            }
        }
        TaskStatus::Ready => {
            if let Some(e) = &info.expired {
                s.push_str(&format!(" (the claim by {} expired)", e.actor));
            }
        }
    }
    if st.status_of(id) == Some(Status::Superseded) {
        s.push_str(", superseded");
    }
    Some(s)
}
