//! When a step needs something only the human can give (DESIGN §14.3).
//!
//! An agent runs kontext with no terminal. When a step needs the human (a
//! Cloudflare API token, typed into a terminal and never into a chat, which is
//! captured and synced), the command does not wait on a prompt: it stops with
//! exit code [`EXIT_CODE`] and says why, the link to open, the one command the
//! human runs in a terminal, and what to run afterwards. With `--json` the same
//! goes out as `{"needs_human": {…}}`. The agent tells the human exactly that,
//! and runs the command it was told to afterwards.

use std::io::IsTerminal;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::Serialize;

/// The exit code of "needs the human": 0 ok, 1 error, 2 refused, 3 stale
/// context (`done`) are taken.
pub const EXIT_CODE: u8 = 4;

/// The permission `kontext server deploy` needs, as Cloudflare's token
/// template URLs name it (a user token: the short key of the permission
/// group, <https://developers.cloudflare.com/fundamentals/api/how-to/account-owned-token-template/>).
pub const CF_PERMISSIONS: &str = r#"[{"key":"workers_scripts","type":"edit"}]"#;

/// A step that only the human can do.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct NeedsHuman {
    /// What is needed, for a program: `cloudflare_token`.
    pub reason: String,
    /// Why, in a sentence.
    pub why: String,
    /// The link the human opens, if there is one.
    pub open: Option<String>,
    /// What the human does there.
    pub steps: Vec<String>,
    /// The one command the human runs in a terminal.
    pub run: String,
    /// What to run afterwards (the agent runs it).
    pub then: Option<String>,
    /// What the agent tells the human, word for word.
    pub say: String,
    pub exit_code: u8,
}

impl std::fmt::Display for NeedsHuman {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.text().trim_end())
    }
}

impl std::error::Error for NeedsHuman {}

impl NeedsHuman {
    /// The Cloudflare API token for `kontext server deploy`.
    pub fn cloudflare_token(then: Option<&str>) -> NeedsHuman {
        let run = format!("{} server deploy", self_command());
        let open = cloudflare_token_url();
        let then = then.map(str::to_string);
        let steps = vec![
            format!("Open {open} (signed in to Cloudflare; create a free account there if you have none)."),
            "The permission \"Workers Scripts: Edit\" is already filled in: click \"Continue to summary\", \
             then \"Create Token\", and copy the token."
                .to_string(),
            format!(
                "In a terminal, run `{run}` and paste the token when it asks (it is not shown, used once, \
                 and not stored). Never paste it into a chat: chats are captured and synced."
            ),
        ];
        let say = format!(
            "kontext needs a Cloudflare API token to put its sync server on your Cloudflare account (free \
             plan). 1) Open {open} , click \"Continue to summary\" and \"Create Token\". 2) In a terminal \
             (not here), run `{run}` and paste the token there. {}",
            match &then {
                Some(t) => format!("3) Tell me when it is done and I will run `{t}`."),
                None => "Tell me when it is done.".to_string(),
            }
        );
        NeedsHuman {
            reason: "cloudflare_token".into(),
            why: "the kontext server is deployed to the human's Cloudflare account, with an API token that \
                  only the human can create, and that must not go through a chat"
                .into(),
            open: Some(open),
            steps,
            run,
            then,
            say,
            exit_code: EXIT_CODE,
        }
    }

    /// A `workers.dev` subdomain for an account that has none: a public name
    /// that only the human picks. `command` is the command that takes it as
    /// `--subdomain <name>` (`kontext server deploy`, or `kontext setup`). The
    /// name is not a secret: the agent may run it once the human says it.
    pub fn workers_subdomain(
        account: &str,
        script: &str,
        command: &str,
        then: Option<&str>,
    ) -> NeedsHuman {
        let run = format!("{command} --subdomain <name>");
        let then = then.map(str::to_string);
        let steps = vec![
            format!(
                "Pick a name for the Cloudflare account's workers.dev subdomain. It is public, and the \
                 server's URL will be https://{script}.<name>.workers.dev."
            ),
            format!("Run `{run}` with that name (or tell the agent the name: it is not a secret)."),
        ];
        let say = format!(
            "Your Cloudflare account ({account}) has no workers.dev subdomain yet. It is part of the \
             server's public URL (https://{script}.<name>.workers.dev), so you pick it: tell me the name \
             and I will run `{run}` with it, or run that yourself. {}",
            match &then {
                Some(t) => format!("Then I will run `{t}`."),
                None => String::new(),
            }
        )
        .trim_end()
        .to_string();
        NeedsHuman {
            reason: "workers_subdomain".into(),
            why: format!(
                "Cloudflare account {account} has no workers.dev subdomain yet; it is a public name that \
                 only the human picks"
            ),
            open: None,
            steps,
            run,
            then,
            say,
            exit_code: EXIT_CODE,
        }
    }

    /// A project that was set up on another machine (the repo's committed
    /// `.kontext.toml` names it) and is not on this one: it comes here with a
    /// one-time invitation from a machine that has it, never by making it
    /// again here (that would be a second project with the same name, with
    /// none of its history). `new_project` is the command for the one other
    /// case: a project that exists nowhere yet.
    pub fn join_project(project: &str, file: &str, new_project: &str) -> NeedsHuman {
        let me = self_command();
        let run = "kontext invite".to_string();
        let then = format!("{me} join <the line `kontext invite` printed>");
        let steps = vec![
            format!(
                "On a machine that has project {project}, in its code repo, run `kontext invite` (an \
                 agent there can run it). It prints a line `kontext join kxj1.…` that works once, for 10 \
                 minutes, and carries a one-time code, not a token."
            ),
            format!(
                "Here, run that line (`{me} join kxj1.…`), or give it to the agent to run: it brings the \
                 project with its history and installs the hooks in this repo."
            ),
            format!(
                "Only if project {project} exists on no machine yet ({file} was committed before it was \
                 set up anywhere): `{new_project}` creates it here instead."
            ),
        ];
        let say = format!(
            "This repo belongs to kontext project {project}, which was set up on another machine, so it is \
             not made again here (that would be a second {project} with none of its history). 1) On a \
             machine that has it, run `kontext invite` in its code repo (or ask the agent there to). 2) It \
             prints a line that starts with `kontext join kxj1.`: run it here, or give it to me and I will \
             run it (it works once, for 10 minutes, and is not a token). If {project} was never set up \
             anywhere, tell me and I will run `{new_project}` instead."
        );
        NeedsHuman {
            reason: "join_project".into(),
            why: format!(
                "this repo's {file} names project {project}, which is not on this machine: it was set up on \
                 another machine, and it comes here with an invitation from there"
            ),
            open: None,
            steps,
            run,
            then: Some(then),
            say,
            exit_code: EXIT_CODE,
        }
    }

    pub fn text(&self) -> String {
        let mut l = vec![format!("Needs you: {}.", self.why)];
        for (i, s) in self.steps.iter().enumerate() {
            l.push(format!("  {}. {s}", i + 1));
        }
        if let Some(t) = &self.then {
            l.push(format!("Then run `{t}`."));
        }
        l.join("\n") + "\n"
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::json!({ "needs_human": self })
    }
}

/// The token template link for `kontext server deploy`: a user API token with
/// "Workers Scripts: Edit" on all the human's accounts, named kontext.
pub fn cloudflare_token_url() -> String {
    format!(
        "https://dash.cloudflare.com/profile/api-tokens?permissionGroupKeys={}&accountId=%2A&zoneId=all&name=kontext",
        url_encode(CF_PERMISSIONS)
    )
}

/// Percent-encodes everything but the unreserved characters (RFC 3986).
pub fn url_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// How the human runs this binary: `kontext` if the one on the `PATH` is this
/// one, otherwise its absolute path (quoted if needed), so the command works
/// as it is printed.
pub fn self_command() -> String {
    let Ok(exe) = std::env::current_exe() else {
        return "kontext".into();
    };
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    if let Some(on_path) = which("kontext")
        && std::fs::canonicalize(&on_path).unwrap_or(on_path) == exe
    {
        return "kontext".into();
    }
    crate::install::shell_quote(&exe.display().to_string())
}

fn which(name: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(name))
        .find(|p| {
            std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}

/// The environment says an agent or CI runs this command, so nobody is there
/// to answer a prompt even when stdin is a terminal (some agent harnesses give
/// commands a pseudo-terminal): `CI`, `CLAUDECODE`, or `KONTEXT_NO_PROMPT=1`.
/// A command that would prompt gives the needs-human result instead.
pub fn no_prompt() -> bool {
    let set = |k: &str| {
        std::env::var(k)
            .ok()
            .map(|v| v.trim().to_ascii_lowercase())
            .is_some_and(|v| !v.is_empty() && v != "0" && v != "false")
    };
    set("CI") || set("CLAUDECODE") || set("KONTEXT_NO_PROMPT")
}

/// How long a prompt on a terminal waits for the human before it gives up
/// with the needs-human result: five minutes (`KONTEXT_PROMPT_WAIT_MS` sets
/// it, for the tests).
pub fn prompt_wait() -> Duration {
    std::env::var("KONTEXT_PROMPT_WAIT_MS")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(Duration::from_secs(5 * 60))
}

/// Waits until stdin has something to read, at most `wait`: false if the
/// time ran out. On a terminal in its usual (canonical) mode that is a whole
/// typed line, or its end.
pub fn stdin_ready(wait: Duration) -> bool {
    let deadline = Instant::now() + wait;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return false;
        }
        let mut fds = libc::pollfd {
            fd: 0,
            events: libc::POLLIN,
            revents: 0,
        };
        let ms = left.as_millis().min(i32::MAX as u128) as i32;
        // SAFETY: one valid pollfd, for stdin.
        let r = unsafe { libc::poll(&mut fds, 1, ms) };
        if r < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return true;
        }
        return r > 0;
    }
}

/// How long a command waits for a secret piped on stdin when stdin is not a
/// terminal (`echo $TOKEN | kontext server deploy`). A pipe that stays open
/// with nothing in it (an agent's shell) never holds a command longer.
pub const STDIN_WAIT: Duration = Duration::from_secs(3);

/// One line from stdin when it is not a terminal, waiting at most `wait` for
/// it: `None` if nothing came (the end of stdin, or the time ran out).
pub fn piped_line(wait: Duration) -> Option<String> {
    if std::io::stdin().is_terminal() {
        return None;
    }
    let deadline = Instant::now() + wait;
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        let mut fds = libc::pollfd {
            fd: 0,
            events: libc::POLLIN,
            revents: 0,
        };
        let ms = left.as_millis().min(i32::MAX as u128) as i32;
        // SAFETY: one valid pollfd, for stdin.
        let r = unsafe { libc::poll(&mut fds, 1, ms) };
        if r < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            break;
        }
        if r == 0 {
            break;
        }
        let mut chunk = [0u8; 4096];
        // SAFETY: reads at most chunk.len() bytes into chunk.
        let n = unsafe { libc::read(0, chunk.as_mut_ptr().cast(), chunk.len()) };
        if n <= 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n as usize]);
        if buf.contains(&b'\n') || buf.len() > 64 * 1024 {
            break;
        }
    }
    let text = String::from_utf8_lossy(&buf);
    let line = text.lines().next().unwrap_or("").trim().to_string();
    (!line.is_empty()).then_some(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_token_link_is_the_documented_template() {
        assert_eq!(
            cloudflare_token_url(),
            "https://dash.cloudflare.com/profile/api-tokens?permissionGroupKeys=\
             %5B%7B%22key%22%3A%22workers_scripts%22%2C%22type%22%3A%22edit%22%7D%5D\
             &accountId=%2A&zoneId=all&name=kontext"
        );
    }

    #[test]
    fn it_says_the_command_and_what_comes_after() {
        let n = NeedsHuman::cloudflare_token(Some("kontext setup"));
        assert_eq!(n.exit_code, 4);
        assert!(n.run.ends_with("server deploy"), "{}", n.run);
        assert_eq!(n.then.as_deref(), Some("kontext setup"));
        let j = n.json();
        assert_eq!(j["needs_human"]["reason"], "cloudflare_token");
        assert!(
            j["needs_human"]["open"]
                .as_str()
                .unwrap()
                .contains("workers_scripts")
        );
        assert!(n.say.contains("not here"), "{}", n.say);
        assert!(n.text().contains("Then run `kontext setup`."));
    }

    #[test]
    fn a_project_from_another_machine_comes_with_an_invitation() {
        let n = NeedsHuman::join_project("billing", ".kontext.toml", "kontext setup --new-project");
        assert_eq!(n.reason, "join_project");
        assert_eq!(n.exit_code, 4);
        assert_eq!(n.run, "kontext invite");
        assert!(
            n.then.as_deref().unwrap().contains(" join "),
            "{:?}",
            n.then
        );
        assert!(n.say.contains("kontext setup --new-project"), "{}", n.say);
        assert!(n.text().contains("kontext join kxj1."), "{}", n.text());
    }

    #[test]
    fn a_subdomain_is_the_humans_to_pick() {
        let n = NeedsHuman::workers_subdomain("acc1", "kontext", "kontext server deploy", None);
        assert_eq!(n.reason, "workers_subdomain");
        assert_eq!(n.exit_code, 4);
        assert!(
            n.run.ends_with("server deploy --subdomain <name>"),
            "{}",
            n.run
        );
        assert!(n.why.contains("acc1"), "{}", n.why);
        assert!(n.say.contains("tell me the name"), "{}", n.say);
    }
}
