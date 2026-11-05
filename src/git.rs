//! The `git` binary as a subprocess (DESIGN §16: no libgit2).

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

/// Runs `git <args>` in `dir`. `Ok(None)` if git exits with an error; `Err` if
/// it could not even be run.
pub fn run(dir: &Path, args: &[&str]) -> Result<Option<String>> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .context("could not run git")?;
    if !out.status.success() {
        return Ok(None);
    }
    Ok(Some(String::from_utf8_lossy(&out.stdout).into_owned()))
}

/// Like [`run`], without the trailing newline, and `None` if the output is
/// empty.
pub fn line(dir: &Path, args: &[&str]) -> Option<String> {
    run(dir, args)
        .ok()
        .flatten()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Like [`run`], but requires it to succeed.
pub fn must(dir: &Path, args: &[&str]) -> Result<String> {
    match run(dir, args)? {
        Some(s) => Ok(s),
        None => bail!("`git {}` failed in {}", args.join(" "), dir.display()),
    }
}

/// The root of the git repo that contains `dir`.
pub fn root(dir: &Path) -> Option<PathBuf> {
    line(dir, &["rev-parse", "--show-toplevel"]).map(PathBuf::from)
}

/// The sha of `HEAD`.
pub fn head(root: &Path) -> Option<String> {
    line(root, &["rev-parse", "--verify", "-q", "HEAD"])
}

/// The blob (or tree) of `path` at `rev`, or `None` if it is not there.
pub fn object_at(root: &Path, rev: &str, path: &str) -> Option<String> {
    line(
        root,
        &["rev-parse", "--verify", "-q", &format!("{rev}:{path}")],
    )
}

/// The `origin` remote, or the first remote there is.
pub fn remote_url(root: &Path) -> Option<String> {
    line(root, &["remote", "get-url", "origin"]).or_else(|| {
        let first = line(root, &["remote"])?;
        let name = first.lines().next()?.trim().to_string();
        line(root, &["remote", "get-url", &name])
    })
}

/// The files git tracks (`git ls-files`), relative to the root.
pub fn tracked_files(root: &Path) -> Option<Vec<String>> {
    let out = run(root, &["ls-files", "-z"]).ok().flatten()?;
    Some(
        out.split('\0')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
    )
}

/// `true` if `path` has uncommitted changes (or git does not track it).
pub fn dirty(root: &Path, path: &str) -> bool {
    line(root, &["status", "--porcelain", "--", path]).is_some()
}

/// The first 7 characters of a sha.
pub fn short(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}
