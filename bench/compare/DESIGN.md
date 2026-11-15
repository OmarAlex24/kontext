# kontext against existing tools: benchmark design and cost

Status: design only. Nothing here has been run, installed or signed up for, and no model was called. Written 2026-11-15 against kontext `origin/main` f778015 (v0.2.0).

Sources. Every claim about another tool cites its own docs at the commit read (shallow clones in `/tmp/kontext-benchmark/repos/`, links at the end). Three labels are used:
- **from source, not documented**: the claim was read in the code, not the docs.
- **not documented**: the docs say nothing about it.
- **to verify in the pilot**: only installing the tool would settle it.

Prices are Anthropic first-party rates as of 2026-11-03, taken from the `claude-api` skill.

---

## 0. Summary

- **Arms (6):**
  - plain Claude Code (defaults, automatic memory on);
  - kontext v0.2.0;
  - gentle-ai (Engram, workflow and review, default preset);
  - mem0 (Claude Code plugin on the hosted platform);
  - Beads;
  - OpenSpec, the files-in-the-repo arm.
- **Tasks (3):**
  - S1, a 3-line bug, and S2, a bug in shared logic. Both measure overhead.
  - L, a 4-day project. Requirements are said once or in passing, some change, and a pasted email plants distractors. One `/compact`, a new chat on day 3, and a new chat on a second machine on day 4. Fourteen hidden checks, one per requirement. No requirement's code is asked for until after its context is lost, and the checks run at the end of every day.
- **Friction axis, no model:** from a clean machine to "machine B sees what A saved", for 9 setups. kontext is worse than mem0, Beads, OpenSpec and Engram-over-git on accounts, secrets and steps. It is better on what happens after setup: every command syncs on its own and it needs no third-party host (§3).
- **Model:** Sonnet 5.5 for every arm, chosen on price. Haiku 5.5 as a second, cheap column, where the arms differ most. Opus 5.5 is an optional confirmation on 3 arms.
- **Runs and cost:** 5 runs per cell.
  - Full run: about **$145** of API-equivalent cost (at most about $200), 10–12 hours on one machine with 4 containers at once.
  - Pilot: about **$15–20**, 3–4 hours.
  - The anchor is the cost per run measured in kontext's own reports of 2026-11-04/05. Tokens × price is a cross-check (§5).

---

## 1. Which tools to compare

### 1.1 Candidates

The criteria are the ones the task names:
- what the tool stores;
- how it syncs across machines;
- how it installs into Claude Code;
- whether it runs headless (`claude -p`) under a throwaway `HOME` and `CLAUDE_CONFIG_DIR`;
- what accounts or keys it needs.

| Tool | What it stores | Across machines | Into Claude Code | Headless, throwaway HOME | Accounts / keys | In? |
|---|---|---|---|---|---|---|
| **kontext** v0.2.0 | Append-only ledger: charter, goals, decisions, proposals, questions, facts, tasks and features; `.pm/` with the user's verbatim words (README "Phase 2 commands", "The PM's memory") | A kontext server on the user's own Cloudflare account, synced on every command, or a git remote with `kontext sync` by hand (README "Working with other people") | `kontext install`: SessionStart hook (+ PreCompact/SessionEnd for the distill), `Bash(kontext:*)` permission, skills, agent files | Yes. `KONTEXT_CONFIG_DIR`, `CLAUDE_CONFIG_DIR` and `kontext install --skills-only --global` follow `$CLAUDE_CONFIG_DIR` (README); its own bench already ran it under `claude -p` | Cloudflare account + API token made by hand; a kontext token per extra machine. The distill (`[distill] enabled = true` by default) calls `claude -p --model haiku`: model cost | **In** (subject) |
| **gentle-ai** (+ Engram) | Engram: local SQLite with FTS5, observations and session summaries written by the agent itself ([engram README L128][eng-readme-128], [DOCS "Agent-Driven Compression"][eng-docs-sync]). gentle-ai adds skills, persona, permissions, the ODD workflow and RDD review ([components][ga-components], [README][ga-readme-review]) | Engram Git Sync: `engram sync` exports chunks into `.engram/`; you commit, push, pull and `engram sync --import` by hand ([DOCS L1934][eng-docs-sync]). Or Engram Cloud, self-hosted (`docker compose`, Postgres, tokens, HTTPS for bearer tokens), with opt-in autosync ([cloud README][eng-cloud], [quickstart L326][eng-cloud-https], [DOCS L2244][eng-docs-autosync]) | `gentle-ai install --agent claude-code …` (TUI or flags, [non-interactive][ga-nonint]); Engram via plugin + `engram setup claude-code` ([AGENT-SETUP L309][eng-agent-setup]) | Non-interactive flags exist ([non-interactive][ga-nonint]). gentle-ai's non-test code never reads `CLAUDE_CONFIG_DIR` (from source, not documented: one hit, in an e2e test), so it writes under `$HOME/.claude`. `GENTLE_AI_HOME` moves its own state ([usage L426][ga-usage-426]). `install.sh` edits shell rc files ([README L289][ga-readme-289]). Engram reads `ENGRAM_DATA_DIR` ([README L130][eng-readme-130]) | None for local use. Engram Cloud needs a host you run | **In** (named by Omar; closest to "configures the agent with memory + workflow") |
| **mem0** (Claude Code plugin 0.3.1; see the note under this table for the other paths) | Memories extracted by Mem0's LLM from captured prompts, answers, changed paths and test results. Shared project memory is keyed by a hash of the git remote; personal memory by `user_id` ([claude-code.mdx][m0-cc]) | Hosted: everything goes to api.mem0.ai. Capture is local, and a background worker flushes every 5 exchanges, after 5 min idle, or at session end/compaction ([L92][m0-cc-92]). Another machine shares project memory through the same git remote and personal memory through the same `user_id` ([L144][m0-cc-144]) | `claude plugin marketplace add mem0ai/mem0` + `claude plugin install mem0@mem0-plugins --scope user --config api_key=…` ([L23-25][m0-cc-23]). Recall runs on the first prompt of a session only if it has ≥ 20 characters ([L46][m0-cc-46]) | Plugin install is non-interactive with `--config`. `MEM0_API_URL` overrides the host (from source, not documented: [memory_core.py L31][m0-src-url]). Behavior under `claude -p` is to verify in the pilot | Mem0 account + API key (`m0-…`). Mem0 pays the extraction model for 30 days (10k adds and 1k retrievals a month). After that you bring your own LLM key, about $3.55–5.34 per 1,000 adds ([pricing L23][m0-pricing], [L46][m0-pricing-46]). Telemetry is tied to the account email ([L180][m0-cc-180]). The self-hosted server needs Docker, Postgres and an LLM key ([server README L20][m0-server]) | **In** (named by Omar; the hosted, automatic-memory option) |
| **Beads** (`bd`) | Issue graph (tasks, epics, dependencies, claims) and `bd remember` facts, in embedded Dolt ([README][bd-readme]) | Dolt over the git remote: `bd dolt push`/`pull` against `refs/dolt/data`; `bd bootstrap` on a new clone ([sync-setup][bd-sync]). Auto-push is opt-in (`dolt.auto-push`, 5 min, [configuration L118][bd-config]). Auto-pull: not documented | `bd init` writes the Claude Code integration by default; `bd setup claude` adds a SessionStart hook running `bd prime`, plus a CLAUDE.md section ([README L35-45][bd-readme-35], [claude-code.md][bd-cc]) | A CLI with JSON output; hooks in project settings. A `--sandbox` flag disables auto-push ([CLI_REFERENCE L337][bd-cli-337]). Behavior under a throwaway HOME is to verify in the pilot | None beyond git. `ANTHROPIC_API_KEY` only for optional AI compaction and dedup ([CLI_REFERENCE L5915][bd-cli-5915]). Dolt must be installed at a pinned version on every machine ([sync-setup][bd-sync]) | **In** (closest to kontext's task graph + memory + sync; DESIGN §15 already named it) |
| **OpenSpec** | Specs and change proposals as Markdown in `openspec/` of the repo ([commands L57][os-commands]) | Through git, with the code | `openspec init --tools claude` writes `.claude/skills/openspec-*` and `.claude/commands/opsx/` ([supported-tools L71, L210][os-tools]) | `--tools` makes init non-interactive ([cli L105][os-cli]). Skills can be invoked in plain words ([README L150][os-readme]); whether they get used in `-p` is to verify in the pilot | None. Node ≥ 22.12 for npm, or brew ([README L132][os-readme]) | **In** (files in the repo: the kind of tool that came closest to kontext last time) |
| **claude-mem** | Observations and summaries made by an observer model from tool activity; SQLite + Chroma in `~/.claude-mem` ([README L273-291][cm-readme-273]) | Opt-in Cloud Sync through cmem.ai Pro: a sync token from cmem.ai and a 30-day trial that asks for a payment method ([cloud-sync L115-126][cm-sync], [README L145][cm-readme-145]) | `npx claude-mem install` or `/plugin install claude-mem` ([README L140, L151][cm-readme]) | `CLAUDE_MEM_DATA_DIR` and `CLAUDE_MEM_WORKER_PORT` (default `37700 + uid % 100`, so arms run at once collide without containers) ([configuration L95, L106][cm-config]). Non-interactive with `--provider` ([README L147][cm-readme-145]) | cmem.ai account for sync. The observer uses your Anthropic plan by default, or an API key, OpenRouter or Gemini ([configuration L71-74][cm-config]) | **Out of the model runs, in the friction axis.** Same mechanism as mem0 (automatic capture → LLM extraction → recall), so one of the two is enough. mem0 was named by Omar; cross-machine claude-mem needs a paid trial with a card. Swap it for mem0 if Omar prefers local-first |
| **GitHub spec-kit** | `.specify/` specs, plans and tasks in the repo ([README][sk-readme]) | git | `specify init --here --integration claude` (non-interactive flag [installation L126][sk-install]) | Likely, through the flag; to verify in the pilot | None; Python 3.11+ and uv ([README L72][sk-readme]) | **Out.** Same files-in-the-repo idea as OpenSpec, with a heavier fixed pipeline (constitution → specify → plan → tasks as slash commands). One arm of this kind is enough; OpenSpec's skills can be invoked in plain words, which fits scripted turns that never name a tool |
| **Task Master** | Tasks parsed from a PRD into `.taskmaster/` ([README L214][tm-readme-214]) | git (not documented as a feature) | `claude mcp add taskmaster-ai -- npx -y task-master-ai` ([README L81][tm-readme]) | Not documented | A separate LLM API key for its own AI commands, unless it uses the Claude Code provider ([README L91-106][tm-readme]) | **Out.** Overlaps Beads (task graph) with no memory of what the user said. Its last commit is 2026-04-23, against days for the others |
| **Letta Code** | Self-editing memory blocks, MemFS tracked in git ([README L23][lc-readme]) | MemFS to a git repo of yours; remote computers need a Letta sign-in ([README L23, L30][lc-readme]) | It is its own agent harness (`letta`), not a Claude Code add-on ([README L55-59][lc-readme]) | Not under `claude -p` | Letta account for remote use | **Out.** It replaces Claude Code, so harness and memory would be confounded |
| **Zep / Graphiti** | Temporal knowledge graph ([README][gr-readme]) | Your Neo4j/FalkorDB, or Zep's hosted service | Through an MCP server (Docker, FalkorDB); "no authentication" by design ([mcp_server README L10, L17][gr-mcp]) | Not documented | OpenAI key by default ([mcp_server README L251][gr-mcp-251]) | **Out.** A library and memory backend, not something a developer installs into Claude Code to keep a project's requirements |

**How mem0 plugs into Claude Code today: three paths.**
- **OpenMemory**, the earlier local MCP server: there is no `openmemory/` directory at this commit. It shows up only in old changelog entries ([changelog][m0-changelog]), so it is not an option today.
- **The hosted MCP server** `https://mcp.mem0.ai/mcp`: added with `npx mcp-add … --clients "claude code"`. You sign in through the browser, or send the API key as a bearer token for headless use. It hands the agent 11 tools (`add_memory`, `search_memories`…) and the agent decides when to save ([mem0-mcp.mdx][m0-mcp]).
- **The Claude Code plugin** captures and recalls on its own through hooks ([claude-code.mdx][m0-cc]).

The arm uses the plugin: it is the path mem0's Claude Code page recommends, and it is the one that needs no agent discipline to save. Every path needs a Mem0 account and key, and mem0's own LLM extracts the memories on the server side. The self-hosted server (FastAPI + Postgres + an OpenAI key, [server README][m0-server]) is the only way without the hosted service, and the plugin reaches it only through `MEM0_API_URL` (from source, not documented).

**Plain Claude Code** is the baseline: no tool, with automatic memory on, which is the default. Automatic memory is left on in every arm, because that is what a user has. The variants with automatic memory off that kontext's bench used are not repeated.

**Possible swap:** if gentle-ai's workflow and review turn out to swamp the memory comparison, add an "Engram only" arm (`engram setup claude-code`) next to gentle-ai rather than instead of it. Omar's question is about gentle-ai as a whole.

### 1.2 What each arm installs (default for Claude Code, versions pinned at build)

| Arm | Setup before day 1 (by the harness, never by the agent) | Non-default changes needed headless (labelled in results) |
|---|---|---|
| plain | nothing | none |
| kontext | `install.sh` (v0.2.0); `kontext init <p> --server <sidecar url>`; `kontext install` in the repo. No charter, goals or decisions are seeded: the requirements reach kontext only through the conversation, the same as every other arm | `KONTEXT_ACTOR=human:owner` set for the setup commands only. The distill command takes the API key from the environment |
| gentle-ai | `gentle-ai install --agent claude-code` with the default preset (`--preset` per the TUI's default, to verify in the pilot); `engram setup claude-code` if the installer does not do it | `CLAUDE_CONFIG_DIR=$HOME/.claude` (§6) so its writes land where Claude Code reads; telemetry off |
| mem0 | `claude plugin marketplace add …`; `claude plugin install … --config api_key=$MEM0_API_KEY` | `MEM0_USER_ID=bench-<run>`, a unique git remote per run (§6), `MEM0_TELEMETRY=false` |
| beads | install `bd` + the pinned Dolt; `bd init` (writes the Claude integration) | none expected |
| openspec | install; `openspec init --tools claude` | none expected |

`--allowedTools` is the same base list for every arm (`Read`, `Edit`, `Write`, `Glob`, `Grep`, `Bash(python3:*)`, `Bash(git:*)`, `Bash(ls|cat|grep|head|tail|sed -n|wc|echo:*)`), plus each arm's own:
- kontext: the bench's `Bash(kontext:*)` set, including `Bash(KONTEXT_* kontext *)`, `Bash(export KONTEXT_*)` and `Bash(timeout * kontext *)`;
- gentle-ai: `Bash(gentle-ai:*)`, `Bash(engram:*)`, `mcp__engram__*`;
- mem0: `mcp__plugin_mem0_*` (exact name to verify);
- beads: `Bash(bd:*)`;
- openspec: `Bash(openspec:*)`, `Skill`.

Every run records the permission denials of each turn (`permission_denials` in the envelope). That count is reported per arm: kontext's own runs once had 153 of them.

---

## 2. Tasks

All three fixtures are Python 3.11 stdlib projects, small enough to build and check in seconds. The fixture repos, the scripted turns and the hidden checks are frozen in one commit before the pilot. That commit's hash goes in every `run.json`, and none of them changes after a real run.

### 2.1 Small tasks: overhead when the work is trivial

One session, one scripted user turn, a few hundred lines of code.

- **S1, a 3-line bug.** `invoices.py: due_date()` adds `days - 1` instead of `days`. One visible test fails. The turn: "The due date on invoices is one day early, the test in tests/test_dates.py shows it. Please fix it." There are 6 hidden checks: the fix, and that the 5 other functions in the file still behave the same. The S1 fixture is 1 module of ~150 lines and 1 test file.
- **S2, a bug in shared logic.** `money.round_half()` rounds half-down, and it is used by 3 modules (`invoices`, `refunds`, `reports`). The visible failing test is in `refunds`; the hidden checks cover all three callers and that no caller grew its own copy of the rounding (an AST check). The turn names only the refunds symptom.

What these measure: time, turns, tokens, cost, hidden checks, interventions. Overhead is each arm's cost and time against plain. The memory these tools add is useless here, so this is where bloat shows. It mirrors gentle-ai's own "3-line bug" and "shared logic" rows ([README L189][ga-readme-bench]), which ran on its own shell with n = 1.

### 2.2 Long task L: what kontext exists for

**Fixture `workshop`.** A small booking library for a community workshop: classes, bookings, waitlist, cancellations and refunds, reminders, a CSV export. About 400 lines over 5 modules, a visible test suite of ~15 tests, and a README. The agent builds it out over four days.

**Requirements:** 14, each with one hidden check. The values below are the planned set; exact wording is frozen with the fixture.

| # | Requirement | How it reaches the agent | Class |
|---|---|---|---|
| R1 | At most 12 people per class | Day 1, first turn, stated plainly | A, plain |
| R2 | No classes on Tuesdays (the space is closed) | Day 1, one clause inside a paragraph about something else | B, in passing |
| R3 | Prices are integers in cents | Day 1, as the user's short answer to a question in a scripted turn ("cents, always") | B, answer |
| R4 | Members get 15 % off | Day 1, once, in passing | B |
| R5 | No bookings less than 2 hours before the start | Day 1, plainly | A |
| R6 | Full refund up to 48 h before; later changed to 24 h | Day 1 (48), day 2 (24) | C, changed |
| R7 | Waitlist holds 5; later "double the waitlist" | Day 1 (5), day 2 relative (10) | C, relative change |
| R8 | SMS reminders; later "forget SMS, email only" | Day 1, discarded day 2 | discarded |
| R9 | Reminder the day before at 18:00 | Day 1, in passing | B |
| R10 | Times in America/Mexico_City | Day 2, once | B |
| R11 | Names compared case-insensitively, stored as typed | Day 2, in passing | B |
| R12 | CSV export uses `;` as separator | Day 2, once | B |
| R13 | A cancelled spot goes to the first person on the waitlist, who gets 2 h to confirm | Day 2 | A |
| R14 | Members can book 14 days ahead, everyone else 7 | Day 2, as an answer to a question | B, answer |

**Distractors.** A pasted email from "the pottery studio next door" on day 2: 10 people per class, closed on Mondays, 72 h refunds, comma CSV. These are near-identical values belonging to someone else, in the style of kontext's own `fill.py` distractors.

**A fixed interface, so a check never fails on a name.** The fixture ships stubs that raise `NotImplementedError` with fixed signatures: `book()`, `price_for()`, `cancel()`, `refund_amount()`, `promote_waitlist()`, `reminders_due()` and `export_csv()`. The turns that ask for them name them, as kontext's `long-memory` did with `late_fee()`.

**No requirement can be coded while it is still in context.** Days 1 and 2 ask only for work that no hidden check touches: the data model, storage, listing classes, a CLI skeleton, a visible bug. Every function under a check is first asked for in a later session than the one where its requirement was said.

**Days.** Each day is a new `claude -p` session, and turns within a day continue it with `--resume`, as in the existing harness.
1. **Day 1, 5 turns:** a charter-like opening (R1, R5), then 4 work turns on the model and storage. R2, R3, R4, R6 (48), R7 (5), R8 (SMS) and R9 are said along the way.
2. **Day 2, 8 turns:** 4 work turns on listing and the CLI, carrying the changes (R6 → 24, R7 → double, R8 discarded), R10–R14 and the pasted email with the distractors. Then 2 filler turns of ~25k tokens each (meeting notes, `fill.py` style), `/compact`, and 1 work turn.
3. **Day 3, new chat, same machine, 3 turns:** "implement `cancel()` and `refund_amount()` with the rule we agreed", "implement `reminders_due()`", "implement `promote_waitlist()`". These carry R3, R6, R7, R8, R9, R10 and R13.
4. **Day 4, new chat, machine B (§2.3), 3 turns:** "implement `book()` and `price_for()` with the member rules", "implement `export_csv()` for the accountant", "go through everything we agreed and make sure the code follows it". These carry R1, R2, R4, R5, R11, R12 and R14. No requirement is restated.

There are 19 scripted turns and about 70k tokens of user text in all, including the filler.

**Snapshots.** The hidden checks run on a snapshot of the repo at the end of every day, in the verification container.
- A requirement whose check already passes at the end of day 2 counts as "coded before the loss" and is reported apart. With the stubs it should be none, and any that do are a leak in the script.
- Day 3's seven requirements are scored on the day-3 snapshot: loss through a new chat.
- Day 4's seven are scored on the day-4 snapshot: loss through a new chat on a new machine.
- Day-3 requirements that regress by the end of day 4 are counted too.

**Hidden checks.** `acceptance/test_requirements.py` has 14 test functions, one per requirement, each named after it (`test_r06_refund_window_24h`). They run against the final code. Checks that run the code, not grep it. The changed and discarded ones also fail on the old value (48 h, waitlist 5, any SMS sending code path called). The distractor values fail their checks.

Results:
- the share of requirements that survive, per run, per class (A, B, C, discarded, answer) and per day (3 against 4);
- which ones failed;
- whether a failure took a distractor's value (a "confusion", as in kontext's bench).

**How the checks stay hidden.**
- They live in the harness's directory on the host. They are never in the fixture repo, its git history, a Docker image layer or the container's filesystem while the agent runs.
- After the last turn, the harness takes the final repo as a tarball (`git archive` plus untracked files). It runs the checks in a separate verification container with the repo mounted read-only and no network.
- Memory stores (mem0's hosted memories, the kontext ledger, Engram, Beads) only ever see what the agent wrote, and the agent never saw the checks.

**How the "user" is scripted.**
- A TOML script with every turn's text, byte-identical for every arm. It never names a tool, a slash command or a memory command.
- Each session's first prompt is over 20 characters, so mem0's first-prompt recall fires ([L46][m0-cc-46]).
- Arm setup happens before day 1 and outside the conversation.

**Fixed reply rule, the same for every arm.** A turn ends with the agent asking something instead of doing the work: no diff, and a question or a request for approval or a signature. The harness then sends one fixed reply: "Use your best judgement with what I've told you so far, and go ahead."
- That counts as one intervention, at most 2 per scripted turn, then it moves on.
- kontext's "sign the plan" or `scope sign` requests, gentle-ai's "Needs your decision" and an OpenSpec proposal waiting for approval all get the same reply.
- Interventions are counted per arm and per kind.

### 2.3 The second machine, and why it is fair

Day 4 runs in a second container, B, on a private Docker network. It starts with:
- the code: the harness commits and pushes A's working tree to a bare git repo on a volume at the end of day 3, for every arm alike, because moving code is not what is being measured;
- each arm's own documented way to move its memory, nothing more.

| Arm | How A's memory reaches B | Steps by the harness (also counted in §3) |
|---|---|---|
| plain | Automatic memory is local to `~/.claude/projects/…` and does not travel; any `CLAUDE.md` the agent wrote travels with git | none |
| kontext | Server, on every command. In the runs the server is the same `worker.js` in workerd through Miniflare, in a sidecar container, never the deployed Worker. `server/dev.mjs` binds `127.0.0.1` ([dev.mjs][kx-dev]), so other containers could not reach it. The harness runs a 10-line script that imports its `options()` and overrides `host` to `0.0.0.0`; kontext's code is not changed. The setup on A gets `KONTEXT_ADMIN_SECRET` (the sidecar's) for `init --server`. A runs `kontext token create --role owner --person owner`; B runs `KONTEXT_TOKEN=… kontext clone --server …` + `kontext install` | 3 commands |
| gentle-ai | Engram Git Sync: `engram sync` + commit/push on A, pull + `engram sync --import` on B ([DOCS L1934][eng-docs-sync]) | 2 commands (+ git) |
| mem0 | Hosted. B gets the plugin, the same key and the same `MEM0_USER_ID`; the repo has the same remote | 2 install commands |
| beads | `bd dolt push` on A, `bd bootstrap` on B ([sync-setup][bd-sync]) | 2 commands |
| openspec | `openspec/` travels with git | none |

Before the day-3 and day-4 boundaries the harness runs one **settle step** per arm, so asynchronous work has landed:
- kontext: `kontext distill --wait`;
- mem0: wait until its status shows no pending flush (or its flush log is quiet), at most 10 min;
- the others: nothing to wait for.

The wait time is recorded. It is not counted in session time.

This is as fair as a design can make it: every arm moves memory the way its own docs say. Where that is manual (Engram, Beads), the step is done for it and counted in the friction axis, not hidden. Day 3 (new chat, same machine) and day 4 (new chat, other machine) are scored apart, so a tool that only loses on the move shows it.

One difference stays. kontext's runs use a local Miniflare server, not Cloudflare (other latency and limits), and mem0 uses its real hosted service. The friction axis covers kontext's real deploy separately.

---

## 3. Install-and-sync friction (no model)

From a clean machine A with Claude Code and git, to "machine B sees what A saved". Code clone and Claude Code itself are not counted. "Manual" means a step done outside a terminal command: a browser, a dashboard, an edited shell file, a secret carried between machines. Every row is **doc-derived**. The friction harness (§7) re-executes the terminal steps in two fresh containers and adds a "pilot-verified" mark; steps that need an account are counted, not executed.

### 3.1 kontext v0.2.0, server path (README "With a kontext server", `install.sh`)

| # | Where | Step | Kind |
|---|---|---|---|
| 1 | A | `curl -fsSL …/install.sh \| sh` | command |
| 2 | A | Add `~/.local/bin` to `PATH` if it is not there (the installer prints the line, it does not edit rc files) | manual, conditional |
| 3 | A | Create a Cloudflare account | manual, account |
| 4 | A | In the Cloudflare dashboard, create an API token with the account permission "Workers Scripts: Edit" | manual, secret 1 |
| 5 | A | `CLOUDFLARE_API_TOKEN=… kontext server deploy` | command |
| 5b | A | On an account with no `workers.dev` subdomain, deploy refuses ("Pick one… run again with --subdomain"): run it again with `--subdomain <name>` (`src/server.rs` L476) | command, conditional |
| 6 | A | Put `export KONTEXT_ACTOR=human:<name>` in the shell rc (README "Who writes") | manual |
| 7 | A | `kontext init <project> --server <url>` | command |
| 8 | A | `kontext install` in the code repo | command |
| 9 | A | `kontext token create --role owner --person <name>` (shown once) | command, secret 2 |
| 10 | B | `curl … install.sh \| sh` (+ `PATH` as in step 2) | command (+ manual) |
| 11 | B | Carry the token from A to B | manual, secret |
| 12 | B | `KONTEXT_TOKEN=… kontext clone --server <url> <project>` | command |
| 13 | B | `export KONTEXT_ACTOR=…` in the rc | manual |
| 14 | B | `kontext install` in the code repo (the hooks hold this machine's binary path) | command |
| after | A, B | Every command pulls and pushes on its own | automatic |

**Total:** 8 commands (+1 conditional); 5–7 manual steps; 1 account (Cloudflare); 2 secrets made by hand or carried (the Cloudflare token and the kontext token); runs on infrastructure the user owns, on the free plan. Steps 3–5 are once per Cloudflare account, so a second project costs 7, 8, 9, 11, 12 and 14.

Git path, for comparison: `init --remote <url>` needs a private repo created by hand. After that, `kontext sync` by hand on every machine, every time.

### 3.2 The others

| Setup | Commands A + B | Manual steps | New accounts | Secrets made or carried | After setup | Source |
|---|---|---|---|---|---|---|
| **mem0 plugin (hosted)** | 2 + 2 (`marketplace add`, `plugin install --config api_key`) | 4–5: sign up, create API key in the dashboard, carry the key to B, restart Claude Code ×2; set `user_id` the same on both for personal memories | 1 (Mem0) + an LLM provider key after 30 days to keep adding | 1 (Mem0 key) | Automatic but asynchronous: flush every 5 exchanges / 5 min idle / session end, then server-side extraction | [claude-code.mdx L18-25, L92, L144][m0-cc], [pricing][m0-pricing] |
| **gentle-ai + Engram, git sync** | 2 + 2 install (`install.sh`, `gentle-ai install`) + 2 + 2 per sync (`engram sync` + git push / git pull + `engram sync --import`) | 1–2 (new terminal after rc edit; TUI choices unless flags) | 0 (uses the git host) | 0 | **Manual on every update**: export, commit, push / pull, import | [README L279-289][ga-readme-289], [DOCS L1934][eng-docs-sync] |
| **gentle-ai + Engram Cloud** | as above + `docker compose up` on a host both reach + `engram cloud config --server` + `engram cloud enroll` on each | Run a host with HTTPS (bearer tokens require it); set `ENGRAM_CLOUD_TOKEN`, `ENGRAM_JWT_SECRET`… on the server; `ENGRAM_CLOUD_AUTOSYNC=1` + token + server in the environment of `engram mcp` on each machine | A VPS or similar | 2+ (cloud token, JWT secret) | Automatic (autosync) | [cloud README][eng-cloud], [quickstart L326][eng-cloud-https], [DOCS L2244][eng-docs-autosync] |
| **Beads** | 2 + 2 (`bd init`, `bd dolt push` / `bd bootstrap`) + Dolt install on both | 1–2 (install the pinned Dolt version, not "latest") | 0 | 0 | Manual `bd dolt push` / `bd dolt pull`; auto-push is opt-in, auto-pull not documented | [README L31-45][bd-readme-35], [sync-setup][bd-sync], [configuration L118][bd-config] |
| **OpenSpec** | 2 + 0–1 (install, `openspec init --tools claude`; B needs the CLI only to run it) | 0 | 0 | 0 | With git: commit/push/pull, which a developer already does | [README L132-138][os-readme] |
| **claude-mem + cmem.ai Pro** | 1 + 1 (`npx claude-mem install`) | Sign in through the browser on each machine; a checkout page with a payment method for the 30-day trial | 1 (cmem.ai, with a card) | handled by the installer | Automatic: the worker syncs on write and pulls at session start | [README L140-147][cm-readme-145], [cloud-sync L115-126][cm-sync] |
| **plain Claude Code** | 0 | 0 | 0 | 0 | Automatic memory does not travel: not documented as a feature. `CLAUDE.md` travels with git | — |

### 3.3 Where kontext stands, plainly

- **Worse than mem0** on getting started. mem0 is 4 commands and one sign-up, with no infrastructure. kontext is 8 commands, a Cloudflare account, and an API token whose permission you pick by hand in a dashboard: the heaviest single step in the table.
- **Worse than Beads, OpenSpec and Engram-over-git on accounts and secrets.** They reuse the git host the user already has, with no new account and no secret. kontext needs Cloudflare plus a token carried to every new machine.
- **Two extra manual steps per machine that the others do not have:** `export KONTEXT_ACTOR`, and `kontext install` again on every machine because the hooks record an absolute binary path.
- **Better after setup.** kontext, mem0, claude-mem Pro and Engram Cloud are the only ones where B sees A with no further effort. Of those, kontext is the only one where the store runs on the user's own account for free with no server to operate: mem0 and claude-mem are third-party hosted (mem0 with telemetry tied to the account email), and Engram Cloud needs a VPS with HTTPS. Sandboxes join kontext with environment variables alone (`kontext token create --role coordinator` prints them).
- For Omar's goal ("install a CLI, get the shared store up quickly, everything in sync with no effort"), the gap is steps 3–5 and 9–11. A deploy that does not need a dashboard-made token, and a way to hand B its token that is not copy and paste, would bring kontext to mem0's count. That is an observation, not part of the benchmark.

---

## 4. Measures

Per run, raw; there is no composite score.

| Measure | Where it comes from |
|---|---|
| Wall time | Harness clock per turn and per session. Settle waits are recorded apart, not in session time |
| Turns | Scripted turns sent; model requests (distinct `message.id` with usage, as `kontext measure` counts them); `num_turns` from the envelope |
| Tokens: input, cache write, cache read, output | The `claude -p` JSON envelope (`modelUsage`), as the difference from the previous call of the same session, because `--resume` reports running totals (the bug fixed in kontext's 2026-11-04 report). This includes subagents. Whether the envelope includes subagents is to verify in the pilot |
| API-equivalent cost | The envelope's cost difference per call, plus the off-transcript costs below. Recomputed from tokens × the price table as a check |
| Off-transcript costs | **kontext distill:** the envelopes of its own `claude -p` calls (the existing `distill_cmd.py` wrapper). **mem0:** the number of `add` calls (from the plugin's local `evidence.sqlite` / flush log, to verify in the pilot) × $3.55–5.34 per 1,000 adds (BYOK rate, [pricing L46][m0-pricing-46]), shown both as $0 (Mem0 pays during the free 30 days) and at the BYOK rate. **gentle-ai/Engram:** none expected (the agent compresses, [DOCS][eng-docs-sync]); any subagent counts through the envelope. Hosted fees: none at these volumes (mem0's free plan) |
| Hidden checks passed | S1/S2: the share of hidden tests. L: per requirement (14), per class, and day 3 against day 4 failures |
| Confusions | L: checks that failed on a distractor's or a superseded value |
| Human interventions | Fixed replies sent (§2.2) + permission denials per run |
| Where the context goes | `kontext measure --json` on each run's main-session transcripts: history re-read per request, what fills it (tool results, tool calls, text), tool calls per name, and `kontext` subcommands (kontext arm only) |

**`kontext measure`, checked against `src/measure.rs`.**
- It reads any Claude Code transcript and needs no project or configuration (README "Measuring conversations").
- It skips subagent (`isSidechain`) lines.
- It reports tokens only, never prices.
- So it gives the breakdown, and the envelope gives the cost. Subagent transcripts are kept and summed apart.
- It never prints message text, so its output can be published.

---

## 5. Model, runs and cost

### 5.1 Model

- **Primary:** `claude-sonnet-5-5` for every arm, default effort, chosen on price. It costs about half as much as Opus 5.5 per run, and that is what pays for 5 runs per cell. It does not separate the arms better: in long-memory, Opus separated them at least as much (Opus `notes` 85 % against kontext 95–99 %; Sonnet `notes` 97 % against kontext 97–100 %).
- **Second column:** `claude-haiku-5-5`, worth it.
  - About a quarter of Sonnet's cost per long run (0.59 against 2.34 USD in long-memory).
  - The arms differ most on it: there, `automem` was 55 ± 28 and `notes` 72 ± 26 against kontext 98 ± 4.
  - Prompts past 100k tokens cost 5× on Haiku. The long task's design keeps most requests under that; the compacted window is where it can cross.
- **Optional:** `claude-opus-5-5` on plain, kontext and the best competitor, long task only. It is the model Omar's teams actually run.
- **Tool-side models stay at the tools' defaults:** kontext's distill (`haiku`), mem0's server-side extractor (its own), gentle-ai's subagent routing (whatever its preset assigns). Their cost is counted (§4). gentle-ai routing to another model is labelled, not overridden.

### 5.2 Variance and what difference counts

From kontext's reports (same harness style, same models):
- **Memory share.** On Sonnet and Opus, SDs were 7–12 points for `automem` and `notes` and 0–7 for kontext. On Haiku, 26–48 points.
- **Tokens and cost per run.** CV about 5–18 % (billing: Sonnet 725k ± 49k tokens; Haiku `automem` ± 14 %).

With 5 runs per cell:
- **Requirements.** The standard error of a mean with SD 10 is about 4.5 points; of a difference, about 6.3. Each of L's 14 requirements is 7.1 points. A difference counts as meaningful when the means differ by **≥ 2 requirements (≥ 14 points)** and a two-sided permutation (Mann-Whitney) test across the 5 against 5 runs gives p < 0.05. With 5 against 5, p < 0.01 needs complete separation.
- **Cost and time.** Differences under **20 %** are not claimed.
- **Small tasks.** Hidden checks will be near 100 % for every arm, so only cost, time and turns are compared, by median and range.

Why 5 and not 3: with 3 runs a 14-point gap is under 2 standard errors at Sonnet's SDs, and Haiku's SDs make 3 runs meaningless. The task asks for at least 3. Five is the smallest count where the gap seen last time is resolvable.

### 5.3 Cost

**Price table** (per million tokens; cache write 5 min = 1.25× input, 1 h = 2×):

| Model | Input | Output | Cache read | Cache write 5 min / 1 h |
|---|---:|---:|---:|---:|
| Sonnet 5.5 | 2.00 | 10.00 | 0.10 | 2.50 / 4.00 |
| Haiku 5.5 (≤ 100k prompt; above: ×5) | 0.10 | 0.50 | 0.01 | 0.125 / 0.20 |
| Opus 5.5 | 4.00 | 20.00 | 0.20 | 5.00 / 8.00 |

**Anchors, measured per run in kontext's reports:**
- billing, 3 days, small: Sonnet $0.26–0.30, Haiku $0.02–0.03, Opus $0.51–0.70;
- long-memory, 4 days, 280k of filler: Sonnet $2.33–2.63, Haiku $0.59–0.69, Opus $4.57–5.53.

L has less filler (~50k) but more code turns, so it is set at the long-memory level.

**Per run, Sonnet 5.5 (assumed):**

| Arm | S1 | S2 | L | Assumption |
|---|---:|---:|---:|---|
| plain | 0.12 | 0.25 | 2.30 | anchors |
| kontext | 0.15 | 0.30 | 2.60 | +6–13 % measured overhead + distill $0.03 |
| gentle-ai | 0.30 | 0.55 | 3.50 | persona, skills and review add context and turns; its own table shows 0.105–0.555 per small task on its shell |
| mem0 | 0.13 | 0.27 | 2.60 | recall injection; adds off-transcript ≈ 30 × $0.0045 |
| beads | 0.14 | 0.30 | 2.70 | `bd prime` ~1–2k tokens per session ([claude-code.md L70][bd-cc]) + tool calls |
| openspec | 0.20 | 0.45 | 3.00 | proposal files written and read |
| **per seed, all arms** | **1.04** | **2.12** | **16.70** | |

**Cross-check, tokens × price on Sonnet, plain:**
- **S1:** about 12 requests × 28k cached history = 336k cache read ($0.03) + 45k written ($0.11) + 3k out ($0.03) ≈ **$0.17**.
- **L:** about 150 requests × 70k history = 10.5M read ($1.05) + 500k written ($1.25 at 5 min, $2.00 at 1 h) + 50k uncached ($0.10) + 50k out ($0.50) ≈ **$2.90–3.65**.

Both come out 26–59 % above the anchors. The rough token counts are the likely cause (a cache-read average of 70k across all of L's requests is probably high), so the table uses the anchors, which are dollars measured from `claude -p`. Which cache TTL `claude -p` uses changes only this cross-check, not the anchors. The uncertainty goes into the total as an upper bound.

**Table** (5 runs per cell):

| Block | Cells | Runs | Cost |
|---|---:|---:|---:|
| Sonnet, S1 + S2 | 6 arms × 2 | 60 | 5 × (1.04 + 2.12) = **$16** |
| Sonnet, L | 6 arms | 30 | 5 × 16.70 = **$84** |
| Haiku, S1 + S2 | 6 × 2 | 60 | ≈ 15 % of Sonnet = **$2.5** |
| Haiku, L | 6 | 30 | ≈ 25 % of Sonnet (long-memory ratio) = **$21** |
| Smoke tests, retries, failed runs | | | +15 % = **$19** |
| **Total** | 36 cells (6 arms × 3 tasks × 2 models) | 180 | **≈ $145**, at most about **$200** if gentle-ai and OpenSpec cost twice the assumption or L runs 30 % over |
| Optional Opus, L, 3 arms | 3 | 15 | 15 × ~$5.2 = **$78** |

- **mem0's extraction:** $0 if the runs fall inside Mem0's free 30 days. At the BYOK rate it is about $1.50: 10 L runs × ~30 adds × $0.0045, plus the small tasks.
- **mem0's retrievals:** only the mem0 arm makes them, about 15 per L run and 2 per small run. That is about 190 in all, well under the free plan's 1,000 a month ([pricing L23][m0-pricing]).

**Pilot** (one run per cell, Sonnet):

| Stage | What | Cost |
|---|---|---:|
| 0 Smoke | Each of the 6 arms: install in its container, check its hooks fire under `claude -p`, write something in session 1 and recall it in session 2, cross the A→B handoff | ~$2 |
| 1 Overhead | S1 × 6 arms | ~$1 |
| 2 Long | L × plain, kontext, gentle-ai, mem0 | ~$11 |
| 3 Haiku | L × plain, kontext on Haiku | ~$1.5 |
| **Pilot total** | | **$15–20** |

### 5.4 How long, on one machine

Estimates, to correct after the pilot: S1/S2 take 2–6 min a run; L takes 30–45 min (19 turns at 60–90 s, the code turns longer, plus the snapshots, the A→B handoff, and settle waits of up to 10 min at two boundaries for mem0).

| Run | Serial | 4 containers at once |
|---|---:|---:|
| Full | 120 small × 4 min + 60 long × 40 min ≈ 48 h | **≈ 12 h** |
| Pilot | ~6 h | **3–4 h** |

API rate limits on the key, not CPU, are the likely ceiling. All arms of one seed run in the same 48-hour window so tool and model versions do not drift.

---

## 6. Isolation and fairness

### 6.1 Isolation: one container per run, never Omar's real configuration

A throwaway `HOME` alone is not enough:
- gentle-ai's installer edits shell rc files ([README L289][ga-readme-289]). Whether it calls a package manager for Claude Code is to verify in the pilot: its README says it never installs an agent itself;
- gentle-ai ignores `CLAUDE_CONFIG_DIR` (from source);
- claude-mem's worker port is derived from the uid;
- mem0's memory is keyed by the repo's remote and the user, on a hosted service;
- the existing kontext bench keeps the real `HOME`.

So every run is a fresh container.

- **Images.** One base image (Debian slim, Python 3.11, git, Node 22, the Claude Code CLI at one pinned version). One layer per arm installs its tool at a pinned version or commit during `docker build`, so installers edit files inside the image only. Image digests go in every `run.json`.
- **One long-lived container per machine per run.** It starts with `docker run -d … sleep infinity`, every turn is a `docker exec … claude -p …` into it, and it is removed when the run ends. Background workers that outlive a turn, such as mem0's flush worker and kontext's distill, then survive between turns as they would on a real machine.
- **Environment of every run:**
  ```
  docker run -d --name bench-<run>-a --network bench-<run> \
    -e HOME=/home/bench \
    -e CLAUDE_CONFIG_DIR=/home/bench/.claude \
    -e KONTEXT_CONFIG_DIR=/home/bench/.config/kontext \
    -e CLAUDE_CODE_EFFORT_LEVEL= \
    --env-file <host 0600 file with ANTHROPIC_API_KEY only> \
    -v <results dir for this run>:/out \
    bench-<arm>@sha256:… sleep infinity
  ```
  `CLAUDE_CONFIG_DIR` is set to `$HOME/.claude`, so tools that ignore the variable (gentle-ai) and tools that read it (Claude Code, Engram, kontext) all write to the same throwaway folder.
- **Host.** Nothing on the host is mounted except the run's own `/out`. Never `~/.claude`, `~/.config`, `~/kontext-home`, `~/.cargo/bin` or any repo. The harness on the host only runs `docker` and copies results out. Docker is installed here (`/opt/homebrew/bin/docker`, presence checked only); the daemon and its resources are to verify in the pilot.
- **Model auth.** `ANTHROPIC_API_KEY` reaches the container by `--env-file` only. It is never baked into an image or written into the container's config. The agent can still read its own environment, so the key must be one made only for the benchmark, with a spend cap. Transcripts are scanned for it before anything is published. It is the only `ANTHROPIC_*` variable, so no `ANTHROPIC_BASE_URL` or `ANTHROPIC_AUTH_TOKEN` leaks in from a Claude Code session. kontext's distill and claude-mem's observer, if ever used, inherit it from the environment. Which credential each call actually used (`apiKeySource` in the init message of `--output-format stream-json`) is to verify in the pilot, and recorded per run. Omar provides the key; this design does not ask for one.
- **Hosted state (mem0).**
  - Each run gets `MEM0_USER_ID=bench-<run>` and a unique remote URL for its repo (`file:///remotes/<run>.git`), so its shared project memory has its own `agent_id`. Whether a `file://` remote feeds the hash is to verify in the pilot; the fallback is a fake `git@example.invalid:bench/<run>.git` remote.
  - After the run, its memories are exported into `/out` and deleted (`/mem0:forget --include-project-memory` or the API).
  - The Mem0 key goes in by `--env-file` like the Anthropic key.
- **Second machine.** Container B is created at the day-4 boundary from the same image, on the same private network as A, the bare git remote and the kontext sidecar. Nothing is copied between A and B except through each arm's own sync path (§2.3) and the git remote.
- **Network.** Containers reach the Anthropic API, Mem0 (mem0 arm), the git remote and the sidecar. Not the deployed kontext server: no `kontext.omarespinosafernandez.workers.dev` URL appears anywhere in the harness.

### 6.2 Threats to fairness and how the design limits each

1. **We wrote kontext, the tasks and the harness.**
   - Fixtures, scripted turns, hidden checks and the analysis rules (§5.2) are frozen in a commit before the pilot, and that hash is checked at every run.
   - L's requirement classes follow kontext's own earlier `long-memory` design, but the values and wording are new, so nothing was tuned on kontext's past failures.
   - The requirement list and hidden checks are written by an agent given only §2.2's class table and the fixture, not kontext's docs.
   - Publish everything: scripts, checks, transcripts, per-run JSON.
   - Optionally, show each competitor's arm setup (§1.2) to its maintainers before the runs. That is outward-facing, so Omar decides.
2. **Default against tuned.** Every arm runs its documented default for Claude Code (§1.2), kontext included: capture off, distill on, `inject`, no seeded charter. Every change needed to run headless is listed and labelled in the results. If a tuned column is ever added, it is added for every arm, each tuned by its own docs' recommendations.
3. **Single-session tools against a multi-day project.**
   - L is built around kontext's own claim. Most of these tools are built for it too (all advertise cross-session memory), but OpenSpec and Beads are spec and task tools, not conversation memory.
   - S1 and S2 are where kontext should look worst (overhead with nothing to remember), and they are reported with the same weight.
   - There is no composite score, and results are reported per task, so no single number hides that trade.
4. **Asynchronous tools are timed out unfairly.** A settle step per arm before every boundary (§2.3), with its wait recorded. The settle cap (10 min) is generous against mem0's documented 5-minute idle flush.
5. **Headless quirks.**
   - Permissions differ per tool, so each gets its own allowed list, and denials are reported, not hidden.
   - `SessionEnd` fires at the end of every `claude -p` call, so every turn ends a session. kontext's distill and mem0's flush run once per turn, not once per session as for an interactive user. That inflates their call counts against real use, and the pilot measures by how much.
   - An arm that cannot run headless at all is reported as "does not run headless" with the error, not dropped.
6. **The scripted user answers in one fixed way.** Tools that ask the human more (kontext's signatures, gentle-ai's "Needs your decision") get the same canned "go ahead", and the count shows that cost.
7. **Version drift and service changes.** Tool versions are pinned and recorded, the Claude Code CLI is pinned, and every arm of a seed runs within 48 hours. mem0's hosted extractor can change underneath. Its plugin version and the date are recorded, and this is stated as a limit.
8. **Model judge bias.** None is used. Every check is a test that runs the code.

---

## 7. What to build

**Where.** `bench/compare/` in this repo for now. It reuses `bench/benchlib`'s turn loop, envelope accounting, `--resume` cost differences, transcript capture and `aggregate.py`. DESIGN §15 already plans a separate `kontext-bench` repo before publishing, and this benchmark should move there before anything is published: a comparison is more credible outside the subject's repo.

| Piece | What | Rough size |
|---|---|---|
| `compare/images/` | Base Dockerfile + one per arm (pinned installs), build script printing digests | 7 files, ~250 lines |
| `compare/arms/<arm>/` | `setup.sh` (pre-day-1, inside the container), `handoff_a.sh` / `handoff_b.sh` (§2.3), `settle.sh`, `allowed_tools.txt`, `teardown.sh` (mem0 export and delete) | 6 × ~80 = ~500 lines |
| `benchlib/container.py` | Run a scenario in container A, then B: start containers, the network, the bare remote and the kontext sidecar (`node server/dev.mjs`); an `--agent-cmd` that is `docker exec … claude -p …`; copy transcripts out; the verification container | ~450 lines |
| `compare/run.py` | CLI: arms × tasks × models × seeds, `--dry-run`, `--yes`, `--jobs`, refuses without `--yes` like `run.py` | ~200 lines |
| Scenarios | `small-due-date` and `small-rounding` (repo + 1 turn + hidden tests), `project-workshop` (repo ~400 lines, script TOML ~350 lines, `acceptance/test_requirements.py` ~300 lines, requirement → test map) | ~1,500 lines incl. fixtures |
| Interventions | The fixed-reply rule in the turn loop: detect "asked instead of acting" (no diff and a question or approval request), send the canned reply, count | ~120 lines |
| Measures | Per-requirement and per-class survival, confusions, interventions, denials, off-transcript costs (mem0 adds, distill), `kontext measure --json` breakdown, `aggregate.py` tables per task × model × arm | ~350 lines |
| `compare/friction/` | Two-container script per setup that runs the terminal steps of §3 and logs each as command or manual, executed or doc-only, plus the table generator | ~350 lines |
| Tests | Dry runs with `fake_agent.py` for plain, kontext, beads and openspec (no model); adapter unit tests; a test that the hidden checks never enter an image or the agent container | ~400 lines |

**Total:** about 4,000 lines including fixtures, and about 2,500 of harness code. Roughly 3–5 days of agent work, then the $15–20 pilot.

---

## 8. To verify in the pilot

1. Each tool installs non-interactively at a pinned version inside the image:
   - gentle-ai's default preset with flags, and whether it installs Engram itself;
   - `claude plugin install … --config` for mem0;
   - Dolt at the pinned version for Beads;
   - `openspec init --tools claude`.
2. Each tool's hooks, plugins, MCP servers and skills load and fire under `claude -p` with `CLAUDE_CONFIG_DIR=$HOME/.claude`.
3. `apiKeySource` shows the API key on every call: agent, distill, any observer.
4. `SessionEnd` fires per `claude -p` call, and how many distills and flushes that causes per L run.
5. mem0:
   - the exact MCP tool names for `--allowedTools`;
   - `agent_id` for a `file://` remote;
   - `MEM0_USER_ID` honored;
   - how to detect "no pending flush" for the settle step;
   - where the add count can be read;
   - deletion works;
   - retrieval count per run against the free limit.
6. gentle-ai:
   - that it writes only under `$HOME` in the container;
   - whether it spawns subagents or a review on S1;
   - its telemetry switch.
7. Engram git sync of chunks from A to B; Beads `bd bootstrap` from a `file://` remote.
8. OpenSpec's skills get used when the turns never name them (if never, report it, do not prompt for it).
9. kontext:
   - the Miniflare sidecar reached from both containers;
   - B joins with a token;
   - the distill works with the API key in the container;
   - the permission denials are gone.
10. Whether the `claude -p` envelope's cost includes subagents, and which cache TTL it uses (±25 % on L's cost).
11. Wall time per L run and the Docker daemon's capacity for 4 containers at once on this machine; the API key's rate limit.
12. The friction steps marked "doc-derived" in §3, executed in two containers (account steps counted only). kontext's real `server deploy` on a throwaway Cloudflare account, only if Omar agrees.

---

## Sources (commit read in brackets)

kontext: README.md, DESIGN.md §14–15, `install.sh`, `src/server.rs`, `src/measure.rs`, `src/config.rs` (template), `bench/README.md`, `bench/reports/2026-11-04-*.md`, `2026-11-05-confirmation.md`, `server/dev.mjs`, all at f778015.

[kx-dev]: https://github.com/OmarAlex24/kontext/blob/f7780153d5036515a6f7a402762e6957e6e987c0/server/dev.mjs#L17-L27
[m0-changelog]: https://github.com/mem0ai/mem0/blob/f1791daa86d469249c9b002f62644d58a1eb96a0/docs/changelog/platform.mdx#L296
[m0-mcp]: https://github.com/mem0ai/mem0/blob/f1791daa86d469249c9b002f62644d58a1eb96a0/docs/platform/mem0-mcp.mdx
[m0-pricing-46]: https://github.com/mem0ai/mem0/blob/f1791daa86d469249c9b002f62644d58a1eb96a0/docs/platform/pricing.mdx#L46
[cm-readme-273]: https://github.com/thedotmack/claude-mem/blob/b3aa095e78865e2aca94a08a97be8196dcdd731c/README.md#L273-L291
[tm-readme-214]: https://github.com/eyaltoledano/claude-task-master/blob/c0c98d367c55296bfe69e65680625b6db437af02/README.md#L214
[gr-mcp-251]: https://github.com/getzep/graphiti/blob/3eafcc8118bbb4d97370166c03bb3c398b6e8f75/mcp_server/README.md#L251
[ga-readme-bench]: https://github.com/Gentleman-Programming/gentle-ai/blob/f2f7d7b16c2cf1afe5e07f717cf8265946ac3feb/README.md#L189
[ga-readme-review]: https://github.com/Gentleman-Programming/gentle-ai/blob/f2f7d7b16c2cf1afe5e07f717cf8265946ac3feb/README.md#L214
[ga-readme-289]: https://github.com/Gentleman-Programming/gentle-ai/blob/f2f7d7b16c2cf1afe5e07f717cf8265946ac3feb/README.md#L279-L289
[ga-nonint]: https://github.com/Gentleman-Programming/gentle-ai/blob/f2f7d7b16c2cf1afe5e07f717cf8265946ac3feb/docs/non-interactive.md
[ga-components]: https://github.com/Gentleman-Programming/gentle-ai/blob/f2f7d7b16c2cf1afe5e07f717cf8265946ac3feb/docs/components.md
[ga-usage-426]: https://github.com/Gentleman-Programming/gentle-ai/blob/f2f7d7b16c2cf1afe5e07f717cf8265946ac3feb/docs/usage.md#L426
[eng-readme-128]: https://github.com/Gentleman-Programming/engram/blob/8a4ce2d948ca0abebdc36b1021456bac67845512/README.md#L128
[eng-readme-130]: https://github.com/Gentleman-Programming/engram/blob/8a4ce2d948ca0abebdc36b1021456bac67845512/README.md#L130
[eng-docs-sync]: https://github.com/Gentleman-Programming/engram/blob/8a4ce2d948ca0abebdc36b1021456bac67845512/DOCS.md#L1934
[eng-docs-autosync]: https://github.com/Gentleman-Programming/engram/blob/8a4ce2d948ca0abebdc36b1021456bac67845512/DOCS.md#L2244
[eng-cloud]: https://github.com/Gentleman-Programming/engram/blob/8a4ce2d948ca0abebdc36b1021456bac67845512/docs/engram-cloud/README.md
[eng-cloud-https]: https://github.com/Gentleman-Programming/engram/blob/8a4ce2d948ca0abebdc36b1021456bac67845512/docs/engram-cloud/quickstart.md#L326
[eng-agent-setup]: https://github.com/Gentleman-Programming/engram/blob/8a4ce2d948ca0abebdc36b1021456bac67845512/docs/AGENT-SETUP.md#L309
[m0-cc]: https://github.com/mem0ai/mem0/blob/f1791daa86d469249c9b002f62644d58a1eb96a0/docs/integrations/claude-code.mdx
[m0-cc-23]: https://github.com/mem0ai/mem0/blob/f1791daa86d469249c9b002f62644d58a1eb96a0/docs/integrations/claude-code.mdx#L23-L25
[m0-cc-46]: https://github.com/mem0ai/mem0/blob/f1791daa86d469249c9b002f62644d58a1eb96a0/docs/integrations/claude-code.mdx#L46
[m0-cc-92]: https://github.com/mem0ai/mem0/blob/f1791daa86d469249c9b002f62644d58a1eb96a0/docs/integrations/claude-code.mdx#L92
[m0-cc-144]: https://github.com/mem0ai/mem0/blob/f1791daa86d469249c9b002f62644d58a1eb96a0/docs/integrations/claude-code.mdx#L144
[m0-cc-180]: https://github.com/mem0ai/mem0/blob/f1791daa86d469249c9b002f62644d58a1eb96a0/docs/integrations/claude-code.mdx#L180
[m0-pricing]: https://github.com/mem0ai/mem0/blob/f1791daa86d469249c9b002f62644d58a1eb96a0/docs/platform/pricing.mdx#L23
[m0-server]: https://github.com/mem0ai/mem0/blob/f1791daa86d469249c9b002f62644d58a1eb96a0/server/README.md#L20
[m0-src-url]: https://github.com/mem0ai/mem0/blob/f1791daa86d469249c9b002f62644d58a1eb96a0/integrations/claude-code-plugin/core/memory_core.py#L31
[bd-readme]: https://github.com/steveyegge/beads/blob/8cdfa796a8f571266f2687f8b80365cbe26a943d/README.md
[bd-readme-35]: https://github.com/steveyegge/beads/blob/8cdfa796a8f571266f2687f8b80365cbe26a943d/README.md#L31-L45
[bd-sync]: https://github.com/steveyegge/beads/blob/8cdfa796a8f571266f2687f8b80365cbe26a943d/docs/getting-started/sync-setup.md
[bd-config]: https://github.com/steveyegge/beads/blob/8cdfa796a8f571266f2687f8b80365cbe26a943d/docs/reference/configuration.md#L118
[bd-cc]: https://github.com/steveyegge/beads/blob/8cdfa796a8f571266f2687f8b80365cbe26a943d/docs/integrations/claude-code.md
[bd-cli-337]: https://github.com/steveyegge/beads/blob/8cdfa796a8f571266f2687f8b80365cbe26a943d/docs/CLI_REFERENCE.md#L337
[bd-cli-5915]: https://github.com/steveyegge/beads/blob/8cdfa796a8f571266f2687f8b80365cbe26a943d/docs/CLI_REFERENCE.md#L5915
[cm-readme]: https://github.com/thedotmack/claude-mem/blob/b3aa095e78865e2aca94a08a97be8196dcdd731c/README.md#L140
[cm-readme-145]: https://github.com/thedotmack/claude-mem/blob/b3aa095e78865e2aca94a08a97be8196dcdd731c/README.md#L145-L149
[cm-sync]: https://github.com/thedotmack/claude-mem/blob/b3aa095e78865e2aca94a08a97be8196dcdd731c/docs/public/cloud-sync.mdx#L115-L126
[cm-config]: https://github.com/thedotmack/claude-mem/blob/b3aa095e78865e2aca94a08a97be8196dcdd731c/docs/public/configuration.mdx#L71-L106
[os-readme]: https://github.com/Fission-AI/OpenSpec/blob/ea635dd255f8686dcf48626bbcbe0e3fd59b6067/README.md#L132-L150
[os-tools]: https://github.com/Fission-AI/OpenSpec/blob/ea635dd255f8686dcf48626bbcbe0e3fd59b6067/docs/supported-tools.md#L71
[os-cli]: https://github.com/Fission-AI/OpenSpec/blob/ea635dd255f8686dcf48626bbcbe0e3fd59b6067/docs/cli.md#L105
[os-commands]: https://github.com/Fission-AI/OpenSpec/blob/ea635dd255f8686dcf48626bbcbe0e3fd59b6067/docs/commands.md#L57
[sk-readme]: https://github.com/github/spec-kit/blob/2ee2b1e8005905db05f5717300a469b3068b908f/README.md#L70-L82
[sk-install]: https://github.com/github/spec-kit/blob/2ee2b1e8005905db05f5717300a469b3068b908f/docs/installation.md#L126
[tm-readme]: https://github.com/eyaltoledano/claude-task-master/blob/c0c98d367c55296bfe69e65680625b6db437af02/README.md#L81-L106
[lc-readme]: https://github.com/letta-ai/letta-code/blob/57d61bd34f963e686847279f2cccee47a4d9c69e/README.md#L23-L59
[gr-readme]: https://github.com/getzep/graphiti/blob/3eafcc8118bbb4d97370166c03bb3c398b6e8f75/README.md
[gr-mcp]: https://github.com/getzep/graphiti/blob/3eafcc8118bbb4d97370166c03bb3c398b6e8f75/mcp_server/README.md#L10-L17

---

## Changes while building

The harness in `bench/compare/` follows the text above, except where building it showed that the design and reality disagree. Each change keeps every arm on its documented Claude Code default.

**Isolation and keys (§6.1)**

1. **`CLAUDE_CONFIG_DIR` is left unset.** gentle-ai writes its MCP servers to `~/.claude.json`. With `CLAUDE_CONFIG_DIR` set, Claude Code reads `$CLAUDE_CONFIG_DIR/.claude.json` instead, so gentle-ai's `engram` and `context7` servers were invisible. Unset is Claude Code's default, and every tool then writes to the one folder Claude Code reads (`~/.claude`, `~/.claude.json`); `claude mcp list` shows both servers connected. `KONTEXT_CONFIG_DIR` is also unset, which is kontext's default. Dry runs alone set `CLAUDE_CONFIG_DIR=$HOME/.claude`, because `fake_agent.py` writes transcripts only when the variable is set.
2. **Keys go on each `docker exec`, by name, and never on `docker run`.** The design's `--env-file` stores the value in the container's config, where `docker inspect` shows it and the daemon keeps it on disk for as long as the container exists. A test caught the same thing with `-e NAME` on `docker run`. So the harness passes `-e ANTHROPIC_API_KEY` (and `-e MEM0_API_KEY` for mem0) on every `docker exec` into that machine. The value sits only in the environment of the `docker` CLI process, and the docker process's own environment is stripped of every `ANTHROPIC_*`, `MEM0_*`, `KONTEXT_*`, `CLAUDE_*`, `OPENAI_*` and `CLOUDFLARE_*` variable from the host. Background workers that a hook starts (mem0's flush, kontext's distill) inherit the key from their exec.
3. **No `/out` bind mount.** Nothing on the host is mounted, not even the results folder. Files move as tar streams over `docker exec … tar`. The bare git remote is a named volume (`bench-<run>-remotes`) shared by A and B.
4. **The kontext sidecar** runs from an image built off `git archive v0.2.0 server`, through a launcher (`images/sidecar.mjs`) that imports `dev.mjs`'s options and listens on `0.0.0.0:8787`. `dev.mjs` binds to localhost, which the other containers cannot reach. Its admin secret is random per run, and since the server needs it at start, it is the one secret passed at `docker run` (it is local and dies with the run).

**Arms (§1.2)**

5. **mem0:**
   - plugin 0.3.3, not 0.3.1; the marketplace is pinned by git URL and commit (`https://github.com/mem0ai/mem0.git#<ref>`, with `--sparse .claude-plugin integrations`);
   - no `plugin install --config api_key=…`, which would write the key into the container's files: the plugin reads `MEM0_API_KEY` from the environment first (`core/memory_core.py`), so the key goes in by name like the Anthropic key;
   - `--allowedTools` allows mem0's MCP tools by server name, `mcp__plugin_mem0_mem0` (a bare server name allows all its tools, per Claude Code's permission docs), instead of the design's `mcp__plugin_mem0_*`. gentle-ai's are allowed the same way: `mcp__engram`, `mcp__plugin_engram_engram`, `mcp__context7`. Whether the pattern form would also work at 2.1.325 was not tested.
6. **gentle-ai:**
   - the preset is `full-gentleman`, which is both the TUI's preselection and the CLI's default (`internal/cli/validate.go`);
   - it installs Engram itself (3.7.2), so there is no separate `engram setup`;
   - it is installed at image build, with `GENTLE_AI_NO_SELF_UPDATE=1` and `gentle-ai telemetry disable`.
7. **Beads:** Dolt is pinned to 2.2.0, and `bd metrics off` is set. `bd init` commits on its own, so the setup does not commit after it.
8. **Telemetry and updates off in every image:** `DISABLE_AUTOUPDATER=1` (Claude Code), `MEM0_TELEMETRY=false`, `OPENSPEC_TELEMETRY=0`, gentle-ai's switch, and `bd metrics off`. None of these changes what the agent sees.
9. **kontext's distill command is wrapped**, as in `bench/README.md`. `setup.sh` rewrites `[distill] command` in the project's `project.toml` to `distill_cmd.py --out $BENCH_OUT/distill -- <the default command>`, so the distill's own envelopes (its cost) are kept. The model and the prompt stay kontext's defaults.
10. **Permission mode `acceptEdits`**, the same for all arms, as in the existing bench. The agent call is `claude -p --output-format stream-json --verbose --include-hook-events`. The init message (`apiKeySource`, MCP servers, plugins) and the hook events are recorded, and cost is the session delta of the result envelope's `total_cost_usd` / `modelUsage`.

**Tasks (§2)**

11. **The `workshop` fixture** is 448 lines over 7 modules (the design said about 400 over 5), plus a `join_waitlist` stub so that R13 has a function to land in. It has 17 visible tests: one fails on purpose (the planted listing-order bug) and one is skipped.
12. **Confusion probes.** Besides the 14 requirement checks, `acceptance/test_confusions.py` holds 8 probes. Each one passes only when the code took a distractor or a superseded value (10 per class, Mondays, 72 h, comma CSV, the old 48 h, …), so a lost requirement can be told apart from a confused one.
13. **The fill turns** are two `fill.py` filler turns of 25,000 tokens each (seeds 501 and 502), using `fill.py`'s measured 2.1 characters per token.
14. **A settle step runs before every new session**, not only at the day-3 and day-4 boundaries: kontext `distill --wait`; mem0 waits until `evidence.sqlite3` has no pending flush. Each wait is recorded apart from session time.

**Measures (§4)**

15. **`kontext measure --json`** runs over each run's transcripts in a network-less `bench-kontext` container. The host has no kontext binary, and the brief installs nothing on the host.

**Friction (§3)**

16. Re-executed in two fresh `bench-base` containers (`friction/REPORT.md`):
    - Beads' installer exits 1 ("bd was installed but is not in PATH") until `~/.local/bin` is on PATH;
    - claude-mem's installer needs `unzip`, which a slim Debian image lacks: one more manual step;
    - gentle-ai's installer edits the shell rc;
    - on Debian, a new login shell already puts `~/.local/bin` on PATH (`~/.profile`), so the PATH steps for kontext, Beads and gentle-ai were not needed there.

## §8, settled now or still open

Settled means it was checked here without a model call. Open means only the pilot can settle it.

1. **Non-interactive installs at pinned versions: settled.** Every image builds headless, with each version asserted in the build: gentle-ai `full-gentleman` (it installs Engram itself), the mem0 plugin (without `--config`, see change 5), Dolt 2.2.0, `openspec init --tools claude`.
2. **Hooks, plugins, MCP servers and skills load under `claude -p`: partly.**
   - Settled: gentle-ai's `engram` and `context7` MCP servers show as connected in `claude mcp list`, and the plugins and skills are in place in each image.
   - Open: that hooks fire under `claude -p` needs a model call. The harness records hook events per turn (`--include-hook-events`).
3. **`apiKeySource` on every call: open.** It is recorded per call from the init message.
4. **`SessionEnd` per `claude -p` call, and the distills and flushes per L run: open.** Both are recorded (hook events, distill envelopes, mem0's evidence).
5. **mem0:**
   - MCP tool name: settled by the server-name rule, `mcp__plugin_mem0_mem0`; open whether the plugin's server registers under that name;
   - `MEM0_USER_ID` and `agent_id` for a `file://` remote: set per run, and read from the plugin's source; open on the hosted service;
   - "no pending flush": `arms/mem0/evidence.py`, from the plugin's own `evidence.sqlite3`; open against real flushes;
   - add count and retrievals: read from the same file; open;
   - deletion: `teardown.sh` exports, then runs `memory_cli.py forget --remote --yes --include-project-memory`; open.
6. **gentle-ai:**
   - writes only under `$HOME`: settled for the install (outside `/home` and `/tmp`, the file lists of `bench-base` and `bench-gentle-ai` are identical; its binaries go to `~/.local/bin`);
   - telemetry: settled (`gentle-ai telemetry disable`);
   - subagents or a review on S1: open.
7. **Engram git sync A→B and Beads `bd bootstrap` from a `file://` remote: settled.** Both ran in the friction containers, and B saw what A saved.
8. **OpenSpec's skills get used without being named: open.** The turns never name a tool (a unit test checks this).
9. **kontext:**
   - the sidecar reached from A and B, and B joins with a token: settled (dry runs and the friction run);
   - the distill with the API key in the container: open (dry runs use a fake distill model);
   - permission denials: open, counted per turn.
10. **Cost includes subagents: settled from earlier evidence, to reconfirm.** The 2026-11-06 reader report shows that the envelope's `modelUsage` includes the subagents' models (Claude Code 2.1.318); the pin is 2.1.325. Cache TTL: open.
11. **Wall time per L run, 4 containers at once, the key's rate limit: partly.**
    - Settled: a full dry run of all tasks × 6 arms ran with `--jobs 3` (1.5 GB and 2 CPUs per container).
    - Open: the real wall time and the rate limit.
12. **Friction steps re-executed: settled** (`friction/REPORT.md`). The steps that need an account (Cloudflare, Mem0, cmem.ai) are counted, not executed; kontext's real `server deploy` was not run.
