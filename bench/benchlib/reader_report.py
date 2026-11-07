"""The reader benchmark's report: one row per variant (configuration, prompt
version, main effort, subagent effort) and size, mean ± standard deviation over
the seeds, and the gate's verdicts, one gate per prompt version."""

import json
import time

from . import pmfixture
from . import reader_grade as rg
from .report import mean_sd
from .reader_runner import CONFIGS, EFFORT_LEVELS, TASKS, model_alias, variant_name, variant_of

ALIASES = ["opus", "sonnet", "haiku"]


def variant_key(r):
    """(config, prompt, main effort, subagent effort) of a run record."""
    return variant_of(r["config"], r.get("prompt_version", "v1"), r.get("effort_main"), r.get("effort_sub"))


def seed_rows(records):
    """One row per (variant, size, seed), with both tasks together."""
    rows = {}
    for r in records:
        key = (variant_key(r), r["size"], r["seed"])
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
            ideas = g.get("ideas")
            if ideas:
                row["ideas_enumerated_pct"] = ideas["enumerated_pct"]
                row["ideas_marked_present"] = ideas["marked_present"]
                row["ideas_coverage_pct"] = ideas["coverage_pct"]
        elif r["task"] == "value":
            row["value_pct"] = g["value_pct"]
    for row in rows.values():
        row["invented_per_call"] = row["invented"] / row["calls"]
    return rows


def aggregate(records):
    """{variant: {size: {metric: {mean, sd}, "seeds": n, "discarded_total": n}}}."""
    rows = seed_rows(records)
    table = {}
    for (v, z, _), row in sorted(rows.items(), key=lambda kv: order_key(kv[0][0], kv[0][1])):
        table.setdefault(v, {}).setdefault(z, []).append(row)
    out = {}
    for v, sizes in table.items():
        for z, rs in sizes.items():
            agg = {"seeds": len(rs), "discarded_total": sum(r.get("discarded", 0) for r in rs)}
            for k in dict.fromkeys(k for r in rs for k in r):
                xs = [r[k] for r in rs if r.get(k) is not None]
                if xs:
                    agg[k] = mean_sd(xs)
            out.setdefault(v, {})[z] = agg
    return out


def by_task(records):
    """{variant: {size: {task: {...}}}}: per call, mean over seeds."""
    groups = {}
    for r in records:
        groups.setdefault((variant_key(r), r["size"], r["task"]), []).append(r)
    out = {}
    for (v, z, t), rs in sorted(groups.items(), key=lambda kv: order_key(kv[0][0], kv[0][1], kv[0][2])):
        main_tools = sum(sum(r["transcript"]["main_tools"].values()) for r in rs)
        sub_tools = sum(sum(r["transcript"]["sub_tools"].values()) for r in rs)
        out.setdefault(v, {}).setdefault(z, {})[t] = {
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
            "effort_seen_main": sorted({e for r in rs for e in r.get("effort_seen_main") or []}),
            "effort_seen_sub": sorted({e for r in rs for e in r.get("effort_seen_sub") or []}),
        }
    return out


def effort_order(e):
    return -1 if e is None else (EFFORT_LEVELS.index(e) if e in EFFORT_LEVELS else 99)


def order_key(variant, size, *rest):
    c, p, em, es = variant
    co = list(CONFIGS).index(c) if c in CONFIGS else 99
    zo = list(pmfixture.SIZES).index(size) if size in pmfixture.SIZES else 99
    return (p, co, effort_order(em), effort_order(es), zo, *[str(x) for x in rest])


def mean_of(agg, k):
    v = agg.get(k)
    return v["mean"] if v else None


def judge(agg, base_recall, base_value, sane, label):
    """The verdict of one delegated variant against its baseline."""
    v = {"verdict": None, "criteria": {}}
    if not agg or base_recall is None or base_value is None:
        v["verdict"] = f"not judged: no {label} and variant runs of both tasks at size {rg.GATE_SIZE}"
        return v
    recall, value, inv = mean_of(agg, "recall"), mean_of(agg, "value_pct"), mean_of(agg, "invented_per_call")
    if recall is None or value is None:
        v["verdict"] = f"not judged: the variant lacks a task at size {rg.GATE_SIZE}"
        return v
    need_r = base_recall - rg.GATE_RECALL_DROP
    need_v = base_value - rg.GATE_VALUE_DROP
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
    if not sane:
        v["verdict"] = "not judged: the task is ill-posed"
    else:
        v["verdict"] = "viable" if ok else "not viable"
    return v


def gate(table):
    """The verdicts at GATE_SIZE (README, "The gate"), one gate per prompt
    version: every delegated variant of a prompt version (any effort) against
    the GATE_BASELINE variant of the same prompt version with the default
    effort. No baseline: its variants are not judged."""
    out = {"size": rg.GATE_SIZE, "baseline": rg.GATE_BASELINE, "prompts": {}}
    for p in sorted({v[1] for v in table}):
        base_v = variant_of(rg.GATE_BASELINE, p)
        base = (table.get(base_v) or {}).get(rg.GATE_SIZE)
        g = {
            "baseline_variant": variant_name(base_v),
            "baseline_recall": mean_of(base, "recall") if base else None,
            "baseline_value": mean_of(base, "value_pct") if base else None,
            "sane": None,
            "variants": {},
        }
        if g["baseline_recall"] is not None:
            g["sane"] = g["baseline_recall"] >= rg.GATE_SANITY_RECALL
        label = f"{rg.GATE_BASELINE} ({p}, default effort)"
        for v in sorted(table, key=lambda v: order_key(v, "")):
            if v[1] != p or "+" not in v[0]:
                continue
            verdict = judge(table[v].get(rg.GATE_SIZE), g["baseline_recall"], g["baseline_value"], g["sane"], label)
            g["variants"][variant_name(v)] = {"config": v[0], "effort_main": v[2], "effort_sub": v[3], **verdict}
        out["prompts"][p] = g
    return out


def cell(agg, k, fmt):
    v = agg.get(k)
    if not v:
        return "–"
    return f"{fmt.format(v['mean'])} ± {fmt.format(v['sd'])}"


def effort_label(e, delegated=True):
    return e if e else ("default" if delegated else "–")


def variant_cells(v):
    c, p, em, es = v
    return f"{c} | {p} | {effort_label(em)} | {effort_label(es, '+' in c)}"


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
    for z in sorted(sizes, key=lambda z: order_key(("", "", None, None), z)):
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
IDEAS_COLUMNS = [
    ("ideas_enumerated_pct", "Omitted ideas enumerated %", "{:.0f}"),
    ("ideas_marked_present", "Enumerated but marked present", "{:.1f}"),
    ("ideas_coverage_pct", "All ideas enumerated %", "{:.0f}"),
]
VARIANT_HEAD = "| Config | Prompt | Main effort | Sub effort |"


def header(records, table, opts):
    seeds = sorted({r["seed"] for r in records})
    l = [
        f"- Date: {time.strftime('%Y-%m-%d %H:%M')}",
        f"- Reader command: `{opts.reader_cmd}`",
    ]
    specs = sorted({(r.get("spec") or {}).get("version") or 0 for r in records})
    shas = sorted({(r.get("spec") or {}).get("sha256") or "?" for r in records})
    l.append(f"- Spec: `bench/reader/spec.toml` version {', '.join(map(str, specs))}, sha256 "
             + ", ".join(f"`{s}`" for s in shas))
    prompts = {}
    for r in records:
        prompts.setdefault(r.get("prompt_version", "v1"), set()).add(r.get("prompt_set_sha256") or "?")
    for p in sorted(prompts):
        l.append(f"- Prompt {p}: sha256 " + ", ".join(f"`{s}`" for s in sorted(prompts[p]))
                 + " (every section it uses, `prompt_set_sha256` in run.json)")
    l += [
        f"- Variants: {', '.join(variant_name(v) for v in table)}",
        f"- Seeds: {', '.join(map(str, seeds))}; tasks: "
        f"{', '.join(t for t in TASKS if any(r['task'] == t for r in records))}",
        f"- Calls: {len(records)}; the exact argv of each one is in its `runs/<run>/run.json`, the run named "
        "`<config>-<prompt>-emain_<effort>-esub_<effort>-<size>-s<seed>-<task>`",
        "",
    ]
    return l


def gate_section(verdicts):
    l = [f"## Gate (size {rg.GATE_SIZE}, against {rg.GATE_BASELINE}, per prompt version)", ""]
    if not verdicts["prompts"]:
        return l + ["No runs.", ""]
    for p, g in verdicts["prompts"].items():
        l += [f"### Prompt {p}", ""]
        if g["baseline_recall"] is None:
            l += [f"No baseline: there are no `{g['baseline_variant']}` audit runs at size {rg.GATE_SIZE}.", ""]
        elif not g["sane"]:
            l += [
                f"**The task is ill-posed:** `{g['baseline_variant']}` itself has an audit recall of "
                f"{g['baseline_recall']:.0f} % at size {rg.GATE_SIZE}, under {rg.GATE_SANITY_RECALL:.0f} %. "
                "Fix the fixture or the prompt before judging the readers.",
                "",
            ]
        else:
            value = f"{g['baseline_value']:.0f} %" if g["baseline_value"] is not None else "–"
            l += [
                f"Baseline `{g['baseline_variant']}`: audit recall {g['baseline_recall']:.0f} %, value "
                f"{value} (sanity: recall ≥ {rg.GATE_SANITY_RECALL:.0f} %, met).",
                "",
            ]
        if not g["variants"]:
            l += ["No delegated variant was run with this prompt.", ""]
            continue
        l += [
            f"| Config | Main effort | Sub effort | Audit recall (≥ baseline − {rg.GATE_RECALL_DROP:g}) | Value % "
            f"(≥ baseline − {rg.GATE_VALUE_DROP:g}) | Invented / call (≤ {rg.GATE_MAX_INVENTED:g}) | Discarded "
            f"reported, total (≤ {rg.GATE_MAX_DISCARDED}) | Verdict |",
            "|---|---|---|---:|---:|---:|---:|---|",
        ]
        for v in g["variants"].values():
            cr = v["criteria"]

            def show(k, fmt):
                if k not in cr:
                    return "–"
                x = cr[k]
                return f"{fmt.format(x['value'])} {'ok' if x['ok'] else 'FAIL'} (limit {fmt.format(x['limit'])})"

            l.append(
                f"| {v['config']} | {effort_label(v['effort_main'])} | {effort_label(v['effort_sub'])} | "
                f"{show('recall', '{:.0f}')} | {show('value', '{:.0f}')} | {show('invented', '{:.2f}')} | "
                f"{show('discarded', '{:.0f}')} | **{v['verdict']}** |"
            )
        l.append("")
    return l


def markdown(records, table, tasks_table, verdicts, opts):
    l = ["# Reader benchmark", ""]
    if any(r["dry_run"] for r in records):
        l += [
            "> **Dry run.** The reader is `bench/fake_reader.py`, with no model: the numbers",
            "> only show that the harness works. They are not results.",
            "",
        ]
    l += header(records, table, opts)
    l += fixture_table(records)
    l += [
        "## Results",
        "",
        "Mean ± standard deviation over the seeds. Per seed, both tasks together: confusions,",
        "cost, input tokens (uncached + cache writes + cache reads), seconds, delegations and",
        "main-session dump reads are the sum of the two calls; invented is per call. Effort",
        "`default`: not passed (the session's own level); the efforts seen are in the next table.",
        "",
        VARIANT_HEAD + " Size | Seeds | " + " | ".join(h for _, h, _ in COLUMNS) + " |",
        "|---|---|---|---|---|---:|" + "---:|" * len(COLUMNS),
    ]
    for v, sizes in table.items():
        for z, agg in sizes.items():
            l.append(f"| {variant_cells(v)} | {z} | {agg['seeds']} | "
                     + " | ".join(cell(agg, k, f) for k, _, f in COLUMNS) + " |")
    l += [
        "",
        "### By task (per call)",
        "",
        VARIANT_HEAD + " Size | Task | Calls | Cost USD | Input tokens | Output tokens | Seconds | "
        "Main tool calls | Subagent tool calls | Agent calls | Errors | Unparseable | Effort seen (main / sub) | "
        "Models (modelUsage) |",
        "|---|---|---|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|---|",
    ]
    for v, sizes in tasks_table.items():
        for z, ts in sizes.items():
            for t, x in ts.items():
                seen = f"{', '.join(x['effort_seen_main']) or '–'} / {', '.join(x['effort_seen_sub']) or '–'}"
                l.append(
                    f"| {variant_cells(v)} | {z} | {t} | {x['calls']} | {cell(x, 'cost_usd', '{:.3f}')} | "
                    f"{cell(x, 'prompt_tokens', '{:,.0f}')} | {cell(x, 'output_tokens', '{:,.0f}')} | "
                    f"{cell(x, 'seconds', '{:.0f}')} | {x['main_tools']:.1f} | {x['sub_tools']:.1f} | "
                    f"{x['agent_calls']:.1f} | {x['errors']} | {x['parse_errors']} | {seen} | "
                    f"{', '.join(x['models_seen']) or '–'} |"
                )
    l.append("")
    ideas_rows = [(v, z, agg) for v, sizes in table.items() for z, agg in sizes.items()
                  if agg.get("ideas_enumerated_pct")]
    if ideas_rows:
        l += [
            "### The audit's `ideas` list (prompt v2; a diagnostic, not gated)",
            "",
            "Per omitted idea: never enumerated (the reader did not see it as an idea, or did not",
            "read it), enumerated but marked as present in items.md (a matching error), or",
            "enumerated as missing. All ideas enumerated: the user's ideas (every kind) that the",
            "list points at. Mean ± standard deviation over the seeds.",
            "",
            VARIANT_HEAD + " Size | Seeds | " + " | ".join(h for _, h, _ in IDEAS_COLUMNS) + " |",
            "|---|---|---|---|---|---:|" + "---:|" * len(IDEAS_COLUMNS),
        ]
        for v, z, agg in ideas_rows:
            l.append(f"| {variant_cells(v)} | {z} | {agg['seeds']} | "
                     + " | ".join(cell(agg, k, f) for k, _, f in IDEAS_COLUMNS) + " |")
        l.append("")
    l += gate_section(verdicts)
    if any(z == "l" for v in table for z in table[v]):
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
        "  reported as missing, plus value answers with a forbidden word. With prompt v2, only `missing`",
        "  is graded this way; the `ideas` list is the diagnostic above.",
        "- **Value %**: questions whose `value` has the expected words and none of the forbidden ones",
        "  (every superseded value of a changed idea, a close distractor).",
        "- **Cost**: `total_cost_usd` and `modelUsage[*].costUSD` of the envelopes (0 in dry runs).",
        "- **Delegations**: calls of the subagent tool in the main session; **main dump reads**: Read or",
        "  Grep calls of the main session on `.pm/dumps/` (in a delegated configuration, the delegation",
        "  was bypassed).",
        "- **Effort seen**: `effort` of the assistant records and `output_config.effort` of the requests",
        "  in the call's transcripts, main session and subagent apart.",
        "",
    ]
    l += warnings(records)
    return "\n".join(l)


def run_label(r):
    return r.get("run") or f"{r['config']}-{r['size']}-s{r['seed']}-{r['task']}"


def warnings(records):
    l = []
    err = [r for r in records if r["is_error"]]
    if err:
        l.append(f"- {len(err)} calls exited with an error or timed out: " + ", ".join(run_label(r) for r in err[:8]))
    bad = [r for r in records if r["grades"].get("parse_error")]
    if bad:
        l.append(f"- {len(bad)} answers could not be parsed (they score 0): "
                 + ", ".join(run_label(r) for r in bad[:8]))
    noideas = [r for r in records if r["grades"].get("ideas") and not r["grades"]["ideas"]["parsed"]]
    if noideas:
        l.append(f"- {len(noideas)} v2 audit answers have no `ideas` list: " + ", ".join(run_label(r) for r in noideas[:8]))
    nodel = [r for r in records if r["models"]["subagent"] and r["transcript"]["agent_calls"] == 0]
    if nodel:
        l.append(f"- {len(nodel)} delegated calls never called the subagent tool.")
    bypass = [r for r in records if r["models"]["subagent"] and r["transcript"]["main_read_dumps"]]
    if bypass:
        l.append(f"- {len(bypass)} delegated calls read `.pm/dumps/` from the main session.")
    for side, what in (("main", "main session"), ("sub", "subagent")):
        off = [r for r in records if r.get(f"effort_{side}")
               and r.get(f"effort_seen_{side}") != [r[f"effort_{side}"]]]
        if off:
            l.append(f"- {len(off)} calls did not show the requested effort in the {what}'s transcript: "
                     + ", ".join(f"{run_label(r)} (asked {r[f'effort_{side}']}, seen "
                                 f"{', '.join(r.get(f'effort_seen_{side}') or []) or 'nothing'})" for r in off[:8]))
    changed = [r for r in records if not r["fixture"].get("unchanged", True)]
    if changed:
        l.append(f"- {len(changed)} calls changed their copy of the fixture.")
    shas = {}
    for r in records:
        shas.setdefault((r["size"], r["seed"]), set()).add(r["fixture"]["sha256"])
    mixed = [k for k, v in shas.items() if len(v) > 1]
    if mixed:
        l.append(f"- Runs of the same size and seed read different fixtures: {mixed}. Use a new --out.")
    prompts = {}
    for r in records:
        prompts.setdefault(r.get("prompt_version", "v1"), set()).add(r.get("prompt_set_sha256"))
    drift = [p for p, s in prompts.items() if len(s) > 1]
    if drift:
        l.append(f"- prompt.md changed between runs of the same prompt version: {', '.join(sorted(drift))}.")
    if len({r["dry_run"] for r in records}) > 1:
        l.append("- This folder mixes dry and real runs.")
    return (["## Warnings", "", *l, ""]) if l else []


def table_json(table):
    """The table keyed by variant name, for report.json."""
    return {
        variant_name(v): {"config": v[0], "prompt": v[1], "effort_main": v[2], "effort_sub": v[3], "sizes": sizes}
        for v, sizes in table.items()
    }


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
                "table": table_json(table),
                "by_task": table_json(tasks_table),
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
