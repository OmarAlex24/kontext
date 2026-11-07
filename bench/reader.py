#!/usr/bin/env python3
"""The reader benchmark: can a cheaper model read a project's `.pm/` memory
folder for an Opus coordinator? See bench/README.md, "Reader benchmark".

    python3 bench/reader.py --dry-run             # dry run, no model
    python3 bench/reader.py --yes                 # for real: spends tokens

Each invocation runs one prompt version and one pair of efforts; several
invocations into the same --out build one report, a row per variant.
"""

import argparse
import os
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from benchlib import pmfixture, reader_report  # noqa: E402
from benchlib import reader_runner as rr  # noqa: E402

RESULTS = Path(__file__).resolve().parent / "results"


def parse(argv):
    p = argparse.ArgumentParser(
        prog="bench/reader.py",
        description="Measures how well each configuration reads a project's .pm/ memory folder, and at what cost.",
    )
    p.add_argument(
        "--configs",
        default=",".join(rr.DEFAULT_CONFIGS),
        help="comma-separated: " + ", ".join(rr.CONFIGS) + f" (default {','.join(rr.DEFAULT_CONFIGS)})",
    )
    p.add_argument("--sizes", default=",".join(pmfixture.SIZES), help="comma-separated: s, m, l (default all)")
    p.add_argument("--seeds", type=int, default=1, help="fixtures per size (1, the pilot)")
    p.add_argument("--first-seed", type=int, default=1, help="first seed, to add seeds to an earlier --out (1)")
    p.add_argument("--tasks", default=",".join(rr.TASKS), help="comma-separated: audit, value (default both)")
    p.add_argument(
        "--out",
        default=None,
        help="results folder (default bench/results/<date>-reader[-dry]/); an existing one is added to",
    )
    p.add_argument("--reader-cmd", default=None, help=f"the base command (default: {rr.DEFAULT_READER_CMD})")
    p.add_argument(
        "--prompt",
        default=rr.DEFAULT_PROMPT,
        choices=list(rr.PROMPTS),
        help=f"prompt version: v1 and v2 of bench/reader/prompt.md, or skill, the texts kontext install ships "
        f"(default {rr.DEFAULT_PROMPT}, the pilot's)",
    )
    p.add_argument(
        "--effort",
        default=None,
        choices=rr.EFFORT_LEVELS,
        help="the main (or only) session's effort, passed as --effort <level> (default: not passed)",
    )
    p.add_argument(
        "--sub-effort",
        default=None,
        choices=rr.EFFORT_LEVELS,
        help="the pm-reader subagent's effort, the `effort` of its --agents entry (delegated configs only)",
    )
    p.add_argument("--jobs", type=int, default=1, help="calls at a time (1)")
    p.add_argument("--keep-work", action="store_true", help="don't delete each call's temporary folder")
    p.add_argument("--timeout", type=int, default=900, help="seconds per call (900)")
    p.add_argument("--dry-run", action="store_true", help="uses bench/fake_reader.py: no model and no cost")
    p.add_argument("--yes", action="store_true", help="confirms a real run (spends tokens)")
    return p.parse_args(argv)


def split(s):
    return [x.strip() for x in s.split(",") if x.strip()]


def out_dir(a):
    if a.out:
        return Path(a.out).resolve()
    stamp = time.strftime("%Y%m%d-%H%M%S")
    out = RESULTS / f"{stamp}-reader{'-dry' if a.dry_run else ''}"
    base, n = out.name, 1
    while out.exists():
        n += 1
        out = out.with_name(f"{base}-{n}")
    return out


def main(argv=None, log=print):
    a = parse(argv if argv is not None else sys.argv[1:])
    configs, sizes, tasks = split(a.configs), split(a.sizes), split(a.tasks)
    for what, got, known in (("configs", configs, rr.CONFIGS), ("sizes", sizes, pmfixture.SIZES), ("tasks", tasks, rr.TASKS)):
        bad = [x for x in got if x not in known]
        if bad or not got:
            log(f"reader.py: unknown or no {what}: {', '.join(bad) or '(none)'} (there are {', '.join(known)})")
            return 2
    if a.seeds < 1 or a.jobs < 1 or a.timeout < 1:
        log("reader.py: --seeds, --jobs and --timeout have to be 1 or more")
        return 2
    calls = len(configs) * len(sizes) * a.seeds * len(tasks)
    if not a.dry_run and not a.yes:
        log(
            f"reader.py: a real run makes {calls} calls to claude -p ({len(configs)} configs × {len(sizes)} sizes "
            f"× {a.seeds} seeds × {len(tasks)} tasks) and spends tokens. The pilot (1 seed, 3 configs × 3 sizes "
            "× 2 tasks = 18 calls) is estimated at 3–10 USD, to be confirmed. Run it under the clean environment "
            "of bench/README.md. Use --dry-run to test the harness, or --yes to confirm it."
        )
        return 2
    out = out_dir(a)
    earlier = rr.load_runs(out)
    if earlier and any(r.get("dry_run") != a.dry_run for r in earlier):
        log(f"reader.py: {out} holds {'real' if a.dry_run else 'dry'} runs; use another --out")
        return 2
    spec_sha = pmfixture.load_spec()["_sha256"]
    if earlier and any((r.get("spec") or {}).get("sha256") != spec_sha for r in earlier):
        log(f"reader.py: {out} holds runs of another version of bench/reader/spec.toml; use another --out")
        return 2
    if a.sub_effort and not any(rr.is_delegated(c) for c in configs):
        log("reader.py: --sub-effort only applies to delegated configurations; none was asked for")
    if os.environ.get(rr.EFFORT_ENV):
        log(f"reader.py: {rr.EFFORT_ENV} is set here; the calls run without it (it would override the efforts)")
    # A dry run never calls a model, whatever --reader-cmd says.
    reader_cmd = rr.fake_reader_cmd() if a.dry_run else (a.reader_cmd or rr.DEFAULT_READER_CMD)
    out.mkdir(parents=True, exist_ok=True)
    opts = rr.Options(
        out,
        reader_cmd=reader_cmd,
        dry_run=a.dry_run,
        effort=a.effort,
        sub_effort=a.sub_effort,
        prompt=a.prompt,
        timeout=a.timeout,
        keep_work=a.keep_work,
        jobs=a.jobs,
        log=log,
    )
    seeds = list(range(a.first_seed, a.first_seed + a.seeds))
    log(f"reader.py: {calls} calls into {out}")
    rr.run_all(opts, configs, sizes, seeds, tasks)
    records = rr.load_runs(out)
    _, _, md = reader_report.write(out, records, opts)
    log(md)
    log(f"Results in {out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
