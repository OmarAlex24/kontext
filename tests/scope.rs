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
        "## Epic order (by their dependencies, every epic counted as open)\nUnchanged: 1. Foundations; 2. Payments.",
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
fn an_epic_that_enters_lists_its_features_and_the_epic_order_says_what_changes() {
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
        text.contains("Now: 1. Foundations; 2. Payments, Catalog."),
        "{text}"
    );
    assert!(
        text.contains("Before: 1. Foundations; 2. Payments."),
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
fn an_idea_on_a_feature_no_signature_covered_is_not_placed_not_discarded() {
    let t = planned();
    let repo = pm_repo(&t.p, "");
    let h = serde_json::from_str::<Value>(&in_dir(&t.p, &repo, &["--json", "scope", "summary"]).1)
        .unwrap()["hash"]
        .as_str()
        .unwrap()
        .to_string();
    let (c, _, err) = in_dir(
        &t.p,
        &repo,
        &["scope", "sign", "--hash", &h, "--approved-by", "omar"],
    );
    assert_eq!(c, 0, "{err}");
    // A feature enters after the plan, gets an idea, and leaves before any
    // signature covers it; another change is then signed.
    let gift = pm_id(&t.p, &["feature", "add", "Gift cards", "--epic", "pay"]);
    fs::write(
        repo.join(".pm/items.md"),
        format!(
            "| id | idea | source | state | where | evidence |\n|---|---|---|---|---|---|\n\
             | I1 | gift cards | | | {gift} | |\n"
        ),
    )
    .unwrap();
    pm_ok(&t.p, &["feature", "drop", &gift, "--why", "later"]);
    let show = t.p.kontext_ok(&["show", &gift]);
    assert!(
        !show.contains("waiting for the owner's signature"),
        "{show}"
    );
    pm_ok(&t.p, &["feature", "add", "Invoices", "--epic", "pay"]);
    let v: Value =
        serde_json::from_str(&in_dir(&t.p, &repo, &["--json", "scope", "summary"]).1).unwrap();
    let text = v["summary"].as_str().unwrap();
    assert!(text.contains("## Leaves (0)"), "{text}");
    assert!(!text.contains(&gift), "it never entered the scope:\n{text}");
    let (c, _, err) = in_dir(
        &t.p,
        &repo,
        &[
            "scope",
            "sign",
            "--hash",
            v["hash"].as_str().unwrap(),
            "--approved-by",
            "omar",
        ],
    );
    assert_eq!(c, 0, "{err}");
    // The owner never saw it leave: the idea is not placed, not discarded.
    let ideas: Value = serde_json::from_str(&in_dir(&t.p, &repo, &["--json", "ideas"]).1).unwrap();
    assert_eq!(ideas["ideas"][0]["state"], "pending", "{ideas}");
    assert_eq!(ideas["not_placed"][0]["id"], "I1", "{ideas}");
    assert_eq!(
        ideas["not_placed"][0]["cites"],
        format!("feature {gift} dropped (it was never in a signed scope)")
    );
}

#[test]
fn the_summary_says_whether_the_ideas_were_checked() {
    let t = planned();
    let repo = pm_repo(&t.p, "| I1 | a loyalty program | | | | |\n");
    // From a folder with no .pm/items.md the summary says so, in the text
    // that is signed.
    let elsewhere = summary(&t.p);
    let text = elsewhere["summary"].as_str().unwrap();
    assert!(
        text.contains("Ideas not checked: no .pm/items.md found from "),
        "{text}"
    );
    assert!(!text.contains("Not placed"), "{text}");
    // From the repo it says how many are not placed, so the hash shown there
    // is refused by a signature run elsewhere.
    let v: Value =
        serde_json::from_str(&in_dir(&t.p, &repo, &["--json", "scope", "summary"]).1).unwrap();
    assert!(
        v["summary"]
            .as_str()
            .unwrap()
            .contains("Not placed: 1 of your ideas.")
    );
    let (c, _, err) = code(
        &t.p,
        PM,
        &[
            "scope",
            "sign",
            "--hash",
            v["hash"].as_str().unwrap(),
            "--approved-by",
            "omar",
        ],
    );
    assert_eq!(c, 2, "{err}");
    assert!(err.contains("show it again"), "{err}");
    // A change carries the line too.
    sign(&t.p);
    pm_ok(&t.p, &["feature", "add", "Invoices", "--epic", "pay"]);
    let text = summary(&t.p)["summary"].as_str().unwrap().to_string();
    assert!(
        text.contains("## Your ideas\nIdeas not checked: no .pm/items.md found from "),
        "{text}"
    );
    let (_, out, _) = in_dir(&t.p, &repo, &["scope", "summary"]);
    assert!(
        out.contains("## Your ideas\nNot placed: 1 of your ideas.\n- I1"),
        "{out}"
    );
}

#[test]
fn the_summary_never_cuts_what_the_signature_binds() {
    let p = Project::new();
    let long = |what: &str, n: usize| -> String {
        let words: Vec<String> = (0..n).map(|i| format!("{what}{i:03}")).collect();
        words.join(" ")
    };
    let (objective, non_goal, done) = (long("obj", 120), long("non", 60), long("done", 60));
    pm_ok(
        &p,
        &[
            "charter",
            "--objective",
            &objective,
            "--non-goal",
            &non_goal,
            "--done-when",
            &done,
            "--title",
            "Tickets",
        ],
    );
    let (goal_title, goal_done) = (long("goal", 30), long("gdone", 50));
    let goal = pm_id(&p, &["goal", "add", &goal_title, "--done-when", &goal_done]);
    // A goal written by another tool, with a body.
    let body_goal = p
        .append(
            "{\"kind\":\"goal\",\"title\":\"Fast checkout\",\"body\":\"Line one of the body.\\nLine two.\",\"actor\":{\"type\":\"human\",\"name\":\"omar\"},\"fields\":{}}\n",
        )
        .remove(0);
    let epic_title = long("epic", 30);
    pm_ok(
        &p,
        &["epic", "add", &epic_title, "--goal", &goal, "--name", "e1"],
    );
    let (feature_title, acceptance) = (long("feat", 30), long("acc", 80));
    pm_ok(
        &p,
        &[
            "feature",
            "add",
            &feature_title,
            "--epic",
            "e1",
            "--acceptance",
            &acceptance,
        ],
    );
    let text = summary(&p)["summary"].as_str().unwrap().to_string();
    assert!(!text.contains('…'), "{text}");
    for whole in [
        "Charter: Tickets".to_string(),
        format!("Objective: {objective}"),
        format!("- {non_goal}"),
        format!("Done when: {done}"),
        format!("{goal_title}. Done when: {goal_done}"),
        format!("- {body_goal} Fast checkout\n  > Line one of the body.\n  > Line two."),
        format!("{epic_title} (coordinator epic:e1)"),
        format!("{feature_title}: when it works, the human sees: {acceptance}"),
    ] {
        assert!(text.contains(&whole), "missing {whole:?} in:\n{text}");
    }
}

#[test]
fn the_plan_summary_has_the_waves_of_kontext_plan() {
    let t = planned();
    // Foundations is finished: `kontext plan` closes it, Payments is wave 1.
    let schema = summary(&t.p)["covers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap().to_string())
        .find(|id| id.starts_with("U-") && *id != t.card && *id != t.refunds)
        .unwrap();
    let task = pm_id(&t.p, &["task", "add", "Tables", "--feature", &schema]);
    t.p.kontext_ok(&["done", &task, "--evidence", "cmd:\"true\"=0"]);
    t.p.kontext_ok(&["done", &schema, "--evidence", "cmd:\"true\"=0"]);
    let plan: Value = serde_json::from_str(&pm_ok(&t.p, &["--json", "plan"])).unwrap();
    assert_eq!(plan["closed"][0]["id"], t.found.as_str(), "{plan}");
    assert_eq!(plan["waves"][0]["epics"][0]["id"], t.pay.as_str(), "{plan}");
    let text = summary(&t.p)["summary"].as_str().unwrap().to_string();
    assert!(
        text.contains(&format!("## Done (1)\n- {} Foundations", t.found)),
        "{text}"
    );
    assert!(
        text.contains(&format!("## Wave 1 (in series)\n- {} Payments", t.pay)),
        "{text}"
    );
    assert!(!text.contains("## Wave 2"), "{text}");
    assert!(text.contains("2 epics, 3 features, 1 wave;"), "{text}");
}

#[test]
fn a_scope_signature_that_does_not_count_is_a_problem_for_the_owner() {
    let t = planned();
    sign(&t.p);
    // Its summary_hash is not its summary's.
    let bad = t
        .p
        .append(
            "{\"kind\":\"scope\",\"title\":\"Plan signed\",\"actor\":{\"type\":\"human\",\"name\":\"omar\"},\"fields\":{\"type\":\"change\",\"covers\":[],\"adds\":[],\"removes\":[],\"summary\":\"x\",\"summary_hash\":\"00\"}}\n",
        )
        .remove(0);
    // Written by an implementer.
    let x = sha256("x");
    let imp = t
        .p
        .append(&format!(
            "{{\"kind\":\"scope\",\"title\":\"Plan signed\",\"actor\":{{\"type\":\"agent\",\"name\":\"impl1\",\"role\":\"implementer\"}},\"fields\":{{\"type\":\"change\",\"covers\":[],\"adds\":[],\"removes\":[],\"summary\":\"x\",\"summary_hash\":\"{x}\"}}}}\n"
        ))
        .remove(0);
    let owner: Value =
        serde_json::from_str(&t.p.kontext_ok(&["--json", "inbox", "--all"])).unwrap();
    let keys: Vec<&str> = owner["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["key"].as_str().unwrap())
        .collect();
    assert!(keys.contains(&format!("scope:{bad}").as_str()), "{keys:?}");
    assert!(
        keys.contains(&format!("not-counted:{imp}").as_str()),
        "{keys:?}"
    );
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

// ── What a signed plan changes (phase 2, PR 2) ──────────────────────────

const IMPL: &[(&str, &str)] = &[("KONTEXT_ACTOR", "agent:impl1")];

/// The status that `show --json` derives for an id.
fn status(p: &Project, id: &str) -> String {
    let v: Value = serde_json::from_str(&pm_ok(p, &["--json", "show", id])).unwrap();
    v["status"].as_str().unwrap_or_default().to_string()
}

#[test]
fn a_signed_plan_signs_its_charter_and_goals_and_the_pm_decides_alone() {
    let t = planned();
    let charter = summary(&t.p)["covers"][0].as_str().unwrap().to_string();
    assert_eq!(status(&t.p, &t.goal), "unsigned");
    // Before the signature the pm needs the owner's approval.
    let (c, _, err) = code(&t.p, PM, &["decide", "Use Stripe", "--why", "3DS"]);
    assert_eq!(c, 2, "{err}");
    let s = sign(&t.p);
    assert_eq!(status(&t.p, &t.goal), "signed");
    assert_eq!(status(&t.p, &charter), "signed");
    // Inside the signed scope the pm decides, accepts and rejects alone.
    let d = pm_id(&t.p, &["decide", "Use Stripe", "--why", "3DS"]);
    assert_eq!(status(&t.p, &d), "accepted");
    let p1 =
        t.p.id_of(IMPL, &["propose", "Retry the webhook", "--why", "flaky"]);
    let p2 =
        t.p.id_of(IMPL, &["propose", "Rewrite in Go", "--why", "speed"]);
    pm_ok(&t.p, &["accept", &p1]);
    pm_ok(&t.p, &["reject", &p2, "--why", "not now"]);
    assert_eq!(status(&t.p, &p2), "rejected");
    let law: Value =
        serde_json::from_str(&pm_ok(&t.p, &["--json", "resume", "--as", "pm"])).unwrap();
    let titles: Vec<&str> = law["decisions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["title"].as_str().unwrap())
        .collect();
    assert!(titles.contains(&"Use Stripe"), "{titles:?}");
    assert!(titles.contains(&"Retry the webhook"), "{titles:?}");
    // The pm's resume says what it may do alone.
    let resume = pm_ok(&t.p, &["resume", "--as", "pm"]);
    assert!(
        resume.contains(&format!("The scope is signed ({s}")),
        "{resume}"
    );
    // Another agent still does not decide.
    let (c, _, _) = code(&t.p, IMPL, &["decide", "Use Go", "--why", "x"]);
    assert_eq!(c, 2);
    // Verify is still clean.
    pm_ok(&t.p, &["verify"]);
}

#[test]
fn new_scope_waits_for_the_signature_and_signed_work_keeps_moving() {
    let t = planned();
    // Without a signed plan nothing is held back.
    let early = pm_id(&t.p, &["task", "add", "Migrations", "--epic", "found"]);
    let out = t.p.kontext_env_ok(IMPL, &["next", "--epic", "found"]);
    assert!(out.contains(&early), "{out}");
    assert!(!out.contains("Skipped"), "{out}");
    sign(&t.p);
    // A feature enters after the signature, with a task, then a task under a
    // signed feature.
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
    let held = pm_id(&t.p, &["task", "add", "PDF", "--feature", &inv]);
    let free = pm_id(&t.p, &["task", "add", "Charge", "--feature", &t.card]);
    let out = t.p.kontext_env_ok(IMPL, &["next", "--epic", "pay"]);
    assert!(out.contains(&free), "{out}");
    assert!(
        out.contains(&format!(
            "Skipped {held}: waiting for your signature: scope change ({inv} Invoices is not signed yet)."
        )),
        "{out}"
    );
    let v: Value =
        serde_json::from_str(&t.p.kontext_env_ok(IMPL, &["--json", "next", "--epic", "pay"]))
            .unwrap();
    assert_eq!(v["skipped"][0]["task"], held.as_str());
    // assign refuses it.
    let (c, _, err) = code(&t.p, PM, &["assign", &held, "--to", "impl1"]);
    assert_eq!(c, 2, "{err}");
    assert!(
        err.contains("waiting for your signature: scope change"),
        "{err}"
    );
    // The views mention the pending change on one line.
    let line = format!("Scope change waiting for the owner's signature: enters {inv} Invoices.");
    for args in [
        &["plan"][..],
        &["resume", "--as", "pm"],
        &["brief", "pay", "--peek"],
        &["standup", "--peek"],
    ] {
        let out = pm_ok(&t.p, args);
        assert!(out.contains(&line), "{args:?} lacks the line:\n{out}");
    }
    // With the signed task closed, nothing else is handed out.
    t.p.kontext_ok(&["done", &free, "--evidence", "cmd:\"true\"=0"]);
    for args in [
        &["next", "--epic", "pay"][..],
        &["next", "--epic", "pay", "--claim"],
    ] {
        let (c, _, err) = code(&t.p, IMPL, args);
        assert_eq!(c, 2, "{args:?}: {err}");
        assert!(err.contains("No task to hand out in epic pay"), "{err}");
        assert!(err.contains(&held), "{err}");
    }
    // Signed, it goes out, and the views drop the line.
    sign(&t.p);
    let out =
        t.p.kontext_env_ok(IMPL, &["next", "--epic", "pay", "--claim"]);
    assert!(out.contains(&held), "{out}");
    assert!(!pm_ok(&t.p, &["plan"]).contains("Scope change waiting"));
}

#[test]
fn a_feature_replaced_and_signed_out_keeps_its_tasks_off_the_board() {
    let t = planned();
    sign(&t.p);
    let rt = pm_id(
        &t.p,
        &["task", "add", "Refund API", "--feature", &t.refunds],
    );
    let v2 = pm_id(&t.p, &["feature", "add", "Refunds v2", "--epic", "pay"]);
    pm_ok(&t.p, &["supersede", &t.refunds, &v2, "--why", "redesigned"]);
    let (c, _, err) = code(&t.p, PM, &["assign", &rt, "--to", "impl1"]);
    assert_eq!(c, 2, "{err}");
    assert!(err.contains("is leaving the scope"), "{err}");
    let s = sign(&t.p);
    // Signed out: still not handed out, with the same refusal as a drop.
    let (c, _, err) = code(&t.p, PM, &["assign", &rt, "--to", "impl1"]);
    assert_eq!(c, 2, "{err}");
    assert!(
        err.contains(&format!(
            "{} Refunds was replaced by {v2} (the owner signed it out in {s}); close the task or move its work to a feature in force",
            t.refunds
        )),
        "{err}"
    );
    assert!(!err.contains("kontext scope summary"), "{err}");
    let (c, _, err) = code(&t.p, IMPL, &["next", "--epic", "pay", "--claim"]);
    assert!(c == 2 || !err.contains(&rt), "{err}");
    // A feature that entered and was replaced before any signature covered
    // it: its tasks stay off the board too.
    let gift = pm_id(&t.p, &["feature", "add", "Gift cards", "--epic", "pay"]);
    let gt = pm_id(&t.p, &["task", "add", "Codes", "--feature", &gift]);
    let gift2 = pm_id(&t.p, &["feature", "add", "Gift cards v2", "--epic", "pay"]);
    pm_ok(&t.p, &["supersede", &gift, &gift2, "--why", "simpler"]);
    let (c, _, err) = code(&t.p, PM, &["assign", &gt, "--to", "impl1"]);
    assert_eq!(c, 2, "{err}");
    assert!(err.contains("it was never in a signed scope"), "{err}");
}

#[test]
fn a_goal_holds_only_the_tasks_that_cite_it_on_their_own() {
    let t = planned();
    let charge = pm_id(&t.p, &["task", "add", "Charge", "--feature", &t.card]);
    sign(&t.p);
    // The goal is replaced: a pending change, but the signed task under a
    // signed feature and epic keeps going.
    let g2 = pm_id(&t.p, &["goal", "add", "Card and wallet payments"]);
    pm_ok(
        &t.p,
        &[
            "supersede",
            &t.goal,
            &g2,
            "--why",
            "wallets",
            "--approved-by",
            "omar",
        ],
    );
    assert!(
        pm_ok(&t.p, &["plan"]).contains(&format!("leaves {} Card payments", t.goal)),
        "the goal leaving is still a pending change"
    );
    pm_ok(&t.p, &["assign", &charge, "--to", "impl1"]);
    // A task that cites the new goal on its own waits; one that only
    // inherits its feature's goals does not.
    let wallet = pm_id(
        &t.p,
        &["task", "add", "Wallet", "--feature", &t.card, "--goal", &g2],
    );
    let plain = pm_id(&t.p, &["task", "add", "Receipt", "--feature", &t.card]);
    let (c, _, err) = code(&t.p, PM, &["assign", &wallet, "--to", "impl1"]);
    assert_eq!(c, 2, "{err}");
    assert!(
        err.contains(&format!("{g2} Card and wallet payments is not signed yet")),
        "{err}"
    );
    pm_ok(&t.p, &["assign", &plain, "--to", "impl1"]);
}

#[test]
fn the_pm_does_not_reject_the_owners_words_alone() {
    let t = planned();
    sign(&t.p);
    let rule = |title: &str| {
        t.p.append(&format!(
            "{{\"kind\":\"proposal\",\"title\":\"{title}\",\"actor\":{{\"type\":\"agent\",\"name\":\"distill\"}},\
             \"fields\":{{\"source\":\"distill\",\"origin\":\"user\",\"user_rule\":true}}}}\n"
        ))
        .remove(0)
    };
    let euros = rule("Prices in euros");
    let (c, _, err) = code(&t.p, PM, &["reject", &euros, "--why", "not now"]);
    assert_eq!(c, 2, "{err}");
    assert!(err.contains("carries the owner's own words"), "{err}");
    assert_eq!(status(&t.p, &euros), "pending");
    // Accepting it alone is fine: it applies anyway.
    let tax = rule("Show prices with tax");
    pm_ok(&t.p, &["accept", &tax]);
    assert_eq!(status(&t.p, &tax), "accepted");
    // A reject written straight to the ledger is stored and does not count.
    let raw = t
        .p
        .append(&format!(
            "{{\"kind\":\"reject\",\"title\":\"Rejects {euros}\",\"why\":\"x\",\"refs\":[\"{euros}\"],\
             \"actor\":{{\"type\":\"agent\",\"name\":\"pm\",\"role\":\"pm\"}},\"fields\":{{}}}}\n"
        ))
        .remove(0);
    assert_eq!(status(&t.p, &euros), "pending");
    let show = t.p.kontext_ok(&["show", &raw]);
    assert!(
        show.contains("rejected the owner's own words without approved_by"),
        "{show}"
    );
    // With their yes it counts.
    pm_ok(
        &t.p,
        &[
            "reject",
            &euros,
            "--why",
            "they said no",
            "--approved-by",
            "omar",
        ],
    );
    assert_eq!(status(&t.p, &euros), "rejected");

    // Without a signed plan the same raw reject reads as it did before scope.
    let u = planned();
    let rule = |title: &str| {
        u.p.append(&format!(
            "{{\"kind\":\"proposal\",\"title\":\"{title}\",\"actor\":{{\"type\":\"agent\",\"name\":\"distill\"}},\
             \"fields\":{{\"source\":\"distill\",\"origin\":\"user\",\"user_rule\":true}}}}\n"
        ))
        .remove(0)
    };
    let euros = rule("Prices in euros");
    let raw = u
        .p
        .append(&format!(
            "{{\"kind\":\"reject\",\"title\":\"Rejects {euros}\",\"why\":\"x\",\"refs\":[\"{euros}\"],\
             \"actor\":{{\"type\":\"agent\",\"name\":\"pm\",\"role\":\"pm\"}},\"fields\":{{}}}}\n"
        ))
        .remove(0);
    assert_eq!(status(&u.p, &euros), "pending");
    let show = u.p.kontext_ok(&["show", &raw]);
    assert!(
        show.contains("is the pm but has no approved_by from an owner")
            && !show.contains("own words"),
        "{show}"
    );
}

#[test]
fn a_feature_leaving_the_scope_holds_its_tasks_until_signed() {
    let t = planned();
    sign(&t.p);
    let rt = pm_id(
        &t.p,
        &["task", "add", "Refund API", "--feature", &t.refunds],
    );
    pm_ok(
        &t.p,
        &["feature", "drop", &t.refunds, "--why", "not for v1"],
    );
    let (c, _, err) = code(&t.p, PM, &["assign", &rt, "--to", "impl1"]);
    assert_eq!(c, 2, "{err}");
    assert!(err.contains("is leaving the scope"), "{err}");
    // Signed out, its work is still not handed out.
    sign(&t.p);
    let (c, _, err) = code(&t.p, PM, &["assign", &rt, "--to", "impl1"]);
    assert_eq!(c, 2, "{err}");
    assert!(
        err.contains(&format!(
            "{} Refunds was dropped (the owner signed it out in S-",
            t.refunds
        )),
        "{err}"
    );
    pm_ok(&t.p, &["feature", "drop", &t.card, "--why", "later"]);
    let plan = pm_ok(&t.p, &["plan"]);
    assert!(
        plan.contains(&format!("leaves {} Card charge", t.card)),
        "{plan}"
    );
}

// ── The inbox with a signed plan (phase 2, PR 3) ────────────────────────

fn owner_inbox(p: &Project) -> Value {
    serde_json::from_str(&p.kontext_ok(&["--json", "inbox", "--all"])).unwrap()
}

fn pm_inbox(p: &Project) -> Value {
    serde_json::from_str(&pm_ok(p, &["--json", "inbox", "--as", "pm", "--all"])).unwrap()
}

fn keys(v: &Value) -> Vec<String> {
    v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["key"].as_str().unwrap().to_string())
        .collect()
}

fn distill_fact(p: &Project, title: &str) -> String {
    p.append(&format!(
        "{{\"kind\":\"fact\",\"title\":\"{title}\",\"actor\":{{\"type\":\"agent\",\"name\":\"distill\"}},\
         \"fields\":{{\"source\":\"distill\",\"origin\":\"user\",\"fingerprints\":[]}}}}\n"
    ))
    .remove(0)
}

#[test]
fn with_a_signed_plan_the_owner_inbox_holds_the_signature_and_the_pm_the_rest() {
    let t = planned();
    let prop =
        t.p.id_of(IMPL, &["propose", "Cache the price list", "--why", "slow"]);
    distill_fact(&t.p, "Prices are in euros");
    // Without a signed plan: unchanged, the proposal and the review are the
    // owner's, and the pm's inbox is empty.
    let before = keys(&owner_inbox(&t.p));
    assert!(before.contains(&prop), "{before:?}");
    assert!(before.contains(&"review".to_string()), "{before:?}");
    assert!(owner_inbox(&t.p).get("pm_items").is_none());
    assert_eq!(pm_inbox(&t.p)["count"], 0);

    sign(&t.p);
    let q = pm_id(&t.p, &["ask", "Which currency?", "--to", "owner"]);
    let v = owner_inbox(&t.p);
    assert_eq!(keys(&v), vec![q.clone()], "only the question");
    assert_eq!(v["pm_items"], 2);
    let text = t.p.kontext_ok(&["inbox"]);
    assert!(
        text.contains("The pm handles 2 more items inside the signed scope"),
        "{text}"
    );
    let pm = pm_inbox(&t.p);
    assert_eq!(keys(&pm), vec![prop.clone(), "review".to_string()]);
    assert!(pm_ok(&t.p, &["resume", "--as", "pm"]).contains("Your inbox: 2 pending items"));

    // A scope change: at most one signature, with the summary's first lines.
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
    pm_id(&t.p, &["task", "add", "PDF", "--feature", &inv]);
    let v = owner_inbox(&t.p);
    let scope = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["key"] == "scope")
        .expect("the scope signature");
    assert_eq!(scope["class"], "sign");
    assert_eq!(
        scope["title"],
        format!("Scope change: enters {inv} Invoices").as_str()
    );
    assert_eq!(scope["blocks_tasks"], 1);
    assert!(
        scope["subs"].as_array().unwrap().iter().any(|s| s["text"]
            .as_str()
            .unwrap()
            .contains("Invoices, in epic Payments")),
        "{scope:#?}"
    );
    let (c, _, err) = code(&t.p, &[], &["inbox", "sign", "scope"]);
    assert_eq!(c, 2, "{err}");
    assert!(err.contains("kontext scope summary"), "{err}");
    // The report follows the same split.
    let report = t.p.kontext_ok(&["report"]);
    assert!(report.contains("Scope change: enters"), "{report}");
    assert!(!report.contains("Cache the price list"), "{report}");

    // The pm resolves its inbox alone; an implementer does not.
    let (c, _, _) = code(&t.p, IMPL, &["inbox", "--as", "pm", "confirm", "review"]);
    assert_eq!(c, 2);
    pm_ok(&t.p, &["inbox", "--as", "pm", "sign", &prop]);
    assert_eq!(status(&t.p, &prop), "accepted");
    pm_ok(&t.p, &["inbox", "--as", "pm", "confirm", "review"]);
    assert_eq!(pm_inbox(&t.p)["count"], 0);
    // The owner's question is not the pm's to close.
    let (c, _, _) = code(&t.p, PM, &["inbox", "--as", "pm", "dismiss", &q]);
    assert_eq!(c, 2);
    assert!(keys(&owner_inbox(&t.p)).contains(&q));
}

#[test]
fn inbox_as_pm_keeps_the_readers_inbox_until_the_pm_acts_alone_then_adds_its_questions() {
    let t = planned();
    let task = pm_id(&t.p, &["task", "add", "Charge", "--feature", &t.card]);
    let q = t.p.id_of(
        IMPL,
        &["ask", "Need sandbox keys", "--to", "pm", "--task", &task],
    );
    // No signed plan: the reader pm's inbox, as it always was.
    let pm = pm_inbox(&t.p);
    assert_eq!(keys(&pm), vec![q.clone()], "{pm}");
    // Signed: the pm's inbox keeps its questions, with commands on its inbox.
    sign(&t.p);
    let pm = pm_inbox(&t.p);
    assert!(keys(&pm).contains(&q), "{pm}");
    let text = pm_ok(&t.p, &["inbox", "--as", "pm"]);
    assert!(
        text.contains(&format!("kontext inbox --as pm dismiss {q}")),
        "{text}"
    );
    assert!(!keys(&owner_inbox(&t.p)).contains(&q));
    pm_ok(
        &t.p,
        &["inbox", "--as", "pm", "answer", &q, "use the test keys"],
    );
    assert!(!keys(&pm_inbox(&t.p)).contains(&q));
}

/// A project that requires SSH signatures, with omar's key.
fn require_signatures(p: &Project) {
    let keys = p.dir.path().join("keys");
    fs::create_dir_all(&keys).unwrap();
    let key = keys.join("omar");
    let out = std::process::Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-C", "omar", "-f"])
        .arg(&key)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let public = fs::read_to_string(key.with_extension("pub")).unwrap();
    let toml = p.home.join("project.toml");
    let text = fs::read_to_string(&toml)
        .unwrap()
        .replace("require_signatures = false", "require_signatures = true");
    fs::write(
        &toml,
        format!(
            "{text}\n[people]\nomar = {{ emails = [\"omar@example.com\"], keys = [\"{}\"] }}\n",
            public.trim()
        ),
    )
    .unwrap();
    let cfg = p.config.join("projects.toml");
    let text = fs::read_to_string(&cfg).unwrap();
    fs::write(
        &cfg,
        format!(
            "{text}\n[signing]\nkey = \"{}\"\n",
            key.with_extension("pub").display()
        ),
    )
    .unwrap();
}

#[test]
fn with_require_signatures_the_inbox_does_not_move_to_the_pm() {
    let t = planned();
    require_signatures(&t.p);
    let prop =
        t.p.id_of(IMPL, &["propose", "Cache the price list", "--why", "slow"]);
    let q = t.p.id_of(IMPL, &["ask", "Need sandbox keys", "--to", "pm"]);
    sign(&t.p);
    let inv = pm_id(&t.p, &["feature", "add", "Invoices", "--epic", "pay"]);
    // The owner keeps what was theirs, plus the scope signature.
    let owner = owner_inbox(&t.p);
    let k = keys(&owner);
    assert!(k.contains(&prop), "{k:?}");
    assert!(k.contains(&"scope".to_string()), "{k:?}");
    assert!(owner.get("pm_items").is_none(), "{owner}");
    assert!(
        !t.p.kontext_ok(&["inbox"]).contains("The pm handles"),
        "no pm footer"
    );
    // `--as pm` is the reader pm's inbox, which the pm does not close.
    assert_eq!(keys(&pm_inbox(&t.p)), vec![q.clone()]);
    let (c, _, _) = code(
        &t.p,
        PM,
        &["inbox", "--as", "pm", "dismiss", &q, "--why", "x"],
    );
    assert_eq!(c, 2);
    assert!(!pm_ok(&t.p, &["resume", "--as", "pm"]).contains("The scope is signed"));
    let _ = inv;
}

#[test]
fn the_pm_dismisses_the_owners_words_only_with_their_yes() {
    let t = planned();
    sign(&t.p);
    let said = distill_fact(&t.p, "No refunds after 30 days");
    let (c, _, err) = code(
        &t.p,
        PM,
        &["inbox", "--as", "pm", "dismiss", "review", "--why", "noise"],
    );
    assert_eq!(c, 2, "{err}");
    assert!(
        err.contains(&format!("{said} carries the owner's own words")),
        "{err}"
    );
    assert!(keys(&pm_inbox(&t.p)).contains(&"review".to_string()));
    // With their yes it counts; confirming needs no yes.
    pm_ok(
        &t.p,
        &[
            "inbox",
            "--as",
            "pm",
            "dismiss",
            "review",
            "--why",
            "they said drop it",
            "--approved-by",
            "omar",
        ],
    );
    assert!(!keys(&pm_inbox(&t.p)).contains(&"review".to_string()));
    distill_fact(&t.p, "Prices include tax");
    pm_ok(&t.p, &["inbox", "--as", "pm", "confirm", "review"]);
    assert_eq!(pm_inbox(&t.p)["count"], 0);
}

#[test]
fn a_pm_dismissal_of_what_is_not_its_own_does_not_count() {
    let t = planned();
    sign(&t.p);
    let q = pm_id(&t.p, &["ask", "Which currency?", "--to", "owner"]);
    let imp = t
        .p
        .append(&format!(
            "{{\"kind\":\"scope\",\"title\":\"Plan signed\",\"actor\":{{\"type\":\"agent\",\"name\":\"impl1\",\"role\":\"implementer\"}},\"fields\":{{\"type\":\"change\",\"covers\":[],\"adds\":[],\"removes\":[],\"summary\":\"x\",\"summary_hash\":\"{}\"}}}}\n",
            sha256("x")
        ))
        .remove(0);
    let said = distill_fact(&t.p, "No refunds after 30 days");
    // Written straight to the ledger (another machine): a pm dismissal of an
    // owner-only problem, the owner's question and the owner's words.
    let dismiss = |items: &str| {
        t.p.append(&format!(
            "{{\"kind\":\"dismiss\",\"title\":\"x\",\"why\":\"x\",\"actor\":{{\"type\":\"agent\",\"name\":\"pm\",\"role\":\"pm\"}},\"fields\":{{\"items\":[{items}]}}}}\n"
        ))
        .remove(0)
    };
    let d1 = dismiss(&format!("\"not-counted:{imp}\""));
    let d2 = dismiss(&format!("\"{q}\""));
    let d3 = dismiss(&format!("\"{said}\""));
    let owner = keys(&owner_inbox(&t.p));
    for k in [format!("not-counted:{imp}"), q.clone()] {
        assert!(owner.contains(&k), "{k} closed by the pm: {owner:?}");
    }
    assert!(keys(&pm_inbox(&t.p)).contains(&"review".to_string()));
    // Each one is stored, does not count, and is the owner's to see.
    for d in [&d1, &d2, &d3] {
        assert!(
            owner.contains(&format!("not-counted:{d}")),
            "{d}: {owner:?}"
        );
    }
    let text = t.p.kontext_ok(&["inbox", "--all"]);
    assert!(
        text.contains("does not count: the pm may not close it"),
        "{text}"
    );
    // And the pm can't clear them.
    let (c, _, _) = code(
        &t.p,
        PM,
        &[
            "inbox",
            "--as",
            "pm",
            "dismiss",
            &format!("not-counted:{d1}"),
            "--why",
            "x",
        ],
    );
    assert_eq!(c, 2);
    // A problem that is the pm's own does close.
    let exp = dismiss("\"drift:X-000000\"");
    assert!(!keys(&owner_inbox(&t.p)).contains(&format!("not-counted:{exp}")));
}

#[test]
fn the_scope_signature_is_not_dismissed_from_the_inbox() {
    let t = planned();
    sign(&t.p);
    pm_id(&t.p, &["feature", "add", "Invoices", "--epic", "pay"]);
    let before = t.p.entries("omar@test").len();
    let (c, _, err) = code(&t.p, &[], &["inbox", "dismiss", "scope", "--why", "x"]);
    assert_eq!(c, 2, "{err}");
    assert!(err.contains("is not dismissed"), "{err}");
    let (c, _, _) = code(&t.p, &[], &["inbox", "confirm", "scope"]);
    assert_eq!(c, 2);
    assert_eq!(t.p.entries("omar@test").len(), before, "nothing written");
}

// ── The tree with a signed plan (phase 2, PR 4) ─────────────────────────

#[test]
fn the_tree_says_whether_the_plan_is_signed_and_marks_what_is_not() {
    let t = planned();
    // Unsigned: no scope in the header or the JSON.
    let text = pm_ok(&t.p, &["tree"]);
    assert!(!text.contains("signed"), "{text}");
    let v: Value = serde_json::from_str(&pm_ok(&t.p, &["--json", "tree"])).unwrap();
    assert!(v.get("scope").is_none());
    let s = sign(&t.p);
    let head = pm_ok(&t.p, &["tree"]).lines().next().unwrap().to_string();
    let v: Value = serde_json::from_str(&pm_ok(&t.p, &["--json", "tree"])).unwrap();
    assert_eq!(v["scope"]["signed"], s.as_str());
    let on = v["scope"]["signed_on"].as_str().unwrap();
    assert!(head.ends_with(&format!("· plan signed {on}")), "{head}");
    assert!(v["scope"].get("pending").is_none());

    // A feature enters and another leaves.
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
    pm_ok(
        &t.p,
        &["feature", "drop", &t.refunds, "--why", "not for v1"],
    );
    let text = pm_ok(&t.p, &["tree"]);
    let mut lines = text.lines();
    assert!(
        lines
            .next()
            .unwrap()
            .ends_with("· 1 scope change waiting for you"),
        "{text}"
    );
    assert_eq!(
        lines.next().unwrap().trim(),
        "enters Invoices · leaves Refunds"
    );
    let line = |title: &str| {
        // The tree's lines, after the header and the scope line.
        text.lines()
            .skip(2)
            .find(|l| l.contains(&format!(" {title} ")) || l.ends_with(&format!(" {title}")))
            .unwrap_or_else(|| panic!("no {title} in:\n{text}"))
            .to_string()
    };
    assert!(line("Invoices").ends_with("not signed"), "{text}");
    assert!(
        line("Refunds").ends_with("dropping, waiting for you"),
        "{text}"
    );
    assert!(!line("Card charge").contains("signed"), "{text}");
    let v: Value = serde_json::from_str(&pm_ok(&t.p, &["--json", "tree"])).unwrap();
    assert_eq!(
        v["scope"]["pending"]["enters"],
        serde_json::json!(["Invoices"])
    );
    let pay = &v["waves"][1]["epics"][0];
    let marks: Vec<(String, Value)> = pay["feature_list"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| (f["id"].as_str().unwrap().to_string(), f["scope"].clone()))
        .collect();
    assert!(marks.contains(&(inv.clone(), serde_json::json!("not_signed"))));
    assert!(marks.contains(&(t.refunds.clone(), serde_json::json!("dropping"))));
    // The dropping feature is listed but does not count.
    assert_eq!(pay["features"]["total"], 2);

    // Signed: the marks go, the dropped feature leaves the tree.
    sign(&t.p);
    let text = pm_ok(&t.p, &["tree"]);
    assert!(!text.contains("Refunds"), "{text}");
    assert!(!text.contains("not signed"), "{text}");
}
