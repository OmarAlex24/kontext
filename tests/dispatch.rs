//! `kontext dispatch` (DESIGN §12.5) against a fake `boat`
//! (`tests/fixtures/fake-boat.sh`, through `KONTEXT_BOAT`) and the local
//! kontext server (`tests/common/server.rs`). Nothing here talks to Boat.
//! Every process runs with a throwaway `HOME` and `KONTEXT_CONFIG_DIR`. The
//! sandbox's scripts run here with a cleared environment, a throwaway home
//! and fakes of `claude`, `gh` and `curl` first on the `PATH` (never a login
//! shell, which could find a real `claude`).

mod common;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use common::BIN;
use common::server::{ADMIN, LocalServer};
use serde_json::Value;

/// The agent's credential in these tests (a made-up OAuth token).
const CRED: &str = "sk-ant-oat01-FAKEcredentialForTests_0123456789";

struct World {
    dir: tempfile::TempDir,
    server: LocalServer,
    config: PathBuf,
    kx_home: PathBuf,
    boat: PathBuf,
    fake: PathBuf,
}

impl World {
    /// The project `demo` on the local server, with omar as its owner and
    /// the agent's credential set.
    fn new() -> Option<World> {
        let w = World::bare()?;
        let out = w.ok_stdin(CRED, &["dispatch", "credential", "set"]);
        assert!(out.contains("a Claude Code OAuth token"), "{out}");
        Some(w)
    }

    /// The same without the credential.
    fn bare() -> Option<World> {
        let dir = tempfile::tempdir().unwrap();
        // Its data kept, so a test can stop the server and start it again.
        let server = LocalServer::start_with(Some(&dir.path().join("server-data")))?;
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
            .env("KONTEXT_DISPATCH_POLL_MS", "100")
            .env("KONTEXT_DISPATCH_FIRST_CHECK_MS", "300")
            .env("KONTEXT_DISPATCH_NEW_GRACE_MS", "0")
            .env_remove("KONTEXT_DISPATCH_TEST_PAUSE")
            .env_remove("KONTEXT_BOAT_TIMEOUT_MS")
            .env_remove("KONTEXT_DISPATCH_STALE_PULL_MS")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.dir.path().join("gitconfig"))
            .env_remove("KONTEXT_NOW")
            .env_remove("KONTEXT_ROLE")
            .env_remove("KONTEXT_TOKEN")
            .env_remove("KONTEXT_SERVER")
            .env_remove("KONTEXT_ADMIN_SECRET")
            .env_remove("KONTEXT_EPIC")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env_remove("CLAUDE_CODE_OAUTH_TOKEN")
            .env_remove("ANTHROPIC_API_KEY")
            .env_remove("SSH_AUTH_SOCK")
            .current_dir(self.dir.path());
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd(args).output().unwrap()
    }

    fn run_stdin(&self, input: &str, args: &[&str]) -> Output {
        let mut child = self
            .cmd(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }

    fn ok_stdin(&self, input: &str, args: &[&str]) -> String {
        let out = self.run_stdin(input, args);
        assert!(out.status.success(), "kontext {args:?}: {}", text(&out));
        String::from_utf8(out.stdout).unwrap()
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
    /// after it, and `small` (one task); the plan signed. Returns the tasks
    /// (epic, id).
    fn plan(&self, sign: bool) -> Vec<(String, String)> {
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
        let mut tasks = Vec::new();
        for (t, e) in [
            ("Schema", "foundations"),
            ("Migrations", "foundations"),
            ("Card charge", "payments"),
            ("Fix the typo", "small"),
        ] {
            let id = self.id(&["task", "add", t, "--epic", e, "--acceptance", "tests pass"]);
            tasks.push((e.to_string(), id));
        }
        if sign {
            let s: Value = serde_json::from_str(&self.ok(&["--json", "scope", "summary"])).unwrap();
            self.ok(&["scope", "sign", "--hash", s["hash"].as_str().unwrap()]);
        }
        tasks
    }

    /// `kontext dispatch status --json`.
    fn status(&self) -> Value {
        serde_json::from_str(&self.ok(&["--json", "dispatch", "status"])).unwrap()
    }

    /// Waits for the run to be in `state`; for a state that is not live,
    /// also for its clean-up and ledger writes to be done (the state is
    /// claimed first, and the service is called after).
    fn wait_state(&self, name: &str, state: &str) {
        let live = ["starting", "running", "resumed"].contains(&state);
        wait_for(&format!("{name} {state}"), || {
            let r = self.run_of(name);
            r["state"] == state
                && (live
                    || ((r["sandbox"].is_null() || r["sandbox_stopped"] == true)
                        && (r["token_id"].is_null() || r["token_revoked"] == true)
                        && r["pm_pending"] != true
                        && r["end_pending"] != true))
        });
    }

    /// The turns Boat was asked to start (`exec … --detach`), in order.
    fn turns(&self) -> Vec<String> {
        self.calls()
            .lines()
            .filter(|l| l.starts_with("exec ") && l.contains(" --detach "))
            .map(str::to_string)
            .collect()
    }

    fn calls(&self) -> String {
        fs::read_to_string(self.fake.join("calls.log")).unwrap_or_default()
    }

    /// Where `what` first shows up in the calls (panics if it does not).
    fn call_at(&self, what: &str) -> usize {
        let calls = self.calls();
        calls
            .lines()
            .position(|l| l.contains(what))
            .unwrap_or_else(|| panic!("no call with {what:?} in:\n{calls}"))
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

    /// Every file of the config folder but the credential's own and the
    /// owner's own server token (`tokens/`), as text.
    fn config_text_but_credential(&self) -> String {
        let keep = self.config.join("dispatch/.agent-credential");
        let tokens = self.config.join("tokens");
        let mut s = String::new();
        walk(&self.config, &mut |p| {
            if p != keep
                && !p.starts_with(&tokens)
                && let Ok(t) = fs::read(p)
            {
                s.push_str(&String::from_utf8_lossy(&t));
            }
        });
        s
    }
}

impl Drop for World {
    /// The watcher stops first, and the test waits for it to let its lock go.
    /// The pid is killed only while it is this world's watcher: its lock is
    /// held and the process is a `kontext … dispatch watch` (a pid file left
    /// behind may name a pid the system gave to another process since).
    fn drop(&mut self) {
        let dir = self.config.join("dispatch/demo");
        if let Ok(pid) = fs::read_to_string(dir.join("watcher.pid"))
            && is_watcher(&dir, pid.trim())
        {
            let _ = Command::new("kill").arg(pid.trim()).output();
        }
        let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < until {
            let held = fs::OpenOptions::new()
                .write(true)
                .open(dir.join("watcher.lock"))
                .is_ok_and(|f| f.try_lock().is_err());
            if !held {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
}

/// Is `pid` the watcher of the dispatch folder `dir`?
fn is_watcher(dir: &Path, pid: &str) -> bool {
    let held = fs::OpenOptions::new()
        .write(true)
        .open(dir.join("watcher.lock"))
        .is_ok_and(|f| f.try_lock().is_err());
    let command = Command::new("ps")
        .args(["-p", pid, "-o", "command="])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();
    held && command.contains("--project demo") && command.contains("dispatch watch")
}

fn walk(dir: &Path, f: &mut dyn FnMut(&Path)) {
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, f);
            } else {
                f(&p);
            }
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
    walk(dir, &mut |p| {
        if let Ok(t) = fs::read(p) {
            s.push_str(&String::from_utf8_lossy(&t));
        }
    });
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
    // Another agent than Claude Code.
    w.configure("provider = \"codex\"\n");
    let o = w.run(&["dispatch", "foundations"]);
    assert_eq!(code(&o), Some(2), "{}", text(&o));
    assert!(text(&o).contains("the only coordinator agent is \"claude\""));
    let toml = w.kx_home.join("project.toml");
    let t = fs::read_to_string(&toml).unwrap();
    fs::write(&toml, t.replace("provider = \"codex\"\n", "")).unwrap();
    // No agent credential on this machine.
    w.ok(&["dispatch", "credential", "rm"]);
    let o = w.run(&["dispatch", "foundations"]);
    assert_eq!(code(&o), Some(2), "{}", text(&o));
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(err.trim().lines().count(), 1, "{err}");
    assert!(err.contains("claude setup-token"), "{err}");
    assert!(err.contains("kontext dispatch credential set"), "{err}");
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
fn the_agent_credential_is_set_from_stdin_kept_0600_and_never_shown() {
    let Some(w) = World::bare() else { return };
    let path = w.config.join("dispatch/.agent-credential");
    let st = w.ok(&["dispatch", "status"]);
    assert!(st.contains("Agent credential: not set"), "{st}");
    assert!(st.contains("claude setup-token"), "{st}");
    // Something else than a Claude credential is refused.
    let o = w.run_stdin("ghp_notit\n", &["dispatch", "credential", "set"]);
    assert_eq!(code(&o), Some(2), "{}", text(&o));
    assert!(!path.exists());
    // It has no argument to take it from.
    let o = w.run(&["dispatch", "credential", "set", CRED]);
    assert_ne!(code(&o), Some(0), "{}", text(&o));
    assert!(!path.exists());
    // From stdin: saved 0600, the value never printed.
    let out = w.ok_stdin(&format!("{CRED}\n"), &["dispatch", "credential", "set"]);
    assert!(out.contains("CLAUDE_CODE_OAUTH_TOKEN"), "{out}");
    assert!(!out.contains(CRED), "{out}");
    use std::os::unix::fs::PermissionsExt;
    let mode = fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600, "{mode:o}");
    assert_eq!(fs::read_to_string(&path).unwrap(), CRED);
    let st = w.ok(&["dispatch", "status"]);
    assert!(
        st.contains("Agent credential: set (a Claude Code OAuth token)"),
        "{st}"
    );
    assert!(!st.contains(CRED), "{st}");
    let js = w.ok(&["--json", "dispatch", "status"]);
    assert!(!js.contains(CRED), "{js}");
    assert!(js.contains("\"credential\": \"oauth_token\""), "{js}");
    // Nowhere in the kontext-home (never synced).
    assert!(!all_text(&w.kx_home).contains(CRED));
    // An API key is taken too, then removed.
    let out = w.ok_stdin("sk-ant-api03-other", &["dispatch", "credential", "set"]);
    assert!(out.contains("ANTHROPIC_API_KEY"), "{out}");
    let out = w.ok(&["dispatch", "credential", "rm"]);
    assert!(out.contains("removed"), "{out}");
    assert!(!path.exists());
}

#[test]
fn dispatch_makes_a_token_and_a_sandbox_and_the_secrets_travel_only_as_files() {
    let Some(w) = World::new() else { return };
    w.plan(true);
    let o = w.run(&["dispatch", "foundations"]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    assert!(out.contains("sandbox bx_fake1"), "{out}");

    // Boat was asked for a sandbox named after the project and the epic,
    // with the variables that are not secret and the secrets as files.
    let calls = w.calls();
    let new = calls.lines().find(|l| l.starts_with("new ")).unwrap();
    for part in [
        "--name kontext-demo-foundations",
        "-e KONTEXT_PROJECT=demo",
        "-e KONTEXT_ACTOR=agent:epic:foundations",
        "-e KONTEXT_ROLE=coordinator",
        "-e KONTEXT_EPIC=foundations",
        "--file .kontext-dispatch-token=",
        "--file .kontext-agent-credential=",
        "--file .kontext-dispatch-turn.sh=",
        "--file .kontext-dispatch-prompt-first.md=",
        "--file .kontext-dispatch-prompt-resume.md=",
        "--type default",
        "--ttl 32400",
    ] {
        assert!(new.contains(part), "{part} not in {new}");
    }
    assert!(
        new.contains(&format!("-e KONTEXT_SERVER={}", w.server.url)),
        "{new}"
    );
    // The token and the credential reached the sandbox as files, and nowhere
    // else: not in a command line, the output, the state or the env.
    let sent = fs::read_to_string(w.fake.join("files/.kontext-dispatch-token")).unwrap();
    assert!(sent.starts_with("kxt1."), "{sent}");
    let cred = fs::read_to_string(w.fake.join("files/.kontext-agent-credential")).unwrap();
    assert_eq!(cred, CRED);
    for secret in ["kxt1.", CRED] {
        assert!(!calls.contains(secret), "{calls}");
        assert!(!out.contains(secret), "{out}");
        assert!(
            !fs::read_to_string(w.fake.join("bx_fake1.env"))
                .unwrap()
                .contains(secret)
        );
    }
    assert!(!w.config_text_but_credential().contains("kxt1."));
    assert!(!w.config_text_but_credential().contains(CRED));
    // No prompt text in a command line either (Boat keeps them all).
    assert!(!calls.contains("one stack of pull requests"), "{calls}");
    // The local copies are gone.
    let tmp = w.config.join("dispatch/demo/tmp");
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
    assert!(status.contains("claude claude-opus-5-5"), "{status}");
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
fn the_coordinator_runs_as_one_detached_process_in_the_sandbox() {
    let Some(w) = World::new() else { return };
    w.plan(true);
    let o = w.run(&["dispatch", "foundations"]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    assert!(out.contains("start script done"), "{out}");
    assert!(
        out.contains("coordinator started: Claude Code claude-opus-5-5 in bx_fake1, turn 1"),
        "{out}"
    );
    assert!(!out.contains("prompted"), "{out}");
    // The start script went with the sandbox; Boat was asked how it went.
    assert!(w.fake.join("bx_fake1.setup-file").exists());
    let calls = w.calls();
    assert!(calls.contains("--setup-file "), "{calls}");
    assert!(calls.contains("info bx_fake1 --json"), "{calls}");
    // One turn, started in the background through a login shell, with the
    // session id the dispatcher picked; then followed by its pid.
    let run = w.run_of("foundations");
    assert_eq!(run["turns"], 1);
    let session = run["session"].as_str().unwrap().to_string();
    assert_eq!(session.len(), 36, "{session}");
    let pid = run["turn_pid"].as_u64().unwrap();
    let turns = w.turns();
    assert_eq!(turns.len(), 1, "{calls}");
    assert_eq!(
        turns[0],
        format!(
            "exec bx_fake1 --detach --json -- bash -lc exec sh \"$HOME/.kontext-dispatch-turn.sh\" \
             start 1 {session}"
        )
    );
    assert!(
        calls.contains(&format!("exec bx_fake1 --status {pid} --json")),
        "{calls}"
    );
    // The runner and the prompts it reads.
    let runner = fs::read_to_string(w.fake.join("files/.kontext-dispatch-turn.sh")).unwrap();
    assert!(runner.contains("MODEL='claude-opus-5-5'"), "{runner}");
    assert!(
        runner.contains("CLAUDE_CODE_OAUTH_TOKEN=\"$(cat \"$CRED\")\" claude -p"),
        "{runner}"
    );
    assert!(
        runner.contains("--dangerously-skip-permissions"),
        "{runner}"
    );
    let first = fs::read_to_string(w.fake.join("files/.kontext-dispatch-prompt-first.md")).unwrap();
    assert!(
        first.contains(
            "kontext --actor agent:epic:foundations --role coordinator brief foundations"
        ),
        "{first}"
    );
    assert!(first.contains("kontext-coordinator.md"), "{first}");
    assert!(first.contains("one stack of pull requests"), "{first}");
    let resume =
        fs::read_to_string(w.fake.join("files/.kontext-dispatch-prompt-resume.md")).unwrap();
    assert!(resume.contains("still open"), "{resume}");
    // Nothing on this machine holds the turn: no turns folder at all.
    assert!(!w.config.join("dispatch/demo/turns").exists());
}

#[test]
fn a_coordinator_that_cannot_start_fails_the_dispatch_and_everything_goes() {
    let Some(w) = World::new() else { return };
    w.plan(true);
    // Claude Code is not logged in: the first turn fails at once.
    fs::write(
        w.fake.join("end-bx_fake1-1"),
        "error:Not logged in · Please run /login",
    )
    .unwrap();
    let o = w.run(&["dispatch", "foundations"]);
    let out = text(&o);
    assert_eq!(code(&o), Some(1), "{out}");
    assert!(
        out.contains("the coordinator did not start: Claude Code said: Not logged in"),
        "{out}"
    );
    assert!(!out.contains("coordinator started"), "{out}");
    // The credential is removed from the sandbox before it stops; the token
    // is revoked; the run says why.
    assert!(w.call_at("rm -f") < w.call_at("stop bx_fake1"));
    assert!(w.fake.join("bx_fake1.credential-removed").exists());
    assert!(w.fake.join("bx_fake1.stopped").exists());
    let run = w.run_of("foundations");
    assert_eq!(run["state"], "stuck");
    assert_eq!(run["token_revoked"], true);
    assert!(
        run["reason"].as_str().unwrap().contains("Not logged in"),
        "{run}"
    );
    // A runner that fails (no claude on the PATH) fails it the same way.
    fs::write(w.fake.join("end-bx_fake2-1"), "exit:127").unwrap();
    let o = w.run(&["dispatch", "foundations"]);
    assert_eq!(code(&o), Some(1), "{}", text(&o));
    assert!(
        text(&o).contains("the coordinator did not start: kontext-dispatch: the runner failed"),
        "{}",
        text(&o)
    );
    assert!(w.fake.join("bx_fake2.stopped").exists());
    // No secret in the output or the state of a failed dispatch.
    for secret in ["kxt1.", CRED] {
        assert!(!text(&o).contains(secret));
        assert!(!w.config_text_but_credential().contains(secret));
        assert!(!w.calls().contains(secret));
    }
}

#[test]
fn stop_ends_the_turn_removes_the_credential_stops_and_revokes() {
    let Some(w) = World::new() else { return };
    w.plan(true);
    w.ok(&["dispatch", "foundations"]);
    let pid = w.run_of("foundations")["turn_pid"].as_u64().unwrap();
    let out = w.ok(&["dispatch", "stop", "foundations"]);
    assert!(out.contains("sandbox bx_fake1 stopped"), "{out}");
    // In this order: the turn ends, the credential goes, the sandbox stops.
    let kill = w.call_at(&format!("exec bx_fake1 --kill {pid} --json"));
    let rm = w.call_at("rm -f");
    let stop = w.call_at("stop bx_fake1");
    assert!(kill < rm && rm < stop, "{}", w.calls());
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
    // Again: there is nothing to stop, and nothing runs in that sandbox.
    let before = w.calls().lines().count();
    let o = w.run(&["dispatch", "stop", "foundations"]);
    assert_eq!(code(&o), Some(2), "{}", text(&o));
    assert_eq!(w.calls().lines().count(), before);
    // And it can be dispatched again: the new sandbox gets its own first
    // turn (finding 1 of the review: the old run's turn was reused).
    let o = w.run(&["dispatch", "foundations"]);
    assert!(o.status.success(), "{}", text(&o));
    let run = w.run_of("foundations");
    assert_eq!(run["sandbox"], "bx_fake2");
    assert_eq!(run["state"], "running");
    assert_eq!(run["turns"], 1);
    let turns = w.turns();
    assert_eq!(turns.len(), 2, "{}", w.calls());
    assert!(turns[1].starts_with("exec bx_fake2 --detach"), "{turns:?}");
    assert!(turns[1].contains(" start 1 "), "{turns:?}");
    assert_ne!(run["turn_pid"].as_u64().unwrap(), pid);
}

#[test]
fn a_credential_that_cannot_be_removed_still_lets_the_sandbox_stop() {
    let Some(w) = World::new() else { return };
    w.plan(true);
    w.ok(&["dispatch", "foundations"]);
    fs::write(w.fake.join("fail-rm"), "").unwrap();
    let o = w.run(&["dispatch", "stop", "foundations"]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(
        text(&o).contains("could not remove the agent's credential from bx_fake1"),
        "{}",
        text(&o)
    );
    assert!(text(&o).contains("stopping it anyway"), "{}", text(&o));
    assert!(w.fake.join("bx_fake1.stopped").exists());
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
    // The token's and the credential's local copies are gone.
    assert_eq!(
        fs::read_dir(w.config.join("dispatch/demo/tmp"))
            .unwrap()
            .count(),
        0
    );
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
    assert!(w.turns().is_empty(), "{}", w.calls());
}

/// The sandbox, here: a home of its own, the files where Boat writes them,
/// the variables of `-e`, and fakes of `curl` (whose install.sh copies this
/// kontext), `gh` (logged in as octo) and `claude`, first on a short `PATH`.
struct Sandbox {
    home: PathBuf,
    bin: PathBuf,
    shm: PathBuf,
    gitconfig: PathBuf,
    vars: Vec<(String, String)>,
    fake: PathBuf,
}

impl Sandbox {
    fn new(w: &World, name: &str) -> Sandbox {
        let root = w.dir.path().join(name);
        let home = root.join("home");
        let bin = root.join("bin");
        let shm = root.join("shm");
        for d in [&home, &bin, &shm] {
            fs::create_dir_all(d).unwrap();
        }
        // git has no author in the sandbox.
        let gitconfig = root.join("gitconfig");
        fs::write(
            &gitconfig,
            "[init]\n\tdefaultBranch = main\n[commit]\n\tgpgsign = false\n",
        )
        .unwrap();
        let script = |name: &str, body: &str| {
            use std::os::unix::fs::PermissionsExt;
            fs::write(bin.join(name), body).unwrap();
            fs::set_permissions(bin.join(name), fs::Permissions::from_mode(0o755)).unwrap();
        };
        script(
            "curl",
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >>\"$FAKE_BOAT_DIR/curl.log\"\n\
             [ -z \"${FAKE_CURL_FAIL:-}\" ] || exit 22\n\
             out=''\nwhile [ $# -gt 0 ]; do case \"$1\" in -o) out=$2; shift 2 ;; *) shift ;; esac; done\n\
             if [ -n \"${FAKE_INSTALL_NOTHING:-}\" ]; then echo 'true' >\"$out\"; exit 0; fi\n\
             cat >\"$out\" <<'EOS'\nset -eu\nmkdir -p \"$KONTEXT_INSTALL_DIR\"\n\
             cp \"$KONTEXT_TEST_BIN\" \"$KONTEXT_INSTALL_DIR/kontext\"\nEOS\n",
        );
        script(
            "gh",
            "#!/bin/sh\ncase \"$*\" in\n*.login*) echo octo ;;\n*.id*) echo 4242 ;;\n*) exit 1 ;;\nesac\n",
        );
        // claude: says what it got (never the credential's value), and
        // answers as Claude Code does, or as when it is not logged in.
        script(
            "claude",
            "#!/bin/sh\n{\n  echo \"cwd: $(pwd)\"\n  echo \"args: $*\"\n\
               [ -n \"${CLAUDE_CODE_OAUTH_TOKEN:-}\" ] && echo 'oauth: set'\n\
               [ \"${CLAUDE_CODE_OAUTH_TOKEN:-}\" = \"$FAKE_EXPECTED_CRED\" ] && echo 'oauth: right'\n\
             } >>\"$FAKE_BOAT_DIR/claude.log\"\n\
             if [ -n \"${FAKE_CLAUDE_LOGGED_OUT:-}\" ]; then\n\
               echo '{\"type\":\"result\",\"is_error\":true,\"result\":\"Not logged in · Please run /login\"}'\n  exit 1\nfi\n\
             echo '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"ok\",\"session_id\":\"s\"}'\n",
        );
        let vars = fs::read_to_string(w.fake.join("bx_fake1.env"))
            .unwrap()
            .lines()
            .filter_map(|l| l.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let sb = Sandbox {
            home,
            bin,
            shm,
            gitconfig,
            vars,
            fake: w.fake.clone(),
        };
        sb.put_files();
        sb
    }

    /// What `--file` writes: every file the fake boat kept.
    fn put_files(&self) {
        for e in fs::read_dir(self.fake.join("files")).unwrap().flatten() {
            fs::copy(e.path(), self.home.join(e.file_name())).unwrap();
        }
    }

    fn cmd(&self, program: &Path) -> Command {
        let mut c = Command::new(program);
        c.env_clear()
            .env("HOME", &self.home)
            .env("PATH", format!("{}:/usr/bin:/bin", self.bin.display()))
            .env("KONTEXT_TEST_BIN", BIN)
            .env("KONTEXT_DISPATCH_SHM", &self.shm)
            .env("KONTEXT_DISPATCH_REPO_WAIT", "0")
            .env("FAKE_BOAT_DIR", &self.fake)
            .env("FAKE_EXPECTED_CRED", CRED)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", &self.gitconfig)
            .current_dir(&self.home);
        for (k, v) in &self.vars {
            c.env(k, v);
        }
        c
    }

    fn sh(&self, script: &Path, args: &[&str]) -> Command {
        let mut c = self.cmd(Path::new("/bin/sh"));
        c.arg(script).args(args);
        c
    }

    fn start_script(&self) -> Command {
        self.sh(&self.fake.join("bx_fake1.setup-file"), &[])
    }

    fn runner(&self, args: &[&str]) -> Output {
        self.sh(&self.home.join(".kontext-dispatch-turn.sh"), args)
            .output()
            .unwrap()
    }
}

fn git_in(w: &World, dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", w.dir.path().join("gitconfig"))
        .output()
        .unwrap();
    assert!(o.status.success(), "git {args:?}: {}", text(&o));
    String::from_utf8(o.stdout).unwrap()
}

fn mode(p: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(p).unwrap().permissions().mode() & 0o777
}

/// The start script, run as the sandbox would. The code repo is not cloned
/// by the service: the script clones it from `[[repos]]`' remote.
#[test]
fn the_start_script_sets_the_sandbox_up_clones_the_repo_and_hides_the_secrets() {
    let Some(w) = World::new() else { return };
    w.plan(true);
    // The code repo's remote: a bare repo with one commit.
    let seed = w.dir.path().join("seed");
    fs::create_dir_all(&seed).unwrap();
    git_in(&w, &seed, &["init", "-q"]);
    fs::write(seed.join("README.md"), "demo\n").unwrap();
    git_in(&w, &seed, &["add", "README.md"]);
    git_in(&w, &seed, &["commit", "-qm", "first"]);
    let origin = w.dir.path().join("origin.git");
    git_in(
        &w,
        w.dir.path(),
        &[
            "clone",
            "-q",
            "--bare",
            seed.to_str().unwrap(),
            origin.to_str().unwrap(),
        ],
    );
    w.configure(&format!(
        "\n[[repos]]\nname = \"demo\"\nremote = {:?}\n",
        origin.to_str().unwrap()
    ));
    w.ok(&["dispatch", "foundations"]);
    let sb = Sandbox::new(&w, "sbx");
    // Where Boat writes the credential, with the mode it gives it.
    use std::os::unix::fs::PermissionsExt;
    let cred_in = sb.home.join(".kontext-agent-credential");
    fs::set_permissions(&cred_in, fs::Permissions::from_mode(0o644)).unwrap();
    let token_file = sb.home.join(".kontext-dispatch-token");

    // First with no memory-backed storage: a 0600 file in the home.
    let o = sb
        .start_script()
        .env("KONTEXT_DISPATCH_SHM", sb.home.join("no-shm"))
        .output()
        .unwrap();
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    assert!(!out.contains("kxt1.") && !out.contains(CRED), "{out}");
    assert!(
        out.contains("ready: project demo, epic foundations"),
        "{out}"
    );
    assert!(!token_file.exists(), "the token file is still there");
    assert!(
        !cred_in.exists(),
        "the credential is still where Boat put it"
    );
    let on_disk = sb.home.join(".kontext-dispatch/agent-credential");
    assert_eq!(fs::read_to_string(&on_disk).unwrap(), CRED);
    assert_eq!(mode(&on_disk), 0o600);
    assert_eq!(mode(&sb.home.join(".kontext-dispatch")), 0o700);
    // The pinned release, downloaded to a file.
    let curl = fs::read_to_string(w.fake.join("curl.log")).unwrap();
    assert!(
        curl.contains(&format!(
            "https://raw.githubusercontent.com/OmarAlex24/kontext/v{}/install.sh",
            env!("CARGO_PKG_VERSION")
        )),
        "{curl}"
    );
    assert!(curl.contains("-o "), "{curl}");
    // The project, its token saved by kontext (0600).
    let cfg = sb.home.join(".config/kontext");
    let projects = fs::read_to_string(cfg.join("projects.toml")).unwrap();
    assert!(projects.contains("[projects.demo]"), "{projects}");
    assert_eq!(mode(&cfg.join("tokens/demo.token")) & 0o077, 0);
    // The repo, cloned by the script; git's author from the GitHub login.
    assert!(w.repo().join("README.md").exists());
    let email = git_in(&w, &w.repo(), &["config", "--local", "user.email"]);
    assert_eq!(email.trim(), "4242+octo@users.noreply.github.com");
    let name = git_in(&w, &w.repo(), &["config", "--local", "user.name"]);
    assert_eq!(name.trim(), "octo");
    // The hooks, skills and agent files, out of git's way.
    assert!(w.repo().join(".kontext.toml").exists());
    assert!(
        w.repo()
            .join(".claude/agents/kontext-coordinator.md")
            .exists()
    );
    assert!(w.repo().join(".claude/settings.local.json").exists());
    assert_eq!(git_in(&w, &w.repo(), &["status", "--porcelain"]), "");
    // The coordinator's first command works there.
    let o = sb
        .cmd(&sb.home.join(".local/bin/kontext"))
        .args(["brief", "foundations"])
        .current_dir(w.repo())
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("Foundations"), "{}", text(&o));

    // Run again with memory-backed storage (a sandbox restarted midway, the
    // credential sent again): it moves there, and the disk copy goes.
    fs::copy(w.fake.join("files/.kontext-agent-credential"), &cred_in).unwrap();
    let o = sb.start_script().output().unwrap();
    assert!(o.status.success(), "{}", text(&o));
    let in_memory = sb.shm.join("kontext-dispatch-agent-credential");
    assert_eq!(fs::read_to_string(&in_memory).unwrap(), CRED);
    assert_eq!(mode(&in_memory), 0o600);
    assert!(!on_disk.exists(), "the disk copy is still there");
    assert!(!cred_in.exists());
    assert_eq!(git_in(&w, &w.repo(), &["status", "--porcelain"]), "");

    // The runner: one turn of Claude Code in the clone, with the credential
    // in its environment only, every permission, and the session id.
    let sid = "0f8fad5b-d9cb-469f-a165-70867728950e";
    let o = sb.runner(&["start", "1", sid]);
    assert!(o.status.success(), "{}", text(&o));
    let log = fs::read_to_string(w.fake.join("claude.log")).unwrap();
    assert!(log.contains("oauth: right"), "{log}");
    assert!(!log.contains(CRED), "{log}");
    assert!(
        log.contains(&format!(
            "cwd: {}",
            w.repo().canonicalize().unwrap().display()
        )) || log.contains(&format!("cwd: {}", w.repo().display())),
        "{log}"
    );
    assert!(
        log.contains("-p You are the coordinator of epic foundations"),
        "{log}"
    );
    assert!(
        log.contains(&format!(
            "--model claude-opus-5-5 --output-format json --dangerously-skip-permissions \
             --session-id {sid}"
        )),
        "{log}"
    );
    let turn = sb.home.join(".kontext-dispatch/turn-1");
    assert_eq!(fs::read_to_string(turn.join("exit")).unwrap().trim(), "0");
    let read = sb.runner(&["read", "1"]);
    let read = String::from_utf8_lossy(&read.stdout).to_string();
    assert!(read.contains("exit=0\n"), "{read}");
    assert!(read.contains("flag=\"is_error\":false\n"), "{read}");
    assert!(read.contains("--- result\n{\"type\":\"result\""), "{read}");
    // The same turn never starts twice.
    let o = sb.runner(&["start", "1", sid]);
    assert_eq!(code(&o), Some(90), "{}", text(&o));
    assert!(text(&o).contains("turn 1 was already started"));
    // The second turn resumes the session, with the resume prompt.
    let o = sb.runner(&["start", "2", sid]);
    assert!(o.status.success(), "{}", text(&o));
    let log = fs::read_to_string(w.fake.join("claude.log")).unwrap();
    assert!(log.contains("-p Epic foundations is still open"), "{log}");
    assert!(log.contains(&format!("--resume {sid}")), "{log}");
    // Not logged in: the turn says so and fails.
    let o = sb
        .sh(
            &sb.home.join(".kontext-dispatch-turn.sh"),
            &["start", "3", sid],
        )
        .env("FAKE_CLAUDE_LOGGED_OUT", "1")
        .output()
        .unwrap();
    assert_eq!(code(&o), Some(1), "{}", text(&o));
    let read = String::from_utf8_lossy(&sb.runner(&["read", "3"]).stdout).to_string();
    assert!(read.contains("exit=1\n"), "{read}");
    assert!(read.contains("flag=\"is_error\":true\n"), "{read}");
    assert!(read.contains("Not logged in"), "{read}");
    // With the credential gone, a turn fails before it calls claude.
    fs::remove_file(&in_memory).unwrap();
    let o = sb.runner(&["start", "4", sid]);
    assert_eq!(code(&o), Some(91), "{}", text(&o));
    let read = String::from_utf8_lossy(&sb.runner(&["read", "4"]).stdout).to_string();
    assert!(read.contains("exit=91\n"), "{read}");
    assert!(read.contains("credential is not in the sandbox"), "{read}");
    // Nothing the scripts left anywhere in the sandbox has a secret, but the
    // credential's own file (now gone) and kontext's token file.
    let mut leaked = Vec::new();
    walk(&sb.home, &mut |p| {
        // kontext's own token file, and the binary (which has the prefix).
        if p.ends_with("tokens/demo.token") || p.ends_with(".local/bin/kontext") {
            return;
        }
        let t = String::from_utf8_lossy(&fs::read(p).unwrap_or_default()).to_string();
        if t.contains(CRED) || t.contains("kxt1.") {
            leaked.push(p.display().to_string());
        }
    });
    assert!(leaked.is_empty(), "{leaked:?}");
    // The command that clears the credential before a stop removes it from
    // memory too: dispatch's clean-up runs it (checked with the fake boat).
}

/// The review's finding 11: an installer that cannot be downloaded, or
/// installs nothing, stops the start script at once and says so.
#[test]
fn a_failed_install_stops_the_start_script_at_once() {
    let Some(w) = World::new() else { return };
    w.plan(true);
    w.ok(&["dispatch", "foundations"]);
    for (var, says) in [
        ("FAKE_CURL_FAIL", "could not download kontext's installer"),
        (
            "FAKE_INSTALL_NOTHING",
            "is not on the PATH after installing it",
        ),
    ] {
        let sb = Sandbox::new(&w, &format!("sbx-{var}"));
        let started = std::time::Instant::now();
        let o = sb.start_script().env(var, "1").output().unwrap();
        let out = text(&o);
        assert_eq!(code(&o), Some(1), "{out}");
        assert!(out.contains(says), "{out}");
        assert!(!out.contains("command not found"), "{out}");
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
        assert!(!sb.home.join(".kontext-dispatch-token").exists());
    }
}

#[test]
fn the_watcher_stops_the_sandbox_when_the_epic_closes_in_the_ledger() {
    let Some(w) = World::new() else { return };
    let tasks = w.plan(true);
    let o = w.run(&["dispatch", "foundations"]);
    assert!(text(&o).contains("watcher running (pid"), "{}", text(&o));
    let st = w.status();
    assert!(st["watcher"].as_u64().is_some(), "{st}");
    // The coordinator's turn is still going; the ledger alone says it closed.
    for (e, t) in &tasks {
        if e == "foundations" {
            w.ok(&["done", t, "--run", "true"]);
        }
    }
    w.wait_state("foundations", "closed");
    let run = w.run_of("foundations");
    assert!(w.fake.join("bx_fake1.stopped").exists());
    assert_eq!(run["token_revoked"], true);
    assert!(!w.token(run["token_id"].as_str().unwrap())["revoked"].is_null());
    // The running turn was ended and the credential removed, before the stop.
    assert!(w.call_at("--kill") < w.call_at("rm -f"));
    assert!(w.call_at("rm -f") < w.call_at("stop bx_fake1"));
    let found = w.ok(&["recall", "sandbox"]);
    assert!(found.contains("the epic closed"), "{found}");
    assert_eq!(w.turns().len(), 1, "it was not resumed");
    // Nothing is live: the watcher stops.
    wait_for("the watcher to stop", || w.status()["watcher"].is_null());
}

#[test]
fn a_coordinator_is_resumed_once_and_then_it_is_stuck() {
    let Some(w) = World::new() else { return };
    w.plan(true);
    w.ok(&["dispatch", "foundations"]);
    let session = w.run_of("foundations")["session"]
        .as_str()
        .unwrap()
        .to_string();
    // Its turn ends with the epic open: once more, same session.
    fs::write(w.fake.join("end-bx_fake1-1"), "").unwrap();
    w.wait_state("foundations", "resumed");
    wait_for("the second turn", || w.turns().len() == 2);
    let second = &w.turns()[1];
    assert!(second.ends_with(&format!("start 2 {session}")), "{second}");
    assert_eq!(w.run_of("foundations")["turns"], 2);
    wait_for("the second turn's pid", || {
        w.run_of("foundations")["turn_pid"].as_u64().is_some()
    });
    // And again: stuck. The pm hears it, the sandbox stops, the token goes.
    fs::write(w.fake.join("end-bx_fake1-2"), "").unwrap();
    w.wait_state("foundations", "stuck");
    let run = w.run_of("foundations");
    assert!(
        run["reason"].as_str().unwrap().contains("stopped again"),
        "{run}"
    );
    assert!(w.fake.join("bx_fake1.stopped").exists());
    assert_eq!(run["token_revoked"], true);
    let pm = w.ok(&[
        "--actor", "agent:pm", "--role", "pm", "resume", "--as", "pm",
    ]);
    assert!(
        pm.contains("Epic foundations is stuck in its sandbox"),
        "{pm}"
    );
    assert_eq!(w.turns().len(), 2, "no third turn");
    let status = w.ok(&["dispatch", "status"]);
    assert!(status.contains("foundations  stuck"), "{status}");
}

#[test]
fn a_failed_turn_is_stuck_at_once_with_its_reason() {
    let Some(w) = World::new() else { return };
    w.plan(true);
    w.ok(&["dispatch", "foundations"]);
    // A token that stopped working while the coordinator ran.
    fs::write(
        w.fake.join("end-bx_fake1-1"),
        "error:Failed to authenticate. API Error: 401",
    )
    .unwrap();
    w.wait_state("foundations", "stuck");
    let run = w.run_of("foundations");
    let reason = run["reason"].as_str().unwrap();
    assert!(
        reason.contains("turn 1 failed: Claude Code said: Failed to authenticate"),
        "{reason}"
    );
    assert_eq!(
        w.turns().len(),
        1,
        "not resumed: it would fail the same way"
    );
    assert!(w.fake.join("bx_fake1.stopped").exists());
    let pm = w.ok(&[
        "--actor", "agent:pm", "--role", "pm", "resume", "--as", "pm",
    ]);
    assert!(pm.contains("Failed to authenticate"), "{pm}");
}

#[test]
fn a_turn_that_cannot_be_followed_decides_nothing() {
    let Some(w) = World::new() else { return };
    w.plan(true);
    w.ok(&["dispatch", "foundations"]);
    // Boat no longer knows the process; and then cannot be asked at all.
    fs::write(w.fake.join("lost-bx_fake1-1"), "").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(800));
    fs::write(w.fake.join("fail-status"), "").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(800));
    assert_eq!(w.run_of("foundations")["state"], "running");
    assert_eq!(w.turns().len(), 1);
    assert!(!w.fake.join("bx_fake1.stopped").exists());
    let log = fs::read_to_string(w.config.join("dispatch/demo/watcher.log")).unwrap();
    assert_eq!(log.matches("cannot be followed").count(), 1, "{log}");
    assert_eq!(log.matches("could not ask").count(), 1, "{log}");
    fs::remove_file(w.fake.join("fail-status")).unwrap();
    w.ok(&["dispatch", "stop", "foundations"]);
}

#[test]
fn a_restarted_watcher_picks_up_and_the_time_limit_marks_it_stuck() {
    let Some(w) = World::new() else { return };
    w.plan(true);
    w.ok(&["dispatch", "foundations"]);
    let pid = w.status()["watcher"].as_u64().unwrap();
    // The watcher dies (the Mac slept, say): the sandbox keeps working.
    assert!(
        Command::new("kill")
            .arg(pid.to_string())
            .status()
            .unwrap()
            .success()
    );
    wait_for("the watcher to be gone", || w.status()["watcher"].is_null());
    let status = w.ok(&["dispatch", "status"]);
    assert!(status.contains("The watcher is not running"), "{status}");
    assert!(
        status.contains("kontext --project demo dispatch watch --detach"),
        "{status}"
    );
    assert_eq!(w.run_of("foundations")["state"], "running");
    assert!(!w.fake.join("bx_fake1.stopped").exists());
    // Started again past the time limit, from the saved state: stuck.
    let out = w.ok_env(
        &[("KONTEXT_NOW", "2099-01-01T00:00:00Z")],
        &["dispatch", "watch", "--detach"],
    );
    // It may already have done its work and left by the time it answers.
    assert!(out.contains("The watcher of demo r"), "{out}");
    w.wait_state("foundations", "stuck");
    let run = w.run_of("foundations");
    assert!(
        run["reason"]
            .as_str()
            .unwrap()
            .contains("its time limit (8h) passed"),
        "{run}"
    );
    assert!(w.fake.join("bx_fake1.stopped").exists());
    assert_eq!(run["token_revoked"], true);
}

// ------------------------------------------------- the review's findings

impl World {
    /// `kontext dispatch <epic>` in the background, with more variables.
    fn spawn(&self, envs: &[(&str, &str)], args: &[&str]) -> std::process::Child {
        let mut c = self.cmd(args);
        for (k, v) in envs {
            c.env(k, v);
        }
        c.stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    }

    fn dispatch_dir(&self) -> PathBuf {
        self.config.join("dispatch/demo")
    }

    /// The files in the dispatch folder's `tmp/`.
    fn tmp_files(&self) -> Vec<String> {
        fs::read_dir(self.dispatch_dir().join("tmp"))
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// No secret anywhere it must not be: the config folder (but the
    /// credential's file and the owner's own token), Boat's calls, the
    /// kontext-home.
    fn assert_no_secret_left(&self) {
        let cfg = self.config_text_but_credential();
        for secret in ["kxt1.", CRED] {
            assert!(!cfg.contains(secret), "{secret} in the config folder");
            assert!(!self.calls().contains(secret), "{secret} in a command line");
            assert!(!all_text(&self.kx_home).contains(secret) || secret == "kxt1.");
        }
    }

    /// The questions to the pm about a stuck epic, as written in the ledger.
    fn stuck_questions(&self) -> usize {
        fs::read_to_string(self.kx_home.join("ledger/omar@mac.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter(|l| {
                l.contains("\"kind\":\"question\"") && l.contains("is stuck in its sandbox")
            })
            .count()
    }

    fn close_epic(&self, tasks: &[(String, String)], epic: &str) {
        for (e, t) in tasks {
            if e == epic {
                self.ok(&["done", t, "--run", "true"]);
            }
        }
    }
}

fn signal(child: &std::process::Child, sig: &str) {
    assert!(
        Command::new("kill")
            .args([sig, &child.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
}

/// The review's tests: the closed-epic refusal.
#[test]
fn a_closed_epic_is_refused() {
    let Some(w) = World::new() else { return };
    let tasks = w.plan(true);
    w.close_epic(&tasks, "small");
    let o = w.run(&["dispatch", "small"]);
    assert_eq!(code(&o), Some(2), "{}", text(&o));
    assert!(text(&o).contains("epic small is closed"), "{}", text(&o));
    assert_eq!(w.calls(), "");
}

/// Findings 2 and 7: `dispatch` killed during the setup wait. The watcher,
/// started right after the reservation, finishes the start.
#[test]
fn a_dispatch_killed_during_the_setup_wait_is_finished_by_the_watcher() {
    let Some(w) = World::new() else { return };
    w.plan(true);
    fs::write(w.fake.join("bx_fake1.setup"), "running").unwrap();
    let mut d = w.spawn(&[], &["dispatch", "foundations"]);
    wait_for("the sandbox", || w.calls().contains("info bx_fake1 --json"));
    assert!(w.status()["watcher"].as_u64().is_some(), "no watcher yet");
    signal(&d, "-TERM");
    d.wait().unwrap();
    assert_eq!(w.run_of("foundations")["state"], "starting");
    assert!(w.turns().is_empty());
    // The start script ends: the watcher starts the first turn.
    fs::write(w.fake.join("bx_fake1.setup"), "done").unwrap();
    wait_for("the turn", || {
        w.run_of("foundations")["turn_pid"].as_u64().is_some()
    });
    let run = w.run_of("foundations");
    assert_eq!(run["state"], "running");
    assert_eq!(run["turns"], 1);
    assert_eq!(w.turns().len(), 1);
    assert!(w.turns()[0].contains(" start 1 "), "{:?}", w.turns());
    assert!(w.tmp_files().is_empty(), "{:?}", w.tmp_files());
    w.assert_no_secret_left();
}

/// Finding 7: killed after the reservation, before anything was made: the
/// run does not hold the epic or a slot.
#[test]
fn a_dispatch_killed_before_it_made_anything_frees_the_epic() {
    let Some(w) = World::new() else { return };
    w.plan(true);
    let mut d = w.spawn(
        &[("KONTEXT_DISPATCH_TEST_PAUSE", "reserved")],
        &["dispatch", "foundations"],
    );
    wait_for("the pause", || {
        w.dispatch_dir().join("paused-reserved").exists()
    });
    signal(&d, "-TERM");
    d.wait().unwrap();
    w.wait_state("foundations", "stuck");
    let run = w.run_of("foundations");
    assert!(
        run["reason"]
            .as_str()
            .unwrap()
            .contains("before it made anything"),
        "{run}"
    );
    assert_eq!(w.calls(), "", "nothing reached Boat");
    let o = w.run(&["dispatch", "foundations"]);
    assert!(o.status.success(), "{}", text(&o));
}

/// Finding 3: killed while Boat makes the sandbox. The token's and the
/// credential's local copies go with the signal, and the sandbox is found
/// by its name; a leftover of a dead process is swept.
#[test]
fn a_dispatch_killed_during_boat_new_leaves_no_secret_and_its_sandbox_is_found() {
    let Some(w) = World::new() else { return };
    w.plan(true);
    let tmp = w.dispatch_dir().join("tmp");
    fs::create_dir_all(&tmp).unwrap();
    fs::write(
        tmp.join("token-99999999-dead"),
        "kxt1.left-by-a-dead-dispatch",
    )
    .unwrap();
    fs::write(w.fake.join("hang-new"), "").unwrap();
    let mut d = w.spawn(&[], &["dispatch", "foundations"]);
    wait_for("boat new", || w.fake.join("bx_fake1.name").exists());
    assert!(!w.tmp_files().is_empty(), "the copies should exist now");
    signal(&d, "-INT");
    d.wait().unwrap();
    assert!(
        w.tmp_files().iter().all(|f| f == "token-99999999-dead"),
        "{:?}",
        w.tmp_files()
    );
    let run = w.run_of("foundations");
    assert!(run["sandbox"].is_null(), "{run}");
    assert_eq!(run["sandbox_name"], "kontext-demo-foundations");
    // Boat finishes making it: the watcher finds it by name and goes on.
    fs::write(w.fake.join("release-new"), "").unwrap();
    wait_for("the turn", || {
        w.run_of("foundations")["turn_pid"].as_u64().is_some()
    });
    let run = w.run_of("foundations");
    assert_eq!(run["sandbox"], "bx_fake1");
    assert_eq!(run["state"], "running");
    assert!(
        w.calls().contains("list --filter rp --json"),
        "{}",
        w.calls()
    );
    wait_for("the sweep", || w.tmp_files().is_empty());
    w.assert_no_secret_left();
}

/// Finding 3: killed with the files written and no sandbox yet; `stop`
/// looks for the sandbox by name, and none is there.
#[test]
fn a_dispatch_killed_before_boat_new_is_ended_and_its_token_revoked() {
    let Some(w) = World::new() else { return };
    w.plan(true);
    let mut d = w.spawn(
        &[("KONTEXT_DISPATCH_TEST_PAUSE", "files")],
        &["dispatch", "foundations"],
    );
    wait_for("the pause", || {
        w.dispatch_dir().join("paused-files").exists()
    });
    assert!(!w.tmp_files().is_empty());
    signal(&d, "-TERM");
    d.wait().unwrap();
    assert!(w.tmp_files().is_empty(), "{:?}", w.tmp_files());
    w.wait_state("foundations", "stuck");
    let run = w.run_of("foundations");
    assert!(run["sandbox"].is_null(), "{run}");
    assert_eq!(run["token_revoked"], true);
    assert!(!w.token(run["token_id"].as_str().unwrap())["revoked"].is_null());
    w.assert_no_secret_left();
}

/// Killed between starting the first turn and saving its process id: the
/// watcher starts the turn again, the runner refuses to run it twice, and
/// the first process is followed.
#[test]
fn a_dispatch_killed_after_starting_the_turn_does_not_run_two_coordinators() {
    let Some(w) = World::new() else { return };
    w.plan(true);
    let mut d = w.spawn(
        &[("KONTEXT_DISPATCH_TEST_PAUSE", "turn")],
        &["dispatch", "foundations"],
    );
    wait_for("the pause", || {
        w.dispatch_dir().join("paused-turn").exists()
    });
    signal(&d, "-TERM");
    d.wait().unwrap();
    let first: u64 = fs::read_to_string(w.fake.join("pids"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    wait_for("the adopted turn", || {
        w.run_of("foundations")["turn_pid"].as_u64() == Some(first)
    });
    assert_eq!(w.run_of("foundations")["state"], "running");
    assert_eq!(w.turns().len(), 2, "{}", w.calls());
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert_eq!(w.turns().len(), 2, "started a third time");
}

/// Finding 4: a stop during the setup wait is not overwritten.
#[test]
fn a_stop_during_the_setup_wait_holds() {
    let Some(w) = World::new() else { return };
    w.plan(true);
    fs::write(w.fake.join("bx_fake1.setup"), "running").unwrap();
    let d = w.spawn(&[], &["dispatch", "foundations"]);
    wait_for("the sandbox", || w.calls().contains("info bx_fake1 --json"));
    w.ok(&["dispatch", "stop", "foundations"]);
    fs::write(w.fake.join("bx_fake1.setup"), "done").unwrap();
    let o = d.wait_with_output().unwrap();
    assert_eq!(code(&o), Some(1), "{}", text(&o));
    assert!(
        text(&o).contains("was stopped while dispatch was starting it"),
        "{}",
        text(&o)
    );
    let run = w.run_of("foundations");
    assert_eq!(run["state"], "stopped");
    assert_eq!(run["sandbox_stopped"], true);
    assert_eq!(run["token_revoked"], true);
    // Nothing ran in the sandbox after it stopped, and the tree does not
    // say it runs.
    let after = w.calls()[w.calls().find("stop bx_fake1").unwrap()..].to_string();
    assert!(!after.contains("exec bx_fake1"), "{after}");
    assert!(w.turns().is_empty());
    assert!(!w.ok(&["tree"]).contains("in a sandbox since"));
}

/// Finding 4: the epic closes during the setup wait; the watcher's close is
/// not overwritten either.
#[test]
fn an_epic_that_closes_during_the_setup_wait_stays_closed() {
    let Some(w) = World::new() else { return };
    let tasks = w.plan(true);
    fs::write(w.fake.join("bx_fake1.setup"), "running").unwrap();
    let d = w.spawn(&[], &["dispatch", "foundations"]);
    wait_for("the sandbox", || w.calls().contains("info bx_fake1 --json"));
    w.close_epic(&tasks, "foundations");
    w.wait_state("foundations", "closed");
    fs::write(w.fake.join("bx_fake1.setup"), "done").unwrap();
    let o = d.wait_with_output().unwrap();
    assert_eq!(code(&o), Some(1), "{}", text(&o));
    assert_eq!(w.run_of("foundations")["state"], "closed");
    assert!(w.turns().is_empty(), "{}", w.calls());
}

/// Findings 5 and 6, with `fail-stop`: a run that was not cleaned up holds
/// its epic and a slot, and the watcher stays until it is cleaned up.
#[test]
fn a_run_not_cleaned_up_holds_its_epic_and_the_watcher_stays() {
    let Some(w) = World::new() else { return };
    let tasks = w.plan(true);
    w.ok(&["dispatch", "small"]);
    fs::write(w.fake.join("fail-stop"), "").unwrap();
    let o = w.run(&["dispatch", "stop", "small"]);
    assert_eq!(code(&o), Some(1), "{}", text(&o));
    assert_eq!(w.run_of("small")["sandbox_stopped"], false);
    // Not dispatched again over it, and it counts toward the cap.
    let o = w.run(&["dispatch", "small"]);
    assert_eq!(code(&o), Some(2), "{}", text(&o));
    assert!(text(&o).contains("not cleaned up yet"), "{}", text(&o));
    w.configure("max_sandboxes = 1\n");
    let o = w.run(&["dispatch", "foundations"]);
    assert_eq!(code(&o), Some(2), "{}", text(&o));
    assert!(text(&o).contains("1 sandboxes are running or not cleaned up"));
    let toml = w.kx_home.join("project.toml");
    let t = fs::read_to_string(&toml).unwrap();
    fs::write(&toml, t.replace("max_sandboxes = 1\n", "")).unwrap();
    // Another epic closes while stops fail: the watcher does not leave.
    w.ok(&["dispatch", "foundations"]);
    w.close_epic(&tasks, "foundations");
    wait_for("closed", || w.run_of("foundations")["state"] == "closed");
    std::thread::sleep(std::time::Duration::from_millis(1500));
    assert!(w.status()["watcher"].as_u64().is_some(), "the watcher left");
    // Stops work again: it cleans both up, then leaves.
    fs::remove_file(w.fake.join("fail-stop")).unwrap();
    wait_for("both cleaned up", || {
        w.run_of("foundations")["sandbox_stopped"] == true
            && w.run_of("small")["sandbox_stopped"] == true
    });
    wait_for("the watcher to leave", || w.status()["watcher"].is_null());
    let o = w.run(&["dispatch", "small"]);
    assert!(o.status.success(), "{}", text(&o));
}

/// Finding 9: past the time limit with the pull failing, the sandbox stops
/// but the epic is not marked stuck until a pull says it.
#[test]
fn a_time_limit_on_a_stale_ledger_waits_for_a_fresh_pull() {
    let Some(mut w) = World::new() else { return };
    w.plan(true);
    w.ok(&["dispatch", "foundations"]);
    let pid = w.status()["watcher"].as_u64().unwrap();
    assert!(
        Command::new("kill")
            .arg(pid.to_string())
            .status()
            .unwrap()
            .success()
    );
    wait_for("the watcher to be gone", || w.status()["watcher"].is_null());
    w.server.stop();
    w.ok_env(
        &[("KONTEXT_NOW", "2099-01-01T00:00:00Z")],
        &["dispatch", "watch", "--detach"],
    );
    wait_for("overdue and stopped", || {
        let r = w.run_of("foundations");
        r["state"] == "overdue" && r["sandbox_stopped"] == true
    });
    std::thread::sleep(std::time::Duration::from_millis(800));
    assert_eq!(w.run_of("foundations")["state"], "overdue");
    assert_eq!(w.stuck_questions(), 0);
    // A redispatch waits for the verdict.
    let o = w.run(&["dispatch", "foundations"]);
    assert_eq!(code(&o), Some(2), "{}", text(&o));
    // The server is back: the epic is open, so it is stuck.
    w.server.restart();
    w.wait_state("foundations", "stuck");
    assert_eq!(w.stuck_questions(), 1);
    assert_eq!(w.run_of("foundations")["token_revoked"], true);
}

/// Finding 10: no `boat` call holds the state lock, and every one has a
/// time limit.
#[test]
fn a_hung_boat_call_blocks_nothing_and_times_out() {
    let Some(w) = World::new() else { return };
    w.plan(true);
    w.ok(&["dispatch", "foundations"]);
    fs::write(w.fake.join("hang-stop"), "").unwrap();
    let started = std::time::Instant::now();
    let stop = w.spawn(
        &[("KONTEXT_BOAT_TIMEOUT_MS", "4000")],
        &["dispatch", "stop", "foundations"],
    );
    wait_for("the hung stop", || w.calls().contains("stop bx_fake1"));
    // Meanwhile the state is free: status and another dispatch go on.
    let t = std::time::Instant::now();
    w.ok(&["dispatch", "status"]);
    let o = w.run(&["dispatch", "small"]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(
        t.elapsed() < std::time::Duration::from_secs(3),
        "{:?}",
        t.elapsed()
    );
    let o = stop.wait_with_output().unwrap();
    assert_eq!(code(&o), Some(1), "{}", text(&o));
    assert!(text(&o).contains("did not answer in 4 s"), "{}", text(&o));
    assert!(started.elapsed() < std::time::Duration::from_secs(20));
    fs::write(w.fake.join("release-stop"), "").unwrap();
}

/// Finding 10: one run's error does not keep the others from their turn.
#[test]
fn one_broken_run_does_not_stop_the_others() {
    let Some(w) = World::new() else { return };
    let tasks = w.plan(true);
    w.ok(&["dispatch", "foundations"]);
    let pid = w.status()["watcher"].as_u64().unwrap();
    assert!(
        Command::new("kill")
            .arg(pid.to_string())
            .status()
            .unwrap()
            .success()
    );
    wait_for("the watcher to be gone", || w.status()["watcher"].is_null());
    // A run that sorts first and that every tick fails on (no such service).
    let path = w.dispatch_dir().join("state.json");
    let mut st: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let mut broken = w.run_of("foundations");
    broken["epic"] = "E-0".into();
    broken["id"] = "broken".into();
    broken["name"] = "broken".into();
    broken["service"] = "nope".into();
    broken["sandbox"] = "bx_none".into();
    st["runs"]["E-0"] = broken;
    fs::write(&path, serde_json::to_string_pretty(&st).unwrap()).unwrap();
    w.ok(&["dispatch", "watch", "--detach"]);
    w.close_epic(&tasks, "foundations");
    w.wait_state("foundations", "closed");
    assert!(w.fake.join("bx_fake1.stopped").exists());
}

/// Finding 10: a stuck run's sandbox stops even when the question to the
/// pm cannot be written; the question follows once it can.
#[test]
fn a_stuck_sandbox_stops_even_when_the_pm_cannot_be_asked() {
    let Some(w) = World::new() else { return };
    w.plan(true);
    w.ok(&["dispatch", "foundations"]);
    use std::os::unix::fs::PermissionsExt;
    let ledger = w.kx_home.join("ledger/omar@mac.jsonl");
    fs::set_permissions(&ledger, fs::Permissions::from_mode(0o444)).unwrap();
    fs::write(
        w.fake.join("end-bx_fake1-1"),
        "error:Failed to authenticate",
    )
    .unwrap();
    wait_for("stopped", || w.fake.join("bx_fake1.stopped").exists());
    let run = w.run_of("foundations");
    assert_eq!(run["state"], "stuck");
    assert_eq!(run["pm_pending"], true);
    assert_eq!(w.stuck_questions(), 0);
    fs::set_permissions(&ledger, fs::Permissions::from_mode(0o644)).unwrap();
    w.wait_state("foundations", "stuck");
    assert_eq!(w.stuck_questions(), 1);
}

/// Finding 12: a second stuck run of an epic gets its own question.
#[test]
fn a_new_stuck_question_does_not_merge_into_an_old_one() {
    let Some(w) = World::new() else { return };
    w.plan(true);
    w.ok(&["dispatch", "foundations"]);
    fs::write(
        w.fake.join("end-bx_fake1-1"),
        "error:Failed to authenticate",
    )
    .unwrap();
    w.wait_state("foundations", "stuck");
    assert_eq!(w.stuck_questions(), 1);
    w.ok(&["dispatch", "foundations"]);
    fs::write(
        w.fake.join("end-bx_fake2-1"),
        "error:Failed to authenticate",
    )
    .unwrap();
    wait_for("the second run stuck", || {
        let r = w.run_of("foundations");
        r["sandbox"] == "bx_fake2" && r["state"] == "stuck" && r["pm_pending"] != true
    });
    assert_eq!(w.stuck_questions(), 2);
    let ledger = fs::read_to_string(w.kx_home.join("ledger/omar@mac.jsonl")).unwrap();
    assert!(ledger.contains("its sandbox (bx_fake2)"), "{ledger}");
}

/// Finding 13: with --json, a failed dispatch says so on stdout too.
#[test]
fn a_failed_dispatch_with_json_says_why_on_stdout() {
    let Some(w) = World::new() else { return };
    w.plan(true);
    fs::write(w.fake.join("bx_fake1.setup"), "failed").unwrap();
    let o = w.run(&["--json", "dispatch", "foundations"]);
    assert_eq!(code(&o), Some(1), "{}", text(&o));
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert!(
        v["error"].as_str().unwrap().contains("start script failed"),
        "{v}"
    );
    assert_eq!(v["run"]["state"], "stuck");
    assert!(!String::from_utf8_lossy(&o.stdout).contains("kxt1."));
}

/// Finding 7, without the pause hook: a `starting` run with nothing made and
/// no dispatch process, as a dispatch killed right after its reservation
/// leaves it. The watcher ends it, and the epic can be dispatched again.
#[test]
fn an_empty_starting_run_is_ended_by_the_watcher() {
    let Some(w) = World::new() else { return };
    w.plan(true);
    // A failed dispatch leaves a run to shape into what a kill leaves.
    fs::write(w.fake.join("fail-new"), "").unwrap();
    let o = w.run(&["dispatch", "foundations"]);
    assert_eq!(code(&o), Some(1), "{}", text(&o));
    fs::remove_file(w.fake.join("fail-new")).unwrap();
    wait_for("the watcher to leave", || w.status()["watcher"].is_null());
    let path = w.dispatch_dir().join("state.json");
    let mut st: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let epic = w.run_of("foundations")["epic"]
        .as_str()
        .unwrap()
        .to_string();
    let r = &mut st["runs"][&epic];
    r["state"] = "starting".into();
    r["token_id"] = Value::Null;
    r["token_revoked"] = false.into();
    r["reason"] = Value::Null;
    r["ended"] = Value::Null;
    fs::write(&path, serde_json::to_string_pretty(&st).unwrap()).unwrap();
    let o = w.run(&["dispatch", "foundations"]);
    assert_eq!(code(&o), Some(2), "{}", text(&o));
    w.ok(&["dispatch", "watch", "--detach"]);
    w.wait_state("foundations", "stuck");
    let o = w.run(&["dispatch", "foundations"]);
    assert!(o.status.success(), "{}", text(&o));
}
