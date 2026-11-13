//! Phase 4: several machines syncing through a kontext server (DESIGN §14.1),
//! here the real `server/worker.js` in a local workerd (`tests/common/server.rs`).
//! Each "machine" has its own `KONTEXT_CONFIG_DIR`, writer and copy of the
//! kontext-home. Without Node or `server/node_modules` these tests are skipped
//! with a message (CI sets `KONTEXT_REQUIRE_SERVER_TESTS=1`, so they run there).

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use common::BIN;
use common::server::{ADMIN, LocalServer};
use kontext::http::Http;
use serde_json::{Value, json};

struct Net {
    dir: tempfile::TempDir,
    server: LocalServer,
}

struct Machine {
    name: String,
    person: String,
    config: PathBuf,
    home: PathBuf,
    gitconfig: PathBuf,
}

impl Net {
    fn new() -> Option<Net> {
        let dir = tempfile::tempdir().unwrap();
        let server = LocalServer::start()?;
        Some(Net { dir, server })
    }

    /// With the server's data on disk, so it can be stopped and started again.
    fn persistent() -> Option<Net> {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("server-state");
        let server = LocalServer::start_with(Some(&state))?;
        Some(Net { dir, server })
    }

    fn url(&self) -> &str {
        &self.server.url
    }

    /// A machine of `person` with its writer (no kontext-home yet).
    fn machine(&self, name: &str, writer: &str, person: &str) -> Machine {
        let base = self.dir.path().join(name);
        let config = base.join("config");
        fs::create_dir_all(&config).unwrap();
        fs::write(
            config.join("projects.toml"),
            format!("writer = \"{writer}\"\n"),
        )
        .unwrap();
        let gitconfig = base.join("gitconfig");
        fs::write(
            &gitconfig,
            format!(
                "[user]\n\tname = {person}\n\temail = {person}@example.com\n[init]\n\t\
                 defaultBranch = main\n[commit]\n\tgpgsign = false\n"
            ),
        )
        .unwrap();
        Machine {
            name: name.into(),
            person: person.into(),
            config,
            home: base.join("home"),
            gitconfig,
        }
    }

    /// `a` creates the project on the server with the admin secret, sets the
    /// owners and sends the config.
    fn start(&self, a: &Machine, owners: &[&str]) {
        let out = a.ok_env(
            &[("KONTEXT_ADMIN_SECRET", ADMIN)],
            &[
                "init",
                "demo",
                "--home",
                a.home.to_str().unwrap(),
                "--server",
                self.url(),
            ],
        );
        assert!(out.contains("Project demo created"), "{out}");
        set_owners(&a.home, owners);
        let out = a.ok(&["sync", "--include-config"]);
        assert!(out.contains("config sent: project.toml"), "{out}");
    }

    /// A token minted by the owner `by` for `writer`, with `role`.
    fn token(&self, by: &Machine, person: &str, role: &str, writer: &str) -> String {
        let r = post(
            &format!("{}/v1/p/demo/tokens", self.url()),
            &by.token(),
            &json!({ "person": person, "role": role, "writer": writer }),
        );
        assert_eq!(r.0, 201, "{}", r.1);
        r.1["token"].as_str().unwrap().to_string()
    }

    /// `m` clones the project from the server with a token for its writer.
    fn join(&self, by: &Machine, m: &Machine) {
        let token = self.token(by, &m.person, "owner", &m.writer());
        let out = m.ok_env(
            &[("KONTEXT_TOKEN", &token)],
            &[
                "clone",
                "--server",
                self.url(),
                "demo",
                "--home",
                m.home.to_str().unwrap(),
            ],
        );
        assert!(out.contains("Project demo cloned"), "{out}");
    }
}

impl Machine {
    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .env("KONTEXT_CONFIG_DIR", &self.config)
            .env("KONTEXT_PROJECT", "demo")
            .env("KONTEXT_ACTOR", format!("human:{}", self.person))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", &self.gitconfig)
            .env_remove("KONTEXT_ROLE")
            .env_remove("KONTEXT_TOKEN")
            .env_remove("KONTEXT_SERVER")
            .env_remove("KONTEXT_ADMIN_SECRET")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env_remove("SSH_AUTH_SOCK")
            .current_dir(self.config.parent().unwrap());
        c
    }

    fn run(&self, envs: &[(&str, &str)], args: &[&str]) -> Output {
        let mut c = self.cmd(args);
        for (k, v) in envs {
            c.env(k, v);
        }
        c.output().unwrap()
    }

    fn ok_env(&self, envs: &[(&str, &str)], args: &[&str]) -> String {
        let out = self.run(envs, args);
        assert!(
            out.status.success(),
            "[{}] kontext {args:?} failed ({:?}): {}\n{}",
            self.name,
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        self.ok_env(&[], args)
    }

    fn writer(&self) -> String {
        let t = fs::read_to_string(self.config.join("projects.toml")).unwrap();
        t.lines()
            .find_map(|l| l.strip_prefix("writer = "))
            .unwrap()
            .trim_matches('"')
            .to_string()
    }

    fn token(&self) -> String {
        fs::read_to_string(self.config.join("tokens/demo.token"))
            .unwrap()
            .trim()
            .to_string()
    }

    /// The ledger files (`<writer>.jsonl`) and their bytes.
    fn ledger(&self) -> BTreeMap<String, Vec<u8>> {
        let mut l = files_in(&self.home.join("ledger"));
        l.retain(|name, _| name.ends_with(".jsonl"));
        l
    }

    fn lines(&self, writer: &str) -> Vec<String> {
        fs::read_to_string(self.home.join("ledger").join(format!("{writer}.jsonl")))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }
}

fn files_in(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    for e in fs::read_dir(dir).unwrap().flatten() {
        if e.file_type().unwrap().is_file() {
            out.insert(
                e.file_name().to_string_lossy().into_owned(),
                fs::read(e.path()).unwrap(),
            );
        }
    }
    out
}

fn set_owners(home: &Path, owners: &[&str]) {
    let toml = home.join("project.toml");
    let owners: Vec<String> = owners.iter().map(|o| format!("{o:?}")).collect();
    let text: String = fs::read_to_string(&toml)
        .unwrap()
        .lines()
        .map(|l| {
            if l.starts_with("owners = ") {
                format!("owners = [{}]\n", owners.join(", "))
            } else {
                format!("{l}\n")
            }
        })
        .collect();
    fs::write(&toml, text).unwrap();
}

fn http() -> Http {
    Http::new(Duration::from_secs(10))
}

fn post(url: &str, token: &str, body: &Value) -> (u16, Value) {
    let auth = format!("Bearer {token}");
    let r = http()
        .request(
            "POST",
            url,
            &[
                ("Authorization", auth.as_str()),
                ("Content-Type", "application/json"),
            ],
            Some(body.to_string().as_bytes()),
        )
        .unwrap();
    (r.status, r.json().unwrap_or(Value::Null))
}

fn frontier(net: &Net, token: &str) -> Value {
    let auth = format!("Bearer {token}");
    let r = http()
        .request(
            "GET",
            &format!("{}/v1/p/demo/frontier", net.url()),
            &[("Authorization", auth.as_str())],
            None,
        )
        .unwrap();
    assert_eq!(r.status, 200, "{}", r.text());
    r.json().unwrap()
}

/// The screenshot used as an attachment.
fn capture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("examples/screenshots")
        .join(name)
}

#[test]
fn two_writers_interleave_and_every_copy_ends_byte_identical() {
    let Some(net) = Net::new() else { return };
    let a = net.machine("a", "omar@a", "omar");
    let b = net.machine("b", "ana@b", "ana");
    net.start(&a, &["omar", "ana"]);
    net.join(&a, &b);

    // Each command pushes what it wrote and pulls before reading: no `sync`.
    a.ok(&["log", "a1"]);
    b.ok(&["log", "b1"]);
    a.ok(&["decide", "Use the server", "--why", "one relay"]);
    b.ok(&["log", "b2"]);
    a.ok(&["log", "a2"]);
    let out = b.ok(&["standup", "--peek"]);
    for want in ["a1", "Use the server", "a2", "b1", "b2"] {
        assert!(out.contains(want), "{want} missing on B:\n{out}");
    }
    let out = a.ok(&["standup", "--peek"]);
    assert!(out.contains("b2"), "{out}");

    assert_eq!(a.ledger(), b.ledger(), "the two copies differ");
    assert_eq!(a.ledger().len(), 2);
    for m in [&a, &b] {
        let out = m.ok(&["verify"]);
        assert!(out.contains("in 2 writers"), "{out}");
        let out = m.ok(&["status"]);
        assert!(out.contains("Own entries not pushed: 0"), "{out}");
        assert!(
            out.contains("syncs through its server"),
            "git is not the remote here: {out}"
        );
    }
    // The server keeps exactly the local lines, byte for byte.
    let f = frontier(&net, &a.token());
    assert_eq!(f["tails"]["omar@a"]["n"], a.lines("omar@a").len());
    assert_eq!(f["tails"]["ana@b"]["n"], b.lines("ana@b").len());
}

#[test]
fn cursors_media_and_config_round_trip() {
    let Some(net) = Net::new() else { return };
    let a = net.machine("a", "omar@a", "omar");
    let b = net.machine("b", "ana@b", "ana");
    net.start(&a, &["omar"]);
    net.join(&a, &b);
    assert!(
        fs::read_to_string(b.home.join("project.toml"))
            .unwrap()
            .contains("owners = [\"omar\"]"),
        "the clone has the server's project.toml"
    );

    // Media: a decision with a screenshot reaches B with the same bytes.
    a.ok(&[
        "decide",
        "The screen follows the mockup",
        "--attach",
        capture("payment-v2.png").to_str().unwrap(),
    ]);
    b.ok(&["sync"]);
    let ma = files_in(&a.home.join("media"));
    assert_eq!(ma.len(), 1, "{:?}", ma.keys());
    assert_eq!(ma, files_in(&b.home.join("media")));
    assert!(b.ok(&["verify"]).contains("OK"));

    // Cursors: omar reads on A; on B the same reader has nothing new.
    a.ok(&["standup"]);
    a.ok(&["sync"]);
    b.ok(&["sync"]);
    assert!(b.home.join("cursors/omar/omar@a.json").exists());
    let out = b.ok_env(&[("KONTEXT_ACTOR", "human:omar")], &["standup", "--peek"]);
    assert!(out.contains("Nothing new since your last time"), "{out}");
    // B's own reading goes up and does not overwrite A's file.
    b.ok(&["standup"]);
    b.ok(&["sync"]);
    a.ok(&["sync"]);
    assert!(a.home.join("cursors/ana/ana@b.json").exists());
    assert!(a.home.join("cursors/omar/omar@a.json").exists());

    // Config: an owner's change goes up only with --include-config.
    set_owners(&a.home, &["omar", "ana"]);
    let out = a.ok(&["sync"]);
    assert!(!out.contains("config sent"), "{out}");
    let out = a.ok(&["sync", "--include-config"]);
    assert!(out.contains("config sent: project.toml"), "{out}");
    let out = b.ok(&["sync"]);
    assert!(out.contains("config received: project.toml"), "{out}");
    assert!(
        fs::read_to_string(b.home.join("project.toml"))
            .unwrap()
            .contains("owners = [\"omar\", \"ana\"]")
    );
}

#[test]
fn a_line_that_forks_a_chain_is_refused_and_never_written() {
    let Some(net) = Net::new() else { return };
    let a = net.machine("a", "omar@a", "omar");
    net.start(&a, &["omar"]);
    a.ok(&["log", "before the copy"]);
    // C is a second copy that wrongly reuses A's token, so A's writer.
    let c = net.machine("c", "omar@c", "omar");
    let out = c.ok_env(
        &[("KONTEXT_TOKEN", &a.token())],
        &[
            "clone",
            "--server",
            net.url(),
            "demo",
            "--home",
            c.home.to_str().unwrap(),
        ],
    );
    assert!(out.contains("Writer of this machine: omar@a"), "{out}");
    fs::create_dir_all(c.config.join("tokens")).unwrap();
    fs::write(c.config.join("tokens/demo.token"), a.token()).unwrap();

    // C writes while it cannot reach the server; then A writes the same `n`.
    c.ok_env(
        &[("KONTEXT_SERVER", "http://127.0.0.1:9")],
        &["log", "written on C"],
    );
    a.ok(&["log", "written on A"]);
    let c_before = c.lines("omar@a");
    let a_before = a.lines("omar@a");
    assert_eq!(c_before.len(), a_before.len());
    assert_ne!(c_before.last(), a_before.last());

    let out = c.run(&[], &["sync"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "{text}");
    assert!(text.contains("REFUSED by the server"), "{text}");
    assert!(text.contains("fork"), "{text}");
    // Nothing was overwritten, on either side.
    assert_eq!(c.lines("omar@a"), c_before);
    let a_out = a.ok(&["sync"]);
    assert!(!a_out.contains("Received"), "{a_out}");
    assert_eq!(a.lines("omar@a"), a_before);
    let status = c.ok(&["status"]);
    assert!(status.contains("REFUSED by the server"), "{status}");
    assert!(a.ok(&["verify"]).contains("OK"));
    assert!(c.ok(&["verify"]).contains("OK"));
}

#[test]
fn pushing_the_same_lines_again_changes_nothing() {
    let Some(net) = Net::new() else { return };
    let a = net.machine("a", "omar@a", "omar");
    net.start(&a, &["omar"]);
    a.ok(&["log", "one"]);
    a.ok(&["log", "two"]);
    let before = frontier(&net, &a.token());
    let lines = a.lines("omar@a");
    let (status, r) = post(
        &format!("{}/v1/p/demo/push", net.url()),
        &a.token(),
        &json!({ "writer": "omar@a", "lines": lines }),
    );
    assert_eq!(status, 200, "{r}");
    assert_eq!(r["accepted"], 0, "{r}");
    assert_eq!(r["duplicates"], lines.len(), "{r}");
    assert_eq!(frontier(&net, &a.token()), before);

    // Losing the local sync state costs requests, never data.
    fs::remove_file(a.home.join(".kontext/server.json")).unwrap();
    let out = a.ok(&["sync"]);
    assert!(out.contains("Nothing to push"), "{out}");
    assert!(out.contains("Nothing new arrived"), "{out}");
    assert_eq!(frontier(&net, &a.token()), before);
    assert_eq!(a.lines("omar@a"), lines);
}

#[test]
fn a_token_only_writes_as_its_own_writer() {
    let Some(net) = Net::new() else { return };
    let a = net.machine("a", "omar@a", "omar");
    let b = net.machine("b", "ana@b", "ana");
    net.start(&a, &["omar"]);
    net.join(&a, &b);
    a.ok(&["log", "from omar"]);
    // B's token tries to push A's lines (they continue the server's chain).
    let (status, r) = post(
        &format!("{}/v1/p/demo/push", net.url()),
        &b.token(),
        &json!({ "writer": "omar@a", "lines": a.lines("omar@a") }),
    );
    assert_eq!(status, 403, "{r}");
    assert_eq!(r["refused"]["reason"], "writer", "{r}");
    // A token for this project does not open another one.
    let (status, _) = post(
        &format!("{}/v1/p/other/push", net.url()),
        &b.token(),
        &json!({ "writer": "ana@b", "lines": [] }),
    );
    assert_eq!(status, 401);
}

#[test]
fn offline_writes_stay_local_and_go_up_when_the_server_is_back() {
    let Some(mut net) = Net::persistent() else {
        return;
    };
    let a = net.machine("a", "omar@a", "omar");
    let b = net.machine("b", "ana@b", "ana");
    net.start(&a, &["omar", "ana"]);
    net.join(&a, &b);
    a.ok(&["log", "while online"]);

    net.server.stop();
    let t = Instant::now();
    b.ok(&["log", "offline 1"]);
    b.ok(&["log", "offline 2"]);
    a.ok(&["log", "A offline too"]);
    let out = b.ok(&["standup", "--peek"]);
    assert!(out.contains("offline 2"), "{out}");
    assert!(
        t.elapsed() < Duration::from_secs(20),
        "commands waited on the network: {:?}",
        t.elapsed()
    );
    let status = b.ok(&["status"]);
    assert!(status.contains("Last error"), "{status}");
    let out = b.run(&[], &["sync"]);
    assert!(!out.status.success(), "sync with no server must fail");

    net.server.restart();
    let out = b.ok(&["sync"]);
    assert!(out.contains("Pushed 2 entries (ana@b: 2)"), "{out}");
    let out = a.ok(&["sync"]);
    assert!(out.contains("Pushed 1 entry (omar@a: 1)"), "{out}");
    assert!(out.contains("Received 2 entries (ana@b: 2)"), "{out}");
    let out = b.ok(&["sync"]);
    assert!(out.contains("Received 1 entry (omar@a: 1)"), "{out}");
    assert_eq!(a.ledger(), b.ledger());
    let status = b.ok(&["status"]);
    assert!(!status.contains("Last error"), "{status}");
    assert!(status.contains("Own entries not pushed: 0"), "{status}");
}

#[test]
fn connect_uploads_an_existing_project_with_its_history() {
    let Some(net) = Net::new() else { return };
    let a = net.machine("a", "omar@a", "omar");
    // A project that synced with git (no remote: commits only), two writers.
    let out = a.ok(&["init", "demo", "--home", a.home.to_str().unwrap()]);
    assert!(out.contains("Project demo created"), "{out}");
    set_owners(&a.home, &["omar"]);
    let second = a.config.parent().unwrap().join("config-second");
    fs::create_dir_all(&second).unwrap();
    fs::write(
        second.join("projects.toml"),
        format!(
            "writer = \"omar@old-laptop\"\n\n[projects.demo]\nhome = \"{}\"\n",
            a.home.display()
        ),
    )
    .unwrap();
    for i in 0..3 {
        a.ok(&["log", &format!("A {i}")]);
        a.ok_env(
            &[("KONTEXT_CONFIG_DIR", second.to_str().unwrap())],
            &["log", &format!("old laptop {i}")],
        );
    }
    a.ok(&[
        "decide",
        "Keep the mockup",
        "--attach",
        capture("payment-v2.png").to_str().unwrap(),
    ]);
    a.ok(&["standup"]);
    a.ok_env(
        &[("KONTEXT_CONFIG_DIR", second.to_str().unwrap())],
        &["standup"],
    );
    let out = a.ok(&["sync"]);
    assert!(out.contains("Commit "), "{out}");
    let git_log = |home: &Path| {
        let o = Command::new("git")
            .args(["log", "--format=%H"])
            .current_dir(home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        String::from_utf8(o.stdout).unwrap()
    };
    let history = git_log(&a.home);
    let ledger = a.ledger();
    assert_eq!(ledger.len(), 2);

    let out = a.ok_env(
        &[("KONTEXT_ADMIN_SECRET", ADMIN)],
        &["server", "connect", net.url()],
    );
    assert!(out.contains("Pushed"), "{out}");
    // Nothing local changed: the same lines, the same git history.
    assert_eq!(a.ledger(), ledger);
    assert_eq!(git_log(&a.home), history);
    let f = frontier(&net, &a.token());
    assert_eq!(f["tails"]["omar@a"]["n"], a.lines("omar@a").len());
    assert_eq!(f["tails"]["omar@old-laptop"]["n"], 3);
    // Running it again uploads nothing more.
    let out = a.ok_env(
        &[("KONTEXT_ADMIN_SECRET", ADMIN)],
        &["server", "connect", net.url()],
    );
    assert!(out.contains("Nothing to push"), "{out}");

    // A new machine gets everything: lines, cursors of both writers, media.
    let b = net.machine("b", "ana@b", "ana");
    net.join(&a, &b);
    assert_eq!(b.ledger(), ledger);
    assert_eq!(
        files_in(&b.home.join("media")),
        files_in(&a.home.join("media"))
    );
    for f in [
        "cursors/omar/omar@a.json",
        "cursors/omar/omar@old-laptop.json",
    ] {
        assert!(b.home.join(f).exists(), "{f} missing on B");
    }
    let out = b.ok(&["verify"]);
    assert!(out.contains("in 2 writers"), "{out}");
}

/// `kontext ui` running on A; killed when dropped.
struct Ui {
    child: Child,
    addr: String,
}

impl Drop for Ui {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn ui_shows_another_machines_write_through_the_websocket() {
    let Some(net) = Net::new() else { return };
    let a = net.machine("a", "omar@a", "omar");
    let b = net.machine("b", "ana@b", "ana");
    net.start(&a, &["omar", "ana"]);
    net.join(&a, &b);

    let mut child = a
        .cmd(&["ui", "--port", "0"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let addr = line
        .trim()
        .strip_prefix("kontext ui: http://")
        .unwrap()
        .trim_end_matches('/')
        .to_string();
    let ui = Ui { child, addr };

    // A foreign Host is still refused, on the event stream too.
    let mut s = TcpStream::connect(&ui.addr).unwrap();
    s.write_all(b"GET /api/events HTTP/1.1\r\nHost: evil.example\r\n\r\n")
        .unwrap();
    let mut refused = String::new();
    s.read_to_string(&mut refused).unwrap();
    assert!(refused.starts_with("HTTP/1.1 403"), "{refused}");

    let mut events = TcpStream::connect(&ui.addr).unwrap();
    events
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    write!(
        events,
        "GET /api/events HTTP/1.1\r\nHost: {}\r\n\r\n",
        ui.addr
    )
    .unwrap();
    let mut seen = String::new();
    let mut buf = [0u8; 4096];
    while !seen.contains("retry: 3000") {
        let n = events.read(&mut buf).unwrap();
        seen.push_str(&String::from_utf8_lossy(&buf[..n]));
    }
    assert!(seen.contains("Content-Type: text/event-stream"), "{seen}");
    assert!(
        seen.contains("Content-Security-Policy: default-src 'none'"),
        "{seen}"
    );
    // Let the UI's WebSocket open and its first pull settle.
    std::thread::sleep(Duration::from_secs(2));
    seen.clear();

    // B writes; A runs no command, only the UI: the WebSocket brings it.
    b.ok(&["log", "live from B"]);
    let deadline = Instant::now() + Duration::from_secs(15);
    while !a.lines("ana@b").iter().any(|l| l.contains("live from B")) {
        assert!(Instant::now() < deadline, "B's line never reached A");
        std::thread::sleep(Duration::from_millis(200));
    }
    while !seen.contains("event: change") {
        let n = events.read(&mut buf).expect("no change event");
        assert!(n > 0, "the event stream closed");
        seen.push_str(&String::from_utf8_lossy(&buf[..n]));
    }
    let mut s = TcpStream::connect(&ui.addr).unwrap();
    write!(
        s,
        "GET /api/state HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
        ui.addr
    )
    .unwrap();
    let mut state = String::new();
    s.read_to_string(&mut state).unwrap();
    assert!(state.contains("live from B"), "{state}");
}

/// The binary before phase 4 (`KONTEXT_OLD_BIN`, by default the one built from
/// e2cd98f in /tmp/kontext-new) reads a copy that syncs with a server: the
/// ledger format did not change, and it ignores `server =` in projects.toml.
#[test]
fn an_old_binary_reads_a_synced_copy() {
    let old = std::env::var("KONTEXT_OLD_BIN")
        .unwrap_or_else(|_| "/tmp/kontext-new/bin/kontext".to_string());
    if !Path::new(&old).is_file() {
        eprintln!("skipped: no old kontext binary at {old} (set KONTEXT_OLD_BIN)");
        return;
    }
    let Some(net) = Net::new() else { return };
    let a = net.machine("a", "omar@a", "omar");
    let b = net.machine("b", "ana@b", "ana");
    net.start(&a, &["omar", "ana"]);
    net.join(&a, &b);
    a.ok(&["log", "from A"]);
    a.ok(&[
        "decide",
        "A decision with a picture",
        "--attach",
        capture("payment-v2.png").to_str().unwrap(),
    ]);
    b.ok(&["log", "from B"]);
    b.ok(&["standup"]);
    b.ok(&["sync"]);

    let run = |args: &[&str]| {
        let out = Command::new(&old)
            .args(args)
            .env("KONTEXT_CONFIG_DIR", &b.config)
            .env("KONTEXT_PROJECT", "demo")
            // omar never read on B, so the standup shows everything.
            .env("KONTEXT_ACTOR", "human:omar")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", &b.gitconfig)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "old kontext {args:?}: {}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    };
    let out = run(&["verify"]);
    assert!(out.contains("OK") && out.contains("in 2 writers"), "{out}");
    let out = run(&["standup", "--peek"]);
    assert!(
        out.contains("from A") && out.contains("A decision with a picture"),
        "{out}"
    );
    let out = run(&["status"]);
    assert!(out.contains("Chains: OK"), "{out}");
}
