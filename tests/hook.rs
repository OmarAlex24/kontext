//! `kontext hook session-start`: the three modes, the caps and that it never fails.

mod common;

use std::fs;
use std::io::Write;
use std::process::{Output, Stdio};

use common::Project;
use serde_json::Value;

fn hook(p: &Project, envs: &[(&str, &str)], stdin: &str) -> Output {
    let mut c = p.cmd(&["hook", "session-start"]);
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
    assert_eq!(out.status.code(), Some(0), "the hook always exits with 0");
    out
}

fn context(out: &Output) -> String {
    let v: Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stdout)));
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "SessionStart");
    v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .to_string()
}

/// Changes `default` in `[context]` (the one from `kontext init` is `inject`).
fn set_default_mode(p: &Project, mode: &str) {
    let toml = p.home.join("project.toml");
    let t = fs::read_to_string(&toml).unwrap();
    assert!(t.contains("default = \"inject\""), "{t}");
    fs::write(
        &toml,
        t.replace("default = \"inject\"", &format!("default = \"{mode}\"")),
    )
    .unwrap();
}

const INPUT: &str = r#"{"session_id":"s-1","source":"startup","hook_event_name":"SessionStart"}"#;

#[test]
fn inject_pointer_and_off() {
    let p = Project::new();
    let a = p.id_of(&[], &["charter", "--objective", "Charge"]);
    p.ctx_ok(&["sign", &a]);
    p.agent_ok("coder", &["ask", "Price?", "--to", "pm"]);

    // pm = "inject" in the project.toml from `kontext init`.
    let text = context(&hook(&p, &[("KONTEXT_ROLE", "pm")], INPUT));
    assert!(
        text.starts_with("# Context of demo for pm (kontext resume)"),
        "{text}"
    );
    assert!(text.contains(&format!("{a} [signed] Charge")), "{text}");
    // The hook does not advance the cursor.
    assert!(!p.home.join("cursors/pm").exists());

    // Another role falls back to default, which is "inject" in `kontext init`.
    let text = context(&hook(&p, &[("KONTEXT_ROLE", "implementer")], INPUT));
    assert!(
        text.starts_with("# Context of demo for implementer (kontext resume)"),
        "{text}"
    );
    assert!(text.contains(&format!("{a} [signed] Charge")), "{text}");

    // With default = "pointer", the pointer.
    set_default_mode(&p, "pointer");
    let text = context(&hook(&p, &[("KONTEXT_ROLE", "implementer")], INPUT));
    assert!(text.lines().count() <= 3, "{text}");
    assert!(
        text.contains("there are 3 updates and 0 questions for you (implementer)"),
        "{text}"
    );
    assert!(text.contains("`kontext resume --as implementer`"), "{text}");
    // What the user said applies unsigned; what was superseded does not.
    assert!(
        text.contains("What the user said applies even if unsigned"),
        "{text}"
    );
    assert!(
        text.contains("what was superseded no longer applies"),
        "{text}"
    );
    // How to invoke kontext so `Bash(kontext:*)` covers it, and who writes.
    assert!(text.contains("`Bash(kontext:*)`"), "{text}");
    assert!(text.contains("no `VAR=…` or `export` in front"), "{text}");
    assert!(text.contains("You write as human:omar"), "{text}");
    let text = context(&hook(
        &p,
        &[
            ("KONTEXT_ROLE", "implementer"),
            ("KONTEXT_ACTOR", "agent:impl-1"),
        ],
        INPUT,
    ));
    assert!(text.lines().count() <= 3, "{text}");
    assert!(
        text.contains("You write as agent:impl-1 (implementer)"),
        "{text}"
    );
    let text = context(&hook(
        &p,
        &[("KONTEXT_ROLE", "implementer"), ("KONTEXT_ACTOR", "")],
        INPUT,
    ));
    assert!(
        text.contains("`kontext --actor agent:<your-name> --role <role> log …`"),
        "{text}"
    );
    let text = context(&hook(&p, &[("KONTEXT_ROLE", "pm")], INPUT));
    assert!(text.contains("## Questions for pm (1)"), "{text}");
    assert!(text.contains("`Bash(kontext:*)`"), "{text}");

    // Without KONTEXT_ROLE: default_role from the repo's .kontext.toml (the hook's cwd).
    let repo = p.dir.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    fs::write(
        repo.join(".kontext.toml"),
        "project = \"demo\"\ndefault_role = \"tester\"\n",
    )
    .unwrap();
    let input = format!(r#"{{"cwd": "{}"}}"#, repo.display());
    let text = context(&hook(&p, &[("KONTEXT_ROLE", "")], &input));
    assert!(text.contains("for you (tester)"), "{text}");

    // Without `[context]` in project.toml, inject for everyone.
    let toml = p.home.join("project.toml");
    let t = fs::read_to_string(&toml).unwrap();
    let sin: String = t
        .lines()
        .filter(|l| {
            !l.starts_with("pm = ")
                && !l.starts_with("coordinator = ")
                && !l.starts_with("default = ")
        })
        .map(|l| format!("{l}\n"))
        .collect();
    fs::write(&toml, &sin).unwrap();
    let text = context(&hook(&p, &[("KONTEXT_ROLE", "scout")], INPUT));
    assert!(
        text.starts_with("# Context of demo for scout (kontext resume)"),
        "{text}"
    );
    fs::write(&toml, &t).unwrap();

    // off: nothing.
    let toml = p.home.join("project.toml");
    let t = fs::read_to_string(&toml).unwrap();
    fs::write(
        &toml,
        t.replace("default = \"pointer\"", "default = \"off\""),
    )
    .unwrap();
    let out = hook(&p, &[("KONTEXT_ROLE", "scout")], INPUT);
    assert!(
        out.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );

    let log = fs::read_to_string(p.home.join(".kontext/hooks.log")).unwrap();
    assert!(
        log.contains("session-start session=s-1 source=startup role=pm mode=inject"),
        "{log}"
    );
    assert!(log.contains("role=scout mode=off"), "{log}");
}

#[test]
fn inject_respects_the_hard_cap() {
    let p = Project::new();
    let mut lines = String::new();
    for i in 0..300 {
        lines.push_str(&format!(
            "{{\"kind\":\"decision\",\"title\":\"Decision {i} with a long title to fill the budget\",\"actor\":{{\"type\":\"human\",\"name\":\"omar\"}}}}\n"
        ));
    }
    p.append(&lines);
    let text = context(&hook(&p, &[("KONTEXT_ROLE", "pm")], INPUT));
    assert!(text.chars().count() <= 9900, "{}", text.chars().count());
    assert!(text.contains("cut for the hook"), "{text}");
}

#[test]
fn never_fails() {
    let p = Project::new();
    // Input that is not JSON.
    let out = hook(&p, &[("KONTEXT_ROLE", "pm")], "this is not json");
    assert!(context(&out).contains("# Context of demo"));
    // A project that does not exist: no output and no noise on stderr.
    let out = hook(&p, &[("KONTEXT_PROJECT", "nothing")], INPUT);
    assert!(out.stdout.is_empty() && out.stderr.is_empty());
    // A broken kontext-home: exits with 0 and notes it in hooks.log.
    fs::write(p.home.join(".kontext/index.sqlite"), "I am not a database").unwrap();
    let out = hook(&p, &[("KONTEXT_ROLE", "pm")], INPUT);
    assert!(out.stdout.is_empty() && out.stderr.is_empty());
    let log = fs::read_to_string(p.home.join(".kontext/hooks.log")).unwrap();
    assert!(log.contains("session-start: error:"), "{log}");
    // Other events: just noted.
    let mut c = p.cmd(&["hook", "pre-compact"]);
    let out = c.stdin(Stdio::null()).output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    // A ledger with a broken line: the hook warns in the context.
    let p = Project::new();
    p.ctx_ok(&["log", "one"]);
    let mut f = fs::OpenOptions::new()
        .append(true)
        .open(p.ledger("omar@test"))
        .unwrap();
    f.write_all(b"{\"roto\":").unwrap();
    let text = context(&hook(&p, &[("KONTEXT_ROLE", "pm")], INPUT));
    assert!(text.contains("CHAIN BROKEN"), "{text}");
}

#[test]
fn coordinator_with_epic_gets_its_brief() {
    let (p, _g, e) = common::with_epic();
    p.ctx_env_ok(
        &[
            ("KONTEXT_ACTOR", "agent:epic:payments"),
            ("KONTEXT_ROLE", "coordinator"),
        ],
        &["task", "add", "Integrate Stripe", "--epic", "payments"],
    );
    let text = context(&hook(
        &p,
        &[
            ("KONTEXT_ROLE", "coordinator"),
            ("KONTEXT_EPIC", "payments"),
        ],
        INPUT,
    ));
    assert!(
        text.starts_with("# Brief for epic Card payments (epic:payments)"),
        "{text}"
    );
    assert!(text.contains("Integrate Stripe"), "{text}");
    assert!(text.contains(&e), "{text}");
    // Without advancing the epic's cursor.
    assert!(!p.home.join("cursors/epic:payments").exists());
    // Without KONTEXT_EPIC, the usual (coordinator = "inject": resume).
    let text = context(&hook(&p, &[("KONTEXT_ROLE", "coordinator")], INPUT));
    assert!(
        text.starts_with("# Context of demo for coordinator"),
        "{text}"
    );
}

fn start(source: &str, sid: &str) -> String {
    format!(r#"{{"session_id":"{sid}","source":"{source}","hook_event_name":"SessionStart"}}"#)
}

/// What the hook put, or `None` if it put nothing.
fn maybe_context(out: &Output) -> Option<String> {
    if out.stdout.is_empty() {
        return None;
    }
    Some(context(out))
}

#[test]
fn resume_only_gets_what_changed() {
    let p = Project::new();
    let a = p.id_of(&[], &["charter", "--objective", "Charge"]);
    p.ctx_ok(&["sign", &a]);
    let pm = [("KONTEXT_ROLE", "pm")];

    // startup: full.
    let full = context(&hook(&p, &pm, &start("startup", "s-a")));
    assert!(full.contains("# Context of demo for pm"), "{full}");
    assert!(p.home.join(".kontext/sessions/s-a.json").is_file());

    // resume with no changes: nothing.
    let out = hook(&p, &pm, &start("resume", "s-a"));
    assert_eq!(maybe_context(&out), None);
    let out = hook(&p, &pm, &start("resume", "s-a"));
    assert_eq!(maybe_context(&out), None);

    // resume after a new entry: only that, without the fixed lines.
    let d = p.id_of(
        &[],
        &[
            "decide",
            "Amounts in cents",
            "--why",
            "float does not add up",
        ],
    );
    let delta = context(&hook(&p, &pm, &start("resume", "s-a")));
    assert!(
        delta.starts_with("# kontext: what's new since the last context of this session (1)"),
        "{delta}"
    );
    assert!(
        delta.contains(&d) && delta.contains("Amounts in cents"),
        "{delta}"
    );
    assert!(!delta.contains("## Charter"), "{delta}");
    assert!(!delta.contains("Bash(kontext:*)"), "{delta}");
    assert!(delta.chars().count() < 400, "{delta}");
    // And it does not repeat.
    assert_eq!(maybe_context(&hook(&p, &pm, &start("resume", "s-a"))), None);

    // What supersedes something else says that the old one no longer applies.
    let f = p.append(
        "{\"kind\":\"fact\",\"title\":\"The rate is 1.5 %\",\"actor\":{\"type\":\"agent\",\"name\":\"distill\"},\"fields\":{\"origin\":\"usuario\",\"fingerprints\":[]}}\n",
    );
    let f = f[0].split_whitespace().next().unwrap().to_string();
    let delta = context(&hook(&p, &pm, &start("resume", "s-a")));
    assert!(delta.contains("The rate is 1.5 %"), "{delta}");
    p.append(&format!(
        "{{\"kind\":\"fact\",\"title\":\"The rate is 1.8 %\",\"actor\":{{\"type\":\"agent\",\"name\":\"distill\"}},\"fields\":{{\"origin\":\"usuario\",\"fingerprints\":[],\"supersedes\":[\"{f}\"]}}}}\n"
    ));
    let delta = context(&hook(&p, &pm, &start("resume", "s-a")));
    assert!(delta.contains("The rate is 1.8 %"), "{delta}");
    assert!(delta.contains(&format!("supersedes {f}")), "{delta}");
    assert!(delta.contains("no longer applies"), "{delta}");

    // compact: full again (the context was lost when compacting).
    let full = context(&hook(&p, &pm, &start("compact", "s-a")));
    assert!(full.contains("# Context of demo for pm"), "{full}");
    assert!(full.contains("Amounts in cents"), "{full}");
    assert_eq!(maybe_context(&hook(&p, &pm, &start("resume", "s-a"))), None);

    // clear, and a session that kontext does not know: full.
    let full = context(&hook(&p, &pm, &start("clear", "s-a")));
    assert!(full.contains("# Context of demo"), "{full}");
    let full = context(&hook(&p, &pm, &start("resume", "s-otra")));
    assert!(full.contains("# Context of demo"), "{full}");

    // Corrupt or empty state: full, and it fixes itself.
    fs::write(p.home.join(".kontext/sessions/s-a.json"), "{roto").unwrap();
    let full = context(&hook(&p, &pm, &start("resume", "s-a")));
    assert!(full.contains("# Context of demo"), "{full}");
    assert_eq!(maybe_context(&hook(&p, &pm, &start("resume", "s-a"))), None);
    fs::write(p.home.join(".kontext/sessions/s-a.json"), "").unwrap();
    let full = context(&hook(&p, &pm, &start("resume", "s-a")));
    assert!(full.contains("# Context of demo"), "{full}");

    // Another role in the same session: full.
    let text = context(&hook(
        &p,
        &[("KONTEXT_ROLE", "implementer")],
        &start("resume", "s-a"),
    ));
    assert!(
        text.starts_with("# Context of demo for implementer"),
        "{text}"
    );

    // A session_id that is not a usable file name: always full.
    let full = context(&hook(&p, &pm, &start("startup", "../x")));
    assert!(full.contains("# Context of demo"), "{full}");
    let full = context(&hook(&p, &pm, &start("resume", "../x")));
    assert!(full.contains("# Context of demo"), "{full}");

    // The state does not dirty the ledger or sync: it lives in .kontext/.
    let ledger = fs::read_to_string(p.ledger("omar@test")).unwrap();
    assert!(!ledger.contains("s-a"), "{ledger}");

    let log = fs::read_to_string(p.home.join(".kontext/hooks.log")).unwrap();
    assert!(
        log.contains("source=resume role=pm mode=inject context=nothing"),
        "{log}"
    );
    assert!(
        log.contains("source=resume role=pm mode=inject context=delta"),
        "{log}"
    );
    assert!(
        log.contains("source=compact role=pm mode=inject context=full"),
        "{log}"
    );
}

#[test]
fn pointer_resume_is_short_too() {
    let p = Project::new();
    set_default_mode(&p, "pointer");
    let imp = [("KONTEXT_ROLE", "implementer")];
    let full = context(&hook(&p, &imp, &start("startup", "s-p")));
    assert!(full.contains("`kontext resume --as implementer`"), "{full}");
    assert_eq!(
        maybe_context(&hook(&p, &imp, &start("resume", "s-p"))),
        None
    );
    let d = p.id_of(&[], &["decide", "Amounts in cents"]);
    let delta = context(&hook(&p, &imp, &start("resume", "s-p")));
    assert!(delta.contains(&d), "{delta}");
    assert!(!delta.contains("Bash(kontext:*)"), "{delta}");
    assert_eq!(
        maybe_context(&hook(&p, &imp, &start("resume", "s-p"))),
        None
    );
}

#[test]
fn coordinator_resume_only_sees_its_epic() {
    let (p, _g, _e) = common::with_epic();
    let env = [
        ("KONTEXT_ROLE", "coordinator"),
        ("KONTEXT_EPIC", "payments"),
    ];
    let full = context(&hook(&p, &env, &start("startup", "s-c")));
    assert!(full.starts_with("# Brief for epic"), "{full}");
    assert_eq!(
        maybe_context(&hook(&p, &env, &start("resume", "s-c"))),
        None
    );
    p.ctx_env_ok(
        &[
            ("KONTEXT_ACTOR", "agent:epic:payments"),
            ("KONTEXT_ROLE", "coordinator"),
        ],
        &["task", "add", "Integrate Stripe", "--epic", "payments"],
    );
    let delta = context(&hook(&p, &env, &start("resume", "s-c")));
    assert!(delta.contains("Integrate Stripe"), "{delta}");
    assert!(!delta.starts_with("# Brief"), "{delta}");
}

#[test]
fn with_distill_the_agent_does_not_repeat_what_it_saves() {
    let p = Project::new();
    set_default_mode(&p, "pointer");
    let on = [("KONTEXT_ROLE", "pm"), ("KONTEXT_DISTILL_DISABLE", "")];
    // `kontext init` leaves the distill on.
    let text = context(&hook(&p, &on, INPUT));
    assert!(
        text.contains(
            "The kontext distill already saves on its own, when this session closes or compacts"
        ),
        "{text}"
    );
    assert!(
        text.contains("do not run `kontext propose` or `kontext log` to repeat that"),
        "{text}"
    );
    assert!(text.contains("`kontext ask`"), "{text}");
    assert!(!text.contains("The conversation is disposable"), "{text}");
    let ptr = [
        ("KONTEXT_ROLE", "implementer"),
        ("KONTEXT_DISTILL_DISABLE", ""),
    ];
    let text = context(&hook(&p, &ptr, INPUT));
    assert!(text.lines().count() <= 3, "{text}");
    assert!(
        text.contains("do not run `kontext propose` or `kontext log`"),
        "{text}"
    );
    // With KONTEXT_DISTILL_DISABLE or `enabled = false`, as before.
    let text = context(&hook(
        &p,
        &[("KONTEXT_ROLE", "pm"), ("KONTEXT_DISTILL_DISABLE", "1")],
        INPUT,
    ));
    assert!(text.contains("The conversation is disposable"), "{text}");
    assert!(!text.contains("The kontext distill"), "{text}");
    let toml = p.home.join("project.toml");
    let t = fs::read_to_string(&toml).unwrap();
    fs::write(&toml, t.replace("enabled = true", "enabled = false")).unwrap();
    let text = context(&hook(&p, &on, INPUT));
    assert!(text.contains("The conversation is disposable"), "{text}");
    let text = context(&hook(&p, &ptr, INPUT));
    assert!(
        text.contains("the `kontext` skill explains what else to check"),
        "{text}"
    );
}

#[test]
fn the_inbox_line_does_not_send_the_agent_to_the_inbox() {
    let p = Project::new();
    // As in the bench: the owner is not the human of this machine's `git config`.
    let toml = p.home.join("project.toml");
    let t = fs::read_to_string(&toml).unwrap();
    fs::write(
        &toml,
        t.replace("owners = [\"omar\"]", "owners = [\"owner\"]"),
    )
    .unwrap();
    p.ctx_env_ok(
        &[("KONTEXT_ACTOR", "human:owner")],
        &["charter", "--objective", "Charge"],
    );
    let text = context(&hook(&p, &[("KONTEXT_ROLE", "pm")], INPUT));
    assert!(
        text.starts_with("owner has 1 pending item in kontext"),
        "{text}"
    );
    assert!(text.contains("Tell them once, when you start"), "{text}");
    assert!(
        text.contains("Don't run `kontext inbox` on your own: it is their inbox"),
        "{text}"
    );
    // And if the agent runs it, it sees the same pending items the hook names
    // (it used to look at the `git config` human's inbox and came out empty).
    let out = p.ctx_env_ok(
        &[("KONTEXT_ACTOR", "agent:pm"), ("KONTEXT_ROLE", "pm")],
        &["inbox", "count"],
    );
    assert!(out.contains('1'), "{out}");
    assert!(!out.contains("(none)"), "{out}");
}
