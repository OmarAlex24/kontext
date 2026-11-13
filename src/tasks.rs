//! Epics and tasks (phase 3, DESIGN §4.3, §7.2 and §7.3): the pieces that share
//! state, commands and views, and the commands that write
//! (`epic add`, `task add`, `assign`, `release`, `reopen`, `next --claim`,
//! `done` and `fact`).
//!
//! Rules:
//! - An epic is named by `fields.name` (a slug of the title if none is given), and
//!   its reader is `epic:<name>`. Its default coordinator is
//!   `epic:<name>`, the actor name from DESIGN §4.2.
//! - A task stores its epic's `id` in `fields.epic`; a decision or a fact touches
//!   an epic if its `fields.epic` is the `id` or the name, if it cites them in
//!   `refs`, or if it carries the name as a tag.
//! - `done` needs evidence: at least one valid item of
//!   `{type: command|file|pr|url, value, exit_code?, excerpt?}`. A command
//!   without `exit_code` 0 does not count.

use std::fmt;
use std::io::Read;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::cursor::{Frontier, validate_reader};
use crate::hlc::Hlc;
use crate::model::{Actor, Entry, RawEntry};
use crate::state::{State, Status, string_list};
use crate::store::{Batch, Refused, Store};

/// Default `[claims] lease`: 2 h.
pub const DEFAULT_LEASE_MS: i64 = 2 * 3600 * 1000;

/// How many lines of `done --run` output are kept.
pub const EXCERPT_LINES: usize = 20;

/// The state of a task. The variant names are Spanish for now (they are `pub`
/// and used across the crate); the serialized and displayed words are English.
#[derive(Serialize, Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum TaskStatus {
    #[default]
    #[serde(rename = "ready")]
    Ready,
    #[serde(rename = "blocked")]
    Blocked,
    #[serde(rename = "assigned")]
    Assigned,
    #[serde(rename = "claimed")]
    Claimed,
    #[serde(rename = "closed")]
    Closed,
}

impl TaskStatus {
    pub const ALL: [TaskStatus; 5] = [
        TaskStatus::Ready,
        TaskStatus::Blocked,
        TaskStatus::Assigned,
        TaskStatus::Claimed,
        TaskStatus::Closed,
    ];

    pub fn label(self) -> &'static str {
        match self {
            TaskStatus::Ready => "ready",
            TaskStatus::Blocked => "blocked",
            TaskStatus::Assigned => "assigned",
            TaskStatus::Claimed => "claimed",
            TaskStatus::Closed => "closed",
        }
    }

    pub fn plural(self) -> &'static str {
        match self {
            TaskStatus::Ready => "ready",
            TaskStatus::Blocked => "blocked",
            TaskStatus::Assigned => "assigned",
            TaskStatus::Claimed => "claimed",
            TaskStatus::Closed => "closed",
        }
    }
}

impl fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// A `done` refusal for stale context (exits with 3).
#[derive(Debug)]
pub struct Stale(pub String);

impl fmt::Display for Stale {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Stale {}

fn refused<T>(msg: impl Into<String>) -> Result<T> {
    Err(Refused(msg.into()).into())
}

// ---------------------------------------------------------------- time

/// Now, in ms: `KONTEXT_NOW` (ISO-8601, for tests) or the clock.
pub fn now_ms() -> i64 {
    std::env::var("KONTEXT_NOW")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .and_then(|s| s.trim().parse::<jiff::Timestamp>().ok())
        .unwrap_or_else(jiff::Timestamp::now)
        .as_millisecond()
}

/// The physical time of an `hlc`, in ms (0 if it cannot be parsed).
pub fn hlc_ms(hlc: &str) -> i64 {
    hlc.parse::<Hlc>().map(|h| h.millis).unwrap_or(0)
}

/// `2026-11-03 18:22 UTC`.
pub fn ms_text(ms: i64) -> String {
    jiff::Timestamp::from_millisecond(ms)
        .map(|t| t.strftime("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_else(|_| "?".into())
}

/// "5 min ago", "2 h ago", "3 days ago".
pub fn ago(now: i64, then: i64) -> String {
    let d = duration_text(now - then);
    if d == "0 min" {
        "less than 1 min ago".into()
    } else {
        format!("{d} ago")
    }
}

/// "5 min", "2 h", "3 days" (rounded down).
pub fn duration_text(ms: i64) -> String {
    let mins = ms.max(0) / 60_000;
    if mins < 60 {
        format!("{mins} min")
    } else if mins < 48 * 60 {
        format!("{} h", mins / 60)
    } else {
        format!("{} days", mins / (24 * 60))
    }
}

/// `2h`, `90m`, `1h30m`, `45s`, `1d` → ms.
pub fn parse_duration_ms(s: &str) -> Result<i64> {
    let s = s.trim();
    if s.is_empty() {
        bail!("empty duration");
    }
    let mut total: i64 = 0;
    let mut num = String::new();
    for c in s.chars() {
        if c.is_ascii_digit() {
            num.push(c);
            continue;
        }
        if c.is_whitespace() {
            continue;
        }
        let unit = match c {
            'd' => 86_400_000,
            'h' => 3_600_000,
            'm' => 60_000,
            's' => 1_000,
            _ => bail!("invalid duration {s:?}: use for example 2h, 90m or 1h30m"),
        };
        let n: i64 = num.parse().map_err(|_| {
            anyhow::anyhow!("invalid duration {s:?}: missing the number before {c}")
        })?;
        total += n * unit;
        num.clear();
    }
    if !num.is_empty() {
        bail!("invalid duration {s:?}: missing the unit (d, h, m or s)");
    }
    if total <= 0 {
        bail!("invalid duration {s:?}: it must be greater than zero");
    }
    Ok(total)
}

// ---------------------------------------------------------------- epics

/// A short name for the reader `epic:<name>`: lowercase, no accents, with dashes.
pub fn slug(title: &str) -> String {
    let mut out = String::new();
    for c in title.trim().to_lowercase().chars() {
        let c = match c {
            'á' | 'à' | 'ä' | 'â' => 'a',
            'é' | 'è' | 'ë' | 'ê' => 'e',
            'í' | 'ì' | 'ï' | 'î' => 'i',
            'ó' | 'ò' | 'ö' | 'ô' => 'o',
            'ú' | 'ù' | 'ü' | 'û' => 'u',
            'ñ' => 'n',
            c => c,
        };
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-').to_string();
    if out.is_empty() { "epic".into() } else { out }
}

/// An epic's name: `fields.name`, or the slug of its title.
pub fn epic_name(e: &Entry) -> String {
    e.fields
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| slug(&e.title))
}

/// An epic's coordinator: `fields.coordinator`, or `epic:<name>`.
pub fn coordinator_of(e: &Entry) -> String {
    e.fields
        .get("coordinator")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("epic:{}", epic_name(e)))
}

/// Is `holder` (`agent:impl-1` or `impl-1`) this actor?
pub fn holder_matches(holder: &str, actor: &Actor) -> bool {
    let holder = holder.trim();
    holder == actor.name || holder == format!("{}:{}", actor.kind, actor.name)
}

/// Is this the epic's coordinator? By name (the one in `fields.coordinator`
/// or `epic:<name>`).
pub fn is_coordinator(actor: &Actor, epic: &Entry) -> bool {
    holder_matches(&coordinator_of(epic), actor)
        || holder_matches(&format!("epic:{}", epic_name(epic)), actor)
}

/// Does the entry touch the epic? `fields.epic` (id, name or `epic:<name>`),
/// a reference, or the name as a tag.
pub fn touches_epic(e: &Entry, epic: &Entry) -> bool {
    let name = epic_name(epic);
    let tagged = format!("epic:{name}");
    let field = e
        .fields
        .get("epic")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default();
    field == epic.id
        || field == name
        || field == tagged
        || e.refs.iter().any(|r| r == &epic.id)
        || e.tags.iter().any(|t| t == &name || t == &tagged)
}

// ---------------------------------------------------------------- evidence

/// Checks the evidence of a `done`: at least one valid item.
pub fn check_evidence(v: Option<&Value>) -> std::result::Result<(), String> {
    let items = match v {
        Some(Value::Array(a)) if !a.is_empty() => a,
        _ => return Err("closing without evidence (fields.evidence is empty)".into()),
    };
    let mut why = Vec::new();
    for it in items {
        match evidence_item_ok(it) {
            Ok(()) => return Ok(()),
            Err(e) => why.push(e),
        }
    }
    Err(format!("no evidence is valid: {}", why.join("; ")))
}

fn evidence_item_ok(it: &Value) -> std::result::Result<(), String> {
    let ty = it.get("type").and_then(Value::as_str).unwrap_or_default();
    let value = it
        .get("value")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default();
    if value.is_empty() {
        return Err(format!("{ty:?} has no value"));
    }
    match ty {
        "command" => match it.get("exit_code").and_then(Value::as_i64) {
            Some(0) => Ok(()),
            Some(n) => Err(format!("command {value:?} exited with {n}")),
            None => Err(format!("command {value:?} has no exit_code")),
        },
        "file" | "pr" | "url" | "image" => Ok(()),
        other => Err(format!(
            "unknown type {other:?} (use command, file, pr, url or image)"
        )),
    }
}

/// A line with the evidence: `` `pnpm test` (0); file src/a.rs ``.
pub fn evidence_summary(v: Option<&Value>) -> String {
    let Some(Value::Array(items)) = v else {
        return "(none)".into();
    };
    let parts: Vec<String> = items
        .iter()
        .map(|it| {
            let ty = it.get("type").and_then(Value::as_str).unwrap_or("?");
            let value = it.get("value").and_then(Value::as_str).unwrap_or("");
            match ty {
                "command" => match it.get("exit_code").and_then(Value::as_i64) {
                    Some(n) => format!("`{value}` ({n})"),
                    None => format!("`{value}` (no code)"),
                },
                "image" => {
                    let name = it.get("name").and_then(Value::as_str).unwrap_or(value);
                    match it.get("screen").and_then(Value::as_str) {
                        Some(s) => format!("screenshot {s} ({name})"),
                        None => format!("screenshot {name}"),
                    }
                }
                _ => format!("{ty} {value}"),
            }
        })
        .collect();
    if parts.is_empty() {
        "(none)".into()
    } else {
        parts.join("; ")
    }
}

/// `cmd:"pnpm test"=0`, `file:path`, `pr:URL`, `url:URL`, or an image:
/// `screenshot.png` (or `image:screenshot.png`). The image comes out as
/// `{"type":"image","path":…}`: whoever writes the `done` copies it to `media/`
/// ([`crate::media::store`]) and replaces it with its path and hash.
pub fn parse_evidence(spec: &str) -> Result<Value> {
    let spec = spec.trim();
    let image = match spec.split_once(':') {
        Some(("image" | "img", rest)) => Some(rest.trim()),
        Some(("cmd" | "command" | "file" | "pr" | "url" | "http" | "https", _)) => None,
        _ if crate::media::is_image_path(spec) => Some(spec),
        _ => None,
    };
    if let Some(path) = image {
        if path.is_empty() {
            bail!("invalid evidence {spec:?}: missing the image path");
        }
        return Ok(json!({"type": "image", "path": path}));
    }
    let Some((ty, rest)) = spec.split_once(':') else {
        bail!(
            "invalid evidence {spec:?}: use cmd:\"<command>\"=<code>, file:<path>, pr:<url>, url:<url> or an image (screenshot.png)"
        );
    };
    let rest = rest.trim();
    match ty {
        "cmd" | "command" => {
            let (cmd, code) = match rest.rsplit_once('=') {
                Some((c, n)) if n.trim().parse::<i64>().is_ok() => {
                    (c.trim(), Some(n.trim().parse::<i64>().unwrap()))
                }
                _ => (rest, None),
            };
            let cmd = cmd
                .strip_prefix('"')
                .and_then(|c| c.strip_suffix('"'))
                .unwrap_or(cmd);
            let mut v = json!({"type": "command", "value": cmd});
            if let Some(n) = code {
                v["exit_code"] = json!(n);
            }
            Ok(v)
        }
        "file" | "pr" | "url" => Ok(json!({"type": ty, "value": rest})),
        // `https://…` without a prefix: a url.
        "https" | "http" => Ok(json!({"type": "url", "value": spec})),
        other => {
            bail!("invalid evidence {spec:?}: unknown type {other:?} (cmd, file, pr, url or image)")
        }
    }
}

/// The `file:` evidence must exist (relative to `cwd`) when the `done` is
/// written. Accepts `path:line`.
pub fn check_file_evidence(evidence: &[Value], cwd: &std::path::Path) -> Result<()> {
    for e in evidence {
        if e.get("type").and_then(Value::as_str) != Some("file") {
            continue;
        }
        let v = e.get("value").and_then(Value::as_str).unwrap_or("").trim();
        let exists = |p: &str| !p.is_empty() && cwd.join(p).exists();
        let without_line = v
            .rsplit_once(':')
            .filter(|(_, n)| {
                let n = n.split('-').next().unwrap_or("");
                !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())
            })
            .map(|(p, _)| p);
        if !exists(v) && !without_line.is_some_and(exists) {
            return refused(format!(
                "evidence file:{v}: does not exist (from {}). Use a path that exists or other evidence",
                cwd.display()
            ));
        }
    }
    Ok(())
}

/// `--evidence-json`: one object or a list of objects.
pub fn parse_evidence_json(s: &str) -> Result<Vec<Value>> {
    let v: Value = serde_json::from_str(s).context("--evidence-json is not valid JSON")?;
    match v {
        Value::Array(a) => Ok(a),
        o @ Value::Object(_) => Ok(vec![o]),
        _ => bail!("--evidence-json must be an object or a list of objects"),
    }
}

/// What `done --run` left behind.
#[derive(Debug, Clone)]
pub struct RunResult {
    pub command: String,
    pub exit_code: i32,
    pub excerpt: String,
}

impl RunResult {
    pub fn evidence(&self) -> Value {
        json!({
            "type": "command",
            "value": self.command,
            "exit_code": self.exit_code,
            "excerpt": self.excerpt,
        })
    }
}

/// Runs `cmd` with `sh -c` and keeps its exit code and the last 20 lines of
/// output (stdout and stderr together).
pub fn run_command(cmd: &str) -> Result<RunResult> {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(format!("exec 2>&1; {cmd}"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .with_context(|| format!("could not run {cmd:?}"))?;
    let mut out = Vec::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_end(&mut out)
        .context("could not read the command's output")?;
    let status = child.wait()?;
    let text = String::from_utf8_lossy(&out);
    let lines: Vec<&str> = text.lines().collect();
    let from = lines.len().saturating_sub(EXCERPT_LINES);
    Ok(RunResult {
        command: cmd.to_string(),
        exit_code: status.code().unwrap_or(-1),
        excerpt: lines[from..].join("\n"),
    })
}

// ---------------------------------------------------------------- stale context

/// What came in after `since` and touches the task (DESIGN §7.3): a decision
/// from its epic (or one that cites the task or the epic), the replacement of
/// a goal the task cites, or the answer to a question about the task.
pub fn stale_entries<'a>(st: &'a State, task: &str, since: &Frontier) -> Vec<&'a Entry> {
    let Some(t) = st.get(task) else {
        return Vec::new();
    };
    let epic = st.epic_of(t);
    let goals = string_list(t.fields.get("goals"));
    let touches = |e: &Entry| -> bool {
        let refs_task = e.refs.iter().any(|r| r == task)
            || e.fields.get("task").and_then(Value::as_str) == Some(task);
        let of_epic = epic.is_some_and(|ep| touches_epic(e, ep));
        match e.kind.as_str() {
            "decision" => st.counts(&e.id) && (refs_task || of_epic),
            "accept" => {
                st.counts(&e.id)
                    && e.refs.first().and_then(|p| st.get(p)).is_some_and(|p| {
                        p.refs.iter().any(|r| r == task)
                            || epic.is_some_and(|ep| touches_epic(p, ep))
                    })
            }
            "supersede" => st.counts(&e.id) && e.refs.first().is_some_and(|g| goals.contains(g)),
            // Also the answer to a question this task joined (a `kontext ask`
            // that repeated an open question).
            "answer" => e.refs.first().and_then(|q| st.get(q)).is_some_and(|q| {
                q.fields.get("task").and_then(Value::as_str) == Some(task)
                    || q.fields.get("feature").and_then(Value::as_str) == Some(task)
                    || st.joined.get(&q.id).into_iter().flatten().any(|c| {
                        st.get(c)
                            .and_then(|c| c.fields.get("task"))
                            .and_then(Value::as_str)
                            == Some(task)
                    })
            }),
            _ => false,
        }
    };
    st.order
        .iter()
        .filter_map(|id| st.get(id))
        .filter(|e| !since.covers(e) && touches(e))
        .collect()
}

// ---------------------------------------------------------------- commands

fn raw(kind: &str, actor: &Actor, title: &str) -> RawEntry {
    let mut r = RawEntry::new(kind, title);
    r.actor = Some(actor.clone());
    r
}

fn write(store: &mut Store, f: impl FnOnce(&mut Batch) -> Result<()>) -> Result<Vec<Entry>> {
    let ((), written) = store.transact(f)?;
    Ok(written)
}

/// Resolves goals: they must exist and be goals.
pub fn resolve_goals(st: &State, ids: &[String]) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for g in ids {
        let id = st.resolve(g)?;
        if st.kind_of(&id) != Some("goal") {
            return refused(format!(
                "--goal {id}: is {}, not a goal",
                st.kind_of(&id).unwrap_or("?")
            ));
        }
        if !out.contains(&id) {
            out.push(id);
        }
    }
    Ok(out)
}

/// Drift warnings for goals that are not in force and signed.
pub fn goal_warnings(st: &State, goals: &[String]) -> Vec<String> {
    if goals.is_empty() {
        return vec![
            "drift: it cites no goal. Every task must serve a goal in the charter; \
             add one with --goal or ask for one with `kontext ask --to pm`"
                .into(),
        ];
    }
    let mut out = Vec::new();
    for g in goals {
        match st.status_of(g) {
            Some(Status::Signed) => {}
            Some(Status::Unsigned) => out.push(format!(
                "unsigned goal: {g} is not signed by an owner yet (`kontext sign {g}`)"
            )),
            Some(Status::Superseded) => out.push(format!(
                "drift: {g} was superseded by {}",
                st.superseded_by.get(g).map(String::as_str).unwrap_or("?")
            )),
            Some(s) => out.push(format!("drift: {g} is {}", s.label())),
            None => out.push(format!("drift: {g} does not exist")),
        }
    }
    out
}

/// What was written and the warnings to show.
pub struct Written {
    pub entries: Vec<Entry>,
    pub warnings: Vec<String>,
}

/// `kontext epic add`.
pub fn epic_add(
    store: &mut Store,
    actor: &Actor,
    title: &str,
    goals: &[String],
    coordinator: Option<&str>,
    name: Option<&str>,
    after: &[String],
) -> Result<Written> {
    if title.trim().is_empty() {
        bail!("the epic needs a title");
    }
    let name = match name.map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) => n.strip_prefix("epic:").unwrap_or(n).to_string(),
        None => slug(title),
    };
    validate_reader(&format!("epic:{name}"))?;
    let mut warnings = Vec::new();
    let entries = write(store, |b| {
        let st = b.state()?;
        if let Some(other) = st.epics().into_iter().find(|e| epic_name(e) == name) {
            return refused(format!(
                "there is already an epic named {name} ({}); use --name to give it another name",
                other.id
            ));
        }
        let goals = resolve_goals(st, goals)?;
        warnings = goal_warnings(st, &goals);
        let deps = resolve_after(st, &name, after, &mut warnings)?;
        let mut r = raw("epic", actor, title.trim());
        r.fields.insert("name".into(), json!(name));
        r.fields.insert("goals".into(), json!(goals));
        let coord = coordinator
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("epic:{name}"));
        r.fields.insert("coordinator".into(), json!(coord));
        if !deps.is_empty() {
            r.fields.insert("depends_on".into(), json!(deps));
        }
        r.refs = goals;
        b.push(r)?;
        Ok(())
    })?;
    Ok(Written { entries, warnings })
}

/// The epics that must close first (`epic add --after`). One that already
/// exists is stored by its `id`; a name that does not exist yet is stored as
/// is, with a warning, so the epic map can be built in any order.
/// `kontext plan` marks it as missing until it is created.
fn resolve_after(
    st: &State,
    own: &str,
    after: &[String],
    warnings: &mut Vec<String>,
) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for a in after {
        let a = a.trim();
        let name = a.strip_prefix("epic:").unwrap_or(a);
        if name.is_empty() {
            continue;
        }
        if name == own {
            return refused(format!("--after {a}: an epic cannot come after itself"));
        }
        let dep = match st.find_epic(name) {
            Ok(id) => id,
            Err(_) if looks_like_id(name) => {
                return refused(format!("--after {a}: there is no epic with that id"));
            }
            Err(_) => {
                validate_reader(&format!("epic:{name}"))
                    .map_err(|e| anyhow::anyhow!("--after {a}: {e}"))?;
                warnings.push(format!(
                    "there is no epic {name} yet; `kontext plan` marks it as missing \
                     until it is created with `--name {name}`"
                ));
                name.to_string()
            }
        };
        if !out.contains(&dep) {
            out.push(dep);
        }
    }
    // An earlier epic that waits for this one by name would close a cycle.
    if !out.is_empty() {
        let mut nodes = crate::plan::nodes(st);
        nodes.push(crate::plan::Node {
            id: "(new)".into(),
            name: own.to_string(),
            title: String::new(),
            after: out.clone(),
            active: true,
            tasks: 0,
            closed_tasks: 0,
            features: 0,
            done_features: 0,
        });
        let plan = crate::plan::compute(&nodes);
        if let Some(c) = plan.cycles.iter().find(|c| c.iter().any(|n| n == own)) {
            let mut l = c.clone();
            l.push(c[0].clone());
            return refused(format!(
                "--after: would close a cycle ({}); remove the extra dependency",
                l.join(" → ")
            ));
        }
    }
    Ok(out)
}

/// Does it look like an entry id (`E-61e`)?
fn looks_like_id(s: &str) -> bool {
    s.len() > 2
        && s.as_bytes()[1] == b'-'
        && s.as_bytes()[0].is_ascii_uppercase()
        && s[2..].chars().all(|c| c.is_ascii_hexdigit())
}

/// What `kontext task add` asks for.
#[derive(Debug, Clone, Default)]
pub struct TaskArgs {
    pub title: String,
    /// The epic (`--epic`); optional when `feature` is given.
    pub epic: Option<String>,
    /// The feature it hangs from (`--feature`).
    pub feature: Option<String>,
    pub goals: Vec<String>,
    pub depends_on: Vec<String>,
    pub acceptance: Option<String>,
    pub body: String,
    pub tags: Vec<String>,
}

/// `kontext task add`. Without `--goal`, the epic's goals (with `--feature`,
/// the feature's). If none remain, it still writes the task and warns about
/// drift. With `--feature`, the epic is the feature's; an `--epic` that names
/// another one is refused.
pub fn task_add(store: &mut Store, actor: &Actor, a: &TaskArgs) -> Result<Written> {
    if a.title.trim().is_empty() {
        bail!("the task needs a title");
    }
    let mut warnings = Vec::new();
    let entries = write(store, |b| {
        let st = b.state()?;
        let given = a.epic.as_deref().map(str::trim).filter(|e| !e.is_empty());
        let feature = match a
            .feature
            .as_deref()
            .map(str::trim)
            .filter(|f| !f.is_empty())
        {
            Some(f) => Some(crate::features::in_force(st, f)?),
            None => None,
        };
        let feature_epic = feature
            .as_deref()
            .and_then(|f| st.get(f))
            .and_then(|f| f.fields.get("epic"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let epic_id = match (given, &feature_epic) {
            (Some(e), Some(fe)) => {
                let id = st.find_epic(e)?;
                if &id != fe {
                    return refused(format!(
                        "--epic {e} ({id}) is not the epic of {} ({fe}); leave --epic out or use the feature's",
                        feature.as_deref().unwrap_or("?")
                    ));
                }
                id
            }
            (Some(e), None) => st.find_epic(e)?,
            (None, Some(fe)) => fe.clone(),
            (None, None) => bail!("missing --epic <epic> (or --feature <U>)"),
        };
        if st.status_of(&epic_id) != Some(Status::Active) {
            return refused(format!(
                "the epic {epic_id} is not in force ({})",
                st.status_of(&epic_id).map(|s| s.label()).unwrap_or("?")
            ));
        }
        let epic = st.get(&epic_id).unwrap();
        let goals = if a.goals.is_empty() {
            let from = feature.as_deref().and_then(|f| st.get(f)).unwrap_or(epic);
            string_list(from.fields.get("goals"))
        } else {
            resolve_goals(st, &a.goals)?
        };
        let mut deps = Vec::new();
        for d in &a.depends_on {
            let id = st.resolve(d)?;
            if st.kind_of(&id) != Some("task") {
                return refused(format!(
                    "--depends-on {id}: is {}, not a task",
                    st.kind_of(&id).unwrap_or("?")
                ));
            }
            if !deps.contains(&id) {
                deps.push(id);
            }
        }
        warnings = goal_warnings(st, &goals);
        let mut r = raw("task", actor, a.title.trim());
        r.body = a.body.clone();
        r.tags = a.tags.clone();
        r.fields.insert("epic".into(), json!(epic_id));
        r.fields.insert("goals".into(), json!(goals));
        r.fields.insert("depends_on".into(), json!(deps));
        r.fields.insert(
            "acceptance".into(),
            json!(a.acceptance.as_deref().unwrap_or("").trim()),
        );
        if let Some(f) = &feature {
            r.fields.insert("feature".into(), json!(f));
        }
        r.refs = vec![epic_id];
        b.push(r)?;
        Ok(())
    })?;
    Ok(Written { entries, warnings })
}

fn open_task(st: &State, id: &str) -> Result<String> {
    let id = st.resolve(id)?;
    if st.kind_of(&id) != Some("task") {
        return refused(format!(
            "{id} is {}, not a task",
            st.kind_of(&id).unwrap_or("?")
        ));
    }
    Ok(id)
}

fn epic_of_task<'a>(st: &'a State, task: &str) -> Option<&'a Entry> {
    st.get(task).and_then(|t| st.epic_of(t))
}

/// `kontext assign <T> --to <name>`: only the coordinator, the pm or an owner.
pub fn assign(store: &mut Store, actor: &Actor, task: &str, to: &str) -> Result<Vec<Entry>> {
    let to = to.trim();
    if to.is_empty() {
        bail!("missing --to <name>");
    }
    write(store, |b| {
        let st = b.state()?;
        let id = open_task(st, task)?;
        if !st.policy.manages(actor, epic_of_task(st, &id)) {
            return refused(format!(
                "only the epic's coordinator, the pm or an owner can assign it ({} is not one of them). \
                 To work on it yourself, claim it with `kontext next --epic <epic> --claim`",
                actor.short()
            ));
        }
        if st.task_status(&id) == Some(TaskStatus::Closed) {
            return refused(format!(
                "{id} is already closed; reopen it with `kontext reopen {id} --why …`"
            ));
        }
        let mut r = raw("assign", actor, &format!("Assigns {id} to {to}"));
        r.refs = vec![id];
        r.fields.insert("to".into(), json!(to));
        b.push(r)?;
        Ok(())
    })
}

/// `kontext release <T>`: the holder, the coordinator, the pm or an owner.
pub fn release(store: &mut Store, actor: &Actor, task: &str, why: &str) -> Result<Vec<Entry>> {
    write(store, |b| {
        let st = b.state()?;
        let id = open_task(st, task)?;
        let info = st.task(&id).cloned().unwrap_or_default();
        let Some(h) = info.holder else {
            return refused(format!("{id} has no owner ({})", info.status));
        };
        if !holder_matches(&h.actor, actor) && !st.policy.manages(actor, epic_of_task(st, &id)) {
            return refused(format!(
                "{id} is held by {}; only the holder, the epic's coordinator, the pm or an owner can release it",
                h.actor
            ));
        }
        let mut r = raw("release", actor, &format!("Releases {id}"));
        r.why = why.trim().to_string();
        r.refs = vec![id];
        b.push(r)?;
        Ok(())
    })
}

/// `kontext reopen <T> --why`.
pub fn reopen(store: &mut Store, actor: &Actor, task: &str, why: &str) -> Result<Vec<Entry>> {
    if why.trim().is_empty() {
        bail!("reopening needs --why");
    }
    write(store, |b| {
        let st = b.state()?;
        let id = st.resolve(task)?;
        if st.kind_of(&id) == Some("feature") {
            crate::features::check_reopen(st, &id)?;
            let mut r = raw("reopen", actor, &format!("Reopens {id}"));
            r.why = why.trim().to_string();
            r.refs = vec![id];
            b.push(r)?;
            return Ok(());
        }
        let id = open_task(st, &id)?;
        if st.task_status(&id) != Some(TaskStatus::Closed) {
            return refused(format!(
                "{id} is not closed ({})",
                st.task_status(&id).map(|s| s.label()).unwrap_or("?")
            ));
        }
        let mut r = raw("reopen", actor, &format!("Reopens {id}"));
        r.why = why.trim().to_string();
        r.refs = vec![id];
        b.push(r)?;
        Ok(())
    })
}

/// The task that falls to `actor` in an epic: the one they already hold
/// (assigned or claimed), or the first ready one by `hlc`.
pub fn pick(st: &State, epic: &str, actor: &Actor) -> Option<(String, bool)> {
    let tasks = st.tasks_of(epic);
    for t in &tasks {
        let info = st.task(&t.id)?;
        if matches!(info.status, TaskStatus::Assigned | TaskStatus::Claimed)
            && info
                .holder
                .as_ref()
                .is_some_and(|h| holder_matches(&h.actor, actor))
        {
            return Some((t.id.clone(), true));
        }
    }
    tasks
        .iter()
        .find(|t| st.task_status(&t.id) == Some(TaskStatus::Ready))
        .map(|t| (t.id.clone(), false))
}

/// Why there are no ready tasks: the blocked ones and what blocks them.
pub fn nothing_ready(st: &State, epic: &str, actor: &Actor) -> String {
    let e = st.get(epic);
    let name = e.map(epic_name).unwrap_or_default();
    let mut l = vec![format!("No ready tasks in epic {name} ({epic}).")];
    let blocked: Vec<&Entry> = st
        .tasks_of(epic)
        .into_iter()
        .filter(|t| st.task_status(&t.id) == Some(TaskStatus::Blocked))
        .collect();
    if !blocked.is_empty() {
        l.push("Blocked:".into());
        for t in blocked {
            let by: Vec<String> = st
                .task(&t.id)
                .unwrap()
                .blocked_by
                .iter()
                .map(|d| match st.task_status(d) {
                    Some(s) => format!("{d} ({s})"),
                    None => format!("{d} (does not exist)"),
                })
                .collect();
            l.push(format!(
                "- {} {} → waits for {}",
                t.id,
                crate::views::one_line(&t.title, 100),
                by.join(", ")
            ));
        }
    }
    let mut counts: Vec<String> = Vec::new();
    for s in [
        TaskStatus::Assigned,
        TaskStatus::Claimed,
        TaskStatus::Closed,
    ] {
        let n = st
            .tasks_of(epic)
            .iter()
            .filter(|t| st.task_status(&t.id) == Some(s))
            .count();
        if n > 0 {
            counts.push(format!("{n} {}", s.plural()));
        }
    }
    if !counts.is_empty() {
        l.push(format!("Also: {}.", counts.join(", ")));
    }
    // Phase 5: if your claim lost to another machine, you find out here.
    let who = format!("{}:{}", actor.kind, actor.name);
    for a in &st.alerts {
        if matches!(a, crate::state::Alert::ClaimLost { loser, .. } if *loser == who) {
            l.push(format!("Warning: {a}"));
        }
    }
    l.join("\n")
}

/// `kontext next --claim`: picks and claims under the same lock. Returns the
/// task and, if one was written, the `claim`.
pub fn next_claim(store: &mut Store, actor: &Actor, epic: &str) -> Result<(String, Option<Entry>)> {
    let ((task, claimed), written) = store.transact(|b| {
        let st = b.state()?;
        let epic = st.find_epic(epic)?;
        let Some((task, mine)) = pick(st, &epic, actor) else {
            return refused(nothing_ready(st, &epic, actor));
        };
        if mine {
            return Ok((task, false));
        }
        let mut r = raw("claim", actor, &format!("Claims {task}"));
        r.refs = vec![task.clone()];
        b.push(r)?;
        Ok((task, true))
    })?;
    Ok((
        task,
        if claimed {
            written.into_iter().next()
        } else {
            None
        },
    ))
}

/// What `kontext done` asks for.
#[derive(Debug, Clone, Default)]
pub struct DoneArgs {
    pub task: String,
    pub evidence: Vec<Value>,
    pub since: Option<Frontier>,
    pub why: String,
}

/// Refusal for stale context, with what to read.
pub fn stale_message(task: &str, stale: &[&Entry], now: &Frontier) -> String {
    let mut l = vec![format!(
        "stale context: something that touches {task} came in after your mark. Read it before closing:"
    )];
    for e in stale {
        let what = match e.kind.as_str() {
            "decision" => format!("decision: {}", crate::views::one_line(&e.title, 120)),
            "accept" => format!(
                "accepted proposal {}",
                e.refs.first().map(String::as_str).unwrap_or("?")
            ),
            "supersede" => format!(
                "goal {} was superseded by {}",
                e.refs.first().map(String::as_str).unwrap_or("?"),
                e.refs.get(1).map(String::as_str).unwrap_or("?")
            ),
            "answer" => {
                let q = e.refs.first().map(String::as_str).unwrap_or("?");
                format!("answer to {q}: {}", crate::views::one_line(&e.body, 120))
            }
            k => crate::views::kind_label(k).to_string(),
        };
        l.push(format!("- {} {what} (`kontext show {}`)", e.id, e.id));
    }
    l.push(String::new());
    l.push(format!(
        "After reading it, adjust your work if needed and retry with the current mark: --since {now}"
    ));
    l.push(format!(
        "If your work still stands as it is, explain why before retrying: kontext log \"…\" --task {task}"
    ));
    l.join("\n")
}

/// What `done` checks before writing (and before running `--run`): that the
/// task is open, that whoever closes it may close it, and that nothing touching
/// it came in after the mark. Returns the full `id`.
pub fn check_done(st: &State, actor: &Actor, a: &DoneArgs) -> Result<String> {
    if st
        .resolve(&a.task)
        .is_ok_and(|id| st.kind_of(&id) == Some("feature"))
    {
        return crate::features::check_validate(st, actor, a);
    }
    let id = open_task(st, &a.task)?;
    let info = st.task(&id).cloned().unwrap_or_default();
    if info.status == TaskStatus::Closed {
        return refused(format!("{id} is already closed"));
    }
    let holds = info
        .holder
        .as_ref()
        .is_some_and(|h| holder_matches(&h.actor, actor));
    if !holds && !st.policy.manages(actor, epic_of_task(st, &id)) {
        let who = match &info.holder {
            Some(h) => format!("held by {}", h.actor),
            None => "nobody holds it: claim it with `kontext next --epic <epic> --claim` \
                     or ask for it to be assigned to you"
                .into(),
        };
        return refused(format!(
            "only the holder, the coordinator of its epic, the pm or an owner can close {id} ({who})"
        ));
    }
    match &a.since {
        Some(since) => {
            let stale = stale_entries(st, &id, since);
            if !stale.is_empty() {
                let now = frontier_of(st);
                return Err(Stale(stale_message(&id, &stale, &now)).into());
            }
        }
        None if !st.policy.is_owner_actor(actor) => {
            return refused(format!(
                "missing --since <mark>: the assignment mark that `kontext next` or `kontext brief` gave you. \
                 It checks that nothing came in that changes your task. Current mark: {}",
                frontier_of(st)
            ));
        }
        None => {}
    }
    Ok(id)
}

/// `kontext done`: checks stale context, ownership and evidence under the
/// lock, and writes the closure.
pub fn done(store: &mut Store, actor: &Actor, a: &DoneArgs) -> Result<Vec<Entry>> {
    if a.evidence.is_empty() {
        return refused(
            "no task is closed without evidence: use --run \"<command>\", \
             --evidence cmd:\"<command>\"=0 | file:<path> | pr:<url> | url:<url>, or --evidence-json",
        );
    }
    if let Err(why) = check_evidence(Some(&Value::Array(a.evidence.clone()))) {
        return refused(format!("invalid evidence: {why}"));
    }
    write(store, |b| {
        let st = b.state()?;
        let id = check_done(st, actor, a)?;
        let verb = if st.kind_of(&id) == Some("feature") {
            "Validates"
        } else {
            "Closes"
        };
        let mut r = raw("done", actor, &format!("{verb} {id}"));
        r.why = a.why.trim().to_string();
        r.refs = vec![id];
        r.fields
            .insert("evidence".into(), Value::Array(a.evidence.clone()));
        if let Some(s) = &a.since {
            r.fields.insert("since".into(), json!(s.encode()));
        }
        b.push(r)?;
        Ok(())
    })
}

/// The mark of a state: the last position of each writer it contains.
pub fn frontier_of(st: &State) -> Frontier {
    let mut f = Frontier::default();
    for e in st.entries.values() {
        let cur = f.0.entry(e.writer.clone()).or_insert(0);
        *cur = (*cur).max(e.n);
    }
    f
}

/// `fields` of a question: `to`, and `task` and `epic` if the question is
/// about a task or goes to an epic.
pub fn question_fields(
    st: &State,
    to: Option<&str>,
    task: Option<&str>,
    feature: Option<&str>,
) -> Result<Map<String, Value>> {
    let mut f = Map::new();
    let mut epic_name_of_task = None;
    let mut task_feature = None;
    if let Some(t) = task.map(str::trim).filter(|t| !t.is_empty()) {
        let id = open_task(st, t)?;
        epic_name_of_task = epic_of_task(st, &id).map(epic_name);
        task_feature = st
            .get(&id)
            .and_then(crate::features::feature_of)
            .map(str::to_string);
        f.insert("task".into(), json!(id));
    }
    // A question about a feature: its id, and its epic like a task's.
    if let Some(u) = feature.map(str::trim).filter(|u| !u.is_empty()) {
        let id = crate::features::resolve(st, u)?;
        if let Some(tf) = &task_feature
            && tf != &id
        {
            return refused(format!(
                "--task hangs from {tf}, not from --feature {id}; leave one of them out"
            ));
        }
        if epic_name_of_task.is_none() {
            epic_name_of_task = st.get(&id).and_then(|x| st.epic_of(x)).map(epic_name);
        }
        f.insert("feature".into(), json!(id));
    }
    let to = match to.map(str::trim).filter(|t| !t.is_empty()) {
        Some(t) => t.to_string(),
        None => match &epic_name_of_task {
            Some(n) => format!("epic:{n}"),
            None => bail!("missing --to <reader> (owner, pm, epic:<x> or a person)"),
        },
    };
    validate_reader(&to)?;
    if let Some(name) = to.strip_prefix("epic:") {
        let id = st.find_epic(name).map_err(|_| {
            anyhow::anyhow!(
                "--to {to}: there is no epic {name:?} (`kontext brief` lists the epics)"
            )
        })?;
        f.insert("epic".into(), json!(epic_name(st.get(&id).unwrap())));
    } else if let Some(n) = epic_name_of_task {
        f.insert("epic".into(), json!(n));
    }
    f.insert("to".into(), json!(to));
    Ok(f)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(parse_duration_ms("2h").unwrap(), 7_200_000);
        assert_eq!(parse_duration_ms("1h30m").unwrap(), 5_400_000);
        assert_eq!(parse_duration_ms("45s").unwrap(), 45_000);
        assert!(parse_duration_ms("2").is_err());
        assert!(parse_duration_ms("2x").is_err());
        assert!(parse_duration_ms("0h").is_err());
    }

    #[test]
    fn slugs() {
        assert_eq!(slug("Card payments"), "card-payments");
        assert_eq!(slug("  Refunds: now!  "), "refunds-now");
        assert_eq!(slug("Café crème"), "cafe-creme");
        assert_eq!(slug("¿?"), "epic");
    }

    #[test]
    fn evidence_specs() {
        let v = parse_evidence("cmd:\"pnpm test\"=0").unwrap();
        assert_eq!(
            v,
            json!({"type":"command","value":"pnpm test","exit_code":0})
        );
        let v = parse_evidence("cmd:cargo test --all=1").unwrap();
        assert_eq!(v["exit_code"], 1);
        assert_eq!(v["value"], "cargo test --all");
        let v = parse_evidence("cmd:make").unwrap();
        assert!(v.get("exit_code").is_none());
        assert_eq!(parse_evidence("file:src/a.rs").unwrap()["type"], "file");
        assert_eq!(
            parse_evidence("https://x.y/z").unwrap(),
            json!({"type":"url","value":"https://x.y/z"})
        );
        assert!(parse_evidence("something").is_err());
        assert!(parse_evidence("foo:bar").is_err());

        assert!(
            check_evidence(Some(&json!([{"type":"command","value":"x","exit_code":0}]))).is_ok()
        );
        assert!(
            check_evidence(Some(&json!([{"type":"command","value":"x","exit_code":1}]))).is_err()
        );
        assert!(check_evidence(Some(&json!([{"type":"command","value":"x"}]))).is_err());
        assert!(check_evidence(Some(&json!([]))).is_err());
        assert!(check_evidence(None).is_err());
        // One valid item is enough.
        assert!(
            check_evidence(Some(&json!([
                {"type":"command","value":"x","exit_code":2},
                {"type":"pr","value":"https://github.com/a/b/pull/1"}
            ])))
            .is_ok()
        );
    }

    #[test]
    fn run_captures_tail_and_code() {
        let r =
            run_command("for i in $(seq 1 30); do echo l$i; done; echo err >&2; exit 3").unwrap();
        assert_eq!(r.exit_code, 3);
        let lines: Vec<&str> = r.excerpt.lines().collect();
        assert_eq!(lines.len(), EXCERPT_LINES);
        assert_eq!(lines.last(), Some(&"err"));
        assert_eq!(lines[0], "l12");
    }

    #[test]
    fn ago_text() {
        assert_eq!(ago(10 * 3_600_000, 8 * 3_600_000), "2 h ago");
        assert_eq!(ago(600_000, 0), "10 min ago");
        assert_eq!(ago(5 * 86_400_000, 0), "5 days ago");
    }
}
