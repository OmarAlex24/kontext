//! The coordinator's turns. Each turn is held by its own process, `kontext
//! dispatch turn <epic> <n>` (hidden), started detached, so it outlives
//! whoever started it: it prompts the coordinator through the provider,
//! which returns when the turn ends, saves the conversation's id as soon as
//! the provider says it, and leaves how the turn ended in a file. Whoever
//! asks later, a restarted watcher included, reads the same answer from
//! [`turn_state`], the one place that decides whether a turn ended.
//!
//! Files in `<config>/dispatch/<project>/turns/`, per epic id and turn:
//! `<epic>-<n>.lock` (held while the turn runs), `<epic>-<n>.exit` (`ended`,
//! or `lost: <why>`), `<epic>-<n>.log` (what the provider printed), and
//! `<epic>-<n>.started` (when it was launched).

use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use anyhow::{Context, Result};

use super::provider::{self, Turn};
use super::state::{self, Run};
use super::{Settings, config_of};
use crate::config;
use crate::tasks::now_ms;

/// How long a launched turn may take to take its lock before it counts as lost.
const START_GRACE_MS: i64 = 30_000;

/// How a turn stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnState {
    /// Its process holds the turn open (or is just starting).
    Running,
    /// The provider said the turn ended.
    Ended,
    /// Nobody knows: the process died, or lost the turn (the network, the
    /// Mac went to sleep). Neither a resume nor stuck follow from this.
    Lost(String),
}

fn turns_dir(project: &str) -> Result<PathBuf> {
    Ok(state::dir(project)?.join("turns"))
}

fn file(dir: &Path, epic: &str, n: u32, ext: &str) -> PathBuf {
    dir.join(format!("{epic}-{n}.{ext}"))
}

/// The one function that says whether the coordinator's turn `n` ended.
pub fn turn_state(project: &str, epic: &str, n: u32) -> TurnState {
    let Ok(dir) = turns_dir(project) else {
        return TurnState::Lost("no dispatch folder".into());
    };
    if let Ok(s) = fs::read_to_string(file(&dir, epic, n, "exit")) {
        let s = s.trim();
        return if s == "ended" {
            TurnState::Ended
        } else {
            TurnState::Lost(s.strip_prefix("lost: ").unwrap_or(s).to_string())
        };
    }
    if let Ok(f) = OpenOptions::new()
        .write(true)
        .open(file(&dir, epic, n, "lock"))
    {
        if f.try_lock().is_err() {
            return TurnState::Running;
        }
        return TurnState::Lost("its process ended without saying how the turn went".into());
    }
    let started = fs::read_to_string(file(&dir, epic, n, "started"))
        .ok()
        .and_then(|s| s.trim().parse::<i64>().ok());
    match started {
        Some(t) if now_ms() - t < START_GRACE_MS => TurnState::Running,
        Some(_) => TurnState::Lost("its process never started".into()),
        None => TurnState::Lost("it was never launched".into()),
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
         Ask the pm, never the user: `kontext ask \"…\" --to pm`.",
        name = run.name
    )
}

/// The prompt after a turn that ended with the epic open.
pub fn resume_prompt(run: &Run) -> String {
    format!(
        "Epic {name} is still open in kontext. Continue from `kontext --actor agent:epic:{name} \
         --role coordinator brief {name}`.",
        name = run.name
    )
}

/// Starts turn `n` of the run in its own detached process. The caller saved
/// the run with `turns = n` first.
pub fn launch(project: &str, epic: &str, n: u32) -> Result<Child> {
    let dir = turns_dir(project)?;
    fs::create_dir_all(&dir)?;
    fs::write(file(&dir, epic, n, "started"), now_ms().to_string())?;
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(file(&dir, epic, n, "log"))?;
    let exe = std::env::current_exe().context("could not find this kontext binary")?;
    let mut c = Command::new(exe);
    c.args([
        "--project",
        project,
        "dispatch",
        "turn",
        epic,
        &n.to_string(),
    ])
    .stdin(Stdio::null())
    .stdout(log.try_clone()?)
    .stderr(log);
    detach(&mut c);
    c.spawn().context("could not start the turn's process")
}

/// Its own process group: a Ctrl-C in the terminal that started it does not
/// reach it.
pub(crate) fn detach(c: &mut Command) {
    use std::os::unix::process::CommandExt;
    c.process_group(0);
}

/// `kontext dispatch turn <epic> <n>`: holds the turn open until it ends.
pub fn hold(project: &str, epic: &str, n: u32) -> Result<()> {
    let dir = turns_dir(project)?;
    fs::create_dir_all(&dir)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(file(&dir, epic, n, "lock"))?;
    if lock.try_lock().is_err() {
        // Another process holds this turn.
        return Ok(());
    }
    let exit = file(&dir, epic, n, "exit");
    if exit.exists() {
        return Ok(());
    }
    let outcome = run_turn(project, epic, n);
    let line = match &outcome {
        Ok(()) => "ended".to_string(),
        Err(e) => format!("lost: {}", one_line(&format!("{e:#}"))),
    };
    config::write_atomic(&exit, format!("{line}\n").as_bytes())?;
    eprintln!("turn {n} of {epic}: {line}");
    drop(lock);
    Ok(())
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn run_turn(project: &str, epic: &str, n: u32) -> Result<()> {
    let run = state::read(project)?
        .remove(epic)
        .with_context(|| format!("no dispatch of {epic}"))?;
    let sandbox = run.sandbox.clone().context("the run has no sandbox")?;
    let (settings, _) = config_of(project)?;
    let prov = provider::of(&run.service)?;
    let text = if n <= 1 {
        first_prompt(project, &run, &settings)
    } else {
        resume_prompt(&run)
    };
    let conversation = if n <= 1 {
        None
    } else {
        Some(
            run.conversation
                .clone()
                .context("no conversation id to resume (boat prompt never gave one)")?,
        )
    };
    let turn = Turn {
        sandbox: &sandbox,
        conversation: conversation.as_deref(),
        provider: &settings.provider,
        model: &settings.model,
        text: &text,
    };
    let mut save = |c: &str| {
        let c = c.to_string();
        if let Err(e) = state::update(project, epic, |r| {
            if r.conversation.is_none() {
                r.conversation = Some(c);
            }
        }) {
            eprintln!("could not save the conversation id: {e:#}");
        }
    };
    prov.prompt(&turn, &mut save)
}
