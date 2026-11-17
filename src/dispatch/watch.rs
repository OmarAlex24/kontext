//! The watcher: a process on the owner's machine (`kontext dispatch watch`,
//! started detached by `dispatch` right after it reserves an epic) that
//! follows every run of the project and decides by the ledger, never by
//! what the agent says:
//!
//! - **The epic closed** (as `kontext plan` computes it, after pulling from
//!   the server): end the turn, remove the credential, stop the sandbox,
//!   revoke the token.
//! - **The coordinator's turn ended with the epic open**: one more turn in
//!   the same Claude Code session; if that turn ends too with the epic open,
//!   it is stuck. A turn whose end or failure was seen while the pull
//!   failed decides nothing yet: the ledger may already say the epic closed.
//! - **The coordinator's turn failed** (Claude Code said so, or it left no
//!   result): stuck, with the reason. Every failure counts as final, a
//!   transient one (an overloaded API, a usage limit) too.
//! - **A turn that cannot be followed** (the service cannot be asked, or
//!   does not know it and what it left cannot be read) decides nothing for
//!   10 minutes in a row; then it is stuck, with the reason.
//! - **The time limit passed** (`[dispatch] hours`): stuck, on a fresh
//!   pull. While the pull fails (for less than 30 minutes) the sandbox is
//!   stopped anyway and the run is `overdue` until a pull says closed or
//!   stuck.
//! - **Stuck**: stop the sandbox and revoke the token, then a new question
//!   to the pm in the ledger, as `agent:dispatcher`.
//! - **A `starting` run whose `dispatch` process is gone** (it holds a lock
//!   while it works): finish the start (find the sandbox by its name, wait
//!   for the start script, start the first turn), or end it as stuck when
//!   that cannot be done.
//!
//! The service is never called while the state lock is held, and one run's
//! error never keeps the others from their turn. What could not be done (a
//! clean-up, the pm's question, an end entry) is tried again on later
//! ticks. One watcher per project and machine (`watcher.lock`). Everything
//! it needs is in the saved state and the sandbox, so a watcher started
//! again (after the Mac slept, or it died) picks up where the last one was;
//! the sandboxes keep working meanwhile. It exits when nothing is left to
//! do, deciding that under the state lock so a new dispatch is never missed.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use super::state::{self, Run, RunState};
use super::turn::{self, TurnState};
use super::{ACTOR, clean_up, config_of, end_entry, new_grace_ms, provider, record};
use crate::config::{Machine, Project};
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
    pid_file(project)
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
    {
        // Its own process group: a Ctrl-C in the terminal that started it
        // does not reach it.
        use std::os::unix::process::CommandExt;
        c.process_group(0);
    }
    let mut child = c.spawn().context("could not start the watcher")?;
    let id = child.id();
    let until = Instant::now() + Duration::from_secs(5);
    while Instant::now() < until {
        if alive(project) {
            // It runs on its own; this process does not wait for it.
            std::thread::spawn(move || child.wait());
            return Ok(true);
        }
        if let Ok(Some(_)) = child.try_wait() {
            // It ended at once. It ran if it wrote its pid (it took the lock,
            // did all there was to do and found nothing live left); otherwise
            // another watcher took the lock, or it failed (its log says why).
            return Ok(alive(project) || pid_file(project) == Some(id));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(false)
}

/// What the pid file says, whether or not that watcher still runs.
fn pid_file(project: &str) -> Option<u32> {
    fs::read_to_string(pid_path(project).ok()?)
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn log(msg: &str) {
    println!("{} {msg}", ms_text(now_ms()));
    let _ = std::io::stdout().flush();
}

/// `kontext dispatch watch`: runs until nothing of the project's runs is
/// left to do.
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
        noted: BTreeSet::new(),
        pull_failing_since: None,
        retry: BTreeMap::new(),
        unfollowed: BTreeMap::new(),
    };
    loop {
        match registered(project) {
            Ok(true) => {}
            Ok(false) => {
                log(&format!(
                    "project {project} is no longer on this machine: the watcher stops"
                ));
                return Ok(());
            }
            // An unreadable projects.toml (being written, say) is tried
            // again: the sandboxes still need a watcher.
            Err(e) => log(&format!("error: {e:#}")),
        }
        if let Err(e) = w.tick() {
            log(&format!("error: {e:#}"));
        }
        match w.done() {
            Ok(true) => {
                log("nothing is live or left to clean up: the watcher stops");
                return Ok(());
            }
            Ok(false) => {}
            Err(e) => log(&format!("error: {e:#}")),
        }
        std::thread::sleep(super::poll());
    }
}

/// Is the project still registered on this machine? Only that ends the
/// watcher: a `project.toml` that cannot be read for a moment does not. A
/// config folder that is gone has no projects.
fn registered(project: &str) -> Result<bool> {
    Ok(Machine::load()?.projects.contains_key(project))
}

/// What to do with a live run.
#[derive(Debug, Clone, PartialEq)]
enum Action {
    Close,
    Stuck(String),
    /// Its time ran out while the ledger could not be pulled: stop it, and
    /// decide closed or stuck on a fresh pull.
    Overdue,
    /// Start (or start again) turn `n`; `Some(..)` on a resume.
    StartTurn(u32),
    Resume,
    /// Follow this process instead (the turn was started twice).
    Adopt(u32),
    /// The sandbox a killed dispatch made, found by its name.
    Found(String),
}

/// How long the pull may fail before a time limit marks a run stuck anyway
/// (30 minutes; `KONTEXT_DISPATCH_STALE_PULL_MS` in the tests).
fn stale_pull_limit() -> Duration {
    Duration::from_millis(
        std::env::var("KONTEXT_DISPATCH_STALE_PULL_MS")
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(30 * 60_000),
    )
}

/// How long a turn may go unfollowed (the service cannot be asked, or what
/// the turn left cannot be read) before the run is stuck (10 minutes;
/// `KONTEXT_DISPATCH_NO_ANSWER_MS` in the tests).
fn no_answer_limit() -> Duration {
    Duration::from_millis(
        std::env::var("KONTEXT_DISPATCH_NO_ANSWER_MS")
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(10 * 60_000),
    )
}

struct Watcher {
    project: String,
    lock: Option<File>,
    /// What was already logged about a run (epic, turn, what), so a turn
    /// that cannot be followed is said once.
    noted: BTreeSet<(String, u32, &'static str)>,
    /// Since when the pull fails.
    pull_failing_since: Option<Instant>,
    /// When a failed clean-up is tried next, and the wait after that.
    retry: BTreeMap<String, (Instant, Duration)>,
    /// Since when a run's turn (run key, turn) could not be followed.
    unfollowed: BTreeMap<(String, u32), Instant>,
}

impl Watcher {
    /// Nothing left to do for any run: let the lock go while holding the
    /// state's, so a dispatch that adds one after this sees no watcher and
    /// starts one.
    fn done(&mut self) -> Result<bool> {
        let l = state::lock(&self.project)?;
        if !l.runs.values().all(Run::settled) {
            return Ok(false);
        }
        self.lock.take();
        drop(l);
        Ok(true)
    }

    /// Pulls (and pushes) with the automatic sync's limits. `true` if the
    /// pull worked, so the ledger is as fresh as it can be.
    fn sync(&mut self, home: &std::path::Path) -> bool {
        let ok = match Link::of(&self.project).map(Link::remote) {
            Ok(Some(remote)) => match remote::sync(home, &remote, &Options::auto()) {
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
            },
            _ => false,
        };
        if ok {
            self.pull_failing_since = None;
        } else if self.pull_failing_since.is_none() {
            self.pull_failing_since = Some(Instant::now());
        }
        ok
    }

    /// The pull has been failing long enough to decide without it.
    fn stale_too_long(&self) -> bool {
        self.pull_failing_since
            .is_some_and(|t| t.elapsed() >= stale_pull_limit())
    }

    fn tick(&mut self) -> Result<()> {
        super::guard::sweep(&self.project);
        let (settings, resolved) = config_of(&self.project)?;
        let fresh = self.sync(&resolved.home);
        let runs = state::read(&self.project)?;
        let mut store = Store::open(&resolved.home, &resolved.writer)?;
        let st = store.state()?;
        let now = now_ms();
        let mut wrote = false;
        // One run's error never keeps the others from their turn.
        for run in runs.values() {
            let closed = st
                .get(&run.epic)
                .is_some_and(|e| plan::node(&st, e).closed());
            match self.step(run, closed, fresh, &settings, now) {
                Ok(w) => wrote |= w,
                Err(e) => log(&format!("{}: {e:#}", run.name)),
            }
        }
        if wrote {
            self.sync(&resolved.home);
        }
        Ok(())
    }

    /// One run's turn of the tick. `true` if it wrote to the ledger.
    fn step(
        &mut self,
        run: &Run,
        closed: bool,
        fresh: bool,
        settings: &super::Settings,
        now: i64,
    ) -> Result<bool> {
        if !run.state.live() {
            return self.after(run, closed, fresh);
        }
        let action = if run.id.is_empty() {
            // Written by a kontext from before run ids and turn processes:
            // its sandbox has no runner to follow, so it is ended.
            Some(Action::Stuck(
                "it was dispatched by an older kontext, which this one cannot follow; its sandbox \
                 was stopped"
                    .into(),
            ))
        } else if closed {
            Some(Action::Close)
        } else if now > run.deadline {
            let why = format!("its time limit ({}h) passed", settings.hours);
            if fresh || self.stale_too_long() {
                Some(Action::Stuck(why))
            } else {
                Some(Action::Overdue)
            }
        } else if run.state == RunState::Starting {
            if super::guard::dispatching(&self.project, &run.epic) {
                None
            } else {
                self.take_over(run, now)?
            }
        } else if run.turns > 0 && run.turn_pid.is_none() {
            // Its turn's process id was never saved (whoever started the
            // turn died first): start it again; the runner refuses a turn
            // it already runs, and that one is adopted.
            Some(Action::StartTurn(run.turns))
        } else if run.turns == 0 {
            None
        } else {
            let prov = provider::of(&run.service)?;
            self.follow(run, prov.as_ref(), fresh)
        };
        match action {
            Some(a) => self.act(run, a),
            None => Ok(false),
        }
    }

    /// What the current turn says.
    fn follow(&mut self, run: &Run, prov: &dyn provider::Provider, fresh: bool) -> Option<Action> {
        let key = (run.key(), run.turns);
        let ts = turn::state(prov, run);
        if !matches!(ts, TurnState::Lost(_) | TurnState::NoAnswer(_)) {
            self.unfollowed.remove(&key);
        }
        match ts {
            TurnState::Ended | TurnState::Failed(_) if !fresh => {
                log(&format!(
                    "{}: the turn ended but the pull failed; deciding on the next tick",
                    run.name
                ));
                None
            }
            TurnState::Ended if run.state == RunState::Running => Some(Action::Resume),
            TurnState::Ended => Some(Action::Stuck(
                "its coordinator stopped again with the epic open, after being resumed".into(),
            )),
            TurnState::Failed(why) => Some(Action::Stuck(format!(
                "its coordinator's turn {} failed: {why}",
                run.turns
            ))),
            TurnState::Adopt(pid) => Some(Action::Adopt(pid)),
            TurnState::Lost(why) | TurnState::NoAnswer(why) => {
                let since = *self.unfollowed.entry(key).or_insert_with(Instant::now);
                let limit = no_answer_limit();
                if since.elapsed() >= limit && fresh {
                    return Some(Action::Stuck(format!(
                        "for {} minutes kontext could not tell how its coordinator's turn {} \
                         stands: {why}",
                        (limit.as_secs() / 60).max(1),
                        run.turns
                    )));
                }
                if self.noted.insert((run.key(), run.turns, "no answer")) {
                    log(&format!(
                        "{}: could not tell how turn {} stands ({why}); asking again on the next \
                         ticks, and stuck if it lasts {} minutes",
                        run.name,
                        run.turns,
                        (limit.as_secs() / 60).max(1)
                    ));
                }
                None
            }
            TurnState::Running => {
                self.noted.remove(&(run.key(), run.turns, "no answer"));
                None
            }
        }
    }

    /// A `starting` run whose dispatch process is gone: finish the start,
    /// or end it when it cannot be finished.
    fn take_over(&mut self, run: &Run, now: i64) -> Result<Option<Action>> {
        if self.noted.insert((run.key(), 0, "take over")) {
            log(&format!(
                "{}: its dispatch stopped midway; the watcher takes the start over",
                run.name
            ));
        }
        let prov = provider::of(&run.service)?;
        let Some(id) = run.sandbox.as_deref() else {
            if run.token_id.is_none() {
                return Ok(Some(Action::Stuck(
                    "its dispatch stopped before it made anything".into(),
                )));
            }
            let Some(asked) = run.sandbox_pending else {
                return Ok(Some(Action::Stuck(
                    "its dispatch stopped before it asked for a sandbox".into(),
                )));
            };
            return Ok(match prov.find(&run.sandbox_name) {
                Ok(Some(id)) => Some(Action::Found(id)),
                // `boat new` may still be at work after its dispatch died.
                Ok(None) if state::wall_ms() - asked > new_grace_ms() => {
                    Some(Action::Stuck(format!(
                        "its dispatch stopped while the sandbox was being made, and no sandbox \
                         named {} is running",
                        run.sandbox_name
                    )))
                }
                Ok(None) => None,
                Err(e) => {
                    log(&format!(
                        "{}: could not look for its sandbox by name ({e:#})",
                        run.name
                    ));
                    None
                }
            });
        };
        if run.turns > 0 {
            return Ok(Some(Action::StartTurn(run.turns)));
        }
        Ok(match prov.setup(id) {
            Ok(provider::Setup::Done) => Some(Action::StartTurn(1)),
            Ok(provider::Setup::Failed(why)) => {
                let f = super::SetupFailed::of(&why);
                log(&format!(
                    "{}: its start script failed; all it printed:\n{}",
                    run.name, f.full
                ));
                Some(Action::Stuck(f.to_string()))
            }
            Ok(provider::Setup::Pending) if now > run.started + super::SETUP_LIMIT_MS => {
                Some(Action::Stuck(format!(
                    "the sandbox's start script did not finish in {} minutes",
                    super::SETUP_LIMIT_MS / 60_000
                )))
            }
            Ok(provider::Setup::Pending) => None,
            Err(e) => {
                log(&format!(
                    "{}: could not ask how its start script went ({e:#})",
                    run.name
                ));
                None
            }
        })
    }

    /// Applies an action. The run is re-read and changed under the state
    /// lock only while it is still as it was read; the service is called
    /// without the lock. `true` if it wrote to the ledger.
    fn act(&mut self, seen: &Run, action: Action) -> Result<bool> {
        let unchanged = |r: &Run| {
            r.id == seen.id
                && r.state == seen.state
                && r.turns == seen.turns
                && r.turn_pid == seen.turn_pid
                && r.sandbox == seen.sandbox
        };
        let prov = provider::of(&seen.service)?;
        match action {
            Action::Found(id) => {
                log(&format!(
                    "{}: found its sandbox {id} by name ({})",
                    seen.name, seen.sandbox_name
                ));
                state::update_own(&self.project, seen, |r| {
                    if r.sandbox.is_none() {
                        r.sandbox = Some(id);
                        r.sandbox_pending = None;
                    }
                })?;
                Ok(false)
            }
            Action::Adopt(pid) => {
                log(&format!(
                    "{}: turn {} already runs as process {pid}; following that one",
                    seen.name, seen.turns
                ));
                state::update_own(&self.project, seen, |r| {
                    if unchanged(r) {
                        r.turn_pid = Some(pid);
                    }
                })?;
                Ok(false)
            }
            Action::Resume => {
                let Some(run) = self.claim(seen, &unchanged, |r| {
                    r.turns += 1;
                    r.state = RunState::Resumed;
                    r.turn_pid = None;
                })?
                else {
                    return Ok(false);
                };
                log(&format!(
                    "{}: its turn ended with the epic open; resuming it once",
                    run.name
                ));
                self.start_turn(&run, prov.as_ref())
            }
            Action::StartTurn(n) => {
                let Some(run) = self.claim(seen, &unchanged, |r| {
                    r.turns = n;
                    if r.session.is_none() {
                        r.session = turn::new_session().ok();
                    }
                })?
                else {
                    return Ok(false);
                };
                log(&format!(
                    "{}: starting turn {n} of its coordinator",
                    run.name
                ));
                self.start_turn(&run, prov.as_ref())
            }
            Action::Close | Action::Stuck(_) | Action::Overdue => {
                let (state, reason) = match &action {
                    Action::Close => (RunState::Closed, None),
                    Action::Stuck(why) => (RunState::Stuck, Some(why.clone())),
                    _ => (RunState::Overdue, None),
                };
                let now = now_ms();
                let Some(mut run) = self.claim(seen, &unchanged, |r| {
                    r.state = state;
                    if reason.is_some() {
                        r.reason.clone_from(&reason);
                    }
                    r.ended = Some(now);
                    r.pm_pending = state == RunState::Stuck;
                    r.end_pending = state != RunState::Overdue;
                    r.lease();
                })?
                else {
                    return Ok(false);
                };
                match &action {
                    Action::Close => log(&format!("{}: the epic closed in the ledger", run.name)),
                    Action::Stuck(why) => log(&format!("{}: stuck, {why}", run.name)),
                    _ => log(&format!(
                        "{}: its time ran out while the ledger cannot be pulled; stopping its \
                         sandbox and deciding closed or stuck on a fresh pull",
                        run.name
                    )),
                }
                // The sandbox stops first, whatever happens to the ledger.
                for f in clean_up(&self.project, prov.as_ref(), &mut run) {
                    log(&format!("{}: {f}", run.name));
                }
                if let Some(saved) = state::merge_clean_up(&self.project, &run)? {
                    run = saved;
                }
                Ok(self.write_ledger(&run))
            }
        }
    }

    /// Changes the run under the lock if `unchanged` still holds and it is
    /// live. The run as saved, or `None`.
    fn claim(
        &self,
        seen: &Run,
        unchanged: &dyn Fn(&Run) -> bool,
        f: impl FnOnce(&mut Run),
    ) -> Result<Option<Run>> {
        let mut l = state::lock(&self.project)?;
        let Some(run) = l.runs.get_mut(&seen.epic) else {
            return Ok(None);
        };
        if !run.state.live() || !unchanged(run) {
            return Ok(None);
        }
        f(run);
        run.updated = now_ms();
        let r = run.clone();
        l.save()?;
        Ok(Some(r))
    }

    /// Starts the run's turn `turns` and saves its process id; a turn that
    /// cannot be started makes the run stuck.
    fn start_turn(&mut self, run: &Run, prov: &dyn provider::Provider) -> Result<bool> {
        let n = run.turns;
        match turn::start(prov, run, n) {
            Ok(pid) => {
                let saved = state::update_own(&self.project, run, |r| {
                    if r.turns == n {
                        r.turn_pid = Some(pid);
                        if r.state == RunState::Starting {
                            r.state = RunState::Running;
                        }
                    }
                });
                if let Err(e) = saved {
                    // Stopped or ended while the turn was on its way.
                    if e.downcast_ref::<state::Gone>().is_some()
                        && let Some(id) = run.sandbox.as_deref()
                    {
                        log(&format!(
                            "{}: it was ended while its turn {n} started; ending that turn and \
                             stopping {id} again",
                            run.name
                        ));
                        super::end_stray_turn(prov, id, pid);
                    }
                    return Err(e);
                }
                Ok(false)
            }
            Err(e) => {
                let why = super::credential::redact(&format!("{e:#}"));
                let now = state::read(&self.project)?.remove(&run.epic);
                match now {
                    Some(r) if r.id == run.id && r.state.live() => self.act(
                        &r,
                        Action::Stuck(format!("its coordinator could not be started: {why}")),
                    ),
                    _ => Ok(false),
                }
            }
        }
    }

    /// A run that is no longer live: what is still left of it (a question
    /// to the pm, its end entry, a clean-up, the verdict of an overdue one).
    fn after(&mut self, seen: &Run, closed: bool, fresh: bool) -> Result<bool> {
        let mut wrote = false;
        // Someone (a `stop`, a failed dispatch) is cleaning it up: its end
        // entry and question wait until they are done.
        if seen.being_cleaned() {
            return Ok(false);
        }
        if seen.state == RunState::Overdue && (fresh || self.stale_too_long()) {
            let hours = config_of(&self.project)?.0.hours;
            let why = format!("its time limit ({hours}h) passed");
            let decided = state::update_id(&self.project, seen, |r| {
                if r.state == RunState::Overdue {
                    if closed {
                        r.state = RunState::Closed;
                    } else {
                        r.state = RunState::Stuck;
                        r.reason = Some(why.clone());
                        r.pm_pending = true;
                    }
                    r.end_pending = true;
                }
            })?;
            if let Some(r) = decided {
                log(&format!(
                    "{}: {}",
                    r.name,
                    if closed {
                        "the epic closed in the ledger".to_string()
                    } else {
                        format!("stuck, {why}")
                    }
                ));
                wrote |= self.write_ledger(&r);
            }
            return Ok(wrote);
        }
        if seen.pm_pending || seen.end_pending {
            wrote |= self.write_ledger(seen);
        }
        if !seen.clean() {
            self.retry_clean_up(seen)?;
        }
        Ok(wrote)
    }

    /// Writes the run's pending question and end entry; what fails is tried
    /// again on the next tick. `true` if something was written.
    fn write_ledger(&mut self, run: &Run) -> bool {
        let Ok((_, resolved)) = config_of(&self.project) else {
            return false;
        };
        let mut wrote = false;
        if run.pm_pending {
            let prov = provider::of(&run.service);
            let hint = match (run.sandbox.as_deref(), prov) {
                (Some(id), Ok(p)) => p.inspect_hint(id, !run.credential_left),
                _ => "It has no sandbox".into(),
            };
            match self.tell_pm(&resolved, run, hint) {
                Ok(()) => {
                    wrote = true;
                    let _ = state::update_id(&self.project, run, |r| r.pm_pending = false);
                }
                Err(e) => log(&format!(
                    "{}: could not ask the pm ({e:#}); trying again on the next tick",
                    run.name
                )),
            }
        }
        // The end entry says the sandbox stopped: only once it did.
        if run.end_pending && run.clean() {
            match record(&resolved.home, &resolved.writer, end_entry(run)) {
                Ok(_) => {
                    wrote = true;
                    let _ = state::update_id(&self.project, run, |r| r.end_pending = false);
                }
                Err(e) => log(&format!(
                    "{}: could not write its end entry ({e:#}); trying again on the next tick",
                    run.name
                )),
            }
        }
        wrote
    }

    /// A new question to the pm, as the dispatcher (never merged into an
    /// older one: each stuck run has its own sandbox and reason).
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
                    "kontext dispatch stopped its sandbox ({}) and revoked its token. {hint}. When \
                     it can go on, `kontext dispatch {}` gives it a new sandbox.",
                    run.sandbox.as_deref().unwrap_or("?"),
                    run.name
                ),
                epic: Some(run.epic.clone()),
                max_pending: proj.max_pending(),
                no_merge: true,
                ..Default::default()
            },
        )?;
        Ok(())
    }

    /// A run that ended but whose sandbox or token is still there: tried
    /// again with a growing wait (5 s up to 5 minutes).
    fn retry_clean_up(&mut self, seen: &Run) -> Result<()> {
        let key = seen.key();
        if let Some((at, _)) = self.retry.get(&key)
            && Instant::now() < *at
        {
            return Ok(());
        }
        let prov = provider::of(&seen.service)?;
        let Some(mut run) = state::lease_clean_up(&self.project, seen)? else {
            return Ok(());
        };
        let failed = clean_up(&self.project, prov.as_ref(), &mut run);
        state::merge_clean_up(&self.project, &run)?;
        if run.clean() {
            for f in &failed {
                log(&format!("{}: {f}", run.name));
            }
            log(&format!("{}: cleaned up", run.name));
            self.retry.remove(&key);
        } else {
            for f in &failed {
                log(&format!("{}: {f}", run.name));
            }
            let wait = self
                .retry
                .get(&key)
                .map(|(_, w)| (*w * 2).min(Duration::from_secs(300)))
                .unwrap_or(super::poll());
            self.retry.insert(key, (Instant::now() + wait, wait));
        }
        Ok(())
    }
}
