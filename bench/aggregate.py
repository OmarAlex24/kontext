#!/usr/bin/env python3
"""Joins the run.json files of several runs (for example, one per model and
variant) into a table per scenario, model and variant, and, if the questions
have a class or a checkpoint, a memory table for each.

    python3 bench/aggregate.py bench/results/real-20261104 > table.md
    python3 bench/aggregate.py --dry <folder>    # includes dry runs
    python3 bench/aggregate.py --cost <folder>   # plus the cost by turn type

With `--cost`, one more table: per cell and turn type (work, filler, question,
compact), the calls and the USD per run, and per call the cache read and
created, the output and the model's internal turns (`num_turns`), from the
envelopes in `runs/*/transcripts/*.jsonl`. And another one with the prompt
cache breaks per run (`cache` in run.json): requests, breaks and tokens that
were written again.
"""

import json
import statistics
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from benchlib import cache, runner  # noqa: E402

CLASS_ORDER = {"A": 0, "B": 1, "C": 2, "cross": 3, "?": 4}


def stats(xs):
    xs = [x for x in xs if x is not None]
    if not xs:
        return None, None
    return statistics.mean(xs), (statistics.stdev(xs) if len(xs) > 1 else 0.0)


def fmt(m, sd, digits=1, pct=False):
    if m is None:
        return "–"
    k = 100 if pct else 1
    return f"{m * k:.{digits}f} ± {sd * k:.{digits}f}"


def accepted(r):
    a = r.get("acceptance") or {}
    if a.get("pct") is not None:
        return a["pct"] / 100
    return 1.0 if a.get("passed") else 0.0


def cost_of(r, part):
    return ((r.get("usage_by") or {}).get(part) or {}).get("cost_usd")


def group_recall(r, group, key):
    b = ((r.get("probes") or {}).get(group) or {}).get(key)
    return b.get("recall") if b else None


# Turn types of the transcripts, grouped to compare costs.
KIND_GROUPS = {"turn": "work", "correction": "work", "fill": "fill", "probe": "probe", "compact": "compact"}
GROUP_ORDER = {g: i for i, g in enumerate(["work", "fill", "probe", "compact"])}
COST_KEYS = ("calls", "cache_read", "cache_creation", "output", "turns", "cost_usd")


def cost_by_kind(run_dir):
    """The cost of a run by turn type: `{type: {calls, cache_read,
    cache_creation, output, turns, cost_usd}}`. With `--resume` the envelope
    carries the session's running total in `modelUsage` and `total_cost_usd` (and
    `usage` does not count `/compact`), so each call counts its difference from
    the previous call of the same session, as the runner does. `num_turns`
    is the call's own."""
    out = {}
    for t in runner.day_transcripts(run_dir):
        cums = {}
        for line in t.read_text().splitlines():
            if not line.strip():
                continue
            rec = json.loads(line)
            env = rec.get("envelope") or {}
            sid = rec.get("session_id")
            cum = runner.usage_of(env)
            # Without `modelUsage` (the made-up agent), the envelope is already the call's.
            u = runner.session_delta(cum, cums.get(sid)) if env.get("modelUsage") else cum
            cums[sid] = cum
            kind = rec.get("kind") or "?"
            b = out.setdefault(KIND_GROUPS.get(kind, kind), dict.fromkeys(COST_KEYS, 0))
            b["calls"] += 1
            b["cache_read"] += u["cache_read_input_tokens"]
            b["cache_creation"] += u["cache_creation_input_tokens"]
            b["output"] += u["output_tokens"]
            b["turns"] += int(env.get("num_turns") or 0)
            b["cost_usd"] += u["cost_usd"]
    return out


def cost_rows(m, v, rs):
    """The `--cost` rows of a cell: one per turn type. Calls and USD are the mean
    per run; the rest, per call (all the runs together)."""
    per_run = [r["_cost_by_kind"] for r in rs if r.get("_cost_by_kind") is not None]
    kinds = sorted({k for c in per_run for k in c}, key=lambda k: (GROUP_ORDER.get(k, 9), k))
    rows = []
    for k in kinds:
        tot = dict.fromkeys(COST_KEYS, 0)
        for c in per_run:
            for key in COST_KEYS:
                tot[key] += (c.get(k) or {}).get(key, 0)
        n = tot["calls"] or 1
        rows.append([
            m, v, k,
            fmt(*stats([(c.get(k) or {}).get("calls", 0) for c in per_run]), 1),
            f"{tot['cache_read'] / n:,.0f}",
            f"{tot['cache_creation'] / n:,.0f}",
            f"{tot['output'] / n:,.0f}",
            f"{tot['turns'] / n:.1f}",
            fmt(*stats([(c.get(k) or {}).get("cost_usd", 0) for c in per_run]), 3),
        ])
    return rows


def cache_row(rs):
    """The cache breaks of a cell, mean per run."""
    cs = [r["cache"] for r in rs if r.get("cache")]
    if not cs:
        return ["–"] * 5
    reqs = sum(c["requests"] for c in cs)
    pct = f"{100 * sum(c['breaks'] for c in cs) / reqs:.1f}" if reqs else "–"
    return [
        fmt(*stats([c["requests"] for c in cs]), 0),
        fmt(*stats([c["breaks"] for c in cs]), 1),
        pct,
        fmt(*stats([c["tokens_rewritten"] for c in cs]), 0),
        fmt(*stats([c.get("idle_breaks", 0) for c in cs]), 1),
    ]


def row(rs):
    """The main row of a cell (scenario, model, variant)."""
    g = [len(r["guards"]["failed"]) / r["guards"]["total"] if r["guards"]["total"] else 0 for r in rs]
    return [
        str(len(rs)),
        fmt(*stats([r["desvio_pct"] for r in rs])),
        fmt(*stats([r["lines"]["out_of_charter"] for r in rs]), 0),
        fmt(*stats(g), 0, pct=True),
        fmt(*stats([r["probes"]["recall"] for r in rs]), 0, pct=True),
        fmt(*stats([r["probes"].get("confusions", 0) for r in rs])),
        fmt(*stats([r["corrections"]["count"] for r in rs])),
        fmt(*stats([accepted(r) for r in rs]), 0, pct=True),
        fmt(*stats([r["tokens"] for r in rs]), 0),
        fmt(*stats([r["usage"]["cost_usd"] for r in rs]), 2),
        fmt(*stats([cost_of(r, "distill") for r in rs]), 3),
        str(sum(r["agent_errors"] for r in rs)),
    ]


def main(argv):
    dry = "--dry" in argv
    costo = "--cost" in argv
    args = [a for a in argv if a not in ("--dry", "--cost")]
    if len(args) != 1:
        print(__doc__.strip(), file=sys.stderr)
        return 2
    root = args[0]
    runs = []
    for p in sorted(Path(root).rglob("run.json")):
        r = json.loads(p.read_text())
        if costo:
            has = (p.parent / "transcripts").is_dir()
            r["_cost_by_kind"] = cost_by_kind(p.parent) if has else None
        runs.append(r)
    runs = [r for r in runs if dry or not r.get("dry_run")]
    if not runs:
        print("aggregate.py: no real runs in", root, file=sys.stderr)
        return 1
    order = {v: i for i, v in enumerate(runner.VARIANTS)}
    out = []
    total_cost = 0.0
    for sc in sorted({r.get("scenario") or "?" for r in runs}):
        sr = [r for r in runs if (r.get("scenario") or "?") == sc]
        keys = sorted(
            {((r.get("model") or "?"), r["variant"]) for r in sr},
            key=lambda mv: (mv[0], order.get(mv[1], 99)),
        )
        groups = {mv: [r for r in sr if ((r.get("model") or "?"), r["variant"]) == mv] for mv in keys}
        out += [
            f"## Scenario {sc}",
            "",
            "| Model | Variant | n | Drift % | Outside the charter (lines) | Failing guards % | Memory % | "
            "Confusions | Corrections | Done/Behavior % | Tokens | Cost USD | Distill USD | Errors |",
            "|---|---|" + "---:|" * 12,
        ]
        for (m, v), rs in groups.items():
            total_cost += sum(r["usage"]["cost_usd"] for r in rs)
            out.append("| " + " | ".join([m, v, *row(rs)]) + " |")
        out.append("")
        for group, title, header in (
            ("by_class", "Memory % by class", "Class"),
            ("by_checkpoint", "Memory % by checkpoint", "Checkpoint"),
        ):
            ks = sorted(
                {k for r in sr for k in ((r.get("probes") or {}).get(group) or {})},
                key=lambda k: (CLASS_ORDER.get(k, 9), int(k) if k.isdigit() else 0, k),
            )
            if not ks or ks == ["?"]:
                continue
            out += [f"### {title}", "", "| Model | Variant | " + " | ".join(f"{header} {k}" for k in ks) + " |"]
            out.append("|---|---|" + "---:|" * len(ks))
            for (m, v), rs in groups.items():
                vals = [fmt(*stats([group_recall(r, group, k) for r in rs]), 0, pct=True) for k in ks]
                out.append("| " + " | ".join([m, v, *vals]) + " |")
            out.append("")
        if costo:
            out += [
                "### Cost by turn type",
                "",
                "Calls and USD: mean per run. Cache, output and internal turns (`num_turns`): per call.",
                "",
                "| Model | Variant | Type | Calls | Cache read | Cache created | Output | Turns | USD |",
                "|---|---|---|" + "---:|" * 6,
            ]
            for (m, v), rs in groups.items():
                out += ["| " + " | ".join(cells) + " |" for cells in cost_rows(m, v, rs)]
            out.append("")
            out += [
                "### Prompt cache breaks",
                "",
                f"Per run. A break is a request that reads from the cache more than {cache.THRESHOLD:,} tokens "
                "less than the previous request's prefix, without compacting between them (no subagents). "
                f"\"After pause\": of those, the ones that arrive more than {cache.IDLE_SECONDS // 60} min after "
                "the previous one (the cache expired; nobody rewrote anything).",
                "",
                "| Model | Variant | Requests | Breaks | Breaks % | Tokens rewritten | After pause |",
                "|---|---|" + "---:|" * 5,
            ]
            for (m, v), rs in groups.items():
                out.append("| " + " | ".join([m, v, *cache_row(rs)]) + " |")
            out.append("")
    out.append(f"Total cost: {total_cost:.2f} USD in {len(runs)} runs.")
    print("\n".join(out))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
