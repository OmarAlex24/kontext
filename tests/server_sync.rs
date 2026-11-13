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
    /// Its own writer's last line on the server, from the frontier.
    fn server_tail(&self, net: &Net) -> u64 {
        frontier(net, &self.token())["tails"][self.writer()]["n"]
            .as_u64()
            .unwrap_or(0)
    }

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
    let projects = c.config.join("projects.toml");
    let registered = fs::read_to_string(&projects).unwrap();
    fs::write(
        &projects,
        registered.replace(net.url(), "http://127.0.0.1:9"),
    )
    .unwrap();
    c.ok(&["log", "written on C"]);
    fs::write(&projects, &registered).unwrap();
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

/// A proxy in front of the local server that stalls every media download
/// (`GET …/media/<name>`) for `stall`: a large file on a slow link.
fn slow_media_proxy(upstream: &str, stall: Duration) -> String {
    stalling_proxy(upstream, stall, |req| {
        req.starts_with("GET ") && req.contains("/media/")
    })
}

/// A proxy in front of the local server that stalls the requests whose first
/// line `slow` picks. One request per connection (it answers with
/// `Connection: close`).
fn stalling_proxy(upstream: &str, stall: Duration, slow: fn(&str) -> bool) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = format!("http://{}", listener.local_addr().unwrap());
    let up = upstream.trim_start_matches("http://").to_string();
    std::thread::spawn(move || {
        for client in listener.incoming().flatten() {
            let up = up.clone();
            std::thread::spawn(move || {
                let mut client = client;
                let mut r = BufReader::new(client.try_clone().unwrap());
                let mut head = String::new();
                loop {
                    let mut l = String::new();
                    if r.read_line(&mut l).unwrap_or(0) == 0 {
                        return;
                    }
                    let end = l == "\r\n";
                    head.push_str(&l);
                    if end {
                        break;
                    }
                }
                let len: usize = head
                    .lines()
                    .find_map(|l| {
                        let (k, v) = l.split_once(':')?;
                        k.eq_ignore_ascii_case("content-length")
                            .then(|| v.trim().parse().ok())?
                    })
                    .unwrap_or(0);
                let mut body = vec![0u8; len];
                r.read_exact(&mut body).unwrap();
                let first = head.lines().next().unwrap_or("").to_string();
                if slow(&first) {
                    std::thread::sleep(stall);
                }
                // Forwarded with the same client the binary uses, so the
                // answer's framing is read right whatever workerd does.
                let mut parts = first.split(' ');
                let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
                let headers: Vec<(String, String)> = head
                    .lines()
                    .skip(1)
                    .filter_map(|l| l.split_once(':'))
                    .filter(|(k, _)| {
                        k.eq_ignore_ascii_case("authorization")
                            || k.eq_ignore_ascii_case("content-type")
                    })
                    .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
                    .collect();
                let h: Vec<(&str, &str)> = headers
                    .iter()
                    .map(|(k, v)| (k.as_str(), v.as_str()))
                    .collect();
                let body = (method != "GET").then_some(body.as_slice());
                let Ok(r) = Http::new(Duration::from_secs(30)).request(
                    method,
                    &format!("http://{up}{path}"),
                    &h,
                    body,
                ) else {
                    return;
                };
                let mut answer = format!(
                    "HTTP/1.1 {} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    r.status,
                    r.body.len()
                )
                .into_bytes();
                answer.extend_from_slice(&r.body);
                let _ = client.write_all(&answer);
            });
        }
    });
    addr
}

/// The reviewer's sequence: a media file that cannot come down within the
/// automatic pull's 2.5 s (a slow link) made that pull fail before the push
/// and put both on a 30 s backoff, so B's own writes stopped going up. Now
/// the push goes first and on its own, the automatic pull brings no media,
/// and the file comes down when a view needs it.
#[test]
fn a_slow_media_download_never_holds_back_the_push() {
    let Some(net) = Net::new() else { return };
    let a = net.machine("a", "omar@a", "omar");
    let b = net.machine("b", "ana@b", "ana");
    net.start(&a, &["omar", "ana"]);
    let proxy = slow_media_proxy(net.url(), Duration::from_secs(4));
    let token = net.token(&a, "ana", "owner", "ana@b");
    let out = b.ok_env(
        &[("KONTEXT_TOKEN", &token)],
        &[
            "clone",
            "--server",
            &proxy,
            "demo",
            "--home",
            b.home.to_str().unwrap(),
        ],
    );
    assert!(out.contains("Project demo cloned"), "{out}");

    a.ok(&[
        "decide",
        "The screen follows the mockup",
        "--attach",
        capture("payment-v2.png").to_str().unwrap(),
    ]);
    // B's commands: each pulls A's decision first (with its picture cited)
    // and pushes what it wrote.
    let t = Instant::now();
    b.ok(&["log", "first from B"]);
    b.ok(&["log", "second from B"]);
    assert!(
        t.elapsed() < Duration::from_secs(12),
        "B's commands waited on the media: {:?}",
        t.elapsed()
    );
    assert_eq!(
        b.server_tail(&net),
        b.lines("ana@b").len() as u64,
        "B's lines did not go up: {}",
        b.ok(&["status"])
    );
    assert!(
        b.lines("omar@a")
            .iter()
            .any(|l| l.contains("The screen follows"))
    );
    // The automatic pull brought no media.
    assert!(files_in_or_empty(&b.home.join("media")).is_empty());
    let status = b.ok(&["status"]);
    assert!(!status.contains("Last error"), "{status}");
    // A view that needs the file brings it (through the slow link, on demand).
    let out = b.ok(&["verify"]);
    assert!(out.contains("OK"), "{out}");
    assert_eq!(
        files_in(&b.home.join("media")),
        files_in(&a.home.join("media"))
    );
}

/// When the pull fails (here it never answers in time) the push still goes,
/// on that command and the next ones, and only the pull backs off.
#[test]
fn a_failed_pull_never_holds_back_the_push() {
    let Some(net) = Net::new() else { return };
    let a = net.machine("a", "omar@a", "omar");
    let b = net.machine("b", "ana@b", "ana");
    net.start(&a, &["omar", "ana"]);
    let proxy = stalling_proxy(net.url(), Duration::from_secs(4), |req| {
        req.starts_with("POST ") && req.contains("/pull ")
    });
    let token = net.token(&a, "ana", "owner", "ana@b");
    b.ok_env(
        &[("KONTEXT_TOKEN", &token)],
        &[
            "clone",
            "--server",
            net.url(),
            "demo",
            "--home",
            b.home.to_str().unwrap(),
        ],
    );
    let projects = b.config.join("projects.toml");
    let t = fs::read_to_string(&projects).unwrap();
    fs::write(&projects, t.replace(net.url(), &proxy)).unwrap();
    a.ok(&["log", "from A"]);
    let t = Instant::now();
    b.ok(&["log", "first from B"]);
    b.ok(&["log", "second from B"]);
    assert!(t.elapsed() < Duration::from_secs(12), "{:?}", t.elapsed());
    assert_eq!(
        b.server_tail(&net),
        b.lines("ana@b").len() as u64,
        "B's lines did not go up: {}",
        b.ok(&["status"])
    );
    let status = b.ok(&["status"]);
    assert!(
        status.contains("Last error") && status.contains("pull"),
        "{status}"
    );
}

fn files_in_or_empty(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    if dir.is_dir() {
        files_in(dir)
    } else {
        BTreeMap::new()
    }
}

/// A line kontext writes in a project with a server is never one the server
/// can't take: over 4 MiB it is refused before it is written (exit 2); up to
/// it, it goes up and comes down byte for byte.
#[test]
fn a_line_over_the_servers_cap_is_refused_before_it_is_written() {
    let Some(net) = Net::new() else { return };
    let a = net.machine("a", "omar@a", "omar");
    let b = net.machine("b", "ana@b", "ana");
    net.start(&a, &["omar", "ana"]);
    net.join(&a, &b);
    let entry = |body_len: usize| {
        json!({ "kind": "journal", "title": "a long one", "body": "x".repeat(body_len) })
            .to_string()
    };
    let append = |m: &Machine, line: String| {
        let mut c = m.cmd(&["debug", "append"]);
        c.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = c.spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(format!("{line}\n").as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    };
    let before = a.lines("omar@a");
    let out = append(&a, entry(4 * 1024 * 1024));
    assert_eq!(
        out.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("4 MiB"));
    assert_eq!(a.lines("omar@a"), before, "nothing was written");
    // Just under the cap: written, pushed, and B gets the same bytes.
    let out = append(&a, entry(3 * 1024 * 1024));
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(a.server_tail(&net), a.lines("omar@a").len() as u64);
    b.ok(&["sync"]);
    assert_eq!(a.ledger(), b.ledger());
}

/// `server connect` goes on past a writer the server refuses (a line over the
/// cap, written before the project had a server, where nothing limits it),
/// uploads every other writer, says which one was left out and why, and
/// exits non-zero.
#[test]
fn connect_goes_on_past_a_refused_writer() {
    let Some(net) = Net::new() else { return };
    let a = net.machine("a", "omar@a", "omar");
    let out = a.ok(&["init", "demo", "--home", a.home.to_str().unwrap()]);
    assert!(out.contains("Project demo created"), "{out}");
    set_owners(&a.home, &["omar"]);
    // A writer that sorts first, with one line over 4 MiB (no server yet: no
    // limit), and the machine's own writer after it.
    let big = a.config.parent().unwrap().join("config-big");
    fs::create_dir_all(&big).unwrap();
    fs::write(
        big.join("projects.toml"),
        format!(
            "writer = \"aaa@big\"\n\n[projects.demo]\nhome = \"{}\"\n",
            a.home.display()
        ),
    )
    .unwrap();
    let mut c = a.cmd(&["debug", "append"]);
    c.env("KONTEXT_CONFIG_DIR", &big)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = c.spawn().unwrap();
    let line = json!({ "kind": "journal", "title": "huge", "body": "y".repeat(5 * 1024 * 1024) });
    child
        .stdin
        .take()
        .unwrap()
        .write_all(format!("{line}\n").as_bytes())
        .unwrap();
    let o = child.wait_with_output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    a.ok(&["log", "own 1"]);
    a.ok(&["log", "own 2"]);

    let out = a.run(
        &[("KONTEXT_ADMIN_SECRET", ADMIN)],
        &["server", "connect", net.url()],
    );
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert_eq!(
        out.status.code(),
        Some(1),
        "{text}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(text.contains("REFUSED by the server: aaa@big"), "{text}");
    assert!(text.contains("too_large"), "{text}");
    assert!(text.contains("omar@a: 2"), "{text}");
    assert!(
        !text.contains("every command now syncs with it (`kontext status`"),
        "{text}"
    );
    let f = frontier(&net, &a.token());
    assert_eq!(f["tails"]["omar@a"]["n"], 2, "{f}");
    assert!(f["tails"].get("aaa@big").is_none(), "{f}");
    // The status says what was left out.
    let status = a.ok(&["status"]);
    assert!(
        status.contains("REFUSED by the server: aaa@big"),
        "{status}"
    );
}

/// `status --json` of a project without a server has no `server` key, as
/// before servers existed.
#[test]
fn status_json_has_no_server_key_without_a_server() {
    let p = common::Project::new();
    let j = p.kontext_ok(&["--json", "status"]);
    let v: Value = serde_json::from_str(&j).unwrap();
    assert!(v.get("server").is_none(), "{j}");
}

/// The environment never takes a registered project somewhere else: on a
/// project without a server, `KONTEXT_SERVER` and `KONTEXT_TOKEN` (even a
/// malformed one) change nothing; on one with a server, a token is only sent
/// to the server it is registered with.
#[test]
fn the_environment_does_not_take_over_a_registered_project() {
    // A listener that notes any connection: no token may ever reach it.
    let trap = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    trap.set_nonblocking(true).unwrap();
    let trap_url = format!("http://{}", trap.local_addr().unwrap());

    let p = common::Project::new();
    p.kontext_ok(&["log", "git only"]);
    let plain = |args: &[&str]| p.kontext_ok(args);
    let with_env = |args: &[&str], token: &str| {
        p.kontext_env_ok(
            &[("KONTEXT_SERVER", &trap_url), ("KONTEXT_TOKEN", token)],
            args,
        )
    };
    let demo_token = {
        // A well-formed token for a project named demo, for another writer.
        let head = "eyJwIjoiZGVtbyIsInciOiJzb21lb25lQGVsc2V3aGVyZSJ9";
        format!("kxt1.{head}.c2VjcmV0")
    };
    for token in ["not-a-token", demo_token.as_str()] {
        let a = plain(&["status"]);
        let b = with_env(&["status"], token);
        assert_eq!(a, b, "status changed with the environment");
        assert!(!b.contains("Server:"), "{b}");
        let j = with_env(&["--json", "status"], token);
        let v: Value = serde_json::from_str(&j).unwrap();
        assert!(v.get("server").is_none(), "{j}");
        assert_eq!(v["writer"], "omar@test", "{j}");
        let out = with_env(&["sync"], token);
        assert!(out.contains("Commit") || out.contains("Nothing"), "{out}");
    }
    assert!(
        trap.accept().is_err(),
        "something connected to KONTEXT_SERVER"
    );

    // A project with a server: KONTEXT_SERVER pointing elsewhere is ignored,
    // and its token never goes there.
    let Some(net) = Net::new() else { return };
    let a = net.machine("a", "omar@a", "omar");
    net.start(&a, &["omar"]);
    a.ok_env(
        &[("KONTEXT_SERVER", &trap_url), ("KONTEXT_TOKEN", &a.token())],
        &["log", "to its own server"],
    );
    assert_eq!(a.server_tail(&net), a.lines("omar@a").len() as u64);
    assert!(trap.accept().is_err(), "the token went to KONTEXT_SERVER");
}

/// A copy that started without a server joins it with a token alone: the
/// admin secret is only for creating the project and uploading every
/// writer. A saved token never goes to another server.
#[test]
fn a_copy_without_a_server_joins_it_with_a_token() {
    let Some(net) = Net::new() else { return };
    let a = net.machine("a", "omar@a", "omar");
    net.start(&a, &["omar", "ana"]);
    let c = net.machine("c", "ana@laptop", "ana");
    let out = c.ok(&["init", "demo", "--home", c.home.to_str().unwrap()]);
    assert!(out.contains("Project demo created"), "{out}");
    c.ok(&["log", "written before the server"]);
    let mine = c.lines("ana@laptop");
    assert_eq!(mine.len(), 1);

    // Without a token, it says how to get one.
    let out = c.run(&[], &["server", "connect", net.url()]);
    let text = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(!out.status.success(), "{text}");
    assert!(
        text.contains("KONTEXT_TOKEN=<token> kontext server connect"),
        "{text}"
    );

    let token = net.token(&a, "ana", "owner", "ana@laptop");
    let out = c.ok_env(
        &[("KONTEXT_TOKEN", &token)],
        &["server", "connect", net.url()],
    );
    assert!(out.contains("Project demo is on"), "{out}");
    assert_eq!(c.token(), token);
    a.ok(&["sync"]);
    assert_eq!(a.lines("ana@laptop"), mine);

    // The saved token is for that server only.
    let trap = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    trap.set_nonblocking(true).unwrap();
    let trap_url = format!("http://{}", trap.local_addr().unwrap());
    let out = c.run(&[], &["server", "connect", &trap_url]);
    assert!(!out.status.success());
    // Nor does one in KONTEXT_TOKEN (a sandbox always has it set).
    let out = c.run(
        &[("KONTEXT_TOKEN", &token)],
        &["server", "connect", &trap_url],
    );
    let text = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(!out.status.success(), "{text}");
    assert!(text.contains("no token of it is sent"), "{text}");
    assert!(trap.accept().is_err(), "a token went to another server");
    // A token the server does not take never replaces the saved one.
    let bad = token.rsplit_once('.').unwrap().0.to_string() + ".not-the-secret";
    let out = c.run(
        &[("KONTEXT_TOKEN", &bad)],
        &["server", "connect", net.url()],
    );
    assert!(!out.status.success());
    assert_eq!(c.token(), token);

    c.ok(&["log", "after connecting"]);
    assert_eq!(c.server_tail(&net), 2);
}

/// The push goes before the pull, so a fresh clone pushed before it had
/// recorded the config it had just brought, and said "the server has no
/// project.toml yet". A config file the server has, byte for byte, is
/// nothing to push and nothing to say.
#[test]
fn a_clone_says_nothing_about_config_the_server_has() {
    let Some(net) = Net::new() else { return };
    let a = net.machine("a", "omar@a", "omar");
    net.start(&a, &["omar", "ana"]);
    let b = net.machine("b", "ana@b", "ana");
    let token = net.token(&a, "ana", "owner", "ana@b");
    let out = b.run(
        &[("KONTEXT_TOKEN", &token)],
        &[
            "clone",
            "--server",
            net.url(),
            "demo",
            "--home",
            b.home.to_str().unwrap(),
        ],
    );
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "{text}");
    assert!(!text.contains("project.toml"), "{text}");
    let out = b.ok(&["sync"]);
    assert!(!out.contains("project.toml"), "{out}");
}

// ------------------------------------------------------------ tokens (§14.2)

/// A charter, a goal and the epic `payments` on `a` (an owner).
fn plan(a: &Machine) {
    let id = |out: String| -> String {
        serde_json::from_str::<Value>(&out).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let c = id(a.ok(&["--json", "charter", "--objective", "Charge by card"]));
    a.ok(&["sign", &c]);
    let g = id(a.ok(&["--json", "goal", "add", "Recurring charge"]));
    a.ok(&["sign", &g]);
    a.ok(&[
        "epic",
        "add",
        "Card payments",
        "--goal",
        &g,
        "--name",
        "payments",
    ]);
}

/// `kontext --json token create …` on `a`.
fn mint(a: &Machine, args: &[&str]) -> Value {
    let mut all = vec!["--json", "token", "create"];
    all.extend_from_slice(args);
    serde_json::from_str(&a.ok(&all)).unwrap()
}

/// A sandbox: an empty config folder and only the variables the token
/// printed. Nothing is registered there.
struct Sandbox {
    _dir: tempfile::TempDir,
    config: PathBuf,
    env: Vec<(String, String)>,
}

impl Sandbox {
    fn new(minted: &Value) -> Sandbox {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        let env = minted["env"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| {
                let (k, v) = l.as_str().unwrap().split_once('=').unwrap();
                (k.to_string(), v.to_string())
            })
            .collect();
        Sandbox {
            _dir: dir,
            config,
            env,
        }
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .env("KONTEXT_CONFIG_DIR", &self.config)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env_remove("KONTEXT_ACTOR")
            .env_remove("KONTEXT_ROLE")
            .env_remove("KONTEXT_ADMIN_SECRET")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .current_dir(self._dir.path());
        for (k, v) in &self.env {
            c.env(k, v);
        }
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd(args).output().unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "[sandbox] kontext {args:?} failed ({:?}): {}\n{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    fn home(&self) -> PathBuf {
        self.config.join("homes/demo")
    }

    fn ledger_bytes(&self, writer: &str) -> Vec<u8> {
        fs::read(self.home().join("ledger").join(format!("{writer}.jsonl"))).unwrap_or_default()
    }
}

#[test]
fn a_coordinator_sandbox_sets_itself_up_and_writes_only_as_its_role() {
    let Some(net) = Net::new() else { return };
    let a = net.machine("a", "omar@a", "omar");
    net.start(&a, &["omar"]);
    plan(&a);
    let m = mint(&a, &["--role", "coordinator", "--epic", "payments"]);
    let writer = m["writer"].as_str().unwrap().to_string();
    assert!(
        writer.starts_with("omar.coordinator.payments.") && writer.ends_with("@kontext"),
        "{writer}"
    );
    assert_eq!(m["actors"], json!(["epic:payments"]));
    let env: Vec<&str> = m["env"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(
        env.contains(&"KONTEXT_ACTOR=agent:epic:payments"),
        "{env:?}"
    );
    assert!(env.contains(&"KONTEXT_ROLE=coordinator"), "{env:?}");
    assert!(env.contains(&"KONTEXT_PROJECT=demo"), "{env:?}");

    // The first command sets the project up (several at once: only once).
    let s = Sandbox::new(&m);
    let kids: Vec<Child> = (0..3)
        .map(|_| {
            s.cmd(&["status"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    let mut set_up = 0;
    for k in kids {
        let out = k.wait_with_output().unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{err}");
        set_up += err.matches("set up project demo").count();
    }
    assert_eq!(set_up, 1);
    let projects = fs::read_to_string(s.config.join("projects.toml")).unwrap();
    assert_eq!(projects.matches("[projects.demo]").count(), 1, "{projects}");
    assert!(s.home().join("project.toml").exists());

    // What a coordinator does: its brief, tasks, a claim on behalf of an
    // implementer, the implementer's journal and done, a question.
    let out = s.ok(&["brief", "payments"]);
    assert!(out.contains("payments"), "{out}");
    let t = serde_json::from_str::<Value>(&s.ok(&[
        "--json",
        "task",
        "add",
        "Charge the card",
        "--epic",
        "payments",
    ]))
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    s.ok(&[
        "--actor",
        "agent:impl-1",
        "--role",
        "implementer",
        "next",
        "--epic",
        "payments",
        "--claim",
    ]);
    s.ok(&[
        "--actor",
        "agent:impl-1",
        "--role",
        "implementer",
        "log",
        "SDK wired",
    ]);
    s.ok(&[
        "--actor",
        "agent:impl-1",
        "--role",
        "implementer",
        "done",
        &t,
        "--run",
        "true",
        "--since",
        "-",
    ]);
    s.ok(&["ask", "Which currency?", "--to", "pm"]);

    // What it may not write is refused before it is in the file (exit 2):
    // a decision by the project's own rule, the rest by the token's.
    let before = s.ledger_bytes(&writer);
    let out = s.run(&["decide", "Use Stripe", "--why", "fast"]);
    assert_eq!(out.status.code(), Some(2));
    for args in [
        vec!["charter", "--objective", "Something else"],
        vec!["--actor", "human:omar", "log", "pretending"],
        vec!["--actor", "agent:epic:other", "log", "another epic"],
        vec!["--actor", "agent:pm", "--role", "pm", "log", "as the pm"],
    ] {
        let out = s.run(&args);
        let err = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {err}");
        assert!(
            err.contains("the server") && err.contains("would refuse"),
            "{err}"
        );
    }
    assert_eq!(
        s.ledger_bytes(&writer),
        before,
        "a refused line was written"
    );

    // Everything it wrote reached the owner.
    let out = a.ok(&["sync"]);
    assert!(out.contains(&format!("{writer}: ")), "{out}");
    assert_eq!(
        a.lines(&writer),
        String::from_utf8(before)
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect::<Vec<_>>()
    );
    let board = a.ok(&["brief", "payments"]);
    assert!(board.contains("Charge the card"), "{board}");
    assert!(a.ok(&["verify"]).contains("OK"));
    let status = s.ok(&["status"]);
    assert!(status.contains("Own entries not pushed: 0"), "{status}");

    // Tokens: listed without secrets, and a revoked one stops at once.
    let list = a.ok(&["token", "list"]);
    assert!(
        list.contains("omar as owner") && list.contains("omar as coordinator"),
        "{list}"
    );
    assert!(!list.contains("kxt1."), "{list}");
    a.ok(&["token", "revoke", m["id"].as_str().unwrap()]);
    let out = s.run(&["sync"]);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success(), "{text}");
    assert!(text.contains("revoked"), "{text}");
}

#[test]
fn a_line_the_role_may_not_write_waits_until_the_writer_gets_a_token_that_may() {
    let Some(net) = Net::new() else { return };
    let a = net.machine("a", "omar@a", "omar");
    net.start(&a, &["omar"]);
    // B wrote as itself before it had a token: a decision, as a human.
    let b = net.machine("b", "ana@b", "ana");
    net.join(&a, &b);
    let impl_token = |a: &Machine| {
        mint(
            a,
            &[
                "--role",
                "implementer",
                "--person",
                "ana",
                "--writer",
                "ana@laptop",
            ],
        )
    };
    let c = net.machine("c", "ana@laptop", "ana");
    let out = c.ok(&["init", "demo", "--home", c.home.to_str().unwrap()]);
    assert!(out.contains("Project demo created"), "{out}");
    c.ok(&["--actor", "agent:helper", "log", "an ordinary note"]);
    c.ok(&["decide", "Ana decides", "--why", "she was here first"]);
    // Now it gets an implementer token for its writer and the server: the
    // agent's note goes up, the human's decision is refused, and it waits there.
    let m = impl_token(&a);
    let token = m["token"].as_str().unwrap();
    let out = c.run(
        &[("KONTEXT_TOKEN", token)],
        &["server", "connect", net.url()],
    );
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert_eq!(out.status.code(), Some(1), "{text}");
    assert!(text.contains("REFUSED by the server"), "{text}");
    assert!(text.contains("identity"), "{text}");
    assert!(
        text.contains("kontext token create --writer ana@laptop"),
        "{text}"
    );
    let f = frontier(&net, &a.token());
    assert_eq!(f["tails"]["ana@laptop"]["n"], 1, "{f}");
    let status = c.ok(&["status"]);
    assert!(status.contains("REFUSED by the server"), "{status}");
    // With that token, nothing more can be written as a human here.
    let out = c.run(&[], &["log", "more"]);
    assert_eq!(out.status.code(), Some(2));

    // The owner revokes it and makes one for the same writer that may: the
    // rest goes up and nothing was lost.
    a.ok(&["token", "revoke", m["id"].as_str().unwrap()]);
    let m = mint(
        &a,
        &[
            "--role",
            "owner",
            "--person",
            "ana",
            "--writer",
            "ana@laptop",
        ],
    );
    let token = m["token"].as_str().unwrap();
    let out = c.ok_env(
        &[("KONTEXT_TOKEN", token)],
        &["server", "connect", net.url()],
    );
    assert!(out.contains("Pushed 1 entry (ana@laptop: 1)"), "{out}");
    let status = c.ok(&["status"]);
    assert!(!status.contains("REFUSED"), "{status}");
    b.ok(&["sync"]);
    assert_eq!(b.lines("ana@laptop"), c.lines("ana@laptop"));
    assert_eq!(b.lines("ana@laptop").len(), 2);
}
