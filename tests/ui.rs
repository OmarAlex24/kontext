//! `kontext ui` (phase 6): the page and the API against the project of
//! `examples/demo.sh`, with the real server on a free port.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use serde_json::Value;
use sha2::{Digest, Sha256};

const BIN: &str = env!("CARGO_BIN_EXE_kontext");

struct Demo {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

fn demo() -> Demo {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("demo");
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/demo.sh");
    let out = Command::new("bash")
        .arg(&script)
        .arg(&root)
        .env("KONTEXT_BIN", BIN)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "demo.sh failed: {}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    Demo { _dir: dir, root }
}

/// `kontext ui` running; it is killed when dropped.
struct Ui {
    child: Child,
    addr: String,
    stderr: Option<std::process::ChildStderr>,
}

impl Drop for Ui {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start(d: &Demo, extra: &[&str]) -> Ui {
    let mut args = vec!["--project", "billing", "ui", "--port", "0"];
    args.extend_from_slice(extra);
    let mut child = Command::new(BIN)
        .args(&args)
        .env("KONTEXT_CONFIG_DIR", d.root.join("config"))
        .env("KONTEXT_ACTOR", "human:omar")
        .env_remove("KONTEXT_PROJECT")
        .current_dir(&d.root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    // "kontext ui: http://127.0.0.1:PORT/"
    let url = line
        .trim()
        .strip_prefix("kontext ui: http://")
        .unwrap_or_else(|| panic!("unexpected first line: {line:?}"));
    let addr = url.trim_end_matches('/').to_string();
    let stderr = child.stderr.take();
    Ui {
        child,
        addr,
        stderr,
    }
}

/// A raw request; returns the code, the headers and the body.
fn request(addr: &str, method: &str, path: &str, host: Option<&str>) -> (u16, String, String) {
    let mut s = TcpStream::connect(addr).unwrap();
    let host = host.unwrap_or(addr);
    let req = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    s.write_all(req.as_bytes()).unwrap();
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).unwrap();
    let text = String::from_utf8(buf).unwrap();
    let (head, body) = text.split_once("\r\n\r\n").unwrap();
    let code: u16 = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    (code, head.to_string(), body.to_string())
}

fn get_json(addr: &str, path: &str) -> Value {
    let (code, _, body) = request(addr, "GET", path, None);
    assert_eq!(code, 200, "{path}: {body}");
    serde_json::from_str(&body).unwrap()
}

fn row(rows: &[Value], f: impl Fn(&Value) -> bool) -> &Value {
    rows.iter().find(|r| f(r)).expect("the row exists")
}

#[test]
fn binds_loopback_by_default_and_serves_an_offline_page() {
    let d = demo();
    let ui = start(&d, &[]);
    assert!(ui.addr.starts_with("127.0.0.1:"), "{}", ui.addr);

    // The ledger page moved to /ledger, as it was.
    let (code, head, body) = request(&ui.addr, "GET", "/ledger", None);
    assert_eq!(code, 200);
    assert!(head.contains("text/html"), "{head}");
    assert!(head.contains("Content-Security-Policy"), "{head}");
    assert!(body.contains("One ledger, one view for each reader"));
    assert!(body.contains("/api/state"));
    // No internet: no fonts, scripts or remote images.
    assert!(
        !body.to_lowercase().contains("http"),
        "the page cites an external URL"
    );
    assert!(!body.contains("/*KONTEXT_STYLE*/") && !body.contains("/*KONTEXT_SCRIPT*/"));

    // Read-only, and only with a loopback Host (against DNS rebinding).
    assert_eq!(request(&ui.addr, "POST", "/api/state", None).0, 405);
    assert_eq!(
        request(&ui.addr, "GET", "/api/state", Some("evil.example:80")).0,
        403
    );
    let port = ui.addr.rsplit_once(':').unwrap().1;
    assert_eq!(
        request(&ui.addr, "GET", "/", Some(&format!("localhost:{port}"))).0,
        200
    );
    assert_eq!(request(&ui.addr, "GET", "/nothing", None).0, 404);

    // The report for the human, with the same CSP; the screenshots go through /media/.
    let (code, head, body) = request(&ui.addr, "GET", "/report", None);
    assert_eq!(code, 200);
    assert!(head.contains("text/html") && head.contains("Content-Security-Policy"));
    assert!(
        body.contains(">Your turn<") && body.contains(">Progress<"),
        "{body}"
    );
    assert!(!body.to_lowercase().contains("http"), "no external URLs");
    // Its CSP is tighter than the page's: only its style and its script run,
    // by hash; the font is embedded and it connects to nothing.
    let csp = head
        .lines()
        .find_map(|l| l.strip_prefix("Content-Security-Policy: "))
        .unwrap();
    assert!(!csp.contains("unsafe-inline"), "{csp}");
    assert!(!csp.contains("connect-src"), "{csp}");
    for d in [
        "default-src 'none'",
        "img-src 'self' data:",
        "font-src data:",
        "base-uri 'none'",
        "form-action 'none'",
        "frame-ancestors 'none'",
    ] {
        assert!(csp.contains(d), "missing {d:?} in {csp}");
    }
    let inner = |open: &str, close: &str| {
        let a = body.find(open).unwrap() + open.len();
        body[a..].split_once(close).unwrap().0.to_string()
    };
    for (directive, content) in [
        ("script-src", inner("<script>", "</script>")),
        ("style-src", inner("<style>", "</style>")),
    ] {
        let hash = kontext::media::base64(Sha256::digest(content.as_bytes()).as_slice());
        assert!(
            csp.contains(&format!("{directive} 'sha256-{hash}'")),
            "{directive} does not let the page's content run: {csp}"
        );
    }
    assert!(
        !body.contains(" style=\""),
        "an inline style= would not pass the CSP"
    );
    assert_eq!(
        request(&ui.addr, "GET", "/report", Some("evil.example:80")).0,
        403
    );
    assert_eq!(
        request(&ui.addr, "GET", "/media/../project.toml", None).0,
        404
    );
    let missing = format!("/media/{}.png", "a".repeat(64));
    assert_eq!(request(&ui.addr, "GET", &missing, None).0, 404);
}

#[test]
fn state_api_has_everything_the_page_draws() {
    let d = demo();
    let ui = start(&d, &[]);
    let s = get_json(&ui.addr, "/api/state");

    assert_eq!(s["project"], "billing");
    assert_eq!(s["you"], "omar");
    assert_eq!(s["status"]["chains_ok"], true);
    let writers: Vec<&str> = s["writers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["name"].as_str().unwrap())
        .collect();
    assert_eq!(writers, ["ana@laptop", "omar@mac"]);

    assert_eq!(s["charter"]["status"], "signed");
    assert_eq!(s["charter"]["signed_by"], "omar");
    assert_eq!(s["goals"].as_array().unwrap().len(), 3);
    assert_eq!(s["decisions"].as_array().unwrap().len(), 3);
    assert_eq!(s["proposals"].as_array().unwrap().len(), 1);
    assert_eq!(s["questions"].as_array().unwrap().len(), 2);

    // The epic with a task in each status (and the drift one, ready).
    let epic = &s["epics"][0];
    assert_eq!(epic["name"], "reminders");
    for (status, n) in [
        ("ready", 2),
        ("blocked", 1),
        ("assigned", 1),
        ("claimed", 1),
        ("closed", 1),
    ] {
        assert_eq!(epic["counts"][status], n, "{status}");
    }
    let tasks = s["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 6);
    let drift: Vec<&Value> = tasks.iter().filter(|t| t["drift"].is_string()).collect();
    assert_eq!(drift.len(), 1);
    assert_eq!(drift[0]["drift"], "Serves no goal: possible drift");

    // Facts with their freshness: one expired.
    let facts = s["facts"].as_array().unwrap();
    assert!(
        facts
            .iter()
            .any(|f| f["freshness"]["state"] == "stale"
                && f["freshness"]["path"] == "src/invoices.py"),
        "{facts:?}"
    );
    let cats: Vec<&str> = s["alerts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["category"].as_str().unwrap())
        .collect();
    assert!(
        cats.contains(&"drift") && cats.contains(&"stale"),
        "{cats:?}"
    );

    // Readers with a cursor: you first, then the others by name.
    let readers = s["readers"].as_array().unwrap();
    let names: Vec<&str> = readers
        .iter()
        .map(|r| r["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["omar", "epic:reminders", "pm"]);
    let omar = &readers[0];
    assert_eq!(omar["is_you"], true);
    assert_eq!(omar["cursor"]["omar@mac"], 23);
    assert_eq!(omar["new"], 10);
    assert!(omar["card"]["waiting"].as_array().unwrap().len() >= 2);
    let pm = &readers[2];
    assert_eq!(pm["cursor"]["ana@laptop"], 2);
    assert_eq!(pm["cursor"]["omar@mac"], 26);
    assert_eq!(pm["new"], 5);
    assert!(
        pm["card"]["epics"][0]
            .as_str()
            .unwrap()
            .starts_with("Payment reminders")
    );

    // The ledger, from the newest to the oldest.
    let ledger = &s["ledger"];
    assert_eq!(ledger["total"], 33);
    let rows = ledger["entries"].as_array().unwrap();
    assert_eq!(rows.len(), 33);
    assert!(ledger["next_before"].is_null());
    let hlcs: Vec<&str> = rows.iter().map(|r| r["hlc"].as_str().unwrap()).collect();
    let mut sorted = hlcs.clone();
    sorted.sort_by(|a, b| b.cmp(a));
    assert_eq!(hlcs, sorted);
    for r in rows {
        for k in [
            "id", "writer", "n", "hash", "prev", "category", "title", "actor",
        ] {
            assert!(!r[k].is_null(), "missing {k} in {r}");
        }
    }
    // Each reader's line falls on an entry that its cursor does not cover.
    let pm_first = pm["first_new"].as_str().unwrap();
    let first = row(rows, |r| r["id"] == pm_first);
    assert_eq!(first["writer"], "ana@laptop");
    assert_eq!(first["n"], 3);

    let charter = row(rows, |r| r["kind"] == "charter");
    assert_eq!(charter["stamp"], "Signed by you");
    assert_eq!(charter["prev"], "inicio");
    let proposal = row(rows, |r| r["kind"] == "proposal");
    assert_eq!(proposal["hollow"], true);
    assert!(proposal["note"].as_str().unwrap().contains("Unreviewed"));
    let stale = row(rows, |r| r["kind"] == "fact" && r["struck"] == true);
    assert!(stale["warning"].as_str().unwrap().starts_with("Expired"));
    let superseded = row(rows, |r| r["kind"] == "decision" && r["struck"] == true);
    assert!(
        superseded["note"]
            .as_str()
            .unwrap()
            .starts_with("Replaced by D-")
    );
    let drift_row = row(rows, |r| r["warning"] == "Serves no goal: possible drift");
    assert_eq!(drift_row["kind"], "task");
    let task = row(rows, |r| {
        r["kind"] == "task" && r["note"].as_str().unwrap().contains("claimed by")
    });
    assert!(task["note"].as_str().unwrap().starts_with("Serves G-"));
    let done = row(rows, |r| r["kind"] == "done");
    assert!(
        done["note"]
            .as_str()
            .unwrap()
            .starts_with("Closed with evidence")
    );
    assert_eq!(done["category"], "task");
}

#[test]
fn ledger_pages_and_entry_endpoint() {
    let d = demo();
    let ui = start(&d, &[]);
    let mut seen = Vec::new();
    let mut before: Option<String> = None;
    loop {
        let path = match &before {
            Some(b) => format!("/api/state?limit=10&before={}", b.replace(':', "%3A")),
            None => "/api/state?limit=10".to_string(),
        };
        let s = get_json(&ui.addr, &path);
        let rows = s["ledger"]["entries"].as_array().unwrap();
        assert!(!rows.is_empty() && rows.len() <= 11, "{}", rows.len());
        seen.extend(rows.iter().map(|r| r["id"].as_str().unwrap().to_string()));
        match s["ledger"]["next_before"].as_str() {
            Some(b) => before = Some(b.to_string()),
            None => break,
        }
    }
    let mut unique = seen.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(seen.len(), 33);
    assert_eq!(unique.len(), 33, "no repeats between pages");
    assert_eq!(request(&ui.addr, "GET", "/api/state?limit=0", None).0, 400);

    let (code, _, body) = request(&ui.addr, "GET", "/api/entry/X-000000", None);
    assert_eq!(code, 404);
    assert!(body.contains("there is no entry"));
    let s = get_json(&ui.addr, "/api/state");
    let id = s["charter"]["id"].as_str().unwrap();
    let e = get_json(&ui.addr, &format!("/api/entry/{id}"));
    assert_eq!(e["entry"]["id"], id);
    assert_eq!(e["status_label"], "signed");
    assert_eq!(e["row"]["stamp"], "Signed by you");
    assert!(!e["refs_rows"].as_array().unwrap().is_empty());
    // A unique prefix also works, as in `kontext show`.
    let e = get_json(&ui.addr, &format!("/api/entry/{}", &id[..5]));
    assert_eq!(e["entry"]["id"], id);
}

#[test]
fn non_loopback_host_warns() {
    let d = demo();
    let mut ui = start(&d, &["--host", "0.0.0.0"]);
    assert!(ui.addr.starts_with("0.0.0.0:"), "{}", ui.addr);
    let _ = ui.child.kill();
    let mut err = String::new();
    ui.stderr.take().unwrap().read_to_string(&mut err).unwrap();
    assert!(err.contains("not loopback"), "{err}");
    assert!(err.contains("has no password"), "{err}");
}

#[test]
fn report_reads_the_ideas_on_each_request_and_its_csp_covers_them() {
    let d = demo();
    // Without `.pm/`, the CSP is the one it always was.
    {
        let ui = start(&d, &[]);
        let (_, head, body) = request(&ui.addr, "GET", "/report", None);
        assert!(!body.contains("id=\"your-ideas\""));
        assert!(head.contains(&format!(
            "Content-Security-Policy: {}",
            kontext::report::csp()
        )));
    }
    // `kontext ui` starts in d.root: make it a repo with `.pm/items.md`.
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(&d.root)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
    };
    git(&["init", "-q"]);
    let pm = d.root.join(".pm");
    std::fs::create_dir_all(&pm).unwrap();
    let header = "| id | idea | source | state | where | evidence |\n|---|---|---|---|---|---|\n";
    std::fs::write(
        pm.join("items.md"),
        format!("{header}| P1 | Export invoices as CSV | | pending | | |\n"),
    )
    .unwrap();
    let ui = start(&d, &[]);
    let (code, head, body) = request(&ui.addr, "GET", "/report", None);
    assert_eq!(code, 200);
    assert!(body.contains("id=\"your-ideas\""), "{body}");
    assert!(body.contains("<span>Your ideas</span><span class=\"n\">1</span>"));
    // Every inline style and script of the page runs under the CSP.
    let csp = head
        .lines()
        .find_map(|l| l.strip_prefix("Content-Security-Policy: "))
        .unwrap();
    for (open, close, directive) in [
        ("<style>", "</style>", "style-src"),
        ("<script>", "</script>", "script-src"),
    ] {
        let blocks: Vec<&str> = body
            .split(open)
            .skip(1)
            .map(|b| b.split_once(close).unwrap().0)
            .collect();
        assert_eq!(blocks.len(), 2, "{directive}");
        let allowed = csp
            .split(';')
            .find(|d| d.trim_start().starts_with(directive))
            .unwrap();
        for b in blocks {
            let hash = kontext::media::base64(Sha256::digest(b.as_bytes()).as_slice());
            assert!(
                allowed.contains(&format!("'sha256-{hash}'")),
                "{directive}: {csp}"
            );
        }
    }
    assert!(!body.contains(" style=\""));
    // An edited items.md shows on reload, with no restart.
    std::fs::write(
        pm.join("items.md"),
        format!(
            "{header}| P1 | Export invoices as CSV | | pending | | |\n| P2 | Send reminders | | pending | | |\n"
        ),
    )
    .unwrap();
    let (_, _, body) = request(&ui.addr, "GET", "/report", None);
    assert!(
        body.contains("<span>Your ideas</span><span class=\"n\">2</span>"),
        "{body}"
    );
    assert!(body.contains("Send reminders"));
}

#[test]
fn the_root_is_the_tree_and_its_api_is_kontext_tree_json() {
    let d = demo();
    let ui = start(&d, &[]);
    let (code, head, body) = request(&ui.addr, "GET", "/", None);
    assert_eq!(code, 200);
    assert!(head.contains("text/html"), "{head}");
    // The same CSP as the ledger page.
    let (_, ledger_head, _) = request(&ui.addr, "GET", "/ledger", None);
    let csp = |h: &str| {
        h.lines()
            .find(|l| l.starts_with("Content-Security-Policy: "))
            .unwrap()
            .to_string()
    };
    assert_eq!(csp(&head), csp(&ledger_head));
    assert!(body.contains("/api/tree"), "{body}");
    assert!(body.contains("href=\"/ledger\"") && body.contains("href=\"/report\""));
    assert!(!body.to_lowercase().contains("http"), "no external URLs");
    assert!(!body.contains("/*KONTEXT_STYLE*/") && !body.contains("/*KONTEXT_SCRIPT*/"));
    assert_eq!(request(&ui.addr, "GET", "/index.html", None).0, 200);

    // /api/tree is what `kontext tree --json` gives.
    let v = get_json(&ui.addr, "/api/tree");
    let out = Command::new(BIN)
        .args(["--project", "billing", "--json", "tree"])
        .env("KONTEXT_CONFIG_DIR", d.root.join("config"))
        .env("KONTEXT_ACTOR", "human:omar")
        .env_remove("KONTEXT_PROJECT")
        .current_dir(&d.root)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let cli: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v, cli);
    assert!(v["waves"].as_array().is_some_and(|w| !w.is_empty()), "{v}");
    // The waves are kontext plan's; closed epics are under `done`.
    assert!(v["done"].is_array(), "{v}");
    assert!(v.get("current_wave").is_none(), "{v}");
    assert_eq!(
        request(&ui.addr, "GET", "/api/tree", Some("evil.example:80")).0,
        403
    );
    assert_eq!(request(&ui.addr, "POST", "/api/tree", None).0, 405);
}
