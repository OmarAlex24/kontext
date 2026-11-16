//! The dispatcher's machine-local state: which epic runs in which sandbox,
//! its conversation, its token's id and its times. It lives in the config
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
    /// The coordinator was prompted.
    Running,
    /// Its turn ended with the epic open and it was prompted once more.
    Resumed,
    /// It stopped again with the epic open, its time ran out, or the
    /// dispatch failed: the pm was told, the sandbox stopped.
    Stuck,
    /// The epic closed in the ledger: the sandbox stopped.
    Closed,
    /// `kontext dispatch stop`.
    Stopped,
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
        }
    }
}

/// An epic's sandbox. The last one of each epic is kept.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Run {
    /// The epic's id.
    pub epic: String,
    pub name: String,
    /// The sandbox service (`boat`).
    pub service: String,
    pub sandbox: Option<String>,
    pub sandbox_name: String,
    pub conversation: Option<String>,
    /// The coordinator token's id (`tok_…`), never the token.
    pub token_id: Option<String>,
    pub state: RunState,
    /// When it was dispatched, last changed and must be done by (ms).
    pub started: i64,
    pub updated: i64,
    pub deadline: i64,
    /// Turns the coordinator was given (the first prompt and the resume).
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
}

impl Run {
    /// Its sandbox and token are gone, or there is nothing to clean up.
    pub fn clean(&self) -> bool {
        (self.sandbox.is_none() || self.sandbox_stopped)
            && (self.token_id.is_none() || self.token_revoked)
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
