//! `kontext changelog`: tasks closed per epic with their evidence, PRs from a
//! fake `gh` grouped by type, and the same output every time.

mod common;

use std::fs;

use common::with_epic;
use serde_json::{Value, json};

#[test]
fn tasks_and_prs_grouped_and_deterministic() {
    let (p, _g, _e) = with_epic();
    p.add_repo_toml("[[repos]]\nname = \"billing\"\nremote = \"git@github.com:acme/billing.git\"\n\n[[repos]]\nname = \"local\"\n");
    let coord = [
        ("KONTEXT_ACTOR", "agent:epic:payments"),
        ("KONTEXT_ROLE", "coordinator"),
    ];
    let t1 = p.id_of(
        &coord,
        &["task", "add", "Integrate Stripe", "--epic", "payments"],
    );
    let t2 = p.id_of(&coord, &["task", "add", "Webhooks", "--epic", "payments"]);
    let t3 = p.id_of(&coord, &["task", "add", "Open", "--epic", "payments"]);
    let old = [("KONTEXT_NOW", "2027-01-01T10:00:00Z")];
    p.ctx_env_ok(&old, &["done", &t2, "--evidence", "cmd:\"cargo test\"=0"]);
    let now = [("KONTEXT_NOW", "2027-02-02T10:00:00Z")];
    p.ctx_env_ok(
        &now,
        &[
            "done",
            &t1,
            "--evidence",
            "cmd:\"cargo test\"=0",
            "--evidence",
            "pr:https://github.com/acme/billing/pull/7",
        ],
    );

    // The fake gh saves its arguments and answers with fixed PRs.
    let gh = p.dir.path().join("gh.sh");
    let args = p.dir.path().join("gh-args.txt");
    let prs = json!([
        {"number": 9, "title": "fix(api): webhook retries", "url": "https://github.com/acme/billing/pull/9", "mergedAt": "2027-02-02T12:00:00Z", "author": {"login": "ana"}},
        {"number": 7, "title": "feat: charge with Stripe", "url": "https://github.com/acme/billing/pull/7", "mergedAt": "2027-02-01T12:00:00Z", "author": {"login": "omar"}},
        {"number": 8, "title": "Actualiza dependencias", "url": "https://github.com/acme/billing/pull/8", "mergedAt": "2027-02-01T13:00:00Z", "author": {"login": "bot"}},
        {"number": 3, "title": "feat: old", "url": "https://github.com/acme/billing/pull/3", "mergedAt": "2026-09-01T12:00:00Z", "author": {"login": "omar"}}
    ]);
    fs::write(p.dir.path().join("prs.json"), prs.to_string()).unwrap();
    fs::write(
        &gh,
        format!(
            "#!/bin/sh\necho \"$@\" > {}\ncat {}\n",
            args.display(),
            p.dir.path().join("prs.json").display()
        ),
    )
    .unwrap();
    let env = [("KONTEXT_GH_BIN", gh.to_str().unwrap())];
    // KONTEXT_GH_BIN is a binary, not a shell command: mark it executable.
    std::process::Command::new("chmod")
        .args(["+x", gh.to_str().unwrap()])
        .status()
        .unwrap();
    let md = p.ctx_env_ok(&env, &["changelog", "--since", "2027-02-01"]);
    let again = p.ctx_env_ok(&env, &["changelog", "--since", "2027-02-01"]);
    assert_eq!(md, again, "the same input gives the same bytes");
    let gh_args = fs::read_to_string(&args).unwrap();
    assert!(
        gh_args.contains("pr list --repo acme/billing --state merged --search merged:>=2027-02-01 --json number,title,url,mergedAt,author"),
        "{gh_args}"
    );

    assert!(
        md.starts_with("# Technical changelog of demo\n\nSince 2027-02-01.\n"),
        "{md}"
    );
    assert!(md.contains("## Closed tasks (1)"), "{md}");
    assert!(md.contains("### Card payments ("), "{md}");
    let pr_line = md
        .find("  - **PR** https://github.com/acme/billing/pull/7")
        .unwrap();
    let cmd_line = md.find("  - command `cargo test` (exited with 0)").unwrap();
    assert!(pr_line < cmd_line, "the PR comes first: {md}");
    assert!(
        md.contains(&format!("- {t1} Integrate Stripe (closed 2027-02-02 10:00")),
        "{md}"
    );
    assert!(!md.contains(&t2) && !md.contains(&t3), "{md}");
    // PRs in the range, by type: feat, fix, then the ones with no type.
    assert!(md.contains("## Merged PRs (3)"), "{md}");
    let feat = md
        .find("#### feat\n- #7 charge with Stripe (2027-02-01, omar)")
        .unwrap();
    let fix = md
        .find("#### fix\n- #9 webhook retries (2027-02-02, ana)")
        .unwrap();
    let otros = md
        .find("#### other\n- #8 Actualiza dependencias (2027-02-01, bot)")
        .unwrap();
    assert!(feat < fix && fix < otros, "{md}");
    assert!(!md.contains("#3 "), "{md}");

    // JSON is stable too. Without --since, all tasks come in.
    let j1 = p.ctx_env_ok(&env, &["changelog", "--format", "json"]);
    let j2 = p.ctx_env_ok(&env, &["--json", "changelog"]);
    assert_eq!(j1, j2);
    let v: Value = serde_json::from_str(&j1).unwrap();
    let tasks: Vec<&str> = v["epics"][0]["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_str().unwrap())
        .collect();
    assert_eq!(tasks, vec![t2.as_str(), t1.as_str()]);
    assert_eq!(
        v["epics"][0]["tasks"][1]["prs"][0],
        "https://github.com/acme/billing/pull/7"
    );
    assert_eq!(v["repos"][0]["groups"][0]["kind"], "feat");
    assert_eq!(
        v["repos"][0]["groups"][0]["prs"].as_array().unwrap().len(),
        2
    );

    // --until with a date includes the whole day.
    let md = p.ctx_env_ok(
        &env,
        &[
            "changelog",
            "--since",
            "2027-01-01",
            "--until",
            "2027-01-01",
        ],
    );
    assert!(md.contains(&t2) && !md.contains(&t1), "{md}");
    assert!(md.contains("From 2027-01-01 to 2027-01-01."), "{md}");

    // No gh: one note and only the tasks.
    let md = p.ctx_env_ok(&[("KONTEXT_GH_BIN", "/no/existe/gh")], &["changelog"]);
    assert!(md.contains("## Closed tasks (2)"), "{md}");
    assert!(!md.contains("## Merged PRs"), "{md}");
    assert!(
        md.contains("Could not read the PRs of billing (acme/billing)"),
        "{md}"
    );
}

#[test]
fn git_ref_as_since() {
    let p = common::Project::new();
    let repo = p.code_repo("app");
    common::git(&repo, &["tag", "v1"]);
    let out = p.ctx_in_ok(&repo, &[], &["changelog", "--since", "v1"]);
    assert!(out.contains("Since v1."), "{out}");
    assert!(out.contains("None in the range."), "{out}");
    let bad = p.ctx_in(&repo, &[], &["changelog", "--since", "no-existe"]);
    assert_eq!(bad.status.code(), Some(1));
}
