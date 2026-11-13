//! `kontext tree`: the whole project on one screen (DESIGN §8).
//!
//! ```text
//! quoro                            3/9 features · wave 2 of 3
//! ├─ Wave 1
//! │  └─ ✓ Foundations              4/4
//! ├─ Wave 2
//! │  ├─ ◐ Payments                 2/3
//! │  │   ├─ ✓ Card charge
//! │  │   ├─ ◐ Refunds              waiting for you
//! │  │   └─ ○ Invoices
//! │  └─ ◐ Catalog                  1/2
//! └─ Not placed (2) · 12 of your messages not audited since Nov 12
//!     ├─ "que el recibo salga en PDF"           Nov 12
//!     └─ "límite de pedidos abiertos"  (Payments, no feature)
//! ```
//!
//! The waves are those of [`crate::plan`] over every epic in force, closed or
//! not (with the closed ones left open, so a finished wave keeps its place);
//! the current wave is the first with an open epic. Closed epics take one
//! line; open ones expand into their features (an epic without features shows
//! its tasks' count). Tasks are never listed. "Not placed" comes from
//! [`crate::ideas`] and is absent without `.pm/items.md`. Read-only: it writes
//! nothing and advances no cursor.

use std::collections::BTreeSet;

use serde::Serialize;

use crate::features::{self, FeatureStatus};
use crate::ideas::{Ideas, NotPlaced, Unaudited};
use crate::plan::{self, Node};
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
    /// The first wave with an open epic (`None`: none is open).
    pub current_wave: Option<usize>,
    pub waves: Vec<TreeWave>,
    /// Epics in force with no wave (a cycle, a missing dependency), each with
    /// its reason.
    pub no_wave: Vec<TreeEpic>,
    /// `None` without `.pm/items.md`.
    pub not_placed: Option<TreeIdeas>,
}

// ------------------------------------------------------------ build

/// The features with an open question for a person that cites them or one
/// of their tasks (`fields.feature`, `fields.task` or `refs`).
fn waiting_features(st: &State, people: &[String]) -> BTreeSet<String> {
    let feature_of = |id: &str| -> Option<String> {
        let e = st.get(id)?;
        match e.kind.as_str() {
            "feature" => Some(e.id.clone()),
            "task" => features::feature_of(e).map(str::to_string),
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

fn feature_node(st: &State, f: &crate::model::Entry, waiting: &BTreeSet<String>) -> TreeFeature {
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
    }
}

fn epic_node(st: &State, n: &Node, waiting: &BTreeSet<String>) -> TreeEpic {
    let feature_list: Vec<TreeFeature> = features::of_epic(st, &n.id)
        .into_iter()
        .map(|f| feature_node(st, f, waiting))
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
    }
}

/// The tree of the project. `ideas`: what `kontext ideas` reads, if there
/// is a `.pm/items.md`; `people`, the names in `[people]`.
pub fn build(st: &State, project: &str, people: &[String], ideas: Option<&Ideas>) -> Tree {
    let nodes = plan::nodes(st);
    // Every epic in force, as if none were closed: a closed epic keeps the
    // wave it had.
    let open_all: Vec<Node> = nodes
        .iter()
        .map(|n| Node {
            tasks: 0,
            closed_tasks: 0,
            features: 0,
            done_features: 0,
            ..n.clone()
        })
        .collect();
    let layout = plan::compute(&open_all);
    let waiting = waiting_features(st, people);
    let node = |id: &str| nodes.iter().find(|n| n.id == id);
    let waves: Vec<TreeWave> = layout
        .waves
        .iter()
        .map(|w| TreeWave {
            n: w.n,
            epics: w
                .epics
                .iter()
                .filter_map(|e| node(&e.id))
                .map(|n| epic_node(st, n, &waiting))
                .collect(),
        })
        .collect();
    let no_wave: Vec<TreeEpic> = layout
        .blocked
        .iter()
        .filter_map(|b| {
            node(&b.epic.id).map(|n| TreeEpic {
                reason: Some(b.reason.clone()),
                ..epic_node(st, n, &waiting)
            })
        })
        .collect();
    let all: Vec<&TreeEpic> = waves
        .iter()
        .flat_map(|w| &w.epics)
        .chain(&no_wave)
        .collect();
    let (fdone, ftotal) = all
        .iter()
        .filter_map(|e| e.features.as_ref())
        .fold((0, 0), |(d, t), c| (d + c.done, t + c.total));
    let current_wave = waves
        .iter()
        .find(|w| w.epics.iter().any(|e| !e.closed))
        .map(|w| w.n);
    Tree {
        project: project.to_string(),
        features: (ftotal > 0).then_some(Count {
            done: fdone,
            total: ftotal,
        }),
        epics: Count {
            done: all.iter().filter(|e| e.closed).count(),
            total: all.len(),
        },
        current_wave,
        not_placed: ideas.map(|i| TreeIdeas {
            count: i.not_placed.len(),
            ideas: i.not_placed.clone(),
            unaudited: i.unaudited.clone(),
        }),
        waves,
        no_wave,
    }
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
    if !t.waves.is_empty() {
        match t.current_wave {
            Some(n) => parts.push(format!("wave {n} of {}", t.waves.len())),
            None if t.no_wave.iter().all(|e| e.closed) => parts.push(format!(
                "{} {}, all closed",
                t.waves.len(),
                if t.waves.len() == 1 { "wave" } else { "waves" }
            )),
            None => {}
        }
    }
    parts.join(" · ")
}

/// The terminal version (the module's doc).
pub fn text(t: &Tree) -> String {
    let mut lines = vec![Line {
        left: t.project.clone(),
        right: header(t),
    }];
    if t.waves.is_empty() && t.no_wave.is_empty() {
        lines.push(Line {
            left: "(no epics yet: `kontext epic add \"<title>\" --goal <G>`)".into(),
            right: String::new(),
        });
    }
    // The groups: each wave, then the epics with no wave, then the ideas.
    let mut groups: Vec<(String, &[TreeEpic])> = t
        .waves
        .iter()
        .map(|w| (format!("Wave {}", w.n), w.epics.as_slice()))
        .collect();
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
