//! `kontext ideas`: the user's ideas in `.pm/items.md` with their state from
//! the ledger.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{Project, git, implementer, with_epic};
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

#[test]
fn mark_audited_counts_the_messages_after_it() {
    let p = Project::new();
    let repo = pm_repo(&p, "app", "| P1 | csv | dumps/2026-11-07.md:5 | | | |\n");
    // Never audited: every entry of every dump.
    fs::write(
        repo.join(".pm/dumps/2026-11-08.md"),
        "## 2026-11-08 09:00\n\n**User:**\n\nA\n\n## 2026-11-08 09:30\n\n**User:**\n\nB\n",
    )
    .unwrap();
    let v = json_in(&p, &repo);
    assert_eq!(v["unaudited"]["count"], 3);
    assert!(v["unaudited"]["since"].is_null());
    let out = ideas_in(&p, &repo, &["ideas"]);
    assert!(out.contains("3 of your messages never audited."), "{out}");

    let out = p
        .cmd(&["ideas", "--mark-audited"])
        .current_dir(&repo)
        .env("KONTEXT_NOW", "2026-11-12T10:00:00Z")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let said = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(said.contains("3 messages in 2 dumps"), "{said}");
    let a: Value =
        serde_json::from_str(&fs::read_to_string(repo.join(".pm/audited.json")).unwrap()).unwrap();
    assert_eq!(a["at"], "2026-11-12T10:00:00Z");
    assert_eq!(a["files"]["dumps/2026-11-07.md"], 1);
    assert_eq!(a["files"]["dumps/2026-11-08.md"], 2);
    let v = json_in(&p, &repo);
    assert_eq!(v["unaudited"]["count"], 0);
    assert_eq!(v["unaudited"]["since"], "2026-11-12T10:00:00Z");
    assert!(!ideas_in(&p, &repo, &["ideas"]).contains("audited"));

    // A new entry in a dump it saw, and a dump it did not list.
    let mut d = fs::read_to_string(repo.join(".pm/dumps/2026-11-08.md")).unwrap();
    d.push_str("\n## 2026-11-08 18:00\n\n**User:**\n\nC\n");
    fs::write(repo.join(".pm/dumps/2026-11-08.md"), d).unwrap();
    fs::write(
        repo.join(".pm/dumps/2026-11-13.md"),
        "## 2026-11-13 08:00\n\n**User:**\n\nD\n",
    )
    .unwrap();
    let v = json_in(&p, &repo);
    assert_eq!(v["unaudited"]["count"], 2);
    let out = ideas_in(&p, &repo, &["ideas"]);
    assert!(
        out.contains("2 of your messages not audited since Nov 12."),
        "{out}"
    );

    // Without items.md the JSON still says it; the text is as before.
    fs::remove_file(repo.join(".pm/items.md")).unwrap();
    let v = json_in(&p, &repo);
    assert_eq!(v["unaudited"]["count"], 2);
    assert_eq!(v["not_placed"].as_array().unwrap().len(), 0);
}

#[test]
fn mark_audited_outside_a_repo_is_refused() {
    let p = Project::new();
    let dir = p.dir.path().join("nogit");
    fs::create_dir_all(&dir).unwrap();
    let out = p
        .cmd(&["ideas", "--mark-audited"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!dir.join(".pm").exists());
}
