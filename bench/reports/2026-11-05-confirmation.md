These runs used the Spanish version of the scenario; the scenario was translated later.

# Confirmation run, 2026-11-05

It checks, with the three models, what the small benchmarks showed ([after the fixes](2026-11-04-mini-after-fixes.md) and [after incremental injection](2026-11-04-mini-incremental-injection.md)): that kontext remembers everything after `/compact` and costs almost the same as having no memory.

**Setup:**
- It ran on `main` (`21ef327`), with #17, #18 and #20 merged.
- `long-memory` scenario, judge sonnet and the clean API environment, same as in [the large run](2026-11-04-long-memory.md).
- 3 models × (`none`, `kontext-inject`) × 3 seeds = 18 runs, **45.63 USD**.
- This time the full sessions were saved to `transcripts/claude/`, so the cache breaks were measured per run.

## Table

Mean ± standard deviation over 3 seeds. The cost includes the agent, the judge and the distill.

| Model | Variant | Memory % | Class C % | Behavior % | Confusions | Cost USD | Overhead |
|---|---|---:|---:|---:|---:|---:|---:|
| haiku | none | 30 ± 13 | 33 ± 58 | 0 | 0.0 | 0.59 | – |
| haiku | kontext-inject | **100 ± 0** | 100 | 100 | 0.0 | 0.64 | +8 % |
| sonnet | none | 25 ± 0 | 0 | 0 | 0.0 | 2.33 | – |
| sonnet | kontext-inject | **100 ± 0** | 100 | 100 | 0.0 | 2.47 | +6 % |
| opus | none | 25 ± 0 | 0 | 0 | 0.0 | 4.58 | – |
| opus | kontext-inject | **100 ± 0** | 100 | 100 | 0.0 | 4.60 | **+0.4 %** |

Compared with the large run of 2026-11-04 (before #18 and #20):

| Model | Memory % before → now | Overhead before → now |
|---|---:|---:|
| haiku | 98 → 100 | +17 % → +8 % |
| sonnet | 100 → 100 | +12 % → +6 % |
| opus | 95 → 100 | +21 % → +0.4 % |

## What the results show

- **`kontext-inject` gets 100 % in all 9 runs**, in all classes and at the four checkpoints, with no confusions. Without memory, `/compact` still erases almost everything (25–30 %).
- **The overhead ended up between +0.4 and +8 %.** With opus it is almost nil: kontext makes more rounds in the work turns (4.9 against 4.4 per call), but makes up for it on the questions, which it answers without searching the repo.
- **The cache doesn't break more than without kontext.** Cache breaks per run, as a % of the requests: haiku 0 against 0.4, opus 0.5 against 0.9, sonnet 2.0 against 2.6. The sonnet ones come from Claude Code, which rewrites the session's first message (they show up the same in `none`); none comes from something kontext rewrites (DESIGN §9.5).

## Limits

- It is still a single scenario and 3 seeds per cell.
- The judge is sonnet for all, including sonnet as the agent.
- The harness, the scenario and kontext were written by the same team. It is not publishable yet.

The runs are in `bench/results/conf-20261105/`. They were launched with `~/.cache/ctx-bench/conf-memoria.sh` as a systemd unit.
