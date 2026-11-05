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
| Tokens and cost | What each call spent (input, output and cache, and dollars), plus the judge and kontext's distill. With `--resume`, the `claude -p` envelope carries in `total_cost_usd` and `modelUsage` the **running total of the session**, so each call counts its difference from the previous call of the same session (and so `/compact` is included too, which does not appear in `usage`). Runs made before this added the running totals; `aggregate.py` recomputes them from their transcripts (`runner.recost`). `run.json` splits them into `usage_by` (`agent`, `judge`, `distill`). In dry runs, 0 |
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
