//! What can be added to an epic after it was written: `kontext epic after`
//! and `kontext epic touches`; a feature's why; and what the user said about
//! a feature in `show`, `brief` and `next`.

mod common;

use std::fs;

use common::{Project, git};
use serde_json::Value;

const PM: &[(&str, &str)] = &[("KONTEXT_ACTOR", "agent:pm"), ("KONTEXT_ROLE", "pm")];
const COORD: &[(&str, &str)] = &[
    ("KONTEXT_ACTOR", "agent:epic:pay"),
    ("KONTEXT_ROLE", "coordinator"),
];

fn pm_ok(p: &Project, args: &[&str]) -> String {
    p.kontext_env_ok(PM, args)
}

fn pm_id(p: &Project, args: &[&str]) -> String {
    p.id_of(PM, args)
}

fn run(p: &Project, envs: &[(&str, &str)], args: &[&str]) -> (i32, String, String) {
    let out = p.kontext_env(envs, args);
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

fn json(p: &Project, args: &[&str]) -> Value {
    let mut a = vec!["--json"];
    a.extend_from_slice(args);
    serde_json::from_str(&pm_ok(p, &a)).unwrap()
}

/// The names of each wave of `kontext plan --json`.
fn waves(p: &Project) -> Vec<Vec<String>> {
    json(p, &["plan"])["waves"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| {
            w["epics"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| e["name"].as_str().unwrap().to_string())
                .collect()
        })
        .collect()
}

struct Planned {
    p: Project,
    found: String,
    pay: String,
    cat: String,
    refunds: String,
}

/// A charter, a goal, foundations, then payments and catalog in parallel,
/// a feature with its why.
fn planned() -> Planned {
    let p = Project::new();
    pm_ok(&p, &["charter", "--objective", "Sell online"]);
    let goal = pm_id(&p, &["goal", "add", "Take payments"]);
    let add = |title: &str, name: &str, after: Option<&str>| {
        let mut a = vec!["epic", "add", title, "--goal", &goal, "--name", name];
        if let Some(x) = after {
            a.extend(["--after", x]);
        }
        pm_id(&p, &a)
    };
    let found = add("Foundations", "found", None);
    let pay = add("Payments", "pay", Some("found"));
    let cat = add("Catalog", "cat", Some("found"));
    let refunds = pm_id(
        &p,
        &[
            "feature",
            "add",
            "Refunds",
            "--epic",
            "pay",
            "--acceptance",
            "the owner refunds an order",
            "--why",
            "customers ask for refunds by email today, and it takes Omar an hour a day",
        ],
    );
    Planned {
        p,
        found,
        pay,
        cat,
        refunds,
    }
}

#[test]
fn an_after_record_moves_an_existing_epic_to_a_later_wave_everywhere() {
    let t = planned();
    let p = &t.p;
    assert_eq!(waves(p), vec![vec!["found"], vec!["pay", "cat"]]);
    let out = pm_ok(
        p,
        &[
            "epic",
            "after",
            "cat",
            "pay",
            "--why",
            "catalog prices come from the billing tables",
        ],
    );
    assert!(out.contains("saved the dependency"), "{out}");
    assert_eq!(waves(p), vec![vec!["found"], vec!["pay"], vec!["cat"]]);
    // The tree draws the plan's waves.
    let tree = json(p, &["tree"]);
    let tw: Vec<usize> = tree["waves"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["epics"].as_array().unwrap().len())
        .collect();
    assert_eq!(tw, vec![1, 1, 1]);
    // The summary says it, with its why, whole.
    let s = pm_ok(p, &["scope", "summary"]);
    assert!(
        s.contains("Comes after Payments (R-")
            && s.contains("): catalog prices come from the billing tables"),
        "{s}"
    );
    assert!(s.contains("## Wave 3 (in series)"), "{s}");
    // `show` of the epic says what it waits for now.
    let show = pm_ok(p, &["show", &t.cat]);
    assert!(show.contains("**Also comes after** (R-"), "{show}");
    assert!(show.contains(&t.pay), "{show}");
}

#[test]
fn after_refuses_itself_repeats_cycles_unknown_epics_and_a_missing_why() {
    let t = planned();
    let p = &t.p;
    let refused = |args: &[&str], want: &str| {
        let (c, _, err) = run(p, PM, args);
        assert_eq!(c, 2, "{args:?}: {err}");
        assert!(err.contains(want), "{args:?}: {err}");
    };
    refused(
        &["epic", "after", "pay", "pay", "--why", "x"],
        "cannot come after itself",
    );
    refused(
        &["epic", "after", "pay", "found", "--why", "x"],
        "already comes after found",
    );
    refused(
        &["epic", "after", "found", "pay", "--why", "x"],
        "would close a cycle",
    );
    refused(
        &["epic", "after", "pay", "ghost", "--why", "x"],
        "there is no epic",
    );
    // A coordinator does not add dependencies: it asks the pm.
    let (c, _, err) = run(p, COORD, &["epic", "after", "cat", "pay", "--why", "x"]);
    assert_eq!(c, 2, "{err}");
    assert!(err.contains("only the pm or an owner"), "{err}");
    // --why is required.
    let (c, _, _) = run(p, PM, &["epic", "after", "cat", "pay"]);
    assert_ne!(c, 0);
    // Nothing of that was written.
    assert_eq!(waves(p), vec![vec!["found"], vec!["pay", "cat"]]);
    let _ = (&t.found, &t.refunds);
}

#[test]
fn touches_normalizes_paths_skips_repeats_and_refuses_globs() {
    let t = planned();
    let p = &t.p;
    pm_ok(
        p,
        &[
            "epic",
            "touches",
            "pay",
            "./src/billing/",
            "src/api/orders.rs",
        ],
    );
    let (c, _, err) = run(p, PM, &["epic", "touches", "pay", "src/billing"]);
    assert_eq!(c, 2, "{err}");
    assert!(err.contains("already touches src/billing"), "{err}");
    let (c, _, err) = run(p, PM, &["epic", "touches", "pay", "src/**"]);
    assert_ne!(c, 0);
    assert!(err.contains("is a glob"), "{err}");
    pm_ok(p, &["epic", "touches", "pay", "src/billing", "docs"]);
    let show = pm_ok(p, &["show", &t.pay]);
    assert!(
        show.contains("- **Touches:** src/billing, src/api/orders.rs, docs"),
        "{show}"
    );
    let s = pm_ok(p, &["scope", "summary"]);
    assert!(
        s.contains("  Touches: src/billing, src/api/orders.rs, docs"),
        "{s}"
    );
    let (c, _, err) = run(p, COORD, &["epic", "touches", "pay", "lib"]);
    assert_eq!(c, 2, "{err}");
}

/// A line that arrives written by someone else than the pm or an owner (an
/// old binary, another machine) is stored, does not count, and raises an
/// alert: the plan ignores it.
#[test]
fn an_after_record_from_a_coordinator_does_not_count() {
    let t = planned();
    let p = &t.p;
    let line = format!(
        "{{\"kind\":\"after\",\"title\":\"cat after pay\",\"why\":\"x\",\
         \"actor\":{{\"type\":\"agent\",\"name\":\"epic:cat\",\"role\":\"coordinator\"}},\
         \"refs\":[\"{}\",\"{}\"],\"fields\":{{\"depends_on\":[\"{}\"]}}}}\n",
        t.cat, t.pay, t.pay
    );
    p.append(&line);
    assert_eq!(waves(p), vec![vec!["found"], vec!["pay", "cat"]]);
    let st = pm_ok(p, &["standup", "--peek"]);
    assert!(st.contains("only the pm or an owner"), "{st}");
}

/// After the signature, a dependency or paths are not a scope change: no
/// summary to sign, no pending line, tasks still handed out, the inbox the
/// same.
#[test]
fn records_after_the_signature_hold_nothing_and_ask_for_no_signature() {
    let t = planned();
    let p = &t.p;
    pm_ok(
        p,
        &[
            "task",
            "add",
            "Refund endpoint",
            "--feature",
            &t.refunds,
            "--acceptance",
            "POST /refunds",
        ],
    );
    let h = json(p, &["scope", "summary"])["hash"]
        .as_str()
        .unwrap()
        .to_string();
    pm_ok(
        p,
        &[
            "scope",
            "sign",
            "--hash",
            &h,
            "--approved-by",
            "omar",
            "--said",
            "sí",
        ],
    );
    let inbox_before = p.kontext_ok(&["inbox", "count"]);
    pm_ok(
        p,
        &[
            "epic",
            "after",
            "cat",
            "pay",
            "--why",
            "shared billing tables",
        ],
    );
    pm_ok(p, &["epic", "touches", "pay", "src/billing"]);
    let s = pm_ok(p, &["scope", "summary"]);
    assert!(s.starts_with("Nothing to sign"), "{s}");
    let plan = pm_ok(p, &["plan"]);
    assert!(!plan.contains("Scope change"), "{plan}");
    assert_eq!(p.kontext_ok(&["inbox", "count"]), inbox_before);
    let out = p.kontext_env_ok(
        &[
            ("KONTEXT_ACTOR", "agent:i1"),
            ("KONTEXT_ROLE", "implementer"),
        ],
        &["next", "--epic", "pay", "--claim"],
    );
    assert!(out.contains("Refund endpoint"), "{out}");
}

/// The feature's why and the user's own sentences that point to it reach the
/// workers: `next`, `brief` and `show`, from the repo that has `.pm/`.
#[test]
fn workers_see_the_why_and_what_the_user_said() {
    let t = planned();
    let p = &t.p;
    pm_ok(
        p,
        &[
            "task",
            "add",
            "Refund endpoint",
            "--feature",
            &t.refunds,
            "--acceptance",
            "POST /refunds",
        ],
    );
    let repo = p.dir.path().join("repo");
    fs::create_dir_all(repo.join(".pm/dumps")).unwrap();
    git(&repo, &["init", "-q"]);
    fs::write(
        repo.join(".pm/dumps/2026-11-12.md"),
        "## 2026-11-12 10:00\n\n**Coordinator asked:**\n\n> Do refunds matter?\n\n**User:**\n\nque el cliente pueda pedir reembolso sin escribirnos\n",
    )
    .unwrap();
    fs::write(
        repo.join(".pm/items.md"),
        format!(
            "| id | idea | source | state | where | evidence |\n|---|---|---|---|---|---|\n\
             | P1 | refunds without email | dumps/2026-11-12.md:9 | pending | {} | |\n\
             | P2 | the question | dumps/2026-11-12.md:5 | pending | {} | |\n",
            t.refunds, t.refunds
        ),
    )
    .unwrap();
    let quote = "\"que el cliente pueda pedir reembolso sin escribirnos\" (P1, Nov 12)";
    let next = p.kontext_in_ok(
        &repo,
        &[
            ("KONTEXT_ACTOR", "agent:i1"),
            ("KONTEXT_ROLE", "implementer"),
        ],
        &["next", "--epic", "pay"],
    );
    assert!(
        next.contains("- Why the feature exists: customers ask for refunds by email today"),
        "{next}"
    );
    assert!(next.contains(quote), "{next}");
    // A line under **Coordinator asked:** is never quoted as the user's.
    assert!(!next.contains("Do refunds matter"), "{next}");
    let brief = p.kontext_in_ok(&repo, PM, &["brief", "pay", "--peek"]);
    assert!(brief.contains("Why it exists: customers ask"), "{brief}");
    assert!(brief.contains(quote), "{brief}");
    let show = p.kontext_in_ok(&repo, PM, &["show", &t.refunds]);
    assert!(show.contains("**What the user said about it**"), "{show}");
    assert!(show.contains(quote), "{show}");
    let v: Value =
        serde_json::from_str(&p.kontext_in_ok(&repo, PM, &["--json", "show", &t.refunds])).unwrap();
    assert_eq!(v["user_said"][0]["idea"], "P1");
    // Without `.pm/` (a sandbox, another checkout) the views still work.
    let next = p.kontext_env_ok(
        &[
            ("KONTEXT_ACTOR", "agent:i1"),
            ("KONTEXT_ROLE", "implementer"),
        ],
        &["next", "--epic", "pay"],
    );
    assert!(!next.contains("What the user said"), "{next}");
    assert!(next.contains("Why the feature exists"), "{next}");
}

/// A feature without a why and no `.pm/`: the views read as before.
#[test]
fn nothing_new_without_a_why_or_quotes() {
    let p = Project::new();
    pm_ok(&p, &["charter", "--objective", "Sell online"]);
    let goal = pm_id(&p, &["goal", "add", "Take payments"]);
    pm_ok(
        &p,
        &["epic", "add", "Payments", "--goal", &goal, "--name", "pay"],
    );
    let f = pm_id(
        &p,
        &[
            "feature",
            "add",
            "Refunds",
            "--epic",
            "pay",
            "--acceptance",
            "x",
        ],
    );
    pm_ok(
        &p,
        &[
            "task",
            "add",
            "Endpoint",
            "--feature",
            &f,
            "--acceptance",
            "y",
        ],
    );
    let brief = pm_ok(&p, &["brief", "pay", "--peek"]);
    assert!(!brief.contains("Why it exists"), "{brief}");
    let next = p.kontext_env_ok(
        &[
            ("KONTEXT_ACTOR", "agent:i1"),
            ("KONTEXT_ROLE", "implementer"),
        ],
        &["--json", "next", "--epic", "pay"],
    );
    let v: Value = serde_json::from_str(&next).unwrap();
    assert!(v.get("user_said").is_none(), "{v}");
    let s = pm_ok(&p, &["scope", "summary"]);
    assert!(!s.contains("Why:") && !s.contains("Touches"), "{s}");
}
