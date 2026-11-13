# kontext

The command is also called `kontext`: the binary, the subcommands, the `KONTEXT_*` variables, `.kontext.toml`, `~/.config/kontext` and `~/kontext-home`.

Project memory for coding agents. A single ledger where entries are only ever appended, and one view for each reader: you, the project manager, each epic coordinator, and the agents that do a single task.

Status: phases 1 (base), 2 (charter, decisions, views, hooks), 3 (epics, tasks and agent teams), 4 (facts with fingerprints, `find`, distill and `changelog`), 5 (several people: `sync`, `clone`, identity and signatures) and 6 (`kontext ui` and the benchmark harness, not run) are implemented, plus the human's inbox, screenshots as evidence and the report ("The human as a reader"). The full design is in [DESIGN.md](./DESIGN.md).

## Install

You need Rust 1.89 or newer (`rustup`) and `git`.

```sh
cargo install --path . --locked
```

That leaves the `kontext` binary in `~/.cargo/bin`. Do this before `kontext install`: the hooks it writes into `.claude/settings.json` record the absolute path of the binary that ran it. Installed from `target/debug/kontext`, they point at a file that `cargo clean` deletes, and from then on they fail without a word (`kontext install` prints a note when that is the case). If the binary moves, run `kontext install` again: it repoints the hooks. `kontext doctor` checks all of this.

### Have an agent install it and migrate the memory

The skill `kontext-migrate` tells an agent how to install kontext in a project and move the memory that already exists into it: a `ctx.py` ledger (the v0 prototype), and the rules and decisions in `CLAUDE.md`, `AGENTS.md`, notes and Claude Code's automatic memory (as proposals that you sign). It asks once about anything it can't work out, and says up front where it is going to write.

To have it in all your sessions before installing anything (the repo is private: it needs `gh auth login`):

```sh
mkdir -p ~/.claude/skills/kontext-migrate
gh api repos/OmarAlex24/kontext/contents/templates/skill-kontext-migrate.md \
  -H "Accept: application/vnd.github.raw" > ~/.claude/skills/kontext-migrate/SKILL.md
```

and ask Claude Code "install kontext in this project". With the binary already installed, `kontext install --skills-only --global` adds (or updates) all the kontext skills in `~/.claude/skills/`, without needing a project. `kontext install` in a repo also installs them.

## See it work

The fastest way to see kontext with real data is the sample project:

```sh
cargo build
examples/demo.sh /tmp/kontext-demo
KONTEXT_CONFIG_DIR=/tmp/kontext-demo/config KONTEXT_ACTOR=human:omar target/debug/kontext --project billing ui --open
```

`examples/demo.sh [folder]` builds, without touching your `~/.config/kontext` (everything goes through `KONTEXT_CONFIG_DIR` inside the folder), a `kontext-home` called `billing` and a code repo with three days of work: charter and goals signed, decisions with guards (one replaced), an epic with tasks in every state, a fact that expired because its file changed, a distill proposal not yet reviewed, a task that serves no goal, questions, a second writer (`ana@laptop`) and three readers with their cursor at different points. At the end it prints the commands to see it (`ui`, `standup --peek`, and any other with the same `KONTEXT_CONFIG_DIR`). It uses `$KONTEXT_BIN`, or `target/debug/kontext`, or the `kontext` on the `PATH`. The `kontext ui` tests run against this same project.

## Phase 1 commands

| Command | What it does |
|---|---|
| `kontext init <name> [--home <path>] [--remote <git-url>]` | Creates a `kontext-home` (default `~/kontext-home/<name>`) with `git init`, `project.toml`, `ledger/`, `cursors/` and `.gitignore`, and registers it in `~/.config/kontext/projects.toml` |
| `kontext verify` | Checks the chain of each ledger file: `n` in sequence, `prev`, `hash` and `id`. Prints `OK: …` or the first problem in each file as `ledger/<writer>.jsonl:<line>: …`, and exits with 1 if there are problems. An entry of a kind from a newer kontext is listed ("… is from a newer kontext version (kind scope): upgrade to read it") but is not a problem; it counts for nothing until you upgrade |
| `kontext reindex` | Rebuilds `.kontext/index.sqlite` from the ledger files |
| `kontext import --from-v0 <path> [--force]` | Imports the ledger of the v0 `ctx.py` prototype (`<path>/ledger/entries.jsonl`) into this writer's file. It can be run more than once: it doesn't duplicate anything |

All of them accept `--project <name>`.

### Which project is used

In this order: `--project`, the `KONTEXT_PROJECT` variable, the nearest `.kontext.toml` going up from the current folder (`project = "<name>"`; in a linked git worktree, up to its root, and then the main checkout's: see "Working in git worktrees"), and if only one project is registered, that one.

### Machine configuration

`~/.config/kontext/projects.toml` (another folder with `KONTEXT_CONFIG_DIR`):

```toml
writer = "omar@t3-machine"     # if missing, <user>@<machine>; kontext init sets it

[projects.billing]
home = "/home/you/kontext-home/billing"
```

`kontext init` rewrites this file, so it loses any comments it has.

### Example

```sh
kontext init shop
kontext import --from-v0 ~/old-ctx
kontext verify
```

## Phase 2 commands

| Command | What it does |
|---|---|
| `kontext charter --objective <text> [--non-goal <text>]… [--done-when <text>] [--title <text>]` | Writes a new, unsigned charter. If there is already a signed charter, the new one replaces it when it is signed |
| `kontext goal add <title> [--done-when <text>]` | Adds a goal to the charter in force (or to the last unsigned one) |
| `kontext sign <id>` | Signs a charter or a goal. Only an owner |
| `kontext decide <title> [--why] [--body] [--tags a,b] [--supersedes <id>]… [--epic <x>] [--guard-cmd <cmd>] [--approved-by <owner>]` | Records a decision. Only an owner, or the pm with `--approved-by` |
| `kontext propose <title> [--why] [--body] [--tags] [--supersedes <id>]…` | Proposes a decision. Anyone can, and that is what agents do |
| `kontext accept <P-id> [--as-decision] [--approved-by <owner>] [--why]` | Accepts a proposal: it becomes law. With `--as-decision` it also writes the decision |
| `kontext reject <P-id> --why <text>` | Rejects a proposal |
| `kontext supersede <old> <new> [--why]` | Marks that one entry replaces another (charters, goals and decisions: only an owner; facts: anyone) |
| `kontext ask <title> --to <reader> [--body]` | Asks `owner`, `pm`, `epic:<x>` or a person |
| `kontext answer <Q-id> <text>` | Answers a question |
| `kontext log <text> [--next <text>] [--task <id>] [--tags a,b]` | Journal entry |
| `kontext standup [--peek] [--as <reader>]` | What happened since your last visit and what is waiting for your answer. Advances your cursor (with `--peek`, it doesn't) |
| `kontext resume --as <role> [--max-chars N] [--peek]` | The context for a role (`pm`, `owner`…): charter, goals, decisions in force, what the user said (the distill notes it, with a date), questions, proposals, facts, journal and what's new, trimmed to the budget by priority (the list of decisions last: then it shows the newest and how many are not shown). Advances the role's cursor |
| `kontext show <id>` | A full entry with its history. Accepts prefixes (`D-7f3`) |
| `kontext recall <words> [--limit N] [--since YYYY-MM-DD]` | Searches the ledger and Claude Code's transcripts |
| `kontext hook session-start` | Called by Claude Code when a session starts. Never fails and doesn't advance cursors |
| `kontext install [--dry-run] [--force] [--role <role>] [--local] [--worktree-role <role>]` | In a code repo: `.kontext.toml`, the start hook and the `Bash(kontext:*)` permission in `.claude/settings.json` (with `--local`, in `.claude/settings.local.json`), the skills `kontext`, `kontext-orchestrate`, `kontext-show`, `kontext-migrate` and `kontext-pm` in `.claude/skills/`, the agent files (phase 3, and the `.pm/` reader), and the repo in `[[repos]]` of `project.toml`. `--worktree-role` sets `worktree_role` in `.kontext.toml`. Shows the diff before writing |
| `kontext install --link-only [--role <role>] [--worktree-role <role>] [--dry-run]` | Only `.kontext.toml`, in the current checkout: nothing under `.claude/`, no skills, no agent files, no `[[repos]]` change. In a linked git worktree it marks it as the PM's (see "Working in git worktrees") |
| `kontext install --skills-only [--global] [--dry-run]` | Only the skills, in the repo's `.claude/skills/` or, with `--global`, in `~/.claude/skills/` (or `$CLAUDE_CONFIG_DIR/skills/`). Not the agent files. Doesn't need a project |

All of them accept `--json` (the ones that write print the new entry; the views print their data), `--actor`, `--role` and `--model` (in `install`, `--role` is the `default_role` of `.kontext.toml`).

IDs are `<type>-<6 hex>`: `A` charter, `G` goal, `E` epic, `T` task, `D` decision, `P` proposal, `Q` question, `F` fact, `J` journal, and `R` for records (signature, acceptance, answer, claim, close…). Where an id is asked for, a unique prefix is enough.

### Who writes

Each entry says who wrote it. In this order: `--actor`, the `KONTEXT_ACTOR` variable, and if there is neither, `agent:unknown`. An agent that forgets to say who it is never ends up recorded as a human. An agent's role and model come from `--role` and `--model`, or from `KONTEXT_ROLE` and `KONTEXT_MODEL`.

Put your name in your shell (`~/.bashrc` or `~/.zshrc`), the same one that is in `owners` in `project.toml`:

```sh
export KONTEXT_ACTOR=human:omar
```

Agents use `KONTEXT_ACTOR=agent:<name>` in the environment they are launched with and, if they know it, `KONTEXT_ROLE`, `KONTEXT_MODEL` and `KONTEXT_SESSION`. Inside a session, an agent passes its identity as arguments (`kontext --actor agent:<name> --role <role> log …`), not as variables in front of the command: Claude Code's `Bash(kontext:*)` permission only covers commands that start with `kontext`, and in `claude -p` nobody approves the others. The hook and `resume` tell it how to write and how to invoke `kontext`.

Only an owner signs, decides, accepts, rejects and replaces charters, goals or decisions. An agent that tries gets a rejection (exit code 2) that tells it to propose. The exception is the pm (`--role pm` or `KONTEXT_ROLE=pm`) with `--approved-by <owner>`, for when an owner has already approved it in the conversation. If an entry of those types arrives from another machine, written by someone who is not an owner, it is kept but doesn't count, and it shows up as an alert in `standup`.

### Cursors

Each reader (`omar`, `pm`, `epic:payments`) has a cursor in `cursors/<reader>/<writer>.json`: how far it has read in each ledger file. `standup` and `resume` show what's new from there and advance it; `--peek` and the start hook don't. Since the cursor is per file, what arrives late when syncing shows up as new even if it was written earlier.

### Typical flow

```sh
export KONTEXT_ACTOR=human:omar
kontext init billing
kontext charter --objective "Charge by card in the app" --non-goal "Invoicing" --done-when "First real charge"
kontext sign A-1a1                       # the id that charter printed
kontext goal add "Monthly recurring charge" --done-when "3 charges in a row with no errors"
kontext sign G-13a
kontext decide "Charge with Stripe, not Conekta" --why "Conekta doesn't support recurring charges with 3DS" --tags payments

cd ~/code/billing-app
kontext install --dry-run                # shows the diff
kontext install

# ...the agents work: they propose, ask, leave journal entries...

kontext standup                          # what happened and what it expects from you
kontext accept P-516 --as-decision
kontext answer Q-733 "199 pesos a month"
kontext show D-f99
```

### Hooks and modes

`kontext install` adds to `.claude/settings.json` a `SessionStart` hook (`startup|resume|clear|compact`) that runs `<kontext path> hook session-start`, and the `Bash(kontext:*)` permission in `permissions.allow`. With `--local` they go in `.claude/settings.local.json` instead, which is the place for them in a public repo (the hook holds an absolute path of your machine) and when sessions run in linked git worktrees (next section). kontext's entries live in one of the two files: `--local` removes kontext's own entries from `settings.json` (nothing else in it), and a later `kontext install` without the flag keeps them in `settings.local.json` and says so. The role comes from `KONTEXT_ROLE`, then from `default_role` in `.kontext.toml`, then, in a linked worktree without a `.kontext.toml` of its own, from `worktree_role` (or `default_role`) in the repo's (the main checkout's, or a tracked one), and if none is set, `pm`. What it puts in depends on `[context]` in `project.toml` (the role, then `default`, then `inject`; `kontext init` sets `inject` for everyone):

- `inject`: the `resume` for the role, trimmed to 9,000 characters (never more than 9,900). With many decisions in force, it lists the newest and says how many more there are and how to search them.
- `pointer`: two lines with how many new items and questions there are and what to run.
- `off`: nothing; the skill remains.

Hook errors are logged to `<kontext-home>/.kontext/hooks.log`; the hook always exits with 0.

With `[capture] enabled = true` in `project.toml` (off by default), `kontext install` also adds `UserPromptSubmit` → `<kontext path> hook user-prompt-submit`, `Stop` → `<kontext path> hook stop` and `SessionEnd` → `<kontext path> hook session-end` (one `SessionEnd` entry, shared with the distill: it does both jobs, each only if it is on). They write your messages into `.pm/dumps/` (see "The PM's memory of your ideas") and print nothing, so nothing goes into the conversation. Turning the setting off later does not remove them, as with the distill's hooks: each run checks it and does nothing.

### Working in git worktrees

Some tools run every agent conversation, the one you talk to (the PM) included, in its own linked git worktree (`git worktree add`), and in a public repo `.kontext.toml` and `.claude/` are not committed, so they exist only in the main checkout. Four rules cover that:

1. **The project is found through the main checkout.** In a linked worktree, `.kontext.toml` is looked for up to the worktree's root; if there is none, the one at the root of the main checkout is used (none for a bare repository), and only then the folders above the worktree, as before. A `.kontext.toml` that git tracks is the repo's file, in every worktree, not one worktree's own. `--project` and `KONTEXT_PROJECT` still win, and "the only project registered" is still the last fallback.
2. **`worktree_role`.** A session in a worktree without a `.kontext.toml` of its own (an untracked one at its root) gets `worktree_role` from the repo's file (the main checkout's, or the tracked one), or else its `default_role`; the main checkout gets `default_role`. Set it with `kontext install --worktree-role agent` in the main checkout (later `install` runs keep it). `agent` is the value for delegated agents whose role you do not know: no line about your pending items, no capture, and the project's view with the facts of every epic; its questions list only shows questions addressed `--to agent` (none, normally), and its cursor is shared by every session with that role.
3. **Mark the PM's worktree.** `kontext install --link-only` in the worktree where you talk to the PM writes only its own `.kontext.toml` (`default_role = "pm"`, untracked by git): that worktree is the PM's. Its own file wins over `worktree_role`. With a tracked `.kontext.toml` the PM's worktree cannot be marked (the file is already there, and it is the repo's): untrack it to mark one.
4. **One `.pm/`.** `.pm/` belongs to the main checkout: `kontext ideas`, the report and the capture use the main checkout's from any linked worktree (with `[capture] worktrees = true`, each worktree's own, as before). The capture writes only from the main checkout and from the PM's worktree; in any other worktree it writes nothing and `hooks.log` says how to mark it. If another conversation runs as `pm` in the PM's worktree or in the main checkout (a coordinator), name the PM's with `kontext capture here` (see "Capture" below): then only it is captured. `kontext doctor` prints the `.pm/` path in use under Capture.

The steps, in this order:

1. In the worktree where you talk to the PM: `kontext --project <name> install --link-only` (it marks it as the PM's; `--project`, because the main checkout has no `.kontext.toml` yet).
2. In the main checkout: `kontext install --local --worktree-role agent`.

The other way round, every worktree, the PM's included, starts as `agent` until the first step is done, and nothing you type there is captured.

What Claude Code reads from `.claude/` in a worktree is in its documentation, not in kontext:

- **Settings** page, "Where Claude Code keeps the local file in a git repository": since v2.1.211, `.claude/settings.local.json` is read at the repo root, and in a worktree, the file at the main checkout's root. Not outside a git repo, when the repo root is your home directory, on Windows, or when the repo root or its `.git` or `.claude` entry is not owned by you. The shared `.claude/settings.json` is read from the session's own folder: a worktree session gets one only if the worktree has it, that is, if git tracks it.
- **Worktrees** page, "What worktrees share with the main checkout": since v2.1.277, a worktree with no `.claude/skills` at its root loads the main checkout's project skills; one with its own loads only that copy. The same goes for `.claude/agents` and `.claude/commands`, also in worktrees made with `git worktree add`.
- **Hooks reference:** the same handler defined in more than one settings file runs once.

So, with `.claude/` not committed, `--local` in the main checkout puts the hooks where every worktree session reads them, and the skills and agent files installed there reach worktrees without a `.claude/skills` or `.claude/agents` of their own. `kontext install --local` in a linked worktree is refused (its own `settings.local.json` is not read). In the exceptions above, or with an older Claude Code, worktree sessions do not get the hooks of an untracked file; committing `.claude/settings.json` puts them in every worktree. A full `kontext install` in a linked worktree without a `.kontext.toml` of its own, when the main checkout has one, is refused: it names the main checkout, and `--link-only` and `--skills-only` still work there. Where it proceeds, `[[repos]]` names the repo after the main checkout's folder, not the worktree's (a worktree of a bare repo leaves `[[repos]]` as it is and says so). When `--local` creates `.claude/settings.local.json`, or `--link-only` creates `.kontext.toml` in a linked worktree, and git neither tracks nor ignores it, `install` prints the line to add to the repo's `.git/info/exclude`, which every worktree shares.

`kontext doctor`, run in a worktree, counts the main checkout's `settings.local.json`, skills and agent files as this session's, and notes hooks that are only in an untracked `settings.json` of the main checkout. While the hooks are in both files, a copy that differs (another binary path, after a reinstall) runs as well; the capture writes a prompt delivered twice once (the same `prompt_id` and question-tool answer are written once, from either checkout), the start context would go in twice, so keep one copy (`--local`).

### Environment variables

| Variable | What for |
|---|---|
| `KONTEXT_ACTOR` | Who writes (`human:<name>` or `agent:<name>`) |
| `KONTEXT_ROLE`, `KONTEXT_MODEL`, `KONTEXT_SESSION` | An agent's role, model and session; `KONTEXT_ROLE` also picks the hook's role |
| `KONTEXT_PROJECT` | Which project to use |
| `KONTEXT_CONFIG_DIR` | Another folder instead of `~/.config/kontext` |
| `KONTEXT_TRANSCRIPTS_GLOB` | Where `recall` looks for transcripts (patterns separated by `:`; default `~/.claude/projects/*/*.jsonl`) |
| `KONTEXT_NOW` | For tests: the time (ISO-8601) that `kontext` uses for the `hlc` of what it writes and to tell whether a claim has expired |
| `KONTEXT_EPIC` | With `KONTEXT_ROLE=coordinator`, the start hook adds the `brief` of that epic (phase 4) |
| `KONTEXT_DISTILL_DISABLE` | If set to anything, the `pre-compact` and `session-end` hooks don't run the distill |
| `KONTEXT_CAPTURE_DISABLE` | If set to anything, the capture hooks (`user-prompt-submit`, `stop`, `session-end`) write nothing |
| `KONTEXT_GH_BIN` | Another binary instead of `gh` for `changelog` (tests) |

## Phase 3 commands

| Command | What it does |
|---|---|
| `kontext epic add <title> --goal <G>… [--coordinator <name>] [--name <short>] [--after <epic>]…` | Creates an epic linked to goals in the charter. Its reader is `epic:<name>` (the name comes from the title if not given) and its default coordinator, `epic:<name>`. `--after`: epics that have to close first (see "Orchestrating several agents") |
| `kontext feature add <title> --epic <epic> [--acceptance <text>] [--goal <G>]…` | Adds a feature: what the human would see working, between the epic and its tasks. Only the pm or an owner. Without `--goal`, it serves the epic's goals |
| `kontext task add <title> --epic <epic> [--feature <U>] [--goal <G>]… [--depends-on <T>]… [--acceptance <text>]` | Adds a task. Without `--goal`, it serves the epic's goals (with `--feature`, the feature's); if none are left, it is still written, with a drift warning. With `--feature`, `--epic` can be left out; one that isn't the feature's epic is refused |
| `kontext assign <T> --to <name>` | Assigns a task. Only the epic's coordinator, the pm or an owner |
| `kontext next --epic <epic> [--claim] [--max-chars N]` | The assignment for the next task: the one you already have, or the first ready one. Brings acceptance, goals, decisions and facts that touch it, and the **assignment mark**. With `--claim` it claims it (atomic on the machine); if none is ready, it exits with 2 and says what blocks it |
| `kontext brief <epic> [--max-chars N] [--peek]` | The coordinator's view: charter, goals and decisions of the epic, the board (ready, claimed, assigned, blocked, closed with their evidence), questions for `epic:<name>`, facts, journal, alerts and the assignment mark. Advances the cursor of `epic:<name>` (with `--peek`, it doesn't) |
| `kontext done <T> --since <mark> [--run <cmd>] [--evidence <spec>]… [--evidence-json <json>]` | Closes with evidence. `--run` runs the command and only closes if it exits with 0 (it keeps the code and the last 20 lines). If something that touches the task came in after the mark, it exits with 3 and lists it |
| `kontext done <U> [--run <cmd>] [--evidence <spec>]… [--since <mark>]` | Validates a feature whose tasks are all closed, against what the human should see. Only the epic's coordinator, the pm or an owner; `--since` is optional |
| `kontext release <T> [--why]` | The task goes back to ready. Whoever has it, the coordinator, the pm or an owner |
| `kontext reopen <T\|U> --why <text>` | Reopens a closed task, or a feature's validation |
| `kontext ask <title> [--to <reader>] [--task <T>] [--feature <U>]` | With `--task`, the question belongs to that task and, if you don't say who it's for, it goes to its epic. Its answer counts as new context in `done`. With `--feature`, it is about that feature |
| `kontext fact <text> [--path <path>]… [--repo <name>] [--epic <epic>] [--tags a,b]` | A fact, with the fingerprint of the files it came from (phase 4). With `--epic` it shows up in that epic's `brief` and `next` |

`<epic>` can be the id (`E-61e`), `epic:<name>` or `<name>`. Evidence is `cmd:"<command>"=<code>`, `file:<path>`, `pr:<url>` or `url:<url>`; a command without exit code 0 doesn't count, and `file:` has to exist (relative to the folder where you run `done`; it accepts `path:line`).

### Task states

`ready` (its dependencies are closed and nobody has it), `blocked` (some dependency is still open), `assigned`, `claimed` and `closed`. Reopening it puts it back to ready (or blocked).

- **Who closes:** `done` and `release` only count if written by whoever has the task, the coordinator of its epic, the pm or an owner. The command line refuses (exit code 2); if it arrives from another machine, it is kept, doesn't count, and shows up as an alert.
- **Expired claims:** a claim with no `kontext log --task` or `done` from its owner during `[claims] lease` (2 h by default) expires and the task goes back to ready. If two machines claim the same task, the one with the lower `hlc` wins.
- **Stale context:** `done --since <mark>` checks whether a decision for the epic (or one that cites the task) came in after the mark, the replacement of a goal the task cites, or the answer to a question about the task. If something came in, it exits with 3, lists it, and gives the mark for now. Agents always pass `--since`; an owner can leave it out.
- **Drift:** a task with no goals, or one that cites a goal that was replaced or doesn't exist, and an epic that cites a replaced goal, show up as alerts in `standup` and `resume` until they are fixed. Citing an unsigned goal shows as "unsigned goal".

### Features

An epic can have **features** (`U-…`): what you would see working ("Refunds"), with `--acceptance` saying what that is. Tasks hang from them (`task add --feature`). A feature is `pending` (no task started), `in progress` (a task started, reopened, or a validation lost), `to validate` (all its tasks closed), `done` (its coordinator, the pm or an owner checked it against its acceptance with `kontext done <U>`) or `superseded`. If a task is reopened or added after the validation, the feature goes back to in progress on its own, with a note. An epic with features is closed in `kontext plan` when all its features are done and its tasks without a feature are closed; the open tasks of a superseded feature don't keep it open, but each one shows an alert until it is closed or its work moves to a feature in force. A task counts toward its feature only in the feature's own epic. An epic without features works exactly as before. `brief` groups the board by feature, `next` says which feature the task is for, and the pm's line per epic says "2/3 features done, 1 to validate".

**Upgrade every machine first.** A binary from before features reports the ledger's chain as broken as soon as it holds a `feature` entry (`kontext verify` doesn't know the kind) and doesn't count validations. Install the new binary on every machine of the project before anyone runs `kontext feature add`. From this version on, a kind added later (such as `scope`) no longer breaks the chain: `verify` and `doctor` list those entries and say to upgrade.

### What each reader sees

- `resume --as pm`: one line per epic ("Payments: 3/5 closed, 1 claimed, 1 blocked; last activity 2 h ago"), what was escalated to the pm, what the user said, and the drift. It doesn't see tasks, facts from epics or the journal of tasks; it does see facts that aren't from an epic.
- `brief <epic>`: the coordinator, with the full board.
- `next --epic <epic>`: the implementer or tester, with a single task.
- `standup`: you, with the closed tasks and their evidence, the drift and the expired claims.
- `show <T>`: the task's state history (who claimed, released, closed or reopened it) and its journal.

### Agent files

`kontext install` also writes `.claude/agents/kontext-coordinator.md`, `kontext-implementer.md`, `kontext-tester.md` and `kontext-scout.md`. They don't set a model: whoever launches them chooses one. It also writes `kontext-pm-reader.md`, the reader of the PM's memory (below, "The PM's memory of your ideas"), the one that sets its model: `haiku`. If one already exists with different content (you changed it by hand), it is kept and you are warned; `kontext install --force` replaces it.

The skills and agent files also keep the conversation you talk to small, since every request re-reads it: bulk reading goes to a subagent, delegated agents hand back about five lines and put the detail in kontext, long output is trimmed, and at the end of a phase the PM tells you it is a good moment to `/compact`. These are instructions, not yet measured ([DESIGN.md §9.6](./DESIGN.md)).

### Team flow

```sh
# The pm (you, or your agent with --role pm), with the charter and goal G-13a signed:
kontext epic add "Card payments" --goal G-13a --name payments
kontext task add "Integrate the Stripe SDK" --epic payments --acceptance "pnpm test passes"
kontext task add "Charge webhooks" --epic payments --depends-on T-4c1 --acceptance "pnpm test:webhooks passes"
kontext task add "Payment screen" --epic payments --acceptance "pnpm e2e passes"
# Launch the kontext-coordinator subagent with the output of:
kontext brief payments

# The coordinator hands out the work: it claims each task on behalf of its implementer and pastes
# the output (with the assignment mark) into the brief of a kontext-implementer.
kontext --actor agent:impl-1 --role implementer next --epic payments --claim
kontext --actor agent:impl-2 --role implementer next --epic payments --claim

# Each implementer, with its identity as arguments (or in the environment it was launched with):
kontext --actor agent:impl-1 --role implementer log "SDK integrated, tests still missing" --task T-4c1
kontext --actor agent:impl-1 --role implementer ask "Do we charge in MXN or USD?" --task T-4c1   # goes to epic:payments
kontext --actor agent:impl-1 --role implementer done T-4c1 --run "pnpm test" --since omar@mac:41
# Exits with 3 if something new came in for the task: read it and retry with the mark it gives.

# The coordinator answers, and escalates only what it can't answer:
KONTEXT_ACTOR=agent:epic:payments KONTEXT_ROLE=coordinator kontext answer Q-9b2 "MXN (D-c3d)"
KONTEXT_ACTOR=agent:epic:payments KONTEXT_ROLE=coordinator kontext ask "Do refunds belong in this epic?" --to pm

kontext resume --as pm      # one line per epic
kontext standup             # what's closed, with its evidence
```

## Orchestrating several agents

kontext doesn't launch agents: the skill `kontext-orchestrate` (installed by `kontext install`, next to the `kontext` one) tells the agent how much structure to set up and how to do it with what its harness has. It is triggered by big tasks, projects from scratch, or "this is a lot of work". It measures the size (S–M, L, XL, XXL) with fixed criteria, sets up PM → one coordinator per epic → scouts, implementers and testers, shows you the structure and the cost (model per level and how many agents), and waits for your yes before launching anything XL or XXL. On a greenfield project it goes in waves, with a `standup` with you at each boundary. The details are in [DESIGN.md §12.1](./DESIGN.md).

| Command | What it does |
|---|---|
| `kontext epic add <title> --goal <G> --after <epic>…` | The epic goes after others: by id, `epic:<name>` or `<name>`, even if they don't exist yet (with a warning). Refuses (exit code 2) to go after itself, an id that doesn't exist, and anything that would close a cycle. Like a task's dependencies, it can't be changed later |
| `kontext plan [--json]` | Calculates the waves of the open epics: which run in series and which in parallel. An epic is closed when all its tasks (at least one) are closed, or, if it has features, when all its features are done and its tasks without a feature are closed; a closed dependency is already satisfied. Exits with 1 if there is a cycle or a dependency on an epic that doesn't exist or is no longer in force; those epics, and the ones waiting on them, show up as "no wave" with their reason |
| `kontext tree [--json]` | The whole project on one screen: the waves, each epic with its count (✓ closed, ◐ started, ○ not yet), the features of the open epics ("to validate", "waiting for you" when a question for a person cites the feature or one of its tasks) and, if `.pm/items.md` exists, your ideas that are not placed and how many of your messages no audit has seen. Tasks are not listed. Writes nothing |

```sh
# The map can be written in any order: integration warns that its dependencies are still missing.
kontext epic add "Integration and e2e tests" --goal G-81c --name integration --after payments --after catalog --after notices
kontext epic add "Foundations" --goal G-81c --name foundations
kontext epic add "Payments" --goal G-81c --name payments --after foundations
kontext epic add "Catalog" --goal G-81c --name catalog --after foundations
kontext epic add "Notices" --goal G-81c --name notices --after foundations
kontext plan
#   # Wave plan: 5 open epics, 0 closed
#
#   ## Wave 1 (in series)
#   - E-98518c foundations: Foundations (no tasks)
#
#   ## Wave 2 (in parallel: 3 epics, one coordinator each)
#   - E-163a45 payments: Payments (no tasks; after foundations)
#   - E-858797 catalog: Catalog (no tasks; after foundations)
#   - E-d83a67 notices: Notices (no tasks; after foundations)
#
#   ## Wave 3 (in series)
#   - E-0738bf integration: Integration and e2e tests (no tasks; after payments, catalog, notices)
kontext epic add "Base" --goal G-81c --name base --after integration --after ghost
kontext plan        # exits with 1: base is left with no wave, and Problems says it goes after 'ghost', which doesn't exist
```

The agent files follow the wave protocol: the coordinator doesn't talk to the user (it escalates with `kontext ask --to pm`), it closes the epic once all its tasks are closed with evidence, and it tells the pm with a `kontext log` and its report; implementers and testers only ask their coordinator.

## The PM's memory of your ideas (`.pm/`)

Over days of work you say many things to the session that coordinates (the PM), some in passing, many as short answers to its questions. The skill `kontext-pm` keeps them so none is silently lost, in a `.pm/` folder at the root of the repo:

- `dumps/<date>.md`: what you said, verbatim and append-only (a long day goes on in `<date>-2.md`, `-3`…, so each file can be read whole). An answer is saved with the questions it answers, or the proposal it replies to (`**Coordinator asked:**`), so "2. merge it yourself" or "go" still means something a week later. Secrets become `[secret removed]`; nothing else is changed.
- `items.md`: one row per idea, with where it went (the kontext task, decision, PR or doc) and its evidence. It points at the work; the plans and tasks stay in kontext.
- `status.md`: where things stand, short. `shots/`: screenshots used as evidence.

Only the PM writes it (and the capture hook below, if it is on), and after each message it lists back the ideas it took and asks whether it missed one. When you come back, or ask where things stand, it runs a coverage audit without reading the dumps itself: the `kontext-pm-reader` subagent (Haiku, read-only) lists every idea in the dumps and reports those missing from `items.md`; the PM checks each one before adding it, and shows you what is done, in progress, waiting for you, pending and newly found. In a public repo it asks before committing `.pm/` (or gitignores it).

### Capture: your messages without the agent remembering them

Without help, the dumps depend on the PM remembering to save each message. With the capture hook, kontext writes them itself, without a model:

```toml
# project.toml
[capture]
enabled = true                          # off by default; run `kontext install` again after turning it on
# skip_prefixes = ["<my-tool-output>"]  # prompts that start like this are machine text
# worktrees = false                     # true: in every linked git worktree (its own .pm/)
```

- Each message you type to the PM session goes into today's dump (`.pm/dumps/<YYYY-MM-DD>.md`, local date, `-2`, `-3`… past 60 KB) the moment you send it, in the skill's format, with the lines of the coordinator's last message that ask something (those with `?`, the last 20 within 2,000 characters) under `**Coordinator asked:**`. If that message asked nothing with a question mark ("Say go and I merge it"), its last paragraph goes there instead (code blocks, a trailing heading or rule skipped; at most 600 characters, whole lines from the end), so a "go" still says what it approved. `kontext ideas` and the report never show those quoted lines as your words. Answers you give through the question tool (`AskUserQuestion`) are added at the end of the turn, numbered with their questions.
- Only the `pm` role, never a subagent. Machine text is skipped: task notifications, system reminders, cross-session and agent messages, local command output and its caveat, `[Request interrupted by user`, delegate reports (`Delegated task node:`) and pull-request updates (`Update on pull request #`) that a harness posts as user turns, a message from another Claude session (`Another Claude session sent a message:`), a bare slash command, an empty prompt, and whatever starts with a prefix in `skip_prefixes`. When in doubt it is written; `hooks.log` says which rule skipped a prompt, never its text. A prompt an agent schedules or sends from another thread arrives as a plain user turn with no fixed wrapper: if those reach the PM's own conversation, put their opening words in `skip_prefixes`.
- Nothing said before capture was on is written. Secrets become `[secret removed]`: those the distill removes, plus, for the capture only, vendor-prefixed tokens (`abc_` and 20 or more letters and digits with a digit) and UUIDs (`abc_<uuid>`), 32-character hex strings with a letter and a digit (not a 40-character git SHA or a 64-character hash) and Slack tokens (`xoxb-…`). When it creates `.pm/dumps/` it also writes `.pm/.gitignore` with `dumps/`, if there is none; it never touches the repo's `.gitignore`.
- The PM no longer appends what the hook wrote: it checks today's dump with one Grep and appends a message only if it is not there.
- It prints nothing and always exits with 0; each run is one line in `<kontext-home>/.kontext/hooks.log`. Its marks live in `<kontext-home>/.kontext/capture/` (local; deleted after 30 days unused).
- **Name the PM's conversation.** The rules above go by role and folder, so two conversations in one checkout (yours with the PM, and a coordinator's whose "user" turns are relays and reports from other threads) look the same. Run `kontext capture here` in the conversation where you talk to the PM (from its Bash tool: it reads `CLAUDE_CODE_SESSION_ID`, which Claude Code sets there; outside Claude Code, `--session <id>`). Once at least one conversation is named, only named ones are captured, still only as `pm` and in a checkout that captures; with none named, everything works as above. The list is for this project on this machine only (`<kontext-home>/.kontext/capture-sessions.json`, not synced, not in `.kontext.toml` or `project.toml`). The id changes on `/clear`: name the conversation again. After `claude --continue`, or `--resume` without an id, that variable may hold another id than the one the hooks send: if your own messages log "not one of the N named", name the `session=` of those log lines with `kontext capture here --session <id>`. `kontext capture list` shows them, `kontext capture forget <id>` or `--all` drops them (with none left, every pm conversation in a capturing checkout is captured again). The commands work with the capture off: name the conversation, check `kontext doctor`, then turn it on. A conversation that is not named logs "not one of the N named on this machine".
- Linked git worktrees (`git worktree add`) are skipped by default: agents delegated into their own worktree have no `KONTEXT_ROLE` and would otherwise count as the PM, and dumps there go when the worktree is removed. The exception is the worktree marked as the PM's (`kontext install --link-only` in it), which writes into the main checkout's `.pm/` (see "Working in git worktrees"). With `worktrees = true` every linked worktree writes into its own `.pm/`.

`kontext install` installs the skill and the reader. `kontext install --skills-only` brings only the skill; in a repo that must not get the start hook, copy the two files by hand:

```sh
mkdir -p .claude/skills/kontext-pm .claude/agents
cp <kontext>/templates/skill-kontext-pm.md .claude/skills/kontext-pm/SKILL.md
cp <kontext>/templates/agents/kontext-pm-reader.md .claude/agents/
```

Why a Haiku reader: in the [reader benchmark](./bench/reports/2026-11-06-reader.md) (one fictional project), with its prompt v2, which these files carry adapted to the layout above, Opus handing the reading to Haiku found every missing idea, as Opus reading by itself did, at about a fifth of the cost; a higher effort added nothing. `python3 bench/reader.py --prompt skill` measures the texts that ship. The details are in [DESIGN.md §12.3](./DESIGN.md).

## Phase 4 commands

| Command | What it does |
|---|---|
| `kontext fact <text> --path <path>… [--repo <name>]` | Saves the fact with a fingerprint per path: `{repo, path, commit, blob}` of the last commit. The paths are resolved against the git repo of the current folder (or against `--repo`, relative to its root) and have to be in a commit. If there is already a fact in force with the same title (ignoring case, accents and punctuation) or the same fingerprints, it saves a confirmation (`confirm`) instead of another fact |
| `kontext find <topic…> [--all] [--limit N] [--json]` | Searches the docs of the project's repos that are on this machine and says which section to read, with its status (in force, proposed, historical), the code paths it cites and who cites it (`git grep`). Historical material (replaced ADRs, paths under `historical`) only shows with `--all` |
| `kontext find --show <path:line>` | Prints a section (up to 200 lines). With several repos: `<repo>:<path>:<line>` |
| `kontext find --check [--baseline <file> [--update-baseline]]` | Docs in force that cite repo paths that no longer exist. With a baseline (`<n> <doc>` per line) it only fails if a doc has more broken paths than before |
| `kontext distill --transcript <path> [--dry-run] [--max-chunks N] [--chunk-chars N]` | Distills a Claude Code transcript: only what's new since last time, without secrets, in chunks, with the model from `[distill] command`. It notes what the human said, decided or asked (not pasted documents or tool output), with its origin and date, as proposals, questions for the owner, facts without a fingerprint, and journal entries from `agent:distill`; if something updates a fact or a proposal in force, it replaces it. At most `[distill] max_entries` (8) per run |
| `kontext changelog [--since <date\|ref>] [--until <date\|ref>] [--format md\|json]` | Technical changelog: the tasks closed in the range by epic, with their evidence (PRs first), and the merged PRs of each GitHub repo in `[[repos]]` (with `gh`), grouped by type (`feat`, `fix`, `perf`, `refactor`, `docs`…; the rest under "other"). The same input gives the same bytes |
| `kontext distill --wait [--timeout 600s]` | Doesn't distill: waits for the distills started by the hooks to finish (each leaves a mark in `.kontext/distill-pending/` while it runs). Exits with 1 if it times out. Useful for scripts that need to know the ledger already has the previous session's content, like the benchmark |
| `kontext hook pre-compact` / `kontext hook session-end` | Called by Claude Code: they start `kontext distill` in the background with the transcript and return right away (`session-end` also runs the capture, if it is on) |
| `kontext hook user-prompt-submit` / `kontext hook stop` | Called by Claude Code with `[capture] enabled = true`: they write your messages into `.pm/dumps/` and print nothing |
| `kontext measure <transcript>… [--since <RFC3339>] [--until <RFC3339>]` | Numbers about Claude Code conversations, from their transcripts: requests, tokens, the history each request re-reads, compactions, models, what fills the history, tools, and which `kontext` commands the agents ran. Prints numbers and names only, never text. Needs no project and no kontext configuration |

### Facts that expire

"Expired" isn't written anywhere: it is calculated when you read. For each fingerprint, the local checkout of the repo is found and `git rev-parse HEAD:<path>` is compared with the stored blob:

- same: the fact is still in force;
- different, or the path no longer exists: **expired** ("F-x expired: src/invoice.ts changed in 9b2a1c3; the next scout reads it again"). It shows up as an alert in `standup` and in the `brief` of its epic (or of all of them, if it has no epic), and it no longer appears in the fact lists of `brief`, `next` or `resume`;
- the repo isn't on this machine: **unreviewed** (unknown; not expired). It shows up in the lists as `[unreviewed]`.

When a scout writes a fact again with the same title as one that expired, the new one replaces it. The blob of each path at each `HEAD` is stored in the index, so the views don't run more git than they need to. The pm still doesn't see facts.

Where each repo is: first `projects.toml`, then the repo in the current folder, if its remote (or its folder name) matches one of the `[[repos]]`:

```toml
# ~/.config/kontext/projects.toml
[projects.billing]
home = "/home/you/kontext-home/billing"

[projects.billing.repos]
shop = "/home/you/code/shop"
```

### Docs that `find` reads

Each repo in `[[repos]]` of `project.toml` says which docs are read; everything is optional:

```toml
[[repos]]
name = "shop"
remote = "github.com/acme/shop"
docs = ["docs/**/*.md", "**/README.md"]       # default ["**/*.md"]
exclude = ["docs/templates/**"]                # in addition to node_modules, target and .git
historical = ["docs/archive/**", "**/*-retro.md"]   # historical by path
roots = ["docs", "scripts", "src"]             # which folders a cited path starts with; by default, the top-level ones git tracks
```

`.gitignore` is respected. A doc's status comes from its `**Status:**` line (`Accepted`, `Superseded by …`, `Proposed`, `draft`…; the first word decides), and an ADR (a file `ADR-NNNN-…`) that another ADR in force says it replaces ("Supersedes: ADR-0003", "Reemplaza a ADR-0031") is historical even if it says `Accepted`. Sections are stored in the index, and a file is only read again if its date or size changed. The ranking is BM25 per field (headings, "Read when", path and body; accents ignored and light plurals; at most two sections per file). [examples/find-repo.toml](./examples/find-repo.toml) is a complete configuration for a fictitious web shop; the tests run it against the docs in `tests/fixtures/find-docs`.

#### Synonyms

`<kontext-home>/synonyms.toml`: groups of terms that are searched as one. A query with any of them also finds the others, with less weight (0.7). They are normalized the same way as the docs (no accents, singular, and `real-time` counts as `real` and `time`).

```toml
groups = [
  ["inventory", "stock", "warehouse"],
  ["payment", "billing", "charge", "stripe"],
  ["deploy", "release", "rollout"],
]
```

[examples/find-synonyms.toml](./examples/find-synonyms.toml) is a short set for that web shop.

### Distill

In `project.toml`:

```toml
[distill]
command = "claude -p --model haiku --output-format json"   # receives the prompt on stdin
enabled = true
timeout = "240s"        # per call; if exceeded, the command's process group is killed
# model = "haiku"       # for the actor; if missing, taken from --model in command
# max_entries = 8       # entries per run, at most
# max_message_chars = 4000   # a longer message (a pasted document) is cut; 0: no cut
```

- The command runs with `sh -c` in an empty temporary folder, with the prompt on stdin and `KONTEXT_DISTILL_DISABLE=1` (so that session isn't distilled again). If its output is the JSON envelope of `claude --output-format json`, `.result` is taken; otherwise the output as is. The JSON object is pulled out of the response even if it comes wrapped in ``` or with text around it. If a chunk fails, it is retried once.
- Before the text is sent, secrets are removed: PEM keys, GitHub tokens, `sk-…`, AWS keys, `Bearer …`, `password=`/`token:`/`api_key=`… and long strings with upper and lower case, digits and high entropy (not UUIDs, hashes or slugs). `--dry-run` shows how many it removed and the first chunk, without calling the model.
- What is read from each transcript is stored in `.kontext/distill-offsets.json` (under `.kontext/distill.lock`): running it again with nothing new does nothing, and if a chunk fails, the next attempt starts there.
- The prompt carries the project name, its owners and what is in force with its id (decisions, pending proposals, facts and open questions). It asks only for what the human said, decided or asked in their messages, or what the assistant concluded and the human accepted; not pasted documents, logs or tool output, unless the human says it applies. Each entry carries `fields.origin` (`user`, `agent`, `document`) and `fields.said_at`. Decisions come out as proposals (`[unreviewed]` in `standup`), never as decisions; questions go to the owner; facts have no fingerprint. Titles that already exist are not repeated.
- If something updates an entry in force, the model gives its id and the new entry carries `fields.supersedes`: a fact or a pending proposal from the distill is replaced on the spot (views only show what is in force; `show` and `recall` keep the history). A decision in force is not replaced: what contradicts it stays as a proposal for an owner.
- `kontext install` adds the `PreCompact` and `SessionEnd` hooks if the distill is on. They start `kontext distill` in the background (its output goes to `.kontext/distill.log`) and return right away. With `enabled = false` or the `KONTEXT_DISTILL_DISABLE` variable, they do nothing.

### Changelog and `gh`

PRs come from `gh pr list --repo <owner/repo> --state merged --search "merged:>=<date>" --json number,title,url,mergedAt,author` for each repo in `[[repos]]` with a GitHub remote. `--since` and `--until` accept a date (`2026-11-01`; for `--until` it covers the whole day), an ISO-8601 date or a git ref (`v1.2`, `main~20`: its commit date in the repo of the current folder). If `gh` is missing or fails, the changelog comes out with the tasks and a note.

### Measuring conversations

`kontext measure` reads each transcript a line at a time (they reach hundreds of MB) and gives one combined report, plus one line per file with several. With `--json`, the same data as one object. The rules:

- Lines of subagents (`isSidechain`) are skipped everywhere. A malformed line is counted and skipped.
- A request is one distinct `message.id` among the `assistant` lines with `message.usage`: Claude Code writes one line per content block with the same usage, so the id counts once and the usage of its last line wins. Ids are distinct per file.
- Tokens are summed over the requests (`input_tokens`, `cache_read_input_tokens`, `cache_creation_input_tokens`, `output_tokens`). The history re-read by a request is its `cache_read_input_tokens`: the report gives the average (integer division) and the maximum. No prices: tokens only.
- What fills the history, in characters: tool results (a string content by its length; a list by the lengths of its `text` blocks, its other blocks, such as images and tool references, count 0 and are reported apart), tool calls (the compact JSON of each `tool_use` input), assistant text, and the text of the `user` lines, split with the capture hook's rule into typed by the human, reports handed back by delegated agents (`Delegated task node:`), pull-request updates (`Update on pull request #`) and other machine text (the other machine prefixes, a bare slash command, and `isMeta` lines); compaction summaries are the `isCompactSummary` lines. Reasoning blocks are not counted.
- Tools: calls per tool name, and result characters per tool name (each result matched to its call by `tool_use_id`); the top 10 of each, all of them with `--json`.
- `kontext` commands: for each `Bash` call, each simple command (split on `&&`, `||`, `;`, `|`, `&` and newlines outside quotes) that runs `kontext`, after any `VAR=value` and the global options (`--actor`, `--project`, `--json`…), counts under its subcommand, or `other` if the next word is not one; the list comes from the CLI itself. The characters of the call's result go to the last `kontext` command in it.
- `--since` and `--until` (inclusive) go by each line's `timestamp`; a line without one takes the previous line's. A request belongs to the time of the first line with its id. The report says the first and last time it actually covered.

### More on hooks

- A coordinator with `KONTEXT_ROLE=coordinator` and `KONTEXT_EPIC=<epic>` gets its epic's `brief` at startup (without advancing its cursor). Without `KONTEXT_EPIC`, it gets the usual output.
- `next` has a budget like `brief` (`--max-chars`, 9,000 by default): it first drops the reasons behind decisions, then the facts, and then shortens the lists. The task, its acceptance, the mark and how to close it are never cut.

## Phase 5 commands

| Command | What it does |
|---|---|
| `kontext clone <git-url> [--home <path>] [--name <name>]` | Brings in an existing `kontext-home` (default `~/kontext-home/<repo name>`), registers it in `projects.toml`, sets this machine's writer if none was set, builds the index and prints the charter and the owners |
| `kontext sync [--include-config]` | Commits your own changes (`ledger/` and `cursors/`, with the message `kontext: <writer> <N> entries`), fetches the others' changes (`fetch` and `rebase`) and pushes (`push -u`). Then it says how many entries arrived from each writer, which new alerts they caused, and whether the chain of what arrived verifies. Without a remote, it only commits |
| `kontext status` | Project, writer, remote, last sync, own entries not committed and not pushed, index, chains, alerts and signatures. `--json` for the UI |
| `kontext doctor` | Checks the installation and says what is wrong and which command fixes it: this binary, the `kontext` on `PATH`, the config and the project, the kontext-home, the hooks in every settings file, the capture, the skills and agent files, and the hook log. Each line is `ok`, a `note` or a `problem` with its fix; exits with 1 if there is any problem. Writes nothing, and works on a broken setup (no `projects.toml`, no project, a missing binary) |
| `kontext verify` | Besides the chains, checks any signatures there are (and, with `require_signatures`, that the required ones are present). Exits with 1 if any doesn't verify |
| `--sign` | Global option: signs even if `projects.toml` has no `[signing] key` (uses `~/.ssh/id_ed25519.pub`) |

### Doctor

`kontext doctor` runs every check on its own, so one failure does not hide the rest, and it never opens the project the usual way (that creates the index). What it looks at:

- **This binary**: its real path. In a cargo build directory (`target/debug`, `target/release`) it is a note, and a problem if a hook runs it.
- **`kontext` on `PATH`**: agents run `kontext` by name, hooks by absolute path. A problem if there is none, or if it is a different file from the one the hooks run.
- **Config and project**: `projects.toml`, the writer, which project the current folder resolves to and how (`--project`, `KONTEXT_PROJECT`, `.kontext.toml`, the main checkout's `.kontext.toml` from a linked worktree, a tracked one, or the only one), and the role sessions here start with, and from where. In a linked worktree not marked as the PM's whose role comes out `pm` (not from `KONTEXT_ROLE`), that line is a note: every worktree session starts as the PM; the fix is `kontext install --link-only` in the PM's worktree, then `kontext install --worktree-role agent` in the main checkout. Without a project, the project checks are skipped and it exits with 1.
- **kontext-home**: it exists and is a git repo, `project.toml` parses, the ledger and this writer's file, the remote, the chains (as `kontext verify`), and own entries not committed or not pushed (a note: `kontext sync`).
- **Hooks**: every `kontext hook <event>` in the repo's `.claude/settings.json` and `settings.local.json` and in your user's settings (in a linked worktree, also the main checkout's `settings.local.json`), with the file it is in and the binary's full path: it must exist, be executable and be the same file everywhere. A problem without a `SessionStart` hook or without the `Bash(kontext:*)` permission. Notes: kontext's hooks in both settings files of a checkout (a copy that differs runs as well); in a linked worktree, or in a main checkout that has linked worktrees, hooks only in the main checkout's `settings.json` when git does not track it (worktree sessions read `settings.json` from their own folder; the fix is `kontext install --local` there).
- **Capture**: `[capture] enabled = true` without its hooks is a problem; its hooks with the capture off, a note. It says whether this checkout captures and why (a main checkout; a worktree marked as the PM's; `worktrees = true`; another role), with a note for an unmarked worktree, and prints the `.pm/` path in use (`.pm/ here: …`). It says how many conversations are named (`kontext capture here`) and, inside Claude Code, whether this one is; a note when the capture is on, this checkout captures as `pm`, some are named and this one is not.
- **Skills and agent files**: missing or from another version (as `kontext install --dry-run` would see them), a note. In a linked worktree with no `.claude/skills` (or `.claude/agents`) of its own, the main checkout's count, as Claude Code loads them there; then your user's (`~/.claude/skills`, `~/.claude/agents`, or under `$CLAUDE_CONFIG_DIR`).
- **Hook log**: in `.kontext/hooks.log`, how many lines carry an error and when the last one was (a note).

## Phase 6 commands

| Command | What it does |
|---|---|
| `kontext ui [--port 4700] [--host 127.0.0.1] [--open] [--as <reader>]` | Serves a local read-only page with the ledger, the cursors, the alerts and the epic boards. It refreshes itself every 5 s. `--port 0` takes a free port; `--open` opens it in the browser; `--as` says who is looking (for "Signed by you" and "For you"; by default, as in `standup`) |

### The page

- **The ledger**, like an accounting book: newest to oldest, with `#` (the position in its writer's file), the fingerprint and the previous one (`← prev`), the type (a colored box: charter, decision, question, fact, task, journal; empty if it's an unreviewed proposal), the entry with a note ("Serves G-x · epic y", "Fingerprint: path at commit", "Closed with evidence: …") and who wrote it. Anything that needs attention is in red ("Serves no goal: possible drift", "Expired: …"); what was replaced, rejected or expired is struck through. A signed charter carries a "Signed by you" stamp (or its owner's). Clicking an entry opens its detail (the same as `kontext show`).
- **The cursors**: for each reader with a cursor, a dotted line "Read up to here by <reader>" under the oldest entry it hasn't read yet, and a bar in its color on the right-hand channel from that line to the newest: what it still has to read. With several writers, the line goes at the first entry (in ledger order) that its cursor doesn't cover.
- **Readers**: for you, what is waiting for your answer (as in `standup`); for the pm, one line per epic and what it is being asked; for each coordinator, its epic's board in numbers.
- **Alerts**: drift, expired facts, conflicts, lost or expired claims, invalid signatures and what doesn't count.
- **Direction**: the charter, the goals, what is not being done, and the decisions in force. **Epics**: each one in columns: ready, blocked, assigned, claimed and closed.

Nothing on the page writes: there are no forms or buttons that change anything. It doesn't call the internet (the CSS and JS are inside the binary) and works in light and dark mode depending on the system.

### API

| Route | What it returns |
|---|---|
| `GET /` | The page |
| `GET /api/state[?before=<hlc>&limit=<n>]` | Everything the page draws: project, `status` (what `kontext status --json` gives), writers, charter, goals, decisions, epics with their counts and tasks, tasks, questions, proposals, facts with their status, alerts, readers with their cursor, and the ledger from newest to oldest in pages of `limit` (100) entries with an `hlc` lower than `before`. `ledger.next_before` is the `before` of the next page |
| `GET /api/entry/<id>` | What `kontext show --json` gives, plus its row; accepts a unique prefix. 404 if it doesn't exist |

### Security

It listens on `127.0.0.1` by default and has no password. With a loopback address it only answers requests whose `Host` is `127.0.0.1`, `localhost` or `[::1]` with its port: this way a page from the internet can't read your ledger through your browser (DNS rebinding). With `--host` set to another address (`0.0.0.0`, your network's IP) it warns on stderr: anyone who can reach that address can read the ledger. It only accepts `GET` and `HEAD`.

## The human as a reader

The human also loses the thread. The rule: **if you haven't answered, you haven't read anything.** Seeing something doesn't count; only acting on it does. The details are in [DESIGN.md §20](./DESIGN.md).

```sh
examples/report.sh /tmp/kontext-report     # a sample project with pending items, problems, screenshots and ideas
```

| Command | What it does |
|---|---|
| `kontext inbox [--all] [--as <human>]` | Your pending items, by what they block and then by age: **sign** (charters, goals, proposals and rules of yours that are unsigned), **answer** (questions for you, grouped by epic), **review** (what the distill noted) and **problems** (closes that didn't count, reopened tasks, expired or lost claims, drift, invalid signatures, conflicts, epics with no wave). The first 5 in full, and "and N more". Each one says what it blocks, who is waiting, since when, and the command that resolves it |
| `kontext inbox --everyone [--all]` | Everyone's inbox, for viewing only: one section per human (the owners in their order and then, by name, anyone with questions via `--to human:<x>` or with someone's name from `[people]`) with what is theirs alone (the questions for them), and a **Common** section at the end with what any owner resolves (sign, review, questions to `owner`) and the problems, shown once. Each section, the first 5 and "and N more" (or all with `--all`). With `--json`, `humans` and `common`. Not used with `--as`, `sign`, `answer`, `dismiss` or `confirm` |
| `kontext inbox count` | One line: "4 pending items (1 new) · oldest, from 3 days ago · the first blocks Payments". With `--everyone`, one per human and one for the common items |
| `kontext inbox sign\|answer\|dismiss\|confirm <n or id>` | Act from the inbox: sign (or accept a proposal), answer (in a group, `2.1`, `2.2`…), take it out unresolved (`--why`) or confirm what the distill noted. Only a human |
| `kontext report [--html] [-o file] [--as <human>]` | The report: the status paragraph with a traffic light, what is yours to do, the problems, your ideas from `.pm/items.md` and where each one went (only when there is one; done and decided ones included, with their evidence), the most recent screenshots per epic (with the before and after) and a bar per goal. Finished tasks are not listed. With `--html`, a single-file page with the screenshots inside (with a sidebar of sections and epics that filters by epic; copies the commands with one click, compares before and after and enlarges screenshots; works offline); also in `kontext ui` as `/report` |
| `kontext capture here [--session <id>]` \| `list` \| `forget <id> \| --all` | Which conversations the capture hook writes from, for this project on this machine: once one is named, only named ones (see "Capture"). `here` names the current one (`CLAUDE_CODE_SESSION_ID`) and says why nothing will be captured from it yet, if so; works with the capture off. `--json` |
| `kontext ideas [--json] [--mark-audited]` | Your ideas from `.pm/items.md` (skill `kontext-pm`) grouped by state: no destination, waiting for you, pending, in progress, decided, done and discarded. The state comes from the ledger through the ids in the `where` column (also the old v0 ids such as `D-0030`): an open question or pending proposal waits for you, and tasks and epics count by their tasks (an epic with features is done only when `kontext plan` closes it), a feature by its own state (a superseded one by its replacement); otherwise the table's state stands. "Not placed: N" lists the ideas whose `where` cites no feature, task or decision that counts (a superseded one counts if its replacement does; one that does not count, or was rejected, does not place it) and that you did not discard, with what they do cite ("Payments, no feature"); "N of your messages not audited since <date>" counts the dump entries after the last `--mark-audited`, which the PM runs right before a coverage audit (it writes `.pm/audited.json` in the same `.pm/` that `kontext ideas` reads, and is refused with exit 2, saying where it looked, if it finds no `.pm/items.md`). Each with the date you said it, where it went and its evidence; a table without a `where` column says so (every idea then shows as no destination). The report shows the same as "Your ideas" |
| `kontext log --status "<2 or 3 sentences>"` | The "where we are" paragraph that the report takes (the latest one counts) |
| `kontext done … --evidence screenshot.png [--screen <screen>]` | A screenshot as evidence (PNG, JPG or WebP, 2 MB at most: `[media] max_size`). It is saved in `media/<sha256>.<ext>` of the kontext-home, travels with `kontext sync`, and `kontext verify` checks its hash. The same screen in two closes gives the before and after |
| `kontext decide\|propose … --attach <file> [--screen <screen>]` | An attachment (image or `.html`): the approved mockup from phase 0, as a reference |
| `kontext ask … [--blocking] [--epic <x>] [--no-merge]` | If an identical open question already exists (same recipient, same epic, similar text), it is added to that one instead of repeated. If the human has `[inbox] max_pending` pending items (5), an agent's question is marked not urgent unless `--blocking` |

- **Nothing is closed by viewing it.** A pending item leaves the inbox when its cause is resolved in the ledger or when you dismiss or confirm it. Neither `kontext inbox` (nor `--everyone`), nor `standup`, the report or the hook moves anything.
- **What's new** is counted from the last thing you wrote to the ledger, not from your last view: the "(new)" in `standup` too. The `standup` cursor still advances, but only so the history isn't repeated.
- **The hook** for the pm's session adds a line at the top with your pending items, or nothing if there are none.
- **The skill `kontext-show`** tells the agent when to show you the report (wave boundaries, "how are we doing?", when coming back after days) and when not to (after every task), how to do it in T3 Code, Claude Code or the terminal, and not to pile questions on you if you don't answer.

```toml
# project.toml
[inbox]
max_pending = 5        # with this many pending items, an agent's `ask` is marked not urgent
[media]
max_size = "2MB"       # cap for each screenshot
```

## Benchmark

`bench/` has the harness that measures how much a project drifts with and without kontext (DESIGN §15): scripted scenarios of several days, four variants (nothing, with `CLAUDE.md`, with `kontext` in `pointer` and in `inject` mode), several seeds, and a table with drift, failing guards, memory, corrections, tokens and whether it finished. It is Python 3.11+ with no dependencies:

```sh
python3 bench/run.py --dry-run --seeds 2      # dry run: a fake agent, no model and no cost
python3 -m unittest discover bench/tests      # the harness tests
```

**A real run spends tokens** (`run.py` asks for `--yes`). How to run it, what each column measures, and the drift formula, fixed before running: [bench/README.md](./bench/README.md).

## Working with other people

The `kontext-home` is a git repo. Each writer (`<user>@<machine>`) has its own file in `ledger/` and its own cursors in `cursors/<reader>/<writer>.json`, so combining what several machines wrote never conflicts: `kontext sync` only adds files and lines.

```sh
# Whoever starts the project
kontext init billing --remote git@github.com:omar/billing-kontext.git
kontext sync                      # the first sync uploads everything and sets the upstream

# Each other person, on their machine
kontext clone git@github.com:omar/billing-kontext.git
#   Project billing cloned into /home/ana/kontext-home/billing-kontext
#   Writer of this machine: ana@laptop
#   Charter: Charge by card
#   Owners: omar

# When starting and ending the day (or whenever you want to see what the others did)
kontext sync
kontext standup
```

- **What `sync` uploads:** only `ledger/` and `cursors/`. If you changed `project.toml` or `synonyms.toml`, it warns and doesn't upload them; `kontext sync --include-config` uploads them. During the commit and the rebase it holds the machine's writer locks, so an agent writing at the same time waits a few milliseconds and never pushes a half-written line.
- **If git conflicts:** with the ledger this shouldn't happen. If it does (for example, two people changed `project.toml` with `--include-config`), `sync` aborts the rebase, leaves the `kontext-home` as it was (with your local commit, not pushed), exits with 2 and explains what conflicted. It never resolves this on its own: fix it by hand with `git -C <kontext-home> pull --rebase`.
- **After pulling:** the state looks the same on every machine. If two `claim`s from different machines took the same task, the one with the lower `hlc` wins, and whoever lost gets an alert ("T-x was claimed first by agent:impl-1 (on omar@mac); agent:impl-2: stop working on it and get another with `kontext next`") in `standup`, in the epic's `brief`, and when it asks for work with `kontext next`. Two owners who replace the same goal or decision, or two signed charters, show up as a conflict in `standup` until someone resolves it.
- **Cursors:** if you read `standup` on one machine and then sync, the other machine no longer shows what you read as new: the maximum per file across all your machines' cursors is used.

### Who is who: `[people]`

In `project.toml`:

```toml
owners = ["omar"]
require_signatures = false

[people]
omar = { emails = ["omar@example.com"], keys = ["ssh-ed25519 AAAAC3Nz… omar@mac"] }
ana = { emails = ["ana@example.com"], keys = ["ssh-ed25519 AAAAC3Nz… ana@laptop"] }
```

- A machine's human name comes from `git config user.email` (as seen from the `kontext-home`) mapped through `[people]`; if it isn't there, from the first name in `git config user.name`. It is the default reader for `standup`.
- If you write as `human:<name>` (`--actor` or `KONTEXT_ACTOR`) and that machine's git email isn't that person's, `kontext` warns on stderr (it doesn't reject). Without `[people]`, or with `require_signatures`, it doesn't warn: in the second case, what counts is the signature.

### Signatures

Each machine says which key it signs with, in `~/.config/kontext/projects.toml`:

```toml
[signing]
key = "~/.ssh/id_ed25519.pub"     # ssh-keygen finds the private key next to it, or in the agent
```

With a key, `sign`, `decide`, `accept`, `reject` and `supersede` from a human (or from the pm with `--approved-by`, on behalf of that owner) are signed: `ssh-keygen -Y sign -n kontext` over the entry's `hash`, and the signature goes in `sig` (which is not part of the `hash` or the `id`). It is verified with `ssh-keygen -Y verify` against the keys in `[people]`; the result is stored in the index, because each verification is a process.

With `require_signatures = true`, those entries only count if the signature verifies with a key of the person who signed (and that person is an owner). Those that don't are kept, don't count, and show up as an alert ("invalid or missing signature"). So as not to write something that wouldn't count, `kontext` refuses (exit code 2) if the machine has no key or if its key doesn't belong to that person in `[people]`. Imported v0 data doesn't need a signature.

## Exit codes

`0` success; `1` error; `2` rejected (a rule isn't met and nothing was written; in `sync`, git conflicted or the local state clashes with what arrived, and nothing was pushed); `3` stale context in `done`.

## Development

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
python3 -m unittest discover bench/tests   # the dry-run harness; needs target/debug/kontext (cargo build)
```

CI: `.github/workflows/ci.yml` runs the first three (fmt, clippy with warnings as errors, and the tests) on Linux with Rust 1.98.1, on every pull request whatever its base branch and on every push to `main`. The harness is not part of it.

The `kontext ui` tests start the server on a free port against the project from `examples/demo.sh` (they need `bash` and `git`). The harness tests don't run from `cargo test`: they are skipped if they can't find the `kontext` binary.

The golden-query test for `find` copies the docs of a fictitious web shop (`tests/fixtures/find-docs`) into a temporary git repo, configures it with `examples/find-repo.toml` and `examples/find-synonyms.toml`, and checks the top results and their status (in force, proposed or historical) for a handful of queries, plus `--check`. The distill tests use a fake model (a shell script); they never call a real model.

The `sync` tests use a "bare" repo in a temporary folder as the remote, a separate git config for each "machine" (`GIT_CONFIG_GLOBAL`), and disposable SSH keys (`ssh-keygen -N ""` in the temporary folder); they never touch a real remote or `~/.ssh`.

The import test runs a real v0 `ctx.py` only if `KONTEXT_V0_SCRIPT` points to one; if the variable is unset, the test is skipped, and the one that uses `tests/fixtures/v0`, generated with that same script, remains.

## License

MIT. See [LICENSE](./LICENSE).
