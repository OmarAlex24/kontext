//! Phase 5: several machines with a git remote (a "bare" repo in a temporary
//! folder). Each "machine" has its own `KONTEXT_CONFIG_DIR`, its writer, its copy
//! of the kontext-home and its git configuration.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use common::BIN;
use serde_json::Value;

struct Net {
    dir: tempfile::TempDir,
    remote: PathBuf,
}

struct Machine {
    name: String,
    writer: String,
    config: PathBuf,
    home: PathBuf,
    gitconfig: PathBuf,
}

impl Net {
    fn new() -> Net {
        let dir = tempfile::tempdir().unwrap();
        let remote = dir.path().join("remote.git");
        let out = Command::new("git")
            .args(["init", "-q", "--bare", "-b", "main"])
            .arg(&remote)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        assert!(out.status.success());
        Net { dir, remote }
    }

    /// A machine with its writer and its git email (no kontext-home yet).
    fn machine(&self, name: &str, writer: &str, email: &str) -> Machine {
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
                "[user]\n\tname = {name}\n\temail = {email}\n[init]\n\tdefaultBranch = main\n\
                 [commit]\n\tgpgsign = false\n"
            ),
        )
        .unwrap();
        Machine {
            name: name.into(),
            writer: writer.into(),
            config,
            home: base.join("home"),
            gitconfig,
        }
    }

    /// The first machine creates the project with a remote and pushes it; the
    /// others clone it.
    fn start(&self, machines: &[&Machine], owners: &[&str], extra_toml: &str) {
        let a = machines[0];
        let remote = self.remote.to_str().unwrap();
        a.ok(&[
            "init",
            "demo",
            "--home",
            a.home.to_str().unwrap(),
            "--remote",
            remote,
        ]);
        let toml = a.home.join("project.toml");
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
        fs::write(&toml, text + extra_toml).unwrap();
        let out = a.ok(&["sync"]);
        assert!(out.contains("Pushed to origin/main"), "{out}");
        for m in &machines[1..] {
            let out = m.ok(&["clone", remote, "--home", m.home.to_str().unwrap()]);
            assert!(out.contains("Project demo cloned"), "{out}");
        }
    }
}

impl Machine {
    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .env("KONTEXT_CONFIG_DIR", &self.config)
            .env("KONTEXT_PROJECT", "demo")
            .env("KONTEXT_ACTOR", "human:omar")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", &self.gitconfig)
            .env_remove("KONTEXT_ROLE")
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

    fn id(&self, envs: &[(&str, &str)], args: &[&str]) -> String {
        let mut a = vec!["--json"];
        a.extend_from_slice(args);
        let v: Value = serde_json::from_str(&self.ok_env(envs, &a)).unwrap();
        v["id"].as_str().unwrap().to_string()
    }

    fn state(&self) -> Value {
        serde_json::from_str(&self.ok(&["debug", "state"])).unwrap()
    }

    fn git(&self, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(&self.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", &self.gitconfig)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    fn ledger_bytes(&self) -> Vec<(String, Vec<u8>)> {
        let mut out: Vec<(String, Vec<u8>)> = fs::read_dir(self.home.join("ledger"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
            .map(|p| {
                (
                    p.file_name().unwrap().to_string_lossy().into_owned(),
                    fs::read(&p).unwrap(),
                )
            })
            .collect();
        out.sort();
        out
    }

    /// `kontext debug append` with `n` journal entries, in the background.
    fn spawn_appends(&self, tag: &str, n: usize) -> std::process::Child {
        let lines: String = (0..n)
            .map(|i| {
                format!(
                    "{{\"kind\":\"journal\",\"title\":\"{tag} {i}\",\"actor\":{{\"type\":\"agent\",\"name\":\"{tag}\"}}}}\n"
                )
            })
            .collect();
        let mut child = self
            .cmd(&["debug", "append"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(lines.as_bytes())
            .unwrap();
        child
    }
}

fn wait_ok(child: std::process::Child) {
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn alerts_of(st: &Value, kind: &str) -> Vec<Value> {
    st["alerts"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|a| a["type"] == kind)
        .cloned()
        .collect()
}

#[test]
fn two_machines_write_sync_and_converge() {
    let net = Net::new();
    let a = net.machine("a", "omar@mac", "omar@example.com");
    let b = net.machine("b", "ana@laptop", "ana@example.com");
    net.start(&[&a, &b], &["omar", "ana"], "");

    for round in 0..3 {
        // Both write at once (and a third batch on A while A syncs: the lock keeps
        // a half-written line from being pushed).
        let wa = a.spawn_appends(&format!("a{round}"), 15);
        let wb = b.spawn_appends(&format!("b{round}"), 15);
        wait_ok(wa);
        wait_ok(wb);
        let during = a.spawn_appends(&format!("a{round}-sync"), 10);
        let (first, second) = if round % 2 == 0 { (&a, &b) } else { (&b, &a) };
        first.ok(&["sync"]);
        wait_ok(during);
        let out = second.ok(&["sync"]);
        assert!(out.contains("Received:"), "{out}");
        first.ok(&["sync"]);
        // Repeating changes nothing and does not collide.
        second.ok(&["sync"]);
        let out = first.ok(&["sync"]);
        assert!(out.contains("Nothing new arrived"), "{out}");
    }

    assert_eq!(a.ledger_bytes(), b.ledger_bytes());
    let (sa, sb) = (a.state(), b.state());
    assert_eq!(sa, sb);
    assert_eq!(sa["entries"], 3 * 40);
    for m in [&a, &b] {
        let v = m.ok(&["verify"]);
        assert!(v.starts_with("OK: 120 entries in 2 writers"), "{v}");
        assert_eq!(m.git(&["status", "--porcelain"]), "");
    }
    // The commits say who and how many.
    let log = a.git(&["log", "--format=%s"]);
    assert!(log.contains("kontext: omar@mac "), "{log}");
    assert!(log.contains("kontext: ana@laptop "), "{log}");

    // status: everything pushed, with the date of the last sync.
    let s: Value = serde_json::from_str(&a.ok(&["--json", "status"])).unwrap();
    assert_eq!(s["writer"], "omar@mac");
    assert_eq!(s["remote"], "origin");
    assert_eq!(s["uncommitted"], 0);
    assert_eq!(s["unpushed"], 0);
    assert_eq!(s["chains_ok"], true);
    assert!(s["last_sync"].is_string(), "{s}");
    a.ok(&["log", "something not pushed"]);
    let s: Value = serde_json::from_str(&a.ok(&["--json", "status"])).unwrap();
    assert_eq!(
        (s["uncommitted"].as_u64(), s["unpushed"].as_u64()),
        (Some(1), Some(1))
    );
    let text = a.ok(&["status"]);
    assert!(text.contains("Writer: omar@mac"), "{text}");
    assert!(text.contains("Chains: OK"), "{text}");
}

#[test]
fn clone_registers_and_prints_charter_and_owners() {
    let net = Net::new();
    let a = net.machine("a", "omar@mac", "omar@example.com");
    let b = net.machine("b", "ana@laptop", "ana@example.com");
    net.start(&[&a], &["omar"], "");
    let ch = a.id(&[], &["charter", "--objective", "Collect by card"]);
    a.ok(&["sign", &ch]);
    a.ok(&["sync"]);
    // Machine B had no writer: clone sets the one it gets.
    fs::write(b.config.join("projects.toml"), "").unwrap();
    let out = b.ok(&[
        "clone",
        net.remote.to_str().unwrap(),
        "--home",
        b.home.to_str().unwrap(),
    ]);
    assert!(out.contains("Charter: Collect by card"), "{out}");
    assert!(out.contains("Owners: omar"), "{out}");
    assert!(out.contains("Entries: 2"), "{out}");
    let cfg = fs::read_to_string(b.config.join("projects.toml")).unwrap();
    assert!(cfg.contains("writer = "), "{cfg}");
    assert!(cfg.contains("[projects.demo]"), "{cfg}");
    // Without a project.toml it is not a kontext-home: the clone is deleted.
    let empty = net.dir.path().join("empty.git");
    Command::new("git")
        .args(["init", "-q", "--bare"])
        .arg(&empty)
        .output()
        .unwrap();
    let target = net.dir.path().join("b").join("otro");
    let out = b.run(
        &[],
        &[
            "clone",
            empty.to_str().unwrap(),
            "--home",
            target.to_str().unwrap(),
        ],
    );
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("is not a kontext-home"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!target.exists());
}

#[test]
fn sync_without_remote_only_commits() {
    let net = Net::new();
    let a = net.machine("a", "omar@mac", "omar@example.com");
    a.ok(&["init", "demo", "--home", a.home.to_str().unwrap()]);
    a.ok(&["log", "one"]);
    let out = a.ok(&["sync"]);
    assert!(
        out.contains("has no remote: only the commit was made"),
        "{out}"
    );
    a.ok(&["log", "two"]);
    let out = a.ok(&["sync"]);
    assert!(out.contains("Commit "), "{out}");
    assert!(out.contains("1 own entry"), "{out}");
    assert_eq!(
        a.git(&["log", "-1", "--format=%s"]),
        "kontext: omar@mac 1 entry"
    );
    assert_eq!(a.git(&["status", "--porcelain"]), "");
}

/// An epic with a task, on both machines.
fn epic_on_both(a: &Machine, b: &Machine) -> String {
    let ch = a.id(&[], &["charter", "--objective", "Collect"]);
    a.ok(&["sign", &ch]);
    let g = a.id(&[], &["goal", "add", "Recurring collection"]);
    a.ok(&["sign", &g]);
    a.ok(&[
        "epic", "add", "Payments", "--goal", &g, "--name", "payments",
    ]);
    let t = a.id(&[], &["task", "add", "Integrate SDK", "--epic", "payments"]);
    a.ok(&["sync"]);
    b.ok(&["sync"]);
    t
}

#[test]
fn cross_machine_claim_earliest_wins_and_loser_is_told() {
    let net = Net::new();
    let a = net.machine("a", "omar@mac", "omar@example.com");
    let b = net.machine("b", "ana@laptop", "ana@example.com");
    net.start(&[&a, &b], &["omar"], "");
    let t = epic_on_both(&a, &b);
    let impl1 = [
        ("KONTEXT_ACTOR", "agent:impl-1"),
        ("KONTEXT_ROLE", "implementer"),
    ];
    let impl2 = [
        ("KONTEXT_ACTOR", "agent:impl-2"),
        ("KONTEXT_ROLE", "implementer"),
    ];
    // Each machine claims the same task without seeing the other; A first.
    let out = a.ok_env(&impl1, &["--json", "next", "--epic", "payments", "--claim"]);
    assert!(out.contains(&t), "{out}");
    std::thread::sleep(std::time::Duration::from_millis(20));
    let out = b.ok_env(&impl2, &["--json", "next", "--epic", "payments", "--claim"]);
    assert!(out.contains(&t), "{out}");

    // B syncs first (it pushes its claim), then A, then B again.
    b.ok(&["sync"]);
    let out = a.ok(&["sync"]);
    assert!(out.contains("New alerts (1)"), "{out}");
    assert!(
        out.contains(&format!(
            "{t} was claimed first by agent:impl-1 (on omar@mac)"
        )),
        "{out}"
    );
    let out = b.ok(&["sync"]);
    assert!(out.contains("was claimed first by agent:impl-1"), "{out}");

    let (sa, sb) = (a.state(), b.state());
    assert_eq!(sa, sb);
    assert_eq!(sa["claims"][&t]["actor"], "agent:impl-1");
    let lost = alerts_of(&sa, "claim_lost");
    assert_eq!(lost.len(), 1, "{sa}");
    assert_eq!(lost[0]["loser"], "agent:impl-2");

    // The loser sees it when asking for work and in the epic's brief.
    let out = b.run(&impl2, &["next", "--epic", "payments"]);
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(text.contains("stop working on it"), "{text}");
    let brief = b.ok(&["brief", "payments", "--peek"]);
    assert!(
        brief.contains("was claimed first by agent:impl-1"),
        "{brief}"
    );
    let standup = a.ok(&["standup", "--peek"]);
    assert!(
        standup.contains("was claimed first by agent:impl-1"),
        "{standup}"
    );
}

#[test]
fn owners_on_two_machines_superseding_the_same_goal_conflict() {
    let net = Net::new();
    let a = net.machine("a", "omar@mac", "omar@example.com");
    let b = net.machine("b", "ana@laptop", "ana@example.com");
    net.start(&[&a, &b], &["omar", "ana"], "");
    let ch = a.id(&[], &["charter", "--objective", "Collect"]);
    a.ok(&["sign", &ch]);
    let g = a.id(&[], &["goal", "add", "Monthly collection"]);
    a.ok(&["sign", &g]);
    a.ok(&["sync"]);
    b.ok(&["sync"]);

    let omar = [("KONTEXT_ACTOR", "human:omar")];
    let ana = [("KONTEXT_ACTOR", "human:ana")];
    let ga = a.id(&omar, &["goal", "add", "Weekly collection"]);
    a.ok_env(&omar, &["supersede", &g, &ga, "--why", "more cash flow"]);
    let gb = b.id(&ana, &["goal", "add", "Yearly collection"]);
    b.ok_env(&ana, &["supersede", &g, &gb, "--why", "fewer fees"]);

    a.ok(&["sync"]);
    let out = b.ok(&["sync"]);
    assert!(
        out.contains(&format!("conflict: {g} is superseded at the same time")),
        "{out}"
    );
    let out = a.ok(&["sync"]);
    assert!(out.contains("conflict"), "{out}");
    let (sa, sb) = (a.state(), b.state());
    assert_eq!(sa, sb);
    let c = alerts_of(&sa, "conflict");
    assert_eq!(c.len(), 1, "{sa}");
    assert_eq!(c[0]["target"], g.as_str());
    for m in [&a, &b] {
        let s = m.ok_env(&omar, &["standup", "--peek"]);
        assert!(s.contains(&format!("conflict: {g}")), "{s}");
    }
}

#[test]
fn cursor_read_on_one_machine_counts_on_the_other() {
    let net = Net::new();
    let a = net.machine("a", "omar@mac", "omar@example.com");
    let b = net.machine("b", "omar@t3", "omar@example.com");
    net.start(&[&a, &b], &["omar"], "");
    a.ok(&["log", "before reading"]);
    let ch = a.id(&[], &["charter", "--objective", "Collect"]);
    a.ok(&["sign", &ch]);
    let out = a.ok(&["standup"]);
    assert!(out.contains("What's new"), "{out}");
    let out = a.ok(&["sync"]);
    assert!(out.contains("Commit "), "{out}");
    let files = a.git(&["show", "--name-only", "--format=", "HEAD"]);
    assert!(files.contains("cursors/omar/omar@mac.json"), "{files}");

    b.ok(&["sync"]);
    let out = b.ok(&["standup", "--peek"]);
    assert!(out.contains("Nothing new since your last time"), "{out}");
    // What B writes afterwards is new, and only that.
    b.ok(&["log", "from the other machine"]);
    let out = b.ok(&["standup"]);
    assert!(out.contains("(1 new entry)"), "{out}");
    assert!(out.contains("from the other machine"), "{out}");
    b.ok(&["sync"]);
    a.ok(&["sync"]);
    // Back on A: the effective cursor is the maximum of the two files.
    let out = a.ok(&["standup", "--peek"]);
    assert!(out.contains("Nothing new since your last time"), "{out}");
    assert!(b.home.join("cursors/omar/omar@mac.json").exists());
    assert!(a.home.join("cursors/omar/omar@t3.json").exists());
}

#[test]
fn unexpected_git_conflict_aborts_and_leaves_the_repo_as_it_was() {
    let net = Net::new();
    let a = net.machine("a", "omar@mac", "omar@example.com");
    let b = net.machine("b", "ana@laptop", "ana@example.com");
    net.start(&[&a, &b], &["omar"], "");
    let edit = |m: &Machine, from: &str, to: &str| {
        let p = m.home.join("project.toml");
        let s = fs::read_to_string(&p).unwrap();
        assert!(s.contains(from));
        fs::write(&p, s.replace(from, to)).unwrap();
    };
    edit(&a, "pm = 9000", "pm = 7000");
    a.ok(&["log", "in A"]);
    let out = a.ok(&["sync", "--include-config"]);
    assert!(out.contains("Pushed"), "{out}");
    let remote_head = a.git(&["rev-parse", "HEAD"]);

    edit(&b, "pm = 9000", "pm = 5000");
    b.ok(&["log", "in B"]);
    let before_ledger = b.ledger_bytes();
    let out = b.run(&[], &["sync", "--include-config"]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("conflicts in project.toml"), "{err}");
    assert!(err.contains("The rebase was aborted"), "{err}");
    // Nothing half-done: no rebase in progress, no conflict markers, the ledger
    // unchanged and the remote untouched.
    assert!(!b.home.join(".git/rebase-merge").exists());
    assert!(!b.home.join(".git/rebase-apply").exists());
    assert_eq!(b.git(&["status", "--porcelain"]), "");
    assert_eq!(b.ledger_bytes(), before_ledger);
    let toml = fs::read_to_string(b.home.join("project.toml")).unwrap();
    assert!(
        toml.contains("pm = 5000") && !toml.contains("<<<<"),
        "{toml}"
    );
    assert_eq!(
        b.git(&["log", "-1", "--format=%s"]),
        "kontext: ana@laptop 1 entry"
    );
    let remote = Command::new("git")
        .args(["rev-parse", "main"])
        .current_dir(&net.remote)
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&remote.stdout).trim(), remote_head);
    assert!(b.ok(&["verify"]).starts_with("OK"));
    // Again, the same: it stays clean.
    let out = b.run(&[], &["sync"]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(b.git(&["status", "--porcelain"]), "");
}

#[test]
fn config_changes_need_include_config() {
    let net = Net::new();
    let a = net.machine("a", "omar@mac", "omar@example.com");
    let b = net.machine("b", "ana@laptop", "ana@example.com");
    net.start(&[&a, &b], &["omar"], "");
    let p = b.home.join("project.toml");
    let local = fs::read_to_string(&p)
        .unwrap()
        .replace("pm = 9000", "pm = 4000");
    fs::write(&p, &local).unwrap();
    // What arrives does not touch project.toml: the ledger is pushed, and
    // project.toml keeps its local change and a warning is shown.
    a.ok(&["log", "in A"]);
    a.ok(&["sync"]);
    b.ok(&["log", "in B"]);
    let out = b.ok(&["sync"]);
    assert!(
        out.contains("project.toml has changes that are not pushed automatically"),
        "{out}"
    );
    assert!(out.contains("Received: 1 entry"), "{out}");
    assert_eq!(fs::read_to_string(&p).unwrap(), local);
    assert_eq!(b.git(&["status", "--porcelain"]), "M project.toml");
    a.ok(&["sync"]);
    assert!(
        !fs::read_to_string(a.home.join("project.toml"))
            .unwrap()
            .contains("pm = 4000")
    );
    // If what arrives also changes project.toml, nothing is pulled.
    let pa = a.home.join("project.toml");
    let s = fs::read_to_string(&pa)
        .unwrap()
        .replace("coordinator = 9000", "coordinator = 8000");
    fs::write(&pa, s).unwrap();
    a.ok(&["sync", "--include-config"]);
    let out = b.run(&[], &["sync"]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("changes also arrived from the remote"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(fs::read_to_string(&p).unwrap(), local);
    assert_eq!(b.git(&["status", "--porcelain"]), "M project.toml");
}

#[test]
fn human_actor_warns_when_git_email_is_someone_else() {
    let net = Net::new();
    let a = net.machine("a", "omar@mac", "omar@example.com");
    net.start(
        &[&a],
        &["omar"],
        "\n[people]\nomar = { emails = [\"omar@example.com\"] }\nana = { emails = [\"ana@example.com\"] }\n",
    );
    let out = a.run(&[("KONTEXT_ACTOR", "human:ana")], &["log", "hola"]);
    assert!(out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("you write as human:ana, but this machine's git email (omar@example.com) belongs to omar"),
        "{err}"
    );
    let out = a.run(&[("KONTEXT_ACTOR", "human:omar")], &["log", "hola"]);
    assert!(
        out.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // The default standup reader comes from the email, not from user.name.
    let cfg = fs::read_to_string(&a.gitconfig)
        .unwrap()
        .replace("omar@example.com", "ana@example.com");
    fs::write(&a.gitconfig, cfg).unwrap();
    let out = a.run(&[("KONTEXT_ACTOR", "agent:x")], &["standup", "--peek"]);
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("# Standup of demo for ana"),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

// ---------------------------------------------------------------- signatures

fn keygen(dir: &Path, name: &str) -> (PathBuf, String) {
    let key = dir.join(name);
    let out = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-C", name, "-f"])
        .arg(&key)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let public = fs::read_to_string(key.with_extension("pub"))
        .unwrap()
        .trim()
        .to_string();
    (key.with_extension("pub"), public)
}

fn set_signing(m: &Machine, key: &Path) {
    let p = m.config.join("projects.toml");
    let mut s = fs::read_to_string(&p).unwrap();
    if let Some(i) = s.find("\n[signing]") {
        s.truncate(i);
    }
    s.push_str(&format!("\n[signing]\nkey = \"{}\"\n", key.display()));
    fs::write(&p, s).unwrap();
}

fn set_require(m: &Machine, on: bool) {
    let p = m.home.join("project.toml");
    let s = fs::read_to_string(&p).unwrap();
    let s = s.replace(
        &format!("require_signatures = {}", !on),
        &format!("require_signatures = {on}"),
    );
    fs::write(&p, s).unwrap();
}

fn ledger_line(m: &Machine, id: &str) -> Value {
    fs::read_to_string(m.home.join(format!("ledger/{}.jsonl", m.writer)))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .find(|v| v["id"] == id)
        .unwrap()
}

#[test]
fn signatures_roundtrip_tamper_require_and_non_owner_keys() {
    let net = Net::new();
    let keys = net.dir.path().join("keys");
    fs::create_dir_all(&keys).unwrap();
    let (omar_key, omar_pub) = keygen(&keys, "omar");
    let (ana_key, ana_pub) = keygen(&keys, "ana");
    let a = net.machine("a", "omar@mac", "omar@example.com");
    net.start(
        &[&a],
        &["omar"],
        &format!(
            "\n[people]\nomar = {{ emails = [\"omar@example.com\"], keys = [\"{omar_pub}\"] }}\n\
             ana = {{ emails = [\"ana@example.com\"], keys = [\"{ana_pub}\"] }}\n"
        ),
    );

    // Without a key, nothing is signed.
    let d0 = a.id(&[], &["decide", "Unsigned"]);
    assert!(ledger_line(&a, &d0)["sig"].is_null());

    // With a key, decide, sign and supersede from omar come out signed; a journal entry does not.
    set_signing(&a, &omar_key);
    let d1 = a.id(&[], &["decide", "Use Rust", "--why", "one binary"]);
    let line = ledger_line(&a, &d1);
    let sig = line["sig"].as_str().unwrap().to_string();
    assert!(sig.starts_with("-----BEGIN SSH SIGNATURE-----"), "{sig}");
    let j = a.id(&[], &["log", "a journal entry"]);
    assert!(ledger_line(&a, &j)["sig"].is_null());
    let out = a.ok(&["verify"]);
    assert!(out.contains("Signatures: 1 valid, 0 invalid"), "{out}");

    // Signed with ana's key, in omar's name: it is not omar's.
    set_signing(&a, &ana_key);
    let d2 = a.id(&[], &["decide", "Use SQLite"]);
    // Ana signs her own with her key, but she is not an owner.
    let d3 = a.id(
        &[("KONTEXT_ACTOR", "human:ana")],
        &["propose", "Use Postgres"],
    );
    let out = a.run(
        &[("KONTEXT_ACTOR", "human:ana")],
        &["decide", "Use Postgres"],
    );
    assert_eq!(out.status.code(), Some(2), "a non-owner cannot decide");
    let out = a.run(&[], &["verify"]);
    assert_eq!(out.status.code(), Some(1));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains(&format!("invalid signature on {d2}")),
        "{text}"
    );
    assert!(text.contains("Signatures: 1 valid, 1 invalid"), "{text}");

    // A tampered signature (another entry's signature pasted into d1) does not
    // verify either, even though the chain is still fine (sig is not in the hash).
    let path = a.home.join("ledger/omar@mac.jsonl");
    let other = ledger_line(&a, &d2)["sig"].as_str().unwrap().to_string();
    let text = fs::read_to_string(&path).unwrap();
    let swapped: String = text
        .lines()
        .map(|l| {
            let mut v: Value = serde_json::from_str(l).unwrap();
            if v["id"] == d1.as_str() {
                v["sig"] = Value::String(other.clone());
            }
            serde_json::to_string(&v).unwrap() + "\n"
        })
        .collect();
    fs::write(&path, swapped).unwrap();
    let out = a.run(&[], &["verify"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("OK: "), "{text}");
    assert!(
        text.contains(&format!("invalid signature on {d1}")),
        "{text}"
    );
    fs::write(&path, text_restore(&path, &d1, &sig)).unwrap();
    let out = a.run(&[], &["verify"]);
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("Signatures: 1 valid, 1 invalid"),
        "{out:?}"
    );

    // require_signatures: the unsigned one (d0) and the one signed with another
    // key (d2) are stored but do not count; omar's signed one (d1) does.
    set_require(&a, true);
    a.ok(&["reindex"]);
    let st = a.state();
    assert_eq!(st["status"][&d0], "ignored", "{st}");
    assert_eq!(st["status"][&d1], "accepted", "{st}");
    assert_eq!(st["status"][&d2], "ignored", "{st}");
    assert_eq!(st["status"][&d3], "pending", "{st}");
    let bad = alerts_of(&st, "bad_signature");
    let ids: Vec<&str> = bad.iter().map(|x| x["id"].as_str().unwrap()).collect();
    assert_eq!(ids, [d0.as_str(), d2.as_str()], "{st}");
    let standup = a.ok(&["standup", "--peek"]);
    assert!(
        standup.contains("signature invalid or missing"),
        "{standup}"
    );

    // The result is kept in the index: without ssh-keygen at hand, the state
    // comes out the same.
    let out = a.run(&[("PATH", "/nonexistent")], &["debug", "state"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(serde_json::from_slice::<Value>(&out.stdout).unwrap(), st);

    // With require_signatures, writing with a key that is not yours is refused
    // before writing; so is writing without a key.
    let out = a.run(&[], &["decide", "Other"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("does not verify for omar"));
    set_signing(&a, &omar_key);
    let d4 = a.id(&[], &["decide", "Signed correctly"]);
    assert_eq!(a.state()["status"][&d4], "accepted");
    let b = net.machine("b", "omar@t3", "omar@example.com");
    a.ok(&["sync", "--include-config"]);
    b.ok(&[
        "clone",
        net.remote.to_str().unwrap(),
        "--home",
        b.home.to_str().unwrap(),
    ]);
    let out = b.run(&[], &["decide", "No key"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("has no key to sign with"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // The other machine reaches the same state by verifying the signatures.
    assert_eq!(a.state(), b.state());
}

/// Puts back the original signature of `id`.
fn text_restore(path: &Path, id: &str, sig: &str) -> String {
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|l| {
            let mut v: Value = serde_json::from_str(l).unwrap();
            if v["id"] == id {
                v["sig"] = Value::String(sig.to_string());
            }
            serde_json::to_string(&v).unwrap() + "\n"
        })
        .collect()
}

/// What was signed when the command was called `ctx` (namespace `ctx`) still
/// verifies; a signature from another namespace does not.
#[test]
fn signatures_from_the_old_ctx_namespace_still_verify() {
    let net = Net::new();
    let keys = net.dir.path().join("keys");
    fs::create_dir_all(&keys).unwrap();
    let (omar_key, omar_pub) = keygen(&keys, "omar");
    let a = net.machine("a", "omar@mac", "omar@example.com");
    net.start(
        &[&a],
        &["omar"],
        &format!(
            "\n[people]\nomar = {{ emails = [\"omar@example.com\"], keys = [\"{omar_pub}\"] }}\n"
        ),
    );
    set_signing(&a, &omar_key);
    let d1 = a.id(&[], &["decide", "Use Rust"]);
    let out = a.ok(&["verify"]);
    assert!(out.contains("Signatures: 1 valid, 0 invalid"), "{out}");
    let hash = ledger_line(&a, &d1)["hash"].as_str().unwrap().to_string();
    let path = a.home.join("ledger/omar@mac.jsonl");
    for (namespace, valid) in [("ctx", true), ("otro", false)] {
        let mut child = Command::new("ssh-keygen")
            .args(["-q", "-Y", "sign", "-n", namespace, "-f"])
            .arg(&omar_key)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write as _;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(hash.as_bytes())
            .unwrap();
        let signed = child.wait_with_output().unwrap();
        assert!(signed.status.success(), "{signed:?}");
        let sig = String::from_utf8(signed.stdout).unwrap();
        fs::write(&path, text_restore(&path, &d1, sig.trim())).unwrap();
        let out = a.run(&[], &["verify"]);
        let text = String::from_utf8_lossy(&out.stdout);
        let want = if valid {
            "Signatures: 1 valid, 0 invalid"
        } else {
            "Signatures: 0 valid, 1 invalid"
        };
        assert!(text.contains(want), "{namespace}: {text}");
    }
}
