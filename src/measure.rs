//! `kontext measure`: numbers about Claude Code conversations, read from
//! their transcripts (JSONL), to compare a project before and after a change
//! (DESIGN §9.6). It needs no project and no kontext configuration, writes
//! nothing, and prints counts, sizes, tool names and `kontext` subcommand
//! names only: never message text, file contents or command arguments.
//!
//! Each file is read line by line (they reach hundreds of MB). The rules:
//!
//! - A line with `isSidechain` set (a subagent's) is skipped everywhere.
//! - A request is one distinct `message.id` among the `assistant` lines that
//!   carry `message.usage`. Claude Code writes one line per content block,
//!   each with the same usage: the id counts once and the usage of the last
//!   line seen for it wins. Ids are distinct per file.
//! - The history re-read by a request is its `cache_read_input_tokens`.
//! - What fills the history is counted in characters (Unicode scalar values):
//!   tool results (a string content by its length; a list, by the lengths of
//!   its `text` blocks, and its other blocks (images, tool references) count 0 and are counted
//!   apart), tool calls (the compact JSON of each `tool_use` input), assistant
//!   text, and the text of `user` lines, split with the capture hook's rule
//!   ([`capture::skip_reason`], without project `skip_prefixes`) into typed by
//!   the human, delegate reports (`Delegated task node:`), pull-request updates
//!   (`Update on pull request #`) and other machine text (every other machine
//!   prefix, a bare slash command, and lines marked `isMeta`, which no
//!   `UserPromptSubmit` hook ever sees). Compaction summaries are the
//!   `isCompactSummary` lines. Reasoning blocks are not counted.
//! - `--since` and `--until` (inclusive) go by each line's `timestamp`; a line
//!   without one takes the previous line's, and with a window, a line before
//!   any timestamp is outside it. A request belongs to the time of the first
//!   line seen with its id.

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use anyhow::{Context, Result, bail};
use clap::CommandFactory;
use jiff::Timestamp;
use serde_json::{Value, json};

use crate::capture::{self, Skip};
use crate::distill;

/// Where the characters of the history go.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Category {
    ToolResults,
    ToolCalls,
    AssistantText,
    TypedByHuman,
    DelegateReports,
    PrUpdates,
    CompactionSummaries,
    OtherMachineText,
}

impl Category {
    pub const ALL: [Category; 8] = [
        Category::ToolResults,
        Category::ToolCalls,
        Category::AssistantText,
        Category::TypedByHuman,
        Category::DelegateReports,
        Category::PrUpdates,
        Category::CompactionSummaries,
        Category::OtherMachineText,
    ];

    /// The key in `--json`.
    pub fn key(self) -> &'static str {
        match self {
            Category::ToolResults => "tool_results",
            Category::ToolCalls => "tool_calls",
            Category::AssistantText => "assistant_text",
            Category::TypedByHuman => "typed_by_human",
            Category::DelegateReports => "delegate_reports",
            Category::PrUpdates => "pr_updates",
            Category::CompactionSummaries => "compaction_summaries",
            Category::OtherMachineText => "other_machine_text",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Category::ToolResults => "tool results",
            Category::ToolCalls => "tool calls",
            Category::AssistantText => "assistant text",
            Category::TypedByHuman => "typed by the human",
            Category::DelegateReports => "delegate reports",
            Category::PrUpdates => "pull-request updates",
            Category::CompactionSummaries => "compaction summaries",
            Category::OtherMachineText => "other machine text",
        }
    }
}

/// The category of the text of a `user` line (not a tool result, not a
/// compaction summary): the capture hook's rule, split by the prefix that
/// matched.
pub fn user_text_category(text: &str, is_meta: bool) -> Category {
    if is_meta {
        return Category::OtherMachineText;
    }
    match capture::skip_reason(text, &[]) {
        None => Category::TypedByHuman,
        Some(Skip::Machine("Delegated task node:")) => Category::DelegateReports,
        Some(Skip::Machine("Update on pull request #")) => Category::PrUpdates,
        Some(_) => Category::OtherMachineText,
    }
}

/// The four token counts of a request.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tokens {
    pub input: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
    pub output: u64,
}

impl Tokens {
    fn of(usage: &Value) -> Tokens {
        let n = |k: &str| usage.get(k).and_then(Value::as_u64).unwrap_or(0);
        Tokens {
            input: n("input_tokens"),
            cache_read: n("cache_read_input_tokens"),
            cache_creation: n("cache_creation_input_tokens"),
            output: n("output_tokens"),
        }
    }

    fn add(&mut self, o: &Tokens) {
        self.input += o.input;
        self.cache_read += o.cache_read;
        self.cache_creation += o.cache_creation;
        self.output += o.output;
    }
}

/// Count and characters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Sized {
    pub count: u64,
    pub chars: u64,
}

impl Sized {
    fn add(&mut self, chars: u64) {
        self.count += 1;
        self.chars += chars;
    }

    fn merge(&mut self, o: &Sized) {
        self.count += o.count;
        self.chars += o.chars;
    }
}

/// What was measured, in one file or in several together.
#[derive(Debug, Clone, Default)]
pub struct Totals {
    /// Non-empty lines read.
    pub lines: u64,
    pub malformed: u64,
    pub sidechain: u64,
    pub outside_window: u64,
    pub requests: u64,
    pub tokens: Tokens,
    pub max_cache_read: u64,
    pub compactions: u64,
    pub models: BTreeMap<String, u64>,
    pub history: BTreeMap<Category, Sized>,
    pub tool_calls: BTreeMap<String, u64>,
    /// Results per tool name (`unknown` when its call was not seen).
    pub tool_results: BTreeMap<String, Sized>,
    /// Non-text blocks in tool results (images, tool references): counted here, 0 characters.
    pub non_text_blocks: u64,
    /// `Bash` calls with at least one `kontext` command.
    pub kontext_bash_calls: u64,
    /// Per subcommand: `count` is calls, `chars` the result characters.
    pub kontext: BTreeMap<String, Sized>,
    pub first: Option<Timestamp>,
    pub last: Option<Timestamp>,
}

impl Totals {
    /// History re-read per request, on average (integer division; 0 without requests).
    pub fn avg_cache_read(&self) -> u64 {
        self.tokens
            .cache_read
            .checked_div(self.requests)
            .unwrap_or(0)
    }

    pub fn history_chars(&self) -> u64 {
        self.history.values().map(|s| s.chars).sum()
    }

    pub fn kontext_result_chars(&self) -> u64 {
        self.kontext.values().map(|s| s.chars).sum()
    }

    fn merge(&mut self, o: &Totals) {
        self.lines += o.lines;
        self.malformed += o.malformed;
        self.sidechain += o.sidechain;
        self.outside_window += o.outside_window;
        self.requests += o.requests;
        self.tokens.add(&o.tokens);
        self.max_cache_read = self.max_cache_read.max(o.max_cache_read);
        self.compactions += o.compactions;
        for (k, v) in &o.models {
            *self.models.entry(k.clone()).or_default() += v;
        }
        for (k, v) in &o.history {
            self.history.entry(*k).or_default().merge(v);
        }
        for (k, v) in &o.tool_calls {
            *self.tool_calls.entry(k.clone()).or_default() += v;
        }
        for (k, v) in &o.tool_results {
            self.tool_results.entry(k.clone()).or_default().merge(v);
        }
        self.non_text_blocks += o.non_text_blocks;
        self.kontext_bash_calls += o.kontext_bash_calls;
        for (k, v) in &o.kontext {
            self.kontext.entry(k.clone()).or_default().merge(v);
        }
        self.first = match (self.first, o.first) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        self.last = match (self.last, o.last) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
    }
}

/// The time window (inclusive).
#[derive(Debug, Clone, Copy, Default)]
pub struct Window {
    pub since: Option<Timestamp>,
    pub until: Option<Timestamp>,
}

impl Window {
    /// Parses `--since` and `--until` (RFC 3339).
    pub fn parse(since: Option<&str>, until: Option<&str>) -> Result<Window> {
        let p = |flag: &str, v: Option<&str>| -> Result<Option<Timestamp>> {
            v.map(|s| {
                s.trim().parse::<Timestamp>().map_err(|e| {
                    anyhow::anyhow!(
                        "{flag} {s:?} is not an RFC 3339 timestamp (like 2026-11-01T09:00:00Z): {e}"
                    )
                })
            })
            .transpose()
        };
        let w = Window {
            since: p("--since", since)?,
            until: p("--until", until)?,
        };
        if let (Some(s), Some(u)) = (w.since, w.until)
            && s > u
        {
            bail!("--since is after --until");
        }
        Ok(w)
    }

    fn is_set(&self) -> bool {
        self.since.is_some() || self.until.is_some()
    }

    fn contains(&self, t: Option<Timestamp>) -> bool {
        match t {
            None => !self.is_set(),
            Some(t) => self.since.is_none_or(|s| t >= s) && self.until.is_none_or(|u| t <= u),
        }
    }
}

/// One request while its file is read.
struct Request {
    counted: bool,
    tokens: Tokens,
    model: Option<String>,
}

/// The `kontext` subcommands and global options, from the CLI definition.
struct KontextCli {
    subcommands: Vec<String>,
    /// Long name of each global option, and whether it takes a value.
    globals: Vec<(String, bool)>,
}

static CLI: LazyLock<KontextCli> = LazyLock::new(|| {
    let cmd = crate::cli::Cli::command();
    KontextCli {
        subcommands: cmd
            .get_subcommands()
            .map(|s| s.get_name().to_string())
            .collect(),
        globals: cmd
            .get_arguments()
            .filter(|a| a.is_global_set())
            .filter_map(|a| {
                a.get_long()
                    .map(|l| (l.to_string(), a.get_action().takes_values()))
            })
            .collect(),
    }
});

/// The words of each simple command in a shell command line: split on `&&`,
/// `||`, `;`, `|`, `&` and newlines outside quotes, and on blanks, with
/// quotes and backslashes removed. Not a shell: enough to find `kontext`.
pub fn shell_segments(cmd: &str) -> Vec<Vec<String>> {
    let mut segs: Vec<Vec<String>> = Vec::new();
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = cmd.chars().peekable();
    let end_word = |words: &mut Vec<String>, word: &mut String, in_word: &mut bool| {
        if *in_word {
            words.push(std::mem::take(word));
            *in_word = false;
        }
    };
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                for q in chars.by_ref() {
                    if q == '\'' {
                        break;
                    }
                    word.push(q);
                }
            }
            '"' => {
                in_word = true;
                while let Some(q) = chars.next() {
                    match q {
                        '"' => break,
                        '\\' => {
                            if let Some(n) = chars.next() {
                                word.push(n);
                            }
                        }
                        _ => word.push(q),
                    }
                }
            }
            '\\' => {
                in_word = true;
                if let Some(n) = chars.next()
                    && n != '\n'
                {
                    word.push(n);
                }
            }
            ';' | '&' | '|' | '\n' => {
                end_word(&mut words, &mut word, &mut in_word);
                if matches!(chars.peek(), Some(n) if *n == c && c != '\n' && c != ';') {
                    chars.next();
                }
                if !words.is_empty() {
                    segs.push(std::mem::take(&mut words));
                }
            }
            c if c.is_whitespace() => end_word(&mut words, &mut word, &mut in_word),
            c => {
                in_word = true;
                word.push(c);
            }
        }
    }
    end_word(&mut words, &mut word, &mut in_word);
    if !words.is_empty() {
        segs.push(words);
    }
    segs
}

fn is_assignment(w: &str) -> bool {
    let Some((name, _)) = w.split_once('=') else {
        return false;
    };
    let mut cs = name.chars();
    cs.next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && cs.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The `kontext` subcommand of each simple command of a `Bash` command line
/// that runs `kontext` (after any `VAR=value` and the global options): its
/// name if it is a real subcommand, `other` if not.
pub fn kontext_subcommands(cmd: &str) -> Vec<String> {
    let cli = &*CLI;
    let mut out = Vec::new();
    for seg in shell_segments(cmd) {
        let mut it = seg.iter().skip_while(|w| is_assignment(w)).peekable();
        if it.next().map(String::as_str) != Some("kontext") {
            continue;
        }
        let mut sub = None;
        while let Some(w) = it.next() {
            if let Some(long) = w.strip_prefix("--") {
                let (name, inline) = match long.split_once('=') {
                    Some((n, _)) => (n, true),
                    None => (long, false),
                };
                match cli.globals.iter().find(|(g, _)| g == name) {
                    Some((_, takes)) => {
                        if *takes && !inline {
                            it.next();
                        }
                        continue;
                    }
                    None => break,
                }
            }
            sub = cli.subcommands.iter().find(|s| *s == w).cloned();
            break;
        }
        out.push(sub.unwrap_or_else(|| "other".to_string()));
    }
    out
}

fn chars(s: &str) -> u64 {
    s.chars().count() as u64
}

/// The characters of a tool result's content, and its non-text blocks.
fn result_chars(content: Option<&Value>) -> (u64, u64) {
    match content {
        Some(Value::String(s)) => (chars(s), 0),
        Some(Value::Array(blocks)) => {
            let mut n = 0;
            let mut other = 0;
            for b in blocks {
                match (b.get("type").and_then(Value::as_str), b.get("text")) {
                    (Some("text"), Some(Value::String(t))) => n += chars(t),
                    _ => other += 1,
                }
            }
            (n, other)
        }
        _ => (0, 0),
    }
}

fn blocks(rec: &Value) -> &[Value] {
    match rec.get("message").and_then(|m| m.get("content")) {
        Some(Value::Array(a)) => a,
        _ => &[],
    }
}

/// Reads one transcript, a line at a time.
pub fn measure_file(path: &Path, window: &Window) -> Result<Totals> {
    let f = File::open(path).with_context(|| format!("could not open {}", path.display()))?;
    let mut reader = BufReader::new(f);
    let mut t = Totals::default();
    let mut requests: HashMap<String, Request> = HashMap::new();
    let mut tool_names: HashMap<String, String> = HashMap::new();
    let mut kontext_calls: HashMap<String, String> = HashMap::new();
    let mut prev_ts: Option<Timestamp> = None;
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let n = reader
            .read_until(b'\n', &mut buf)
            .with_context(|| format!("could not read {}", path.display()))?;
        if n == 0 {
            break;
        }
        let line = String::from_utf8_lossy(&buf);
        if line.trim().is_empty() {
            continue;
        }
        t.lines += 1;
        let rec = match serde_json::from_str::<Value>(&line) {
            Ok(v @ Value::Object(_)) => v,
            _ => {
                t.malformed += 1;
                continue;
            }
        };
        if distill::flag(&rec, "isSidechain") {
            t.sidechain += 1;
            continue;
        }
        if let Some(ts) = rec
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(|s| s.parse::<Timestamp>().ok())
        {
            prev_ts = Some(ts);
        }
        let inside = window.contains(prev_ts);
        let kind = rec.get("type").and_then(Value::as_str).unwrap_or("");

        // The requests: counted by the time of their first line, with the
        // usage of their last one.
        if kind == "assistant"
            && let Some(usage) = rec.get("message").and_then(|m| m.get("usage"))
            && let Some(id) = capture::message_id(&rec)
        {
            let r = requests.entry(id.to_string()).or_insert(Request {
                counted: inside,
                tokens: Tokens::default(),
                model: None,
            });
            r.tokens = Tokens::of(usage);
            if let Some(m) = rec
                .get("message")
                .and_then(|m| m.get("model"))
                .and_then(Value::as_str)
            {
                r.model = Some(m.to_string());
            }
        }
        // Calls are named even outside the window, so a result inside it
        // finds its tool.
        if kind == "assistant" {
            for b in blocks(&rec) {
                if b.get("type").and_then(Value::as_str) != Some("tool_use") {
                    continue;
                }
                let name = b.get("name").and_then(Value::as_str).unwrap_or("unknown");
                let id = b.get("id").and_then(Value::as_str).unwrap_or("");
                tool_names.insert(id.to_string(), name.to_string());
                if !inside {
                    continue;
                }
                let input = b.get("input").cloned().unwrap_or(Value::Null);
                t.history
                    .entry(Category::ToolCalls)
                    .or_default()
                    .add(chars(&input.to_string()));
                *t.tool_calls.entry(name.to_string()).or_default() += 1;
                if name == "Bash"
                    && let Some(cmd) = input.get("command").and_then(Value::as_str)
                {
                    let subs = kontext_subcommands(cmd);
                    if let Some(last) = subs.last() {
                        t.kontext_bash_calls += 1;
                        kontext_calls.insert(id.to_string(), last.clone());
                        for s in &subs {
                            t.kontext.entry(s.clone()).or_default().count += 1;
                        }
                    }
                }
            }
        }
        if !inside {
            t.outside_window += 1;
            continue;
        }
        if let Some(ts) = prev_ts {
            t.first = Some(t.first.map_or(ts, |f| f.min(ts)));
            t.last = Some(t.last.map_or(ts, |l| l.max(ts)));
        }
        if distill::flag(&rec, "isCompactSummary") {
            t.compactions += 1;
            t.history
                .entry(Category::CompactionSummaries)
                .or_default()
                .add(chars(&distill::raw_text_blocks(&rec)));
            continue;
        }
        match kind {
            "assistant" => {
                let text = distill::raw_text_blocks(&rec);
                if !text.is_empty() {
                    t.history
                        .entry(Category::AssistantText)
                        .or_default()
                        .add(chars(&text));
                }
            }
            "user" => {
                for b in blocks(&rec) {
                    if b.get("type").and_then(Value::as_str) != Some("tool_result") {
                        continue;
                    }
                    let id = b.get("tool_use_id").and_then(Value::as_str).unwrap_or("");
                    let (n, other) = result_chars(b.get("content"));
                    t.non_text_blocks += other;
                    t.history.entry(Category::ToolResults).or_default().add(n);
                    let name = tool_names.get(id).map_or("unknown", String::as_str);
                    t.tool_results.entry(name.to_string()).or_default().add(n);
                    if let Some(sub) = kontext_calls.get(id) {
                        t.kontext.entry(sub.clone()).or_default().chars += n;
                    }
                }
                let text = distill::raw_text_blocks(&rec);
                if !text.is_empty() {
                    let cat = user_text_category(&text, distill::flag(&rec, "isMeta"));
                    t.history.entry(cat).or_default().add(chars(&text));
                }
            }
            _ => {}
        }
    }
    for r in requests.values().filter(|r| r.counted) {
        t.requests += 1;
        t.tokens.add(&r.tokens);
        t.max_cache_read = t.max_cache_read.max(r.tokens.cache_read);
        if let Some(m) = &r.model {
            *t.models.entry(m.clone()).or_default() += 1;
        }
    }
    Ok(t)
}

/// The measurement of several transcripts: each one, and all together.
#[derive(Debug, Clone)]
pub struct Measurement {
    pub window: Window,
    pub files: Vec<(PathBuf, Totals)>,
    pub total: Totals,
}

/// Measures the transcripts. A missing file is an error naming it.
pub fn measure(paths: &[PathBuf], window: Window) -> Result<Measurement> {
    let mut files = Vec::new();
    let mut total = Totals::default();
    for p in paths {
        let t = measure_file(p, &window)?;
        total.merge(&t);
        files.push((p.clone(), t));
    }
    Ok(Measurement {
        window,
        files,
        total,
    })
}

/// `name → n`, largest first (then by name).
fn ranked<T: Copy>(m: &BTreeMap<String, T>, key: impl Fn(&T) -> u64) -> Vec<(&str, T)> {
    let mut v: Vec<(&str, T)> = m.iter().map(|(k, x)| (k.as_str(), *x)).collect();
    v.sort_by(|a, b| key(&b.1).cmp(&key(&a.1)).then(a.0.cmp(b.0)));
    v
}

fn share(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        0.0
    } else {
        part as f64 / whole as f64
    }
}

/// `1234567` → `1,234,567`.
pub fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn ts(t: Option<Timestamp>) -> Value {
    t.map_or(Value::Null, |t| json!(t.to_string()))
}

impl Measurement {
    pub fn json(&self) -> Value {
        let t = &self.total;
        let whole = t.history_chars();
        json!({
            "files": self.files.iter().map(|(p, f)| json!({
                "path": p.display().to_string(),
                "requests": f.requests,
                "avg_cache_read_per_request": f.avg_cache_read(),
                "lines": f.lines,
                "malformed_lines": f.malformed,
            })).collect::<Vec<_>>(),
            "since": ts(self.window.since),
            "until": ts(self.window.until),
            "first_timestamp": ts(t.first),
            "last_timestamp": ts(t.last),
            "lines": t.lines,
            "malformed_lines": t.malformed,
            "sidechain_lines": t.sidechain,
            "lines_outside_window": t.outside_window,
            "requests": t.requests,
            "tokens": {
                "input": t.tokens.input,
                "cache_read": t.tokens.cache_read,
                "cache_creation": t.tokens.cache_creation,
                "output": t.tokens.output,
            },
            "avg_cache_read_per_request": t.avg_cache_read(),
            "max_cache_read": t.max_cache_read,
            "compactions": t.compactions,
            "models": ranked(&t.models, |n| *n).iter()
                .map(|(m, n)| json!({"model": m, "requests": n})).collect::<Vec<_>>(),
            "history": {
                "chars": whole,
                "categories": Category::ALL.iter().map(|c| {
                    let s = t.history.get(c).copied().unwrap_or_default();
                    json!({
                        "category": c.key(),
                        "count": s.count,
                        "chars": s.chars,
                        "share": (share(s.chars, whole) * 10_000.0).round() / 10_000.0,
                    })
                }).collect::<Vec<_>>(),
            },
            "tools": {
                "calls": ranked(&t.tool_calls, |n| *n).iter()
                    .map(|(name, n)| json!({"name": name, "calls": n})).collect::<Vec<_>>(),
                "results": ranked(&t.tool_results, |s| s.chars).iter()
                    .map(|(name, s)| json!({"name": name, "results": s.count, "chars": s.chars}))
                    .collect::<Vec<_>>(),
                "result_non_text_blocks": t.non_text_blocks,
            },
            "kontext": {
                "bash_calls": t.kontext_bash_calls,
                "result_chars": t.kontext_result_chars(),
                "subcommands": ranked(&t.kontext, |s| s.count).iter()
                    .map(|(name, s)| json!({"subcommand": name, "calls": s.count, "result_chars": s.chars}))
                    .collect::<Vec<_>>(),
            },
        })
    }

    pub fn text(&self) -> String {
        let t = &self.total;
        let n = |x: u64| thousands(x);
        let mut l = Vec::new();
        let mut read = format!(
            "{} {} ({} lines",
            self.files.len(),
            if self.files.len() == 1 {
                "transcript"
            } else {
                "transcripts"
            },
            n(t.lines)
        );
        for (count, what) in [
            (t.malformed, "malformed, skipped"),
            (t.sidechain, "from subagents (sidechain), skipped"),
            (t.outside_window, "outside the time window"),
        ] {
            if count > 0 {
                read.push_str(&format!("; {} {what}", n(count)));
            }
        }
        l.push(read + ")");
        let window = match (self.window.since, self.window.until) {
            (None, None) => String::new(),
            (s, u) => format!(
                " (window: {} to {})",
                s.map_or("the start".into(), |t| t.to_string()),
                u.map_or("the end".into(), |t| t.to_string())
            ),
        };
        match (t.first, t.last) {
            (Some(a), Some(b)) => l.push(format!("Covers: {a} to {b}{window}")),
            _ => l.push(format!("Covers: no timestamped line{window}")),
        }
        l.push(format!("Requests: {}", n(t.requests)));
        l.push(format!(
            "Tokens: input {}; cache read {}; cache creation {}; output {}",
            n(t.tokens.input),
            n(t.tokens.cache_read),
            n(t.tokens.cache_creation),
            n(t.tokens.output)
        ));
        l.push(format!(
            "History re-read per request: {} tokens on average, {} at most",
            n(t.avg_cache_read()),
            n(t.max_cache_read)
        ));
        l.push(format!("Compactions: {}", n(t.compactions)));
        let models: Vec<String> = ranked(&t.models, |x| *x)
            .iter()
            .map(|(m, c)| format!("{m} ({})", n(*c)))
            .collect();
        l.push(format!(
            "Models: {}",
            if models.is_empty() {
                "none".into()
            } else {
                models.join(", ")
            }
        ));

        let whole = t.history_chars();
        l.push(String::new());
        l.push(format!(
            "What fills the history ({} characters; characters, share, items):",
            n(whole)
        ));
        for c in Category::ALL {
            let s = t.history.get(&c).copied().unwrap_or_default();
            l.push(format!(
                "  {:<22} {:>14}  {:>5.1} %  {:>8}",
                c.label(),
                n(s.chars),
                share(s.chars, whole) * 100.0,
                n(s.count)
            ));
        }
        if t.non_text_blocks > 0 {
            l.push(format!(
                "  ({} non-text blocks in tool results, such as images and tool references, counted as 0 characters)",
                n(t.non_text_blocks)
            ));
        }

        l.push(String::new());
        l.push("Tools by calls (top 10):".into());
        for (name, c) in ranked(&t.tool_calls, |x| *x).iter().take(10) {
            l.push(format!("  {name:<30} {:>10}", n(*c)));
        }
        l.push("Tools by result characters (top 10; characters, results):".into());
        for (name, s) in ranked(&t.tool_results, |s| s.chars).iter().take(10) {
            l.push(format!(
                "  {name:<30} {:>14}  {:>8}",
                n(s.chars),
                n(s.count)
            ));
        }

        l.push(String::new());
        l.push(format!(
            "kontext commands (Bash calls with kontext: {}; their result characters: {}):",
            n(t.kontext_bash_calls),
            n(t.kontext_result_chars())
        ));
        if !t.kontext.is_empty() {
            l.push(format!(
                "  {:<14} {:>8}  {:>17}",
                "", "calls", "result characters"
            ));
        }
        for (name, s) in ranked(&t.kontext, |s| s.count) {
            l.push(format!(
                "  {name:<14} {:>8}  {:>17}",
                n(s.count),
                n(s.chars)
            ));
        }

        if self.files.len() > 1 {
            l.push(String::new());
            l.push("Per file (requests; history re-read per request, on average):".into());
            for (p, f) in &self.files {
                l.push(format!(
                    "  {}: {}; {} tokens",
                    p.display(),
                    n(f.requests),
                    n(f.avg_cache_read())
                ));
            }
        }
        l.join("\n") + "\n"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_and_subcommands() {
        assert_eq!(
            shell_segments(r#"cd "a b" && kontext log 'x; y' ; echo "q\"z" | wc"#),
            vec![
                vec!["cd".to_string(), "a b".into()],
                vec!["kontext".into(), "log".into(), "x; y".into()],
                vec!["echo".into(), "q\"z".into()],
                vec!["wc".into()],
            ]
        );
        assert_eq!(kontext_subcommands("kontext resume --as pm"), ["resume"]);
        assert_eq!(
            kontext_subcommands("kontext --actor agent:x --role pm propose t"),
            ["propose"]
        );
        assert_eq!(
            kontext_subcommands("kontext --project=demo status"),
            ["status"]
        );
        assert_eq!(kontext_subcommands("kontext --json resume"), ["resume"]);
        assert_eq!(
            kontext_subcommands("KONTEXT_ACTOR=agent:a FOO=1 kontext inbox count"),
            ["inbox"]
        );
        assert_eq!(
            kontext_subcommands("cd /x && kontext next --epic e; kontext done T-1 || true"),
            ["next", "done"]
        );
        // A quoted separator does not split; an unknown word is `other`.
        assert_eq!(
            kontext_subcommands("kontext log 'a && kontext show'"),
            ["log"]
        );
        assert_eq!(kontext_subcommands("kontext frobnicate"), ["other"]);
        assert_eq!(kontext_subcommands("kontext --version"), ["other"]);
        assert_eq!(kontext_subcommands("kontext"), ["other"]);
        assert!(kontext_subcommands("echo kontext resume").is_empty());
        assert!(kontext_subcommands("./kontext resume").is_empty());
        // The list comes from clap: every documented subcommand is in it.
        for s in ["doctor", "measure", "hook", "install", "resume", "inbox"] {
            assert!(CLI.subcommands.iter().any(|x| x == s), "{s}");
        }
    }

    #[test]
    fn user_text_categories() {
        let c = |t: &str| user_text_category(t, false);
        assert_eq!(c("go"), Category::TypedByHuman);
        assert_eq!(
            c("Delegated task node: invoices export"),
            Category::DelegateReports
        );
        assert_eq!(c("Update on pull request #12"), Category::PrUpdates);
        assert_eq!(c("<task-notification>x"), Category::OtherMachineText);
        // Another session's message, in Claude Code's wrapper: not the human's.
        assert_eq!(
            c("Another Claude session sent a message: the invoices export is merged"),
            Category::OtherMachineText
        );
        assert_eq!(c("/clear"), Category::OtherMachineText);
        assert_eq!(user_text_category("go", true), Category::OtherMachineText);
    }

    #[test]
    fn window_and_numbers() {
        assert!(Window::parse(Some("yesterday"), None).is_err());
        assert!(Window::parse(Some("2026-11-02T00:00:00Z"), Some("2026-11-01T00:00:00Z")).is_err());
        let w = Window::parse(Some("2026-11-01T00:00:00Z"), Some("2026-11-01T10:00:00Z")).unwrap();
        let t = |s: &str| Some(s.parse::<Timestamp>().unwrap());
        assert!(w.contains(t("2026-11-01T00:00:00Z")));
        assert!(w.contains(t("2026-11-01T10:00:00Z")));
        assert!(!w.contains(t("2026-11-01T10:00:00.001Z")));
        assert!(!w.contains(None));
        assert!(Window::default().contains(None));
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_234_567), "1,234,567");
        assert_eq!(Totals::default().avg_cache_read(), 0);
        assert_eq!(result_chars(Some(&json!("añb"))), (3, 0));
        assert_eq!(
            result_chars(Some(&json!([
                {"type": "text", "text": "ab"},
                {"type": "image", "source": {}},
                {"type": "text", "text": "c"}
            ]))),
            (3, 1)
        );
    }
}
