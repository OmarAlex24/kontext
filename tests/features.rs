//! Features (DESIGN §4.3, §5, §8 and §12.1): who adds and validates them, their
//! derived state, the epic's closure in `kontext plan`, and the views. An epic
//! without features must read exactly as before.

mod common;

use common::{Project, implementer, with_epic};
use serde_json::Value;

fn feature(p: &Project, title: &str, extra: &[&str]) -> String {
    let mut args = vec!["feature", "add", title, "--epic", "payments"];
    args.extend_from_slice(extra);
    p.id_of(&[], &args)
}

fn task_in(p: &Project, title: &str, u: &str) -> String {
    p.id_of(&[], &["task", "add", title, "--feature", u])
}

fn close(p: &Project, t: &str) {
    p.kontext_ok(&["done", t, "--evidence", "cmd:\"cargo test\"=0"]);
}

fn show_json(p: &Project, id: &str) -> Value {
    serde_json::from_str(&p.kontext_ok(&["--json", "show", id])).unwrap()
}

fn fstatus(p: &Project, u: &str) -> String {
    show_json(p, u)["feature"]["status"]
        .as_str()
        .unwrap()
        .to_string()
}

fn stderr(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

fn coordinator() -> Vec<(&'static str, String)> {
    vec![
        ("KONTEXT_ACTOR", "agent:epic:payments".to_string()),
        ("KONTEXT_ROLE", "coordinator".to_string()),
    ]
}

fn pm() -> Vec<(&'static str, String)> {
    vec![
        ("KONTEXT_ACTOR", "agent:pm".to_string()),
        ("KONTEXT_ROLE", "pm".to_string()),
    ]
}

#[test]
fn who_adds_features_and_where_tasks_hang() {
    let (p, g, e) = with_epic();
    // An owner and the pm add features; their goals default to the epic's.
    let u = feature(
        &p,
        "Refunds",
        &["--acceptance", "the owner refunds an order"],
    );
    let out = p.run_as_ok(&pm(), &["feature", "add", "Invoices", "--epic", "payments"]);
    assert!(out.contains(": saved the feature"), "{out}");
    let f = show_json(&p, &u)["entry"].clone();
    assert!(u.starts_with("U-"), "{u}");
    assert_eq!(f["kind"], "feature");
    assert_eq!(f["fields"]["epic"], e.as_str());
    assert_eq!(f["fields"]["goals"][0], g.as_str());
    assert_eq!(f["fields"]["acceptance"], "the owner refunds an order");

    // Neither a coordinator nor an implementer: refused, nothing written.
    let before = p.entries("omar@test").len();
    for who in [coordinator(), implementer("impl-1")] {
        let out = p.run_as(&who, &["feature", "add", "X", "--epic", "payments"]);
        assert_eq!(out.status.code(), Some(2), "{out:?}");
        assert!(stderr(&out).contains("only the pm or an owner"), "{out:?}");
    }
    assert_eq!(p.entries("omar@test").len(), before);

    // A task under a feature: its epic comes from the feature.
    let t = task_in(&p, "Refund endpoint", &u);
    let te = show_json(&p, &t)["entry"].clone();
    assert_eq!(te["fields"]["feature"], u.as_str());
    assert_eq!(te["fields"]["epic"], e.as_str());
    // With the same epic it is fine; with another one, refused.
    p.kontext_ok(&["task", "add", "Two", "--feature", &u, "--epic", "payments"]);
    p.id_of(
        &[],
        &["epic", "add", "Catalog", "--goal", &g, "--name", "catalog"],
    );
    let out = p.kontext(&["task", "add", "Bad", "--feature", &u, "--epic", "catalog"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("is not the epic of"), "{out:?}");
    // --feature has to be a feature.
    let out = p.kontext(&["task", "add", "Bad", "--feature", &t]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("not a feature"), "{out:?}");
    // Without --epic or --feature, clap refuses.
    assert_eq!(p.kontext(&["task", "add", "Bad"]).status.code(), Some(2));
    // A task without a feature has no `feature` field.
    let plain = p.id_of(&[], &["task", "add", "Plain", "--epic", "payments"]);
    assert!(
        show_json(&p, &plain)["entry"]["fields"]
            .get("feature")
            .is_none()
    );
}

#[test]
fn feature_state_follows_its_tasks_and_its_validation() {
    let (p, _, _) = with_epic();
    let u = feature(&p, "Refunds", &[]);
    assert_eq!(fstatus(&p, &u), "pending");
    let t1 = task_in(&p, "Endpoint", &u);
    let t2 = task_in(&p, "Admin button", &u);
    assert_eq!(fstatus(&p, &u), "pending");
    p.kontext_ok(&["assign", &t1, "--to", "agent:impl-1"]);
    assert_eq!(fstatus(&p, &u), "in progress");
    close(&p, &t1);
    assert_eq!(fstatus(&p, &u), "in progress");
    close(&p, &t2);
    assert_eq!(fstatus(&p, &u), "to validate");

    // Validated by the epic's coordinator, without --since.
    let out = p.run_as_ok(
        &coordinator(),
        &[
            "done",
            &u,
            "--evidence",
            "url:https://example.com/refund.mp4",
        ],
    );
    assert!(out.contains(&format!("{u} validated.")), "{out}");
    assert_eq!(fstatus(&p, &u), "done");
    let text = p.kontext_ok(&["show", &u]);
    assert!(
        text.contains("- **Status:** done (validated in R-"),
        "{text}"
    );
    assert!(text.contains("validation by agent:epic:payments"), "{text}");

    // A task reopened after the validation: in progress, with a note; no record needed.
    p.kontext_ok(&["reopen", &t2, "--why", "the button is missing"]);
    let v = show_json(&p, &u);
    assert_eq!(v["feature"]["status"], "in progress");
    let note = v["feature"]["note"].as_str().unwrap().to_string();
    assert!(
        note.contains(&format!("{t2} was reopened after the validation")),
        "{note}"
    );
    // Closed again: to validate (the old validation does not cover it).
    close(&p, &t2);
    assert_eq!(fstatus(&p, &u), "to validate");
    p.kontext_ok(&["done", &u, "--evidence", "url:https://example.com/2"]);
    assert_eq!(fstatus(&p, &u), "done");

    // A new task after the validation: in progress.
    let t3 = task_in(&p, "Email the receipt", &u);
    let v = show_json(&p, &u);
    assert_eq!(v["feature"]["status"], "in progress");
    let note = v["feature"]["note"].as_str().unwrap().to_string();
    assert!(
        note.contains(&format!("{t3} was added after the validation")),
        "{note}"
    );
    close(&p, &t3);
    p.kontext_ok(&["done", &u, "--evidence", "url:https://example.com/3"]);
    assert_eq!(fstatus(&p, &u), "done");

    // Reopening the feature itself.
    p.kontext_ok(&["reopen", &u, "--why", "the owner can't find it"]);
    assert_eq!(fstatus(&p, &u), "to validate");
    let out = p.kontext(&["reopen", &u, "--why", "again"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("is not validated"), "{out:?}");
}

#[test]
fn validating_has_rules_and_refusals() {
    let (p, _, _) = with_epic();
    let u = feature(&p, "Refunds", &[]);
    // No tasks: it can't be validated.
    let out = p.kontext(&["done", &u, "--evidence", "url:https://x"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("it has no tasks yet"), "{out:?}");
    let t = task_in(&p, "Endpoint", &u);
    let out = p.kontext(&["done", &u, "--evidence", "url:https://x"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        stderr(&out).contains(&format!("1 still open: {t}")),
        "{out:?}"
    );
    close(&p, &t);
    // An implementer can't validate; without evidence it is refused.
    let out = p.run_as(
        &implementer("impl-1"),
        &["done", &u, "--evidence", "url:https://x"],
    );
    assert_eq!(out.status.code(), Some(2));
    assert!(
        stderr(&out).contains("only the coordinator of its epic"),
        "{out:?}"
    );
    assert_eq!(p.kontext(&["done", &u]).status.code(), Some(2));
    // The pm validates.
    p.run_as_ok(&pm(), &["done", &u, "--evidence", "url:https://x"]);
    let out = p.kontext(&["done", &u, "--evidence", "url:https://x"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("already validated"), "{out:?}");
}

fn raw(kind: &str, actor: &str, refs: &str, fields: &str) -> String {
    let (ty, name) = actor.split_once(':').unwrap();
    let role = if ty == "agent" {
        ",\"role\":\"implementer\""
    } else {
        ""
    };
    format!(
        "{{\"kind\":\"{kind}\",\"title\":\"x\",\"actor\":{{\"type\":\"{ty}\",\"name\":\"{name}\"{role}}},\"refs\":[{refs}],\"fields\":{fields}}}\n"
    )
}

#[test]
fn entries_from_another_machine_that_do_not_count() {
    let (p, _, e) = with_epic();
    // A feature written by an implementer (only another machine or version
    // does that): stored, not counted, with an alert.
    let fields = format!("{{\"epic\":\"{e}\",\"goals\":[]}}");
    let bad = p.append(&raw(
        "feature",
        "agent:impl-1",
        &format!("\"{e}\""),
        &fields,
    ))[0]
        .clone();
    assert_eq!(fstatus(&p, &bad), "not counted");
    let st = p.state();
    assert!(
        st["not_counting"][&bad]
            .as_str()
            .unwrap()
            .contains("only the pm or an owner")
    );
    // Tasks can't hang from it.
    let out = p.kontext(&["task", "add", "X", "--feature", &bad]);
    assert_eq!(out.status.code(), Some(2));

    // A validation while a task is still open, and one by an implementer:
    // stored, not counted, an alert in standup and a problem in the inbox.
    let u = feature(&p, "Refunds", &[]);
    let t = task_in(&p, "Endpoint", &u);
    let ev = r#"{"evidence":[{"type":"url","value":"https://x"}]}"#;
    let early = p.append(&raw("done", "human:omar", &format!("\"{u}\""), ev))[0].clone();
    close(&p, &t);
    assert_eq!(fstatus(&p, &u), "to validate");
    let by_impl = p.append(&raw("done", "agent:impl-1", &format!("\"{u}\""), ev))[0].clone();
    assert_eq!(fstatus(&p, &u), "to validate");
    let st = p.state();
    assert!(
        st["not_counting"][&early]
            .as_str()
            .unwrap()
            .contains("not all its tasks are closed")
    );
    assert!(
        st["not_counting"][&by_impl]
            .as_str()
            .unwrap()
            .contains("only the epic's coordinator")
    );
    let standup = p.kontext_ok(&["standup", "--peek"]);
    assert!(
        standup.contains(&format!("{early} (done for {u}")),
        "{standup}"
    );
    let inbox = p.kontext_ok(&["inbox", "--all"]);
    assert!(
        inbox.contains("Validation that didn't count: Refunds"),
        "{inbox}"
    );
}

#[test]
fn supersede_a_feature_with_another() {
    let (p, _, _) = with_epic();
    let old = feature(&p, "Refunds", &[]);
    let new = feature(&p, "Refunds and credits", &[]);
    let out = p.run_as(&coordinator(), &["supersede", &old, &new]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("only the pm or an owner"), "{out:?}");
    p.run_as_ok(&pm(), &["supersede", &old, &new, "--why", "merged"]);
    assert_eq!(fstatus(&p, &old), "superseded");
    let out = p.kontext(&["task", "add", "X", "--feature", &old]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("was superseded by"), "{out:?}");
    // Only by another feature.
    let t = task_in(&p, "Endpoint", &new);
    assert_eq!(p.kontext(&["supersede", &new, &t]).status.code(), Some(2));
}

#[test]
fn ask_about_a_feature() {
    let (p, _, _) = with_epic();
    let u = feature(&p, "Refunds", &[]);
    let q = p.id_of(
        &[],
        &[
            "ask",
            "Partial refunds too?",
            "--to",
            "owner",
            "--feature",
            &u,
        ],
    );
    let qe = show_json(&p, &q)["entry"].clone();
    assert_eq!(qe["fields"]["feature"], u.as_str());
    assert_eq!(qe["fields"]["epic"], "payments");
    assert_eq!(qe["refs"][0], u.as_str());
    // Without --to it goes to the feature's epic, like a task's.
    let q2 = p.id_of(
        &[],
        &["ask", "Which provider for refunds?", "--feature", &u],
    );
    assert_eq!(show_json(&p, &q2)["entry"]["fields"]["to"], "epic:payments");
    // The same question again joins it, and the join keeps the feature.
    let out = p.kontext_ok(&[
        "--json",
        "ask",
        "Partial refunds too?",
        "--to",
        "owner",
        "--feature",
        &u,
    ]);
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["kind"], "confirm");
    assert_eq!(v["fields"]["feature"], u.as_str());
    // A task of another feature with --feature: refused.
    let other = feature(&p, "Invoices", &[]);
    let t = task_in(&p, "Endpoint", &other);
    let out = p.kontext(&["ask", "?", "--task", &t, "--feature", &u]);
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn plan_closes_an_epic_when_its_features_are_done() {
    let (p, g, _) = with_epic();
    p.id_of(
        &[],
        &[
            "epic", "add", "After", "--goal", &g, "--name", "after", "--after", "payments",
        ],
    );
    let u1 = feature(&p, "Card charge", &[]);
    let u2 = feature(&p, "Refunds", &[]);
    let t1 = task_in(&p, "Stripe", &u1);
    let t2 = task_in(&p, "Refund", &u2);
    close(&p, &t1);
    close(&p, &t2);
    // Every task closed, no feature validated: still open.
    let text = p.kontext_ok(&["plan"]);
    assert!(
        text.contains("payments: Card payments (0/2 features done; 2/2 tasks closed)"),
        "{text}"
    );
    assert!(text.contains("## Wave 2"), "{text}");
    p.kontext_ok(&["done", &u1, "--evidence", "url:https://x"]);
    p.kontext_ok(&["done", &u2, "--evidence", "url:https://x"]);
    let text = p.kontext_ok(&["plan"]);
    assert!(text.contains("## Closed (1)"), "{text}");
    assert!(
        text.contains("(2/2 features done; 2/2 tasks closed)"),
        "{text}"
    );
    let v: Value = serde_json::from_str(&p.kontext_ok(&["--json", "plan"])).unwrap();
    assert_eq!(v["closed"][0]["features"]["total"], 2);
    // The epic without features has no `features` key.
    assert!(v["waves"][0]["epics"][0].get("features").is_none());
    // A feature added later reopens the epic; superseded, it no longer counts.
    let u3 = feature(&p, "Old idea", &[]);
    assert!(!p.kontext_ok(&["plan"]).contains("## Closed (1)"));
    p.kontext_ok(&["supersede", &u3, &u2]);
    assert!(p.kontext_ok(&["plan"]).contains("## Closed (1)"));
}

#[test]
fn views_with_and_without_features() {
    let (p, _, e) = with_epic();
    // Without features, the JSON views have no feature keys.
    let t0 = p.id_of(&[], &["task", "add", "Loose", "--epic", "payments"]);
    let brief: Value =
        serde_json::from_str(&p.kontext_ok(&["--json", "brief", "payments", "--peek"])).unwrap();
    assert!(brief.get("features").is_none());
    let next: Value = serde_json::from_str(
        &p.run_as_ok(&implementer("i"), &["--json", "next", "--epic", "payments"]),
    )
    .unwrap();
    assert!(next.get("feature").is_none());
    assert!(show_json(&p, &t0).get("feature").is_none());
    let text = p.kontext_ok(&["brief", "payments", "--peek"]);
    assert!(text.contains("### Ready (1)"), "{text}");

    let u = feature(
        &p,
        "Refunds",
        &["--acceptance", "the owner refunds an order"],
    );
    let t = task_in(&p, "Endpoint", &u);
    // brief: grouped by feature, loose tasks last.
    let text = p.kontext_ok(&["brief", "payments", "--peek"]);
    assert!(text.contains("0/1 features done."), "{text}");
    assert!(
        text.contains(&format!(
            "### Feature {u} Refunds [pending (0/1 tasks closed)]"
        )),
        "{text}"
    );
    assert!(
        text.contains("When it works, the human sees: the owner refunds an order"),
        "{text}"
    );
    assert!(text.contains(&format!("- [ready] {t} Endpoint")), "{text}");
    assert!(
        text.contains(&format!("### No feature (1)\n- [ready] {t0} Loose")),
        "{text}"
    );
    assert!(!text.contains("### Ready ("), "{text}");
    let brief: Value =
        serde_json::from_str(&p.kontext_ok(&["--json", "brief", "payments", "--peek"])).unwrap();
    assert_eq!(brief["features"][0]["id"], u.as_str());
    assert_eq!(brief["features"][0]["feature_status"], "pending");
    // next: the feature's title and acceptance.
    let (claimed, _) = p.claim("impl-1", "payments");
    assert_eq!(claimed, t0);
    p.run_as_ok(
        &implementer("impl-1"),
        &["done", &t0, "--evidence", "url:https://x", "--since", "-"],
    );
    let (claimed, _) = p.claim("impl-1", "payments");
    assert_eq!(claimed, t);
    let text = p.run_as_ok(&implementer("impl-1"), &["next", "--epic", "payments"]);
    assert!(
        text.contains(&format!(
            "- Feature: {u} Refunds (in progress). When it works, the human sees: the owner refunds an order"
        )),
        "{text}"
    );
    let next: Value = serde_json::from_str(&p.run_as_ok(
        &implementer("impl-1"),
        &["--json", "next", "--epic", "payments"],
    ))
    .unwrap();
    assert_eq!(next["feature"]["id"], u.as_str());
    // The pm's line per epic, and show of the epic.
    close(&p, &t);
    let resume = p.kontext_ok(&["resume", "--as", "pm", "--peek"]);
    assert!(
        resume.contains(&format!(
            "- {e} Card payments: 0/1 features done, 1 to validate; 2/2 closed;"
        )),
        "{resume}"
    );
    let show = p.kontext_ok(&["show", &e]);
    assert!(
        show.contains(&format!(
            "**Features** (0/1 features done, 1 to validate)\n\n- {u} [to validate] Refunds"
        )),
        "{show}"
    );
    // show of the feature: its tasks and its (empty) validation history.
    let show = p.kontext_ok(&["show", &u]);
    assert!(show.contains("**Tasks** (1/1 closed)"), "{show}");
    assert!(show.contains(&format!("- {t} [closed] Endpoint")), "{show}");
    assert!(show.contains("(not validated yet)"), "{show}");
}

fn plan_json(p: &Project) -> Value {
    serde_json::from_str(&p.kontext_ok(&["--json", "plan"])).unwrap()
}

fn closed_epics(p: &Project) -> Vec<String> {
    plan_json(p)["closed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn an_epic_with_features_also_waits_for_its_tasks_without_a_feature() {
    let (p, _, _) = with_epic();
    // A task without a feature keeps its meaning: open, it keeps the epic open.
    let loose = p.id_of(&[], &["task", "add", "Loose", "--epic", "payments"]);
    let u = feature(&p, "Refunds", &[]);
    let t = task_in(&p, "Endpoint", &u);
    close(&p, &t);
    p.kontext_ok(&["done", &u, "--evidence", "url:https://x"]);
    assert_eq!(fstatus(&p, &u), "done");
    assert!(closed_epics(&p).is_empty(), "{}", plan_json(&p));
    close(&p, &loose);
    assert_eq!(closed_epics(&p), ["payments"]);

    // Open tasks of a superseded feature don't block it, but each one raises
    // an alert until it is closed or moved.
    let old = feature(&p, "Old refunds", &[]);
    let stuck = task_in(&p, "Old endpoint", &old);
    assert!(closed_epics(&p).is_empty());
    p.run_as_ok(&pm(), &["supersede", &old, &u, "--why", "merged"]);
    assert_eq!(closed_epics(&p), ["payments"]);
    let alert = format!(
        "open task {stuck} of superseded feature {old}: close it with evidence or move the work to a feature in force"
    );
    let standup = p.kontext_ok(&["standup", "--peek"]);
    assert!(standup.contains(&alert), "{standup}");
    close(&p, &stuck);
    let standup = p.kontext_ok(&["standup", "--peek"]);
    assert!(!standup.contains("of superseded feature"), "{standup}");
}

#[test]
fn a_task_of_another_epic_does_not_count_toward_a_feature() {
    let (p, g, _) = with_epic();
    let u = feature(&p, "Refunds", &[]);
    let t = task_in(&p, "Endpoint", &u);
    close(&p, &t);
    p.kontext_ok(&["done", &u, "--evidence", "url:https://x"]);
    let other = p.id_of(
        &[],
        &["epic", "add", "Other", "--goal", &g, "--name", "other"],
    );
    // Only another machine writes this: the CLI refuses it.
    let fields = format!("{{\"epic\":\"{other}\",\"feature\":\"{u}\",\"goals\":[\"{g}\"]}}");
    let stray = p.append(&raw("task", "human:omar", &format!("\"{other}\""), &fields))[0].clone();
    assert_eq!(fstatus(&p, &u), "done");
    let info = show_json(&p, &u)["feature"].clone();
    assert_eq!(info["tasks"], serde_json::json!([t]));
    assert!(closed_epics(&p).contains(&"payments".to_string()));
    let standup = p.kontext_ok(&["standup", "--peek"]);
    assert!(
        standup.contains(&format!("{stray} (task for {u}")),
        "{standup}"
    );
    // In its own epic it is a task without a feature, as before.
    assert!(!closed_epics(&p).contains(&"other".to_string()));
    let next = p.run_as_ok(&implementer("impl-1"), &["next", "--epic", "other"]);
    assert!(!next.contains("- Feature:"), "{next}");
}

#[test]
fn a_feature_that_lost_its_validation_is_in_progress() {
    let (p, _, _) = with_epic();
    let u = feature(&p, "Refunds", &[]);
    let t = task_in(&p, "Endpoint", &u);
    close(&p, &t);
    p.kontext_ok(&["done", &u, "--evidence", "url:https://x"]);
    p.kontext_ok(&["reopen", &t, "--why", "bug"]);
    assert_eq!(fstatus(&p, &u), "in progress");
    let show = p.kontext_ok(&["show", &u]);
    assert!(show.contains("- **Status:** in progress"), "{show}");
    let brief = p.kontext_ok(&["brief", "payments", "--peek"]);
    assert!(
        brief.contains(&format!("### Feature {u} Refunds [in progress")),
        "{brief}"
    );
    // A task added after the validation: in progress too.
    close(&p, &t);
    p.kontext_ok(&["done", &u, "--evidence", "url:https://x"]);
    task_in(&p, "Receipt", &u);
    assert_eq!(fstatus(&p, &u), "in progress");
}

/// A line of kind `scope` (a kind of a newer version) for `writer`, after
/// `prev` (`None`: the first line of the file), with its chain and hash right.
fn scope_line(writer: &str, prev: Option<&Value>) -> (String, String) {
    use kontext::model::{GENESIS, canonical, compute_hash, content_digest};
    let (n, prev) = match prev {
        Some(l) => (l["n"].as_u64().unwrap() + 1, l["hash"].clone()),
        None => (1, GENESIS.into()),
    };
    let mut v = serde_json::json!({
        "v": 1, "writer": writer, "n": n, "prev": prev,
        "hlc": "2026-11-20T10:00:00.000Z.0000",
        "actor": {"type": "human", "name": "omar"}, "kind": "scope",
        "title": "Checkout scope", "body": "", "why": "", "tags": [], "refs": [],
        "fields": {}, "sig": null,
    });
    let id = format!("S-{}", &content_digest(&v)[..6]);
    v["id"] = id.clone().into();
    v["hash"] = compute_hash(&v).into();
    (id, canonical(&v) + "\n")
}

#[test]
fn an_entry_kind_from_a_newer_version_is_listed_and_left_out() {
    use std::io::Write;
    let (p, _, _) = with_epic();
    let before = p.state()["entries"].as_u64().unwrap();
    // One in this writer's file, one in another machine's.
    let last = p.entries("omar@test").last().cloned().unwrap();
    let (mine, line) = scope_line("omar@test", Some(&last));
    std::fs::OpenOptions::new()
        .append(true)
        .open(p.ledger("omar@test"))
        .unwrap()
        .write_all(line.as_bytes())
        .unwrap();
    let (theirs, line) = scope_line("zoe@mac", None);
    std::fs::write(p.ledger("zoe@mac"), line).unwrap();
    // Writing on top of it works, and the state leaves both out.
    p.kontext_ok(&["task", "add", "After", "--epic", "payments"]);
    assert_eq!(p.state()["entries"].as_u64().unwrap(), before + 1);

    let out = p.kontext(&["verify"]);
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success(), "{text}");
    for id in [&mine, &theirs] {
        let line = format!("{id} is from a newer kontext version (kind scope): upgrade to read it");
        assert_eq!(text.matches(&line).count(), 1, "{text}");
    }
    let doctor = String::from_utf8_lossy(&p.kontext(&["doctor"]).stdout).to_string();
    for id in [&mine, &theirs] {
        assert_eq!(
            doctor
                .matches(&format!(
                    "{id} is from a newer kontext version (kind scope)"
                ))
                .count(),
            1,
            "{doctor}"
        );
    }
    assert!(doctor.contains("chains: OK"), "{doctor}");
    let resume = p.kontext_ok(&["resume", "--as", "pm", "--peek"]);
    assert!(!resume.contains("CHAIN BROKEN"), "{resume}");
    let standup = p.kontext_ok(&["standup", "--peek"]);
    assert!(!standup.contains(&mine), "{standup}");
    // Writing an unknown kind is still refused.
    let out = p
        .spawn_append(&p.config, "{\"kind\":\"scope\",\"title\":\"x\"}\n", 0)
        .wait_with_output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("unknown entry kind"));
}
