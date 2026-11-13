//! `kontext tree`: the whole project on one screen (DESIGN §8).
//!
//! ```text
//! quoro                            3/9 features · 2 waves to go
//! ├─ Done
//! │  └─ ✓ Foundations              4/4 tasks
//! ├─ Wave 1
//! │  ├─ ◐ Payments                 2/3
//! │  │   ├─ ✓ Card charge
//! │  │   ├─ ◐ Refunds              waiting for you
//! │  │   └─ ○ Invoices
//! │  └─ ◐ Catalog                  1/2
//! ├─ Wave 2
//! │  └─ ○ Integration              0/2
//! └─ Not placed (2) · 12 of your messages not audited since Nov 12
//!     ├─ "que el recibo salga en PDF"           Nov 12
//!     └─ "límite de pedidos abiertos"  (Payments, no feature)
//! ```
//!
//! The waves, their numbers and the epics with no wave (and why) are exactly
//! those of `kontext plan` ([`crate::plan::of_state`]): only open epics have a
//! wave. Closed epics go first, one line each, under "Done". Open epics expand
//! into their features (an epic without features shows its tasks' count).
//! Tasks are never listed. "Not placed" comes from [`crate::ideas`] and is
//! absent without `.pm/items.md`. Read-only: it writes nothing and advances no
//! cursor.
//!
//! With a signed plan (`kontext scope sign`) the header says so ("plan signed
//! Nov 12"), or that a scope change waits for the owner, with what enters
//! and leaves on one line; a feature or epic that entered after the
//! signature says "not signed", and a feature dropped since, "dropping,
//! waiting for you" (it stays listed until the owner signs).

use std::collections::BTreeSet;

use serde::Serialize;

use crate::features::{self, FeatureStatus};
use crate::ideas::{Ideas, NotPlaced, Unaudited};
use crate::plan::{self, Node, PlanEpic};
use crate::state::State;
use crate::tasks::TaskStatus;
use crate::views::{field, one_line};

/// How far an epic or a feature went.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Progress {
    Done,
    InProgress,
    Pending,
}

impl Progress {
    pub fn mark(self) -> &'static str {
        match self {
            Progress::Done => "✓",
            Progress::InProgress => "◐",
            Progress::Pending => "○",
        }
    }
}

/// Where a feature or an epic stands against the signed scope.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScopeMark {
    /// It entered after the last signature.
    NotSigned,
    /// It was dropped and the owner has not signed that yet.
    Dropping,
}

impl ScopeMark {
    pub fn label(self) -> &'static str {
        match self {
            ScopeMark::NotSigned => "not signed",
            ScopeMark::Dropping => "dropping, waiting for you",
        }
    }
}

/// The signed scope, in the header (only with a signed plan).
#[derive(Serialize, Clone, Debug)]
pub struct TreeScope {
    /// The last scope signature (`S-…`) and its day ("Nov 12").
    pub signed: String,
    pub signed_on: String,
    /// The change that waits for the owner, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending: Option<PendingScope>,
}

#[derive(Serialize, Clone, Debug)]
pub struct PendingScope {
    /// Titles of what enters and leaves.
    pub enters: Vec<String>,
    pub leaves: Vec<String>,
}

impl PendingScope {
    /// "enters Invoices · leaves Refunds".
    pub fn line(&self) -> String {
        let mut parts = Vec::new();
        if !self.enters.is_empty() {
            parts.push(format!("enters {}", self.enters.join(", ")));
        }
        if !self.leaves.is_empty() {
            parts.push(format!("leaves {}", self.leaves.join(", ")));
        }
        parts.join(" · ")
    }
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct Count {
    pub done: usize,
    pub total: usize,
}

#[derive(Serialize, Clone, Debug)]
pub struct TreeFeature {
    pub id: String,
    pub title: String,
    pub progress: Progress,
    /// `pending`, `in progress`, `to validate` or `done`.
    pub status: FeatureStatus,
    pub tasks: Count,
    /// An open question for a person cites it or one of its tasks.
    pub waiting_for_you: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub acceptance: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<ScopeMark>,
}

#[derive(Serialize, Clone, Debug)]
pub struct TreeEpic {
    pub id: String,
    pub name: String,
    pub title: String,
    pub closed: bool,
    pub progress: Progress,
    pub tasks: Count,
    /// Its features in force, done and total (`None`: it has none).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub features: Option<Count>,
    /// Its features in force, in ledger order.
    pub feature_list: Vec<TreeFeature>,
    /// Why it has no wave (only in `no_wave`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<ScopeMark>,
}

#[derive(Serialize, Clone, Debug)]
pub struct TreeWave {
    pub n: usize,
    pub epics: Vec<TreeEpic>,
}

#[derive(Serialize, Clone, Debug)]
pub struct TreeIdeas {
    pub count: usize,
    pub ideas: Vec<NotPlaced>,
    pub unaudited: Unaudited,
}

#[derive(Serialize, Clone, Debug)]
pub struct Tree {
    pub project: String,
    /// Features in force across the project (`None`: there are none).
    pub features: Option<Count>,
    pub epics: Count,
    /// The closed epics, one line each.
    pub done: Vec<TreeEpic>,
    /// The waves of the open epics, as `kontext plan` numbers them.
    pub waves: Vec<TreeWave>,
    /// Epics in force with no wave (a cycle, a missing dependency), each with
    /// its reason.
    pub no_wave: Vec<TreeEpic>,
    /// `None` without `.pm/items.md`.
    pub not_placed: Option<TreeIdeas>,
    /// `None` without a signed plan.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<TreeScope>,
}

// ------------------------------------------------------------ build

/// The features with an open question for a person that cites them or one
/// of their tasks (`fields.feature`, `fields.task` or `refs`).
fn waiting_features(st: &State, people: &[String]) -> BTreeSet<String> {
    let feature_of = |id: &str| -> Option<String> {
        let e = st.get(id)?;
        match e.kind.as_str() {
            "feature" => Some(e.id.clone()),
            "task" => features::counted_feature(st, e).map(str::to_string),
            _ => None,
        }
    };
    let mut out = BTreeSet::new();
    for q in st.open_questions() {
        if !crate::inbox::waits_for_a_human(st, q, people) {
            continue;
        }
        let cited = field(q, "feature")
            .into_iter()
            .chain(field(q, "task"))
            .map(str::to_string)
            .chain(q.refs.iter().cloned());
        for id in cited {
            if let Some(f) = feature_of(&id) {
                out.insert(f);
            }
        }
    }
    out
}

/// What enters and leaves the signed scope (empty without a pending change).
#[derive(Default)]
struct Marks {
    adds: BTreeSet<String>,
    removes: BTreeSet<String>,
}

impl Marks {
    fn of(st: &State) -> Marks {
        crate::scope::pending(st)
            .map(|d| Marks {
                adds: d.adds.into_iter().collect(),
                removes: d.removes.into_iter().collect(),
            })
            .unwrap_or_default()
    }

    fn mark(&self, id: &str) -> Option<ScopeMark> {
        if self.adds.contains(id) {
            Some(ScopeMark::NotSigned)
        } else if self.removes.contains(id) {
            Some(ScopeMark::Dropping)
        } else {
            None
        }
    }
}

fn feature_node(
    st: &State,
    f: &crate::model::Entry,
    waiting: &BTreeSet<String>,
    marks: &Marks,
) -> TreeFeature {
    let info = st.feature(&f.id).cloned().unwrap_or_default();
    let progress = match info.status {
        FeatureStatus::Done => Progress::Done,
        FeatureStatus::InProgress | FeatureStatus::ToValidate => Progress::InProgress,
        _ => Progress::Pending,
    };
    TreeFeature {
        id: f.id.clone(),
        title: f.title.trim().to_string(),
        progress,
        status: info.status,
        tasks: Count {
            done: info.closed_tasks,
            total: info.tasks.len(),
        },
        waiting_for_you: waiting.contains(&f.id),
        acceptance: features::acceptance(f).map(str::to_string),
        scope: marks.mark(&f.id),
    }
}

fn epic_node(st: &State, n: &Node, waiting: &BTreeSet<String>, marks: &Marks) -> TreeEpic {
    // Its features in force, and the dropped ones the owner has not signed
    // yet (they stay listed, marked, until they do).
    let feature_list: Vec<TreeFeature> = st
        .of_kind("feature")
        .filter(|f| field(f, "epic") == Some(n.id.as_str()))
        .filter(|f| {
            marks.removes.contains(&f.id) || st.feature(&f.id).is_some_and(|i| i.status.in_force())
        })
        .map(|f| feature_node(st, f, waiting, marks))
        .collect();
    let closed = n.closed();
    let started = feature_list.iter().any(|f| f.progress != Progress::Pending)
        || st.tasks_of(&n.id).iter().any(|t| {
            matches!(
                st.task_status(&t.id),
                Some(TaskStatus::Assigned | TaskStatus::Claimed | TaskStatus::Closed)
            )
        });
    TreeEpic {
        id: n.id.clone(),
        name: n.name.clone(),
        title: n.title.clone(),
        closed,
        progress: if closed {
            Progress::Done
        } else if started {
            Progress::InProgress
        } else {
            Progress::Pending
        },
        tasks: Count {
            done: n.closed_tasks,
            total: n.tasks,
        },
        features: (n.features > 0).then_some(Count {
            done: n.done_features,
            total: n.features,
        }),
        feature_list,
        reason: None,
        scope: marks.mark(&n.id),
    }
}

/// The tree of the project. `ideas`: what `kontext ideas` reads, if there
/// is a `.pm/items.md`; `people`, the names in `[people]`.
pub fn build(st: &State, project: &str, people: &[String], ideas: Option<&Ideas>) -> Tree {
    let nodes = plan::nodes(st);
    let layout = plan::compute(&nodes);
    let waiting = waiting_features(st, people);
    let marks = Marks::of(st);
    let node = |e: &PlanEpic| nodes.iter().find(|n| n.id == e.id);
    let epics = |list: &[PlanEpic]| -> Vec<TreeEpic> {
        list.iter()
            .filter_map(node)
            .map(|n| epic_node(st, n, &waiting, &marks))
            .collect()
    };
    let done = epics(&layout.closed);
    let waves: Vec<TreeWave> = layout
        .waves
        .iter()
        .map(|w| TreeWave {
            n: w.n,
            epics: epics(&w.epics),
        })
        .collect();
    let no_wave: Vec<TreeEpic> = layout
        .blocked
        .iter()
        .filter_map(|b| {
            node(&b.epic).map(|n| TreeEpic {
                reason: Some(b.reason.clone()),
                ..epic_node(st, n, &waiting, &marks)
            })
        })
        .collect();
    let all: Vec<&TreeEpic> = done
        .iter()
        .chain(waves.iter().flat_map(|w| &w.epics))
        .chain(&no_wave)
        .collect();
    let (fdone, ftotal) = all
        .iter()
        .filter_map(|e| e.features.as_ref())
        .fold((0, 0), |(d, t), c| (d + c.done, t + c.total));
    Tree {
        project: project.to_string(),
        features: (ftotal > 0).then_some(Count {
            done: fdone,
            total: ftotal,
        }),
        epics: Count {
            done: done.len(),
            total: all.len(),
        },
        not_placed: ideas.map(|i| TreeIdeas {
            count: i.not_placed.len(),
            ideas: i.not_placed.clone(),
            unaudited: i.unaudited.clone(),
        }),
        done,
        waves,
        no_wave,
        scope: tree_scope(st),
    }
}

fn tree_scope(st: &State) -> Option<TreeScope> {
    let s = crate::scope::last(st)?;
    let title = |id: &String| {
        st.get(id)
            .map(|e| one_line(&e.title, 40))
            .unwrap_or_else(|| id.clone())
    };
    Some(TreeScope {
        signed: s.id.clone(),
        signed_on: crate::scope::day(&s.hlc),
        pending: crate::scope::pending(st).map(|d| PendingScope {
            enters: d.adds.iter().map(title).collect(),
            leaves: d.removes.iter().map(title).collect(),
        }),
    })
}

// ------------------------------------------------------------ text

/// A line: the drawn part (branches, mark and title) and what goes in the
/// right column.
struct Line {
    left: String,
    right: String,
}

fn width(s: &str) -> usize {
    s.chars().count()
}

fn epic_count(e: &TreeEpic) -> String {
    match &e.features {
        Some(c) => format!("{}/{}", c.done, c.total),
        None => format!("{}/{} tasks", e.tasks.done, e.tasks.total),
    }
}

fn feature_note(f: &TreeFeature) -> String {
    let mut parts = Vec::new();
    if let Some(m) = f.scope {
        parts.push(m.label());
    }
    if f.status == FeatureStatus::ToValidate {
        parts.push("to validate");
    }
    if f.waiting_for_you {
        parts.push("waiting for you");
    }
    parts.join(" · ")
}

fn epic_lines(e: &TreeEpic, indent: &str, last: bool, out: &mut Vec<Line>) {
    let (branch, more) = if last {
        ("└─ ", "    ")
    } else {
        ("├─ ", "│   ")
    };
    let mut right = epic_count(e);
    if let Some(m) = e.scope {
        right.push_str(&format!(" · {}", m.label()));
    }
    if let Some(r) = &e.reason {
        right.push_str(&format!(" · {r}"));
    }
    out.push(Line {
        left: format!(
            "{indent}{branch}{} {}",
            e.progress.mark(),
            one_line(&e.title, 40)
        ),
        right,
    });
    if e.closed {
        return;
    }
    let n = e.feature_list.len();
    for (i, f) in e.feature_list.iter().enumerate() {
        let b = if i + 1 == n { "└─ " } else { "├─ " };
        out.push(Line {
            left: format!(
                "{indent}{more}{b}{} {}",
                f.progress.mark(),
                one_line(&f.title, 40)
            ),
            right: feature_note(f),
        });
    }
}

fn header(t: &Tree) -> String {
    let mut parts = Vec::new();
    match &t.features {
        Some(c) => parts.push(format!("{}/{} features", c.done, c.total)),
        None => parts.push(format!("{}/{} epics closed", t.epics.done, t.epics.total)),
    }
    let n = t.waves.len();
    if n > 0 {
        parts.push(format!(
            "{n} {} to go",
            if n == 1 { "wave" } else { "waves" }
        ));
    }
    if !t.no_wave.is_empty() {
        parts.push(format!("{} with no wave", t.no_wave.len()));
    }
    if n == 0 && t.no_wave.is_empty() && !t.done.is_empty() {
        parts.push("all done".to_string());
    }
    match &t.scope {
        Some(TreeScope {
            pending: Some(_), ..
        }) => parts.push("1 scope change waiting for you".to_string()),
        Some(s) => parts.push(format!("plan signed {}", s.signed_on)),
        None => {}
    }
    parts.join(" · ")
}

/// The terminal version (the module's doc).
pub fn text(t: &Tree) -> String {
    let mut lines = vec![Line {
        left: t.project.clone(),
        right: header(t),
    }];
    if let Some(p) = t.scope.as_ref().and_then(|s| s.pending.as_ref()) {
        lines.push(Line {
            left: String::new(),
            right: p.line(),
        });
    }
    if t.done.is_empty() && t.waves.is_empty() && t.no_wave.is_empty() {
        lines.push(Line {
            left: "(no epics yet: `kontext epic add \"<title>\" --goal <G>`)".into(),
            right: String::new(),
        });
    }
    // The groups: the closed epics, each wave, the epics with no wave, then
    // the ideas.
    let mut groups: Vec<(String, &[TreeEpic])> = Vec::new();
    if !t.done.is_empty() {
        groups.push(("Done".to_string(), t.done.as_slice()));
    }
    groups.extend(
        t.waves
            .iter()
            .map(|w| (format!("Wave {}", w.n), w.epics.as_slice())),
    );
    if !t.no_wave.is_empty() {
        groups.push(("No wave".to_string(), t.no_wave.as_slice()));
    }
    let n_groups = groups.len() + usize::from(t.not_placed.is_some());
    for (gi, (name, epics)) in groups.iter().enumerate() {
        let last = gi + 1 == n_groups;
        lines.push(Line {
            left: format!("{}{name}", if last { "└─ " } else { "├─ " }),
            right: String::new(),
        });
        let indent = if last { "   " } else { "│  " };
        for (i, e) in epics.iter().enumerate() {
            epic_lines(e, indent, i + 1 == epics.len(), &mut lines);
        }
    }
    // The right column starts after the widest drawn part.
    let col = lines
        .iter()
        .filter(|l| !l.right.is_empty())
        .map(|l| width(&l.left))
        .max()
        .unwrap_or(0)
        + 2;
    let mut out: Vec<String> = lines
        .iter()
        .map(|l| {
            if l.right.is_empty() {
                l.left.clone()
            } else {
                format!("{}{}{}", l.left, " ".repeat(col - width(&l.left)), l.right)
            }
        })
        .collect();
    if let Some(np) = &t.not_placed {
        out.extend(ideas_lines(np));
    }
    out.join("\n") + "\n"
}

fn ideas_lines(np: &TreeIdeas) -> Vec<String> {
    let mut head = format!("└─ Not placed ({})", np.count);
    if let Some(u) = crate::ideas::unaudited_text(&np.unaudited) {
        head.push_str(&format!(" · {u}"));
    }
    let rows: Vec<(String, String)> = np
        .ideas
        .iter()
        .map(|i| {
            let words = i.quote.as_deref().unwrap_or(&i.idea);
            let mut right = Vec::new();
            if let Some(d) = &i.said {
                right.push(crate::ideas::month_day(d));
            }
            if let Some(c) = &i.cites {
                right.push(format!("({})", one_line(c, 60)));
            }
            (format!("\"{}\"", one_line(words, 50)), right.join("  "))
        })
        .collect();
    let col = rows.iter().map(|(l, _)| width(l)).max().unwrap_or(0) + 2;
    let mut out = vec![head];
    for (i, (l, r)) in rows.iter().enumerate() {
        let b = if i + 1 == rows.len() {
            "└─ "
        } else {
            "├─ "
        };
        if r.is_empty() {
            out.push(format!("    {b}{l}"));
        } else {
            out.push(format!("    {b}{l}{}{r}", " ".repeat(col - width(l))));
        }
    }
    out
}

impl Tree {
    pub fn json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }
}
