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
        // The code repo's clone "in the sandbox": a folder of the test.
        let repo = w.dir.path().join("sbx").join("demo");
        w.configure(&format!(
            "\n[dispatch]\nrepo_dir = {:?}\n",
            repo.to_str().unwrap()
        ));
        w.ok(&["sync", "--include-config"]);
        Some(w)
    }

    fn repo(&self) -> PathBuf {
        self.dir.path().join("sbx").join("demo")
    }

    /// A file of the turns' folder.
    fn turn_file(&self, epic: &str, n: u32, ext: &str) -> PathBuf {
        self.config
            .join("dispatch/demo/turns")
            .join(format!("{epic}-{n}.{ext}"))
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

    /// Adds lines at the end of project.toml (its last table is `[dispatch]`).
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

impl Drop for World {
    /// Every turn the fake boat holds ends, and the test waits for the
    /// turns' processes to say so: none outlives it.
    fn drop(&mut self) {
        let _ = fs::write(self.fake.join("end-all"), "");
        let turns = self.config.join("dispatch/demo/turns");
        let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < until {
            let open = fs::read_dir(&turns)
                .map(|rd| {
                    rd.flatten().any(|e| {
                        let p = e.path();
                        p.extension().is_some_and(|x| x == "started")
                            && !p.with_extension("exit").exists()
                    })
                })
                .unwrap_or(false);
            if !open {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
}

/// Waits up to 15 s for `f`.
fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    let until = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while !f() {
        assert!(
            std::time::Instant::now() < until,
            "timed out waiting for {what}"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
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
    w.configure("max_sandboxes = 1\n");
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

#[test]
fn the_coordinator_is_prompted_in_a_process_of_its_own() {
    let Some(w) = World::new() else { return };
    w.plan(true);
    let o = w.run(&["dispatch", "foundations"]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("start script done"), "{}", text(&o));
    // The start script went with the sandbox; Boat was asked how it went.
    assert!(w.fake.join("bx_fake1.setup-file").exists());
    let calls = w.calls();
    assert!(calls.contains("--setup-file "), "{calls}");
    assert!(calls.contains("info bx_fake1 --json"), "{calls}");
    let run = w.run_of("foundations");
    let epic = run["epic"].as_str().unwrap().to_string();
    assert_eq!(run["turns"], 1);
    // The prompt, held open: claude with opus-5-5, a new conversation,
    // pointing at the brief.
    wait_for("the conversation id", || {
        w.run_of("foundations")["conversation"] == "cv_bx_fake1"
    });
    let prompt = w
        .calls()
        .lines()
        .find(|l| l.starts_with("prompt "))
        .unwrap()
        .to_string();
    assert!(
        prompt.starts_with("prompt bx_fake1 --json --provider claude --model opus-5-5 --new "),
        "{prompt}"
    );
    assert!(
        prompt.contains(
            "kontext --actor agent:epic:foundations --role coordinator brief foundations"
        ),
        "{prompt}"
    );
    assert!(prompt.contains("kontext-coordinator.md"), "{prompt}");
    assert!(prompt.contains("one stack of pull requests"), "{prompt}");
    // The turn runs until Boat says it ended; then its process says so.
    assert!(!w.turn_file(&epic, 1, "exit").exists());
    fs::write(w.fake.join("end-bx_fake1-1"), "").unwrap();
    wait_for("the turn's end", || {
        fs::read_to_string(w.turn_file(&epic, 1, "exit")).is_ok_and(|s| s.trim() == "ended")
    });
}

#[test]
fn a_turn_that_cannot_be_followed_is_lost_not_ended() {
    let Some(w) = World::new() else { return };
    w.plan(true);
    w.ok(&["dispatch", "foundations"]);
    let epic = w.run_of("foundations")["epic"]
        .as_str()
        .unwrap()
        .to_string();
    fs::write(w.fake.join("end-bx_fake1-1"), "1").unwrap();
    wait_for("the turn's end", || w.turn_file(&epic, 1, "exit").exists());
    let s = fs::read_to_string(w.turn_file(&epic, 1, "exit")).unwrap();
    assert!(s.starts_with("lost: boat prompt failed"), "{s}");
}

#[test]
fn a_failed_start_script_stops_the_sandbox_and_revokes_the_token() {
    let Some(w) = World::new() else { return };
    w.plan(true);
    fs::write(w.fake.join("bx_fake1.setup"), "failed").unwrap();
    fs::write(w.fake.join("bx_fake1.setup-error"), "exit code 7").unwrap();
    let o = w.run(&["dispatch", "foundations"]);
    assert_eq!(code(&o), Some(1), "{}", text(&o));
    assert!(
        text(&o).contains("start script failed: exit code 7"),
        "{}",
        text(&o)
    );
    assert!(w.fake.join("bx_fake1.stopped").exists());
    let run = w.run_of("foundations");
    assert_eq!(run["state"], "stuck");
    assert_eq!(run["token_revoked"], true);
    assert!(!w.calls().contains("prompt "), "{}", w.calls());
}

/// The start script, run as the sandbox would: a home of its own, the token
/// file where Boat writes it, the variables of `-e`, a fake `curl` whose
/// install.sh copies this kontext, and the code repo already cloned.
#[test]
fn the_start_script_sets_the_sandbox_up_and_removes_the_token_file() {
    let Some(w) = World::new() else { return };
    w.plan(true);
    w.ok(&["dispatch", "foundations"]);
    let sbx = w.dir.path().join("sbx");
    fs::create_dir_all(w.repo()).unwrap();
    let git = |args: &[&str]| {
        let o = Command::new("git")
            .args(args)
            .current_dir(w.repo())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", w.dir.path().join("gitconfig"))
            .output()
            .unwrap();
        assert!(o.status.success(), "git {args:?}: {}", text(&o));
        String::from_utf8(o.stdout).unwrap()
    };
    git(&["init", "-q"]);
    fs::write(w.repo().join("README.md"), "demo\n").unwrap();
    git(&["add", "README.md"]);
    git(&["commit", "-qm", "first"]);
    let token_file = sbx.join(".kontext-dispatch-token");
    fs::copy(w.fake.join("files/.kontext-dispatch-token"), &token_file).unwrap();
    let bin = w.dir.path().join("fakebin");
    fs::create_dir_all(&bin).unwrap();
    fs::write(
        bin.join("curl"),
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >>\"$FAKE_BOAT_DIR/curl.log\"\ncat <<'EOS'\nset -eu\n\
         mkdir -p \"$KONTEXT_INSTALL_DIR\"\ncp \"$KONTEXT_TEST_BIN\" \"$KONTEXT_INSTALL_DIR/kontext\"\nEOS\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(bin.join("curl"), fs::Permissions::from_mode(0o755)).unwrap();
    let vars: Vec<(String, String)> = fs::read_to_string(w.fake.join("bx_fake1.env"))
        .unwrap()
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let sandbox = |c: &mut Command| {
        c.env_clear()
            .env("HOME", &sbx)
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env("KONTEXT_TEST_BIN", BIN)
            .env("FAKE_BOAT_DIR", &w.fake)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", w.dir.path().join("gitconfig"));
        for (k, v) in &vars {
            c.env(k, v);
        }
    };
    let mut c = Command::new("sh");
    c.arg(w.fake.join("bx_fake1.setup-file"));
    sandbox(&mut c);
    let o = c.output().unwrap();
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    assert!(!out.contains("kxt1."), "{out}");
    assert!(
        out.contains("ready: project demo, epic foundations"),
        "{out}"
    );
    assert!(!token_file.exists(), "the token file is still there");
    // The pinned release.
    let curl = fs::read_to_string(w.fake.join("curl.log")).unwrap();
    assert!(
        curl.contains(&format!(
            "https://raw.githubusercontent.com/OmarAlex24/kontext/v{}/install.sh",
            env!("CARGO_PKG_VERSION")
        )),
        "{curl}"
    );
    // The project, its token saved by kontext (0600).
    let cfg = sbx.join(".config/kontext");
    let projects = fs::read_to_string(cfg.join("projects.toml")).unwrap();
    assert!(projects.contains("[projects.demo]"), "{projects}");
    let mode = fs::metadata(cfg.join("tokens/demo.token"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o077, 0, "{mode:o}");
    // The hooks, skills and agent files, out of git's way.
    assert!(w.repo().join(".kontext.toml").exists());
    assert!(
        w.repo()
            .join(".claude/agents/kontext-coordinator.md")
            .exists()
    );
    assert!(w.repo().join(".claude/settings.local.json").exists());
    assert_eq!(git(&["status", "--porcelain"]), "");
    // The coordinator's first command works there.
    let mut c = Command::new(sbx.join(".local/bin/kontext"));
    c.args(["brief", "foundations"]).current_dir(w.repo());
    sandbox(&mut c);
    let o = c.output().unwrap();
    assert!(o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("Foundations"), "{}", text(&o));
    // Run again (a sandbox restarted midway), it changes nothing that is right.
    let mut c = Command::new("sh");
    c.arg(w.fake.join("bx_fake1.setup-file"));
    sandbox(&mut c);
    let o = c.output().unwrap();
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(git(&["status", "--porcelain"]), "");
}
