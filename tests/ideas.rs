//! `kontext ideas`: the user's ideas in `.pm/items.md` with their state from
//! the ledger.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{Project, git, implementer, reader_result, with_epic};
use serde_json::Value;

const HEADER: &str =
    "| id | idea | source | state | where | evidence |\n|---|---|---|---|---|---|\n";

/// A git repo `<tmp>/<name>` with `.pm/items.md` (these rows) and one dump.
fn pm_repo(p: &Project, name: &str, rows: &str) -> PathBuf {
    let repo = p.dir.path().join(name);
    fs::create_dir_all(repo.join(".pm/dumps")).unwrap();
    git(&repo, &["init", "-q"]);
    write_items(&repo, rows);
    fs::write(
        repo.join(".pm/dumps/2026-11-07.md"),
        "## 2026-11-07 10:42\n\n**User:**\n\nlet customers export their invoices as CSV\n",
    )
    .unwrap();
    repo
}

fn write_items(repo: &Path, rows: &str) {
    fs::write(
        repo.join(".pm/items.md"),
        format!("# Ideas\n\n{HEADER}{rows}"),
    )
    .unwrap();
}

fn ideas_in(p: &Project, dir: &Path, args: &[&str]) -> String {
    let out = p.cmd(args).current_dir(dir).output().unwrap();
    assert!(
        out.status.success(),
        "kontext {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn json_in(p: &Project, dir: &Path) -> Value {
    serde_json::from_str(&ideas_in(p, dir, &["--json", "ideas"])).unwrap()
}

/// id → state shown.
fn states(v: &Value) -> Vec<(String, String)> {
    v["ideas"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["id"].as_str().unwrap().to_string(),
                i["state"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn state_of(v: &Value, id: &str) -> String {
    states(v)
        .into_iter()
        .find(|(i, _)| i == id)
        .map(|(_, s)| s)
        .unwrap_or_else(|| panic!("no idea {id} in {v}"))
}

#[test]
fn without_pm_it_says_so_and_succeeds() {
    let p = Project::new();
    let repo = p.dir.path().join("bare");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    let out = ideas_in(&p, &repo, &["ideas"]);
    assert_eq!(out.lines().count(), 1, "{out}");
    assert!(out.contains("no .pm/items.md"), "{out}");
    assert!(out.contains("kontext-pm"), "{out}");
    let v = json_in(&p, &repo);
    assert_eq!(v["total"], 0);
    assert_eq!(v["counts"].as_array().unwrap().len(), 7);
    assert_eq!(v["ideas"].as_array().unwrap().len(), 0);
}

#[test]
fn json_has_the_idea_its_source_and_its_references() {
    let p = Project::new();
    let q = p.id_of(&[], &["ask", "Which CSV columns?", "--to", "owner"]);
    let d = p.id_of(&[], &["decide", "Invoices export as CSV", "--why", "asked"]);
    let repo = pm_repo(
        &p,
        "app",
        &format!(
            "| P1 | CSV export | dumps/2026-11-07.md:5 | pending | {q} | |\n\
             | P2 | CSV only | dumps/2026-11-07.md:5 | decided | {d}; D-9999 | PR #12, shots/csv.png |\n\
             | P3 | dropped | dumps/2026-11-07.md:1 | discarded | | dumps/2026-11-07.md:5 |\n"
        ),
    );
    let v = json_in(&p, &repo);
    assert_eq!(v["project"], "demo");
    assert_eq!(v["total"], 3);
    let counts: Vec<(String, u64)> = v["counts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["state"].as_str().unwrap().to_string(),
                c["count"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(counts[1], ("waiting for you".to_string(), 1));
    assert_eq!(counts[4], ("decided".to_string(), 1));
    assert_eq!(counts[6], ("discarded".to_string(), 1));

    let p1 = &v["ideas"][0];
    assert_eq!(p1["id"], "P1");
    assert_eq!(p1["said"], "2026-11-07");
    assert_eq!(p1["source"]["file"], "dumps/2026-11-07.md");
    assert_eq!(p1["source"]["line"], 5);
    assert_eq!(p1["quote"], "let customers export their invoices as CSV");
    assert_eq!(p1["state"], "waiting for you");
    assert_eq!(p1["written_state"], "pending");
    assert_eq!(p1["state_from"], "ledger");
    assert_eq!(p1["refs"][0]["id"], q.as_str());
    assert_eq!(p1["refs"][0]["kind"], "question");
    assert_eq!(p1["refs"][0]["status"], "open");
    assert_eq!(p1["refs"][0]["title"], "Which CSV columns?");

    let p2 = &v["ideas"][1];
    assert_eq!(p2["state"], "decided");
    assert_eq!(p2["state_from"], "table");
    assert_eq!(p2["where"], format!("{d}; D-9999"));
    assert_eq!(p2["evidence"][0]["kind"], "text");
    assert_eq!(p2["evidence"][0]["value"], "PR #12");
    assert_eq!(p2["evidence"][1]["kind"], "image");
    assert_eq!(p2["evidence"][1]["value"], "shots/csv.png");
    assert_eq!(p2["notes"][0], "D-9999 is not in the ledger");

    let out = ideas_in(&p, &repo, &["ideas"]);
    assert!(
        out.contains("3 ideas · waiting for you 1 · decided 1 · discarded 1."),
        "{out}"
    );
    assert!(out.contains("- P1 · 2026-11-07 · CSV export"), "{out}");
    assert!(
        out.contains("evidence: PR #12, shots/csv.png (image)"),
        "{out}"
    );
    assert!(out.contains("note: D-9999 is not in the ledger"), "{out}");
}

#[test]
fn tasks_and_epics_decide_the_state() {
    let (p, g, e) = with_epic();
    let t1 = p.id_of(
        &[],
        &["task", "add", "Export endpoint", "--epic", "payments"],
    );
    let _t2 = p.id_of(&[], &["task", "add", "Export button", "--epic", "payments"]);
    let empty = p.id_of(
        &[],
        &["epic", "add", "Reports", "--goal", &g, "--name", "reports"],
    );
    // The written state says otherwise: the ledger wins.
    let repo = pm_repo(
        &p,
        "app",
        &format!(
            "| P1 | endpoint | | done | {t1} | PR #1 |\n\
             | P2 | export | | decided | {e} | |\n\
             | P3 | reports | | in progress | {empty} | |\n"
        ),
    );
    let v = json_in(&p, &repo);
    assert_eq!(state_of(&v, "P1"), "pending");
    assert_eq!(state_of(&v, "P2"), "pending");
    assert_eq!(state_of(&v, "P3"), "pending", "an epic with no tasks");
    assert_eq!(v["ideas"][0]["state_from"], "ledger");

    let (claimed, _) = p.claim("impl-1", "payments");
    assert_eq!(claimed, t1);
    let v = json_in(&p, &repo);
    assert_eq!(state_of(&v, "P1"), "in progress");
    assert_eq!(state_of(&v, "P2"), "in progress");

    p.run_as_ok(
        &implementer("impl-1"),
        &["done", &t1, "--run", "true", "--since", "-"],
    );
    let v = json_in(&p, &repo);
    assert_eq!(state_of(&v, "P1"), "done");
    assert_eq!(state_of(&v, "P2"), "in progress", "one task is still open");
    assert_eq!(state_of(&v, "P3"), "pending");
}

/// Entries as `kontext import --from-v0` leaves them: `fields.v0_id`.
fn v0_entries(p: &Project) -> (String, String) {
    let ids = p.append(
        "{\"kind\":\"decision\",\"title\":\"Invoices are numbered per year\",\"actor\":{\"type\":\"human\",\"name\":\"omar\"},\"fields\":{\"v0_id\":\"D-0042\",\"status\":\"accepted\"}}\n\
         {\"kind\":\"proposal\",\"title\":\"Send overdue invoices a reminder email\",\"actor\":{\"type\":\"agent\",\"name\":\"coordinator\"},\"fields\":{\"v0_id\":\"P-0007\",\"status\":\"pending\"}}\n",
    );
    (ids[0].clone(), ids[1].clone())
}

#[test]
fn v0_ids_resolve_in_show_and_in_ideas() {
    let p = Project::new();
    let (d, pr) = v0_entries(&p);
    assert!(d.starts_with("D-") && pr.starts_with("P-"), "{d} {pr}");
    let out = p.kontext_ok(&["show", "D-0042"]);
    assert!(out.contains(&d), "{out}");
    assert!(out.contains("Invoices are numbered per year"), "{out}");
    let out = p.kontext_ok(&["show", "p-0007"]);
    assert!(out.contains(&pr), "{out}");
    let out = p.kontext(&["show", "D-0043"]);
    assert!(!out.status.success());

    let repo = pm_repo(
        &p,
        "app",
        "| P1 | yearly numbering | | done | D-0042: numbering per year | PR #9 |\n\
         | P2 | overdue reminders | | pending | P-0007 | |\n",
    );
    let v = json_in(&p, &repo);
    assert_eq!(state_of(&v, "P1"), "done");
    assert_eq!(v["ideas"][0]["refs"][0]["id"], d.as_str());
    assert_eq!(v["ideas"][0]["refs"][0]["cited"], "D-0042");
    // A v0 proposal from an agent that nobody accepted waits for the human.
    assert_eq!(state_of(&v, "P2"), "waiting for you");
}

#[test]
fn finds_pm_in_a_project_repo_from_elsewhere() {
    let p = Project::new();
    let repo = p.code_repo("app");
    fs::create_dir_all(repo.join(".pm")).unwrap();
    write_items(&repo, "| P1 | export | | pending | | |\n");
    let cfg = p.config.join("projects.toml");
    let mut s = fs::read_to_string(&cfg).unwrap();
    s.push_str(&format!(
        "\n[projects.demo.repos]\napp = \"{}\"\n",
        repo.display()
    ));
    fs::write(&cfg, s).unwrap();
    // The temporary folder is not a git repo: it falls back to `app`.
    let out = ideas_in(&p, p.dir.path(), &["ideas"]);
    assert!(out.contains("1 idea · no destination 1."), "{out}");
    assert!(out.contains("- P1 · export"), "{out}");
}

#[test]
fn features_decide_the_state_and_follow_their_replacement() {
    let (p, _g, _e) = with_epic();
    let u = p.id_of(
        &[],
        &[
            "feature",
            "add",
            "Refunds",
            "--epic",
            "payments",
            "--acceptance",
            "a refund",
        ],
    );
    let t = p.id_of(&[], &["task", "add", "Refund endpoint", "--feature", &u]);
    let repo = pm_repo(
        &p,
        "app",
        &format!("| P1 | refunds | dumps/2026-11-07.md:5 | done | {u} | |\n"),
    );
    let v = json_in(&p, &repo);
    assert_eq!(state_of(&v, "P1"), "pending");
    assert_eq!(v["ideas"][0]["state_from"], "ledger");
    assert_eq!(v["ideas"][0]["refs"][0]["kind"], "feature");
    assert_eq!(v["ideas"][0]["refs"][0]["status"], "pending");

    // All its tasks closed: to validate shows as in progress.
    p.kontext_ok(&["done", &t, "--evidence", "cmd:\"cargo test\"=0"]);
    let v = json_in(&p, &repo);
    assert_eq!(state_of(&v, "P1"), "in progress");
    assert_eq!(v["ideas"][0]["refs"][0]["status"], "to validate");

    p.kontext_ok(&["done", &u, "--evidence", "url:https://example.com/refund"]);
    assert_eq!(state_of(&json_in(&p, &repo), "P1"), "done");

    // Superseded: the replacement decides, with a note.
    let u2 = p.id_of(
        &[],
        &["feature", "add", "Partial refunds", "--epic", "payments"],
    );
    p.kontext_ok(&["supersede", &u, &u2, "--why", "partial too"]);
    let v = json_in(&p, &repo);
    assert_eq!(state_of(&v, "P1"), "pending");
    let note = v["ideas"][0]["notes"][0].as_str().unwrap();
    assert!(
        note.contains(&format!("{u} was superseded by {u2}")),
        "{note}"
    );
    assert_eq!(v["ideas"][0]["placed"], true);
}

#[test]
fn placed_needs_a_feature_task_or_decision() {
    let (p, _g, e) = with_epic();
    let u = p.id_of(&[], &["feature", "add", "Refunds", "--epic", "payments"]);
    let t = p.id_of(&[], &["task", "add", "Loose", "--epic", "payments"]);
    let d = p.id_of(&[], &["decide", "Use Stripe", "--why", "3DS"]);
    let q = p.id_of(&[], &["ask", "Which currency?", "--to", "owner"]);
    let repo = pm_repo(
        &p,
        "app",
        &format!(
            "| P1 | refunds | dumps/2026-11-07.md:5 | pending | {u} | |\n\
             | P2 | loose | | pending | {t} | |\n\
             | P3 | stripe | | decided | {d} | |\n\
             | P4 | dropped | | discarded | | |\n\
             | P5 | payments | | pending | {e} | |\n\
             | P6 | currency | | pending | {q} | |\n\
             | P7 | someday | | pending | ask the accountant | |\n\
             | P8 | csv | dumps/2026-11-07.md:5 | | | |\n"
        ),
    );
    let v = json_in(&p, &repo);
    let placed: Vec<bool> = v["ideas"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["placed"].as_bool().unwrap())
        .collect();
    assert_eq!(placed, [true, true, true, true, false, false, false, false]);
    // The states are as before: placed is apart.
    assert_eq!(state_of(&v, "P5"), "pending");
    assert_eq!(state_of(&v, "P6"), "waiting for you");
    let np = v["not_placed"].as_array().unwrap();
    let ids: Vec<&str> = np.iter().map(|n| n["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["P5", "P6", "P7", "P8"]);
    assert_eq!(np[0]["cites"], "Card payments, no feature");
    assert_eq!(np[1]["cites"], format!("question {q} open"));
    assert_eq!(np[2]["cites"], "ask the accountant");
    assert!(np[3].get("cites").is_none(), "{}", np[3]);
    assert_eq!(np[3]["quote"], "let customers export their invoices as CSV");
    assert_eq!(np[3]["said"], "2026-11-07");
    assert!(v["ideas"][0].get("cites").is_none());

    let out = ideas_in(&p, &repo, &["ideas"]);
    assert!(out.contains("Not placed: 4 (P5, P6, P7, P8)."), "{out}");
    assert!(
        out.contains("  not placed: Card payments, no feature"),
        "{out}"
    );
}

/// Ties `repo` to the project (`kontext install` writes this file): only a
/// `.pm/` of the project's repos is marked.
fn tie(repo: &Path) {
    fs::write(repo.join(".kontext.toml"), "project = \"demo\"\n").unwrap();
}

/// `kontext [--json] ideas --mark-audited --from <file>` in `dir`, with
/// `result` saved to that file.
fn mark(p: &Project, dir: &Path, result: &str, json: bool) -> std::process::Output {
    let f = p.dir.path().join("reader.json");
    fs::write(&f, result).unwrap();
    let mut args = vec!["ideas", "--mark-audited", "--from", f.to_str().unwrap()];
    if json {
        args.insert(0, "--json");
    }
    p.cmd(&args)
        .current_dir(dir)
        .env("KONTEXT_NOW", "2026-11-12T10:00:00Z")
        .output()
        .unwrap()
}

fn mark_ok(p: &Project, dir: &Path, result: &str) -> Value {
    let out = mark(p, dir, result, true);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

/// Refused with exit 2; its stderr.
fn mark_refused(p: &Project, dir: &Path, result: &str) -> String {
    let out = mark(p, dir, result, false);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert_eq!(out.status.code(), Some(2), "{err}");
    assert!(out.stdout.is_empty());
    err
}

fn audited_json(repo: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(repo.join(".pm/audited.json")).unwrap()).unwrap()
}

fn messages(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|m| format!("{}:{}", m["file"].as_str().unwrap(), m["line"]))
        .collect()
}

/// A dump with three messages, under a repo of the project: headings at
/// lines 1, 7 and 14; the second one's text wraps over two lines.
fn three_messages(p: &Project) -> PathBuf {
    let repo = pm_repo(p, "app", "| P1 | csv | dumps/2026-11-07.md:5 | | | |\n");
    tie(&repo);
    fs::write(
        repo.join(".pm/dumps/2026-11-08.md"),
        "## 2026-11-08 09:00\n\n**User:**\n\npay by card\n\n\
         ## 2026-11-08 09:30\n\n**User:**\n\na dark mode for\n  the  admin\n\n\
         ## 2026-11-08 18:00\n\n**User:**\n\nexport every report to PDF\n",
    )
    .unwrap();
    repo
}

#[test]
fn bare_mark_audited_is_refused_and_says_how_the_audit_runs() {
    let p = Project::new();
    let repo = three_messages(&p);
    let out = p
        .cmd(&["ideas", "--mark-audited"])
        .current_dir(&repo)
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert_eq!(out.status.code(), Some(2), "{err}");
    for needed in [
        "--mark-audited needs the reader's result",
        "kontext-pm-reader",
        "Audit task",
        "--mark-audited --from <file>",
        "Nothing was marked",
    ] {
        assert!(err.contains(needed), "{needed}: {err}");
    }
    assert!(!repo.join(".pm/audited.json").exists());
    assert_eq!(json_in(&p, &repo)["unaudited"]["count"], 4);
}

/// The messages marked are the ones the result lists, in order per dump: one
/// listed after one it skips stays unaudited. Quotes may wrap lines and
/// carry other whitespace.
#[test]
fn a_valid_result_marks_exactly_the_messages_it_lists() {
    let p = Project::new();
    let repo = three_messages(&p);
    let v = json_in(&p, &repo);
    assert_eq!(v["unaudited"]["count"], 4);
    assert!(v["unaudited"]["since"].is_null());
    let result = serde_json::json!({
        "read": [
            {"file": ".pm/dumps/2026-11-07.md", "line": 1},
            {"file": ".pm/dumps/2026-11-08.md", "line": 1},
            {"file": ".pm/dumps/2026-11-08.md", "line": 14},
        ],
        "ideas": [
            {"quote": "let customers export their invoices as CSV", "file": ".pm/dumps/2026-11-07.md",
             "line": 5, "in_items": true, "discarded": false},
            {"quote": "a dark mode for the admin", "file": ".pm/dumps/2026-11-08.md", "line": 11,
             "in_items": false, "discarded": false},
        ],
        "missing": [
            {"quote": "pay by card", "file": "dumps/2026-11-08.md", "line": 6},
        ],
    })
    .to_string();
    let v = mark_ok(&p, &repo, &result);
    assert_eq!(
        messages(&v["marked"]),
        ["dumps/2026-11-07.md:1", "dumps/2026-11-08.md:1"]
    );
    assert_eq!(messages(&v["left"]), ["dumps/2026-11-08.md:14"]);
    assert_eq!(v["unaudited"], 2);
    let a = audited_json(&repo);
    assert_eq!(a["at"], "2026-11-12T10:00:00Z");
    assert_eq!(
        a["files"],
        serde_json::json!({"dumps/2026-11-07.md": 1, "dumps/2026-11-08.md": 1})
    );
    let v = json_in(&p, &repo);
    assert_eq!(v["unaudited"]["count"], 2);
    assert_eq!(v["unaudited"]["since"], "2026-11-12T10:00:00Z");

    // The text says what it marked and what it left.
    fs::remove_file(repo.join(".pm/audited.json")).unwrap();
    let out = mark(&p, &repo, &result, false);
    assert!(out.status.success());
    let said = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        said.contains("2 messages newly audited (dumps/2026-11-07.md:1, dumps/2026-11-08.md:1)"),
        "{said}"
    );
    assert!(
        said.contains("Not marked, because a message before them in the same dump is not in the result: dumps/2026-11-08.md:14"),
        "{said}"
    );
}

/// A quote that is not in its cited file, or not near its line, is refused
/// and marks nothing: an earlier mark stays as it was, byte for byte.
#[test]
fn a_citation_not_in_its_file_is_refused_and_marks_nothing() {
    let p = Project::new();
    let repo = three_messages(&p);
    let before = "{\"at\": \"2026-11-10T00:00:00Z\", \"files\": {\"dumps/2026-11-07.md\": 1}}\n";
    fs::write(repo.join(".pm/audited.json"), before).unwrap();
    let read = serde_json::json!([
        {"file": ".pm/dumps/2026-11-08.md", "line": 1},
        {"file": ".pm/dumps/2026-11-08.md", "line": 7},
        {"file": ".pm/dumps/2026-11-08.md", "line": 14},
    ]);
    let made_up = serde_json::json!({"read": read, "ideas": [], "missing": [
        {"quote": "pay in bitcoin", "file": ".pm/dumps/2026-11-08.md", "line": 5},
    ]});
    let err = mark_refused(&p, &repo, &made_up.to_string());
    for needed in [
        "did not pass the checks, so nothing was marked",
        "citations: missing[0]: .pm/dumps/2026-11-08.md:5: the quote \"pay in bitcoin\" is not in that file",
        "never edit it",
    ] {
        assert!(err.contains(needed), "{needed}: {err}");
    }
    // Real, but cited far from where it is; a case change is not the same text.
    let far = serde_json::json!({"read": read, "ideas": [
        {"quote": "export every report to PDF", "file": ".pm/dumps/2026-11-08.md", "line": 5},
        {"quote": "Pay by card", "file": ".pm/dumps/2026-11-08.md", "line": 5},
    ], "missing": []});
    let err = mark_refused(&p, &repo, &far.to_string());
    assert!(
        err.contains("ideas[0]: .pm/dumps/2026-11-08.md:5: the quote \"export every report to PDF\" is not in that file within 2 lines of line 5 (it is at line 18)"),
        "{err}"
    );
    assert!(err.contains("ideas[1]"), "{err}");
    // A file outside .pm/.
    let outside = serde_json::json!({"read": read, "ideas": [
        {"quote": "pay by card", "file": "src/main.rs", "line": 5},
    ], "missing": []});
    let err = mark_refused(&p, &repo, &outside.to_string());
    assert!(err.contains("src/main.rs:5 is not a file in .pm/"), "{err}");
    assert_eq!(
        fs::read_to_string(repo.join(".pm/audited.json")).unwrap(),
        before
    );
}

/// `read` must name real messages; the reply must be an audit answer.
#[test]
fn a_result_that_lists_a_message_that_does_not_exist_is_refused() {
    let p = Project::new();
    let repo = three_messages(&p);
    let with_read =
        |read: Value| serde_json::json!({"read": read, "ideas": [], "missing": []}).to_string();
    let err = mark_refused(
        &p,
        &repo,
        &with_read(serde_json::json!([
            {"file": ".pm/dumps/2026-11-07.md", "line": 1},
            {"file": ".pm/dumps/2026-11-07.md", "line": 3},
        ])),
    );
    assert!(
        err.contains("read: read[1]: .pm/dumps/2026-11-07.md:3 is not the heading of a message"),
        "{err}"
    );
    let err = mark_refused(
        &p,
        &repo,
        &with_read(serde_json::json!([{"file": ".pm/dumps/2026-12-01.md", "line": 1}])),
    );
    assert!(
        err.contains("read: read[0]: .pm/dumps/2026-12-01.md:1: there is no such dump"),
        "{err}"
    );
    let err = mark_refused(
        &p,
        &repo,
        &with_read(serde_json::json!([{"file": ".pm/dumps/2026-11-07.md", "line": 99}])),
    );
    assert!(err.contains("2026-11-07.md:99 is not the heading"), "{err}");
    // A Value task answer, a reply without `read`, and one in a code fence.
    let err = mark_refused(
        &p,
        &repo,
        r#"{"read": [], "answers": [{"id": "q1", "value": "x", "quote": "pay by card", "file": ".pm/dumps/2026-11-08.md", "line": 5}]}"#,
    );
    assert!(err.contains("this is a Value task answer"), "{err}");
    let err = mark_refused(&p, &repo, r#"{"ideas": [], "missing": []}"#);
    assert!(err.contains("format: no `read` list"), "{err}");
    let err = mark_refused(&p, &repo, "```json\n{\"read\": []}\n```");
    assert!(err.contains("format: not JSON"), "{err}");
    assert!(!repo.join(".pm/audited.json").exists());
}

/// After a new message, a second audit marks only the new one; the same
/// result again (stale) is refused. A mark the old command wrote stays valid.
#[test]
fn a_second_audit_after_a_new_message_marks_only_the_new_one() {
    let p = Project::new();
    let repo = three_messages(&p);
    // The shape v0.2.0's bare --mark-audited wrote: every message then.
    fs::write(
        repo.join(".pm/audited.json"),
        "{\"at\": \"2026-11-10T00:00:00Z\", \"files\": {\"dumps/2026-11-07.md\": 1, \"dumps/2026-11-08.md\": 3}}\n",
    )
    .unwrap();
    assert_eq!(json_in(&p, &repo)["unaudited"]["count"], 0);
    let old = reader_result(&repo.join(".pm"));
    let err = mark_refused(&p, &repo, &old);
    assert!(
        err.contains("covers no message that is not audited yet, so nothing was marked: every message it lists (4) was audited already. Every message in the dumps is audited already: there is nothing to mark."),
        "{err}"
    );

    let mut d = fs::read_to_string(repo.join(".pm/dumps/2026-11-08.md")).unwrap();
    d.push_str("\n## 2026-11-08 20:00\n\n**User:**\n\nand invoices in PDF\n");
    fs::write(repo.join(".pm/dumps/2026-11-08.md"), d).unwrap();
    fs::write(
        repo.join(".pm/dumps/2026-11-13.md"),
        "## 2026-11-13 08:00\n\n**User:**\n\nD\n",
    )
    .unwrap();
    let v = json_in(&p, &repo);
    assert_eq!(v["unaudited"]["count"], 2);
    assert_eq!(v["unaudited"]["since"], "2026-11-10T00:00:00Z");
    assert!(
        ideas_in(&p, &repo, &["ideas"]).contains("2 of your messages not audited since Nov 10."),
    );
    // The stale result still covers nothing new, and now there is something to audit.
    let err = mark_refused(&p, &repo, &old);
    assert!(
        err.contains("covers no message that is not audited yet"),
        "{err}"
    );
    assert!(
        err.contains("2 not audited yet: launch kontext-pm-reader again"),
        "{err}"
    );

    // Through stdin: every message listed, only the two new ones marked.
    let mut child = p
        .cmd(&["--json", "ideas", "--mark-audited", "--from", "-"])
        .current_dir(&repo)
        .env("KONTEXT_NOW", "2026-11-13T10:00:00Z")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(reader_result(&repo.join(".pm")).as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        messages(&v["marked"]),
        ["dumps/2026-11-08.md:20", "dumps/2026-11-13.md:1"]
    );
    assert_eq!(v["unaudited"], 0);
    assert_eq!(v["messages"], 6);
    assert_eq!(
        audited_json(&repo)["files"],
        serde_json::json!({"dumps/2026-11-07.md": 1, "dumps/2026-11-08.md": 4, "dumps/2026-11-13.md": 1})
    );
    assert_eq!(json_in(&p, &repo)["unaudited"]["count"], 0);
}

#[test]
fn mark_audited_outside_a_repo_is_refused() {
    let p = Project::new();
    let dir = p.dir.path().join("nogit");
    fs::create_dir_all(&dir).unwrap();
    let err = mark_refused(&p, &dir, r#"{"read": [], "ideas": [], "missing": []}"#);
    assert!(
        err.contains("No .pm/items.md found in a repo of demo"),
        "{err}"
    );
    assert!(!dir.join(".pm").exists());
}

/// It marks the `.pm/` that `handoff` reads: the project's repo, also from
/// another folder; never one in a repo that is not the project's.
#[test]
fn mark_audited_writes_in_the_projects_pm() {
    let p = Project::new();
    let app = pm_repo(&p, "app", "| P1 | csv | dumps/2026-11-07.md:5 | | | |\n");
    p.add_repo_toml("[[repos]]\nname = \"app\"\n");
    p.map_repo("app", &app);
    // Another git repo, with no .pm of its own.
    let other = p.dir.path().join("other");
    fs::create_dir_all(&other).unwrap();
    git(&other, &["init", "-q"]);
    assert_eq!(json_in(&p, &other)["unaudited"]["count"], 1);
    let v = mark_ok(&p, &other, &reader_result(&app.join(".pm")));
    assert_eq!(messages(&v["marked"]), ["dumps/2026-11-07.md:1"]);
    assert!(app.join(".pm/audited.json").is_file());
    assert!(!other.join(".pm").exists());
    assert_eq!(json_in(&p, &other)["unaudited"]["count"], 0);

    // No items.md in a repo of the project: refused, saying where one counts.
    fs::remove_file(app.join(".pm/items.md")).unwrap();
    fs::remove_file(app.join(".pm/audited.json")).unwrap();
    let err = mark_refused(&p, &other, &reader_result(&app.join(".pm")));
    assert!(
        err.contains("No .pm/items.md found in a repo of demo"),
        "{err}"
    );
    assert!(err.contains("kontext install --link-only"), "{err}");
    assert!(!app.join(".pm/audited.json").exists());

    // A repo that is not the project's, with its own .pm/items.md: refused.
    let stray = pm_repo(&p, "stray", "");
    let err = mark_refused(&p, &stray, &reader_result(&stray.join(".pm")));
    assert!(err.contains("is in a repo that is not demo's"), "{err}");
    assert!(!stray.join(".pm/audited.json").exists());
}

#[test]
fn only_what_counts_places_an_idea() {
    let (p, _g, e) = with_epic();
    // A feature from someone who is not the pm or an owner (only another
    // machine writes it): stored, not counted.
    let fields = format!("{{\"epic\":\"{e}\",\"goals\":[]}}");
    let line = format!(
        "{{\"kind\":\"feature\",\"title\":\"x\",\"actor\":{{\"type\":\"agent\",\"name\":\"impl-1\",\"role\":\"implementer\"}},\"refs\":[\"{e}\"],\"fields\":{fields}}}\n"
    );
    let bad = p.append(&line)[0].clone();
    // A decision by an agent that is not an owner: does not count either.
    let d = p.append(
        "{\"kind\":\"decision\",\"title\":\"Use Conekta\",\"actor\":{\"type\":\"agent\",\"name\":\"impl-1\"},\"fields\":{}}\n",
    )[0]
    .clone();
    // A decision superseded by one in force follows it.
    let old = p.id_of(&[], &["decide", "Use Stripe", "--why", "3DS"]);
    p.id_of(
        &[],
        &[
            "decide",
            "Use Stripe v2",
            "--why",
            "3DS2",
            "--supersedes",
            &old,
        ],
    );
    let repo = pm_repo(
        &p,
        "app",
        &format!(
            "| P1 | refunds | | pending | {bad} | |\n\
             | P2 | conekta | | decided | {d} | |\n\
             | P3 | stripe | | decided | {old} | |\n"
        ),
    );
    let v = json_in(&p, &repo);
    let placed: Vec<bool> = v["ideas"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["placed"].as_bool().unwrap())
        .collect();
    assert_eq!(placed, [false, false, true], "{v}");
    let note = v["ideas"][0]["notes"][0].as_str().unwrap();
    assert!(note.contains(&format!("{bad} does not count")), "{note}");
    let np = v["not_placed"].as_array().unwrap();
    assert_eq!(np[0]["cites"], format!("feature {bad} not counted"));
    assert_eq!(np[1]["cites"], format!("decision {d} not counted"));
}

#[test]
fn an_epic_with_features_is_done_only_when_plan_closes_it() {
    let (p, _g, e) = with_epic();
    let u = p.id_of(&[], &["feature", "add", "Refunds", "--epic", "payments"]);
    let t = p.id_of(&[], &["task", "add", "Refund endpoint", "--feature", &u]);
    p.kontext_ok(&["done", &t, "--evidence", "cmd:\"cargo test\"=0"]);
    let repo = pm_repo(
        &p,
        "app",
        &format!(
            "| P1 | payments | | pending | {e} | |\n\
             | P2 | refunds | | pending | {u} | |\n"
        ),
    );
    // Every task closed, the feature not validated: not done.
    assert_eq!(state_of(&json_in(&p, &repo), "P1"), "in progress");
    p.kontext_ok(&["done", &u, "--evidence", "url:https://x"]);
    assert_eq!(state_of(&json_in(&p, &repo), "P1"), "done");
    // A task without a feature, open: the epic is open again.
    let loose = p.id_of(&[], &["task", "add", "Loose", "--epic", "payments"]);
    assert_eq!(state_of(&json_in(&p, &repo), "P1"), "in progress");
    p.kontext_ok(&["done", &loose, "--evidence", "cmd:\"cargo test\"=0"]);
    assert_eq!(state_of(&json_in(&p, &repo), "P1"), "done");
    // A feature whose validation was lost by a reopened task: in progress.
    p.kontext_ok(&["reopen", &t, "--why", "bug"]);
    let v = json_in(&p, &repo);
    assert_eq!(state_of(&v, "P2"), "in progress");
    assert_eq!(state_of(&v, "P1"), "in progress");
}
