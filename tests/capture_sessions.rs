//! Named conversations: with at least one `session_id` named for the project
//! on this machine (`kontext capture here`), the capture hook writes only for
//! those, on top of its role and checkout rules; with none named, it behaves
//! as before. `kontext capture list|forget` and what `kontext doctor` says.
//!
//! Every run sets PATH, HOME, KONTEXT_CONFIG_DIR and CLAUDE_CONFIG_DIR, and
//! sets or removes CLAUDE_CODE_SESSION_ID, so nothing of the real machine is
//! read.

mod common;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};

/// Far from UTC, so a date in the wrong zone shows.
const TZ: &str = "Asia/Tokyo";
/// 2026-11-07 10:42 in Tokyo.
const NOW: &str = "2026-11-07T01:42:00Z";
const DUMP: &str = ".pm/dumps/2026-11-07.md";
/// The PM's conversation and a coordinator's, in the same folder.
const PM: &str = "0b7e5d1c-pm-invoices";
const COORD: &str = "5f2a9c3e-coord-billing";

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
    fn main(&self) -> PathBuf {
        self.root().join("billing-app")
    }
    fn list_file(&self) -> PathBuf {
        self.khome().join(".kontext/capture-sessions.json")
    }
    fn log(&self) -> String {
        fs::read_to_string(self.khome().join(".kontext/hooks.log")).unwrap_or_default()
    }
    fn transcript(&self, session: &str) -> PathBuf {
        self.root()
            .join("transcripts")
            .join(format!("{session}.jsonl"))
    }

    /// `kontext` in `cwd`, as the conversation `session` (`None`: outside
    /// Claude Code, the variable unset).
    fn cmd(&self, args: &[&str], cwd: &Path, session: Option<&str>) -> Command {
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
            .env("KONTEXT_NOW", NOW)
            .env_remove("KONTEXT_PROJECT")
            .env_remove("KONTEXT_ROLE")
            .env_remove("KONTEXT_EPIC")
            .env_remove("KONTEXT_CAPTURE_DISABLE")
            .env_remove("CLAUDE_CODE_SESSION_ID");
        if let Some(s) = session {
            c.env("CLAUDE_CODE_SESSION_ID", s);
        }
        c
    }

    fn run(&self, args: &[&str], cwd: &Path, session: Option<&str>) -> Output {
        self.cmd(args, cwd, session).output().unwrap()
    }

    fn ok(&self, args: &[&str], cwd: &Path, session: Option<&str>) -> String {
        let out = self.run(args, cwd, session);
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

    fn worktree(&self, name: &str) -> PathBuf {
        let p = self.root().join(name);
        self.git(
            &self.main(),
            &["worktree", "add", "-q", "-b", name, p.to_str().unwrap()],
        );
        fs::canonicalize(p).unwrap()
    }

    /// Runs a hook as Claude Code does: the variable is the payload's
    /// session. Capture hooks print nothing and exit with 0.
    fn hook(&self, event: &str, payload: &Value) {
        let session = payload.get("session_id").and_then(Value::as_str);
        let mut child = self
            .cmd(&["hook", event], &self.root(), session)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.to_string().as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert_eq!(out.status.code(), Some(0), "{event}");
        assert!(out.stdout.is_empty(), "{event}: {:?}", out.stdout);
        assert!(out.stderr.is_empty(), "{event}: {:?}", out.stderr);
    }

    fn prompt(&self, cwd: &Path, session: &str, text: &str, id: &str) {
        self.hook(
            "user-prompt-submit",
            &json!({"session_id": session, "cwd": cwd, "prompt": text, "prompt_id": id,
                    "transcript_path": self.transcript(session),
                    "hook_event_name": "UserPromptSubmit"}),
        );
    }

    fn stop(&self, cwd: &Path, session: &str) {
        self.hook(
            "stop",
            &json!({"session_id": session, "cwd": cwd,
                    "transcript_path": self.transcript(session), "hook_event_name": "Stop"}),
        );
    }

    /// A question-tool answer appended to the session's transcript.
    fn answer(&self, session: &str, tool: &str, question: &str, answer: &str) {
        let line = json!({"type": "user", "isSidechain": false, "timestamp": "2026-11-07T01:43:00Z",
            "uuid": format!("r-{tool}"),
            "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": tool, "content": "ok"}]},
            "toolUseResult": {"questions": [{"question": question, "header": "H"}],
                              "answers": {question: answer}}});
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.transcript(session))
            .unwrap();
        writeln!(f, "{line}").unwrap();
    }

    fn set_capture(&self, section: &str) {
        let toml = self.khome().join("project.toml");
        let t = fs::read_to_string(&toml).unwrap();
        let t = match t.find("\n[capture]\n") {
            // Replaces the section written before.
            Some(i) => {
                let end = t[i + 1..].find("\n\n").map(|e| i + 1 + e).unwrap();
                format!("{}\n[capture]\n{section}{}", &t[..i], &t[end..])
            }
            None => t.replacen(
                "\n[claims]",
                &format!("\n[capture]\n{section}\n\n[claims]"),
                1,
            ),
        };
        fs::write(&toml, t).unwrap();
    }
}

fn dump_of(dir: &Path) -> String {
    fs::read_to_string(dir.join(DUMP)).unwrap_or_default()
}

/// A project `demo` and a code repo with one commit, linked by its main
/// checkout's untracked `.kontext.toml` (`default_role = "pm"`). Capture off.
fn setup() -> W {
    let w = W {
        dir: tempfile::tempdir().unwrap(),
    };
    for d in ["h", "claude", "config", "billing-app", "transcripts"] {
        fs::create_dir_all(w.root().join(d)).unwrap();
    }
    fs::write(w.config().join("projects.toml"), "writer = \"omar@test\"\n").unwrap();
    let k = w.khome();
    w.ok(
        &["init", "demo", "--home", k.to_str().unwrap()],
        &w.root(),
        None,
    );
    w.git(&w.main(), &["init", "-q"]);
    w.git(&w.main(), &["commit", "-q", "--allow-empty", "-m", "init"]);
    w.ok(&["install", "--link-only"], &w.main(), None);
    w
}

/// The checkout where the PM's and the coordinator's conversations run, and
/// the main checkout, whose `.pm/` it writes into.
fn checkout(w: &W, marked_worktree: bool) -> PathBuf {
    if marked_worktree {
        let pm = w.worktree("pm-desk");
        w.ok(&["install", "--link-only"], &pm, None);
        pm
    } else {
        w.main()
    }
}

#[test]
fn two_conversations_in_one_checkout_only_the_named_one_is_captured() {
    for marked in [false, true] {
        let w = setup();
        w.set_capture("enabled = true");
        let here = checkout(&w, marked);

        // None named: both are written, as before.
        w.prompt(&here, PM, "add a CSV export of paid invoices", "p-1");
        w.prompt(&here, COORD, "relay: the billing epic is done", "c-1");
        w.answer(PM, "toolu_pm_1", "Keep the old export?", "drop it");
        w.answer(COORD, "toolu_co_1", "Merge the billing PR?", "merge it now");
        w.stop(&here, PM);
        w.stop(&here, COORD);
        let d = dump_of(&w.main());
        for text in [
            "add a CSV export of paid invoices",
            "relay: the billing epic is done",
            "drop it",
            "merge it now",
        ] {
            assert!(d.contains(text), "marked={marked}: {text}: {d}");
        }
        assert!(!w.list_file().exists());

        // The PM names its conversation from a Bash tool in it.
        let out = w.ok(&["capture", "here"], &here, Some(PM));
        assert!(
            out.contains(&format!(
                "Named conversation {PM} (from $CLAUDE_CODE_SESSION_ID)"
            )),
            "{out}"
        );
        assert!(!out.contains("Nothing is captured from it"), "{out}");

        w.prompt(&here, PM, "and a total per page", "p-2");
        w.prompt(&here, COORD, "relay: the paging task is claimed", "c-2");
        w.answer(PM, "toolu_pm_2", "Thirty per page?", "fifty");
        w.answer(COORD, "toolu_co_2", "Close the billing epic?", "close it");
        w.stop(&here, PM);
        w.stop(&here, COORD);
        w.hook(
            "session-end",
            &json!({"session_id": COORD, "cwd": here,
                    "transcript_path": w.transcript(COORD), "hook_event_name": "SessionEnd"}),
        );
        let d = dump_of(&w.main());
        assert!(d.contains("and a total per page"), "{d}");
        assert!(d.contains("fifty"), "{d}");
        assert!(!d.contains("paging task is claimed"), "{d}");
        assert!(!d.contains("close it"), "{d}");
        if marked {
            assert!(!here.join(".pm").exists());
        }
        let log = w.log();
        assert_eq!(
            log.matches(
                "capture: this conversation is not one of the 1 named on this machine, nothing \
                 written (`kontext capture here` in it names it)"
            )
            .count(),
            3,
            "the prompt, the stop and the session end: {log}"
        );
        assert!(log.contains(&format!("stop session={COORD}")), "{log}");
        // Neither prompt nor answer text (the paths and ids name the theme).
        for text in ["relay", "paging", "close it", "CSV", "fifty", "per page"] {
            assert!(!log.contains(text), "{text} in {log}");
        }
    }
}

#[test]
fn the_pm_named_in_its_worktree_and_a_pm_session_in_the_main_checkout() {
    let w = setup();
    w.set_capture("enabled = true");
    let pm = checkout(&w, true);
    w.ok(&["capture", "here"], &pm, Some(PM));
    // A second coordinator in the main checkout, also as pm, not named.
    w.prompt(&w.main(), COORD, "report: the export task closed", "c-1");
    w.prompt(&pm, PM, "drop the old export", "p-1");
    assert_eq!(
        dump_of(&w.main()),
        "## 2026-11-07 10:42\n\n**User:**\n\ndrop the old export\n"
    );
    assert!(!pm.join(".pm").exists());
}

#[test]
fn a_named_conversation_in_an_unmarked_worktree_writes_nothing() {
    let w = setup();
    w.set_capture("enabled = true");
    let agent = w.worktree("agent-1");
    let out = w.ok(&["capture", "here"], &agent, Some(PM));
    assert!(
        out.contains(
            "Nothing is captured from it here yet: this is a linked git worktree not marked as \
             the PM's (if the PM works here, run `kontext install --link-only` in it)."
        ),
        "{out}"
    );
    w.prompt(&agent, PM, "a brief for the invoices task", "p-1");
    assert!(!w.main().join(".pm").exists());
    assert!(!agent.join(".pm").exists());
    let log = w.log();
    let line = log.lines().last().unwrap();
    assert!(
        line.contains("a linked git worktree not marked as the PM's, nothing written"),
        "{line}"
    );
    assert!(!line.contains("named"), "{line}");

    // Another role: named, still not captured, and `here` says why.
    fs::write(
        w.main().join(".kontext.toml"),
        "project = \"demo\"\ndefault_role = \"coordinator\"\n",
    )
    .unwrap();
    let out = w.ok(&["capture", "here"], &w.main(), Some(COORD));
    assert!(
        out.contains("Nothing is captured from it here yet: sessions here have role coordinator"),
        "{out}"
    );
    w.prompt(&w.main(), COORD, "an epic note", "c-1");
    assert!(w.log().contains("role \"coordinator\" is not pm"));
    assert!(!w.main().join(".pm").exists());
}

#[test]
fn without_a_session_id_nothing_is_written_once_conversations_are_named() {
    let w = setup();
    w.set_capture("enabled = true");
    let payload = json!({"cwd": w.main(), "prompt": "the invoices need paging", "prompt_id": "p-1",
                         "hook_event_name": "UserPromptSubmit"});
    // None named: written, as before.
    w.hook("user-prompt-submit", &payload);
    assert!(dump_of(&w.main()).contains("the invoices need paging"));

    w.ok(&["capture", "here", "--session", PM], &w.main(), None);
    let before = dump_of(&w.main());
    let mut p2 = payload.clone();
    p2["prompt"] = json!("and a total per page");
    p2["prompt_id"] = json!("p-2");
    w.hook("user-prompt-submit", &p2);
    p2["session_id"] = json!("  ");
    w.hook("user-prompt-submit", &p2);
    assert_eq!(dump_of(&w.main()), before);
    let log = w.log();
    assert_eq!(
        log.matches(
            "capture: 1 named on this machine and no session_id in the payload, nothing written"
        )
        .count(),
        2,
        "{log}"
    );
    assert!(!log.contains("total per page"), "{log}");
}

/// Path and content of every file under `dir`.
fn files(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(files(&p));
        } else {
            out.push((p.clone(), fs::read(&p).unwrap()));
        }
    }
    out.sort();
    out
}

#[test]
fn here_list_and_forget() {
    let w = setup();
    let main = w.main();
    // Outside Claude Code, without --session: refused, and it says why.
    let out = w.run(&["capture", "here"], &main, None);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("CLAUDE_CODE_SESSION_ID is not set") && err.contains("--session <id>"),
        "{err}"
    );
    assert!(!w.list_file().exists());

    // With the capture off: named, and it says the capture is off.
    let toml_before = fs::read(w.khome().join("project.toml")).unwrap();
    let link_before = fs::read(main.join(".kontext.toml")).unwrap();
    let out = w.ok(&["capture", "here"], &main, Some(PM));
    let khome = w.khome();
    for needed in [
        format!("Named conversation {PM} (from $CLAUDE_CODE_SESSION_ID) for the capture."),
        format!(
            "Project demo, kontext-home {}; the list is {} (this machine only).",
            khome.display(),
            w.list_file().display()
        ),
        "Named (1): only these conversations are captured.".into(),
        format!(
            "  {PM}  named 2026-11-07T01:42:00Z in {}  (this conversation)",
            main.display()
        ),
        "A conversation's id changes on /clear: run `kontext capture here` in it again.".into(),
        "Capture is off ([capture] enabled is not true in project.toml)".into(),
    ] {
        assert!(out.contains(&needed), "{needed:?}: {out}");
    }
    // Machine-local: neither project.toml nor .kontext.toml changes, and git
    // does not see the list.
    assert_eq!(fs::read(khome.join("project.toml")).unwrap(), toml_before);
    assert_eq!(fs::read(main.join(".kontext.toml")).unwrap(), link_before);
    let status = Command::new("git")
        .args(["status", "--porcelain", "--ignored=no"])
        .current_dir(&khome)
        .env("HOME", w.root().join("h"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        !String::from_utf8_lossy(&status.stdout).contains("capture-sessions"),
        "{status:?}"
    );
    let v: Value = serde_json::from_str(&fs::read_to_string(w.list_file()).unwrap()).unwrap();
    assert_eq!(v["sessions"][0]["id"], PM);

    let out = w.ok(&["capture", "here"], &main, Some(PM));
    assert!(
        out.contains(&format!(
            "Conversation {PM} (from $CLAUDE_CODE_SESSION_ID) was already named."
        )),
        "{out}"
    );
    // --session wins over the variable.
    let out = w.ok(&["capture", "here", "--session", COORD], &main, Some(PM));
    assert!(
        out.contains(&format!("Named conversation {COORD} (from --session)")),
        "{out}"
    );

    let v: Value =
        serde_json::from_str(&w.ok(&["--json", "capture", "list"], &main, None)).unwrap();
    let ids: Vec<&str> = v["named"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, [PM, COORD]);
    assert_eq!(v["project"], "demo");
    assert_eq!(
        fs::canonicalize(v["home"].as_str().unwrap()).unwrap(),
        fs::canonicalize(&khome).unwrap()
    );
    assert_eq!(v["capture_enabled"], false);
    assert_eq!(v["current"], Value::Null);

    // Forgetting one that is not named is refused.
    let out = w.run(&["capture", "forget", "s-unknown"], &main, None);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("s-unknown is not a named conversation"));

    // With the capture on, the coordinator alone is named: the PM's prompt is skipped.
    w.set_capture("enabled = true");
    let out = w.ok(&["capture", "forget", PM], &main, None);
    assert!(out.contains(&format!("Forgot {PM}.")), "{out}");
    assert!(out.contains("Named (1)"), "{out}");
    w.prompt(&main, PM, "the invoices export first", "p-1");
    assert!(!main.join(".pm").exists());

    // Forgetting the last one brings back today's behaviour.
    let out = w.ok(&["capture", "forget", COORD], &main, None);
    assert!(
        out.contains(
            "No conversation is named: every pm conversation in a capturing checkout is captured."
        ),
        "{out}"
    );
    assert!(!w.list_file().exists());
    w.prompt(&main, PM, "the invoices export first", "p-2");
    assert!(dump_of(&main).contains("the invoices export first"));

    // --all.
    w.ok(&["capture", "here", "--session", "s-a"], &main, None);
    w.ok(&["capture", "here", "--session", "s-b"], &main, None);
    let out = w.ok(&["capture", "forget", "--all"], &main, None);
    assert!(out.contains("Forgot 2 named conversations."), "{out}");
    let out = w.ok(&["capture", "forget", "--all"], &main, None);
    assert!(
        out.contains("No conversation was named; nothing to forget."),
        "{out}"
    );
    assert!(!w.run(&["capture", "forget"], &main, None).status.success());
}

#[test]
fn an_unreadable_list_writes_nothing_until_it_is_cleared() {
    let w = setup();
    w.set_capture("enabled = true");
    fs::create_dir_all(w.khome().join(".kontext")).unwrap();
    fs::write(w.list_file(), "not json").unwrap();
    w.prompt(&w.main(), PM, "the invoices need paging", "p-1");
    assert!(!w.main().join(".pm").exists());
    let log = w.log();
    assert!(
        log.contains("user-prompt-submit: error: the list of named conversations cannot be read"),
        "{log}"
    );
    assert!(!log.contains("paging"), "{log}");
    let checks = doctor(&w, &w.main(), None);
    assert!(
        checks.iter().any(|c| c.0 == "problem"
            && c.1.contains("cannot be read")
            && c.2.contains("kontext capture forget --all")),
        "{checks:?}"
    );
    w.ok(&["capture", "forget", "--all"], &w.main(), None);
    w.prompt(&w.main(), PM, "the invoices need paging", "p-2");
    assert!(dump_of(&w.main()).contains("the invoices need paging"));
}

fn age(p: &Path, days: u64) {
    let t = SystemTime::now() - Duration::from_secs(days * 24 * 3600);
    fs::File::options()
        .write(true)
        .open(p)
        .unwrap()
        .set_modified(t)
        .unwrap();
}

#[test]
fn the_list_is_not_a_mark_and_survives_their_expiry() {
    let w = setup();
    w.set_capture("enabled = true");
    w.ok(&["capture", "here", "--session", PM], &w.main(), None);
    w.prompt(&w.main(), PM, "the invoices export", "p-1");
    let marks = w.khome().join(".kontext/capture");
    let old: Vec<PathBuf> = fs::read_dir(&marks)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(old.len(), 1, "one mark, for the one transcript: {old:?}");
    // Both untouched for longer than the marks live.
    age(&old[0], 40);
    age(&w.list_file(), 40);
    let list = fs::read(w.list_file()).unwrap();
    // Another transcript of the same conversation: its mark is saved, and the
    // old ones pruned.
    w.hook(
        "user-prompt-submit",
        &json!({"session_id": PM, "cwd": w.main(), "prompt": "and the paging", "prompt_id": "p-2",
                "transcript_path": w.transcript("other")}),
    );
    assert!(!old[0].exists(), "the old mark is pruned");
    assert_eq!(fs::read(w.list_file()).unwrap(), list, "the list stays");
    let names: Vec<String> = fs::read_dir(&marks)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names.len(), 1, "{names:?}");
    assert!(!names.iter().any(|n| n.contains("sessions")), "{names:?}");
    let d = dump_of(&w.main());
    assert!(
        d.contains("the invoices export") && d.contains("and the paging"),
        "{d}"
    );
}

/// `kontext doctor --json` in `cwd`: the Capture checks as (level, message, fix).
fn doctor(w: &W, cwd: &Path, session: Option<&str>) -> Vec<(String, String, String)> {
    let out = w.run(&["doctor", "--json"], cwd, session);
    let v: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "{e}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    });
    v["checks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["section"] == "capture")
        .map(|c| {
            (
                c["level"].as_str().unwrap().to_string(),
                c["message"].as_str().unwrap().to_string(),
                c["fix"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect()
}

fn find<'a>(
    checks: &'a [(String, String, String)],
    needle: &str,
) -> Option<&'a (String, String, String)> {
    checks.iter().find(|c| c.1.contains(needle))
}

#[test]
fn doctor_says_which_conversations_are_named() {
    let w = setup();
    let main = w.main();
    let none = "no conversation is named on this machine: every pm conversation in a capturing \
                checkout is captured";
    // Capture off, none named.
    let c = doctor(&w, &main, Some(PM));
    assert_eq!(find(&c, none).unwrap().0, "ok", "{c:?}");
    assert_eq!(
        find(&c, "this conversation").unwrap().1,
        format!("this conversation ({PM}) is not named; none is")
    );

    w.ok(&["capture", "here", "--session", PM], &main, None);
    // Capture off: plain ok lines, no note.
    let c = doctor(&w, &main, Some(COORD));
    let n = find(&c, "1 conversation named on this machine").unwrap();
    assert_eq!(n.0, "ok");
    assert!(n.1.contains(".kontext/capture-sessions.json"), "{n:?}");
    assert_eq!(
        find(&c, "this conversation").unwrap(),
        &(
            "ok".into(),
            format!("this conversation ({COORD}) is not named"),
            String::new()
        )
    );
    assert!(
        c.iter().all(|x| x.0 != "note" || !x.1.contains("named")),
        "{c:?}"
    );

    // Capture on, this conversation named.
    w.set_capture("enabled = true");
    let snapshot = files(&w.khome().join(".kontext"));
    let c = doctor(&w, &main, Some(PM));
    assert_eq!(
        find(&c, "this conversation").unwrap(),
        &(
            "ok".into(),
            format!("this conversation ({PM}, from $CLAUDE_CODE_SESSION_ID) is named"),
            String::new()
        )
    );
    // Not named, in a checkout that captures, as pm: the note.
    let c = doctor(&w, &main, Some(COORD));
    let note = find(&c, "this conversation").unwrap();
    assert_eq!(note.0, "note", "{c:?}");
    assert_eq!(
        note.1,
        format!(
            "this conversation ({COORD}, from $CLAUDE_CODE_SESSION_ID) is not named: nothing \
             typed in it is captured"
        )
    );
    assert_eq!(
        note.2,
        "if this is the conversation where the human talks to the PM, `kontext capture here`; \
         one that gets its messages from other agents stays unnamed"
    );
    // Outside Claude Code: no line about this conversation.
    let c = doctor(&w, &main, None);
    assert!(find(&c, "this conversation").is_none(), "{c:?}");
    assert!(find(&c, "1 conversation named").is_some(), "{c:?}");
    // Not a capturing checkout (an unmarked worktree): no note.
    let agent = w.worktree("agent-1");
    let c = doctor(&w, &agent, Some(COORD));
    assert_eq!(find(&c, "this conversation").unwrap().0, "ok", "{c:?}");
    // Doctor wrote nothing.
    assert_eq!(files(&w.khome().join(".kontext")), snapshot);
}
