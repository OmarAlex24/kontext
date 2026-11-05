These runs used the Spanish version of the scenario; the scenario was translated later.

# Small benchmark after incremental injection, 2026-11-04

This benchmark checks whether the changes to lower the cost keep the memory and how much they save. It repeats the same cells as [the previous small benchmark](2026-11-04-mini-after-fixes.md), except the `automem` control, because the scenario did not change.

**Setup:**
- It ran on the branch `perf/inyeccion-incremental` (`84a0b75`).
- Scenario, judge (sonnet) and environment are the same.
- In total there were 8 runs, for 13.37 USD.

**What changed in kontext:**
- **Incremental injection.** In `resume`, the start hook only puts in what is new in that session, or nothing if nothing changed. The full block goes in at startup and after compacting.
- **Fewer calls to kontext.** With the distill active, the agent no longer repeats with `propose` or `log` what the distill saves by itself. It also doesn't check `kontext inbox`, which is the human's inbox.
- **Dates in the distill.** It no longer turns "already in force" into a date.

Before launching, it was checked with haiku through the API that what is injected at startup stays visible after several `--resume` calls. It was also checked that `/compact` triggers the hook with `source=compact`.

## Comparison

"No memory" comes from the large run (`none`, 5 seeds). "Before" is the previous small benchmark, and "now" is this one.

| Cell | n | Memory % before → now | Cost USD before → now | No memory | Overhead before → now |
|---|---:|---:|---:|---:|---:|
| haiku `kontext-pointer` | 3 | 100 → 98 | 0.72 → **0.66** | 0.59 | +22 % → +12 % |
| haiku `kontext-inject` | 3 | 97 → **100** | 0.76 → **0.61** | 0.59 | +29 % → **+3 %** |
| opus `kontext-inject` | 2 | 100 → 100 | 5.67 → **4.79** | 4.57 | +24 % → **+5 %** |

Behavior is 100 % and confusions are 0 or 0.3 in all cells. Haiku with `kontext-pointer` failed one class B question in one seed.

### Where the savings are

Opus with `kontext-inject`, USD per run by type of turn (`aggregate.py --costo`):

| Type | No memory | Before | Now |
|---|---:|---:|---:|
| Work (10 calls) | 0.64 | 1.29 | 0.87 |
| Filler (14) | 2.56 | 2.89 | **2.58** |
| Question (20) | 0.51 | 0.72 | **0.47** |
| Compact (3) | 0.78 | 0.65 | 0.76 |

- **Fillers and questions cost the same as with no memory.** The saving comes from not repeating the kontext block on every `--resume`.
- **What's left is in the work turns**: 5.6 internal rounds per call, against 4.2 with no memory and 8.7 before. It is the agent consulting kontext. About 0.03 USD from the distill is added to that.

### Prompt cache

It was measured on the full Claude Code sessions with `benchlib/cache.py`:

| Variant | Breaks now | Breaks in the large run |
|---|---:|---:|
| `kontext-inject` | 1 of 357 requests (0.3 %) | 1.5 % |
| `kontext-pointer` | 0 of 258 | 0.3 % |
| `none` (reference) | – | 1.3 % |

The invariant was written down in DESIGN §9.5: kontext only appends to the end of the conversation and never modifies what is already cached.

## Limits

- There are 2 or 3 seeds per cell: it shows the direction of the change.
- sonnet, `none` and `notes` were not repeated.
- The runs this time did not save their sessions to `transcripts/claude/`, because that measure was added later. The breaks were measured on the shared `CLAUDE_CONFIG_DIR`, filtering by time.

The runs are in `bench/results/mini2-20261104/`. They were launched with `~/.cache/ctx-bench/mini2-memoria.sh` as a systemd unit.
