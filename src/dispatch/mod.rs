//! `kontext dispatch` (DESIGN §12.5): an open epic of a ready wave gets its
//! own sandbox, with a coordinator token for that epic, and a coordinator
//! agent in it. No model decides anything here: the checks, the state and
//! when a sandbox stops all come from the ledger and this machine's state.
//!
//! - [`dispatch`] checks (each refusal is exit 2 and one line), reserves the
//!   epic under the state lock, makes the token, makes the sandbox with the
//!   token as a file and its start script ([`script`]), waits for the script,
//!   and prompts the coordinator in a process of its own ([`turn`]). Anything
//!   that fails after the token stops the sandbox and revokes the token.
//! - [`status`] and [`stop`].
//! - The ledger gets a journal entry when an epic is dispatched and when its
//!   sandbox stops (`fields.dispatch`: `start` or `end`), so `kontext tree`,
//!   the epic's brief and the pm see them.

pub mod provider;
pub mod script;
pub mod state;
pub mod turn;

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

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

use provider::{NewSandbox, Provider};
use state::{Run, RunState};

/// Who writes the dispatcher's entries in the ledger.
pub const ACTOR: &str = "dispatcher";
/// Where the token file goes in the sandbox (relative paths start in the
/// sandbox user's home).
pub const TOKEN_PATH: &str = ".kontext-dispatch-token";

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
            model: s(d.model).unwrap_or_else(|| "opus-5-5".into()),
            machine: s(d.machine).unwrap_or_else(|| "default".into()),
            repo_dir: s(d.repo_dir).or_else(|| {
                p.repos
                    .first()
                    .map(|r| format!("/home/user/{}", repo_folder(r)))
            }),
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
const SETUP_LIMIT_MS: i64 = 20 * 60_000;

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

/// A local file with the token, readable only by this user, removed when
/// dropped (on every path out of [`dispatch`]).
struct TokenFile(PathBuf);

impl TokenFile {
    fn write(dir: &Path, token: &str) -> Result<TokenFile> {
        use std::os::unix::fs::OpenOptionsExt;
        let tmp = dir.join("tmp");
        fs::create_dir_all(&tmp)?;
        let path = tmp.join(format!("token-{}", config::random_hex(8)?));
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .context("could not write the token file")?;
        let t = TokenFile(path);
        f.write_all(token.as_bytes())?;
        f.sync_all()?;
        Ok(t)
    }
}

impl Drop for TokenFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
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
    let mut r = journal(
        run,
        format!(
            "Epic {}: its sandbox ({}) stopped, {what}",
            run.name,
            run.sandbox.as_deref().unwrap_or("?")
        ),
        "end",
    );
    r.fields.insert("outcome".into(), json!(run.state.label()));
    r
}

/// Stops the run's sandbox and revokes its token, as far as each is not
/// done yet. Returns what failed (the run keeps saying what is left).
pub fn clean_up(project: &str, p: &dyn Provider, run: &mut Run) -> Vec<String> {
    let mut failed = Vec::new();
    if let Some(id) = run.sandbox.clone()
        && !run.sandbox_stopped
    {
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
    let ready = {
        let mut store = Store::open(&home, &resolved.writer)?;
        let st = store.state()?;
        check_epic(&st, epic)?
    };
    if let Some(w) = &ready.warning {
        say(&format!("warning: {w}"));
    }

    // The reservation: the cap and "one sandbox per epic" hold even when
    // two dispatches run at once.
    let now = now_ms();
    let mut run = {
        let mut l = state::lock(project)?;
        if let Some(r) = l.runs.get(&ready.epic).filter(|r| r.state.live()) {
            return refused(format!(
                "epic {} already has a sandbox ({}, {})",
                ready.name,
                r.sandbox.as_deref().unwrap_or("being made"),
                r.state.label()
            ));
        }
        let live = l.live();
        if live >= settings.max_sandboxes {
            return refused(format!(
                "{live} sandboxes are running, the most at once ([dispatch] max_sandboxes = {}); \
                 `kontext dispatch status` lists them",
                settings.max_sandboxes
            ));
        }
        let run = Run {
            epic: ready.epic.clone(),
            name: ready.name.clone(),
            service: prov.service().into(),
            sandbox: None,
            sandbox_name: sandbox_name(project, &ready.name),
            conversation: None,
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
        };
        l.runs.insert(ready.epic.clone(), run.clone());
        l.save()?;
        run
    };

    match start(
        project,
        &home,
        &remote,
        &settings,
        prov.as_ref(),
        &mut run,
        say,
    ) {
        Ok(()) => Ok(run),
        Err(e) => {
            // Whatever was made goes: the sandbox stops, the token is revoked.
            let mut left = clean_up(project, prov.as_ref(), &mut run);
            let why = format!("{e:#}");
            run.state = RunState::Stuck;
            run.reason = Some(format!("the dispatch failed: {why}"));
            run.ended = Some(now_ms());
            let saved = run.clone();
            if let Err(se) = state::update(project, &run.epic, |r| *r = saved) {
                left.push(format!("could not save the state: {se:#}"));
            }
            for l in &left {
                say(&format!("warning: {l}"));
            }
            Err(e)
        }
    }
}

/// From the reservation to a running coordinator.
fn start(
    project: &str,
    home: &Path,
    remote: &Remote,
    s: &Settings,
    prov: &dyn Provider,
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
    let snapshot = run.clone();
    state::update(project, &run.epic, |r| *r = snapshot)?;
    say(&format!(
        "token {} made: coordinator of {}, writer {}",
        minted.id, run.name, minted.writer
    ));

    let dir = state::dir(project)?;
    let token_file = TokenFile::write(&dir, &minted.token)?;
    drop(minted);
    let script = dir
        .join("tmp")
        .join(format!("start-{}.sh", config::random_hex(4)?));
    fs::write(
        &script,
        script::start_script(&script::ScriptArgs {
            server: remote.url.clone(),
            project: project.into(),
            epic: run.name.clone(),
            version: s.kontext_version.clone(),
            repo_dir: s.repo_dir.clone().unwrap_or_default(),
        }),
    )?;
    let spec = NewSandbox {
        name: run.sandbox_name.clone(),
        environment: s.environment.clone(),
        machine: s.machine.clone(),
        // On for the time limit and an hour more; the watcher stops it.
        ttl_secs: (s.hours + 1) * 3600,
        env: sandbox_env(&remote.url, project, &run.name),
        files: vec![(TOKEN_PATH.into(), token_file.0.clone())],
        setup: Some(script.clone()),
    };
    let created = prov.create(&spec);
    drop(token_file);
    let _ = fs::remove_file(&script);
    let id = match created {
        Ok(id) => id,
        Err(f) => {
            if let Some(id) = f.id.clone() {
                run.sandbox = Some(id);
            }
            return Err(anyhow::Error::new(f));
        }
    };
    run.sandbox = Some(id.clone());
    let snapshot = run.clone();
    state::update(project, &run.epic, |r| *r = snapshot)?;
    say(&format!(
        "sandbox {id} ({}) made with {}; waiting for its start script",
        run.sandbox_name, s.service
    ));

    // The start script runs in the background: wait for it.
    let until = now_ms() + SETUP_LIMIT_MS;
    loop {
        match prov.setup(&id)? {
            provider::Setup::Done => break,
            provider::Setup::Failed(why) => {
                anyhow::bail!("the sandbox's start script failed: {why}")
            }
            provider::Setup::Pending if now_ms() > until => anyhow::bail!(
                "the sandbox's start script did not finish in {} minutes",
                SETUP_LIMIT_MS / 60_000
            ),
            provider::Setup::Pending => std::thread::sleep(poll()),
        }
    }
    say("start script done: kontext and the project are set up in the sandbox");

    // The coordinator's first turn, held by a process of its own.
    run.state = RunState::Running;
    run.turns = 1;
    let snapshot = run.clone();
    state::update(project, &run.epic, |r| *r = snapshot)?;
    turn::launch(project, &run.epic, 1)?;
    say(&format!(
        "coordinator prompted: {} {} (`kontext brief {}`)",
        s.provider, s.model, run.name
    ));
    let w = config::resolve_project(Some(project))?.writer;
    record(home, &w, start_entry(run, s))?;
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
        if self.runs.is_empty() {
            l.push("No epic was dispatched from this machine.".into());
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
        }
        l.join("\n") + "\n"
    }
}

fn left_text(run: &Run) -> String {
    let mut left = Vec::new();
    if run.sandbox.is_some() && !run.sandbox_stopped {
        left.push("the sandbox is not stopped");
    }
    if run.token_id.is_some() && !run.token_revoked {
        left.push("the token is not revoked");
    }
    left.join(", ")
}

// ------------------------------------------------------------ stop

/// The run of `epic` (an id, `epic:<name>` or a name) on this machine.
fn find_run<'a>(runs: &'a std::collections::BTreeMap<String, Run>, epic: &str) -> Option<&'a Run> {
    let e = epic.trim();
    let name = e.strip_prefix("epic:").unwrap_or(e);
    runs.get(e)
        .or_else(|| runs.values().find(|r| r.name == name))
        .or_else(|| {
            runs.values()
                .find(|r| e.len() >= 4 && r.epic.starts_with(e))
        })
}

/// `kontext dispatch stop <epic>`: stops its sandbox and revokes its token.
/// A run that already ended but was not fully cleaned up is cleaned up again.
pub fn stop(project: &str, epic: &str) -> Result<(Run, Vec<String>)> {
    let resolved = config::resolve_project(Some(project))?;
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
    let prov = provider::of(&found.service)?;
    let mut run = found.clone();
    let failed = clean_up(project, prov.as_ref(), &mut run);
    let was_live = run.state.live();
    if was_live {
        run.state = RunState::Stopped;
        run.ended = Some(now_ms());
    }
    run.updated = now_ms();
    l.runs.insert(run.epic.clone(), run.clone());
    l.save()?;
    drop(l);
    if was_live {
        record(&resolved.home, &resolved.writer, end_entry(&run))?;
    }
    Ok((run, failed))
}
