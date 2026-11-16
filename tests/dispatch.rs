//! `kontext dispatch` (DESIGN §12.5) against a fake `boat`
//! (`tests/fixtures/fake-boat.sh`, through `KONTEXT_BOAT`) and the local
//! kontext server (`tests/common/server.rs`). Nothing here talks to Boat.
//! Every process runs with a throwaway `HOME` and `KONTEXT_CONFIG_DIR`.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use common::BIN;
use common::server::{ADMIN, LocalServer};
use serde_json::Value;

struct World {
    dir: tempfile::TempDir,
    server: LocalServer,
    config: PathBuf,
    kx_home: PathBuf,
    boat: PathBuf,
    fake: PathBuf,
}

impl World {
    /// The project `demo` on the local server, with omar as its owner.
    fn new() -> Option<World> {
        let server = LocalServer::start()?;
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        fs::create_dir_all(&config).unwrap();
        fs::write(config.join("projects.toml"), "writer = \"omar@mac\"\n").unwrap();
        fs::write(
            dir.path().join("gitconfig"),
            "[user]\n\tname = omar\n\temail = omar@example.com\n[init]\n\tdefaultBranch = main\n\
             [commit]\n\tgpgsign = false\n",
        )
        .unwrap();
        let fake = dir.path().join("fake-boat");
        fs::create_dir_all(&fake).unwrap();
        let boat = dir.path().join("bin").join("boat");
        fs::create_dir_all(boat.parent().unwrap()).unwrap();
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-boat.sh"),
            &boat,
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&boat, fs::Permissions::from_mode(0o755)).unwrap();
        let kx_home = dir.path().join("kx-home");
        let w = World {
            dir,
            server,
            config,
            kx_home,
            boat,
            fake,
        };
        let out = w.ok_env(
            &[("KONTEXT_ADMIN_SECRET", ADMIN)],
            &[
                "init",
                "demo",
                "--home",
                w.kx_home.to_str().unwrap(),
                "--server",
                &w.server.url,
            ],
        );
        assert!(out.contains("Project demo created"), "{out}");
        w.set_owners();
        w.ok(&["sync", "--include-config"]);
        Some(w)
    }

    fn set_owners(&self) {
        let toml = self.kx_home.join("project.toml");
        let text: String = fs::read_to_string(&toml)
            .unwrap()
            .lines()
            .map(|l| {
                if l.starts_with("owners = ") {
                    "owners = [\"omar\"]\n".to_string()
                } else {
                    format!("{l}\n")
                }
            })
            .collect();
        fs::write(&toml, text).unwrap();
    }

    /// Adds lines at the end of project.toml (a `[dispatch]` section).
    fn configure(&self, extra: &str) {
        let toml = self.kx_home.join("project.toml");
        let mut text = fs::read_to_string(&toml).unwrap();
        text.push_str(extra);
        fs::write(&toml, text).unwrap();
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .env("HOME", self.dir.path())
            .env("KONTEXT_CONFIG_DIR", &self.config)
            .env("KONTEXT_PROJECT", "demo")
            .env("KONTEXT_ACTOR", "human:omar")
            .env("KONTEXT_BOAT", &self.boat)
            .env("FAKE_BOAT_DIR", &self.fake)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.dir.path().join("gitconfig"))
            .env_remove("KONTEXT_ROLE")
            .env_remove("KONTEXT_TOKEN")
            .env_remove("KONTEXT_SERVER")
            .env_remove("KONTEXT_ADMIN_SECRET")
            .env_remove("KONTEXT_EPIC")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env_remove("SSH_AUTH_SOCK")
            .current_dir(self.dir.path());
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd(args).output().unwrap()
    }

    fn ok_env(&self, envs: &[(&str, &str)], args: &[&str]) -> String {
        let mut c = self.cmd(args);
        for (k, v) in envs {
            c.env(k, v);
        }
        let out = c.output().unwrap();
        assert!(
            out.status.success(),
            "kontext {args:?} failed ({:?}): {}\n{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        self.ok_env(&[], args)
    }

    fn id(&self, args: &[&str]) -> String {
        let mut a = vec!["--json"];
        a.extend_from_slice(args);
        let v: Value = serde_json::from_str(&self.ok(&a)).unwrap();
        v["id"].as_str().unwrap().to_string()
    }

    /// Charter and goal signed; epics `foundations` (two tasks), `payments`
    /// after it, and `small` (one task); the plan signed.
    fn plan(&self, sign: bool) {
        let c = self.id(&["charter", "--objective", "Charge by card"]);
        self.ok(&["sign", &c]);
        let g = self.id(&["goal", "add", "Recurring charge"]);
        self.ok(&["sign", &g]);
        for (title, name, after) in [
            ("Foundations", "foundations", None),
            ("Payments", "payments", Some("foundations")),
            ("Small fix", "small", None),
        ] {
            let mut a = vec!["epic", "add", title, "--goal", &g, "--name", name];
            if let Some(x) = after {
                a.extend(["--after", x]);
            }
            self.ok(&a);
        }
        for (t, e) in [
            ("Schema", "foundations"),
            ("Migrations", "foundations"),
            ("Card charge", "payments"),
            ("Fix the typo", "small"),
        ] {
            self.ok(&["task", "add", t, "--epic", e, "--acceptance", "tests pass"]);
        }
        if sign {
            let s: Value = serde_json::from_str(&self.ok(&["--json", "scope", "summary"])).unwrap();
            self.ok(&["scope", "sign", "--hash", s["hash"].as_str().unwrap()]);
        }
    }

    fn calls(&self) -> String {
        fs::read_to_string(self.fake.join("calls.log")).unwrap_or_default()
    }

    fn state(&self) -> Value {
        let p = self.config.join("dispatch/demo/state.json");
        serde_json::from_str(&fs::read_to_string(p).unwrap()).unwrap()
    }

    fn run_of(&self, name: &str) -> Value {
        let s = self.state();
        s["runs"]
            .as_object()
            .unwrap()
            .values()
            .find(|r| r["name"] == name)
            .cloned()
            .unwrap_or_else(|| panic!("no run of {name}: {s}"))
    }

    fn tokens(&self) -> Vec<Value> {
        let v: Value = serde_json::from_str(&self.ok(&["--json", "token", "list"])).unwrap();
        v.as_array().unwrap().clone()
    }

    fn token(&self, id: &str) -> Value {
        self.tokens()
            .into_iter()
            .find(|t| t["id"] == id)
            .unwrap_or_else(|| panic!("no token {id}"))
    }
}

fn code(o: &Output) -> Option<i32> {
    o.status.code()
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

/// Every file under `dir`, as text.
fn all_text(dir: &Path) -> String {
    let mut s = String::new();
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                s.push_str(&all_text(&p));
            } else if let Ok(t) = fs::read(&p) {
                s.push_str(&String::from_utf8_lossy(&t));
            }
        }
    }
    s
}

#[test]
fn dispatch_refuses_with_one_line_why() {
    let Some(w) = World::new() else { return };
    w.plan(false);
    // The plan is not signed.
    let o = w.run(&["dispatch", "foundations"]);
    assert_eq!(code(&o), Some(2), "{}", text(&o));
    assert!(text(&o).contains("the plan is not signed"), "{}", text(&o));
    let s: Value = serde_json::from_str(&w.ok(&["--json", "scope", "summary"])).unwrap();
    w.ok(&["scope", "sign", "--hash", s["hash"].as_str().unwrap()]);
    // An epic whose wave is not ready, and one that does not exist.
    let o = w.run(&["dispatch", "payments"]);
    assert_eq!(code(&o), Some(2), "{}", text(&o));
    assert!(text(&o).contains("waits for foundations"), "{}", text(&o));
    let o = w.run(&["dispatch", "ghost"]);
    assert_eq!(code(&o), Some(2), "{}", text(&o));
    // Each refusal is one line, and nothing reached Boat.
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(err.trim().lines().count(), 1, "{err}");
    assert_eq!(w.calls(), "");

    // A project without a server.
    let solo = tempfile::tempdir().unwrap();
    let cfg = solo.path().join("config");
    fs::create_dir_all(&cfg).unwrap();
    fs::write(cfg.join("projects.toml"), "writer = \"omar@solo\"\n").unwrap();
    let run = |args: &[&str]| {
        Command::new(BIN)
            .args(args)
            .env("HOME", solo.path())
            .env("KONTEXT_CONFIG_DIR", &cfg)
            .env("KONTEXT_PROJECT", "solo")
            .env("KONTEXT_BOAT", &w.boat)
            .env("FAKE_BOAT_DIR", &w.fake)
            .env_remove("KONTEXT_TOKEN")
            .env_remove("KONTEXT_SERVER")
            .current_dir(solo.path())
            .output()
            .unwrap()
    };
    let home = solo.path().join("home");
    assert!(
        run(&["init", "solo", "--home", home.to_str().unwrap()])
            .status
            .success()
    );
    let o = run(&["dispatch", "x"]);
    assert_eq!(code(&o), Some(2), "{}", text(&o));
    assert!(
        text(&o).contains("does not sync through a kontext server"),
        "{}",
        text(&o)
    );
}

#[test]
fn dispatch_makes_a_token_and_a_sandbox_and_the_token_travels_only_as_a_file() {
    let Some(w) = World::new() else { return };
    w.plan(true);
    let o = w.run(&["dispatch", "foundations"]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    assert!(out.contains("sandbox bx_fake1"), "{out}");

    // Boat was asked for a sandbox named after the project and the epic,
    // with the variables that are not secret and the token as a file.
    let calls = w.calls();
    let new = calls.lines().find(|l| l.starts_with("new ")).unwrap();
    for part in [
        "--name kontext-demo-foundations",
        "-e KONTEXT_PROJECT=demo",
        "-e KONTEXT_ACTOR=agent:epic:foundations",
        "-e KONTEXT_ROLE=coordinator",
        "-e KONTEXT_EPIC=foundations",
        "--file .kontext-dispatch-token=",
        "--type default",
        "--ttl 32400",
    ] {
        assert!(new.contains(part), "{part} not in {new}");
    }
    assert!(
        new.contains(&format!("-e KONTEXT_SERVER={}", w.server.url)),
        "{new}"
    );
    // The token reached the sandbox as a file, and nowhere else.
    let sent = fs::read_to_string(w.fake.join("files/.kontext-dispatch-token")).unwrap();
    assert!(sent.starts_with("kxt1."), "{sent}");
    assert!(!calls.contains("kxt1."), "{calls}");
    assert!(!out.contains("kxt1."), "{out}");
    let dispatch_dir = w.config.join("dispatch");
    assert!(!all_text(&dispatch_dir).contains("kxt1."));
    // The local copy is gone.
    let tmp = dispatch_dir.join("demo/tmp");
    assert_eq!(fs::read_dir(&tmp).unwrap().count(), 0);

    // The state, the token and the ledger.
    let run = w.run_of("foundations");
    assert_eq!(run["state"], "running");
    assert_eq!(run["sandbox"], "bx_fake1");
    let tok = w.token(run["token_id"].as_str().unwrap());
    assert_eq!(tok["role"], "coordinator");
    assert!(tok["revoked"].is_null(), "{tok}");
    assert!(tok["expires"].as_i64().is_some(), "{tok}");
    let tree = w.ok(&["tree"]);
    assert!(tree.contains("in a sandbox since"), "{tree}");
    let status = w.ok(&["dispatch", "status"]);
    assert!(
        status.contains("foundations  running  bx_fake1"),
        "{status}"
    );
    assert!(status.contains("0/2 tasks"), "{status}");
    let st: Value = serde_json::from_str(&w.ok(&["--json", "dispatch", "status"])).unwrap();
    assert_eq!(st["live"], 1);
    assert_eq!(st["runs"][0]["progress"]["total"], 2);

    // One sandbox per epic, and the cap.
    let o = w.run(&["dispatch", "foundations"]);
    assert_eq!(code(&o), Some(2), "{}", text(&o));
    assert!(text(&o).contains("already has a sandbox"), "{}", text(&o));
    w.configure("\n[dispatch]\nmax_sandboxes = 1\n");
    let o = w.run(&["dispatch", "small"]);
    assert_eq!(code(&o), Some(2), "{}", text(&o));
    assert!(text(&o).contains("1 sandboxes are running"), "{}", text(&o));
}

#[test]
fn a_single_open_task_warns_and_goes_on() {
    let Some(w) = World::new() else { return };
    w.plan(true);
    let o = w.run(&["dispatch", "small"]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(
        text(&o).contains("warning: epic small has a single open task"),
        "{}",
        text(&o)
    );
}

#[test]
fn stop_stops_the_sandbox_and_revokes_the_token() {
    let Some(w) = World::new() else { return };
    w.plan(true);
    w.ok(&["dispatch", "foundations"]);
    let out = w.ok(&["dispatch", "stop", "foundations"]);
    assert!(out.contains("sandbox bx_fake1 stopped"), "{out}");
    assert!(w.calls().contains("stop bx_fake1"), "{}", w.calls());
    assert!(w.fake.join("bx_fake1.stopped").exists());
    let run = w.run_of("foundations");
    assert_eq!(run["state"], "stopped");
    assert_eq!(run["token_revoked"], true);
    assert!(
        !w.token(run["token_id"].as_str().unwrap())["revoked"].is_null(),
        "the token is not revoked"
    );
    // The ledger has its end, and the tree no longer says it runs.
    let tree = w.ok(&["tree"]);
    assert!(!tree.contains("in a sandbox since"), "{tree}");
    let found = w.ok(&["recall", "sandbox"]);
    assert!(found.contains("stopped by the owner"), "{found}");
    // Again: there is nothing to stop.
    let o = w.run(&["dispatch", "stop", "foundations"]);
    assert_eq!(code(&o), Some(2), "{}", text(&o));
    // And it can be dispatched again.
    let o = w.run(&["dispatch", "foundations"]);
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(w.run_of("foundations")["sandbox"], "bx_fake2");
}

#[test]
fn a_failed_dispatch_stops_what_it_made_and_revokes_the_token() {
    let Some(w) = World::new() else { return };
    w.plan(true);
    fs::write(w.fake.join("fail-new-late"), "").unwrap();
    let o = w.run(&["dispatch", "foundations"]);
    assert_eq!(code(&o), Some(1), "{}", text(&o));
    assert!(text(&o).contains("the machine went away"), "{}", text(&o));
    assert!(w.calls().contains("stop bx_fake1"), "{}", w.calls());
    let run = w.run_of("foundations");
    assert_eq!(run["state"], "stuck");
    assert_eq!(run["sandbox"], "bx_fake1");
    assert_eq!(run["token_revoked"], true);
    assert!(
        run["reason"]
            .as_str()
            .unwrap()
            .contains("the dispatch failed"),
        "{run}"
    );
    assert!(!w.token(run["token_id"].as_str().unwrap())["revoked"].is_null());
    // A failed dispatch does not hold the epic.
    fs::remove_file(w.fake.join("fail-new-late")).unwrap();
    let o = w.run(&["dispatch", "foundations"]);
    assert!(o.status.success(), "{}", text(&o));
    // When Boat makes nothing, there is nothing to stop.
    fs::write(w.fake.join("fail-new"), "").unwrap();
    let o = w.run(&["dispatch", "small"]);
    assert_eq!(code(&o), Some(1), "{}", text(&o));
    assert!(text(&o).contains("no_ready_machine"), "{}", text(&o));
    let run = w.run_of("small");
    assert!(run["sandbox"].is_null(), "{run}");
    assert_eq!(run["token_revoked"], true);
}
