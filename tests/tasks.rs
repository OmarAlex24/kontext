//! Epics and tasks from the binary: `task add`, `next`, `assign`, `release`,
//! `reopen`, `done` (evidence, `--run`, stale context), claims that expire,
//! owners and task questions.

mod common;

use common::{Project, implementer, with_epic};

fn task(p: &Project, title: &str, extra: &[&str]) -> String {
    let mut args = vec!["task", "add", title, "--epic", "payments"];
    args.extend_from_slice(extra);
    p.id_of(&[], &args)
}

fn status(p: &Project, t: &str) -> String {
    p.state()["tasks"][t].as_str().unwrap().to_string()
}

fn stderr(o: &std::process::Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn task_add_defaults_goals_and_warns_on_drift() {
    let (p, g, e) = with_epic();
    let t = task(&p, "Integrate SDK", &["--acceptance", "cargo test passes"]);
    let entry = p
        .entries("omar@test")
        .into_iter()
        .find(|x| x["id"] == t.as_str())
        .unwrap();
    assert_eq!(entry["fields"]["goals"][0], g.as_str());
    assert_eq!(entry["fields"]["epic"], e.as_str());
    assert_eq!(entry["fields"]["acceptance"], "cargo test passes");
    // By epic id, or by epic:<name>.
    p.kontext_ok(&["task", "add", "Another", "--epic", &e]);
    p.kontext_ok(&["task", "add", "One more", "--epic", "epic:payments"]);
    assert_eq!(
        p.kontext(&["task", "add", "x", "--epic", "nothing"])
            .status
            .code(),
        Some(1)
    );

    // An unsigned goal: it is written, with a warning, and becomes an alert.
    let g2 = p.id_of(&[], &["goal", "add", "Refunds"]);
    let out = p.kontext_ok(&["task", "add", "Refund", "--epic", "payments", "--goal", &g2]);
    assert!(
        out.contains(&format!("Warning: unsigned goal: {g2}")),
        "{out}"
    );

    // An epic without goals (it came from another version): the task is written with a drift warning.
    let bare = p.append(
        "{\"kind\":\"epic\",\"title\":\"Loose\",\"actor\":{\"type\":\"human\",\"name\":\"omar\"},\"fields\":{\"name\":\"loose\",\"goals\":[]}}\n",
    );
    let out = p.kontext_ok(&["task", "add", "No course", "--epic", "loose"]);
    assert!(out.contains("Warning: drift: it cites no goal"), "{out}");
    let standup = p.kontext_ok(&["standup", "--peek"]);
    assert!(standup.contains("drift: the epic"), "{standup}");
    assert!(standup.contains(&bare[0]), "{standup}");
    assert!(standup.contains("does not cite any goal"), "{standup}");
}

#[test]
fn next_picks_in_order_and_dependencies_unblock() {
    let (p, _, _) = with_epic();
    let t1 = task(&p, "First", &[]);
    let t2 = task(&p, "Second", &["--depends-on", &t1]);
    let t3 = task(&p, "Third", &[]);
    assert_eq!(status(&p, &t2), "blocked");

    // Without --claim it writes nothing.
    let before = p.entries("omar@test").len();
    let out = p.run_as_ok(&implementer("impl-1"), &["next", "--epic", "payments"]);
    assert!(
        out.starts_with(&format!("# Assignment: {t1} First")),
        "{out}"
    );
    assert!(out.contains("No one holds it yet"), "{out}");
    assert_eq!(p.entries("omar@test").len(), before);

    let (a, _) = p.claim("impl-1", "payments");
    let (b, _) = p.claim("impl-2", "payments");
    assert_eq!((a.as_str(), b.as_str()), (t1.as_str(), t3.as_str()));
    // Whoever already has a task gets it back, without another claim.
    let (again, _) = p.claim("impl-1", "payments");
    assert_eq!(again, t1);
    assert_eq!(
        p.entries("omar@test")
            .iter()
            .filter(|e| e["kind"] == "claim")
            .count(),
        2
    );
    // Nothing ready is left: it exits with 2 and says what blocks.
    let out = p.run_as(
        &implementer("impl-3"),
        &["next", "--epic", "payments", "--claim"],
    );
    assert_eq!(out.status.code(), Some(2));
    let err = stderr(&out);
    assert!(err.contains("No ready tasks"), "{err}");
    assert!(
        err.contains(&format!("- {t2} Second → waits for {t1} (claimed)")),
        "{err}"
    );

    let out = p.run_as_ok(
        &implementer("impl-1"),
        &["done", &t1, "--run", "true", "--since", "-"],
    );
    assert!(out.contains(&format!("Now ready: {t2} Second")), "{out}");
    assert_eq!(status(&p, &t1), "closed");
    assert_eq!(status(&p, &t2), "ready");
}

#[test]
fn done_needs_evidence_and_a_fresh_mark() {
    let (p, g, _) = with_epic();
    let t = task(&p, "Integrate SDK", &["--acceptance", "cargo test passes"]);
    let (_, mark) = p.claim("impl-1", "payments");
    let me = implementer("impl-1");

    // Without evidence, without --since, or with evidence that is not valid: it does not close.
    for args in [
        vec!["done", &t, "--since", &mark],
        vec!["done", &t, "--evidence", "cmd:\"cargo test\"=0"],
        vec![
            "done",
            &t,
            "--evidence",
            "cmd:cargo test=1",
            "--since",
            &mark,
        ],
        vec!["done", &t, "--evidence", "cmd:cargo test", "--since", &mark],
    ] {
        let out = p.run_as(&me, &args);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {}", stderr(&out));
    }
    // --run with a command that fails: it does not close, and it shows why.
    let out = p.run_as(
        &me,
        &["done", &t, "--run", "echo red; exit 4", "--since", &mark],
    );
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("exited with 4"), "{}", stderr(&out));
    assert!(stderr(&out).contains("red"), "{}", stderr(&out));
    assert_eq!(status(&p, &t), "claimed");

    // Another agent cannot close it.
    let out = p.run_as(
        &implementer("intruder"),
        &[
            "done",
            &t,
            "--evidence",
            "pr:https://x/pull/1",
            "--since",
            &mark,
        ],
    );
    assert_eq!(out.status.code(), Some(2));
    assert!(
        stderr(&out).contains("held by agent:impl-1"),
        "{}",
        stderr(&out)
    );

    // A decision from the epic comes in after the mark: stale context.
    let d = p.id_of(&[], &["decide", "Charge in MXN", "--epic", "payments"]);
    let out = p.run_as(&me, &["done", &t, "--run", "true", "--since", &mark]);
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("stale context"), "{err}");
    assert!(
        err.contains(&format!("- {d} decision: Charge in MXN")),
        "{err}"
    );
    let fresh = err
        .split("--since ")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_string();
    assert_ne!(fresh, mark);

    // Another goal replaces the one the task cites: that is stale context too.
    let g2 = p.id_of(&[], &["goal", "add", "Recurring billing v2"]);
    p.kontext_ok(&["supersede", &g, &g2]);
    let out = p.run_as(&me, &["done", &t, "--run", "true", "--since", &fresh]);
    assert_eq!(out.status.code(), Some(3));
    assert!(stderr(&out).contains(&format!("goal {g} was superseded by {g2}")));
    let fresh = p.kontext_ok(&["--json", "brief", "payments", "--peek"]);
    let fresh: serde_json::Value = serde_json::from_str(&fresh).unwrap();
    let fresh = fresh["mark"].as_str().unwrap();

    // The file: evidence must exist (relative to the current folder).
    let out = p.run_as(
        &me,
        &[
            "done",
            &t,
            "--evidence",
            "file:missing.rs",
            "--since",
            fresh,
        ],
    );
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("evidence file:missing.rs: does not exist"));
    std::fs::write(p.dir.path().join("Cargo.toml"), "[package]\n").unwrap();

    // With the new mark, it closes; the evidence keeps the code and the output.
    p.run_as_ok(
        &me,
        &[
            "done",
            &t,
            "--run",
            "echo green",
            "--evidence",
            "file:Cargo.toml",
            "--since",
            fresh,
        ],
    );
    assert_eq!(status(&p, &t), "closed");
    let done = p
        .entries("omar@test")
        .into_iter()
        .rfind(|e| e["kind"] == "done")
        .unwrap();
    assert_eq!(done["fields"]["evidence"][0]["exit_code"], 0);
    assert_eq!(done["fields"]["evidence"][0]["excerpt"], "green");
    assert_eq!(done["fields"]["evidence"][1]["type"], "file");
    assert_eq!(done["fields"]["since"], fresh);
    // Already closed: it does not close twice.
    let out = p.run_as(&me, &["done", &t, "--run", "true", "--since", fresh]);
    assert_eq!(out.status.code(), Some(2));
    // An owner can close without --since.
    let t2 = task(&p, "Another", &[]);
    p.kontext_ok(&[
        "done",
        &t2,
        "--evidence",
        "pr:https://github.com/a/b/pull/2",
    ]);
    assert_eq!(status(&p, &t2), "closed");
}

#[test]
fn answer_to_a_task_question_is_stale_context() {
    let (p, _, _) = with_epic();
    let t = task(&p, "Integrate SDK", &[]);
    let (_, mark) = p.claim("impl-1", "payments");
    let me = implementer("impl-1");
    // --task without --to: it goes to the task's epic.
    let q = p.id_of(
        &me.iter().map(|(k, v)| (*k, v.as_str())).collect::<Vec<_>>(),
        &["ask", "Which currency?", "--task", &t],
    );
    let entry = p
        .entries("omar@test")
        .into_iter()
        .find(|e| e["id"] == q.as_str())
        .unwrap();
    assert_eq!(entry["fields"]["to"], "epic:payments");
    assert_eq!(entry["fields"]["task"], t.as_str());
    let brief = p.kontext_ok(&["brief", "payments", "--peek"]);
    assert!(
        brief.contains(&format!(
            "- {q} Which currency? [task {t}] (from agent:impl-1 (implementer))"
        )),
        "{brief}"
    );
    p.kontext_env_ok(
        &[
            ("KONTEXT_ACTOR", "agent:epic:payments"),
            ("KONTEXT_ROLE", "coordinator"),
        ],
        &["answer", &q, "MXN"],
    );
    let out = p.run_as(&me, &["done", &t, "--run", "true", "--since", &mark]);
    assert_eq!(out.status.code(), Some(3));
    assert!(
        stderr(&out).contains(&format!("answer to {q}: MXN")),
        "{}",
        stderr(&out)
    );
    // The task's assignment already carries the answer.
    let out = p.run_as_ok(&me, &["next", "--epic", "payments"]);
    assert!(
        out.contains(&format!("- {q} Which currency? → MXN")),
        "{out}"
    );
    // --to epic:<x> for an epic that does not exist: an error.
    assert_ne!(
        p.kontext(&["ask", "x", "--to", "epic:nothing"])
            .status
            .code(),
        Some(0)
    );
}

#[test]
fn assign_release_reopen_and_who_may_do_them() {
    let (p, _, _) = with_epic();
    let t = task(&p, "Integrate SDK", &[]);
    let coord = [
        ("KONTEXT_ACTOR", "agent:epic:payments"),
        ("KONTEXT_ROLE", "coordinator"),
    ];
    // An implementer does not assign.
    let out = p.run_as(
        &implementer("impl-1"),
        &["assign", &t, "--to", "agent:impl-1"],
    );
    assert_eq!(out.status.code(), Some(2));
    p.kontext_env_ok(&coord, &["assign", &t, "--to", "agent:impl-1"]);
    assert_eq!(status(&p, &t), "assigned");
    // The assignee receives their task with next, without claiming another.
    let (got, _) = p.claim("impl-1", "payments");
    assert_eq!(got, t);
    // Another agent cannot release it; the assignee can.
    assert_eq!(
        p.run_as(&implementer("impl-2"), &["release", &t])
            .status
            .code(),
        Some(2)
    );
    p.run_as_ok(
        &implementer("impl-1"),
        &["release", &t, "--why", "got stuck"],
    );
    assert_eq!(status(&p, &t), "ready");
    // The coordinator closes it; anyone can reopen it with a reason.
    p.kontext_env_ok(&coord, &["done", &t, "--run", "true", "--since", "-"]);
    assert_eq!(p.kontext(&["reopen", &t]).status.code(), Some(2));
    p.run_as_ok(
        &implementer("tester"),
        &["reopen", &t, "--why", "fails with 3DS cards"],
    );
    assert_eq!(status(&p, &t), "ready");
    let show = p.kontext_ok(&["show", &t]);
    for want in [
        "**Status history**",
        "assignment by agent:epic:payments (coordinator): assigned to agent:impl-1",
        "release by agent:impl-1 (implementer): released (it was held by agent:impl-1) (why: got stuck)",
        "close by agent:epic:payments (coordinator): closed with evidence: `true` (0)",
        "reopening by agent:tester (implementer): reopened (why: fails with 3DS cards)",
        "- now: ready",
    ] {
        assert!(show.contains(want), "missing {want:?}:\n{show}");
    }
}

#[test]
fn claims_expire_after_the_lease() {
    let (p, _, _) = with_epic();
    let t = task(&p, "Integrate SDK", &[]);
    let now = jiff::Timestamp::now();
    let later = |h: i64| (now + jiff::SignedDuration::from_hours(h)).to_string();
    p.claim("impl-1", "payments");
    // One hour later, with a journal entry, it is still claimed.
    let mut me = implementer("impl-1");
    me.push(("KONTEXT_NOW", later(1)));
    p.run_as_ok(&me, &["log", "moving on", "--task", &t[..5]]);
    let at = |h: i64| vec![("KONTEXT_NOW", later(h))];
    let brief = p.run_as_ok(&at(2), &["brief", "payments", "--peek"]);
    assert!(brief.contains("### Claimed (1)"), "{brief}");
    // Three hours with nothing from the journal: it expires.
    let brief = p.run_as_ok(&at(4), &["brief", "payments", "--peek"]);
    assert!(brief.contains("### Ready (1)"), "{brief}");
    assert!(
        brief.contains("(the claim by agent:impl-1 expired)"),
        "{brief}"
    );
    let standup = p.run_as_ok(&at(4), &["standup", "--peek"]);
    assert!(
        standup.contains(&format!("the claim on {t} by agent:impl-1 expired")),
        "{standup}"
    );
    // Another agent claims it and wins; the alert goes away.
    let mut other = implementer("impl-2");
    other.push(("KONTEXT_NOW", later(4)));
    let out = p.run_as_ok(&other, &["--json", "next", "--epic", "payments", "--claim"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["task"]["id"], t.as_str());
    assert_eq!(v["task"]["holder"]["actor"], "agent:impl-2");
    let standup = p.run_as_ok(&at(4), &["standup", "--peek"]);
    assert!(!standup.contains("expired"), "{standup}");
}

#[test]
fn lease_comes_from_project_toml() {
    let (p, _, _) = with_epic();
    task(&p, "Integrate SDK", &[]);
    let toml = p.home.join("project.toml");
    let text = std::fs::read_to_string(&toml).unwrap();
    std::fs::write(&toml, text.replace("lease = \"2h\"", "lease = \"30m\"")).unwrap();
    p.claim("impl-1", "payments");
    let in_40 = (jiff::Timestamp::now() + jiff::SignedDuration::from_mins(40)).to_string();
    let brief = p.kontext_env_ok(&[("KONTEXT_NOW", &in_40)], &["brief", "payments", "--peek"]);
    assert!(brief.contains("expired"), "{brief}");
}

#[test]
fn next_respects_its_budget() {
    let (p, _g, _e) = common::with_epic();
    let coord = [
        ("KONTEXT_ACTOR", "agent:epic:payments"),
        ("KONTEXT_ROLE", "coordinator"),
    ];
    p.kontext_env_ok(
        &coord,
        &[
            "task",
            "add",
            "Integrate Stripe",
            "--epic",
            "payments",
            "--acceptance",
            "cargo test passes",
        ],
    );
    for i in 0..40 {
        p.kontext_ok(&[
            "decide",
            &format!("Decision {i} for the epic with a long title to take up space"),
            "--why",
            &"because yes, with a fairly long reason ".repeat(3),
            "--epic",
            "payments",
        ]);
        p.agent_ok(
            "scout",
            &[
                "fact",
                &format!("Fact {i} for the epic with long text to take up space"),
                "--epic",
                "payments",
            ],
        );
    }
    let full = p.agent_ok(
        "impl-1",
        &["next", "--epic", "payments", "--max-chars", "100000"],
    );
    assert!(full.contains("why: because yes"), "{full}");
    let small = p.agent_ok(
        "impl-1",
        &["next", "--epic", "payments", "--max-chars", "3000"],
    );
    assert!(small.len() < full.len());
    assert!(!small.contains("why: because yes"), "{small}");
    assert!(small.contains("- Acceptance: cargo test passes"), "{small}");
    assert!(small.contains("Assignment mark: `"), "{small}");
    assert!(small.contains("- Close: `kontext done"), "{small}");
    // By default, 9,000.
    let default = p.agent_ok("impl-1", &["next", "--epic", "payments"]);
    assert!(
        default.chars().count() <= 9000 || default.contains("(kontext: this goes over 9000"),
        "{}",
        default.chars().count()
    );
    assert!(!default.contains("why: because yes"));
}
