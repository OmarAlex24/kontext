# kontext against five other setups

The comparison benchmark of `DESIGN.md`. Each run puts plain Claude Code, kontext v0.3.0, gentle-ai + Engram, the mem0 plugin, Beads or OpenSpec on a small task (S1, S2) or the 4-day project L. Each machine is its own container, and day 4 runs on a second container, machine B. The analysis rules were fixed before any model call (`ANALYSIS.md`).

> **A real run spends tokens.** `run.py` refuses without `--yes`. It also refuses if the fixtures, turns, hidden checks or analysis rules differ from `FREEZE.json`.

## What runs where

- **Host:** `python3` (3.11+, standard library only) and `docker`. Nothing else is installed on the host, and no container mounts a host folder: files move as tar streams over `docker exec`.
- **Images** (`images/`, versions in `versions.env`): `bench-base` (Debian slim + Node 22 + Claude Code 2.1.325, user `bench`), one image per arm, `bench-sidecar` (kontext's `server/` at v0.2.0 under Miniflare) and `bench-verify` (Python only).
- **Containers per run:**
  - a private network;
  - a named volume holding the bare git remote;
  - machine A;
  - machine B on day 4;
  - the kontext sidecar (kontext arm only);
  - the verification container (`--network none`), which receives the snapshot and the hidden checks only after the agent's containers are done with them.
- **Arms** (`arms/<arm>/`): `setup.sh`, `handoff_a.sh` / `handoff_b.sh`, `settle.sh`, `teardown.sh`, `allowed_tools.txt` (see `arms/README.md`).

## Commands

```sh
# Build the images (model-free; prints each image's id)
bench/compare/images/build.sh

# Dry run: bench/fake_agent.py inside the containers, no model, no key
python3 bench/compare/run.py --dry-run --tasks smoke,S1,S2,L --seeds 1 --jobs 3 --fill-scale 0.05

# Tests: no Docker; then with Docker and the images
python3 -m unittest discover bench/tests
BENCH_DOCKER_TESTS=1 python3 -m unittest bench.tests.test_compare_docker

# Friction (design §3), re-executed in two containers per setup
python3 bench/compare/friction/run.py
```

### The pilot (spends tokens)

The pilot runs in stages, in this order. Use a key made only for the benchmark, with a spend cap.

| Variable | Read by | Passed to |
|---|---|---|
| `ANTHROPIC_API_KEY` | `run.py`, every arm | each `docker exec` into a machine, by name only |
| `MEM0_API_KEY` | `run.py`, the mem0 arm | the mem0 machines only, by name only |

Nothing else is read from the environment. `env -i` keeps a stray `ANTHROPIC_BASE_URL` or token out of the run (the harness also strips them). Homebrew goes first in its PATH so that `python3` is 3.11+, not macOS's 3.9. Each stage is one line, so it works in zsh and bash alike:

```sh
# 0. smoke: the 6 arms, 1 seed
env -i HOME="$HOME" PATH=/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin ANTHROPIC_API_KEY="$ANTHROPIC_API_KEY" MEM0_API_KEY="$MEM0_API_KEY" python3 bench/compare/run.py --yes --tasks smoke --arms plain,kontext,gentle-ai,mem0,beads,openspec --models claude-sonnet-5-5 --seeds 1
# 1. S1: the 6 arms
env -i HOME="$HOME" PATH=/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin ANTHROPIC_API_KEY="$ANTHROPIC_API_KEY" MEM0_API_KEY="$MEM0_API_KEY" python3 bench/compare/run.py --yes --tasks S1 --arms plain,kontext,gentle-ai,mem0,beads,openspec --models claude-sonnet-5-5 --seeds 1
# 2. L on Sonnet: plain, kontext, gentle-ai, mem0
env -i HOME="$HOME" PATH=/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin ANTHROPIC_API_KEY="$ANTHROPIC_API_KEY" MEM0_API_KEY="$MEM0_API_KEY" python3 bench/compare/run.py --yes --tasks L --arms plain,kontext,gentle-ai,mem0 --models claude-sonnet-5-5 --seeds 1
# 3. L on Haiku: plain, kontext (no mem0, so no MEM0_API_KEY)
env -i HOME="$HOME" PATH=/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin ANTHROPIC_API_KEY="$ANTHROPIC_API_KEY" python3 bench/compare/run.py --yes --tasks L --arms plain,kontext --models claude-haiku-5-5 --seeds 1
```

`--jobs 3` runs 3 cells at a time; the full dry run used 3 (4 is untested). Each machine container is capped at 1.5 GB and 2 CPUs (`BENCH_CONTAINER_MEMORY`, `BENCH_CONTAINER_CPUS`).

| Option | What it does |
|---|---|
| `--arms`, `--tasks`, `--models`, `--seeds N`, `--first-seed` | The cells: arms × tasks × models × seeds |
| `--dry-run` | The fake agent and a fake distill model; no key is read |
| `--yes` | Confirms a real run |
| `--jobs N` | Cells at a time |
| `--fill-scale` | Multiplies L's two filler turns (1.0 = 25k tokens each) |
| `--turn-timeout`, `--settle-timeout` | Seconds per agent call (1200) and per settle step (600) |
| `--keep-containers` | Leaves the containers for inspection (then `docker rm -f` them by the `org.kontext.bench.run` label) |
| `--no-measure` | Skips `kontext measure --json` over the transcripts |

## Results

`bench/compare/results/<stamp>[-dry]/` (git-ignored):
- `plan.json`: the cells, the image ids, the freeze status, and the names of the keys read;
- `report.md` / `report.json`: the tables per task × model × arm, with the verdicts of `ANALYSIS.md`;
- `runs/<task>-<arm>-<model>-s<seed>/`:
  - `run.json`: score, checks per day, cost by source, interventions, denials, settles, hooks and `apiKeySource`;
  - `transcripts/` (gzipped, keys redacted);
  - `snapshots/day-N.tar.gz`;
  - `arm-scripts/*.log`;
  - `out-<machine>/` (the distill envelopes, mem0's evidence).

Before `run.py` exits, it scans the whole folder for every key value it read. A file with a hit is deleted, and the run exits 3.

The report of a finished run is kept in `bench/compare/reports/<date>-<name>/`, with a note on how to read it. The full run on kontext v0.3.0: [`reports/2026-11-16-full-v0.3.0/`](reports/2026-11-16-full-v0.3.0/README.md).

## The freeze

`FREEZE.json` records the commit and a sha256 over the frozen paths (`complib/freeze.py`):
- `scenarios/`;
- `ANALYSIS.md`;
- `complib/interventions.py`;
- `complib/analysis.py`;
- `benchlib/fill.py`.

To change any of them before the pilot, commit the change, then rewrite `FREEZE.json`:

```sh
python3 -c 'import sys; sys.path.insert(0, "bench/compare"); from complib import freeze; freeze.write(freeze.git_head(), "why")'
```

After the first paid cell, a change means a new benchmark, not a re-freeze.
