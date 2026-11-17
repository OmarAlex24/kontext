//! `kontext dispatch` (DESIGN §12.5): an open epic of a ready wave gets its
//! own sandbox, with a coordinator token for that epic, and a coordinator
//! agent in it. No model decides anything here: the checks, the state and
//! when a sandbox stops all come from the ledger and this machine's state.
//!
//! - [`dispatch`] checks (each refusal is exit 2 and one line), reserves the
//!   epic under the state lock, makes the token, makes the sandbox with the
//!   token, the agent's credential ([`credential`]), the prompts and the
//!   runner as files and its start script ([`script`]), waits for the
//!   script, starts the coordinator's first turn in the sandbox ([`turn`])
//!   and checks for a short while that it did not fail at once. Anything that
//!   fails after the token ends the turn, removes the credential, stops the
//!   sandbox and revokes the token.
//! - [`status`] and [`stop`].
//! - The ledger gets a journal entry when an epic is dispatched and when its
//!   sandbox stops (`fields.dispatch`: `start` or `end`), so `kontext tree`,
//!   the epic's brief and the pm see them.

pub mod credential;
pub mod guard;
pub mod provider;
pub mod script;
pub mod state;
pub mod turn;
pub mod watch;

use std::path::Path;

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::json;

use crate::config::{self, Project};
use crate::model::{Actor, Entry, RawEntry};
use crate::plan;
use crate::remote::{Link, Remote};
use crate::state::{State, Status};
use crate::store::{Refused, Store};
use crate::tasks::{self, epic_name, now_ms};

use guard::TmpFile;
use provider::{NewSandbox, Provider};
use state::{Run, RunState};

/// Who writes the dispatcher's entries in the ledger.
pub const ACTOR: &str = "dispatcher";

/// `[dispatch]` with its defaults.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Settings {
    pub service: String,
    pub max_sandboxes: usize,
    pub hours: u64,
    pub environment: Option<String>,
    pub provider: String,
    pub model: String,
    pub machine: String,
    pub repo_dir: Option<String>,
    /// Where the start script clones the code repo from when the service
    /// did not (the first `[[repos]]`' remote, as an https URL).
    pub repo_url: Option<String>,
    pub kontext_version: String,
}

/// The last part of a repo's remote or name: `github.com/acme/shop.git` → `shop`.
fn repo_folder(r: &config::Repo) -> String {
    let base = r.remote.as_deref().unwrap_or(&r.name);
    let last = base
        .trim_end_matches('/')
        .rsplit(['/', ':'])
        .next()
        .unwrap_or(base);
    last.strip_suffix(".git").unwrap_or(last).to_string()
}

impl Settings {
    pub fn of(p: &Project) -> Settings {
        let d = p.dispatch.clone().unwrap_or_default();
        let s = |v: Option<String>| v.map(|x| x.trim().to_string()).filter(|x| !x.is_empty());
        Settings {
            service: s(d.service).unwrap_or_else(|| "boat".into()),
            max_sandboxes: d.max_sandboxes.unwrap_or(3),
            hours: d.hours.unwrap_or(8),
            environment: s(d.environment),
            provider: s(d.provider).unwrap_or_else(|| "claude".into()),
            model: s(d.model).unwrap_or_else(|| "claude-opus-5-5".into()),
            machine: s(d.machine).unwrap_or_else(|| "default".into()),
            repo_dir: s(d.repo_dir).or_else(|| {
                p.repos
                    .first()
                    .map(|r| format!("/home/user/{}", repo_folder(r)))
            }),
            repo_url: p
                .repos
                .first()
                .and_then(|r| r.remote.as_deref())
                .filter(|r| !r.trim().is_empty())
                .map(script::clone_url),
            kontext_version: s(d.kontext_version)
                .unwrap_or_else(|| format!("v{}", env!("CARGO_PKG_VERSION"))),
        }
    }

    pub fn limit_ms(&self) -> i64 {
        self.hours as i64 * 3_600_000
    }
}

/// The project's `[dispatch]` and where it is on this machine.
pub fn config_of(project: &str) -> Result<(Settings, config::Resolved)> {
    let resolved = config::resolve_project(Some(project))?;
    let proj = Project::load(&resolved.home)?;
    Ok((Settings::of(&proj), resolved))
}

/// How long the start script may take.
pub(crate) const SETUP_LIMIT_MS: i64 = 20 * 60_000;

/// How long after `boat new` was asked a sandbox may still show up under
/// its name: longer than `boat new` may take (its limit and a minute more;
/// `KONTEXT_DISPATCH_NEW_GRACE_MS` in the tests).
pub(crate) fn new_grace_ms() -> i64 {
    std::env::var("KONTEXT_DISPATCH_NEW_GRACE_MS")
        .ok()
        .and_then(|s| s.trim().parse::<i64>().ok())
        .unwrap_or_else(|| provider::limit_of(&["new".into()]).as_millis() as i64 + 60_000)
}

/// The one line that says why a start script failed, from all it printed
/// (Boat's `setupError` is the script's whole stderr, installer chatter
/// included): its last `kontext-dispatch:` line, else its last line.
pub fn failure_line(text: &str) -> String {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    lines
        .iter()
        .rev()
        .find(|l| l.starts_with("kontext-dispatch:"))
        .or(lines.last())
        .map(|l| credential::redact(l))
        .unwrap_or_else(|| "the start script failed".into())
}

/// The start script failed: one line for the reason, and all it printed.
#[derive(Debug)]
pub struct SetupFailed {
    pub line: String,
    /// Everything the script printed, redacted.
    pub full: String,
}

impl std::fmt::Display for SetupFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the sandbox's start script failed: {}", self.line)
    }
}

impl std::error::Error for SetupFailed {}

impl SetupFailed {
    pub fn of(text: &str) -> SetupFailed {
        SetupFailed {
            line: failure_line(text),
            full: credential::redact(text.trim()),
        }
    }
}

/// How often the dispatcher asks the service (5 s; `KONTEXT_DISPATCH_POLL_MS`
/// in the tests).
pub fn poll() -> std::time::Duration {
    let ms = std::env::var("KONTEXT_DISPATCH_POLL_MS")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(5000);
    std::time::Duration::from_millis(ms)
}

fn refused<T>(msg: impl Into<String>) -> Result<T> {
    Err(Refused(msg.into()).into())
}

/// The project's server, or a refusal: a sandbox syncs through it.
fn server(project: &str) -> Result<Remote> {
    match Link::of(project)? {
        Link::Ready(r) => Ok(r),
        Link::NoToken(url) => refused(format!(
            "project {project} has server {url} but no token on this machine: dispatch needs \
             this machine's owner token"
        )),
        Link::None => refused(format!(
            "project {project} does not sync through a kontext server, and a sandbox can only \
             reach its memory through one (`kontext server connect <url>`)"
        )),
    }
}

/// What the checks found: the epic, and a warning for small work.
#[derive(Debug, Clone)]
pub struct Ready {
    pub epic: String,
    pub name: String,
    pub title: String,
    pub warning: Option<String>,
}

/// The ledger's checks: the plan is signed, the epic is open and in a wave
/// whose dependencies are all closed (as `kontext plan` computes it).
pub fn check_epic(st: &State, epic: &str) -> Result<Ready> {
    if !crate::scope::plan_signed(st) {
        return refused(
            "the plan is not signed: dispatch runs signed epics only (`kontext scope summary`, \
             then the owner signs it)",
        );
    }
    let id = match st.find_epic(epic) {
        Ok(id) => id,
        Err(e) => return refused(format!("{e}")),
    };
    let entry = st.get(&id).context("the epic is not in this copy")?;
    let name = epic_name(entry);
    if st.status_of(&id) != Some(Status::Active) {
        return refused(format!("epic {name} ({id}) is no longer in force"));
    }
    let layout = plan::of_state(st);
    if layout.closed.iter().any(|e| e.id == id) {
        return refused(format!("epic {name} is closed"));
    }
    if let Some(b) = layout.blocked.iter().find(|b| b.epic.id == id) {
        return refused(format!("epic {name} has no wave: it {}", b.reason));
    }
    let Some((wave, pe)) = layout
        .waves
        .iter()
        .find_map(|w| w.epics.iter().find(|e| e.id == id).map(|e| (w.n, e)))
    else {
        return refused(format!("epic {name} is in no wave of `kontext plan`"));
    };
    if !pe.after.is_empty() {
        return refused(format!(
            "epic {name} is in wave {wave}, which is not ready: it waits for {}",
            pe.after.join(", ")
        ));
    }
    let open = pe.tasks - pe.closed_tasks;
    let warning = (open == 1).then(|| {
        format!(
            "epic {name} has a single open task: small work does not need a sandbox (one agent \
             does it; DESIGN §12.1)"
        )
    });
    Ok(Ready {
        epic: id,
        name,
        title: entry.title.trim().to_string(),
        warning,
    })
}

/// A sandbox name that says project and epic: `kontext-<project>-<epic>`.
pub fn sandbox_name(project: &str, epic: &str) -> String {
    let clean = |s: &str| -> String {
        s.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c.to_ascii_lowercase()
                } else {
                    '-'
                }
            })
            .collect()
    };
    let mut n = format!("kontext-{}-{}", clean(project), clean(epic));
    n.truncate(120);
    n
}

/// The variables of the sandbox that are not secret.
pub fn sandbox_env(server: &str, project: &str, name: &str) -> Vec<(String, String)> {
    vec![
        ("KONTEXT_SERVER".into(), server.into()),
        ("KONTEXT_PROJECT".into(), project.into()),
        ("KONTEXT_ACTOR".into(), format!("agent:epic:{name}")),
        ("KONTEXT_ROLE".into(), "coordinator".into()),
        ("KONTEXT_EPIC".into(), name.into()),
    ]
}

/// Writes one of the dispatcher's entries in the ledger, as `agent:dispatcher`.
pub fn record(home: &Path, writer: &str, raw: RawEntry) -> Result<Entry> {
    let mut store = Store::open(home, writer)?;
    let ((), written) = store.transact(|b| {
        b.push(raw)?;
        Ok(())
    })?;
    written.into_iter().next().context("nothing was written")
}

fn journal(run: &Run, title: String, phase: &str) -> RawEntry {
    let mut r = RawEntry::new("journal", &title);
    r.actor = Some(Actor::agent(ACTOR));
    r.tags = vec!["dispatch".into(), run.name.clone()];
    r.fields.insert("epic".into(), json!(run.epic));
    r.fields.insert("dispatch".into(), json!(phase));
    r.fields.insert("service".into(), json!(run.service));
    if let Some(s) = &run.sandbox {
        r.fields.insert("sandbox".into(), json!(s));
    }
    r
}

/// The journal entry of a dispatch.
pub fn start_entry(run: &Run, s: &Settings) -> RawEntry {
    let title = format!(
        "Epic {} dispatched to a {} sandbox ({}): coordinator {} {}, {}h at most",
        run.name,
        run.service,
        run.sandbox.as_deref().unwrap_or("?"),
        s.provider,
        s.model,
        s.hours
    );
    journal(run, title, "start")
}

/// The journal entry of its end.
pub fn end_entry(run: &Run) -> RawEntry {
    let what = match run.state {
        RunState::Closed => "the epic closed".to_string(),
        RunState::Stopped => "stopped by the owner".to_string(),
        _ => format!(
            "stuck: {}",
            run.reason.as_deref().unwrap_or("no reason recorded")
        ),
    };
    let title = if run.forgotten {
        format!(
            "Epic {}: its run was forgotten by the owner, so kontext did not see its sandbox ({}) \
             stop; {what}",
            run.name,
            run.sandbox.as_deref().unwrap_or("?")
        )
    } else {
        format!(
            "Epic {}: its sandbox ({}) stopped, {what}",
            run.name,
            run.sandbox.as_deref().unwrap_or("?")
        )
    };
    let mut r = journal(run, title, "end");
    r.fields.insert("outcome".into(), json!(run.state.label()));
    if run.forgotten {
        r.fields.insert("forgotten".into(), json!(true));
    }
    r
}

/// What to tell the owner when the agent's credential could not be removed.
pub const REPLACE_CREDENTIAL: &str = "replace the agent's credential: `claude setup-token`, then \
     `kontext dispatch credential set`";

/// Ends the run's sandbox and revokes its token, as far as each is not done
/// yet: a sandbox `boat new` made without saying its id is looked up by
/// name, a turn still running is ended, the agent's credential is removed
/// from the sandbox (and the sandbox stopped anyway if that fails, which the
/// run records), the sandbox is stopped, the token revoked. Nothing runs in
/// a sandbox already stopped, and a sandbox the service says is not running
/// has nothing to end or remove. Returns what went wrong, to say (the run
/// keeps saying what is left; [`Run::clean`]).
pub fn clean_up(project: &str, p: &dyn Provider, run: &mut Run) -> Vec<String> {
    let mut failed = Vec::new();
    if run.sandbox.is_none()
        && let Some(asked) = run.sandbox_pending
    {
        match p.find(&run.sandbox_name) {
            Ok(Some(id)) => {
                run.sandbox = Some(id);
                run.sandbox_stopped = false;
                run.sandbox_pending = None;
            }
            // None shows up: nobody is still waiting on `boat new` for it
            // (this run's dispatch is at work and sees what it returns, or
            // more time passed than `boat new` may take).
            Ok(None)
                if guard::dispatching(project, &run.epic)
                    || state::wall_ms() - asked > new_grace_ms() =>
            {
                run.looked = Some(asked);
                run.sandbox_pending = None;
            }
            Ok(None) => failed.push(format!(
                "no sandbox named {} shows up yet, but Boat may still be making it; it is looked \
                 for again",
                run.sandbox_name
            )),
            Err(e) => failed.push(format!(
                "could not tell whether a sandbox named {} exists ({e:#}); the run stays open \
                 until Boat says",
                run.sandbox_name
            )),
        }
    }
    if let Some(id) = run.sandbox.clone()
        && !run.sandbox_stopped
    {
        // Boat refuses `exec` on a stopped sandbox (and does not wake it):
        // then there is nothing to end or remove in it.
        let mut down = false;
        if let Some(pid) = run.turn_pid {
            let ask = p.process(&id, pid);
            match ask {
                Ok(provider::Process::Exited(_) | provider::Process::Unknown) => {}
                Err(e) if provider::not_running(&e) => down = true,
                _ => match p.kill(&id, pid) {
                    Ok(()) => {}
                    Err(e) if provider::not_running(&e) => down = true,
                    Err(e) => failed.push(format!(
                        "could not end the coordinator's turn in {id} ({e:#}); stopping the \
                         sandbox ends it"
                    )),
                },
            }
        }
        if !down {
            match p.exec(&id, &script::remove_credential_command()) {
                Ok(o) if o.code == 0 => {}
                Err(e) if provider::not_running(&e) => down = true,
                Ok(o) => {
                    run.credential_left = true;
                    failed.push(format!(
                        "could not remove the agent's credential from {id} (exit code {}); \
                         stopping it anyway, so {REPLACE_CREDENTIAL}",
                        o.code
                    ));
                }
                Err(e) => {
                    run.credential_left = true;
                    failed.push(format!(
                        "could not remove the agent's credential from {id} ({e:#}); stopping it \
                         anyway, so {REPLACE_CREDENTIAL}"
                    ));
                }
            }
        }
        if down {
            failed.push(format!(
                "{id} was already stopped (by Boat's time limit or by hand), so nothing ran in it; \
                 a stop kontext did not make keeps the agent's credential in the sandbox's saved \
                 state"
            ));
        }
        match p.stop(&id) {
            Ok(()) => run.sandbox_stopped = true,
            Err(e) => failed.push(format!("{e:#}")),
        }
    }
    if let Some(t) = run.token_id.clone()
        && !run.token_revoked
    {
        match crate::remote::token_revoke(project, &t) {
            Ok(_) => run.token_revoked = true,
            Err(e) => failed.push(format!("could not revoke token {t}: {e:#}")),
        }
    }
    failed
}

/// What `dispatch` prints as it goes.
pub type Say<'a> = &'a mut dyn FnMut(&str);

/// `kontext dispatch <epic>`.
pub fn dispatch(project: &str, epic: &str, say: Say) -> Result<Run> {
    let remote = server(project)?;
    let resolved = config::resolve_project(Some(project))?;
    let home = resolved.home.clone();
    let proj = Project::load(&home)?;
    let settings = Settings::of(&proj);
    let prov = provider::of(&settings.service).map_err(|e| Refused(format!("{e:#}")))?;
    if settings.repo_dir.is_none() {
        return refused(
            "project.toml has no [[repos]]: say where the code repo's clone is in the sandbox with \
             [dispatch] repo_dir",
        );
    }
    if settings.provider != "claude" {
        return refused(format!(
            "[dispatch] provider = {:?}: the only coordinator agent is \"claude\" (Claude Code, \
             run headless in the sandbox)",
            settings.provider
        ));
    }
    let Some(cred) = credential::load()? else {
        return refused(credential::MISSING);
    };
    let ready = {
        let mut store = Store::open(&home, &resolved.writer)?;
        let st = store.state()?;
        check_epic(&st, epic)?
    };
    if let Some(w) = &ready.warning {
        say(&format!("warning: {w}"));
    }
    guard::sweep(project);

    // The reservation: the cap and "one sandbox per epic" hold even when
    // two dispatches run at once. This process holds the epic's dispatch
    // lock until it is done, so the watcher knows someone is at work.
    let now = now_ms();
    let (mut run, _working) = {
        let mut l = state::lock(project)?;
        if let Some(r) = l.runs.get(&ready.epic) {
            if r.state.live() {
                return refused(format!(
                    "epic {} already has a sandbox ({}, {})",
                    ready.name,
                    r.sandbox.as_deref().unwrap_or("being made"),
                    r.state.label()
                ));
            }
            if r.state == RunState::Overdue {
                return refused(format!(
                    "epic {}'s last sandbox ({}) ran out of time while the ledger could not be \
                     pulled; the watcher decides whether it closed or is stuck once it can (if \
                     `kontext dispatch status` says it is not running: `{}`)",
                    ready.name,
                    r.sandbox.as_deref().unwrap_or("?"),
                    watch::restart_command(project)
                ));
            }
            if !r.clean() {
                return refused(format!(
                    "epic {}'s last sandbox is not cleaned up yet ({}): `kontext dispatch stop {}` \
                     tries again",
                    ready.name,
                    left_text(r),
                    ready.name
                ));
            }
        }
        let billing = l.billing();
        if billing >= settings.max_sandboxes {
            return refused(format!(
                "{billing} sandboxes are running or not cleaned up yet, the most at once \
                 ([dispatch] max_sandboxes = {}); `kontext dispatch status` lists them",
                settings.max_sandboxes
            ));
        }
        let working = guard::DispatchLock::take(project, &ready.epic)
            .map_err(|e| Refused(format!("{e:#}")))?;
        let run = Run {
            id: config::random_hex(8)?,
            epic: ready.epic.clone(),
            name: ready.name.clone(),
            service: prov.service().into(),
            sandbox: None,
            sandbox_name: sandbox_name(project, &ready.name),
            session: None,
            turn_pid: None,
            token_id: None,
            state: RunState::Starting,
            started: now,
            updated: now,
            deadline: now + settings.limit_ms(),
            turns: 0,
            reason: None,
            sandbox_stopped: false,
            token_revoked: false,
            ended: None,
            pm_pending: false,
            end_pending: false,
            cleaning_until: None,
            sandbox_pending: None,
            credential_left: false,
            forgotten: false,
            looked: None,
        };
        l.runs.insert(ready.epic.clone(), run.clone());
        l.save()?;
        (run, working)
    };
    guard::on_signals();
    // The watcher follows the run from now on: if this process dies, it
    // finishes the start or ends the run.
    let watcher = watch::ensure(project);
    guard::checkpoint(project, "reserved");

    let started = start(
        project,
        &home,
        &remote,
        &settings,
        prov.as_ref(),
        &cred,
        &mut run,
        say,
    );
    let outcome = match started {
        Ok(()) => Ok(run),
        Err(e) if e.downcast_ref::<state::Gone>().is_some() => {
            let now = e.downcast::<state::Gone>().map(|g| g.0).ok();
            for l in give_up(project, prov.as_ref(), &run, now.as_ref()) {
                say(&format!("warning: {l}"));
            }
            Err(anyhow::anyhow!(
                "epic {} was {} while dispatch was starting it: dispatch stops here",
                run.name,
                now.as_ref().map(|r| r.state.label()).unwrap_or("replaced")
            ))
        }
        Err(e) => {
            // Whatever was made goes: the turn ends, the credential is
            // removed, the sandbox stops, the token is revoked. The run is
            // marked stuck (with the clean-up lease) first, so nobody else
            // acts on it meanwhile.
            let why = credential::redact(&format!("{e:#}"));
            let ended = now_ms();
            let claimed = state::update_own(project, &run, |r| {
                r.state = RunState::Stuck;
                r.reason = Some(format!("the dispatch failed: {why}"));
                r.ended = Some(ended);
                r.lease();
            });
            let left = match claimed {
                Ok(_) => {
                    let mut left = clean_up(project, prov.as_ref(), &mut run);
                    if let Err(se) = state::merge_clean_up(project, &run) {
                        left.push(format!("could not save the state: {se:#}"));
                    }
                    left
                }
                // Someone else ended it meanwhile.
                Err(g) => {
                    let now = g.downcast::<state::Gone>().map(|g| g.0).ok();
                    give_up(project, prov.as_ref(), &run, now.as_ref())
                }
            };
            for l in &left {
                say(&format!("warning: {l}"));
            }
            Err(e)
        }
    };
    if outcome.is_ok() {
        match watcher {
            Ok(true) => say(&format!(
                "watcher running (pid {}): it stops the sandbox when the epic closes",
                watch::pid(project)
                    .map(|p| p.to_string())
                    .unwrap_or_else(|| "?".into())
            )),
            Ok(false) => say(&format!(
                "warning: the watcher did not start; start it with `{}`",
                watch::restart_command(project)
            )),
            Err(e) => say(&format!(
                "warning: could not start the watcher ({e:#}); start it with `{}`",
                watch::restart_command(project)
            )),
        }
    }
    outcome
}

/// The run changed while `dispatch` worked on it (stopped, closed, stuck or
/// replaced): what this process made that the saved run does not know of
/// is added to it and cleaned up, and nothing else is touched.
fn give_up(project: &str, p: &dyn Provider, mine: &Run, now: Option<&Run>) -> Vec<String> {
    let ours = now.is_some_and(|r| r.id == mine.id);
    let mut todo = if ours {
        // What this process made goes into the saved run; whoever is
        // cleaning it up (or the watcher, later) takes it from there.
        let added = state::update_id(project, mine, |r| {
            if r.sandbox.is_none() && mine.sandbox.is_some() {
                r.sandbox.clone_from(&mine.sandbox);
                r.sandbox_stopped = false;
            }
            if r.token_id.is_none() && mine.token_id.is_some() {
                r.token_id.clone_from(&mine.token_id);
                r.token_revoked = false;
            }
            // `boat new` failed or was cut off without an id after someone
            // else looked: the sandbox may exist, so it is looked up again.
            // A new mark, so a clean-up that looked before does not clear it.
            if r.sandbox.is_none() && mine.sandbox.is_none() && mine.sandbox_pending.is_some() {
                r.sandbox_pending =
                    Some(state::wall_ms().max(mine.sandbox_pending.unwrap_or(0) + 1));
            }
        });
        match added.and_then(|_| state::lease_clean_up(project, mine)) {
            Ok(Some(r)) if !r.state.live() => r,
            _ => return Vec::new(),
        }
    } else {
        mine.clone()
    };
    if todo.clean() {
        return Vec::new();
    }
    let left = clean_up(project, p, &mut todo);
    if ours && let Err(e) = state::merge_clean_up(project, &todo) {
        return vec![format!("could not save the state: {e:#}")];
    }
    left
}

/// A turn started while someone stopped or ended the run (its start was
/// already on its way to the sandbox): it is ended and the sandbox stopped
/// again, as far as the service lets (both best effort; Boat refuses `exec`
/// on a stopped sandbox, and a second stop of a stopped one does nothing).
pub(crate) fn end_stray_turn(p: &dyn Provider, id: &str, pid: u32) {
    let _ = p.kill(id, pid);
    let _ = p.stop(id);
}

/// Has someone else changed the run (it is another one now, or not live)?
fn still_mine(project: &str, mine: &Run) -> Result<()> {
    match state::read(project)?.remove(&mine.epic) {
        Some(r) if r.id == mine.id && r.state.live() => Ok(()),
        Some(r) => Err(state::Gone(r).into()),
        None => Err(state::Gone(mine.clone()).into()),
    }
}

/// How long `dispatch` watches the first turn before it leaves it to the
/// watcher (15 s; `KONTEXT_DISPATCH_FIRST_CHECK_MS` in the tests).
fn first_check() -> std::time::Duration {
    let ms = std::env::var("KONTEXT_DISPATCH_FIRST_CHECK_MS")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(15_000);
    std::time::Duration::from_millis(ms)
}

/// From the reservation to a running coordinator. Every write re-reads the
/// run under the lock, stops with [`state::Gone`] if it is no longer this
/// one, and writes only the fields it changed.
#[allow(clippy::too_many_arguments)]
fn start(
    project: &str,
    home: &Path,
    remote: &Remote,
    s: &Settings,
    prov: &dyn Provider,
    cred: &credential::Credential,
    run: &mut Run,
    say: Say,
) -> Result<()> {
    // The token outlasts the epic's time limit by two hours.
    let minted = crate::remote::token_create(
        project,
        home,
        &crate::remote::TokenArgs {
            role: "coordinator".into(),
            epic: Some(run.epic.clone()),
            person: None,
            ttl: Some(format!("{}h", s.hours + 2)),
            writer: None,
        },
    )
    .context("could not make the coordinator token")?;
    run.token_id = Some(minted.id.clone());
    let token_id = minted.id.clone();
    state::update_own(project, run, |r| r.token_id = Some(token_id))?;
    say(&format!(
        "token {} made: coordinator of {}, writer {}",
        minted.id, run.name, minted.writer
    ));

    // Everything the sandbox gets as a file: the secrets, the prompts (the
    // service keeps every command line, so they never go in one) and the
    // runner. Each local copy goes when this function returns, or on a
    // signal.
    let dir = state::dir(project)?;
    let repo_dir = s.repo_dir.clone().unwrap_or_default();
    let token_file = TmpFile::write(&dir, "token", &minted.token)?;
    drop(minted);
    let cred_file = TmpFile::write(&dir, "credential", cred.value())?;
    let first = TmpFile::write(&dir, "prompt", &turn::first_prompt(project, run, s))?;
    let resume = TmpFile::write(&dir, "prompt", &turn::resume_prompt(run))?;
    let runner = TmpFile::write(
        &dir,
        "runner",
        &script::runner_script(&script::RunnerArgs {
            epic: run.name.clone(),
            repo_dir: repo_dir.clone(),
            model: s.model.clone(),
            credential_var: cred.kind.var(),
        }),
    )?;
    let start_script = TmpFile::write(
        &dir,
        "start",
        &script::start_script(&script::ScriptArgs {
            server: remote.url.clone(),
            project: project.into(),
            epic: run.name.clone(),
            version: s.kontext_version.clone(),
            repo_dir,
            repo_url: s.repo_url.clone(),
        }),
    )?;
    guard::checkpoint(project, "files");
    // From here a sandbox may exist before its id is known: the run says
    // so, and stays unsettled until the id is known or a look-up by name
    // says there is none.
    let asked = state::wall_ms();
    run.sandbox_pending = Some(asked);
    state::update_own(project, run, |r| r.sandbox_pending = Some(asked))?;
    let spec = NewSandbox {
        name: run.sandbox_name.clone(),
        environment: s.environment.clone(),
        machine: s.machine.clone(),
        // On for the time limit and an hour more; the watcher stops it.
        ttl_secs: (s.hours + 1) * 3600,
        env: sandbox_env(&remote.url, project, &run.name),
        files: vec![
            (script::TOKEN_PATH.into(), token_file.0.clone()),
            (script::CREDENTIAL_PATH.into(), cred_file.0.clone()),
            (script::RUNNER_PATH.into(), runner.0.clone()),
            (script::PROMPT_FIRST_PATH.into(), first.0.clone()),
            (script::PROMPT_RESUME_PATH.into(), resume.0.clone()),
        ],
        setup: Some(start_script.0.clone()),
    };
    let created = prov.create(&spec);
    drop((token_file, cred_file, first, resume, runner, start_script));
    let id = match created {
        Ok(id) => id,
        Err(f) => {
            if let Some(id) = f.id.clone() {
                run.sandbox = Some(id.clone());
                run.sandbox_pending = None;
                let _ = state::update_own(project, run, |r| {
                    r.sandbox = Some(id);
                    r.sandbox_pending = None;
                });
            }
            return Err(anyhow::Error::new(f));
        }
    };
    run.sandbox = Some(id.clone());
    run.sandbox_pending = None;
    let sid = id.clone();
    state::update_own(project, run, |r| {
        r.sandbox = Some(sid);
        r.sandbox_pending = None;
    })?;
    say(&format!(
        "sandbox {id} ({}) made with {}; waiting for its start script",
        run.sandbox_name, s.service
    ));

    // The start script runs in the background: wait for it, as long as the
    // run is still this one (a stop or the watcher may end it meanwhile).
    let until = now_ms() + SETUP_LIMIT_MS;
    loop {
        still_mine(project, run)?;
        match prov.setup(&id)? {
            provider::Setup::Done => break,
            provider::Setup::Failed(why) => {
                let f = SetupFailed::of(&why);
                say(&format!(
                    "the start script printed more: `boat info {id} --json` shows it (setupError)"
                ));
                return Err(anyhow::Error::new(f));
            }
            provider::Setup::Pending if now_ms() > until => anyhow::bail!(
                "the sandbox's start script did not finish in {} minutes",
                SETUP_LIMIT_MS / 60_000
            ),
            provider::Setup::Pending => std::thread::sleep(poll()),
        }
    }
    say("start script done: kontext, the project and the repo's clone are set up in the sandbox");

    // The coordinator's first turn: Claude Code, headless, in the sandbox.
    // The run stays `starting` until the turn's process id is saved, so a
    // watcher that finds it so (this process died) starts the turn again;
    // the runner never runs a turn twice.
    run.turns = 1;
    run.session = Some(turn::new_session()?);
    let session = run.session.clone();
    state::update_own(project, run, |r| {
        r.turns = 1;
        r.session = session;
        r.turn_pid = None;
    })?;
    let pid = turn::start(prov, run, 1)?;
    guard::checkpoint(project, "turn");
    run.turn_pid = Some(pid);
    run.state = RunState::Running;
    if let Err(e) = state::update_own(project, run, |r| {
        r.turn_pid = Some(pid);
        r.state = RunState::Running;
    }) {
        if e.downcast_ref::<state::Gone>().is_some() {
            end_stray_turn(prov, &id, pid);
        }
        return Err(e);
    }

    // A turn that cannot start (no login, no claude, no clone) fails within
    // seconds: then the dispatch fails, and everything goes.
    let until = std::time::Instant::now() + first_check();
    loop {
        match turn::state(prov, run) {
            turn::TurnState::Failed(why) => {
                anyhow::bail!("the coordinator did not start: {why}")
            }
            turn::TurnState::Ended | turn::TurnState::Lost(_) | turn::TurnState::Adopt(_) => {
                break;
            }
            turn::TurnState::Running | turn::TurnState::NoAnswer(_) => {
                if std::time::Instant::now() >= until {
                    break;
                }
                std::thread::sleep(poll());
            }
        }
    }
    say(&format!(
        "coordinator started: Claude Code {} in {id}, turn 1 (process {pid}), with every \
         permission inside the sandbox (`kontext brief {}`)",
        s.model, run.name
    ));
    let w = config::resolve_project(Some(project))?.writer;
    if let Err(e) = record(home, &w, start_entry(run, s)) {
        say(&format!(
            "warning: could not write the dispatch's journal entry ({e:#}); the sandbox runs"
        ));
    }
    Ok(())
}

// ------------------------------------------------------------ status

#[derive(Serialize, Debug, Clone)]
pub struct Progress {
    pub done: usize,
    pub total: usize,
    /// `features` or `tasks`.
    pub of: &'static str,
}

#[derive(Serialize, Debug, Clone)]
pub struct Row {
    #[serde(flatten)]
    pub run: Run,
    /// Done and total features (or tasks, for an epic without features), as
    /// `kontext tree` counts them.
    pub progress: Option<Progress>,
    pub closed_in_ledger: bool,
}

#[derive(Serialize, Debug, Clone)]
pub struct StatusReport {
    pub project: String,
    pub settings: Settings,
    pub live: usize,
    /// The watcher's pid, if it runs on this machine.
    pub watcher: Option<u32>,
    /// Which agent credential this machine has (never the credential).
    pub credential: Option<credential::Kind>,
    pub runs: Vec<Row>,
}

/// `kontext dispatch status`.
pub fn status(project: &str) -> Result<StatusReport> {
    let resolved = config::resolve_project(Some(project))?;
    let proj = Project::load(&resolved.home)?;
    let settings = Settings::of(&proj);
    let runs = state::read(project)?;
    let mut store = Store::open(&resolved.home, &resolved.writer)?;
    let st = store.state()?;
    let mut rows: Vec<Row> = runs
        .into_values()
        .map(|run| {
            let node = st.get(&run.epic).map(|e| plan::node(&st, e));
            let progress = node.as_ref().map(|n| {
                if n.features > 0 {
                    Progress {
                        done: n.done_features,
                        total: n.features,
                        of: "features",
                    }
                } else {
                    Progress {
                        done: n.closed_tasks,
                        total: n.tasks,
                        of: "tasks",
                    }
                }
            });
            Row {
                closed_in_ledger: node.as_ref().is_some_and(plan::Node::closed),
                progress,
                run,
            }
        })
        .collect();
    // Live ones first, then by when they started.
    rows.sort_by_key(|r| (!r.run.state.live(), r.run.started));
    Ok(StatusReport {
        project: project.into(),
        live: rows.iter().filter(|r| r.run.state.live()).count(),
        watcher: watch::pid(project).or_else(|| watch::alive(project).then_some(0)),
        credential: credential::kind(),
        settings,
        runs: rows,
    })
}

impl StatusReport {
    pub fn text(&self) -> String {
        let mut l = vec![format!(
            "Dispatch of {}: {} of {} sandboxes running ({} with {} {}, {}h per epic)",
            self.project,
            self.live,
            self.settings.max_sandboxes,
            self.settings.service,
            self.settings.provider,
            self.settings.model,
            self.settings.hours
        )];
        match self.credential {
            Some(k) => l.push(format!("Agent credential: set ({}).", k.label())),
            None => l.push(format!("Agent credential: not set. {}.", {
                let m = credential::MISSING;
                let mut c = m.chars();
                c.next()
                    .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                    .unwrap_or_default()
            })),
        }
        if self.runs.is_empty() {
            l.push("No epic was dispatched from this machine.".into());
        }
        match self.watcher {
            Some(p) => l.push(format!("The watcher runs (pid {p}).")),
            None if self.runs.iter().any(|r| !r.run.settled()) => l.push(format!(
                "The watcher is not running: the sandboxes keep working, but nothing stops them \
                 when their epic closes, resumes them, marks them stuck, decides an overdue one \
                 or finishes a clean-up. Start it again with `{}`; it picks up from the saved \
                 state.",
                watch::restart_command(&self.project)
            )),
            None => {}
        }
        let now = now_ms();
        for r in &self.runs {
            let run = &r.run;
            let progress = r
                .progress
                .as_ref()
                .map(|p| format!("{}/{} {}", p.done, p.total, p.of))
                .unwrap_or_else(|| "not in this copy".into());
            let since = if run.state.live() {
                format!("since {}", tasks::ago(now, run.started))
            } else {
                format!(
                    "after {}",
                    tasks::duration_text(run.ended.unwrap_or(run.updated) - run.started)
                )
            };
            l.push(format!(
                "- {}  {}  {}  {}  {progress}  {since}",
                run.name,
                run.state.label(),
                run.sandbox.as_deref().unwrap_or("(no sandbox)"),
                run.sandbox_name,
            ));
            if let Some(why) = &run.reason {
                l.push(format!("    {why}"));
            }
            if !run.state.live() && !run.clean() {
                l.push(format!(
                    "    not cleaned up yet ({}): `kontext dispatch stop {}` tries again",
                    left_text(run),
                    run.name
                ));
            }
            if run.credential_left {
                l.push(format!(
                    "    the agent's credential could not be removed from its sandbox before it \
                     stopped: {REPLACE_CREDENTIAL}"
                ));
            }
            if run.forgotten {
                l.push(
                    "    forgotten by the owner (`dispatch stop --forget`): kontext did not see \
                     its sandbox stop"
                        .into(),
                );
            }
        }
        l.join("\n") + "\n"
    }
}

fn left_text(run: &Run) -> String {
    let mut left = Vec::new();
    if run.sandbox.is_some() && !run.sandbox_stopped {
        left.push("the sandbox is not stopped".to_string());
    }
    if run.sandbox.is_none() && run.sandbox_pending.is_some() {
        left.push(format!(
            "boat new gave no id, and whether a sandbox named {} exists is not known yet",
            run.sandbox_name
        ));
    }
    if run.token_id.is_some() && !run.token_revoked {
        left.push("the token is not revoked".to_string());
    }
    left.join(", ")
}

// ------------------------------------------------------------ stop

/// The run of `epic` (an id, `epic:<name>` or a name) on this machine.
pub fn find_run<'a>(
    runs: &'a std::collections::BTreeMap<String, Run>,
    epic: &str,
) -> Option<&'a Run> {
    let e = epic.trim();
    let name = e.strip_prefix("epic:").unwrap_or(e);
    runs.get(e)
        .or_else(|| runs.values().find(|r| r.name == name))
        .or_else(|| {
            runs.values()
                .find(|r| e.len() >= 4 && r.epic.starts_with(e))
        })
}

/// `kontext dispatch stop <epic>`: ends its turn, removes the credential,
/// stops its sandbox and revokes its token. A run that already ended but
/// was not fully cleaned up is cleaned up again. The run is marked stopped
/// under the state lock first; the service is called without the lock.
pub fn stop(project: &str, epic: &str) -> Result<(Run, Vec<String>)> {
    let resolved = config::resolve_project(Some(project))?;
    guard::sweep(project);
    let claimed = {
        let mut l = state::lock(project)?;
        let Some(found) = find_run(&l.runs, epic).cloned() else {
            return refused(format!(
                "epic {epic} was not dispatched from this machine (`kontext dispatch status`)"
            ));
        };
        if !found.state.live() && found.clean() {
            return refused(format!(
                "epic {}'s sandbox already stopped ({})",
                found.name,
                found.state.label()
            ));
        }
        let mut run = found;
        if run.being_cleaned() {
            return refused(format!(
                "epic {}'s sandbox is being cleaned up right now; `kontext dispatch status` shows \
                 how it ends",
                run.name
            ));
        }
        if run.state.live() {
            run.state = RunState::Stopped;
            run.ended = Some(now_ms());
            run.end_pending = true;
        }
        run.lease();
        run.updated = now_ms();
        l.runs.insert(run.epic.clone(), run.clone());
        l.save()?;
        run
    };
    let prov = provider::of(&claimed.service)?;
    let mut run = claimed;
    let mut failed = clean_up(project, prov.as_ref(), &mut run);
    if let Some(saved) = state::merge_clean_up(project, &run)? {
        run = saved;
    }
    // The end entry says the sandbox stopped: only once it did.
    if run.end_pending && run.clean() {
        match record(&resolved.home, &resolved.writer, end_entry(&run)) {
            Ok(_) => {
                if let Some(saved) = state::update_id(project, &run, |r| r.end_pending = false)? {
                    run = saved;
                }
            }
            Err(e) => failed.push(format!(
                "could not write the end entry ({e:#}); the watcher tries again"
            )),
        }
    }
    Ok((run, failed))
}

/// `kontext dispatch stop <epic> --forget`: the owner's way out of a clean-up
/// that can never finish. Only for a run that already ended and is not
/// clean; the token's revoke is tried once more (it is kontext's own
/// server), then the run counts as clean and its end entry is written.
/// Returns the run and what kontext did not verify.
pub fn forget(project: &str, epic: &str) -> Result<(Run, Vec<String>)> {
    let resolved = config::resolve_project(Some(project))?;
    let mut run = {
        let mut l = state::lock(project)?;
        let Some(found) = find_run(&l.runs, epic).cloned() else {
            return refused(format!(
                "epic {epic} was not dispatched from this machine (`kontext dispatch status`)"
            ));
        };
        if found.state.live() {
            return refused(format!(
                "epic {}'s run is {}: `kontext dispatch stop {}` ends it; --forget is only for a \
                 run whose clean-up cannot finish",
                found.name,
                found.state.label(),
                found.name
            ));
        }
        if found.clean() {
            return refused(format!(
                "epic {}'s run is already cleaned up: there is nothing to forget",
                found.name
            ));
        }
        if found.being_cleaned() {
            return refused(format!(
                "epic {}'s sandbox is being cleaned up right now; `kontext dispatch status` shows \
                 how it ends",
                found.name
            ));
        }
        let mut run = found;
        run.lease();
        l.runs.insert(run.epic.clone(), run.clone());
        l.save()?;
        run
    };
    if let Some(t) = run.token_id.clone()
        && !run.token_revoked
        && crate::remote::token_revoke(project, &t).is_ok()
    {
        run.token_revoked = true;
    }
    let mut unverified = Vec::new();
    if let Some(id) = run.sandbox.as_deref().filter(|_| !run.sandbox_stopped) {
        unverified.push(format!(
            "that sandbox {id} stopped (`boat list --all` shows it; `boat stop {id}` stops it)"
        ));
    }
    if run.sandbox.is_none() && run.sandbox_pending.is_some() {
        unverified.push(format!(
            "whether a sandbox named {} exists (`boat list --all` shows it)",
            run.sandbox_name
        ));
    }
    if (run.sandbox.is_some() && !run.sandbox_stopped) || run.credential_left {
        unverified.push(format!(
            "that the agent's credential is out of it: to be sure, {REPLACE_CREDENTIAL}"
        ));
    }
    if let Some(t) = run.token_id.as_deref().filter(|_| !run.token_revoked) {
        unverified.push(format!(
            "that token {t} is revoked (`kontext token revoke {t}`)"
        ));
    }
    let revoked = run.token_revoked;
    let mut run = state::update_id(project, &run, |r| {
        r.forgotten = true;
        r.cleaning_until = None;
        r.token_revoked |= revoked;
        r.end_pending = true;
    })?
    .context("the run changed meanwhile")?;
    match record(&resolved.home, &resolved.writer, end_entry(&run)) {
        Ok(_) => {
            if let Some(saved) = state::update_id(project, &run, |r| r.end_pending = false)? {
                run = saved;
            }
        }
        Err(e) => unverified.push(format!(
            "the end entry in the ledger ({e:#}); the watcher writes it later"
        )),
    }
    Ok((run, unverified))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What real Boat's `setupError` looked like on 2026-11-16: the whole
    /// stderr of the script.
    #[test]
    fn a_start_script_failure_is_one_line() {
        let text = "Installing kontext v0.3.0…\ninstalled to /home/user/.local/bin\n\
            kontext: GET http://x/whoami: io: Connection refused\n\
            remote: Write access to repository not granted.\n\
            fatal: unable to access 'https://github.com/a/b/': 403\n\
            kontext-dispatch: could not clone https://github.com/a/b into /home/user/b\n";
        assert_eq!(
            failure_line(text),
            "kontext-dispatch: could not clone https://github.com/a/b into /home/user/b"
        );
        assert_eq!(failure_line("one\ntwo\n\n"), "two");
        assert_eq!(failure_line(""), "the start script failed");
        assert_eq!(
            failure_line("kontext-dispatch: key sk-ant-oat01-x"),
            "kontext-dispatch: key [redacted]"
        );
    }

    /// The review's finding 1: the watcher waits for a sandbox at least as
    /// long as `boat new` may take.
    #[test]
    fn the_new_grace_outlasts_boat_new() {
        if std::env::var("KONTEXT_DISPATCH_NEW_GRACE_MS").is_ok() {
            return;
        }
        assert!(new_grace_ms() > provider::limit_of(&["new".into()]).as_millis() as i64);
    }
}
