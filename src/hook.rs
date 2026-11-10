//! `kontext hook <event>` (DESIGN §9): the entry point for the Claude Code hooks.
//!
//! A hook never blocks the session: it always exits with 0. What happens (and
//! any errors) is noted in `<kontext-home>/.kontext/hooks.log`. If there is
//! no project at all, nothing is written, not even to stderr.
//!
//! `pre-compact` and `session-end` start `kontext distill` in the background
//! with the transcript Claude Code sends (unless `KONTEXT_DISTILL_DISABLE` or
//! `[distill] enabled = false` is set), and return right away.
//!
//! `user-prompt-submit`, `stop` and `session-end` run the capture
//! ([`crate::capture`]) if `[capture] enabled = true`: the user's messages go
//! into `.pm/dumps/` of the code repo. They never print anything.
//!
//! The start hook does not advance any cursor. What it puts in the context may
//! not be read carefully (the session starts with another task in mind), so
//! new items still count as new until someone runs `kontext resume` or
//! `kontext standup` on purpose.
//!
//! Claude Code runs the start hook on every `claude --resume`, and everything
//! it puts stays in the conversation. So it stores, per `session_id`, how far
//! the ledger reached last time (`.kontext/sessions/`, local), and on a
//! `resume` it only puts what is new (DESIGN §9.4).

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::app;
use crate::capture;
use crate::config::{self, Project};
use crate::cursor::Frontier;
use crate::distill;
use crate::store::Store;

/// What is asked in `inject` mode (DESIGN §9.1).
pub const INJECT_MAX: usize = 9000;
/// Claude Code swaps the text for a path if it goes over 10,000.
pub const HARD_CAP: usize = 9900;

fn log_line(home: &Path, msg: &str) {
    let dir = config::internal_dir(home);
    let _ = fs::create_dir_all(&dir);
    if let Ok(mut f) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("hooks.log"))
    {
        let _ = writeln!(f, "{} {msg}", jiff::Timestamp::now());
    }
}

fn read_input(stdin: &mut dyn Read) -> Value {
    let mut s = String::new();
    if stdin.read_to_string(&mut s).is_err() {
        return json!({});
    }
    match serde_json::from_str::<Value>(&s) {
        Ok(v @ Value::Object(_)) => v,
        _ => json!({}),
    }
}

/// The role: `$KONTEXT_ROLE`, then `default_role` in `.kontext.toml`, then `pm`.
pub fn role_for(dir: &Path) -> String {
    if let Ok(r) = std::env::var("KONTEXT_ROLE")
        && !r.trim().is_empty()
    {
        return r.trim().to_string();
    }
    if let Ok(Some((_, link))) = config::find_repo_link(dir)
        && let Some(r) = link.default_role.filter(|r| !r.trim().is_empty())
    {
        return r;
    }
    "pm".into()
}

/// The mode of the role in `[context]`: its own, then `default`, then `inject`.
pub fn mode_for(project: &Project, role: &str) -> String {
    project
        .context
        .get(role)
        .or_else(|| project.context.get("default"))
        .cloned()
        .unwrap_or_else(|| "inject".into())
}

fn cap(text: String) -> String {
    if text.chars().count() <= HARD_CAP {
        return text;
    }
    let note = "\n… (cut for the hook; run `kontext resume` to see the rest)\n";
    let keep = HARD_CAP - note.chars().count();
    let mut out: String = text.chars().take(keep).collect();
    out.push_str(note);
    out
}

/// What Claude Code sends to the start hook that the hook cares about.
#[derive(Debug, Clone, Copy, Default)]
pub struct Start<'a> {
    /// `startup`, `resume`, `clear` or `compact`.
    pub source: &'a str,
    pub session_id: Option<&'a str>,
}

/// What the start hook put in, and how.
#[derive(Debug, Clone)]
pub struct StartContext {
    pub mode: String,
    pub text: Option<String>,
    /// `full`, `delta`, `nothing` (a `resume` with no changes) or `off`.
    pub how: &'static str,
    /// If what was given to the session could not be saved, why.
    pub save_error: Option<String>,
}

/// What the hook already gave a Claude Code session (DESIGN §9.4): with
/// which role and mode, and how far the ledger reached. It lives in
/// `.kontext/sessions/<session_id>.json`, which is local and not synced.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Given {
    role: String,
    /// `inject`, `pointer` or `brief` (a coordinator with `KONTEXT_EPIC`).
    mode: String,
    epic: Option<String>,
    mark: BTreeMap<String, u64>,
}

/// A `delta` longer than this saves nothing: the full context goes instead.
pub const DELTA_MAX_INJECT: usize = INJECT_MAX / 2;
/// In `pointer` mode the full context is three lines: a long `delta` is not useful either.
pub const DELTA_MAX_POINTER: usize = 1500;
/// What was given to a session is deleted after this many days.
const GIVEN_DAYS: u64 = 30;

fn sessions_dir(home: &Path) -> PathBuf {
    config::internal_dir(home).join("sessions")
}

/// The path of a session, if its id works as a file name (Claude Code
/// sends a UUID).
fn given_path(home: &Path, session_id: &str) -> Option<PathBuf> {
    let ok = !session_id.is_empty()
        && session_id.len() <= 128
        && session_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'));
    ok.then(|| sessions_dir(home).join(format!("{session_id}.json")))
}

/// What was given to the session, or `None` if there is none or it cannot be
/// read (and then the full context goes).
fn load_given(home: &Path, session_id: &str) -> Option<Given> {
    let text = fs::read_to_string(given_path(home, session_id)?).ok()?;
    serde_json::from_str(&text).ok()
}

fn save_given(home: &Path, session_id: &str, g: &Given) -> Result<()> {
    let Some(p) = given_path(home, session_id) else {
        return Ok(());
    };
    fs::create_dir_all(sessions_dir(home))?;
    config::write_atomic(&p, (serde_json::to_string(g)? + "\n").as_bytes())?;
    prune_given(home);
    Ok(())
}

/// Deletes the records of sessions older than [`GIVEN_DAYS`] days.
fn prune_given(home: &Path) {
    prune_older(&sessions_dir(home), GIVEN_DAYS);
}

/// Deletes the files in `dir` not changed for more than `days` days.
pub fn prune_older(dir: &Path, days: u64) {
    let Ok(rd) = fs::read_dir(dir) else {
        return;
    };
    let max = std::time::Duration::from_secs(days * 24 * 3600);
    for ent in rd.flatten() {
        let old = ent
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > max);
        if old {
            let _ = fs::remove_file(ent.path());
        }
    }
}

/// The text the start hook puts in, or `None` in `off` mode. A coordinator
/// with `KONTEXT_EPIC` gets the brief of its epic (without advancing its
/// cursor).
///
/// On a `resume` of a session it already gave context to, with the same role
/// and mode, it only puts what entered the ledger since then (nothing if
/// nothing entered): the earlier part is still in the conversation. On
/// `startup`, `clear` and `compact` (after compacting, the context was lost),
/// or if it does not know what it gave the session, it puts the full context
/// (DESIGN §9.4).
pub fn session_start_context(
    store: &mut Store,
    project: &Project,
    role: &str,
    start: Start,
) -> Result<StartContext> {
    let mode = mode_for(project, role);
    if mode == "off" {
        return Ok(StartContext {
            mode,
            text: None,
            how: "off",
            save_error: None,
        });
    }
    let epic = std::env::var("KONTEXT_EPIC")
        .ok()
        .map(|e| e.trim().to_string())
        .filter(|e| !e.is_empty())
        .filter(|_| role == "coordinator");
    let kind = match (&epic, mode.as_str()) {
        (Some(_), _) => "brief",
        (None, "inject") => "inject",
        // `pointer`, or a value that is not a mode (a typo in `project.toml`):
        // the short one. What is not configured falls back to `inject`, but
        // `mode_for` already resolved that.
        _ => "pointer",
    };
    // The cursor is taken before reading: what comes in while the text is
    // being built comes out again on the next `resume` instead of being lost.
    let mark = store.frontier()?;
    let home = store.home().to_path_buf();
    let given = Given {
        role: role.to_string(),
        mode: kind.to_string(),
        epic: epic.clone(),
        mark: mark.0.clone(),
    };
    let save = |g: &Given| {
        start
            .session_id
            .and_then(|sid| save_given(&home, sid, g).err())
            .map(|e| format!("{e:#}"))
    };
    let prev = match (start.source, start.session_id) {
        ("resume", Some(sid)) => load_given(&home, sid)
            .filter(|p| p.role == given.role && p.mode == given.mode && p.epic == given.epic),
        _ => None,
    };
    if let Some(prev) = prev {
        let (_, delta) =
            app::session_delta(store, project, role, epic.as_deref(), &Frontier(prev.mark))?;
        let max = if kind == "pointer" {
            DELTA_MAX_POINTER
        } else {
            DELTA_MAX_INJECT
        };
        if delta.as_ref().is_none_or(|t| t.chars().count() <= max) {
            return Ok(StartContext {
                mode,
                how: if delta.is_some() { "delta" } else { "nothing" },
                text: delta,
                save_error: save(&given),
            });
        }
    }
    // The session that talks to the human (the pm) starts with its pending
    // items: one line on top, or nothing if there are none.
    let line = if role == "pm" {
        inbox_line(store, project)?
    } else {
        None
    };
    let head = line.map(|l| l + "\n\n").unwrap_or_default();
    let room = |max: usize| max.saturating_sub(head.chars().count());
    let text = match kind {
        "brief" => {
            let max = app::budget(project, role).min(INJECT_MAX);
            let epic = epic.as_deref().unwrap_or_default();
            cap(app::brief(store, project, epic, Some(max))?.view.text)
        }
        "inject" => {
            let max = room(app::budget(project, role).min(INJECT_MAX));
            let body = app::resume(store, project, role, Some(max))?.view.text;
            cap(head + &body)
        }
        _ => cap(head + &app::pointer(store, project, role)?),
    };
    Ok(StartContext {
        mode,
        text: Some(text),
        how: "full",
        save_error: save(&given),
    })
}

/// “omar has 4 pending items in kontext (oldest, from 3 days ago; the first
/// blocks Payments).”, for the first owner (or the human of this machine).
pub fn inbox_line(store: &mut Store, project: &Project) -> Result<Option<String>> {
    let st = app::state(store, project)?;
    let human = crate::inbox::default_human(&st)
        .unwrap_or_else(|| config::human_name_in(project, store.home()));
    let ib = crate::inbox::build(&st, &project.name, &human);
    Ok(crate::inbox::hook_line(&ib))
}

fn run_inner(
    event: &str,
    data: &Value,
    stdout: &mut dyn Write,
) -> Option<(PathBuf, Result<String>)> {
    let dir = data
        .get("cwd")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .filter(|d| d.is_dir())
        .or_else(|| std::env::current_dir().ok())?;
    let resolved = config::resolve_project_in(None, &dir).ok()?;
    let home = resolved.home.clone();
    let res = (|| -> Result<String> {
        let mut note = format!(
            "{event} session={} source={}",
            data.get("session_id")
                .and_then(Value::as_str)
                .unwrap_or("-"),
            data.get("source").and_then(Value::as_str).unwrap_or("-"),
        );
        if event == "pre-compact" {
            note.push_str(": ");
            note.push_str(&distill_hook(&home, &resolved.name, &dir, data)?);
            return Ok(note);
        }
        // `session-end` does both jobs, each only if it is on, and an error
        // in one does not stop the other.
        if event == "session-end" {
            let d = distill_hook(&home, &resolved.name, &dir, data)
                .unwrap_or_else(|e| format!("distill error: {e:#}"));
            let c = capture::hook(&home, &dir, event, data)
                .unwrap_or_else(|e| format!("capture error: {e:#}"));
            note.push_str(&format!(": {d}; {c}"));
            return Ok(note);
        }
        if matches!(event, "user-prompt-submit" | "stop") {
            note.push_str(": ");
            note.push_str(&capture::hook(&home, &dir, event, data)?);
            return Ok(note);
        }
        if event != "session-start" {
            note.push_str(": unknown event, nothing to do");
            return Ok(note);
        }
        let project = Project::load(&home)?;
        let mut store = Store::open(&home, &resolved.writer)?;
        let role = role_for(&dir);
        let start = Start {
            source: data.get("source").and_then(Value::as_str).unwrap_or(""),
            session_id: data.get("session_id").and_then(Value::as_str),
        };
        let cx = session_start_context(&mut store, &project, &role, start)?;
        note.push_str(&format!(" role={role} mode={} context={}", cx.mode, cx.how));
        if let Some(e) = &cx.save_error {
            note.push_str(&format!(
                " (what was given to the session was not saved: {e})"
            ));
        }
        if let Some(t) = cx.text {
            note.push_str(&format!(" chars={}", t.chars().count()));
            let out = json!({
                "hookSpecificOutput": {
                    "hookEventName": "SessionStart",
                    "additionalContext": t,
                }
            });
            writeln!(stdout, "{out}")?;
            stdout.flush()?;
        }
        Ok(note)
    })();
    Some((home, res))
}

/// `pre-compact` and `session-end`: start the distill of the transcript in the
/// background and return right away.
fn distill_hook(home: &Path, project_name: &str, dir: &Path, data: &Value) -> Result<String> {
    if std::env::var("KONTEXT_DISTILL_DISABLE").is_ok_and(|v| !v.trim().is_empty()) {
        return Ok("distill off because of KONTEXT_DISTILL_DISABLE".into());
    }
    let project = Project::load(home)?;
    if !distill::Settings::of(&project)?.enabled {
        return Ok("distill off ([distill] enabled = false)".into());
    }
    let Some(tp) = data
        .get("transcript_path")
        .and_then(Value::as_str)
        .filter(|t| !t.trim().is_empty())
    else {
        return Ok("no transcript_path, nothing to distill".into());
    };
    let tp = config::expand_tilde(tp)?;
    if !tp.is_file() {
        return Ok(format!("transcript not found: {}", tp.display()));
    }
    let pid = distill::spawn_background(home, project_name, dir, &tp)?;
    Ok(format!(
        "distilling {} in the background (pid {pid})",
        tp.display()
    ))
}

/// Runs the hook. Always returns without error.
pub fn run(event: &str, stdin: &mut dyn Read, stdout: &mut dyn Write) {
    let data = read_input(stdin);
    // A panic must not pollute the session: it is noted in hooks.log, not stderr.
    std::panic::set_hook(Box::new(|_| {}));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_inner(event, &data, stdout)
    }));
    match result {
        Ok(Some((home, Ok(note)))) => log_line(&home, &note),
        Ok(Some((home, Err(e)))) => log_line(&home, &format!("{event}: error: {e:#}")),
        // Without a project there is nowhere to note it: stay quiet.
        Ok(None) => {}
        Err(_) => {
            if let Ok(r) = config::resolve_project(None) {
                log_line(&r.home, &format!("{event}: error: panic in the hook"));
            }
        }
    }
}
