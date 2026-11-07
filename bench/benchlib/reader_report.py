"""The reader benchmark's report: one row per configuration and size, mean ±
standard deviation over the seeds, and the gate's verdicts."""

import json
import time

from . import pmfixture
from . import reader_grade as rg
from .report import mean_sd
from .reader_runner import CONFIGS, TASKS, model_alias

ALIASES = ["opus", "sonnet", "haiku"]


def seed_rows(records):
    """One row per (config, size, seed), with both tasks together."""
    rows = {}
    for r in records:
        key = (r["config"], r["size"], r["seed"])
        row = rows.setdefault(
            key,
            {
                "calls": 0,
                "invented": 0,
                "confusions": 0,
                "cost_usd": 0.0,
                "prompt_tokens": 0,
                "output_tokens": 0,
                "seconds": 0.0,
                "delegations": 0,
                "main_dump_reads": 0,
                **{f"cost:{a}": 0.0 for a in ALIASES},
            },
        )
        g = r["grades"]
        row["calls"] += 1
        row["invented"] += g["invented"]
        row["confusions"] += g["confusions"]
        row["cost_usd"] += r["usage"]["cost_usd"]
        row["prompt_tokens"] += r["usage"]["prompt_tokens"]
        row["output_tokens"] += r["usage"]["output_tokens"]
        row["seconds"] += r["seconds"]
        row["delegations"] += r["transcript"]["agent_calls"]
        row["main_dump_reads"] += r["transcript"]["main_dump_reads"]
        for model, u in r["usage_by_model"].items():
            k = f"cost:{model_alias(model)}"
            row[k] = row.get(k, 0.0) + u["cost_usd"]
        if r["task"] == "audit":
            row["recall"] = g["recall"]
            row["precision"] = g["precision"]
            row["discarded"] = g["discarded"]
        elif r["task"] == "value":
            row["value_pct"] = g["value_pct"]
    for row in rows.values():
        row["invented_per_call"] = row["invented"] / row["calls"]
    return rows


def aggregate(records):
    """{config: {size: {metric: {mean, sd}, "seeds": n, "discarded_total": n}}}."""
    rows = seed_rows(records)
    table = {}
    for (c, z, _), row in sorted(rows.items(), key=lambda kv: order_key(kv[0])):
        table.setdefault(c, {}).setdefault(z, []).append(row)
    out = {}
    for c, sizes in table.items():
        for z, rs in sizes.items():
            agg = {"seeds": len(rs), "discarded_total": sum(r.get("discarded", 0) for r in rs)}
            for k in dict.fromkeys(k for r in rs for k in r):
                xs = [r[k] for r in rs if r.get(k) is not None]
                if xs:
                    agg[k] = mean_sd(xs)
            out.setdefault(c, {})[z] = agg
    return out


def by_task(records):
    """{config: {size: {task: {...}}}}: per call, mean over seeds."""
    groups = {}
    for r in records:
        groups.setdefault((r["config"], r["size"], r["task"]), []).append(r)
    out = {}
    for (c, z, t), rs in sorted(groups.items(), key=lambda kv: order_key(kv[0])):
        main_tools = sum(sum(r["transcript"]["main_tools"].values()) for r in rs)
        sub_tools = sum(sum(r["transcript"]["sub_tools"].values()) for r in rs)
        out.setdefault(c, {}).setdefault(z, {})[t] = {
            "calls": len(rs),
            "cost_usd": mean_sd([r["usage"]["cost_usd"] for r in rs]),
            "prompt_tokens": mean_sd([r["usage"]["prompt_tokens"] for r in rs]),
            "output_tokens": mean_sd([r["usage"]["output_tokens"] for r in rs]),
            "seconds": mean_sd([r["seconds"] for r in rs]),
            "main_tools": main_tools / len(rs),
            "sub_tools": sub_tools / len(rs),
            "agent_calls": sum(r["transcript"]["agent_calls"] for r in rs) / len(rs),
            "errors": sum(1 for r in rs if r["is_error"]),
            "parse_errors": sum(1 for r in rs if r["grades"].get("parse_error")),
            "models_seen": sorted({m for r in rs for m in r["usage_by_model"]}),
            "sub_models_seen": sorted({m for r in rs for m in r["transcript"]["sub_models"]}),
        }
    return out


def order_key(key):
    c, z = key[0], key[1]
    co = list(CONFIGS).index(c) if c in CONFIGS else 99
    zo = list(pmfixture.SIZES).index(z) if z in pmfixture.SIZES else 99
    return (co, zo, *[str(x) for x in key[2:]])


def mean_of(agg, k):
    v = agg.get(k)
    return v["mean"] if v else None


def gate(table):
    """The verdict of each delegated configuration at GATE_SIZE (README, "The gate")."""
    base = (table.get(rg.GATE_BASELINE) or {}).get(rg.GATE_SIZE)
    out = {
        "size": rg.GATE_SIZE,
        "baseline": rg.GATE_BASELINE,
        "baseline_recall": mean_of(base, "recall") if base else None,
        "baseline_value": mean_of(base, "value_pct") if base else None,
        "sane": None,
        "configs": {},
    }
    if out["baseline_recall"] is not None:
        out["sane"] = out["baseline_recall"] >= rg.GATE_SANITY_RECALL
    for c in table:
        if "+" not in c:
            continue
        agg = table[c].get(rg.GATE_SIZE)
        v = {"verdict": None, "criteria": {}}
        if not agg or out["baseline_recall"] is None or out["baseline_value"] is None:
            v["verdict"] = f"not judged: no {rg.GATE_BASELINE} and {c} runs of both tasks at size {rg.GATE_SIZE}"
            out["configs"][c] = v
            continue
        recall, value, inv = mean_of(agg, "recall"), mean_of(agg, "value_pct"), mean_of(agg, "invented_per_call")
        if recall is None or value is None:
            v["verdict"] = f"not judged: {c} lacks a task at size {rg.GATE_SIZE}"
            out["configs"][c] = v
            continue
        need_r = out["baseline_recall"] - rg.GATE_RECALL_DROP
        need_v = out["baseline_value"] - rg.GATE_VALUE_DROP
        v["criteria"] = {
            "recall": {"value": recall, "limit": need_r, "ok": recall >= need_r},
            "value": {"value": value, "limit": need_v, "ok": value >= need_v},
            "invented": {"value": inv, "limit": rg.GATE_MAX_INVENTED, "ok": inv <= rg.GATE_MAX_INVENTED},
            "discarded": {
                "value": agg["discarded_total"],
                "limit": rg.GATE_MAX_DISCARDED,
                "ok": agg["discarded_total"] <= rg.GATE_MAX_DISCARDED,
            },
        }
        ok = all(x["ok"] for x in v["criteria"].values())
        if not out["sane"]:
            v["verdict"] = "not judged: the task is ill-posed"
        else:
            v["verdict"] = "viable" if ok else "not viable"
        out["configs"][c] = v
    return out


def cell(agg, k, fmt):
    v = agg.get(k)
    if not v:
        return "–"
    return f"{fmt.format(v['mean'])} ± {fmt.format(v['sd'])}"


def fixture_table(records):
    """Per size: the fixture's characters, the tokens they should be, and the
    prompt tokens the envelopes report per call of the direct configurations."""
    sizes = {}
    for r in records:
        s = sizes.setdefault(r["size"], {"chars": {}, "tokens": [], "peak": []})
        s["chars"][(r["seed"])] = r["fixture"]["chars"]
        if r["models"]["subagent"] is None:
            s["tokens"].append(r["usage"]["prompt_tokens"])
            s["peak"].append(r["transcript"].get("main_peak_context", 0))

    def avg(xs):
        return f"{sum(xs) / len(xs):,.0f}" if xs else "–"

    l = [
        "### Fixture sizes",
        "",
        "Peak context: the largest single request of a direct call's session (system prompt plus",
        "what was read), the figure to recalibrate the sizes with. Input tokens: the sum over the",
        "call's requests, which counts the context again on every turn (a cost figure).",
        "",
        f"| Size | Target tokens | Characters | Tokens at {pmfixture.CHARS_PER_TOKEN:g} chars/token | "
        "Peak context per direct call | Input tokens per direct call |",
        "|---|---:|---:|---:|---:|---:|",
    ]
    for z in sorted(sizes, key=lambda z: order_key(("", z))):
        s = sizes[z]
        chars = sum(s["chars"].values()) / len(s["chars"])
        target = f"{pmfixture.SIZES[z]:,}" if z in pmfixture.SIZES else "?"
        l.append(
            f"| {z} | {target} | {chars:,.0f} | {chars / pmfixture.CHARS_PER_TOKEN:,.0f} | "
            f"{avg(s['peak'])} | {avg(s['tokens'])} |"
        )
    return l + [""]


COLUMNS = [
    ("recall", "Audit recall %", "{:.0f}"),
    ("precision", "Precision %", "{:.0f}"),
    ("invented_per_call", "Invented / call", "{:.1f}"),
    ("confusions", "Confusions", "{:.1f}"),
    ("value_pct", "Value %", "{:.0f}"),
    ("cost_usd", "Cost USD", "{:.3f}"),
    ("cost:opus", "opus USD", "{:.3f}"),
    ("cost:sonnet", "sonnet USD", "{:.3f}"),
    ("cost:haiku", "haiku USD", "{:.3f}"),
    ("prompt_tokens", "Input tokens", "{:,.0f}"),
    ("seconds", "Seconds", "{:.0f}"),
    ("delegations", "Delegations", "{:.1f}"),
    ("main_dump_reads", "Main dump reads", "{:.1f}"),
]


def markdown(records, table, tasks_table, verdicts, opts):
    seeds = sorted({r["seed"] for r in records})
    l = ["# Reader benchmark", ""]
    if any(r["dry_run"] for r in records):
        l += [
            "> **Dry run.** The reader is `bench/fake_reader.py`, with no model: the numbers",
            "> only show that the harness works. They are not results.",
            "",
        ]
    l += [
        f"- Date: {time.strftime('%Y-%m-%d %H:%M')}",
        f"- Reader command: `{opts.reader_cmd}`" + (f" with `--effort {opts.effort}`" if opts.effort else ""),
        f"- Configurations: {', '.join(table)}; seeds: {', '.join(map(str, seeds))}; "
        f"tasks: {', '.join(t for t in TASKS if any(r['task'] == t for r in records))}",
        f"- Calls: {len(records)}; the exact argv of each one is in its `runs/<config>-<size>-s<seed>-<task>/run.json`",
        "",
    ]
    l += fixture_table(records)
    l += [
        "## Results",
        "",
        "Mean ± standard deviation over the seeds. Per seed, both tasks together: confusions,",
        "cost, input tokens (uncached + cache writes + cache reads), seconds, delegations and",
        "main-session dump reads are the sum of the two calls; invented is per call.",
        "",
        "| Config | Size | Seeds | " + " | ".join(h for _, h, _ in COLUMNS) + " |",
        "|---|---|---:|" + "---:|" * len(COLUMNS),
    ]
    for c, sizes in table.items():
        for z, agg in sizes.items():
            l.append(f"| {c} | {z} | {agg['seeds']} | " + " | ".join(cell(agg, k, f) for k, _, f in COLUMNS) + " |")
    l += [
        "",
        "### By task (per call)",
        "",
        "| Config | Size | Task | Calls | Cost USD | Input tokens | Output tokens | Seconds | "
        "Main tool calls | Subagent tool calls | Agent calls | Errors | Unparseable | Models (modelUsage) |",
        "|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|",
    ]
    for c, sizes in tasks_table.items():
        for z, ts in sizes.items():
            for t, x in ts.items():
                l.append(
                    f"| {c} | {z} | {t} | {x['calls']} | {cell(x, 'cost_usd', '{:.3f}')} | "
                    f"{cell(x, 'prompt_tokens', '{:,.0f}')} | {cell(x, 'output_tokens', '{:,.0f}')} | "
                    f"{cell(x, 'seconds', '{:.0f}')} | {x['main_tools']:.1f} | {x['sub_tools']:.1f} | "
                    f"{x['agent_calls']:.1f} | {x['errors']} | {x['parse_errors']} | "
                    f"{', '.join(x['models_seen']) or '–'} |"
                )
    l += ["", f"## Gate (size {rg.GATE_SIZE}, against {rg.GATE_BASELINE})", ""]
    if verdicts["baseline_recall"] is None:
        l += [f"Not evaluated: there are no `{rg.GATE_BASELINE}` audit runs at size {rg.GATE_SIZE}.", ""]
    elif not verdicts["sane"]:
        l += [
            f"**The task is ill-posed:** `{rg.GATE_BASELINE}` itself has an audit recall of "
            f"{verdicts['baseline_recall']:.0f} % at size {rg.GATE_SIZE}, under {rg.GATE_SANITY_RECALL:.0f} %. "
            "Fix the fixture or the prompt before judging the readers.",
            "",
        ]
    else:
        l += [
            f"Baseline: audit recall {verdicts['baseline_recall']:.0f} %, value "
            f"{verdicts['baseline_value']:.0f} % (sanity: recall ≥ {rg.GATE_SANITY_RECALL:.0f} %, met).",
            "",
        ]
    if verdicts["configs"]:
        l += [
            f"| Config | Audit recall (≥ baseline − {rg.GATE_RECALL_DROP:g}) | Value % (≥ baseline − "
            f"{rg.GATE_VALUE_DROP:g}) | Invented / call (≤ {rg.GATE_MAX_INVENTED:g}) | Discarded reported, "
            f"total (≤ {rg.GATE_MAX_DISCARDED}) | Verdict |",
            "|---|---:|---:|---:|---:|---|",
        ]
        for c, v in verdicts["configs"].items():
            cr = v["criteria"]

            def show(k, fmt):
                if k not in cr:
                    return "–"
                x = cr[k]
                return f"{fmt.format(x['value'])} {'ok' if x['ok'] else 'FAIL'} (limit {fmt.format(x['limit'])})"

            l.append(
                f"| {c} | {show('recall', '{:.0f}')} | {show('value', '{:.0f}')} | {show('invented', '{:.2f}')} | "
                f"{show('discarded', '{:.0f}')} | **{v['verdict']}** |"
            )
        l.append("")
    else:
        l += ["No delegated configuration was run.", ""]
    if any(z == "l" for c in table for z in table[c]):
        l += ["Size `l` is reported, not gated.", ""]
    l += [
        "## How it is calculated",
        "",
        f"- A citation is valid if its quote (normalized: case, accents, whitespace, quotes and",
        f"  punctuation; at least {rg.MIN_QUOTE_CHARS} characters) is in lines [line − {rg.LINE_SLACK}, "
        f"line + {rg.LINE_SLACK}] of the cited file.",
        "  Otherwise it is **invented**. A valid one maps to the planted sentence whose lines it overlaps",
        "  (± 2) and whose text it shares.",
        "- **Audit recall**: omitted ideas cited / omitted ideas. **Precision**: omitted ideas cited / valid",
        "  citations (deduplicated by what they point at). **Confusions**: discarded ideas or distractors",
        "  reported as missing, plus value answers with a forbidden word.",
        "- **Value %**: questions whose `value` has the expected words and none of the forbidden ones",
        "  (the old value of a changed idea, a close distractor).",
        "- **Cost**: `total_cost_usd` and `modelUsage[*].costUSD` of the envelopes (0 in dry runs).",
        "- **Delegations**: calls of the subagent tool in the main session; **main dump reads**: Read or",
        "  Grep calls of the main session on `.pm/dumps/` (in a delegated configuration, the delegation",
        "  was bypassed).",
        "",
    ]
    l += warnings(records)
    return "\n".join(l)


def warnings(records):
    l = []
    err = [r for r in records if r["is_error"]]
    if err:
        l.append(f"- {len(err)} calls exited with an error or timed out: "
                 + ", ".join(f"{r['config']}-{r['size']}-s{r['seed']}-{r['task']}" for r in err[:8]))
    bad = [r for r in records if r["grades"].get("parse_error")]
    if bad:
        l.append(f"- {len(bad)} answers could not be parsed (they score 0): "
                 + ", ".join(f"{r['config']}-{r['size']}-s{r['seed']}-{r['task']}" for r in bad[:8]))
    nodel = [r for r in records if r["models"]["subagent"] and r["transcript"]["agent_calls"] == 0]
    if nodel:
        l.append(f"- {len(nodel)} delegated calls never called the subagent tool.")
    bypass = [r for r in records if r["models"]["subagent"] and r["transcript"]["main_read_dumps"]]
    if bypass:
        l.append(f"- {len(bypass)} delegated calls read `.pm/dumps/` from the main session.")
    changed = [r for r in records if not r["fixture"].get("unchanged", True)]
    if changed:
        l.append(f"- {len(changed)} calls changed their copy of the fixture.")
    shas = {}
    for r in records:
        shas.setdefault((r["size"], r["seed"]), set()).add(r["fixture"]["sha256"])
    mixed = [k for k, v in shas.items() if len(v) > 1]
    if mixed:
        l.append(f"- Runs of the same size and seed read different fixtures: {mixed}. Use a new --out.")
    if len({r["dry_run"] for r in records}) > 1:
        l.append("- This folder mixes dry and real runs.")
    return (["## Warnings", "", *l, ""]) if l else []


def write(out, records, opts):
    table = aggregate(records)
    tasks_table = by_task(records)
    verdicts = gate(table)
    md = markdown(records, table, tasks_table, verdicts, opts)
    (out / "report.md").write_text(md)
    (out / "report.json").write_text(
        json.dumps(
            {
                "dry_run": any(r["dry_run"] for r in records),
                "table": table,
                "by_task": tasks_table,
                "gate": verdicts,
                "gate_constants": {
                    "size": rg.GATE_SIZE,
                    "baseline": rg.GATE_BASELINE,
                    "recall_drop": rg.GATE_RECALL_DROP,
                    "value_drop": rg.GATE_VALUE_DROP,
                    "max_invented_per_call": rg.GATE_MAX_INVENTED,
                    "max_discarded_total": rg.GATE_MAX_DISCARDED,
                    "sanity_recall": rg.GATE_SANITY_RECALL,
                },
            },
            ensure_ascii=False,
            indent=2,
        )
    )
    return table, verdicts, md
