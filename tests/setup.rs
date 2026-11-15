//! `kontext setup`: from nothing to installed and syncing in one command, as an
//! agent runs it (no terminal). The server is the real `server/worker.js` in a
//! local workerd (`tests/common/server.rs`); "the human deployed it" is played
//! by writing the local server and its admin secret in `servers.json`, as
//! `kontext server deploy` does (the deploy itself is tested against a fake
//! Cloudflare API in `tests/server_deploy.rs`).

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use common::BIN;
use common::server::{ADMIN, LocalServer};
use serde_json::{Value, json};

/// One machine: its own HOME, config folder and git identity.
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

    /// A new code repo (`git init`) in this machine's HOME.
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

    /// The local server, as `kontext server deploy` leaves it in servers.json.
    fn deployed(&self, url: &str) {
        fs::write(
            self.config.join("servers.json"),
            json!({ "servers": [{ "url": url, "account_id": "acc", "script": "kontext", "admin_secret": ADMIN }] })
                .to_string(),
        )
        .unwrap();
    }

    fn cmd(&self, dir: &Path, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
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
            .env_remove("SSH_AUTH_SOCK");
        c
    }

    fn run(&self, dir: &Path, args: &[&str]) -> Output {
        self.cmd(dir, args).output().unwrap()
    }

    fn setup(&self, dir: &Path, extra: &[&str]) -> (Option<i32>, Value, String) {
        let mut a = vec!["setup", "--json"];
        a.extend_from_slice(extra);
        let o = self.run(dir, &a);
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
        let v = serde_json::from_slice(&o.stdout).unwrap_or(Value::Null);
        (o.status.code(), v, text)
    }
}

fn statuses(v: &Value) -> BTreeMap<String, String> {
    v["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["step"].as_str().unwrap().to_string(),
                s["status"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

/// Every file under `dir`, but the internal `.kontext/` folder (index, locks)
/// and `.git/`.
fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if name == ".kontext" || name == ".git" {
                continue;
            }
            if e.file_type().unwrap().is_dir() {
                stack.push(p);
            } else {
                out.insert(
                    p.strip_prefix(dir).unwrap().to_path_buf(),
                    fs::read(&p).unwrap(),
                );
            }
        }
    }
    out
}

#[test]
fn setup_stops_for_the_human_then_finishes_and_then_changes_nothing() {
    let Some(server) = LocalServer::start() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", "omar@a");
    let repo = a.repo("billing");

    // No server on this machine and no Cloudflare token: the project and the
    // install are done, and it stops with 4 saying what the human does.
    let (code, v, text) = a.setup(&repo, &[]);
    assert_eq!(code, Some(4), "{text}");
    let s = statuses(&v);
    assert_eq!(s["project"], "done", "{text}");
    assert_eq!(s["install"], "done", "{text}");
    assert_eq!(s["server"], "needed", "{text}");
    assert_eq!(v["project"], "billing");
    assert_eq!(v["ok"], false);
    let n = &v["needs_human"];
    assert_eq!(n["reason"], "cloudflare_token");
    assert_eq!(n["exit_code"], 4);
    // As it runs here: the test binary is not the `kontext` on the PATH.
    assert!(
        n["then"].as_str().unwrap().ends_with("/kontext setup"),
        "{text}"
    );
    assert_eq!(v["command"], "setup");
    assert!(
        n["open"]
            .as_str()
            .unwrap()
            .starts_with("https://dash.cloudflare.com/profile/api-tokens?permissionGroupKeys=")
    );
    assert!(n["run"].as_str().unwrap().ends_with(" server deploy"));
    assert!(repo.join(".kontext.toml").exists());
    assert!(repo.join(".claude/settings.json").exists());
    let home = a.base.join("home/kontext-home/billing");
    assert!(home.join("project.toml").exists());

    // The text says the same, and what to tell the human.
    let o = a.run(&repo, &["setup"]);
    assert_eq!(o.status.code(), Some(4));
    let t = String::from_utf8_lossy(&o.stdout);
    assert!(t.contains("server          needed"), "{t}");
    assert!(t.contains("Needs you:"), "{t}");
    assert!(t.contains("word for word"), "{t}");

    // The human deployed (servers.json has the server and its admin secret).
    a.deployed(&server.url);
    let o = a.run(
        &repo,
        &["--actor", "human:omar", "log", "written before the server"],
    );
    assert!(o.status.success());
    let (code, v, text) = a.setup(&repo, &[]);
    assert_eq!(code, Some(0), "{text}");
    let s = statuses(&v);
    assert_eq!(s["project"], "already", "{text}");
    assert_eq!(s["install"], "already", "{text}");
    assert_eq!(s["server"], "already", "{text}");
    assert_eq!(s["server project"], "done", "{text}");
    assert_eq!(s["sync"], "done", "{text}");
    assert_eq!(v["ok"], true);
    assert!(v["needs_human"].is_null());
    assert_eq!(v["server"], server.url.as_str());
    // The token is saved (0600), never printed.
    let token = fs::read_to_string(a.config.join("tokens/billing.token")).unwrap();
    assert!(!text.contains(token.trim()), "{text}");
    // Every command syncs with it from now on.
    let o = a.run(&repo, &["status"]);
    let t = String::from_utf8_lossy(&o.stdout);
    assert!(t.contains(&server.url), "{t}");
    assert!(t.contains("Own entries not pushed: 0"), "{t}");

    // Again: everything already, and nothing changes in the repo or the home.
    let before = (snapshot(&repo), snapshot(&home));
    let (code, v, text) = a.setup(&repo, &[]);
    assert_eq!(code, Some(0), "{text}");
    let s = statuses(&v);
    for step in ["project", "install", "server", "server project"] {
        assert_eq!(s[step], "already", "{step}: {text}");
    }
    assert_eq!(s["sync"], "done");
    assert_eq!(
        v["steps"][4]["detail"], "nothing new to send or bring",
        "{text}"
    );
    assert_eq!((snapshot(&repo), snapshot(&home)), before);

    // A second project on the same account: one command.
    let repo2 = a.repo("catalog");
    let (code, v, text) = a.setup(&repo2, &[]);
    assert_eq!(code, Some(0), "{text}");
    let s = statuses(&v);
    assert_eq!(s["project"], "done");
    assert_eq!(s["install"], "done");
    assert_eq!(s["server"], "already");
    assert_eq!(s["server project"], "done");
    assert_eq!(v["project"], "catalog");
}

#[test]
fn setup_refuses_what_it_should_not_do_and_says_what_to_run() {
    let Some(server) = LocalServer::start() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", "omar@a");
    let b = Machine::new(dir.path(), "b", "omar@b");

    // Not in a git repo.
    let plain = a.base.join("home/plain");
    fs::create_dir_all(&plain).unwrap();
    let (code, _, text) = a.setup(&plain, &[]);
    assert_eq!(code, Some(2), "{text}");
    assert!(text.contains("git init"), "{text}");

    // A project made on A, then B (which also has the admin secret) runs
    // setup for a repo of the same name: it is not made twice, B joins.
    a.deployed(&server.url);
    let ra = a.repo("billing");
    let (code, _, text) = a.setup(&ra, &[]);
    assert_eq!(code, Some(0), "{text}");
    b.deployed(&server.url);
    let rb = b.repo("billing");
    let (code, _, text) = b.setup(&rb, &[]);
    assert_eq!(code, Some(2), "{text}");
    assert!(text.contains("kontext invite"), "{text}");
    assert!(!b.base.join("home/kontext-home/billing").exists());

    // --project: another name.
    let (code, v, text) = b.setup(&rb, &["--project", "billing-b"]);
    assert_eq!(code, Some(0), "{text}");
    assert_eq!(v["project"], "billing-b");

    // Two servers known: it asks which, with the command.
    let both = json!({ "servers": [
        { "url": server.url, "admin_secret": ADMIN },
        { "url": "http://127.0.0.1:9", "admin_secret": "x" },
    ] });
    fs::write(a.config.join("servers.json"), both.to_string()).unwrap();
    let rc = a.repo("notes");
    let (code, _, text) = a.setup(&rc, &[]);
    assert_eq!(code, Some(2), "{text}");
    assert!(text.contains("kontext setup --server <url>"), "{text}");
    let (code, _, text) = a.setup(&rc, &["--server", &server.url]);
    assert_eq!(code, Some(0), "{text}");

    // --no-server: the project and the install only.
    let c = Machine::new(dir.path(), "c", "omar@c");
    let rd = c.repo("local-only");
    let (code, v, text) = c.setup(&rd, &["--no-server"]);
    assert_eq!(code, Some(0), "{text}");
    assert_eq!(statuses(&v)["server"], "skipped");
    assert!(!c.config.join("servers.json").exists());
}

#[test]
fn setup_help_is_enough_for_an_agent() {
    let o = Command::new(BIN)
        .args(["setup", "--help"])
        .output()
        .unwrap();
    let t = String::from_utf8_lossy(&o.stdout);
    for want in [
        "Exit codes",
        "4 a step needs the human",
        "never ask for the token in the chat",
        "kontext setup` again",
        "kontext invite",
        "--json",
    ] {
        assert!(t.contains(want), "{want}: {t}");
    }
}

#[test]
fn a_server_that_does_not_answer_leaves_the_local_steps_done_and_exit_0() {
    let dir = tempfile::tempdir().unwrap();
    // One that accepts and never answers, and one that is down.
    let hung = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let hung_url = format!("http://{}", hung.local_addr().unwrap());
    let down_url = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", l.local_addr().unwrap())
    };
    for (n, url) in [("hung", &hung_url), ("down", &down_url)] {
        let a = Machine::new(dir.path(), n, "omar@a");
        a.deployed(url);
        let r = a.repo("billing");
        let started = std::time::Instant::now();
        let (code, v, text) = a.setup(&r, &[]);
        assert!(
            started.elapsed().as_secs() < 15,
            "{n}: took {:?}",
            started.elapsed()
        );
        assert_eq!(code, Some(0), "{n}: {text}");
        assert_eq!(v["ok"], false, "{text}");
        assert_eq!(v["offline"], true, "{text}");
        let s = statuses(&v);
        assert_eq!(s["project"], "done", "{text}");
        assert_eq!(s["install"], "done", "{text}");
        assert_eq!(s["server project"], "needed", "{text}");
        assert_eq!(s["sync"], "needed", "{text}");
        assert!(
            v["next"].as_str().unwrap().contains("setup` again when"),
            "{text}"
        );
        // As text, the same, with no error.
        let o = a.run(&r, &["setup"]);
        let t = String::from_utf8_lossy(&o.stdout);
        assert!(o.status.success(), "{t}");
        assert!(t.contains("did not answer"), "{t}");
        // The ledger still takes entries.
        let o = a.run(&r, &["--actor", "human:omar", "log", "offline"]);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    }
    drop(hung);
}
