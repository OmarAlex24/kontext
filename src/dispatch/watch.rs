//! The watcher: a process on the owner's machine (`kontext dispatch watch`,
//! started detached by `dispatch`) that follows every live sandbox of the
//! project and decides by the ledger, never by what the agent says:
//!
//! - **The epic closed** (as `kontext plan` computes it, after pulling from
//!   the server): stop the sandbox, revoke the token.
//! - **The coordinator's turn ended with the epic open**: prompt it once more
//!   in the same conversation; if that turn ends too with the epic open, it
//!   is stuck. A turn that could not be followed to its end ([`TurnState::Lost`])
//!   decides nothing, and neither does a turn whose end was seen while the
//!   pull failed: the ledger may already say the epic closed.
//! - **The time limit passed** (`[dispatch] hours`): stuck.
//! - **Stuck**: a question to the pm in the ledger, as `agent:dispatcher`,
//!   then stop the sandbox and revoke the token.
//!
//! One watcher per project and machine (`watcher.lock`). Everything it
//! needs is in the saved state and the turns' files, so a watcher started
//! again (after the Mac slept, or it died) picks up where the last one was;
//! the sandboxes keep working meanwhile. It exits when no sandbox is live,
//! deciding that under the state lock so a new dispatch is never missed.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use super::state::{self, Run, RunState};
use super::turn::{self, TurnState};
use super::{ACTOR, clean_up, config_of, end_entry, provider, record};
use crate::config::Project;
use crate::model::Actor;
use crate::plan;
use crate::remote::{self, Link, Options};
use crate::store::Store;
use crate::tasks::{ms_text, now_ms};

fn lock_path(project: &str) -> Result<PathBuf> {
    Ok(state::dir(project)?.join("watcher.lock"))
}

fn log_path(project: &str) -> Result<PathBuf> {
    Ok(state::dir(project)?.join("watcher.log"))
}

fn pid_path(project: &str) -> Result<PathBuf> {
    Ok(state::dir(project)?.join("watcher.pid"))
}

/// Is a watcher running for the project on this machine? It holds the lock
/// while it lives (the lock goes with the process, however it ends).
pub fn alive(project: &str) -> bool {
    let Ok(p) = lock_path(project) else {
        return false;
    };
    match OpenOptions::new().write(true).open(p) {
        Ok(f) => f.try_lock().is_err(),
        Err(_) => false,
    }
}

/// The watcher's pid, while it runs.
pub fn pid(project: &str) -> Option<u32> {
    if !alive(project) {
        return None;
    }
    fs::read_to_string(pid_path(project).ok()?)
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// The command that starts it again.
pub fn restart_command(project: &str) -> String {
    format!("kontext --project {project} dispatch watch --detach")
}

/// Starts a watcher in the background unless one runs, and waits up to 5 s
/// for it to hold its lock. `Ok(false)` if it did not come up.
pub fn ensure(project: &str) -> Result<bool> {
    if alive(project) {
        return Ok(true);
    }
    let dir = state::dir(project)?;
    fs::create_dir_all(&dir)?;
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path(project)?)?;
    let exe = std::env::current_exe().context("could not find this kontext binary")?;
    let mut c = Command::new(exe);
    c.args(["--project", project, "dispatch", "watch"])
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    turn::detach(&mut c);
    let mut child = c.spawn().context("could not start the watcher")?;
    let until = Instant::now() + Duration::from_secs(5);
    while Instant::now() < until {
        if alive(project) {
            // It runs on its own; this process does not wait for it.
            std::thread::spawn(move || child.wait());
            return Ok(true);
        }
        if let Ok(Some(_)) = child.try_wait() {
            // It ended at once: another watcher took the lock, or it failed
            // (its log says why).
            return Ok(alive(project));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(false)
}

fn log(msg: &str) {
    println!("{} {msg}", ms_text(now_ms()));
    let _ = std::io::stdout().flush();
}

/// `kontext dispatch watch`: runs until no sandbox of the project is live.
pub fn run(project: &str) -> Result<()> {
    let dir = state::dir(project)?;
    fs::create_dir_all(&dir)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(lock_path(project)?)?;
    if lock.try_lock().is_err() {
        log(&format!("a watcher for {project} is already running"));
        return Ok(());
    }
    fs::write(pid_path(project)?, std::process::id().to_string())?;
    log(&format!("watching the sandboxes of {project}"));
    let mut w = Watcher {
        project: project.to_string(),
        lock: Some(lock),
        turns: Vec::new(),
        noted: BTreeSet::new(),
    };
    loop {
        if config_of(project).is_err() {
            log(&format!(
                "project {project} is no longer on this machine: the watcher stops"
            ));
            return Ok(());
        }
        if let Err(e) = w.tick() {
            log(&format!("error: {e:#}"));
        }
        if w.done()? {
            log("no sandbox is live: the watcher stops");
            return Ok(());
        }
        std::thread::sleep(super::poll());
    }
}

/// What to do with a live run.
#[derive(Debug, Clone, PartialEq)]
enum Action {
    Close,
    Resume,
    Stuck(String),
}

struct Watcher {
    project: String,
    lock: Option<File>,
    /// The turns' processes this watcher started (reaped as they end).
    turns: Vec<Child>,
    /// Lost turns already logged.
    noted: BTreeSet<(String, u32)>,
}

impl Watcher {
    /// No live run: let the lock go while holding the state's, so a
    /// dispatch that adds one after this sees no watcher and starts one.
    fn done(&mut self) -> Result<bool> {
        let l = state::lock(&self.project)?;
        if l.live() > 0 {
            return Ok(false);
        }
        self.lock.take();
        drop(l);
        Ok(true)
    }

    /// Pulls (and pushes) with the automatic sync's limits. `true` if the
    /// pull worked, so the ledger is as fresh as it can be.
    fn sync(&self, home: &std::path::Path) -> bool {
        let Ok(Some(remote)) = Link::of(&self.project).map(Link::remote) else {
            return false;
        };
        match remote::sync(home, &remote, &Options::auto()) {
            Ok(r) => {
                for e in &r.errors {
                    log(&format!("sync: {e}"));
                }
                r.errors.is_empty()
            }
            Err(e) => {
                log(&format!("sync failed: {e:#}"));
                false
            }
        }
    }

    fn tick(&mut self) -> Result<()> {
        self.turns.retain_mut(|c| matches!(c.try_wait(), Ok(None)));
        let (settings, resolved) = config_of(&self.project)?;
        let fresh = self.sync(&resolved.home);
        let runs = state::read(&self.project)?;
        let mut store = Store::open(&resolved.home, &resolved.writer)?;
        let st = store.state()?;
        let now = now_ms();
        let mut wrote = false;
        for run in runs.values() {
            if !run.state.live() {
                if !run.clean() {
                    self.retry_clean_up(run)?;
                }
                continue;
            }
            let closed = st
                .get(&run.epic)
                .is_some_and(|e| plan::node(&st, e).closed());
            let action = if closed {
                Some(Action::Close)
            } else if now > run.deadline {
                Some(Action::Stuck(format!(
                    "its time limit ({}h) passed",
                    settings.hours
                )))
            } else if run.turns == 0 {
                None
            } else {
                match turn::turn_state(&self.project, &run.epic, run.turns) {
                    TurnState::Ended if !fresh => {
                        log(&format!(
                            "{}: the turn ended but the pull failed; deciding on the next tick",
                            run.name
                        ));
                        None
                    }
                    TurnState::Ended if run.state == RunState::Running => Some(Action::Resume),
                    TurnState::Ended => Some(Action::Stuck(
                        "its coordinator stopped again with the epic open, after being resumed"
                            .into(),
                    )),
                    TurnState::Lost(why) => {
                        if self.noted.insert((run.epic.clone(), run.turns)) {
                            log(&format!(
                                "{}: turn {} could not be followed ({why}); only the ledger and \
                                 the time limit decide now",
                                run.name, run.turns
                            ));
                        }
                        None
                    }
                    TurnState::Running => None,
                }
            };
            if let Some(a) = action {
                wrote |= self.act(run, a)?;
            }
        }
        if wrote {
            self.sync(&resolved.home);
        }
        Ok(())
    }

    /// Applies an action under the state lock, if the run is still as it was
    /// read. `true` if it wrote to the ledger.
    fn act(&mut self, seen: &Run, action: Action) -> Result<bool> {
        let mut l = state::lock(&self.project)?;
        let Some(run) = l.runs.get(&seen.epic).cloned() else {
            return Ok(false);
        };
        if !run.state.live() || run.turns != seen.turns {
            return Ok(false);
        }
        let mut run = run;
        let prov = provider::of(&run.service)?;
        let (_, resolved) = config_of(&self.project)?;
        match action {
            Action::Resume => {
                run.turns += 1;
                run.state = RunState::Resumed;
                run.updated = now_ms();
                l.runs.insert(run.epic.clone(), run.clone());
                l.save()?;
                drop(l);
                log(&format!(
                    "{}: its turn ended with the epic open; resuming it once",
                    run.name
                ));
                self.turns
                    .push(turn::launch(&self.project, &run.epic, run.turns)?);
                Ok(false)
            }
            Action::Close | Action::Stuck(_) => {
                if let Action::Stuck(why) = &action {
                    run.state = RunState::Stuck;
                    run.reason = Some(why.clone());
                    let hint = run
                        .sandbox
                        .as_deref()
                        .map(|id| prov.inspect_hint(id))
                        .unwrap_or_else(|| "It has no sandbox".into());
                    self.tell_pm(&resolved, &run, hint)?;
                    log(&format!("{}: stuck, {why}", run.name));
                } else {
                    run.state = RunState::Closed;
                    log(&format!("{}: the epic closed in the ledger", run.name));
                }
                let failed = clean_up(&self.project, prov.as_ref(), &mut run);
                for f in &failed {
                    log(&format!("{}: {f}", run.name));
                }
                run.ended = Some(now_ms());
                run.updated = now_ms();
                l.runs.insert(run.epic.clone(), run.clone());
                l.save()?;
                drop(l);
                record(&resolved.home, &resolved.writer, end_entry(&run))?;
                Ok(true)
            }
        }
    }

    /// A question to the pm, as the dispatcher.
    fn tell_pm(&self, r: &crate::config::Resolved, run: &Run, hint: String) -> Result<()> {
        let proj = Project::load(&r.home)?;
        let mut store = Store::open(&r.home, &r.writer)?;
        let why = run.reason.as_deref().unwrap_or("no reason recorded");
        crate::write::ask(
            &mut store,
            &Actor::agent(ACTOR),
            &crate::write::AskArgs {
                title: format!("Epic {} is stuck in its sandbox: {why}", run.name),
                to: Some("pm".into()),
                body: format!(
                    "kontext dispatch stops its sandbox ({}) and revokes its token. {hint}. When \
                     it can go on, `kontext dispatch {}` gives it a new sandbox.",
                    run.sandbox.as_deref().unwrap_or("?"),
                    run.name
                ),
                epic: Some(run.epic.clone()),
                max_pending: proj.max_pending(),
                ..Default::default()
            },
        )?;
        Ok(())
    }

    /// A run that ended but whose sandbox or token is still there.
    fn retry_clean_up(&self, seen: &Run) -> Result<()> {
        let mut l = state::lock(&self.project)?;
        let Some(mut run) = l.runs.get(&seen.epic).cloned() else {
            return Ok(());
        };
        if run.state.live() || run.clean() {
            return Ok(());
        }
        let prov = provider::of(&run.service)?;
        let failed = clean_up(&self.project, prov.as_ref(), &mut run);
        if failed.is_empty() {
            log(&format!("{}: cleaned up", run.name));
        }
        run.updated = now_ms();
        l.runs.insert(run.epic.clone(), run);
        l.save()
    }
}
