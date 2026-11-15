//! `kontext invite` on one machine and `kontext join` on another, against the
//! real `server/worker.js` in a local workerd. Each machine has its own HOME,
//! config folder and git identity.

mod common;

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};

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

    // Again on B: already, and the code is not spent twice.
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
    let (code, _, text) = b.json(&rb, &["join", &format!("kxj1.{b64}")]);
    assert_eq!(code, Some(2), "{text}");
    assert!(text.contains("needs an update"), "{text}");
    assert!(text.contains("kontext server deploy"), "{text}");
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
    let (code, _, text) = a.json(&a.base, &["--project", "billing", "invite"]);
    assert_eq!(code, Some(2), "{text}");
    assert!(text.contains("needs an update"), "{text}");
    assert!(
        !log.lock().unwrap().iter().any(|l| l.contains("invites")),
        "{:?}",
        log.lock().unwrap()
    );
}
