//! Several people (DESIGN §10 and §14, phase 5): `kontext sync`, `kontext clone`
//! and `kontext status`.
//!
//! `sync` pushes and pulls the `kontext-home` with git:
//! 1. `git fetch` from the remote (without the lock: it can take a while).
//! 2. Under the locks of all the writers on this machine (so no `append` or
//!    cursor is left half-done in the commit, and nobody writes while the rebase
//!    rewrites the files): `git add` of `ledger/` and `cursors/` (and of
//!    `project.toml`/`synonyms.toml` only with `--include-config`), commit
//!    `kontext: <writer> <N> entries`, and `git rebase` onto what was fetched.
//!    This is `pull --rebase` in two steps, so the lock is not held while the
//!    network is in use. If git collides (it should not: each writer has its own
//!    file), the rebase is aborted and the repo is left as it was.
//! 3. `git push -u` (without the lock). If someone pushed in the meantime, it
//!    starts again from 1 (three times at most).
//!
//! Afterwards it catches up the index and summarizes what was received per
//! writer, the new alerts, and the chain of the files that arrived.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::config::{self, Machine, Project};
use crate::model::Entry;
use crate::state::Alert;
use crate::store::{Refused, Store, ledger_files, lock_writer, read_tail};
use crate::verify::{self, Problem};
use crate::views::plural;

/// The versioned configuration files that `sync` only pushes with
/// `--include-config`.
pub const CONFIG_FILES: &[&str] = &["project.toml", "synonyms.toml"];

fn git_cmd(dir: &Path, args: &[&str]) -> Result<Output> {
    Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .context("could not run git")
}

/// `git <args>`, requiring it to succeed; returns stdout.
fn git_ok(dir: &Path, args: &[&str]) -> Result<String> {
    let out = git_cmd(dir, args)?;
    if !out.status.success() {
        bail!(
            "`git {}` failed in {}: {}",
            args.join(" "),
            dir.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// `git <args>`; `None` if it fails.
fn git_try(dir: &Path, args: &[&str]) -> Option<String> {
    git_cmd(dir, args)
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// If the repo has no `user.email`, the commit carries the writer as the author.
fn identity_args(home: &Path, name: &str) -> Vec<String> {
    if config::git_email(home).is_some() {
        return Vec::new();
    }
    vec![
        "-c".into(),
        format!("user.name={name}"),
        "-c".into(),
        format!("user.email={name}"),
    ]
}

fn lines(s: &str) -> Vec<String> {
    s.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// The remote: `origin`, or the first one there is.
pub fn remote_name(home: &Path) -> Option<String> {
    let all = lines(&git_try(home, &["remote"])?);
    if all.iter().any(|r| r == "origin") {
        return Some("origin".into());
    }
    all.into_iter().next()
}

fn branch(home: &Path) -> Option<String> {
    git_try(home, &["symbolic-ref", "--short", "-q", "HEAD"]).filter(|s| !s.is_empty())
}

fn has_rev(home: &Path, rev: &str) -> bool {
    git_try(
        home,
        &["rev-parse", "-q", "--verify", &format!("{rev}^{{commit}}")],
    )
    .is_some()
}

/// Is `home` the root of a git repo?
pub fn is_repo_root(home: &Path) -> bool {
    let Some(top) = git_try(home, &["rev-parse", "--show-toplevel"]) else {
        return false;
    };
    let a = fs::canonicalize(&top).unwrap_or_else(|_| PathBuf::from(&top));
    let b = fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    a == b
}

/// How many lines the file of `writer` has at `rev` (0 if it is not there).
fn lines_at(home: &Path, rev: &str, writer: &str) -> u64 {
    git_try(home, &["show", &format!("{rev}:ledger/{writer}.jsonl")])
        .map(|s| s.lines().filter(|l| !l.trim().is_empty()).count() as u64)
        .unwrap_or(0)
}

fn last_sync_file(home: &Path) -> PathBuf {
    config::internal_dir(home).join("last-sync")
}

/// When this machine last synced with the remote.
pub fn last_sync(home: &Path) -> Option<String> {
    fs::read_to_string(last_sync_file(home))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// What `kontext sync` did.
#[derive(Debug, Clone, Serialize, Default)]
pub struct SyncReport {
    pub writer: String,
    pub remote: Option<String>,
    pub branch: Option<String>,
    /// The new local commit (short sha) and how many of its own entries it carries.
    pub commit: Option<String>,
    pub committed_entries: u64,
    pub pulled: bool,
    pub pushed: bool,
    /// Entries received per writer.
    pub received: BTreeMap<String, u64>,
    /// Alerts that were not there before bringing in others' changes.
    pub new_alerts: Vec<Alert>,
    /// Chain problems in the files that arrived.
    pub incoming_problems: Vec<Problem>,
    /// Chain problems in other files.
    pub other_problems: Vec<Problem>,
    pub warnings: Vec<String>,
}

impl SyncReport {
    pub fn received_total(&self) -> u64 {
        self.received.values().sum()
    }

    pub fn ok(&self) -> bool {
        self.incoming_problems.is_empty() && self.other_problems.is_empty()
    }

    pub fn text(&self) -> String {
        let mut l = Vec::new();
        if !self.incoming_problems.is_empty() {
            l.push("WARNING! What arrived does not pass `kontext verify`:".to_string());
            l.extend(self.incoming_problems.iter().map(|p| format!("  - {p}")));
            l.push(
                "  Check with `kontext verify` and compare with the remote's history (`git log -p`) \
                 before trusting those entries."
                    .into(),
            );
        }
        for w in &self.warnings {
            l.push(format!("Warning: {w}"));
        }
        match &self.commit {
            Some(c) => l.push(format!(
                "Commit {c}: {}.",
                plural(self.committed_entries as usize, "own entry", "own entries")
            )),
            None => l.push("Nothing local to save.".into()),
        }
        match &self.remote {
            None => l.push(
                "This kontext-home has no remote: only the commit was made. Add one with \
                 `git -C <kontext-home> remote add origin <url>`."
                    .into(),
            ),
            Some(r) => {
                let b = self.branch.as_deref().unwrap_or("?");
                if self.pushed {
                    l.push(format!("Pushed to {r}/{b}."));
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
                        "Received: {} ({}).",
                        plural(self.received_total() as usize, "entry", "entries"),
                        parts.join(", ")
                    ));
                }
            }
        }
        if !self.new_alerts.is_empty() {
            l.push(format!("New alerts ({}):", self.new_alerts.len()));
            l.extend(self.new_alerts.iter().map(|a| format!("  - {a}")));
        }
        if !self.other_problems.is_empty() {
            l.push("The local chain has problems (`kontext verify`):".into());
            l.extend(self.other_problems.iter().map(|p| format!("  - {p}")));
        }
        l.join("\n") + "\n"
    }
}

/// Takes the locks of all the writers on this machine (those with a file, and
/// its own), in order, so there are no cycles.
fn lock_all(home: &Path, own: &str) -> Result<Vec<File>> {
    let mut writers: Vec<String> = ledger_files(&home.join("ledger"))?
        .into_iter()
        .map(|(w, _)| w)
        .collect();
    writers.push(own.to_string());
    writers.sort();
    writers.dedup();
    writers.iter().map(|w| lock_writer(home, w)).collect()
}

/// `git add` and commit of the writer's own files. Returns the short sha and the entries.
fn commit_local(
    home: &Path,
    writer: &str,
    include_config: bool,
    warnings: &mut Vec<String>,
) -> Result<Option<(String, u64)>> {
    if !has_rev(home, "HEAD") {
        // First commit: everything (project.toml, .gitignore, the folders).
        git_ok(home, &["add", "-A"])?;
    } else {
        let mut paths: Vec<&str> = ["ledger", "cursors", "media"]
            .into_iter()
            .filter(|p| home.join(p).exists())
            .collect();
        let config_changed: Vec<&str> = CONFIG_FILES
            .iter()
            .copied()
            .filter(|f| {
                git_try(home, &["status", "--porcelain", "--", f]).is_some_and(|s| !s.is_empty())
            })
            .collect();
        if include_config {
            paths.extend(config_changed.iter().copied());
        } else if !config_changed.is_empty() {
            warnings.push(format!(
                "{} has changes that are not pushed automatically: `kontext sync --include-config` pushes them",
                config_changed.join(" and ")
            ));
        }
        if !paths.is_empty() {
            let mut args = vec!["add", "-A", "--"];
            args.extend(paths);
            git_ok(home, &args)?;
        }
        let others: Vec<String> = lines(&git_ok(home, &["status", "--porcelain"])?)
            .into_iter()
            .filter_map(|l| l.get(3..).map(str::to_string))
            .filter(|p| {
                !p.starts_with("ledger/")
                    && !p.starts_with("cursors/")
                    && !p.starts_with("media/")
                    && !CONFIG_FILES.contains(&p.as_str())
            })
            .collect();
        if !others.is_empty() {
            warnings.push(format!(
                "there are other changes in the kontext-home that kontext does not push: {}",
                others.join(", ")
            ));
        }
    }
    if git_try(home, &["diff", "--cached", "--quiet"]).is_some() {
        return Ok(None);
    }
    let numstat = git_ok(home, &["diff", "--cached", "--numstat", "--", "ledger"])?;
    let added: u64 = numstat
        .lines()
        .filter(|l| l.ends_with(".jsonl"))
        .filter_map(|l| l.split_whitespace().next()?.parse::<u64>().ok())
        .sum();
    let msg = format!(
        "kontext: {writer} {}",
        plural(added as usize, "entry", "entries")
    );
    let mut args = identity_args(home, writer);
    args.extend(["commit", "-q", "--no-verify", "-m", &msg].map(String::from));
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    git_ok(home, &a)?;
    let sha = git_ok(home, &["rev-parse", "--short", "HEAD"])?
        .trim()
        .to_string();
    Ok(Some((sha, added)))
}

/// Rebases the local work onto `upstream`. If git collides, it is aborted and
/// explained; the repo is left as it was before the rebase. Returns whether there
/// was anything to bring in.
fn rebase_onto(home: &Path, upstream: &str, writer: &str) -> Result<bool> {
    if git_try(home, &["merge-base", "--is-ancestor", upstream, "HEAD"]).is_some() {
        return Ok(false);
    }
    // What is not pushed (config without --include-config) is set aside during
    // the rebase, only if what arrives does not touch it: if it does, git would
    // leave it with conflict markers when putting it back.
    let dirty = lines(&git_ok(home, &["diff", "--name-only", "HEAD"])?);
    if !dirty.is_empty() {
        let range = format!("HEAD...{upstream}");
        let mut args = vec!["diff", "--name-only", range.as_str(), "--"];
        args.extend(dirty.iter().map(String::as_str));
        let touched = lines(&git_ok(home, &args)?);
        if !touched.is_empty() {
            return Err(Refused(format!(
                "{} has changes here and changes also arrived from the remote: nothing was pulled or \
                 pushed (your local commit is still there). Push them with `kontext sync --include-config` \
                 or discard them with `git -C {} checkout -- {}` and run `kontext sync` again",
                touched.join(" and "),
                home.display(),
                touched.join(" ")
            ))
            .into());
        }
    }
    let mut args = identity_args(home, writer);
    args.extend(["rebase", "-q"].map(String::from));
    if !dirty.is_empty() {
        args.push("--autostash".into());
    }
    args.push(upstream.to_string());
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = git_cmd(home, &a)?;
    if out.status.success() {
        return Ok(true);
    }
    let conflicted = git_try(home, &["diff", "--name-only", "--diff-filter=U"])
        .map(|s| lines(&s))
        .unwrap_or_default();
    let git_dir = home.join(".git");
    if git_dir.join("rebase-merge").exists() || git_dir.join("rebase-apply").exists() {
        git_ok(home, &["rebase", "--abort"])?;
    }
    let what = if conflicted.is_empty() {
        String::from_utf8_lossy(&out.stderr).trim().to_string()
    } else {
        format!("conflicts in {}", conflicted.join(", "))
    };
    Err(Refused(format!(
        "git could not combine the local work with the remote's ({what}). The rebase was aborted and \
         the kontext-home was left as it was (with your local commit, not pushed). With the ledger this \
         should not happen (each writer has its own file); if project.toml or synonyms.toml collided, \
         resolve it by hand with `git -C {} pull --rebase` and run `kontext sync` again",
        home.display()
    ))
    .into())
}

/// `kontext sync`.
pub fn sync(store: &mut Store, include_config: bool) -> Result<SyncReport> {
    let home = store.home().to_path_buf();
    let writer = store.writer().to_string();
    if !is_repo_root(&home) {
        bail!(
            "{} is not a git repo: run `git -C {} init` and then `kontext sync` again",
            home.display(),
            home.display()
        );
    }
    let Some(branch) = branch(&home) else {
        bail!(
            "the kontext-home {} is not on a branch (detached HEAD); switch to one with git checkout",
            home.display()
        );
    };
    let remote = remote_name(&home);
    let mut report = SyncReport {
        writer: writer.clone(),
        remote: remote.clone(),
        branch: Some(branch.clone()),
        ..SyncReport::default()
    };
    let before = store.state()?;
    let before_frontier = store.frontier()?;

    let mut committed = false;
    for attempt in 0..3 {
        if let Some(r) = &remote {
            git_ok(&home, &["fetch", "-q", r]).with_context(|| {
                format!("could not fetch from {r} (local work stays in a commit, not pushed)")
            })?;
        }
        let upstream = remote
            .as_ref()
            .map(|r| format!("refs/remotes/{r}/{branch}"))
            .filter(|u| has_rev(&home, u));
        {
            let _locks = lock_all(&home, &writer)?;
            if !committed {
                if let Some((sha, n)) =
                    commit_local(&home, &writer, include_config, &mut report.warnings)?
                {
                    report.commit = Some(sha);
                    report.committed_entries = n;
                }
                committed = true;
            }
            if let Some(u) = &upstream {
                report.pulled |= rebase_onto(&home, u, &writer)?;
            }
        }
        let Some(r) = &remote else {
            break;
        };
        if !has_rev(&home, "HEAD") {
            break;
        }
        let out = git_cmd(&home, &["push", "-q", "-u", r, &branch])?;
        if out.status.success() {
            report.pushed = true;
            break;
        }
        let err = String::from_utf8_lossy(&out.stderr).to_string();
        let raced = err.contains("rejected") || err.contains("fetch first");
        if !raced || attempt == 2 {
            bail!(
                "could not push to {r}: {}. Your local commit is still there; run `kontext sync` again",
                err.trim()
            );
        }
    }
    if remote.is_some() {
        let now = jiff::Timestamp::now().to_string();
        fs::create_dir_all(config::internal_dir(&home))?;
        config::write_atomic(&last_sync_file(&home), now.as_bytes())?;
    }

    // What arrived.
    let after_frontier = store.frontier()?;
    for (w, n) in &after_frontier.0 {
        let old = before_frontier.get(w);
        if *n > old {
            report.received.insert(w.clone(), n - old);
        }
    }
    let after = store.state()?;
    report.new_alerts = after
        .alerts
        .iter()
        .filter(|a| !before.alerts.contains(a))
        .cloned()
        .collect();
    let v = verify::run(&home)?;
    for p in v.problems {
        let incoming = report
            .received
            .keys()
            .any(|w| p.file == format!("ledger/{w}.jsonl"));
        if incoming {
            report.incoming_problems.push(p);
        } else {
            report.other_problems.push(p);
        }
    }
    Ok(report)
}

/// `kontext status`.
#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub project: String,
    pub home: String,
    pub writer: String,
    pub remote: Option<String>,
    pub remote_url: Option<String>,
    pub branch: Option<String>,
    pub last_sync: Option<String>,
    /// Own entries not committed yet.
    pub uncommitted: u64,
    /// Own entries the remote does not have (according to the last fetch).
    pub unpushed: Option<u64>,
    /// Entries in the index, how many were ingested now, and whether it was rebuilt.
    pub index_entries: u64,
    pub index_caught_up: u64,
    pub index_rebuilt: bool,
    pub chains_ok: bool,
    pub chain_problems: Vec<String>,
    pub alerts: usize,
    pub require_signatures: bool,
    pub signing_key: Option<String>,
    /// The kontext server, if the project has one (without the network).
    pub server: Option<crate::remote::Status>,
}

/// The writer's own entries not committed yet and, with a remote, not pushed
/// (according to the last fetch). Read-only: `git rev-parse`, `remote`,
/// `symbolic-ref` and `show`, and the tail of the writer's file.
#[derive(Debug, Clone, Default)]
pub struct Pending {
    pub repo: bool,
    pub remote: Option<String>,
    pub branch: Option<String>,
    pub uncommitted: u64,
    pub unpushed: Option<u64>,
}

pub fn pending(home: &Path, writer: &str) -> Pending {
    let file = home.join("ledger").join(format!("{writer}.jsonl"));
    let local = read_tail(&file).map(|t| t.0).unwrap_or(0);
    let repo = is_repo_root(home);
    let remote = if repo { remote_name(home) } else { None };
    let branch = if repo { branch(home) } else { None };
    let committed = if repo && has_rev(home, "HEAD") {
        lines_at(home, "HEAD", writer)
    } else {
        0
    };
    let unpushed = match (&remote, &branch) {
        (Some(r), Some(b)) => {
            let rref = format!("refs/remotes/{r}/{b}");
            let there = if has_rev(home, &rref) {
                lines_at(home, &rref, writer)
            } else {
                0
            };
            Some(local.saturating_sub(there))
        }
        _ => None,
    };
    Pending {
        repo,
        remote,
        branch,
        uncommitted: local.saturating_sub(committed),
        unpushed,
    }
}

pub fn status(store: &mut Store, project: &Project) -> Result<Status> {
    let home = store.home().to_path_buf();
    let writer = store.writer().to_string();
    let cu = store.index().catch_up()?;
    let Pending {
        remote,
        branch,
        uncommitted,
        unpushed,
        ..
    } = pending(&home, &writer);
    let report = verify::run(&home)?;
    let st = store.state()?;
    let machine = Machine::load()?;
    let resolved_name = Machine::load()?
        .projects
        .iter()
        .find(|(_, r)| {
            config::expand_tilde(&r.home)
                .ok()
                .and_then(|h| fs::canonicalize(h).ok())
                == fs::canonicalize(&home).ok()
        })
        .map(|(n, _)| n.clone())
        .unwrap_or_else(|| project.name.clone());
    let server = crate::remote::Link::of(&resolved_name)
        .ok()
        .and_then(|l| crate::remote::status(&home, &l));
    Ok(Status {
        server,
        project: project.name.clone(),
        home: home.display().to_string(),
        writer,
        remote_url: remote
            .as_ref()
            .and_then(|r| git_try(&home, &["remote", "get-url", r])),
        remote,
        branch,
        last_sync: last_sync(&home),
        uncommitted,
        unpushed,
        index_entries: store.index().count()?,
        index_caught_up: cu.entries,
        index_rebuilt: cu.rebuilt,
        chains_ok: report.ok(),
        chain_problems: report.problems.iter().map(ToString::to_string).collect(),
        alerts: st.alerts.len(),
        require_signatures: project.require_signatures,
        signing_key: machine.signing_key()?.map(|p| p.display().to_string()),
    })
}

impl Status {
    pub fn text(&self) -> String {
        let mut l = vec![format!("Project {} in {}", self.project, self.home)];
        l.push(format!("Writer: {}", self.writer));
        match (&self.remote, &self.remote_url) {
            (Some(r), Some(u)) => l.push(format!(
                "Remote: {r} ({u}), branch {}",
                self.branch.as_deref().unwrap_or("?")
            )),
            (Some(r), None) => l.push(format!("Remote: {r}")),
            _ if self.server.is_some() => {
                l.push("Git remote: none (this project syncs through its server)".into())
            }
            _ => l.push("Remote: none (`kontext sync` only commits)".into()),
        }
        // Git's last sync and commits mean nothing for a project that syncs
        // only through its server: its own lines below say what is pending.
        if self.server.is_none() || self.remote.is_some() {
            l.push(format!(
                "Last sync: {}",
                self.last_sync
                    .as_deref()
                    .unwrap_or("never from this machine")
            ));
            let mut pending = format!("Own entries not committed: {}", self.uncommitted);
            if let Some(u) = self.unpushed {
                pending.push_str(&format!("; not pushed: {u} (according to the last fetch)"));
            }
            l.push(pending);
        }
        l.push(format!(
            "Index: {} entries{}",
            self.index_entries,
            if self.index_rebuilt {
                " (rebuilt now)".to_string()
            } else if self.index_caught_up > 0 {
                format!(" ({} new ingested now)", self.index_caught_up)
            } else {
                " (up to date)".to_string()
            }
        ));
        if self.chains_ok {
            l.push("Chains: OK".into());
        } else {
            l.push(format!(
                "Chains: FAILED ({}); run `kontext verify`",
                self.chain_problems.len()
            ));
            l.extend(self.chain_problems.iter().map(|p| format!("  - {p}")));
        }
        if let Some(s) = &self.server {
            l.extend(s.lines());
        }
        l.push(format!("Alerts: {}", self.alerts));
        l.push(format!(
            "Signatures: {}{}",
            if self.require_signatures {
                "required"
            } else {
                "optional"
            },
            match &self.signing_key {
                Some(k) => format!("; this machine signs with {k}"),
                None => "; this machine has no [signing] key".into(),
            }
        ));
        l.join("\n") + "\n"
    }
}

/// What `kontext clone` brought in.
#[derive(Debug, Clone, Serialize)]
pub struct Cloned {
    pub name: String,
    pub home: String,
    pub writer: String,
    pub charter: Option<String>,
    pub charter_signed: bool,
    pub owners: Vec<String>,
    pub entries: u64,
    pub warnings: Vec<String>,
}

fn url_basename(url: &str) -> String {
    let t = url.trim().trim_end_matches('/');
    let last = t.rsplit(['/', ':']).next().unwrap_or(t);
    last.strip_suffix(".git").unwrap_or(last).to_string()
}

/// `kontext clone <url>`: fetches a kontext-home, registers it, sets this
/// machine's writer if it had none, and builds the index.
pub fn clone(url: &str, home: Option<PathBuf>, name: Option<&str>) -> Result<Cloned> {
    let machine = Machine::load()?;
    if let Some(n) = name
        && machine.projects.contains_key(n)
    {
        bail!(
            "project {n:?} is already registered in {}",
            Machine::path()?.display()
        );
    }
    let home = match home {
        Some(h) => h,
        None => {
            let base = url_basename(url);
            if base.is_empty() {
                bail!("could not get a name from {url:?}: use --home <path>");
            }
            config::home_dir()?.join("kontext-home").join(&base)
        }
    };
    let home = if home.is_absolute() {
        home
    } else {
        std::env::current_dir()?.join(home)
    };
    if home.exists() && fs::read_dir(&home)?.next().is_some() {
        bail!("{} already exists and is not empty", home.display());
    }
    fs::create_dir_all(home.parent().context("path without a folder")?)?;
    let out = Command::new("git")
        .arg("clone")
        .arg("-q")
        .arg(url)
        .arg(&home)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .context("could not run git")?;
    if !out.status.success() {
        bail!(
            "could not clone {url}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let home = fs::canonicalize(&home)?;
    // If something fails, only what this command just created is deleted.
    let undo = |e: anyhow::Error| -> anyhow::Error {
        let _ = fs::remove_dir_all(&home);
        e
    };
    let project = Project::load(&home).map_err(|e| {
        undo(e.context(format!(
            "{url} is not a kontext-home (it has no project.toml); the copy was deleted"
        )))
    })?;
    let name = name.map(str::to_string).unwrap_or(project.name.clone());
    if !crate::cli::valid_name(&name) {
        return Err(undo(anyhow::anyhow!(
            "invalid name {name:?}: use --name with letters, digits, - _ and ."
        )));
    }
    if machine.projects.contains_key(&name) {
        return Err(undo(anyhow::anyhow!(
            "project {name:?} is already registered in {}; use --name <other>",
            Machine::path()?.display()
        )));
    }
    Machine::register(&name, &home).map_err(undo)?;
    let writer = Machine::load()?.writer()?;
    let mut warnings = Vec::new();
    if home.join("ledger").join(format!("{writer}.jsonl")).exists() {
        warnings.push(format!(
            "the ledger already has a file for {writer}. If it is not from this machine, change `writer` \
             in {} before writing: two machines with the same writer collide when syncing",
            Machine::path()?.display()
        ));
    }
    let mut store = Store::open(&home, &writer)?;
    let st = store.state()?;
    let charter: Option<&Entry> = st.charter().or_else(|| st.latest_charter());
    Ok(Cloned {
        name,
        home: home.display().to_string(),
        writer,
        charter: charter.map(|c| c.title.trim().to_string()),
        charter_signed: st.charter().is_some(),
        owners: project.owners.clone(),
        entries: store.index().count()?,
        warnings,
    })
}

impl Cloned {
    pub fn text(&self) -> String {
        let mut l = vec![format!("Project {} cloned into {}", self.name, self.home)];
        l.push(format!("Writer of this machine: {}", self.writer));
        l.push(match &self.charter {
            Some(c) if self.charter_signed => format!("Charter: {c}"),
            Some(c) => format!("Charter (unsigned): {c}"),
            None => "Charter: none yet".into(),
        });
        l.push(format!(
            "Owners: {}",
            if self.owners.is_empty() {
                "(none)".to_string()
            } else {
                self.owners.join(", ")
            }
        ));
        l.push(format!("Entries: {}", self.entries));
        for w in &self.warnings {
            l.push(format!("Warning: {w}"));
        }
        l.join("\n") + "\n"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basenames() {
        assert_eq!(url_basename("git@github.com:omar/billing.git"), "billing");
        assert_eq!(url_basename("/tmp/x/remote.git/"), "remote");
        assert_eq!(url_basename("https://h/o/kontext-home"), "kontext-home");
    }
}
