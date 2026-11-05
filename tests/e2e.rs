//! The phase 2 flow end to end, from the binary.

mod common;

use common::Project;

#[test]
fn charter_to_standup() {
    let p = Project::new();
    let owner = [("KONTEXT_ACTOR", "human:omar")];
    let agent = [
        ("KONTEXT_ACTOR", "agent:coder"),
        ("KONTEXT_ROLE", "implementer"),
    ];

    let out = p.kontext_env_ok(
        &owner,
        &[
            "charter",
            "--objective",
            "Charge by card",
            "--non-goal",
            "Invoicing",
            "--done-when",
            "First real charge",
        ],
    );
    let a = out.split(':').next().unwrap().to_string();
    assert!(a.starts_with("A-"), "{out}");
    assert!(out.contains(&format!("kontext sign {a}")), "{out}");
    let g = p.id_of(&owner, &["goal", "add", "Recurring charge"]);
    p.kontext_env_ok(&owner, &["sign", &a]);
    p.kontext_env_ok(&owner, &["sign", &g]);
    let d = p.id_of(
        &owner,
        &[
            "decide",
            "Use Stripe",
            "--why",
            "recurring 3DS",
            "--tags",
            "payments",
        ],
    );
    // The agent cannot decide, but it can propose.
    let out = p.kontext_env(&agent, &["decide", "Use Conekta"]);
    assert_eq!(out.status.code(), Some(2));
    let pr = p.id_of(
        &agent,
        &[
            "propose",
            "Use Conekta",
            "--why",
            "cheaper",
            "--supersedes",
            &d,
        ],
    );
    p.kontext_env_ok(
        &agent,
        &["log", "Tried both SDKs", "--next", "wait for a decision"],
    );

    let out = p.kontext_env_ok(&owner, &["standup"]);
    assert!(
        out.contains(&format!("- {d} Use Stripe (human:omar)")),
        "{out}"
    );
    assert!(out.contains("## Waiting for your answer (1)"), "{out}");
    // "New": it arrived after the last thing omar wrote (his last
    // action), even if it is his first standup.
    assert!(
        out.contains(&format!(
            "- {pr} proposal (new) from agent:coder (implementer): Use Conekta"
        )),
        "{out}"
    );
    assert!(out.contains("## Closed (2)"), "{out}");

    // Second time: nothing new in the ledger, but the proposal still waits
    // and is still "new": seeing it does not count, only acting does.
    let out = p.kontext_env_ok(&owner, &["standup"]);
    assert!(out.contains("Nothing new since your last time"), "{out}");
    assert!(
        out.contains(&format!("- {pr} proposal (new) from agent:coder")),
        "{out}"
    );
    // Phase 5: the proposal would supersede a decision in force; it shows as an alert
    // while it stays pending.
    assert!(
        out.contains(&format!(
            "- {pr} proposes to supersede {d}, which is in force"
        )),
        "{out}"
    );
    assert!(out.contains("Summary: 0 new decisions, 0 closed items, 1 item waiting for your answer, 1 alert (0 new entries)."), "{out}");

    // The pm sees everything in its resume; the chain is still fine.
    let out = p.kontext_ok(&["resume", "--as", "pm"]);
    assert!(
        out.contains(&format!("{a} [signed] Charge by card")),
        "{out}"
    );
    assert!(
        out.contains(&format!("- {d} Use Stripe (why: recurring 3DS)")),
        "{out}"
    );
    assert!(
        out.contains(&format!(
            "- {pr} Use Conekta from agent:coder (implementer) (why: cheaper)"
        )),
        "{out}"
    );
    assert!(p.kontext_ok(&["verify"]).starts_with("OK: 7 entries"));
}
