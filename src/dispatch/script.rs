//! The sandbox's start script (`boat new --setup-file`). It holds no secret:
//! the token arrives as a file ([`super::TOKEN_PATH`], in the sandbox user's
//! home), the script reads it only into the environment of the one command
//! that sets the project up, and removes it on every way out. It never
//! echoes a command (no `set -x`): the service keeps what it prints.
//!
//! In order: install the pinned kontext release with `install.sh`; set the
//! project up from the server with the token (`kontext clone --server`,
//! retried for a while, since the server may not be reachable yet); wait for
//! the service to clone the code repo; run `kontext install --local` there,
//! which the environment variables alone do not do (they bring the ledger,
//! not the hooks, skills or agent files), and keep what it wrote out of
//! git's way (`info/exclude`) so the coordinator never commits it.

/// What the script needs. None of it is secret.
#[derive(Debug, Clone)]
pub struct ScriptArgs {
    pub server: String,
    pub project: String,
    pub epic: String,
    /// A release tag (`v0.3.0`).
    pub version: String,
    pub repo_dir: String,
}

/// `'…'` for `sh`, whatever is inside.
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// The script.
pub fn start_script(a: &ScriptArgs) -> String {
    let version = a.version.trim();
    let bare = version.strip_prefix('v').unwrap_or(version);
    format!(
        r#"#!/bin/sh
# kontext dispatch: sets this sandbox up for epic {epic_q} of project {project_q}.
# It holds no secret: the token comes as a file, read only by the command that
# needs it, and removed on every way out.
set -eu
umask 077
SERVER={server}
PROJECT={project}
VERSION={version_q}
REPO_DIR={repo}
HOME=${{HOME:-/home/user}}
TOKEN_FILE="$HOME/{token_path}"
trap 'rm -f "$TOKEN_FILE"' EXIT
say() {{ printf 'kontext-dispatch: %s\n' "$*" >&2; }}

# 1. kontext, at the pinned release.
BIN_DIR="$HOME/.local/bin"
mkdir -p "$BIN_DIR"
PATH="$BIN_DIR:$PATH"
export PATH
if [ "$(kontext --version 2>/dev/null || true)" != "kontext {bare}" ]; then
    curl -fsSL "https://raw.githubusercontent.com/OmarAlex24/kontext/$VERSION/install.sh" |
        KONTEXT_VERSION="$VERSION" KONTEXT_INSTALL_DIR="$BIN_DIR" sh
fi

# 2. The project, from its server, with the token from the file (kontext keeps
#    it in its config folder, 0600). The server may take a while to answer.
n=0
until kontext --project "$PROJECT" status >/dev/null 2>&1; do
    n=$((n + 1))
    if [ "$n" -gt 60 ]; then
        say "could not set project $PROJECT up from $SERVER"
        exit 1
    fi
    if [ ! -s "$TOKEN_FILE" ]; then
        say "the token file is missing"
        exit 1
    fi
    KONTEXT_TOKEN="$(cat "$TOKEN_FILE")" kontext clone --server "$SERVER" "$PROJECT" ||
        sleep 10
done
rm -f "$TOKEN_FILE"
kontext --project "$PROJECT" sync >/dev/null 2>&1 || true

# 3. The code repo: wait for its clone, then the hooks, skills and agent files,
#    kept out of git's way so they never reach a commit.
n=0
until [ -d "$REPO_DIR/.git" ]; do
    n=$((n + 1))
    if [ "$n" -gt 120 ]; then
        say "$REPO_DIR has no git clone"
        exit 1
    fi
    sleep 5
done
cd "$REPO_DIR"
kontext --project "$PROJECT" --role coordinator install --local
exclude="$(git rev-parse --git-path info/exclude)"
mkdir -p "$(dirname "$exclude")"
for p in .kontext.toml .claude/settings.local.json '.claude/skills/kontext*/' '.claude/agents/kontext-*.md' .pm/; do
    grep -qxF "$p" "$exclude" 2>/dev/null || printf '%s\n' "$p" >>"$exclude"
done
say "ready: project $PROJECT, epic {epic_plain}, repo $REPO_DIR, $(kontext --version)"
"#,
        epic_q = sh_comment(&a.epic),
        project_q = sh_comment(&a.project),
        server = sh_quote(&a.server),
        project = sh_quote(&a.project),
        version_q = sh_quote(version),
        repo = sh_quote(&a.repo_dir),
        token_path = super::TOKEN_PATH,
        bare = sh_comment(bare),
        epic_plain = sh_comment(&a.epic),
    )
}

/// Text that goes inside the script outside quotes (a comment, a message, a
/// version compared in double quotes): only characters that mean nothing to
/// `sh` there.
fn sh_comment(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "-_.:@/".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting_survives_anything() {
        assert_eq!(sh_quote("a'b"), r"'a'\''b'");
        let s = start_script(&ScriptArgs {
            server: "http://127.0.0.1:1/'; rm -rf / #".into(),
            project: "demo".into(),
            epic: "pay$(x)".into(),
            version: "v0.3.0".into(),
            repo_dir: "/home/user/x y".into(),
        });
        assert!(
            s.contains(r"SERVER='http://127.0.0.1:1/'\''; rm -rf / #'"),
            "{s}"
        );
        assert!(s.contains("epic pay__x_"), "{s}");
        assert!(s.contains("REPO_DIR='/home/user/x y'"), "{s}");
        assert!(s.contains("\"kontext 0.3.0\""), "{s}");
        assert!(!s.contains("set -x"));
    }
}
