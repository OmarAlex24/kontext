//! `kontext epic add --after` and `kontext plan` from the binary: order, parallel,
//! cycles, and dependencies on epics that don't exist.

mod common;

use common::{Project, with_epic};

fn epic(p: &Project, g: &str, name: &str, after: &[&str]) -> String {
    let title = format!("Epic {name}");
    let mut args = vec!["epic", "add", &title, "--goal", g, "--name", name];
    for a in after {
        args.extend_from_slice(&["--after", a]);
    }
    p.id_of(&[], &args)
}

fn plan_json(p: &Project) -> (Option<i32>, serde_json::Value) {
    let out = p.ctx(&["--json", "plan"]);
    (
        out.status.code(),
        serde_json::from_slice(&out.stdout).unwrap(),
    )
}

fn waves(v: &serde_json::Value) -> Vec<Vec<String>> {
    v["waves"]
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

#[test]
fn after_stores_ids_and_plan_orders_waves() {
    // `payments` already exists (from with_epic); we use it as the foundation.
    let (p, g, payments) = with_epic();
    let catalog = epic(&p, &g, "catalog", &["payments"]);
    // By id, by epic:<name>, and one that doesn't exist yet (notice).
    let out = p.ctx_ok(&[
        "epic",
        "add",
        "Notices",
        "--goal",
        &g,
        "--name",
        "notices",
        "--after",
        &payments,
        "--after",
        "epic:payments",
    ]);
    assert!(!out.contains("Warning"), "{out}");
    let out = p.ctx_ok(&[
        "epic",
        "add",
        "Integration",
        "--goal",
        &g,
        "--name",
        "integration",
        "--after",
        "catalog",
        "--after",
        "notices",
        "--after",
        "e2e",
    ]);
    assert!(out.contains("Warning: there is no epic e2e yet"), "{out}");
    let entries = p.entries("omar@test");
    let get = |id: &str| entries.iter().find(|e| e["id"] == id).unwrap().clone();
    assert_eq!(get(&catalog)["fields"]["depends_on"][0], payments.as_str());
    // `payments` has no dependencies: it doesn't store the field.
    assert!(get(&payments)["fields"].get("depends_on").is_none());

    // e2e is missing: integration has no wave and exits with 1.
    let (code, v) = plan_json(&p);
    assert_eq!(code, Some(1));
    assert_eq!(v["ok"], false);
    assert_eq!(v["missing"][0]["after"], "e2e");
    assert_eq!(v["missing"][0]["reason"], "does not exist");
    assert_eq!(v["blocked"][0]["epic"]["name"], "integration");
    assert_eq!(
        waves(&v),
        vec![vec!["payments"], vec!["catalog", "notices"]]
    );

    // e2e gets created (no dependencies): everything lines up.
    epic(&p, &g, "e2e", &[]);
    let (code, v) = plan_json(&p);
    assert_eq!(code, Some(0));
    assert_eq!(
        waves(&v),
        vec![
            vec!["payments", "e2e"],
            vec!["catalog", "notices"],
            vec!["integration"]
        ]
    );
    assert_eq!(v["waves"][0]["parallel"], true);
    assert_eq!(v["waves"][2]["parallel"], false);
    let text = p.ctx_ok(&["plan"]);
    assert!(
        text.starts_with("# Wave plan: 5 open epics, 0 closed"),
        "{text}"
    );
    assert!(
        text.contains("## Wave 2 (in parallel: 2 epics, one coordinator each)"),
        "{text}"
    );
    assert!(text.contains("## Wave 3 (in series)"), "{text}");
    assert!(
        text.contains("integration: Integration (no tasks; after catalog, notices, e2e)"),
        "{text}"
    );

    // Closing all of payments's tasks takes it out of the plan: catalog moves to wave 1.
    let t = p.id_of(
        &[],
        &[
            "task",
            "add",
            "SDK",
            "--epic",
            "payments",
            "--acceptance",
            "true",
        ],
    );
    p.ctx_ok(&["done", &t, "--run", "true"]);
    let (_, v) = plan_json(&p);
    assert_eq!(
        waves(&v),
        vec![vec!["catalog", "notices", "e2e"], vec!["integration"]]
    );
    assert_eq!(v["closed"][0]["name"], "payments");
}

#[test]
fn cycles_are_refused_on_write_and_reported_if_they_arrive() {
    let (p, g, _) = with_epic();
    // a waits for b (which doesn't exist yet); b after c, and c after a: a cycle.
    epic(&p, &g, "a", &["b"]);
    epic(&p, &g, "c", &["a"]);
    let out = p.ctx(&[
        "epic", "add", "B", "--goal", &g, "--name", "b", "--after", "c",
    ]);
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("would close a cycle (a → b → c → a)"), "{err}");

    // The same cycle, arrived from another machine or version: plan reports it.
    p.append(&format!(
        "{{\"kind\":\"epic\",\"title\":\"B\",\"actor\":{{\"type\":\"human\",\"name\":\"omar\"}},\"fields\":{{\"name\":\"b\",\"goals\":[\"{g}\"],\"depends_on\":[\"c\"]}}}}\n"
    ));
    epic(&p, &g, "d", &["b"]);
    let (code, v) = plan_json(&p);
    assert_eq!(code, Some(1));
    assert_eq!(v["cycles"], serde_json::json!([["a", "b", "c"]]));
    assert_eq!(waves(&v), vec![vec!["payments"]]);
    let out = p.ctx(&["plan"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("- Cycle: a → b → c → a"), "{text}");
    assert!(text.contains("waits for b, which has no wave"), "{text}");
    assert!(text.contains("kontext ask --to owner"), "{text}");
}

#[test]
fn after_refuses_itself_and_unknown_ids() {
    let (p, g, _) = with_epic();
    let out = p.ctx(&[
        "epic", "add", "Yo", "--goal", &g, "--name", "yo", "--after", "yo",
    ]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("cannot come after itself"));
    let out = p.ctx(&[
        "epic", "add", "X", "--goal", &g, "--name", "x", "--after", "E-abc123",
    ]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("there is no epic with that id"));
}

#[test]
fn plan_without_epics() {
    let p = Project::new();
    let out = p.ctx_ok(&["plan"]);
    assert!(out.contains("There are no epics"), "{out}");
}
