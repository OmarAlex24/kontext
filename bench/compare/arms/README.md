# Arm scripts

One folder per arm. The runner copies `bench/compare/arms/` to `/opt/bench/arms/`
in each agent container and runs these scripts there as `bench`, with `bash`,
never inside the conversation. Each one is optional; a missing script is a no-op.

| Script | When | Machine |
|---|---|---|
| `setup.sh` | Before day 1, after the fixture is committed and pushed | A |
| `handoff_a.sh` | End of day 3, after the harness pushed A's working tree | A |
| `handoff_b.sh` | Day 4, after the harness cloned the code on B | B |
| `settle.sh` | Before every new session (days 2–4) and before the hidden checks | the current one |
| `teardown.sh` | After the last day | the current one |
| `allowed_tools.txt` | The arm's own `--allowedTools`, added to the common list | — |

Environment every script gets: `REPO` (the code repo), `BENCH_RUN` (run id),
`BENCH_REMOTE` (the per-run bare git remote, `file:///remotes/<run>.git`),
`BENCH_OUT` (a folder copied to the results at the end), `BENCH_DRY` (`1` in
dry runs: no model and no hosted service may be called). Arm-specific variables
are listed at the top of each script.

A line on stdout of the form `BENCH_SECRET NAME=value` is a secret handed from
A to B: the runner keeps it in memory, passes it to `handoff_b.sh` by name and
never writes it anywhere. A line `BENCH_STEP <kind> <text>` counts a step
(`command` or `manual`) for the friction axis.
