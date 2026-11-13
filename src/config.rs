//! Configuration (DESIGN §11): `~/.config/kontext/projects.toml` per machine,
//! the versioned `<kontext-home>/project.toml`, and `<repo>/.kontext.toml`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

/// The configuration folder: `$KONTEXT_CONFIG_DIR` or `~/.config/kontext`.
pub fn config_dir() -> Result<PathBuf> {
    if let Some(d) = std::env::var_os("KONTEXT_CONFIG_DIR").filter(|d| !d.is_empty()) {
        return Ok(PathBuf::from(d));
    }
    Ok(home_dir()?.join(".config").join("kontext"))
}

/// The internal folder of a kontext-home (index, locks, logs, distill marks):
/// local, never uploaded.
pub const INTERNAL_DIR: &str = ".kontext";

/// `<home>/.kontext`.
pub fn internal_dir(home: &Path) -> PathBuf {
    home.join(INTERNAL_DIR)
}

/// Creates `<home>/.kontext` with a `.gitignore` inside that ignores all of it,
/// so it does not show up in `git status` whatever the versioned `.gitignore`
/// of the kontext-home says.
pub fn ensure_internal_dir(home: &Path) -> Result<PathBuf> {
    let dir = internal_dir(home);
    fs::create_dir_all(&dir)?;
    let ignore = dir.join(".gitignore");
    if !ignore.exists() {
        fs::write(&ignore, "*\n")?;
    }
    Ok(dir)
}

pub fn home_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .context("the HOME variable is not set")
}

/// Expands a leading `~`.
pub fn expand_tilde(p: &str) -> Result<PathBuf> {
    if p == "~" {
        return home_dir();
    }
    if let Some(rest) = p.strip_prefix("~/") {
        return Ok(home_dir()?.join(rest));
    }
    Ok(PathBuf::from(p))
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ProjectRef {
    pub home: String,
    /// Where each repo of `[[repos]]` is on this machine (phase 4):
    /// `[projects.<p>.repos] <nombre> = "<ruta>"`.
    #[serde(default)]
    pub repos: BTreeMap<String, String>,
}

/// `projects.toml`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Machine {
    pub writer: Option<String>,
    #[serde(default)]
    pub projects: BTreeMap<String, ProjectRef>,
    /// Phase 5: which SSH key signs the entries that need a signature on this
    /// machine (`[signing] key = "~/.ssh/id_ed25519.pub"`).
    #[serde(default)]
    pub signing: Option<Signing>,
}

/// `[signing]` of `projects.toml`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Signing {
    /// The key (public or private; `ssh-keygen -Y sign` looks for the private
    /// one next to the public one, or in the agent).
    pub key: Option<String>,
}

impl Machine {
    pub fn path() -> Result<PathBuf> {
        Ok(config_dir()?.join("projects.toml"))
    }

    pub fn load() -> Result<Machine> {
        let p = Machine::path()?;
        match fs::read_to_string(&p) {
            Ok(s) => toml::from_str(&s).with_context(|| format!("could not read {}", p.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Machine::default()),
            Err(e) => Err(e).with_context(|| format!("could not read {}", p.display())),
        }
    }

    /// The writer of this machine: `writer` from `projects.toml`, or
    /// `<user>@<machine>`.
    pub fn writer(&self) -> Result<String> {
        let w = match &self.writer {
            Some(w) => w.clone(),
            None => default_writer(),
        };
        validate_writer(&w)?;
        Ok(w)
    }

    /// The key from `[signing] key`, already expanded.
    pub fn signing_key(&self) -> Result<Option<PathBuf>> {
        match self.signing.as_ref().and_then(|s| s.key.as_deref()) {
            Some(k) if !k.trim().is_empty() => Ok(Some(expand_tilde(k.trim())?)),
            _ => Ok(None),
        }
    }

    pub fn project_home(&self, name: &str) -> Result<PathBuf> {
        match self.projects.get(name) {
            Some(p) => expand_tilde(&p.home),
            None => bail!(
                "project {name:?} is not registered in {}",
                Machine::path()?.display()
            ),
        }
    }

    /// Registers a project. If the file has no `writer`, it sets this machine's
    /// so that it does not change even if the computer's name changes.
    pub fn register(name: &str, home: &Path) -> Result<()> {
        let p = Machine::path()?;
        let mut table: toml::Table = match fs::read_to_string(&p) {
            Ok(s) => {
                toml::from_str(&s).with_context(|| format!("could not read {}", p.display()))?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => toml::Table::new(),
            Err(e) => return Err(e).with_context(|| format!("could not read {}", p.display())),
        };
        if !table.contains_key("writer") {
            table.insert("writer".into(), default_writer().into());
        }
        let projects = table
            .entry("projects")
            .or_insert_with(|| toml::Value::Table(toml::Table::new()))
            .as_table_mut()
            .context("`projects` in projects.toml must be a table")?;
        if projects.contains_key(name) {
            bail!("project {name:?} is already registered in {}", p.display());
        }
        let mut entry = toml::Table::new();
        entry.insert("home".into(), home.display().to_string().into());
        projects.insert(name.into(), toml::Value::Table(entry));
        fs::create_dir_all(p.parent().unwrap())?;
        write_atomic(&p, toml::to_string(&table)?.as_bytes())
    }
}

/// `<user>@<machine>`, in lowercase and without the domain.
pub fn default_writer() -> String {
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "user".into());
    let host = hostname();
    let clean = |s: &str| -> String {
        s.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                    c.to_ascii_lowercase()
                } else {
                    '-'
                }
            })
            .collect()
    };
    format!("{}@{}", clean(&user), clean(&host))
}

fn hostname() -> String {
    let raw = std::env::var("HOSTNAME")
        .ok()
        .filter(|h| !h.trim().is_empty())
        .or_else(|| {
            Command::new("hostname")
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        })
        .or_else(|| fs::read_to_string("/etc/hostname").ok())
        .unwrap_or_else(|| "localhost".into());
    let raw = raw.trim();
    raw.split('.').next().unwrap_or(raw).to_string()
}

/// The writer is part of a file name: only letters, digits, and `._@-`.
pub fn validate_writer(w: &str) -> Result<()> {
    let ok = !w.is_empty()
        && !w.starts_with('.')
        && w.len() <= 128
        && w.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '@' | '-'));
    if !ok {
        bail!("invalid writer {w:?}: use letters, digits, and . _ @ - (for example omar@mac)");
    }
    Ok(())
}

/// `project.toml` (DESIGN §11). In phase 1 only `name` and `owners` are used;
/// the rest is read so that a format error shows up early.
#[derive(Debug, Clone, Deserialize)]
pub struct Project {
    pub name: String,
    #[serde(default)]
    pub owners: Vec<String>,
    #[serde(default)]
    pub require_signatures: bool,
    #[serde(default)]
    pub repos: Vec<Repo>,
    #[serde(default)]
    pub context: BTreeMap<String, String>,
    #[serde(default)]
    pub budgets: BTreeMap<String, u64>,
    #[serde(default)]
    pub distill: Option<Distill>,
    /// The capture hook (DESIGN §12.3): off unless `enabled = true`.
    #[serde(default)]
    pub capture: Option<CaptureCfg>,
    #[serde(default)]
    pub claims: Option<Claims>,
    /// Phase 5: who is who. `omar = { emails = ["…"], keys = ["ssh-ed25519 AAAA… omar@mac"] }`.
    #[serde(default)]
    pub people: BTreeMap<String, Person>,
    /// The human's inbox: with how many pending items `kontext ask` stops the agents.
    #[serde(default)]
    pub inbox: Option<InboxCfg>,
    /// The evidence images: the limit for each one.
    #[serde(default)]
    pub media: Option<MediaCfg>,
}

/// `[inbox]` of `project.toml`.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct InboxCfg {
    /// With this many unanswered pending items, an agent's question to the human
    /// is marked as not urgent unless it says `--blocking` (5).
    pub max_pending: Option<usize>,
}

/// `[media]` of `project.toml`.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct MediaCfg {
    /// Limit for each image: `"2MB"`, `"500KB"`, or bytes (2 MB).
    pub max_size: Option<toml::Value>,
}

/// How many pending items stop `kontext ask` by default.
pub const DEFAULT_MAX_PENDING: usize = 5;

impl Project {
    /// `[inbox] max_pending`, or 5.
    pub fn max_pending(&self) -> usize {
        self.inbox
            .as_ref()
            .and_then(|i| i.max_pending)
            .unwrap_or(DEFAULT_MAX_PENDING)
    }

    /// `[media] max_size` in bytes, or 2 MB.
    pub fn media_max_bytes(&self) -> Result<u64> {
        match self.media.as_ref().and_then(|m| m.max_size.as_ref()) {
            None => Ok(crate::media::DEFAULT_MAX_BYTES),
            Some(toml::Value::Integer(n)) if *n > 0 => Ok(*n as u64),
            Some(toml::Value::String(s)) => crate::media::parse_size(s)
                .map_err(|e| anyhow::anyhow!("[media] max_size in project.toml: {e}")),
            Some(v) => anyhow::bail!(
                "[media] max_size in project.toml: {v} is not a size (use \"2MB\", \"500KB\", or bytes)"
            ),
        }
    }
}

/// A person in `[people]`: their git emails and their public SSH keys.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct Person {
    #[serde(default)]
    pub emails: Vec<String>,
    #[serde(default)]
    pub keys: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Repo {
    pub name: String,
    #[serde(default)]
    pub remote: Option<String>,
    /// Which docs `kontext find` reads (globs from the repo root). By default,
    /// `**/*.md`.
    #[serde(default)]
    pub docs: Vec<String>,
    /// Globs that `kontext find` never reads (in addition to node_modules, target, and .git).
    #[serde(default)]
    pub exclude: Vec<String>,
    /// Doc globs that are historical by their path (snapshots, old plans):
    /// `kontext find` hides them unless `--all` is passed.
    #[serde(default)]
    pub historical: Vec<String>,
    /// Top-level folders that a repo path cited in a doc starts with. By default,
    /// the top-level folders that git tracks.
    #[serde(default)]
    pub roots: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Distill {
    pub command: Option<String>,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// How long a call to the model may take (`240s`, `4m`).
    #[serde(default)]
    pub timeout: Option<String>,
    /// The model, for the actor of what the distill writes. If it is not set,
    /// it is taken from `--model <x>` in `command`.
    #[serde(default)]
    pub model: Option<String>,
    /// The most entries each run writes (8).
    #[serde(default)]
    pub max_entries: Option<usize>,
    /// Messages longer than this (a pasted document) are cut before they are
    /// sent to the model (4,000 characters; 0: never cut).
    #[serde(default)]
    pub max_message_chars: Option<usize>,
}

fn yes() -> bool {
    true
}

/// `[capture]` of `project.toml`: the model-free hook that writes each of the
/// user's messages into `.pm/dumps/` of the code repo.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct CaptureCfg {
    /// Off unless it says `true`.
    #[serde(default)]
    pub enabled: bool,
    /// Prompts that start with one of these (after leading whitespace) are
    /// machine text and are not written, on top of the built-in ones.
    #[serde(default)]
    pub skip_prefixes: Vec<String>,
    /// Capture in every linked git worktree, into its own `.pm/` (off by
    /// default: only a worktree marked as the PM's captures, into the main
    /// checkout's `.pm/`).
    #[serde(default)]
    pub worktrees: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Claims {
    pub lease: Option<String>,
}

impl Project {
    pub fn load(home: &Path) -> Result<Project> {
        let p = home.join("project.toml");
        let s = fs::read_to_string(&p).with_context(|| {
            format!(
                "{} is not a kontext-home: {} is missing",
                home.display(),
                p.display()
            )
        })?;
        toml::from_str(&s).with_context(|| format!("could not read {}", p.display()))
    }

    /// The `project.toml` that `kontext init` writes, with the default values
    /// from DESIGN §11.
    pub fn template(name: &str, owner: &str) -> String {
        format!(
            r#"name = {name}
owners = [{owner}]                 # who signs the charter and decides
require_signatures = false        # phase 5

# Who is who (phase 5): the git email says which person writes, and the
# keys verify their signatures.
# [people]
# omar = {{ emails = ["omar@example.com"], keys = ["ssh-ed25519 AAAA… omar@mac"] }}

# [[repos]]
# name = "my-repo"
# remote = "github.com/user/my-repo"
# docs = ["docs/**/*.md", "**/README.md"]

[context]
pm = "inject"
coordinator = "inject"
default = "inject"

[budgets]
pm = 9000
coordinator = 9000

[distill]
command = "claude -p --model haiku --output-format json"   # receives the prompt on stdin
enabled = true

# Capture: kontext writes each message you send to the pm session, verbatim,
# into .pm/dumps/<date>.md of the code repo (no model). Off by default; run
# `kontext install` again after turning it on.
# [capture]
# enabled = true
# skip_prefixes = ["<my-tool-output>"]   # prompts that start like this are machine text
# worktrees = false                      # true: in every linked git worktree (its own .pm/)

[claims]
lease = "2h"

# The human's inbox: with this many pending items, an agent's question
# is marked as not urgent unless it says --blocking.
# [inbox]
# max_pending = 5

# Limit for each screenshot of evidence.
# [media]
# max_size = "2MB"
"#,
            name = toml::Value::from(name),
            owner = toml::Value::from(owner),
        )
    }
}

impl Project {
    /// The person in `[people]` who has this email (case-insensitive).
    pub fn person_by_email(&self, email: &str) -> Option<&str> {
        let email = email.trim();
        self.people
            .iter()
            .find(|(_, p)| {
                p.emails
                    .iter()
                    .any(|e| e.trim().eq_ignore_ascii_case(email))
            })
            .map(|(n, _)| n.as_str())
    }
}

/// `git config user.email` as seen from `dir` (respects the repo's configuration).
pub fn git_email(dir: &Path) -> Option<String> {
    Command::new("git")
        .args(["config", "user.email"])
        .current_dir(dir)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

/// The human on this machine for a project: the person in `[people]` with the
/// git email of the kontext-home, or else [`human_name`].
pub fn human_name_in(project: &Project, home: &Path) -> String {
    git_email(home)
        .and_then(|e| project.person_by_email(&e).map(str::to_string))
        .unwrap_or_else(human_name)
}

/// Phase 5 (DESIGN §14): if someone writes as `human:<name>` and this machine's
/// git email does not belong to that person in `[people]`, a warning. It is not
/// rejected: with `require_signatures` the signature is what counts, and without
/// `[people]` there is nothing to compare against.
pub fn identity_warning(project: &Project, home: &Path, name: &str) -> Option<String> {
    if project.people.is_empty() || project.require_signatures {
        return None;
    }
    let email = git_email(home);
    let mapped = email.as_deref().and_then(|e| project.person_by_email(e));
    if mapped == Some(name) {
        return None;
    }
    let who = match (&email, mapped) {
        (None, _) => "there is no `git config user.email` on this machine".to_string(),
        (Some(e), Some(p)) => format!("this machine's git email ({e}) belongs to {p}"),
        (Some(e), None) => format!("this machine's git email ({e}) is not in [people]"),
    };
    Some(format!(
        "you write as human:{name}, but {who}. If you are {name}, add your email to \
         [people] in project.toml"
    ))
}

/// The human's name on this machine: the first word of
/// `git config user.name`, or `$USER`, in lowercase.
pub fn human_name() -> String {
    let from_git = Command::new("git")
        .args(["config", "user.name"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
    let raw = from_git
        .filter(|s| !s.trim().is_empty())
        .or_else(|| std::env::var("USER").ok())
        .unwrap_or_else(|| "owner".into());
    raw.split_whitespace()
        .next()
        .unwrap_or("owner")
        .to_lowercase()
}

/// `.kontext.toml` of a code repo.
#[derive(Debug, Clone, Deserialize)]
pub struct RepoLink {
    pub project: String,
    #[serde(default)]
    pub default_role: Option<String>,
    /// The role of a session in a linked git worktree that has no
    /// `.kontext.toml` of its own (read from the main checkout's file).
    #[serde(default)]
    pub worktree_role: Option<String>,
}

/// Whose a `.kontext.toml` found for a folder is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkKind {
    /// The checkout's own: in a main checkout (or outside git) the nearest
    /// one; in a linked git worktree, one git does not track.
    Own,
    /// In a linked git worktree, one git tracks: the repo's file, the same in
    /// every checkout, so not the worktree's own.
    Tracked,
    /// In a linked git worktree without one, the main checkout's.
    MainCheckout,
}

/// A `.kontext.toml` found for a folder.
#[derive(Debug, Clone)]
pub struct FoundLink {
    pub path: PathBuf,
    pub link: RepoLink,
    pub kind: LinkKind,
}

impl FoundLink {
    /// The repo's file rather than a linked worktree's own: its
    /// `worktree_role` applies.
    pub fn for_worktrees(&self) -> bool {
        self.kind != LinkKind::Own
    }
}

fn read_link(p: &Path) -> Result<RepoLink> {
    let s = fs::read_to_string(p)?;
    toml::from_str(&s).with_context(|| format!("could not read {}", p.display()))
}

fn found(path: PathBuf, kind: LinkKind) -> Result<Option<FoundLink>> {
    let link = read_link(&path)?;
    Ok(Some(FoundLink { path, link, kind }))
}

/// The `.kontext.toml` for `start`: the nearest one walking up from it. In a
/// linked git worktree it first walks up only to the worktree's root (a file
/// git tracks there is the repo's, [`LinkKind::Tracked`]), then takes the one
/// at the root of the main checkout (there is none for a bare repository),
/// and only then goes on walking up from above the worktree.
pub fn find_repo_link(start: &Path) -> Result<Option<FoundLink>> {
    let mut above: Option<PathBuf> = None;
    if let Some((root, true)) = crate::git::checkout(start) {
        let start = start.canonicalize().unwrap_or_else(|_| start.to_path_buf());
        let root = root.canonicalize().unwrap_or(root);
        for dir in start.ancestors().take_while(|d| d.starts_with(&root)) {
            let p = dir.join(".kontext.toml");
            if p.is_file() {
                let rel = p.strip_prefix(&root).unwrap_or(&p).to_string_lossy();
                let kind = if crate::git::tracked(&root, &rel) {
                    LinkKind::Tracked
                } else {
                    LinkKind::Own
                };
                return found(p, kind);
            }
        }
        if let Some(p) = crate::git::main_checkout(&root).map(|m| m.join(".kontext.toml"))
            && p.is_file()
        {
            return found(p, LinkKind::MainCheckout);
        }
        match root.parent() {
            Some(p) => above = Some(p.to_path_buf()),
            None => return Ok(None),
        }
    }
    for dir in above.as_deref().unwrap_or(start).ancestors() {
        let p = dir.join(".kontext.toml");
        if p.is_file() {
            return found(p, LinkKind::Own);
        }
    }
    Ok(None)
}

/// The role of a session in `dir`, and where it came from: `$KONTEXT_ROLE`;
/// the `default_role` of the checkout's own `.kontext.toml`; in a linked
/// worktree without one, the repo's file (tracked, or the main checkout's):
/// its `worktree_role`, then its `default_role`; then `pm`.
pub fn role_in(dir: &Path) -> (String, String) {
    if let Ok(r) = std::env::var("KONTEXT_ROLE")
        && !r.trim().is_empty()
    {
        return (r.trim().to_string(), "$KONTEXT_ROLE".into());
    }
    let set = |r: &Option<String>| {
        r.as_deref()
            .map(str::trim)
            .filter(|r| !r.is_empty())
            .map(str::to_string)
    };
    if let Ok(Some(f)) = find_repo_link(dir) {
        let keys: &[(&str, &Option<String>)] = if f.for_worktrees() {
            &[
                ("worktree_role", &f.link.worktree_role),
                ("default_role", &f.link.default_role),
            ]
        } else {
            &[("default_role", &f.link.default_role)]
        };
        for (key, value) in keys {
            if let Some(r) = set(value) {
                return (r, format!("{key} in {}", f.path.display()));
            }
        }
    }
    ("pm".into(), "no role set".into())
}

/// A project that is already resolved.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub name: String,
    pub home: PathBuf,
    pub writer: String,
}

/// Which project to use: `--project`, then `$KONTEXT_PROJECT`, then the nearest
/// `.kontext.toml` going up (in a linked git worktree without one, the main
/// checkout's: [`find_repo_link`]), and if only one is registered, that one.
pub fn resolve_project(flag: Option<&str>) -> Result<Resolved> {
    resolve_project_in(flag, &std::env::current_dir()?)
}

/// Like [`resolve_project`], looking for the `.kontext.toml` from `dir`.
pub fn resolve_project_in(flag: Option<&str>, dir: &Path) -> Result<Resolved> {
    let machine = Machine::load()?;
    let writer = machine.writer()?;
    let (name, _) = choose_project(flag, dir, &machine)?;
    let home = machine.project_home(&name)?;
    Project::load(&home)?;
    Ok(Resolved { name, home, writer })
}

/// Where the name of the project came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectSource {
    /// `--project`.
    Flag,
    /// `$KONTEXT_PROJECT`.
    Env,
    /// The `.kontext.toml` at this path, and whose it is.
    Link(PathBuf, LinkKind),
    /// The only project registered.
    OnlyOne,
}

/// The name of the project to use and where it came from, in the order of
/// [`resolve_project`], without checking that it is registered.
pub fn choose_project(
    flag: Option<&str>,
    dir: &Path,
    machine: &Machine,
) -> Result<(String, ProjectSource)> {
    let env = std::env::var("KONTEXT_PROJECT")
        .ok()
        .filter(|s| !s.is_empty());
    Ok(if let Some(n) = flag {
        (n.to_string(), ProjectSource::Flag)
    } else if let Some(n) = env {
        (n, ProjectSource::Env)
    } else if let Some(f) = find_repo_link(dir)? {
        (f.link.project, ProjectSource::Link(f.path, f.kind))
    } else if machine.projects.len() == 1 {
        (
            machine.projects.keys().next().unwrap().clone(),
            ProjectSource::OnlyOne,
        )
    } else if machine.projects.is_empty() {
        bail!(
            "no projects are registered in {}. Create one with `kontext init <name>`",
            Machine::path()?.display()
        );
    } else {
        let names: Vec<&str> = machine.projects.keys().map(String::as_str).collect();
        bail!(
            "several projects are registered ({}). Choose one with --project <name>, \
             the KONTEXT_PROJECT variable, or a .kontext.toml with project = \"<name>\" in the repo",
            names.join(", ")
        );
    })
}

/// Writes a file in one go (temporary file and `rename`).
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().context("path without a folder")?;
    let tmp = dir.join(format!(
        ".{}.tmp-{}",
        path.file_name().unwrap().to_string_lossy(),
        std::process::id()
    ));
    fs::write(&tmp, bytes)?;
    fs::File::open(&tmp)?.sync_all()?;
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Writes a file only its owner can read (0600), in one go, creating its folder
/// (0700) if needed: secrets and tokens.
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let dir = path.parent().context("path without a folder")?;
    if !dir.exists() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .with_context(|| format!("could not create {}", dir.display()))?;
    }
    let tmp = dir.join(format!(
        ".{}.tmp-{}",
        path.file_name().unwrap().to_string_lossy(),
        std::process::id()
    ));
    let _ = fs::remove_file(&tmp);
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .with_context(|| format!("could not write {}", tmp.display()))?;
    f.write_all(bytes)?;
    f.sync_all()?;
    drop(f);
    fs::rename(&tmp, path).with_context(|| format!("could not write {}", path.display()))?;
    Ok(())
}

/// `n` random bytes from the system, in hex.
pub fn random_hex(n: usize) -> Result<String> {
    use std::io::Read;
    let mut buf = vec![0u8; n];
    fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .context("could not read /dev/urandom")?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_files_are_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sub").join("secret");
        write_private(&p, b"x").unwrap();
        write_private(&p, b"y").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"y");
        assert_eq!(
            fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(p.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(random_hex(16).unwrap().len(), 32);
        assert_ne!(random_hex(16).unwrap(), random_hex(16).unwrap());
    }

    #[test]
    fn writer_validation() {
        assert!(validate_writer("omar@t3-machine").is_ok());
        assert!(validate_writer("ana_1@laptop.local").is_ok());
        assert!(validate_writer("../etc").is_err());
        assert!(validate_writer("a/b").is_err());
        assert!(validate_writer("").is_err());
    }

    #[test]
    fn template_parses_with_defaults() {
        let p: Project = toml::from_str(&Project::template("billing", "omar")).unwrap();
        assert_eq!(p.name, "billing");
        assert_eq!(p.owners, vec!["omar"]);
        assert!(!p.require_signatures);
        assert_eq!(p.context.get("default").map(String::as_str), Some("inject"));
        assert_eq!(p.budgets.get("pm"), Some(&9000));
        assert_eq!(p.claims.unwrap().lease.as_deref(), Some("2h"));
    }

    #[test]
    fn default_writer_shape() {
        let w = default_writer();
        assert!(w.contains('@'));
        assert!(validate_writer(&w).is_ok());
    }
}
