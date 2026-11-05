//! The human's inbox (`kontext inbox`): the classes, that seeing it doesn't
//! close it and acting does, the order by impact, the groups and the
//! duplicates of `ask`, the brake, and "new" since the human's last action.

mod common;

use std::fs;
use std::io::Write;
use std::process::Stdio;

use common::{Project, with_epic};
use serde_json::Value;

fn inbox(p: &Project) -> Value {
    serde_json::from_str(&p.ctx_ok(&["--json", "inbox", "--all"])).unwrap()
}

fn items(p: &Project) -> Vec<Value> {
    inbox(p)["items"].as_array().unwrap().clone()
}

fn classes(p: &Project) -> Vec<String> {
    items(p)
        .iter()
        .map(|i| i["class"].as_str().unwrap().to_string())
        .collect()
}

fn find<'a>(items: &'a [Value], key: &str) -> &'a Value {
    items
        .iter()
        .find(|i| i["key"] == key || i["ids"].as_array().unwrap().iter().any(|x| x == key))
        .unwrap_or_else(|| panic!("no pending item {key}: {items:#?}"))
}

const PM: [(&str, &str); 2] = [("KONTEXT_ACTOR", "agent:pm"), ("KONTEXT_ROLE", "pm")];

fn coord(epic: &str) -> Vec<(&'static str, String)> {
    vec![
        ("KONTEXT_ACTOR", format!("agent:epic:{epic}")),
        ("KONTEXT_ROLE", "coordinator".to_string()),
    ]
}

fn distill_fact(p: &Project, title: &str) -> String {
    p.append(&format!(
        "{{\"kind\":\"fact\",\"title\":\"{title}\",\"actor\":{{\"type\":\"agent\",\"name\":\"distill\"}},\
         \"fields\":{{\"source\":\"distill\",\"origin\":\"user\",\"fingerprints\":[]}}}}\n"
    ))
    .remove(0)
}

#[test]
fn each_class_is_derived_from_the_ledger() {
    let (p, g, e) = with_epic();
    // sign: an unsigned goal and an agent's proposal.
    let g2 = p.id_of(&PM, &["goal", "add", "Refunds"]);
    let pr = p.id_of(
        &[("KONTEXT_ACTOR", "agent:epic:payments")],
        &["propose", "Save cards in Stripe"],
    );
    // answer: a question for the owner.
    let q = p.id_of(&PM, &["ask", "Monthly price?", "--to", "owner"]);
    // review: what the distill noted.
    let f = distill_fact(&p, "The pilot customer is Lopez Hardware");
    // problem: a closed task that a tester reopens.
    let c = coord("payments");
    let t = p.run_as_ok(
        &c,
        &["--json", "task", "add", "Webhooks", "--epic", "payments"],
    );
    let t: Value = serde_json::from_str(&t).unwrap();
    let t = t["id"].as_str().unwrap().to_string();
    p.ctx_ok(&["done", &t, "--evidence", "cmd:\"cargo test\"=0"]);
    p.agent_ok("tester", &["reopen", &t, "--why", "charges twice"]);

    let it = items(&p);
    assert_eq!(find(&it, &g2)["class"], "sign");
    assert_eq!(find(&it, &pr)["class"], "sign");
    assert_eq!(find(&it, &q)["class"], "answer");
    let rev = find(&it, "review");
    assert_eq!(rev["class"], "review");
    assert!(rev["ids"].as_array().unwrap().iter().any(|x| x == &f));
    let prob = find(&it, &t);
    assert_eq!(prob["class"], "problem");
    assert!(
        prob["title"]
            .as_str()
            .unwrap()
            .contains("Reopened: Webhooks")
    );
    assert!(prob["why"].as_str().unwrap().contains("agent:tester"));
    assert_eq!(it.len(), 5, "{it:#?}");
    // The signed goal and the question for the pm are not the human's.
    assert!(!it.iter().any(|i| i["key"] == g.as_str()));
    p.agent_ok("coder", &["ask", "Which SDK?", "--to", "pm"]);
    assert_eq!(items(&p).len(), 5);
    // Drift is a problem too.
    p.append(&format!(
        "{{\"kind\":\"task\",\"title\":\"Redesign the logo\",\"actor\":{{\"type\":\"human\",\"name\":\"omar\"}},\"fields\":{{\"epic\":\"{}\",\"goals\":[]}}}}\n",
        e
    ));
    assert!(
        items(&p)
            .iter()
            .any(|i| i["key"].as_str().unwrap().starts_with("drift:")),
        "{:#?}",
        items(&p)
    );
}

#[test]
fn seeing_does_not_close_only_acting_does() {
    let (p, _, _) = with_epic();
    let g2 = p.id_of(&PM, &["goal", "add", "Refunds"]);
    let q = p.id_of(&PM, &["ask", "Monthly price?", "--to", "owner"]);
    let pr = p.id_of(
        &[("KONTEXT_ACTOR", "agent:coder")],
        &["propose", "Use Rust"],
    );
    let f = distill_fact(&p, "Notices go out at 9");
    assert_eq!(items(&p).len(), 4);

    // See it every way: the inbox, the count, the standup, the report and the hook.
    p.ctx_ok(&["inbox"]);
    p.ctx_ok(&["inbox", "count"]);
    p.ctx_ok(&["standup"]);
    p.ctx_ok(&["report", "--html"]);
    p.ctx_ok(&["report"]);
    let mut hook = p
        .cmd(&["hook", "session-start"])
        .env("KONTEXT_ROLE", "pm")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    hook.stdin.take().unwrap().write_all(b"{}").unwrap();
    hook.wait_with_output().unwrap();
    assert_eq!(items(&p).len(), 4, "seeing it closes nothing");

    // An agent can't close the human's pending items.
    let out = p.ctx_env(&[("KONTEXT_ACTOR", "agent:pm")], &["inbox", "dismiss", &q]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(items(&p).len(), 4);

    // Acting does: sign, answer, dismiss and confirm.
    let out = p.ctx_ok(&["inbox", "sign", &g2]);
    assert!(out.contains("saved the signature"), "{out}");
    assert!(out.contains("3 pending items"), "{out}");
    p.ctx_ok(&["inbox", "answer", &q, "199 dollars"]);
    p.ctx_ok(&["inbox", "dismiss", &pr, "--why", "does not apply"]);
    assert_eq!(classes(&p), ["review"]);
    p.ctx_ok(&["inbox", "confirm", "review"]);
    assert!(items(&p).is_empty());
    assert_eq!(p.ctx_ok(&["inbox", "count"]).trim(), "No pending items.");
    // The dismissal stays in the ledger, with its reason, and the proposal is still pending.
    let show = p.ctx_ok(&["show", &pr]);
    assert!(show.contains("pending"), "{show}");
    let st = p.state();
    assert_eq!(st["status"][&g2], "signed");
    assert_eq!(st["status"][&f], "active");
    // What was dismissed doesn't keep waiting in the standup either.
    let standup = p.ctx_ok(&["standup", "--peek"]);
    assert!(!standup.contains(&pr), "{standup}");
}

#[test]
fn sign_by_number_and_answer_inside_a_group() {
    let (p, _, _) = with_epic();
    let c = coord("payments");
    let q1 = p.run_as_ok(
        &c,
        &["--json", "ask", "Do we charge in MXN?", "--to", "owner"],
    );
    let q1: Value = serde_json::from_str(&q1).unwrap();
    let q1 = q1["id"].as_str().unwrap().to_string();
    let q2 = p.run_as_ok(&c, &["--json", "ask", "Who pays the fee?", "--to", "owner"]);
    let q2: Value = serde_json::from_str(&q2).unwrap();
    let q2 = q2["id"].as_str().unwrap().to_string();
    let it = items(&p);
    assert_eq!(
        it.len(),
        1,
        "the two questions of payments go together: {it:#?}"
    );
    assert_eq!(it[0]["title"], "2 questions in Card payments");
    assert_eq!(it[0]["subs"].as_array().unwrap().len(), 2);
    // Without saying which, it doesn't answer.
    let out = p.ctx(&["inbox", "answer", "1", "yes"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("1.1"));
    // With 1.2 it answers the second; the first stays.
    p.ctx_ok(&["inbox", "answer", "1.2", "The customer"]);
    let st = p.state();
    assert_eq!(st["status"][&q2], "answered");
    assert_eq!(st["status"][&q1], "open");
    assert_eq!(items(&p).len(), 1);
    // Signing something that is not to be signed is refused.
    let out = p.ctx(&["inbox", "sign", "1"]);
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn ordered_by_impact_then_age() {
    let (p, g, _) = with_epic();
    // Warnings goes after payments.
    p.ctx_ok(&[
        "epic", "add", "Notices", "--goal", &g, "--name", "notices", "--after", "payments",
    ]);
    let c = coord("payments");
    let t = p.run_as_ok(&c, &["--json", "task", "add", "SDK", "--epic", "payments"]);
    let t = serde_json::from_str::<Value>(&t).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    p.run_as_ok(
        &c,
        &[
            "task",
            "add",
            "Webhooks",
            "--epic",
            "payments",
            "--depends-on",
            &t,
        ],
    );
    // The oldest one blocks nothing.
    let old = p.id_of(&PM, &["ask", "Button color?", "--to", "owner"]);
    // The new one comes from a task: it stops that task, the one that depends
    // on it, and the epic that comes after.
    let new = p.id_of(
        &[
            ("KONTEXT_ACTOR", "agent:impl-1"),
            ("KONTEXT_ROLE", "implementer"),
        ],
        &["ask", "Which Stripe key?", "--task", &t, "--to", "owner"],
    );
    let it = items(&p);
    assert_eq!(it[0]["key"], new.as_str());
    assert_eq!(it[0]["blocks_tasks"], 2);
    assert_eq!(it[0]["blocks_epics"], serde_json::json!(["Notices"]));
    assert!(
        it[0]["line"]
            .as_str()
            .unwrap()
            .starts_with("blocks Notices (and 2 tasks) · waits agent:impl-1 · "),
        "{}",
        it[0]["line"]
    );
    assert_eq!(it[1]["key"], old.as_str());
    // With the same impact, the oldest first.
    let older = p.id_of(&PM, &["ask", "New logo?", "--to", "owner"]);
    let it = items(&p);
    assert_eq!(it[1]["key"], old.as_str());
    assert_eq!(it[2]["key"], older.as_str());
    // The text: the first 5 in full and "and N more".
    for i in 0..5 {
        p.id_of(
            &PM,
            &[
                "ask",
                &format!("Topic {i} different {i}{i}?"),
                "--to",
                "owner",
                "--no-merge",
            ],
        );
    }
    let text = p.ctx_ok(&["inbox"]);
    assert!(text.contains("\n5. "), "{text}");
    assert!(!text.contains("\n6. "), "{text}");
    assert!(
        text.contains("And 3 more: 3 to answer. `kontext inbox --all` shows them."),
        "{text}"
    );
    assert!(p.ctx_ok(&["inbox", "--all"]).contains("\n8. "));
}

#[test]
fn ask_joins_a_question_that_is_already_open() {
    let (p, _, _) = with_epic();
    let c = coord("payments");
    let q = p.run_as_ok(
        &c,
        &[
            "--json",
            "ask",
            "Do we charge in MXN or USD?",
            "--to",
            "owner",
        ],
    );
    let q = serde_json::from_str::<Value>(&q).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    // Another agent asks the same thing, in other words, from a payments task.
    let t = p.run_as_ok(&c, &["--json", "task", "add", "SDK", "--epic", "payments"]);
    let t = serde_json::from_str::<Value>(&t).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let out = p.ctx_env_ok(
        &[("KONTEXT_ACTOR", "agent:impl-1")],
        &["ask", "do we charge in usd?", "--task", &t, "--to", "owner"],
    );
    assert!(
        out.contains(&format!("{q} already asks the same thing")),
        "{out}"
    );
    let st = p.state();
    let questions = st["status"]
        .as_object()
        .unwrap()
        .keys()
        .filter(|k| k.starts_with("Q-"))
        .count();
    assert_eq!(questions, 1, "no other question was written");
    let it = items(&p);
    assert_eq!(it.len(), 1);
    assert_eq!(
        it[0]["waiting"],
        serde_json::json!(["agent:epic:payments", "agent:impl-1"])
    );
    // Now the question stops the task of whoever joined.
    assert_eq!(it[0]["blocks_tasks"], 1);
    // The answer counts as new context for that task.
    let (task, mark) = p.claim("impl-1", "payments");
    assert_eq!(task, t);
    p.ctx_ok(&["answer", &q, "MXN"]);
    let out = p.run_as(
        &common::implementer("impl-1"),
        &["done", &t, "--since", &mark, "--evidence", "cmd:\"x\"=0"],
    );
    assert_eq!(
        out.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Another epic, another recipient or --no-merge: a new question.
    let g = p.state()["status"]
        .as_object()
        .unwrap()
        .keys()
        .find(|k| k.starts_with("G-"))
        .unwrap()
        .clone();
    p.ctx_ok(&["epic", "add", "Notices", "--goal", &g, "--name", "notices"]);
    let out = p.run_as_ok(
        &coord("notices"),
        &["ask", "Do we charge in MXN or USD?", "--to", "owner"],
    );
    assert!(out.contains("saved the question"), "{out}");
    let out = p.run_as_ok(&c, &["ask", "Do we charge in MXN or USD?", "--to", "pm"]);
    assert!(out.contains("saved the question"), "{out}");
    let out = p.run_as_ok(&c, &["ask", "Do we charge in pesos?", "--to", "owner"]);
    assert!(out.contains("saved the question"), "{out}");
    let out = p.run_as_ok(
        &c,
        &[
            "ask",
            "Do we charge in pesos?",
            "--to",
            "owner",
            "--no-merge",
        ],
    );
    assert!(out.contains("saved the question"), "{out}");
}

#[test]
fn the_brake_marks_agent_questions_as_not_urgent() {
    let p = Project::new();
    let toml = p.home.join("project.toml");
    let mut text = fs::read_to_string(&toml).unwrap();
    text.push_str("\n[inbox]\nmax_pending = 2\n");
    fs::write(&toml, text).unwrap();
    p.id_of(&PM, &["ask", "One?", "--to", "owner"]);
    let out = p.ctx_env_ok(&PM, &["ask", "Two, different?", "--to", "owner"]);
    assert!(
        !out.contains("Warning"),
        "with 1 pending item it does not brake: {out}"
    );
    // With 2 pending items, the third is marked not urgent and warns.
    let out = p.ctx_env_ok(&PM, &["ask", "Three separate topics?", "--to", "owner"]);
    assert!(
        out.contains("the human has 2 unread pending items; does this block you?"),
        "{out}"
    );
    let it = items(&p);
    let third = it
        .iter()
        .find(|i| i["title"] == "Three separate topics?")
        .unwrap();
    assert_eq!(third["not_urgent"], true);
    assert!(third["line"].as_str().unwrap().starts_with("not urgent"));
    // With --blocking it doesn't brake.
    let out = p.ctx_env_ok(
        &PM,
        &["ask", "Four more things?", "--to", "owner", "--blocking"],
    );
    assert!(!out.contains("Warning"), "{out}");
    let it = items(&p);
    let fourth = it
        .iter()
        .find(|i| i["title"] == "Four more things?")
        .unwrap();
    assert_eq!(fourth["not_urgent"], false);
    // To another agent (not the human), it doesn't brake, nor to a human.
    let out = p.ctx_env_ok(&PM, &["ask", "Five?", "--to", "coder"]);
    assert!(!out.contains("Warning"), "{out}");
    let out = p.ctx_ok(&["ask", "Six new ones?", "--to", "owner"]);
    assert!(!out.contains("Warning"), "{out}");
}

#[test]
fn new_counts_from_the_last_human_action() {
    let p = Project::new();
    // omar writes something: that is their last action.
    p.ctx_ok(&["log", "Starting"]);
    p.id_of(&PM, &["ask", "One?", "--to", "owner"]);
    let ib = inbox(&p);
    assert_eq!(ib["new"], 1);
    // Seeing it (standup moves its cursor) doesn't make it old.
    p.ctx_ok(&["standup"]);
    p.ctx_ok(&["inbox"]);
    assert_eq!(inbox(&p)["new"], 1);
    assert!(p.ctx_ok(&["standup", "--peek"]).contains("(new)"));
    // Any action by omar in the ledger does: what was already there stops being new.
    p.ctx_ok(&["log", "Reviewed today's items"]);
    let ib = inbox(&p);
    assert_eq!(ib["count"], 1);
    assert_eq!(ib["new"], 0);
    assert!(!p.ctx_ok(&["standup", "--peek"]).contains("(new)"));
    // What arrives later is new.
    p.id_of(&PM, &["ask", "Another different thing?", "--to", "owner"]);
    assert_eq!(inbox(&p)["new"], 1);
    assert!(
        p.ctx_ok(&["inbox", "count"])
            .starts_with("2 pending items (1 new)")
    );
}

#[test]
fn hook_tells_the_pm_session_how_many() {
    let (p, _, _) = with_epic();
    let run = |p: &Project| -> String {
        let mut c = p
            .cmd(&["hook", "session-start"])
            .env("KONTEXT_ROLE", "pm")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        c.stdin.take().unwrap().write_all(b"{}").unwrap();
        let out = c.wait_with_output().unwrap();
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        v["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let text = run(&p);
    assert!(
        text.starts_with("# Context of demo for pm"),
        "nothing if there is none: {text}"
    );
    p.id_of(&PM, &["goal", "add", "Refunds"]);
    let text = run(&p);
    assert!(
        text.starts_with("omar has 1 pending item in kontext (oldest, from "),
        "{text}"
    );
    assert!(text.contains("# Context of demo for pm"), "{text}");
    assert!(text.chars().count() <= 9900);
    // Another role doesn't get it.
    let mut c = p
        .cmd(&["hook", "session-start"])
        .env("KONTEXT_ROLE", "coordinator")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    c.stdin.take().unwrap().write_all(b"{}").unwrap();
    let out = String::from_utf8(c.wait_with_output().unwrap().stdout).unwrap();
    assert!(!out.contains("pending item in kontext"), "{out}");
}

/// `owners = ["omar", "ana"]` in project.toml.
fn two_owners(p: &Project) {
    let toml = p.home.join("project.toml");
    let t = fs::read_to_string(&toml).unwrap();
    fs::write(
        &toml,
        t.replace("owners = [\"omar\"]", "owners = [\"omar\", \"ana\"]"),
    )
    .unwrap();
}

fn everyone(p: &Project, all: bool) -> Value {
    let mut args = vec!["--json", "inbox", "--everyone"];
    if all {
        args.push("--all");
    }
    serde_json::from_str(&p.ctx_ok(&args)).unwrap()
}

fn keys_of(section: &Value) -> Vec<String> {
    section["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["key"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn everyone_has_a_section_per_human_and_one_common() {
    let (p, _, _) = with_epic();
    two_owners(&p);
    // Each person's: questions by name (with or without `human:`).
    let qo = p.id_of(
        &PM,
        &[
            "ask",
            "How much does Stripe charge per card?",
            "--to",
            "omar",
        ],
    );
    let qo2 = p.id_of(&PM, &["ask", "Bank for deposits?", "--to", "human:omar"]);
    let qa = p.id_of(&PM, &["ask", "Colors of the receipt?", "--to", "ana"]);
    let ql = p.id_of(
        &PM,
        &["ask", "Term of the legal contract?", "--to", "human:luis"],
    );
    // The common ones: the same for every owner, and the problems.
    let qw = p.id_of(&PM, &["ask", "Monthly price?", "--to", "owner"]);
    let g2 = p.id_of(&PM, &["goal", "add", "Refunds"]);
    distill_fact(&p, "The pilot customer is Lopez Hardware");
    let c = coord("payments");
    let t: Value = serde_json::from_str(&p.run_as_ok(
        &c,
        &["--json", "task", "add", "Webhooks", "--epic", "payments"],
    ))
    .unwrap();
    let t = t["id"].as_str().unwrap().to_string();
    p.ctx_ok(&["done", &t, "--evidence", "cmd:\"cargo test\"=0"]);
    p.agent_ok("tester", &["reopen", &t, "--why", "charges twice"]);
    // A question for an agent doesn't make anyone a human.
    p.agent_ok("coder", &["ask", "Which SDK?", "--to", "coder"]);

    let before = fs::read_to_string(p.ledger("omar@test")).unwrap();
    let ev = everyone(&p, false);
    let humans = ev["humans"].as_array().unwrap();
    let names: Vec<&str> = humans
        .iter()
        .map(|h| h["human"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["omar", "ana", "luis"], "{ev:#}");
    assert_eq!(humans[0]["owner"], true);
    assert_eq!(humans[2]["owner"], false);
    let mut omar = keys_of(&humans[0]);
    omar.sort();
    let mut want = vec![qo.clone(), qo2.clone()];
    want.sort();
    assert_eq!(omar, want, "{ev:#}");
    assert_eq!(keys_of(&humans[1]), std::slice::from_ref(&qa));
    assert_eq!(keys_of(&humans[2]), std::slice::from_ref(&ql));
    assert_eq!(humans[2]["count"], 1);
    let common = keys_of(&ev["common"]);
    assert!(common.contains(&qw), "{common:?}");
    assert!(common.contains(&g2), "{common:?}");
    assert!(common.contains(&"review".to_string()), "{common:?}");
    assert_eq!(ev["common"]["count"], 4, "{ev:#}");
    assert_eq!(ev["count"], 8, "{ev:#}");
    // The problem appears once, in the common section.
    let text = ev.to_string();
    assert_eq!(text.matches("Reopened: Webhooks").count(), 1, "{ev:#}");

    // In text: one section per human, in order, and the common one at the end.
    let out = p.ctx_ok(&["inbox", "--everyone"]);
    let at = |s: &str| {
        out.find(s)
            .unwrap_or_else(|| panic!("missing {s:?} in:\n{out}"))
    };
    assert!(
        out.starts_with("# Pending for everyone in demo: 8"),
        "{out}"
    );
    assert!(
        at("## omar: 2 pending items") < at("## ana: 1 pending item"),
        "{out}"
    );
    assert!(
        at("## ana:") < at("## luis (not an owner): 1 pending item"),
        "{out}"
    );
    assert!(at("## luis") < at("## Common: 4 pending items"), "{out}");
    assert_eq!(out.matches("Reopened: Webhooks").count(), 1, "{out}");
    assert_eq!(out.matches("Monthly price?").count(), 1, "{out}");
    assert!(out.contains("from conversations"), "{out}");
    assert!(!out.contains("Which SDK?"), "{out}");

    // One line per human with `count`.
    let count = p.ctx_ok(&["inbox", "count", "--everyone"]);
    let lines: Vec<&str> = count.lines().collect();
    assert_eq!(lines.len(), 4, "{count}");
    assert!(lines[0].starts_with("omar: 2 pending items"), "{count}");
    assert!(lines[1].starts_with("ana: 1 pending item"), "{count}");
    assert!(
        lines[2].starts_with("luis (not an owner): 1 pending item"),
        "{count}"
    );
    assert!(lines[3].starts_with("common: 4 pending items"), "{count}");

    // Seeing it closes nothing and writes nothing.
    assert_eq!(fs::read_to_string(p.ledger("omar@test")).unwrap(), before);
    assert_eq!(everyone(&p, false), ev);

    // `kontext inbox` is still one person's: omar's doesn't carry ana's.
    let mine = keys_of(&inbox(&p));
    assert!(mine.contains(&qo) && mine.contains(&qw), "{mine:?}");
    assert!(!mine.contains(&qa) && !mine.contains(&ql), "{mine:?}");
    // And luis sees theirs (`--to human:luis` is luis).
    let luis: Value =
        serde_json::from_str(&p.ctx_ok(&["--json", "inbox", "--as", "luis"])).unwrap();
    assert!(keys_of(&luis).contains(&ql), "{luis:#}");

    // When ana answers, their section goes away and they are listed as quiet.
    p.ctx_env_ok(
        &[("KONTEXT_ACTOR", "human:ana")],
        &["answer", &qa, "Blue and white"],
    );
    let ev = everyone(&p, false);
    let names: Vec<&str> = ev["humans"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["human"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["omar", "luis"], "{ev:#}");
    assert_eq!(ev["quiet"], serde_json::json!(["ana"]));
    let out = p.ctx_ok(&["inbox", "--everyone"]);
    assert!(out.contains("No pending items of their own: ana."), "{out}");
    let count = p.ctx_ok(&["inbox", "count", "--everyone"]);
    assert!(
        count.contains("ana: no pending items of their own."),
        "{count}"
    );
}

#[test]
fn everyone_shows_five_per_section_unless_all() {
    let p = Project::new();
    for q in [
        "Charge currency?",
        "Support hours?",
        "Language of the emails?",
        "New or old logo?",
        "How many charge retries?",
        "Invoice with CFDI?",
    ] {
        p.ctx_env_ok(&PM, &["ask", q, "--to", "omar", "--no-merge"]);
    }
    let ev = everyone(&p, false);
    assert_eq!(ev["humans"][0]["count"], 6, "{ev:#}");
    assert_eq!(ev["humans"][0]["items"].as_array().unwrap().len(), 5);
    assert_eq!(ev["humans"][0]["more"], 1);
    let out = p.ctx_ok(&["inbox", "--everyone"]);
    assert!(
        out.contains("And 1 more: 1 to answer. `kontext inbox --everyone --all` shows them."),
        "{out}"
    );
    // No numbers: this view's numbers are nobody's inbox numbers.
    assert!(!out.contains("\n1. "), "{out}");

    let ev = everyone(&p, true);
    assert_eq!(ev["humans"][0]["items"].as_array().unwrap().len(), 6);
    assert_eq!(ev["humans"][0]["more"], 0);
    let out = p.ctx_ok(&["inbox", "--everyone", "--all"]);
    assert!(!out.contains("And 1 more"), "{out}");
    assert!(out.contains("Invoice with CFDI?"), "{out}");

    // Nothing from anyone.
    let q = Project::new();
    let out = q.ctx_ok(&["inbox", "--everyone"]);
    assert!(
        out.contains("Nothing is waiting for anyone's action."),
        "{out}"
    );
}

#[test]
fn everyone_refuses_as_and_acting() {
    let p = Project::new();
    let out = p.ctx(&["inbox", "--everyone", "--as", "omar"]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("--everyone and --as don't go together"),
        "{err}"
    );
    for args in [
        &["inbox", "--everyone", "sign", "1"][..],
        &["inbox", "dismiss", "1", "--everyone", "--why", "x"][..],
    ] {
        let out = p.ctx(args);
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("--everyone is only for viewing"), "{err}");
    }
}
