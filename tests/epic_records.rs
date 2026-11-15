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
    goal: String,
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
        goal,
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
    // The same folder written another way is the same path.
    for same in [
        "/src/billing",
        "src/./billing",
        "src//billing/",
        "src/Billing",
    ] {
        let (c, _, err) = run(p, PM, &["epic", "touches", "pay", same]);
        assert_eq!(c, 2, "{same}: {err}");
        assert!(err.contains("already touches"), "{same}: {err}");
    }
    // Refused, each saying what to type instead.
    for (bad, says) in [
        ("\"src/billing\"", "without quotes"),
        ("'src/billing'", "without quotes"),
        ("src/billing{a,b}", "give each path separately"),
        ("src/../src/billing", "from the repo's root"),
    ] {
        let (c, _, err) = run(p, PM, &["epic", "touches", "pay", bad]);
        assert_ne!(c, 0, "{bad}");
        assert!(err.contains(says), "{bad}: {err}");
    }
    let why = pm_id(
        p,
        &[
            "epic",
            "touches",
            "pay",
            "src/billing",
            "docs",
            "--why",
            "the refund   rules\nare documented",
        ],
    );
    let show = pm_ok(p, &["show", &t.pay]);
    assert!(
        show.contains("- **Touches:** src/billing, src/api/orders.rs, docs"),
        "{show}"
    );
    assert!(
        show.contains(&format!(
            "  - Why docs ({why}): the refund rules are documented\n"
        )),
        "{show}"
    );
    let s = pm_ok(p, &["scope", "summary"]);
    // The paths, then each record's why, whole and on one line (the first
    // record has none: no line for it).
    assert!(
        s.contains(&format!(
            "  Touches: src/billing, src/api/orders.rs, docs\n    Why docs ({why}): the refund rules are documented\n"
        )),
        "{s}"
    );
    assert_eq!(s.matches("    Why ").count(), 1, "{s}");
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
    // Its reason names what it adds, not features.
    let touches = format!(
        "{{\"kind\":\"touches\",\"title\":\"pay touches src\",\"why\":\"x\",\
         \"actor\":{{\"type\":\"agent\",\"name\":\"epic:pay\",\"role\":\"coordinator\"}},\
         \"refs\":[\"{}\"],\"fields\":{{\"paths\":[\"src\"]}}}}\n",
        t.pay
    );
    let ids = p.append(&touches);
    let show = pm_ok(p, &["show", &ids[0]]);
    assert!(
        show.contains("only the pm or an owner adds paths to an epic"),
        "{show}"
    );
    assert!(!show.contains("features"), "{show}");
    let st = pm_ok(p, &["standup", "--peek"]);
    assert!(st.contains("adds a dependency to an epic"), "{st}");
    assert!(!st.contains("add or supersede features"), "{st}");
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

const IMPL: &[(&str, &str)] = &[
    ("KONTEXT_ACTOR", "agent:i1"),
    ("KONTEXT_ROLE", "implementer"),
];

/// A repo with `.pm/`: one user message per quote, each a row citing `cites`.
fn pm_with_quotes(p: &Project, quotes: &[&str], cites: &str) -> std::path::PathBuf {
    let repo = p.dir.path().join("repo");
    fs::create_dir_all(repo.join(".pm/dumps")).unwrap();
    git(&repo, &["init", "-q"]);
    let mut dump = String::from("## 2026-11-12 10:00\n\n**User:**\n");
    let mut items = String::from(
        "| id | idea | source | state | where | evidence |\n|---|---|---|---|---|---|\n",
    );
    for (i, q) in quotes.iter().enumerate() {
        dump.push_str(&format!("\n{q}\n"));
        let line = 5 + 2 * i;
        items.push_str(&format!(
            "| P{} | idea {} | dumps/2026-11-12.md:{line} | pending | {cites} | |\n",
            i + 1,
            i + 1
        ));
    }
    fs::write(repo.join(".pm/dumps/2026-11-12.md"), dump).unwrap();
    fs::write(repo.join(".pm/items.md"), items).unwrap();
    repo
}

/// Superseding a feature to give it its why keeps what the user said about
/// it: `show` and `next` follow the replacement, as `kontext ideas` does.
#[test]
fn the_quotes_follow_a_superseded_feature_to_its_replacement() {
    let p = Project::new();
    pm_ok(&p, &["charter", "--objective", "Sell online"]);
    let goal = pm_id(&p, &["goal", "add", "Take payments"]);
    pm_ok(
        &p,
        &["epic", "add", "Payments", "--goal", &goal, "--name", "pay"],
    );
    let feature = |why: Option<&str>| {
        let mut a = vec![
            "feature",
            "add",
            "Refunds",
            "--epic",
            "pay",
            "--acceptance",
            "the owner refunds",
        ];
        if let Some(w) = why {
            a.extend(["--why", w]);
        }
        pm_id(&p, &a)
    };
    let old = feature(None);
    let repo = pm_with_quotes(&p, &["refunds without email"], &old);
    let new = feature(Some("refunds by hand take a day"));
    pm_ok(&p, &["supersede", &old, &new, "--why", "give it its why"]);
    // Twice: a chain is followed to its end.
    let newer = feature(Some("refunds by hand take a whole day"));
    pm_ok(&p, &["supersede", &new, &newer, "--why", "a better why"]);
    pm_ok(
        &p,
        &[
            "task",
            "add",
            "Refund endpoint",
            "--feature",
            &newer,
            "--acceptance",
            "POST /refunds",
        ],
    );
    let quote = "\"refunds without email\" (P1, Nov 12)";
    let show = p.kontext_in_ok(&repo, PM, &["show", &newer]);
    assert!(show.contains(quote), "{show}");
    let next = p.kontext_in_ok(&repo, IMPL, &["next", "--epic", "pay"]);
    assert!(next.contains(quote), "{next}");
}

/// `next` cuts a feature's why as `brief` does (400 characters), and less at
/// its tightest level, so a long why does not blow `--max-chars`. The plan's
/// summary still prints it whole.
#[test]
fn next_cuts_a_long_why() {
    let p = Project::new();
    pm_ok(&p, &["charter", "--objective", "Sell online"]);
    let goal = pm_id(&p, &["goal", "add", "Take payments"]);
    let long = "refunds by hand take a day. ".repeat(300);
    let short = "refunds by hand take a day.";
    let mut sizes = Vec::new();
    for (name, why) in [("long", long.as_str()), ("short", short)] {
        pm_ok(&p, &["epic", "add", name, "--goal", &goal, "--name", name]);
        let f = pm_id(
            &p,
            &[
                "feature",
                "add",
                name,
                "--epic",
                name,
                "--acceptance",
                "x",
                "--why",
                why,
            ],
        );
        pm_ok(
            &p,
            &["task", "add", name, "--feature", &f, "--acceptance", "y"],
        );
        let next =
            |max: &str| p.kontext_env_ok(IMPL, &["next", "--epic", name, "--max-chars", max]);
        sizes.push((next("1200").chars().count(), next("4000")));
    }
    let (long_tight, long_roomy) = sizes[0].clone();
    let short_tight = sizes[1].0;
    // Under a budget nothing fits in, the why adds little over a short one.
    assert!(
        long_tight <= short_tight + 200,
        "{long_tight} vs {short_tight}"
    );
    // With room, it is cut at 400 characters and the whole fits the budget.
    assert!(long_roomy.chars().count() <= 4000, "{long_roomy}");
    let line = long_roomy
        .lines()
        .find(|l| l.starts_with("- Why the feature exists:"))
        .unwrap();
    assert!(line.chars().count() < 460, "{line}");
    let summary = pm_ok(&p, &["scope", "summary"]);
    assert!(summary.contains(long.trim()), "the summary prints it whole");
}

/// Past three quotes, `next` points at the feature, whose `show` lists them all.
#[test]
fn next_points_at_the_feature_for_the_other_quotes() {
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
    let quotes = ["first quote", "second quote", "third quote", "fourth quote"];
    let repo = pm_with_quotes(p, &quotes, &t.refunds);
    let next = p.kontext_in_ok(
        &repo,
        IMPL,
        &["next", "--epic", "pay", "--max-chars", "1200"],
    );
    let hint = format!("… 1 more: `kontext show {}`", t.refunds);
    assert!(next.contains(&hint), "{next}");
    assert!(!next.contains("fourth quote"), "{next}");
    let show = p.kontext_in_ok(&repo, PM, &["show", &t.refunds]);
    assert!(show.contains("\"fourth quote\" (P4, Nov 12)"), "{show}");
}

fn sign(p: &Project) -> String {
    let h = json(p, &["scope", "summary"])["hash"]
        .as_str()
        .unwrap()
        .to_string();
    let out = pm_ok(
        p,
        &[
            "--json",
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
    let v: Value = serde_json::from_str(&out).unwrap();
    v["id"].as_str().unwrap_or_default().to_string()
}

/// A change summary's "Before" is the order the signature saw: an `after`
/// added since does not rewrite it. Each one added since is listed, and one
/// that makes a signed epic wait on scope that enters says so.
#[test]
fn a_change_summary_compares_against_the_dependencies_the_signature_saw() {
    // A: an `after` between two signed epics, then a feature (a change).
    let t = planned();
    let p = &t.p;
    sign(p);
    let r = pm_id(
        p,
        &[
            "epic",
            "after",
            "cat",
            "pay",
            "--why",
            "prices feed invoices",
        ],
    );
    pm_ok(
        p,
        &[
            "feature",
            "add",
            "Exports",
            "--epic",
            "cat",
            "--acceptance",
            "csv",
        ],
    );
    let s = pm_ok(p, &["scope", "summary"]);
    assert!(
        s.contains("Now: 1. Foundations; 2. Payments; 3. Catalog."),
        "{s}"
    );
    assert!(
        s.contains("Before: 1. Foundations; 2. Payments, Catalog."),
        "{s}"
    );
    assert!(!s.contains("Unchanged"), "{s}");
    assert!(
        s.contains(&format!(
            "## Dependencies added since the signature (1)\n- Catalog comes after Payments ({r}): prices feed invoices\n"
        )),
        "{s}"
    );
    assert!(!s.contains("⚠"), "Payments is signed: {s}");

    // B: a signed epic made to wait on an epic that is not signed yet.
    let t = planned();
    let p = &t.p;
    sign(p);
    pm_ok(
        p,
        &[
            "epic", "add", "Shipping", "--goal", &t.goal, "--name", "ship", "--after", "found",
        ],
    );
    pm_ok(p, &["epic", "after", "cat", "ship", "--why", "rates first"]);
    let s = pm_ok(p, &["scope", "summary"]);
    assert!(
        s.contains("Before: 1. Foundations; 2. Payments, Catalog."),
        "{s}"
    );
    assert!(
        s.contains(
            "  ⚠ Catalog is signed and now waits on Shipping, which enters with this change and is not signed yet"
        ),
        "{s}"
    );
    // Once signed, the next signature saw it: nothing is "added since".
    sign(p);
    pm_ok(
        p,
        &[
            "feature",
            "add",
            "Exports",
            "--epic",
            "cat",
            "--acceptance",
            "csv",
        ],
    );
    let s = pm_ok(p, &["scope", "summary"]);
    assert!(!s.contains("Dependencies added since"), "{s}");
    assert!(s.contains("Unchanged: "), "{s}");
}

/// Two epics of one wave that touch the same paths: a warning in `plan`,
/// `tree` and the summary (never a reorder). One pair is resolved with a
/// dependency, another with a parallel record, which shows with its why.
#[test]
fn overlapping_epics_warn_until_a_dependency_or_a_parallel_record() {
    let t = planned();
    let p = &t.p;
    let g = json(p, &["resume", "--as", "pm"])["goals"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    pm_ok(
        p,
        &[
            "epic", "add", "Search", "--goal", &g, "--name", "search", "--after", "found",
        ],
    );
    pm_ok(p, &["epic", "touches", "pay", "src/billing"]);
    pm_ok(
        p,
        &[
            "epic",
            "touches",
            "cat",
            "src/billing/prices.rs",
            "src/catalog",
        ],
    );
    pm_ok(p, &["epic", "touches", "search", "src/catalog/index.rs"]);
    let plan = pm_ok(p, &["plan"]);
    assert!(
        plan.contains("  ⚠ same paths as Catalog (src/billing/prices.rs)"),
        "{plan}"
    );
    assert!(
        plan.contains("  ⚠ same paths as Search (src/catalog/index.rs)"),
        "{plan}"
    );
    // Only a warning: same waves, exit 0.
    assert_eq!(waves(p), vec![vec!["found"], vec!["pay", "cat", "search"]]);
    let tree = pm_ok(p, &["tree"]);
    assert!(
        tree.contains("⚠ same paths as Payments (src/billing/prices.rs)"),
        "{tree}"
    );
    let tj = json(p, &["tree"]);
    assert_eq!(
        tj["waves"][1]["epics"][0]["overlaps"][0]["paths"][0],
        "src/billing/prices.rs"
    );
    let s = pm_ok(p, &["scope", "summary"]);
    assert!(
        s.contains("  ⚠ Same paths as Catalog in this wave: src/billing/prices.rs"),
        "{s}"
    );
    // Pair 1: search waits for catalog. Pair 2: payments and catalog are fine.
    pm_ok(
        p,
        &[
            "epic",
            "after",
            "search",
            "cat",
            "--why",
            "the index reads the catalog tables",
        ],
    );
    let why = "catalog only reads prices.rs; payments owns it and the catalog PR goes second";
    pm_ok(p, &["epic", "parallel", "pay", "cat", "--why", why]);
    let plan = pm_ok(p, &["plan"]);
    assert!(!plan.contains("⚠"), "{plan}");
    assert_eq!(
        waves(p),
        vec![vec!["found"], vec!["pay", "cat"], vec!["search"]]
    );
    assert!(!pm_ok(p, &["tree"]).contains("⚠"));
    let s = pm_ok(p, &["scope", "summary"]);
    assert!(!s.contains("⚠"), "{s}");
    assert!(
        s.contains("  May run in parallel with Catalog on src/billing/prices.rs (R-")
            && s.contains(why),
        "{s}"
    );
    assert!(s.contains("Comes after Catalog (R-"), "{s}");
    // Refusals: the same pair again, one epic twice, no why, a coordinator.
    let (c, _, err) = run(p, PM, &["epic", "parallel", "cat", "pay", "--why", "x"]);
    assert_eq!(c, 2, "{err}");
    assert!(err.contains("already have a parallel record"), "{err}");
    let (c, _, _) = run(p, PM, &["epic", "parallel", "pay", "pay", "--why", "x"]);
    assert_eq!(c, 2);
    let (c, _, _) = run(
        p,
        COORD,
        &["epic", "parallel", "pay", "found", "--why", "x"],
    );
    assert_eq!(c, 2);
}

/// A parallel record from someone who is not the pm or an owner does not
/// silence anything.
#[test]
fn a_parallel_record_from_a_coordinator_does_not_count() {
    let t = planned();
    let p = &t.p;
    pm_ok(p, &["epic", "touches", "pay", "src/billing"]);
    pm_ok(p, &["epic", "touches", "cat", "src/billing"]);
    let line = format!(
        "{{\"kind\":\"parallel\",\"title\":\"pay and cat\",\"why\":\"x\",\
         \"actor\":{{\"type\":\"agent\",\"name\":\"epic:pay\",\"role\":\"coordinator\"}},\
         \"refs\":[\"{}\",\"{}\"],\"fields\":{{\"paths\":[\"src/billing\"]}}}}\n",
        t.pay, t.cat
    );
    p.append(&line);
    assert!(pm_ok(p, &["plan"]).contains("⚠ same paths as Catalog (src/billing)"));
}

/// A `parallel` record covers only the shared paths it stored. Written when
/// the pair shares nothing, it is refused; one already in a ledger with no
/// paths covers nothing. A path shared later warns again, and a new record
/// for the pair accepts it.
#[test]
fn a_parallel_record_covers_only_the_paths_it_stored() {
    let t = planned();
    let p = &t.p;
    let (c, _, err) = run(p, PM, &["epic", "parallel", "pay", "cat", "--why", "x"]);
    assert_eq!(c, 2, "{err}");
    assert!(err.contains("share no recorded path"), "{err}");
    assert!(err.contains("kontext epic touches"), "{err}");
    // An old binary stored one with no paths: it covers nothing.
    let legacy = p.append(&format!(
        "{{\"kind\":\"parallel\",\"title\":\"pay and cat\",\"why\":\"they never share files\",\
         \"actor\":{{\"type\":\"agent\",\"name\":\"pm\",\"role\":\"pm\"}},\
         \"refs\":[\"{}\",\"{}\"],\"fields\":{{\"paths\":[]}}}}\n",
        t.pay, t.cat
    ))[0]
        .clone();
    pm_ok(p, &["epic", "touches", "pay", "src/billing"]);
    pm_ok(p, &["epic", "touches", "cat", "src/billing/prices.rs"]);
    let plan = pm_ok(p, &["plan"]);
    assert!(
        plan.contains(&format!(
            "⚠ same paths as Catalog (src/billing/prices.rs; not covered by {legacy})"
        )),
        "{plan}"
    );
    assert!(pm_ok(p, &["tree"]).contains("⚠ same paths as Catalog"));
    let s = pm_ok(p, &["scope", "summary"]);
    assert!(!s.contains("May run in parallel"), "{s}");
    assert!(
        s.contains(&format!(
            "  Parallel record with Catalog ({legacy}) covers no path (they shared none when it was written): they never share files"
        )),
        "{s}"
    );
    assert!(
        s.contains(&format!(
            "  ⚠ Same paths as Catalog in this wave: src/billing/prices.rs (not covered by {legacy}: it covers no path)"
        )),
        "{s}"
    );
    // The owner accepts the overlap with a new record: it covers prices.rs.
    let ok = pm_id(
        p,
        &["epic", "parallel", "pay", "cat", "--why", "a new fn only"],
    );
    assert!(!pm_ok(p, &["plan"]).contains("⚠"));
    let (c, _, err) = run(p, PM, &["epic", "parallel", "cat", "pay", "--why", "x"]);
    assert_eq!(c, 2, "{err}");
    assert!(err.contains("already have a parallel record"), "{err}");
    // A path they share later is not covered: the ⚠ is back, for it only.
    pm_ok(p, &["epic", "touches", "cat", "src/billing/tax.rs"]);
    let plan = pm_ok(p, &["plan"]);
    assert!(
        plan.contains(&format!(
            "⚠ same paths as Catalog (src/billing/tax.rs; not covered by {legacy}, {ok})"
        )),
        "{plan}"
    );
    let s = pm_ok(p, &["scope", "summary"]);
    assert!(
        s.contains(&format!(
            "  May run in parallel with Catalog on src/billing/prices.rs ({ok}): a new fn only"
        )),
        "{s}"
    );
    assert!(
        s.contains(&format!(
            "  ⚠ Same paths as Catalog in this wave: src/billing/tax.rs (not covered by {legacy}, {ok}: they cover only src/billing/prices.rs)"
        )),
        "{s}"
    );
    pm_ok(
        p,
        &["epic", "parallel", "pay", "cat", "--why", "tax.rs too"],
    );
    assert!(!pm_ok(p, &["plan"]).contains("⚠"));
}

/// Paths another writer stored raw still overlap: they are compared by
/// their key, case ignored, and printed as stored.
#[test]
fn raw_paths_from_another_writer_still_warn() {
    let t = planned();
    let p = &t.p;
    pm_ok(p, &["epic", "touches", "pay", "src/billing"]);
    for raw in [
        "/src/billing",
        "src/./billing/x.rs",
        "src/Billing",
        "\\\"src/billing\\\"",
        "src/billing{a,b}",
    ] {
        let t2 = planned();
        let q = &t2.p;
        pm_ok(q, &["epic", "touches", "pay", "src/billing"]);
        q.append(&format!(
            "{{\"kind\":\"touches\",\"title\":\"cat touches\",\"why\":\"\",\
             \"actor\":{{\"type\":\"agent\",\"name\":\"pm\",\"role\":\"pm\"}},\
             \"refs\":[\"{}\"],\"fields\":{{\"paths\":[\"{raw}\"]}}}}\n",
            t2.cat
        ));
        let plan = pm_ok(q, &["plan"]);
        assert!(plan.contains("⚠ same paths as Catalog"), "{raw}: {plan}");
    }
    // The controls: a file under it warns, a sibling prefix does not.
    pm_ok(p, &["epic", "touches", "cat", "src/bill"]);
    assert!(!pm_ok(p, &["plan"]).contains("⚠"));
    pm_ok(p, &["epic", "touches", "cat", "src/billing/y.rs"]);
    assert!(pm_ok(p, &["plan"]).contains("⚠ same paths as Catalog (src/billing/y.rs)"));
}
