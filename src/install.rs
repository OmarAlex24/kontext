//! `kontext install` (DESIGN §9.3): prepares a code repo to use `kontext`.
//!
//! - `.kontext.toml` with `project` and `default_role`, if it does not exist.
//! - The `SessionStart` hook in `.claude/settings.json`, merged without
//!   touching the rest (like `merge_hooks` in v0). If a `kontext` start hook
//!   already exists with another path, its path is changed instead of adding
//!   another one. With the distill on, also `PreCompact` and `SessionEnd`
//!   (phase 4).
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
//! - The `Bash(kontext:*)` permission in `permissions.allow` of
//!   `.claude/settings.json`, if it is missing.
//! - The repo in `[[repos]]` of `project.toml`, if it is missing.
//!
//! Before writing, the diff of each file is shown. With `--dry-run` nothing
//! is written. Running it twice changes nothing the second time.
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
    /// Replaces the agent files even if they have changes.
    pub force: bool,
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
    })
}

/// What `kontext install` would do, without writing anything.
pub fn plan(o: &Options) -> Result<Plan> {
    let mut changes = Vec::new();

    let link = o.repo.join(".kontext.toml");
    let before = read_opt(&link)?;
    let after = match &before {
        Some(b) => b.clone(),
        None => format!(
            "project = {}\ndefault_role = {}\n",
            toml::Value::from(o.project_name),
            toml::Value::from(o.default_role)
        ),
    };
    changes.push(Change {
        path: link,
        before,
        after,
    });

    let settings = o.repo.join(".claude").join("settings.json");
    let before = read_opt(&settings)?;
    let parsed = match before.as_deref().filter(|t| !t.trim().is_empty()) {
        Some(t) => serde_json::from_str::<Json>(t).map_err(|e| {
            anyhow::anyhow!(
                "{} is not valid JSON ({e}); fix it first. Nothing was written",
                settings.display()
            )
        })?,
        None => Json::obj(),
    };
    if !matches!(parsed, Json::Obj(_)) {
        bail!(
            "{} is not a JSON object; nothing was written",
            settings.display()
        );
    }
    let mut merged = merge_hooks(&parsed, &format!("{} hook session-start", o.exe))?;
    if o.distill {
        for (hook_event, kontext_event) in
            [("PreCompact", "pre-compact"), ("SessionEnd", "session-end")]
        {
            merged = merge_hook(
                &merged,
                hook_event,
                kontext_event,
                &format!("{} hook {kontext_event}", o.exe),
                None,
            )?;
        }
    }
    merged = merge_permission(&merged, PERMISSION)?;
    let after = if merged == parsed && before.is_some() {
        before.clone().unwrap()
    } else {
        serde_json::to_string_pretty(&merged)? + "\n"
    };
    changes.push(Change {
        path: settings,
        before,
        after,
    });

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
    let name = o
        .repo
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".into());
    let remote = git_out(o.repo, &["remote", "get-url", "origin"]).map(|r| normalize_remote(&r));
    let after = with_repo(&text, &project, &name, remote.as_deref())?;
    changes.push(Change {
        path: ptoml,
        before: Some(text),
        after,
    });
    Ok(Plan { changes, kept })
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
}
