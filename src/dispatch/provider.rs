//! The sandbox service behind `kontext dispatch`. Everything that depends on
//! the service is behind [`Provider`], so another one can be added next to
//! [`Boat`] without touching the dispatcher.
//!
//! [`Boat`] runs the `boat` CLI (`$KONTEXT_BOAT`, or `boat` on the `PATH`):
//! the tests point it at a fake. It never passes a secret in an argument or a
//! variable: a secret goes as a file (`--file`). Every call has a time limit
//! (`KONTEXT_BOAT_TIMEOUT_MS` replaces them all, for the tests), after which
//! the `boat` process is killed and the call is an error.

use std::path::PathBuf;
use std::process::{Command, ExitStatus, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::Value;

/// A sandbox to make.
#[derive(Debug, Clone, Default)]
pub struct NewSandbox {
    pub name: String,
    /// The service's named environment (repos, credentials).
    pub environment: Option<String>,
    pub machine: String,
    /// How long it may stay on, in seconds.
    pub ttl_secs: u64,
    /// Variables that are not secret.
    pub env: Vec<(String, String)>,
    /// Files written in the sandbox before it is ready: (path there, local file).
    pub files: Vec<(String, PathBuf)>,
    /// A local shell script the sandbox runs once it is ready, in the background.
    pub setup: Option<PathBuf>,
}

/// Where the start script is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Setup {
    /// Not done yet.
    Pending,
    Done,
    Failed(String),
}

/// A process started in the sandbox with [`Provider::detach`], as the
/// service says it stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Process {
    Running,
    /// It ended, with this exit code (a signal's is 128 + the signal).
    Exited(i32),
    /// The service does not know it (`known: false`, or `lost`).
    Unknown,
}

/// What a short command printed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecOut {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// `create` failed. If the service made the sandbox before failing, `id`
/// says which, so the caller can stop it.
#[derive(Debug)]
pub struct CreateFailed {
    pub id: Option<String>,
    pub why: String,
}

impl std::fmt::Display for CreateFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.why)
    }
}

impl std::error::Error for CreateFailed {}

/// The service refused a command because the sandbox is not running (Boat:
/// `machine_not_running`, for a stopped sandbox; it does not wake it).
#[derive(Debug)]
pub struct NotRunning(pub String);

impl std::fmt::Display for NotRunning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for NotRunning {}

/// Is this error the service saying the sandbox is not running?
pub fn not_running(e: &anyhow::Error) -> bool {
    e.downcast_ref::<NotRunning>().is_some()
}

/// A sandbox service.
pub trait Provider {
    /// Its name in `[dispatch] service` and in the state.
    fn service(&self) -> &'static str;
    /// Makes a sandbox and returns its id.
    fn create(&self, s: &NewSandbox) -> std::result::Result<String, CreateFailed>;
    /// Stops it, keeping its data.
    fn stop(&self, id: &str) -> Result<()>;
    /// A running (or starting) sandbox with this name, if there is one: a
    /// dispatch killed while the service made its sandbox left no id.
    /// `Ok(None)` only when the service's whole answer says there is none;
    /// an answer that may be partial is an error.
    fn find(&self, name: &str) -> Result<Option<String>>;
    /// How its start script went.
    fn setup(&self, id: &str) -> Result<Setup>;
    /// Runs a short command in the sandbox and returns what it printed
    /// (an error when it could not be run or its output was cut). The
    /// service may keep the command line: it never carries a secret.
    fn exec(&self, id: &str, cmd: &[String]) -> Result<ExecOut>;
    /// Starts a command in the sandbox in the background and returns its
    /// process id there.
    fn detach(&self, id: &str, cmd: &[String]) -> Result<u32>;
    /// How a process started with [`Provider::detach`] stands (an error
    /// when the service could not be asked).
    fn process(&self, id: &str, pid: u32) -> Result<Process>;
    /// Ends that process and every process it started.
    fn kill(&self, id: &str, pid: u32) -> Result<()>;
    /// How a person looks at what happened in a stopped sandbox (one
    /// line). `resume` is false when bringing it back is not safe (the
    /// agent's credential may still be in it).
    fn inspect_hint(&self, id: &str, resume: bool) -> String;
}

/// The provider for `[dispatch] service`.
pub fn of(service: &str) -> Result<Box<dyn Provider>> {
    match service {
        "boat" => Ok(Box::new(Boat::new())),
        other => bail!("[dispatch] service = {other:?}: the only sandbox service is \"boat\""),
    }
}

/// Boat (boat.dev), through its CLI.
pub struct Boat {
    bin: String,
}

impl Default for Boat {
    fn default() -> Self {
        Boat::new()
    }
}

impl Boat {
    pub fn new() -> Boat {
        let bin = std::env::var("KONTEXT_BOAT")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "boat".into());
        Boat { bin }
    }

    pub(crate) fn command(&self, args: &[String]) -> Command {
        let mut c = Command::new(&self.bin);
        c.args(args).env("BOAT_NO_UPDATE", "1").stdin(Stdio::null());
        c
    }

    fn limit(args: &[String]) -> Duration {
        std::env::var("KONTEXT_BOAT_TIMEOUT_MS")
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .map(Duration::from_millis)
            .unwrap_or_else(|| limit_of(args))
    }

    /// Runs `boat`, and keeps what it printed even when it did not end in
    /// time (`status: None`).
    fn run_partial(&self, args: &[String]) -> Result<Ran> {
        run_with_limit(self.command(args), Boat::limit(args))
            .with_context(|| format!("could not run {} (is Boat's CLI installed?)", self.bin))
    }

    fn run(&self, args: &[String]) -> Result<Output> {
        let r = self.run_partial(args)?;
        r.output().ok_or_else(|| timed_out(args))
    }
}

fn timed_out(args: &[String]) -> anyhow::Error {
    anyhow::anyhow!(
        "boat {} did not answer in {} s",
        args.first().map(String::as_str).unwrap_or(""),
        Boat::limit(args).as_secs()
    )
}

/// How long a `boat` command may take: `new` waits for a machine, `stop`
/// for a snapshot, the rest are quick.
pub(crate) fn limit_of(args: &[String]) -> Duration {
    match args.first().map(String::as_str) {
        Some("new") => Duration::from_secs(15 * 60),
        Some("stop") => Duration::from_secs(5 * 60),
        _ => Duration::from_secs(120),
    }
}

/// What a command printed, and how it ended (`None`: it did not end within
/// its limit and was killed; what it printed until then is kept).
pub(crate) struct Ran {
    pub status: Option<ExitStatus>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl Ran {
    fn output(self) -> Option<Output> {
        Some(Output {
            status: self.status?,
            stdout: self.stdout,
            stderr: self.stderr,
        })
    }
}

/// Reads `r` into a shared buffer as it comes, so what was printed before a
/// kill can be taken without waiting for the pipe to close.
fn drain(
    mut r: impl std::io::Read + Send + 'static,
) -> (Arc<Mutex<Vec<u8>>>, std::thread::JoinHandle<()>) {
    let buf = Arc::new(Mutex::new(Vec::new()));
    let b = buf.clone();
    let h = std::thread::spawn(move || {
        let mut chunk = [0u8; 8192];
        loop {
            match r.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => b
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .extend_from_slice(&chunk[..n]),
            }
        }
    });
    (buf, h)
}

fn take(b: &Arc<Mutex<Vec<u8>>>) -> Vec<u8> {
    std::mem::take(&mut *b.lock().unwrap_or_else(|e| e.into_inner()))
}

/// Runs `c` and returns what it printed and how it ended. Past `limit` it
/// is killed, and what it printed until then is returned with no status.
pub(crate) fn run_with_limit(mut c: Command, limit: Duration) -> std::io::Result<Ran> {
    let mut child = c.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    let (o, oh) = drain(child.stdout.take().unwrap());
    let (e, eh) = drain(child.stderr.take().unwrap());
    let until = Instant::now() + limit;
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break Some(s);
        }
        if Instant::now() >= until {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    if status.is_some() {
        let _ = oh.join();
        let _ = eh.join();
    } else {
        // Whatever it started may still hold the pipes: wait a little for
        // the last of its output, never for the pipes to close.
        let t = Instant::now() + Duration::from_millis(300);
        while Instant::now() < t && !(oh.is_finished() && eh.is_finished()) {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    Ok(Ran {
        status,
        stdout: take(&o),
        stderr: take(&e),
    })
}

/// Did `boat` answer that the sandbox is not running (`machine_not_running`)?
fn machine_not_running(stdout: &[u8], stderr: &[u8]) -> Option<String> {
    json_lines(stdout)
        .into_iter()
        .chain(json_lines(stderr))
        .find(|v| v.get("code").and_then(Value::as_str) == Some("machine_not_running"))
        .map(|v| {
            v.get("error")
                .and_then(Value::as_str)
                .unwrap_or("the sandbox is not running")
                .to_string()
        })
}

/// The output of a `boat exec` call, or [`NotRunning`] when Boat refused it
/// because the sandbox is stopped.
fn exec_answer(o: Output) -> Result<Output> {
    if !o.status.success()
        && let Some(why) = machine_not_running(&o.stdout, &o.stderr)
    {
        return Err(NotRunning(why).into());
    }
    Ok(o)
}

/// The JSON objects of a JSON Lines output (other lines are skipped).
pub(crate) fn json_lines(out: &[u8]) -> Vec<Value> {
    String::from_utf8_lossy(out)
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l.trim()).ok())
        .filter(Value::is_object)
        .collect()
}

/// Why a `boat` command failed: its `{"event":"error"}` line, else the
/// last line of stderr, else its exit code.
pub(crate) fn failure(what: &str, o: &Output) -> String {
    let err = json_lines(&o.stdout).into_iter().rev().find_map(|v| {
        (v.get("event").and_then(Value::as_str) == Some("error"))
            .then(|| v.get("error").and_then(Value::as_str).map(str::to_string))
            .flatten()
    });
    let stderr = String::from_utf8_lossy(&o.stderr);
    let last = stderr.lines().rev().find(|l| !l.trim().is_empty());
    let why = err
        .or_else(|| last.map(|l| l.trim().to_string()))
        .unwrap_or_else(|| format!("exit code {:?}", o.status.code()));
    format!("{what} failed: {why}")
}

/// The sandbox id in `boat new --json`: the `ready` event's `id`, or the
/// last `id` any line gave (a sandbox made before the command failed).
pub(crate) fn new_sandbox_id(lines: &[Value]) -> Option<String> {
    let id = |v: &Value| {
        v.get("id")
            .or_else(|| v.get("sandbox").and_then(|s| s.get("id")))
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string)
    };
    lines
        .iter()
        .find(|v| v.get("event").and_then(Value::as_str) == Some("ready"))
        .and_then(id)
        .or_else(|| lines.iter().rev().find_map(id))
}

impl Provider for Boat {
    fn service(&self) -> &'static str {
        "boat"
    }

    fn create(&self, s: &NewSandbox) -> std::result::Result<String, CreateFailed> {
        let mut a: Vec<String> = vec![
            "new".into(),
            "--json".into(),
            "--name".into(),
            s.name.clone(),
            "--type".into(),
            s.machine.clone(),
            "--ttl".into(),
            s.ttl_secs.to_string(),
        ];
        if let Some(e) = &s.environment {
            a.extend(["--environment".into(), e.clone()]);
        }
        for (k, v) in &s.env {
            a.extend(["-e".into(), format!("{k}={v}")]);
        }
        for (there, here) in &s.files {
            a.extend(["--file".into(), format!("{there}={}", here.display())]);
        }
        if let Some(f) = &s.setup {
            a.extend(["--setup-file".into(), f.display().to_string()]);
        }
        let r = self.run_partial(&a).map_err(|e| CreateFailed {
            id: None,
            why: format!("{e:#}"),
        })?;
        // Cut off by its limit: the `created` line it printed may say which
        // sandbox Boat made.
        let id = new_sandbox_id(&json_lines(&r.stdout));
        let Some(o) = r.output() else {
            return Err(CreateFailed {
                id,
                why: format!("{:#}", timed_out(&a)),
            });
        };
        if !o.status.success() {
            return Err(CreateFailed {
                id,
                why: failure("boat new", &o),
            });
        }
        id.ok_or_else(|| CreateFailed {
            id: None,
            why: "boat new printed no sandbox id".into(),
        })
    }

    fn stop(&self, id: &str) -> Result<()> {
        let o = self.run(&["stop".into(), id.into(), "--json".into(), "--yes".into()])?;
        if !o.status.success() {
            bail!("{}", failure(&format!("boat stop {id}"), &o));
        }
        Ok(())
    }

    fn find(&self, name: &str) -> Result<Option<String>> {
        let o = self.run(&[
            "list".into(),
            "--filter".into(),
            "rp".into(),
            "--json".into(),
        ])?;
        if !o.status.success() {
            bail!("{}", failure("boat list", &o));
        }
        list_lookup(&String::from_utf8_lossy(&o.stdout), name)
    }

    fn inspect_hint(&self, id: &str, resume: bool) -> String {
        let look = format!(
            "`boat history {id}` lists the commands run in it, and each of the coordinator's \
             turns left its result in ~/.kontext-dispatch/turn-<n>/ there"
        );
        if resume {
            format!(
                "{look}; `boat resume {id}` brings the sandbox back to look inside (its data is \
                 kept 14 days; a resume does not bring the agent's credential back)"
            )
        } else {
            format!(
                "{look}; do not resume it before the agent's credential is replaced (`claude \
                 setup-token`, then `kontext dispatch credential set`): it could not be removed \
                 and may still be in the sandbox's saved state"
            )
        }
    }

    fn setup(&self, id: &str) -> Result<Setup> {
        let o = self.run(&["info".into(), id.into(), "--json".into()])?;
        if !o.status.success() {
            bail!("{}", failure(&format!("boat info {id}"), &o));
        }
        let v = json_lines(&o.stdout)
            .into_iter()
            .last()
            .context("boat info printed no JSON")?;
        Ok(setup_of(&v))
    }

    fn exec(&self, id: &str, cmd: &[String]) -> Result<ExecOut> {
        let mut a: Vec<String> = vec![
            "exec".into(),
            id.into(),
            "--json".into(),
            "--timeout".into(),
            "60".into(),
            "--".into(),
        ];
        a.extend(cmd.iter().cloned());
        let o = exec_answer(self.run(&a)?)?;
        let v = json_lines(&o.stdout).into_iter().last();
        match v {
            Some(v) if v.get("exitCode").is_some() || v.get("stdout").is_some() => exec_out(&v),
            _ => bail!("{}", failure(&format!("boat exec {id}"), &o)),
        }
    }

    fn detach(&self, id: &str, cmd: &[String]) -> Result<u32> {
        let mut a: Vec<String> = vec![
            "exec".into(),
            id.into(),
            "--detach".into(),
            "--json".into(),
            "--".into(),
        ];
        a.extend(cmd.iter().cloned());
        let o = exec_answer(self.run(&a)?)?;
        let pid = json_lines(&o.stdout)
            .into_iter()
            .rev()
            .find_map(|v| pid_of(&v));
        match pid {
            Some(p) if o.status.success() => Ok(p),
            _ => bail!("{}", failure(&format!("boat exec {id} --detach"), &o)),
        }
    }

    fn process(&self, id: &str, pid: u32) -> Result<Process> {
        let o = exec_answer(self.run(&[
            "exec".into(),
            id.into(),
            "--status".into(),
            pid.to_string(),
            "--json".into(),
        ])?)?;
        let v = json_lines(&o.stdout).into_iter().last();
        match v.as_ref().and_then(process_of) {
            Some(p) => Ok(p),
            None => bail!("{}", failure(&format!("boat exec {id} --status {pid}"), &o)),
        }
    }

    fn kill(&self, id: &str, pid: u32) -> Result<()> {
        let o = exec_answer(self.run(&[
            "exec".into(),
            id.into(),
            "--kill".into(),
            pid.to_string(),
            "--json".into(),
        ])?)?;
        if !o.status.success() {
            bail!("{}", failure(&format!("boat exec {id} --kill {pid}"), &o));
        }
        Ok(())
    }
}

/// The sandbox named `name` in `boat list --json`'s answer: one document
/// `{"pageInfo":{"hasMore":…},"sandboxes":[…]}` (Boat CLI 1.0.100), or a
/// bare list. Not found on a page that says more follow (the CLI has no
/// option to ask for them) is not known yet: an error, as is an answer of
/// another shape.
pub(crate) fn list_lookup(text: &str, name: &str) -> Result<Option<String>> {
    let v: Value = serde_json::from_str(text.trim())
        .ok()
        .or_else(|| json_lines(text.as_bytes()).into_iter().last())
        .context("boat list printed nothing this kontext can read")?;
    let (list, more) = match &v {
        Value::Array(_) => (&v, false),
        Value::Object(o) => match o.get("sandboxes") {
            Some(l @ Value::Array(_)) => (
                l,
                v.pointer("/pageInfo/hasMore").and_then(Value::as_bool) == Some(true),
            ),
            _ => bail!("boat list printed no list of sandboxes"),
        },
        _ => bail!("boat list printed no list of sandboxes"),
    };
    let mut found = None;
    find_named(list, name, &mut found);
    if found.is_none() && more {
        bail!(
            "boat list shows only its first page of sandboxes, and {name} is not on it: whether \
             it exists is not known yet"
        );
    }
    Ok(found)
}

/// The last object anywhere in `v` with this `name` and an `id`.
fn find_named(v: &Value, name: &str, found: &mut Option<String>) {
    match v {
        Value::Array(a) => a.iter().for_each(|x| find_named(x, name, found)),
        Value::Object(o) => {
            if o.get("name").and_then(Value::as_str) == Some(name)
                && let Some(id) = o.get("id").and_then(Value::as_str)
            {
                *found = Some(id.to_string());
            }
            o.values().for_each(|x| find_named(x, name, found));
        }
        _ => {}
    }
}

/// `boat exec --json`: `exitCode`, `stdout`, `stderr`. A command that timed
/// out or whose output was cut is an error, never a result.
pub(crate) fn exec_out(v: &Value) -> Result<ExecOut> {
    let flag = |k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
    if flag("timedOut") {
        bail!("the command timed out in the sandbox");
    }
    if flag("stdoutTruncated") || flag("stderrTruncated") {
        bail!("the command's output was cut");
    }
    if flag("launcherFailed") {
        bail!("the sandbox could not start the command");
    }
    let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    Ok(ExecOut {
        code: v
            .get("exitCode")
            .and_then(Value::as_i64)
            .map(|c| c as i32)
            .unwrap_or(-1),
        stdout: s("stdout"),
        stderr: s("stderr"),
    })
}

/// The process id in `boat exec --detach --json`: `pid`, else `processId`.
pub(crate) fn pid_of(v: &Value) -> Option<u32> {
    v.get("pid")
        .or_else(|| v.get("processId"))
        .and_then(Value::as_u64)
        .and_then(|p| u32::try_from(p).ok())
}

/// `boat exec --status --json`: `known: false` or `status: lost` is a
/// process the service does not know; `running: true` runs; otherwise it
/// ended, with `exitCode` (or 128 + the signal's number, when only the
/// signal is given). `None` when the object says none of that.
pub(crate) fn process_of(v: &Value) -> Option<Process> {
    let status = v.get("status").and_then(Value::as_str).unwrap_or("");
    if v.get("known").and_then(Value::as_bool) == Some(false) || status == "lost" {
        return Some(Process::Unknown);
    }
    match v.get("running").and_then(Value::as_bool) {
        Some(true) => return Some(Process::Running),
        Some(false) => {}
        None if status == "running" => return Some(Process::Running),
        None if status.is_empty() => return None,
        None => {}
    }
    let code = v
        .get("exitCode")
        .or_else(|| v.get("command").and_then(|c| c.get("exitCode")))
        .and_then(Value::as_i64)
        .map(|c| c as i32);
    let signal = v.get("signal").and_then(Value::as_str).map(|s| match s {
        "SIGKILL" => 137,
        "SIGTERM" => 143,
        "SIGINT" => 130,
        _ => 128,
    });
    Some(Process::Exited(code.or(signal).unwrap_or(-1)))
}

/// `setupStatus` and `setupError` of `boat info --json`: `pending` and
/// `running` are not done; `done` is; `failed` is, with its error. Any other
/// value is an error that carries it, so a change in Boat is seen.
pub(crate) fn setup_of(v: &Value) -> Setup {
    let s = |k: &str| {
        v.get(k)
            .or_else(|| v.get("sandbox").and_then(|x| x.get(k)))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    match s("setupStatus").as_str() {
        "pending" | "running" => Setup::Pending,
        "done" => Setup::Done,
        "failed" => Setup::Failed(match s("setupError") {
            e if e.is_empty() => "the start script failed".into(),
            e => e,
        }),
        "" => Setup::Failed("boat info has no setupStatus".into()),
        other => Setup::Failed(format!("unknown setupStatus {other:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_sandbox_id_comes_from_the_ready_event() {
        let lines = vec![
            json!({"event": "created", "id": "bx_a"}),
            json!({"event": "state", "state": "starting"}),
            json!({"event": "ready", "id": "bx_a", "ip": "x"}),
        ];
        assert_eq!(new_sandbox_id(&lines).as_deref(), Some("bx_a"));
        // Made, then failed before ready: the id it had.
        let lines = vec![json!({"event": "created", "id": "bx_b"})];
        assert_eq!(new_sandbox_id(&lines).as_deref(), Some("bx_b"));
        assert_eq!(new_sandbox_id(&[json!({"event": "state"})]), None);
    }

    #[test]
    fn setup_status() {
        assert_eq!(setup_of(&json!({"setupStatus": "running"})), Setup::Pending);
        assert_eq!(setup_of(&json!({"setupStatus": "done"})), Setup::Done);
        assert_eq!(
            setup_of(&json!({"setupStatus": "failed", "setupError": "exit 1"})),
            Setup::Failed("exit 1".into())
        );
        assert!(matches!(
            setup_of(&json!({"setupStatus": "weird"})),
            Setup::Failed(w) if w.contains("weird")
        ));
    }

    #[test]
    fn a_sandbox_is_found_by_name_in_any_shape() {
        let mut f = None;
        find_named(
            &json!({"sandboxes": [{"id": "bx_a", "name": "x"}, {"sandbox": {"id": "bx_b", "name": "kx"}}]}),
            "kx",
            &mut f,
        );
        assert_eq!(f.as_deref(), Some("bx_b"));
    }

    #[test]
    fn a_command_past_its_limit_is_killed_and_what_it_printed_is_kept() {
        let mut c = Command::new("sh");
        c.args([
            "-c",
            "echo '{\"event\":\"created\",\"id\":\"bx_a\"}'; sleep 5",
        ]);
        let t = Instant::now();
        let r = run_with_limit(c, Duration::from_millis(500)).unwrap();
        assert!(r.status.is_none());
        assert!(t.elapsed() < Duration::from_secs(3));
        // The review's finding 1: the id of the sandbox Boat made survives.
        assert_eq!(
            new_sandbox_id(&json_lines(&r.stdout)).as_deref(),
            Some("bx_a")
        );
        let mut c = Command::new("echo");
        c.arg("hi");
        let o = run_with_limit(c, Duration::from_secs(5))
            .unwrap()
            .output()
            .unwrap();
        assert_eq!(o.stdout, b"hi\n");
    }

    /// `boat list --filter rp --json` as Boat CLI 1.0.100 printed it.
    #[test]
    fn list_lookups() {
        let page = |more: bool, names: &[&str]| {
            json!({"pageInfo": {"hasMore": more, "limit": 200, "nextCursor": null},
                "sandboxes": names.iter().enumerate()
                    .map(|(i, n)| json!({"id": format!("bx_{i}"), "name": n, "state": "idle"}))
                    .collect::<Vec<_>>()})
            .to_string()
        };
        assert_eq!(
            list_lookup(&page(false, &["a", "kx"]), "kx")
                .unwrap()
                .as_deref(),
            Some("bx_1")
        );
        assert_eq!(list_lookup(&page(false, &["a"]), "kx").unwrap(), None);
        // More pages, and not on this one: not known.
        assert!(list_lookup(&page(true, &["a"]), "kx").is_err());
        assert_eq!(
            list_lookup(&page(true, &["kx"]), "kx").unwrap().as_deref(),
            Some("bx_0")
        );
        // Nothing readable is not "none".
        assert!(list_lookup("", "kx").is_err());
        assert!(list_lookup("{\"error\":\"x\"}", "kx").is_err());
    }

    #[test]
    fn a_stopped_sandbox_is_not_running() {
        let out = br#"{"code":"machine_not_running","error":"Sandbox bx_a is stopped. Run `boat resume bx_a`, then retry.","event":"error","status":409}"#;
        assert!(
            machine_not_running(out, b"")
                .unwrap()
                .contains("is stopped")
        );
        assert!(machine_not_running(b"{\"code\":\"x\"}", b"").is_none());
    }

    /// The shapes `boat exec` printed in the probe of 2026-11-16 (CLI 1.0.100).
    #[test]
    fn exec_shapes_from_real_boat() {
        let detach = json!({"command": "x", "commandId": "cmd_uzwf4xablasb", "pid": 7203,
            "processId": 7203, "logPath": "/home/user/.ascii/processes/7203.log", "success": true});
        assert_eq!(pid_of(&detach), Some(7203));
        let running = json!({"known": true, "running": true, "status": "running",
            "exitCode": null, "signal": null, "pid": 7203});
        assert_eq!(process_of(&running), Some(Process::Running));
        let exited = json!({"known": true, "running": false, "status": "exited",
            "exitCode": 7, "signal": null});
        assert_eq!(process_of(&exited), Some(Process::Exited(7)));
        let killed = json!({"known": true, "running": false, "status": "exited",
            "exitCode": 143, "signal": "SIGTERM"});
        assert_eq!(process_of(&killed), Some(Process::Exited(143)));
        let lost = json!({"known": false, "running": false, "status": "lost"});
        assert_eq!(process_of(&lost), Some(Process::Unknown));
        assert_eq!(process_of(&json!({"error": "x"})), None);
        let sync = json!({"commandId": "cmd_y", "exitCode": 0, "stdout": "ok\n", "stderr": "",
            "stdoutTruncated": false, "success": true, "timedOut": false});
        let o = exec_out(&sync).unwrap();
        assert_eq!((o.code, o.stdout.as_str()), (0, "ok\n"));
        let cut = json!({"exitCode": 0, "stdout": "", "stdoutTruncated": true});
        assert!(exec_out(&cut).is_err());
        let slow = json!({"exitCode": null, "stdout": "", "timedOut": true});
        assert!(exec_out(&slow).is_err());
    }
}
