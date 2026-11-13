---
name: kontext-migrate
description: Installs or configures kontext in a project and migrates the memory that already exists into it. Use it when the user asks to install, configure, or start using kontext; to migrate the project's memory to kontext; to move from ctx.py or from the rules and decisions in CLAUDE.md, AGENTS.md, or Claude Code's automatic memory to kontext. It works even if kontext is not installed yet.
---

# Installing kontext and migrating the project's memory

You are going to leave the project with kontext working and with the memory that already exists inside it, without losing anything and without deciding for the human. Do the steps in order. Until step 3 you write nothing.

Rules for all the work:

- **Never write to a destination without first saying which path it goes to.** Wherever there is a `--dry-run`, run it first and show what comes out.
- **Agents do not decide.** Whatever you find as a decision or a rule comes in as a proposal; the human signs it from `kontext inbox`.
- **Do not delete or edit the source files** (CLAUDE.md, AGENTS.md, notes, the automatic memory, the `ctx.py` ledger). kontext reads them; it does not replace them.
- Invoke `kontext` as a bare command, with the identity in the arguments (`kontext --actor agent:kontext-migrate …`), as the `kontext` skill says.

## 1. Diagnosis (read-only)

Find this out and report it to the human before writing anything:

1. **The binary.** `kontext --version`.
2. **A v0 `ctx.py` ledger:** a folder with `ledger/entries.jsonl`, with or without a `ctx.py` next to it. Look in the repo (`git ls-files | grep -E 'ctx\.py|ledger/entries\.jsonl'`), in `$CTX_HOME` if it is set, and in neighboring repos: `find ~ -maxdepth 4 -path '*/ledger/entries.jsonl' -not -path '*/node_modules/*'` (a kontext-home has `ledger/<writer>.jsonl`, not `entries.jsonl`; do not confuse them). Note how many entries it has (`wc -l <path>/ledger/entries.jsonl`) and the date of the last one.
3. **Loose project memory:**
   - `CLAUDE.md`, `AGENTS.md` (at the root and in subfolders: `git ls-files | grep -iE '(^|/)(claude|agents)\.md$'`), and `~/.claude/CLAUDE.md` only if it talks about this project. Look for rules («always…», «never…», «use X, not Y») and decisions with their reasons.
   - Notes: `docs/decisions*` (or the Spanish `docs/decisiones*`), `ADR-*`, `NOTES.md`, `TODO.md`, and similar. Accepted ADRs are not copied: `kontext find` already reads them; just note that they exist.
   - Claude Code's automatic memory: `~/.claude/projects/<folder>/memory/`, where `<folder>` is the absolute path of the repo with every character that is not a letter or a digit replaced by `-` (`/home/ana/code/app` → `-home-ana-code-app`). Read `MEMORY.md` and the files it links to.
4. **Claude Code in the repo:** what is in `.claude/settings.json` and `settings.local.json` (hooks and permissions) and in `.claude/skills/` and `.claude/agents/`, so you do not overwrite anything that is not yours.
5. **If there is already a kontext project:** `kontext status` (and `kontext verify`) from the repo.

Report in a short list: what there is, where it is, and how much (for example "CLAUDE.md: 3 rules and 1 decision; automatic memory: 2 notes; ctx.py: 41 entries, the last one from 2026-10-30").

## 2. Installing the binary, if it is missing

```sh
curl -fsSL https://raw.githubusercontent.com/OmarAlex24/kontext/main/install.sh | sh
```

That installs the release binary for macOS (Apple silicon) or Linux (x86_64, arm64; static, so it runs in any container) in `~/.local/bin`, checked against the release's checksums; it needs no Rust. If it says there is no release yet, or the machine is something else, build it:

```sh
cargo install --git https://github.com/OmarAlex24/kontext --locked
```

The repo is public: both need network access to github.com, not `gh` or git credentials. If there is a local checkout, `cargo install --path <checkout> --locked`. It needs Rust 1.89 or newer (`rustup`). Check with `kontext --version`.

If there is no way to install it (no network, no Rust), stop here and tell the human: the rest needs the binary.

With the binary, leave this skill and the others where any session on this machine can see them: `kontext install --skills-only --global --dry-run` and then without `--dry-run` (they are written to `~/.claude/skills/`).

## 3. What the human decides, and the rest

Ask **only once, all together**, only what you cannot work out:

- **The owner** (who signs the charter and the decisions): propose the first name from `git config user.name`, in lowercase.
- **Where the kontext-home lives** (propose `~/kontext-home/<name>`) and **how it is shared with other people, machines or sandboxes**: not at all (it works alone), a **git remote** (`kontext sync` by hand), or a **kontext server** (every command syncs on its own, sandboxes set themselves up with a token, and live updates in `kontext ui`; see "The kontext server" below). Offer the server when there is more than one machine, or agents in sandboxes or cloud threads.
- **The cutover day**, only if another system is in use (for example `ctx.py`): until that day it keeps being used and is imported again; from that day on, you only write to kontext.

In the same message, lay out the plan: the commands you are going to run and **the paths each one writes to** (`~/.config/kontext/projects.toml`, the kontext-home, `.kontext.toml`, `.claude/settings.json` or, with `--local` (step 5), `.claude/settings.local.json`, `.claude/skills/`, `.claude/agents/`). Wait for the answer.

Everything else you decide yourself with the default values, without asking: the project name (the name of the repo folder), the context mode (`inject` for everyone, which `kontext init` leaves), the distill on as `init` leaves it, `default_role = "pm"`, and the agent files from `kontext install`.

## 4. Migrating

In this order.

**a) Creating the project** (if there is none: `kontext status` fails). From the code repo:

```sh
kontext init <name> --home <path> [--remote <url>]
```

With a kontext server instead of a git remote (it needs the server's admin secret, below):

```sh
kontext init <name> --home <path> --server <url>
```

If the project already exists (`kontext status` works), `kontext server connect <url>` attaches it to the server and uploads everything, its history included; it changes nothing local, and running it again resumes.

`init` makes the first name from `git config user.name` the owner. If the owner the human named is someone else, change the `owners = [...]` line in `<home>/project.toml` (say so first: it is a write).

**b) The `ctx.py` ledger** (if there is one). It has no `--dry-run`, but it can be repeated without duplicating anything:

```sh
kontext import --from-v0 <folder with ledger/entries.jsonl>
kontext verify
```

If it says the v0 chain is broken, stop and tell the human; `--force` only if they ask for it. With a cutover day, repeat the `import` on that day (it brings only what is new).

**c) The loose memory.** Each decision or rule you found in CLAUDE.md, AGENTS.md, notes, or the automatic memory comes in as a **proposal**, never as a decision:

1. Before each one, check whether it is already there: `kontext recall <two or three keywords>`. If there is already a decision, a proposal, or an entry imported from v0 that says the same thing, do not repeat it.
2. Write it with its origin:

   ```sh
   kontext --actor agent:kontext-migrate propose "<the rule or decision, short>" --why "<the reason, if the source gives one>" --body "Source: <path>:<line> (<CLAUDE.md|AGENTS.md|note|automatic memory>). Text: «<verbatim quote>»" --tags migrated,<topic>
   ```

   One proposal per rule. Quote the text as it is; do not improve it or join two rules into one. If the quote contains `` ` ``, `$`, or double quotes, put that argument in single quotes (or escape them): inside double quotes, the shell runs whatever is between backticks.
3. What is neither a rule nor a decision (session style instructions, paths, test commands, a person's data) does not go in: note it under "Left out", with its reason.

The human signs them afterwards from `kontext inbox` (`kontext inbox sign <n>`); you do not accept them.

## 5. Configuring

In each code repo of the project:

```sh
kontext install --role pm --dry-run
kontext install --role pm
```

It leaves `.kontext.toml` with `default_role = "pm"`, the `SessionStart` hook (and `PreCompact` and `SessionEnd` with the distill on), the `Bash(kontext:*)` permission, the skills, and the agent files, without touching the rest of `.claude/settings.json`. If it warns that it kept an agent file that was changed by hand, tell the human.

If the repo is public, or the sessions run in linked git worktrees (`git worktree list` shows more than one; the first is the main checkout; some tools put every conversation in its own), do it in two steps, in this order (each first with `--dry-run`):

1. In the worktree where the human talks to the PM: `kontext --project <name> install --link-only`. It writes only that worktree's `.kontext.toml`, which marks it as the PM's, so its messages are captured into the main checkout's `.pm/`. Skip it if the PM works in the main checkout.
2. In the main checkout: `kontext --project <name> install --role pm --local --worktree-role agent`. The hooks and the permission go in `.claude/settings.local.json`, which is not committed and which Claude Code reads from the main checkout's root in every worktree (its settings docs); the skills and agent files there reach worktrees that have no `.claude/skills` or `.claude/agents` of their own (its worktrees docs). Delegated agents in the other worktrees start as `agent`, not as the PM.

The other way round, the PM's worktree starts as `agent` until it is marked. A full `kontext install` in a worktree that has no `.kontext.toml` is refused once the main checkout has one. If `kontext install` prints a line for `.git/info/exclude`, offer to add it, so a `git add -A` does not commit the file. Say every path first, as with any write.

The capture hook (it writes what the human types to the PM into `.pm/dumps/`) stays off unless the human asks for it. To turn it on, in this order: in the conversation where the human talks to the PM, run `kontext capture here` (it names that conversation, so only it is captured, not a coordinator that shares its folder); run `kontext doctor` there and check that its Capture section says this conversation is named; then set `[capture] enabled = true` in `project.toml` and run `kontext install` again (with the same flags as above).

### The kontext server

Only if the human chose it. Each owner runs their own: a Cloudflare Worker on their account (the free plan is enough).

1. **Deploying** (once per Cloudflare account): the human makes an API token in the Cloudflare dashboard with the account permission **Workers Scripts: Edit**, and runs `kontext server deploy` with it in `CLOUDFLARE_API_TOKEN` (and `CLOUDFLARE_ACCOUNT_ID` if the token sees more than one account; `--subdomain <name>` the first time if the account has no workers.dev subdomain). It prints the server's URL. Do not ask for the Cloudflare token in the chat: the human runs that command, or sets the variable where you run.
2. The **admin secret** stays in `servers.json` in the kontext config folder (mode 0600) of the machine that deployed; `init --server` and `server connect` use it from there. On another machine, `KONTEXT_ADMIN_SECRET`.
3. **Other people and machines:** the owner makes each one a token (`kontext token create --role owner --person <name>`; it is shown once) and they run `KONTEXT_TOKEN=<token> kontext clone --server <url> <project>`.
4. **Sandboxes and cloud threads:** `kontext token create --role coordinator --epic <name>` (or `implementer`, `tester`, `scout`; `--ttl 8h` for a sandbox that ends) prints `KONTEXT_SERVER`, `KONTEXT_PROJECT`, `KONTEXT_TOKEN`, `KONTEXT_ACTOR` and `KONTEXT_ROLE`; set them in the sandbox's environment, and its first kontext command sets the project up there. Each token writes only what its role may (DESIGN §14.2).

**Never put a token, the admin secret, the server URL or the kontext-home in the code repo**, least of all a public one: tokens live in the config folder (0600) or in the environment. `kontext token list` and `kontext token revoke <id>` manage them.

## 6. Checking

```sh
kontext verify
kontext resume --as pm --peek
echo '{"session_id": "kontext-migrate-test", "source": "startup", "cwd": "<repo path>"}' | kontext hook session-start
kontext inbox count
kontext doctor
```

- `verify` says `OK` for each file in the ledger.
- `resume` brings the charter (if there is one), the imported decisions (only the newest, with a count of the rest, if there are many), and the new proposals.
- The hook prints a JSON with `hookSpecificOutput.additionalContext` that starts with the project's context. If it prints nothing, check `<home>/.kontext/hooks.log`.
- `inbox count` counts what belongs to the human (at least the proposals you wrote).
- `doctor` is the last check: it ends with `No problems` (notes are fine). Each `problem` line comes with its fix; report every one to the human with that fix, word for word. A hook pointing at a binary in `target/debug` means kontext was not installed with `cargo install`: go back to step 2.

## 7. Summary for the human

End with this, short:

```
kontext is installed in <repo> (project <name>, home <path>, shared through <git remote url | kontext server url | nothing>).

Migrated:
- ctx.py: <N> entries imported from <path> (<by type>); verify OK
- loose memory: <M> proposals (<from where: CLAUDE.md 3, automatic memory 1…>)

kontext doctor: <no problems | each problem and its fix>

Your turn to sign: `kontext inbox` (<n> pending). Each proposal says where it came from.

Left out:
- <what> — <why>

To continue: <cutover day and what to do that day; other repos where to run kontext install>.
```
