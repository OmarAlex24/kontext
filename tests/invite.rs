//! `kontext invite` on one machine and `kontext join` on another, against the
//! real `server/worker.js` in a local workerd. Each machine has its own HOME,
//! config folder and git identity.

mod common;

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::BIN;
use common::server::{ADMIN, LocalServer};
use serde_json::{Value, json};

struct Machine {
    base: PathBuf,
    config: PathBuf,
    gitconfig: PathBuf,
}

impl Machine {
    fn new(dir: &Path, name: &str, writer: &str) -> Machine {
        let base = dir.join(name);
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
            "[user]\n\tname = Omar\n\temail = omar@example.com\n[init]\n\tdefaultBranch = main\n\
             [commit]\n\tgpgsign = false\n",
        )
        .unwrap();
        fs::create_dir_all(base.join("home")).unwrap();
        Machine {
            base,
            config,
            gitconfig,
        }
    }

    fn repo(&self, name: &str) -> PathBuf {
        let r = self.base.join("home").join("code").join(name);
        fs::create_dir_all(&r).unwrap();
        let st = Command::new("git")
            .args(["init", "-q"])
            .current_dir(&r)
            .env("GIT_CONFIG_GLOBAL", &self.gitconfig)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .status()
            .unwrap();
        assert!(st.success());
        r.canonicalize().unwrap()
    }

    fn run(&self, dir: &Path, args: &[&str]) -> Output {
        Command::new(BIN)
            .args(args)
            .current_dir(dir)
            .env("HOME", self.base.join("home"))
            .env("KONTEXT_CONFIG_DIR", &self.config)
            .env("GIT_CONFIG_GLOBAL", &self.gitconfig)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdin(Stdio::null())
            .env_remove("KONTEXT_PROJECT")
            .env_remove("KONTEXT_ACTOR")
            .env_remove("KONTEXT_ROLE")
            .env_remove("KONTEXT_TOKEN")
            .env_remove("KONTEXT_SERVER")
            .env_remove("KONTEXT_ADMIN_SECRET")
            .env_remove("CLOUDFLARE_API_TOKEN")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env_remove("CLAUDE_CONFIG_DIR")
            .env_remove("SSH_AUTH_SOCK")
            .output()
            .unwrap()
    }

    fn json(&self, dir: &Path, args: &[&str]) -> (Option<i32>, Value, String) {
        let mut a = vec!["--json"];
        a.extend_from_slice(args);
        let o = self.run(dir, &a);
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
        (
            o.status.code(),
            serde_json::from_slice(&o.stdout).unwrap_or(Value::Null),
            text,
        )
    }
}

/// A owns project `billing` on the local server, set up with `kontext setup`.
fn owner(dir: &Path, server: &LocalServer) -> (Machine, PathBuf) {
    let a = Machine::new(dir, "a", "omar@a");
    fs::write(
        a.config.join("servers.json"),
        json!({ "servers": [{ "url": server.url, "admin_secret": ADMIN }] }).to_string(),
    )
    .unwrap();
    let repo = a.repo("billing");
    let (code, _, text) = a.json(&repo, &["setup"]);
    assert_eq!(code, Some(0), "{text}");
    (a, repo)
}

#[test]
fn invite_on_one_machine_and_join_on_another() {
    let Some(server) = LocalServer::start() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let (a, ra) = owner(dir.path(), &server);
    let o = a.run(&ra, &["--actor", "human:omar", "log", "written on A"]);
    assert!(o.status.success());

    // A invites (owner by default): a join string and the lines to run.
    let (code, inv, text) = a.json(&ra, &["invite"]);
    assert_eq!(code, Some(0), "{text}");
    let join = inv["join"].as_str().unwrap().to_string();
    assert!(join.starts_with("kxj1."), "{join}");
    assert_eq!(inv["command"], format!("kontext join {join}"));
    assert!(
        inv["install_command"]
            .as_str()
            .unwrap()
            .ends_with(&format!("install.sh | sh -s -- {join}"))
    );
    assert_eq!(inv["role"], "owner");
    assert_eq!(inv["person"], "omar");
    assert!(inv["writer"].as_str().unwrap().starts_with("omar.owner."));
    // The text says it works once and for a short time; it carries no token.
    let o = a.run(&ra, &["invite", "--role", "scout", "--person", "ana"]);
    let t = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success(), "{t}");
    assert!(t.contains("works once"), "{t}");
    assert!(t.contains("kontext join kxj1."), "{t}");
    assert!(!t.contains("kxt1."), "{t}");

    // B joins in its code repo: one command, project, token and hooks.
    let b = Machine::new(dir.path(), "b", "omar@b");
    let rb = b.repo("billing");
    let (code, v, text) = b.json(&rb, &["join", &join]);
    assert_eq!(code, Some(0), "{text}");
    let steps: Vec<(&str, &str)> = v["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| (s["step"].as_str().unwrap(), s["status"].as_str().unwrap()))
        .collect();
    assert_eq!(steps, [("project", "done"), ("install", "done")], "{text}");
    assert_eq!(v["ok"], true);
    assert_eq!(v["command"], "join");
    // B's own token: saved (0600), never printed.
    let token_file = b.config.join("tokens/billing.token");
    let token = fs::read_to_string(&token_file).unwrap();
    assert!(token.starts_with("kxt1."));
    assert!(!text.contains(token.trim()));
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&token_file).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    assert!(rb.join(".kontext.toml").exists());
    assert!(rb.join(".claude/settings.json").exists());
    // B sees what A wrote, and A sees what B writes.
    let o = b.run(&rb, &["--actor", "human:omar", "log", "written on B"]);
    assert!(o.status.success());
    let o = a.run(&ra, &["standup", "--peek"]);
    let t = String::from_utf8_lossy(&o.stdout);
    assert!(t.contains("written on B"), "{t}");
    let o = b.run(&rb, &["standup", "--peek"]);
    let t = String::from_utf8_lossy(&o.stdout);
    assert!(t.contains("written on A"), "{t}");

    // Again on B: already, and the code is not spent twice; as text, it
    // says which command it is.
    let o = b.run(&rb, &["join", &join]);
    let t = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success(), "{t}");
    assert!(t.contains("kontext join in "), "{t}");
    assert!(t.contains("project         already"), "{t}");
    let (code, v, text) = b.json(&rb, &["join", &join]);
    assert_eq!(code, Some(0), "{text}");
    assert_eq!(v["steps"][0]["status"], "already");
    assert_eq!(v["steps"][1]["status"], "already");

    // A third machine with the same line: used, refused, and it says what to do.
    let c = Machine::new(dir.path(), "c", "omar@c");
    let rc = c.repo("billing");
    let (code, _, text) = c.json(&rc, &["join", &join]);
    assert_eq!(code, Some(2), "{text}");
    assert!(text.contains("already used"), "{text}");
    assert!(text.contains("kontext invite"), "{text}");
    assert!(!c.base.join("home/kontext-home/billing").exists());

    // An expired line.
    let (code, inv, text) = a.json(&ra, &["invite", "--valid-for", "1s"]);
    assert_eq!(code, Some(0), "{text}");
    std::thread::sleep(std::time::Duration::from_millis(1500));
    let (code, _, text) = c.json(&rc, &["join", inv["join"].as_str().unwrap()]);
    assert_eq!(code, Some(2), "{text}");
    assert!(text.contains("expired"), "{text}");

    // Outside a code repo it brings the project and says where to install.
    let d = Machine::new(dir.path(), "d", "omar@d");
    let (_, inv, _) = a.json(&ra, &["invite"]);
    let plain = d.base.join("home");
    let (code, v, text) = d.json(&plain, &["join", inv["join"].as_str().unwrap()]);
    assert_eq!(code, Some(0), "{text}");
    assert_eq!(v["steps"][1]["status"], "skipped");
    assert!(
        v["steps"][1]["detail"]
            .as_str()
            .unwrap()
            .contains("kontext setup --project billing")
    );

    // Not a join string.
    let (code, _, text) = c.json(&rc, &["join", "kxt1.abc.def"]);
    assert_eq!(code, Some(2), "{text}");
    assert!(text.contains("kontext invite"), "{text}");
}

/// base64url without padding.
fn b64url(s: &str) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for c in s.as_bytes().chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..(c.len() + 1) {
            out.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

/// A kontext server deployed before invitations: it answers `/v1/info`
/// without `features`. Every request is logged.
fn old_server() -> (String, Arc<Mutex<Vec<String>>>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    let log = Arc::new(Mutex::new(Vec::new()));
    let lg = log.clone();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut line = String::new();
            if r.read_line(&mut line).unwrap_or(0) == 0 {
                continue;
            }
            let mut len = 0usize;
            loop {
                let mut h = String::new();
                r.read_line(&mut h).unwrap();
                if h.trim().is_empty() {
                    break;
                }
                if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0u8; len];
            r.read_exact(&mut body).unwrap();
            lg.lock().unwrap().push(line.trim().to_string());
            let (status, text) = if line.starts_with("GET /v1/info ") {
                (200, json!({ "kontext_server": 1 }).to_string())
            } else {
                (
                    401,
                    json!({ "error": "unauthorized", "message": "unknown token for this project" })
                        .to_string(),
                )
            };
            let mut s = s;
            let _ = write!(
                s,
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                text.len()
            );
        }
    });
    (url, log)
}

#[test]
fn an_older_server_is_said_to_need_an_update() {
    let (url, log) = old_server();
    let dir = tempfile::tempdir().unwrap();
    let b = Machine::new(dir.path(), "b", "omar@b");
    let rb = b.repo("billing");
    // A join string for the old server: nothing but /v1/info is asked.
    let claims = json!({ "s": url, "p": "billing", "c": "kxi1.abc" }).to_string();
    let b64 = b64url(&claims);
    let (code, v, text) = b.json(&rb, &["join", &format!("kxj1.{b64}")]);
    assert_eq!(code, Some(4), "{text}");
    assert_eq!(v["needs_human"]["reason"], "server_update", "{text}");
    assert_eq!(v["needs_human"]["run"], "kontext server deploy", "{text}");
    assert!(
        v["needs_human"]["then"].as_str().unwrap().contains("join"),
        "{text}"
    );
    assert_eq!(*log.lock().unwrap(), ["GET /v1/info HTTP/1.1"]);
    assert!(!b.config.join("tokens/billing.token").exists());

    // An owner of a project on that server: invite says the same.
    let a = Machine::new(dir.path(), "a", "omar@a");
    let home = a.base.join("home/kh");
    let o = a.run(
        &a.base,
        &["init", "billing", "--home", home.to_str().unwrap()],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let projects = a.config.join("projects.toml");
    let t = fs::read_to_string(&projects).unwrap();
    fs::write(&projects, format!("{t}server = \"{url}\"\n")).unwrap();
    // A token with owner claims for that project (the old server never sees it
    // checked: invite asks /v1/info first).
    let token_claims =
        json!({ "p": "billing", "w": "omar@a", "n": "omar", "r": "owner" }).to_string();
    fs::create_dir_all(a.config.join("tokens")).unwrap();
    fs::write(
        a.config.join("tokens/billing.token"),
        format!("kxt1.{}.secret\n", b64url(&token_claims)),
    )
    .unwrap();
    log.lock().unwrap().clear();
    let (code, v, text) = a.json(&a.base, &["--project", "billing", "invite"]);
    assert_eq!(code, Some(4), "{text}");
    assert_eq!(v["needs_human"]["reason"], "server_update", "{text}");
    assert!(
        v["needs_human"]["then"]
            .as_str()
            .unwrap()
            .ends_with("invite"),
        "{text}"
    );
    assert!(
        !log.lock().unwrap().iter().any(|l| l.contains("invites")),
        "{:?}",
        log.lock().unwrap()
    );

    // A server that is down: join says the line was not used and to run it
    // again; invite says nothing was made; neither hangs.
    let down = {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", l.local_addr().unwrap())
    };
    let claims = json!({ "s": down, "p": "billing", "c": "kxi1.abc" }).to_string();
    let started = std::time::Instant::now();
    let (code, _, text) = b.json(&rb, &["join", &format!("kxj1.{}", b64url(&claims))]);
    assert_eq!(code, Some(1), "{text}");
    assert!(text.contains("did not answer"), "{text}");
    assert!(text.contains("was not used"), "{text}");
    assert!(!b.config.join("tokens/billing.token").exists());
    let t = fs::read_to_string(&projects).unwrap().replace(&url, &down);
    fs::write(&projects, t).unwrap();
    let (code, _, text) = a.json(&a.base, &["--project", "billing", "invite"]);
    assert_eq!(code, Some(1), "{text}");
    assert!(text.contains("did not answer"), "{text}");
    assert!(text.contains("run `kontext invite` again"), "{text}");
    assert!(started.elapsed().as_secs() < 20, "{:?}", started.elapsed());
}

/// A proxy in front of the local server that logs every request (with the
/// token it carries) and, while `hold` is set, answers nothing but
/// `/v1/info` and `POST …/join`: a server that takes the code and then stops
/// answering.
fn proxy(upstream: &str) -> (String, Arc<Mutex<Vec<String>>>, Arc<AtomicBool>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    let up = upstream.trim_start_matches("http://").to_string();
    let log = Arc::new(Mutex::new(Vec::new()));
    let hold = Arc::new(AtomicBool::new(false));
    let (lg, hd) = (log.clone(), hold.clone());
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let (lg, hd, up) = (lg.clone(), hd.clone(), up.clone());
            std::thread::spawn(move || {
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut line = String::new();
                if r.read_line(&mut line).unwrap_or(0) == 0 {
                    return;
                }
                let mut head = Vec::new();
                let (mut len, mut auth) = (0usize, String::new());
                loop {
                    let mut h = String::new();
                    r.read_line(&mut h).unwrap();
                    if h.trim().is_empty() {
                        break;
                    }
                    let low = h.to_ascii_lowercase();
                    if let Some(v) = low.strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap_or(0);
                    }
                    if low.starts_with("authorization:") {
                        auth = h.trim()["authorization:".len()..].trim().to_string();
                    }
                    if !low.starts_with("connection:") {
                        head.push(h);
                    }
                }
                let mut body = vec![0u8; len];
                r.read_exact(&mut body).unwrap();
                let line = line.trim().to_string();
                lg.lock().unwrap().push(format!("{line} {auth}"));
                let open = line.starts_with("GET /v1/info ") || line.contains("/join ");
                if hd.load(Ordering::SeqCst) && !open {
                    std::thread::sleep(Duration::from_secs(30));
                    return;
                }
                let mut u = std::net::TcpStream::connect(&up).unwrap();
                let mut req = format!("{line}\r\n").into_bytes();
                for h in head {
                    req.extend_from_slice(h.as_bytes());
                }
                req.extend_from_slice(b"Connection: close\r\n\r\n");
                req.extend_from_slice(&body);
                u.write_all(&req).unwrap();
                let answer = read_answer(&mut BufReader::new(u));
                let mut s = s;
                let _ = s.write_all(&answer);
            });
        }
    });
    (url, log, hold)
}

/// One HTTP answer, as its bytes: the head, then a body of Content-Length
/// bytes or chunks.
fn read_answer(r: &mut impl BufRead) -> Vec<u8> {
    let mut out = Vec::new();
    let (mut len, mut chunked) = (0usize, false);
    loop {
        let mut h = String::new();
        if r.read_line(&mut h).unwrap_or(0) == 0 {
            return out;
        }
        out.extend_from_slice(h.as_bytes());
        let low = h.to_ascii_lowercase();
        if let Some(v) = low.strip_prefix("content-length:") {
            len = v.trim().parse().unwrap_or(0);
        }
        if low.starts_with("transfer-encoding:") && low.contains("chunked") {
            chunked = true;
        }
        if h.trim().is_empty() {
            break;
        }
    }
    if !chunked {
        let mut body = vec![0u8; len];
        r.read_exact(&mut body).unwrap();
        out.extend_from_slice(&body);
        return out;
    }
    loop {
        let mut size = String::new();
        r.read_line(&mut size).unwrap();
        out.extend_from_slice(size.as_bytes());
        let n = usize::from_str_radix(size.trim().split(';').next().unwrap(), 16).unwrap();
        let mut chunk = vec![0u8; n + 2];
        r.read_exact(&mut chunk).unwrap();
        out.extend_from_slice(&chunk);
        if n == 0 {
            return out;
        }
    }
}

/// A owns `billing`, set up through `url` (a proxy, so A's join lines name it).
fn owner_via(dir: &Path, url: &str) -> (Machine, PathBuf) {
    let a = Machine::new(dir, "a", "omar@a");
    fs::write(
        a.config.join("servers.json"),
        json!({ "servers": [{ "url": url, "admin_secret": ADMIN }] }).to_string(),
    )
    .unwrap();
    let repo = a.repo("billing");
    let (code, v, text) = a.json(&repo, &["setup"]);
    assert_eq!(code, Some(0), "{text}");
    assert_eq!(v["ok"], true, "{text}");
    (a, repo)
}

#[test]
fn a_join_cut_off_after_the_code_resumes_and_its_token_goes_nowhere_else() {
    let Some(server) = LocalServer::start() else {
        return;
    };
    let (url, log, hold) = proxy(&server.url);
    let dir = tempfile::tempdir().unwrap();
    let (a, ra) = owner_via(dir.path(), &url);
    let (code, inv, text) = a.json(&ra, &["invite"]);
    assert_eq!(code, Some(0), "{text}");
    let join = inv["join"].as_str().unwrap().to_string();

    // The server takes the code and then stops answering: within the limit,
    // join says what happened and to run the same line again.
    hold.store(true, Ordering::SeqCst);
    log.lock().unwrap().clear();
    let b = Machine::new(dir.path(), "b", "omar@b");
    let rb = b.repo("billing");
    let started = Instant::now();
    let (code, _, text) = b.json(&rb, &["join", &join]);
    assert!(
        started.elapsed() < Duration::from_secs(13),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(code, Some(1), "{text}");
    // Where it was about to join, before anything was sent.
    assert!(
        text.contains("kontext join: project billing on 127.0.0.1"),
        "{text}"
    );
    assert!(text.contains("took the code"), "{text}");
    assert!(text.contains("Run the same `kontext join"), "{text}");
    assert!(text.contains("does not need the code again"), "{text}");
    assert!(!text.contains("timeout: global"), "{text}");
    let token = b.config.join("tokens/billing.token");
    assert!(token.exists());
    let issuer = fs::read_to_string(b.config.join("tokens/billing.server")).unwrap();
    assert_eq!(issuer.lines().next(), Some(url.as_str()));

    // A line for the same project that names another server: the saved
    // token is not sent there; nothing is sent at all, and it says what to do.
    let (evil, evil_log) = old_server();
    let claims = json!({ "s": evil, "p": "billing", "c": "kxi1.other" }).to_string();
    let (code, _, text) = b.json(&rb, &["join", &format!("kxj1.{}", b64url(&claims))]);
    assert_eq!(code, Some(2), "{text}");
    assert!(text.contains("only ever sent to"), "{text}");
    assert!(text.contains(&token.display().to_string()), "{text}");
    assert!(
        evil_log.lock().unwrap().is_empty(),
        "{:?}",
        evil_log.lock().unwrap()
    );

    // A plain-http line to a host that is not this machine: refused before
    // anything is sent.
    let claims =
        json!({ "s": "http://kontext.example.com", "p": "other", "c": "kxi1.x" }).to_string();
    let (code, _, text) = b.json(&rb, &["join", &format!("kxj1.{}", b64url(&claims))]);
    assert_eq!(code, Some(2), "{text}");
    assert!(text.contains("not https://"), "{text}");

    // The server answers again: the same line finishes, with the saved token
    // and without spending the code again.
    hold.store(false, Ordering::SeqCst);
    let joins_before = log
        .lock()
        .unwrap()
        .iter()
        .filter(|l| l.contains("/join "))
        .count();
    assert_eq!(joins_before, 1);
    let (code, v, text) = b.json(&rb, &["join", &join]);
    assert_eq!(code, Some(0), "{text}");
    assert_eq!(v["steps"][0]["status"], "done", "{text}");
    let joins = log
        .lock()
        .unwrap()
        .iter()
        .filter(|l| l.contains("/join "))
        .count();
    assert_eq!(joins, 1, "{:?}", log.lock().unwrap());
    let o = b.run(
        &rb,
        &["--actor", "human:omar", "log", "from B after the cut"],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    // Another project's saved token, written by a version that kept no record
    // of its server, is not sent to a server other than the one configured.
    let c = Machine::new(dir.path(), "c", "omar@c");
    let rc = c.repo("billing");
    fs::create_dir_all(c.config.join("tokens")).unwrap();
    fs::copy(&token, c.config.join("tokens/billing.token")).unwrap();
    let (_, inv, _) = a.json(&ra, &["invite"]);
    let (code, _, text) = c.json(&rc, &["join", inv["join"].as_str().unwrap()]);
    assert_eq!(code, Some(2), "{text}");
    assert!(
        text.contains("no record of the server that issued it"),
        "{text}"
    );
}

#[test]
fn invitations_are_an_owners_and_die_with_the_token_that_made_them() {
    let Some(server) = LocalServer::start() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let (a, ra) = owner(dir.path(), &server);

    // B joins as an owner, and invites a third machine; then A revokes B's
    // token: B's unused invitation dies with it.
    let (_, inv, _) = a.json(&ra, &["invite"]);
    let b = Machine::new(dir.path(), "b", "omar@b");
    let rb = b.repo("billing");
    let (code, _, text) = b.json(&rb, &["join", inv["join"].as_str().unwrap()]);
    assert_eq!(code, Some(0), "{text}");
    let (code, by_b, text) = b.json(&rb, &["invite", "--role", "implementer", "--person", "ana"]);
    assert_eq!(code, Some(0), "{text}");
    // The note says when, not a placeholder.
    let note = by_b["note"].as_str().unwrap_or_else(|| panic!("{text}"));
    assert!(!note.contains("{when}"), "{note}");
    assert!(note.contains(&jiff_year()), "{note}");
    let (_, list, ltext) = a.json(&ra, &["token", "list"]);
    let writer_b = fs::read_to_string(b.config.join("tokens/billing.token")).unwrap();
    let id = list
        .as_array()
        .unwrap_or_else(|| panic!("{ltext}"))
        .iter()
        .find(|t| {
            t["created_by"]
                .as_str()
                .unwrap_or("")
                .starts_with("invite:")
                && t["revoked"].is_null()
        })
        .map(|t| t["id"].as_str().unwrap().to_string())
        .unwrap_or_else(|| panic!("{ltext} {writer_b}"));
    let o = a.run(&ra, &["token", "revoke", &id]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let c = Machine::new(dir.path(), "c", "ana@c");
    let rc = c.repo("billing");
    let (code, _, text) = c.json(&rc, &["join", by_b["join"].as_str().unwrap()]);
    assert_eq!(code, Some(2), "{text}");
    assert!(text.contains("kontext invite"), "{text}");
    assert!(!c.config.join("tokens/billing.token").exists());

    // An implementer cannot invite: exit 2, and it says who can.
    let (_, inv, _) = a.json(&ra, &["invite", "--role", "implementer", "--person", "ana"]);
    let (code, _, text) = c.json(&rc, &["join", inv["join"].as_str().unwrap()]);
    assert_eq!(code, Some(0), "{text}");
    let (code, _, text) = c.json(&rc, &["invite"]);
    assert_eq!(code, Some(2), "{text}");
    assert!(text.contains("only an owner"), "{text}");
    assert!(text.contains("an implementer token"), "{text}");

    // An invitation is a day at most.
    let (code, _, text) = a.json(&ra, &["invite", "--valid-for", "25h"]);
    assert_eq!(code, Some(2), "{text}");
    assert!(text.contains("a day at most"), "{text}");
    let (code, _, text) = a.json(&ra, &["invite", "--valid-for", "24h"]);
    assert_eq!(code, Some(0), "{text}");
}

/// This year, as the note's timestamp writes it.
fn jiff_year() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    // Good enough for a check: the year the note's date starts with.
    let year = 1970 + secs / 31_556_952;
    format!("{year}-")
}
