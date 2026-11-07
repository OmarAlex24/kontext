# kontext benchmark

The harness that measures how much a project drifts with and without kontext (DESIGN §15). It lives in this repo for now; before publishing it can move to a separate repo, `kontext-bench`.

> **Warning: a real run spends tokens.** Each variant and each seed opens one agent session per day and sends it every turn, the corrections and the memory questions; with the memory judge, one more call per question. The `billing` scenario with 4 variants and 3 seeds is about 120 calls to the agent. `run.py` refuses to run without `--dry-run` or `--yes`. Running and publishing the benchmark is outside phases 1 to 6 (DESIGN §19): it is confirmed first.

## How to run it

You need Python 3.11 or newer (standard library only), `git`, and the `kontext` binary (`cargo build`; the harness looks for `--kontext-bin`, `$KONTEXT_BIN`, `target/debug/kontext`, `target/release/kontext` and then the `kontext` on the `PATH`).

```sh
# Dry run: the agent is bench/fake_agent.py; it calls no model and costs nothing
python3 bench/run.py --dry-run --seeds 2

# The harness tests (they run the same dry, with 2 seeds)
python3 -m unittest discover bench/tests

# For real (spends tokens)
python3 bench/run.py --yes --seeds 3 --model claude-sonnet-5 \
  --judge-cmd "claude -p --model haiku --output-format json"
```

| Option | What it does |
|---|---|
| `--scenario <name or path>` | Scenario in `bench/scenarios/` (default `billing`; also `long-memory`) |
| `--variants a,b` | `none`, `automem`, `notes`, `kontext-pointer`, `kontext-inject` (default, all five). The `kontext-*` ids are the keys of the results that `aggregate.py` compares, and the `kontext` key of each `result.json`. |
| `--seeds N` | Runs per variant (3) |
| `--dry-run` | Uses `bench/fake_agent.py` and the keyword judge |
| `--agent-cmd "<cmd>"` | The agent; it gets each turn on stdin. Default `claude -p --output-format json --permission-mode acceptEdits --allowedTools …` (the permissions, below under "Clean environment") |
| `--model <m>` | Added to the agent command as `--model <m>` |
| `--judge-cmd "<cmd>"` | Memory judge: gets the prompt on stdin and answers `{"score": 0 or 1, "forbidden": true or false}` (also inside the envelope of `claude -p --output-format json`). Without it, keywords |
| `--turn-timeout S` | Seconds per turn (900) |
| `--keep-work` | Does not delete each run's temporary repo (its path is kept in `run.json`) |
| `--fill-scale X` | Multiplies the size of the filler turns (1.0; `0.1` for cheap pilots) |
| `--distill-cmd "<cmd>"` | Distill model of kontext when the scenario turns it on (default `claude -p --model claude-haiku-5-5 --output-format json`; in dry runs, `bench/fake_distill.py`) |
| `--distill-wait S` | Most seconds to wait for the distill to finish before the next turn (900) |
| `--out <folder>` | Where to leave the results (default `bench/results/`, which is not versioned) |
| `--yes` | Confirms a real run |

`acceptEdits` lets the agent edit files; the commands it can run without asking are the ones in `--allowedTools` of the default command (the tests, `kontext` and reading). If you change the command with `--agent-cmd`, do it the same way in all the variants.

## What a run does

For each variant and seed:

1. Copies the scenario's `repo/` to a temporary folder, `git init` and one commit.
2. Prepares the variant:
   - `none`: nothing.
   - `automem`: nothing, but with Claude Code's automatic memory turned on. In the other variants it is turned off (`CLAUDE_CODE_DISABLE_AUTO_MEMORY=1`), so a memory that is not the one being measured does not leak in.
   - `notes`: a `CLAUDE.md` with the charter and the decisions written by hand, and the instruction to keep it up to date. The agent may edit it.
   - `kontext-pointer` and `kontext-inject`: their own `kontext-home` and `projects.toml` in the temporary folder (`KONTEXT_CONFIG_DIR`; it never touches `~/.config/kontext`), with `[context]` set to `pointer` or `inject`; `kontext install` in the repo and, as owner, the charter, the goals and the decisions with their guard, signed. The agent runs with `KONTEXT_ROLE=pm`. `KONTEXT_TRANSCRIPTS_GLOB` points at the Claude Code transcripts of this repo (the run's), so that `kontext recall` sees neither the user's nor other runs'. The distill is off, unless the scenario says `[bench] distill = true` (below).
3. One commit with that: it is the starting point everything is measured against.
4. Each day is a **new session** of the agent (so context is lost between days). Within the day, each turn continues the same session (`--resume <session_id>`, with the `session_id` of the JSON envelope).
5. On a temptation turn, it checks before and after whether the agent did what was asked: with `decision:<id>`, whether that decision's guard passed and now fails; with `scope`, whether the lines outside the charter grew. If it did, the turn's `correction` (or a generic one) is sent and counts as a correction. A correction reminds; it does not order an undo: whether the agent undoes the change or not is also measured.
6. The memory questions go where the scenario puts them (turns with `kind = "probe"`) or at the end of their day (`[[probes]]`), in the same session.
7. At the end: the changed lines, the guards, and the acceptance tests (they are copied to `tests/` at that moment; the agent did not see them).

The variants get the same turns. The first turn of day 1 puts the charter and the decisions in the conversation, so `none` also hears them once.

### Clean environment and API billing

A run that gets published must not see the user's configuration or spend their subscription. Run the harness with an empty environment, its own Claude configuration and the API key:

```sh
env -i HOME="$HOME" PATH="$PWD/target/release:$HOME/.local/bin:/usr/bin:/bin" LANG=C.UTF-8 \
  CLAUDE_CONFIG_DIR="$HOME/.cache/kontext-bench/claude-config" \
  ANTHROPIC_API_KEY="$(cat clave.txt)" \
  python3 bench/run.py --yes --seeds 5 --model claude-sonnet-5-5 --variants none \
    --judge-cmd 'claude -p --model claude-haiku-5-5 --output-format json' --out bench/results/x/sonnet/none
```

`env -i` also removes `ANTHROPIC_BASE_URL` and `ANTHROPIC_AUTH_TOKEN`, which a Claude Code session may have set.

The default agent command (`DEFAULT_AGENT_CMD` in `benchlib/runner.py`) already carries `--allowedTools` with `AGENT_ALLOWED_TOOLS`, the same for all variants. In `claude -p` nobody approves anything: what is not there does not run. The `kontext-*` variants need `Bash(kontext:*)`, but that permission only covers commands that start with `kontext`. In long-memory (2026-11-04) the agents also wrote `KONTEXT_ACTOR=… kontext …`, `export KONTEXT_ACTOR=…` and on the next line `kontext …`, `timeout 100 kontext …`, and `kontext resume … | sed -n …` / `| head` / `| tail`, and Claude Code asked them for approval. So the list carries `Bash(KONTEXT_* kontext *)`, `Bash(export KONTEXT_*)`, `Bash(timeout * kontext *)`, `Bash(head:*)`, `Bash(tail:*)`, `Bash(sed -n:*)` and `Bash(echo:*)`, plus `python3`, `git`, `ls`, `cat`, `grep`, `wc`, `sort` and `uniq`. kontext also tells the agent how to call it (the hook, `resume` and the skill): bare, with the identity in `--actor`/`--role`. With `--agent-cmd` the whole command is replaced, permissions included. The denials of each turn are kept in `envelope.permission_denials` of the transcripts. To run models and variants in parallel, one process for each with its own `--out`, and then `python3 bench/aggregate.py bench/results/x` puts everything into one table per model and variant.

## What is measured

| Measure | Calculation |
|---|---|
| Outside the charter (lines) | Changed lines (added + removed, `git diff --numstat` against the starting point, with new files) in files that are not in the `scope` of any goal. Counted per file, not per piece |
| Failing guards % | Guards of the decisions that fail at the end (`sh -c <guard>`, expected to exit with 0) |
| Memory % | Memory questions answered right: with all the ideas in `expected` and without asserting as current any of `forbidden`. The judge is a model (`--judge-cmd`); in dry runs, or without a judge, an answer counts if it has all the expected words and none of the forbidden ones, as whole words, ignoring case and accents, with numbers normalized ("1,5 %" = "1.5%", "3,250" = "3250"). `run.json` keeps both grades when there is a judge, to compare with a hand-graded sample. Also by class (A, B, C, cross) and by checkpoint |
| Confusions | Answers that asserted a forbidden idea (the old value of a fact that changed, a distractor), per run |
| Corrections | Scripted "we already decided that" turns that were needed (step 5) |
| Tokens and cost | What each call spent (input, output and cache, and dollars), plus the judge and kontext's distill. With `--resume`, the `claude -p` envelope carries in `total_cost_usd` and `modelUsage` the **running total of the session**, so each call counts its difference from the previous call of the same session (and so `/compact` is included too, which does not appear in `usage`). `run.json` splits them into `usage_by` (`agent`, `judge`, `distill`). In dry runs, 0 |
| Done % | Acceptance tests that pass (from the `unittest` output; if the command is not `unittest`, 100 or 0 according to its exit code). With `[acceptance] label` it gets a different name: in `long-memory` it is **Behavior %** |

The paths in `scope.ignore` (notes, `.kontext.toml`, `.claude/`, caches) count neither inside nor outside the charter.

### The drift formula

Fixed before any real run; it is not changed after seeing results (DESIGN §15). It is in `bench/benchlib/metrics.py` (`desvio_pct`, `GUARD_WEIGHT`):

```
drift % = 100 × (L_out + W × G_failing) / (L_total + W × G_total)
```

- `L_out`: changed lines outside the charter; `L_total`: all the changed lines (without `scope.ignore`).
- `G_failing`: guards that fail at the end; `G_total`: all the scenario's guards.
- `W = 50`: how many lines a guard weighs. A violated decision is a big drift even if it touches few lines; 50 lines is a median function.
- If nothing changed and there are no guards, the drift is 0.

Example: 20 lines outside of 100, and one of two guards fails → 100 × (20 + 50) / (100 + 100) = 35 %.

## Results

`bench/results/<date>-<scenario>[-dry]/`:

- `report.md`: the table, one row per variant, mean ± standard deviation over the seeds.
- `report.json`: the same in JSON.
- `runs/<variant>-s<seed>/run.json`: all the measures of the run (files outside the charter, each guard, each question with its answer, usage by token type, the acceptance output).
- `runs/<variant>-s<seed>/transcripts/day-N.jsonl`: each call to the agent (turn, correction, question, filler or compaction) with the command, the raw JSON envelope and stderr. For a filler turn the request is saved, not the document: its `sha256`, its characters, the seed and the distractors (the text comes out the same with `benchlib/fill.py`).
- `runs/<variant>-s<seed>/transcripts/claude/*.jsonl.gz`: the full Claude Code sessions of that run (the subagents' in their subfolders), compressed with gzip. Without this they only stay in the shared `CLAUDE_CONFIG_DIR`, mixed with other runs'. From them comes `cache` in `run.json`: requests, prompt cache breaks (a request that reads more than 2,000 tokens less than the previous request's prefix, without compacting between them; without subagents), tokens that were written again and, of those, the ones that arrived after more than 5 minutes (the cache expired). See DESIGN §9.5.
- `runs/<variant>-s<seed>/distill/*.json`: with the distill on, the envelope of each call to the distill model (which counts toward the cost).
- `bench/aggregate.py <folder>` joins several runs into one table per scenario, model and variant, plus one by memory class and one by checkpoint (`--dry` includes the dry runs). With `--cost`, a table of the prompt cache breaks per run and another of the cost by turn type (work, which includes the corrections; fill; probe; compact): calls and USD per run, and per call the cache read and created, the output and the model's internal turns (`num_turns`). It comes from the envelopes of the transcripts, with each call's difference from the previous call of its session, like the total cost.

## Scenario format

`bench/scenarios/<name>/scenario.toml`, plus `repo/` (the starting code) and the acceptance tests folder. See `scenarios/billing/scenario.toml`, commented.

| Section | What it holds |
|---|---|
| `name`, `description` | |
| `[charter]` | `title`, `objective`, `non_goals[]`, `done_when` |
| `[[goals]]` | `id` (local to the scenario), `title`, `done_when`, `scope[]`: path globs that serve that goal (`**` crosses folders) |
| `[scope] ignore[]` | Paths that don't count |
| `[[decisions]]` | `id`, `title`, `why`, `guard` (command that exits 0 if it holds), `expect` (`exit0`, the default) |
| `[acceptance]` | `files` (folder that is copied to `tests/` at the end), `command`, `timeout` |
| `[[days]]` with `[[days.turns]]` | Each turn: `text`; optional `temptation` (`decision:<id>` or `scope`) and `correction`. With `kind`, other types (below) |
| `[[probes]]` | `day` (asked at the end of that day), `question`, `expected[]` (words for the judge); optionally the same as a question in a turn |
| `[bench]` | `distill = true`: turns kontext's distill on in the `kontext-*` variants |
| `[[memory]]` | The facts that are measured: `id`, `class`, `said` and, if it changes, `changed`. Both sentences have to appear word for word in a turn (the harness checks it); the distill and the made-up agent use them |
| `[[distractors]]` | `id` and `text`: a sentence almost identical to a fact, which the filler plants |
| `[acceptance] label` | The name of the column (default "Done %") |
| `[[fake.rules]]`, `[[fake.answers]]`, `[fake]` | Only for `--dry-run`: what the made-up agent does (below) |

### Turn types

| `kind` | What it does | Fields |
|---|---|---|
| `work` (default) | Sends `text` | `text`, `temptation`, `correction` |
| `fill` | Generates a plausible document (minutes, tickets, logbook, emails or `mixed`) with `benchlib/fill.py`, deterministically, with the distractors planted, and sends `ask` and the document | `ask` (what is asked about the text), `docs`, `tokens` (approximate: 2.1 characters per token, measured with `count_tokens` on the Spanish version of this filler; multiplied by `--fill-scale`), `seed`, `distractors[]` (ids) |
| `compact` | Sends `/compact` in the same session. The harness checks in the Claude Code transcript that a `compact_boundary` was left, and saves its `preTokens` (how full the window was) | |
| `probe` | A memory question at that point of the day | `id`, `class` (`A`, `B`, `C` or `cross`), `checkpoint` (number), `question`, `expected[]`, `forbidden[]`, `note` (context for the judge) |

In `expected` and `forbidden`, `a|b` accepts either form. `forbidden` is what, asserted as current or correct, makes the answer bad: the old value of a fact that changed, or a distractor. Mentioning it as history ("it used to be 1.5 %, now it is 1.8 %") does not count for the model judge; the word judge does count it, which is why in dry runs the made-up answers only state the value in force.

### The distill in the runs

With `[bench] distill = true`, the `kontext-*` variants set `[distill] enabled = true`, and `kontext install` adds the `PreCompact` and `SessionEnd` hooks. Claude Code runs them when compacting and when each `claude -p` ends; each one starts `kontext distill` in the background. Before the next turn the harness runs `kontext distill --wait` (it waits until no mark is left in `.kontext/distill-pending/`), so the next turn or session already sees what was distilled. The time it waited is kept in the transcript (`distill_wait_s`).

`[distill] command` is `bench/distill_cmd.py --out <run>/distill -- <--distill-cmd>`: it runs the model with the same stdin and the agent's environment (inherited from the hook: the same Claude configuration and the same key), and saves its envelope to add to the cost. What the distill wrote is kept in `run.json` (`distill.entries`).

## Scenario `long-memory`

It measures long-term memory: telling the agent something once, filling its window, compacting, opening new sessions, asking it later, and the code it writes at the end using that data.

- **Repo:** the same small billing project (Arteaga Distributors, amounts in cents, Postmark).
- **Facts (`[[memory]]`):** A, the charter and two decisions recorded before day 1. B, five facts said in passing on day 1, mixed with normal work (Ruiz Hardware Store's extension until the 15th, never on Sundays, sent at 9:40, 4 days of grace, the contact at Olvera Building Supplies with its extension). C, two facts from day 1 that change on day 2 (the late-fee rate from 1.5 % to 1.8 % monthly, the threshold from 3,250 to 4,100 pesos). Distractors in the filler: Ríos Hardware Store until the 20th, the North branch on Saturdays, the 9:30 cash cut-off, 7 days of grace in 2023, Itzel Barrera with extension 4471, the 2 % rate of the 2023 draft, 41,000 pesos for legal.
- **Days** (each one, a new session):
  1. Charter, three work turns with the B and the C (old value), 5 fillers of ~20k tokens, `/compact`, **checkpoint 1**: 4 B questions.
  2. **Checkpoint 2**: 4 B questions. The C changes with a work turn, another turn, 5 fillers, `/compact`.
  3. **Checkpoint 3**: 2 A, 2 C, 2 B and 1 cross (an invoice's late fee: the rate in force and the grace days). A work turn, 4 fillers, `/compact`.
  4. Three code tasks that depend on the facts, without repeating them ("with the rate we agreed on"): `late_fee()`, `schedule_send()` and `should_remind()`. **Checkpoint 4**: 2 B, 1 C and 2 crosses.
- **Behavior %:** the tests in `acceptance/test_behavior.py` (9), which depend on the rate in force, the grace days, Sunday, the time, the threshold in force and the extension; the agent's own tests don't count.
- **Filler:** ~100k tokens per session on days 1 and 2, ~80k on day 3 (280k in total with `--fill-scale 1`). With the charter, the work and what Claude Code carries, the window reaches about 140k before compacting.

```sh
python3 bench/run.py --dry-run --scenario long-memory --seeds 1 --fill-scale 0.02
```

**Judge:** with questions like these, `claude-haiku-5-5` as judge is unstable (in the smoke test it graded the same answer differently depending on how long it was). Use sonnet, without the system prompt or the tools of Claude Code, which are ~16k tokens per call (~0.004 USD per question instead of ~0.016):

```sh
--judge-cmd 'claude -p --model claude-sonnet-5-5 --output-format json --tools "" --setting-sources "" --system-prompt "You are the judge of a benchmark. Reply only with the JSON that is asked of you."'
```

The distill, by default, uses haiku with the same bare setup (`--distill-cmd` changes it).

**Cost:** the smoke test (haiku, one seed, `--fill-scale 0.15`, with the filler of that time at 24k per turn) cost 0.12 USD on `none` and 0.35 on `kontext-inject` (0.23 agent, 0.02 judge, 0.10 distill with Claude Code's full system prompt). With `--fill-scale 1` the filler adds about 230k tokens written to the cache and 650k read per run; the estimate is ~0.2–0.5 USD per run with haiku, ~2 with sonnet and ~3–4 with opus. Most of it is the filler: each turn writes ~20k new tokens to the cache and rereads what came before, and compacting reads the whole window. With haiku, what goes past 100k prompt tokens costs 5 times more.

## The made-up agent

`bench/fake_agent.py` is invoked like `claude -p` and returns the same JSON envelope, with usage and cost at 0. It follows the scenario's `[[fake.rules]]`: the first one whose `when` appears in the turn (and whose `if_known` phrases it knows all) writes its files (`write`, with `mode = "append"`, `if_missing` or `only_if`). With `unless_known`, it refuses if any of those words is in what it knows: what was said earlier in the same session since the last compaction, `CLAUDE.md`, and what the `SessionStart` hook that `kontext install` set up prints, which runs on every call as in Claude Code (if the hook is a pointer to `kontext resume`, it runs it). So, without a model, `none` forgets between days and falls into the temptations, and the other variants don't. It answers the questions with `[[fake.answers]]` (the first one whose `when` is in the question; of its `options`, the first whose `known` phrases it knows all) or, if there is none, with the lines of what it knows that share words with the question.

Like Claude Code, it writes its transcript to `$CLAUDE_CONFIG_DIR/projects/<repo>/<session>.jsonl` (in dry runs the harness gives it its own folder), runs `PreCompact` on `/compact` (and leaves a `compact_boundary`) and `SessionEnd` when each call ends. That way kontext's distill really runs, with `bench/fake_distill.py` as its model: it proposes each sentence of `[[memory]]` that it sees in the transcript. `/compact` makes it forget the session except the last `[fake] compact_keeps` turns. With `CLAUDE.md`, it notes there the `[[memory]]` sentences it hears.

The numbers of a dry run only prove that the harness works end to end (preparing, days, measures, table). They say nothing about kontext.

## Known limitations

- With a real `claude` and no `CLAUDE_CONFIG_DIR`, the user's configuration (`~/.claude`, their `CLAUDE.md`, their skills and hooks) gets into all the variants. Use the clean environment above.
- `claude -p` does run the project's `SessionStart` hook (checked with Claude Code 2.1.318).
- The filler asks not to write files, but a real agent may do it anyway; those lines count as "outside the charter".
- The `resume --as pm` view does not show facts (each coordinator sees them). If the distill saves a B fact as a fact and not as a proposal, the `kontext-inject` variant does not get it in the hook and the agent has to look for it (`kontext recall`). That is part of what is measured.
- "Outside the charter" is calculated per file. A change outside the scope inside a file that does serve a goal is not noticed; the guards cover what matters of that kind.

## Reader benchmark

It measures whether a cheaper model can read a project's memory folder for an Opus coordinator, and at what cost. The folder is `.pm/`: `dumps/*.md` (the user's brain-dumps, verbatim and never edited, with material they pasted from elsewhere), `items.md` (the numbered list of ideas with their state: pending, in progress, done or discarded) and `status.md` (a short "where we are"). Every configuration gets byte-identical `.pm/` folders and the same two reading tasks.

| `--configs` | Who reads | |
|---|---|---|
| `opus` | Opus 5.5 (`claude-opus-5-5`) reads `.pm/` itself | default |
| `opus+sonnet` | Opus 5.5 is the main session and delegates the reading to a `pm-reader` subagent on `claude-sonnet-5-5`; Opus gives the final answer | default |
| `opus+haiku` | The same with the subagent on `claude-haiku-5-5` | default |
| `sonnet`, `haiku` | That model reads directly | diagnosis only, not gated |

> **Warning: a real run spends tokens.** The pilot is one seed: the three default configurations × 3 sizes × 2 tasks = 18 calls, roughly 3–10 USD (an estimate; the pilot confirms it). `reader.py` refuses to run without `--dry-run` or `--yes`.

```sh
# Dry run: the reader is bench/fake_reader.py; no model, no cost
python3 bench/reader.py --dry-run --seeds 2

# Several variants into one folder, one report: prompt v1, prompt v2, and v2
# with the subagent at effort high (audit only)
python3 bench/reader.py --dry-run --prompt v1 --sizes m --seeds 2 --out /tmp/reader-v2-dry
python3 bench/reader.py --dry-run --prompt v2 --sizes m --seeds 2 --out /tmp/reader-v2-dry
python3 bench/reader.py --dry-run --prompt v2 --configs opus+sonnet,opus+haiku --sub-effort high \
  --sizes m --seeds 2 --tasks audit --out /tmp/reader-v2-dry

# For real, under the clean environment (above, "Clean environment and API billing")
env -i HOME="$HOME" PATH="$HOME/.local/bin:/usr/bin:/bin" LANG=C.UTF-8 \
  CLAUDE_CONFIG_DIR="$HOME/.cache/kontext-bench/claude-config" \
  ANTHROPIC_API_KEY="$(cat clave.txt)" \
  python3 bench/reader.py --yes
```

| Option | What it does |
|---|---|
| `--configs a,b` | Default `opus,opus+sonnet,opus+haiku`; also `sonnet`, `haiku` |
| `--sizes a,b` | `s`, `m`, `l` (default all three) |
| `--seeds N`, `--first-seed K` | Fixtures per size (1, the pilot) and the first one; to add seeds later, the same `--out` and `--first-seed` |
| `--tasks a,b` | `audit`, `value` (default both) |
| `--out <folder>` | Default `bench/results/<date>-reader[-dry]/`. An existing folder is added to, and the report covers every run in it, whatever its variant. It refuses a folder of real runs for a dry one (and the other way round), and one whose runs read another version of the spec |
| `--reader-cmd "<cmd>"` | The base command. Default `claude -p --output-format json --setting-sources "" --strict-mcp-config`; the harness adds `--model`, `--tools`, `--agents` and `--effort`. Ignored with `--dry-run`, which always uses the made-up reader |
| `--prompt v1\|v2\|skill` | The prompt version (below, "Prompts"). Default `v1`, the pilot's, so earlier commands keep meaning the same |
| `--effort <level>` | The main session's effort (the only session of a direct configuration), passed as `--effort`. `low`, `medium`, `high`, `xhigh` or `max`; default: not passed |
| `--sub-effort <level>` | The `pm-reader` subagent's effort: the `effort` field of its `--agents` entry, which overrides the session's. Delegated configurations only; same levels; default: not passed |
| `--jobs N` | Calls at a time (1) |
| `--timeout S` | Seconds per call (900; raise it for `l`) |
| `--keep-work` | Does not delete each call's temporary folder |
| `--dry-run`, `--yes` | Made-up reader, or confirms a real run |

### The calls

Each call is one fresh `claude -p`, with the prompt on stdin, in a fresh temporary copy of the fixture: calls never share files or a Claude Code project key. `reader.py` inherits the environment, sets `CLAUDE_CODE_DISABLE_AUTO_MEMORY=1` and removes `CLAUDE_CODE_EFFORT_LEVEL` (it would override both `--effort` and the subagent's `effort`; `reader.py` says so when it finds it set, and never sets it).

- Direct: `--model <id> --tools Read,Grep,Glob`; the prompt is the reader's instructions and the task.
- Delegated: `--model claude-opus-5-5 --tools Read,Grep,Glob,Agent --agents '{"pm-reader": {"description": …, "prompt": <the reader's instructions>, "tools": ["Read", "Grep", "Glob"], "model": <subagent id>, "effort": <--sub-effort, if given>}}'`; the prompt tells the main session to delegate the reading of `.pm/` to `pm-reader`, not to read the dumps itself, and to return the final JSON.

The subagent tool's name (`Agent`) and the `--agents` field names are in one constant, `SUBAGENT` in `benchlib/reader_runner.py`. The pilot confirmed `model` with a full id: the subagents' requests ran on `claude-sonnet-5-5` and `claude-haiku-5-5`. Every call also records the effort it really ran at, read from its transcripts (`effort` of the assistant records and `output_config.effort` of the requests, main session and subagent apart; the pilot's ran at `medium` on both sides without `--effort`), and the report warns when it differs from the one asked for.

### Prompts

`bench/reader/prompt.md`, by `## ` section. Every `run.json` records the prompt version, the sha256 of the exact text sent on stdin and of the subagent's prompt, and the sha256 of every section the version uses together (the one the report's header lists for each version).

- **v1** (the pilot's, the default): `Reader` (the future body of a `pm-reader` agent), `Audit task`, `Value task`, `Delegation`. These sections are never edited; a test pins their sha256.
- **v2**: `Reader v2`, `Audit task v2`, `Value task v2` and the same `Delegation`. It tests the pilot's hypotheses: (1) the definition of an idea was too narrow, (2) the readers skimmed instead of going sentence by sentence. Its reader instructions define an idea broadly (anything the user states about the project that the work should do or respect: a feature, a rule, a limit, a number, a default, a preference or a decision, however it is phrased and wherever it appears, one sentence of a paragraph about something else included). The audit task gives a procedure (tell the user's text from the pasted material; go through every sentence and list each idea with its citation; apply later changes and discards; compare each one with items.md by meaning; report the absent ones) and asks for the list too. The value task says that a value can change more than once and that a change can be relative. v2 is generic: a test checks that it has none of the fixture's words, numbers, dates or wording.

- **skill**: the texts `kontext install` ships, read from `templates/` at run time, so a paid run measures exactly what ships. The reader's instructions are the body of `templates/agents/kontext-pm-reader.md` (everything after its frontmatter, the HTML comment included, as Claude Code passes it); the tasks are the `## Audit task` and `## Value task` sections of `templates/skill-kontext-pm.md` (a `## ` line inside a fenced block of the skill is not a heading); the main session gets the same `Delegation`. Its gate is its own: delegated variants against `opus` with `--prompt skill`. The same genericity test as v2 applies to it. What it does not reproduce: the `--agents` entry keeps the bench's name (`pm-reader`), description and full model id (the shipped file says `kontext-pm-reader` and `model: haiku`, the alias of the same Haiku); the fixture keeps its own layout (no `## <date> <HH:MM>` entries, no `**Coordinator asked:**` blocks, and items.md is a list, not the shipped table); and the skill's own procedure around the reader (checking each reported idea before adding it) is the coordinator's, not measured here.

Hypothesis (3), effort, is `--effort` and `--sub-effort`.

### Fixtures

`benchlib/pmfixture.py` turns `bench/reader/spec.toml`, a size and a seed into a `.pm/` folder and its ground truth. The same inputs give the same bytes. The spec is Crumb, an ordering app for a neighborhood bakery. It is version 2 (`version` in the file; truth.json and run.json record it with the sha256 of the file's bytes, and a results folder never mixes versions); version 1 was the pilot's. 17 ideas:

- 7 stated plainly, among them the two the pilot's ground truth had left out ("Product pages should be simple, a photo, the price and what is inside.", "The customer picks a slot when ordering and gets a reminder.");
- 3 that change value in later dumps: pickup slots 15 → 30 → 20 minutes (twice), order history 12 → "twice as many" (24) and the pre-order cutoff 8 pm → "one hour earlier" (7 pm), the last two stated relatively, so the reader has to work the value out;
- 5 said in passing, one sentence in a paragraph about something else: the pilot's two (custom cakes need 48 hours of notice, a favorite order to repeat) and three phrased as facts, not requests (at most three open orders per customer, prices include tax, closed on Mondays);
- 2 discarded later ("Forget the dark mode").

And 12 distractors: near-identical ideas of other people or projects ("the café next door takes pre-orders until 9 pm", "the flower shop on the corner lets each customer keep up to five open orders"), planted inside the pasted material by `benchlib/fill.py`.

The spec's fairness rule: every sentence of the user's own paragraphs that states something the app should do or respect is a planted idea; the rest is narrative or context, with no requests, rules or values the questions ask about. A test scans the unplanted sentences for request words and for those values. The generator checks the rest: every sentence appears verbatim and exactly once, each placeholder is used once, each change comes in a later dump than the sentence it changes, an outright change states its value and a relative one never names it, and every question scores its value in force and forbids every superseded value and its close distractor's.

- **Dumps:** 3 dated files, each the user's paragraphs (one per line) with pasted material between them (`fill.generate`: minutes, tickets, logbooks, emails of the user's day job).
- **items.md:** a list with ids and states, each idea paraphrased (so the audit is semantic, not string matching). It omits exactly 3 live ideas, at least one said in passing; it never lists a discarded idea as pending (absent or `discarded`, by seed); it carries a superseded value of at least one changed idea.
- **The seed decides:** which ideas are omitted (the seed alone, so every size of a seed omits the same ones: the passing ideas take turns as the first, so seeds 1–5 omit each of them at least once, and the other two are drawn from the clear and passing ones), which changed ones are stale, whether each discarded one is absent or listed, the paraphrase of each item, the order of the user's paragraphs and where the pasted material falls between them, the 3 optional distractors (6 are always there: the value questions rely on them) and where each one goes.
- **Sizes**, of the whole `.pm/`: `s` ~10k tokens, `m` ~50k, `l` ~150k, defined in characters at 4 characters per token (`pmfixture.CHARS_PER_TOKEN`; not measured: the usual figure for English prose, and the filler, with its numbers and names, is probably denser). To recalibrate after the pilot, the report shows the characters of each size next to the peak context of the direct calls (the largest single request in the session's transcript: the system prompt plus what was read). The input tokens of the envelopes are a cost figure, not a size: they add up every request of the call, and each one carries the whole context again. At `m` the first dump is ~100k characters and ~1,500 lines, at `l` ~300k and ~4,700: the reader has to read in pieces. At `l`, reading everything verbatim may not fit in a direct reader's window and Claude Code may compact in the middle of the task; that is one reason `l` is reported and not gated.
- **Ground truth** (`fixtures/<size>-s<seed>/truth.json`, next to `root/`, the folder that gets copied; the reader never sees it): each idea's state (omitted, listed or discarded) and, for every planted sentence (the ideas', the changes', the discards', the distractors' and the items'), its file and its final line range, found in the files once they are written.

### Tasks

1. `audit`: "Which of the user's ideas in `.pm/dumps/` are not in `.pm/items.md`? Do not include ideas the user later discarded, nor ideas from other people or projects." Answer `{"missing": [{"quote", "file", "line"}]}`; with prompt v2, `{"ideas": [{"quote", "file", "line", "in_items", "discarded"}], "missing": [...]}`, `ideas` being every idea the reader found.
2. `value`: 6 questions (`[[questions]]` in the spec), each with a close distractor: the pickup slot (changed twice), the order history page and the pre-order cutoff (changed relatively), the custom cake notice, the open-orders limit and the closing day (said in passing). The three changed ones may be stale in items.md (at least one is, in every fixture). Answer `{"answers": [{"id", "value", "quote", "file", "line"}]}`.

The JSON comes from `.result` of the envelope, as `src/distill.rs` reads it: fences and text around it are tolerated. An answer that does not parse scores 0 and is flagged in the report.

### Grading

Deterministic, no model judge (`benchlib/reader_grade.py`):

- A citation is **valid** if its quote, normalized (case, accents, whitespace, quotes, markdown and punctuation; `…` splits it into pieces that have to appear in order), has at least 15 characters and is inside lines [line − 2, line + 2] of the cited file. The path may be absolute or relative. Anything else is an **invented** citation, whatever the reason (not in the file, wrong line, no such file, too short, missing fields; `run.json` keeps the reason).
- A valid citation maps to the planted sentence whose range it overlaps (± 2 lines) and whose text it shares: one contains the other, or 60 % of the shorter one's words. Range alone would blame a filler line next to a distractor on the distractor.
- **audit:** a true positive cites an omitted idea (deduplicated by idea); citing a listed idea is a false positive (also when it was cited by its change sentence or with its stale value: it is in the list); citing a discarded idea (by any of its sentences) or a distractor is a **confusion**; anything else valid is "other". Recall = omitted ideas found / omitted ideas; precision = omitted ideas found / valid citations, deduplicated by what they point at.
- **value:** `metrics.keyword_verdict` on the `value` field, with the question's `expected` and `forbidden` (same `a|b` syntax as the scenarios); a forbidden word is a confusion. Value % = mean score. Its citation is checked the same way (invalid = invented) but does not change the score; `run.json` also says whether it cites the sentence that sets the value in force (the last change). As in the dry-run judge of the scenarios, a value that mentions the old one as history ("24, twice the 12") is a forbidden hit; the prompt asks for the value alone.
- **ideas** (prompt v2 audit only; a diagnostic, never an input of the gate): `missing` is graded exactly as in v1. Each entry of `ideas` is checked and mapped like a citation; per omitted idea, it was **never enumerated** (no valid entry points at it: the reader did not take it for an idea, or did not read it; hypotheses 1 and 2), **enumerated but marked present** (`in_items: true`: a matching error), or enumerated as missing. The report shows "omitted ideas enumerated %", "enumerated but marked present" and the share of all the user's ideas the list points at; `run.json` has the status of each omitted idea and the counts by kind.

### The gate

Fixed before any real run; it is not changed after seeing results. The constants are `GATE_*` in `benchlib/reader_grade.py`. At size `m`, mean over the seeds, against `opus`, a delegated configuration `opus+X` is **viable** if:

1. its audit recall ≥ `opus` audit recall − 10 points;
2. its value % ≥ `opus` value % − 10 points;
3. it invents ≤ 1 citation per run on average, a run being one call (one task of one seed), both tasks counted;
4. it reports ≤ 1 discarded idea as missing in total across the seeds.

Sanity check: if `opus` itself has an audit recall under 80 % at `m`, the report says the task is ill-posed and judges no configuration. Size `l` is reported, not gated. The report prints each verdict mechanically.

The gate is computed separately for each prompt version: every delegated variant of a prompt version, at any effort, is compared with the `opus` variant of the same prompt version with the default effort (neither `--effort` nor `--sub-effort`). If that baseline is missing, the version's variants are not judged; a variant that lacks a task at `m` (an audit-only batch, for example) is not judged either. The constants and thresholds are the same for every version.

### Results

`bench/results/<date>-reader[-dry]/`:

A run is keyed by its variant (configuration, prompt version, main effort, subagent effort), size, seed and task. The key names its folder, `<config>-<prompt>-emain_<effort>-esub_<effort>-<size>-s<seed>-<task>` (`default` when the effort was not passed, `esub_none` for a direct configuration; for example `opus+haiku-v2-emain_default-esub_high-m-s3-audit`), and is in its `run.json`, so several invocations can share one `--out` and build one report without mixing.

- `report.md` and `report.json`: a header with the spec's version and sha256 and, per prompt version, its sha256; one row per variant and size, mean ± standard deviation over the seeds, of audit recall, precision, invented citations per call, confusions, value %, cost in USD (total and per model: opus, sonnet, haiku), input tokens (uncached, cache writes and cache reads), seconds, delegations and main-session dump reads (per seed, both tasks together); a table per task, with the effort seen in the main session and the subagent; for v2 audits, the `ideas` diagnostic; the fixture sizes; the gate, per prompt version; warnings (errors, unparseable answers, v2 audits without `ideas`, delegated calls that never delegated or that read the dumps from the main session, efforts seen other than the ones asked for, a prompt.md changed between runs of one version).
- `fixtures/<size>-s<seed>/`: `root/.pm/` and `truth.json`.
- `runs/<run>/run.json`: the key, the exact argv, the prompt, its version and the sha256s above, the spec's version and sha256, the efforts asked for (`effort_main`, `effort_sub`; `null` when not passed) and seen (`effort_seen_main`, `effort_seen_sub`), the raw envelope, the parsed answer and every graded citation, wall time, usage in total and per model from `modelUsage` (tokens by type and `costUSD` of each model, as the envelope gives them), the fixture's sha256, and from the transcripts: tool calls by name in the main session and in subagents, calls of the subagent tool, their `subagent_type` and any `effort` they passed, the main session's Read or Grep calls on `.pm/dumps/`, and the models seen.
- `runs/…/transcripts/claude/*.jsonl.gz`: the call's Claude Code sessions, the subagents' in `<session>/subagents/`.

Cost always comes from the envelopes, so it is 0 in dry runs. Claude Code may also use a small model of its own during a call; it appears in `modelUsage` and in the per-model cost, and counts.

### The made-up reader

`bench/fake_reader.py` is invoked like `claude -p`, accepts the same flags and returns the same envelope with zero usage. It uses no model and does not read the ground truth: it looks for the spec's sentences in the copy of `.pm/` it runs in and answers from them (for v2, it also lists every idea sentence it finds; for values, the last of an idea's sentences it finds gives the value in force). With `--agents` it "delegates": one call of the subagent tool in the main session, the reads in a subagent transcript, and both models in `modelUsage`. Its records carry `effort` as Claude Code's do (`--effort`, default `medium`; the subagent's `effort`, else the session's; `CLAUDE_CODE_EFFORT_LEVEL` over both). Knobs for the tests: `FAKE_READER_MISS=<n>` (leaves out n missing ideas; in v2, never enumerated), `FAKE_READER_MARK_PRESENT=1` (v2: enumerates a missing idea but marks it `in_items: true`), `FAKE_READER_INVENT=1` (a fabricated citation), `FAKE_READER_DISCARD=1` (reports a discarded idea), `FAKE_READER_DISTRACT=1` (reports a distractor), `FAKE_READER_STALE=1` (the value before the one in force), `FAKE_READER_BYPASS=1` (the main session reads the dumps too) and `FAKE_READER_GARBLE=1` (no JSON). As with the made-up agent, a dry run only proves that the harness works.
