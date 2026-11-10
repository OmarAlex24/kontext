//! Sessions in linked git worktrees (`git worktree add`) whose `.kontext.toml`
//! and `.claude/` exist only in the main checkout: the project is found
//! through the main checkout, the role follows `worktree_role`, only a
//! worktree marked as the PM's captures (into the main checkout's `.pm/`),
//! and `kontext install --link-only`, `--worktree-role` and `--local`.
//!
//! Every run sets PATH, HOME, KONTEXT_CONFIG_DIR and CLAUDE_CONFIG_DIR, so
//! nothing of the real machine is read.

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};

use serde_json::{Value, json};

/// Far from UTC, so a date in the wrong zone shows.
const TZ: &str = "Asia/Tokyo";
/// 2026-11-07 10:42 in Tokyo.
const NOW: &str = "2026-11-07T01:42:00Z";
const DUMP: &str = ".pm/dumps/2026-11-07.md";

struct W {
    dir: tempfile::TempDir,
}

impl W {
    fn root(&self) -> PathBuf {
        fs::canonicalize(self.dir.path()).unwrap()
    }
    fn config(&self) -> PathBuf {
        self.root().join("config")
    }
    fn khome(&self) -> PathBuf {
        self.root().join("khome")
    }
    /// The main checkout of the code repo.
    fn main(&self) -> PathBuf {
        self.root().join("billing-app")
    }
    fn log(&self) -> String {
        fs::read_to_string(self.khome().join(".kontext/hooks.log")).unwrap_or_default()
    }

    fn cmd(&self, args: &[&str], cwd: &Path) -> Command {
        let mut c = Command::new(common::BIN);
        c.args(args)
            .current_dir(cwd)
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", self.root().join("h"))
            .env("KONTEXT_CONFIG_DIR", self.config())
            .env("CLAUDE_CONFIG_DIR", self.root().join("claude"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("TZ", TZ)
            .env("KONTEXT_DISTILL_DISABLE", "1")
            .env("KONTEXT_ACTOR", "human:omar")
            .env_remove("KONTEXT_PROJECT")
            .env_remove("KONTEXT_ROLE")
            .env_remove("KONTEXT_EPIC")
            .env_remove("KONTEXT_CAPTURE_DISABLE")
            .env_remove("KONTEXT_NOW");
        c
    }

    fn run(&self, args: &[&str], cwd: &Path) -> Output {
        self.cmd(args, cwd).output().unwrap()
    }

    fn ok(&self, args: &[&str], cwd: &Path) -> String {
        let out = self.run(args, cwd);
        assert!(
            out.status.success(),
            "{args:?} in {}: {}{}",
            cwd.display(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    fn git(&self, dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
            .args(args)
            .current_dir(dir)
            .env("HOME", self.root().join("h"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// `git worktree add` of a new branch `name`, at `<root>/<name>` or `at`.
    fn worktree(&self, name: &str, at: Option<PathBuf>) -> PathBuf {
        let p = at.unwrap_or_else(|| self.root().join(name));
        self.git(
            &self.main(),
            &["worktree", "add", "-q", "-b", name, p.to_str().unwrap()],
        );
        fs::canonicalize(p).unwrap()
    }

    fn hook_cmd(&self, event: &str, envs: &[(&str, &str)]) -> Command {
        let mut c = self.cmd(&["hook", event], &self.root());
        c.env("KONTEXT_NOW", NOW);
        for (k, v) in envs {
            c.env(k, v);
        }
        c.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        c
    }

    fn spawn_hook(&self, event: &str, envs: &[(&str, &str)], payload: &Value) -> Child {
        let mut child = self.hook_cmd(event, envs).spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.to_string().as_bytes())
            .unwrap();
        child
    }

    /// Runs a hook; capture hooks print nothing, and every hook exits with 0.
    fn hook(&self, event: &str, envs: &[(&str, &str)], payload: &Value) -> String {
        let out = self
            .spawn_hook(event, envs, payload)
            .wait_with_output()
            .unwrap();
        assert_eq!(out.status.code(), Some(0), "{event}");
        assert!(out.stderr.is_empty(), "{event}: {:?}", out.stderr);
        String::from_utf8(out.stdout).unwrap()
    }

    /// The role the start hook gave a session in `cwd` (from `hooks.log`).
    fn start_role(&self, cwd: &Path, envs: &[(&str, &str)]) -> String {
        let before = self.log().len();
        let out = self.hook(
            "session-start",
            envs,
            &json!({"session_id": "s-role", "source": "startup", "cwd": cwd}),
        );
        assert!(out.contains("additionalContext"), "{out}");
        let log = self.log();
        let line = log[before..].lines().last().unwrap_or_default().to_string();
        line.split_whitespace()
            .find_map(|w| w.strip_prefix("role="))
            .unwrap_or_else(|| panic!("no role in {line:?}"))
            .to_string()
    }

    fn prompt(&self, cwd: &Path, transcript: &str, text: &str, id: &str) {
        let out = self.hook(
            "user-prompt-submit",
            &[],
            &self.prompt_payload(cwd, transcript, text, id),
        );
        assert!(out.is_empty(), "{out}");
    }

    fn prompt_payload(&self, cwd: &Path, transcript: &str, text: &str, id: &str) -> Value {
        json!({"session_id": transcript, "cwd": cwd, "prompt": text, "prompt_id": id,
               "transcript_path": self.root().join("transcripts").join(format!("{transcript}.jsonl")),
               "hook_event_name": "UserPromptSubmit"})
    }

    fn set_capture(&self, section: &str) {
        let toml = self.khome().join("project.toml");
        let t = fs::read_to_string(&toml).unwrap();
        fs::write(
            &toml,
            t.replacen(
                "\n[claims]",
                &format!("\n[capture]\n{section}\n\n[claims]"),
                1,
            ),
        )
        .unwrap();
    }
}

/// Two projects registered (`demo` and `other`), and a code repo with one
/// commit whose main checkout is linked to `demo` by its own `.kontext.toml`
/// (untracked, as in a public repo).
fn setup() -> W {
    let w = W {
        dir: tempfile::tempdir().unwrap(),
    };
    for d in ["h", "claude", "config", "billing-app", "transcripts"] {
        fs::create_dir_all(w.root().join(d)).unwrap();
    }
    fs::write(w.config().join("projects.toml"), "writer = \"omar@test\"\n").unwrap();
    let k = w.khome();
    w.ok(&["init", "demo", "--home", k.to_str().unwrap()], &w.root());
    let k2 = w.root().join("khome-other");
    w.ok(
        &["init", "other", "--home", k2.to_str().unwrap()],
        &w.root(),
    );
    w.git(&w.main(), &["init", "-q"]);
    w.git(&w.main(), &["commit", "-q", "--allow-empty", "-m", "init"]);
    let out = w.ok(&["--project", "demo", "install", "--link-only"], &w.main());
    assert!(out.contains("+project = \"demo\""), "{out}");
    w
}

fn project_of(w: &W, cwd: &Path) -> String {
    let out = w.ok(&["status"], cwd);
    out.lines().next().unwrap().to_string()
}

#[test]
fn the_project_is_found_through_the_main_checkout() {
    let w = setup();
    let wt = w.worktree("agent-1", None);
    assert!(!wt.join(".kontext.toml").exists());
    // Two projects are registered: only the main checkout's file can say demo.
    assert!(project_of(&w, &wt).starts_with("Project demo in"));
    let sub = wt.join("src/invoices");
    fs::create_dir_all(&sub).unwrap();
    assert!(project_of(&w, &sub).starts_with("Project demo in"));

    // A worktree's own file wins.
    let own = w.worktree("agent-2", None);
    fs::write(own.join(".kontext.toml"), "project = \"other\"\n").unwrap();
    assert!(project_of(&w, &own).starts_with("Project other in"));
    // --project and $KONTEXT_PROJECT still win over both.
    let out = w.ok(&["--project", "other", "status"], &wt);
    assert!(out.starts_with("Project other in"), "{out}");
    let out = w
        .cmd(&["status"], &wt)
        .env("KONTEXT_PROJECT", "other")
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("Project other in"));

    // Without the main checkout's file, nothing is found (two projects).
    fs::remove_file(w.main().join(".kontext.toml")).unwrap();
    let out = w.run(&["status"], &wt);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("several projects are registered"),
        "{out:?}"
    );

    // A bare repository has no main checkout: a .kontext.toml in its git
    // dir is not one.
    let bare = w.root().join("shop.git");
    fs::create_dir_all(&bare).unwrap();
    w.git(&bare, &["init", "-q", "--bare"]);
    w.git(&wt, &["push", "-q", bare.to_str().unwrap(), "agent-1:main"]);
    fs::write(bare.join(".kontext.toml"), "project = \"demo\"\n").unwrap();
    let bwt = w.root().join("shop-wt");
    w.git(
        &bare,
        &["worktree", "add", "-q", bwt.to_str().unwrap(), "main"],
    );
    let out = w.run(&["status"], &bwt);
    assert!(!out.status.success(), "{out:?}");

    // A .kontext.toml in a folder above the checkouts (a parent of several
    // repos) is still found, after the worktree's and the main checkout's.
    fs::write(w.root().join(".kontext.toml"), "project = \"other\"\n").unwrap();
    for dir in [&wt, &bwt] {
        assert!(project_of(&w, dir).starts_with("Project other in"));
    }
    fs::write(w.main().join(".kontext.toml"), "project = \"demo\"\n").unwrap();
    assert!(project_of(&w, &wt).starts_with("Project demo in"));
}

#[test]
fn the_role_of_a_session_follows_the_order() {
    let w = setup();
    let wt = w.worktree("agent-1", None);
    // A worktree under the main checkout: walking up would reach the main
    // checkout's file, but it is not the worktree's own.
    let nested = w.worktree("agent-2", Some(w.main().join(".worktrees/agent-2")));

    // No worktree_role: the main checkout's default_role.
    assert_eq!(w.start_role(&w.main(), &[]), "pm");
    assert_eq!(w.start_role(&wt, &[]), "pm");

    // worktree_role, set without editing the file by hand.
    let out = w.ok(
        &["install", "--link-only", "--worktree-role", "agent"],
        &w.main(),
    );
    assert!(out.contains("+worktree_role = \"agent\""), "{out}");
    assert_eq!(w.start_role(&wt, &[]), "agent");
    assert_eq!(w.start_role(&nested, &[]), "agent");
    assert_eq!(w.start_role(&wt.join("."), &[]), "agent");
    // The main checkout keeps its own default_role.
    assert_eq!(w.start_role(&w.main(), &[]), "pm");
    // $KONTEXT_ROLE first.
    assert_eq!(
        w.start_role(&wt, &[("KONTEXT_ROLE", "coordinator")]),
        "coordinator"
    );

    // A worktree marked as the PM's: its own default_role wins.
    let pm = w.worktree("pm-desk", None);
    w.ok(&["install", "--link-only"], &pm);
    assert_eq!(w.start_role(&pm, &[]), "pm");
    // Its own file with another default_role.
    let own = w.worktree("agent-3", None);
    w.ok(&["install", "--link-only", "--role", "tester"], &own);
    assert_eq!(w.start_role(&own, &[]), "tester");

    // The main checkout's file with neither key: pm.
    fs::write(w.main().join(".kontext.toml"), "project = \"demo\"\n").unwrap();
    assert_eq!(w.start_role(&wt, &[]), "pm");
    assert_eq!(w.start_role(&w.main(), &[]), "pm");
    // Only worktree_role: worktrees get it, the main checkout does not.
    fs::write(
        w.main().join(".kontext.toml"),
        "project = \"demo\"\nworktree_role = \"agent\"\n",
    )
    .unwrap();
    assert_eq!(w.start_role(&wt, &[]), "agent");
    assert_eq!(w.start_role(&w.main(), &[]), "pm");
}

/// Path, size and content hash of every file under `dir`, `.git` included.
fn snapshot(dir: &Path) -> BTreeMap<PathBuf, (u64, String)> {
    fn walk(d: &Path, out: &mut BTreeMap<PathBuf, (u64, String)>) {
        let Ok(rd) = fs::read_dir(d) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if e.file_type().unwrap().is_dir() {
                out.insert(p.clone(), (0, "dir".into()));
                walk(&p, out);
            } else {
                let bytes = fs::read(&p).unwrap_or_default();
                out.insert(p, (bytes.len() as u64, kontext::model::sha256_hex(&bytes)));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, &mut out);
    out
}

#[test]
fn link_only_writes_one_file_and_worktree_role_survives_install() {
    let w = setup();
    let wt = w.worktree("pm-desk", None);
    let before = snapshot(&w.root());
    let out = w.ok(&["install", "--link-only", "--dry-run"], &wt);
    assert!(out.contains("+default_role = \"pm\""), "{out}");
    assert!(out.contains("nothing was written"), "{out}");
    assert_eq!(snapshot(&w.root()), before, "the dry run wrote something");

    let out = w.ok(&["install", "--link-only"], &wt);
    assert!(out.contains("Wrote"), "{out}");
    let mut after = snapshot(&w.root());
    let link = wt.join(".kontext.toml");
    assert!(after.remove(&link).is_some());
    assert_eq!(
        after, before,
        "--link-only wrote something besides {link:?}"
    );
    assert_eq!(
        fs::read_to_string(&link).unwrap(),
        "project = \"demo\"\ndefault_role = \"pm\"\n"
    );
    let out = w.ok(&["install", "--link-only"], &wt);
    assert!(out.contains("nothing to change"), "{out}");
    // worktree_role belongs to the main checkout's file.
    let out = w.run(&["install", "--worktree-role", "agent"], &wt);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("main checkout"));

    // Set with a full install, kept by every later one (and by --local).
    let main = w.main();
    w.ok(&["install", "--worktree-role", "agent"], &main);
    let want = "project = \"demo\"\ndefault_role = \"pm\"\nworktree_role = \"agent\"\n";
    assert_eq!(
        fs::read_to_string(main.join(".kontext.toml")).unwrap(),
        want
    );
    w.ok(&["install"], &main);
    w.ok(&["install", "--local"], &main);
    let out = w.ok(&["install", "--role", "coordinator"], &main);
    assert!(out.contains("nothing to change"), "{out}");
    assert_eq!(
        fs::read_to_string(main.join(".kontext.toml")).unwrap(),
        want
    );
    // Changed in place, not added twice.
    w.ok(&["install", "--worktree-role", "implementer"], &main);
    assert_eq!(
        fs::read_to_string(main.join(".kontext.toml")).unwrap(),
        "project = \"demo\"\ndefault_role = \"pm\"\nworktree_role = \"implementer\"\n"
    );
}

fn dump_of(dir: &Path) -> String {
    fs::read_to_string(dir.join(DUMP)).unwrap_or_default()
}

#[test]
fn only_the_pms_worktree_captures_into_the_main_checkouts_pm() {
    let w = setup();
    w.set_capture("enabled = true");
    let agent = w.worktree("agent-1", None);
    let pm = w.worktree("pm-desk", None);
    w.ok(&["install", "--link-only"], &pm);

    // A delegated agent: no KONTEXT_ROLE, it counts as pm, but its worktree
    // is not marked.
    w.prompt(
        &agent,
        "s-agent",
        "brief for the invoices export task",
        "p-1",
    );
    assert!(!agent.join(".pm").exists());
    assert!(!w.main().join(".pm").exists());
    let log = w.log();
    assert!(
        log.contains("capture: a linked git worktree not marked as the PM's, nothing written")
            && log.contains("kontext install --link-only"),
        "{log}"
    );
    assert!(!log.contains("brief"), "{log}");

    // The PM's worktree writes into the main checkout's .pm/.
    w.prompt(&pm, "s-pm", "drop the old export", "p-2");
    assert!(!pm.join(".pm").exists());
    assert_eq!(
        dump_of(&w.main()),
        "## 2026-11-07 10:42\n\n**User:**\n\ndrop the old export\n"
    );
    assert_eq!(
        fs::read_to_string(w.main().join(".pm/.gitignore")).unwrap(),
        "dumps/\n"
    );

    // With worktree_role, the agent is not pm: the role check stays.
    w.ok(
        &["install", "--link-only", "--worktree-role", "agent"],
        &w.main(),
    );
    w.prompt(&agent, "s-agent", "another brief", "p-3");
    assert!(w.log().contains("role \"agent\" is not pm"), "{}", w.log());
    // A tracked .kontext.toml is in every worktree: it marks none.
    w.git(&w.main(), &["add", ".kontext.toml"]);
    w.git(&w.main(), &["commit", "-q", "-m", "link"]);
    let tracked = w.worktree("agent-2", None);
    fs::write(
        tracked.join(".kontext.toml"),
        "project = \"demo\"\ndefault_role = \"pm\"\n",
    )
    .unwrap();
    w.prompt(&tracked, "s-tracked", "a third brief", "p-4");
    assert!(!dump_of(&w.main()).contains("third"));
    assert!(!tracked.join(".pm").exists());
    // The log says why `--link-only` would not mark it.
    assert!(
        w.log().contains("its .kontext.toml is tracked by git"),
        "{}",
        w.log()
    );
    // Its own file without default_role: what to set.
    let bare_link = w.worktree("agent-3", None);
    fs::write(bare_link.join(".kontext.toml"), "project = \"demo\"\n").unwrap();
    w.git(&bare_link, &["rm", "-q", "--cached", ".kontext.toml"]);
    w.prompt(&bare_link, "s-own", "a fourth brief", "p-5");
    assert!(
        w.log().contains("set default_role = \"pm\" in"),
        "{}",
        w.log()
    );
    assert!(!dump_of(&w.main()).contains("fourth"));
}

#[test]
fn with_worktrees_true_each_worktree_writes_its_own_pm() {
    let w = setup();
    w.set_capture("enabled = true\nworktrees = true");
    let agent = w.worktree("agent-1", None);
    w.prompt(&agent, "s-agent", "written in the worktree", "p-1");
    assert_eq!(
        dump_of(&agent),
        "## 2026-11-07 10:42\n\n**User:**\n\nwritten in the worktree\n"
    );
    assert!(!w.main().join(".pm").exists());
    // `kontext ideas` there reads the worktree's own .pm/, as before.
    fs::write(
        agent.join(".pm/items.md"),
        "| id | idea | source | state | where | evidence |\n|---|---|---|---|---|---|\n\
         | P1 | CSV export | dumps/2026-11-07.md:5 | pending | | |\n",
    )
    .unwrap();
    let v: Value = serde_json::from_str(&w.ok(&["--json", "ideas"], &agent)).unwrap();
    assert_eq!(
        fs::canonicalize(v["path"].as_str().unwrap()).unwrap(),
        agent.join(".pm/items.md")
    );
}

#[test]
fn the_same_prompt_twice_is_written_once_from_either_checkout() {
    let w = setup();
    w.set_capture("enabled = true");
    let pm = w.worktree("pm-desk", None);
    w.ok(&["install", "--link-only"], &pm);
    // Hooks in both settings files deliver each prompt twice, here from the
    // main checkout and the PM's worktree.
    w.prompt(&w.main(), "s-1", "add a CSV export of paid invoices", "p-1");
    w.prompt(&pm, "s-1", "add a CSV export of paid invoices", "p-1");
    w.prompt(&pm, "s-1", "add a CSV export of paid invoices", "p-1");
    assert_eq!(dump_of(&w.main()).matches("CSV export").count(), 1);
    assert!(w.log().contains("the prompt was already written"));

    // The same answer through the question tool, seen by two stops.
    let tp = w.root().join("transcripts/s-1.jsonl");
    let answer = json!({"type": "user", "isSidechain": false, "timestamp": "2026-11-07T01:43:00Z",
        "uuid": "r-1",
        "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": "ok"}]},
        "toolUseResult": {"questions": [{"question": "Keep the old export?", "header": "Export"}],
                          "answers": {"Keep the old export?": "drop it"}}});
    fs::write(&tp, format!("{answer}\n")).unwrap();
    // The mark was taken before the file existed (offset 0): the answer is new.
    for cwd in [w.main(), pm.clone()] {
        let out = w.hook(
            "stop",
            &[],
            &json!({"session_id": "s-1", "cwd": cwd, "transcript_path": tp}),
        );
        assert!(out.is_empty());
    }
    let d = dump_of(&w.main());
    assert_eq!(d.matches("drop it").count(), 1, "{d}");
    assert_eq!(d.matches("## 2026-11-07").count(), 2, "{d}");
}

#[test]
fn two_checkouts_appending_at_once_do_not_interleave() {
    let w = setup();
    w.set_capture("enabled = true");
    let pm = w.worktree("pm-desk", None);
    w.ok(&["install", "--link-only"], &pm);
    let n = 8;
    let text = |i: usize| -> String {
        let c = (b'a' + i as u8) as char;
        format!("invoice note {i} {}", c.to_string().repeat(2500))
    };
    let mut children = Vec::new();
    for i in 0..n {
        let (cwd, session) = if i % 2 == 0 {
            (w.main(), "s-main")
        } else {
            (pm.clone(), "s-pm")
        };
        let payload = w.prompt_payload(&cwd, session, &text(i), &format!("p-{i}"));
        children.push(w.spawn_hook("user-prompt-submit", &[], &payload));
    }
    for c in children {
        let out = c.wait_with_output().unwrap();
        assert_eq!(out.status.code(), Some(0));
    }
    let all: String = fs::read_dir(w.main().join(".pm/dumps"))
        .unwrap()
        .map(|e| fs::read_to_string(e.unwrap().path()).unwrap())
        .collect();
    assert_eq!(all.matches("## 2026-11-07 10:42").count(), n, "{all}");
    for i in 0..n {
        let entry = format!("**User:**\n\n{}\n", text(i));
        assert_eq!(all.matches(&entry).count(), 1, "entry {i} is not whole");
    }
}

#[test]
fn ideas_from_a_worktree_read_the_main_checkouts_pm() {
    let w = setup();
    let pm_dir = w.main().join(".pm");
    fs::create_dir_all(&pm_dir).unwrap();
    fs::write(
        pm_dir.join("items.md"),
        "| id | idea | source | state | where | evidence |\n|---|---|---|---|---|---|\n\
         | P1 | CSV export of invoices | dumps/2026-11-07.md:5 | pending | | |\n",
    )
    .unwrap();
    let pm = w.worktree("pm-desk", None);
    w.ok(&["install", "--link-only"], &pm);
    let agent = w.worktree("agent-1", None);
    for cwd in [&pm, &agent, &w.main()] {
        let v: Value = serde_json::from_str(&w.ok(&["--json", "ideas"], cwd)).unwrap();
        let path = PathBuf::from(v["path"].as_str().unwrap());
        assert_eq!(
            fs::canonicalize(path).unwrap(),
            fs::canonicalize(pm_dir.join("items.md")).unwrap(),
            "{}",
            cwd.display()
        );
        assert_eq!(v["total"], 1);
    }
}

fn settings(dir: &Path, name: &str) -> Option<String> {
    fs::read_to_string(dir.join(".claude").join(name)).ok()
}

fn json_of(text: &str) -> Value {
    serde_json::from_str(text).unwrap()
}

#[test]
fn install_local_moves_kontexts_entries_and_nothing_else() {
    let w = setup();
    let main = w.main();
    let theirs = json!({
        "model": "sonnet",
        "hooks": {"PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "/usr/bin/true"}]}]},
        "permissions": {"allow": ["Bash(ls:*)"], "deny": ["Read(.env)"]}
    });
    fs::create_dir_all(main.join(".claude")).unwrap();
    fs::write(
        main.join(".claude/settings.json"),
        serde_json::to_string_pretty(&theirs).unwrap(),
    )
    .unwrap();
    w.ok(&["install"], &main);
    let shared = settings(&main, "settings.json").unwrap();
    assert!(shared.contains("hook session-start") && shared.contains("Bash(kontext:*)"));

    // The dry run lists both files and writes nothing.
    let before = snapshot(&w.root());
    let out = w.ok(&["install", "--local", "--dry-run"], &main);
    assert!(out.contains("settings.json (before)"), "{out}");
    assert!(out.contains("settings.local.json (before)"), "{out}");
    assert_eq!(snapshot(&w.root()), before);

    let out = w.ok(&["install", "--local"], &main);
    assert!(out.contains("settings.local.json"), "{out}");
    // settings.json is what it was before kontext.
    assert_eq!(json_of(&settings(&main, "settings.json").unwrap()), theirs);
    let local = json_of(&settings(&main, "settings.local.json").unwrap());
    assert!(
        local["hooks"]["SessionStart"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .ends_with("hook session-start")
    );
    assert_eq!(local["permissions"]["allow"], json!(["Bash(kontext:*)"]));

    // A later plain install keeps them there and says so.
    let shared = settings(&main, "settings.json").unwrap();
    let out = w.ok(&["install"], &main);
    assert!(
        out.contains("kept there") && out.contains("nothing to change"),
        "{out}"
    );
    assert_eq!(settings(&main, "settings.json").unwrap(), shared);
    let out = w.ok(&["install", "--local"], &main);
    assert!(out.contains("nothing to change"), "{out}");

    // As left by a copy by hand: in both files. A plain install touches
    // only the local one and says so; --local removes the copy.
    let both = w.root().join("both");
    fs::create_dir_all(&both).unwrap();
    w.git(&both, &["init", "-q"]);
    w.ok(&["--project", "demo", "install"], &both);
    let shared = settings(&both, "settings.json").unwrap();
    fs::write(both.join(".claude/settings.local.json"), &shared).unwrap();
    let out = w.ok(&["install"], &both);
    assert!(out.contains("in both"), "{out}");
    assert_eq!(settings(&both, "settings.json").unwrap(), shared);
    w.ok(&["install", "--local"], &both);
    assert_eq!(
        json_of(&settings(&both, "settings.json").unwrap()),
        json!({})
    );

    // Only the permission in settings.local.json: the hooks decide where
    // kontext's entries are, so a plain install keeps using settings.json
    // and adds no hooks to the local file.
    let perm = w.root().join("perm");
    fs::create_dir_all(perm.join(".claude")).unwrap();
    w.git(&perm, &["init", "-q"]);
    let local = "{\"permissions\": {\"allow\": [\"Bash(kontext:*)\"]}}";
    fs::write(perm.join(".claude/settings.local.json"), local).unwrap();
    w.ok(&["--project", "demo", "install"], &perm);
    assert_eq!(settings(&perm, "settings.local.json").unwrap(), local);
    assert!(
        settings(&perm, "settings.json")
            .unwrap()
            .contains("hook session-start")
    );
}

#[test]
fn install_local_without_a_settings_json_does_not_create_one() {
    let w = setup();
    let main = w.main();
    let out = w.ok(&["install", "--local"], &main);
    assert!(out.contains("settings.local.json"), "{out}");
    assert!(settings(&main, "settings.json").is_none());
    // A settings.json with nothing of kontext's stays byte for byte.
    let text = "{\"model\":\"sonnet\"}";
    fs::write(main.join(".claude/settings.json"), text).unwrap();
    let out = w.ok(&["install", "--local"], &main);
    assert!(out.contains("nothing to change"), "{out}");
    assert_eq!(settings(&main, "settings.json").unwrap(), text);
}

#[test]
fn a_tracked_kontext_toml_gives_worktrees_its_worktree_role() {
    let w = setup();
    let main = w.main();
    w.ok(
        &["install", "--link-only", "--worktree-role", "agent"],
        &main,
    );
    w.git(&main, &["add", ".kontext.toml"]);
    w.git(&main, &["commit", "-q", "-m", "kontext"]);
    let lane = w.worktree("lane-1", None);
    // The lane has the file, but it is the repo's, not the lane's own.
    assert!(lane.join(".kontext.toml").exists());
    assert_eq!(w.start_role(&lane, &[]), "agent");
    assert_eq!(w.start_role(&lane.join("."), &[]), "agent");
    assert_eq!(w.start_role(&main, &[]), "pm");
    assert!(project_of(&w, &lane).starts_with("Project demo in"));
    // It cannot be marked: --link-only keeps the file there.
    let out = w.ok(&["install", "--link-only"], &lane);
    assert!(out.contains("nothing to change"), "{out}");
    assert_eq!(w.start_role(&lane, &[]), "agent");
}

#[test]
fn a_full_install_in_an_unmarked_worktree_is_refused() {
    let w = setup();
    let main = w.main();
    let lane = w.worktree("agent-1", None);
    let before = snapshot(&w.root());
    let out = w.run(&["install"], &lane);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains(&format!(
            "installed in the main checkout ({})",
            main.display()
        )) && err.contains("`kontext install --link-only` marks this worktree"),
        "{err}"
    );
    assert_eq!(snapshot(&w.root()), before, "the refusal wrote something");
    // --local is read from the main checkout's root: refused in any worktree.
    let out = w.run(&["install", "--local"], &lane);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("run `kontext install --local` in the main checkout"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // --skills-only and --link-only still work there.
    w.ok(&["install", "--skills-only"], &lane);
    assert!(lane.join(".claude/skills/kontext/SKILL.md").exists());
    w.ok(&["install", "--link-only"], &lane);
    // Marked, it has a .kontext.toml of its own: a full install proceeds.
    w.ok(&["install", "--dry-run"], &lane);
}

#[test]
fn repos_name_the_repo_not_the_worktree() {
    let w = setup();
    let main = w.main();
    // The main checkout without a .kontext.toml: a full install in a
    // worktree proceeds, and [[repos]] gets the main checkout's name.
    fs::remove_file(main.join(".kontext.toml")).unwrap();
    let lane = w.worktree("agent-7", None);
    w.ok(&["--project", "demo", "install"], &lane);
    let ptoml = fs::read_to_string(w.khome().join("project.toml")).unwrap();
    assert!(ptoml.contains("name = \"billing-app\""), "{ptoml}");
    assert!(!ptoml.contains("agent-7"), "{ptoml}");

    // A worktree of a bare repo has no main checkout: [[repos]] stays.
    let bare = w.root().join("ledger.git");
    w.git(
        &w.root(),
        &[
            "clone",
            "-q",
            "--bare",
            main.to_str().unwrap(),
            bare.to_str().unwrap(),
        ],
    );
    let bare_lane = w.root().join("ledger-lane");
    w.git(
        &bare,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "lane",
            bare_lane.to_str().unwrap(),
        ],
    );
    let out = w.ok(&["--project", "demo", "install"], &bare_lane);
    assert!(
        out.contains("a linked worktree of a bare git repo"),
        "{out}"
    );
    let after = fs::read_to_string(w.khome().join("project.toml")).unwrap();
    assert_eq!(after, ptoml);
}

#[test]
fn new_files_git_would_commit_get_an_exclude_note() {
    let w = setup();
    let main = w.main();
    let exclude = main.join(".git/info/exclude");
    fs::create_dir_all(exclude.parent().unwrap()).unwrap();
    let note = |line: &str| {
        format!(
            "add the line `{line}` to {}",
            fs::canonicalize(&exclude).unwrap().display()
        )
    };

    // --link-only in a linked worktree.
    let lane = w.worktree("pm-desk", None);
    let out = w.ok(&["install", "--link-only"], &lane);
    assert!(out.contains(&note("/.kontext.toml")), "{out}");
    let mut text = fs::read_to_string(&exclude).unwrap_or_default();
    text.push_str("/.kontext.toml\n");
    fs::write(&exclude, &text).unwrap();
    // Ignored now (every worktree shares the file): no note.
    let lane2 = w.worktree("pm-desk-2", None);
    let out = w.ok(&["install", "--link-only"], &lane2);
    assert!(!out.contains("info/exclude"), "{out}");

    // --local creating settings.local.json in the main checkout.
    let out = w.ok(&["install", "--local"], &main);
    assert!(out.contains(&note("/.claude/settings.local.json")), "{out}");
    // Not when it already exists.
    let out = w.ok(&["install", "--local"], &main);
    assert!(!out.contains("info/exclude"), "{out}");
    // Nor when git ignores it.
    fs::remove_file(main.join(".claude/settings.local.json")).unwrap();
    text.push_str("/.claude/settings.local.json\n");
    fs::write(&exclude, &text).unwrap();
    let out = w.ok(&["install", "--local"], &main);
    assert!(
        out.contains("Wrote") && !out.contains("info/exclude"),
        "{out}"
    );
}
