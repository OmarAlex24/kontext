//! The coordinator agent's credential, on the owner's machine: one file in
//! the kontext config folder (`<config>/dispatch/.agent-credential`, mode
//! 0600; a project's name never starts with a dot, so no project's folder
//! can be called that). Never in a kontext-home, a repo or the dispatch
//! state, never synced.
//!
//! `kontext dispatch credential set` reads it from stdin (or a hidden prompt
//! on a terminal), never from an argument. A Claude Code OAuth token (from
//! `claude setup-token`, `sk-ant-oat…`) goes to the agent as
//! `CLAUDE_CODE_OAUTH_TOKEN`; an API key (`sk-ant-api…`) as
//! `ANTHROPIC_API_KEY`. Anything else is refused.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::config;
use crate::store::Refused;

/// What kind of credential it is, which says the variable it goes in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    OauthToken,
    ApiKey,
}

impl Kind {
    /// The variable Claude Code reads it from.
    pub fn var(self) -> &'static str {
        match self {
            Kind::OauthToken => "CLAUDE_CODE_OAUTH_TOKEN",
            Kind::ApiKey => "ANTHROPIC_API_KEY",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Kind::OauthToken => "a Claude Code OAuth token",
            Kind::ApiKey => "an Anthropic API key",
        }
    }

    pub fn of(value: &str) -> Option<Kind> {
        if value.starts_with("sk-ant-oat") {
            Some(Kind::OauthToken)
        } else if value.starts_with("sk-ant-api") {
            Some(Kind::ApiKey)
        } else {
            None
        }
    }
}

/// The credential, read from its file. Its `Debug` never shows the value.
pub struct Credential {
    pub kind: Kind,
    value: String,
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Credential({:?})", self.kind)
    }
}

impl Credential {
    pub fn value(&self) -> &str {
        &self.value
    }
}

/// `<config>/dispatch/.agent-credential`.
pub fn path() -> Result<PathBuf> {
    Ok(config::config_dir()?
        .join("dispatch")
        .join(".agent-credential"))
}

/// What a refusal says when there is none.
pub const MISSING: &str = "no agent credential on this machine: make one with `claude setup-token` \
     and save it with `kontext dispatch credential set` (it reads it from stdin)";

/// Checks a value: one line, no spaces, a known prefix.
pub fn check(value: &str) -> Result<Kind> {
    let v = value.trim();
    if v.is_empty() {
        return Err(Refused("no credential given on stdin".into()).into());
    }
    if v.chars().any(char::is_whitespace) {
        return Err(
            Refused("the credential is one word with no spaces or line breaks".into()).into(),
        );
    }
    Kind::of(v).ok_or_else(|| {
        Refused(
            "that is not a Claude Code OAuth token (sk-ant-oat…, from `claude setup-token`) or an \
             Anthropic API key (sk-ant-api…)"
                .into(),
        )
        .into()
    })
}

/// Saves it (mode 0600, replacing the one there was).
pub fn set(value: &str) -> Result<Kind> {
    use std::os::unix::fs::OpenOptionsExt;
    let kind = check(value)?;
    let p = path()?;
    let dir = p.parent().context("no dispatch folder")?;
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".agent-credential.{}", config::random_hex(4)?));
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .context("could not write the credential file")?;
    let written = f
        .write_all(value.trim().as_bytes())
        .and_then(|()| f.sync_all())
        .and_then(|()| fs::rename(&tmp, &p));
    if let Err(e) = written {
        let _ = fs::remove_file(&tmp);
        return Err(e).context("could not save the credential");
    }
    Ok(kind)
}

/// Removes it. `false` if there was none.
pub fn rm() -> Result<bool> {
    match fs::remove_file(path()?) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e).context("could not remove the credential"),
    }
}

/// The credential, if one is saved and valid.
pub fn load() -> Result<Option<Credential>> {
    let s = match fs::read_to_string(path()?) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).context("could not read the agent's credential"),
    };
    let value = s.trim().to_string();
    Ok(Kind::of(&value).map(|kind| Credential { kind, value }))
}

/// Which kind is saved, without keeping the value.
pub fn kind() -> Option<Kind> {
    load().ok().flatten().map(|c| c.kind)
}

/// Text with anything that looks like an Anthropic credential replaced:
/// failure reasons come from the agent's output and go into the state, the
/// watcher's log and the pm's question.
pub fn redact(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("sk-ant-") {
        out.push_str(&rest[..i]);
        out.push_str("[redacted]");
        let tail = &rest[i..];
        let end = tail
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
            .unwrap_or(tail.len());
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_and_refusals() {
        assert_eq!(check("sk-ant-oat01-abc").unwrap(), Kind::OauthToken);
        assert_eq!(check(" sk-ant-api03-x\n").unwrap(), Kind::ApiKey);
        assert!(check("ghp_abc").is_err());
        assert!(check("sk-ant-oat01 abc").is_err());
        assert!(check("").is_err());
        assert_eq!(Kind::OauthToken.var(), "CLAUDE_CODE_OAUTH_TOKEN");
    }

    #[test]
    fn redact_hides_credentials() {
        assert_eq!(
            redact("bad key sk-ant-oat01-AbC_9-z, try again"),
            "bad key [redacted], try again"
        );
        assert_eq!(redact("nothing here"), "nothing here");
    }
}
