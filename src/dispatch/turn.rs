//! The coordinator's turns. A turn is one process in the sandbox, the
//! runner ([`super::script::runner_script`]) started with the service's
//! detach (`boat exec --detach`), so nothing on the owner's machine holds it
//! open. The run's state keeps the turn's process id; whoever asks (the
//! watcher, a restarted watcher, `dispatch` itself) asks the service how
//! that process stands and, once it exited, reads what the runner left.
//! [`state`] is the one place that decides how a turn went.

use anyhow::{Context, Result};
use serde_json::Value;

use super::Settings;
use super::credential::redact;
use super::provider::{Process, Provider};
use super::script::{ALREADY_STARTED, RUNNER_PATH};
use super::state::Run;
use crate::config;

/// How a turn stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnState {
    /// Its process runs.
    Running,
    /// Claude Code said the turn ended without an error: the ledger decides
    /// what follows.
    Ended,
    /// Claude Code said it failed, or the turn left no result or a non-zero
    /// exit code. Another turn would fail the same way.
    Failed(String),
    /// The service does not know the process: the turn cannot be followed.
    /// Neither a resume nor stuck follow from this.
    Lost(String),
    /// The service could not be asked, or what the turn left could not be
    /// read this time. Ask again later.
    NoAnswer(String),
}

/// A new session id for Claude Code (a random uuid, version 4).
pub fn new_session() -> Result<String> {
    let h = config::random_hex(16)?;
    let variant = u8::from_str_radix(&h[16..17], 16).context("bad random hex")? & 0x3 | 0x8;
    Ok(format!(
        "{}-{}-4{}-{:x}{}-{}",
        &h[0..8],
        &h[8..12],
        &h[13..16],
        variant,
        &h[17..20],
        &h[20..32]
    ))
}

/// The command that starts turn `n` in the background: a login shell, which
/// has `claude` on its `PATH`, running the runner. It carries no secret and
/// no prompt text (the service keeps every command line).
pub fn start_command(n: u32, session: &str) -> Vec<String> {
    vec![
        "bash".into(),
        "-lc".into(),
        format!("exec sh \"$HOME/{RUNNER_PATH}\" start {n} {session}"),
    ]
}

/// The command that prints what turn `n` left.
pub fn read_command(n: u32) -> Vec<String> {
    vec![
        "sh".into(),
        "-c".into(),
        format!("exec sh \"$HOME/{RUNNER_PATH}\" read {n}"),
    ]
}

/// Starts turn `n` of the run and returns its process id in the sandbox.
pub fn start(prov: &dyn Provider, run: &Run, n: u32) -> Result<u32> {
    let sandbox = run.sandbox.as_deref().context("the run has no sandbox")?;
    let session = run
        .session
        .as_deref()
        .context("the run has no Claude Code session id")?;
    prov.detach(sandbox, &start_command(n, session))
        .with_context(|| format!("could not start the coordinator's turn {n}"))
}

/// What `read` printed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Summary {
    pub pid: Option<u32>,
    /// The runner's exit code, once it wrote one.
    pub exit: Option<i32>,
    /// `"is_error":…` as grep found it in the result.
    pub flag: Option<bool>,
    /// The result, when what was read of it is whole JSON.
    pub result: Option<Value>,
    pub stderr: String,
}

pub fn parse_summary(out: &str) -> Summary {
    let mut s = Summary::default();
    let (head, rest) = out.split_once("--- result\n").unwrap_or((out, ""));
    let (result, stderr) = rest.split_once("--- stderr\n").unwrap_or((rest, ""));
    for l in head.lines() {
        if let Some(v) = l.strip_prefix("pid=") {
            s.pid = v.trim().parse().ok();
        } else if let Some(v) = l.strip_prefix("exit=") {
            s.exit = v.trim().parse().ok();
        } else if let Some(v) = l.strip_prefix("flag=") {
            s.flag = match v.trim() {
                "\"is_error\":true" => Some(true),
                "\"is_error\":false" => Some(false),
                _ => None,
            };
        }
    }
    s.result = serde_json::from_str::<Value>(result.trim())
        .ok()
        .filter(Value::is_object);
    s.stderr = stderr.trim().to_string();
    s
}

fn one_line(s: &str, max: usize) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() <= max {
        return s;
    }
    let cut: String = s.chars().take(max).collect();
    format!("{cut}…")
}

/// How a turn whose process exited with `code` went, from what it left.
pub fn verdict(s: &Summary, code: i32) -> TurnState {
    let Some(exit) = s.exit else {
        return TurnState::Failed(format!(
            "its process ended (exit code {code}) without saying how the turn went"
        ));
    };
    let is_error = s
        .result
        .as_ref()
        .and_then(|v| v.get("is_error"))
        .and_then(Value::as_bool)
        .or(s.flag);
    if exit == 0 && is_error == Some(false) {
        return TurnState::Ended;
    }
    let said = s
        .result
        .as_ref()
        .filter(|_| is_error == Some(true))
        .and_then(|v| v.get("result"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let why = match (said, is_error) {
        (Some(r), _) if !r.trim().is_empty() => format!("Claude Code said: {}", one_line(&r, 300)),
        _ if !s.stderr.is_empty() => one_line(&s.stderr, 300),
        (_, None) if exit == 0 => "it left no result that could be read".into(),
        _ => format!("claude exited with code {exit}"),
    };
    TurnState::Failed(redact(&why))
}

/// The one function that says how the run's current turn stands.
pub fn state(prov: &dyn Provider, run: &Run) -> TurnState {
    let Some(sandbox) = run.sandbox.as_deref() else {
        return TurnState::Lost("the run has no sandbox".into());
    };
    let Some(pid) = run.turn_pid else {
        return TurnState::Lost("its process id was never saved".into());
    };
    let code = match prov.process(sandbox, pid) {
        Err(e) => return TurnState::NoAnswer(redact(&format!("{e:#}"))),
        Ok(Process::Running) => return TurnState::Running,
        Ok(Process::Unknown) => {
            return TurnState::Lost(format!(
                "{} does not know process {pid} in {sandbox}",
                prov.service()
            ));
        }
        Ok(Process::Exited(c)) => c,
    };
    match prov.exec(sandbox, &read_command(run.turns)) {
        Err(e) => TurnState::NoAnswer(redact(&format!("could not read its result: {e:#}"))),
        Ok(o) if o.code != 0 => TurnState::NoAnswer(redact(&format!(
            "could not read its result (exit code {}): {}",
            o.code,
            one_line(&o.stderr, 200)
        ))),
        Ok(_) if code == ALREADY_STARTED => TurnState::Lost(format!(
            "turn {} was started twice; process {pid} did not run it",
            run.turns
        )),
        Ok(o) => verdict(&parse_summary(&o.stdout), code),
    }
}

/// The opening prompt: short, pointing at the brief and the coordinator's file.
pub fn first_prompt(project: &str, run: &Run, s: &Settings) -> String {
    let repo = s.repo_dir.as_deref().unwrap_or("the code repo");
    format!(
        "You are the coordinator of epic {name} of the kontext project {project}, in a sandbox of \
         your own. Work in {repo}. Follow .claude/agents/kontext-coordinator.md: start with \
         `kontext --actor agent:epic:{name} --role coordinator brief {name}` and run the epic \
         until kontext has it closed. Deliver its work as one stack of pull requests (gh stack). \
         Ask the pm, never the user: `kontext ask \"…\" --to pm`.\n",
        name = run.name
    )
}

/// The prompt after a turn that ended with the epic open.
pub fn resume_prompt(run: &Run) -> String {
    format!(
        "Epic {name} is still open in kontext. Continue from `kontext --actor agent:epic:{name} \
         --role coordinator brief {name}`.\n",
        name = run.name
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessions_are_uuids() {
        let s = new_session().unwrap();
        let parts: Vec<&str> = s.split('-').collect();
        assert_eq!(
            parts.iter().map(|p| p.len()).collect::<Vec<_>>(),
            [8, 4, 4, 4, 12]
        );
        assert!(parts[2].starts_with('4'), "{s}");
        assert!("89ab".contains(&parts[3][..1]), "{s}");
        assert_ne!(s, new_session().unwrap());
    }

    #[test]
    fn verdicts() {
        let ok = parse_summary(
            "pid=7\nexit=0\nflag=\"is_error\":false\n--- result\n{\"is_error\":false,\"result\":\"done\"}\n--- stderr\n\n",
        );
        assert_eq!(ok.pid, Some(7));
        assert_eq!(verdict(&ok, 0), TurnState::Ended);
        // Too long to read whole: the flag decides.
        let long = parse_summary(
            "pid=7\nexit=0\nflag=\"is_error\":false\n--- result\n{\"is_error\":false,\"res\n--- stderr\n\n",
        );
        assert!(long.result.is_none());
        assert_eq!(verdict(&long, 0), TurnState::Ended);
        // Not logged in (the shape claude printed in the probe).
        let out = parse_summary(
            "pid=7\nexit=1\nflag=\"is_error\":true\n--- result\n{\"is_error\":true,\"result\":\"Not logged in · Please run /login\",\"api_error_status\":null}\n--- stderr\n\n",
        );
        assert_eq!(
            verdict(&out, 1),
            TurnState::Failed("Claude Code said: Not logged in · Please run /login".into())
        );
        // A runner failure, with its message.
        let gone = parse_summary(
            "pid=7\nexit=127\nflag=\n--- result\n\n--- stderr\nkontext-dispatch: claude is not on the PATH\n",
        );
        assert_eq!(
            verdict(&gone, 127),
            TurnState::Failed("kontext-dispatch: claude is not on the PATH".into())
        );
        // Killed: no exit code written.
        let killed = parse_summary("pid=7\nexit=\nflag=\n--- result\n\n--- stderr\n\n");
        assert!(matches!(verdict(&killed, 143), TurnState::Failed(w) if w.contains("143")));
        // Exit 0 but nothing readable.
        let empty = parse_summary("pid=7\nexit=0\nflag=\n--- result\n\n--- stderr\n\n");
        assert!(matches!(verdict(&empty, 0), TurnState::Failed(w) if w.contains("no result")));
        // A credential in the output never goes further.
        let leak = parse_summary(
            "pid=7\nexit=1\nflag=\n--- result\n\n--- stderr\nbad key sk-ant-oat01-secret\n",
        );
        assert!(matches!(verdict(&leak, 1), TurnState::Failed(w) if !w.contains("secret")));
    }
}
