//! What can be added to an epic after it was written (DESIGN §4.3, "Planning
//! checks"): a dependency on another epic (`after`) and the paths it is
//! expected to touch (`touches`).
//!
//! Both are records (`refs[0]` is the epic) that only the pm or an owner
//! writes, like a feature. Neither is scope: one added after the signature
//! holds no task and asks for no signature (execution order is the pm's
//! call). `kontext plan` reads an epic's dependencies as its own
//! `fields.depends_on` plus every `after` record that counts
//! ([`crate::plan::node`]), so the tree, the summary and the cycle check see
//! the same waves.

use anyhow::{Result, bail};
use serde_json::json;

use crate::model::{Actor, Entry, RawEntry};
use crate::state::{State, Status, string_list};
use crate::store::{Refused, Store};
use crate::tasks::{Written, epic_name};

fn refused<T>(msg: impl Into<String>) -> Result<T> {
    Err(Refused(msg.into()).into())
}

/// The record kinds this module reads, all with `refs[0]` = the epic.
pub const EPIC_RECORDS: &[&str] = &["after", "touches"];

/// The `after` and `touches` records of an epic that count, in `hlc` order.
pub fn records<'a>(st: &'a State, epic: &str, kind: &str) -> Vec<&'a Entry> {
    st.epic_records
        .get(epic)
        .map(|ids| {
            ids.iter()
                .filter_map(|id| st.get(id))
                .filter(|e| e.kind == kind)
                .collect()
        })
        .unwrap_or_default()
}

/// The dependencies `after` records added to an epic (ids), in order.
pub fn added_after(st: &State, epic: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for r in records(st, epic, "after") {
        for d in string_list(r.fields.get("depends_on")) {
            if !out.contains(&d) {
                out.push(d);
            }
        }
    }
    out
}

/// The paths an epic is expected to touch (every `touches` record that
/// counts, in order, without repeats).
pub fn touches(st: &State, epic: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for r in records(st, epic, "touches") {
        for p in string_list(r.fields.get("paths")) {
            if !out.contains(&p) {
                out.push(p);
            }
        }
    }
    out
}

/// A path as `touches` stores it: relative, `/`-separated, with no `./` in
/// front and no `/` at the end. A folder covers everything under it.
pub fn normalize_path(p: &str) -> Result<String> {
    let mut s = p.trim().replace('\\', "/");
    while let Some(rest) = s.strip_prefix("./") {
        s = rest.to_string();
    }
    while s.ends_with('/') {
        s.pop();
    }
    while s.contains("//") {
        s = s.replace("//", "/");
    }
    if s.is_empty() || s == "." {
        bail!("an empty path: give a file or a folder of the repo (src/billing covers all of it)");
    }
    if s.contains(['*', '?', '[']) {
        bail!(
            "{p:?} is a glob: give the folder or the file instead (src/billing covers everything \
             under it)"
        );
    }
    if s.split('/').any(|c| c == "..") {
        bail!("{p:?} leaves the repo: give a path inside it");
    }
    Ok(s)
}

/// The epic named by `s`, which must be in force.
fn epic_in_force<'a>(st: &'a State, s: &str) -> Result<&'a Entry> {
    let id = match st.find_epic(s) {
        Ok(id) => id,
        Err(e) => return refused(e.to_string()),
    };
    let e = st.get(&id).expect("found");
    if st.status_of(&id) != Some(Status::Active) {
        return refused(format!("{id} {} is no longer in force", e.title.trim()));
    }
    Ok(e)
}

fn may_write(st: &State, actor: &Actor, what: &str) -> Result<()> {
    if crate::features::may_write(st, actor).is_err() {
        return refused(format!(
            "only the pm or an owner adds {what} to an epic ({} is neither). A coordinator who \
             needs one asks the pm: `kontext ask --to pm`",
            actor.short()
        ));
    }
    if let Some(why) = st.policy.own_limit(actor, None) {
        return refused(why);
    }
    Ok(())
}

/// `kontext epic after <epic> <dep>… --why`: the epic waits for these
/// epics, which already exist. Refuses (exit 2) an epic or dependency not in
/// force, the epic itself, one it already waits for, and a cycle.
pub fn add_after(
    store: &mut Store,
    actor: &Actor,
    epic: &str,
    deps: &[String],
    why: &str,
) -> Result<Written> {
    if why.trim().is_empty() {
        bail!("--why is required: say why it has to wait (it shows in the plan the owner signs)");
    }
    if deps.iter().all(|d| d.trim().is_empty()) {
        bail!("name at least one epic it comes after");
    }
    let entries = {
        let ((), w) = store.transact(|b| {
            let st = b.state()?;
            may_write(st, actor, "a dependency")?;
            let e = epic_in_force(st, epic)?;
            let name = epic_name(e);
            let all = crate::plan::nodes(st);
            let node = crate::plan::node(st, e);
            let mut new: Vec<String> = Vec::new();
            for d in deps.iter().map(|d| d.trim()).filter(|d| !d.is_empty()) {
                let dep = epic_in_force(st, d)?;
                if dep.id == e.id {
                    return refused(format!("{name} cannot come after itself"));
                }
                let already = node
                    .after
                    .iter()
                    .any(|a| crate::plan::resolve(&all, a).is_some_and(|n| n.id == dep.id));
                if already {
                    return refused(format!(
                        "{name} already comes after {} ({})",
                        epic_name(dep),
                        dep.id
                    ));
                }
                if !new.contains(&dep.id) {
                    new.push(dep.id.clone());
                }
            }
            // The plan with the new edges, every epic counted as open (a
            // closed one can't close a cycle either): refused if it has one.
            let mut nodes = all.clone();
            for n in nodes.iter_mut() {
                n.features = 0;
                n.tasks = 0;
                if n.id == e.id {
                    n.after.extend(new.iter().cloned());
                }
            }
            let plan = crate::plan::compute(&nodes);
            if let Some(c) = plan.cycles.iter().find(|c| c.contains(&name)) {
                let mut l = c.clone();
                l.push(c[0].clone());
                return refused(format!(
                    "{name} after {}: would close a cycle ({})",
                    new.join(", "),
                    l.join(" → ")
                ));
            }
            let titles: Vec<String> = new
                .iter()
                .filter_map(|id| st.get(id))
                .map(epic_name)
                .collect();
            let mut r = RawEntry::new("after", &format!("{name} after {}", titles.join(", ")));
            r.actor = Some(actor.clone());
            r.why = why.trim().to_string();
            r.refs = std::iter::once(e.id.clone()).chain(new.clone()).collect();
            r.fields.insert("depends_on".into(), json!(new));
            b.push(r)?;
            Ok(())
        })?;
        w
    };
    Ok(Written {
        entries,
        warnings: Vec::new(),
    })
}

/// `kontext epic touches <epic> <path>… [--why]`: the paths it is expected
/// to touch. Paths it already has are skipped; with none new, refused.
pub fn add_touches(
    store: &mut Store,
    actor: &Actor,
    epic: &str,
    paths: &[String],
    why: &str,
) -> Result<Written> {
    let mut clean: Vec<String> = Vec::new();
    for p in paths {
        let n = normalize_path(p)?;
        if !clean.contains(&n) {
            clean.push(n);
        }
    }
    if clean.is_empty() {
        bail!("name at least one path (a file or a folder of the repo)");
    }
    let ((), entries) = store.transact(|b| {
        let st = b.state()?;
        may_write(st, actor, "paths")?;
        let e = epic_in_force(st, epic)?;
        let have = touches(st, &e.id);
        let new: Vec<String> = clean
            .iter()
            .filter(|p| !have.contains(p))
            .cloned()
            .collect();
        if new.is_empty() {
            return refused(format!(
                "{} already touches {}",
                epic_name(e),
                clean.join(", ")
            ));
        }
        let mut r = RawEntry::new(
            "touches",
            &format!("{} touches {}", epic_name(e), new.join(", ")),
        );
        r.actor = Some(actor.clone());
        r.why = why.trim().to_string();
        r.refs = vec![e.id.clone()];
        r.fields.insert("paths".into(), json!(new));
        b.push(r)?;
        Ok(())
    })?;
    Ok(Written {
        entries,
        warnings: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_normalized() {
        assert_eq!(normalize_path("./src/billing/").unwrap(), "src/billing");
        assert_eq!(normalize_path("src//a.rs").unwrap(), "src/a.rs");
        assert!(normalize_path("src/**").is_err());
        assert!(normalize_path("../x").is_err());
        assert!(normalize_path(" / ").is_err());
    }
}
