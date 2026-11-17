//! The dispatcher's machine-local state: which epic runs in which sandbox,
//! the coordinator's session and its turn's process there, its token's id
//! and its times. It lives in the config
//! folder (`<config>/dispatch/<project>/`), never in the kontext-home or the
//! code repo, and is not synced: the ledger has the dispatch and its end
//! (journal entries), and this file has what only this machine needs to stop
//! the sandbox and revoke the token.
//!
//! `state.json` is read and written under `state.lock` (an exclusive file
//! lock), and written in one go, so a reader without the lock always sees a
//! whole file.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config;

/// Where a run stands.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    /// Being made: the token, the sandbox, its start script.
    Starting,
    /// The coordinator's first turn was started.
    Running,
    /// Its turn ended with the epic open and it was given one more.
    Resumed,
    /// It stopped again with the epic open, its time ran out, or the
    /// dispatch failed: the pm was told, the sandbox stopped.
    Stuck,
    /// The epic closed in the ledger: the sandbox stopped.
    Closed,
    /// `kontext dispatch stop`.
    Stopped,
    /// Its time ran out while the ledger could not be pulled: its sandbox is
    /// stopped, and the next fresh pull says whether it closed or is stuck.
    Overdue,
}

impl RunState {
    /// The sandbox should be running.
    pub fn live(self) -> bool {
        matches!(
            self,
            RunState::Starting | RunState::Running | RunState::Resumed
        )
    }

    pub fn label(self) -> &'static str {
        match self {
            RunState::Starting => "starting",
            RunState::Running => "running",
            RunState::Resumed => "resumed",
            RunState::Stuck => "stuck",
            RunState::Closed => "closed",
            RunState::Stopped => "stopped",
            RunState::Overdue => "overdue",
        }
    }
}

/// An epic's sandbox. The last one of each epic is kept.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Run {
    /// This run's own id (random), so whoever writes it can tell it is still
    /// the run it started and not a later one of the same epic.
    #[serde(default)]
    pub id: String,
    /// The epic's id.
    pub epic: String,
    pub name: String,
    /// The sandbox service (`boat`).
    pub service: String,
    pub sandbox: Option<String>,
    pub sandbox_name: String,
    /// The coordinator's Claude Code session id (a uuid the dispatcher
    /// picks: `--session-id` on the first turn, `--resume` after).
    #[serde(default)]
    pub session: Option<String>,
    /// The process id, in the sandbox, of the current turn (`turns`).
    #[serde(default)]
    pub turn_pid: Option<u32>,
    /// The coordinator token's id (`tok_…`), never the token.
    pub token_id: Option<String>,
    pub state: RunState,
    /// When it was dispatched, last changed and must be done by (ms).
    pub started: i64,
    pub updated: i64,
    pub deadline: i64,
    /// Turns the coordinator was given (the first and the resume).
    #[serde(default)]
    pub turns: u32,
    /// Why it is stuck, or what went wrong.
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub sandbox_stopped: bool,
    #[serde(default)]
    pub token_revoked: bool,
    #[serde(default)]
    pub ended: Option<i64>,
    /// It is stuck but the question to the pm is not written yet.
    #[serde(default)]
    pub pm_pending: bool,
    /// It ended but its end entry is not in the ledger yet.
    #[serde(default)]
    pub end_pending: bool,
    /// Someone is cleaning it up until then (wall-clock ms): nobody else
    /// does meanwhile, so a sandbox is never stopped twice or sent a command
    /// after it stopped. It expires, in case that process died.
    #[serde(default)]
    pub cleaning_until: Option<i64>,
}

impl Run {
    /// Its sandbox and token are gone, or there is nothing to clean up.
    pub fn clean(&self) -> bool {
        (self.sandbox.is_none() || self.sandbox_stopped)
            && (self.token_id.is_none() || self.token_revoked)
    }

    /// Its sandbox may still be running (or its token valid): it counts
    /// toward `max_sandboxes`.
    pub fn billing(&self) -> bool {
        self.state.live() || !self.clean()
    }

    /// The epic cannot be dispatched again yet.
    pub fn holds_epic(&self) -> bool {
        self.billing() || self.state == RunState::Overdue
    }

    /// Nothing is left to do for it: the watcher may leave.
    pub fn settled(&self) -> bool {
        !self.holds_epic() && !self.pm_pending && !self.end_pending
    }
}

/// `<config>/dispatch/<project>/`.
pub fn dir(project: &str) -> Result<PathBuf> {
    if !crate::cli::valid_name(project) {
        anyhow::bail!("invalid project name {project:?}");
    }
    Ok(config::config_dir()?.join("dispatch").join(project))
}

fn ensure_dir(project: &str) -> Result<PathBuf> {
    // Only inside a config folder that exists: a watcher whose config went
    // away (a test's folder, removed) must not make it again.
    let cfg = config::config_dir()?;
    if !cfg.is_dir() {
        anyhow::bail!("the kontext config folder {} does not exist", cfg.display());
    }
    let d = dir(project)?;
    fs::create_dir_all(&d).with_context(|| format!("could not create {}", d.display()))?;
    Ok(d)
}

fn state_path(d: &Path) -> PathBuf {
    d.join("state.json")
}

#[derive(Serialize, Deserialize, Default)]
struct StateFile {
    #[serde(default)]
    runs: BTreeMap<String, Run>,
}

fn load(d: &Path) -> Result<BTreeMap<String, Run>> {
    match fs::read_to_string(state_path(d)) {
        Ok(s) => Ok(serde_json::from_str::<StateFile>(&s)
            .with_context(|| format!("could not read {}", state_path(d).display()))?
            .runs),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(e) => Err(e.into()),
    }
}

/// The runs, without the lock (for reading).
pub fn read(project: &str) -> Result<BTreeMap<String, Run>> {
    load(&dir(project)?)
}

/// The state under its lock: change `runs`, then [`Locked::save`].
pub struct Locked {
    _lock: File,
    dir: PathBuf,
    pub runs: BTreeMap<String, Run>,
}

/// Takes the state's lock (waits for it) and reads the runs.
pub fn lock(project: &str) -> Result<Locked> {
    let d = ensure_dir(project)?;
    let f = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(d.join("state.lock"))?;
    f.lock().context("could not lock the dispatch state")?;
    let runs = load(&d)?;
    Ok(Locked {
        _lock: f,
        dir: d,
        runs,
    })
}

impl Locked {
    pub fn save(&self) -> Result<()> {
        let body = serde_json::to_string_pretty(&StateFile {
            runs: self.runs.clone(),
        })?;
        config::write_atomic(&state_path(&self.dir), body.as_bytes())
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The runs whose sandbox should be running.
    pub fn live(&self) -> usize {
        self.runs.values().filter(|r| r.state.live()).count()
    }

    /// The runs that count toward `max_sandboxes`: live, or not cleaned up.
    pub fn billing(&self) -> usize {
        self.runs.values().filter(|r| r.billing()).count()
    }
}

/// The run changed while someone worked on it: it was stopped, closed or
/// marked stuck, or replaced by a later run of the epic.
#[derive(Debug)]
pub struct Gone(pub Run);

impl std::fmt::Display for Gone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the run of epic {} changed while this worked on it (it is {} now)",
            self.0.name,
            self.0.state.label()
        )
    }
}

impl std::error::Error for Gone {}

/// Changes the run `mine` (by its id) under the lock, only while it is
/// still that run and live; `f` changes only what it means to. Otherwise a
/// [`Gone`] error with the run as it is now (or as it was, if it is gone).
pub fn update_own(project: &str, mine: &Run, f: impl FnOnce(&mut Run)) -> Result<Run> {
    let mut l = lock(project)?;
    let Some(run) = l.runs.get_mut(&mine.epic) else {
        return Err(Gone(mine.clone()).into());
    };
    if run.id != mine.id || !run.state.live() {
        return Err(Gone(run.clone()).into());
    }
    f(run);
    run.updated = crate::tasks::now_ms();
    let r = run.clone();
    l.save()?;
    Ok(r)
}

/// Changes one run under the lock and saves it.
pub fn update(project: &str, epic: &str, f: impl FnOnce(&mut Run)) -> Result<Run> {
    let mut l = lock(project)?;
    let run = l
        .runs
        .get_mut(epic)
        .with_context(|| format!("no dispatch of {epic} on this machine"))?;
    f(run);
    run.updated = crate::tasks::now_ms();
    let r = run.clone();
    l.save()?;
    Ok(r)
}

/// Changes the run `mine` (by its id) under the lock, live or not. `None`
/// if the epic's run is another one now.
pub fn update_id(project: &str, mine: &Run, f: impl FnOnce(&mut Run)) -> Result<Option<Run>> {
    let mut l = lock(project)?;
    let Some(run) = l.runs.get_mut(&mine.epic).filter(|r| r.id == mine.id) else {
        return Ok(None);
    };
    f(run);
    run.updated = crate::tasks::now_ms();
    let r = run.clone();
    l.save()?;
    Ok(Some(r))
}

/// The wall clock in ms (not `KONTEXT_NOW`: leases are between processes).
fn wall_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// How long a clean-up lease lasts: longer than a `boat stop` may take.
const LEASE_MS: i64 = 6 * 60_000;

impl Run {
    /// Someone else is cleaning it up right now.
    pub fn being_cleaned(&self) -> bool {
        self.cleaning_until.is_some_and(|t| t > wall_ms())
    }

    /// Takes the clean-up lease (call under the lock, then save).
    pub fn lease(&mut self) {
        self.cleaning_until = Some(wall_ms() + LEASE_MS);
    }
}

/// Takes the lease to clean up the run `mine` (by its id): the run as saved
/// when it is not clean and nobody else is cleaning it, else `None`.
pub fn lease_clean_up(project: &str, mine: &Run) -> Result<Option<Run>> {
    let mut l = lock(project)?;
    let Some(run) = l.runs.get_mut(&mine.epic).filter(|r| r.id == mine.id) else {
        return Ok(None);
    };
    if run.clean() || run.being_cleaned() {
        return Ok(None);
    }
    run.lease();
    let r = run.clone();
    l.save()?;
    Ok(Some(r))
}

/// Saves what a clean-up of `done` did (it ran without the lock): a sandbox
/// it stopped, a token it revoked. It only ever adds what was done, and
/// lets the clean-up lease go.
pub fn merge_clean_up(project: &str, done: &Run) -> Result<Option<Run>> {
    update_id(project, done, |r| {
        r.cleaning_until = None;
        if r.sandbox.is_none() {
            r.sandbox.clone_from(&done.sandbox);
        }
        if done.sandbox_stopped && r.sandbox == done.sandbox {
            r.sandbox_stopped = true;
        }
        if r.token_id.is_none() {
            r.token_id.clone_from(&done.token_id);
        }
        if done.token_revoked && r.token_id == done.token_id {
            r.token_revoked = true;
        }
    })
}
