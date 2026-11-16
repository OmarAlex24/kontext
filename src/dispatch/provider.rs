//! The sandbox service behind `kontext dispatch`. Everything that depends on
//! the service is behind [`Provider`], so another one can be added next to
//! [`Boat`] without touching the dispatcher.
//!
//! [`Boat`] runs the `boat` CLI (`$KONTEXT_BOAT`, or `boat` on the `PATH`):
//! the tests point it at a fake. It never passes a secret in an argument or a
//! variable: a secret goes as a file (`--file`).

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

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

/// A prompt for the coordinator.
#[derive(Debug, Clone)]
pub struct Turn<'a> {
    pub sandbox: &'a str,
    /// Continue this conversation; `None` starts a new one.
    pub conversation: Option<&'a str>,
    /// The agent and the model, as the service names them.
    pub provider: &'a str,
    pub model: &'a str,
    pub text: &'a str,
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

/// A sandbox service.
pub trait Provider {
    /// Its name in `[dispatch] service` and in the state.
    fn service(&self) -> &'static str;
    /// Makes a sandbox and returns its id.
    fn create(&self, s: &NewSandbox) -> std::result::Result<String, CreateFailed>;
    /// Stops it, keeping its data.
    fn stop(&self, id: &str) -> Result<()>;
    /// How its start script went.
    fn setup(&self, id: &str) -> Result<Setup>;
    /// Prompts the coordinator and returns when its turn ends: `Ok` only
    /// when the service says the turn ended (a turn that cannot be followed
    /// to its end, for a network failure say, is an error). Calls
    /// `conversation` with the conversation's id as soon as it is known.
    fn prompt(&self, t: &Turn, conversation: &mut dyn FnMut(&str)) -> Result<()>;
    /// How a person looks at what happened in a stopped sandbox (one line).
    fn inspect_hint(&self, id: &str) -> String;
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

    fn run(&self, args: &[String]) -> Result<Output> {
        self.command(args)
            .output()
            .with_context(|| format!("could not run {} (is Boat's CLI installed?)", self.bin))
    }
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
        let o = self.run(&a).map_err(|e| CreateFailed {
            id: None,
            why: format!("{e:#}"),
        })?;
        let id = new_sandbox_id(&json_lines(&o.stdout));
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

    fn inspect_hint(&self, id: &str) -> String {
        format!(
            "`boat events {id}` shows what the coordinator did; `boat resume {id}` brings the \
             sandbox back (its data is kept 14 days)"
        )
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

    fn prompt(&self, t: &Turn, conversation: &mut dyn FnMut(&str)) -> Result<()> {
        use std::io::{BufRead, BufReader};
        let mut a: Vec<String> = vec![
            "prompt".into(),
            t.sandbox.into(),
            "--json".into(),
            "--provider".into(),
            t.provider.into(),
            "--model".into(),
            t.model.into(),
        ];
        match t.conversation {
            Some(c) => a.extend(["--resume".into(), c.into()]),
            None => a.push("--new".into()),
        }
        a.push(t.text.into());
        let mut child = self
            .command(&a)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("could not run {}", self.bin))?;
        let stderr = child.stderr.take().unwrap();
        let errs = std::thread::spawn(move || {
            let mut s = String::new();
            let _ = std::io::Read::read_to_string(&mut BufReader::new(stderr), &mut s);
            s
        });
        let mut seen = false;
        let mut last_error = None;
        for line in BufReader::new(child.stdout.take().unwrap()).lines() {
            let Ok(line) = line else { break };
            let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
                continue;
            };
            if !seen && let Some(c) = conversation_id(&v) {
                seen = true;
                conversation(&c);
            }
            if v.get("event").and_then(Value::as_str) == Some("error") {
                last_error = v.get("error").and_then(Value::as_str).map(str::to_string);
            }
        }
        let status = child.wait()?;
        let stderr = errs.join().unwrap_or_default();
        if !status.success() {
            let why = last_error
                .or_else(|| {
                    stderr
                        .lines()
                        .rev()
                        .find(|l| !l.trim().is_empty())
                        .map(|l| l.trim().to_string())
                })
                .unwrap_or_else(|| format!("exit code {:?}", status.code()));
            bail!("boat prompt failed: {why}");
        }
        Ok(())
    }
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

/// The conversation's id in a line of `boat prompt --json`: `conversationId`,
/// at the top or under `data`.
pub(crate) fn conversation_id(v: &Value) -> Option<String> {
    v.get("conversationId")
        .or_else(|| v.get("data").and_then(|d| d.get("conversationId")))
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
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
    fn setup_status_and_conversation_ids() {
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
        let q = json!({"event": "queued", "conversationId": "cv_1"});
        assert_eq!(conversation_id(&q).as_deref(), Some("cv_1"));
        let d = json!({"event": "prompt", "data": {"conversationId": "cv_2"}});
        assert_eq!(conversation_id(&d).as_deref(), Some("cv_2"));
    }
}
