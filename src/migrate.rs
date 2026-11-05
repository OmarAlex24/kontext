//! `kontext migrate` (DESIGN §18.1): moves an install of the old `ctx`
//! command over to kontext, and prints the one-line hint when kontext cannot
//! find its configuration but finds the old one.
//!
//! What gets renamed: `~/.config/ctx` → `~/.config/kontext`, each kontext-home's
//! internal folder `.ctx/` → `.kontext/`, the repo's `.ctx.toml` →
//! `.kontext.toml`, the hook `<path>/ctx hook <event>` and the `Bash(ctx…)`
//! permissions in `.claude/settings.json` (and `settings.local.json`), and the
//! `ctx*` skills and agent files in the repo. Kontext-homes are not moved:
//! one at `~/ctx-home/x` stays there.
//!
//! The plan is built first (without writing) and printed with the paths it
//! touches; with `--dry-run` it ends there. Running it twice does nothing the
//! second time.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use regex::Regex;

use crate::config::{self, write_atomic};
use crate::install::{self, Json};
use crate::model::sha256_hex;

/// The internal folder of a kontext-home used by the old command.
pub const LEGACY_INTERNAL_DIR: &str = ".ctx";
/// The `.ctx.toml` of a repo used by the old command.
pub const LEGACY_REPO_LINK: &str = ".ctx.toml";

/// The skills of the old command and their replacement. The `kontext-*`
/// entries are the Spanish names kontext used before its translation to English.
pub const LEGACY_SKILLS: &[(&str, &str)] = &[
    ("ctx", "kontext"),
    ("ctx-orquestar", "kontext-orchestrate"),
    ("ctx-mostrar", "kontext-show"),
    ("kontext-orquestar", "kontext-orchestrate"),
    ("kontext-mostrar", "kontext-show"),
    ("kontext-migrar", "kontext-migrate"),
];

/// The agent files of the old command, their replacement, and the fingerprint
/// (sha256) of each version that `ctx install` wrote. A file that does not
/// match was changed by hand: it is kept.
pub const LEGACY_AGENTS: &[(&str, &str, &[&str])] = &[
    (
        "ctx-coordinator.md",
        "kontext-coordinator.md",
        &[
            "67c1b0aff73fc51bd550575df860df25b8bce79e0be3bd18f497206dc01f470c",
            "811ffc171cdc6a308fcd1cb729e039dd85f9dfb5abcc603825bb7f885a7654b6",
            "90ec1e6a472d58ec61f219b5a0a3a147e985dcc575fe07e745218401d72f5052",
            "b50ddb22f3af8ced9580973e4a1910ce8455dd313bc8c978d99f6afec529bea2",
        ],
    ),
    (
        "ctx-implementer.md",
        "kontext-implementer.md",
        &[
            "24060c574810dfb120838548e3387f7c050b85c41310343e4f7f94d3fddac3b7",
            "497c8f1c509965396ae119265557482396b323c7f99afc5d91bdfce59adbdf45",
            "5ceff083f935a7fff3b4a414f8fd68e935e3d8a6545ac3c91e308d08e39cc253",
            "5e252991b69dcdb2d39aa7f011c183777482fb51ecceaf1bdca7e551300d93d5",
            "7aa0d2a5922760a573a4f9bb907540576e8c5ab9316bd4a6ac40420ca4634567",
            "d966953a8b58af3a14c7656f24780ad13c8f7a1eb2c1791358579d6b8fd22d90",
        ],
    ),
    (
        "ctx-tester.md",
        "kontext-tester.md",
        &[
            "188208634e30dade0a92be422e99e8722e97935e5489d0bb7a9f1b3e31266dd0",
            "5525711ba7fae339f42b9647929523856ed96e604ffdf436729cc050a0e8e9f9",
            "a16516dd31185bfaa9b4ca5563ee1bc7d84e3787344332b98e115ee2b7a2ee2d",
            "f6d1498019170bde2d22255e585b0c7a0f9e0794ffca50f6a416ebcef6956b2f",
        ],
    ),
    (
        "ctx-scout.md",
        "kontext-scout.md",
        &[
            "72d5873757048f6f867aeffc77ecc80e03ea8ce4434855244dc045521bf845ff",
            "a29e4da9d2128b3428638cd7f811fdbba1838e77072d6948235d3fa5915103f2",
            "a835c8a98e93a6f72b30051bf420ba9b49c4d38d8c76a61d160a356385b607f6",
        ],
    ),
];

/// The variables of the old command. `CTX_HOME` is not one of them: it belongs to `ctx.py` (v0).
pub const LEGACY_ENV: &[&str] = &[
    "CTX_ACTOR",
    "CTX_ROLE",
    "CTX_MODEL",
    "CTX_SESSION",
    "CTX_PROJECT",
    "CTX_EPIC",
    "CTX_CONFIG_DIR",
    "CTX_TRANSCRIPTS_GLOB",
    "CTX_DISTILL_DISABLE",
    "CTX_NOW",
    "CTX_GH_BIN",
];

fn env_set(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| !v.is_empty())
}

/// The configuration folder of the old command: `$CTX_CONFIG_DIR`, or
/// `~/.config/ctx` if `$KONTEXT_CONFIG_DIR` is not set either (when kontext has
/// its own folder, the old command's usual one is not its own).
pub fn legacy_config_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("CTX_CONFIG_DIR").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(d));
    }
    if env_set("KONTEXT_CONFIG_DIR") {
        return None;
    }
    Some(config::home_dir().ok()?.join(".config").join("ctx"))
}

/// The `CTX_*` variables in the environment without their `KONTEXT_*`
/// counterpart: kontext no longer reads them.
pub fn legacy_env() -> Vec<&'static str> {
    LEGACY_ENV
        .iter()
        .copied()
        .filter(|v| env_set(v) && !env_set(&v.replacen("CTX_", "KONTEXT_", 1)))
        .collect()
}

/// `true` if the command is the hook of an old `ctx` (`<path>/ctx hook <event>`).
fn is_legacy_hook(cmd: &str) -> bool {
    hook_event_of(cmd).is_some_and(|(bin, _)| bin == "ctx")
}

/// The binary and the event of `<path>/<binary> hook <event>`.
fn hook_event_of(cmd: &str) -> Option<(&str, &'static str)> {
    ["session-start", "pre-compact", "session-end"]
        .into_iter()
        .find_map(|ev| install::hook_binary(cmd, ev).map(|b| (b, ev)))
}

/// All the `command` values of the hooks in a `settings.json`.
fn hook_commands(settings: &Json) -> Vec<String> {
    let mut out = Vec::new();
    let Json::Obj(root) = settings else {
        return out;
    };
    let Some(Json::Obj(hooks)) = root.get("hooks") else {
        return out;
    };
    for groups in hooks.values() {
        let Json::Arr(groups) = groups else { continue };
        for g in groups {
            let Json::Obj(g) = g else { continue };
            let Some(Json::Arr(hs)) = g.get("hooks") else {
                continue;
            };
            for h in hs {
                if let Json::Obj(h) = h
                    && let Some(Json::Str(c)) = h.get("command")
                {
                    out.push(c.clone());
                }
            }
        }
    }
    out
}

fn read_json(p: &Path) -> Option<Json> {
    let t = fs::read_to_string(p).ok()?;
    serde_json::from_str(&t).ok()
}

/// What is left of the old command and gets in kontext's way.
#[derive(Debug, Default)]
pub struct Leftovers {
    /// The old command's `projects.toml`, if kontext does not have its own.
    pub config: Option<PathBuf>,
    /// A `.ctx.toml` above the folder, with no closer `.kontext.toml`.
    pub repo_link: Option<PathBuf>,
    /// A `.claude/settings.json` with the start hook of the old `ctx` and
    /// without kontext's.
    pub hook: Option<PathBuf>,
    /// `CTX_*` variables without their `KONTEXT_*`.
    pub env: Vec<&'static str>,
}

impl Leftovers {
    pub fn is_empty(&self) -> bool {
        self.config.is_none()
            && self.repo_link.is_none()
            && self.hook.is_none()
            && self.env.is_empty()
    }

    /// What is left, looking for the repo from `dir`.
    pub fn find(dir: &Path) -> Leftovers {
        let mut l = Leftovers {
            env: legacy_env(),
            ..Leftovers::default()
        };
        let has_config = config::Machine::path().is_ok_and(|p| p.is_file());
        if !has_config
            && let Some(old) = legacy_config_dir().map(|d| d.join("projects.toml"))
            && old.is_file()
        {
            l.config = Some(old);
        }
        for d in dir.ancestors() {
            if d.join(".kontext.toml").is_file() {
                break;
            }
            let old = d.join(LEGACY_REPO_LINK);
            if old.is_file() {
                l.repo_link = Some(old);
                break;
            }
        }
        let settings = install::repo_root(dir)
            .join(".claude")
            .join("settings.json");
        if let Some(s) = read_json(&settings) {
            let cmds = hook_commands(&s);
            let old = cmds
                .iter()
                .any(|c| install::hook_binary(c, "session-start") == Some("ctx"));
            let new = cmds
                .iter()
                .any(|c| install::hook_binary(c, "session-start") == Some("kontext"));
            if old && !new {
                l.hook = Some(settings);
            }
        }
        l
    }

    /// The line that says so, or `None` if nothing is left.
    pub fn hint(&self) -> Option<String> {
        if self.is_empty() {
            return None;
        }
        let mut what = Vec::new();
        if let Some(p) = &self.config {
            what.push(p.display().to_string());
        }
        if let Some(p) = &self.repo_link {
            what.push(p.display().to_string());
        }
        if let Some(p) = &self.hook {
            what.push(format!(
                "the `ctx hook session-start` hook in {}",
                p.display()
            ));
        }
        if !self.env.is_empty() {
            what.push(format!(
                "{} in the environment (now KONTEXT_*)",
                self.env.join(", ")
            ));
        }
        Some(format!(
            "there is an install of the old `ctx` command ({}): `kontext migrate --dry-run` shows how to move it to kontext (or use the kontext-migrate skill)",
            what.join("; ")
        ))
    }
}

/// One step of the plan.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// Moves a folder. If `to` already exists, it moves whatever inside is not
    /// there yet and deletes `from` if it ends up empty. `internal`: it is the
    /// internal folder of a kontext-home (it leaves its `.gitignore` there).
    MoveDir {
        from: PathBuf,
        to: PathBuf,
        internal: bool,
    },
    /// Renames a file.
    Rename { from: PathBuf, to: PathBuf },
    /// Writes a file (with the diff against what was there).
    Write {
        path: PathBuf,
        before: Option<String>,
        after: String,
        /// Whether the diff is printed (not for the templates: skills and agents).
        diff: bool,
    },
    /// Deletes a file, and its folder if it ends up empty.
    Remove { path: PathBuf },
}

impl Step {
    fn describe(&self) -> String {
        match self {
            Step::MoveDir { from, to, .. } if to.exists() => format!(
                "merge {} into {} (what is already in both stays in {})",
                from.display(),
                to.display(),
                leftover_name(from).display()
            ),
            Step::MoveDir { from, to, .. } => {
                format!("move {} → {}", from.display(), to.display())
            }
            Step::Rename { from, to } => format!("rename {} → {}", from.display(), to.display()),
            Step::Write { path, before, .. } => match before {
                Some(_) => format!("change {}", path.display()),
                None => format!("create {}", path.display()),
            },
            Step::Remove { path } => format!("delete {}", path.display()),
        }
    }

    /// The paths it writes to (or moves or deletes).
    fn paths(&self) -> Vec<&Path> {
        match self {
            Step::MoveDir { from, to, .. } | Step::Rename { from, to } => vec![from, to],
            Step::Write { path, .. } | Step::Remove { path } => vec![path],
        }
    }

    fn apply(&self) -> Result<()> {
        match self {
            Step::MoveDir { from, to, internal } => {
                move_dir(from, to)?;
                if *internal && let Some(home) = to.parent() {
                    config::ensure_internal_dir(home)?;
                }
            }
            Step::Rename { from, to } => fs::rename(from, to).with_context(|| {
                format!("could not rename {} to {}", from.display(), to.display())
            })?,
            Step::Write { path, after, .. } => {
                if let Some(d) = path.parent() {
                    fs::create_dir_all(d)?;
                }
                write_atomic(path, after.as_bytes())
                    .with_context(|| format!("could not write {}", path.display()))?;
            }
            Step::Remove { path } => {
                fs::remove_file(path)
                    .with_context(|| format!("could not delete {}", path.display()))?;
                if let Some(d) = path.parent() {
                    let _ = fs::remove_dir(d);
                }
            }
        }
        Ok(())
    }
}

/// Moves `from` to `to`; if `to` exists, it moves whatever inside is not there
/// yet (what clashes stays in `from`, which is only deleted if it ends up empty).
fn move_dir(from: &Path, to: &Path) -> Result<()> {
    if !to.exists() {
        if let Some(d) = to.parent() {
            fs::create_dir_all(d)?;
        }
        return fs::rename(from, to)
            .with_context(|| format!("could not move {} to {}", from.display(), to.display()));
    }
    for ent in fs::read_dir(from)? {
        let ent = ent?;
        let dest = to.join(ent.file_name());
        if !dest.exists() {
            fs::rename(ent.path(), &dest).with_context(|| {
                format!(
                    "could not move {} to {}",
                    ent.path().display(),
                    dest.display()
                )
            })?;
        }
    }
    if fs::remove_dir(from).is_err() {
        // What clashed is not lost: it is set aside, and the old folder is
        // gone (the next run has nothing to do).
        let aside = leftover_name(from);
        fs::rename(from, &aside)
            .with_context(|| format!("could not move {} to {}", from.display(), aside.display()))?;
    }
    Ok(())
}

/// `<folder>.migrated` (or `.migrated-2`, `-3`… if it already exists).
fn leftover_name(from: &Path) -> PathBuf {
    let base = from.as_os_str().to_string_lossy().into_owned();
    let mut p = PathBuf::from(format!("{base}.migrated"));
    let mut i = 2;
    while p.exists() {
        p = PathBuf::from(format!("{base}.migrated-{i}"));
        i += 1;
    }
    p
}

/// The migration plan.
#[derive(Debug, Default)]
pub struct Plan {
    pub steps: Vec<Step>,
    /// What is not touched and why, and what is left to do by hand.
    pub notes: Vec<String>,
}

pub struct Options<'a> {
    /// The folder it runs in (the code repo, if there is one).
    pub dir: &'a Path,
    /// The kontext binary path, already quoted if needed.
    pub exe: String,
}

fn read_opt(p: &Path) -> Result<Option<String>> {
    match fs::read_to_string(p) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("could not read {}", p.display())),
    }
}

/// Changes in a path what gets renamed: the old configuration folder for the
/// new one, and an internal `.ctx` folder for `.kontext`.
fn rewrite_path(s: &str, old_dir: &Path, new_dir: &Path, default_dirs: bool) -> String {
    let mut out = s.to_string();
    let old = old_dir.display().to_string();
    if let Some(rest) = out.strip_prefix(&old)
        && (rest.is_empty() || rest.starts_with('/'))
    {
        out = format!("{}{rest}", new_dir.display());
    }
    if default_dirs
        && let Some(rest) = out.strip_prefix("~/.config/ctx")
        && (rest.is_empty() || rest.starts_with('/'))
    {
        out = format!("~/.config/kontext{rest}");
    }
    let re = Regex::new(r"/\.ctx(/|$)").unwrap();
    re.replace_all(&out, "/.kontext$1").into_owned()
}

fn rewrite_values(v: &mut toml::Value, f: &dyn Fn(&str) -> String) -> bool {
    match v {
        toml::Value::String(s) => {
            let n = f(s);
            let changed = n != *s;
            *s = n;
            changed
        }
        toml::Value::Array(a) => a.iter_mut().fold(false, |c, x| rewrite_values(x, f) | c),
        toml::Value::Table(t) => t
            .iter_mut()
            .fold(false, |c, (_, x)| rewrite_values(x, f) | c),
        _ => false,
    }
}

/// Merges into `into` what is in `from` but not there yet: `writer`,
/// `[signing]`, and each project in `[projects]`.
fn merge_machine(into: &mut toml::Table, from: &toml::Table) {
    for (k, v) in from {
        match (k.as_str(), into.get_mut(k)) {
            ("projects", Some(toml::Value::Table(projects))) => {
                if let toml::Value::Table(old) = v {
                    for (name, p) in old {
                        projects.entry(name.clone()).or_insert_with(|| p.clone());
                    }
                }
            }
            (_, None) => {
                into.insert(k.clone(), v.clone());
            }
            _ => {}
        }
    }
}

/// The folders of the kontext-homes in a `projects.toml`.
fn homes_of(table: &toml::Table) -> Vec<PathBuf> {
    let Some(toml::Value::Table(projects)) = table.get("projects") else {
        return Vec::new();
    };
    projects
        .values()
        .filter_map(|p| p.get("home")?.as_str().map(str::to_string))
        .filter_map(|h| config::expand_tilde(&h).ok())
        .collect()
}

/// The paths of the repos in `[projects.<p>.repos]`.
fn repos_of(table: &toml::Table) -> Vec<PathBuf> {
    let Some(toml::Value::Table(projects)) = table.get("projects") else {
        return Vec::new();
    };
    projects
        .values()
        .filter_map(|p| p.get("repos")?.as_table().cloned())
        .flat_map(|t| {
            t.into_iter()
                .filter_map(|(_, v)| v.as_str().map(str::to_string))
        })
        .filter_map(|r| config::expand_tilde(&r).ok())
        .collect()
}

/// Changes the hook and permissions of the old command to kontext's in a
/// `settings.json`. If kontext's hook for the same event is already there, the
/// old one is removed instead of duplicated. The rest is not touched.
pub fn migrate_settings(settings: &Json, exe: &str) -> Json {
    let mut s = settings.clone();
    let Json::Obj(root) = &mut s else {
        return s;
    };
    if let Some(Json::Obj(hooks)) = root.get_mut("hooks") {
        for groups in hooks.values_mut() {
            let Json::Arr(groups) = groups else { continue };
            let cmds: Vec<String> = groups.iter().flat_map(hook_commands_of_group).collect();
            for g in groups.iter_mut() {
                let Json::Obj(g) = g else { continue };
                let Some(Json::Arr(hs)) = g.get_mut("hooks") else {
                    continue;
                };
                hs.retain_mut(|h| {
                    let Json::Obj(h) = h else { return true };
                    let Some(Json::Str(c)) = h.get("command") else {
                        return true;
                    };
                    if !is_legacy_hook(c) {
                        return true;
                    }
                    let (_, ev) = hook_event_of(c).unwrap();
                    let has_new = cmds
                        .iter()
                        .any(|x| install::hook_binary(x, ev) == Some("kontext"));
                    if has_new {
                        return false;
                    }
                    h.insert("command".into(), Json::Str(format!("{exe} hook {ev}")));
                    true
                });
            }
            groups.retain(|g| match g {
                Json::Obj(g) => !matches!(g.get("hooks"), Some(Json::Arr(hs)) if hs.is_empty()),
                _ => true,
            });
        }
    }
    if let Some(Json::Obj(perms)) = root.get_mut("permissions") {
        for list in perms.values_mut() {
            let Json::Arr(items) = list else { continue };
            let mut seen: Vec<String> = Vec::new();
            let mut out = Vec::new();
            for it in items.drain(..) {
                let it = match it {
                    Json::Str(p) => Json::Str(migrate_permission(&p)),
                    other => other,
                };
                if let Json::Str(p) = &it {
                    if seen.contains(p) {
                        continue;
                    }
                    seen.push(p.clone());
                }
                out.push(it);
            }
            *items = out;
        }
    }
    s
}

fn hook_commands_of_group(g: &Json) -> Vec<String> {
    let Json::Obj(g) = g else { return Vec::new() };
    let Some(Json::Arr(hs)) = g.get("hooks") else {
        return Vec::new();
    };
    hs.iter()
        .filter_map(|h| match h {
            Json::Obj(h) => match h.get("command") {
                Some(Json::Str(c)) => Some(c.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// The permission strings of a `settings.json`.
fn permission_strings(settings: &Json) -> Vec<String> {
    let Json::Obj(root) = settings else {
        return Vec::new();
    };
    let Some(Json::Obj(perms)) = root.get("permissions") else {
        return Vec::new();
    };
    perms
        .values()
        .filter_map(|l| match l {
            Json::Arr(items) => Some(items),
            _ => None,
        })
        .flatten()
        .filter_map(|it| match it {
            Json::Str(p) => Some(p.clone()),
            _ => None,
        })
        .collect()
}

/// `text` with the hooks and permissions of the old command changed in the text
/// itself, without reformatting the rest.
fn rewrite_text(text: &str, parsed: &Json, exe: &str) -> String {
    let lit = |s: &str| serde_json::to_string(s).unwrap_or_default();
    let mut out = text.to_string();
    for c in hook_commands(parsed) {
        if is_legacy_hook(&c) {
            let (_, ev) = hook_event_of(&c).unwrap();
            out = out.replace(&lit(&c), &lit(&format!("{exe} hook {ev}")));
        }
    }
    for p in permission_strings(parsed) {
        let n = migrate_permission(&p);
        if n != p {
            out = out.replace(&lit(&p), &lit(&n));
        }
    }
    out
}

/// `Bash(ctx:*)` → `Bash(kontext:*)`, `Bash(CTX_* ctx *)` →
/// `Bash(KONTEXT_* kontext *)`. Permissions that do not name the old command
/// stay the same.
pub fn migrate_permission(p: &str) -> String {
    let Some(inner) = p.strip_prefix("Bash(").and_then(|r| r.strip_suffix(')')) else {
        return p.to_string();
    };
    let cmd = Regex::new(r"(^|[\s;&|(])ctx([\s:]|$)").unwrap();
    if !cmd.is_match(inner) {
        return p.to_string();
    }
    let inner = cmd.replace_all(inner, "${1}kontext${2}");
    let inner = Regex::new(r"\bCTX_")
        .unwrap()
        .replace_all(&inner, "KONTEXT_");
    format!("Bash({inner})")
}

/// Builds the plan without writing anything.
pub fn plan(o: &Options) -> Result<Plan> {
    let mut plan = Plan::default();

    // 1. The machine's configuration.
    let new_dir = config::config_dir()?;
    let new_file = new_dir.join("projects.toml");
    let mut machine: Option<toml::Table> = match read_opt(&new_file)? {
        Some(t) => Some(
            toml::from_str(&t).with_context(|| format!("could not read {}", new_file.display()))?,
        ),
        None => None,
    };
    if let Some(old_dir) = legacy_config_dir().filter(|d| *d != new_dir && d.is_dir()) {
        let old_file = old_dir.join("projects.toml");
        let default_dirs = !env_set("CTX_CONFIG_DIR") && !env_set("KONTEXT_CONFIG_DIR");
        match read_opt(&old_file)? {
            Some(text) => {
                let mut old: toml::Table = toml::from_str(&text)
                    .with_context(|| format!("could not read {}", old_file.display()))?;
                let f = |s: &str| rewrite_path(s, &old_dir, &new_dir, default_dirs);
                let mut wrapped = toml::Value::Table(old.clone());
                let rewritten = rewrite_values(&mut wrapped, &f);
                if let toml::Value::Table(t) = wrapped {
                    old = t;
                }
                match machine.as_mut() {
                    None => {
                        plan.steps.push(Step::MoveDir {
                            from: old_dir.clone(),
                            to: new_dir.clone(),
                            internal: false,
                        });
                        if rewritten {
                            plan.steps.push(Step::Write {
                                path: new_file.clone(),
                                before: Some(text),
                                after: toml::to_string(&old)?,
                                diff: true,
                            });
                        }
                        machine = Some(old);
                    }
                    Some(cur) => {
                        let writer = |t: &toml::Table| {
                            t.get("writer").and_then(|w| w.as_str()).map(str::to_string)
                        };
                        if let (Some(w), Some(old_w)) = (writer(cur), writer(&old))
                            && w != old_w
                        {
                            plan.notes.push(format!(
                                "This machine's writer is still {w} (with the old command it was {old_w}): new entries go to ledger/{w}.jsonl."
                            ));
                        }
                        let before = toml::to_string(cur)?;
                        merge_machine(cur, &old);
                        let after = toml::to_string(cur)?;
                        if after != before {
                            plan.steps.push(Step::Write {
                                path: new_file.clone(),
                                before: read_opt(&new_file)?,
                                after,
                                diff: true,
                            });
                        }
                        plan.steps.push(Step::MoveDir {
                            from: old_dir.clone(),
                            to: new_dir.clone(),
                            internal: false,
                        });
                        plan.notes.push(format!(
                            "{} already existed: what was only in {} is added to it (the writer, the key, and the missing projects), and the old one is left at {}.",
                            new_file.display(),
                            old_file.display(),
                            leftover_name(&old_dir).join("projects.toml").display()
                        ));
                    }
                }
            }
            None => plan.notes.push(format!(
                "{} exists but has no projects.toml; leaving it alone.",
                old_dir.display()
            )),
        }
    }

    // 2. The internal folder of each kontext-home (the homes are not moved).
    let homes = machine.as_ref().map(homes_of).unwrap_or_default();
    for home in &homes {
        let old = home.join(LEGACY_INTERNAL_DIR);
        if old.is_dir() {
            plan.steps.push(Step::MoveDir {
                from: old,
                to: config::internal_dir(home),
                internal: true,
            });
        }
    }

    // 3. The repo of the current folder.
    let repo = install::repo_root(o.dir);
    if !homes.contains(&repo) {
        repo_steps(&repo, &o.exe, &mut plan)?;
    }

    // Other repos on this machine that still use the old name.
    if let Some(m) = &machine {
        for r in repos_of(m) {
            if r != repo && r.join(LEGACY_REPO_LINK).is_file() {
                plan.notes.push(format!(
                    "{} still has {LEGACY_REPO_LINK}: run `kontext migrate` there too.",
                    r.display()
                ));
            }
        }
    }
    let env = legacy_env();
    if !env.is_empty() {
        plan.notes.push(format!(
            "Your environment has {}, which kontext no longer reads: rename them to KONTEXT_* where you define them (~/.bashrc, ~/.zshrc, your agents' launcher).",
            env.join(", ")
        ));
    }
    Ok(plan)
}

fn repo_steps(repo: &Path, exe: &str, plan: &mut Plan) -> Result<()> {
    let old_link = repo.join(LEGACY_REPO_LINK);
    let new_link = repo.join(".kontext.toml");
    if old_link.is_file() {
        if new_link.exists() {
            plan.notes.push(format!(
                "{} already exists: {} is left as it is (delete it if it is no longer needed).",
                new_link.display(),
                old_link.display()
            ));
        } else {
            plan.steps.push(Step::Rename {
                from: old_link,
                to: new_link,
            });
        }
    }

    let claude = repo.join(".claude");
    for name in ["settings.json", "settings.local.json"] {
        let path = claude.join(name);
        let Some(text) = read_opt(&path)? else {
            continue;
        };
        if text.trim().is_empty() {
            continue;
        }
        let Ok(parsed) = serde_json::from_str::<Json>(&text) else {
            plan.notes.push(format!(
                "{} is not valid JSON: leaving it alone (change the ctx hook and permissions by hand).",
                path.display()
            ));
            continue;
        };
        let migrated = migrate_settings(&parsed, exe);
        if migrated != parsed {
            // Changing only the texts is almost always enough, and it keeps the
            // file's format as is. If that does not give the same result (a
            // repeated hook was removed), the whole JSON is written.
            let textual = rewrite_text(&text, &parsed, exe);
            let after = if serde_json::from_str::<Json>(&textual).ok().as_ref() == Some(&migrated) {
                textual
            } else {
                serde_json::to_string_pretty(&migrated)? + "\n"
            };
            plan.steps.push(Step::Write {
                path,
                before: Some(text),
                after,
                diff: true,
            });
        }
    }

    let skills = claude.join("skills");
    for (old, new) in LEGACY_SKILLS {
        let old_path = skills.join(old).join("SKILL.md");
        if !old_path.is_file() {
            continue;
        }
        let new_path = skills.join(new).join("SKILL.md");
        let text = install::SKILLS
            .iter()
            .find(|(d, _)| d == new)
            .map(|(_, t)| *t)
            .context("missing the template of a skill")?;
        let before = read_opt(&new_path)?;
        if before.as_deref() != Some(text) {
            plan.steps.push(Step::Write {
                path: new_path,
                before,
                after: text.to_string(),
                diff: false,
            });
        }
        plan.steps.push(Step::Remove { path: old_path });
    }

    let agents = claude.join("agents");
    for (old, new, known) in LEGACY_AGENTS {
        let old_path = agents.join(old);
        let Some(content) = read_opt(&old_path)? else {
            continue;
        };
        if !known.contains(&sha256_hex(content.as_bytes()).as_str()) {
            plan.notes.push(format!(
                "Kept {}: you changed it by hand. Move your changes to {} and delete it.",
                old_path.display(),
                agents.join(new).display()
            ));
            continue;
        }
        let new_path = agents.join(new);
        if !new_path.exists() {
            let text = install::AGENTS
                .iter()
                .find(|(n, _)| n == new)
                .map(|(_, t)| *t)
                .context("missing the template of an agent file")?;
            plan.steps.push(Step::Write {
                path: new_path,
                before: None,
                after: text.to_string(),
                diff: false,
            });
        }
        plan.steps.push(Step::Remove { path: old_path });
    }
    Ok(())
}

impl Plan {
    pub fn is_noop(&self) -> bool {
        self.steps.is_empty()
    }

    /// The paths it touches, without repeats and in order.
    pub fn touched(&self) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = Vec::new();
        for p in self.steps.iter().flat_map(Step::paths) {
            if !out.iter().any(|x| x == p) {
                out.push(p.to_path_buf());
            }
        }
        out
    }

    /// The plan to print: the paths, the steps, and the diffs.
    pub fn text(&self) -> String {
        let mut out = String::new();
        if self.is_noop() {
            out.push_str("Nothing from the old `ctx` command to migrate.\n");
        } else {
            out.push_str("Paths it touches:\n");
            for p in self.touched() {
                out.push_str(&format!("  {}\n", p.display()));
            }
            out.push_str("\nSteps:\n");
            for (i, s) in self.steps.iter().enumerate() {
                out.push_str(&format!("  {}. {}\n", i + 1, s.describe()));
            }
            for s in &self.steps {
                if let Step::Write {
                    path,
                    before,
                    after,
                    diff: true,
                } = s
                {
                    out.push('\n');
                    out.push_str(&install::unified_diff(
                        &path.display().to_string(),
                        before.as_deref().unwrap_or(""),
                        after,
                    ));
                }
            }
        }
        if !self.notes.is_empty() {
            out.push('\n');
            for n in &self.notes {
                out.push_str(&format!("- {n}\n"));
            }
        }
        out
    }

    /// Runs the steps in order.
    pub fn apply(&self) -> Result<()> {
        for s in &self.steps {
            s.apply()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permissions() {
        assert_eq!(migrate_permission("Bash(ctx:*)"), "Bash(kontext:*)");
        assert_eq!(
            migrate_permission("Bash(CTX_* ctx *)"),
            "Bash(KONTEXT_* kontext *)"
        );
        assert_eq!(
            migrate_permission("Bash(timeout * ctx *)"),
            "Bash(timeout * kontext *)"
        );
        assert_eq!(migrate_permission("Bash(ls:*)"), "Bash(ls:*)");
        assert_eq!(
            migrate_permission("Bash(python3 ctx.py:*)"),
            "Bash(python3 ctx.py:*)"
        );
        assert_eq!(migrate_permission("Read(ctx/**)"), "Read(ctx/**)");
    }

    #[test]
    fn paths() {
        let old = Path::new("/h/.config/ctx");
        let new = Path::new("/h/.config/kontext");
        assert_eq!(
            rewrite_path("/h/.config/ctx/key.pub", old, new, true),
            "/h/.config/kontext/key.pub"
        );
        assert_eq!(
            rewrite_path("~/.config/ctx/key.pub", old, new, true),
            "~/.config/kontext/key.pub"
        );
        assert_eq!(
            rewrite_path("/h/ctx-home/x", old, new, true),
            "/h/ctx-home/x"
        );
        assert_eq!(
            rewrite_path("/h/x/.ctx/tmp", old, new, true),
            "/h/x/.kontext/tmp"
        );
        assert_eq!(rewrite_path("/h/x/.ctxy", old, new, true), "/h/x/.ctxy");
    }

    #[test]
    fn settings_hooks_and_permissions() {
        let before: Json = serde_json::from_str(
            r#"{"z": 1, "permissions": {"allow": ["Bash(ls:*)", "Bash(ctx:*)"]},
                "hooks": {"SessionStart": [{"matcher": "startup", "hooks": [
                    {"type": "command", "command": "/old/bin/ctx hook session-start", "timeout": 15}]}],
                  "PreCompact": [{"hooks": [{"type": "command", "command": "/old/ctx hook pre-compact"}]},
                                 {"hooks": [{"type": "command", "command": "/new/kontext hook pre-compact"}]}],
                  "PreToolUse": [{"hooks": [{"type": "command", "command": "echo ctx"}]}]}}"#,
        )
        .unwrap();
        let after = migrate_settings(&before, "/new/kontext");
        let text = serde_json::to_string(&after).unwrap();
        assert!(
            text.starts_with(r#"{"z":1,"permissions":{"allow":["Bash(ls:*)","Bash(kontext:*)"]}"#),
            "{text}"
        );
        assert!(
            text.contains(r#""command":"/new/kontext hook session-start","timeout":15"#),
            "{text}"
        );
        // kontext already had one for PreCompact: the old one is removed.
        assert_eq!(text.matches("hook pre-compact").count(), 1, "{text}");
        assert!(!text.contains("/old/"), "{text}");
        assert!(text.contains("echo ctx"), "{text}");
        assert_eq!(migrate_settings(&after, "/new/kontext"), after);
    }
}
