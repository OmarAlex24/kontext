//! The capture hook: `kontext hook user-prompt-submit|stop|session-end` write
//! the user's messages into `.pm/dumps/` of the code repo, in the
//! `kontext-pm` skill's format, and never print anything.

mod common;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use common::Project;
use serde_json::{Value, json};

/// Far from UTC, so a date in the wrong zone shows.
const TZ: &str = "Asia/Tokyo";
/// 2026-11-07 10:42 in Tokyo.
const NOW: &str = "2026-11-07T01:42:00Z";

struct Cap {
    p: Project,
    repo: PathBuf,
    transcript: PathBuf,
}

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap()
            .status
            .success()
    );
}

fn set_capture(p: &Project, section: &str) {
    let toml = p.home.join("project.toml");
    let mut t = fs::read_to_string(&toml).unwrap();
    t.push_str(&format!("\n[capture]\n{section}\n"));
    fs::write(&toml, t).unwrap();
}

/// A project with `[capture] enabled = true` and a git repo `billing-app`.
fn cap() -> Cap {
    cap_with("enabled = true")
}

fn cap_with(section: &str) -> Cap {
    let p = Project::new();
    set_capture(&p, section);
    let repo = p.dir.path().join("billing-app");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    fs::write(
        repo.join(".kontext.toml"),
        "project = \"demo\"\ndefault_role = \"pm\"\n",
    )
    .unwrap();
    let transcript = p.dir.path().join("transcripts").join("s-1.jsonl");
    fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    Cap {
        p,
        repo,
        transcript,
    }
}

/// Runs the hook; it must print nothing and exit with 0, whatever happens.
fn run(p: &Project, event: &str, envs: &[(&str, &str)], stdin: &str) {
    let mut c = p.cmd(&["hook", event]);
    c.env("TZ", TZ)
        .env("KONTEXT_DISTILL_DISABLE", "1")
        .env_remove("KONTEXT_ROLE")
        .env_remove("KONTEXT_CAPTURE_DISABLE")
        .env_remove("KONTEXT_NOW");
    for (k, v) in envs {
        c.env(k, v);
    }
    let mut child = c
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{event}: the hook exits with 0");
    assert!(
        out.stdout.is_empty(),
        "{event}: stdout {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        out.stderr.is_empty(),
        "{event}: stderr {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
}

impl Cap {
    fn payload(&self, event: &str, prompt: Option<(&str, &str)>) -> String {
        let mut v = json!({
            "session_id": "s-1",
            "transcript_path": self.transcript,
            "cwd": self.repo,
            "permission_mode": "default",
            "hook_event_name": event,
        });
        if let Some((text, id)) = prompt {
            v["prompt"] = json!(text);
            v["prompt_id"] = json!(id);
        }
        v.to_string()
    }

    fn prompt_at(&self, now: &str, text: &str, id: &str) {
        run(
            &self.p,
            "user-prompt-submit",
            &[("KONTEXT_NOW", now)],
            &self.payload("UserPromptSubmit", Some((text, id))),
        );
    }

    fn prompt(&self, text: &str, id: &str) {
        self.prompt_at(NOW, text, id);
    }

    fn trigger(&self, event: &str) {
        let name = if event == "stop" {
            "Stop"
        } else {
            "SessionEnd"
        };
        run(
            &self.p,
            event,
            &[("KONTEXT_NOW", NOW)],
            &self.payload(name, None),
        );
    }

    fn dumps(&self) -> PathBuf {
        self.repo.join(".pm/dumps")
    }

    fn dump(&self, name: &str) -> String {
        fs::read_to_string(self.dumps().join(name)).unwrap_or_default()
    }

    fn log(&self) -> String {
        fs::read_to_string(self.p.home.join(".kontext/hooks.log")).unwrap_or_default()
    }

    fn add(&self, lines: &[Value]) {
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.transcript)
            .unwrap();
        for l in lines {
            writeln!(f, "{l}").unwrap();
        }
    }
}

fn user_line(text: &str, ts: &str) -> Value {
    json!({"type": "user", "isSidechain": false, "promptSource": "sdk", "timestamp": ts, "uuid": format!("u-{ts}"),
           "message": {"role": "user", "content": text}})
}

fn assistant_line(text: &str, id: &str, ts: &str) -> Value {
    json!({"type": "assistant", "isSidechain": false, "timestamp": ts, "uuid": format!("a-{id}-{ts}"),
           "message": {"id": id, "role": "assistant", "model": "claude-test", "content": [{"type": "text", "text": text}]}})
}

fn tool_use_line(id: &str, tool: &str, ts: &str) -> Value {
    json!({"type": "assistant", "isSidechain": false, "timestamp": ts,
           "message": {"id": format!("m-{tool}"), "role": "assistant", "model": "claude-test",
                       "content": [{"type": "tool_use", "id": tool, "name": "AskUserQuestion", "input": {}}]},
           "uuid": format!("tu-{id}")})
}

fn answer_line(tool: &str, ts: &str, qa: &[(&str, &str)]) -> Value {
    let questions: Vec<Value> = qa
        .iter()
        .map(|(q, _)| json!({"question": q, "header": "H", "options": [{"label": "a"}], "multiSelect": false}))
        .collect();
    let answers: serde_json::Map<String, Value> =
        qa.iter().map(|(q, a)| (q.to_string(), json!(a))).collect();
    json!({"type": "user", "isSidechain": false, "timestamp": ts, "uuid": format!("r-{tool}"),
           "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": tool, "content": "answered"}]},
           "toolUseResult": {"questions": questions, "answers": answers}})
}

#[test]
fn a_typed_prompt_lands_in_the_dump_with_the_question_it_answers() {
    let c = cap();
    // First prompt of the session: the transcript does not exist yet.
    c.prompt("add a CSV export of invoices", "p-1");
    assert_eq!(
        c.dump("2026-11-07.md"),
        "## 2026-11-07 10:42\n\n**User:**\n\nadd a CSV export of invoices\n"
    );
    assert_eq!(
        fs::read_to_string(c.repo.join(".pm/.gitignore")).unwrap(),
        "dumps/\n"
    );

    // The coordinator answers with questions; a subagent and Claude Code's
    // own records also have text with `?`, and do not count.
    c.add(&[
        user_line("add a CSV export of invoices", "2026-11-07T01:42:00Z"),
        assistant_line(
            "The export is ready.\n\n1. Do we keep the old export or drop it?\n2. Who merges the PRs, you or me?\n\nTell me.",
            "m-1",
            "2026-11-07T01:43:00Z",
        ),
        json!({"type": "assistant", "isSidechain": true, "timestamp": "2026-11-07T01:43:10Z",
               "message": {"id": "m-side", "role": "assistant", "content": [{"type": "text", "text": "Subagent: which file?"}]}}),
        json!({"type": "api-request-blob", "message": {"content": [{"type": "text", "text": "Blob: is this a question?"}]}}),
        json!({"type": "system", "subtype": "stop_hook_summary", "content": "Anything?"}),
    ]);
    c.prompt_at(
        "2026-11-07T01:45:00Z",
        "1. drop it\n2. merge them yourself",
        "p-2",
    );
    assert_eq!(
        c.dump("2026-11-07.md"),
        "## 2026-11-07 10:42\n\n**User:**\n\nadd a CSV export of invoices\n\n\
         ## 2026-11-07 10:45\n\n**Coordinator asked:**\n\n\
         > 1. Do we keep the old export or drop it?\n> 2. Who merges the PRs, you or me?\n\n\
         **User:**\n\n1. drop it\n2. merge them yourself\n"
    );

    // The last assistant text has no question: no block.
    c.add(&[
        user_line("1. drop it\n2. merge them yourself", "2026-11-07T01:45:00Z"),
        assistant_line("Done, both merged.", "m-2", "2026-11-07T01:46:00Z"),
    ]);
    c.prompt_at("2026-11-07T01:47:00Z", "thanks, next the PDF", "p-3");
    let d = c.dump("2026-11-07.md");
    assert!(
        d.ends_with("\n\n## 2026-11-07 10:47\n\n**User:**\n\nthanks, next the PDF\n"),
        "{d}"
    );
    // The same prompt_id again (a retried hook) is not written twice.
    c.prompt_at("2026-11-07T01:47:00Z", "thanks, next the PDF", "p-3");
    assert_eq!(c.dump("2026-11-07.md"), d);

    let log = c.log();
    assert!(log.contains("user-prompt-submit session=s-1"), "{log}");
    assert!(log.contains("capture: wrote 1 entry"), "{log}");
    assert!(log.contains(".pm/dumps/2026-11-07.md"), "{log}");
    assert!(log.contains("the prompt was already written"), "{log}");
    for secret_word in ["CSV", "invoices", "merge", "PDF", "keep the old", "drop it"] {
        assert!(!log.contains(secret_word), "{secret_word} in {log}");
    }
}

#[test]
fn machine_prompts_are_skipped_by_rule() {
    let c = cap_with("enabled = true\nskip_prefixes = [\"<invoice-bot>\"]");
    let prompts = [
        "<task-notification>\n<task-id>a1</task-id> finished the invoices</task-notification>",
        "[SYSTEM NOTIFICATION - the invoices job ended]",
        "<system-reminder>the invoices</system-reminder>",
        "<cross-session-message from=\"x\">invoices</cross-session-message>",
        "<agent-message>invoices</agent-message>",
        "<local-command-stdout>invoices</local-command-stdout>",
        "<command-name>/model</command-name>",
        "<command-message>invoices</command-message>",
        "<bash-input>ls invoices</bash-input>",
        "<bash-stdout>invoices.csv</bash-stdout>",
        "Caveat: The messages below were generated by the user while running local commands. invoices",
        "   \n  <task-notification>indented invoices</task-notification>",
        "/clear",
        "/kontext:audit",
        "",
        "   \n ",
        "<invoice-bot> 3 invoices synced",
    ];
    for (i, text) in prompts.iter().enumerate() {
        c.prompt(text, &format!("p-{i}"));
    }
    assert!(!c.dumps().exists(), "nothing written");
    assert!(!c.repo.join(".pm").exists(), "no .pm/ either");
    let log = c.log();
    for rule in [
        "starts with \"<task-notification>\"",
        "starts with \"[SYSTEM NOTIFICATION\"",
        "starts with \"Caveat: The messages below",
        "a slash command only",
        "skipped the prompt (empty)",
        "starts with \"<invoice-bot>\" from [capture] skip_prefixes",
    ] {
        assert!(log.contains(rule), "{rule}: {log}");
    }
    assert_eq!(log.matches("skipped the prompt").count(), prompts.len());
    assert!(!log.contains("invoices"), "{log}");
    assert!(!log.contains("clear"), "{log}");

    // When in doubt it is written: a slash command with words, a tag in the middle.
    c.prompt("/review the invoices export", "q-1");
    c.prompt("look at <task-notification> in the logs", "q-2");
    let d = c.dump("2026-11-07.md");
    assert!(d.contains("/review the invoices export"), "{d}");
    assert!(d.contains("look at <task-notification> in the logs"), "{d}");
}

#[test]
fn secrets_are_replaced_with_the_skill_marker() {
    let c = cap();
    c.add(&[assistant_line(
        "Which key goes in, sk-abcdefghijklmnopqrstuvwx?",
        "m-1",
        "2026-11-07T01:40:00Z",
    )]);
    c.prompt(
        "use password=hunter2 and ghp_abcdefghijklmnopqrstuvwxyz0123 for the invoices",
        "p-1",
    );
    let d = c.dump("2026-11-07.md");
    assert!(
        !d.contains("hunter2") && !d.contains("ghp_") && !d.contains("sk-abc"),
        "{d}"
    );
    assert!(
        d.contains("use password=[secret removed] and [secret removed] for the invoices"),
        "{d}"
    );
    assert!(d.contains("> Which key goes in, [secret removed]?"), "{d}");
    assert!(!d.contains("REDACTED"), "{d}");
}

#[test]
fn nothing_is_written_when_off_for_another_role_or_outside_git() {
    // Off by default: `kontext init` leaves no [capture].
    let p = Project::new();
    let t = fs::read_to_string(p.home.join("project.toml")).unwrap();
    assert!(!t.contains("\n[capture]"), "{t}");
    let repo = p.dir.path().join("billing-app");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    let payload = json!({"session_id": "s-0", "cwd": repo, "prompt": "add the invoices export", "prompt_id": "p"}).to_string();
    run(&p, "user-prompt-submit", &[], &payload);
    assert!(!repo.join(".pm").exists());
    let log = fs::read_to_string(p.home.join(".kontext/hooks.log")).unwrap();
    assert!(
        log.contains("capture off ([capture] enabled is not true)"),
        "{log}"
    );
    // `enabled = false` is the same.
    set_capture(&p, "enabled = false");
    run(&p, "user-prompt-submit", &[], &payload);
    assert!(!repo.join(".pm").exists());

    let c = cap();
    // The env switch.
    run(
        &c.p,
        "user-prompt-submit",
        &[("KONTEXT_CAPTURE_DISABLE", "1")],
        &c.payload("UserPromptSubmit", Some(("add the invoices export", "p-1"))),
    );
    assert!(
        c.log()
            .contains("capture off because of KONTEXT_CAPTURE_DISABLE")
    );
    // Another role, from the environment or from .kontext.toml.
    run(
        &c.p,
        "user-prompt-submit",
        &[("KONTEXT_ROLE", "coordinator")],
        &c.payload("UserPromptSubmit", Some(("add the invoices export", "p-2"))),
    );
    fs::write(
        c.repo.join(".kontext.toml"),
        "project = \"demo\"\ndefault_role = \"implementer\"\n",
    )
    .unwrap();
    c.prompt("add the invoices export", "p-3");
    let log = c.log();
    assert!(log.contains("role \"coordinator\" is not pm"), "{log}");
    assert!(log.contains("role \"implementer\" is not pm"), "{log}");
    assert!(!c.repo.join(".pm").exists());

    // Outside a git repo.
    let loose = c.p.dir.path().join("loose");
    fs::create_dir_all(&loose).unwrap();
    let payload =
        json!({"session_id": "s-2", "cwd": loose, "prompt": "add the invoices export"}).to_string();
    run(
        &c.p,
        "user-prompt-submit",
        &[("KONTEXT_ROLE", "pm")],
        &payload,
    );
    assert!(!loose.join(".pm").exists());
    let log = c.log();
    assert!(log.contains("not in a git repo, nothing written"), "{log}");
    assert!(!log.contains("invoices"), "{log}");
}

#[test]
fn a_full_dump_goes_on_in_the_next_file() {
    let c = cap();
    fs::create_dir_all(c.dumps()).unwrap();
    let filler = format!(
        "## 2026-11-07 08:00\n\n**User:**\n\n{}\n",
        "x".repeat(59_950)
    );
    fs::write(c.dumps().join("2026-11-07.md"), &filler).unwrap();
    c.prompt("the invoices list needs paging", "p-1");
    assert_eq!(
        c.dump("2026-11-07.md"),
        filler,
        "the full one is left alone"
    );
    assert_eq!(
        c.dump("2026-11-07-2.md"),
        "## 2026-11-07 10:42\n\n**User:**\n\nthe invoices list needs paging\n"
    );
    c.prompt("and a total per page", "p-2");
    assert!(
        c.dump("2026-11-07-2.md")
            .ends_with("\n\n## 2026-11-07 10:42\n\n**User:**\n\nand a total per page\n")
    );
    assert!(!c.dumps().join("2026-11-07-3.md").exists());
    // `dumps/` already existed: no .gitignore is added.
    assert!(!c.repo.join(".pm/.gitignore").exists());
}

#[test]
fn question_tool_answers_are_written_once() {
    let c = cap();
    // First prompt: no transcript yet, so the mark starts at 0.
    c.prompt("plan the invoices export", "p-1");
    c.add(&[
        user_line("plan the invoices export", "2026-11-07T01:42:00Z"),
        tool_use_line("1", "toolu_1", "2026-11-07T01:43:00Z"),
        answer_line(
            "toolu_1",
            "2026-11-07T01:44:00Z",
            &[("Do we keep the old export or drop it?", "drop it")],
        ),
        assistant_line("Noted.", "m-2", "2026-11-07T01:44:30Z"),
    ]);
    c.trigger("stop");
    let one = "\n\n## 2026-11-07 10:44\n\n**Coordinator asked:**\n\n\
               > Do we keep the old export or drop it?\n\n**User:**\n\ndrop it\n";
    let d = c.dump("2026-11-07.md");
    assert!(d.ends_with(one), "{d}");
    // Again, and from another trigger: nothing new.
    c.trigger("stop");
    c.trigger("session-end");
    assert_eq!(c.dump("2026-11-07.md"), d);

    // Several questions: numbered, in question order. This one crosses
    // Tokyo's midnight: it goes in the next day's dump, dated by its own time.
    c.add(&[
        tool_use_line("2", "toolu_2", "2026-11-07T15:29:00Z"),
        answer_line(
            "toolu_2",
            "2026-11-07T15:30:00Z",
            &[
                ("Do we keep the old export or drop it?", "drop it"),
                ("Who merges the PRs, you or me?", "merge them yourself"),
            ],
        ),
    ]);
    c.trigger("session-end");
    assert_eq!(
        c.dump("2026-11-08.md"),
        "## 2026-11-08 00:30\n\n**Coordinator asked:**\n\n\
         > 1. Do we keep the old export or drop it?\n> 2. Who merges the PRs, you or me?\n\n\
         **User:**\n\n1. drop it\n2. merge them yourself\n"
    );
    // The next prompt brings only itself.
    c.prompt_at("2026-11-07T15:31:00Z", "good, go on", "p-2");
    c.trigger("stop");
    let d8 = c.dump("2026-11-08.md");
    assert_eq!(d8.matches("## ").count(), 2, "{d8}");
    assert_eq!(c.dump("2026-11-07.md"), d);
    let log = c.log();
    assert!(log.contains("stop session=s-1"), "{log}");
    assert!(log.contains("1 entry (1 from the question tool)"), "{log}");
    assert!(log.contains("session-end session=s-1"), "{log}");
    assert!(
        log.contains("distill off because of KONTEXT_DISTILL_DISABLE; capture:"),
        "{log}"
    );
    assert!(!log.contains("drop it") && !log.contains("merges"), "{log}");
}

#[test]
fn no_backfill_on_first_sight_and_a_shrunk_transcript_starts_at_its_end() {
    let c = cap();
    // Capture is turned on in the middle of a session: what is already in the
    // transcript is not written.
    c.add(&[
        tool_use_line("1", "toolu_old", "2026-11-07T01:00:00Z"),
        answer_line(
            "toolu_old",
            "2026-11-07T01:01:00Z",
            &[("Old question?", "old answer")],
        ),
    ]);
    c.trigger("stop");
    assert!(!c.dumps().exists());
    c.add(&[answer_line(
        "toolu_new",
        "2026-11-07T01:50:00Z",
        &[("New question?", "new answer")],
    )]);
    c.trigger("stop");
    let d = c.dump("2026-11-07.md");
    assert!(d.contains("new answer") && !d.contains("old answer"), "{d}");

    // The file shrank (rewritten): it starts again from its new end.
    fs::write(&c.transcript, "").unwrap();
    c.add(&[answer_line(
        "toolu_short",
        "2026-11-07T01:55:00Z",
        &[("Short question?", "short answer")],
    )]);
    c.trigger("stop");
    assert_eq!(c.dump("2026-11-07.md"), d);
    c.add(&[answer_line(
        "toolu_after",
        "2026-11-07T01:56:00Z",
        &[("After question?", "after answer")],
    )]);
    c.trigger("stop");
    let d2 = c.dump("2026-11-07.md");
    assert!(
        d2.contains("after answer") && !d2.contains("short answer"),
        "{d2}"
    );

    // A transcript first seen missing that then appears with older history
    // (a continued session): only what came after the first sight.
    let c = cap();
    c.prompt("continue the invoices work", "p-1");
    c.add(&[
        answer_line(
            "toolu_before",
            "2026-11-06T09:00:00Z",
            &[("Before?", "said yesterday")],
        ),
        answer_line(
            "toolu_later",
            "2026-11-07T01:43:00Z",
            &[("Later?", "said now")],
        ),
    ]);
    c.trigger("stop");
    let d = c.dump("2026-11-07.md");
    assert!(
        d.contains("said now") && !d.contains("said yesterday"),
        "{d}"
    );
    assert!(!c.dumps().join("2026-11-06.md").exists());
}

#[test]
fn the_pm_gitignore_is_written_once_and_never_touched() {
    let c = cap();
    c.prompt("first idea for the invoices", "p-1");
    assert_eq!(
        fs::read_to_string(c.repo.join(".pm/.gitignore")).unwrap(),
        "dumps/\n"
    );
    // The user deletes it on purpose: it does not come back.
    fs::remove_file(c.repo.join(".pm/.gitignore")).unwrap();
    c.prompt("second idea", "p-2");
    assert!(!c.repo.join(".pm/.gitignore").exists());
    // The repo's root .gitignore is never touched.
    assert!(!c.repo.join(".gitignore").exists());

    // An existing .pm/.gitignore is left alone.
    let c = cap();
    fs::create_dir_all(c.repo.join(".pm")).unwrap();
    fs::write(c.repo.join(".pm/.gitignore"), "shots/\n").unwrap();
    c.prompt("an idea", "p-1");
    assert_eq!(
        fs::read_to_string(c.repo.join(".pm/.gitignore")).unwrap(),
        "shots/\n"
    );
    assert!(c.dump("2026-11-07.md").contains("an idea"));
}

#[test]
fn it_never_prints_and_always_exits_with_zero() {
    let c = cap();
    for event in ["user-prompt-submit", "stop", "session-end"] {
        for stdin in [
            "",
            "this is not json",
            "[1, 2, 3]",
            "{\"prompt\": 42, \"transcript_path\": 7, \"cwd\": false}",
            "{\"prompt\": \"hi\", \"transcript_path\": \"/nonexistent/x.jsonl\"}",
        ] {
            run(&c.p, event, &[], stdin);
        }
    }
    // An unreadable transcript: a folder instead of a file.
    let dir_as_file = c.p.dir.path().join("transcripts").join("dir.jsonl");
    fs::create_dir_all(&dir_as_file).unwrap();
    let payload = json!({"session_id": "s-9", "cwd": c.repo, "transcript_path": dir_as_file,
                         "prompt": "still captured", "prompt_id": "p-9"})
    .to_string();
    run(
        &c.p,
        "user-prompt-submit",
        &[("KONTEXT_NOW", NOW)],
        &payload,
    );
    assert!(c.dump("2026-11-07.md").contains("still captured"));
    // A transcript with garbage lines.
    fs::write(
        &c.transcript,
        "not json\n{\"type\":\"assistant\"\n\u{0}\u{1}\n",
    )
    .unwrap();
    c.prompt("after garbage", "p-10");
    c.trigger("stop");
    // A project that does not exist.
    run(
        &c.p,
        "user-prompt-submit",
        &[("KONTEXT_PROJECT", "nothing")],
        &c.payload("UserPromptSubmit", Some(("x", "p-11"))),
    );
    // A dumps path that cannot be written: an error in hooks.log, nothing printed.
    let c = cap();
    fs::create_dir_all(c.repo.join(".pm")).unwrap();
    fs::write(c.repo.join(".pm/dumps"), "a file, not a folder").unwrap();
    c.prompt("cannot be written", "p-1");
    let log = c.log();
    assert!(log.contains("user-prompt-submit: error:"), "{log}");
    assert!(!log.contains("cannot be written"), "{log}");
}

/// What agents read does not change with the capture on.
#[test]
fn agent_outputs_do_not_change_with_capture_on() {
    let (p, _g, e) = common::with_epic();
    p.agent_ok("coder", &["ask", "Which currency?", "--to", "pm"]);
    let snap = |p: &Project| -> Vec<Vec<u8>> {
        let envs = [("KONTEXT_NOW", NOW), ("KONTEXT_ROLE", "pm")];
        let mut out = Vec::new();
        for args in [
            vec!["resume", "--as", "pm", "--peek"],
            vec!["standup", "--peek"],
            vec!["brief", e.as_str(), "--peek"],
            vec!["inbox"],
        ] {
            out.push(p.kontext_env(&envs, &args).stdout);
        }
        let mut c = p.cmd(&["hook", "session-start"]);
        c.env("KONTEXT_NOW", NOW).env("KONTEXT_ROLE", "pm");
        let mut child = c
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(br#"{"session_id":"s-x","source":"startup"}"#)
            .unwrap();
        out.push(child.wait_with_output().unwrap().stdout);
        out
    };
    let before = snap(&p);
    assert!(before.iter().all(|o| !o.is_empty()));
    set_capture(&p, "enabled = true\nskip_prefixes = [\"<invoice-bot>\"]");
    assert_eq!(snap(&p), before);
}

/// A linked worktree of the repo (`git worktree add`), with one commit so it
/// can be made.
fn linked_worktree(c: &Cap) -> PathBuf {
    git(
        &c.repo,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "init",
        ],
    );
    let wt = c.p.dir.path().join("billing-app-agent");
    git(
        &c.repo,
        &["worktree", "add", "-q", "-b", "agent", wt.to_str().unwrap()],
    );
    wt
}

fn prompt_in(c: &Cap, cwd: &Path, text: &str, id: &str) {
    let payload = json!({"session_id": "s-wt", "cwd": cwd, "prompt": text, "prompt_id": id,
                         "transcript_path": c.transcript})
    .to_string();
    run(
        &c.p,
        "user-prompt-submit",
        &[("KONTEXT_NOW", NOW)],
        &payload,
    );
}

#[test]
fn linked_worktrees_are_skipped_unless_enabled() {
    let c = cap();
    let wt = linked_worktree(&c);
    // A delegated agent in its own worktree, with no KONTEXT_ROLE: it counts
    // as pm, but its prompts are not the human's.
    prompt_in(&c, &wt, "brief for the invoices export task", "p-1");
    let sub = wt.join("src");
    fs::create_dir_all(&sub).unwrap();
    prompt_in(&c, &sub, "another brief", "p-2");
    assert!(!wt.join(".pm").exists());
    let log = c.log();
    assert_eq!(
        log.matches(
            "capture: a linked git worktree, nothing written ([capture] worktrees is not true)"
        )
        .count(),
        2,
        "{log}"
    );
    assert!(!log.contains("brief"), "{log}");
    // The main checkout is unaffected.
    prompt_in(&c, &c.repo, "the human's message", "p-3");
    assert!(c.dump("2026-11-07.md").contains("the human's message"));

    // With `worktrees = true`, the worktree's own .pm/.
    let c = cap_with("enabled = true\nworktrees = true");
    let wt = linked_worktree(&c);
    prompt_in(&c, &wt, "written in the worktree", "p-1");
    let d = fs::read_to_string(wt.join(".pm/dumps/2026-11-07.md")).unwrap();
    assert_eq!(
        d,
        "## 2026-11-07 10:42\n\n**User:**\n\nwritten in the worktree\n"
    );
    assert!(!c.dumps().exists(), "not in the main checkout's .pm/");
}
