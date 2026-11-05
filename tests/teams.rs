//! Team views: `brief`, `next`, the per-epic line for the pm (without tasks)
//! and the full phase 3 flow.

mod common;

use common::{implementer, with_epic};

#[test]
fn brief_next_and_pm_views() {
    let (p, g, e) = with_epic();
    let coord = [
        ("KONTEXT_ACTOR", "agent:epic:payments"),
        ("KONTEXT_ROLE", "coordinator"),
    ];
    let t1 = p.id_of(
        &coord,
        &[
            "task",
            "add",
            "Integrate Stripe SDK",
            "--epic",
            "payments",
            "--acceptance",
            "cargo test passes",
        ],
    );
    let t2 = p.id_of(
        &coord,
        &[
            "task",
            "add",
            "Billing webhooks",
            "--epic",
            "payments",
            "--depends-on",
            &t1,
        ],
    );
    let t3 = p.id_of(
        &coord,
        &["task", "add", "Payment screen", "--epic", "payments"],
    );
    let d = p.id_of(
        &[],
        &[
            "decide",
            "Charge with Stripe",
            "--why",
            "3DS",
            "--epic",
            "payments",
        ],
    );
    let d_other = p.id_of(&[], &["decide", "Use Rust", "--tags", "base"]);
    let f = p.id_of(
        &[("KONTEXT_ACTOR", "agent:scout")],
        &[
            "fact",
            "The Stripe SDK requires Node 20 (package.json:12)",
            "--epic",
            "payments",
        ],
    );
    let q_epic = p.id_of(
        &[("KONTEXT_ACTOR", "agent:impl-1")],
        &["ask", "Which currency?", "--to", "epic:payments"],
    );
    let q_pm = p.id_of(
        &coord,
        &["ask", "Build the screen before the webhooks?", "--to", "pm"],
    );
    let q_owner = p.id_of(
        &[("KONTEXT_ACTOR", "agent:pm"), ("KONTEXT_ROLE", "pm")],
        &["ask", "Price?", "--to", "owner"],
    );

    let (claimed, mark) = p.claim("impl-1", "payments");
    assert_eq!(claimed, t1);

    // next: the assignment carries acceptance, goals, decisions, facts and the mark.
    let out = p.run_as_ok(&implementer("impl-1"), &["next", "--epic", "payments"]);
    for want in [
        format!("# Assignment: {t1} Integrate Stripe SDK"),
        format!("Epic Card payments (epic:payments, {e}) in demo. Status: claimed by agent:impl-1"),
        format!("Assignment mark: `{mark}`"),
        "- Acceptance: cargo test passes".into(),
        format!("- Goal: {g} [signed] Recurring charge"),
        format!("- {d} Charge with Stripe (why: 3DS)"),
        format!("- {f} The Stripe SDK requires Node 20"),
        format!("kontext done {t1} --run \"<command that tests the acceptance>\" --since {mark}"),
    ] {
        assert!(out.contains(&want), "missing {want:?}:\n{out}");
    }
    assert!(!out.contains(&d_other), "{out}");

    // brief: board, epic decisions, questions for epic:payments, facts and the mark.
    let brief = p.kontext_ok(&["brief", "epic:payments"]);
    for want in [
        "# Brief for epic Card payments (epic:payments) in demo".to_string(),
        format!("## Epic goals (1)\n- {g} [signed] Recurring charge"),
        format!("## Epic decisions (1)\n- {d} Charge with Stripe (why: 3DS)"),
        format!("## Other decisions in force (1)\n- {d_other} Use Rust"),
        "## Board (3 tasks: 1 ready, 1 blocked, 1 claimed)".into(),
        format!("- {t3} Payment screen"),
        format!("- {t1} Integrate Stripe SDK → claimed by agent:impl-1"),
        format!("- {t2} Billing webhooks → waits for {t1} (claimed)"),
        format!("## Questions for epic:payments (1)\n- {q_epic} Which currency?"),
        format!("## Epic facts (1)\n- {f} "),
        "## Assignment mark\n`".into(),
    ] {
        assert!(brief.contains(&want), "missing {want:?}:\n{brief}");
    }
    assert!(
        !brief.contains(&q_pm) && !brief.contains(&q_owner),
        "{brief}"
    );
    // brief advances the cursor of epic:payments (as resume does); --peek does not.
    let again = p.kontext_ok(&["brief", "payments", "--peek"]);
    assert!(
        again.contains("## What's new since your last visit (0)"),
        "{again}"
    );
    // With a small budget, the open tasks and the mark are never dropped.
    let tiny = p.kontext_ok(&["brief", "payments", "--peek", "--max-chars", "200"]);
    assert!(
        tiny.contains(&format!("- {t3} Payment screen")) && tiny.contains("## Assignment mark")
    );
    assert!(!tiny.contains("(why: 3DS)"), "{tiny}");
    let v: serde_json::Value =
        serde_json::from_str(&p.kontext_ok(&["--json", "brief", &e, "--peek"])).unwrap();
    assert_eq!(v["reader"], "epic:payments");
    assert_eq!(v["tasks"].as_array().unwrap().len(), 3);

    // resume --as pm: one line per epic, what was escalated to the pm, no tasks or facts.
    let pm = p.kontext_ok(&["resume", "--as", "pm"]);
    assert!(
        pm.contains(&format!(
            "## Epics (1)\n- {e} Card payments: 0/3 closed, 1 claimed, 1 ready, 1 blocked; last activity "
        )),
        "{pm}"
    );
    assert!(
        pm.contains(&format!("## Questions for pm (1)\n- {q_pm}")),
        "{pm}"
    );
    for hidden in [
        "Integrate Stripe SDK",
        "Billing webhooks",
        "Payment screen",
        "Node 20",
        "Which currency?",
    ] {
        assert!(
            !pm.contains(hidden),
            "the pm must not see {hidden:?}:\n{pm}"
        );
    }
    assert!(pm.contains("from the epics' work"), "{pm}");
    // The owner does see the facts, and their question reaches them in standup.
    let owner = p.kontext_ok(&["resume", "--as", "owner", "--peek"]);
    assert!(owner.contains("Node 20"), "{owner}");
    let standup = p.kontext_ok(&["standup", "--peek"]);
    assert!(
        standup.contains(&format!(
            "- {q_owner} question (new) from agent:pm (pm): Price?"
        )),
        "{standup}"
    );
}

#[test]
fn drift_shows_in_pm_resume_without_task_titles() {
    let (p, g, _) = with_epic();
    p.kontext_ok(&["task", "add", "Secret thing", "--epic", "payments"]);
    let g2 = p.id_of(&[], &["goal", "add", "Another goal"]);
    p.kontext_ok(&["sign", &g2]);
    p.kontext_ok(&["supersede", &g, &g2]);
    let pm = p.kontext_ok(&["resume", "--as", "pm", "--peek"]);
    assert!(
        pm.contains(&format!("cites {g}, which was superseded by {g2}")),
        "{pm}"
    );
    assert!(pm.contains("drift: the epic"), "{pm}");
    assert!(pm.contains("2 drift"), "{pm}");
    assert!(!pm.contains("Secret thing"), "{pm}");
}

#[test]
fn team_flow_end_to_end() {
    let p = common::Project::new();
    let a = p.id_of(
        &[],
        &[
            "charter",
            "--objective",
            "Charge by card",
            "--done-when",
            "First real charge",
        ],
    );
    p.kontext_ok(&["sign", &a]);
    let g = p.id_of(&[], &["goal", "add", "Recurring billing"]);
    p.kontext_ok(&["sign", &g]);
    let pm = [("KONTEXT_ACTOR", "agent:pm"), ("KONTEXT_ROLE", "pm")];
    let e = p.id_of(&pm, &["epic", "add", "Payments", "--goal", &g]);
    let coord = [
        ("KONTEXT_ACTOR", "agent:epic:payments"),
        ("KONTEXT_ROLE", "coordinator"),
    ];
    let t1 = p.id_of(
        &coord,
        &[
            "task",
            "add",
            "SDK",
            "--epic",
            "payments",
            "--acceptance",
            "true exits with 0",
        ],
    );
    let t2 = p.id_of(
        &coord,
        &["task", "add", "Webhooks", "--epic", &e, "--depends-on", &t1],
    );
    let t3 = p.id_of(
        &coord,
        &["task", "add", "Screen", "--epic", "epic:payments"],
    );
    p.kontext_ok(&["standup"]);

    let (got, mark) = p.claim("impl-1", "payments");
    assert_eq!(got, t1);
    p.run_as_ok(
        &implementer("impl-1"),
        &["log", "SDK integrated", "--task", &t1],
    );
    p.run_as_ok(
        &implementer("impl-1"),
        &["done", &t1, "--run", "true", "--since", &mark],
    );
    let state = p.state();
    assert_eq!(state["tasks"][&t1], "closed");
    assert_eq!(state["tasks"][&t2], "ready");
    assert_eq!(state["tasks"][&t3], "ready");

    let standup = p.kontext_ok(&["standup"]);
    assert!(
        standup.contains(&format!(
            "- {t1} task closed by agent:impl-1 (implementer): SDK · evidence: `true` (0)"
        )),
        "{standup}"
    );
    let pm_view = p.kontext_ok(&["resume", "--as", "pm"]);
    assert!(
        pm_view.contains("- ") && pm_view.contains("Payments: 1/3 closed, 2 ready; last activity "),
        "{pm_view}"
    );
    assert!(p.kontext_ok(&["verify"]).starts_with("OK: "));
}
