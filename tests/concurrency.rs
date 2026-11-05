//! Many processes writing at once (DESIGN §7.1, §7.2 and §16).

mod common;

use std::collections::HashSet;
use std::time::Instant;

use common::{Project, soon};

fn lines(proc: usize, count: usize) -> String {
    (0..count)
        .map(|i| {
            format!(
                "{{\"kind\":\"journal\",\"title\":\"proceso {proc} entrada {i}\",\"tags\":[\"p{proc}\"]}}\n"
            )
        })
        .collect()
}

fn wait_all(children: Vec<std::process::Child>) {
    for c in children {
        let out = c.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "a process failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// Checks a file: `n` from 1 to `expected` with no gaps and unique ids.
fn check_file(p: &Project, writer: &str, expected: usize) -> HashSet<String> {
    let entries = p.entries(writer);
    assert_eq!(entries.len(), expected);
    let ns: Vec<u64> = entries.iter().map(|e| e["n"].as_u64().unwrap()).collect();
    assert_eq!(ns, (1..=expected as u64).collect::<Vec<_>>());
    let ids: HashSet<String> = entries
        .iter()
        .map(|e| e["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids.len(), expected, "ids repetidos");
    ids
}

#[test]
fn thirty_two_processes_same_writer() {
    const PROCS: usize = 32;
    const EACH: usize = 50;
    let p = Project::new();
    let start = soon(1500);
    let children: Vec<_> = (0..PROCS)
        .map(|i| p.spawn_append(&p.config, &lines(i, EACH), start))
        .collect();
    wait_all(children);
    let elapsed = jiff::Timestamp::now().as_millisecond() - start;
    eprintln!("{PROCS} processes x {EACH} entries, same writer: {elapsed} ms");

    check_file(&p, "omar@test", PROCS * EACH);
    let out = p.kontext_ok(&["verify"]);
    assert_eq!(out.trim(), "OK: 1600 entries in 1 writer");
    let idx = kontext::index::Index::open(&p.home).unwrap();
    assert_eq!(idx.count().unwrap(), 1600);
    // The order of each process is kept within the file.
    let titles: Vec<String> = p
        .entries("omar@test")
        .iter()
        .map(|e| e["title"].as_str().unwrap().to_string())
        .collect();
    for i in 0..PROCS {
        let mine: Vec<&String> = titles
            .iter()
            .filter(|t| t.starts_with(&format!("proceso {i} ")))
            .collect();
        let want: Vec<String> = (0..EACH)
            .map(|j| format!("proceso {i} entrada {j}"))
            .collect();
        assert_eq!(mine, want.iter().collect::<Vec<_>>());
    }
    // And the hlcs of one writer only grow.
    let hlcs: Vec<String> = p
        .entries("omar@test")
        .iter()
        .map(|e| e["hlc"].as_str().unwrap().to_string())
        .collect();
    assert!(hlcs.windows(2).all(|w| w[0] < w[1]), "hlc not increasing");
}

#[test]
fn two_writers_at_once() {
    const PROCS: usize = 16;
    const EACH: usize = 50;
    let p = Project::new();
    let a = p.machine("omar@mac");
    let b = p.machine("ana@laptop");
    let start = soon(1500);
    let t = Instant::now();
    let children: Vec<_> = (0..PROCS * 2)
        .map(|i| p.spawn_append(if i % 2 == 0 { &a } else { &b }, &lines(i, EACH), start))
        .collect();
    wait_all(children);
    eprintln!(
        "2 writers x {PROCS} processes x {EACH} entries: {:?}",
        t.elapsed()
    );

    let ids_a = check_file(&p, "omar@mac", PROCS * EACH);
    let ids_b = check_file(&p, "ana@laptop", PROCS * EACH);
    assert!(ids_a.is_disjoint(&ids_b));
    let out = p.kontext_ok(&["verify"]);
    assert_eq!(out.trim(), "OK: 1600 entries in 2 writers");
    let idx = kontext::index::Index::open(&p.home).unwrap();
    assert_eq!(idx.count().unwrap(), 1600);
}

#[test]
fn only_one_claim_wins() {
    const PROCS: usize = 16;
    let p = Project::new();
    let task = p
        .append("{\"kind\":\"task\",\"title\":\"Charge with Stripe\"}\n")
        .remove(0);
    assert!(task.starts_with("T-"));
    let start = soon(1000);
    let children: Vec<_> = (0..PROCS)
        .map(|_| {
            p.cmd(&["debug", "claim", &task, "--start-at", &start.to_string()])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    let mut won = 0;
    let mut refused = 0;
    for c in children {
        let out = c.wait_with_output().unwrap();
        match out.status.code() {
            Some(0) => won += 1,
            Some(2) => {
                refused += 1;
                assert!(String::from_utf8_lossy(&out.stderr).contains("is already held by"));
            }
            other => panic!(
                "unexpected exit {other:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            ),
        }
    }
    assert_eq!((won, refused), (1, PROCS - 1));
    let entries = p.entries("omar@test");
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[1]["kind"], "claim");
    assert_eq!(entries[1]["refs"][0], task.as_str());
    let state: serde_json::Value =
        serde_json::from_str(&p.kontext_ok(&["debug", "state"])).unwrap();
    assert_eq!(state["status"][&task], "claimed");
    p.kontext_ok(&["verify"]);
}

#[test]
fn sixteen_agents_next_claim_five_tasks() {
    const PROCS: usize = 16;
    const TASKS: usize = 5;
    let (p, _, _) = common::with_epic();
    let tasks: HashSet<String> = (0..TASKS)
        .map(|i| {
            p.id_of(
                &[],
                &["task", "add", &format!("Task {i}"), "--epic", "payments"],
            )
        })
        .collect();
    let children: Vec<_> = (0..PROCS)
        .map(|i| {
            p.cmd(&["--json", "next", "--epic", "payments", "--claim"])
                .env("KONTEXT_ACTOR", format!("agent:impl-{i}"))
                .env("KONTEXT_ROLE", "implementer")
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    let mut won = HashSet::new();
    let mut refused = 0;
    for c in children {
        let out = c.wait_with_output().unwrap();
        match out.status.code() {
            Some(0) => {
                let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
                assert!(v["claimed"].is_string(), "{v}");
                assert!(won.insert(v["task"]["id"].as_str().unwrap().to_string()));
            }
            Some(2) => {
                refused += 1;
                let err = String::from_utf8_lossy(&out.stderr);
                assert!(err.contains("No ready tasks"), "{err}");
            }
            other => panic!(
                "unexpected exit {other:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            ),
        }
    }
    assert_eq!((won.len(), refused), (TASKS, PROCS - TASKS));
    assert_eq!(won, tasks);
    // One claim per task, each from a different agent, and all of them count.
    let claims: Vec<serde_json::Value> = p
        .entries("omar@test")
        .into_iter()
        .filter(|e| e["kind"] == "claim")
        .collect();
    assert_eq!(claims.len(), TASKS);
    let targets: HashSet<&str> = claims
        .iter()
        .map(|c| c["refs"][0].as_str().unwrap())
        .collect();
    let actors: HashSet<&str> = claims
        .iter()
        .map(|c| c["actor"]["name"].as_str().unwrap())
        .collect();
    assert_eq!((targets.len(), actors.len()), (TASKS, TASKS));
    let state = p.state();
    for t in &tasks {
        assert_eq!(state["tasks"][t], "claimed");
    }
    p.kontext_ok(&["verify"]);
}
