//! The sandbox's two scripts. Neither holds a secret, and neither echoes a
//! command (no `set -x`): the service keeps what they print.
//!
//! **The start script** (`boat new --setup-file`), in order: the agent's
//! credential (a file Boat writes with mode 644) is set to 0600 at once and
//! moved to memory-backed storage (`/dev/shm`, when it is there and
//! writable; else a 0600 file in `~/.kontext-dispatch/`), because a stopped
//! sandbox's disk is kept for days; kontext is installed at the pinned
//! release (the installer is downloaded to a file and checked, and so is
//! `kontext --version` after it); the project is set up from the server with
//! the token from its file (read only into the environment of that one
//! command, retried for a while, and removed on every way out); the code
//! repo is the service's clone if it shows up within half a minute, else the
//! script clones it from `[[repos]]`' remote with the sandbox's GitHub login;
//! git gets an author in that clone when it has none (the GitHub login and
//! its noreply address); and `kontext install --local` runs there, with what
//! it wrote kept out of git's way (`info/exclude`).
//!
//! **The runner** (`~/.kontext-dispatch-turn.sh`, sent as a file): `start
//! <n> <session>` runs one turn of the coordinator, headless Claude Code in
//! the repo's clone with every permission, `--session-id` on the first turn
//! and `--resume` after, and leaves its JSON result, its stderr and its exit
//! code in `~/.kontext-dispatch/turn-<n>/`. It refuses to start a turn
//! twice. The credential goes only into the environment of the `claude`
//! process (the processes its tools start do not see it). `read <n>` prints
//! what a turn left, small enough for one `boat exec`.

/// Where things are in the sandbox. Relative paths start in the sandbox
/// user's home, which is where `--file` puts them.
pub const TOKEN_PATH: &str = ".kontext-dispatch-token";
pub const CREDENTIAL_PATH: &str = ".kontext-agent-credential";
pub const RUNNER_PATH: &str = ".kontext-dispatch-turn.sh";
pub const PROMPT_FIRST_PATH: &str = ".kontext-dispatch-prompt-first.md";
pub const PROMPT_RESUME_PATH: &str = ".kontext-dispatch-prompt-resume.md";

/// The exit code of a turn the runner would not start twice.
pub const ALREADY_STARTED: i32 = 90;

/// The command that removes the agent's credential from the sandbox,
/// wherever the start script put it (or before it moved it).
pub fn remove_credential_command() -> Vec<String> {
    vec![
        "sh".into(),
        "-c".into(),
        format!(
            "rm -f \"${{KONTEXT_DISPATCH_SHM:-/dev/shm}}/kontext-dispatch-agent-credential\" \
             \"$HOME/.kontext-dispatch/agent-credential\" \"$HOME/{CREDENTIAL_PATH}\""
        ),
    ]
}

/// What the start script needs. None of it is secret.
#[derive(Debug, Clone)]
pub struct ScriptArgs {
    pub server: String,
    pub project: String,
    pub epic: String,
    /// A release tag (`v0.3.0`).
    pub version: String,
    pub repo_dir: String,
    /// Where to clone the code repo from when the service did not.
    pub repo_url: Option<String>,
}

/// `'…'` for `sh`, whatever is inside.
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// A remote as `[[repos]]` writes it (`github.com/acme/shop`), as a URL
/// `git clone` takes with the sandbox's GitHub login (https).
pub fn clone_url(remote: &str) -> String {
    let r = remote.trim();
    if let Some(rest) = r
        .strip_prefix("git@github.com:")
        .or_else(|| r.strip_prefix("ssh://git@github.com/"))
    {
        return format!("https://github.com/{rest}");
    }
    if r.starts_with("github.com/") {
        return format!("https://{r}");
    }
    r.to_string()
}

/// The start script.
pub fn start_script(a: &ScriptArgs) -> String {
    let version = a.version.trim();
    let bare = version.strip_prefix('v').unwrap_or(version);
    format!(
        r#"#!/bin/sh
# kontext dispatch: sets this sandbox up for epic {epic_q} of project {project_q}.
# It holds no secret: the token and the agent's credential come as files. The
# token is read only by the command that needs it and removed on every way
# out; the credential is moved to memory at once.
set -eu
umask 077
SERVER={server}
PROJECT={project}
VERSION={version_q}
REPO_DIR={repo}
REPO_URL={repo_url}
HOME=${{HOME:-/home/user}}
TOKEN_FILE="$HOME/{token_path}"
CRED_IN="$HOME/{cred_path}"
D="$HOME/.kontext-dispatch"
SHM=${{KONTEXT_DISPATCH_SHM:-/dev/shm}}
trap 'rm -f "$TOKEN_FILE"' EXIT
say() {{ printf 'kontext-dispatch: %s\n' "$*" >&2; }}

# 0. The agent's credential: 0600 at once, then to memory-backed storage (a
#    stopped sandbox's disk is kept for days), else a 0600 file in $D.
mkdir -p "$D"
chmod 700 "$D"
if [ -f "$CRED_IN" ]; then
    chmod 600 "$CRED_IN"
    if [ -d "$SHM" ] && [ -w "$SHM" ]; then
        CRED="$SHM/kontext-dispatch-agent-credential"
    else
        CRED="$D/agent-credential"
    fi
    cat "$CRED_IN" >"$CRED.tmp"
    chmod 600 "$CRED.tmp"
    mv -f "$CRED.tmp" "$CRED"
    rm -f "$CRED_IN"
    if [ "$CRED" != "$D/agent-credential" ]; then
        rm -f "$D/agent-credential"
    fi
fi
if [ ! -s "$SHM/kontext-dispatch-agent-credential" ] && [ ! -s "$D/agent-credential" ]; then
    say "the agent's credential did not reach the sandbox"
    exit 1
fi

# 1. kontext, at the pinned release. A failed download or install stops here.
BIN_DIR="$HOME/.local/bin"
mkdir -p "$BIN_DIR"
PATH="$BIN_DIR:$PATH"
export PATH
if [ "$(kontext --version 2>/dev/null || true)" != "kontext {bare}" ]; then
    installer="$D/install.sh"
    if ! curl -fsSL -o "$installer" "https://raw.githubusercontent.com/OmarAlex24/kontext/$VERSION/install.sh"; then
        say "could not download kontext's installer for $VERSION"
        exit 1
    fi
    if ! KONTEXT_VERSION="$VERSION" KONTEXT_INSTALL_DIR="$BIN_DIR" sh "$installer"; then
        say "kontext's installer for $VERSION failed"
        exit 1
    fi
    rm -f "$installer"
    if [ "$(kontext --version 2>/dev/null || true)" != "kontext {bare}" ]; then
        say "kontext $VERSION is not on the PATH after installing it"
        exit 1
    fi
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

# 3. The code repo: the service's clone if it shows up soon, else ours, with
#    the sandbox's GitHub login.
n=0
while [ ! -e "$REPO_DIR/.git" ] && [ "$n" -lt "${{KONTEXT_DISPATCH_REPO_WAIT:-6}}" ]; do
    n=$((n + 1))
    sleep 5
done
if [ ! -e "$REPO_DIR/.git" ]; then
    if [ -z "$REPO_URL" ]; then
        say "$REPO_DIR has no git clone, and project.toml's [[repos]] has no remote to clone it from"
        exit 1
    fi
    if ! git clone --quiet "$REPO_URL" "$REPO_DIR"; then
        say "could not clone $REPO_URL into $REPO_DIR"
        exit 1
    fi
fi
cd "$REPO_DIR"

# 4. A git author in the clone when git has none: the sandbox's GitHub login
#    and its noreply address.
if [ -z "$(git config user.email 2>/dev/null || true)" ]; then
    login="$(gh api user --jq .login 2>/dev/null || true)"
    uid="$(gh api user --jq .id 2>/dev/null || true)"
    if [ -z "$login" ] || [ -z "$uid" ]; then
        say "git has no author and gh gave no GitHub login to make one from"
        exit 1
    fi
    git config user.name "$login"
    git config user.email "$uid+$login@users.noreply.github.com"
fi

# 5. The hooks, skills and agent files, kept out of git's way so they never
#    reach a commit.
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
        repo_url = sh_quote(a.repo_url.as_deref().unwrap_or("")),
        token_path = TOKEN_PATH,
        cred_path = CREDENTIAL_PATH,
        bare = sh_comment(bare),
        epic_plain = sh_comment(&a.epic),
    )
}

/// What the runner needs. None of it is secret.
#[derive(Debug, Clone)]
pub struct RunnerArgs {
    pub epic: String,
    pub repo_dir: String,
    /// The Claude Code model (`claude-opus-5-5`).
    pub model: String,
    /// The variable the credential goes in (`CLAUDE_CODE_OAUTH_TOKEN` or
    /// `ANTHROPIC_API_KEY`).
    pub credential_var: &'static str,
}

/// The runner.
pub fn runner_script(a: &RunnerArgs) -> String {
    format!(
        r#"#!/bin/sh
# kontext dispatch: one turn of the coordinator of epic {epic_q}, started with
# `boat exec --detach`, or what a turn left, read with `boat exec`:
#   sh ~/{runner} start <n> <session id>
#   sh ~/{runner} read <n>
# It holds no secret: the agent's credential is read from its file into the
# environment of the one claude process.
set -u
umask 077
HOME=${{HOME:-/home/user}}
D="$HOME/.kontext-dispatch"
SHM=${{KONTEXT_DISPATCH_SHM:-/dev/shm}}
REPO_DIR={repo}
MODEL={model}
what=${{1:-}}
n=${{2:-}}
case "$n" in
'' | *[!0-9]*)
    echo "kontext-dispatch: usage: sh $0 start <n> <session> | read <n>" >&2
    exit 2
    ;;
esac
T="$D/turn-$n"
case "$what" in
start)
    sid=${{3:-}}
    mkdir -p "$D"
    if ! mkdir "$T" 2>/dev/null; then
        echo "kontext-dispatch: turn $n was already started" >&2
        exit {already}
    fi
    echo "$$" >"$T/pid"
    fail() {{
        printf 'kontext-dispatch: %s\n' "$2" >"$T/stderr"
        echo "$1" >"$T/exit"
        exit "$1"
    }}
    case "$sid" in '' | *[!0-9a-f-]*) fail 2 "no session id" ;; esac
    CRED=""
    for f in "$SHM/kontext-dispatch-agent-credential" "$D/agent-credential"; do
        if [ -s "$f" ]; then
            CRED=$f
            break
        fi
    done
    [ -n "$CRED" ] || fail 91 "the agent's credential is not in the sandbox"
    command -v claude >/dev/null 2>&1 || fail 127 "claude is not on the PATH"
    cd "$REPO_DIR" 2>/dev/null || fail 92 "there is no clone at $REPO_DIR"
    if [ "$n" = 1 ]; then
        prompt="$HOME/{first}"
        set -- --session-id "$sid"
    else
        prompt="$HOME/{resume}"
        set -- --resume "$sid"
    fi
    [ -s "$prompt" ] || fail 93 "the prompt file $prompt is missing"
    code=0
    {var}="$(cat "$CRED")" claude -p "$(cat "$prompt")" --model "$MODEL" \
        --output-format json --dangerously-skip-permissions "$@" \
        </dev/null >"$T/result.json" 2>"$T/stderr" || code=$?
    echo "$code" >"$T/exit.tmp"
    mv -f "$T/exit.tmp" "$T/exit"
    exit "$code"
    ;;
read)
    printf 'pid=%s\n' "$(cat "$T/pid" 2>/dev/null || true)"
    printf 'exit=%s\n' "$(cat "$T/exit" 2>/dev/null || true)"
    printf 'flag=%s\n' "$(grep -o '"is_error":[a-z]*' "$T/result.json" 2>/dev/null | head -n 1 || true)"
    echo '--- result'
    head -c 6000 "$T/result.json" 2>/dev/null || true
    echo
    echo '--- stderr'
    tail -c 1500 "$T/stderr" 2>/dev/null || true
    echo
    ;;
*)
    echo "kontext-dispatch: usage: sh $0 start <n> <session> | read <n>" >&2
    exit 2
    ;;
esac
"#,
        epic_q = sh_comment(&a.epic),
        runner = RUNNER_PATH,
        repo = sh_quote(&a.repo_dir),
        model = sh_quote(&a.model),
        already = ALREADY_STARTED,
        first = PROMPT_FIRST_PATH,
        resume = PROMPT_RESUME_PATH,
        var = a.credential_var,
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
            repo_url: Some("https://github.com/a/b'c".into()),
        });
        assert!(
            s.contains(r"SERVER='http://127.0.0.1:1/'\''; rm -rf / #'"),
            "{s}"
        );
        assert!(s.contains("epic pay__x_"), "{s}");
        assert!(s.contains("REPO_DIR='/home/user/x y'"), "{s}");
        assert!(s.contains(r"REPO_URL='https://github.com/a/b'\''c'"), "{s}");
        assert!(s.contains("\"kontext 0.3.0\""), "{s}");
        assert!(!s.contains("set -x"));
        let r = runner_script(&RunnerArgs {
            epic: "pay$(x)".into(),
            repo_dir: "/home/user/x y".into(),
            model: "claude-opus-5-5'".into(),
            credential_var: "ANTHROPIC_API_KEY",
        });
        assert!(r.contains(r"MODEL='claude-opus-5-5'\'''"), "{r}");
        assert!(
            r.contains(r#"ANTHROPIC_API_KEY="$(cat "$CRED")" claude -p"#),
            "{r}"
        );
        assert!(!r.contains("set -x"));
    }

    #[test]
    fn clone_urls() {
        assert_eq!(
            clone_url("github.com/acme/shop"),
            "https://github.com/acme/shop"
        );
        assert_eq!(
            clone_url("git@github.com:acme/shop.git"),
            "https://github.com/acme/shop.git"
        );
        assert_eq!(
            clone_url("https://gitlab.com/a/b"),
            "https://gitlab.com/a/b"
        );
    }
}
