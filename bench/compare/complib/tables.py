"""The tables (design §4): one per task × model, a row per arm, raw measures,
no composite score; the verdicts of ANALYSIS.md against plain and kontext."""

import json
import statistics
from pathlib import Path

from . import analysis
from .arms import ARM_NAMES

TASK_ORDER = {"smoke": 0, "S1": 1, "S2": 2, "L": 3}


def load_runs(folder, include_dry=True):
    out = []
    for p in sorted(Path(folder).rglob("run.json")):
        try:
            r = json.loads(p.read_text())
        except json.JSONDecodeError:
            continue
        if r.get("arm") and (include_dry or not r.get("dry_run")):
            out.append(r)
    return out


def med(xs):
    xs = [x for x in xs if x is not None]
    if not xs:
        return "–"
    m = statistics.median(xs)
    if len(xs) == 1:
        return _f(m)
    return f"{_f(m)} [{_f(min(xs))}–{_f(max(xs))}]"


def mean_sd(xs, digits=1):
    xs = [x for x in xs if x is not None]
    if not xs:
        return "–"
    sd = statistics.stdev(xs) if len(xs) > 1 else 0.0
    return f"{statistics.mean(xs):.{digits}f} ± {sd:.{digits}f}"


def _f(x):
    if isinstance(x, float):
        return f"{x:.3f}" if abs(x) < 10 else f"{x:.1f}"
    return str(x)


def cost(r):
    return (r.get("usage") or {}).get("cost_usd")


def groups(runs):
    g = {}
    for r in runs:
        g.setdefault((r["task"], r.get("model_tag")), {}).setdefault(r["arm"], []).append(r)
    return dict(sorted(g.items(), key=lambda kv: (TASK_ORDER.get(kv[0][0], 9), kv[0][1] or "")))


def kept(r):
    return ((r.get("requirements") or {}).get("kept"))


def report(runs):
    md, js = [], {}
    for (task, model), arms in groups(runs).items():
        md.append(f"## {task} · {model}\n")
        rows = []
        head = ["arm", "runs", "score", "cost USD (median [range])", "distill USD", "session s", "settle s",
                "scripted turns", "model requests", "interventions", "denials", "errors"]  # fmt: skip
        md.append("| " + " | ".join(head) + " |")
        md.append("|" + "---|" * len(head))
        for arm in [a for a in ARM_NAMES if a in arms]:
            rs = arms[arm]
            if task == "L":
                sc = mean_sd([kept(r) for r in rs]) + " / 14"
            elif task == "smoke":
                sc = mean_sd([(r.get("smoke") or {}).get("recalled") for r in rs]) + f" / {(rs[0].get('smoke') or {}).get('of', '?')}"
            else:
                sc = mean_sd([((r.get("hidden") or {}).get("pct")) for r in rs]) + " %"
            row = [
                arm, str(len(rs)), sc, med([cost(r) for r in rs]),
                med([((r.get("usage_by") or {}).get("distill") or {}).get("cost_usd") for r in rs]),
                med([r.get("session_seconds") for r in rs]), med([(r.get("settle") or {}).get("seconds") for r in rs]),
                med([(r.get("turns") or {}).get("scripted") for r in rs]), med([(r.get("turns") or {}).get("model_requests") for r in rs]),
                med([(r.get("interventions") or {}).get("count") for r in rs]), med([(r.get("permission_denials") or {}).get("count") for r in rs]),
                str(sum(len(r.get("errors") or []) + (r.get("agent_errors") or 0) for r in rs)),
            ]  # fmt: skip
            rows.append(dict(zip(head, row)))
            md.append("| " + " | ".join(row) + " |")
        js[f"{task}/{model}"] = {"rows": rows}
        md.append("")
        if task == "L":
            md += l_tables(arms)
        md += verdicts(task, arms)
    return "\n".join(md), js


def l_tables(arms):
    md = ["### L by class, by day, confusions\n"]
    classes = ["A", "B", "answer", "C", "discarded"]
    head = ["arm"] + [f"class {c}" for c in classes] + ["day 3 (new chat)", "day 4 (machine B)", "confusions", "coded before loss", "regressed"]
    md.append("| " + " | ".join(head) + " |")
    md.append("|" + "---|" * len(head))
    for arm in [a for a in ARM_NAMES if a in arms]:
        rs = arms[arm]
        req = [r.get("requirements") or {} for r in rs]

        def share(bucket, key):
            vals = [(q.get(bucket) or {}).get(key) for q in req]
            vals = [v for v in vals if v]
            if not vals:
                return "–"
            return f"{statistics.mean(v['kept'] for v in vals):.1f} / {vals[0]['of']}"

        row = [arm] + [share("by_class", c) for c in classes] + [share("by_day", "3"), share("by_day", "4")]
        row += [med([q.get("confusions") for q in req]),
                ", ".join(sorted({x for q in req for x in q.get("coded_before_loss", [])})) or "none",
                ", ".join(sorted({x for q in req for x in q.get("regressed", [])})) or "none"]  # fmt: skip
        md.append("| " + " | ".join(row) + " |")
    md.append("")
    fails = {}
    for arm, rs in arms.items():
        for r in rs:
            for x in (r.get("requirements") or {}).get("failed", []):
                fails.setdefault(arm, {}).setdefault(x, 0)
                fails[arm][x] += 1
    if fails:
        md.append("Failed requirements (runs): " + "; ".join(f"{a}: " + ", ".join(f"{k}×{v}" for k, v in sorted(f.items())) for a, f in fails.items()))
        md.append("")
    return md


def verdicts(task, arms):
    md = []
    for base in ("plain", "kontext"):
        if base not in arms:
            continue
        lines = []
        for arm, rs in arms.items():
            if arm == base:
                continue
            b = arms[base]
            c = analysis.cost_verdict([cost(r) for r in rs], [cost(r) for r in b])
            t = analysis.cost_verdict([r.get("session_seconds") for r in rs], [r.get("session_seconds") for r in b])
            line = f"- {arm} vs {base}: cost {c}, time {t}"
            if task == "L":
                line += ", requirements " + analysis.requirements_verdict([kept(r) for r in rs], [kept(r) for r in b])
            lines.append(line)
        if lines:
            md.append(f"Against {base} (ANALYSIS.md rules):\n")
            md += lines
            md.append("")
    return md


def write(folder, runs=None):
    runs = runs if runs is not None else load_runs(folder)
    md, js = report(runs)
    header = "# Comparison benchmark\n\n"
    if any(r.get("dry_run") for r in runs):
        header += "**Dry run**: the agent is bench/fake_agent.py; the numbers only show that the harness works.\n\n"
    fz = {json.dumps(r.get("freeze"), sort_keys=True) for r in runs}
    if len(fz) > 1:
        header += "**Warning:** these runs were made with different frozen files.\n\n"
    Path(folder, "report.md").write_text(header + md + "\n")
    Path(folder, "report.json").write_text(json.dumps(js, indent=2, ensure_ascii=False))
    return header + md
