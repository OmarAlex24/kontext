//! Commands that write: the owner rule, references and the state they leave.

mod common;

use common::Project;
use serde_json::Value;

fn state(p: &Project) -> Value {
    serde_json::from_str(&p.ctx_ok(&["debug", "state"])).unwrap()
}

#[test]
fn agents_cannot_decide_but_can_propose() {
    let p = Project::new();
    let out = p.ctx_env(
        &[("KONTEXT_ACTOR", "agent:coder")],
        &["decide", "Use Conekta"],
    );
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("only an owner can record a decision"), "{err}");
    assert!(err.contains("kontext propose"), "{err}");
    // Without KONTEXT_ACTOR it writes as agent:unknown, which does not decide either.
    let out = p.ctx_env(&[("KONTEXT_ACTOR", "")], &["decide", "x"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        !p.ledger("omar@test").exists(),
        "it must not write anything"
    );

    let out = p.agent_ok("coder", &["propose", "Use Conekta", "--why", "barato"]);
    assert!(out.contains("saved the proposal"), "{out}");
    for cmd in [
        &["sign", "P-"][..],
        &["accept", "P-"],
        &["reject", "P-", "--why", "no"],
    ] {
        let out = p.ctx_env(&[("KONTEXT_ACTOR", "agent:coder")], cmd);
        assert_eq!(out.status.code(), Some(2), "{cmd:?}");
    }
}

#[test]
fn pm_with_approved_by_counts_and_actor_fields_come_from_env() {
    let p = Project::new();
    let pm = [
        ("KONTEXT_ACTOR", "agent:pm"),
        ("KONTEXT_ROLE", "pm"),
        ("KONTEXT_MODEL", "claude-opus-5-5"),
        ("KONTEXT_SESSION", "s-1"),
    ];
    // Without approved_by, no.
    assert_eq!(p.ctx_env(&pm, &["decide", "Stripe"]).status.code(), Some(2));
    // approved_by from someone who is not an owner, neither.
    let out = p.ctx_env(&pm, &["decide", "Stripe", "--approved-by", "ana"]);
    assert_eq!(out.status.code(), Some(2));
    let id = p.id_of(&pm, &["decide", "Stripe", "--approved-by", "omar"]);
    let e = &p.entries("omar@test")[0];
    assert_eq!(e["id"], id.as_str());
    assert_eq!(e["actor"]["role"], "pm");
    assert_eq!(e["actor"]["model"], "claude-opus-5-5");
    assert_eq!(e["actor"]["session"], "s-1");
    assert_eq!(e["fields"]["approved_by"], "omar");
    assert_eq!(state(&p)["status"][&id], "accepted");
    // A human who is not an owner cannot decide from the command line either.
    let out = p.ctx_env(&[("KONTEXT_ACTOR", "human:ana")], &["decide", "x"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("is not in owners"));
}

/// The same with arguments instead of variables in front of the command, which a
/// `Bash(kontext:*)` permission in Claude Code does not cover. Arguments win over
/// variables.
#[test]
fn actor_role_and_model_also_come_from_flags() {
    let p = Project::new();
    let none = [
        ("KONTEXT_ACTOR", ""),
        ("KONTEXT_ROLE", ""),
        ("KONTEXT_MODEL", ""),
    ];
    let id = p.id_of(
        &none,
        &[
            "--actor",
            "agent:pm",
            "--role",
            "pm",
            "--model",
            "claude-opus-5-5",
            "decide",
            "Stripe",
            "--approved-by",
            "omar",
        ],
    );
    let e = &p.entries("omar@test")[0];
    assert_eq!(e["id"], id.as_str());
    assert_eq!(e["actor"]["role"], "pm");
    assert_eq!(e["actor"]["model"], "claude-opus-5-5");
    assert_eq!(state(&p)["status"][&id], "accepted");
    // Also after the subcommand, and above $KONTEXT_ROLE.
    let impl_env = [
        ("KONTEXT_ACTOR", "agent:impl-1"),
        ("KONTEXT_ROLE", "implementer"),
    ];
    let out = p.ctx_env(&impl_env, &["decide", "x", "--approved-by", "omar"]);
    assert_eq!(out.status.code(), Some(2));
    p.id_of(&impl_env, &["log", "Tried the flag", "--role", "tester"]);
    let last = p.entries("omar@test").pop().unwrap();
    assert_eq!(last["actor"]["name"], "impl-1");
    assert_eq!(last["actor"]["role"], "tester");
}

#[test]
fn non_owner_entry_from_another_machine_is_kept_but_does_not_count() {
    let p = Project::new();
    let d = p.id_of(&[], &["decide", "Stripe"]);
    // A decision from ana, who is not an owner, arrives through sync.
    let ana = p.machine("ana@laptop");
    let line = format!(
        "{{\"kind\":\"decision\",\"title\":\"Conekta\",\"actor\":{{\"type\":\"human\",\"name\":\"ana\"}},\"fields\":{{\"supersedes\":[\"{d}\"]}}}}\n"
    );
    let out = p.spawn_append(&ana, &line, 0).wait_with_output().unwrap();
    assert!(out.status.success());
    let st = state(&p);
    assert_eq!(st["status"][&d], "accepted", "{st}");
    let alerts = st["alerts"].as_array().unwrap();
    assert_eq!(alerts.len(), 1, "{st}");
    assert_eq!(alerts[0]["type"], "not_counting");
    let standup = p.ctx_ok(&["standup", "--peek"]);
    assert!(
        standup.contains("does not count: it was written by someone who is not an owner"),
        "{standup}"
    );
}

#[test]
fn charter_goals_signing_and_refs() {
    let p = Project::new();
    // Without a charter there are no goals.
    let out = p.ctx(&["goal", "add", "Charge"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("there is no charter"));

    let a = p.id_of(
        &[],
        &[
            "charter",
            "--objective",
            "Charge by card",
            "--non-goal",
            "Facturar",
            "--non-goal",
            "Inventario",
            "--done-when",
            "First real charge",
        ],
    );
    let g = p.id_of(
        &[],
        &[
            "goal",
            "add",
            "Recurring charge",
            "--done-when",
            "3 charges",
        ],
    );
    let e = p.entries("omar@test");
    assert_eq!(
        e[0]["fields"]["non_goals"],
        serde_json::json!(["Facturar", "Inventario"])
    );
    assert_eq!(e[1]["fields"]["charter"], a.as_str());
    p.ctx_ok(&["sign", &a[..4]]);
    p.ctx_ok(&["sign", &g]);
    // Signing twice is refused.
    let out = p.ctx(&["sign", &g]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("is already signed"));
    // A decision is not signed.
    let d = p.id_of(&[], &["decide", "Stripe"]);
    assert_eq!(p.ctx(&["sign", &d]).status.code(), Some(2));
    // A new charter supersedes the signed one when it is signed.
    let a2 = p.id_of(&[], &["charter", "--objective", "Charge and invoice"]);
    p.ctx_ok(&["sign", &a2]);
    let st = state(&p);
    assert_eq!(st["status"][&a], "superseded", "{st}");
    assert_eq!(st["status"][&a2], "signed");
    assert_eq!(st["alerts"], serde_json::json!([]));
}

#[test]
fn decisions_supersede_and_proposals_become_law() {
    let p = Project::new();
    let d1 = p.id_of(
        &[],
        &[
            "decide",
            "Conekta",
            "--tags",
            "payments,core",
            "--guard-cmd",
            "true",
        ],
    );
    let e = &p.entries("omar@test")[0];
    assert_eq!(e["tags"], serde_json::json!(["payments", "core"]));
    assert_eq!(e["fields"]["guard"]["cmd"], "true");
    // --supersedes has to be a decision in force.
    let out = p.ctx(&["decide", "x", "--supersedes", "D-zzzzzz"]);
    assert!(!out.status.success());
    let pr = p.id_of(
        &[("KONTEXT_ACTOR", "agent:coder")],
        &["propose", "Stripe", "--why", "3DS", "--supersedes", &d1],
    );
    let st = state(&p);
    assert_eq!(st["status"][&d1], "accepted");
    assert_eq!(st["status"][&pr], "pending");
    // Accepting with --as-decision writes the decision and the accept.
    let d2 = p.id_of(&[], &["accept", &pr, "--as-decision"]);
    assert!(d2.starts_with("D-"));
    let st = state(&p);
    assert_eq!(st["status"][&pr], "accepted");
    assert_eq!(st["status"][&d1], "superseded");
    assert_eq!(st["superseded_by"][&d1], d2.as_str());
    // It is no longer pending.
    let out = p.ctx(&["accept", &pr]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("is already accepted"));
    // Explicit supersede and its validation.
    let d3 = p.id_of(&[], &["decide", "Stripe with 3DS"]);
    p.ctx_ok(&["supersede", &d2, &d3, "--why", "clearer"]);
    let out = p.ctx(&["supersede", &d2, &d3]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("is already superseded"));
    let show = p.ctx_ok(&["show", &d3]);
    assert!(
        show.contains(&format!(
            "{d1} (superseded) → {d2} (superseded) → **{d3} (this one)**"
        )),
        "{show}"
    );
    // A rejected proposal needs --why.
    let p2 = p.id_of(&[("KONTEXT_ACTOR", "agent:coder")], &["propose", "Other"]);
    p.ctx_ok(&["reject", &p2, "--why", "no aplica"]);
    assert_eq!(state(&p)["status"][&p2], "rejected");
}

#[test]
fn questions_answers_and_journal() {
    let p = Project::new();
    let q = p.id_of(
        &[("KONTEXT_ACTOR", "agent:coder")],
        &[
            "ask",
            "How much do we charge?",
            "--to",
            "owner",
            "--body",
            "precio",
        ],
    );
    assert_eq!(p.entries("omar@test")[0]["fields"]["to"], "owner");
    assert!(p.ctx(&["ask", "x", "--to", "../etc"]).status.code() != Some(0));
    p.ctx_ok(&["answer", &q, "199 a month"]);
    assert_eq!(state(&p)["status"][&q], "answered");
    let show = p.ctx_ok(&["show", &q]);
    assert!(show.contains("199 a month"), "{show}");
    let j = p.id_of(
        &[],
        &[
            "log",
            "Integrated the SDK",
            "--next",
            "pruebas",
            "--task",
            "T-1",
        ],
    );
    let e = p.entries("omar@test");
    let journal = e.iter().find(|x| x["id"] == j.as_str()).unwrap();
    assert_eq!(journal["fields"]["next"], "pruebas");
    assert_eq!(journal["fields"]["task"], "T-1");
    // Answering something that is not a question.
    assert_eq!(p.ctx(&["answer", &j, "x"]).status.code(), Some(2));
}
