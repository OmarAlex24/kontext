//! `kontext tree`: waves, epics, their features and the ideas not placed, on
//! one screen, without writing anything.

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use common::{Project, git, with_epic};
use serde_json::Value;

fn tree_in(p: &Project, dir: &Path, json: bool) -> String {
    let args: &[&str] = if json { &["--json", "tree"] } else { &["tree"] };
    let out = p.cmd(args).current_dir(dir).output().unwrap();
    assert!(
        out.status.success(),
        "kontext tree failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn close(p: &Project, t: &str) {
    p.kontext_ok(&["done", t, "--evidence", "cmd:\"cargo test\"=0"]);
}

/// Every file under `dir`, with its bytes.
fn snapshot(dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
    for e in fs::read_dir(dir).unwrap().flatten() {
        let path = e.path();
        if path.is_dir() {
            snapshot(&path, out);
        } else {
            out.insert(path.display().to_string(), fs::read(&path).unwrap());
        }
    }
}

/// payments (wave 1) with three features, catalog after it (wave 2, tasks
/// only) and integration after an epic that does not exist (no wave).
fn project() -> (Project, [String; 3], String) {
    let (p, g, _) = with_epic();
    let feature = |title: &str| {
        p.id_of(
            &[],
            &[
                "feature",
                "add",
                title,
                "--epic",
                "payments",
                "--acceptance",
                "it works",
            ],
        )
    };
    let u1 = feature("Card charge");
    let u2 = feature("Refunds");
    let u3 = feature("Invoices");
    let task = |title: &str, u: &str| p.id_of(&[], &["task", "add", title, "--feature", u]);
    let t1 = task("Stripe client", &u1);
    let t2 = task("Refund endpoint", &u2);
    close(&p, &t1);
    p.kontext_ok(&["done", &u1, "--evidence", "url:https://example.com/charge"]);
    close(&p, &t2);
    p.kontext_ok(&["ask", "PDF or HTML?", "--to", "owner", "--feature", &u3]);
    p.kontext_ok(&[
        "epic", "add", "Catalog", "--goal", &g, "--name", "catalog", "--after", "payments",
    ]);
    p.kontext_ok(&["task", "add", "List products", "--epic", "catalog"]);
    p.kontext_ok(&[
        "epic",
        "add",
        "Integration",
        "--goal",
        &g,
        "--name",
        "integration",
        "--after",
        "e2e",
    ]);
    (p, [u1, u2, u3], t2)
}

#[test]
fn the_tree_has_waves_epics_and_features() {
    let (p, [u1, u2, u3], _) = project();
    let dir = p.dir.path().join("elsewhere");
    fs::create_dir_all(&dir).unwrap();
    let out = tree_in(&p, &dir, false);
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines[0].starts_with("demo "), "{out}");
    assert!(lines[0].ends_with("1/3 features · wave 1 of 2"), "{out}");
    assert_eq!(lines[1], "├─ Wave 1", "{out}");
    assert!(lines[2].starts_with("│  └─ ◐ Card payments"), "{out}");
    assert!(lines[2].trim_end().ends_with("1/3"), "{out}");
    assert!(lines[3].starts_with("│      ├─ ✓ Card charge"), "{out}");
    assert!(lines[4].starts_with("│      ├─ ◐ Refunds"), "{out}");
    assert!(lines[4].ends_with("to validate"), "{out}");
    assert!(lines[5].starts_with("│      └─ ○ Invoices"), "{out}");
    assert!(lines[5].ends_with("waiting for you"), "{out}");
    assert_eq!(lines[6], "├─ Wave 2", "{out}");
    assert!(lines[7].starts_with("│  └─ ○ Catalog"), "{out}");
    assert!(lines[7].ends_with("0/1 tasks"), "{out}");
    assert_eq!(lines[8], "└─ No wave", "{out}");
    assert!(lines[9].starts_with("   └─ ○ Integration"), "{out}");
    assert!(
        lines[9].ends_with("0/0 tasks · depends on \"e2e\", which does not exist"),
        "{out}"
    );
    assert_eq!(lines.len(), 10, "no tasks are listed and no ideas: {out}");
    // The right column is aligned.
    let col =
        |l: &str| l.chars().count() - l.split("  ").last().unwrap().trim_start().chars().count();
    assert_eq!(col(lines[2]), col(lines[4]), "{out}");

    let v: Value = serde_json::from_str(&tree_in(&p, &dir, true)).unwrap();
    assert_eq!(v["features"]["done"], 1);
    assert_eq!(v["features"]["total"], 3);
    assert_eq!(v["current_wave"], 1);
    let pay = &v["waves"][0]["epics"][0];
    assert_eq!(pay["name"], "payments");
    assert_eq!(pay["progress"], "in_progress");
    let fl = pay["feature_list"].as_array().unwrap();
    assert_eq!(fl[0]["id"], u1.as_str());
    assert_eq!(fl[0]["status"], "done");
    assert_eq!(fl[1]["id"], u2.as_str());
    assert_eq!(fl[1]["status"], "to validate");
    assert_eq!(fl[2]["id"], u3.as_str());
    assert_eq!(fl[2]["waiting_for_you"], true);
    assert_eq!(fl[2]["acceptance"], "it works");
    assert!(v["waves"][1]["epics"][0].get("features").is_none());
    assert_eq!(v["no_wave"][0]["name"], "integration");
    assert!(v["no_wave"][0]["reason"].as_str().unwrap().contains("e2e"));
    assert!(v["not_placed"].is_null());
}

#[test]
fn a_closed_epic_takes_one_line_and_its_wave_stays() {
    let (p, [_, u2, u3], _) = project();
    p.kontext_ok(&["done", &u2, "--evidence", "url:https://example.com/refund"]);
    let t3 = p.id_of(&[], &["task", "add", "PDF invoice", "--feature", &u3]);
    close(&p, &t3);
    p.kontext_ok(&["done", &u3, "--evidence", "url:https://example.com/pdf"]);
    let dir = p.dir.path().join("elsewhere");
    fs::create_dir_all(&dir).unwrap();
    let out = tree_in(&p, &dir, false);
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines[0].ends_with("3/3 features · wave 2 of 2"), "{out}");
    assert!(lines[2].starts_with("│  └─ ✓ Card payments"), "{out}");
    assert!(lines[2].ends_with("3/3"), "{out}");
    assert_eq!(lines[3], "├─ Wave 2", "the closed epic is one line: {out}");
}

#[test]
fn a_question_about_a_task_of_the_feature_waits_for_you_too() {
    let (p, [_, u2, _], t2) = project();
    // Reopen the task so the question can cite it.
    p.kontext_ok(&["reopen", &t2, "--why", "edge case"]);
    p.kontext_ok(&["ask", "Partial refunds?", "--to", "owner", "--task", &t2]);
    let dir = p.dir.path().join("elsewhere");
    fs::create_dir_all(&dir).unwrap();
    let v: Value = serde_json::from_str(&tree_in(&p, &dir, true)).unwrap();
    let f = &v["waves"][0]["epics"][0]["feature_list"][1];
    assert_eq!(f["id"], u2.as_str());
    assert_eq!(f["waiting_for_you"], true);
    // A question to an agent does not.
    let (p, [u1, ..], _) = project();
    p.kontext_ok(&["ask", "Which SDK?", "--to", "coder", "--feature", &u1]);
    let v: Value = serde_json::from_str(&tree_in(&p, &dir, true)).unwrap();
    assert_eq!(
        v["waves"][0]["epics"][0]["feature_list"][0]["waiting_for_you"],
        false
    );
}

#[test]
fn not_placed_comes_from_items_md_and_nothing_is_written() {
    let (p, [u1, ..], _) = project();
    let repo = p.dir.path().join("app");
    fs::create_dir_all(repo.join(".pm/dumps")).unwrap();
    git(&repo, &["init", "-q"]);
    // Without items.md: no section.
    let out = tree_in(&p, &repo, false);
    assert!(!out.contains("Not placed"), "{out}");
    fs::write(
        repo.join(".pm/dumps/2026-11-12.md"),
        "## 2026-11-12 10:00\n**User:**\nque el recibo salga en PDF\n\n## 2026-11-12 11:00\n**User:**\nlímite de pedidos\n",
    )
    .unwrap();
    fs::write(
        repo.join(".pm/items.md"),
        format!(
            "| id | idea | source | state | where | evidence |\n|---|---|---|---|---|---|\n\
             | I-1 | receipt | dumps/2026-11-12.md:3 | pending | | |\n\
             | I-2 | charge | dumps/2026-11-12.md:3 | pending | {u1} | |\n\
             | I-3 | limit | dumps/2026-11-12.md:7 | pending | payments epic, see below | |\n"
        ),
    )
    .unwrap();
    let mut before = BTreeMap::new();
    snapshot(p.dir.path(), &mut before);
    let out = tree_in(&p, &repo, false);
    let json = tree_in(&p, &repo, true);
    let mut after = BTreeMap::new();
    snapshot(p.dir.path(), &mut after);
    assert!(before == after, "kontext tree wrote or changed a file");

    let tail: Vec<&str> = out.lines().rev().take(3).collect();
    assert_eq!(
        tail[2], "└─ Not placed (2) · 2 of your messages never audited",
        "{out}"
    );
    assert!(
        tail[1].starts_with("    ├─ \"que el recibo salga en PDF\""),
        "{out}"
    );
    assert!(tail[1].ends_with("Nov 12"), "{out}");
    assert!(tail[0].starts_with("    └─ \"límite de pedidos\""), "{out}");
    assert!(tail[0].ends_with("(payments epic, see below)"), "{out}");
    // The waves above now draw as not last.
    assert!(out.contains("├─ No wave"), "{out}");
    let v: Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["not_placed"]["count"], 2);
    assert_eq!(v["not_placed"]["ideas"][0]["id"], "I-1");
    assert_eq!(v["not_placed"]["unaudited"]["count"], 2);
}

#[test]
fn a_project_without_features_counts_epics() {
    let (p, _, _) = with_epic();
    let t = p.id_of(&[], &["task", "add", "Charge", "--epic", "payments"]);
    let dir = p.dir.path().join("elsewhere");
    fs::create_dir_all(&dir).unwrap();
    let out = tree_in(&p, &dir, false);
    assert!(
        out.lines()
            .next()
            .unwrap()
            .ends_with("0/1 epics closed · wave 1 of 1"),
        "{out}"
    );
    close(&p, &t);
    let out = tree_in(&p, &dir, false);
    assert!(
        out.lines()
            .next()
            .unwrap()
            .ends_with("1/1 epics closed · 1 wave, all closed"),
        "{out}"
    );
    assert!(out.contains("└─ ✓ Card payments"), "{out}");
    assert!(out.contains("1/1 tasks"), "{out}");
}
