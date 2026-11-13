//! Views (DESIGN §8): `standup`, `resume` and `show`, in Markdown and JSON.
//!
//! Views write nothing: they get the state and what is new since the reader's
//! cursor, and the command decides whether the cursor advances.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{Value, json};

use crate::model::{Entry, is_record_kind};
use crate::state::{State, Status, string_list};

/// Default number of characters (DESIGN §9.1: Claude Code cuts at 10,000).
pub const DEFAULT_BUDGET: usize = 9000;

/// The English name of each kind.
pub fn kind_label(kind: &str) -> &str {
    match kind {
        "charter" => "charter",
        "goal" => "goal",
        "epic" => "epic",
        "decision" => "decision",
        "proposal" => "proposal",
        "question" => "question",
        "fact" => "fact",
        "task" => "task",
        "journal" => "journal",
        "feature" => "feature",
        "scope" => "scope signature",
        "sign" => "signature",
        "accept" => "acceptance",
        "reject" => "rejection",
        "answer" => "answer",
        "supersede" => "supersession",
        "assign" => "assignment",
        "claim" => "claim",
        "release" => "release",
        "done" => "close",
        "reopen" => "reopening",
        "confirm" => "confirmation",
        "dismiss" => "dismissal",
        other => other,
    }
}

/// Everything on one line, cut to `limit` characters.
pub fn one_line(s: &str, limit: usize) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= limit {
        return flat;
    }
    let cut: String = flat.chars().take(limit.saturating_sub(1)).collect();
    format!("{}…", cut.trim_end())
}

/// `2026-11-03 18:22` from an `hlc`.
pub fn when(hlc: &str) -> String {
    let s = hlc.get(..16).unwrap_or(hlc);
    s.replace('T', " ")
}

pub(crate) fn field<'a>(e: &'a Entry, k: &str) -> Option<&'a str> {
    e.fields
        .get(k)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
}

pub fn actor_text(e: &Entry) -> String {
    match field(e, "approved_by") {
        Some(a) => format!("{} (approved by {a})", e.actor.short()),
        None => e.actor.short(),
    }
}

pub(crate) fn unreviewed(e: &Entry) -> bool {
    e.actor.is_agent() && e.actor.name == "distill"
}

/// Who a question is for: its `to` (`owner` if it has none), without the
/// `human:` of `--to human:<name>`.
pub fn question_to(q: &Entry) -> &str {
    let to = field(q, "to").unwrap_or("owner");
    to.strip_prefix("human:").unwrap_or(to)
}

/// Is the question for this reader? `owner` includes every owner by name;
/// `human:<name>` is the same as `<name>`.
pub fn question_is_for(st: &State, q: &Entry, reader: &str) -> bool {
    let to = question_to(q);
    let reader = reader.strip_prefix("human:").unwrap_or(reader);
    if to == reader {
        return true;
    }
    let reader_is_owner = reader == "owner" || st.policy.is_owner(reader);
    (to == "owner" && reader_is_owner) || (reader == "owner" && st.policy.is_owner(to))
}

/// One line that explains what a record did (`sign`, `accept`…).
pub(crate) fn record_line(st: &State, r: &Entry) -> String {
    let target = r.refs.first().map(String::as_str).unwrap_or("?");
    let title = st
        .get(target)
        .map(|t| one_line(&t.title, 90))
        .unwrap_or_default();
    let by = actor_text(r);
    match r.kind.as_str() {
        "sign" => format!("{target} signed by {by}: {title}"),
        "accept" => match field(r, "decision_id") {
            Some(d) => format!("{target} accepted by {by} and became decision {d}: {title}"),
            None => format!("{target} accepted by {by}: {title}"),
        },
        "reject" => format!(
            "{target} rejected by {by}: {title} (why: {})",
            one_line(&r.why, 120)
        ),
        "answer" => format!(
            "{target} answered by {by}: {title} → {}",
            one_line(&r.body, 140)
        ),
        "supersede" => format!(
            "{target} superseded by {} ({by}): {title}",
            r.refs.get(1).map(String::as_str).unwrap_or("?")
        ),
        other => format!("{} of {target} by {by}", kind_label(other)),
    }
}

/// A line of a view: the `id` and its text (for `--json`).
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub id: String,
    pub kind: String,
    pub text: String,
}

impl Item {
    fn new(e: &Entry, text: String) -> Item {
        Item {
            id: e.id.clone(),
            kind: e.kind.clone(),
            text,
        }
    }

    fn md(&self) -> String {
        format!("- {} {}", self.id, self.text)
    }
}

/// `kontext standup`: what happened since your cursor and what waits for you.
#[derive(Serialize, Debug, Clone)]
pub struct Standup {
    pub project: String,
    pub reader: String,
    /// When your cursor last advanced (`None` the first time).
    pub since: Option<String>,
    pub new_entries: usize,
    pub decisions: Vec<Item>,
    pub closed: Vec<Item>,
    pub waiting: Vec<Item>,
    pub alerts: Vec<String>,
    pub journal: Vec<Item>,
    /// Other new entries by kind (facts, questions for others…).
    pub other: BTreeMap<String, usize>,
    /// With a signed plan: the scope change that waits for the owner.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_scope: Option<String>,
}

pub fn standup(
    st: &State,
    new: &[Entry],
    project: &str,
    reader: &str,
    since: Option<String>,
) -> Standup {
    let new_ids: std::collections::BTreeSet<&str> = new.iter().map(|e| e.id.as_str()).collect();
    let mut s = Standup {
        project: project.to_string(),
        reader: reader.to_string(),
        since,
        new_entries: new.len(),
        decisions: Vec::new(),
        closed: Vec::new(),
        waiting: Vec::new(),
        alerts: Vec::new(),
        journal: Vec::new(),
        other: BTreeMap::new(),
        pending_scope: crate::scope::pending_line(st),
    };
    for e in new {
        let counts = st.counts(&e.id);
        match e.kind.as_str() {
            "decision" if counts => {
                let mut text = format!("{} ({})", one_line(&e.title, 140), actor_text(e));
                if !e.why.trim().is_empty() {
                    text.push_str(&format!(" · why: {}", one_line(&e.why, 140)));
                }
                if let Some(by) = st.superseded_by.get(&e.id) {
                    text.push_str(&format!(" · already superseded by {by}"));
                }
                s.decisions.push(Item::new(e, text));
            }
            "journal" => {
                let mut text = format!("{}: {}", e.actor.short(), one_line(&e.title, 160));
                if let Some(n) = field(e, "next") {
                    text.push_str(&format!(" → next: {}", one_line(n, 100)));
                }
                s.journal.push(Item::new(e, text));
            }
            "done" | "reopen" if counts => {
                let target = e.refs.first().cloned().unwrap_or_default();
                let title = st
                    .get(&target)
                    .map(|t| one_line(&t.title, 120))
                    .unwrap_or_default();
                let feature = st.kind_of(&target) == Some("feature");
                let text = if e.kind == "done" && feature {
                    format!(
                        "feature validated by {}: {title} · evidence: {}",
                        e.actor.short(),
                        crate::tasks::evidence_summary(e.fields.get("evidence"))
                    )
                } else if feature {
                    format!(
                        "feature validation reopened by {}: {title} (why: {})",
                        e.actor.short(),
                        one_line(&e.why, 120)
                    )
                } else if e.kind == "done" {
                    format!(
                        "task closed by {}: {title} · evidence: {}",
                        e.actor.short(),
                        crate::tasks::evidence_summary(e.fields.get("evidence"))
                    )
                } else {
                    format!(
                        "task reopened by {}: {title} (why: {})",
                        e.actor.short(),
                        one_line(&e.why, 120)
                    )
                };
                s.closed.push(Item {
                    id: target,
                    kind: e.kind.clone(),
                    text,
                });
            }
            k if is_record_kind(k) => {
                if counts && matches!(k, "sign" | "accept" | "reject" | "answer" | "supersede") {
                    let target = e.refs.first().cloned().unwrap_or_default();
                    s.closed.push(Item {
                        id: target,
                        kind: e.kind.clone(),
                        text: record_line(st, e)
                            .split_once(' ')
                            .map(|x| x.1.to_string())
                            .unwrap_or_default(),
                    });
                } else if counts {
                    *s.other.entry(kind_label(k).to_string()).or_default() += 1;
                }
            }
            // Proposals, questions, charters and goals go to "waiting for your
            // answer" while they are still open.
            "proposal" | "question" | "charter" | "goal" => {
                let waiting = match e.kind.as_str() {
                    "question" => {
                        st.status_of(&e.id) == Some(Status::Open) && question_is_for(st, e, reader)
                    }
                    "proposal" => st.status_of(&e.id) == Some(Status::Pending),
                    _ => st.status_of(&e.id) == Some(Status::Unsigned),
                };
                if !waiting {
                    *s.other.entry(kind_label(&e.kind).to_string()).or_default() += 1;
                }
            }
            k => {
                if counts {
                    *s.other.entry(kind_label(k).to_string()).or_default() += 1;
                }
            }
        }
    }
    // What waits for an answer is "new" if it arrived after the last thing the
    // reader wrote (seeing it does not count as read); if the reader never
    // wrote, its cursor is used.
    let first_time = s.since.is_none();
    let last_action = crate::inbox::last_action(st, reader);
    let is_new = |e: &Entry| {
        let new = match &last_action {
            Some(l) => e.hlc.as_str() > l.as_str(),
            None => !first_time && new_ids.contains(e.id.as_str()),
        };
        if new { " (new)" } else { "" }
    };
    // What the human already dismissed from his inbox no longer waits for him.
    let dismissed = |e: &Entry| st.inbox_closed.contains_key(&e.id);
    for c in st.unsigned_charters().into_iter().filter(|c| !dismissed(c)) {
        s.waiting.push(Item::new(
            c,
            format!(
                "unsigned charter{}: {} → `kontext sign {}`",
                is_new(c),
                one_line(&c.title, 140),
                c.id
            ),
        ));
    }
    for g in st
        .with_status("goal", &[Status::Unsigned])
        .into_iter()
        .filter(|g| !dismissed(g))
    {
        s.waiting.push(Item::new(
            g,
            format!(
                "unsigned goal{}: {} → `kontext sign {}`",
                is_new(g),
                one_line(&g.title, 140),
                g.id
            ),
        ));
    }
    for q in st.open_questions() {
        if question_is_for(st, q, reader) && !dismissed(q) {
            let body = if q.body.trim().is_empty() {
                String::new()
            } else {
                format!(": {}", one_line(&q.body, 120))
            };
            s.waiting.push(Item::new(
                q,
                format!(
                    "question{} from {}: {}{body} → `kontext answer {} \"…\"`",
                    is_new(q),
                    q.actor.short(),
                    one_line(&q.title, 140),
                    q.id
                ),
            ));
        }
    }
    for p in st.pending_proposals().into_iter().filter(|p| !dismissed(p)) {
        // A user rule already applies: what waits is the signature of the charter.
        let (flag, what) = if user_rule(p) {
            (
                "",
                "user rule (already in force; still needs signing for the charter)",
            )
        } else if unreviewed(p) {
            ("[not reviewed] ", "proposal")
        } else {
            ("", "proposal")
        };
        s.waiting.push(Item::new(
            p,
            format!(
                "{flag}{what}{} from {}: {} → `kontext accept {}` or `kontext reject {} --why …`",
                is_new(p),
                p.actor.short(),
                one_line(&p.title, 140),
                p.id,
                p.id
            ),
        ));
    }
    for a in &st.alerts {
        let touches_new = a.ids().iter().any(|id| new_ids.contains(id));
        if a.is_standing() || touches_new {
            s.alerts.push(a.to_string());
        }
    }
    s
}

pub(crate) fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

impl Standup {
    pub fn summary(&self) -> String {
        format!(
            "Summary: {}, {}, {}, {} ({}).",
            plural(self.decisions.len(), "new decision", "new decisions"),
            plural(self.closed.len(), "closed item", "closed items"),
            plural(
                self.waiting.len(),
                "item waiting for your answer",
                "items waiting for your answer"
            ),
            plural(self.alerts.len(), "alert", "alerts"),
            plural(self.new_entries, "new entry", "new entries"),
        )
    }

    pub fn markdown(&self) -> String {
        let mut l = vec![format!("# Standup of {} for {}", self.project, self.reader)];
        if let Some(p) = &self.pending_scope {
            l.push(p.clone());
        }
        let since = match &self.since {
            Some(d) => format!("since your last time ({d})"),
            None => "since the beginning (this is your first standup)".to_string(),
        };
        if self.new_entries == 0 {
            l.push(String::new());
            l.push(format!("Nothing new {since}."));
            l.push(String::new());
            if self.waiting.is_empty() {
                l.push("Nothing is waiting for your answer.".into());
            } else {
                l.push(format!(
                    "## Still waiting for your answer ({})",
                    self.waiting.len()
                ));
                l.extend(self.waiting.iter().map(Item::md));
            }
            if !self.alerts.is_empty() {
                l.push(String::new());
                l.push(format!("## Alerts ({})", self.alerts.len()));
                l.extend(self.alerts.iter().map(|a| format!("- {a}")));
            }
            l.push(String::new());
            l.push(self.summary());
            return l.join("\n") + "\n";
        }
        l.push(format!("What's new {since}."));
        let section = |l: &mut Vec<String>, title: &str, items: &[Item], cap: usize| {
            if items.is_empty() {
                return;
            }
            l.push(String::new());
            l.push(format!("## {title} ({})", items.len()));
            l.extend(items.iter().take(cap).map(Item::md));
            if items.len() > cap {
                l.push(format!(
                    "- … {} more, see `kontext show <id>`",
                    items.len() - cap
                ));
            }
        };
        section(&mut l, "New decisions", &self.decisions, usize::MAX);
        section(&mut l, "Closed", &self.closed, usize::MAX);
        section(&mut l, "Waiting for your answer", &self.waiting, usize::MAX);
        if !self.alerts.is_empty() {
            l.push(String::new());
            l.push(format!("## Alerts ({})", self.alerts.len()));
            l.extend(self.alerts.iter().map(|a| format!("- {a}")));
        }
        section(&mut l, "Progress (journal)", &self.journal, 10);
        if !self.other.is_empty() {
            let parts: Vec<String> = self
                .other
                .iter()
                .map(|(k, n)| format!("{k}: {n}"))
                .collect();
            l.push(String::new());
            l.push(format!("Also: {}.", parts.join(", ")));
        }
        l.push(String::new());
        l.push(self.summary());
        l.join("\n") + "\n"
    }
}

/// What `resume` needs besides the state.
#[derive(Debug, Clone)]
pub struct ResumeOpts {
    pub project: String,
    pub role: String,
    pub max_chars: usize,
    /// Generation date and time (`2026-11-03 18:22 UTC`).
    pub generated: String,
    /// Result of `verify`: `None` if the chain is fine, or the problem.
    pub chain_problem: Option<String>,
    pub entries: usize,
    /// Who writes according to the environment (`$KONTEXT_ACTOR`), if set.
    pub writer: Option<String>,
    /// Is the distill on? Then there is no need to note what the user said or
    /// the progress.
    pub distill: bool,
}

/// What to write and what not to when the distill is on: it already saves what
/// the user said and the progress (in long-memory, agents spent a model turn on
/// every `kontext propose`/`kontext log` that repeated it).
pub const DISTILL_HINT: &str = "The kontext distill already saves on its own, when this session \
    closes or compacts, what the user said, decided or asked, and the progress: do not run \
    `kontext propose` or `kontext log` to repeat that. Use kontext to look things up (`kontext show \
    <id>`, `kontext recall <topic>`), to ask the human (`kontext ask`), and for what the distill \
    cannot take from the conversation (a finding in the code with `kontext fact --path`, or your \
    own proposal to change a decision).";

/// How to invoke `kontext` so a `Bash(kontext:*)` permission in Claude Code
/// covers it: a standalone command that starts with `kontext`. With variables
/// in front, `export`, `cd … &&`, pipes or subshells, Claude Code asks for
/// approval, and in a session with nobody there (`claude -p`) the command does
/// not run.
pub fn invoke_hint(writer: Option<&str>) -> String {
    let rule = "Invoke kontext as a standalone command that starts with `kontext`: no `VAR=…` or \
                `export` in front, no `cd … &&`, no pipes (`| head`, `| sed`) or `( … )`; that way \
                a `Bash(kontext:*)` permission covers it and it does not wait for approval.";
    match writer {
        Some(w) => format!("You write as {w} (already in the environment). {rule}"),
        None => format!(
            "{rule} Your identity goes in the arguments: `kontext --actor agent:<your-name> --role <role> log …`."
        ),
    }
}

/// How much is shown at each level of cutting (DESIGN §8).
#[derive(Debug, Clone, Copy)]
struct Level {
    journal: usize,
    /// Without what was superseded or what does not count, in "What's new".
    new_current_only: bool,
    new_cap: Option<usize>,
    decision_detail: bool,
    facts: Option<usize>,
    list_cap: Option<usize>,
    /// How many of "What the user said" (the most recent).
    user_cap: Option<usize>,
    /// The last cut, only when everything above is not enough: how many of
    /// the decisions in force are listed (`None`: all of them).
    decisions: Option<DecisionCut>,
}

/// How many titles of the decisions in force are listed: the `shown` newest,
/// with a notice of how many are not and, with `by_tag`, how many of each
/// first tag.
#[derive(Debug, Clone, Copy)]
pub(crate) struct DecisionCut {
    pub shown: usize,
    pub by_tag: bool,
}

/// At most this many tags in the count of what is not shown.
const HIDDEN_TAGS_N: usize = 10;

const JOURNAL_N: usize = 10;
const FACTS_N: usize = 15;
const LIST_CAP: usize = 5;
const NEW_N: usize = 30;

/// Did the user say it? (`fields.origin = "user"`, the distill marks it.)
pub(crate) fn from_user(e: &Entry) -> bool {
    field(e, "origin") == Some("user")
}

/// Is it a rule or instruction the user gave (not a decision that needs an
/// owner's signature)? The distill marks it in `fields.user_rule`.
pub(crate) fn user_rule(e: &Entry) -> bool {
    from_user(e) && e.fields.get("user_rule").and_then(Value::as_bool) == Some(true)
}

/// What the user said and still applies: active facts and pending proposals
/// with user origin, in order. What is already law shows up in the decisions.
pub fn user_said(st: &State) -> Vec<&Entry> {
    st.order
        .iter()
        .filter_map(|id| st.get(id))
        .filter(|e| from_user(e))
        .filter(|e| match e.kind.as_str() {
            "fact" => {
                crate::facts::is_live(st, &e.id) && st.status_of(&e.id) == Some(Status::Active)
            }
            "proposal" => st.status_of(&e.id) == Some(Status::Pending),
            _ => false,
        })
        .collect()
}

/// The mark of a user rule that is not superseded: it applies even when it is
/// not signed (signing is for the charter, not for believing the user).
pub(crate) const USER_RULE_MARK: &str = "[user rule, in force]";

/// A line of "What the user said": the value in force first and, if it
/// supersedes another, the old one at the end, marked as no longer applying.
fn user_line(st: &State, e: &Entry) -> String {
    let date = field(e, "said_at")
        .map(|d| one_line(d, 16))
        .unwrap_or_else(|| when(&e.hlc));
    let kind = match e.kind.as_str() {
        "proposal" if user_rule(e) => format!(" {USER_RULE_MARK}"),
        "proposal" => " [proposal]".into(),
        _ => String::new(),
    };
    let old = string_list(e.fields.get("supersedes"))
        .into_iter()
        .filter(|o| st.superseded_by.get(o).map(String::as_str) == Some(e.id.as_str()))
        .map(|o| match st.get(&o).map(|x| one_line(&x.title, 120)) {
            Some(t) if !t.is_empty() => format!("{o} «{t}»"),
            _ => o,
        })
        .collect::<Vec<_>>();
    let upd = if old.is_empty() {
        String::new()
    } else {
        format!(" (supersedes {}, which no longer applies)", old.join("; "))
    };
    format!("- {} ({date}){kind} {}{upd}", e.id, one_line(&e.title, 200))
}

/// The facts a role sees: for the pm, those that do not belong to an epic; the
/// others, all of them. Without the user's, which have their own section.
fn facts_for(st: &State, pm: bool) -> Vec<&Entry> {
    st.live_facts()
        .into_iter()
        .filter(|f| !from_user(f))
        .filter(|f| !pm || field(f, "epic").is_none())
        .collect()
}

pub(crate) fn questions_for<'a>(st: &'a State, role: &str) -> Vec<&'a Entry> {
    st.open_questions()
        .into_iter()
        .filter(|q| question_is_for(st, q, role))
        .collect()
}

pub(crate) fn capped(l: &mut Vec<String>, items: Vec<String>, cap: Option<usize>, more: &str) {
    let n = items.len();
    let cap = cap.unwrap_or(usize::MAX);
    l.extend(items.into_iter().take(cap));
    if n > cap {
        l.push(format!("- … {} more, {more}", n - cap));
    }
}

pub(crate) fn new_line(st: &State, e: &Entry) -> String {
    if is_record_kind(&e.kind) {
        return format!("- {}", record_line(st, e));
    }
    let mark = match st.status_of(&e.id) {
        Some(Status::Ignored) => " [not counted]".to_string(),
        Some(Status::Superseded) => match st.superseded_by.get(&e.id) {
            Some(by) => format!(" [superseded by {by}, no longer applies]"),
            None => " [superseded, no longer applies]".to_string(),
        },
        _ => String::new(),
    };
    format!(
        "- {} {}{mark}: {} ({})",
        e.id,
        kind_label(&e.kind),
        one_line(&e.title, 140),
        e.actor.short()
    )
}

pub(crate) fn charter_lines(st: &State) -> Vec<String> {
    let mut l = vec!["## Charter".to_string()];
    let Some(c) = st.charter().or_else(|| st.latest_charter()) else {
        l.push("No charter. Create one with `kontext charter --objective …`, and have an owner sign it with `kontext sign <id>`.".into());
        return l;
    };
    let status = st.status_of(&c.id).map(|s| s.label()).unwrap_or("?");
    l.push(format!("{} [{status}] {}", c.id, c.title.trim()));
    if let Some(o) = field(c, "objective") {
        l.push(format!("- Objective: {}", o.trim()));
    }
    let non = string_list(c.fields.get("non_goals"));
    if !non.is_empty() {
        l.push(format!("- Not doing: {}", non.join("; ")));
    }
    if let Some(d) = field(c, "done_when") {
        l.push(format!("- Done when: {}", d.trim()));
    }
    for u in st.unsigned_charters().into_iter().filter(|u| u.id != c.id) {
        l.push(format!(
            "- Another unsigned charter: {} {} (`kontext show {}`)",
            u.id,
            one_line(&u.title, 120),
            u.id
        ));
    }
    l
}

pub(crate) fn goal_lines(st: &State) -> Vec<String> {
    let goals = st.goals();
    let mut l = vec![format!("## Goals ({})", goals.len())];
    if goals.is_empty() {
        l.push("- (none)".into());
    }
    for g in goals {
        let mark = if st.status_of(&g.id) == Some(Status::Unsigned) {
            " [unsigned]"
        } else {
            ""
        };
        let done = field(g, "done_when")
            .map(|d| format!(" (done when: {})", d.trim()))
            .unwrap_or_default();
        l.push(format!("- {}{mark} {}{done}", g.id, g.title.trim()));
    }
    l
}

/// The first tag of a decision, which groups it in `resume`.
fn first_tag(d: &Entry) -> (bool, String) {
    let tag = d.tags.first().cloned();
    (tag.is_none(), tag.unwrap_or_else(|| "no tag".into()))
}

/// The line that says a list of decisions in force is partial (`detail` goes
/// after "not shown", e.g. the count by tag), with how to reach the rest.
pub(crate) fn partial_notice(hidden: usize, detail: &str) -> String {
    format!(
        "- … {} in force not shown{detail}. This list is partial: before you propose or decide \
         something, search whether it is already decided with `kontext recall <words>`; read one \
         with `kontext show <id>`.",
        plural(hidden, "older decision", "older decisions")
    )
}

/// The largest `n` in `0..=total` whose text fits in `max` characters, and
/// that text; `None` if not even `n = 0` fits. It searches by halves, so a
/// text that is not shorter with a smaller `n` may leave a few out; what it
/// returns always fits.
pub(crate) fn largest_fitting(
    total: usize,
    max: usize,
    mut build: impl FnMut(usize) -> String,
) -> Option<String> {
    let fits = |t: &str| t.chars().count() <= max;
    let mut best = Some(build(0)).filter(|t| fits(t))?;
    let (mut lo, mut hi) = (0, total);
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let t = build(mid);
        if fits(&t) {
            lo = mid;
            best = t;
        } else {
            hi = mid - 1;
        }
    }
    Some(best)
}

pub(crate) fn decision_lines(st: &State, detail: bool, cut: Option<DecisionCut>) -> Vec<String> {
    let mut law = st.law();
    let mut l = vec![format!("## Decisions in force ({})", law.len())];
    if law.is_empty() {
        l.push("- (none)".into());
    }
    // The newest stay: `law` is in ledger order.
    if let Some(cut) = cut {
        let hidden: Vec<&Entry> = law.drain(..law.len().saturating_sub(cut.shown)).collect();
        if !hidden.is_empty() {
            let mut detail = String::new();
            if cut.by_tag {
                let mut n: BTreeMap<(bool, String), usize> = BTreeMap::new();
                for d in &hidden {
                    *n.entry(first_tag(d)).or_default() += 1;
                }
                let mut n: Vec<((bool, String), usize)> = n.into_iter().collect();
                n.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
                let mut parts: Vec<String> = n
                    .iter()
                    .take(HIDDEN_TAGS_N)
                    .map(|((_, tag), k)| format!("{tag} {k}"))
                    .collect();
                if n.len() > HIDDEN_TAGS_N {
                    parts.push(format!(
                        "{} more",
                        plural(n.len() - HIDDEN_TAGS_N, "tag", "tags")
                    ));
                }
                detail = format!(" (by first tag: {})", parts.join(", "));
            }
            l.push(partial_notice(hidden.len(), &detail));
        }
    }
    let mut groups: BTreeMap<(bool, String), Vec<&Entry>> = BTreeMap::new();
    for d in law {
        groups.entry(first_tag(d)).or_default().push(d);
    }
    for ((_, tag), ds) in groups {
        if !detail {
            let titles: Vec<String> = ds
                .iter()
                .map(|d| format!("{} {}", d.id, d.title.trim()))
                .collect();
            l.push(format!("- **{tag}**: {}", titles.join("; ")));
            continue;
        }
        l.push(format!("### {tag}"));
        for d in ds {
            let why = if d.why.trim().is_empty() {
                String::new()
            } else {
                format!(" (why: {})", one_line(&d.why, 160))
            };
            l.push(format!("- {} {}{why}", d.id, d.title.trim()));
        }
    }
    l
}

fn build_resume(st: &State, new: &[Entry], o: &ResumeOpts, lv: Level) -> String {
    let mut l = vec![format!(
        "# Context of {} for {} (kontext resume)",
        o.project, o.role
    )];
    let chain = match &o.chain_problem {
        None => "chain OK".to_string(),
        Some(p) => format!("CHAIN BROKEN ({p}): run `kontext verify` and stop"),
    };
    l.push(format!(
        "Generated {}. Ledger: {}, {chain}.",
        o.generated,
        plural(o.entries, "entry", "entries")
    ));
    if o.role == "pm" {
        l.extend(crate::scope::pm_line(st));
        let n = crate::inbox::build_pm(st, &o.project).items.len();
        if n > 0 && crate::scope::pm_acts_alone(st) {
            l.push(format!(
                "Your inbox: {} (proposals, what the distill noted, problems), yours to resolve \
                 without the owner: `kontext inbox --as pm`.",
                plural(n, "pending item", "pending items")
            ));
        }
    }
    l.extend(crate::scope::pending_line(st));
    l.push(String::new());
    // With a signed plan the pm decides inside the scope (its line above).
    let others = if o.role == "pm" && crate::scope::pm_acts_alone(st) {
        "Other agents"
    } else {
        "Agents"
    };
    if o.distill {
        l.push(format!(
            "{DISTILL_HINT} {others} do not decide. What the user said applies even when unsigned; \
             what was superseded no longer applies."
        ));
    } else {
        l.push(format!(
            "The conversation is disposable: what is not in kontext is lost. {others} propose \
             (`kontext propose`), they do not decide. What the user said applies even when unsigned; \
             what was superseded no longer applies. Details of any id: `kontext show <id>`."
        ));
    }
    l.push(invoke_hint(o.writer.as_deref()));
    l.push(String::new());
    l.extend(charter_lines(st));
    l.push(String::new());
    l.extend(goal_lines(st));
    l.push(String::new());
    l.extend(decision_lines(st, lv.decision_detail, lv.decisions));
    l.push(String::new());
    l.extend(crate::board::epic_lines(st));
    // The pm sees one line per epic, not the tasks or facts of the epics
    // (DESIGN §8).
    let pm = o.role == "pm";

    let said = user_said(st);
    if !said.is_empty() {
        l.push(String::new());
        l.push(format!("## What the user said ({})", said.len()));
        l.push(
            "The user said this in the conversation (the distill noted it, with the date). It \
             applies now, even when unsigned: the signature is for the charter, not for believing \
             the user. Each line is what is in force; a line that says \"supersedes …, which no \
             longer applies\" is the old value: do not present it as current. A [user rule, in \
             force] applies as stated; if it contradicts a signed decision in force, the decision \
             wins and you raise it with `kontext ask`. The [proposal] items (scope, architecture) \
             are still proposals: an owner turns them into decisions with `kontext accept <id>`."
                .into(),
        );
        let from = said.len().saturating_sub(lv.user_cap.unwrap_or(usize::MAX));
        if from > 0 {
            l.push(format!(
                "- … {} older, see `kontext resume --as {} --json`",
                from, o.role
            ));
        }
        l.extend(said[from..].iter().map(|e| user_line(st, e)));
    }
    // What is already above is not repeated in "What's new".
    let above: std::collections::BTreeSet<&str> = said.iter().map(|e| e.id.as_str()).collect();

    let more = format!("see `kontext resume --as {} --json`", o.role);
    let qs = questions_for(st, &o.role);
    l.push(String::new());
    l.push(format!("## Questions for {} ({})", o.role, qs.len()));
    if qs.is_empty() {
        l.push("- (none)".into());
    }
    let items = qs
        .iter()
        .map(|q| {
            let body = if q.body.trim().is_empty() || lv.list_cap.is_some() {
                String::new()
            } else {
                format!(": {}", one_line(&q.body, 160))
            };
            format!(
                "- {} {}{body} (from {})",
                q.id,
                one_line(&q.title, 160),
                q.actor.short()
            )
        })
        .collect();
    capped(&mut l, items, lv.list_cap, &more);

    let ps: Vec<&Entry> = st
        .pending_proposals()
        .into_iter()
        .filter(|p| !from_user(p))
        .collect();
    l.push(String::new());
    l.push(format!("## Pending proposals ({})", ps.len()));
    if ps.is_empty() {
        l.push("- (none)".into());
    }
    let items = ps
        .iter()
        .map(|p| {
            let flag = if unreviewed(p) { "[not reviewed] " } else { "" };
            let why = if p.why.trim().is_empty() || !lv.decision_detail {
                String::new()
            } else {
                format!(" (why: {})", one_line(&p.why, 120))
            };
            format!(
                "- {} {flag}{} from {}{why}",
                p.id,
                one_line(&p.title, 160),
                p.actor.short()
            )
        })
        .collect();
    capped(&mut l, items, lv.list_cap, &more);

    let facts = facts_for(st, pm);
    l.push(String::new());
    l.push(format!("## Recent facts ({})", facts.len()));
    if facts.is_empty() {
        l.push("- (none)".into());
    }
    let shown: Vec<String> = facts
        .iter()
        .rev()
        .map(|f| {
            let flag = if unreviewed(f) { " [not reviewed]" } else { "" };
            format!(
                "- {}{}{flag} {}",
                f.id,
                crate::facts::mark(st, &f.id),
                one_line(&f.title, 160)
            )
        })
        .collect();
    capped(
        &mut l,
        shown,
        Some(lv.facts.unwrap_or(FACTS_N)),
        "see `kontext recall <topic>`",
    );

    let journal: Vec<&Entry> = st
        .journal()
        .into_iter()
        .filter(|j| !pm || !crate::board::task_level(j))
        .collect();
    let from = journal.len().saturating_sub(lv.journal);
    l.push(String::new());
    l.push(format!(
        "## Journal (last {} of {})",
        journal.len() - from,
        journal.len()
    ));
    if journal.len() == from {
        l.push("- (nothing)".into());
    }
    for j in &journal[from..] {
        let task = field(j, "task")
            .map(|t| format!(" [task {t}]"))
            .unwrap_or_default();
        let next = field(j, "next")
            .map(|n| format!(" → next: {}", one_line(n, 100)))
            .unwrap_or_default();
        l.push(format!(
            "- {} {}{task} {}{next}",
            when(&j.hlc),
            j.actor.short(),
            one_line(&j.title, 160)
        ));
    }

    // The pm does not see facts, nor the ones that went stale (each coordinator sees them).
    let alerts: Vec<&crate::state::Alert> = st
        .alerts
        .iter()
        .filter(|a| !pm || !matches!(a, crate::state::Alert::FactStale { .. }))
        .collect();
    if !alerts.is_empty() {
        l.push(String::new());
        l.push(format!("## Alerts ({})", alerts.len()));
        let items = alerts.iter().map(|a| format!("- {a}")).collect();
        capped(&mut l, items, lv.list_cap, "see `kontext standup`");
    }

    l.push(String::new());
    l.push(format!(
        "## What's new since your last time ({})",
        new.len()
    ));
    if new.is_empty() {
        l.push("- (nothing)".into());
    }
    let (hidden, shown): (Vec<&Entry>, Vec<&Entry>) =
        new.iter().partition(|e| pm && crate::board::task_level(e));
    let current = |e: &&Entry| {
        !lv.new_current_only
            || !matches!(
                st.status_of(&e.id),
                Some(Status::Superseded | Status::Ignored | Status::Rejected)
            )
    };
    let shown: Vec<&Entry> = shown
        .into_iter()
        .filter(current)
        .filter(|e| !above.contains(e.id.as_str()))
        .collect();
    let items = shown.iter().map(|e| new_line(st, e)).collect();
    capped(
        &mut l,
        items,
        Some(lv.new_cap.unwrap_or(NEW_N)),
        "see `kontext standup --peek`",
    );
    if !hidden.is_empty() {
        l.push(format!(
            "- … and {} from the epics' work (tasks, task journal, facts): each coordinator sees them with `kontext brief <epic>`",
            plural(hidden.len(), "entry", "entries")
        ));
    }
    l.join("\n") + "\n"
}

/// `kontext resume --as <role>`, cut to the budget by priority (DESIGN §8):
/// first the journal goes, then what was superseded and what is new, the facts,
/// the reasons of the decisions, the lists of what is open and at the end the
/// oldest of what the user said. If that is not enough, the list of decisions
/// in force gets what is left: its newest titles, with a line that says how
/// many older ones are not shown and how to search them. The charter and the
/// goals are never cut; if it still does not fit, that is said at the end.
pub fn resume(st: &State, new: &[Entry], o: &ResumeOpts) -> String {
    let mut lv = Level {
        journal: JOURNAL_N,
        new_current_only: false,
        new_cap: None,
        decision_detail: true,
        facts: None,
        list_cap: None,
        user_cap: None,
        decisions: None,
    };
    let fits = |t: &str| t.chars().count() <= o.max_chars;
    let mut text = build_resume(st, new, o, lv);
    while !fits(&text) && lv.journal > 0 {
        lv.journal -= 1;
        text = build_resume(st, new, o, lv);
    }
    let steps: [fn(&mut Level); 9] = [
        |l| l.new_current_only = true,
        |l| l.new_cap = Some(10),
        |l| l.new_cap = Some(0),
        |l| l.facts = Some(5),
        |l| l.facts = Some(0),
        |l| l.decision_detail = false,
        |l| l.list_cap = Some(LIST_CAP),
        |l| l.user_cap = Some(15),
        |l| l.user_cap = Some(5),
    ];
    for step in steps {
        if fits(&text) {
            break;
        }
        step(&mut lv);
        text = build_resume(st, new, o, lv);
    }
    if fits(&text) {
        return text;
    }
    // A long ledger of decisions: the rest keeps its room, the decisions get
    // what is left. The count by tag goes first, if it fits.
    let total = st.law().len();
    for by_tag in [true, false] {
        let found = largest_fitting(total, o.max_chars, |shown| {
            lv.decisions = Some(DecisionCut { shown, by_tag });
            build_resume(st, new, o, lv)
        });
        if let Some(t) = found {
            return t;
        }
    }
    // Not even the rest fits: the shorter of all the titles and none.
    lv.decisions = Some(DecisionCut {
        shown: 0,
        by_tag: false,
    });
    let none = build_resume(st, new, o, lv);
    let without = if none.chars().count() < text.chars().count() {
        text = none;
        " even without the titles of the decisions in force"
    } else {
        ""
    };
    text.push_str(&format!(
        "\n(kontext: this is over {} characters{without}; the charter and the goals are never cut)\n",
        o.max_chars
    ));
    text
}

/// What [`session_delta`] needs.
#[derive(Debug, Clone)]
pub struct DeltaOpts {
    pub role: String,
    /// Result of `verify`: `None` if the chain is fine, or the problem.
    pub chain_problem: Option<String>,
}

/// What the start hook puts in a `resume` of a session it already gave its
/// context (DESIGN §9.4): only what entered the ledger since then, without the
/// fixed lines. `None` if there is nothing to say.
///
/// The user's items come out as in "What the user said" (with what they
/// supersede and no longer apply); the rest as in "What's new" (signatures,
/// acceptances and supersessions say which entry they touch). The pm does not
/// see the epics' work, only how many entries there are.
pub fn session_delta(st: &State, new: &[Entry], o: &DeltaOpts) -> Option<String> {
    let pm = o.role == "pm";
    let said: std::collections::BTreeSet<&str> =
        user_said(st).into_iter().map(|e| e.id.as_str()).collect();
    // As in `resume`: the pm does see what the user said and the facts that do
    // not belong to an epic.
    let (hidden, shown): (Vec<&Entry>, Vec<&Entry>) = new.iter().partition(|e| {
        pm && crate::board::task_level(e)
            && !said.contains(e.id.as_str())
            && !(e.kind == "fact" && field(e, "epic").is_none())
    });
    let mut items: Vec<String> = shown
        .iter()
        .map(|e| {
            if said.contains(e.id.as_str()) {
                user_line(st, e)
            } else {
                new_line(st, e)
            }
        })
        .collect();
    if !hidden.is_empty() {
        items.push(format!(
            "- … and {} from the epics' work: each coordinator sees them with `kontext brief <epic>`",
            plural(hidden.len(), "entry", "entries")
        ));
    }
    let chain = o
        .chain_problem
        .as_ref()
        .map(|p| format!("CHAIN BROKEN ({p}): run `kontext verify` and stop."));
    if items.is_empty() && chain.is_none() {
        return None;
    }
    let mut l = vec![format!(
        "# kontext: what's new since the last context of this session ({})",
        new.len()
    )];
    l.extend(chain);
    if !items.is_empty() {
        l.push(
            "The rest of that context still holds; what here says \"no longer applies\" has been superseded."
                .into(),
        );
        l.extend(items);
    }
    Some(l.join("\n") + "\n")
}

pub(crate) fn entry_json(st: &State, e: &Entry) -> Value {
    let mut v = json!({
        "id": e.id,
        "kind": e.kind,
        "title": e.title,
        "why": e.why,
        "body": e.body,
        "tags": e.tags,
        "actor": e.actor.short(),
        "hlc": e.hlc,
        "status": st.status_of(&e.id),
        "fields": e.fields,
    });
    if let Some(fr) = st.freshness.get(&e.id) {
        v["freshness"] = json!(fr);
    }
    // What the user said applies even when not signed, as long as nothing
    // supersedes it.
    if from_user(e) {
        v["in_force"] = json!(match st.status_of(&e.id) {
            Some(Status::Active | Status::Accepted) => true,
            Some(Status::Pending) => user_rule(e),
            _ => false,
        });
    }
    v
}

/// `kontext resume --json`: the same, without cutting.
pub fn resume_json(st: &State, new: &[Entry], o: &ResumeOpts) -> Value {
    let pm = o.role == "pm";
    let list = |v: Vec<&Entry>| -> Vec<Value> {
        v.into_iter()
            .filter(|e| !pm || !crate::board::task_level(e))
            .map(|e| entry_json(st, e))
            .collect()
    };
    let epics: Vec<Value> = st
        .epics()
        .into_iter()
        .map(|e| {
            json!({
                "id": e.id,
                "name": crate::tasks::epic_name(e),
                "title": e.title,
                "line": crate::board::epic_line(st, e),
                "counts": crate::board::counts(st, &e.id)
                    .into_iter()
                    .map(|(s, n)| (s.label().to_string(), json!(n)))
                    .collect::<serde_json::Map<String, Value>>(),
            })
        })
        .collect();
    let mut v = json!({
        "epics": epics,
        "project": o.project,
        "role": o.role,
        "generated": o.generated,
        "chain_ok": o.chain_problem.is_none(),
        "chain_problem": o.chain_problem,
        "entries": o.entries,
        "charter": st.charter().or_else(|| st.latest_charter()).map(|c| entry_json(st, c)),
        "goals": list(st.goals()),
        "decisions": list(st.law()),
        "questions": list(questions_for(st, &o.role)),
        "user_said": user_said(st).into_iter().map(|e| entry_json(st, e)).collect::<Vec<_>>(),
        "proposals": list(st.pending_proposals()),
        "facts": facts_for(st, pm).into_iter().map(|e| entry_json(st, e)).collect::<Vec<_>>(),
        "journal": list(st.journal()),
        "alerts": st.alerts.iter().filter(|a| !pm || !matches!(a, crate::state::Alert::FactStale { .. })).collect::<Vec<_>>(),
        "new": list(new.iter().collect()),
    });
    if let Some(p) = crate::scope::pending_line(st) {
        v["pending_scope"] = json!(p);
    }
    v
}

pub(crate) fn ref_label(st: &State, id: &str) -> String {
    match st.get(id) {
        None => format!("{id} (not in the ledger)"),
        Some(e) => {
            let status = st
                .status_of(id)
                .map(|s| s.label().to_string())
                .unwrap_or_else(|| kind_label(&e.kind).to_string());
            if e.title.trim().is_empty() {
                format!("{id} ({status})")
            } else {
                format!("{id} ({}; {status})", one_line(&e.title, 80))
            }
        }
    }
}

/// Fields that `show` already displays on its own.
const SHOWN_FIELDS: &[&str] = &[
    "fingerprints",
    "supersedes",
    "approved_by",
    "objective",
    "non_goals",
    "done_when",
];

/// `kontext show <id>`: the full entry and its history.
pub fn show(st: &State, id: &str) -> String {
    let Some(e) = st.get(id) else {
        return format!("No entry with id {id}\n");
    };
    let status = st.status_of(id);
    let mark = match status {
        Some(Status::Superseded) if st.dropped.contains_key(id) => "[dropped] ".into(),
        Some(Status::Superseded) => format!(
            "[superseded by {}, no longer applies] ",
            st.superseded_by.get(id).map(String::as_str).unwrap_or("?")
        ),
        Some(Status::Ignored) => "[not counted] ".into(),
        Some(Status::Rejected) => "[rejected] ".into(),
        _ => String::new(),
    };
    let title = if e.title.trim().is_empty() {
        "(no title)".to_string()
    } else {
        one_line(&e.title, 100_000)
    };
    let mut l = vec![format!("## {} {mark}{title}", e.id), String::new()];
    l.push(format!("- **Kind:** {}", kind_label(&e.kind)));
    let status_text = match (crate::board::task_status_text(st, id), status) {
        (Some(t), _) => t,
        (None, _) if e.kind == "feature" => {
            crate::features::status_text(st, id).unwrap_or_else(|| "?".into())
        }
        (None, Some(s)) => {
            let mut t = s.label().to_string();
            if let Some(p) = st.promoted_to.get(id) {
                t.push_str(&format!(", became decision {p}"));
            } else if e.kind == "proposal" && s == Status::Accepted {
                t.push_str(" (counts as a decision in force)");
            } else if s == Status::Pending && user_rule(e) {
                t.push_str(
                    ", awaiting signature; it is a user rule and already applies (the signature is for the charter)",
                );
            }
            if let Some(r) = st.signed_by.get(id) {
                t.push_str(&format!(" ({r})"));
            }
            t
        }
        (None, None) if is_record_kind(&e.kind) => {
            format!("{} record on {}", kind_label(&e.kind), e.refs.join(", "))
        }
        (None, None) => "?".into(),
    };
    l.push(format!("- **Status:** {status_text}"));
    if let Some(reason) = st.not_counting.get(id) {
        l.push(format!(
            "- **Not counted:** written by someone who is not an owner ({reason})"
        ));
    }
    l.push(format!("- **Who:** {}", actor_text(e)));
    l.push(format!("- **When:** {} (hlc {})", when(&e.hlc), e.hlc));
    l.push(format!("- **Writer:** {} #{}", e.writer, e.n));
    if !e.tags.is_empty() {
        l.push(format!("- **Tags:** {}", e.tags.join(", ")));
    }
    if !e.refs.is_empty() {
        let refs: Vec<String> = e.refs.iter().map(|r| ref_label(st, r)).collect();
        l.push(format!("- **Refs:** {}", refs.join("; ")));
    }
    if let Some(o) = field(e, "objective") {
        l.push(format!("- **Objective:** {o}"));
    }
    let non = string_list(e.fields.get("non_goals"));
    if !non.is_empty() {
        l.push(format!("- **Not doing:** {}", non.join("; ")));
    }
    if let Some(d) = field(e, "done_when") {
        l.push(format!("- **Done when:** {d}"));
    }
    let sup = string_list(e.fields.get("supersedes"));
    if !sup.is_empty() {
        // It already supersedes if it is law, if it was in turn superseded, or
        // if what it cites was superseded by it (a fact or a pending proposal).
        let replaced = sup.iter().any(|x| {
            st.supersessions
                .get(x)
                .is_some_and(|by| by.iter().any(|b| b == id))
        });
        let verb = if replaced || st.is_law(id) || st.status_of(id) == Some(Status::Superseded) {
            "Supersedes"
        } else {
            "Would supersede (if it counts)"
        };
        let labels: Vec<String> = sup.iter().map(|x| ref_label(st, x)).collect();
        l.push(format!("- **{verb}:** {}", labels.join("; ")));
    }
    if let Some(by) = st.supersessions.get(id) {
        let labels: Vec<String> = by.iter().map(|x| ref_label(st, x)).collect();
        l.push(format!("- **Superseded by:** {}", labels.join("; ")));
    }
    let chain = st.supersede_chain(id);
    if chain.len() > 1 {
        let parts: Vec<String> = chain
            .iter()
            .map(|x| {
                if x == id {
                    format!("**{x} (this one)**")
                } else {
                    format!(
                        "{x} ({})",
                        st.status_of(x).map(|s| s.label()).unwrap_or("?")
                    )
                }
            })
            .collect();
        l.push(format!(
            "- **Supersession chain (oldest to newest):** {}",
            parts.join(" → ")
        ));
    }
    let fps = crate::facts::fingerprints_of(e);
    if !fps.is_empty() {
        let labels: Vec<String> = fps.iter().map(|f| f.label()).collect();
        l.push(format!("- **Fingerprints:** {}", labels.join("; ")));
    }
    if let Some(fr) = st.freshness.get(id) {
        l.push(format!("- **Freshness:** {}", crate::facts::describe(fr)));
    }
    if e.kind == "scope" {
        l.extend(crate::scope::show_lines(st, e));
    }
    for (k, v) in &e.fields {
        if e.kind == "scope"
            && matches!(
                k.as_str(),
                "type" | "covers" | "adds" | "removes" | "summary" | "summary_hash" | "said"
            )
        {
            continue;
        }
        if SHOWN_FIELDS.contains(&k.as_str()) || v.is_null() || v == &json!("") || v == &json!([]) {
            continue;
        }
        let text = match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        l.push(format!("- **{k}:** {}", text.trim().replace('\n', "\n  ")));
    }
    for (label, text) in [("Why", &e.why), ("Body", &e.body)] {
        if !text.trim().is_empty() {
            l.push(String::new());
            l.push(format!("**{label}**"));
            l.push(String::new());
            l.push(text.trim().to_string());
        }
    }
    if e.kind == "question" {
        l.push(String::new());
        l.push("**Answers**".into());
        l.push(String::new());
        let answers = st.answers.get(id).cloned().unwrap_or_default();
        if answers.is_empty() {
            l.push("(none yet)".into());
        }
        for a in answers.iter().filter_map(|a| st.get(a)) {
            l.push(format!(
                "- {} from {}, {}: {}",
                a.id,
                actor_text(a),
                when(&a.hlc),
                a.body.trim().replace('\n', "\n  ")
            ));
        }
    }
    if let Some(info) = st.task(id) {
        l.push(String::new());
        l.push("**Status history**".into());
        l.push(String::new());
        l.push(format!("- {} created by {}", when(&e.hlc), actor_text(e)));
        for h in &info.history {
            let why = st
                .get(&h.record)
                .map(|r| r.why.trim())
                .filter(|w| !w.is_empty())
                .map(|w| format!(" (why: {})", one_line(w, 160)))
                .unwrap_or_default();
            l.push(format!(
                "- {} {} {} by {}: {}{why}",
                when(&h.hlc),
                h.record,
                kind_label(&h.kind),
                h.actor,
                h.note
            ));
        }
        if let Some(x) = &info.expired {
            l.push(format!(
                "- the claim of {} expired (no journal entry or close within the [claims] lease)",
                x.actor
            ));
        }
        l.push(format!(
            "- now: {}",
            crate::board::task_status_text(st, id).unwrap_or_default()
        ));
        let journal: Vec<&Entry> = st
            .journal()
            .into_iter()
            .filter(|j| field(j, "task") == Some(id))
            .collect();
        if !journal.is_empty() {
            l.push(String::new());
            l.push("**Task journal**".into());
            l.push(String::new());
            for j in journal {
                let next = field(j, "next")
                    .map(|n| format!(" → next: {}", one_line(n, 100)))
                    .unwrap_or_default();
                l.push(format!(
                    "- {} {} {}: {}{next}",
                    when(&j.hlc),
                    j.id,
                    j.actor.short(),
                    one_line(&j.title, 160)
                ));
            }
        }
    }
    if e.kind == "feature"
        && let Some(info) = st.feature(id)
    {
        l.push(String::new());
        l.push(format!(
            "**Tasks** ({}/{} closed)",
            info.closed_tasks,
            info.tasks.len()
        ));
        l.push(String::new());
        if info.tasks.is_empty() {
            l.push(format!(
                "(none yet: `kontext task add \"<title>\" --feature {id} --acceptance \"…\"`)"
            ));
        }
        for t in info.tasks.iter().filter_map(|t| st.get(t)) {
            l.push(format!(
                "- {} [{}] {}",
                t.id,
                st.task_status(&t.id).map(|s| s.label()).unwrap_or("?"),
                one_line(&t.title, 120)
            ));
        }
        l.push(String::new());
        l.push("**Validation history**".into());
        l.push(String::new());
        if info.history.is_empty() {
            l.push("(not validated yet)".into());
        }
        for h in &info.history {
            let why = st
                .get(&h.record)
                .map(|r| r.why.trim())
                .filter(|w| !w.is_empty())
                .map(|w| format!(" (why: {})", one_line(w, 160)))
                .unwrap_or_default();
            let what = if h.kind == "done" {
                "validation"
            } else {
                kind_label(&h.kind)
            };
            l.push(format!(
                "- {} {} {what} by {}: {}{why}",
                when(&h.hlc),
                h.record,
                h.actor,
                h.note
            ));
        }
    }
    if e.kind == "epic" {
        let fs: Vec<&Entry> = st
            .of_kind("feature")
            .filter(|f| field(f, "epic") == Some(id))
            .collect();
        if !fs.is_empty() {
            l.push(String::new());
            l.push(format!(
                "**Features** ({})",
                crate::features::counts_text(st, id)
            ));
            l.push(String::new());
            for f in fs {
                l.push(format!(
                    "- {} [{}] {}",
                    f.id,
                    st.feature(&f.id).map(|i| i.status.label()).unwrap_or("?"),
                    one_line(&f.title, 120)
                ));
            }
        }
        let ts = st.tasks_of(id);
        l.push(String::new());
        l.push(format!("**Tasks** ({})", crate::board::epic_line(st, e)));
        l.push(String::new());
        if ts.is_empty() {
            l.push("(none)".into());
        }
        for t in ts {
            l.push(format!(
                "- {} [{}] {}",
                t.id,
                st.task_status(&t.id).map(|s| s.label()).unwrap_or("?"),
                one_line(&t.title, 120)
            ));
        }
    }
    let in_task_history = |r: &Entry| {
        st.task(id)
            .is_some_and(|i| i.history.iter().any(|h| h.record == r.id))
            || st
                .feature(id)
                .is_some_and(|i| i.history.iter().any(|h| h.record == r.id))
    };
    let history: Vec<&Entry> = st
        .referrers(id)
        .into_iter()
        .filter(|r| !(e.kind == "question" && r.kind == "answer"))
        .filter(|r| !in_task_history(r))
        .filter(|r| !(e.kind == "epic" && matches!(r.kind.as_str(), "task" | "feature")))
        .collect();
    if !history.is_empty() {
        l.push(String::new());
        l.push("**History**".into());
        l.push(String::new());
        for r in history {
            let note = if st.counts(&r.id) {
                ""
            } else {
                " [not counted]"
            };
            let text = [&r.why, &r.body]
                .iter()
                .filter(|t| !t.trim().is_empty())
                .map(|t| one_line(t, 200))
                .collect::<Vec<_>>()
                .join(" · ");
            let detail = if is_record_kind(&r.kind) {
                text
            } else {
                one_line(&r.title, 120)
            };
            l.push(format!(
                "- {} {}{note} by {}, {}{}",
                r.id,
                kind_label(&r.kind),
                actor_text(r),
                when(&r.hlc),
                if detail.is_empty() {
                    String::new()
                } else {
                    format!(": {detail}")
                }
            ));
        }
    }
    l.join("\n") + "\n"
}

/// `kontext show --json`.
pub fn show_json(st: &State, id: &str) -> Value {
    let Some(e) = st.get(id) else {
        return Value::Null;
    };
    let mut v = json!({
        "entry": e,
        "task": st.task(id),
        "status": st.status_of(id),
        "counts": st.counts(id),
        "not_counting": st.not_counting.get(id),
        "superseded_by": st.supersessions.get(id),
        "chain": st.supersede_chain(id),
        "answers": st.answers.get(id),
        "freshness": st.freshness.get(id),
        "history": st.referrers(id),
    });
    if let Some(f) = st.feature(id) {
        v["feature"] = json!(f);
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Actor, GENESIS};
    use crate::state::Policy;
    use serde_json::{Map, json};

    fn e(id: &str, n: usize, kind: &str, title: &str) -> Entry {
        Entry {
            v: 1,
            id: id.into(),
            writer: "w".into(),
            n: 1,
            prev: GENESIS.into(),
            hash: String::new(),
            hlc: format!("2026-11-03T18:{:02}:{:02}.000Z.0000", n / 60, n % 60),
            actor: Actor::human("omar"),
            kind: kind.into(),
            title: title.into(),
            body: String::new(),
            why: String::new(),
            tags: vec![],
            refs: vec![],
            fields: Map::new(),
            sig: None,
            extra: Map::new(),
        }
    }

    fn said(mut x: Entry) -> Entry {
        x.actor = Actor::agent("distill");
        x.fields.insert("origin".into(), json!("user"));
        x.fields.insert("said_at".into(), json!("2026-11-03 10:00"));
        x
    }

    fn opts(max: usize) -> ResumeOpts {
        ResumeOpts {
            project: "demo".into(),
            role: "pm".into(),
            max_chars: max,
            generated: "now".into(),
            chain_problem: None,
            entries: 0,
            writer: None,
            distill: false,
        }
    }

    /// A small ledger whose `resume` fits once the existing ladder runs.
    fn small_ledger() -> Vec<Entry> {
        let mut v = Vec::new();
        let mut c = e("A-000001", 1, "charter", "Invoices for small shops");
        c.fields
            .insert("objective".into(), json!("Send invoices by email"));
        v.push(c);
        let mut s = e("R-000001", 2, "sign", "");
        s.refs = vec!["A-000001".into()];
        v.push(s);
        v.push(e("G-000001", 3, "goal", "First invoice sent"));
        for (i, tag) in ["billing", "invoices", "billing"].iter().enumerate() {
            let mut d = e(
                &format!("D-00000{i}"),
                10 + i,
                "decision",
                &format!("Invoice rule number {i}"),
            );
            d.why = "because the accountant asked for it".into();
            d.tags = vec![tag.to_string()];
            v.push(d);
        }
        let mut q = e("Q-000001", 20, "question", "Which tax rate applies?");
        q.fields.insert("to".into(), json!("pm"));
        q.actor = Actor::agent("coder");
        v.push(q);
        let mut p = e("P-000001", 21, "proposal", "Number invoices per year");
        p.actor = Actor::agent("coder");
        p.why = "the accountant resets the count every year".into();
        v.push(p);
        let mut f = e("F-000001", 22, "fact", "The billing table has no index");
        f.actor = Actor::agent("scout");
        v.push(f);
        for i in 0..4 {
            v.push(e(
                &format!("J-00000{i}"),
                30 + i,
                "journal",
                &format!("Worked on the invoice template, step {i}"),
            ));
        }
        v
    }

    const SMALL_HEAD: &str = "# Context of demo for pm (kontext resume)
Generated now. Ledger: 0 entries, chain OK.

The conversation is disposable: what is not in kontext is lost. Agents propose (`kontext propose`), they do not decide. What the user said applies even when unsigned; what was superseded no longer applies. Details of any id: `kontext show <id>`.
Invoke kontext as a standalone command that starts with `kontext`: no `VAR=…` or `export` in front, no `cd … &&`, no pipes (`| head`, `| sed`) or `( … )`; that way a `Bash(kontext:*)` permission covers it and it does not wait for approval. Your identity goes in the arguments: `kontext --actor agent:<your-name> --role <role> log …`.

## Charter
A-000001 [signed] Invoices for small shops
- Objective: Send invoices by email

## Goals (1)
- G-000001 [unsigned] First invoice sent

## Decisions in force (3)
";

    const SMALL_FULL: &str = "### billing
- D-000000 Invoice rule number 0 (why: because the accountant asked for it)
- D-000002 Invoice rule number 2 (why: because the accountant asked for it)
### invoices
- D-000001 Invoice rule number 1 (why: because the accountant asked for it)

## Epics (0)
- (none)

## Questions for pm (1)
- Q-000001 Which tax rate applies? (from agent:coder)

## Pending proposals (1)
- P-000001 Number invoices per year from agent:coder (why: the accountant resets the count every year)

## Recent facts (1)
- F-000001 The billing table has no index

## Journal (last 4 of 4)
- 2026-11-03 18:00 human:omar Worked on the invoice template, step 0
- 2026-11-03 18:00 human:omar Worked on the invoice template, step 1
- 2026-11-03 18:00 human:omar Worked on the invoice template, step 2
- 2026-11-03 18:00 human:omar Worked on the invoice template, step 3

## What's new since your last time (13)
- A-000001 charter: Invoices for small shops (human:omar)
- A-000001 signed by human:omar: Invoices for small shops
- G-000001 goal: First invoice sent (human:omar)
- D-000000 decision: Invoice rule number 0 (human:omar)
- D-000001 decision: Invoice rule number 1 (human:omar)
- D-000002 decision: Invoice rule number 2 (human:omar)
- Q-000001 question: Which tax rate applies? (agent:coder)
- P-000001 proposal: Number invoices per year (agent:coder)
- J-000000 journal: Worked on the invoice template, step 0 (human:omar)
- J-000001 journal: Worked on the invoice template, step 1 (human:omar)
- J-000002 journal: Worked on the invoice template, step 2 (human:omar)
- J-000003 journal: Worked on the invoice template, step 3 (human:omar)
- … and 1 entry from the epics' work (tasks, task journal, facts): each coordinator sees them with `kontext brief <epic>`
";

    const SMALL_CUT: &str = "- **billing**: D-000000 Invoice rule number 0; D-000002 Invoice rule number 2
- **invoices**: D-000001 Invoice rule number 1

## Epics (0)
- (none)

## Questions for pm (1)
- Q-000001 Which tax rate applies? (from agent:coder)

## Pending proposals (1)
- P-000001 Number invoices per year from agent:coder

## Recent facts (1)
- … 1 more, see `kontext recall <topic>`

## Journal (last 0 of 4)
- (nothing)

## What's new since your last time (13)
- … 12 more, see `kontext standup --peek`
- … and 1 entry from the epics' work (tasks, task journal, facts): each coordinator sees them with `kontext brief <epic>`
";

    /// What fits with the existing ladder comes out exactly as before the cut
    /// of the decisions list existed (texts taken from that version).
    #[test]
    fn small_ledger_output_is_unchanged() {
        let v = small_ledger();
        let st = State::build_at(v.clone(), &Policy::new(&["omar"]), 0);
        assert_eq!(
            resume(&st, &v, &opts(9000)),
            format!("{SMALL_HEAD}{SMALL_FULL}")
        );
        assert_eq!(
            resume(&st, &v, &opts(1500)),
            format!("{SMALL_HEAD}{SMALL_CUT}")
        );
    }

    /// A ledger with 320 decisions in force under six tags, plus what the pm
    /// must see: questions for it, pending proposals and a conflict.
    fn large_ledger() -> Vec<Entry> {
        let tags = ["billing", "invoices", "taxes", "email", "pdf", "exports"];
        let mut v = Vec::new();
        let mut c = e("A-000001", 1, "charter", "Invoices for small shops");
        c.fields
            .insert("objective".into(), json!("Send invoices by email"));
        v.push(c);
        for i in 0..320 {
            let mut d = e(
                &format!("D-1{i:05}"),
                100 + i,
                "decision",
                &format!("Invoice rule {i}: round totals to cents before the tax is added"),
            );
            d.why = "the accountant asked for it in the review".into();
            d.tags = vec![tags[i % tags.len()].into()];
            v.push(d);
        }
        // Two decisions that supersede the same one: a conflict alert.
        v.push(e("D-200000", 500, "decision", "Old export format"));
        for (id, n) in [("D-200001", 501), ("D-200002", 502)] {
            let mut d = e(id, n, "decision", "New export format");
            d.fields.insert("supersedes".into(), json!(["D-200000"]));
            v.push(d);
        }
        for i in 0..3 {
            let mut q = e(
                &format!("Q-00000{i}"),
                600 + i,
                "question",
                &format!("Which tax rate applies to region {i}?"),
            );
            q.fields.insert("to".into(), json!("pm"));
            q.actor = Actor::agent("coder");
            v.push(q);
        }
        for i in 0..4 {
            let mut p = e(
                &format!("P-00000{i}"),
                700 + i,
                "proposal",
                &format!("Number invoices per year, variant {i}"),
            );
            p.actor = Actor::agent("coder");
            v.push(p);
        }
        v
    }

    #[test]
    fn a_long_list_of_decisions_is_cut_and_the_rest_stays() {
        let v = large_ledger();
        let st = State::build_at(v.clone(), &Policy::new(&["omar"]), 0);
        let law = st.law().len();
        assert_eq!(law, 322);
        let out = resume(&st, &v, &opts(9000));
        assert!(out.chars().count() <= 9000, "{}", out.chars().count());
        assert!(!out.contains("(kontext: this is over"), "{out}");
        // The other sections, with their items.
        for want in [
            "A-000001 [unsigned] Invoices for small shops",
            "## Questions for pm (3)",
            "- Q-000002 Which tax rate applies to region 2? (from agent:coder)",
            "## Pending proposals (4)",
            "- P-000003 Number invoices per year, variant 3 from agent:coder",
            "## Alerts (1)",
            "- conflict: D-200000 is superseded at the same time by D-200001 and D-200002.",
            "## What's new since your last time (331)\n- … 330 more, see `kontext standup --peek`\n",
        ] {
            assert!(out.contains(want), "missing {want:?}:\n{out}");
        }
        // The newest decisions stay; the notice says how many are not shown.
        let sec = &out[out.find("## Decisions in force (322)").expect(&out)..];
        let sec = &sec[..sec.find("\n## ").unwrap()];
        let shown = sec.matches("D-1").count() + sec.matches("D-2").count();
        assert!(shown > 10 && shown < law, "{shown}\n{sec}");
        assert!(
            sec.contains(&format!(
                "- … {} older decisions in force not shown (by first tag: ",
                law - shown
            )),
            "{sec}"
        );
        assert!(sec.contains("`kontext recall <words>`") && sec.contains("`kontext show <id>`"));
        assert!(sec.contains("D-200002 New export format"), "{sec}");
        assert!(!sec.contains("D-100000 "), "{sec}");
        // The last line is whole.
        assert!(
            out.ends_with("\n- … 330 more, see `kontext standup --peek`\n"),
            "{out}"
        );
        // `--json` is not cut.
        let js = resume_json(&st, &v, &opts(9000));
        assert_eq!(js["decisions"].as_array().unwrap().len(), law);

        // Not even the rest fits: no titles, and the closing note says so.
        let tiny = resume(&st, &v, &opts(1500));
        assert!(
            tiny.contains("- … 322 older decisions in force not shown."),
            "{tiny}"
        );
        assert!(
            tiny.ends_with(
                "(kontext: this is over 1500 characters even without the titles of the decisions in force; the charter and the goals are never cut)\n"
            ),
            "{tiny}"
        );
    }

    #[test]
    fn budget_keeps_what_the_user_said_before_the_rest() {
        let mut v = vec![e("D-000001", 1, "decision", "A decision in force")];
        for i in 0..12 {
            v.push(said(e(
                &format!("F-1{i:05}"),
                10 + i,
                "fact",
                &format!("User fact number {i}"),
            )));
        }
        for i in 0..40 {
            v.push(e(
                &format!("J-2{i:05}"),
                100 + i,
                "journal",
                &"a long journal entry ".repeat(3),
            ));
            let mut q = e(
                &format!("Q-3{i:05}"),
                200 + i,
                "question",
                &format!("Open question {i} for the pm"),
            );
            q.fields.insert("to".into(), json!("pm"));
            v.push(q);
        }
        // A user fact that was already superseded does not show.
        let mut upd = said(e("F-900000", 999, "fact", "User fact number 0, corrected"));
        upd.fields.insert("supersedes".into(), json!(["F-100000"]));
        v.push(upd);
        let st = State::build_at(v.clone(), &Policy::new(&["omar"]), 0);
        let new: Vec<Entry> = v.clone();
        let full = resume(&st, &new, &opts(1_000_000));
        assert!(full.contains("## What the user said (12)"), "{full}");
        // Not repeated in "What's new".
        let newsec = &full[full.find("## What's new").unwrap()..];
        assert!(!newsec.contains("User fact"), "{newsec}");
        assert!(full.contains("- F-900000 (2026-11-03 10:00) User fact number 0, corrected (supersedes F-100000 «User fact number 0», which no longer applies)"), "{full}");
        assert!(!full.contains("number 0\n"), "{full}");
        // With little room, the journal, what is new and what is open go, not the user's.
        let small = resume(&st, &new, &opts(3500));
        assert!(small.chars().count() <= 3500, "{}", small.chars().count());
        for i in 1..12 {
            assert!(
                small.contains(&format!("User fact number {i}")),
                "missing {i}:\n{small}"
            );
        }
        assert!(small.contains("## Journal (last 0 of 40)"), "{small}");
        assert!(
            small.contains("more, see `kontext standup --peek`"),
            "{small}"
        );
        assert!(small.contains("A decision in force"));
        // A user rule shows as such; a proposal of theirs, as a proposal.
        let mut v2 = v.clone();
        let mut rule = said(e("P-a00001", 1500, "proposal", "Notify before 9"));
        rule.fields.insert("user_rule".into(), json!(true));
        v2.push(rule);
        v2.push(said(e("P-a00002", 1501, "proposal", "Switch databases")));
        let mut agent_rule = e("P-a00003", 1502, "proposal", "An agent's rule");
        agent_rule.actor = Actor::agent("distill");
        agent_rule.fields.insert("user_rule".into(), json!(true));
        v2.push(agent_rule);
        let st2 = State::build_at(v2.clone(), &Policy::new(&["omar"]), 0);
        let out = resume(&st2, &v2, &opts(1_000_000));
        assert!(
            out.contains("- P-a00001 (2026-11-03 10:00) [user rule, in force] Notify before 9"),
            "{out}"
        );
        assert!(
            out.contains("- P-a00002 (2026-11-03 10:00) [proposal] Switch databases"),
            "{out}"
        );
        assert!(
            out.contains("applies as stated") && out.contains("`kontext ask`"),
            "{out}"
        );
        assert!(
            out.contains("It applies now, even when unsigned: the signature is for the charter"),
            "{out}"
        );
        // The agent's one stays in pending proposals, not as a rule.
        let pending = &out[out.find("## Pending proposals").unwrap()..];
        assert!(
            pending.contains("P-a00003 [not reviewed] An agent's rule"),
            "{pending}"
        );

        // And with less, the oldest of the user's is summed up in one line.
        let tiny = resume(&st, &new, &opts(1200));
        assert!(tiny.contains("A decision in force"));
        assert!(
            tiny.contains("older, see `kontext resume --as pm --json`"),
            "{tiny}"
        );
        assert!(tiny.contains("User fact number 11"), "{tiny}");
    }

    /// The memory-long benchmark case: the user gave one rate and then another
    /// that supersedes it, both as unsigned rules. The one in force is the new
    /// one and shows as in force; the old one only appears afterwards, marked as
    /// no longer applying, in "What the user said", in "What's new", in `show`
    /// and in the human's view (`standup`).
    #[test]
    fn a_user_rule_applies_unsigned_and_the_replaced_one_does_not() {
        let rule = |id: &str, n: usize, title: &str| {
            let mut r = said(e(id, n, "proposal", title));
            r.fields.insert("user_rule".into(), json!(true));
            r
        };
        let old = rule("P-b00001", 10, "The late fee is 1.5 % per month");
        let mut new = rule(
            "P-b00002",
            20,
            "The late fee now applies at 1.8 % per month",
        );
        new.fields.insert("supersedes".into(), json!(["P-b00001"]));
        let v = vec![old, new];
        let st = State::build_at(v.clone(), &Policy::new(&["omar"]), 0);
        assert_eq!(st.status_of("P-b00001"), Some(Status::Superseded));
        assert_eq!(st.status_of("P-b00002"), Some(Status::Pending));

        let out = resume(&st, &v, &opts(1_000_000));
        let said = &out[out.find("## What the user said (1)").expect(&out)..];
        let said = &said[..said.find("\n## ").unwrap_or(said.len())];
        assert!(
            said.contains(
                "- P-b00002 (2026-11-03 10:00) [user rule, in force] The late fee now applies at 1.8 % per month \
                 (supersedes P-b00001 «The late fee is 1.5 % per month», which no longer applies)"
            ),
            "{said}"
        );
        // New first; the old one only as superseded.
        assert!(said.find("1.8 %").unwrap() < said.find("1.5 %").unwrap());
        assert!(!out.contains("unsigned)]"), "{out}");
        let newsec = &out[out.find("## What's new").unwrap()..];
        assert!(
            newsec.contains("P-b00001 proposal [superseded by P-b00002, no longer applies]"),
            "{newsec}"
        );
        assert!(out.contains("## Pending proposals (0)"), "{out}");

        let show_new = show(&st, "P-b00002");
        assert!(
            show_new.contains("awaiting signature; it is a user rule and already applies"),
            "{show_new}"
        );
        let show_old = show(&st, "P-b00001");
        assert!(
            show_old.contains("[superseded by P-b00002, no longer applies]"),
            "{show_old}"
        );

        let js = resume_json(&st, &v, &opts(1_000_000));
        assert_eq!(js["user_said"][0]["id"], "P-b00002");
        assert_eq!(js["user_said"][0]["in_force"], true);

        let up = standup(&st, &v, "demo", "human:omar", None).markdown();
        assert!(
            up.contains("user rule (already in force; still needs signing for the charter)"),
            "{up}"
        );
    }
}
