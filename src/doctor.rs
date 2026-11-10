//! `kontext doctor`: checks an installation and says what is wrong and which
//! command fixes it.
//!
//! It is made for a broken setup, so it never starts with "open the project
//! or fail": each check runs on its own and all are reported. It writes
//! nothing: no `Store::open` (it creates the index), no locks, no
//! `.kontext/`, and of git only commands that do not touch the index
//! (`rev-parse`, `remote`, `symbolic-ref`, `show`). Each finding is `ok`, a
//! `note`, or a `problem` with its fix; any problem makes it exit with 1.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::capture;
use crate::config::{self, Machine, Project, ProjectSource};
use crate::install;
use crate::store::read_tail;
use crate::sync;
use crate::verify;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Ok,
    Note,
    Problem,
}

impl Level {
    fn key(self) -> &'static str {
        match self {
            Level::Ok => "ok",
            Level::Note => "note",
            Level::Problem => "problem",
        }
    }
}

/// One line of the report.
#[derive(Debug, Clone)]
pub struct Finding {
    /// `binary`, `path`, `config`, `home`, `hooks`, `capture`, `skills` or `hook_log`.
    pub section: &'static str,
    pub level: Level,
    pub message: String,
    /// The command (or what to do) that fixes it.
    pub fix: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Report {
    pub findings: Vec<Finding>,
}

const SECTIONS: &[(&str, &str)] = &[
    ("binary", "This binary"),
    ("path", "kontext on PATH"),
    ("config", "Config and project"),
    ("home", "kontext-home"),
    ("hooks", "Hooks"),
    ("capture", "Capture"),
    ("skills", "Skills and agent files"),
    ("hook_log", "Hook log"),
];

impl Report {
    fn add(&mut self, section: &'static str, level: Level, message: String, fix: Option<String>) {
        self.findings.push(Finding {
            section,
            level,
            message,
            fix,
        });
    }

    fn ok(&mut self, section: &'static str, message: String) {
        self.add(section, Level::Ok, message, None);
    }

    fn note(&mut self, section: &'static str, message: String, fix: Option<String>) {
        self.add(section, Level::Note, message, fix);
    }

    fn problem(&mut self, section: &'static str, message: String, fix: String) {
        self.add(section, Level::Problem, message, Some(fix));
    }

    pub fn problems(&self) -> usize {
        self.count(Level::Problem)
    }

    fn count(&self, level: Level) -> usize {
        self.findings.iter().filter(|f| f.level == level).count()
    }

    pub fn json(&self) -> Value {
        json!({
            "ok": self.problems() == 0,
            "problems": self.problems(),
            "notes": self.count(Level::Note),
            "checks": self.findings.iter().map(|f| json!({
                "section": f.section,
                "level": f.level.key(),
                "message": f.message,
                "fix": f.fix,
            })).collect::<Vec<_>>(),
        })
    }

    pub fn text(&self) -> String {
        let mut out = String::new();
        for (key, title) in SECTIONS {
            let lines: Vec<&Finding> = self.findings.iter().filter(|f| f.section == *key).collect();
            if lines.is_empty() {
                continue;
            }
            out.push_str(title);
            out.push('\n');
            for f in lines {
                out.push_str(&format!("  {:<8} {}\n", f.level.key(), f.message));
                if let Some(fix) = &f.fix {
                    out.push_str(&format!("  {:<8} fix: {fix}\n", ""));
                }
            }
            out.push('\n');
        }
        let (p, n) = (self.problems(), self.count(Level::Note));
        out.push_str(&match (p, n) {
            (0, 0) => "No problems.\n".to_string(),
            (0, n) => format!("No problems; {}.\n", plural(n, "note", "notes")),
            (p, n) => format!(
                "{}, {}.\n",
                plural(p, "problem", "problems"),
                plural(n, "note", "notes")
            ),
        });
        out
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// `p` canonicalized, if it exists.
fn canon(p: &Path) -> Option<PathBuf> {
    std::fs::canonicalize(p).ok()
}

fn is_executable(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

/// The first `kontext` on `PATH`.
fn on_path() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .filter(|d| !d.as_os_str().is_empty())
        .map(|d| d.join("kontext"))
        .find(|p| is_executable(p))
}

/// A `kontext hook <event>` command found in a settings file.
#[derive(Debug, Clone)]
struct Hook {
    file: PathBuf,
    /// The Claude Code event (`SessionStart`).
    event: String,
    /// The kontext event (`session-start`).
    kontext_event: String,
    bin: PathBuf,
}

/// What the settings files say about kontext.
#[derive(Debug, Default)]
struct Settings {
    hooks: Vec<Hook>,
    permission: bool,
}

/// The settings Claude Code reads for this repo: the repo's shared and local
/// ones and the user's (`$CLAUDE_CONFIG_DIR/settings.json` or
/// `~/.claude/settings.json`).
fn settings_files(repo: &Path) -> Vec<PathBuf> {
    let mut v = vec![
        repo.join(".claude").join("settings.json"),
        repo.join(".claude").join("settings.local.json"),
    ];
    match std::env::var_os("CLAUDE_CONFIG_DIR").filter(|d| !d.is_empty()) {
        Some(d) => v.push(PathBuf::from(d).join("settings.json")),
        None => {
            if let Ok(h) = config::home_dir() {
                v.push(h.join(".claude").join("settings.json"));
            }
        }
    }
    v
}

fn read_settings(files: &[PathBuf], r: &mut Report) -> Settings {
    let mut s = Settings::default();
    let mut found = Vec::new();
    for f in files {
        let Ok(text) = std::fs::read_to_string(f) else {
            continue;
        };
        found.push(f.display().to_string());
        let v: Value = if text.trim().is_empty() {
            json!({})
        } else {
            match serde_json::from_str(&text) {
                Ok(v) => v,
                Err(e) => {
                    r.problem(
                        "hooks",
                        format!(
                            "{} is not valid JSON ({e}): Claude Code ignores it",
                            f.display()
                        ),
                        format!("fix the JSON in {}", f.display()),
                    );
                    continue;
                }
            }
        };
        if let Some(allow) = v.pointer("/permissions/allow").and_then(Value::as_array)
            && allow
                .iter()
                .any(|p| p.as_str() == Some(install::PERMISSION))
        {
            s.permission = true;
        }
        let Some(events) = v.get("hooks").and_then(Value::as_object) else {
            continue;
        };
        for (event, groups) in events {
            let groups = groups.as_array().map(Vec::as_slice).unwrap_or_default();
            for g in groups {
                let hooks = g.get("hooks").and_then(Value::as_array);
                for h in hooks.map(Vec::as_slice).unwrap_or_default() {
                    let Some(cmd) = h.get("command").and_then(Value::as_str) else {
                        continue;
                    };
                    let Some((bin, kev)) = install::hook_parts(cmd) else {
                        continue;
                    };
                    let bin = PathBuf::from(bin);
                    if bin.file_name().is_some_and(|n| n == "kontext") {
                        s.hooks.push(Hook {
                            file: f.clone(),
                            event: event.clone(),
                            kontext_event: kev,
                            bin,
                        });
                    }
                }
            }
        }
    }
    if found.is_empty() {
        r.note(
            "hooks",
            format!(
                "no settings file found ({})",
                files
                    .iter()
                    .map(|f| f.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            None,
        );
    } else {
        r.ok("hooks", format!("read {}", found.join(", ")));
    }
    s
}

/// What to run so the hooks point at an installed binary.
fn reinstall_fix(repo: &Path, checkout: Option<&Path>) -> String {
    let install = match checkout {
        Some(c) => format!("`cargo install --path {} --locked`", c.display()),
        None => "install kontext (`cargo install --path <checkout> --locked`)".to_string(),
    };
    format!(
        "{install}, then run `kontext install` in {} with the installed binary \
         (`~/.cargo/bin/kontext install`): it repoints the hooks",
        repo.display()
    )
}

/// Runs every check. `flag` is the global `--project`.
pub fn run(flag: Option<&str>) -> Report {
    let mut r = Report::default();
    let cwd = std::env::current_dir().ok();
    let repo = cwd.as_deref().map(install::repo_root);
    let exe = std::env::current_exe().ok().map(|e| canon(&e).unwrap_or(e));

    // The hooks first: the binary and PATH checks compare against them.
    let settings = match &repo {
        Some(repo) => read_settings(&settings_files(repo), &mut r),
        None => Settings::default(),
    };
    let hook_bins: Vec<PathBuf> = {
        let mut v: Vec<PathBuf> = settings
            .hooks
            .iter()
            .map(|h| canon(&h.bin).unwrap_or_else(|| h.bin.clone()))
            .collect();
        v.sort();
        v.dedup();
        v
    };
    let repo_shown = repo.clone().unwrap_or_else(|| PathBuf::from("<repo>"));

    check_binary(&mut r, exe.as_deref(), &hook_bins, &repo_shown);
    check_path(&mut r, exe.as_deref(), &hook_bins, &repo_shown);
    let project = check_config(&mut r, flag, cwd.as_deref());
    if let Some((_, home, writer)) = &project {
        check_home(&mut r, home, writer.as_deref());
    }
    check_hooks(&mut r, &settings, exe.as_deref(), &hook_bins, &repo_shown);
    let loaded = project
        .as_ref()
        .and_then(|(name, home, _)| Project::load(home).ok().map(|p| (name, home, p)));
    if let Some((_, _, p)) = &loaded {
        check_capture(&mut r, p, &settings, cwd.as_deref(), &repo_shown);
    }
    if let Some(repo) = &repo {
        let exe_q = hook_bins
            .first()
            .or(exe.as_ref())
            .map(|p| install::shell_quote(&p.display().to_string()))
            .unwrap_or_else(|| "kontext".into());
        let with_project = loaded
            .as_ref()
            .map(|(n, h, p)| (n.as_str(), h.as_path(), p));
        check_skills(&mut r, repo, with_project, exe_q);
    }
    if let Some((_, home, _)) = &project {
        check_hook_log(&mut r, home);
    }
    r
}

fn check_binary(r: &mut Report, exe: Option<&Path>, hook_bins: &[PathBuf], repo: &Path) {
    let Some(exe) = exe else {
        r.problem(
            "binary",
            "could not find the path of this binary".into(),
            "run kontext by its full path".into(),
        );
        return;
    };
    r.ok("binary", exe.display().to_string());
    if let Some(checkout) = install::cargo_build_checkout(exe) {
        if hook_bins.iter().any(|b| b == exe) {
            r.problem(
                "binary",
                format!(
                    "it is in a cargo build directory and hooks run it: `cargo clean` in {} \
                     deletes it and the hooks stop working without a word",
                    checkout.display()
                ),
                reinstall_fix(repo, Some(&checkout)),
            );
        } else {
            r.note(
                "binary",
                format!(
                    "it is in a cargo build directory: `cargo clean` in {} deletes it \
                     (no hook found runs it)",
                    checkout.display()
                ),
                None,
            );
        }
    }
}

fn check_path(r: &mut Report, exe: Option<&Path>, hook_bins: &[PathBuf], repo: &Path) {
    let Some(found) = on_path() else {
        r.problem(
            "path",
            "no `kontext` on PATH: agents run `kontext` by name (the `Bash(kontext:*)` permission)"
                .into(),
            "`cargo install --path <checkout> --locked` (it goes to ~/.cargo/bin) and put \
             ~/.cargo/bin on PATH"
                .into(),
        );
        return;
    };
    let real = canon(&found).unwrap_or_else(|| found.clone());
    let shown = if real == found {
        found.display().to_string()
    } else {
        format!("{} (→ {})", found.display(), real.display())
    };
    let hooks: Vec<&PathBuf> = hook_bins.iter().filter(|b| b.exists()).collect();
    match hooks.as_slice() {
        [] => match exe {
            Some(e) if e != real => r.note(
                "path",
                format!("{shown}; this binary is {}", e.display()),
                None,
            ),
            _ => r.ok("path", shown),
        },
        [h] if **h == real => r.ok("path", format!("{shown}, the binary the hooks run")),
        [h] => r.problem(
            "path",
            format!(
                "{shown}, but the hooks run {}: agents and hooks run different kontext binaries",
                h.display()
            ),
            format!(
                "run `kontext install` in {} with the kontext on PATH, so the hooks use it",
                repo.display()
            ),
        ),
        many => {
            if many.iter().all(|h| **h != real) {
                r.problem(
                    "path",
                    format!("{shown}, which none of the hooks run"),
                    format!(
                        "run `kontext install` in {} with the kontext on PATH",
                        repo.display()
                    ),
                );
            } else {
                r.ok("path", shown);
            }
        }
    }
}

/// The project, its home and this machine's writer, if they can be found.
fn check_config(
    r: &mut Report,
    flag: Option<&str>,
    cwd: Option<&Path>,
) -> Option<(String, PathBuf, Option<String>)> {
    let dir = match config::config_dir() {
        Ok(d) => d,
        Err(e) => {
            r.problem(
                "config",
                format!("no config dir: {e:#}"),
                "set HOME (or KONTEXT_CONFIG_DIR)".into(),
            );
            return None;
        }
    };
    let file = dir.join("projects.toml");
    if !file.is_file() {
        r.problem(
            "config",
            format!(
                "{} does not exist: no project is registered; the project checks are skipped",
                file.display()
            ),
            "`kontext init <name>` creates a project; `kontext clone <git-url>` brings one in"
                .into(),
        );
        return None;
    }
    let machine = match Machine::load() {
        Ok(m) => m,
        Err(e) => {
            r.problem(
                "config",
                format!("{e:#}"),
                format!("fix {} by hand", file.display()),
            );
            return None;
        }
    };
    r.ok(
        "config",
        format!(
            "{} ({} registered)",
            file.display(),
            plural(machine.projects.len(), "project", "projects")
        ),
    );
    let writer = match machine.writer() {
        Ok(w) => {
            r.ok("config", format!("writer: {w}"));
            Some(w)
        }
        Err(e) => {
            r.problem(
                "config",
                format!("{e:#}"),
                format!("set a valid `writer` in {}", file.display()),
            );
            None
        }
    };
    let here = cwd.unwrap_or(Path::new("."));
    let (name, source) = match config::choose_project(flag, here, &machine) {
        Ok(x) => x,
        Err(e) => {
            r.problem(
                "config",
                format!("no project for {}: {e:#}; the project checks are skipped", here.display()),
                "run `kontext install` in the code repo (it writes .kontext.toml), or pass --project <name>"
                    .into(),
            );
            return None;
        }
    };
    let how = match &source {
        ProjectSource::Flag => "from --project".to_string(),
        ProjectSource::Env => "from $KONTEXT_PROJECT".to_string(),
        ProjectSource::Link(p) => format!("from {}", p.display()),
        ProjectSource::OnlyOne => "the only project registered".to_string(),
    };
    match machine.project_home(&name) {
        Ok(home) => {
            r.ok(
                "config",
                format!("project {name} ({how}), home {}", home.display()),
            );
            Some((name, home, writer))
        }
        Err(e) => {
            r.problem(
                "config",
                format!("project {name} ({how}): {e:#}; the project checks are skipped"),
                format!(
                    "register it (`kontext clone <git-url>` or `kontext init {name}`), or fix the name"
                ),
            );
            None
        }
    }
}

fn check_home(r: &mut Report, home: &Path, writer: Option<&str>) {
    if !home.is_dir() {
        r.problem(
            "home",
            format!("{} does not exist", home.display()),
            "`kontext clone <git-url> --home <path>` brings it back; or fix `home` in projects.toml"
                .into(),
        );
        return;
    }
    if sync::is_repo_root(home) {
        r.ok("home", format!("{} is a git repo", home.display()));
    } else {
        r.problem(
            "home",
            format!(
                "{} is not the root of a git repo: `kontext sync` cannot commit",
                home.display()
            ),
            format!(
                "git -C {} init",
                install::shell_quote(&home.display().to_string())
            ),
        );
    }
    match Project::load(home) {
        Ok(p) => r.ok("home", format!("project.toml parses (name {})", p.name)),
        Err(e) => r.problem(
            "home",
            format!("{e:#}"),
            format!("fix {} by hand", home.join("project.toml").display()),
        ),
    }
    let ledger = home.join("ledger");
    if !ledger.is_dir() {
        r.problem(
            "home",
            format!("{} is missing", ledger.display()),
            "this is not a kontext-home: fix `home` in projects.toml".into(),
        );
        return;
    }
    if let Some(w) = writer {
        let file = ledger.join(format!("{w}.jsonl"));
        match read_tail(&file) {
            Ok((0, _)) => r.ok("home", format!("no entries from writer {w} yet")),
            Ok((n, _)) => r.ok(
                "home",
                format!(
                    "this writer's file: ledger/{w}.jsonl ({})",
                    plural(n as usize, "entry", "entries")
                ),
            ),
            Err(e) => r.problem(
                "home",
                format!("ledger/{w}.jsonl: {e:#}"),
                "kontext verify".into(),
            ),
        }
    }
    match sync::remote_name(home) {
        Some(remote) => r.ok("home", format!("remote: {remote}")),
        None => r.note(
            "home",
            "no git remote: `kontext sync` only commits".into(),
            None,
        ),
    }
    match verify::run_with_media(home) {
        Ok(rep) if rep.ok() => r.ok("home", format!("chains: {}", rep.summary())),
        Ok(rep) => {
            let first: Vec<String> = rep
                .problems
                .iter()
                .chain(&rep.media_problems)
                .take(3)
                .map(ToString::to_string)
                .collect();
            r.problem(
                "home",
                format!("chains: {} ({})", rep.summary(), first.join("; ")),
                "`kontext verify` shows each problem".into(),
            );
        }
        Err(e) => r.problem(
            "home",
            format!("could not check the chains: {e:#}"),
            "kontext verify".into(),
        ),
    }
    if let Some(w) = writer {
        let p = sync::pending(home, w);
        let unpushed = p.unpushed.unwrap_or(0);
        if p.uncommitted > 0 || unpushed > 0 {
            let mut what = format!(
                "{} not committed",
                plural(p.uncommitted as usize, "own entry", "own entries")
            );
            if let Some(u) = p.unpushed {
                what.push_str(&format!(", {u} not pushed (according to the last fetch)"));
            }
            r.note("home", what, Some("kontext sync".into()));
        }
    }
}

fn check_hooks(
    r: &mut Report,
    s: &Settings,
    exe: Option<&Path>,
    hook_bins: &[PathBuf],
    repo: &Path,
) {
    // One line per binary: its events and where they are.
    for bin in hook_bins {
        let of_bin: Vec<&Hook> = s
            .hooks
            .iter()
            .filter(|h| canon(&h.bin).unwrap_or_else(|| h.bin.clone()) == *bin)
            .collect();
        let mut events: Vec<&str> = Vec::new();
        for h in &of_bin {
            if !events.contains(&h.event.as_str()) {
                events.push(&h.event);
            }
        }
        let mut files: Vec<String> = of_bin
            .iter()
            .map(|h| h.file.display().to_string())
            .collect();
        files.sort();
        files.dedup();
        let what = format!(
            "{} → {} (in {})",
            events.join(", "),
            bin.display(),
            files.join(", ")
        );
        let checkout = install::cargo_build_checkout(bin);
        if !bin.exists() {
            r.problem(
                "hooks",
                format!("{what}: the binary is missing, these hooks fail without a word"),
                reinstall_fix(repo, None),
            );
        } else if !is_executable(bin) {
            r.problem(
                "hooks",
                format!("{what}: the binary is not executable"),
                format!(
                    "chmod +x {}",
                    install::shell_quote(&bin.display().to_string())
                ),
            );
        } else if checkout.is_some() && exe != Some(bin.as_path()) {
            r.problem(
                "hooks",
                format!("{what}: a cargo build directory, which `cargo clean` deletes"),
                reinstall_fix(repo, checkout.as_deref()),
            );
        } else if exe.is_some_and(|e| e != bin) {
            r.note(
                "hooks",
                format!(
                    "{what}; this binary is {}",
                    exe.map(|e| e.display().to_string()).unwrap_or_default()
                ),
                None,
            );
        } else {
            r.ok("hooks", format!("{what}, this binary"));
        }
    }
    if hook_bins.len() > 1 {
        r.problem(
            "hooks",
            format!("the hooks point at {} different binaries", hook_bins.len()),
            format!(
                "run `kontext install` in {} (it repoints the repo's hooks) and remove the \
                 stale `kontext hook` entries from the other settings files by hand",
                repo.display()
            ),
        );
    }
    if !s
        .hooks
        .iter()
        .any(|h| h.event == "SessionStart" && h.kontext_event == "session-start")
    {
        r.problem(
            "hooks",
            "no `SessionStart` hook runs `kontext hook session-start`: sessions start without kontext"
                .into(),
            format!("kontext install (in {})", repo.display()),
        );
    }
    if s.permission {
        r.ok(
            "hooks",
            format!("permission {} is there", install::PERMISSION),
        );
    } else {
        r.problem(
            "hooks",
            format!(
                "no {} permission: each `kontext` an agent runs asks for approval",
                install::PERMISSION
            ),
            format!("kontext install (in {})", repo.display()),
        );
    }
}

fn check_capture(r: &mut Report, project: &Project, s: &Settings, cwd: Option<&Path>, repo: &Path) {
    let settings = capture::Settings::of(project);
    let has = |ev: &str| s.hooks.iter().any(|h| h.kontext_event == ev);
    let missing: Vec<&str> = [
        ("UserPromptSubmit", "user-prompt-submit"),
        ("Stop", "stop"),
        ("SessionEnd", "session-end"),
    ]
    .iter()
    .filter(|(_, ev)| !has(ev))
    .map(|(name, _)| *name)
    .collect();
    let any = has("user-prompt-submit") || has("stop");
    match (settings.enabled, missing.is_empty()) {
        (true, true) => r.ok("capture", "on, with its hooks".into()),
        (true, false) => r.problem(
            "capture",
            format!(
                "[capture] enabled = true, but these hooks are missing: {}; nothing is captured",
                missing.join(", ")
            ),
            format!("kontext install (in {})", repo.display()),
        ),
        (false, _) if any => r.note(
            "capture",
            "capture hooks are installed but [capture] enabled is not true: they do nothing".into(),
            None,
        ),
        (false, _) => r.ok("capture", "off".into()),
    }
    if settings.enabled
        && !settings.worktrees
        && let Some((_, true)) = cwd.and_then(crate::git::checkout)
    {
        r.note(
            "capture",
            "this is a linked git worktree and [capture] worktrees is not true: messages typed \
             here are not captured"
                .into(),
            None,
        );
    }
}

fn check_skills(
    r: &mut Report,
    repo: &Path,
    project: Option<(&str, &Path, &Project)>,
    exe: String,
) {
    let skills_dir = repo.join(".claude").join("skills");
    let agents_dir = repo.join(".claude").join("agents");
    let plan = match project {
        Some((name, home, p)) => install::plan(&install::Options {
            repo,
            project_name: name,
            home,
            default_role: "pm",
            exe,
            distill: crate::distill::Settings::of(p).is_ok_and(|d| d.enabled),
            capture: capture::Settings::of(p).enabled,
            force: false,
        }),
        None => install::plan_skills(&skills_dir),
    };
    let plan = match plan {
        Ok(p) => p,
        Err(e) => {
            r.note("skills", format!("could not check them: {e:#}"), None);
            return;
        }
    };
    let rel = |p: &Path| p.strip_prefix(repo).unwrap_or(p).display().to_string();
    let stale = |dir: &Path| -> (Vec<String>, Vec<String>) {
        let mut missing = Vec::new();
        let mut old = Vec::new();
        for c in plan
            .changes
            .iter()
            .filter(|c| c.path.starts_with(dir) && c.changed())
        {
            if c.before.is_some() {
                old.push(rel(&c.path));
            } else {
                missing.push(rel(&c.path));
            }
        }
        (missing, old)
    };
    let (missing, old) = stale(&skills_dir);
    if missing.is_empty() && old.is_empty() {
        r.ok(
            "skills",
            format!("skills current in {}", skills_dir.display()),
        );
    } else {
        let user = match std::env::var_os("CLAUDE_CONFIG_DIR").filter(|d| !d.is_empty()) {
            Some(d) => Some(PathBuf::from(d).join("skills")),
            None => config::home_dir()
                .ok()
                .map(|h| h.join(".claude").join("skills")),
        };
        let global_ok = old.is_empty()
            && user
                .as_ref()
                .and_then(|u| install::plan_skills(u).ok())
                .is_some_and(|p| p.is_noop());
        if global_ok {
            r.ok(
                "skills",
                format!(
                    "skills current in {}",
                    user.map(|u| u.display().to_string()).unwrap_or_default()
                ),
            );
        } else {
            r.note(
                "skills",
                outdated("skills", &missing, &old),
                Some("kontext install".into()),
            );
        }
    }
    if project.is_none() {
        r.note(
            "skills",
            "agent files not checked: there is no project".into(),
            None,
        );
        return;
    }
    let (missing, old) = stale(&agents_dir);
    if missing.is_empty() && old.is_empty() && plan.kept.is_empty() {
        r.ok(
            "skills",
            format!("agent files current in {}", agents_dir.display()),
        );
    } else if !missing.is_empty() || !old.is_empty() {
        r.note(
            "skills",
            outdated("agent files", &missing, &old),
            Some("kontext install".into()),
        );
    }
    if !plan.kept.is_empty() {
        let kept: Vec<String> = plan.kept.iter().map(|p| rel(p)).collect();
        r.note(
            "skills",
            format!(
                "agent files that differ from this version (changed by hand, or from another version): {}",
                kept.join(", ")
            ),
            Some("`kontext install --force` replaces them".into()),
        );
    }
}

fn outdated(what: &str, missing: &[String], old: &[String]) -> String {
    let mut parts = Vec::new();
    if !missing.is_empty() {
        parts.push(format!("missing: {}", missing.join(", ")));
    }
    if !old.is_empty() {
        parts.push(format!("not this version: {}", old.join(", ")));
    }
    format!("{what} not current ({})", parts.join("; "))
}

/// `hooks.log` lines are `<RFC 3339 time> <what happened>` (`hook::log_line`),
/// and an error says `error: ` (`<event>: error: …`, `distill error: …`,
/// `capture error: …`).
fn check_hook_log(r: &mut Report, home: &Path) {
    let path = config::internal_dir(home).join("hooks.log");
    let Ok(f) = File::open(&path) else {
        r.ok("hook_log", format!("no {} yet", path.display()));
        return;
    };
    let (mut lines, mut known, mut errors) = (0u64, 0u64, 0u64);
    let mut last_error: Option<String> = None;
    for line in BufReader::new(f).lines().map_while(Result::ok) {
        if line.trim().is_empty() {
            continue;
        }
        lines += 1;
        let Some((ts, msg)) = line.split_once(' ') else {
            continue;
        };
        if ts.parse::<jiff::Timestamp>().is_err() {
            continue;
        }
        known += 1;
        if msg.contains(" error: ") || msg.starts_with("error: ") {
            errors += 1;
            last_error = Some(ts.to_string());
        }
    }
    if known == 0 && lines > 0 {
        r.note(
            "hook_log",
            format!("{}: format not recognized", path.display()),
            None,
        );
        return;
    }
    match last_error {
        None => r.ok(
            "hook_log",
            format!(
                "{}: {}, no errors",
                path.display(),
                plural(lines as usize, "line", "lines")
            ),
        ),
        Some(at) => r.note(
            "hook_log",
            format!(
                "{}: {} with an error, the last at {at}",
                path.display(),
                plural(errors as usize, "line", "lines")
            ),
            Some(format!("read the error lines in {}", path.display())),
        ),
    }
}
