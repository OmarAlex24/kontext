"""The table: one row per variant, mean ± standard deviation over the seeds.
With memory questions by class or by checkpoint, also a memory table for each."""

import json
import statistics
import time

from .metrics import GUARD_WEIGHT

COLUMNS = [
    ("desvio_pct", "Drift %", "{:.1f}"),
    ("out_lines", "Outside the charter (lines)", "{:.1f}"),
    ("guards_failed_pct", "Failing guards %", "{:.0f}"),
    ("recall_pct", "Memory %", "{:.0f}"),
    ("confusions", "Confusions", "{:.1f}"),
    ("corrections", "Corrections", "{:.1f}"),
    ("tokens", "Tokens", "{:,.0f}"),
    ("cost_usd", "Cost USD", "{:.2f}"),
    ("acceptance_pct", "Done %", "{:.0f}"),
]
CLASS_ORDER = {"A": 0, "B": 1, "C": 2, "cross": 3, "?": 4}


def acceptance_label(sc):
    return sc.acceptance.get("label", "Done %")


def row_values(r):
    g = r["guards"]
    p = r["probes"]
    out = {
        "desvio_pct": r["desvio_pct"],
        "out_lines": r["lines"]["out_of_charter"],
        "guards_failed_pct": 100.0 * len(g["failed"]) / g["total"] if g["total"] else 0.0,
        "recall_pct": 100.0 * p["recall"] if p["recall"] is not None else 0.0,
        "confusions": p.get("confusions", 0),
        "corrections": r["corrections"]["count"],
        "tokens": r["tokens"],
        "cost_usd": r["usage"]["cost_usd"],
        "acceptance_pct": r["acceptance"]["pct"] or 0.0,
    }
    for group in ("by_class", "by_checkpoint"):
        for k, b in (p.get(group) or {}).items():
            if b.get("recall") is not None:
                out[f"{group}:{k}"] = 100.0 * b["recall"]
    for part, u in (r.get("usage_by") or {}).items():
        out[f"cost:{part}"] = u["cost_usd"]
    return out


def mean_sd(xs):
    return {"mean": statistics.fmean(xs), "sd": statistics.stdev(xs) if len(xs) > 1 else 0.0}


def aggregate(results, variants):
    table = {}
    for v in variants:
        rows = [row_values(r) for r in results if r["variant"] == v]
        if not rows:
            continue
        agg = {"runs": len(rows)}
        for key in dict.fromkeys(k for row in rows for k in row):
            agg[key] = mean_sd([x[key] for x in rows if key in x])
        table[v] = agg
    return table


def cell(stat, fmt):
    return f"{fmt.format(stat['mean'])} ± {fmt.format(stat['sd'])}"


def group_keys(table, group):
    keys = [k.split(":", 1)[1] for agg in table.values() for k in agg if k.startswith(group + ":")]
    return sorted(
        dict.fromkeys(keys),
        key=lambda k: (CLASS_ORDER.get(k, 9), int(k) if k.isdigit() else 0, k),
    )


def group_table(table, group, title, header):
    """Memory % by class or by checkpoint; nothing if there is only one group
    without a class (scenarios like billing)."""
    keys = group_keys(table, group)
    if not keys or keys == ["?"]:
        return []
    l = [f"### {title}", "", "| Variant | " + " | ".join(f"{header} {k}" for k in keys) + " |"]
    l.append("|---|" + "---:|" * len(keys))
    for v, agg in table.items():
        cells = [cell(agg[f"{group}:{k}"], "{:.0f}") if f"{group}:{k}" in agg else "–" for k in keys]
        l.append(f"| {v} | " + " | ".join(cells) + " |")
    return l + [""]


def markdown(sc, table, results, dry_run, agent_cmd, model):
    seeds = max((t["runs"] for t in table.values()), default=0)
    probes = sc.all_probes()
    turns = [t for d in sc.days for t in d]
    fill_scale = results[0].get("fill_scale", 1.0) if results else 1.0
    l = [f"# kontext benchmark: scenario {sc.name}", ""]
    if dry_run:
        l += [
            "> **Dry run.** The agent is `bench/fake_agent.py`, with no model: the numbers",
            "> only show that the harness works. They are not results and are not published.",
            "",
        ]
    l += [
        f"- Date: {time.strftime('%Y-%m-%d %H:%M')}",
        f"- Agent: `{agent_cmd}`" + (f" with `--model {model}`" if model else ""),
        f"- Seeds per variant: {seeds}",
        f"- Days: {len(sc.days)}; temptations: {sum(1 for t in turns if t.temptation)}; "
        f"memory questions: {len(probes)}; guards: {len(sc.decisions)}",
    ]
    fills = [t for t in turns if t.kind == "fill"]
    if fills:
        tok = sum(t.tokens for t in fills) * fill_scale
        l.append(
            f"- Filler: {len(fills)} turns, about {tok:,.0f} tokens in total (--fill-scale {fill_scale:g}); "
            f"compactions: {sum(1 for t in turns if t.kind == 'compact')}; distill in kontext: "
            f"{'on' if sc.distill else 'off'}"
        )
    l += ["", "Mean ± standard deviation over the seeds.", ""]
    cols = [(k, acceptance_label(sc) if k == "acceptance_pct" else h, f) for k, h, f in COLUMNS]
    l.append("| Variant | " + " | ".join(c[1] for c in cols) + " |")
    l.append("|---|" + "---:|" * len(cols))
    for v, agg in table.items():
        l.append(f"| {v} | " + " | ".join(cell(agg[k], fmt) for k, _, fmt in cols) + " |")
    l.append("")
    l += group_table(table, "by_class", "Memory % by class", "Class")
    l += group_table(table, "by_checkpoint", "Memory % by checkpoint", "Checkpoint")
    parts = [p for p in ("agent", "judge", "distill") if any(f"cost:{p}" in a for a in table.values())]
    if parts:
        l += ["### Cost USD by part", "", "| Variant | " + " | ".join(parts) + " |"]
        l.append("|---|" + "---:|" * len(parts))
        for v, agg in table.items():
            l.append(f"| {v} | " + " | ".join(cell(agg[f"cost:{p}"], "{:.3f}") for p in parts) + " |")
        l.append("")
    l += [
        "## How it is calculated",
        "",
        f"- **Drift %** = 100 × (L_out + {GUARD_WEIGHT} × G_failing) / (L_total + {GUARD_WEIGHT} × G_total).",
        "  L = changed lines (added + removed) against the starting point; L_out, the ones in",
        "  files that are not in the `scope` of any goal; G = guards of the decisions.",
        "- **Memory %**: memory questions answered right: with all the expected ideas and",
        "  without asserting any forbidden one (the old value of a fact that changed, a distractor).",
        "- **Confusions**: answers that asserted a forbidden idea, per run.",
        "- **Corrections**: scripted turns of \"we already decided that\" that were needed, because",
        "  the agent did what a temptation asked.",
        f"- **{acceptance_label(sc)}**: acceptance tests that pass.",
        "- **Tokens and cost**: the agent, the judge and kontext's distill together.",
        "",
        "Details of each run in `runs/<variant>-s<seed>/run.json` and the transcripts in",
        "`runs/<variant>-s<seed>/transcripts/`.",
        "",
    ]
    errors = sum(r["agent_errors"] for r in results)
    if errors:
        l += [f"Warning: {errors} calls to the agent exited with an error; check the transcripts.", ""]
    bad = sum(1 for r in results for c in r.get("compacts", []) if not c["ok"])
    if bad:
        l += [f"Warning: {bad} compactions did not leave a `compact_boundary` in the Claude Code transcript.", ""]
    derr = sum(len((r.get("distill") or {}).get("wait_errors", [])) for r in results)
    dfail = sum((r.get("distill") or {}).get("failed", 0) for r in results)
    if derr or dfail:
        l += [
            f"Warning: the distill failed {dfail} times and the wait {derr} times; "
            "check `distill/` and `.kontext/distill.log`.",
            "",
        ]
    return "\n".join(l)


def write(out, sc, results, variants, cfg):
    table = aggregate(results, variants)
    (out / "report.json").write_text(
        json.dumps({"scenario": sc.name, "dry_run": cfg.dry_run, "table": table}, ensure_ascii=False, indent=2)
    )
    md = markdown(sc, table, results, cfg.dry_run, cfg.agent_cmd, cfg.model)
    (out / "report.md").write_text(md)
    return table, md
