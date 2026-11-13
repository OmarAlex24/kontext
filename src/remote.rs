//! Syncing with a kontext server (DESIGN §14.1).
//!
//! The local `kontext-home` stays the working copy: every command reads and
//! writes it as before, and the server is a relay of ledger lines.
//! - **Push** on every write: after the command, outside the writer's lock,
//!   this machine's new lines go up from the server's tail. What was not
//!   pushed (offline) goes up on the next command or `kontext sync`.
//! - **Pull** on every command and in the start hook: the frontier first (one
//!   request); if the server has lines beyond the local files, they are
//!   appended to each writer's file under that writer's lock, only if they
//!   continue its local chain and their hash is right. A line that does not
//!   is never written: it is noted as a fork, and `kontext status` shows it.
//! - Cursor files, `project.toml`/`synonyms.toml` (versioned, pushed only with
//!   `--include-config`, as with git) and `media/` go the same way.
//!
//! The network never blocks or breaks a command: short timeouts, and after a
//! failure no automatic attempt in that direction for 30 s; the error is kept
//! in `.kontext/server.json` and `kontext status` shows it. Push and pull are
//! independent: the push goes first, and a failed pull never skips or delays
//! it. The automatic pull brings no media: a file comes down when a view
//! needs it ([`ensure_media`]) or with `kontext sync`.
//!
//! §9.5: what a pull brings is only appended to the ledger files; it shows up
//! through the existing views and the start hook, never by rewriting what a
//! session already received.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::config::{self, Machine, Token, validate_writer, write_atomic};
use crate::cursor;
use crate::http::{Http, Reply};
use crate::media;
use crate::model::{Entry, compute_hash, sha256_hex};
use crate::store::{ledger_files, lock_writer, read_tail};

/// How long the automatic sync waits after a network failure before trying
/// again on its own.
const OFFLINE_BACKOFF_MS: i64 = 30_000;
/// Lines and bytes per push request.
const PUSH_LINES: usize = 500;
const PUSH_BYTES: usize = 2 * 1024 * 1024;

/// A project's server and token.
#[derive(Debug, Clone)]
pub struct Remote {
    pub url: String,
    pub token: Token,
}

/// What a project has.
#[derive(Debug, Clone)]
pub enum Link {
    /// No server: git sync, as before.
    None,
    /// A server URL but no token for it.
    NoToken(String),
    Ready(Remote),
}

impl Link {
    pub fn of(project: &str) -> Result<Link> {
        let machine = Machine::load()?;
        let Some(url) = machine.server_for(project) else {
            return Ok(Link::None);
        };
        Ok(match config::token_for(project)? {
            Some(token) => Link::Ready(Remote { url, token }),
            None => Link::NoToken(url),
        })
    }

    pub fn remote(self) -> Option<Remote> {
        match self {
            Link::Ready(r) => Some(r),
            _ => None,
        }
    }
}

/// A connection to the server with one token.
pub struct Conn {
    http: Http,
    base: String,
    auth: String,
}

impl Conn {
    pub fn new(url: &str, project: &str, secret: &str, timeout: Duration) -> Conn {
        Conn {
            http: Http::new(timeout),
            base: format!("{}/v1/p/{project}", url.trim_end_matches('/')),
            auth: format!("Bearer {secret}"),
        }
    }

    pub fn of(r: &Remote, timeout: Duration) -> Conn {
        Conn::new(&r.url, &r.token.project, &r.token.raw, timeout)
    }

    fn send(
        &self,
        method: &str,
        path: &str,
        ct: Option<&str>,
        body: Option<&[u8]>,
    ) -> Result<Reply> {
        let url = format!("{}/{path}", self.base);
        let mut h = vec![("Authorization", self.auth.as_str())];
        if let Some(ct) = ct {
            h.push(("Content-Type", ct));
        }
        let r = self.http.request(method, &url, &h, body)?;
        if r.status == 401 {
            let msg = r
                .json()
                .ok()
                .and_then(|v| v.get("message").and_then(Value::as_str).map(str::to_string))
                .unwrap_or_else(|| r.text());
            return Err(TokenRefused(msg).into());
        }
        Ok(r)
    }

    pub fn get(&self, path: &str) -> Result<Reply> {
        self.send("GET", path, None, None)
    }

    pub fn json(&self, method: &str, path: &str, body: &Value) -> Result<Reply> {
        self.send(
            method,
            path,
            Some("application/json"),
            Some(body.to_string().as_bytes()),
        )
    }

    pub fn bytes(&self, method: &str, path: &str, body: &[u8]) -> Result<Reply> {
        self.send(method, path, Some("application/octet-stream"), Some(body))
    }

    /// A JSON answer that must be 2xx.
    pub fn ok(r: Reply, what: &str) -> Result<Value> {
        let v = r.json().with_context(|| what.to_string())?;
        if !r.ok() {
            bail!(
                "{what}: the server answered {}: {}",
                r.status,
                v.get("message").and_then(Value::as_str).unwrap_or("")
            );
        }
        Ok(v)
    }
}

/// The server does not take the token (unknown, revoked or expired).
#[derive(Debug)]
pub struct TokenRefused(pub String);

impl std::fmt::Display for TokenRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the server refused the token: {}", self.0)
    }
}

impl std::error::Error for TokenRefused {}

/// A line the server refused, or a pulled line that does not continue the
/// local chain.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Problem {
    pub writer: String,
    pub n: Option<u64>,
    /// `fork`, `gap`, `writer`, `identity`, `invalid`, or `hash` (pulled).
    pub reason: String,
    pub message: String,
    pub at: String,
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.n {
            Some(n) => write!(
                f,
                "{} line {n}: {} ({})",
                self.writer, self.message, self.reason
            ),
            None => write!(f, "{}: {} ({})", self.writer, self.message, self.reason),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ConfigSeen {
    pub version: u64,
    pub sha256: String,
}

/// `.kontext/server.json`: what this copy knows of the server. Local; losing
/// it costs a few requests (everything is idempotent), never data.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub url: Option<String>,
    /// The server's `seq` up to which cursors, config and media were pulled.
    #[serde(default)]
    pub seq: u64,
    /// The server's last line per writer, as last seen.
    #[serde(default)]
    pub server_tails: BTreeMap<String, u64>,
    #[serde(default)]
    pub cursor_seq: u64,
    #[serde(default)]
    pub media_seq: u64,
    /// Media the server is known to have.
    #[serde(default)]
    pub media_known: BTreeSet<String>,
    /// `<reader>/<writer>` → the stamp of the cursor file last pushed.
    #[serde(default)]
    pub cursors_pushed: BTreeMap<String, i64>,
    #[serde(default)]
    pub config: BTreeMap<String, ConfigSeen>,
    #[serde(default)]
    pub last_contact: Option<String>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub last_error_at: Option<String>,
    /// The last push's error, until a push works.
    #[serde(default)]
    pub push_error: Option<String>,
    /// The last pull's error, until a pull works.
    #[serde(default)]
    pub pull_error: Option<String>,
    /// No automatic push before this time (ms), after a failed one.
    #[serde(default)]
    pub push_offline_until_ms: i64,
    /// No automatic pull before this time (ms), after a failed one.
    #[serde(default)]
    pub pull_offline_until_ms: i64,
    /// The lines the server refused (a fork, or a line this token may not
    /// write), one per writer. Cleared when a push of that writer goes through.
    #[serde(default)]
    pub refused: Vec<Problem>,
    /// Pulled lines that were not written (they do not continue the local chain).
    #[serde(default)]
    pub forks: Vec<Problem>,
}

pub fn state_path(home: &Path) -> PathBuf {
    config::internal_dir(home).join("server.json")
}

impl State {
    pub fn load(home: &Path, url: &str) -> State {
        let s: State = fs::read_to_string(state_path(home))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        // Another server: start over.
        if s.url.as_deref() != Some(url) {
            return State {
                url: Some(url.to_string()),
                ..State::default()
            };
        }
        s
    }

    pub fn save(&self, home: &Path) -> Result<()> {
        config::ensure_internal_dir(home)?;
        write_atomic(
            &state_path(home),
            serde_json::to_string_pretty(self)?.as_bytes(),
        )
    }

    /// What a run did, direction by direction: a direction that failed is
    /// not tried again automatically for 30 s, and its error stays until it
    /// works again; one that worked is tried again at once.
    fn record(&mut self, o: &Outcome) {
        let until = now_ms() + OFFLINE_BACKOFF_MS;
        if o.pushed {
            self.push_offline_until_ms = if o.push_err.is_some() { until } else { 0 };
            self.push_error = o.push_err.as_ref().map(|e| format!("{e:#}"));
        }
        if o.pulled {
            self.pull_offline_until_ms = if o.pull_err.is_some() { until } else { 0 };
            self.pull_error = o.pull_err.as_ref().map(|e| format!("{e:#}"));
        }
        if o.contacted {
            self.last_contact = Some(now());
        }
        let errors: Vec<(&str, &String)> = [("push", &self.push_error), ("pull", &self.pull_error)]
            .into_iter()
            .filter_map(|(d, e)| e.as_ref().map(|e| (d, e)))
            .collect();
        let joined = match errors.as_slice() {
            [] => None,
            // Both failed the same way (the frontier, which both need).
            [(_, a), (_, b)] if a == b => Some(a.to_string()),
            _ => Some(
                errors
                    .iter()
                    .map(|(d, e)| format!("{d}: {e}"))
                    .collect::<Vec<_>>()
                    .join("; "),
            ),
        };
        if joined != self.last_error {
            self.last_error_at = joined.as_ref().map(|_| now());
            self.last_error = joined;
        }
    }

    /// Notes a refused line of a writer (one per writer).
    fn refuse(&mut self, p: Problem) {
        self.refused.retain(|x| x.writer != p.writer);
        self.refused.push(p);
    }
}

fn now() -> String {
    jiff::Timestamp::now().to_string()
}

fn now_ms() -> i64 {
    jiff::Timestamp::now().as_millisecond()
}

/// The lock that keeps two kontext processes of this copy from syncing at once.
fn sync_lock(home: &Path, wait: bool) -> Result<Option<File>> {
    let dir = config::internal_dir(home).join("locks");
    fs::create_dir_all(&dir)?;
    let f = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("server.lock"))?;
    if wait {
        f.lock()?;
        return Ok(Some(f));
    }
    match f.try_lock() {
        Ok(()) => Ok(Some(f)),
        Err(_) => Ok(None),
    }
}

/// How far each local ledger file goes (its last `n`), from the files.
pub fn local_frontier(home: &Path) -> Result<BTreeMap<String, u64>> {
    let mut out = BTreeMap::new();
    for (w, p) in ledger_files(&home.join("ledger"))? {
        let (n, _) = read_tail(&p).unwrap_or((0, String::new()));
        out.insert(w, n);
    }
    Ok(out)
}

/// What to do.
#[derive(Debug, Clone)]
pub struct Options {
    /// Per request.
    pub timeout: Duration,
    /// Stop starting new requests after this long (the rest waits for the next run).
    pub budget: Duration,
    pub pull: bool,
    pub push: bool,
    pub include_config: bool,
    /// Bring the media the server has and this copy lacks. Not in the
    /// automatic pull: a file comes down when a view needs it.
    pub media: bool,
    /// Push every writer's file and every cursor file with the admin secret
    /// (`kontext server connect`).
    pub admin: Option<String>,
}

impl Options {
    /// Before and after an ordinary command.
    pub fn auto() -> Options {
        Options {
            timeout: Duration::from_millis(2500),
            budget: Duration::from_secs(6),
            pull: true,
            push: true,
            include_config: false,
            media: false,
            admin: None,
        }
    }

    /// `kontext sync`.
    pub fn full(include_config: bool) -> Options {
        Options {
            timeout: Duration::from_secs(30),
            budget: Duration::from_secs(600),
            pull: true,
            push: true,
            include_config,
            media: true,
            admin: None,
        }
    }
}

/// What a sync did.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Report {
    pub url: String,
    pub project: String,
    pub writer: String,
    /// Entries received per writer.
    pub received: BTreeMap<String, u64>,
    /// Own entries pushed (with the admin secret, every writer's).
    pub pushed: BTreeMap<String, u64>,
    pub cursors_in: u64,
    pub cursors_out: u64,
    pub media_in: u64,
    pub media_out: u64,
    pub config_in: Vec<String>,
    pub config_out: Vec<String>,
    pub warnings: Vec<String>,
    /// The lines the server refused in this run, one per writer: nothing
    /// after each was pushed for that writer; the other writers went on.
    pub refused: Vec<Problem>,
    /// A direction (push or pull) that failed while the other worked.
    pub errors: Vec<String>,
    /// Pulled lines not written in this run.
    pub forks: Vec<Problem>,
    /// Not everything fit in the time budget.
    pub partial: bool,
}

fn plural(n: u64, one: &str, many: &str) -> String {
    crate::views::plural(n as usize, one, many)
}

impl Report {
    pub fn received_total(&self) -> u64 {
        self.received.values().sum()
    }

    pub fn pushed_total(&self) -> u64 {
        self.pushed.values().sum()
    }

    pub fn ok(&self) -> bool {
        self.refused.is_empty() && self.forks.is_empty() && self.errors.is_empty()
    }

    pub fn text(&self) -> String {
        let mut l = vec![format!(
            "Server {} (project {}, writer {}).",
            self.url, self.project, self.writer
        )];
        for r in &self.refused {
            l.push(format!(
                "REFUSED by the server: {r}. Nothing after it from {} was pushed; the local ledger is unchanged.",
                r.writer
            ));
            l.push(refused_hint(r));
        }
        for e in &self.errors {
            l.push(format!("FAILED: {e}"));
        }
        for f in &self.forks {
            l.push(format!(
                "NOT WRITTEN: a line from the server does not continue this copy's chain: {f}. \
                 The local file was left as it was; compare it with the server (`kontext verify`)."
            ));
        }
        for w in &self.warnings {
            l.push(format!("Warning: {w}"));
        }
        if self.pushed.is_empty() {
            l.push("Nothing to push.".into());
        } else {
            let parts: Vec<String> = self
                .pushed
                .iter()
                .map(|(w, n)| format!("{w}: {n}"))
                .collect();
            l.push(format!(
                "Pushed {} ({}).",
                plural(self.pushed_total(), "entry", "entries"),
                parts.join(", ")
            ));
        }
        if self.received.is_empty() {
            l.push("Nothing new arrived.".into());
        } else {
            let parts: Vec<String> = self
                .received
                .iter()
                .map(|(w, n)| format!("{w}: {n}"))
                .collect();
            l.push(format!(
                "Received {} ({}).",
                plural(self.received_total(), "entry", "entries"),
                parts.join(", ")
            ));
        }
        let mut other = Vec::new();
        if self.cursors_out + self.cursors_in > 0 {
            other.push(format!(
                "cursors sent {}, received {}",
                self.cursors_out, self.cursors_in
            ));
        }
        if self.media_out + self.media_in > 0 {
            other.push(format!(
                "media sent {}, received {}",
                self.media_out, self.media_in
            ));
        }
        if !self.config_out.is_empty() {
            other.push(format!("config sent: {}", self.config_out.join(", ")));
        }
        if !self.config_in.is_empty() {
            other.push(format!("config received: {}", self.config_in.join(", ")));
        }
        if !other.is_empty() {
            l.push(format!("Also: {}.", other.join("; ")));
        }
        if self.partial {
            l.push("Not everything fit in this run: run `kontext sync` again.".into());
        }
        l.join("\n") + "\n"
    }
}

/// What to do about a refused line.
pub fn refused_hint(r: &Problem) -> String {
    match r.reason.as_str() {
        "fork" => format!(
            "The server has a different line {} for {}: another copy wrote as the same writer. \
             Each machine and sandbox needs its own writer (a token per writer); keep this copy's \
             file aside and clone again, or ask the owner.",
            r.n.unwrap_or(0),
            r.writer
        ),
        "identity" | "writer" => {
            "This token may not write that line: write it with the right role, \
             or with a token for that role (`kontext token create`)."
                .into()
        }
        "too_large" => format!(
            "The line is over the server's limit of 4 MiB per line, so it can never be pushed, and nothing \
             after it from {} can either (this binary refuses such a line before writing it in a project \
             with a server; this one was written before). Everything else was uploaded. Go on writing with \
             another writer (a new token for this machine).",
            r.writer
        ),
        _ => "Run `kontext sync` again; if it repeats, check `kontext verify`.".into(),
    }
}

/// What a run did: the report, and the error of each direction it tried.
struct Outcome {
    report: Report,
    pushed: bool,
    pulled: bool,
    push_err: Option<anyhow::Error>,
    pull_err: Option<anyhow::Error>,
    /// The server answered at least once.
    contacted: bool,
}

impl Outcome {
    /// `Err` only if every direction it tried failed (the first error);
    /// otherwise the report, with the direction that failed in `errors`.
    fn into_result(mut self) -> Result<Report> {
        let tried = [self.pushed, self.pulled].iter().filter(|x| **x).count();
        let failed = [&self.push_err, &self.pull_err]
            .iter()
            .filter(|e| e.is_some())
            .count();
        if tried > 0 && failed == tried {
            return Err(self.push_err.or(self.pull_err).unwrap());
        }
        for (d, e) in [("push", &self.push_err), ("pull", &self.pull_err)] {
            if let Some(e) = e {
                self.report.errors.push(format!("the {d} failed: {e:#}"));
            }
        }
        Ok(self.report)
    }
}

/// Syncs a kontext-home with its server: push, then pull, independently.
/// `Err` if every direction it tried failed; the errors are kept for
/// `kontext status`.
pub fn sync(home: &Path, remote: &Remote, opts: &Options) -> Result<Report> {
    let _lock = sync_lock(home, true)?;
    let mut state = State::load(home, &remote.url);
    let o = run(home, remote, opts, &mut state);
    state.record(&o);
    state.save(home)?;
    o.into_result()
}

/// The automatic sync around a command: quiet, bounded, and skipped while
/// another process of this copy is syncing. Each direction is skipped for
/// 30 s after it failed, on its own: a failed pull never holds back the push.
/// With `pull` false it only pushes, and only if there is something to push.
/// Returns the report when something ran and not all of it failed.
pub fn auto(home: &Path, remote: &Remote, pull: bool) -> Option<Report> {
    let lock = sync_lock(home, false).ok()??;
    let mut state = State::load(home, &remote.url);
    let now = now_ms();
    let push = state.push_offline_until_ms <= now && has_unpushed(home, remote, &state);
    let pull = pull && state.pull_offline_until_ms <= now;
    if !push && !pull {
        return None;
    }
    let mut opts = Options::auto();
    opts.push = push;
    opts.pull = pull;
    let o = run(home, remote, &opts, &mut state);
    state.record(&o);
    let _ = state.save(home);
    drop(lock);
    o.into_result().ok()
}

/// The warnings of an automatic sync, for stderr (never in the hook).
pub fn auto_warnings(r: &Report) -> Vec<String> {
    let mut out = Vec::new();
    for p in &r.refused {
        out.push(format!("the server refused {p}. {}", refused_hint(p)));
    }
    for f in &r.forks {
        out.push(format!(
            "a line from the server was not written: {f} (see `kontext status`)"
        ));
    }
    out
}

/// Does this copy have something to push (own lines, cursor files)?
fn has_unpushed(home: &Path, remote: &Remote, state: &State) -> bool {
    let w = &remote.token.writer;
    let local = read_tail(&home.join("ledger").join(format!("{w}.jsonl")))
        .map(|t| t.0)
        .unwrap_or(0);
    if local > state.server_tails.get(w).copied().unwrap_or(0) {
        return true;
    }
    !own_cursors(home, w, state).is_empty()
}

/// One run: the frontier (both directions need it), then the push, then the
/// pull, each on its own: an error in one is recorded and the other goes on.
fn run(home: &Path, remote: &Remote, opts: &Options, state: &mut State) -> Outcome {
    let started = Instant::now();
    let conn = Conn::of(remote, opts.timeout);
    let mut o = Outcome {
        report: Report {
            url: remote.url.clone(),
            project: remote.token.project.clone(),
            writer: remote.token.writer.clone(),
            ..Report::default()
        },
        pushed: opts.push,
        pulled: opts.pull,
        push_err: None,
        pull_err: None,
        contacted: false,
    };
    let f = match conn.get("frontier").and_then(|r| Conn::ok(r, "frontier")) {
        Ok(f) => f,
        Err(e) => {
            if opts.push && opts.pull {
                o.push_err = Some(anyhow::anyhow!("{e:#}"));
            }
            if opts.pull {
                o.pull_err = Some(e);
            } else {
                o.push_err = Some(e);
            }
            return o;
        }
    };
    o.contacted = true;
    let server_tails: BTreeMap<String, (u64, String)> = f
        .get("tails")
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .filter_map(|(w, t)| {
                    let n = t.get("n")?.as_u64()?;
                    let h = t.get("hash").and_then(Value::as_str).unwrap_or("");
                    Some((w.clone(), (n, h.to_string())))
                })
                .collect()
        })
        .unwrap_or_default();
    let tails: BTreeMap<String, u64> = server_tails.iter().map(|(w, t)| (w.clone(), t.0)).collect();
    state.server_tails = tails.clone();
    let seq = f.get("seq").and_then(Value::as_u64).unwrap_or(0);

    // The same length with a different last line is a fork that comparing
    // lengths alone would miss (another copy wrote as the same writer while
    // this one was offline). Nothing is written or pushed for that writer.
    let diverged = match diverged_writers(home, &server_tails) {
        Ok(d) => d,
        Err(e) => {
            o.push_err = opts.push.then(|| anyhow::anyhow!("{e:#}"));
            o.pull_err = opts.pull.then_some(e);
            return o;
        }
    };
    for (w, n) in &diverged {
        let p = Problem {
            writer: w.clone(),
            n: Some(*n),
            reason: "fork".into(),
            message: "the server has a different line here: another copy wrote as the same writer"
                .into(),
            at: now(),
        };
        if *w == remote.token.writer {
            o.report.refused.push(p.clone());
            state.refuse(p);
        } else {
            state
                .forks
                .retain(|x| !(x.writer == p.writer && x.n == p.n));
            state.forks.push(p.clone());
            o.report.forks.push(p);
        }
    }

    if opts.push
        && let Err(e) = push_all(home, remote, &conn, opts, state, &mut o.report, &started)
    {
        o.push_err = Some(e);
    }
    if opts.pull {
        match pull_all(
            home,
            remote,
            &conn,
            opts,
            state,
            &mut o.report,
            &started,
            &f,
            &diverged,
        ) {
            Ok(()) => {
                if !o.report.partial {
                    state.seq = seq;
                }
            }
            Err(e) => o.pull_err = Some(e),
        }
    }
    o
}

/// Stop starting new requests: the run is over its budget.
fn out_of_time(report: &mut Report, started: &Instant, opts: &Options) -> bool {
    if started.elapsed() > opts.budget {
        report.partial = true;
        true
    } else {
        false
    }
}

/// Own lines (with the admin secret, every writer's, going on past a writer
/// the server refuses), cursor files, media and config.
fn push_all(
    home: &Path,
    remote: &Remote,
    conn: &Conn,
    opts: &Options,
    state: &mut State,
    report: &mut Report,
    started: &Instant,
) -> Result<()> {
    let admin = opts
        .admin
        .as_ref()
        .map(|s| Conn::new(&remote.url, &remote.token.project, s, opts.timeout));
    let writers: Vec<String> = match &admin {
        Some(_) => ledger_files(&home.join("ledger"))?
            .into_iter()
            .map(|(w, _)| w)
            .collect(),
        None => vec![remote.token.writer.clone()],
    };
    let c = admin.as_ref().unwrap_or(conn);
    for w in writers {
        if out_of_time(report, started, opts) {
            break;
        }
        if let Some(p) = push_lines(home, c, &w, state, report, admin.is_some())? {
            report.refused.push(p.clone());
            state.refuse(p);
        }
    }
    push_cursors(home, c, remote, state, report, admin.is_some())?;
    push_media(home, conn, state, report, started, opts)?;
    push_config(home, c, opts.include_config, state, report)?;
    Ok(())
}

/// What the server has that this copy lacks: lines, cursor files, config and
/// (only with `opts.media`) media.
#[allow(clippy::too_many_arguments)]
fn pull_all(
    home: &Path,
    remote: &Remote,
    conn: &Conn,
    opts: &Options,
    state: &mut State,
    report: &mut Report,
    started: &Instant,
    f: &Value,
    diverged: &BTreeMap<String, u64>,
) -> Result<()> {
    let tails = state.server_tails.clone();
    let seq = f.get("seq").and_then(Value::as_u64).unwrap_or(0);
    let mut local = local_frontier(home)?;
    let behind = tails
        .iter()
        .any(|(w, n)| *n > local.get(w).copied().unwrap_or(0));
    if behind {
        pull_lines(home, conn, &mut local, state, report, &|r: &mut Report| {
            out_of_time(r, started, opts)
        })?;
    }
    // A fork noted earlier whose writer now matches the server is gone.
    let now_local = local_frontier(home)?;
    let forks_now = report.forks.clone();
    state.forks.retain(|p| {
        forks_now.contains(p)
            || diverged.contains_key(&p.writer)
            || now_local.get(&p.writer).copied().unwrap_or(0)
                < tails.get(&p.writer).copied().unwrap_or(0)
    });
    if seq != state.seq {
        pull_cursors(home, conn, remote, state, report)?;
        pull_config(home, conn, f.get("config"), state, report)?;
    }
    // Media go by their own mark (`media_seq`), not by `seq`: the automatic
    // pull moves `seq` without bringing them, and `kontext sync` still must.
    if opts.media {
        pull_media(home, conn, state, report, started, opts)?;
    }
    Ok(())
}

/// The writers whose local file ends at the same `n` as the server's but with
/// another hash, with that `n`.
fn diverged_writers(
    home: &Path,
    server: &BTreeMap<String, (u64, String)>,
) -> Result<BTreeMap<String, u64>> {
    let mut out = BTreeMap::new();
    for (w, p) in ledger_files(&home.join("ledger"))? {
        let Some((sn, sh)) = server.get(&w) else {
            continue;
        };
        let Ok((n, h)) = read_tail(&p) else { continue };
        if n > 0 && n == *sn && !sh.is_empty() && h != *sh {
            out.insert(w, n);
        }
    }
    Ok(out)
}

fn pull_lines(
    home: &Path,
    conn: &Conn,
    local: &mut BTreeMap<String, u64>,
    state: &mut State,
    report: &mut Report,
    out_of_time: &dyn Fn(&mut Report) -> bool,
) -> Result<()> {
    loop {
        if out_of_time(report) {
            return Ok(());
        }
        let since: serde_json::Map<String, Value> =
            local.iter().map(|(w, n)| (w.clone(), json!(n))).collect();
        let v = Conn::ok(
            conn.json("POST", "pull", &json!({ "since": since }))?,
            "pull",
        )?;
        let lines: Vec<(String, u64, String)> = v
            .get("lines")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|l| {
                        Some((
                            l.get("writer")?.as_str()?.to_string(),
                            l.get("n")?.as_u64()?,
                            l.get("line")?.as_str()?.to_string(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        if lines.is_empty() {
            return Ok(());
        }
        let (got, forks) = apply_lines(home, &lines)?;
        for (w, n) in got {
            *report.received.entry(w).or_default() += n;
        }
        for fk in forks {
            state
                .forks
                .retain(|p| !(p.writer == fk.writer && p.n == fk.n));
            state.forks.push(fk.clone());
            report.forks.push(fk);
        }
        let before = local.clone();
        *local = local_frontier(home)?;
        // A writer whose lines were not written is asked for no more.
        for fk in &report.forks {
            local.insert(fk.writer.clone(), u64::MAX / 2);
        }
        let more = v.get("more").and_then(Value::as_bool).unwrap_or(false);
        if !more || *local == before {
            return Ok(());
        }
    }
}

/// Appends pulled lines to their writers' files, each under its writer's lock,
/// only where they continue the local chain with a right hash. Returns how
/// many were written per writer and what was not.
pub fn apply_lines(
    home: &Path,
    lines: &[(String, u64, String)],
) -> Result<(BTreeMap<String, u64>, Vec<Problem>)> {
    let mut by: BTreeMap<&str, Vec<(u64, &str)>> = BTreeMap::new();
    for (w, n, l) in lines {
        by.entry(w.as_str()).or_default().push((*n, l.as_str()));
    }
    let mut written = BTreeMap::new();
    let mut forks = Vec::new();
    let ledger = home.join("ledger");
    fs::create_dir_all(&ledger)?;
    for (w, group) in by {
        let problem = |n: Option<u64>, reason: &str, message: String| Problem {
            writer: w.to_string(),
            n,
            reason: reason.into(),
            message,
            at: now(),
        };
        if validate_writer(w).is_err() {
            forks.push(problem(
                None,
                "invalid",
                format!("{w:?} is not a writer name"),
            ));
            continue;
        }
        let _lock = lock_writer(home, w)?;
        let path = ledger.join(format!("{w}.jsonl"));
        let (mut tail_n, mut tail_hash) = match read_tail(&path) {
            Ok(t) => t,
            Err(e) => {
                forks.push(problem(
                    None,
                    "invalid",
                    format!("the local file cannot take more: {e:#}"),
                ));
                continue;
            }
        };
        let mut local_lines: Option<Vec<String>> = None;
        let mut buf = String::new();
        let mut count = 0u64;
        for (n, line) in group {
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                forks.push(problem(
                    Some(n),
                    "invalid",
                    "the line from the server is not JSON".into(),
                ));
                break;
            };
            let hash = v.get("hash").and_then(Value::as_str).unwrap_or("");
            if n <= tail_n {
                let local = local_lines.get_or_insert_with(|| {
                    fs::read_to_string(&path)
                        .unwrap_or_default()
                        .lines()
                        .map(str::to_string)
                        .collect()
                });
                let same = n >= 1
                    && local
                        .get((n - 1) as usize)
                        .and_then(|l| serde_json::from_str::<Value>(l).ok())
                        .and_then(|lv| lv.get("hash").and_then(Value::as_str).map(str::to_string))
                        .is_some_and(|h| h == hash);
                if same {
                    continue;
                }
                forks.push(problem(
                    Some(n),
                    "fork",
                    format!("this copy has a different line {n} for {w}"),
                ));
                break;
            }
            let ok_shape = v.get("writer").and_then(Value::as_str) == Some(w)
                && v.get("n").and_then(Value::as_u64) == Some(n)
                && n == tail_n + 1;
            if !ok_shape {
                forks.push(problem(
                    Some(n),
                    "gap",
                    format!("expected line {} of {w}", tail_n + 1),
                ));
                break;
            }
            if v.get("prev").and_then(Value::as_str) != Some(tail_hash.as_str()) {
                forks.push(problem(
                    Some(n),
                    "fork",
                    format!("line {n} does not follow this copy's line {tail_n} of {w}"),
                ));
                break;
            }
            if compute_hash(&v) != hash || Entry::from_value(v.clone()).is_err() {
                forks.push(problem(
                    Some(n),
                    "hash",
                    "the line's hash does not match its content".into(),
                ));
                break;
            }
            buf.push_str(line);
            buf.push('\n');
            tail_n = n;
            tail_hash = hash.to_string();
            count += 1;
        }
        if count > 0 {
            let created = !path.exists();
            let mut f = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .with_context(|| format!("could not open {}", path.display()))?;
            f.write_all(buf.as_bytes())?;
            f.sync_all()?;
            if created {
                File::open(&ledger)?.sync_all()?;
            }
            written.insert(w.to_string(), count);
        }
    }
    Ok((written, forks))
}

/// Pushes one writer's lines the server lacks. `Some` if the server refused
/// one, or (with `each_writer`, `kontext server connect`, which goes on with
/// the other writers) answered an error for this writer's lines.
fn push_lines(
    home: &Path,
    conn: &Conn,
    writer: &str,
    state: &mut State,
    report: &mut Report,
    each_writer: bool,
) -> Result<Option<Problem>> {
    let path = home.join("ledger").join(format!("{writer}.jsonl"));
    let local_n = read_tail(&path).map(|t| t.0).unwrap_or(0);
    let mut from = state.server_tails.get(writer).copied().unwrap_or(0);
    if local_n <= from {
        return Ok(None);
    }
    let text = fs::read_to_string(&path)?;
    let lines: Vec<&str> = text.lines().collect();
    let mut tries = 0;
    while (from as usize) < lines.len() {
        let mut batch = Vec::new();
        let mut bytes = 0;
        for l in &lines[from as usize..] {
            if batch.len() >= PUSH_LINES || (!batch.is_empty() && bytes + l.len() > PUSH_BYTES) {
                break;
            }
            bytes += l.len();
            batch.push(*l);
        }
        let r = conn.json("POST", "push", &json!({ "writer": writer, "lines": batch }))?;
        let v = r.json().context("push")?;
        let accepted = v.get("accepted").and_then(Value::as_u64).unwrap_or(0);
        let tail = v
            .get("tail")
            .and_then(|t| t.get("n"))
            .and_then(Value::as_u64)
            .unwrap_or(from);
        if accepted > 0 {
            *report.pushed.entry(writer.to_string()).or_default() += accepted;
        }
        state.server_tails.insert(writer.to_string(), tail);
        if let Some(rf) = v.get("refused").filter(|x| !x.is_null()) {
            let reason = rf
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            // The server has fewer lines than this copy thought: from its tail.
            if reason == "gap" && tail < from && tries < 3 {
                tries += 1;
                from = tail;
                continue;
            }
            return Ok(Some(Problem {
                writer: writer.to_string(),
                n: rf.get("n").and_then(Value::as_u64),
                reason,
                message: rf
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                at: now(),
            }));
        }
        if !r.ok() {
            let message = format!(
                "the server answered {}: {}",
                r.status,
                v.get("message").and_then(Value::as_str).unwrap_or("")
            );
            if each_writer {
                return Ok(Some(Problem {
                    writer: writer.to_string(),
                    n: None,
                    reason: "error".into(),
                    message,
                    at: now(),
                }));
            }
            bail!("push: {message}");
        }
        if tail <= from {
            break;
        }
        from = tail;
    }
    state.refused.retain(|p| p.writer != writer);
    Ok(None)
}

/// The cursor files of this writer that changed since they were last pushed:
/// `(key, reader, path, stamp)`.
fn own_cursors(home: &Path, writer: &str, state: &State) -> Vec<(String, String, PathBuf, i64)> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(home.join("cursors")) else {
        return out;
    };
    for d in rd.flatten() {
        let reader = d.file_name().to_string_lossy().into_owned();
        if cursor::validate_reader(&reader).is_err() {
            continue;
        }
        let p = d.path().join(format!("{writer}.json"));
        let Some(stamp) = mtime_ms(&p) else {
            continue;
        };
        let key = format!("{reader}/{writer}");
        if state.cursors_pushed.get(&key).is_none_or(|s| stamp > *s) {
            out.push((key, reader, p, stamp));
        }
    }
    out
}

fn mtime_ms(p: &Path) -> Option<i64> {
    let t = fs::metadata(p).ok()?.modified().ok()?;
    jiff::Timestamp::try_from(t)
        .ok()
        .map(|t| t.as_millisecond())
}

fn push_cursors(
    home: &Path,
    conn: &Conn,
    remote: &Remote,
    state: &mut State,
    report: &mut Report,
    all: bool,
) -> Result<()> {
    let writers: Vec<String> = if all {
        // Every machine's cursor files (`kontext server connect`).
        let mut ws = BTreeSet::new();
        for d in fs::read_dir(home.join("cursors"))
            .into_iter()
            .flatten()
            .flatten()
        {
            for f in fs::read_dir(d.path()).into_iter().flatten().flatten() {
                let name = f.file_name().to_string_lossy().into_owned();
                if let Some(w) = name.strip_suffix(".json")
                    && validate_writer(w).is_ok()
                {
                    ws.insert(w.to_string());
                }
            }
        }
        ws.into_iter().collect()
    } else {
        vec![remote.token.writer.clone()]
    };
    let mut items = Vec::new();
    let mut keys = Vec::new();
    for w in &writers {
        for (key, reader, path, stamp) in own_cursors(home, w, state) {
            let Ok(body) = fs::read_to_string(&path) else {
                continue;
            };
            items.push(json!({ "reader": reader, "writer": w, "body": body, "stamp": stamp }));
            keys.push((key, stamp));
        }
    }
    if items.is_empty() {
        return Ok(());
    }
    let v = Conn::ok(
        conn.json("POST", "cursors", &json!({ "cursors": items }))?,
        "cursors",
    )?;
    report.cursors_out += v.get("stored").and_then(Value::as_u64).unwrap_or(0);
    for (k, s) in keys {
        state.cursors_pushed.insert(k, s);
    }
    Ok(())
}

fn pull_cursors(
    home: &Path,
    conn: &Conn,
    remote: &Remote,
    state: &mut State,
    report: &mut Report,
) -> Result<()> {
    let v = Conn::ok(
        conn.get(&format!("cursors?since={}", state.cursor_seq))?,
        "cursors",
    )?;
    let own = &remote.token.writer;
    let mut max = state.cursor_seq;
    for c in v
        .get("cursors")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let reader = c.get("reader").and_then(Value::as_str).unwrap_or("");
        let writer = c.get("writer").and_then(Value::as_str).unwrap_or("");
        let body = c.get("body").and_then(Value::as_str).unwrap_or("");
        let stamp = c.get("stamp").and_then(Value::as_i64).unwrap_or(0);
        max = max.max(c.get("seq").and_then(Value::as_u64).unwrap_or(0));
        // Never this machine's own file: only it writes that one.
        if writer == own
            || cursor::validate_reader(reader).is_err()
            || validate_writer(writer).is_err()
        {
            continue;
        }
        if serde_json::from_str::<BTreeMap<String, u64>>(body).is_err() {
            report.warnings.push(format!(
                "the cursor {reader}/{writer} from the server is not valid; skipped"
            ));
            continue;
        }
        let dir = cursor::reader_dir(home, reader);
        let p = dir.join(format!("{writer}.json"));
        if fs::read_to_string(&p).is_ok_and(|t| t == body) {
            continue;
        }
        fs::create_dir_all(&dir)?;
        write_atomic(&p, body.as_bytes())?;
        if let Ok(t) = jiff::Timestamp::from_millisecond(stamp) {
            let _ = File::options()
                .write(true)
                .open(&p)
                .and_then(|f| f.set_modified(std::time::SystemTime::from(t)));
        }
        report.cursors_in += 1;
    }
    state.cursor_seq = max;
    Ok(())
}

fn file_sha(p: &Path) -> Option<String> {
    fs::read(p).ok().map(|b| sha256_hex(&b))
}

fn pull_config(
    home: &Path,
    conn: &Conn,
    versions: Option<&Value>,
    state: &mut State,
    report: &mut Report,
) -> Result<()> {
    let Some(versions) = versions.and_then(Value::as_object) else {
        return Ok(());
    };
    let newer: Vec<String> = versions
        .iter()
        .filter(|(name, v)| {
            crate::sync::CONFIG_FILES.contains(&name.as_str())
                && v.get("version").and_then(Value::as_u64).unwrap_or(0)
                    > state.config.get(*name).map(|c| c.version).unwrap_or(0)
        })
        .map(|(n, _)| n.clone())
        .collect();
    if newer.is_empty() {
        return Ok(());
    }
    let all = Conn::ok(conn.get("config")?, "config")?;
    for name in newer {
        let Some(f) = all.get("files").and_then(|f| f.get(&name)) else {
            continue;
        };
        let body = f.get("body").and_then(Value::as_str).unwrap_or("");
        let version = f.get("version").and_then(Value::as_u64).unwrap_or(0);
        let sha = f
            .get("sha256")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let path = home.join(&name);
        let local = file_sha(&path);
        let seen = state.config.get(&name).map(|c| c.sha256.clone());
        let untouched = local.is_none() || local == seen || local.as_deref() == Some(sha.as_str());
        if !untouched {
            let aside = config::internal_dir(home).join("server-config");
            fs::create_dir_all(&aside)?;
            write_atomic(&aside.join(&name), body.as_bytes())?;
            report.warnings.push(format!(
                "{name} changed here and on the server (version {version}): the local one was kept, and \
                 the server's is in {}. Merge them by hand, then push with `kontext sync --include-config`",
                aside.join(&name).display()
            ));
            continue;
        }
        if local.as_deref() != Some(sha.as_str()) {
            write_atomic(&path, body.as_bytes())?;
            report.config_in.push(name.clone());
        }
        state.config.insert(
            name,
            ConfigSeen {
                version,
                sha256: sha,
            },
        );
    }
    Ok(())
}

fn push_config(
    home: &Path,
    conn: &Conn,
    include: bool,
    state: &mut State,
    report: &mut Report,
) -> Result<()> {
    for name in crate::sync::CONFIG_FILES {
        let Ok(body) = fs::read_to_string(home.join(name)) else {
            continue;
        };
        let sha = sha256_hex(body.as_bytes());
        let seen = state.config.get(*name).cloned().unwrap_or_default();
        if seen.sha256 == sha {
            continue;
        }
        if !include {
            report.warnings.push(if seen.version > 0 {
                format!(
                    "{name} has changes that are not pushed automatically: `kontext sync --include-config` pushes them"
                )
            } else {
                format!("the server has no {name} yet: `kontext sync --include-config` pushes it")
            });
            continue;
        }
        let r = conn.json(
            "PUT",
            &format!("config/{name}"),
            &json!({ "base": seen.version, "body": body }),
        )?;
        let v = r.json().context("config")?;
        match r.status {
            200..=299 => {
                let version = v
                    .get("version")
                    .and_then(Value::as_u64)
                    .unwrap_or(seen.version);
                state.config.insert(
                    name.to_string(),
                    ConfigSeen {
                        version,
                        sha256: sha,
                    },
                );
                if v.get("changed").and_then(Value::as_bool).unwrap_or(true) {
                    report.config_out.push(name.to_string());
                }
            }
            409 => report.warnings.push(format!(
                "{name} changed on the server (version {}) since this copy last saw it: run `kontext sync` to \
                 bring it in, merge, and push again",
                v.get("version").and_then(Value::as_u64).unwrap_or(0)
            )),
            403 => report
                .warnings
                .push(format!("{name}: only an owner token pushes config")),
            s => bail!(
                "config {name}: the server answered {s}: {}",
                v.get("message").and_then(Value::as_str).unwrap_or("")
            ),
        }
    }
    Ok(())
}

fn pull_media(
    home: &Path,
    conn: &Conn,
    state: &mut State,
    report: &mut Report,
    started: &Instant,
    opts: &Options,
) -> Result<()> {
    let v = Conn::ok(
        conn.get(&format!("media?since={}", state.media_seq))?,
        "media",
    )?;
    let mut max = state.media_seq;
    let mut complete = true;
    for m in v
        .get("media")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let name = m
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let seq = m.get("seq").and_then(Value::as_u64).unwrap_or(0);
        let Some((sha, _)) = media::valid_media_name(&name) else {
            continue;
        };
        let sha = sha.to_string();
        let path = home.join("media").join(&name);
        if !path.exists() {
            if started.elapsed() > opts.budget {
                report.partial = true;
                break;
            }
            let r = conn.get(&format!("media/{name}"))?;
            if !r.ok() || sha256_hex(&r.body) != sha {
                complete = false;
                report.warnings.push(format!(
                    "could not bring media/{name} ({}); it is tried again on the next sync",
                    r.status
                ));
                continue;
            }
            fs::create_dir_all(home.join("media"))?;
            write_atomic(&path, &r.body)?;
            report.media_in += 1;
        }
        state.media_known.insert(name);
        if complete {
            max = max.max(seq);
        }
    }
    state.media_seq = max;
    Ok(())
}

fn push_media(
    home: &Path,
    conn: &Conn,
    state: &mut State,
    report: &mut Report,
    started: &Instant,
    opts: &Options,
) -> Result<()> {
    let Ok(rd) = fs::read_dir(home.join("media")) else {
        return Ok(());
    };
    let mut names: Vec<String> = rd
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| media::valid_media_name(n).is_some() && !state.media_known.contains(n))
        .collect();
    names.sort();
    for name in names {
        if started.elapsed() > opts.budget {
            report.partial = true;
            break;
        }
        let bytes = fs::read(home.join("media").join(&name))?;
        let r = conn.bytes("PUT", &format!("media/{name}"), &bytes)?;
        if r.ok() {
            if r.status == 201 {
                report.media_out += 1;
            }
            state.media_known.insert(name);
        } else {
            report.warnings.push(format!(
                "media/{name}: the server answered {}: {}",
                r.status,
                r.text()
            ));
        }
    }
    Ok(())
}

/// The project's WebSocket URL (`ws://` or `wss://`).
fn ws_url(remote: &Remote) -> Result<String> {
    let base = remote.url.trim_end_matches('/');
    let ws = if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        bail!("the server URL {base} is not http:// or https://");
    };
    Ok(format!("{ws}/v1/p/{}/ws", remote.token.project))
}

/// Listens to the project's WebSocket (`kontext ui`): the server sends its
/// frontier when the socket opens and after every change. `on_frontier` is
/// called with each one and returns whether to keep listening. A WebSocket
/// ping every 50 s keeps the connection up; the runtime answers it without
/// waking the Durable Object ("Ping/pong handling does not interrupt
/// hibernation", <https://developers.cloudflare.com/durable-objects/best-practices/websockets/>).
/// Returns when the socket closes or fails.
pub fn watch(remote: &Remote, mut on_frontier: impl FnMut(&Value) -> bool) -> Result<()> {
    use tungstenite::client::IntoClientRequest;
    use tungstenite::stream::MaybeTlsStream;
    use tungstenite::{Error as WsError, Message};

    let mut req = ws_url(remote)?.into_client_request()?;
    req.headers_mut().insert(
        "Authorization",
        format!("Bearer {}", remote.token.raw).parse()?,
    );
    let (mut sock, _) = match tungstenite::connect(req) {
        Ok(s) => s,
        Err(WsError::Http(r)) if r.status().as_u16() == 401 => {
            return Err(TokenRefused("the server refused the token".into()).into());
        }
        Err(e) => return Err(anyhow::Error::new(e).context("connecting to the server's WebSocket")),
    };
    let tick = Some(Duration::from_secs(50));
    match sock.get_mut() {
        MaybeTlsStream::Plain(s) => s.set_read_timeout(tick)?,
        MaybeTlsStream::Rustls(s) => s.sock.set_read_timeout(tick)?,
        _ => {}
    }
    loop {
        match sock.read() {
            Ok(Message::Text(t)) => {
                let v: Value = serde_json::from_str(t.as_str()).unwrap_or(Value::Null);
                if v.get("type").and_then(Value::as_str) == Some("frontier") && !on_frontier(&v) {
                    let _ = sock.close(None);
                    return Ok(());
                }
            }
            Ok(Message::Close(_)) => return Ok(()),
            Ok(_) => {}
            Err(WsError::Io(e))
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                sock.send(Message::Ping(Vec::new().into()))?;
            }
            Err(WsError::ConnectionClosed | WsError::AlreadyClosed) => return Ok(()),
            Err(e) => return Err(e.into()),
        }
    }
}

/// The registered project whose kontext-home is `home`, if any.
pub fn project_for_home(home: &Path) -> Option<(Machine, String)> {
    let machine = Machine::load().ok()?;
    let want = fs::canonicalize(home).ok()?;
    let name = machine
        .projects
        .keys()
        .find(|name| {
            machine
                .project_home(name)
                .ok()
                .and_then(|h| fs::canonicalize(h).ok())
                .is_some_and(|h| h == want)
        })?
        .clone();
    Some((machine, name))
}

/// The server of the project registered with `home`, if it has one.
pub fn server_for_home(home: &Path) -> Option<String> {
    let (machine, name) = project_for_home(home)?;
    machine.server_for(&name)
}

/// The server and token of the project registered with `home`, if it has both.
pub fn remote_for_home(home: &Path) -> Option<Remote> {
    let (_, name) = project_for_home(home)?;
    Link::of(&name).ok()?.remote()
}

/// Brings the media files in `names` (`<sha256>.<ext>`) that this copy lacks
/// from the project's server, for a view that needs them: the automatic pull
/// brings none. Bounded (10 s a file, 30 s in all), and skipped while the
/// pull is backing off after a failure; it stops at the first network error.
/// Without a server, or with every file here, it does nothing. Returns the
/// warnings.
pub fn ensure_media<I: IntoIterator<Item = String>>(home: &Path, names: I) -> Vec<String> {
    let mut missing: Vec<String> = names
        .into_iter()
        .filter(|n| media::valid_media_name(n).is_some() && !home.join("media").join(n).exists())
        .collect();
    missing.sort();
    missing.dedup();
    if missing.is_empty() {
        return Vec::new();
    }
    let Some(remote) = remote_for_home(home) else {
        return Vec::new();
    };
    if State::load(home, &remote.url).pull_offline_until_ms > now_ms() {
        return Vec::new();
    }
    let conn = Conn::of(&remote, Duration::from_secs(10));
    let started = Instant::now();
    let mut warnings = Vec::new();
    for name in missing {
        if started.elapsed() > Duration::from_secs(30) {
            warnings.push(
                "not every media file came from the server in time: `kontext sync` brings them"
                    .into(),
            );
            break;
        }
        let r = match conn.get(&format!("media/{name}")) {
            Ok(r) => r,
            Err(e) => {
                warnings.push(format!(
                    "could not bring media/{name} from the server: {e:#}"
                ));
                break;
            }
        };
        let sha = media::valid_media_name(&name)
            .map(|(s, _)| s.to_string())
            .unwrap_or_default();
        if !r.ok() || sha256_hex(&r.body) != sha {
            warnings.push(format!("the server has no media/{name} ({})", r.status));
            continue;
        }
        let path = home.join("media").join(&name);
        if fs::create_dir_all(home.join("media")).is_ok() && write_atomic(&path, &r.body).is_ok() {
            continue;
        }
        warnings.push(format!("could not save media/{name}"));
    }
    warnings
}

/// For `kontext status`: the server, without the network.
#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub url: String,
    pub project: Option<String>,
    pub writer: Option<String>,
    pub token: bool,
    pub last_contact: Option<String>,
    pub last_error: Option<String>,
    pub last_error_at: Option<String>,
    /// Own entries the server does not have, as last seen.
    pub unpushed: Option<u64>,
    pub refused: Vec<Problem>,
    pub forks: Vec<Problem>,
}

pub fn status(home: &Path, link: &Link) -> Option<Status> {
    let (url, remote) = match link {
        Link::None => return None,
        Link::NoToken(u) => (u.clone(), None),
        Link::Ready(r) => (r.url.clone(), Some(r)),
    };
    let st = State::load(home, &url);
    let unpushed = remote.map(|r| {
        let w = &r.token.writer;
        let local = read_tail(&home.join("ledger").join(format!("{w}.jsonl")))
            .map(|t| t.0)
            .unwrap_or(0);
        local.saturating_sub(st.server_tails.get(w).copied().unwrap_or(0))
    });
    Some(Status {
        url,
        project: remote.map(|r| r.token.project.clone()),
        writer: remote.map(|r| r.token.writer.clone()),
        token: remote.is_some(),
        last_contact: st.last_contact,
        last_error: st.last_error,
        last_error_at: st.last_error_at,
        unpushed,
        refused: st.refused,
        forks: st.forks,
    })
}

impl Status {
    pub fn lines(&self) -> Vec<String> {
        let mut l = vec![format!(
            "Server: {}{}",
            self.url,
            match (&self.project, &self.writer) {
                (Some(p), Some(w)) => format!(" (project {p}, token for writer {w})"),
                _ => " (no token: ask an owner for one and save it in the kontext config folder, \
                       tokens/<project>.token, mode 0600)"
                    .into(),
            }
        )];
        l.push(format!(
            "Last contact: {}",
            self.last_contact
                .as_deref()
                .unwrap_or("never from this copy")
        ));
        if let Some(e) = &self.last_error {
            l.push(format!(
                "Last error ({}): {e}",
                self.last_error_at.as_deref().unwrap_or("?")
            ));
        }
        if let Some(u) = self.unpushed {
            l.push(format!(
                "Own entries not pushed: {u} (as last seen; `kontext sync` pushes them)"
            ));
        }
        for r in &self.refused {
            l.push(format!("REFUSED by the server: {r}"));
            l.push(format!("  {}", refused_hint(r)));
        }
        for f in &self.forks {
            l.push(format!("NOT WRITTEN (from the server): {f}"));
        }
        l
    }
}

/// `POST create` with the admin secret: the project on the server and its
/// first owner token, bound to `writer`. `Ok(None)` if it already exists.
pub fn create_project(
    url: &str,
    admin: &str,
    project: &str,
    person: &str,
    writer: &str,
) -> Result<Option<Token>> {
    let conn = Conn::new(url, project, admin, Duration::from_secs(30));
    let r = match conn.json(
        "POST",
        "create",
        &json!({ "person": person, "writer": writer }),
    ) {
        Err(e) if e.downcast_ref::<TokenRefused>().is_some() => bail!(
            "the server did not take the admin secret: is it the one `kontext server deploy` saved for {url} \
             (or $KONTEXT_ADMIN_SECRET)?"
        ),
        r => r?,
    };
    if r.status == 409 {
        return Ok(None);
    }
    let v = Conn::ok(r, "create the project on the server")?;
    let raw = v
        .get("token")
        .and_then(Value::as_str)
        .context("the server gave no token")?;
    Ok(Some(Token::parse(raw)?))
}

/// The admin secret for `url`, or an error that says where to get it.
pub fn require_admin(url: &str) -> Result<String> {
    crate::server::admin_secret(url)?.with_context(|| {
        format!(
            "no admin secret for {url}: run `kontext server deploy` from this machine, or set \
             KONTEXT_ADMIN_SECRET to the one in servers.json of the machine that deployed it"
        )
    })
}

/// `kontext server connect <url>`: attaches an existing project to a server
/// and uploads what this copy has. With the admin secret it creates the
/// project there if needed and uploads everything (every writer's lines, the
/// cursors, the config and the media); with only a token (a copy that started
/// without a server and joins one) it uploads its own writer's lines, and a
/// token in `$KONTEXT_TOKEN` replaces the saved one (a new token after a
/// revoke). A saved token goes only to the server it was saved for. Running
/// it again resumes where it stopped: the server keeps what it has and only
/// takes what it lacks.
pub fn connect(name: &str, home: &Path, writer: &str, url: &str) -> Result<Report> {
    let url = crate::server::normalize_url(url);
    let admin = crate::server::admin_secret(&url)?;
    let created = match &admin {
        Some(admin) => {
            let project = crate::config::Project::load(home)?;
            let person = config::human_name_in(&project, home);
            create_project(&url, admin, name, &person, writer)?
        }
        None => None,
    };
    let token = match created {
        // Made once by the server: saved before anything else can fail.
        Some(t) => {
            t.save(name)?;
            t
        }
        None => {
            let configured = Machine::load()?.server_for(name);
            // A project already on another server: neither the saved token
            // nor $KONTEXT_TOKEN goes to this URL (rule: a token only goes to
            // its own project's configured server). Moving a project is
            // `server connect` with the admin secret of the new server.
            if let Some(s) = configured.as_deref().filter(|s| *s != url) {
                bail!(
                    "project {name} syncs with {s}, not {url}: no token of it is sent to another server. \
                     To move it, run `kontext server connect {url}` with that server's admin secret"
                );
            }
            let given = env_token(name)?;
            let saved = config::token_for(name)?.filter(|t| t.project == name);
            let Some(t) = given.clone().or(saved) else {
                bail!(
                    "{}this machine has no token for {name} on {url}. Ask the project's owner for one \
                     (`kontext token create`) and run `KONTEXT_TOKEN=<token> kontext server connect {url}`{}",
                    if admin.is_some() {
                        format!("project {name} already exists on {url} and ")
                    } else {
                        String::new()
                    },
                    if admin.is_some() {
                        ""
                    } else {
                        "; to create the project there, run it with the admin secret (`kontext server \
                         deploy` saved it on the machine that deployed, or set KONTEXT_ADMIN_SECRET)"
                    }
                )
            };
            if t.writer != writer {
                bail!(
                    "the token for {name} writes as {}, not as this machine's writer {writer}",
                    t.writer
                );
            }
            if given.is_some() {
                // Saved, like the token `clone --server` gets, once the
                // server takes it: a bad one never replaces a working one.
                let remote = Remote {
                    url: url.clone(),
                    token: t.clone(),
                };
                let who = Conn::of(&remote, Duration::from_secs(30)).get("whoami")?;
                Conn::ok(who, "check the token")?;
                t.save(name)?;
            }
            t
        }
    };
    Machine::set_server(name, Some(&url))?;
    let remote = Remote { url, token };
    let mut opts = Options::full(true);
    opts.admin = admin;
    sync(home, &remote, &opts)
}

/// `$KONTEXT_TOKEN`, if it is set and is a token for `project`.
fn env_token(project: &str) -> Result<Option<Token>> {
    match std::env::var("KONTEXT_TOKEN") {
        Ok(raw) if !raw.trim().is_empty() => {
            let t = Token::parse(&raw).context("$KONTEXT_TOKEN")?;
            Ok((t.project == project).then_some(t))
        }
        _ => Ok(None),
    }
}

/// What `kontext clone --server` needs.
pub struct CloneArgs {
    pub url: String,
    pub project: String,
    pub home: Option<PathBuf>,
    pub name: Option<String>,
}

/// `kontext clone --server <url> <project>`: brings a project down from its
/// server with the token in `$KONTEXT_TOKEN`, registers it, and saves the
/// token (0600). This copy writes as the token's writer.
pub fn clone(a: CloneArgs) -> Result<(crate::sync::Cloned, Report)> {
    let url = crate::server::normalize_url(&a.url);
    let raw = std::env::var("KONTEXT_TOKEN")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .context(
            "set KONTEXT_TOKEN to a token for the project (the owner makes one with `kontext token create`)",
        )?;
    let token = Token::parse(&raw).context("$KONTEXT_TOKEN")?;
    if token.project != a.project {
        bail!(
            "the token in KONTEXT_TOKEN is for project {}, not {}",
            token.project,
            a.project
        );
    }
    let name = a.name.clone().unwrap_or_else(|| a.project.clone());
    if !crate::cli::valid_name(&name) {
        bail!("invalid name {name:?}: use --name with letters, digits, - _ and .");
    }
    let machine = Machine::load()?;
    if machine.projects.contains_key(&name) {
        bail!(
            "project {name:?} is already registered in {}; use --name <other>",
            Machine::path()?.display()
        );
    }
    let home = match a.home {
        Some(h) => h,
        None => config::home_dir()?.join("kontext-home").join(&name),
    };
    let home = if home.is_absolute() {
        home
    } else {
        std::env::current_dir()?.join(home)
    };
    if home.exists() && fs::read_dir(&home)?.next().is_some() {
        bail!("{} already exists and is not empty", home.display());
    }
    let remote = Remote {
        url: url.clone(),
        token,
    };
    let conn = Conn::of(&remote, Duration::from_secs(30));
    let config_files = Conn::ok(conn.get("config")?, "config")?;
    let Some(project_toml) = config_files
        .get("files")
        .and_then(|f| f.get("project.toml"))
        .and_then(|f| f.get("body"))
        .and_then(Value::as_str)
    else {
        bail!("{url} has no project.toml for {} yet", a.project);
    };
    fs::create_dir_all(&home)?;
    let home = fs::canonicalize(&home)?;
    let undo = |e: anyhow::Error| -> anyhow::Error {
        let _ = fs::remove_dir_all(&home);
        e
    };
    let setup = || -> Result<()> {
        for d in ["ledger", "cursors"] {
            fs::create_dir_all(home.join(d))?;
            fs::write(home.join(d).join(".gitkeep"), "")?;
        }
        fs::write(home.join("project.toml"), project_toml)?;
        fs::write(
            home.join(".gitignore"),
            format!("{}/\n", config::INTERNAL_DIR),
        )?;
        // A git repo as `kontext init` makes, so `git log` works; the server
        // is the transport.
        let _ = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&home)
            .output();
        crate::config::Project::load(&home)?;
        Ok(())
    };
    setup().map_err(undo)?;
    Machine::register(&name, &home).map_err(undo)?;
    Machine::set_server(&name, Some(&url))?;
    remote.token.save(&name)?;
    let report = sync(&home, &remote, &Options::full(false))?;
    let writer = remote.token.writer.clone();
    let mut warnings = Vec::new();
    if report.received.contains_key(&writer) {
        warnings.push(format!(
            "the server already has lines from {writer}: this copy continues that writer's file. Only one \
             copy at a time may write as {writer}"
        ));
    }
    let project = crate::config::Project::load(&home)?;
    let mut store = crate::store::Store::open(&home, &writer)?;
    let st = store.state()?;
    let charter = st.charter().or_else(|| st.latest_charter());
    let cloned = crate::sync::Cloned {
        name,
        home: home.display().to_string(),
        writer,
        charter: charter.map(|c| c.title.trim().to_string()),
        charter_signed: st.charter().is_some(),
        owners: project.owners.clone(),
        entries: store.index().count()?,
        warnings,
    };
    Ok((cloned, report))
}
