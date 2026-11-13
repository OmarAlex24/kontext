//! `kontext plan`: the wave order of the open epics, from their dependencies
//! (`fields.depends_on`, which `kontext epic add --after` writes).
//!
//! It is deterministic and writes nothing. Rules:
//! - An epic is **closed** if it has at least one task and all of them are
//!   closed; otherwise it is **open**. Only epics in force count. An epic
//!   with at least one feature in force is closed instead when all its
//!   features in force are done (validated) and all its tasks without a
//!   feature are closed (a superseded feature's open tasks don't count; they
//!   raise an alert).
//! - A dependency is looked up by `id` or by name (`epic:<name>`). A closed
//!   dependency is already met. One that doesn't exist, or that is no longer in
//!   force, is a problem.
//! - An open epic goes in the wave after its last open dependency. Epics in the
//!   same wave don't depend on each other: they run in parallel.
//! - Epics in a cycle, epics that depend on something missing, and epics that
//!   wait on one of those get **no wave**, with their reason.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use serde_json::Value;

use crate::model::Entry;
use crate::state::{State, Status, string_list};
use crate::tasks::{TaskStatus, epic_name};

/// An epic, with what the plan needs.
#[derive(Debug, Clone)]
pub struct Node {
    pub id: String,
    pub name: String,
    pub title: String,
    /// What `fields.depends_on` stores: ids or names.
    pub after: Vec<String>,
    /// Still in force?
    pub active: bool,
    pub tasks: usize,
    pub closed_tasks: usize,
    /// Its features in force, and how many are done (0 and 0: no features).
    pub features: usize,
    pub done_features: usize,
    /// Its open tasks without a feature (they keep an epic with features open).
    pub open_loose: usize,
}

impl Node {
    pub fn closed(&self) -> bool {
        if self.features > 0 {
            return self.done_features == self.features && self.open_loose == 0;
        }
        self.tasks > 0 && self.closed_tasks == self.tasks
    }
}

/// An epic's features in the plan (only for an epic that has any).
#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct FeatureCount {
    pub total: usize,
    pub done: usize,
}

/// An epic in the output.
#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct PlanEpic {
    pub id: String,
    pub name: String,
    pub title: String,
    /// The dependencies that are still open (names).
    pub after: Vec<String>,
    pub tasks: usize,
    pub closed_tasks: usize,
    /// Only for an epic with features in force.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub features: Option<FeatureCount>,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct Wave {
    pub n: usize,
    /// More than one epic: they run in parallel, one coordinator each.
    pub parallel: bool,
    pub epics: Vec<PlanEpic>,
}

/// A dependency that can't be met.
#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct Missing {
    /// The epic that asks for it (name).
    pub epic: String,
    /// What it asks for, as stored.
    pub after: String,
    /// "does not exist" or "no longer in force (E-…)".
    pub reason: String,
}

/// An open epic that can't go in any wave.
#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct Blocked {
    pub epic: PlanEpic,
    pub reason: String,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct Plan {
    /// No cycles and no missing dependencies.
    pub ok: bool,
    pub waves: Vec<Wave>,
    pub closed: Vec<PlanEpic>,
    pub blocked: Vec<Blocked>,
    pub missing: Vec<Missing>,
    /// Each cycle as a list of names, in the order they wait on each other.
    pub cycles: Vec<Vec<String>>,
}

/// The epics in the state, in `hlc` order.
pub fn nodes(st: &State) -> Vec<Node> {
    st.of_kind("epic").map(|e| node(st, e)).collect()
}

/// One epic, with what the plan needs (its closure is [`Node::closed`]).
pub fn node(st: &State, e: &Entry) -> Node {
    let tasks = st.tasks_of(&e.id);
    let (done_features, features, _) = crate::features::counts(st, &e.id);
    Node {
        id: e.id.clone(),
        name: epic_name(e),
        title: e.title.trim().to_string(),
        after: string_list(e.fields.get("depends_on")),
        active: st.status_of(&e.id) == Some(Status::Active),
        tasks: tasks.len(),
        closed_tasks: tasks
            .iter()
            .filter(|t| st.task_status(&t.id) == Some(TaskStatus::Closed))
            .count(),
        features,
        done_features,
        open_loose: tasks
            .iter()
            .filter(|t| {
                crate::features::counted_feature(st, t).is_none()
                    && st.task_status(&t.id) != Some(TaskStatus::Closed)
            })
            .count(),
    }
}

/// `kontext plan` over the state.
pub fn of_state(st: &State) -> Plan {
    compute(&nodes(st))
}

/// Looks up a dependency: by `id`, then by name, preferring the one in force.
pub fn resolve<'a>(nodes: &'a [Node], dep: &str) -> Option<&'a Node> {
    let dep = dep.trim();
    if let Some(n) = nodes.iter().find(|n| n.id == dep) {
        return Some(n);
    }
    let name = dep.strip_prefix("epic:").unwrap_or(dep);
    let named: Vec<&Node> = nodes.iter().filter(|n| n.name == name).collect();
    named
        .iter()
        .rev()
        .find(|n| n.active)
        .or(named.last())
        .copied()
}

fn plan_epic(n: &Node, after: Vec<String>) -> PlanEpic {
    PlanEpic {
        id: n.id.clone(),
        name: n.name.clone(),
        title: n.title.clone(),
        after,
        tasks: n.tasks,
        closed_tasks: n.closed_tasks,
        features: (n.features > 0).then_some(FeatureCount {
            total: n.features,
            done: n.done_features,
        }),
    }
}

/// The wave plan. Pure: the same `nodes` give the same plan.
pub fn compute(nodes: &[Node]) -> Plan {
    let open: Vec<&Node> = nodes.iter().filter(|n| n.active && !n.closed()).collect();
    let closed: Vec<PlanEpic> = nodes
        .iter()
        .filter(|n| n.active && n.closed())
        .map(|n| plan_epic(n, Vec::new()))
        .collect();
    let index: BTreeMap<&str, usize> = open
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id.as_str(), i))
        .collect();

    // Edges: i waits for j (both open). Missing ones are kept apart.
    let mut edges: Vec<Vec<usize>> = vec![Vec::new(); open.len()];
    let mut missing = Vec::new();
    let mut bad: BTreeMap<usize, String> = BTreeMap::new();
    for (i, n) in open.iter().enumerate() {
        for dep in &n.after {
            match resolve(nodes, dep) {
                None => {
                    missing.push(Missing {
                        epic: n.name.clone(),
                        after: dep.clone(),
                        reason: "does not exist".into(),
                    });
                    bad.entry(i)
                        .or_insert_with(|| format!("depends on \"{dep}\", which does not exist"));
                }
                Some(d) if !d.active => {
                    missing.push(Missing {
                        epic: n.name.clone(),
                        after: dep.clone(),
                        reason: format!("no longer in force ({})", d.id),
                    });
                    bad.entry(i).or_insert_with(|| {
                        format!(
                            "depends on {} ({}), which is no longer in force",
                            d.name, d.id
                        )
                    });
                }
                Some(d) if d.closed() => {}
                Some(d) => {
                    let j = index[d.id.as_str()];
                    if !edges[i].contains(&j) {
                        edges[i].push(j);
                    }
                }
            }
        }
    }

    // Cycles: strongly connected components with more than one epic, or one
    // that waits on itself.
    let cycles = cycles(&edges);
    for c in &cycles {
        let names: Vec<&str> = c.iter().map(|&i| open[i].name.as_str()).collect();
        for &i in c {
            bad.entry(i)
                .or_insert_with(|| format!("is in a cycle: {}", cycle_text(&names)));
        }
    }

    // Whatever waits (directly or indirectly) on something with no wave gets none either.
    loop {
        let mut grew = false;
        for (i, deps) in edges.iter().enumerate() {
            if bad.contains_key(&i) {
                continue;
            }
            if let Some(&j) = deps.iter().find(|j| bad.contains_key(j)) {
                bad.insert(i, format!("waits for {}, which has no wave", open[j].name));
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }

    // Waves: each epic goes after the last wave of its dependencies.
    let mut wave_of: BTreeMap<usize, usize> = BTreeMap::new();
    loop {
        let mut grew = false;
        for (i, deps) in edges.iter().enumerate() {
            if bad.contains_key(&i) || wave_of.contains_key(&i) {
                continue;
            }
            if deps.iter().all(|j| wave_of.contains_key(j)) {
                let w = deps.iter().map(|j| wave_of[j]).max().unwrap_or(0) + 1;
                wave_of.insert(i, w);
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    let after_names =
        |i: usize| -> Vec<String> { edges[i].iter().map(|&j| open[j].name.clone()).collect() };
    let last = wave_of.values().copied().max().unwrap_or(0);
    let waves = (1..=last)
        .map(|w| {
            let epics: Vec<PlanEpic> = (0..open.len())
                .filter(|i| wave_of.get(i) == Some(&w))
                .map(|i| plan_epic(open[i], after_names(i)))
                .collect();
            Wave {
                n: w,
                parallel: epics.len() > 1,
                epics,
            }
        })
        .collect();
    let blocked = bad
        .iter()
        .map(|(&i, reason)| Blocked {
            epic: plan_epic(open[i], after_names(i)),
            reason: reason.clone(),
        })
        .collect();
    let cycles: Vec<Vec<String>> = cycles
        .iter()
        .map(|c| c.iter().map(|&i| open[i].name.clone()).collect())
        .collect();
    Plan {
        ok: missing.is_empty() && cycles.is_empty(),
        waves,
        closed,
        blocked,
        missing,
        cycles,
    }
}

fn cycle_text(names: &[&str]) -> String {
    let mut l: Vec<&str> = names.to_vec();
    if let Some(first) = names.first() {
        l.push(first);
    }
    l.join(" → ")
}

/// The cycles of the graph (Tarjan), each as a path back to its first epic:
/// `a` waits for `b`, which waits for `a`. They start at the oldest epic in the
/// cycle and come out in that order.
fn cycles(edges: &[Vec<usize>]) -> Vec<Vec<usize>> {
    struct T<'a> {
        edges: &'a [Vec<usize>],
        index: Vec<Option<usize>>,
        low: Vec<usize>,
        on: Vec<bool>,
        stack: Vec<usize>,
        next: usize,
        sccs: Vec<Vec<usize>>,
    }
    fn visit(t: &mut T, v: usize) {
        t.index[v] = Some(t.next);
        t.low[v] = t.next;
        t.next += 1;
        t.stack.push(v);
        t.on[v] = true;
        for &w in &t.edges[v] {
            match t.index[w] {
                None => {
                    visit(t, w);
                    t.low[v] = t.low[v].min(t.low[w]);
                }
                Some(iw) if t.on[w] => t.low[v] = t.low[v].min(iw),
                _ => {}
            }
        }
        if Some(t.low[v]) == t.index[v] {
            let mut scc = Vec::new();
            while let Some(w) = t.stack.pop() {
                t.on[w] = false;
                scc.push(w);
                if w == v {
                    break;
                }
            }
            t.sccs.push(scc);
        }
    }
    let n = edges.len();
    let mut t = T {
        edges,
        index: vec![None; n],
        low: vec![0; n],
        on: vec![false; n],
        stack: Vec::new(),
        next: 0,
        sccs: Vec::new(),
    };
    for v in 0..n {
        if t.index[v].is_none() {
            visit(&mut t, v);
        }
    }
    let mut out: Vec<Vec<usize>> = t
        .sccs
        .into_iter()
        .filter(|s| s.len() > 1 || edges[s[0]].contains(&s[0]))
        .map(|s| {
            let set: BTreeSet<usize> = s.iter().copied().collect();
            path_in(edges, &set)
        })
        .collect();
    out.sort();
    out
}

/// A path inside a component that starts at its oldest epic and returns to it.
fn path_in(edges: &[Vec<usize>], set: &BTreeSet<usize>) -> Vec<usize> {
    let start = *set.iter().next().unwrap();
    // Depth-first search until back at `start`.
    fn dfs(
        edges: &[Vec<usize>],
        set: &BTreeSet<usize>,
        start: usize,
        v: usize,
        path: &mut Vec<usize>,
    ) -> bool {
        for &w in &edges[v] {
            if w == start {
                return true;
            }
            if set.contains(&w) && !path.contains(&w) {
                path.push(w);
                if dfs(edges, set, start, w, path) {
                    return true;
                }
                path.pop();
            }
        }
        false
    }
    let mut path = vec![start];
    dfs(edges, set, start, start, &mut path);
    path
}

fn epic_text(e: &PlanEpic) -> String {
    let tasks = if e.tasks == 0 {
        "no tasks".to_string()
    } else {
        format!("{}/{} tasks closed", e.closed_tasks, e.tasks)
    };
    let tasks = match &e.features {
        Some(f) => format!("{}/{} features done; {tasks}", f.done, f.total),
        None => tasks,
    };
    let after = if e.after.is_empty() {
        String::new()
    } else {
        format!("; after {}", e.after.join(", "))
    };
    format!("- {} {}: {} ({tasks}{after})", e.id, e.name, e.title)
}

impl Plan {
    pub fn text(&self) -> String {
        let open: usize =
            self.waves.iter().map(|w| w.epics.len()).sum::<usize>() + self.blocked.len();
        let mut l = vec![format!(
            "# Wave plan: {open} open {}, {} closed",
            if open == 1 { "epic" } else { "epics" },
            self.closed.len(),
        )];
        if open == 0 && self.closed.is_empty() {
            l.push(String::new());
            l.push("There are no epics. Create them with `kontext epic add \"<title>\" --goal <G> [--after <epic>]`.".into());
        }
        for w in &self.waves {
            l.push(String::new());
            if w.parallel {
                l.push(format!(
                    "## Wave {} (in parallel: {} epics, one coordinator each)",
                    w.n,
                    w.epics.len()
                ));
            } else {
                l.push(format!("## Wave {} (in series)", w.n));
            }
            l.extend(w.epics.iter().map(epic_text));
        }
        if !self.blocked.is_empty() {
            l.push(String::new());
            l.push(format!("## No wave ({})", self.blocked.len()));
            for b in &self.blocked {
                l.push(format!("{} — {}", epic_text(&b.epic), b.reason));
            }
        }
        if !self.closed.is_empty() {
            l.push(String::new());
            l.push(format!("## Closed ({})", self.closed.len()));
            l.extend(self.closed.iter().map(epic_text));
        }
        if !self.ok {
            l.push(String::new());
            l.push("## Problems".into());
            for c in &self.cycles {
                let names: Vec<&str> = c.iter().map(String::as_str).collect();
                l.push(format!("- Cycle: {}", cycle_text(&names)));
            }
            for m in &self.missing {
                l.push(format!(
                    "- {} comes after \"{}\", which {}",
                    m.epic, m.after, m.reason
                ));
            }
            if !self.missing.is_empty() {
                l.push(
                    "An epic that is missing gets created with that name: `kontext epic add \"<title>\" \
                     --goal <G> --name <name>`."
                        .into(),
                );
            }
            if !self.cycles.is_empty() {
                l.push(
                    "`kontext epic add` won't close a cycle; this one came from another machine or \
                     another version. An epic's dependencies can't be edited: escalate it to the \
                     owner (`kontext ask --to owner`) before you launch those epics."
                        .into(),
                );
            }
        }
        l.join("\n") + "\n"
    }

    pub fn json(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(name: &str, after: &[&str]) -> Node {
        Node {
            id: format!("E-{name}"),
            name: name.into(),
            title: name.to_uppercase(),
            after: after.iter().map(|s| s.to_string()).collect(),
            active: true,
            tasks: 2,
            closed_tasks: 0,
            features: 0,
            done_features: 0,
            open_loose: 0,
        }
    }

    fn names(w: &Wave) -> Vec<&str> {
        w.epics.iter().map(|e| e.name.as_str()).collect()
    }

    #[test]
    fn serial_order() {
        // c after b, b after a: three waves of one each.
        let p = compute(&[node("c", &["b"]), node("a", &[]), node("b", &["E-a"])]);
        assert!(p.ok);
        let w: Vec<Vec<&str>> = p.waves.iter().map(names).collect();
        assert_eq!(w, vec![vec!["a"], vec!["b"], vec!["c"]]);
        assert!(p.waves.iter().all(|w| !w.parallel));
        assert_eq!(p.waves[2].epics[0].after, vec!["b"]);
    }

    #[test]
    fn parallel_wave_and_closed_deps() {
        // foundations → (payments, catalog, notices) → integration; "old" is closed.
        let mut old = node("old", &[]);
        old.closed_tasks = 2;
        let p = compute(&[
            old,
            node("foundations", &["old"]),
            node("payments", &["foundations"]),
            node("catalog", &["epic:foundations"]),
            node("notices", &["foundations", "old"]),
            node("integration", &["payments", "catalog", "notices"]),
        ]);
        assert!(p.ok, "{p:?}");
        let w: Vec<Vec<&str>> = p.waves.iter().map(names).collect();
        assert_eq!(
            w,
            vec![
                vec!["foundations"],
                vec!["payments", "catalog", "notices"],
                vec!["integration"]
            ]
        );
        assert!(!p.waves[0].parallel && p.waves[1].parallel && !p.waves[2].parallel);
        assert_eq!(p.closed.len(), 1);
        // A closed dependency is not listed as pending.
        assert!(p.waves[0].epics[0].after.is_empty());
        let text = p.text();
        assert!(text.contains("## Wave 2 (in parallel: 3 epics"), "{text}");
        assert!(text.contains("## Closed (1)"), "{text}");
    }

    #[test]
    fn epic_without_tasks_is_open() {
        let mut a = node("a", &[]);
        a.tasks = 0;
        let p = compute(&[a, node("b", &["a"])]);
        assert_eq!(p.waves.len(), 2);
        assert!(p.text().contains("(no tasks)"));
    }

    #[test]
    fn cycle_is_reported_and_blocks_dependents() {
        let p = compute(&[
            node("base", &[]),
            node("a", &["base", "c"]),
            node("b", &["a"]),
            node("c", &["b"]),
            node("d", &["c"]),
            node("self", &["self"]),
        ]);
        assert!(!p.ok);
        assert_eq!(
            p.cycles,
            vec![
                vec!["a".to_string(), "c".into(), "b".into()],
                vec!["self".to_string()]
            ]
        );
        let w: Vec<Vec<&str>> = p.waves.iter().map(names).collect();
        assert_eq!(w, vec![vec!["base"]]);
        let blocked: Vec<(&str, &str)> = p
            .blocked
            .iter()
            .map(|b| (b.epic.name.as_str(), b.reason.as_str()))
            .collect();
        assert_eq!(blocked.len(), 5);
        assert!(blocked.contains(&("a", "is in a cycle: a → c → b → a")));
        assert!(blocked.contains(&("d", "waits for c, which has no wave")));
        let text = p.text();
        assert!(text.contains("- Cycle: a → c → b → a"), "{text}");
        assert!(text.contains("- Cycle: self → self"), "{text}");
    }

    #[test]
    fn missing_and_inactive_dependencies() {
        let mut old = node("old", &[]);
        old.active = false;
        let p = compute(&[
            old,
            node("a", &["ghost"]),
            node("b", &["old"]),
            node("c", &["a"]),
            node("d", &[]),
        ]);
        assert!(!p.ok);
        assert_eq!(
            p.missing,
            vec![
                Missing {
                    epic: "a".into(),
                    after: "ghost".into(),
                    reason: "does not exist".into()
                },
                Missing {
                    epic: "b".into(),
                    after: "old".into(),
                    reason: "no longer in force (E-old)".into()
                },
            ]
        );
        let w: Vec<Vec<&str>> = p.waves.iter().map(names).collect();
        assert_eq!(w, vec![vec!["d"]]);
        let reasons: BTreeMap<&str, &str> = p
            .blocked
            .iter()
            .map(|b| (b.epic.name.as_str(), b.reason.as_str()))
            .collect();
        assert_eq!(reasons["a"], "depends on \"ghost\", which does not exist");
        assert_eq!(reasons["c"], "waits for a, which has no wave");
        assert!(
            p.text()
                .contains("- a comes after \"ghost\", which does not exist")
        );
        // The replaced epic doesn't show up in any list.
        assert!(!p.text().contains("E-old old"));
    }

    #[test]
    fn an_epic_with_features_closes_when_its_features_are_done() {
        // All tasks closed, but one of two features not validated yet: open.
        let mut a = node("a", &[]);
        a.closed_tasks = 2;
        a.features = 2;
        a.done_features = 1;
        let p = compute(&[a.clone(), node("b", &["a"])]);
        assert_eq!(p.waves.len(), 2);
        assert!(
            p.text().contains("(1/2 features done; 2/2 tasks closed)"),
            "{}",
            p.text()
        );
        assert_eq!(p.json()["waves"][0]["epics"][0]["features"]["done"], 1);
        // Without features the key is not there.
        assert!(p.json()["waves"][1]["epics"][0].get("features").is_none());
        a.done_features = 2;
        let p = compute(&[a, node("b", &["a"])]);
        assert_eq!(p.closed.len(), 1);
        assert_eq!(p.waves.len(), 1);
    }

    #[test]
    fn deterministic() {
        let n = [node("a", &[]), node("b", &["a"]), node("c", &["a"])];
        assert_eq!(compute(&n), compute(&n));
        assert_eq!(compute(&n).text(), compute(&n).text());
    }
}
