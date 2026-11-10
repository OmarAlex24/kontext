//! `kontext measure`: exact numbers from an invented transcript, without a
//! project or any kontext configuration, and none of its text in the output.

mod common;

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::{Value, json};

const TYPED: &str = "Add the invoices export";
const TYPED_2: &str = "go";
const SAID: &str = "I will read the billing module.";
const SAID_2: &str = "Done.";
const READ_RESULT: &str = "fn invoice() {}\n";
const BASH_RESULT: &str = "resume output";
const DELEGATE: &str = "Delegated task node: export done";
const PR: &str = "Update on pull request #7: checks passed";
const SUMMARY: &str = "Summary of the invoices work";
const META: &str = "Caveat: generated while running local commands";
const BASH_CMD: &str =
    "KONTEXT_ACTOR=agent:pm kontext --json resume --as pm && kontext log 'checked billing'";

fn usage(input: u64, read: u64, created: u64, output: u64) -> Value {
    json!({"input_tokens": input, "cache_read_input_tokens": read,
           "cache_creation_input_tokens": created, "output_tokens": output})
}

fn assistant(ts: &str, id: &str, model: &str, content: Value, usage: Value) -> Value {
    json!({"type": "assistant", "timestamp": ts,
           "message": {"id": id, "model": model, "role": "assistant", "content": content, "usage": usage}})
}

fn user(ts: Option<&str>, content: Value) -> Value {
    let mut v = json!({"type": "user", "message": {"role": "user", "content": content}});
    if let Some(ts) = ts {
        v["timestamp"] = json!(ts);
    }
    v
}

/// The invented transcript: every kind of line `measure` tells apart.
fn transcript() -> String {
    let read_input = json!({"file_path": "src/billing.rs"});
    let bash_input = json!({"command": BASH_CMD});
    let mut lines = vec![
        user(Some("2026-11-01T09:00:00Z"), json!(TYPED)),
        // One request, two lines (one per block), the usage changing: the last wins.
        assistant(
            "2026-11-01T09:00:01Z",
            "msg_1",
            "model-a",
            json!([{"type": "text", "text": SAID}]),
            usage(10, 100, 5, 1),
        ),
        assistant(
            "2026-11-01T09:00:01Z",
            "msg_1",
            "model-a",
            json!([{"type": "tool_use", "id": "toolu_1", "name": "Read", "input": read_input}]),
            usage(10, 100, 5, 40),
        ),
        user(
            Some("2026-11-01T09:00:02Z"),
            json!([{"type": "tool_result", "tool_use_id": "toolu_1", "content": READ_RESULT}]),
        ),
        // A subagent's line: skipped everywhere.
        {
            let mut v = assistant(
                "2026-11-01T09:00:02Z",
                "msg_side",
                "model-a",
                json!([{"type": "text", "text": "subagent work on invoices"}]),
                usage(1, 999, 1, 1),
            );
            v["isSidechain"] = json!(true);
            v
        },
        assistant(
            "2026-11-01T09:00:03Z",
            "msg_2",
            "model-a",
            json!([{"type": "tool_use", "id": "toolu_2", "name": "Bash", "input": bash_input}]),
            usage(2, 300, 0, 20),
        ),
        // No timestamp: it takes the previous line's.
        user(
            None,
            json!([{"type": "tool_result", "tool_use_id": "toolu_2", "content": [
                {"type": "text", "text": BASH_RESULT},
                {"type": "image", "source": {"type": "base64", "data": "AAAA"}}
            ]}]),
        ),
        user(Some("2026-11-01T09:01:00Z"), json!(DELEGATE)),
        user(
            Some("2026-11-01T09:02:00Z"),
            json!([{"type": "text", "text": PR}]),
        ),
        {
            let mut v = user(Some("2026-11-01T09:03:00Z"), json!(SUMMARY));
            v["isCompactSummary"] = json!(true);
            v
        },
        {
            let mut v = user(Some("2026-11-01T09:03:30Z"), json!(META));
            v["isMeta"] = json!(true);
            v
        },
        assistant(
            "2026-11-01T09:04:00Z",
            "msg_3",
            "model-b",
            json!([{"type": "text", "text": SAID_2}]),
            usage(1, 50, 0, 3),
        ),
        user(Some("2026-11-01T09:05:00Z"), json!(TYPED_2)),
        // The next day: outside `--until`.
        user(Some("2026-11-02T10:00:00Z"), json!("drop the old export")),
        assistant(
            "2026-11-02T10:00:01Z",
            "msg_4",
            "model-a",
            json!([{"type": "text", "text": "Dropped."}]),
            usage(1, 7777, 0, 1),
        ),
    ]
    .into_iter()
    .map(|v| v.to_string())
    .collect::<Vec<_>>();
    lines.insert(10, "{not json".into());
    lines.join("\n") + "\n"
}

fn measure(dir: &Path, args: &[&str]) -> Output {
    // No project and no config: an empty config dir, no HOME config either.
    Command::new(common::BIN)
        .arg("measure")
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("KONTEXT_CONFIG_DIR", dir.join("no-config"))
        .env("CLAUDE_CONFIG_DIR", dir.join("no-claude"))
        .env_remove("KONTEXT_PROJECT")
        .output()
        .unwrap()
}

fn json_of(out: &Output) -> Value {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

fn chars(s: &str) -> u64 {
    s.chars().count() as u64
}

fn category<'a>(v: &'a Value, key: &str) -> &'a Value {
    v["history"]["categories"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["category"] == key)
        .unwrap_or_else(|| panic!("no {key}"))
}

/// Nothing from the transcript's text, arguments or results comes out.
fn assert_private(out: &str) {
    for secret in [
        TYPED,
        SAID,
        READ_RESULT.trim(),
        BASH_RESULT,
        "export done",
        "checks passed",
        SUMMARY,
        "Caveat",
        "src/billing.rs",
        "checked billing",
        "--as pm",
        "subagent work",
        "drop the old export",
        "Dropped",
    ] {
        assert!(!out.contains(secret), "{secret:?} leaked:\n{out}");
    }
}

#[test]
fn measures_an_invented_transcript_exactly() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("session.jsonl");
    fs::write(&file, transcript()).unwrap();
    let f = file.to_str().unwrap();

    let v = json_of(&measure(
        dir.path(),
        &["--json", f, "--until", "2026-11-01T23:59:59Z"],
    ));
    assert_eq!(v["lines"], 16);
    assert_eq!(v["malformed_lines"], 1);
    assert_eq!(v["sidechain_lines"], 1);
    assert_eq!(v["lines_outside_window"], 2);
    assert_eq!(v["first_timestamp"], "2026-11-01T09:00:00Z");
    assert_eq!(v["last_timestamp"], "2026-11-01T09:05:00Z");
    assert_eq!(v["until"], "2026-11-01T23:59:59Z");
    assert_eq!(v["since"], Value::Null);
    // msg_1 once (its last usage), msg_2 and msg_3; not the sidechain's or msg_4.
    assert_eq!(v["requests"], 3);
    assert_eq!(
        v["tokens"],
        json!({"input": 13, "cache_read": 450, "cache_creation": 5, "output": 63})
    );
    assert_eq!(v["avg_cache_read_per_request"], 150);
    assert_eq!(v["max_cache_read"], 300);
    assert_eq!(v["compactions"], 1);
    assert_eq!(
        v["models"],
        json!([{"model": "model-a", "requests": 2}, {"model": "model-b", "requests": 1}])
    );

    let call_chars = chars(&json!({"file_path": "src/billing.rs"}).to_string())
        + chars(&json!({"command": BASH_CMD}).to_string());
    let expect = [
        ("tool_results", 2, chars(READ_RESULT) + chars(BASH_RESULT)),
        ("tool_calls", 2, call_chars),
        ("assistant_text", 2, chars(SAID) + chars(SAID_2)),
        ("typed_by_human", 2, chars(TYPED) + chars(TYPED_2)),
        ("delegate_reports", 1, chars(DELEGATE)),
        ("pr_updates", 1, chars(PR)),
        ("compaction_summaries", 1, chars(SUMMARY)),
        ("other_machine_text", 1, chars(META)),
    ];
    let total: u64 = expect.iter().map(|e| e.2).sum();
    assert_eq!(v["history"]["chars"], total);
    for (key, count, n) in expect {
        let c = category(&v, key);
        assert_eq!(c["count"], count, "{key}");
        assert_eq!(c["chars"], n, "{key}");
    }
    // The literal sizes, so a change in how they are counted shows up here.
    assert_eq!(category(&v, "tool_results")["chars"], 29);
    assert_eq!(category(&v, "typed_by_human")["chars"], 25);

    assert_eq!(
        v["tools"]["calls"],
        json!([{"name": "Bash", "calls": 1}, {"name": "Read", "calls": 1}])
    );
    assert_eq!(
        v["tools"]["results"],
        json!([{"name": "Read", "results": 1, "chars": 16}, {"name": "Bash", "results": 1, "chars": 13}])
    );
    assert_eq!(v["tools"]["result_non_text_blocks"], 1);
    // One Bash call, two kontext commands; its result goes to the last one.
    assert_eq!(v["kontext"]["bash_calls"], 1);
    assert_eq!(v["kontext"]["result_chars"], 13);
    assert_eq!(
        v["kontext"]["subcommands"],
        json!([{"subcommand": "log", "calls": 1, "result_chars": 13},
               {"subcommand": "resume", "calls": 1, "result_chars": 0}])
    );
    assert_private(&String::from_utf8_lossy(
        &measure(
            dir.path(),
            &["--json", f, "--until", "2026-11-01T23:59:59Z"],
        )
        .stdout,
    ));

    // Without a window: msg_4 and the next day's lines count too.
    let all = json_of(&measure(dir.path(), &["--json", f]));
    assert_eq!(all["requests"], 4);
    assert_eq!(all["max_cache_read"], 7777);
    assert_eq!(all["lines_outside_window"], 0);
    assert_eq!(all["last_timestamp"], "2026-11-02T10:00:01Z");
    // --since is inclusive.
    let late = json_of(&measure(
        dir.path(),
        &["--json", f, "--since", "2026-11-01T09:04:00Z"],
    ));
    assert_eq!(late["requests"], 2);
    assert_eq!(late["first_timestamp"], "2026-11-01T09:04:00Z");

    // The text report: the same numbers, none of the text.
    let out = measure(dir.path(), &[f, "--until", "2026-11-01T23:59:59Z"]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    for needed in [
        "Requests: 3",
        "Tokens: input 13; cache read 450; cache creation 5; output 63",
        "History re-read per request: 150 tokens on average, 300 at most",
        "Compactions: 1",
        "Models: model-a (2), model-b (1)",
        "Covers: 2026-11-01T09:00:00Z to 2026-11-01T09:05:00Z",
        "1 malformed, skipped",
        "kontext commands (Bash calls with kontext: 1; their result characters: 13):",
    ] {
        assert!(text.contains(needed), "missing {needed:?}:\n{text}");
    }
    assert_private(&text);
}

#[test]
fn several_files_and_errors() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.jsonl");
    let b = dir.path().join("b.jsonl");
    fs::write(&a, transcript()).unwrap();
    // The same message id in another file is another request.
    fs::write(
        &b,
        assistant(
            "2026-11-01T12:00:00Z",
            "msg_1",
            "model-a",
            json!([{"type": "text", "text": "ok"}]),
            usage(0, 1000, 0, 1),
        )
        .to_string()
            + "\n",
    )
    .unwrap();
    let (fa, fb) = (a.to_str().unwrap(), b.to_str().unwrap());
    let v = json_of(&measure(dir.path(), &["--json", fa, fb]));
    assert_eq!(v["requests"], 5);
    assert_eq!(v["files"][0]["requests"], 4);
    assert_eq!(v["files"][1]["requests"], 1);
    assert_eq!(v["files"][1]["avg_cache_read_per_request"], 1000);
    let text = String::from_utf8_lossy(&measure(dir.path(), &[fa, fb]).stdout).into_owned();
    assert!(text.contains("Per file (requests;"), "{text}");
    assert!(text.contains(&format!("{fa}: 4; ")), "{text}");
    assert!(text.contains(&format!("{fb}: 1; 1,000 tokens")), "{text}");

    // A missing file names the file; a bad timestamp says which flag.
    let missing = dir.path().join("nope.jsonl");
    let out = measure(dir.path(), &[fa, missing.to_str().unwrap()]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("nope.jsonl"), "{err}");
    let out = measure(dir.path(), &[fa, "--since", "last tuesday"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--since") && err.contains("RFC 3339"), "{err}");
    // It wrote nothing, not even a config dir.
    assert!(!dir.path().join("no-config").exists());
}
