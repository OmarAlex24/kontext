These runs used the Spanish version of the scenario; the scenario was translated later.

# Small benchmark after the long-memory fixes, 2026-11-04

This benchmark checks whether the three fixes from [long-memory](2026-11-04-long-memory.md) improved anything, without repeating the 75 runs. Only the cells where kontext was failing were repeated, plus `automem` with haiku as a control.

**Setup:** scenario, judge (sonnet) and environment the same as in the large run, except for two things:
- **Branch:** it ran on `fix/memoria-vigente` (`f2b5908`).
- **Agent command:** `DEFAULT_AGENT_CMD` was used, which now includes the new permissions.

In total there were 11 runs, for **17.51 USD**.

## Comparison

"Before" is the large run, with 5 seeds per cell. "Now" has the seeds shown in the n column.

| Cell | n | Memory % before → now | Class C before → now | Behavior % before → now | kontext denials before → now | Cost USD before → now |
|---|---:|---:|---:|---:|---:|---:|
| haiku `kontext-pointer` | 3 | 89 → **100** | 53 → **100** | 100 → 100 | 22 → 3 | 0.68 → 0.72 |
| haiku `kontext-inject` | 3 | 98 → 97 | 93 → 89 | 100 → 100 | 0 → 0 | 0.69 → 0.76 |
| opus `kontext-inject` | 2 | 95 → **100** | 73 → **100** | 100 → 100 | 39 → 0 | 5.53 → 5.67 |
| haiku `automem` (control) | 3 | 55 → 32 | 13 → 33 | 51 → 0 | 6 → 2 | 0.59 → 0.57 |

The denials column is the total number of kontext calls that Claude Code blocked in each cell. In the large run there were 5 seeds per cell, and here there are 2 or 3.

## What the results show

- **`kontext-pointer` with haiku was the weak point, and now it gets 100 %** in the three seeds, including on the data that changes. Before, it lost 22 writes to kontext because of permissions; now it loses 3. It seems that what helped most was that the agent can finally write to kontext.
- **With opus, `kontext-inject` stops hesitating.** In the two seeds it gets 100 % right: it no longer answers "there is no rate in force", and none of its calls to kontext were blocked (before, 39).
- **`kontext-inject` with haiku stays the same**, 97 against 98 %. The only failure is the rate in one seed. The distill noted that the circular "is in force from 2026-11-05", one day after the run. That is, the distill's model turned "already in force" into a wrong absolute date.
- **The control didn't go up.** `automem` with haiku was already bimodal in the large run: 80, 80, 65, 25 and 25 %. Here it gave 25, 25 and 45 %. The scenario fix does not explain kontext's improvement. With 3 seeds, the drop in `automem` is noise more than effect.
- **It costs a little more.** Between 3 and 10 % per run, because the agent now reads and writes more in kontext, since it is no longer blocked.

## Limits

- There are 2 or 3 seeds per cell. It shows the direction of the change, not something to publish.
- sonnet, `none` and `notes` were not repeated.
- One case is left to fix: the distill should not turn "already in force" or "from today" into a date that is not the session's date.

The runs are in `bench/results/mini-20261104/`. They were launched with `~/.cache/ctx-bench/mini-memoria.sh` as a systemd unit.
