//! The signed scope: `kontext scope summary`, `kontext scope sign`, the
//! pending scope change, `kontext feature drop` and putting a feature back.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{Project, git};
use serde_json::Value;

const PM: &[(&str, &str)] = &[("KONTEXT_ACTOR", "agent:pm"), ("KONTEXT_ROLE", "pm")];

fn pm_ok(p: &Project, args: &[&str]) -> String {
    p.kontext_env_ok(PM, args)
}

fn pm_id(p: &Project, args: &[&str]) -> String {
    p.id_of(PM, args)
}

fn code(p: &Project, envs: &[(&str, &str)], args: &[&str]) -> (i32, String, String) {
    let out = p.kontext_env(envs, args);
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

/// What the pm writes in phase 0, nothing signed: a charter, a goal, two
/// epics (payments after foundations) and three features.
struct Planned {
    p: Project,
    goal: String,
    found: String,
    pay: String,
    card: String,
    refunds: String,
}

fn planned() -> Planned {
    let p = Project::new();
    pm_ok(
        &p,
        &[
            "charter",
            "--objective",
            "Sell tickets online",
            "--non-goal",
            "No mobile app",
            "--done-when",
            "First event sold out",
        ],
    );
    let goal = pm_id(
        &p,
        &[
            "goal",
            "add",
            "Card payments",
            "--done-when",
            "A real charge",
        ],
    );
    let found = pm_id(
        &p,
        &[
            "epic",
            "add",
            "Foundations",
            "--goal",
            &goal,
            "--name",
            "found",
        ],
    );
    let pay = pm_id(
        &p,
        &[
            "epic", "add", "Payments", "--goal", &goal, "--name", "pay", "--after", "found",
        ],
    );
    pm_ok(
        &p,
        &[
            "feature",
            "add",
            "Schema",
            "--epic",
            "found",
            "--acceptance",
            "tables exist",
        ],
    );
    let card = pm_id(
        &p,
        &[
            "feature",
            "add",
            "Card charge",
            "--epic",
            "pay",
            "--acceptance",
            "the buyer pays and gets a receipt",
        ],
    );
    let refunds = pm_id(
        &p,
        &[
            "feature",
            "add",
            "Refunds",
            "--epic",
            "pay",
            "--acceptance",
            "the owner refunds from the admin",
        ],
    );
    Planned {
        p,
        goal,
        found,
        pay,
        card,
        refunds,
    }
}

fn summary(p: &Project) -> Value {
    serde_json::from_str(&pm_ok(p, &["--json", "scope", "summary"])).unwrap()
}

fn hash(p: &Project) -> String {
    summary(p)["hash"].as_str().unwrap().to_string()
}

/// The pm signs on the owner's yes.
fn sign(p: &Project) -> String {
    let h = hash(p);
    pm_id(
        p,
        &[
            "scope",
            "sign",
            "--hash",
            &h,
            "--approved-by",
            "omar",
            "--said",
            "sí, adelante",
        ],
    )
}

fn sha256(s: &str) -> String {
    use sha2::{Digest, Sha256};
    let d = Sha256::digest(s.as_bytes());
    d.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn the_plan_summary_says_what_is_signed_and_ends_with_its_hash() {
    let t = planned();
    let text = pm_ok(&t.p, &["scope", "summary"]);
    for want in [
        "# Plan for demo",
        "Objective: Sell tickets online",
        "- No mobile app",
        "Done when: First event sold out",
        "## Goals (1)",
        "Card payments. Done when: A real charge",
        "## Wave 1 (in series)",
        "Foundations (coordinator epic:found)",
        "## Wave 2 (in series)",
        "Card charge: when it works, the human sees: the buyer pays and gets a receipt",
        "2 epics, 3 features, 2 waves; one coordinator per epic (2).",
    ] {
        assert!(text.contains(want), "missing {want:?} in:\n{text}");
    }
    let v = summary(&t.p);
    assert_eq!(v["type"], "plan");
    let h = v["hash"].as_str().unwrap();
    assert_eq!(h, sha256(v["summary"].as_str().unwrap()));
    let last = text.trim_end().lines().last().unwrap();
    assert!(last.starts_with(&format!("Hash: {h}")), "{last}");
    // It covers the charter, the goal, both epics and the three features.
    let covers: Vec<&str> = v["covers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap())
        .collect();
    for id in [&t.goal, &t.found, &t.pay, &t.card, &t.refunds] {
        assert!(covers.contains(&id.as_str()), "{id} not in {covers:?}");
        assert!(text.contains(id.as_str()), "{id} not in the text");
    }
}

#[test]
fn the_hash_is_deterministic_and_task_work_does_not_move_it() {
    let t = planned();
    let h1 = hash(&t.p);
    assert_eq!(h1, hash(&t.p));
    let task = pm_id(
        &t.p,
        &["task", "add", "Charge endpoint", "--feature", &t.card],
    );
    t.p.kontext_ok(&["done", &task, "--evidence", "cmd:\"true\"=0"]);
    pm_ok(&t.p, &["log", "worked on it", "--task", &task]);
    assert_eq!(h1, hash(&t.p), "tasks are not scope");
    // A decision is not scope either.
    t.p.kontext_ok(&["decide", "Use Stripe", "--why", "3DS"]);
    let v = summary(&t.p);
    assert_ne!(
        h1,
        v["hash"].as_str().unwrap(),
        "the decision count is part of the text"
    );
    assert!(
        v["summary"]
            .as_str()
            .unwrap()
            .contains("1 decision in force")
    );
}

#[test]
fn sign_refuses_a_wrong_hash_a_non_owner_and_the_pm_without_approval() {
    let t = planned();
    let h = hash(&t.p);
    // The pm alone.
    let (c, _, err) = code(&t.p, PM, &["scope", "sign", "--hash", &h]);
    assert_eq!(c, 2, "{err}");
    assert!(err.contains("only an owner signs the scope"), "{err}");
    // A human who is not an owner.
    let (c, _, err) = code(
        &t.p,
        &[("KONTEXT_ACTOR", "human:ana")],
        &["scope", "sign", "--hash", &h],
    );
    assert_eq!(c, 2, "{err}");
    // A hash that is not the summary's.
    let (c, _, err) = code(
        &t.p,
        PM,
        &[
            "scope",
            "sign",
            "--hash",
            "0123456789ab",
            "--approved-by",
            "omar",
        ],
    );
    assert_eq!(c, 2, "{err}");
    assert!(
        err.contains("the scope changed since that summary; show it again"),
        "{err}"
    );
    // The scope changed after the summary was shown.
    pm_ok(&t.p, &["feature", "add", "Invoices", "--epic", "pay"]);
    let (c, _, err) = code(
        &t.p,
        PM,
        &["scope", "sign", "--hash", &h, "--approved-by", "omar"],
    );
    assert_eq!(c, 2, "{err}");
    assert!(err.contains("show it again"), "{err}");
    // Nothing was written.
    assert!(
        t.p.entries("omar@test")
            .iter()
            .all(|e| e["kind"] != "scope"),
        "a refused signature writes nothing"
    );
}

#[test]
fn signing_stores_exactly_the_summary_shown_and_show_prints_it() {
    let t = planned();
    let v = summary(&t.p);
    let s = sign(&t.p);
    assert!(s.starts_with("S-"), "{s}");
    let e =
        t.p.entries("omar@test")
            .into_iter()
            .find(|e| e["id"] == s.as_str())
            .unwrap();
    assert_eq!(e["kind"], "scope");
    assert_eq!(e["fields"]["type"], "plan");
    assert_eq!(e["fields"]["summary"], v["summary"]);
    assert_eq!(e["fields"]["summary_hash"], v["hash"]);
    assert_eq!(e["fields"]["said"], "sí, adelante");
    assert_eq!(e["fields"]["approved_by"], "omar");
    assert_eq!(e["fields"]["covers"], v["covers"]);
    let show = t.p.kontext_ok(&["show", &s]);
    assert!(show.contains("- **Type:** plan"), "{show}");
    assert!(
        show.contains("- **The owner said:** sí, adelante"),
        "{show}"
    );
    assert!(show.contains(v["summary"].as_str().unwrap()), "{show}");
    // Now there is nothing to sign.
    let out = pm_ok(&t.p, &["scope", "summary"]);
    assert!(
        out.starts_with(&format!("Nothing to sign: the scope signed in {s}")),
        "{out}"
    );
    // An owner signs too, without --approved-by.
    pm_ok(&t.p, &["feature", "add", "Invoices", "--epic", "pay"]);
    let h = hash(&t.p);
    t.p.kontext_ok(&["scope", "sign", "--hash", &h[..8]]);
    assert!(pm_ok(&t.p, &["scope", "summary"]).starts_with("Nothing to sign"));
}

#[test]
fn nothing_to_sign_without_a_charter() {
    let p = Project::new();
    let (c, out, _) = code(&p, PM, &["scope", "summary"]);
    assert_eq!(c, 0);
    assert!(
        out.starts_with("Nothing to sign: there is no charter yet"),
        "{out}"
    );
    let (c, _, err) = code(
        &p,
        PM,
        &[
            "scope",
            "sign",
            "--hash",
            "0123456789",
            "--approved-by",
            "omar",
        ],
    );
    assert_eq!(c, 2, "{err}");
}

#[test]
fn a_change_is_derived_from_the_signed_scope() {
    let t = planned();
    sign(&t.p);
    // A decision, a task and a validation are not scope changes.
    pm_ok(&t.p, &["decide", "Use Stripe", "--approved-by", "omar"]);
    let task = pm_id(&t.p, &["task", "add", "Charge", "--feature", &t.card]);
    t.p.kontext_ok(&["done", &task, "--evidence", "cmd:\"true\"=0"]);
    t.p.kontext_ok(&["done", &t.card, "--evidence", "cmd:\"true\"=0"]);
    assert_eq!(summary(&t.p)["type"], "nothing");
    // A feature enters, another leaves.
    let inv = pm_id(
        &t.p,
        &[
            "feature",
            "add",
            "Invoices",
            "--epic",
            "pay",
            "--acceptance",
            "a PDF per order",
        ],
    );
    let rt = pm_id(
        &t.p,
        &["task", "add", "Refund API", "--feature", &t.refunds],
    );
    t.p.kontext_ok(&["done", &rt, "--evidence", "cmd:\"true\"=0"]);
    pm_ok(
        &t.p,
        &["feature", "drop", &t.refunds, "--why", "not for v1"],
    );
    let v = summary(&t.p);
    assert_eq!(v["type"], "change");
    assert_eq!(v["adds"], serde_json::json!([inv]));
    assert_eq!(v["removes"], serde_json::json!([t.refunds]));
    let text = v["summary"].as_str().unwrap();
    for want in [
        "# Scope change for demo",
        "## Enters (1)",
        "Invoices, in epic Payments: when it works, the human sees: a PDF per order",
        "## Leaves (1)",
        "Refunds (dropped)",
        &format!("Work thrown away: 1 closed task ({rt})"),
        "## Epics it touches\nPayments",
        "Unchanged: Wave 1: Foundations; Wave 2: Payments.",
    ] {
        assert!(text.contains(want), "missing {want:?} in:\n{text}");
    }
    // Signing the change: nothing pending, and it says what it changed.
    let s = sign(&t.p);
    assert_eq!(summary(&t.p)["type"], "nothing");
    let e =
        t.p.entries("omar@test")
            .into_iter()
            .find(|e| e["id"] == s.as_str())
            .unwrap();
    assert_eq!(e["fields"]["type"], "change");
    assert_eq!(e["fields"]["adds"], serde_json::json!([inv]));
    assert_eq!(e["fields"]["removes"], serde_json::json!([t.refunds]));
}

#[test]
fn an_epic_that_enters_lists_its_features_and_the_waves_say_what_changes() {
    let t = planned();
    sign(&t.p);
    pm_id(
        &t.p,
        &[
            "epic", "add", "Catalog", "--goal", &t.goal, "--name", "cat", "--after", "found",
        ],
    );
    pm_ok(
        &t.p,
        &[
            "feature",
            "add",
            "Search",
            "--epic",
            "cat",
            "--acceptance",
            "finds",
        ],
    );
    let text = summary(&t.p)["summary"].as_str().unwrap().to_string();
    assert!(text.contains("- Epic E-"), "{text}");
    assert!(text.contains("  - U-"), "{text}");
    assert!(
        !text.contains("- Feature U-"),
        "a feature of an epic that enters is listed under it:\n{text}"
    );
    assert!(
        text.contains("Now: Wave 1: Foundations; Wave 2: Payments, Catalog."),
        "{text}"
    );
    assert!(
        text.contains("Before: Wave 1: Foundations; Wave 2: Payments."),
        "{text}"
    );
}

#[test]
fn drop_and_put_back() {
    let t = planned();
    // Only the pm or an owner drops.
    let (c, _, err) = code(
        &t.p,
        &[
            ("KONTEXT_ACTOR", "agent:epic:pay"),
            ("KONTEXT_ROLE", "coordinator"),
        ],
        &["feature", "drop", &t.refunds, "--why", "x"],
    );
    assert_eq!(c, 2, "{err}");
    sign(&t.p);
    pm_ok(
        &t.p,
        &["feature", "drop", &t.refunds, "--why", "not for v1"],
    );
    let show = t.p.kontext_ok(&["show", &t.refunds]);
    assert!(show.contains("[dropped]"), "{show}");
    assert!(show.contains("waiting for the owner's signature"), "{show}");
    assert_eq!(summary(&t.p)["removes"], serde_json::json!([t.refunds]));
    // A dropped feature does not take tasks and can't be dropped again.
    let (c, _, err) = code(&t.p, PM, &["task", "add", "X", "--feature", &t.refunds]);
    assert_eq!(c, 2, "{err}");
    let (c, _, _) = code(&t.p, PM, &["feature", "drop", &t.refunds, "--why", "again"]);
    assert_eq!(c, 2);
    // Put back: nothing pending.
    pm_ok(&t.p, &["reopen", &t.refunds, "--why", "the owner wants it"]);
    assert_eq!(summary(&t.p)["type"], "nothing");
    let show = t.p.kontext_ok(&["show", &t.refunds]);
    assert!(!show.contains("[dropped]"), "{show}");
    // A reopen of a feature that is neither validated nor dropped is refused.
    let (c, _, _) = code(&t.p, PM, &["reopen", &t.refunds, "--why", "x"]);
    assert_eq!(c, 2);
}

#[test]
fn a_dropped_feature_does_not_hold_its_epic() {
    let t = planned();
    let task = pm_id(&t.p, &["task", "add", "Charge", "--feature", &t.card]);
    t.p.kontext_ok(&["done", &task, "--evidence", "cmd:\"true\"=0"]);
    t.p.kontext_ok(&["done", &t.card, "--evidence", "cmd:\"true\"=0"]);
    let open = pm_id(&t.p, &["task", "add", "Refund", "--feature", &t.refunds]);
    pm_ok(
        &t.p,
        &["feature", "drop", &t.refunds, "--why", "not for v1"],
    );
    let plan: Value = serde_json::from_str(&t.p.kontext_ok(&["--json", "plan"])).unwrap();
    // Its only other feature is done, so the epic closes.
    let pay = plan["closed"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "pay")
        .unwrap_or_else(|| panic!("pay is not closed: {plan}"));
    assert_eq!(pay["features"]["total"], 1, "{pay}");
    // Its open task raises an alert.
    let out = t.p.kontext_ok(&["standup", "--peek"]);
    assert!(
        out.contains(&format!(
            "open task {open} of dropped feature {}",
            t.refunds
        )),
        "{out}"
    );
}

/// A git repo with `.pm/items.md` (these rows).
fn pm_repo(p: &Project, rows: &str) -> PathBuf {
    let repo = p.dir.path().join("app");
    fs::create_dir_all(repo.join(".pm/dumps")).unwrap();
    git(&repo, &["init", "-q"]);
    fs::write(
        repo.join(".pm/items.md"),
        format!(
            "# Ideas\n\n| id | idea | source | state | where | evidence |\n|---|---|---|---|---|---|\n{rows}"
        ),
    )
    .unwrap();
    repo
}

fn in_dir(p: &Project, dir: &Path, args: &[&str]) -> (i32, String, String) {
    let mut c = p.cmd(args);
    c.current_dir(dir)
        .env("KONTEXT_ACTOR", "agent:pm")
        .env("KONTEXT_ROLE", "pm");
    let out = c.output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

#[test]
fn a_plan_with_ideas_not_placed_is_refused() {
    let t = planned();
    let repo = pm_repo(
        &t.p,
        &format!(
            "| I1 | card payments | | | {} | |\n| I2 | a loyalty program | | | | |\n",
            t.card
        ),
    );
    let (c, out, _) = in_dir(&t.p, &repo, &["scope", "summary"]);
    assert_eq!(c, 0);
    assert!(
        out.contains("Not placed: 1 of your ideas. Place or discard them before signing:\n- I2"),
        "{out}"
    );
    let v: Value =
        serde_json::from_str(&in_dir(&t.p, &repo, &["--json", "scope", "summary"]).1).unwrap();
    let h = v["hash"].as_str().unwrap();
    let (c, _, err) = in_dir(
        &t.p,
        &repo,
        &["scope", "sign", "--hash", h, "--approved-by", "omar"],
    );
    assert_eq!(c, 2, "{err}");
    assert!(
        err.contains("1 of the user's ideas are not placed"),
        "{err}"
    );
    assert!(err.contains("I2"), "{err}");
    // A discarded idea counts as placed.
    fs::write(
        repo.join(".pm/items.md"),
        format!(
            "| id | idea | source | state | where | evidence |\n|---|---|---|---|---|---|\n\
             | I1 | card payments | | | {} | |\n| I2 | a loyalty program | | discarded | | |\n",
            t.card
        ),
    )
    .unwrap();
    let v: Value =
        serde_json::from_str(&in_dir(&t.p, &repo, &["--json", "scope", "summary"]).1).unwrap();
    let h = v["hash"].as_str().unwrap();
    assert!(
        v["summary"]
            .as_str()
            .unwrap()
            .contains("Not placed: 0 of your ideas.")
    );
    let (c, _, err) = in_dir(
        &t.p,
        &repo,
        &["scope", "sign", "--hash", h, "--approved-by", "omar"],
    );
    assert_eq!(c, 0, "{err}");
}

#[test]
fn an_idea_whose_feature_was_dropped_waits_then_counts_as_discarded() {
    let t = planned();
    let repo = pm_repo(&t.p, &format!("| I1 | refunds | | | {} | |\n", t.refunds));
    let v: Value =
        serde_json::from_str(&in_dir(&t.p, &repo, &["--json", "scope", "summary"]).1).unwrap();
    let h = v["hash"].as_str().unwrap().to_string();
    let (c, _, err) = in_dir(
        &t.p,
        &repo,
        &["scope", "sign", "--hash", &h, "--approved-by", "omar"],
    );
    assert_eq!(c, 0, "{err}");
    pm_ok(
        &t.p,
        &["feature", "drop", &t.refunds, "--why", "not for v1"],
    );
    let ideas: Value = serde_json::from_str(&in_dir(&t.p, &repo, &["--json", "ideas"]).1).unwrap();
    assert_eq!(ideas["not_placed"][0]["id"], "I1", "{ideas}");
    assert_eq!(
        ideas["not_placed"][0]["cites"],
        format!("feature {} dropped, waiting for your signature", t.refunds)
    );
    // The change says which ideas leave with it.
    let v: Value =
        serde_json::from_str(&in_dir(&t.p, &repo, &["--json", "scope", "summary"]).1).unwrap();
    assert!(
        v["summary"]
            .as_str()
            .unwrap()
            .contains("Your ideas that pointed to it, which count as discarded once you sign: I1"),
        "{v}"
    );
    let h = v["hash"].as_str().unwrap().to_string();
    let (c, out, err) = in_dir(
        &t.p,
        &repo,
        &[
            "--json",
            "scope",
            "sign",
            "--hash",
            &h,
            "--approved-by",
            "omar",
        ],
    );
    assert_eq!(c, 0, "{err}");
    let s: Value = serde_json::from_str(&out).unwrap();
    let ideas: Value = serde_json::from_str(&in_dir(&t.p, &repo, &["--json", "ideas"]).1).unwrap();
    assert_eq!(ideas["not_placed"], serde_json::json!([]));
    assert_eq!(ideas["ideas"][0]["state"], "discarded");
    let notes = ideas["ideas"][0]["notes"].to_string();
    assert!(notes.contains(s["id"].as_str().unwrap()), "{notes}");
}

#[test]
fn verify_checks_the_summary_hash() {
    let t = planned();
    sign(&t.p);
    t.p.kontext_ok(&["verify"]);
    // Another tool changes the signed summary and keeps the chain valid
    // (new id and hash): the summary no longer matches its summary_hash.
    let path = t.p.ledger("omar@test");
    let text = fs::read_to_string(&path).unwrap();
    let mut lines: Vec<Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let i = lines.iter().position(|e| e["kind"] == "scope").unwrap();
    assert_eq!(i, lines.len() - 1, "the signature is the last line");
    let mut e = lines[i].clone();
    e["fields"]["summary"] = Value::String("# Plan for demo\n\nsomething else".into());
    e["id"] = Value::String(kontext::model::compute_id(&e).unwrap());
    let h = kontext::model::compute_hash(&e);
    e["hash"] = Value::String(h);
    lines[i] = e;
    let out: String = lines
        .iter()
        .map(|l| kontext::model::canonical(l) + "\n")
        .collect();
    fs::write(&path, out).unwrap();
    let (c, out, _) = code(&t.p, &[], &["verify"]);
    assert_eq!(c, 1, "{out}");
    assert!(
        out.contains("the summary_hash of the scope signature does not match its summary"),
        "{out}"
    );
    // And it does not count: there is no signed plan.
    t.p.kontext_ok(&["reindex"]);
    assert_eq!(summary(&t.p)["type"], "plan");
}
