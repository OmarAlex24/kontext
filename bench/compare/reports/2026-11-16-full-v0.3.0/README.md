# Full run, kontext v0.3.0 against five setups (2026-11-16)

`report.md` and `report.json` are the output of `complib/tables.py` over every run of the full comparison, unchanged. This note says what was run and what to read next to the numbers. Nothing in it replaces the tables.

## What was run

- **Cells:** 180 = 3 tasks (S1, S2, L) × 6 arms (plain, kontext, gentle-ai, mem0, beads, openspec) × 2 models (`claude-sonnet-5-5`, `claude-haiku-5-5`) × 5 seeds. No cell errored. In every cell the agent ran only on the cell's model; kontext's distill runs on Haiku in both models' cells, and its cost is the "distill USD" column.
- **When:** 2026-11-16, 02:49 to 05:37 UTC, in five calls of `run.py`, one per seed (`--seeds 1 --first-seed N`), all into one results folder.
- **Code:** commit `a36aa45` (merged as PR #50). Frozen paths as in `FREEZE.json`: digest `727cb77e…`, recorded and found the same in all 180 cells.
- **Versions** (`images/versions.env`): Claude Code 2.1.325, kontext v0.3.0, gentle-ai 4.5.0 with Engram 3.7.2, the mem0 plugin at `f1791da`, Beads 1.3.4 with Dolt 2.2.0, OpenSpec 2.0.0.
- **Cost:** 38.73 USD, the sum of what each `claude -p` envelope reported (rule 6 of `ANALYSIS.md`).
- **Not in the repo:** the per-run folders (`run.json`, transcripts, snapshots, arm logs; 125 MB). They are on the machine that ran it. `report.md` can be rebuilt from them with `tables.write(<results folder>)`.

## L, the 4-day project (requirements kept, of 14)

| Arm | Sonnet 5.5 | Haiku 5.5 |
|---|---|---|
| kontext | 12.8 ± 0.4 | 12.6 ± 0.9 |
| gentle-ai | 6.4 ± 1.3 | 6.6 ± 0.9 |
| mem0 | 2.6 ± 1.1 | 6.8 ± 2.6 |
| beads | 1.4 ± 2.6 | 6.0 ± 3.9 |
| openspec | 0.2 ± 0.4 | 5.2 ± 2.8 |
| plain | 0.2 ± 0.4 | 3.8 ± 3.0 |

By rule 2 of `ANALYSIS.md` (a difference of at least 2 requirements and p < 0.05):

- **Sonnet:** kontext differs from every other arm (p = 0.008 each). Against plain it keeps 12.6 more, with cost +12 % and time +7 %, both under the 20 % that rule 3 asks for a claim. gentle-ai differs from plain (+6.2, cost +84 %) and so does mem0 (+2.4, p = 0.016). Beads and OpenSpec: no claim.
- **Haiku:** kontext differs from every other arm (p = 0.008 or 0.016). Against plain it keeps 8.8 more, with cost +46 % and time +29 %. No other arm differs from plain.

## Read these with the numbers

1. **Haiku, kontext: 5.0 of the 12.6 were already passing before the chat was lost.** Rule 5 reports apart the requirements whose check passes at the end of day 2. In kontext's five Haiku runs that was 9, 7, 2, 7 and 0 requirements. Leaving them out, the runs keep 4, 4, 11, 6 and 13 of the 5, 7, 12, 7 and 14 that remain. So on Haiku the L score is not a memory result on a par with Sonnet's. The same happened less often in Beads (1 and 6 requirements, in two runs) and OpenSpec (3, 1 and 1, in three runs), and in no run of plain, gentle-ai or mem0. On Sonnet it happened in no run of any arm.
2. **gentle-ai on Sonnet scored 0 of 7 on machine B in all five runs, because nothing was saved on machine A.** The agent made no `mem_save` call in any of the five runs (it called `mem_search`, `mem_search_prompts` and `mem_current_project`), so `engram sync` exported 0 observations each time, and its output says prompts are excluded from git sync unless `--include-prompts` is passed. The handoff steps themselves ran. On Haiku the agent made 14 `mem_save` calls over the five runs, and the sync carried 3, 4, 2, 0 and 2 observations.
3. **Beads on Haiku: 17 of the 41 refused commands in its L runs named `bd`** (counted from the transcripts), several of them compound commands such as `bd update … && cat > file`, which the allow-list does not cover. On Sonnet, 1 of 11. OpenSpec on Haiku: 3 of 42. No refused command named the arm's own tool in kontext, gentle-ai or mem0.
4. **kontext and mem0 on Sonnet work through hooks:** in the L runs the agent ran no `kontext` command and called no mem0 tool.
5. **mem0's remote data was deleted.** All 30 mem0 cells ended with the plugin's `forget` at exit 0 and remote status `deleted`, 588 memories in total.

## S1 and S2, the small tasks

Every arm scored 100 % in every run, as rule 4 expects, so only cost and time are compared. Median cost per run, in USD:

| Task · model | plain | kontext | gentle-ai |
|---|---|---|---|
| S1 · Sonnet | 0.031 | 0.035 | 0.160 |
| S2 · Sonnet | 0.037 | 0.044 | 0.145 |
| S1 · Haiku | 0.002 | 0.004 | 0.010 |
| S2 · Haiku | 0.003 | 0.005 | 0.010 |

The other arms and the times are in `report.md`.

## Limits

- One fictional project, one harness (Claude Code with `claude -p`), two models, five seeds per cell.
- kontext's runs use a local server in a container, not Cloudflare; mem0 uses its hosted service (design §2).
- The install-and-sync steps of each setup are not part of this run: see `../../friction/REPORT.md`.
