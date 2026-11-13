//! Derived state (DESIGN §5). Everything is sorted by `hlc` (and by `id` on
//! ties) and walked in order, applying the rules of each ledger entry.
//!
//! Owner rule (DESIGN §4.3 and §14): `sign`, `accept`, `reject`, a `decision`
//! and a `supersede` of a charter, a goal, a decision or a proposal count only
//! if a human listed in `owners` of `project.toml` writes them, or an agent
//! with role `pm` and `fields.approved_by` set to the name of an owner. What
//! doesn't count is stored anyway and shows up as an alert.
//!
//! Conflicts (two supersessions in force for the same target, two signed
//! charters in force) are computed at the end of the walk and never resolve on
//! their own: they are resolved by another entry that supersedes both.
//!
//! Tasks (phase 3, DESIGN §4.3 and §7.2): `assign`, `claim`, `release`, `done`
//! and `reopen` are applied in `hlc` order. `done` and `release` count only if
//! the task's holder, the epic's coordinator, the pm or an owner writes them;
//! `done` also needs valid evidence. A `claim` expires if its holder writes no
//! `journal` or `done` for the task within `[claims] lease` (2 h): if someone
//! else claims it later, the newer claim wins; if nobody does, the task becomes
//! ready again when read. Each task's status (ready, blocked, assigned, claimed,
//! closed) is computed at the end, once all its dependencies have been seen.
//!
//! Phase 4 adds fact staleness: [`State::freshness`] and [`Alert::FactStale`],
//! which are filled after the walk with [`crate::facts::annotate`] because they
//! need git.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;

use anyhow::{Result, bail};
use serde::Serialize;
use serde_json::Value;

use crate::config::Project;
use crate::model::{Actor, Entry, is_record_kind};
use crate::tasks::{self, DEFAULT_LEASE_MS, TaskStatus};

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Charter or goal not signed yet.
    Unsigned,
    Signed,
    /// Decision in force, or accepted proposal.
    Accepted,
    /// Proposal not reviewed yet.
    Pending,
    Rejected,
    /// Question not answered yet, or task ready.
    Open,
    Answered,
    /// Task with a holder.
    Claimed,
    Done,
    /// Epic, fact or journal.
    Active,
    Superseded,
    /// An entry that needed an owner and was not one: stored, but not counted.
    Ignored,
}

impl Status {
    /// How the status reads in the views.
    pub fn label(self) -> &'static str {
        match self {
            Status::Unsigned => "unsigned",
            Status::Signed => "signed",
            Status::Accepted => "accepted",
            Status::Pending => "pending",
            Status::Rejected => "rejected",
            Status::Open => "open",
            Status::Answered => "answered",
            Status::Claimed => "claimed",
            Status::Done => "closed",
            Status::Active => "in force",
            Status::Superseded => "superseded",
            Status::Ignored => "not counted",
        }
    }
}

/// Who holds a task.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct Claim {
    /// The `claim` or `assign` entry that gave it.
    pub record: String,
    pub kind: String,
    /// Who holds it: `agent:impl-1` in a `claim`; in an `assign`, the
    /// `fields.to` as written (`agent:impl-1` or just `impl-1`).
    pub actor: String,
    pub hlc: String,
}

/// Something that happened to a task, for `show` and the views.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct TaskEvent {
    pub record: String,
    pub kind: String,
    pub actor: String,
    pub hlc: String,
    /// Did the status change?
    pub counted: bool,
    /// What happened, in one sentence ("claimed by agent:impl-1", "does not count: …").
    pub note: String,
}

/// The status of a task.
#[derive(Serialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct TaskInfo {
    pub status: TaskStatus,
    /// Who holds it now (not counting an expired claim).
    pub holder: Option<Claim>,
    /// Last activity of the holder (ms): their claim, journal or close.
    pub last_activity: i64,
    /// The `done` that closed it.
    pub done: Option<String>,
    /// A claim that expired without anyone else taking it.
    pub expired: Option<Claim>,
    /// Dependencies still open (or that do not exist).
    pub blocked_by: Vec<String>,
    pub history: Vec<TaskEvent>,
}

/// The drift reason of a task or epic that cites no goal. `ui.rs` matches it.
pub const NO_GOAL_REASON: &str = "does not cite any goal";

/// Something someone needs to see. Later phases add variants (drift, stale
/// context, a stale fact, a guard that fails).
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Alert {
    /// An entry that only counts if an owner writes it, but someone else did.
    NotCounting {
        id: String,
        kind: String,
        actor: String,
        reason: String,
    },
    /// Two or more entries in force supersede the same one.
    Conflict { target: String, by: Vec<String> },
    /// More than one signed charter not superseded: the last signed one counts.
    CharterConflict { active: String, others: Vec<String> },
    /// Drift: a task or epic with no goals, or one that cites a superseded goal
    /// or a goal that does not exist.
    Drift {
        id: String,
        kind: String,
        reason: String,
    },
    /// A task or epic cites a goal that no owner has signed yet.
    UnsignedGoal {
        id: String,
        kind: String,
        goal: String,
    },
    /// An `assign`, `release` or `done` that does not count (because of who
    /// wrote it, or a close without valid evidence).
    TaskRecordIgnored {
        id: String,
        task: String,
        kind: String,
        actor: String,
        reason: String,
    },
    /// A claim expired without a `journal` or `done` from its holder: the task
    /// is ready again.
    ClaimExpired {
        task: String,
        holder: String,
        since: String,
    },
    /// Phase 5: with `require_signatures`, an entry that is signed (`sign`,
    /// `decision`, `accept`, `reject`, `supersede`) without a signature that
    /// verifies against an owner's keys. It is stored, but does not count.
    BadSignature {
        id: String,
        kind: String,
        actor: String,
        reason: String,
    },
    /// Phase 5: a `claim` that arrived from another machine and lost to an
    /// earlier one (the lower `hlc` wins). Whoever lost has to drop it.
    ClaimLost {
        task: String,
        /// The entry that lost.
        record: String,
        /// Who lost (`agent:impl-2`).
        loser: String,
        /// Who holds it (`agent:impl-1`).
        winner: String,
        /// The writer (machine) of the one that won.
        winner_writer: String,
    },
    /// Phase 5: a pending proposal that says it supersedes a decision in force
    /// (`fields.supersedes`). It shows until it is accepted or rejected.
    ProposalSupersedes { proposal: String, decision: String },
    /// Phase 4: a fact whose fingerprint no longer matches `HEAD` (it is not
    /// written; [`crate::facts::annotate`] adds it when reading).
    FactStale {
        id: String,
        repo: String,
        path: String,
        /// Short `HEAD` where the change was seen.
        head: String,
        /// The path no longer exists.
        missing: bool,
    },
}

impl Alert {
    /// The entries the alert touches.
    pub fn ids(&self) -> Vec<&str> {
        match self {
            Alert::NotCounting { id, .. } => vec![id],
            Alert::Conflict { target, by } => {
                let mut v = vec![target.as_str()];
                v.extend(by.iter().map(String::as_str));
                v
            }
            Alert::CharterConflict { active, others } => {
                let mut v = vec![active.as_str()];
                v.extend(others.iter().map(String::as_str));
                v
            }
            Alert::Drift { id, .. } => vec![id],
            Alert::UnsignedGoal { id, goal, .. } => vec![id, goal],
            Alert::TaskRecordIgnored { id, task, .. } => vec![id, task],
            Alert::ClaimExpired { task, .. } => vec![task],
            Alert::FactStale { id, .. } => vec![id],
            Alert::BadSignature { id, .. } => vec![id],
            Alert::ClaimLost { task, record, .. } => vec![task, record],
            Alert::ProposalSupersedes { proposal, decision } => vec![proposal, decision],
        }
    }

    pub fn is_conflict(&self) -> bool {
        matches!(self, Alert::Conflict { .. } | Alert::CharterConflict { .. })
    }

    /// Alerts that keep showing in `standup` until someone resolves them (not
    /// only when they are new).
    pub fn is_standing(&self) -> bool {
        self.is_conflict()
            || matches!(
                self,
                Alert::Drift { .. }
                    | Alert::UnsignedGoal { .. }
                    | Alert::ClaimExpired { .. }
                    | Alert::FactStale { .. }
                    | Alert::ClaimLost { .. }
                    | Alert::ProposalSupersedes { .. }
            )
    }

    pub fn is_drift(&self) -> bool {
        matches!(self, Alert::Drift { .. } | Alert::UnsignedGoal { .. })
    }
}

impl fmt::Display for Alert {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Alert::NotCounting {
                id, kind, reason, ..
            } => write!(
                f,
                "{id} ({kind}) does not count: it was written by someone who is not an owner ({reason})"
            ),
            Alert::Conflict { target, by } => write!(
                f,
                "conflict: {target} is superseded at the same time by {}. Decide which one counts with a \
                 decision that supersedes all of them (`kontext decide … --supersedes …`)",
                by.join(" and ")
            ),
            Alert::CharterConflict { active, others } => write!(
                f,
                "conflict: more than one signed charter is in force. The last signed one counts ({active}); \
                 {} is still not superseded. Write and sign a charter that supersedes them",
                others.join(", ")
            ),
            Alert::Drift { id, kind, reason } => {
                let what = if kind == "epic" {
                    "the epic"
                } else {
                    "the task"
                };
                write!(f, "drift: {what} {id} {reason}")
            }
            Alert::UnsignedGoal { id, kind, goal } => {
                let what = if kind == "epic" {
                    "the epic"
                } else {
                    "the task"
                };
                write!(
                    f,
                    "unsigned goal: {what} {id} cites {goal}, which no owner has signed yet (`kontext sign {goal}`)"
                )
            }
            Alert::TaskRecordIgnored {
                id,
                task,
                kind,
                actor,
                reason,
            } => write!(
                f,
                "{id} ({kind} for {task}, by {actor}) does not count: {reason}"
            ),
            Alert::ClaimExpired {
                task,
                holder,
                since,
            } => write!(
                f,
                "the claim on {task} by {holder} expired (no journal or close since {since}); the task is ready again"
            ),
            Alert::BadSignature {
                id, kind, reason, ..
            } => write!(
                f,
                "{id} ({kind}) does not count: signature invalid or missing ({reason}); project.toml sets require_signatures"
            ),
            Alert::ClaimLost {
                task,
                loser,
                winner,
                winner_writer,
                ..
            } => write!(
                f,
                "{task} was claimed first by {winner} (on {winner_writer}); {loser}: stop working on it and get another with `kontext next`"
            ),
            Alert::ProposalSupersedes { proposal, decision } => write!(
                f,
                "{proposal} proposes to supersede {decision}, which is in force: `kontext accept {proposal}` or `kontext reject {proposal} --why …`"
            ),
            Alert::FactStale {
                id,
                path,
                head,
                missing,
                ..
            } => write!(
                f,
                "{id} went stale: {path} {} at {head}; the next scout will read it again",
                if *missing {
                    "no longer exists"
                } else {
                    "changed"
                }
            ),
        }
    }
}

/// Who can do what: the `owners` in `project.toml`, and how long a claim
/// lasts (`[claims] lease`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    pub owners: Vec<String>,
    /// How long a `claim` stays valid without activity from its holder, in milliseconds.
    pub lease_ms: i64,
    /// Phase 5: `require_signatures` from `project.toml`.
    pub require_signatures: bool,
    /// Phase 5: the result of verifying the signature of each entry that needs
    /// one (only with `require_signatures`). The store fills it before the walk,
    /// because verifying needs `ssh-keygen`; an entry that is not here does not count.
    pub signatures: BTreeMap<String, crate::sign::Verdict>,
}

impl Default for Policy {
    fn default() -> Policy {
        Policy {
            owners: Vec::new(),
            lease_ms: DEFAULT_LEASE_MS,
            require_signatures: false,
            signatures: BTreeMap::new(),
        }
    }
}

impl Policy {
    pub fn new(owners: &[&str]) -> Policy {
        Policy {
            owners: owners.iter().map(|s| s.to_string()).collect(),
            ..Policy::default()
        }
    }

    /// The owners from the `project.toml` in kontext-home. Without a `project.toml`
    /// (low-level tests) there are no owners, and nothing restricted counts.
    pub fn load(home: &Path) -> Result<Policy> {
        if !home.join("project.toml").exists() {
            return Ok(Policy::default());
        }
        let project = Project::load(home)?;
        let lease_ms = match project.claims.as_ref().and_then(|c| c.lease.as_deref()) {
            Some(l) => tasks::parse_duration_ms(l)
                .map_err(|e| anyhow::anyhow!("[claims] lease de project.toml: {e}"))?,
            None => DEFAULT_LEASE_MS,
        };
        Ok(Policy {
            owners: project.owners,
            lease_ms,
            require_signatures: project.require_signatures,
            signatures: BTreeMap::new(),
        })
    }

    /// Is it a human owner?
    pub fn is_owner_actor(&self, actor: &Actor) -> bool {
        actor.is_human() && self.is_owner(&actor.name)
    }

    /// Who manages the tasks of an epic: an owner, the pm or the epic's
    /// coordinator.
    pub fn manages(&self, actor: &Actor, epic: Option<&Entry>) -> bool {
        if self.is_owner_actor(actor) || (actor.is_agent() && actor.role.as_deref() == Some("pm")) {
            return true;
        }
        epic.is_some_and(|e| tasks::is_coordinator(actor, e))
    }

    pub fn is_owner(&self, name: &str) -> bool {
        self.owners.iter().any(|o| o == name)
    }

    /// `Ok` if `actor` (with `approved_by`) can sign, accept, reject, decide or
    /// supersede. Otherwise, the reason.
    pub fn check(&self, actor: &Actor, approved_by: Option<&str>) -> Result<(), String> {
        let approved = approved_by.map(str::trim).filter(|s| !s.is_empty());
        if actor.is_human() {
            if self.is_owner(&actor.name) {
                return Ok(());
            }
            return Err(format!(
                "human:{} is not in owners of project.toml",
                actor.name
            ));
        }
        if actor.role.as_deref() != Some("pm") {
            return Err(format!(
                "agent:{} is an agent; an agent only counts with role pm and approved_by from an owner",
                actor.name
            ));
        }
        match approved {
            None => Err(format!(
                "agent:{} is the pm but has no approved_by from an owner",
                actor.name
            )),
            Some(o) if !self.is_owner(o) => Err(format!(
                "agent:{} has approved_by = {o}, which is not in owners",
                actor.name
            )),
            Some(_) => Ok(()),
        }
    }

    /// The rule for an entry already written. Anything imported from v0 is judged
    /// by the v0 rule, which had already accepted it: any human, or
    /// `agent:coordinator` with `approved_by`.
    fn check_entry(&self, e: &Entry) -> Result<(), String> {
        let approved = e.fields.get("approved_by").and_then(Value::as_str);
        if e.fields.contains_key("v0_id") {
            let ok = e.actor.is_human()
                || (e.actor.name == "coordinator" && approved.is_some_and(|a| !a.is_empty()));
            return if ok {
                Ok(())
            } else {
                Err(format!(
                    "{} (imported from v0) is neither a human nor a coordinator with approved_by",
                    e.actor.short()
                ))
            };
        }
        self.check(&e.actor, approved)
    }
}

#[derive(Default, Debug, Clone)]
pub struct State {
    pub policy: Policy,
    /// All entries, by `id`.
    pub entries: BTreeMap<String, Entry>,
    /// The `id`s in `hlc` order.
    pub order: Vec<String>,
    /// Status of each entry with its own identity.
    pub status: BTreeMap<String, Status>,
    /// The last supersession of each superseded entry.
    pub superseded_by: BTreeMap<String, String>,
    /// All entries that superseded each one, in order (if more than one is in
    /// force, it is a conflict).
    pub supersessions: BTreeMap<String, Vec<String>>,
    /// Task → who holds it.
    pub claims: BTreeMap<String, Claim>,
    /// Task → its status, its holder and its history.
    pub tasks: BTreeMap<String, TaskInfo>,
    /// Feature → its state, its tasks and its validations (empty in a project
    /// without features).
    pub features: BTreeMap<String, crate::features::FeatureInfo>,
    /// Now (ms since 1970): to know whether a claim expired. Comes from
    /// `KONTEXT_NOW` or the clock.
    pub now_ms: i64,
    /// Entries that do not count, with the reason.
    pub not_counting: BTreeMap<String, String>,
    /// Charter or goal → the `sign` entry that signed it.
    pub signed_by: BTreeMap<String, String>,
    /// Proposal → the `accept` entry.
    pub accepted_by: BTreeMap<String, String>,
    /// Proposal → the `reject` entry.
    pub rejected_by: BTreeMap<String, String>,
    /// Accepted proposal → the decision that made it law.
    pub promoted_to: BTreeMap<String, String>,
    /// Question → its answers.
    pub answers: BTreeMap<String, Vec<String>>,
    /// The charter in force: the last signed one that was not superseded.
    pub active_charter: Option<String>,
    pub alerts: Vec<Alert>,
    /// Phase 4: the freshness of each fact with fingerprints. Empty until
    /// [`crate::facts::annotate`] is called (it needs git; the walk does not).
    pub freshness: BTreeMap<String, crate::facts::Freshness>,
    /// The human's inbox: key of a pending item → the entry (`dismiss` or
    /// `confirm` from a human, with `fields.items`) that closed it.
    pub inbox_closed: BTreeMap<String, String>,
    /// Question → the `confirm`s from people who asked the same thing later
    /// (`kontext ask` joins the open question instead of repeating it).
    pub joined: BTreeMap<String, Vec<String>>,
    /// The old id of each entry imported from v0 (`fields.v0_id`, `D-0030`) →
    /// its id here. [`State::resolve`] looks them up.
    pub v0_ids: BTreeMap<String, String>,
    /// Charters in the order they were signed.
    sign_order: Vec<String>,
}

impl State {
    /// The state at the current time (`KONTEXT_NOW` or the clock).
    pub fn build(entries: impl IntoIterator<Item = Entry>, policy: &Policy) -> State {
        State::build_at(entries, policy, tasks::now_ms())
    }

    /// The state as seen at the instant `now_ms` (for claims that expire).
    pub fn build_at(
        entries: impl IntoIterator<Item = Entry>,
        policy: &Policy,
        now_ms: i64,
    ) -> State {
        let mut all: Vec<Entry> = entries.into_iter().collect();
        all.sort_by(|a, b| (&a.hlc, &a.id).cmp(&(&b.hlc, &b.id)));
        let mut st = State {
            policy: policy.clone(),
            now_ms,
            ..State::default()
        };
        for e in all {
            if st.entries.contains_key(&e.id) {
                continue;
            }
            st.apply(&e);
            if let Some(v0) = e.fields.get("v0_id").and_then(Value::as_str) {
                st.v0_ids
                    .entry(v0.trim().to_uppercase())
                    .or_insert_with(|| e.id.clone());
            }
            st.order.push(e.id.clone());
            st.entries.insert(e.id.clone(), e);
        }
        st.finish();
        st
    }

    pub fn get(&self, id: &str) -> Option<&Entry> {
        self.entries.get(id)
    }

    pub fn status_of(&self, id: &str) -> Option<Status> {
        self.status.get(id).copied()
    }

    pub fn kind_of(&self, id: &str) -> Option<&str> {
        self.entries.get(id).map(|e| e.kind.as_str())
    }

    pub fn claim_of(&self, task: &str) -> Option<&Claim> {
        self.claims.get(task)
    }

    pub fn task(&self, id: &str) -> Option<&TaskInfo> {
        self.tasks.get(id)
    }

    pub fn task_status(&self, id: &str) -> Option<TaskStatus> {
        self.tasks.get(id).map(|t| t.status)
    }

    pub fn feature(&self, id: &str) -> Option<&crate::features::FeatureInfo> {
        self.features.get(id)
    }

    /// Epics in force, in order.
    pub fn epics(&self) -> Vec<&Entry> {
        self.with_status("epic", &[Status::Active])
    }

    /// The epic of a task.
    pub fn epic_of(&self, task: &Entry) -> Option<&Entry> {
        task.fields
            .get("epic")
            .and_then(Value::as_str)
            .and_then(|e| self.get(e))
            .filter(|e| e.kind == "epic")
    }

    /// The tasks of an epic, in `hlc` order.
    pub fn tasks_of(&self, epic: &str) -> Vec<&Entry> {
        self.of_kind("task")
            .filter(|t| t.fields.get("epic").and_then(Value::as_str) == Some(epic))
            .collect()
    }

    /// An epic from `E-…`, `epic:<name>` or `<name>`.
    pub fn find_epic(&self, s: &str) -> Result<String> {
        let s = s.trim();
        let name = s.strip_prefix("epic:").unwrap_or(s);
        let by_name: Vec<&Entry> = self
            .of_kind("epic")
            .filter(|e| tasks::epic_name(e) == name)
            .collect();
        if let Some(e) = by_name
            .iter()
            .rev()
            .find(|e| self.status_of(&e.id) == Some(Status::Active))
            .or(by_name.last())
        {
            return Ok(e.id.clone());
        }
        let id = self
            .resolve(name)
            .map_err(|_| anyhow::anyhow!("there is no epic {s:?} (neither by name nor by id)"))?;
        if self.kind_of(&id) != Some("epic") {
            bail!("{id} is {}, not an epic", self.kind_of(&id).unwrap_or("?"));
        }
        Ok(id)
    }

    pub fn counts(&self, id: &str) -> bool {
        !self.not_counting.contains_key(id)
    }

    /// A full `id` from a unique prefix (`D-7f3`, `d-7f3a9c`), or from the id
    /// an entry had in v0 (`D-0030`, kept in `fields.v0_id`). The v0 id wins
    /// over a prefix: `D-0030` is also a prefix of `D-0030a1`.
    pub fn resolve(&self, s: &str) -> Result<String> {
        let s = s.trim();
        if s.is_empty() {
            bail!("missing id");
        }
        if self.entries.contains_key(s) {
            return Ok(s.to_string());
        }
        if let Some(id) = self.v0_ids.get(&s.to_uppercase()) {
            return Ok(id.clone());
        }
        let up = match s.split_once('-') {
            Some((p, rest)) => format!("{}-{}", p.to_uppercase(), rest.to_lowercase()),
            None => s.to_uppercase(),
        };
        let hits: Vec<&String> = self
            .entries
            .range(up.clone()..)
            .take_while(|(k, _)| k.starts_with(&up))
            .map(|(k, _)| k)
            .collect();
        match hits.as_slice() {
            [one] => Ok((*one).clone()),
            [] => bail!("there is no entry with id {s}"),
            many => bail!(
                "{s} is ambiguous: it could be {}. Type more characters",
                many.iter()
                    .take(8)
                    .map(|x| x.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }

    /// The entries of a kind, in `hlc` order.
    pub fn of_kind<'a>(&'a self, kind: &str) -> impl Iterator<Item = &'a Entry> + use<'a> {
        let kind = kind.to_string();
        self.order
            .iter()
            .filter_map(|id| self.entries.get(id))
            .filter(move |e| e.kind == kind)
    }

    /// Is it law? A decision in force, or an accepted proposal that did not
    /// become a decision (as in v0).
    pub fn is_law(&self, id: &str) -> bool {
        match (self.kind_of(id), self.status_of(id)) {
            (Some("decision"), Some(Status::Accepted)) => true,
            (Some("proposal"), Some(Status::Accepted)) => !self.promoted_to.contains_key(id),
            _ => false,
        }
    }

    /// Decisions in force and accepted proposals not promoted, in order.
    pub fn law(&self) -> Vec<&Entry> {
        self.order
            .iter()
            .filter(|id| self.is_law(id))
            .filter_map(|id| self.entries.get(id))
            .collect()
    }

    pub fn charter(&self) -> Option<&Entry> {
        self.active_charter.as_ref().and_then(|id| self.get(id))
    }

    /// The last charter not superseded, signed or not.
    pub fn latest_charter(&self) -> Option<&Entry> {
        self.of_kind("charter")
            .filter(|e| {
                matches!(
                    self.status_of(&e.id),
                    Some(Status::Signed | Status::Unsigned)
                )
            })
            .last()
    }

    /// Signed charters still in force (usually one).
    pub fn signed_charters(&self) -> Vec<&Entry> {
        self.with_status("charter", &[Status::Signed])
    }

    /// Unsigned charters that were not superseded.
    pub fn unsigned_charters(&self) -> Vec<&Entry> {
        self.with_status("charter", &[Status::Unsigned])
    }

    /// Goals in force (signed or to be signed).
    pub fn goals(&self) -> Vec<&Entry> {
        self.with_status("goal", &[Status::Signed, Status::Unsigned])
    }

    pub fn pending_proposals(&self) -> Vec<&Entry> {
        self.with_status("proposal", &[Status::Pending])
    }

    pub fn open_questions(&self) -> Vec<&Entry> {
        self.with_status("question", &[Status::Open])
    }

    pub fn facts(&self) -> Vec<&Entry> {
        self.with_status("fact", &[Status::Active])
    }

    /// Facts that still hold: active, and with no fingerprint that went stale.
    pub fn live_facts(&self) -> Vec<&Entry> {
        self.facts()
            .into_iter()
            .filter(|f| crate::facts::is_live(self, &f.id))
            .collect()
    }

    pub fn journal(&self) -> Vec<&Entry> {
        self.of_kind("journal").collect()
    }

    pub fn with_status(&self, kind: &str, statuses: &[Status]) -> Vec<&Entry> {
        self.of_kind(kind)
            .filter(|e| self.status_of(&e.id).is_some_and(|s| statuses.contains(&s)))
            .collect()
    }

    /// The entries that cite `id` (in `refs` or in `fields.supersedes`), in
    /// order.
    pub fn referrers(&self, id: &str) -> Vec<&Entry> {
        self.order
            .iter()
            .filter_map(|x| self.entries.get(x))
            .filter(|e| {
                e.id != id
                    && (e.refs.iter().any(|r| r == id)
                        || string_list(e.fields.get("supersedes"))
                            .iter()
                            .any(|r| r == id))
            })
            .collect()
    }

    /// What `id` superseded (directly).
    pub fn supersedes_of(&self, id: &str) -> Vec<String> {
        self.supersessions
            .iter()
            .filter(|(_, by)| by.iter().any(|b| b == id))
            .map(|(t, _)| t.clone())
            .collect()
    }

    /// The supersession chain around `id`, from the oldest to the newest
    /// (includes `id`).
    pub fn supersede_chain(&self, id: &str) -> Vec<String> {
        let mut back = Vec::new();
        let mut seen = BTreeSet::from([id.to_string()]);
        let mut stack = vec![id.to_string()];
        while let Some(x) = stack.pop() {
            for p in self.supersedes_of(&x) {
                if seen.insert(p.clone()) {
                    back.push(p.clone());
                    stack.push(p);
                }
            }
        }
        back.sort_by_key(|x| self.position(x));
        back.push(id.to_string());
        let mut next = self.superseded_by.get(id).cloned();
        while let Some(n) = next {
            if !seen.insert(n.clone()) {
                break;
            }
            next = self.superseded_by.get(&n).cloned();
            back.push(n);
        }
        back
    }

    fn position(&self, id: &str) -> usize {
        self.order
            .iter()
            .position(|x| x == id)
            .unwrap_or(usize::MAX)
    }

    fn set(&mut self, id: &str, s: Status) {
        if let Some(cur) = self.status.get_mut(id) {
            *cur = s;
        }
    }

    /// Checks the owner rule. If it does not count, records it as an alert.
    fn authorize(&mut self, e: &Entry) -> bool {
        match self.policy.check_entry(e) {
            Ok(())
                if self.policy.require_signatures && crate::sign::wants_signature(e).is_some() =>
            {
                let reason = match self.policy.signatures.get(&e.id) {
                    Some(crate::sign::Verdict::Valid) => return true,
                    Some(crate::sign::Verdict::Invalid(r)) => r.clone(),
                    None => "the signature was not checked".to_string(),
                };
                self.not_counting.insert(e.id.clone(), reason.clone());
                self.alerts.push(Alert::BadSignature {
                    id: e.id.clone(),
                    kind: e.kind.clone(),
                    actor: e.actor.short(),
                    reason,
                });
                if !is_record_kind(&e.kind) {
                    self.status.insert(e.id.clone(), Status::Ignored);
                }
                false
            }
            Ok(()) => true,
            Err(reason) => {
                self.not_counting.insert(e.id.clone(), reason.clone());
                self.alerts.push(Alert::NotCounting {
                    id: e.id.clone(),
                    kind: e.kind.clone(),
                    actor: e.actor.short(),
                    reason,
                });
                if !is_record_kind(&e.kind) {
                    self.status.insert(e.id.clone(), Status::Ignored);
                }
                false
            }
        }
    }

    /// Does `e` (a fact or a proposal) supersede `old` right away? Yes if `old`
    /// is a fact in force, or a pending proposal from the same actor, or from
    /// another agent if `e` is also from an agent, or from anyone if `e` meets
    /// the owner rule. A human's proposal is only superseded by that human or an
    /// owner. Never a decision, a charter or a goal: that is still an owner's call.
    fn replaceable_now(&self, e: &Entry, old: &str) -> bool {
        let Some(target) = self.get(old) else {
            return false;
        };
        match (target.kind.as_str(), self.status_of(old)) {
            ("fact", Some(Status::Active)) => true,
            ("proposal", Some(Status::Pending)) => {
                (target.actor.kind == e.actor.kind && target.actor.name == e.actor.name)
                    || (target.actor.is_agent() && e.actor.is_agent())
                    || self.policy.check_entry(e).is_ok()
            }
            _ => false,
        }
    }

    fn supersede(&mut self, target: &str, by: &str) {
        if target == by || !self.status.contains_key(target) {
            return;
        }
        let list = self.supersessions.entry(target.to_string()).or_default();
        if !list.iter().any(|b| b == by) {
            list.push(by.to_string());
        }
        self.set(target, Status::Superseded);
        self.superseded_by
            .insert(target.to_string(), by.to_string());
    }

    fn fields_list(&self, id: &str, key: &str) -> Vec<String> {
        self.get(id)
            .map(|x| string_list(x.fields.get(key)))
            .unwrap_or_default()
    }

    fn apply(&mut self, e: &Entry) {
        if !is_record_kind(&e.kind) {
            let initial = match e.kind.as_str() {
                "charter" | "goal" => Status::Unsigned,
                "decision" => Status::Accepted,
                "proposal" => Status::Pending,
                "question" | "task" => Status::Open,
                _ => Status::Active,
            };
            self.status.insert(e.id.clone(), initial);
            if e.kind == "task" {
                self.tasks.insert(e.id.clone(), TaskInfo::default());
                // A task added to a feature after it was validated: the
                // validation no longer covers all its work.
                if let Some(f) = crate::features::feature_of(e).map(str::to_string) {
                    self.unvalidate(&f, e, format!("{} was added after the validation", e.id));
                }
            }
            if e.kind == "feature" {
                self.features
                    .insert(e.id.clone(), crate::features::FeatureInfo::default());
                if let Err(reason) = crate::features::may_write(self, &e.actor) {
                    self.not_counting.insert(e.id.clone(), reason.clone());
                    self.alerts.push(Alert::NotCounting {
                        id: e.id.clone(),
                        kind: e.kind.clone(),
                        actor: e.actor.short(),
                        reason,
                    });
                    self.status.insert(e.id.clone(), Status::Ignored);
                }
            }
            if e.kind == "journal" {
                self.touch_task(e);
            }
            if e.kind == "decision" && self.authorize(e) {
                for old in string_list(e.fields.get("supersedes")) {
                    self.supersede(&old, &e.id);
                }
            }
            if matches!(e.kind.as_str(), "fact" | "proposal") {
                // A fact or a proposal that updates other data supersedes it
                // right away if that data is not law: a fact, or a pending proposal from the
                // same actor (or from anyone, if the writer is an owner).
                // Something that supersedes a decision waits until it is accepted.
                for old in string_list(e.fields.get("supersedes")) {
                    if self.replaceable_now(e, &old) {
                        self.supersede(&old, &e.id);
                    }
                }
            }
            return;
        }
        if e.kind == "dismiss" || (e.kind == "confirm" && e.fields.contains_key("items")) {
            return self.close_inbox_items(e);
        }
        let Some(target) = e.refs.first().cloned() else {
            return;
        };
        let target_kind = self.kind_of(&target).map(str::to_string);
        let target_status = self.status_of(&target);
        match e.kind.as_str() {
            "sign" => {
                if !matches!(target_kind.as_deref(), Some("charter" | "goal")) {
                    return;
                }
                if !self.authorize(e) || target_status != Some(Status::Unsigned) {
                    return;
                }
                self.set(&target, Status::Signed);
                self.signed_by.insert(target.clone(), e.id.clone());
                if target_kind.as_deref() == Some("charter") {
                    for old in self.fields_list(&target, "supersedes") {
                        self.supersede(&old, &target);
                    }
                    self.sign_order.push(target);
                }
            }
            "accept" => {
                if target_kind.as_deref() != Some("proposal") || !self.authorize(e) {
                    return;
                }
                if target_status != Some(Status::Pending) {
                    return;
                }
                self.set(&target, Status::Accepted);
                self.accepted_by.insert(target.clone(), e.id.clone());
                match e.fields.get("decision_id").and_then(Value::as_str) {
                    Some(d) => {
                        self.promoted_to.insert(target.clone(), d.to_string());
                    }
                    None => {
                        // The proposal itself becomes law and supersedes what it names.
                        for old in self.fields_list(&target, "supersedes") {
                            self.supersede(&old, &target);
                        }
                    }
                }
            }
            "reject" => {
                if target_kind.as_deref() != Some("proposal") || !self.authorize(e) {
                    return;
                }
                if target_status == Some(Status::Pending) {
                    self.set(&target, Status::Rejected);
                    self.rejected_by.insert(target, e.id.clone());
                }
            }
            "answer" => {
                if target_kind.as_deref() == Some("question") {
                    self.set(&target, Status::Answered);
                    self.answers.entry(target).or_default().push(e.id.clone());
                }
            }
            "supersede" if target_kind.as_deref() == Some("feature") => {
                // A feature replaced by another: the pm or an owner.
                if let Err(reason) = crate::features::may_write(self, &e.actor) {
                    self.not_counting.insert(e.id.clone(), reason.clone());
                    self.alerts.push(Alert::NotCounting {
                        id: e.id.clone(),
                        kind: e.kind.clone(),
                        actor: e.actor.short(),
                        reason,
                    });
                    return;
                }
                match e.refs.get(1) {
                    Some(by) => self.supersede(&target, by),
                    None => self.set(&target, Status::Superseded),
                }
            }
            "supersede" => {
                let restricted = matches!(
                    target_kind.as_deref(),
                    Some("charter" | "goal" | "decision" | "proposal")
                );
                if target_kind.is_none() || (restricted && !self.authorize(e)) {
                    return;
                }
                match e.refs.get(1) {
                    Some(by) => self.supersede(&target, by),
                    None => self.set(&target, Status::Superseded),
                }
            }
            "assign" | "claim" | "release" | "done" | "reopen"
                if target_kind.as_deref() == Some("task") =>
            {
                self.apply_task(e, &target);
            }
            "done" | "reopen" if target_kind.as_deref() == Some("feature") => {
                self.apply_feature(e, &target);
            }
            "confirm" if target_kind.as_deref() == Some("question") => {
                self.joined.entry(target).or_default().push(e.id.clone());
            }
            _ => {}
        }
    }

    /// `dismiss` (and the `confirm` with `fields.items` for what the distill
    /// noted): closes items in the inbox. It only counts if a human writes it:
    /// seeing it closes nothing, and an agent cannot close them on the human's behalf.
    fn close_inbox_items(&mut self, e: &Entry) {
        if !e.actor.is_human() {
            self.not_counting.insert(
                e.id.clone(),
                format!(
                    "only the human can close their pending items ({} is not one)",
                    e.actor.short()
                ),
            );
            return;
        }
        let mut items = string_list(e.fields.get("items"));
        if items.is_empty() {
            items = e.refs.clone();
        }
        for k in items {
            self.inbox_closed.entry(k).or_insert_with(|| e.id.clone());
        }
    }

    fn event(&mut self, task: &str, e: &Entry, counted: bool, note: String) {
        if let Some(t) = self.tasks.get_mut(task) {
            t.history.push(TaskEvent {
                record: e.id.clone(),
                kind: e.kind.clone(),
                actor: e.actor.short(),
                hlc: e.hlc.clone(),
                counted,
                note,
            });
        }
    }

    /// An entry that does not count because of who wrote it or what it contains:
    /// it stays in the history and shows up as an alert.
    fn task_ignored(&mut self, task: &str, e: &Entry, reason: String) {
        self.not_counting.insert(e.id.clone(), reason.clone());
        self.alerts.push(Alert::TaskRecordIgnored {
            id: e.id.clone(),
            task: task.to_string(),
            kind: e.kind.clone(),
            actor: e.actor.short(),
            reason: reason.clone(),
        });
        self.event(task, e, false, format!("does not count: {reason}"));
    }

    /// A journal or a close from whoever holds the task renews their claim.
    fn touch_task(&mut self, e: &Entry) {
        let Some(task) = e.fields.get("task").and_then(Value::as_str) else {
            return;
        };
        let ms = tasks::hlc_ms(&e.hlc);
        if let Some(t) = self.tasks.get_mut(task)
            && let Some(h) = &t.holder
            && tasks::holder_matches(&h.actor, &e.actor)
        {
            t.last_activity = t.last_activity.max(ms);
        }
    }

    fn apply_task(&mut self, e: &Entry, task: &str) {
        let epic = self.get(task).and_then(|t| self.epic_of(t)).cloned();
        let manages = self.policy.manages(&e.actor, epic.as_ref());
        let lease = self.policy.lease_ms;
        let ms = tasks::hlc_ms(&e.hlc);
        let info = self.tasks.get(task).cloned().unwrap_or_default();
        let holds = info
            .holder
            .as_ref()
            .is_some_and(|h| tasks::holder_matches(&h.actor, &e.actor));
        let new_claim = |who: String| Claim {
            record: e.id.clone(),
            kind: e.kind.clone(),
            actor: who,
            hlc: e.hlc.clone(),
        };
        match e.kind.as_str() {
            "assign" => {
                let to = e
                    .fields
                    .get("to")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .unwrap_or_default()
                    .to_string();
                if !manages {
                    let reason = format!(
                        "only the epic's coordinator, the pm or an owner can assign ({} is not one)",
                        e.actor.short()
                    );
                    return self.task_ignored(task, e, reason);
                }
                if to.is_empty() {
                    return self.task_ignored(task, e, "does not say to whom (fields.to)".into());
                }
                if info.done.is_some() {
                    return self.event(task, e, false, "does not count: already closed".into());
                }
                self.hold(task, new_claim(to.clone()), ms);
                self.event(task, e, true, format!("assigned to {to}"));
            }
            "claim" => {
                let who = format!("{}:{}", e.actor.kind, e.actor.name);
                if info.done.is_some() {
                    return self.event(task, e, false, "does not count: already closed".into());
                }
                match &info.holder {
                    Some(_) if holds => {
                        if let Some(t) = self.tasks.get_mut(task) {
                            t.last_activity = t.last_activity.max(ms);
                        }
                        self.event(task, e, true, format!("{who} renewed their claim"));
                    }
                    Some(h) if h.kind == "claim" && info.last_activity + lease < ms => {
                        let old = h.actor.clone();
                        self.hold(task, new_claim(who.clone()), ms);
                        self.event(
                            task,
                            e,
                            true,
                            format!("claimed by {who} (the claim of {old} had expired)"),
                        );
                    }
                    Some(h) => {
                        let note = format!(
                            "does not count: already held by {} ({}, the lower hlc wins)",
                            h.actor, h.record
                        );
                        // From another machine (phase 5): whoever lost did not know
                        // when claiming it; they are told until it closes.
                        let winner_writer = self
                            .get(&h.record)
                            .map(|w| w.writer.clone())
                            .unwrap_or_default();
                        if !winner_writer.is_empty() && winner_writer != e.writer {
                            self.alerts.push(Alert::ClaimLost {
                                task: task.to_string(),
                                record: e.id.clone(),
                                loser: who.clone(),
                                winner: h.actor.clone(),
                                winner_writer,
                            });
                        }
                        self.event(task, e, false, note);
                    }
                    None => {
                        self.hold(task, new_claim(who.clone()), ms);
                        self.event(task, e, true, format!("claimed by {who}"));
                    }
                }
            }
            "release" => {
                let Some(h) = info.holder.clone() else {
                    return self.event(task, e, false, "does not count: it had no holder".into());
                };
                if info.done.is_some() {
                    return self.event(task, e, false, "does not count: already closed".into());
                }
                if !holds && !manages {
                    let reason = format!(
                        "only whoever holds it ({}), the epic's coordinator, the pm or an owner can release it",
                        h.actor
                    );
                    return self.task_ignored(task, e, reason);
                }
                self.unhold(task);
                self.event(
                    task,
                    e,
                    true,
                    format!("released (it was held by {})", h.actor),
                );
            }
            "done" => {
                if info.done.is_some() {
                    return self.event(task, e, false, "does not count: already closed".into());
                }
                if !holds && !manages {
                    let who = info
                        .holder
                        .as_ref()
                        .map(|h| format!("held by {}", h.actor))
                        .unwrap_or_else(|| "nobody holds it".into());
                    let reason = format!(
                        "only whoever holds it ({who}), the epic's coordinator, the pm or an owner can close it"
                    );
                    return self.task_ignored(task, e, reason);
                }
                if let Err(reason) = tasks::check_evidence(e.fields.get("evidence")) {
                    return self.task_ignored(task, e, reason);
                }
                if let Some(t) = self.tasks.get_mut(task) {
                    t.done = Some(e.id.clone());
                    if holds {
                        t.last_activity = t.last_activity.max(ms);
                    }
                }
                self.set(task, Status::Done);
                let summary = tasks::evidence_summary(e.fields.get("evidence"));
                self.event(task, e, true, format!("closed with evidence: {summary}"));
            }
            "reopen" => {
                if info.done.is_none() {
                    return self.event(task, e, false, "does not count: it was not closed".into());
                }
                if let Some(t) = self.tasks.get_mut(task) {
                    t.done = None;
                }
                self.unhold(task);
                self.event(task, e, true, "reopened".into());
                let feature = self
                    .get(task)
                    .and_then(crate::features::feature_of)
                    .map(str::to_string);
                if let Some(f) = feature {
                    self.unvalidate(&f, e, format!("{task} was reopened after the validation"));
                }
            }
            _ => {}
        }
    }

    fn feature_event(&mut self, f: &str, e: &Entry, counted: bool, note: String) {
        if let Some(i) = self.features.get_mut(f) {
            i.history.push(TaskEvent {
                record: e.id.clone(),
                kind: e.kind.clone(),
                actor: e.actor.short(),
                hlc: e.hlc.clone(),
                counted,
                note,
            });
        }
    }

    /// A validation or reopening of a feature that does not count: it stays in
    /// the feature's history and shows up as an alert, like a task's.
    fn feature_ignored(&mut self, f: &str, e: &Entry, reason: String) {
        self.not_counting.insert(e.id.clone(), reason.clone());
        self.alerts.push(Alert::TaskRecordIgnored {
            id: e.id.clone(),
            task: f.to_string(),
            kind: e.kind.clone(),
            actor: e.actor.short(),
            reason: reason.clone(),
        });
        self.feature_event(f, e, false, format!("does not count: {reason}"));
    }

    /// A feature's validation stops holding (a task reopened or added after
    /// it). Nothing is written: the state says so, with the reason.
    fn unvalidate(&mut self, f: &str, e: &Entry, note: String) {
        let Some(i) = self.features.get_mut(f) else {
            return;
        };
        let Some(v) = i.validated.take() else {
            return;
        };
        let note = format!("{note} ({v}); validate it again when its tasks are closed");
        i.note = Some(note.clone());
        self.feature_event(f, e, true, note);
    }

    /// `done` (validate) and `reopen` on a feature.
    fn apply_feature(&mut self, e: &Entry, f: &str) {
        let info = self.features.get(f).cloned().unwrap_or_default();
        match e.kind.as_str() {
            "done" => {
                if info.validated.is_some() {
                    return self.feature_event(
                        f,
                        e,
                        false,
                        "does not count: already validated".into(),
                    );
                }
                if let Some(s @ (Status::Superseded | Status::Ignored)) = self.status_of(f) {
                    let reason = format!("the feature is {}", s.label());
                    return self.feature_ignored(f, e, reason);
                }
                let epic = self.get(f).and_then(|x| self.epic_of(x)).cloned();
                if !self.policy.manages(&e.actor, epic.as_ref()) {
                    let reason = format!(
                        "only the epic's coordinator, the pm or an owner can validate a feature ({} is not one)",
                        e.actor.short()
                    );
                    return self.feature_ignored(f, e, reason);
                }
                if let Some(why) = crate::features::unready(self, f) {
                    return self.feature_ignored(f, e, format!("{why} when it was validated"));
                }
                if let Err(reason) = tasks::check_evidence(e.fields.get("evidence")) {
                    return self.feature_ignored(f, e, reason);
                }
                if let Some(i) = self.features.get_mut(f) {
                    i.validated = Some(e.id.clone());
                    i.note = None;
                }
                let summary = tasks::evidence_summary(e.fields.get("evidence"));
                self.feature_event(f, e, true, format!("validated with evidence: {summary}"));
            }
            "reopen" => {
                if info.validated.is_none() {
                    return self.feature_event(
                        f,
                        e,
                        false,
                        "does not count: it was not validated".into(),
                    );
                }
                if let Some(i) = self.features.get_mut(f) {
                    i.validated = None;
                    i.note = None;
                }
                self.feature_event(f, e, true, "validation reopened".into());
            }
            _ => {}
        }
    }

    /// The final state of each feature, once its tasks' states are known.
    fn finish_features(&mut self) {
        let ids: Vec<String> = self.features.keys().cloned().collect();
        for id in ids {
            let tasks = crate::features::task_ids(self, &id);
            let closed = tasks
                .iter()
                .filter(|t| self.task_status(t) == Some(TaskStatus::Closed))
                .count();
            let mut info = self.features[&id].clone();
            info.tasks = tasks;
            info.closed_tasks = closed;
            info.status = crate::features::derive(self, &id, &info);
            self.features.insert(id, info);
        }
    }

    fn hold(&mut self, task: &str, c: Claim, ms: i64) {
        if let Some(t) = self.tasks.get_mut(task) {
            t.holder = Some(c.clone());
            t.last_activity = ms;
            t.expired = None;
        }
        self.claims.insert(task.to_string(), c);
        self.set(task, Status::Claimed);
    }

    fn unhold(&mut self, task: &str) {
        if let Some(t) = self.tasks.get_mut(task) {
            t.holder = None;
        }
        self.claims.remove(task);
        self.set(task, Status::Open);
    }

    /// Final status of each task (with its dependencies already seen, and the
    /// claim expired or not according to `now_ms`) and the drift.
    fn finish_tasks(&mut self) {
        let ids: Vec<String> = self.of_kind("task").map(|t| t.id.clone()).collect();
        let lease = self.policy.lease_ms;
        let now = self.now_ms;
        let mut expired = Vec::new();
        for id in &ids {
            let t = self.tasks.get_mut(id).unwrap();
            if t.done.is_none()
                && let Some(h) = &t.holder
                && h.kind == "claim"
                && t.last_activity + lease < now
            {
                t.expired = t.holder.take();
                expired.push(id.clone());
            }
        }
        for id in &expired {
            let t = &self.tasks[id];
            let h = t.expired.clone().unwrap();
            let since = tasks::ms_text(t.last_activity);
            self.claims.remove(id);
            self.set(id, Status::Open);
            self.alerts.push(Alert::ClaimExpired {
                task: id.clone(),
                holder: h.actor,
                since,
            });
        }
        let done: BTreeSet<String> = ids
            .iter()
            .filter(|id| self.tasks[*id].done.is_some())
            .cloned()
            .collect();
        for id in &ids {
            let deps = self.fields_list(id, "depends_on");
            let t = self.tasks.get_mut(id).unwrap();
            t.blocked_by = deps.into_iter().filter(|d| !done.contains(d)).collect();
            t.status = if t.done.is_some() {
                TaskStatus::Closed
            } else if let Some(h) = &t.holder {
                if h.kind == "assign" {
                    TaskStatus::Assigned
                } else {
                    TaskStatus::Claimed
                }
            } else if t.blocked_by.is_empty() {
                TaskStatus::Ready
            } else {
                TaskStatus::Blocked
            };
        }
        // Drift: open tasks and epics in force.
        let mut drift = Vec::new();
        for e in self.of_kind("task").chain(self.of_kind("epic")) {
            if e.kind == "task" && self.task_status(&e.id) == Some(TaskStatus::Closed) {
                continue;
            }
            if e.kind == "epic" && self.status_of(&e.id) != Some(Status::Active) {
                continue;
            }
            let goals = string_list(e.fields.get("goals"));
            if goals.is_empty() {
                drift.push(Alert::Drift {
                    id: e.id.clone(),
                    kind: e.kind.clone(),
                    reason: NO_GOAL_REASON.into(),
                });
            }
            for g in goals {
                let reason = match (self.kind_of(&g), self.status_of(&g)) {
                    (Some("goal"), Some(Status::Signed)) => continue,
                    (Some("goal"), Some(Status::Unsigned)) => {
                        drift.push(Alert::UnsignedGoal {
                            id: e.id.clone(),
                            kind: e.kind.clone(),
                            goal: g.clone(),
                        });
                        continue;
                    }
                    (Some("goal"), Some(Status::Superseded)) => format!(
                        "cites {g}, which was superseded by {}",
                        self.superseded_by
                            .get(&g)
                            .map(String::as_str)
                            .unwrap_or("?")
                    ),
                    (Some("goal"), Some(s)) => format!("cites {g}, which is {}", s.label()),
                    _ => format!("cites {g}, which is not a goal in the ledger"),
                };
                drift.push(Alert::Drift {
                    id: e.id.clone(),
                    kind: e.kind.clone(),
                    reason,
                });
            }
        }
        self.alerts.extend(drift);
    }

    /// Was the decision `d` made from the proposal `p`? As `kontext accept
    /// --as-decision` records it: the `accept` of `p` points to `d`
    /// (`fields.decision_id`), and `d` says `fields.from_proposal = p`.
    /// Either one is enough (v0 wrote both too).
    fn made_from(&self, d: &str, p: &str) -> bool {
        self.promoted_to.get(p).is_some_and(|x| x == d)
            || self.get(d).is_some_and(|e| {
                e.kind == "decision"
                    && e.fields.get("from_proposal").and_then(Value::as_str) == Some(p)
            })
    }

    /// What is computed once the whole ledger has been walked: the charter in
    /// force and the conflicts.
    fn finish(&mut self) {
        let mut live: Vec<String> = Vec::new();
        for c in self.sign_order.iter().rev() {
            if self.status_of(c) == Some(Status::Signed) && !live.contains(c) {
                live.push(c.clone());
            }
        }
        self.active_charter = live.first().cloned();
        if live.len() > 1 {
            self.alerts.push(Alert::CharterConflict {
                active: live[0].clone(),
                others: live[1..].to_vec(),
            });
        }
        let mut conflicts = Vec::new();
        for (target, by) in &self.supersessions {
            let alive: Vec<String> = by
                .iter()
                .filter(|b| {
                    !matches!(
                        self.status_of(b),
                        Some(Status::Superseded | Status::Rejected | Status::Ignored)
                    )
                })
                .cloned()
                .collect();
            // A proposal and the decision made from it are one superseder.
            let alive: Vec<String> = alive
                .iter()
                .filter(|p| !alive.iter().any(|d| self.made_from(d, p)))
                .cloned()
                .collect();
            if alive.len() > 1 {
                conflicts.push(Alert::Conflict {
                    target: target.clone(),
                    by: alive,
                });
            }
        }
        self.alerts.extend(conflicts);
        // A pending proposal that would supersede a decision in force.
        let mut contradictions = Vec::new();
        for p in self.pending_proposals() {
            for d in string_list(p.fields.get("supersedes")) {
                if self.is_law(&d) {
                    contradictions.push(Alert::ProposalSupersedes {
                        proposal: p.id.clone(),
                        decision: d,
                    });
                }
            }
        }
        self.alerts.extend(contradictions);
        self.finish_tasks();
        self.finish_features();
        // A lost claim stops mattering when the task closes, or when the loser
        // holds it after all.
        let tasks = &self.tasks;
        self.alerts.retain(|a| match a {
            Alert::ClaimLost { task, loser, .. } => tasks.get(task).is_some_and(|t| {
                t.done.is_none() && t.holder.as_ref().is_none_or(|h| &h.actor != loser)
            }),
            _ => true,
        });
    }
}

pub fn string_list(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|x| x.as_str().map(str::to_string))
            .collect(),
        Some(Value::String(s)) if !s.is_empty() => vec![s.clone()],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Actor, GENESIS};
    use crate::tasks::TaskStatus;
    use serde_json::{Map, json};

    fn e(id: &str, hlc: &str, kind: &str, refs: &[&str]) -> Entry {
        Entry {
            v: 1,
            id: id.into(),
            writer: "w".into(),
            n: 1,
            prev: GENESIS.into(),
            hash: String::new(),
            hlc: format!("2026-11-03T18:22:10.{hlc}Z.0000"),
            actor: Actor::human("omar"),
            kind: kind.into(),
            title: String::new(),
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

    fn pm() -> Actor {
        let mut a = Actor::agent("pm");
        a.role = Some("pm".into());
        a
    }

    /// The test entries are from 2026-11-03T18:22:10; "now" is one minute
    /// later, so the claims do not expire according to the real clock.
    const NOW: &str = "2026-11-03T18:23:10Z";

    fn now_ms(s: &str) -> i64 {
        s.parse::<jiff::Timestamp>().unwrap().as_millisecond()
    }

    fn build(entries: Vec<Entry>) -> State {
        State::build_at(entries, &Policy::new(&["omar"]), now_ms(NOW))
    }

    #[test]
    fn records_change_status_in_hlc_order() {
        // They arrive out of order: the state comes out in `hlc` order.
        let st = build(vec![
            e("R-000002", "002", "answer", &["Q-000001"]),
            e("Q-000001", "001", "question", &[]),
            e("P-000001", "003", "proposal", &[]),
            e("R-000003", "004", "reject", &["P-000001"]),
            e("D-000001", "005", "decision", &[]),
            e("D-000002", "006", "decision", &[]),
            e("R-000004", "007", "supersede", &["D-000001", "D-000002"]),
        ]);
        assert_eq!(st.status_of("Q-000001"), Some(Status::Answered));
        assert_eq!(st.answers["Q-000001"], vec!["R-000002"]);
        assert_eq!(st.status_of("P-000001"), Some(Status::Rejected));
        assert_eq!(st.status_of("D-000001"), Some(Status::Superseded));
        assert_eq!(st.superseded_by["D-000001"], "D-000002");
        assert_eq!(st.status_of("D-000002"), Some(Status::Accepted));
        assert_eq!(st.order[0], "Q-000001");
        assert!(st.alerts.is_empty(), "{:?}", st.alerts);
    }

    #[test]
    fn first_claim_by_hlc_wins() {
        let st = build(vec![
            e("T-000001", "001", "task", &[]),
            e("R-00000b", "003", "claim", &["T-000001"]),
            e("R-00000a", "002", "claim", &["T-000001"]),
        ]);
        assert_eq!(st.claim_of("T-000001").unwrap().record, "R-00000a");
        assert_eq!(st.status_of("T-000001"), Some(Status::Claimed));
        let st = build(vec![
            e("T-000001", "001", "task", &[]),
            e("R-00000a", "002", "claim", &["T-000001"]),
            e("R-00000c", "004", "release", &["T-000001"]),
        ]);
        assert!(st.claim_of("T-000001").is_none());
        assert_eq!(st.status_of("T-000001"), Some(Status::Open));
    }

    #[test]
    fn owner_rule() {
        let st = build(vec![
            // Any agent decides: does not count.
            by(e("D-00000a", "001", "decision", &[]), Actor::agent("x")),
            // The pm with approved_by from an owner: counts.
            by(
                with(
                    e("D-00000b", "002", "decision", &[]),
                    "approved_by",
                    json!("omar"),
                ),
                pm(),
            ),
            // The pm with approved_by from someone who is not an owner: does not count.
            by(
                with(
                    e("D-00000c", "003", "decision", &[]),
                    "approved_by",
                    json!("ana"),
                ),
                pm(),
            ),
            // A human who is not an owner signs: stored, but does not count.
            e("A-000001", "004", "charter", &[]),
            by(
                e("R-000001", "005", "sign", &["A-000001"]),
                Actor::human("ana"),
            ),
        ]);
        assert_eq!(st.status_of("D-00000a"), Some(Status::Ignored));
        assert!(st.is_law("D-00000b"));
        assert_eq!(st.status_of("D-00000c"), Some(Status::Ignored));
        assert_eq!(st.status_of("A-000001"), Some(Status::Unsigned));
        assert!(st.charter().is_none());
        assert!(st.entries.contains_key("R-000001"));
        let not: Vec<&str> = st
            .alerts
            .iter()
            .filter_map(|a| match a {
                Alert::NotCounting { id, .. } => Some(id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(not, vec!["D-00000a", "D-00000c", "R-000001"]);
        assert!(st.alerts[0].to_string().contains("does not count"));
    }

    #[test]
    fn supersede_chain_and_conflict() {
        let st = build(vec![
            e("D-000001", "001", "decision", &[]),
            with(
                e("D-000002", "002", "decision", &[]),
                "supersedes",
                json!(["D-000001"]),
            ),
            with(
                e("D-000003", "003", "decision", &[]),
                "supersedes",
                json!(["D-000002"]),
            ),
        ]);
        assert_eq!(
            st.supersede_chain("D-000002"),
            vec!["D-000001", "D-000002", "D-000003"]
        );
        assert_eq!(st.law().len(), 1);
        assert!(st.alerts.is_empty());

        // Two decisions in force supersede the same one: conflict.
        let mut v = vec![
            e("D-000001", "001", "decision", &[]),
            with(
                e("D-00000a", "002", "decision", &[]),
                "supersedes",
                json!(["D-000001"]),
            ),
            with(
                e("D-00000b", "003", "decision", &[]),
                "supersedes",
                json!(["D-000001"]),
            ),
        ];
        let st = build(v.clone());
        assert_eq!(
            st.alerts,
            vec![Alert::Conflict {
                target: "D-000001".into(),
                by: vec!["D-00000a".into(), "D-00000b".into()],
            }]
        );
        // A decision that supersedes both resolves it.
        v.push(with(
            e("D-00000c", "004", "decision", &[]),
            "supersedes",
            json!(["D-00000a", "D-00000b"]),
        ));
        assert!(build(v).alerts.is_empty());
    }

    /// v0 copied a proposal's `supersedes` onto the decision made from it: the
    /// target is superseded by the proposal and by its decision, which are the
    /// same decision at two moments. That is not a conflict.
    #[test]
    fn a_proposal_and_its_decision_are_one_superseder() {
        let coder = Actor::agent("coder");
        let v0_shape = vec![
            by(e("P-000001", "001", "proposal", &[]), coder.clone()),
            by(
                with(
                    e("P-000002", "002", "proposal", &[]),
                    "supersedes",
                    json!(["P-000001"]),
                ),
                coder.clone(),
            ),
            with(
                with(
                    e("D-000001", "003", "decision", &["P-000002"]),
                    "from_proposal",
                    json!("P-000002"),
                ),
                "supersedes",
                json!(["P-000001"]),
            ),
            with(
                e("R-000001", "004", "accept", &["P-000002"]),
                "decision_id",
                json!("D-000001"),
            ),
        ];
        let st = build(v0_shape.clone());
        assert_eq!(st.status_of("P-000001"), Some(Status::Superseded));
        assert_eq!(st.promoted_to["P-000002"], "D-000001");
        assert!(st.alerts.is_empty(), "{:?}", st.alerts);
        // The same before the `accept` is read: the decision says where it
        // comes from.
        let st = build(v0_shape[..3].to_vec());
        assert!(st.alerts.is_empty(), "{:?}", st.alerts);

        // Two unrelated entries in force that supersede the same one still
        // conflict: a pending proposal and a decision that was not made from it.
        let st = build(vec![
            by(e("P-000001", "001", "proposal", &[]), coder.clone()),
            by(
                with(
                    e("P-000002", "002", "proposal", &[]),
                    "supersedes",
                    json!(["P-000001"]),
                ),
                coder.clone(),
            ),
            with(
                e("D-000001", "003", "decision", &[]),
                "supersedes",
                json!(["P-000001"]),
            ),
        ]);
        assert_eq!(
            st.alerts,
            vec![Alert::Conflict {
                target: "P-000001".into(),
                by: vec!["P-000002".into(), "D-000001".into()],
            }]
        );
    }

    #[test]
    fn facts_and_pending_proposals_are_replaced_by_their_update() {
        let distill = Actor::agent("distill");
        let other = Actor::agent("scout");
        let st = build(vec![
            by(e("F-000001", "001", "fact", &[]), other.clone()),
            by(
                with(
                    e("F-000002", "002", "fact", &[]),
                    "supersedes",
                    json!(["F-000001"]),
                ),
                distill.clone(),
            ),
            by(e("P-000001", "003", "proposal", &[]), distill.clone()),
            by(
                with(
                    e("P-000002", "004", "proposal", &[]),
                    "supersedes",
                    json!(["P-000001"]),
                ),
                distill.clone(),
            ),
            // From a human who is not an owner: stays pending alongside.
            by(e("P-00000a", "005", "proposal", &[]), Actor::human("ana")),
            by(
                with(
                    e("P-00000b", "006", "proposal", &[]),
                    "supersedes",
                    json!(["P-00000a"]),
                ),
                distill.clone(),
            ),
            // An owner can supersede someone else's proposal.
            with(
                e("P-00000c", "007", "proposal", &[]),
                "supersedes",
                json!(["P-00000b"]),
            ),
        ]);
        assert_eq!(st.status_of("F-000001"), Some(Status::Superseded));
        assert_eq!(st.superseded_by["F-000001"], "F-000002");
        let facts: Vec<&str> = st.facts().iter().map(|f| f.id.as_str()).collect();
        assert_eq!(facts, vec!["F-000002"]);
        assert_eq!(st.status_of("P-000001"), Some(Status::Superseded));
        assert_eq!(st.status_of("P-00000a"), Some(Status::Pending));
        // Another agent's proposal does get replaced (an agent updates what another agent proposed).
        let agents = build(vec![
            by(e("P-000001", "001", "proposal", &[]), other.clone()),
            by(
                with(
                    e("P-000002", "002", "proposal", &[]),
                    "supersedes",
                    json!(["P-000001"]),
                ),
                distill.clone(),
            ),
        ]);
        assert_eq!(agents.status_of("P-000001"), Some(Status::Superseded));
        assert_eq!(st.status_of("P-00000b"), Some(Status::Superseded));
        let pending: Vec<&str> = st
            .pending_proposals()
            .iter()
            .map(|p| p.id.as_str())
            .collect();
        assert_eq!(pending, vec!["P-000002", "P-00000a", "P-00000c"]);
        assert_eq!(st.supersede_chain("P-000002"), vec!["P-000001", "P-000002"]);
        assert!(st.alerts.is_empty(), "{:?}", st.alerts);
    }

    #[test]
    fn an_update_never_replaces_law_by_itself() {
        let distill = Actor::agent("distill");
        let st = build(vec![
            e("D-000001", "001", "decision", &[]),
            by(
                with(
                    e("F-000001", "002", "fact", &[]),
                    "supersedes",
                    json!(["D-000001"]),
                ),
                distill.clone(),
            ),
            by(
                with(
                    e("P-000001", "003", "proposal", &[]),
                    "supersedes",
                    json!(["D-000001"]),
                ),
                distill,
            ),
        ]);
        assert_eq!(st.status_of("D-000001"), Some(Status::Accepted));
        assert_eq!(st.status_of("P-000001"), Some(Status::Pending));
        // The proposal stays as always: pending, with its alert.
        assert!(
            matches!(st.alerts[..], [Alert::ProposalSupersedes { .. }]),
            "{:?}",
            st.alerts
        );
    }

    #[test]
    fn charters_latest_signed_wins_with_alert() {
        let st = build(vec![
            e("A-000001", "001", "charter", &[]),
            e("R-000001", "002", "sign", &["A-000001"]),
            e("A-000002", "003", "charter", &[]),
            e("R-000002", "004", "sign", &["A-000002"]),
        ]);
        assert_eq!(st.active_charter.as_deref(), Some("A-000002"));
        assert_eq!(
            st.alerts,
            vec![Alert::CharterConflict {
                active: "A-000002".into(),
                others: vec!["A-000001".into()],
            }]
        );
        // If the new one says it supersedes the previous one, there is no conflict.
        let st = build(vec![
            e("A-000001", "001", "charter", &[]),
            e("R-000001", "002", "sign", &["A-000001"]),
            with(
                e("A-000002", "003", "charter", &[]),
                "supersedes",
                json!(["A-000001"]),
            ),
            e("R-000002", "004", "sign", &["A-000002"]),
        ]);
        assert_eq!(st.active_charter.as_deref(), Some("A-000002"));
        assert_eq!(st.status_of("A-000001"), Some(Status::Superseded));
        assert!(st.alerts.is_empty());
    }

    #[test]
    fn accepted_proposal_is_law_or_promoted() {
        let st = build(vec![
            e("D-000001", "001", "decision", &[]),
            by(
                with(
                    e("P-000001", "002", "proposal", &[]),
                    "supersedes",
                    json!(["D-000001"]),
                ),
                Actor::agent("x"),
            ),
            e("R-000001", "003", "accept", &["P-000001"]),
            by(e("P-000002", "004", "proposal", &[]), Actor::agent("x")),
            e("D-000002", "005", "decision", &["P-000002"]),
            with(
                e("R-000002", "006", "accept", &["P-000002"]),
                "decision_id",
                json!("D-000002"),
            ),
        ]);
        assert!(st.is_law("P-000001"));
        assert_eq!(st.status_of("D-000001"), Some(Status::Superseded));
        assert_eq!(st.promoted_to["P-000002"], "D-000002");
        assert!(!st.is_law("P-000002"));
        let law: Vec<&str> = st.law().iter().map(|e| e.id.as_str()).collect();
        assert_eq!(law, vec!["P-000001", "D-000002"]);
    }

    #[test]
    fn resolve_prefixes() {
        let st = build(vec![
            e("D-7f3a9c", "001", "decision", &[]),
            e("D-7f3b00", "002", "decision", &[]),
        ]);
        assert_eq!(st.resolve("d-7f3a").unwrap(), "D-7f3a9c");
        assert!(
            st.resolve("D-7f3")
                .unwrap_err()
                .to_string()
                .contains("ambiguous")
        );
        assert!(st.resolve("Q-1").is_err());
    }

    #[test]
    fn resolve_v0_ids() {
        // `D-0015` is also a prefix of `D-0015ab`: the v0 id wins.
        let st = build(vec![
            with(
                e("D-a1b2c3", "001", "decision", &[]),
                "v0_id",
                json!("D-0015"),
            ),
            e("D-0015ab", "002", "decision", &[]),
        ]);
        assert_eq!(st.resolve("D-0015").unwrap(), "D-a1b2c3");
        assert_eq!(st.resolve("d-0015").unwrap(), "D-a1b2c3");
        assert_eq!(st.resolve("D-0015a").unwrap(), "D-0015ab");
        assert!(st.resolve("D-0016").is_err());
    }

    // ------------------------------------------------------------ tasks

    fn at(mut x: Entry, hlc: &str) -> Entry {
        x.hlc = format!("2026-11-03T{hlc}.000Z.0000");
        x
    }

    fn agent(name: &str, role: &str) -> Actor {
        let mut a = Actor::agent(name);
        a.role = Some(role.into());
        a
    }

    fn task(id: &str, hlc: &str, goals: &[&str], deps: &[&str]) -> Entry {
        let mut t = e(id, hlc, "task", &["E-000001"]);
        t.fields.insert("epic".into(), json!("E-000001"));
        t.fields.insert("goals".into(), json!(goals));
        t.fields.insert("depends_on".into(), json!(deps));
        t
    }

    fn rec(id: &str, hlc: &str, kind: &str, task: &str, who: Actor) -> Entry {
        by(e(id, hlc, kind, &[task]), who)
    }

    fn ok_evidence(x: Entry) -> Entry {
        with(
            x,
            "evidence",
            json!([{"type": "command", "value": "cargo test", "exit_code": 0}]),
        )
    }

    /// Signed charter and goal, and the epic "payments" with that goal.
    fn base() -> Vec<Entry> {
        let mut epic = e("E-000001", "004", "epic", &["G-000001"]);
        epic.fields.insert("name".into(), json!("payments"));
        epic.fields.insert("goals".into(), json!(["G-000001"]));
        epic.fields
            .insert("coordinator".into(), json!("epic:payments"));
        vec![
            e("A-000001", "000", "charter", &[]),
            e("R-0000a1", "001", "sign", &["A-000001"]),
            e("G-000001", "002", "goal", &["A-000001"]),
            e("R-0000a2", "003", "sign", &["G-000001"]),
            epic,
        ]
    }

    fn statuses(st: &State, ids: &[&str]) -> Vec<TaskStatus> {
        ids.iter().map(|t| st.task_status(t).unwrap()).collect()
    }

    #[test]
    fn task_transitions_and_dependencies() {
        use TaskStatus::*;
        let mut v = base();
        v.push(task("T-000001", "010", &["G-000001"], &[]));
        v.push(task("T-000002", "011", &["G-000001"], &["T-000001"]));
        v.push(task("T-000003", "012", &["G-000001"], &[]));
        let st = build(v.clone());
        assert_eq!(
            statuses(&st, &["T-000001", "T-000002", "T-000003"]),
            [Ready, Blocked, Ready]
        );
        assert_eq!(st.task("T-000002").unwrap().blocked_by, vec!["T-000001"]);
        assert!(st.alerts.is_empty(), "{:?}", st.alerts);

        let impl1 = agent("impl-1", "implementer");
        v.push(rec("R-000101", "020", "claim", "T-000001", impl1.clone()));
        v.push(with(
            rec(
                "R-000102",
                "021",
                "assign",
                "T-000003",
                agent("epic:payments", "coordinator"),
            ),
            "to",
            json!("agent:impl-2"),
        ));
        let st = build(v.clone());
        assert_eq!(
            statuses(&st, &["T-000001", "T-000002", "T-000003"]),
            [Claimed, Blocked, Assigned]
        );
        assert_eq!(st.claim_of("T-000001").unwrap().actor, "agent:impl-1");
        assert_eq!(st.status_of("T-000001"), Some(Status::Claimed));

        v.push(ok_evidence(rec(
            "R-000103",
            "022",
            "done",
            "T-000001",
            impl1.clone(),
        )));
        let st = build(v.clone());
        assert_eq!(statuses(&st, &["T-000001", "T-000002"]), [Closed, Ready]);
        assert_eq!(st.status_of("T-000001"), Some(Status::Done));

        // Reopen: it is ready again, with no holder, and the dependent task is blocked.
        v.push(rec(
            "R-000104",
            "023",
            "reopen",
            "T-000001",
            Actor::human("omar"),
        ));
        let st = build(v.clone());
        assert_eq!(statuses(&st, &["T-000001", "T-000002"]), [Ready, Blocked]);
        assert!(st.claim_of("T-000001").is_none());
        let notes: Vec<&str> = st
            .task("T-000001")
            .unwrap()
            .history
            .iter()
            .map(|h| h.note.as_str())
            .collect();
        assert_eq!(notes[0], "claimed by agent:impl-1");
        assert!(
            notes[1].starts_with("closed with evidence: `cargo test` (0)"),
            "{notes:?}"
        );
        assert_eq!(notes[2], "reopened");
    }

    #[test]
    fn claim_lease_expires_without_activity() {
        let mut v = base();
        v.push(task("T-000001", "010", &["G-000001"], &[]));
        let impl1 = agent("impl-1", "implementer");
        v.push(at(
            rec("R-000101", "", "claim", "T-000001", impl1.clone()),
            "18:30:00",
        ));
        let lease_end = now_ms("2026-11-03T20:30:00Z");
        let policy = Policy::new(&["omar"]);

        // Just before 2 h it is still claimed; after that it is ready again.
        let st = State::build_at(v.clone(), &policy, lease_end - 1);
        assert_eq!(st.task_status("T-000001"), Some(TaskStatus::Claimed));
        let st = State::build_at(v.clone(), &policy, lease_end + 1);
        assert_eq!(st.task_status("T-000001"), Some(TaskStatus::Ready));
        assert!(st.claim_of("T-000001").is_none());
        assert!(
            matches!(
                &st.alerts[..],
                [Alert::ClaimExpired { task, holder, .. }] if task == "T-000001" && holder == "agent:impl-1"
            ),
            "{:?}",
            st.alerts
        );

        // A journal from its holder renews it; one from anyone else does not.
        let mut j = at(
            by(e("J-000001", "", "journal", &[]), Actor::agent("otro")),
            "19:30:00",
        );
        j.fields.insert("task".into(), json!("T-000001"));
        v.push(j.clone());
        let st = State::build_at(v.clone(), &policy, lease_end + 1);
        assert_eq!(st.task_status("T-000001"), Some(TaskStatus::Ready));
        j.id = "J-000002".into();
        j.actor = impl1.clone();
        v.push(j);
        let st = State::build_at(v.clone(), &policy, lease_end + 1);
        assert_eq!(st.task_status("T-000001"), Some(TaskStatus::Claimed));

        // Once expired, someone else claims it: the newer claim wins and there is no alert.
        let mut v = base();
        v.push(task("T-000001", "010", &["G-000001"], &[]));
        v.push(at(
            rec("R-000101", "", "claim", "T-000001", impl1),
            "18:30:00",
        ));
        v.push(at(
            rec(
                "R-000102",
                "",
                "claim",
                "T-000001",
                agent("impl-2", "implementer"),
            ),
            "21:00:00",
        ));
        let st = State::build_at(v, &policy, now_ms("2026-11-03T21:01:00Z"));
        assert_eq!(st.claim_of("T-000001").unwrap().actor, "agent:impl-2");
        assert!(st.alerts.is_empty(), "{:?}", st.alerts);
        // With a longer lease, the second claim does not count.
        let long = Policy {
            lease_ms: 4 * 3_600_000,
            ..policy
        };
        let mut v = base();
        v.push(task("T-000001", "010", &["G-000001"], &[]));
        v.push(at(
            rec(
                "R-000101",
                "",
                "claim",
                "T-000001",
                agent("impl-1", "implementer"),
            ),
            "18:30:00",
        ));
        v.push(at(
            rec(
                "R-000102",
                "",
                "claim",
                "T-000001",
                agent("impl-2", "implementer"),
            ),
            "21:00:00",
        ));
        let st = State::build_at(v, &long, now_ms("2026-11-03T21:01:00Z"));
        assert_eq!(st.claim_of("T-000001").unwrap().actor, "agent:impl-1");
    }

    #[test]
    fn cross_writer_claims_earliest_hlc_wins() {
        let mut v = base();
        v.push(task("T-000001", "010", &["G-000001"], &[]));
        // They arrive when syncing, each from another machine and out of order.
        let mut late = rec(
            "R-0000ff",
            "031",
            "claim",
            "T-000001",
            agent("ana-bot", "implementer"),
        );
        late.writer = "ana@laptop".into();
        let mut early = rec(
            "R-0000ee",
            "030",
            "claim",
            "T-000001",
            agent("omar-bot", "implementer"),
        );
        early.writer = "omar@mac".into();
        let mut a = v.clone();
        a.extend([late.clone(), early.clone()]);
        let mut b = v;
        b.extend([early, late]);
        for st in [build(a), build(b)] {
            let c = st.claim_of("T-000001").unwrap();
            assert_eq!(
                (c.record.as_str(), c.actor.as_str()),
                ("R-0000ee", "agent:omar-bot")
            );
            let h = &st.task("T-000001").unwrap().history;
            assert!(
                !h[1].counted && h[1].note.contains("already held by agent:omar-bot"),
                "{h:?}"
            );
            // Phase 5: whoever lost from another machine gets the alert.
            let lost: Vec<&Alert> = st
                .alerts
                .iter()
                .filter(|a| matches!(a, Alert::ClaimLost { .. }))
                .collect();
            assert_eq!(
                lost,
                [&Alert::ClaimLost {
                    task: "T-000001".into(),
                    record: "R-0000ff".into(),
                    loser: "agent:ana-bot".into(),
                    winner: "agent:omar-bot".into(),
                    winner_writer: "omar@mac".into(),
                }]
            );
            assert!(lost[0].to_string().contains("stop working on it"));
            assert!(lost[0].is_standing());
        }
    }

    #[test]
    fn lost_claim_alert_goes_away_when_the_task_closes() {
        let mut v = base();
        v.push(task("T-000001", "010", &["G-000001"], &[]));
        let mut early = rec(
            "R-0000ee",
            "030",
            "claim",
            "T-000001",
            agent("a", "implementer"),
        );
        early.writer = "omar@mac".into();
        let mut late = rec(
            "R-0000ff",
            "031",
            "claim",
            "T-000001",
            agent("b", "implementer"),
        );
        late.writer = "ana@laptop".into();
        // From the same writer there is no alert (on one machine a claim is atomic).
        let mut same = rec(
            "R-0000dd",
            "032",
            "claim",
            "T-000001",
            agent("c", "implementer"),
        );
        same.writer = "omar@mac".into();
        v.extend([early.clone(), late, same]);
        let st = build(v.clone());
        let n = |st: &State| {
            st.alerts
                .iter()
                .filter(|a| matches!(a, Alert::ClaimLost { .. }))
                .count()
        };
        assert_eq!(n(&st), 1);
        let mut done = ok_evidence(rec(
            "R-0000aa",
            "040",
            "done",
            "T-000001",
            agent("a", "implementer"),
        ));
        done.writer = "omar@mac".into();
        v.push(done);
        let st = build(v);
        assert_eq!(st.task_status("T-000001"), Some(TaskStatus::Closed));
        assert_eq!(n(&st), 0);
    }

    #[test]
    fn pending_proposal_that_supersedes_law_alerts() {
        let p = by(
            with(
                e("P-000001", "002", "proposal", &["D-000001"]),
                "supersedes",
                json!(["D-000001"]),
            ),
            Actor::agent("coder"),
        );
        let mut v = vec![e("D-000001", "001", "decision", &[]), p];
        let st = build(v.clone());
        assert_eq!(
            st.alerts,
            [Alert::ProposalSupersedes {
                proposal: "P-000001".into(),
                decision: "D-000001".into()
            }]
        );
        assert!(
            st.alerts[0]
                .to_string()
                .starts_with("P-000001 proposes to supersede D-000001")
        );
        // Rejecting it clears the alert.
        v.push(with(
            e("R-000001", "003", "reject", &["P-000001"]),
            "x",
            json!(1),
        ));
        assert!(build(v).alerts.is_empty());
    }

    #[test]
    fn require_signatures_counts_only_verified() {
        use crate::sign::Verdict;
        let mut policy = Policy::new(&["omar"]);
        policy.require_signatures = true;
        policy.signatures.insert("D-00000b".into(), Verdict::Valid);
        policy.signatures.insert(
            "D-00000c".into(),
            Verdict::Invalid("not signed by omar".into()),
        );
        let mut imported = e("D-00000e", "005", "decision", &[]);
        imported.fields.insert("v0_id".into(), json!("D-0001"));
        let v = vec![
            e("D-00000a", "001", "decision", &[]),
            e("D-00000b", "002", "decision", &[]),
            e("D-00000c", "003", "decision", &[]),
            // A fact is not signed: no change.
            e("F-00000d", "004", "fact", &[]),
            imported,
        ];
        let st = State::build_at(v, &policy, now_ms(NOW));
        assert_eq!(st.status_of("D-00000a"), Some(Status::Ignored));
        assert!(st.is_law("D-00000b"));
        assert_eq!(st.status_of("D-00000c"), Some(Status::Ignored));
        assert_eq!(st.status_of("F-00000d"), Some(Status::Active));
        assert!(
            st.is_law("D-00000e"),
            "what was imported from v0 needs no signature"
        );
        let bad: Vec<String> = st.alerts.iter().map(|a| a.to_string()).collect();
        assert_eq!(bad.len(), 2, "{bad:?}");
        assert!(
            bad[0].contains("signature invalid or missing (the signature was not checked)"),
            "{bad:?}"
        );
        assert!(bad[1].contains("not signed by omar"), "{bad:?}");
    }

    #[test]
    fn done_and_release_only_count_from_the_holder_or_managers() {
        let mut v = base();
        v.push(task("T-000001", "010", &["G-000001"], &[]));
        v.push(task("T-000002", "011", &["G-000001"], &[]));
        let impl1 = agent("impl-1", "implementer");
        v.push(rec("R-000101", "020", "claim", "T-000001", impl1.clone()));
        v.push(rec("R-000102", "021", "claim", "T-000002", impl1.clone()));
        // Another agent closes and releases: stored, does not count, and warns.
        v.push(ok_evidence(rec(
            "R-000103",
            "022",
            "done",
            "T-000001",
            agent("intruso", "implementer"),
        )));
        v.push(rec(
            "R-000104",
            "023",
            "release",
            "T-000002",
            agent("intruso", "implementer"),
        ));
        // Its holder closes without valid evidence: does not count either.
        v.push(with(
            rec("R-000105", "024", "done", "T-000001", impl1.clone()),
            "evidence",
            json!([{"type": "command", "value": "cargo test", "exit_code": 1}]),
        ));
        let st = build(v.clone());
        assert_eq!(st.task_status("T-000001"), Some(TaskStatus::Claimed));
        assert_eq!(st.task_status("T-000002"), Some(TaskStatus::Claimed));
        let ignored: Vec<&str> = st
            .alerts
            .iter()
            .filter_map(|a| match a {
                Alert::TaskRecordIgnored { id, .. } => Some(id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(ignored, ["R-000103", "R-000104", "R-000105"]);
        assert!(
            st.alerts[0].to_string().contains("only whoever holds it"),
            "{}",
            st.alerts[0]
        );
        assert!(
            st.alerts[2].to_string().contains("exited with 1"),
            "{}",
            st.alerts[2]
        );
        assert!(!st.counts("R-000103"));

        // The epic's coordinator and the pm do count.
        v.push(ok_evidence(rec(
            "R-000106",
            "025",
            "done",
            "T-000001",
            agent("epic:payments", "coordinator"),
        )));
        v.push(rec("R-000107", "026", "release", "T-000002", pm()));
        let st = build(v);
        assert_eq!(st.task_status("T-000001"), Some(TaskStatus::Closed));
        assert_eq!(st.task_status("T-000002"), Some(TaskStatus::Ready));
    }

    #[test]
    fn drift_alerts() {
        let mut v = base();
        v.push(e("G-000002", "005", "goal", &["A-000001"]));
        v.push(e("G-000003", "006", "goal", &["A-000001"]));
        v.push(e("R-0000a3", "007", "sign", &["G-000003"]));
        v.push(e("G-000004", "008", "goal", &["A-000001"]));
        v.push(e("R-0000a4", "009", "supersede", &["G-000003", "G-000004"]));
        v.push(task("T-000001", "010", &[], &[]));
        v.push(task("T-000002", "011", &["G-000003"], &[]));
        v.push(task("T-000003", "012", &["G-000002"], &[]));
        v.push(task("T-000004", "013", &["G-999999"], &[]));
        v.push(task("T-000005", "014", &["G-000001"], &[]));
        let mut epic2 = e("E-000002", "015", "epic", &[]);
        epic2.fields.insert("goals".into(), json!(["G-000003"]));
        v.push(epic2);
        // A closed task that cites a superseded goal is no longer drift.
        v.push(task("T-000006", "016", &["G-000003"], &[]));
        v.push(rec(
            "R-000101",
            "017",
            "claim",
            "T-000006",
            Actor::human("omar"),
        ));
        v.push(ok_evidence(rec(
            "R-000102",
            "018",
            "done",
            "T-000006",
            Actor::human("omar"),
        )));
        let st = build(v);
        let got: Vec<String> = st.alerts.iter().map(ToString::to_string).collect();
        assert_eq!(
            got,
            [
                "drift: the task T-000001 does not cite any goal",
                "drift: the task T-000002 cites G-000003, which was superseded by G-000004",
                "unsigned goal: the task T-000003 cites G-000002, which no owner has signed yet (`kontext sign G-000002`)",
                "drift: the task T-000004 cites G-999999, which is not a goal in the ledger",
                "drift: the epic E-000002 cites G-000003, which was superseded by G-000004",
            ],
        );
        assert!(st.alerts.iter().all(Alert::is_standing));
    }
}
