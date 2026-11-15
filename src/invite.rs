//! `kontext invite` and `kontext join` (DESIGN §14.3): another machine joins a
//! project with one command and a one-time code, instead of a long-lived token
//! carried by hand.
//!
//! - `invite` (an owner) asks the server for a code bound to the token the new
//!   machine will get (role, person, epic, writer and lifetime, as `token
//!   create`). The server keeps only the code's sha256; the code is good for
//!   one use, ten minutes by default. It prints a join string,
//!   `kxj1.<base64url of {"s": server, "p": project, "c": code}>`, and the line
//!   to run on the other machine.
//! - `join <string>` trades the code for that machine's own token (it never
//!   leaves that machine and is never printed), brings the project as `clone
//!   --server` does and, inside a code repo, installs the hooks as `install`.
//!   Run again it resumes: a token it already got is used instead of the code.
//!
//! A server deployed before invitations (no `invites` in `GET /v1/info`) is
//! not sent the code: both commands say it needs `kontext server deploy`.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::{Value, json};

use crate::config::{self, Machine, Token};
use crate::http::Http;
use crate::remote::{self, CloneArgs, Conn, TokenArgs};
use crate::setup::Outcome;
use crate::store::Refused;

/// The prefix of a join string.
pub const JOIN_PREFIX: &str = "kxj1.";
/// How long an invitation is good for, unless `--valid-for` says otherwise.
pub const DEFAULT_VALID: &str = "10m";
/// The installer, which takes a join string as its argument.
pub const INSTALL_URL: &str =
    "https://raw.githubusercontent.com/OmarAlex24/kontext/main/install.sh";

/// What a join string carries.
#[derive(Debug, Clone, PartialEq)]
pub struct JoinString {
    pub server: String,
    pub project: String,
    pub code: String,
}

impl JoinString {
    pub fn encode(&self) -> String {
        let claims = json!({ "s": self.server, "p": self.project, "c": self.code });
        let b = crate::media::base64(claims.to_string().as_bytes())
            .replace('+', "-")
            .replace('/', "_")
            .replace('=', "");
        format!("{JOIN_PREFIX}{b}")
    }

    pub fn parse(s: &str) -> Result<JoinString> {
        let bad = || {
            anyhow::Error::from(Refused(
                "that is not a join string (they start with kxj1.): on a machine that has the project, \
                 `kontext invite` prints one"
                    .into(),
            ))
        };
        let rest = s.trim().strip_prefix(JOIN_PREFIX).ok_or_else(bad)?;
        let bytes = config::base64url_decode(rest).map_err(|_| bad())?;
        let v: Value = serde_json::from_slice(&bytes).map_err(|_| bad())?;
        let text = |k: &str| {
            v.get(k)
                .and_then(Value::as_str)
                .map(str::to_string)
                .filter(|s| !s.is_empty())
        };
        let (Some(server), Some(project), Some(code)) = (text("s"), text("p"), text("c")) else {
            return Err(bad());
        };
        // The claims become a URL and paths: checked before anything is sent.
        if !(server.starts_with("https://") || server.starts_with("http://"))
            || server.contains(char::is_whitespace)
        {
            return Err(bad());
        }
        if !crate::cli::valid_name(&project) {
            return Err(bad());
        }
        Ok(JoinString {
            server: crate::server::normalize_url(&server),
            project,
            code,
        })
    }
}

/// Does the server at `url` take invitations? A server deployed before them
/// answers `/v1/info` without `features`.
pub fn require_invites(url: &str) -> Result<()> {
    let http = Http::new(Duration::from_secs(10));
    let r = http.request("GET", &format!("{url}/v1/info"), &[], None)?;
    let v = r.json().ok().filter(|_| r.ok());
    let Some(v) = v.filter(|v| v.get("kontext_server").is_some()) else {
        bail!(
            "{url} does not answer as a kontext server (GET /v1/info: {})",
            r.status
        );
    };
    let has = v
        .get("features")
        .and_then(Value::as_array)
        .is_some_and(|f| f.iter().any(|x| x.as_str() == Some("invites")));
    if !has {
        return Err(Refused(format!(
            "the kontext server at {url} was deployed before invitations and needs an update: on the \
             machine that deployed it, run `kontext server deploy` (it keeps the projects and the admin \
             secret), then try again"
        ))
        .into());
    }
    Ok(())
}

/// What `kontext invite` asks for.
#[derive(Debug, Clone)]
pub struct InviteArgs {
    pub token: TokenArgs,
    /// How long the code is good for (`10m`, `2h`; a day at most).
    pub valid_for: Option<String>,
}

/// An invitation just made.
#[derive(Debug, Clone, Serialize)]
pub struct Invited {
    /// The join string: it carries the one-time code.
    pub join: String,
    /// The line to run on the other machine, with kontext.
    pub command: String,
    /// The line to run there without kontext: install and join.
    pub install_command: String,
    pub project: String,
    pub server: String,
    pub person: String,
    pub role: String,
    pub epic: Option<String>,
    pub writer: String,
    /// When the code stops working (ms since the epoch).
    pub expires: i64,
    /// The token's own lifetime, if it has one.
    pub token_ttl: Option<String>,
    pub note: String,
}

impl Invited {
    pub fn text(&self) -> String {
        let when = jiff::Timestamp::from_millisecond(self.expires)
            .map(|t| t.to_string())
            .unwrap_or_default();
        let mut what = format!("{} as {}", self.person, self.role);
        if let Some(e) = &self.epic {
            what.push_str(&format!(" of epic {e}"));
        }
        let mut l = vec![
            format!(
                "Invitation to project {} for {what} (writer {}){}.",
                self.project,
                self.writer,
                match &self.token_ttl {
                    Some(t) => format!(", a token that lasts {t}"),
                    None => String::new(),
                }
            ),
            "On the other machine, in its code repo (so the hooks are installed too), run:".into(),
            format!("  {}", self.command),
            "If kontext is not installed there, this installs it and joins in one line:".into(),
            format!("  {}", self.install_command),
            self.note.replace("{when}", &when),
        ];
        l.retain(|x| !x.is_empty());
        l.join("\n") + "\n"
    }
}

/// `kontext invite`.
pub fn invite(project: &str, home: &std::path::Path, a: &InviteArgs) -> Result<Invited> {
    let (remote, conn, mut body) = remote::token_body(project, home, &a.token)?;
    let valid = a.valid_for.as_deref().unwrap_or(DEFAULT_VALID);
    let valid_ms = crate::tasks::parse_duration_ms(valid).context("--valid-for")?;
    if valid_ms > 24 * 3600 * 1000 {
        bail!("--valid-for is a day at most (an invitation is for now, not for later)");
    }
    body["valid_ms"] = json!(valid_ms);
    let v = require_invites(&remote.url)
        .and_then(|()| conn.json("POST", "invites", &body))
        .map_err(|e| {
            offline(
                e,
                &remote.url,
                "no invitation was made; run `kontext invite` again when it answers",
            )
        })?;
    let v = Conn::ok(v, "invite")?;
    let code = v
        .get("code")
        .and_then(Value::as_str)
        .context("the server sent no code")?
        .to_string();
    let join = JoinString {
        server: remote.url.clone(),
        project: remote.token.project.clone(),
        code,
    }
    .encode();
    let s = |k: &str| body.get(k).and_then(Value::as_str).map(str::to_string);
    Ok(Invited {
        command: format!("kontext join {join}"),
        install_command: format!("curl -fsSL {INSTALL_URL} | sh -s -- {join}"),
        join,
        project: remote.token.project.clone(),
        server: remote.url.clone(),
        person: s("person").unwrap_or_default(),
        role: s("role").unwrap_or_default(),
        epic: s("epic"),
        writer: s("writer").unwrap_or_default(),
        expires: v.get("expires").and_then(Value::as_i64).unwrap_or(0),
        token_ttl: a.token.ttl.clone(),
        note: format!(
            "The line works once and until {{when}} ({valid}); it carries a one-time code, not a token: \
             the token is made for that machine when it joins and never leaves it. Use it right away; \
             a new `kontext invite` makes another."
        ),
    })
}

/// What `kontext join` asks for.
#[derive(Debug, Clone, Default)]
pub struct JoinArgs {
    pub join: String,
    pub home: Option<PathBuf>,
    pub name: Option<String>,
    /// Do not install in the current repo.
    pub no_install: bool,
    /// `install`'s flags.
    pub role: String,
    pub force: bool,
    pub local: bool,
    pub worktree_role: Option<String>,
}

/// `kontext join <string>`.
pub fn join(a: &JoinArgs) -> Result<Outcome> {
    let j = JoinString::parse(&a.join)?;
    let name = a.name.clone().unwrap_or_else(|| j.project.clone());
    if !crate::cli::valid_name(&name) {
        bail!("invalid name {name:?}: use --name with letters, digits, - _ and .");
    }
    let cwd = std::env::current_dir()?;
    let mut out = Outcome {
        command: "join",
        repo: cwd.display().to_string(),
        project: name.clone(),
        home: None,
        writer: None,
        server: Some(j.server.clone()),
        steps: Vec::new(),
        ok: false,
        offline: false,
        needs_human: None,
        notes: Vec::new(),
        next: String::new(),
    };
    let machine = Machine::load()?;
    if machine.projects.contains_key(&name) {
        // Already joined: the code is not spent again.
        let own = machine.server_for(&name);
        let token = config::token_for(&name)?;
        if own.as_deref() != Some(j.server.as_str()) || token.is_none() {
            return Err(Refused(format!(
                "project {name} is already registered on this machine{}: join under another name with \
                 `kontext join <string> --name <other>`",
                match own {
                    Some(s) => format!(" and syncs with {s}"),
                    None => " without a server".into(),
                }
            ))
            .into());
        }
        let home = machine.project_home(&name)?;
        out.step(
            "project",
            "already",
            format!("{name} in {} (syncs with {})", home.display(), j.server),
        );
    } else {
        // A token got by an earlier run that stopped before the project was
        // set up; otherwise the code is traded for one now.
        let saved = config::token_for(&name)
            .ok()
            .flatten()
            .filter(|t| t.project == j.project);
        let (token, how) = match saved {
            Some(t) => (t, "with the token an earlier `kontext join` got"),
            None => {
                let t = require_invites(&j.server)
                    .and_then(|()| trade(&j))
                    .map_err(|e| {
                        offline(
                            e,
                            &j.server,
                            "the line was not used; run the same `kontext join …` again when it \
                             answers, while the line is still valid",
                        )
                    })?;
                // Saved before anything else can fail: the code is spent.
                t.save(&name)?;
                (t, "with a token made for this machine")
            }
        };
        let (c, r) = remote::clone(CloneArgs {
            url: j.server.clone(),
            project: j.project.clone(),
            home: a.home.clone(),
            name: Some(name.clone()),
            quick: false,
            token: Some(token),
        })?;
        out.step(
            "project",
            "done",
            format!(
                "brought {} from {} into {} ({}, writer {}), {how}",
                j.project,
                j.server,
                c.home,
                crate::views::plural(c.entries as usize, "entry", "entries"),
                c.writer
            ),
        );
        out.notes.extend(c.warnings.iter().cloned());
        out.notes.extend(r.warnings.iter().cloned());
        if !r.ok() || r.partial {
            out.notes.push(
                "not everything came down in this run: `kontext sync` brings the rest".into(),
            );
        }
    }
    let resolved = config::resolve_project(Some(&name))?;
    out.home = Some(resolved.home.display().to_string());
    out.writer = Some(resolved.writer.clone());

    // The hooks, in the code repo this runs in.
    let repo = crate::git::checkout(&cwd);
    match repo {
        _ if a.no_install => out.step(
            "install",
            "skipped",
            "--no-install: run `kontext setup` in the code repo to install the hooks",
        ),
        None => out.step(
            "install",
            "skipped",
            format!(
                "{} is not in a code repo: run `kontext setup --project {name}` in the code repo to install \
                 the hooks",
                cwd.display()
            ),
        ),
        Some((_, true)) => out.step(
            "install",
            "skipped",
            "this is a linked git worktree: run `kontext setup` in the main checkout to install the hooks",
        ),
        Some((root, false)) => {
            let root = root.canonicalize().unwrap_or(root);
            if root == resolved.home {
                out.step("install", "skipped", "this is the kontext-home, not a code repo");
            } else {
                out.repo = root.display().to_string();
                crate::setup::install_step(
                    &mut out,
                    &name,
                    &crate::setup::Args {
                        project: Some(name.clone()),
                        role: a.role.clone(),
                        force: a.force,
                        local: a.local,
                        worktree_role: a.worktree_role.clone(),
                        ..crate::setup::Args::default()
                    },
                )?;
            }
        }
    }
    out.ok = true;
    out.next = format!(
        "Done: project {name} is on this machine and syncs with {}. Agents read and write it with `kontext …`.",
        j.server
    );
    Ok(out)
}

/// `POST join` with the code: this machine's token. A used, expired or
/// unknown code is refused, and says what to do.
/// A server that did not answer: say so and what to do, keep anything else.
fn offline(e: anyhow::Error, url: &str, next: &str) -> anyhow::Error {
    if remote::unreachable(&e) {
        anyhow::anyhow!("{url} did not answer ({e:#}): {next}")
    } else {
        e
    }
}

fn trade(j: &JoinString) -> Result<Token> {
    let conn = Conn::new(&j.server, &j.project, &j.code, Duration::from_secs(30));
    let r = conn.json("POST", "join", &json!({}))?;
    let v = r.json().unwrap_or(Value::Null);
    if r.status == 201 {
        let raw = v
            .get("token")
            .and_then(Value::as_str)
            .context("the server sent no token")?;
        let t = Token::parse(raw)?;
        if t.project != j.project {
            bail!(
                "the server sent a token for project {}, not {}",
                t.project,
                j.project
            );
        }
        return Ok(t);
    }
    let msg = v.get("message").and_then(Value::as_str).unwrap_or("");
    match v.get("error").and_then(Value::as_str) {
        Some("invite_used" | "invite_expired" | "invite_unknown") => Err(Refused(format!(
            "{msg}. On a machine that has project {}, `kontext invite` makes a new one",
            j.project
        ))
        .into()),
        Some("no_project") => Err(Refused(format!(
            "project {} is not on {} (was it removed?)",
            j.project, j.server
        ))
        .into()),
        Some("writer_taken") => Err(Refused(format!(
            "{msg}. Ask for a new invitation (`kontext invite --writer <another>`)"
        ))
        .into()),
        _ => bail!("{}: the server answered {}: {msg}", j.server, r.status),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_join_string_round_trips_and_bad_ones_are_refused() {
        let j = JoinString {
            server: "https://kontext.omar.workers.dev".into(),
            project: "billing".into(),
            code: "kxi1.abcDEF_-123".into(),
        };
        let s = j.encode();
        assert!(s.starts_with("kxj1."));
        assert!(!s.contains(['+', '/', '=', ' ']));
        assert_eq!(JoinString::parse(&format!("  {s}\n")).unwrap(), j);
        for bad in [
            "kxt1.abc.def",
            "kxj1.!!!",
            &JoinString {
                project: "..".into(),
                ..j.clone()
            }
            .encode(),
            &JoinString {
                server: "file:///etc".into(),
                ..j.clone()
            }
            .encode(),
        ] {
            let e = JoinString::parse(bad).unwrap_err();
            assert!(e.downcast_ref::<Refused>().is_some(), "{bad}: {e}");
        }
    }
}
