//! Views: key lines of `standup` and `resume`, budget cutting,
//! `show` and `recall`.

mod common;

use std::fs;

use common::Project;

/// A project with charter, goals, decisions, proposals, questions and journal.
fn seeded() -> (Project, Vec<String>) {
    let p = Project::new();
    let a = p.id_of(
        &[],
        &[
            "charter",
            "--objective",
            "Charge by card in the app",
            "--non-goal",
            "Invoicing",
            "--done-when",
            "First real charge",
        ],
    );
    p.ctx_ok(&["sign", &a]);
    let g = p.id_of(&[], &["goal", "add", "Recurring charge monthly"]);
    p.ctx_ok(&["sign", &g]);
    let g2 = p.id_of(&[], &["goal", "add", "Refunds from the panel"]);
    let d = p.id_of(
        &[],
        &[
            "decide",
            "Charge with Stripe, not with Conekta",
            "--why",
            "Conekta does not support recurring charges with 3DS",
            "--tags",
            "payments",
        ],
    );
    let pr = p.id_of(
        &[("KONTEXT_ACTOR", "agent:distill")],
        &["propose", "Save tokenized cards"],
    );
    let q = p.id_of(
        &[("KONTEXT_ACTOR", "agent:coder")],
        &["ask", "What is the monthly price?", "--to", "owner"],
    );
    let q2 = p.id_of(
        &[("KONTEXT_ACTOR", "agent:coder")],
        &["ask", "Which epic goes first?", "--to", "pm"],
    );
    p.agent_ok(
        "coder",
        &["log", "Integrated the Stripe SDK", "--next", "webhooks"],
    );
    (p, vec![a, g, g2, d, pr, q, q2])
}

#[test]
fn standup_shows_what_waits_for_you_then_is_empty() {
    let (p, ids) = seeded();
    let [_, _, g2, d, pr, q, q2] = &ids[..] else {
        unreachable!()
    };
    let out = p.ctx_ok(&["standup"]);
    assert!(out.starts_with("# Standup of demo for omar\n"), "{out}");
    assert!(out.contains("## New decisions (1)"), "{out}");
    assert!(
        out.contains(&format!(
            "- {d} Charge with Stripe, not with Conekta (human:omar)"
        )),
        "{out}"
    );
    assert!(out.contains("## Waiting for your answer (3)"), "{out}");
    assert!(out.contains(&format!("- {g2} unsigned goal")), "{out}");
    assert!(
        // "(new)": it arrived after the last thing omar wrote.
        out.contains(&format!(
            "- {pr} [not reviewed] proposal (new) from agent:distill"
        )),
        "{out}"
    );
    assert!(
        out.contains(&format!(
            "- {q} question (new) from agent:coder: What is the monthly price?"
        )),
        "{out}"
    );
    // The question for the pm is not for you.
    assert!(!out.contains(q2.as_str()), "{out}");
    assert!(out.contains("## Progress (journal) (1)"), "{out}");
    assert!(
        out.contains(
            "Summary: 1 new decision, 2 closed items, 3 items waiting for your answer, 0 alerts"
        ),
        "{out}"
    );

    // Second time: nothing new, but what is pending stays.
    let out = p.ctx_ok(&["standup"]);
    assert!(out.contains("Nothing new since your last time ("), "{out}");
    assert!(
        out.contains("## Still waiting for your answer (3)"),
        "{out}"
    );
    assert!(!out.contains("New decisions"), "{out}");

    // Answering and accepting shows as «closed» in the next one.
    p.ctx_ok(&["answer", q, "199"]);
    p.ctx_ok(&["accept", pr]);
    let out = p.ctx_ok(&["standup"]);
    assert!(out.contains("## Closed (2)"), "{out}");
    assert!(
        out.contains(&format!("- {q} answered by human:omar")),
        "{out}"
    );
    assert!(
        out.contains(&format!("- {pr} accepted by human:omar")),
        "{out}"
    );
    assert!(out.contains("## Waiting for your answer (1)"), "{out}");
}

#[test]
fn standup_reports_conflicts() {
    let p = Project::new();
    let d = p.id_of(&[], &["decide", "Base"]);
    p.ctx_ok(&["standup"]);
    // Two machines supersede the same decision without seeing each other.
    p.ctx_ok(&["decide", "Option A", "--supersedes", &d]);
    let ana = p.machine("omar@laptop");
    let line = format!(
        "{{\"kind\":\"decision\",\"title\":\"Option B\",\"actor\":{{\"type\":\"human\",\"name\":\"omar\"}},\"fields\":{{\"supersedes\":[\"{d}\"]}}}}\n"
    );
    assert!(
        p.spawn_append(&ana, &line, 0)
            .wait_with_output()
            .unwrap()
            .status
            .success()
    );
    let out = p.ctx_ok(&["standup"]);
    assert!(out.contains("## Alerts (1)"), "{out}");
    assert!(
        out.contains(&format!("conflict: {d} is superseded at the same time")),
        "{out}"
    );
    // Conflicts keep showing until they are resolved.
    let out = p.ctx_ok(&["standup"]);
    assert!(out.contains("conflict:"), "{out}");
}

/// The text of a section up to the next `## `.
fn section<'a>(text: &'a str, heading: &str) -> &'a str {
    let start = text
        .find(heading)
        .unwrap_or_else(|| panic!("missing {heading}:\n{text}"));
    let rest = &text[start + heading.len()..];
    &rest[..rest.find("\n## ").unwrap_or(rest.len())]
}

#[test]
fn resume_layout() {
    let (p, ids) = seeded();
    let [a, g, g2, d, pr, q, q2] = &ids[..] else {
        unreachable!()
    };
    let out = p.ctx_ok(&["resume", "--as", "pm", "--peek"]);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], "# Context of demo for pm (kontext resume)");
    assert!(
        lines[1].starts_with("Generated ") && lines[1].contains("chain OK"),
        "{out}"
    );
    for want in [
        "## Charter".to_string(),
        format!("{a} [signed] Charge by card in the app"),
        "- Not doing: Invoicing".into(),
        "- Done when: First real charge".into(),
        "## Goals (2)".into(),
        format!("- {g} Recurring charge monthly"),
        format!("- {g2} [unsigned] Refunds from the panel"),
        "## Decisions in force (1)".into(),
        "### payments".into(),
        format!(
            "- {d} Charge with Stripe, not with Conekta (why: Conekta does not support recurring charges with 3DS)"
        ),
        "## Questions for pm (1)".into(),
        "## Pending proposals (1)".into(),
        format!("- {pr} [not reviewed] Save tokenized cards from agent:distill"),
        "## Journal (last 1 of 1)".into(),
        "## What's new since your last time (".into(),
    ] {
        assert!(out.contains(&want), "missing {want:?}:\n{out}");
    }
    let questions = section(&out, "## Questions for pm");
    assert!(
        questions.contains(q2.as_str()) && !questions.contains(q.as_str()),
        "{out}"
    );
    // For the owner, the questions to the owner.
    let out = p.ctx_ok(&["resume", "--as", "owner", "--peek"]);
    let questions = section(&out, "## Questions for owner (1)");
    assert!(
        questions.contains(q.as_str()) && !questions.contains(q2.as_str()),
        "{out}"
    );
    // JSON with the same sections.
    let v: serde_json::Value =
        serde_json::from_str(&p.ctx_ok(&["--json", "resume", "--as", "pm", "--peek"])).unwrap();
    assert_eq!(v["charter"]["id"], a.as_str());
    assert_eq!(v["decisions"][0]["id"], d.as_str());
    assert_eq!(v["chain_ok"], true);
}

#[test]
fn tiny_budget_never_drops_charter_goals_or_decision_titles() {
    let (p, ids) = seeded();
    for i in 0..30 {
        p.ctx_ok(&["log", &format!("journal entry number {i} with some text")]);
        p.ctx_ok(&[
            "decide",
            &format!("Decision {i}"),
            "--why",
            &"a long reason ".repeat(10),
            "--tags",
            &format!("t{}", i % 3),
        ]);
    }
    let out = p.ctx_ok(&["resume", "--as", "pm", "--max-chars", "300", "--peek"]);
    assert!(
        out.contains(&format!("{} [signed] Charge by card in the app", ids[0])),
        "{out}"
    );
    assert!(out.contains("Recurring charge monthly") && out.contains("Refunds from the panel"));
    for i in 0..30 {
        assert!(
            out.contains(&format!("Decision {i}")),
            "missing Decision {i}:\n{out}"
        );
    }
    assert!(out.contains(&format!("{} Charge with Stripe, not with Conekta", ids[3])));
    // The rest was cut: no journal, no why, lists with "… N more".
    assert!(out.contains("## Journal (last 0 of 31)"), "{out}");
    assert!(!out.contains("a long reason"), "{out}");
    assert!(out.contains("- **t0**: "), "{out}");
    assert!(out.contains("more, see `kontext standup --peek`"), "{out}");
    assert!(
        out.contains("(kontext: this is over 300 characters;"),
        "{out}"
    );

    // With a medium budget, the old journal goes first.
    let full = p.ctx_ok(&["resume", "--as", "pm", "--max-chars", "100000", "--peek"]);
    assert!(full.contains("## Journal (last 10 of 31)"), "{full}");
    let mid = p.ctx_ok(&[
        "resume",
        "--as",
        "pm",
        "--max-chars",
        &(full.chars().count() - 200).to_string(),
        "--peek",
    ]);
    assert!(mid.contains("a long reason"), "{mid}");
    assert!(!mid.contains("## Journal (last 10 of 31)"), "{mid}");
    assert!(mid.chars().count() <= full.chars().count() - 200);
}

#[test]
fn show_resolves_prefixes_and_history() {
    let (p, ids) = seeded();
    let pr = &ids[4];
    p.ctx_ok(&["accept", pr, "--as-decision", "--why", "yes"]);
    let out = p.ctx_ok(&["show", &pr.to_lowercase()[..5]]);
    assert!(
        out.starts_with(&format!("## {pr} Save tokenized cards")),
        "{out}"
    );
    assert!(
        out.contains("- **Status:** accepted, became decision D-"),
        "{out}"
    );
    assert!(out.contains("**History**"), "{out}");
    assert!(out.contains("acceptance by human:omar"), "{out}");
    let v: serde_json::Value = serde_json::from_str(&p.ctx_ok(&["--json", "show", pr])).unwrap();
    assert_eq!(v["entry"]["id"], pr.as_str());
    assert_eq!(v["history"].as_array().unwrap().len(), 2);
    let out = p.ctx(&["show", "Z-1"]);
    assert!(!out.status.success());
}

#[test]
fn recall_searches_ledger_and_transcripts() {
    let (p, ids) = seeded();
    let tr = p.dir.path().join("transcripts/proj");
    fs::create_dir_all(&tr).unwrap();
    fs::write(
        tr.join("s1.jsonl"),
        concat!(
            r#"{"type":"user","sessionId":"abc","timestamp":"2026-10-01T10:00:00Z","message":{"role":"user","content":"shall we use Conekta?<system-reminder>conekta hidden</system-reminder>"}}"#,
            "\n",
            r#"{"type":"assistant","sessionId":"abc","timestamp":"2026-10-01T10:00:05Z","message":{"role":"assistant","content":[{"type":"thinking","thinking":"conekta"},{"type":"text","text":"Conekta no tiene 3DS recurrente"}]}}"#,
            "\n",
            r#"{"type":"summary","summary":"conekta"}"#,
            "\n"
        ),
    )
    .unwrap();
    let glob = format!("{}/transcripts/*/*.jsonl", p.dir.path().display());
    let env = [("KONTEXT_TRANSCRIPTS_GLOB", glob.as_str())];
    let out = p.ctx_env_ok(&env, &["recall", "conekta"]);
    assert!(
        out.starts_with("# kontext recall «conekta»: 3 of 3"),
        "{out}"
    );
    assert!(
        out.contains(&format!("[ledger] {} decision, accepted", ids[3])),
        "{out}"
    );
    assert!(
        out.contains("[transcript] abc assistant: Conekta no tiene 3DS recurrente"),
        "{out}"
    );
    assert!(
        out.contains("[transcript] abc user: shall we use Conekta?"),
        "{out}"
    );
    assert!(!out.contains("hidden"), "{out}");
    // --since leaves out the old transcript; --limit cuts.
    let v: serde_json::Value = serde_json::from_str(&p.ctx_env_ok(
        &env,
        &["--json", "recall", "conekta", "--since", "2026-10-02"],
    ))
    .unwrap();
    assert_eq!(v["total"], 1);
    let v: serde_json::Value =
        serde_json::from_str(&p.ctx_env_ok(&env, &["--json", "recall", "conekta", "--limit", "1"]))
            .unwrap();
    assert_eq!(v["hits"].as_array().unwrap().len(), 1);
    assert_eq!(v["total"], 3);
    let out = p.ctx_env_ok(&env, &["recall", "nothingatall"]);
    assert!(out.starts_with("Nothing for «nothingatall»"), "{out}");
}
