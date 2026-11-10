//! `kontext install` (DESIGN §9.3): prepares a code repo to use `kontext`.
//!
//! - `.kontext.toml` with `project` and `default_role`, if it does not exist
//!   (an existing one is kept as it is). `--worktree-role <role>` sets its
//!   `worktree_role` key ([`with_worktree_role`]). `--link-only` writes only
//!   this file ([`plan_link_only`]): in a linked git worktree, with
//!   `default_role = "pm"`, it marks the worktree as the PM's. A new file
//!   there that git neither tracks nor ignores gets a note with the line for
//!   `info/exclude` ([`exclude_note`]); so does a new `settings.local.json`
//!   with `--local`.
//! - The `SessionStart` hook in `.claude/settings.json` (with `--local`, or
//!   when kontext's hooks are already there, `.claude/settings.local.json`:
//!   kontext's entries live in one of the two, and `--local` removes them from
//!   `settings.json` without touching the rest, [`remove_kontext`]), merged without
//!   touching the rest (like `merge_hooks` in v0). If a `kontext` start hook
//!   already exists with another path, its path is changed instead of adding
//!   another one. With the distill on, also `PreCompact` and `SessionEnd`
//!   (phase 4); with the capture on (`[capture] enabled = true`), also
//!   `UserPromptSubmit`, `Stop` and `SessionEnd` (one `SessionEnd` entry for
//!   both). Turning either off later does not remove its hooks: they check
//!   the setting each time and do nothing.
//! - The skill `.claude/skills/kontext/SKILL.md` (from `templates/skill-kontext.md`)
//!   and the orchestrate skill, `.claude/skills/kontext-orchestrate/SKILL.md`
//!   (from `templates/skill-kontext-orchestrate.md`), the skill that shows the
//!   human how things are going, `.claude/skills/kontext-show/SKILL.md`, the
//!   skill that installs kontext and migrates the project's memory into it,
//!   `.claude/skills/kontext-migrate/SKILL.md`, and the PM's memory of the
//!   user's ideas, `.claude/skills/kontext-pm/SKILL.md`. All five are always
//!   replaced. With `--skills-only [--global]`, only those, in the repo or in
//!   `~/.claude/skills/` ([`plan_skills`]).
//! - The agent files `.claude/agents/kontext-*.md` (from `templates/agents/`),
//!   among them `kontext-pm-reader.md`, the reader the `kontext-pm` skill
//!   launches. If one already exists with different content (you changed it
//!   by hand, or it comes from another version), it is kept and a warning is
//!   shown; `--force` replaces it.
//! - The `Bash(kontext:*)` permission in `permissions.allow` of the same
//!   settings file as the hooks, if it is missing.
//! - The repo in `[[repos]]` of `project.toml`, if it is missing: named after
//!   the folder, in a linked worktree the main checkout's (a worktree of a
//!   bare repo leaves `[[repos]]` alone, with a note).
//!
//! Before writing, the diff of each file is shown. With `--dry-run` nothing
//! is written. Running it twice changes nothing the second time.
//!
//! The hooks record the absolute path of the running binary: run from a
//! cargo build directory ([`cargo_build_checkout`]), `install` prints a note
//! that a `cargo clean` deletes what they point at (`kontext doctor` checks it).
//!
//! `settings.json` is read and written preserving the order of its keys
//! (serde_json's `Value` sorts them, and that would change lines that are not
//! ours).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::config::{Project, write_atomic};

pub const SKILL: &str = include_str!("../templates/skill-kontext.md");

/// The skills: folder in `.claude/skills/` and content.
pub const SKILLS: &[(&str, &str)] = &[
    ("kontext", SKILL),
    (
        "kontext-orchestrate",
        include_str!("../templates/skill-kontext-orchestrate.md"),
    ),
    (
        "kontext-show",
        include_str!("../templates/skill-kontext-show.md"),
    ),
    (
        "kontext-migrate",
        include_str!("../templates/skill-kontext-migrate.md"),
    ),
    (
        "kontext-pm",
        include_str!("../templates/skill-kontext-pm.md"),
    ),
];

/// The agent files (DESIGN §12): name and content.
pub const AGENTS: &[(&str, &str)] = &[
    (
        "kontext-coordinator.md",
        include_str!("../templates/agents/kontext-coordinator.md"),
    ),
    (
        "kontext-implementer.md",
        include_str!("../templates/agents/kontext-implementer.md"),
    ),
    (
        "kontext-tester.md",
        include_str!("../templates/agents/kontext-tester.md"),
    ),
    (
        "kontext-scout.md",
        include_str!("../templates/agents/kontext-scout.md"),
    ),
    (
        "kontext-pm-reader.md",
        include_str!("../templates/agents/kontext-pm-reader.md"),
    ),
];

/// JSON that preserves key order.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(untagged)]
pub enum Json {
    Null,
    Bool(bool),
    Num(serde_json::Number),
    Str(String),
    Arr(Vec<Json>),
    Obj(IndexMap<String, Json>),
}

impl Json {
    fn obj() -> Json {
        Json::Obj(IndexMap::new())
    }

    fn str_of(&self, k: &str) -> Option<&str> {
        match self {
            Json::Obj(m) => match m.get(k) {
                Some(Json::Str(s)) => Some(s),
                _ => None,
            },
            _ => None,
        }
    }
}

/// The Claude Code permission that covers kontext commands. It only covers
/// commands that start with `kontext` (no variables in front, no pipes).
pub const PERMISSION: &str = "Bash(kontext:*)";

/// The binary name of a `<path>/<binary> hook <event>` command.
pub fn hook_binary<'a>(cmd: &'a str, event: &str) -> Option<&'a str> {
    let rest = cmd.trim().strip_suffix(&format!("hook {event}"))?;
    let bin = rest.trim().trim_matches(['\'', '"']);
    Path::new(bin).file_name()?.to_str()
}

/// The binary's full path and the kontext event of a `<binary> hook <event>`
/// command, the path unquoted ([`shell_unquote`]): what `kontext doctor`
/// checks. [`hook_binary`] only needs the file name.
pub fn hook_parts(cmd: &str) -> Option<(String, String)> {
    let cmd = cmd.trim();
    let (rest, event) = cmd.rsplit_once(char::is_whitespace)?;
    let bin = rest.trim_end().strip_suffix("hook")?;
    if !bin.ends_with(char::is_whitespace) || event.is_empty() {
        return None;
    }
    let bin = shell_unquote(bin.trim())?;
    (!bin.is_empty()).then(|| (bin, event.to_string()))
}

/// The inverse of [`shell_quote`] (and plain double quotes): `None` if a
/// quote is left open or the text is more than one word.
pub fn shell_unquote(s: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' => loop {
                match chars.next()? {
                    '\'' => break,
                    q => out.push(q),
                }
            },
            '"' => loop {
                match chars.next()? {
                    '"' => break,
                    '\\' => out.push(chars.next()?),
                    q => out.push(q),
                }
            },
            '\\' => out.push(chars.next()?),
            c if c.is_whitespace() => return None,
            c => out.push(c),
        }
    }
    Some(out)
}

/// If `exe` is inside a cargo build directory (a `target/debug` or
/// `target/release` component), the folder that holds `target`: a
/// `cargo clean` there deletes the binary.
pub fn cargo_build_checkout(exe: &Path) -> Option<PathBuf> {
    let comps: Vec<_> = exe.components().collect();
    let i = comps.windows(2).rposition(|w| {
        w[0].as_os_str() == "target"
            && (w[1].as_os_str() == "debug" || w[1].as_os_str() == "release")
    })?;
    Some(comps[..i].iter().collect())
}

/// `true` if the command is `kontext hook <event>` from any `kontext`:
/// `install` changes its path instead of adding another one.
fn is_kontext_hook(cmd: &str, event: &str) -> bool {
    hook_binary(cmd, event).is_some_and(|n| n == "kontext")
}

/// `true` if the command is the start hook of some `kontext`.
#[cfg(test)]
fn is_kontext_session_start(cmd: &str) -> bool {
    is_kontext_hook(cmd, "session-start")
}

/// Merges a `kontext` hook into `settings`: `hooks.<Event>` with `command`
/// (`<kontext> hook <event>`). If a `kontext` hook for that event already
/// exists with another path, its path is changed instead of adding another.
pub fn merge_hook(
    settings: &Json,
    hook_event: &str,
    kontext_event: &str,
    command: &str,
    matcher: Option<&str>,
) -> Result<Json> {
    let mut s = settings.clone();
    let Json::Obj(root) = &mut s else {
        bail!("settings.json is not a JSON object; leaving it alone");
    };
    let hooks = root.entry("hooks".to_string()).or_insert_with(Json::obj);
    let Json::Obj(hooks) = hooks else {
        bail!("`hooks` in settings.json is not an object; leaving it alone");
    };
    let groups = hooks
        .entry(hook_event.to_string())
        .or_insert_with(|| Json::Arr(Vec::new()));
    let Json::Arr(groups) = groups else {
        bail!("`hooks.{hook_event}` in settings.json is not a list; leaving it alone");
    };
    for g in groups.iter_mut() {
        let Json::Obj(g) = g else { continue };
        let Some(Json::Arr(hs)) = g.get_mut("hooks") else {
            continue;
        };
        for h in hs.iter_mut() {
            let Some(cmd) = h.str_of("command").map(str::to_string) else {
                continue;
            };
            if cmd == command {
                return Ok(s);
            }
            if is_kontext_hook(&cmd, kontext_event)
                && let Json::Obj(h) = h
            {
                h.insert("command".into(), Json::Str(command.to_string()));
                return Ok(s);
            }
        }
    }
    let mut hook = IndexMap::new();
    hook.insert("type".to_string(), Json::Str("command".into()));
    hook.insert("command".to_string(), Json::Str(command.to_string()));
    hook.insert("timeout".to_string(), Json::Num(15.into()));
    let mut group = IndexMap::new();
    if let Some(m) = matcher {
        group.insert("matcher".to_string(), Json::Str(m.into()));
    }
    group.insert("hooks".to_string(), Json::Arr(vec![Json::Obj(hook)]));
    groups.push(Json::Obj(group));
    Ok(s)
}

/// Adds `perm` to `permissions.allow` of `settings` if it is not there. Leaves
/// the rest alone.
pub fn merge_permission(settings: &Json, perm: &str) -> Result<Json> {
    let mut s = settings.clone();
    let Json::Obj(root) = &mut s else {
        bail!("settings.json is not a JSON object; leaving it alone");
    };
    let perms = root
        .entry("permissions".to_string())
        .or_insert_with(Json::obj);
    let Json::Obj(perms) = perms else {
        bail!("`permissions` in settings.json is not an object; leaving it alone");
    };
    let allow = perms
        .entry("allow".to_string())
        .or_insert_with(|| Json::Arr(Vec::new()));
    let Json::Arr(allow) = allow else {
        bail!("`permissions.allow` in settings.json is not a list; leaving it alone");
    };
    if !allow.iter().any(|p| matches!(p, Json::Str(x) if x == perm)) {
        allow.push(Json::Str(perm.to_string()));
    }
    Ok(s)
}

/// `true` if the command is `<path>/kontext hook <event>`, for any event.
fn is_kontext_command(cmd: &str) -> bool {
    hook_parts(cmd)
        .is_some_and(|(bin, _)| Path::new(&bin).file_name().is_some_and(|n| n == "kontext"))
}

/// `true` if `settings` has any of kontext's entries: a `kontext hook` or the
/// [`PERMISSION`].
pub fn has_kontext(settings: &Json) -> bool {
    remove_kontext(settings) != *settings
}

/// `true` if `settings` has a `kontext hook` command: where they are decides
/// which settings file kontext's entries live in (the permission alone does
/// not).
pub fn has_kontext_hooks(settings: &Json) -> bool {
    let Json::Obj(root) = settings else {
        return false;
    };
    let Some(Json::Obj(events)) = root.get("hooks") else {
        return false;
    };
    events.values().any(|groups| match groups {
        Json::Arr(groups) => groups.iter().any(|g| match g {
            Json::Obj(g) => match g.get("hooks") {
                Some(Json::Arr(hs)) => hs
                    .iter()
                    .any(|h| h.str_of("command").is_some_and(is_kontext_command)),
                _ => false,
            },
            _ => false,
        }),
        _ => false,
    })
}

/// `settings` without kontext's entries: its hooks (and the groups and event
/// lists left empty by removing them) and the [`PERMISSION`] (and the
/// `allow` and `permissions` left empty by removing it). Everything else, and
/// a container that was already empty, stays as it was.
pub fn remove_kontext(settings: &Json) -> Json {
    let mut s = settings.clone();
    let Json::Obj(root) = &mut s else {
        return s;
    };
    if let Some(Json::Obj(hooks)) = root.get_mut("hooks") {
        let before = hooks.len();
        hooks.retain(|_, groups| {
            let Json::Arr(groups) = groups else {
                return true;
            };
            let n = groups.len();
            groups.retain_mut(|g| {
                let Json::Obj(g) = g else { return true };
                let Some(Json::Arr(hs)) = g.get_mut("hooks") else {
                    return true;
                };
                let m = hs.len();
                hs.retain(|h| !h.str_of("command").is_some_and(is_kontext_command));
                hs.len() == m || !hs.is_empty()
            });
            groups.len() == n || !groups.is_empty()
        });
        if hooks.is_empty() && before > 0 {
            root.shift_remove("hooks");
        }
    }
    if let Some(Json::Obj(perms)) = root.get_mut("permissions") {
        let mut emptied = false;
        if let Some(Json::Arr(allow)) = perms.get_mut("allow") {
            let n = allow.len();
            allow.retain(|p| !matches!(p, Json::Str(x) if x == PERMISSION));
            if allow.is_empty() && n > 0 {
                perms.shift_remove("allow");
                emptied = perms.is_empty();
            }
        }
        if emptied {
            root.shift_remove("permissions");
        }
    }
    s
}

/// Merges the `SessionStart` hook into `settings`. Returns the result.
pub fn merge_hooks(settings: &Json, command: &str) -> Result<Json> {
    merge_hook(
        settings,
        "SessionStart",
        "session-start",
        command,
        Some("startup|resume|clear|compact"),
    )
}

/// The binary path, in single quotes if needed.
pub fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '@' | '+' | ':')
        })
    {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

pub fn unified_diff(path: &str, before: &str, after: &str) -> String {
    similar::TextDiff::from_lines(before, after)
        .unified_diff()
        .context_radius(3)
        .header(&format!("{path} (before)"), &format!("{path} (after)"))
        .to_string()
}

/// One change per file.
#[derive(Debug, Clone)]
pub struct Change {
    pub path: PathBuf,
    pub before: Option<String>,
    pub after: String,
}

impl Change {
    pub fn changed(&self) -> bool {
        self.before.as_deref() != Some(self.after.as_str())
    }
}

#[derive(Debug, Clone)]
pub struct Plan {
    pub changes: Vec<Change>,
    /// Agent files that already exist with other content and are kept.
    pub kept: Vec<PathBuf>,
    /// What to tell the user besides the diffs (where the hooks stay).
    pub notes: Vec<String>,
}

fn read_opt(p: &Path) -> Result<Option<String>> {
    match fs::read_to_string(p) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("could not read {}", p.display())),
    }
}

fn git_out(dir: &Path, args: &[&str]) -> Option<String> {
    Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

/// The root of the git repo that contains `dir`, or `dir`.
pub fn repo_root(dir: &Path) -> PathBuf {
    git_out(dir, &["rev-parse", "--show-toplevel"])
        .map(PathBuf::from)
        .unwrap_or_else(|| dir.to_path_buf())
}

/// `git@github.com:a/b.git` or `https://github.com/a/b.git` → `github.com/a/b`.
pub fn normalize_remote(url: &str) -> String {
    let mut s = url.trim().to_string();
    for p in ["https://", "http://", "ssh://", "git://"] {
        if let Some(rest) = s.strip_prefix(p) {
            s = rest.to_string();
        }
    }
    if let Some((user, rest)) = s.split_once('@')
        && !user.contains('/')
    {
        s = rest.to_string();
    }
    if !s.contains("://")
        && let Some((host, path)) = s.split_once(':')
        && !host.contains('/')
    {
        s = format!("{host}/{path}");
    }
    s.trim_end_matches('/').trim_end_matches(".git").to_string()
}

/// The `project.toml` with this repo in `[[repos]]` (or the same text if it
/// was already there). It is appended at the end so the rest is not reformatted.
fn with_repo(text: &str, project: &Project, name: &str, remote: Option<&str>) -> Result<String> {
    let known = project.repos.iter().any(|r| {
        r.name == name
            || (remote.is_some() && r.remote.as_deref().map(normalize_remote).as_deref() == remote)
    });
    if known {
        return Ok(text.to_string());
    }
    let mut out = text.to_string();
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&format!(
        "\n[[repos]]\nname = {}\n",
        toml::Value::from(name)
    ));
    if let Some(r) = remote {
        out.push_str(&format!("remote = {}\n", toml::Value::from(r)));
    }
    let parsed: Project = toml::from_str(&out).context(
        "could not add the repo to [[repos]] of project.toml without breaking it; add it by hand",
    )?;
    if !parsed.repos.iter().any(|r| r.name == name) {
        bail!("could not add the repo to [[repos]] of project.toml; add it by hand");
    }
    Ok(out)
}

pub struct Options<'a> {
    /// The folder of the code repo.
    pub repo: &'a Path,
    pub project_name: &'a str,
    pub home: &'a Path,
    pub default_role: &'a str,
    /// The binary path, already quoted if needed.
    pub exe: String,
    /// Also installs `PreCompact` and `SessionEnd` (the distill is on).
    pub distill: bool,
    /// Also installs `UserPromptSubmit`, `Stop` and `SessionEnd` (the capture
    /// is on).
    pub capture: bool,
    /// Replaces the agent files even if they have changes.
    pub force: bool,
    /// The hooks and the permission go in `.claude/settings.local.json`, and
    /// kontext's own entries leave `.claude/settings.json`.
    pub local: bool,
    /// Sets `worktree_role` in `.kontext.toml`.
    pub worktree_role: Option<&'a str>,
}

/// The `.kontext.toml` of `repo`: kept as it is if it exists, else `project`
/// and `default_role`; with `worktree_role`, that key set too.
pub fn plan_link(
    repo: &Path,
    project_name: &str,
    default_role: &str,
    worktree_role: Option<&str>,
) -> Result<Change> {
    let path = repo.join(".kontext.toml");
    let before = read_opt(&path)?;
    let mut after = match &before {
        Some(b) => b.clone(),
        None => format!(
            "project = {}\ndefault_role = {}\n",
            toml::Value::from(project_name),
            toml::Value::from(default_role)
        ),
    };
    if let Some(r) = worktree_role {
        after = with_worktree_role(&after, r)
            .with_context(|| format!("could not set worktree_role in {}", path.display()))?;
    }
    Ok(Change {
        path,
        before,
        after,
    })
}

/// `text` (a `.kontext.toml`) with `worktree_role = "<role>"` among its
/// top-level keys: that line replaced if there is one, otherwise added after
/// the last of them (before the first table). The rest of the file stays as
/// it was.
pub fn with_worktree_role(text: &str, role: &str) -> Result<String> {
    let line = format!("worktree_role = {}", toml::Value::from(role));
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let top = lines
        .iter()
        .position(|l| l.trim_start().starts_with('['))
        .unwrap_or(lines.len());
    let is_key = |l: &String| {
        l.trim_start()
            .strip_prefix("worktree_role")
            .is_some_and(|r| r.trim_start().starts_with('='))
    };
    match lines[..top].iter().position(is_key) {
        Some(i) => lines[i] = line,
        None => {
            let at = lines[..top]
                .iter()
                .rposition(|l| !l.trim().is_empty())
                .map_or(0, |i| i + 1);
            lines.insert(at, line);
        }
    }
    let out = lines.join("\n") + "\n";
    let parsed: crate::config::RepoLink = toml::from_str(&out)?;
    if parsed.worktree_role.as_deref() != Some(role) {
        bail!("it does not read back; set it by hand");
    }
    Ok(out)
}

/// Where the settings of a repo are, read.
struct SettingsFile {
    path: PathBuf,
    before: Option<String>,
    json: Json,
}

fn read_settings(path: PathBuf) -> Result<SettingsFile> {
    let before = read_opt(&path)?;
    let json = match before.as_deref().filter(|t| !t.trim().is_empty()) {
        Some(t) => serde_json::from_str::<Json>(t).map_err(|e| {
            anyhow::anyhow!(
                "{} is not valid JSON ({e}); fix it first. Nothing was written",
                path.display()
            )
        })?,
        None => Json::obj(),
    };
    if !matches!(json, Json::Obj(_)) {
        bail!(
            "{} is not a JSON object; nothing was written",
            path.display()
        );
    }
    Ok(SettingsFile { path, before, json })
}

impl SettingsFile {
    /// The change to `json`: the file as it was if nothing changed.
    fn change(self, json: &Json) -> Result<Change> {
        let after = match &self.before {
            Some(b) if *json == self.json => b.clone(),
            _ => serde_json::to_string_pretty(json)? + "\n",
        };
        Ok(Change {
            path: self.path,
            before: self.before,
            after,
        })
    }
}

/// Only the skills, in `dir` (a Claude Code `skills/` folder). They are always
/// replaced, as in [`plan`].
pub fn plan_skills(dir: &Path) -> Result<Plan> {
    let mut changes = Vec::new();
    for (name, text) in SKILLS {
        let path = dir.join(name).join("SKILL.md");
        changes.push(Change {
            before: read_opt(&path)?,
            path,
            after: text.to_string(),
        });
    }
    Ok(Plan {
        changes,
        kept: Vec::new(),
        notes: Vec::new(),
    })
}

/// What `kontext install` would do, without writing anything.
pub fn plan(o: &Options) -> Result<Plan> {
    let mut changes = Vec::new();
    let mut notes = Vec::new();

    let linked = matches!(crate::git::checkout(o.repo), Some((_, true)));
    let link = plan_link(o.repo, o.project_name, o.default_role, o.worktree_role)?;
    if link.before.is_none() && o.default_role == "pm" && linked {
        notes.push(format!(
            "This is a linked git worktree: its own .kontext.toml with default_role = \"pm\" marks \
             it as the PM's (`{}` writes only that file). Claude Code reads .claude/settings.json \
             from the session's folder and .claude/settings.local.json from the main checkout's \
             root (its settings docs): the hooks written here reach sessions in this worktree \
             only; for every checkout, run `kontext install --local` in the main checkout.",
            crate::capture::MARK_COMMAND
        ));
    }
    changes.push(link);

    // kontext's hooks and permission live in one of the two files: in
    // `settings.local.json` with `--local` or if they are already there.
    let claude = o.repo.join(".claude");
    let shared = read_settings(claude.join("settings.json"))?;
    let local_path = claude.join("settings.local.json");
    let local = if o.local {
        Some(read_settings(local_path)?)
    } else {
        // Plain `install` only looks: a broken local file is not its business.
        read_settings(local_path)
            .ok()
            .filter(|l| has_kontext_hooks(&l.json))
    };
    let in_shared = has_kontext_hooks(&shared.json);
    let target = match local {
        Some(l) => {
            if o.local {
                let cleaned = remove_kontext(&shared.json);
                if cleaned != shared.json {
                    changes.push(shared.change(&cleaned)?);
                }
            } else if in_shared {
                notes.push(format!(
                    "kontext's hooks are in both {} and {}: a copy that differs (another binary \
                     path) runs as well. This changes only the second; `kontext install --local` \
                     removes the copy in the first.",
                    claude.join("settings.json").display(),
                    l.path.display()
                ));
            } else {
                notes.push(format!(
                    "kontext's hooks and permission are in {} (`kontext install --local`): kept there.",
                    l.path.display()
                ));
            }
            l
        }
        None => shared,
    };
    let parsed = target.json.clone();
    let mut merged = merge_hooks(&parsed, &format!("{} hook session-start", o.exe))?;
    // `SessionEnd` is shared by the distill and the capture: one entry, which
    // does both jobs.
    let mut events: Vec<(&str, &str)> = Vec::new();
    if o.distill {
        events.extend([("PreCompact", "pre-compact"), ("SessionEnd", "session-end")]);
    }
    if o.capture {
        events.extend([("UserPromptSubmit", "user-prompt-submit"), ("Stop", "stop")]);
        if !o.distill {
            events.push(("SessionEnd", "session-end"));
        }
    }
    for (hook_event, kontext_event) in events {
        merged = merge_hook(
            &merged,
            hook_event,
            kontext_event,
            &format!("{} hook {kontext_event}", o.exe),
            None,
        )?;
    }
    merged = merge_permission(&merged, PERMISSION)?;
    let change = target.change(&merged)?;
    if o.local && change.before.is_none() {
        notes.extend(exclude_note(o.repo, ".claude/settings.local.json"));
    }
    changes.push(change);

    for (dir, text) in SKILLS {
        let skill = o
            .repo
            .join(".claude")
            .join("skills")
            .join(dir)
            .join("SKILL.md");
        changes.push(Change {
            before: read_opt(&skill)?,
            path: skill,
            after: text.to_string(),
        });
    }

    let mut kept = Vec::new();
    let agents = o.repo.join(".claude").join("agents");
    for (name, text) in AGENTS {
        let path = agents.join(name);
        let before = read_opt(&path)?;
        match &before {
            Some(b) if b != text && !o.force => kept.push(path),
            _ => changes.push(Change {
                path,
                before,
                after: text.to_string(),
            }),
        }
    }

    let ptoml = o.home.join("project.toml");
    let text = fs::read_to_string(&ptoml)
        .with_context(|| format!("could not read {}", ptoml.display()))?;
    let project: Project =
        toml::from_str(&text).with_context(|| format!("could not read {}", ptoml.display()))?;
    // `[[repos]]` names the repo: in a linked worktree, after its main
    // checkout's folder, not the worktree's. A worktree of a bare repo has
    // no main checkout: `[[repos]]` is left as it is.
    let named = if linked {
        crate::git::main_checkout(o.repo)
    } else {
        Some(o.repo.to_path_buf())
    };
    match named {
        Some(dir) => {
            let name = dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "repo".into());
            let remote =
                git_out(o.repo, &["remote", "get-url", "origin"]).map(|r| normalize_remote(&r));
            let after = with_repo(&text, &project, &name, remote.as_deref())?;
            changes.push(Change {
                path: ptoml,
                before: Some(text),
                after,
            });
        }
        None => notes.push(format!(
            "This is a linked worktree of a bare git repo: [[repos]] of {} is left as it is (it \
             names repos, not worktrees); add the repo there by hand if it is missing.",
            ptoml.display()
        )),
    }
    Ok(Plan {
        changes,
        kept,
        notes,
    })
}

/// `kontext install --link-only`: only the `.kontext.toml` of `repo`, as
/// [`plan`] writes it.
pub fn plan_link_only(
    repo: &Path,
    project_name: &str,
    default_role: &str,
    worktree_role: Option<&str>,
) -> Result<Plan> {
    let link = plan_link(repo, project_name, default_role, worktree_role)?;
    let mut notes = Vec::new();
    if link.before.is_none() && matches!(crate::git::checkout(repo), Some((_, true))) {
        notes.extend(exclude_note(repo, ".kontext.toml"));
    }
    Ok(Plan {
        changes: vec![link],
        kept: Vec::new(),
        notes,
    })
}

/// A note for a file kontext creates at `rel` under `repo` that git neither
/// tracks nor ignores (a `git add -A` would commit it): the line for the
/// repo's `info/exclude`, which every worktree shares.
fn exclude_note(repo: &Path, rel: &str) -> Option<String> {
    let exclude = crate::git::untracked_unignored(repo, rel)?;
    let prefix = git_out(repo, &["rev-parse", "--show-prefix"]).unwrap_or_default();
    Some(format!(
        "git neither tracks nor ignores {rel}, so a `git add -A` would commit it: to keep it out \
         of every checkout of this repo, add the line `/{prefix}{rel}` to {}.",
        exclude.display()
    ))
}

impl Plan {
    pub fn is_noop(&self) -> bool {
        self.changes.iter().all(|c| !c.changed())
    }

    /// The diffs, to show them before writing.
    pub fn diffs(&self) -> String {
        let mut out = String::new();
        for c in self.changes.iter().filter(|c| c.changed()) {
            let p = c.path.display().to_string();
            out.push_str(&unified_diff(
                &p,
                c.before.as_deref().unwrap_or(""),
                &c.after,
            ));
        }
        out
    }

    /// Writes what changed. Returns the paths written.
    pub fn apply(&self) -> Result<Vec<PathBuf>> {
        let mut written = Vec::new();
        for c in self.changes.iter().filter(|c| c.changed()) {
            if let Some(d) = c.path.parent() {
                fs::create_dir_all(d)?;
            }
            write_atomic(&c.path, c.after.as_bytes())
                .with_context(|| format!("could not write {}", c.path.display()))?;
            written.push(c.path.clone());
        }
        Ok(written)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_keeps_order_and_is_idempotent() {
        let before: Json = serde_json::from_str(
            r#"{"zeta": 1, "alpha": {"b": 2, "a": 1}, "hooks": {"PreToolUse": [{"matcher": "Bash", "hooks": []}]}}"#,
        )
        .unwrap();
        let once = merge_hooks(&before, "/bin/kontext hook session-start").unwrap();
        let text = serde_json::to_string(&once).unwrap();
        assert!(
            text.starts_with(r#"{"zeta":1,"alpha":{"b":2,"a":1},"hooks":{"PreToolUse""#),
            "{text}"
        );
        assert!(text.contains(r#""SessionStart":[{"matcher":"startup|resume|clear|compact","hooks":[{"type":"command","command":"/bin/kontext hook session-start","timeout":15}]}]"#), "{text}");
        assert_eq!(
            merge_hooks(&once, "/bin/kontext hook session-start").unwrap(),
            once
        );
        // Another binary path: it is changed, not duplicated.
        let moved = merge_hooks(&once, "/opt/kontext hook session-start").unwrap();
        let text = serde_json::to_string(&moved).unwrap();
        assert_eq!(text.matches("hook session-start").count(), 1);
        assert!(text.contains("/opt/kontext"));
    }

    #[test]
    fn worktree_role_goes_among_the_top_level_keys() {
        let t = "project = \"billing\"\ndefault_role = \"pm\"\n";
        assert_eq!(
            with_worktree_role(t, "agent").unwrap(),
            "project = \"billing\"\ndefault_role = \"pm\"\nworktree_role = \"agent\"\n"
        );
        // Replaced, not added twice; before a table and its blank line.
        let t =
            "# link\nproject = \"billing\"\nworktree_role = \"x\"\n\n[extra]\nworktree_role = 1\n";
        assert_eq!(
            with_worktree_role(t, "tester").unwrap(),
            "# link\nproject = \"billing\"\nworktree_role = \"tester\"\n\n[extra]\nworktree_role = 1\n"
        );
        let t = "project = \"billing\"\n\n[extra]\nn = 1\n";
        assert_eq!(
            with_worktree_role(t, "agent").unwrap(),
            "project = \"billing\"\nworktree_role = \"agent\"\n\n[extra]\nn = 1\n"
        );
    }

    #[test]
    fn remove_kontext_leaves_the_rest() {
        let theirs: Json = serde_json::from_str(
            r#"{"zeta": 1, "hooks": {"Stop": [{"hooks": [{"type": "command", "command": "/bin/other hook stop"}]}], "Empty": []}, "permissions": {"allow": ["Bash(ls:*)"], "deny": []}}"#,
        )
        .unwrap();
        assert!(!has_kontext(&theirs));
        assert_eq!(remove_kontext(&theirs), theirs);
        let mut ours = merge_hooks(&theirs, "/bin/kontext hook session-start").unwrap();
        ours = merge_hook(&ours, "Stop", "stop", "'/a b/kontext' hook stop", None).unwrap();
        ours = merge_permission(&ours, PERMISSION).unwrap();
        assert!(has_kontext(&ours));
        assert_eq!(remove_kontext(&ours), theirs);
        // Only kontext's: nothing is left, not even the containers it emptied.
        let only = merge_permission(
            &merge_hooks(&Json::obj(), "/bin/kontext hook session-start").unwrap(),
            PERMISSION,
        )
        .unwrap();
        assert_eq!(remove_kontext(&only), Json::obj());
    }

    #[test]
    fn remotes() {
        for (u, want) in [
            ("git@github.com:acme/shop.git", "github.com/acme/shop"),
            ("https://github.com/acme/shop.git", "github.com/acme/shop"),
            ("ssh://git@github.com/a/b", "github.com/a/b"),
        ] {
            assert_eq!(normalize_remote(u), want);
        }
    }

    #[test]
    fn quoting() {
        assert_eq!(
            shell_quote("/home/x/.cargo/bin/kontext"),
            "/home/x/.cargo/bin/kontext"
        );
        assert_eq!(shell_quote("/tmp/a b/kontext"), "'/tmp/a b/kontext'");
        assert!(is_kontext_session_start(
            "'/tmp/a b/kontext' hook session-start"
        ));
        assert!(!is_kontext_session_start("/bin/other hook session-start"));
    }

    #[test]
    fn hook_paths_round_trip() {
        for path in [
            "/home/x/.cargo/bin/kontext",
            "/tmp/a b/kontext",
            "/tmp/it's here/kontext",
            "/opt/o'neil/kontext",
        ] {
            let cmd = format!("{} hook session-start", shell_quote(path));
            assert_eq!(
                hook_parts(&cmd),
                Some((path.to_string(), "session-start".to_string())),
                "{cmd}"
            );
            assert_eq!(shell_unquote(&shell_quote(path)).as_deref(), Some(path));
        }
        assert_eq!(
            hook_parts("\"/x y/kontext\"  hook  stop "),
            Some(("/x y/kontext".into(), "stop".into()))
        );
        assert_eq!(hook_parts("echo hola"), None);
        assert_eq!(hook_parts("/bin/xhook stop"), None);
        assert_eq!(hook_parts("hook stop"), None);
        assert_eq!(hook_parts("/tmp/a b/kontext hook stop"), None);
        assert_eq!(hook_parts("'/tmp/open hook stop"), None);
    }

    #[test]
    fn build_dirs() {
        assert_eq!(
            cargo_build_checkout(Path::new("/src/kontext/target/debug/kontext")),
            Some(PathBuf::from("/src/kontext"))
        );
        assert_eq!(
            cargo_build_checkout(Path::new("/w/target/release/deps/kontext")),
            Some(PathBuf::from("/w"))
        );
        assert_eq!(
            cargo_build_checkout(Path::new("/home/x/.cargo/bin/kontext")),
            None
        );
        assert_eq!(cargo_build_checkout(Path::new("/srv/target/kontext")), None);
    }
}
