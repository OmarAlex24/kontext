#!/usr/bin/env python3
"""The comparison benchmark: kontext against five other setups, one container
per machine (bench/compare/DESIGN.md, README.md).

    python3 bench/compare/run.py --dry-run                       # no model, no key, no cost
    python3 bench/compare/run.py --tasks S1 --arms plain,kontext \\
        --models claude-sonnet-5-5 --seeds 1 --yes               # spends tokens

A real run reads ANTHROPIC_API_KEY (every arm) and MEM0_API_KEY (the mem0 arm)
from this process's environment, passes each only to the containers that need
it, by name, and never writes either anywhere. It refuses to spend without
--yes, and refuses a paid cell if the frozen files differ from FREEZE.json.
"""

import argparse
import concurrent.futures
import json
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parent))

from benchlib import container  # noqa: E402
from complib import cell, freeze, keys, script, tables  # noqa: E402
from complib.arms import ARM_NAMES, ARMS, KEY_NAMES  # noqa: E402

IMAGES = ["bench-base:latest", "bench-verify:latest", "bench-sidecar:latest"] + [f"bench-{a}:latest" for a in ARM_NAMES]


def parse(argv):
    p = argparse.ArgumentParser(prog="bench/compare/run.py", description=__doc__.split("\n\n")[0])
    p.add_argument("--arms", default=",".join(ARM_NAMES), help="comma-separated: " + ", ".join(ARM_NAMES))
    p.add_argument("--tasks", default="S1,S2,L", help="comma-separated: smoke, S1, S2, L (default S1,S2,L)")
    p.add_argument("--models", default="claude-sonnet-5-5", help="comma-separated model ids (ignored with --dry-run)")
    p.add_argument("--seeds", type=int, default=5, help="runs per cell (5)")
    p.add_argument("--first-seed", type=int, default=1)
    p.add_argument("--jobs", type=int, default=1, help="cells at a time, one container set each (1; 4 fit in 8 GB)")
    p.add_argument("--dry-run", action="store_true", help="bench/fake_agent.py in the containers: no model, no key")
    p.add_argument("--yes", action="store_true", help="confirms a real run (spends tokens)")
    p.add_argument("--out", default=None, help="results folder (default bench/compare/results/<stamp>[-dry])")
    p.add_argument("--fill-scale", type=float, default=1.0, help="multiplies the filler turns of L (1.0)")
    p.add_argument("--turn-timeout", type=int, default=1200, help="seconds per agent call (1200)")
    p.add_argument("--settle-timeout", type=int, default=600, help="most seconds a settle step waits (600)")
    p.add_argument("--keep-containers", action="store_true", help="leave the containers for inspection")
    p.add_argument("--no-measure", action="store_true", help="skip `kontext measure` over the transcripts")
    return p.parse_args(argv)


def plan(a):
    arms = [x.strip() for x in a.arms.split(",") if x.strip()]
    tasks = [x.strip() for x in a.tasks.split(",") if x.strip()]
    models = [None] if a.dry_run else [x.strip() for x in a.models.split(",") if x.strip()]
    bad = [x for x in arms if x not in ARMS] + [x for x in tasks if x not in script.TASKS]
    if bad:
        raise SystemExit(f"run.py: unknown arms or tasks: {', '.join(bad)}")
    cells = [(t, arm, m, s) for t in tasks for m in models for arm in arms for s in range(a.first_seed, a.first_seed + a.seeds)]
    return arms, tasks, models, cells


def main(argv=None, log=print):
    a = parse(argv if argv is not None else sys.argv[1:])
    arms, tasks, models, cells = plan(a)
    scenarios = {t: script.load(t) for t in tasks}
    calls = sum(scenarios[t].calls() for t, *_ in cells)
    fz = freeze.status()
    if not a.dry_run:
        if not a.yes:
            log(
                f"run.py: a real run is {len(cells)} cells and about {calls} agent calls (plus fixed replies, "
                "kontext's distill and mem0's extraction), and spends tokens. Use --dry-run to test the harness, "
                "or --yes to confirm it."
            )
            return 2
        if not fz["frozen"]:
            log(f"run.py: refusing a paid run: {fz['reason']} (frozen digest {fz['recorded_digest']}, now {fz['digest']})")
            return 2
        needed = sorted({k for arm in arms for k in ARMS[arm].secret_names(False)})
        missing = [k for k in needed if not keys.from_env([k])]
        if missing:
            log(f"run.py: set {', '.join(missing)} in the environment (read by name, never written)")
            return 2
    docker = container.Docker(log=log)
    if not docker.available():
        log("run.py: docker is not installed")
        return 2
    images = {i: docker.image_id(i) for i in IMAGES}
    need = {"bench-base:latest", "bench-verify:latest", "bench-kontext:latest"} | {ARMS[x].image for x in arms}
    if "kontext" in arms:
        need.add("bench-sidecar:latest")
    missing = sorted(i for i in need if not images.get(i))
    if missing:
        log(f"run.py: missing images {', '.join(missing)}: run bench/compare/images/build.sh")
        return 2
    stamp = time.strftime("%Y%m%d-%H%M%S")
    out = Path(a.out) if a.out else HERE / "results" / f"{stamp}{'-dry' if a.dry_run else ''}"
    out.mkdir(parents=True, exist_ok=True)
    secret_values = {} if a.dry_run else keys.from_env(list(KEY_NAMES))
    (out / "plan.json").write_text(
        json.dumps(
            {
                "argv": argv if argv is not None else sys.argv[1:],
                "cells": [{"task": t, "arm": arm, "model": m, "seed": s} for t, arm, m, s in cells],
                "images": images,
                "freeze": fz,
                "keys_read": sorted(secret_values),
            },
            indent=2,
        )
    )

    def one(c):
        t, arm, m, s = c
        cfg = cell.CellConfig(
            dry_run=a.dry_run, model=m, keys=secret_values, fill_scale=a.fill_scale, turn_timeout=a.turn_timeout,
            settle_timeout=a.settle_timeout, images=images, freeze_status=fz, keep_containers=a.keep_containers,
            measure=not a.no_measure, log=log, docker=docker,
        )  # fmt: skip
        run_dir = out / "runs" / f"{t}-{arm}-{cell.model_tag(m, a.dry_run)}-s{s}"
        log(f"cell {t} · {arm} · {cell.model_tag(m, a.dry_run)} · seed {s}: start")
        r = cell.Cell(scenarios[t], arm, s, run_dir, cfg).run()
        log(f"cell {t} · {arm} · seed {s}: done in {r['wall_seconds']} s, {len(r['errors'])} errors")
        return r

    results = []
    with concurrent.futures.ThreadPoolExecutor(max_workers=max(1, a.jobs)) as ex:
        for r in ex.map(one, cells):
            results.append(r)
    md = tables.write(out, results)
    log(md)
    hits = keys.scan(out, secret_values)
    if hits:
        log(f"run.py: a key value is in {len(hits)} file(s) of {out}: " + ", ".join(f"{f} ({n})" for f, n in hits))
        return 3
    log(f"Results in {out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
