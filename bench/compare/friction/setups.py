"""The setups of the friction axis (design §3): every step from a clean machine
A with Claude Code and git to "machine B sees what A saved", as the tools' own
docs give them.

Each step is counted as the design counts it: `kind` is `command` (a terminal
command) or `manual` (done outside a terminal command: a browser, a dashboard,
an edited shell file, a new terminal, a secret carried between machines);
`account` marks a new account and `secret` a secret made by hand or shown once
(a step can be both a command and a secret, like `kontext token create`).
`conditional` steps are counted apart. Steps with `run` are re-executed by
friction/run.py in two fresh containers of the base image (Claude Code, git,
nothing else); steps without it are doc-only, with the reason. `harness` steps
are not counted: they stand in for the code clone (not counted in §3) or for the
agent saving something on A, and `check` steps confirm on B that it arrived.

`design` is the row of the design's §3 table, to compare with what the steps
add up to.
"""

from dataclasses import dataclass, field

KONTEXT_INSTALL = 'curl -fsSL "https://raw.githubusercontent.com/OmarAlex24/kontext/v0.2.0/install.sh" | KONTEXT_VERSION=v0.2.0 sh'
NEW_TERMINAL_HAS = "env -i HOME=$HOME PATH=/usr/local/bin:/usr/bin:/bin bash -lic 'command -v {0}' >/dev/null 2>&1"
CODE_A = "mkdir -p ~/work/repo && cd ~/work/repo && git init -q -b main && echo demo > README.md && git add -A && git commit -q -m init && git remote add origin $BENCH_REMOTE && git init -q --bare ${BENCH_REMOTE#file://} && git push -q origin main"
CODE_B = "git clone -q $BENCH_REMOTE ~/work/repo"


@dataclass
class Step:
    where: str  # A, B or A+B
    kind: str  # command, manual, harness, check
    text: str
    run: str | None = None
    doc: str | None = None  # why it is doc-only
    conditional: bool = False
    # For a conditional step: `run` exits 0 when the step was NOT needed here.
    probe: bool = False
    root: bool = False
    secret_out: str | None = None  # the step prints NAME=value for a later step
    secret_in: list = field(default_factory=list)
    timeout: int = 600
    account: bool = False
    secret: bool = False


@dataclass
class Setup:
    name: str
    title: str
    steps: list
    design: dict
    sidecar: bool = False
    after: str = ""


SETUPS = [
    Setup(
        "kontext",
        "kontext v0.2.0, server path (README \"With a kontext server\", install.sh)",
        sidecar=True,
        after="Every command pulls and pushes on its own",
        design={"commands": "8 (+1)", "manual": "5–7", "accounts": "1", "secrets": "2"},
        steps=[
            Step("A", "command", "curl -fsSL …/install.sh | sh", run=KONTEXT_INSTALL),
            Step("A", "manual", "Add ~/.local/bin to PATH if it is not there", run=NEW_TERMINAL_HAS.format("kontext"), conditional=True, probe=True),
            Step("A", "manual", "Create a Cloudflare account", doc="needs an account", account=True),
            Step("A", "manual", "In the Cloudflare dashboard, create an API token (Workers Scripts: Edit)", doc="needs an account", secret=True),
            Step("A", "command", "CLOUDFLARE_API_TOKEN=… kontext server deploy", doc="needs a Cloudflare account; the runs use the local sidecar instead"),
            Step("A", "command", "kontext server deploy --subdomain <name> (account without a workers.dev subdomain)", doc="needs a Cloudflare account", conditional=True),
            Step("A", "manual", "Put export KONTEXT_ACTOR=human:<name> in the shell rc", run="echo 'export KONTEXT_ACTOR=human:owner' >> ~/.bashrc"),
            Step("A", "harness", "the code repo and its remote (not counted)", run=CODE_A),
            Step("A", "command", "kontext init <project> --server <url>", run="cd ~/work/repo && KONTEXT_ACTOR=human:owner kontext init demo --server $KONTEXT_SERVER_URL", secret_in=["KONTEXT_ADMIN_SECRET"]),
            Step("A", "command", "kontext install in the code repo", run="cd ~/work/repo && KONTEXT_ACTOR=human:owner kontext install && git add -A && git commit -q -m kontext && git push -q origin main"),
            Step("A", "command", "kontext token create --role owner --person <name> (shown once)", run="cd ~/work/repo && KONTEXT_ACTOR=human:owner kontext token create --role owner --person owner | sed -n 's/^ *KONTEXT_TOKEN=/KONTEXT_TOKEN=/p'", secret_out="KONTEXT_TOKEN", secret=True),
            Step("A", "harness", "A saves something (stands in for the agent)", run="cd ~/work/repo && KONTEXT_ACTOR=human:owner kontext log 'release codename Blue Heron'"),
            Step("B", "command", "curl … install.sh | sh", run=KONTEXT_INSTALL),
            Step("B", "manual", "PATH as in step 2", run=NEW_TERMINAL_HAS.format("kontext"), conditional=True, probe=True),
            Step("B", "manual", "Carry the token from A to B", run="test -n \"$KONTEXT_TOKEN\"", secret_in=["KONTEXT_TOKEN"]),
            Step("B", "harness", "clone the code (not counted)", run=CODE_B),
            Step("B", "command", "KONTEXT_TOKEN=… kontext clone --server <url> <project>", run="KONTEXT_ACTOR=human:owner kontext clone --server $KONTEXT_SERVER_URL demo", secret_in=["KONTEXT_TOKEN"]),
            Step("B", "manual", "export KONTEXT_ACTOR=… in the rc", run="echo 'export KONTEXT_ACTOR=human:owner' >> ~/.bashrc"),
            Step("B", "command", "kontext install in the code repo", run="cd ~/work/repo && KONTEXT_ACTOR=human:owner kontext install"),
            Step("B", "check", "B sees what A saved", run="cd ~/work/repo && KONTEXT_ACTOR=human:owner kontext recall 'Blue Heron' | grep -qi heron"),
        ],
    ),
    Setup(
        "mem0",
        "mem0 plugin (hosted)",
        after="Automatic but asynchronous: flush every 5 exchanges / 5 min idle / session end, then server-side extraction",
        design={"commands": "2 + 2", "manual": "4–5", "accounts": "1", "secrets": "1"},
        steps=[
            Step("A", "manual", "Sign up at app.mem0.ai", doc="needs an account", account=True),
            Step("A", "manual", "Create an API key in the dashboard", doc="needs an account", secret=True),
            Step("A", "command", "claude plugin marketplace add mem0ai/mem0", run="claude plugin marketplace add mem0ai/mem0"),
            Step("A", "command", "claude plugin install mem0@mem0-plugins --scope user --config api_key=…", run="claude plugin install mem0@mem0-plugins --scope user && claude plugin list | grep -q mem0"),
            Step("A", "manual", "Restart Claude Code (or /reload-plugins)", doc="interactive"),
            Step("B", "manual", "Carry the key to B", doc="needs an account"),
            Step("B", "command", "claude plugin marketplace add mem0ai/mem0", run="claude plugin marketplace add mem0ai/mem0"),
            Step("B", "command", "claude plugin install … --config api_key=…", run="claude plugin install mem0@mem0-plugins --scope user && claude plugin list | grep -q mem0"),
            Step("B", "manual", "Restart Claude Code", doc="interactive"),
            Step("A+B", "manual", "Set the same user_id on both, for personal memories", doc="needs an account", conditional=True),
            Step("B", "check", "B sees what A saved", doc="needs an account and a model call"),
        ],
    ),
    Setup(
        "gentle-ai-git",
        "gentle-ai + Engram, git sync",
        after="Manual on every update: export, commit, push / pull, import",
        design={"commands": "2 + 2 install, + 2 + 2 per sync", "count": "4 + 4", "manual": "1–2", "accounts": "0", "secrets": "0"},
        steps=[
            Step("A", "command", "curl …/gentle-ai/…/install.sh | bash", run="curl -fsSL https://raw.githubusercontent.com/Gentleman-Programming/gentle-ai/main/scripts/install.sh -o /tmp/install.sh && bash /tmp/install.sh </dev/null"),
            Step("A", "manual", "Open a new terminal after the rc edit", run=NEW_TERMINAL_HAS.format("gentle-ai"), conditional=True, probe=True),
            Step("A", "command", "gentle-ai install --agent claude-code --preset full-gentleman", run="PATH=$HOME/.local/bin:$PATH gentle-ai install --agent claude-code --preset full-gentleman </dev/null"),
            Step("A", "harness", "the code repo and its remote (not counted)", run=CODE_A),
            Step("A", "harness", "A saves something (stands in for the agent)", run="cd ~/work/repo && PATH=$HOME/.local/bin:$PATH engram save 'release codename' 'The release codename is Blue Heron' --project demo"),
            Step("A", "command", "engram sync (per sync)", run="cd ~/work/repo && PATH=$HOME/.local/bin:$PATH engram sync --project demo"),
            Step("A", "command", "git add .engram, commit, push (per sync)", run="cd ~/work/repo && git add -A .engram && git commit -q -m 'engram sync' && git push -q origin main"),
            Step("B", "command", "curl … install.sh | bash", run="curl -fsSL https://raw.githubusercontent.com/Gentleman-Programming/gentle-ai/main/scripts/install.sh -o /tmp/install.sh && bash /tmp/install.sh </dev/null"),
            Step("B", "command", "gentle-ai install …", run="PATH=$HOME/.local/bin:$PATH gentle-ai install --agent claude-code --preset full-gentleman </dev/null"),
            Step("B", "command", "git pull (per sync; here the first clone)", run=CODE_B),
            Step("B", "command", "engram sync --import (per sync)", run="cd ~/work/repo && PATH=$HOME/.local/bin:$PATH engram sync --import"),
            Step("B", "check", "B sees what A saved", run="cd ~/work/repo && PATH=$HOME/.local/bin:$PATH engram search 'Blue Heron' --project demo | grep -qi heron"),
        ],
    ),
    Setup(
        "gentle-ai-cloud",
        "gentle-ai + Engram Cloud",
        after="Automatic (autosync)",
        design={"commands": "as above + docker compose up + engram cloud config + enroll on each", "manual": "a host with HTTPS, server secrets, autosync env on each", "accounts": "a VPS or similar", "secrets": "2+"},
        steps=[
            Step("A+B", "command", "install as in the git-sync setup (4 commands)", doc="counted in gentle-ai-git"),
            Step("host", "manual", "A VPS or similar both machines reach", doc="needs a host", account=True),
            Step("host", "manual", "Run it with HTTPS (bearer tokens require it)", doc="needs a host"),
            Step("host", "manual", "Set ENGRAM_CLOUD_TOKEN, ENGRAM_JWT_SECRET… on the server", doc="needs a host", secret=True),
            Step("host", "command", "docker compose up", doc="needs a host"),
            Step("A", "command", "engram cloud config --server …", doc="needs a host"),
            Step("A", "command", "engram cloud enroll", doc="needs a host"),
            Step("B", "command", "engram cloud config --server …", doc="needs a host"),
            Step("B", "command", "engram cloud enroll", doc="needs a host"),
            Step("A+B", "manual", "ENGRAM_CLOUD_AUTOSYNC=1 + token + server in the environment of engram mcp", doc="needs a host"),
            Step("A+B", "manual", "carry the cloud token to each machine", doc="needs a host", secret=True),
        ],
    ),
    Setup(
        "beads",
        "Beads",
        after="Manual bd dolt push / bd dolt pull; auto-push is opt-in, auto-pull not documented",
        design={"commands": "2 + 2 + Dolt install on both", "manual": "1–2", "accounts": "0", "secrets": "0"},
        steps=[
            # The installer exits 1 ("bd was installed but is not in PATH") when
            # ~/.local/bin is not on PATH, after installing bd there.
            Step("A", "command", "curl …/beads/…/install.sh | bash", run="curl -fsSL https://raw.githubusercontent.com/gastownhall/beads/main/scripts/install.sh -o /tmp/install.sh && (bash /tmp/install.sh </dev/null; test -x ~/.local/bin/bd)"),
            Step("A", "manual", "Add ~/.local/bin to PATH (the installer stops with an error until it is)", run=NEW_TERMINAL_HAS.format("bd"), conditional=True, probe=True),
            Step("A", "manual", "Install the pinned Dolt version, not latest", run="arch=$(uname -m | sed -e 's/^x86_64$/amd64/' -e 's/^aarch64$/arm64/'); curl -fsSL https://github.com/dolthub/dolt/releases/download/v2.2.0/dolt-linux-$arch.tar.gz | tar -xz -C /tmp && install -m 0755 /tmp/dolt-linux-$arch/bin/dolt /usr/local/bin/dolt", root=True),
            Step("A", "harness", "the code repo and its remote (not counted)", run=CODE_A),
            Step("A", "command", "bd init", run="cd ~/work/repo && PATH=$HOME/.local/bin:$PATH bd init --quiet && git push -q origin main"),
            Step("A", "harness", "A saves something (stands in for the agent)", run="cd ~/work/repo && PATH=$HOME/.local/bin:$PATH bd create 'Release codename Blue Heron'"),
            Step("A", "command", "bd dolt push", run="cd ~/work/repo && PATH=$HOME/.local/bin:$PATH bd dolt push"),
            Step("B", "command", "curl … install.sh | bash", run="curl -fsSL https://raw.githubusercontent.com/gastownhall/beads/main/scripts/install.sh -o /tmp/install.sh && (bash /tmp/install.sh </dev/null; test -x ~/.local/bin/bd)"),
            Step("B", "manual", "PATH as on A", run=NEW_TERMINAL_HAS.format("bd"), conditional=True, probe=True),
            Step("B", "manual", "Install the pinned Dolt version", run="arch=$(uname -m | sed -e 's/^x86_64$/amd64/' -e 's/^aarch64$/arm64/'); curl -fsSL https://github.com/dolthub/dolt/releases/download/v2.2.0/dolt-linux-$arch.tar.gz | tar -xz -C /tmp && install -m 0755 /tmp/dolt-linux-$arch/bin/dolt /usr/local/bin/dolt", root=True),
            Step("B", "harness", "clone the code (not counted)", run=CODE_B),
            Step("B", "command", "bd bootstrap", run="cd ~/work/repo && PATH=$HOME/.local/bin:$PATH bd bootstrap"),
            Step("B", "check", "B sees what A saved", run="cd ~/work/repo && export PATH=$HOME/.local/bin:$PATH && bd list 2>/dev/null; bd list 2>/dev/null | grep -qi heron"),
        ],
    ),
    Setup(
        "openspec",
        "OpenSpec",
        after="With git: commit/push/pull, which a developer already does",
        design={"commands": "2 + 0–1", "manual": "0", "accounts": "0", "secrets": "0"},
        steps=[
            Step("A", "command", "npm install -g @fission-ai/openspec@latest", run="npm install -g @fission-ai/openspec@latest", root=True),
            Step("A", "harness", "the code repo and its remote (not counted)", run=CODE_A),
            Step("A", "command", "openspec init --tools claude", run="cd ~/work/repo && OPENSPEC_TELEMETRY=0 openspec init --tools claude"),
            Step("A", "harness", "A saves something (stands in for the agent)", run="cd ~/work/repo && mkdir -p openspec/specs/release && printf '# Release\\n\\n## Purpose\\nThe release codename is Blue Heron.\\n\\n## Requirements\\n### Requirement: Codename\\nThe release SHALL be called Blue Heron.\\n\\n#### Scenario: naming\\n- **WHEN** a release is cut\\n- **THEN** it is called Blue Heron\\n' > openspec/specs/release/spec.md && git add -A && git commit -q -m spec && git push -q origin main"),
            Step("B", "command", "npm install -g @fission-ai/openspec (only to run it on B)", run="npm install -g @fission-ai/openspec@latest", root=True, conditional=True),
            Step("B", "harness", "clone the code (not counted)", run=CODE_B),
            Step("B", "check", "B sees what A saved", run="cd ~/work/repo && grep -qi heron openspec/specs/release/spec.md && OPENSPEC_TELEMETRY=0 openspec list --specs | grep -qi release"),
        ],
    ),
    Setup(
        "claude-mem",
        "claude-mem + cmem.ai Pro",
        after="Automatic: the worker syncs on write and pulls at session start",
        design={"commands": "1 + 1", "manual": "sign in through the browser on each machine; a checkout page with a payment method", "accounts": "1 (with a card)", "secrets": "handled by the installer"},
        steps=[
            Step("A", "manual", "Install unzip (Bun's installer, which claude-mem runs, needs it; not in the docs)", run="apt-get update -qq && apt-get install -y -qq unzip >/dev/null", root=True, conditional=True),
            Step("A", "command", "npx claude-mem install", run="CLAUDE_MEM_ONLINE_OPTIN=false timeout 300 npx -y claude-mem install </dev/null", timeout=360),
            Step("A", "manual", "Sign in through the browser; 30-day trial checkout with a payment method", doc="needs an account and a card", account=True),
            Step("B", "manual", "Install unzip, as on A", run="apt-get update -qq && apt-get install -y -qq unzip >/dev/null", root=True, conditional=True),
            Step("B", "command", "npx claude-mem install", run="CLAUDE_MEM_ONLINE_OPTIN=false timeout 300 npx -y claude-mem install </dev/null", timeout=360),
            Step("B", "manual", "Sign in through the browser", doc="needs an account"),
            Step("B", "check", "B sees what A saved", doc="needs an account and a model call"),
        ],
    ),
    Setup(
        "plain",
        "plain Claude Code",
        after="Automatic memory does not travel; CLAUDE.md travels with git",
        design={"commands": "0", "manual": "0", "accounts": "0", "secrets": "0"},
        steps=[],
    ),
]
