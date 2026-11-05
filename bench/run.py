#!/usr/bin/env python3
"""Harness of the kontext benchmark (DESIGN §15). See bench/README.md.

    python3 bench/run.py --dry-run                  # dry run, no model
    python3 bench/run.py --yes --seeds 3 --model …  # for real: spends tokens
"""

import argparse
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from benchlib import report, runner, scenario  # noqa: E402


def parse(argv):
    p = argparse.ArgumentParser(
        prog="bench/run.py",
        description="Runs a kontext benchmark scenario with each variant and seed.",
    )
    p.add_argument("--scenario", default="billing", help="name in bench/scenarios/ or a path (billing; also long-memory)")
    p.add_argument(
        "--variants",
        default=",".join(runner.VARIANTS),
        help="comma-separated: " + ", ".join(runner.VARIANTS),
    )
    p.add_argument("--seeds", type=int, default=3, help="runs per variant (3)")
    p.add_argument("--first-seed", type=int, default=1, help="first seed, to repeat the missing ones (1)")
    p.add_argument("--dry-run", action="store_true", help="uses bench/fake_agent.py: no model and no cost")
    p.add_argument("--agent-cmd", default=None, help=f"the agent command (default: {runner.DEFAULT_AGENT_CMD})")
    p.add_argument("--model", default=None, help="added to the agent command as --model <x>")
    p.add_argument("--judge-cmd", default=None, help="the memory judge's command (prompt on stdin); without it, keywords")
    p.add_argument(
        "--kontext-bin",
        dest="kontext_bin",
        default=None,
        help="the kontext binary (default $KONTEXT_BIN, target/debug/kontext, or the one in PATH)",
    )
    p.add_argument("--out", default=str(Path(__file__).resolve().parent / "results"), help="where to leave the results")
    p.add_argument("--turn-timeout", type=int, default=900, help="seconds per turn (900)")
    p.add_argument("--keep-work", action="store_true", help="don't delete each run's temporary repos")
    p.add_argument(
        "--fill-scale",
        type=float,
        default=1.0,
        help="multiplies the size of the filler turns (1.0; 0.1 for cheap pilots)",
    )
    p.add_argument(
        "--distill-cmd",
        default=None,
        help=f"the distill model in the kontext variants, if the scenario turns it on (default: {runner.DEFAULT_DISTILL_CMD})",
    )
    p.add_argument("--distill-wait", type=int, default=900, help="maximum seconds to wait for the distill (900)")
    p.add_argument("--yes", action="store_true", help="confirms a real run (spends tokens)")
    return p.parse_args(argv)


def main(argv=None, log=print):
    a = parse(argv if argv is not None else sys.argv[1:])
    sc = scenario.load(a.scenario)
    variants = [v.strip() for v in a.variants.split(",") if v.strip()]
    bad = [v for v in variants if v not in runner.VARIANTS]
    if bad:
        log(f"run.py: unknown variants: {', '.join(bad)}")
        return 2
    if a.seeds < 1:
        log("run.py: --seeds has to be 1 or more")
        return 2
    if a.fill_scale <= 0:
        log("run.py: --fill-scale has to be greater than 0")
        return 2
    if not a.dry_run and not a.yes:
        calls = a.seeds * len(variants) * (sum(len(d) for d in sc.days) + len(sc.probes))
        fill_tokens = sum(t.tokens for d in sc.days for t in d if t.kind == "fill") * a.fill_scale
        extra = (
            f" Each run adds about {fill_tokens:,.0f} filler tokens (--fill-scale {a.fill_scale:g})."
            if fill_tokens
            else ""
        )
        log(
            "run.py: a real run calls the agent about "
            f"{calls} times (plus corrections) and spends tokens.{extra} "
            "Use --dry-run to test the harness, or --yes to confirm it."
        )
        return 2
    agent_cmd = a.agent_cmd
    if a.dry_run:
        agent_cmd = f"{runner.python()} {runner.FAKE_AGENT} -p --output-format json"
    elif not agent_cmd:
        agent_cmd = runner.DEFAULT_AGENT_CMD
    kontext_bin = runner.find_kontext(a.kontext_bin)
    if any(v.startswith("kontext-") for v in variants) and not kontext_bin:
        log("run.py: the kontext-* variants need kontext: run `cargo build` or use --kontext-bin")
        return 2
    cfg = runner.Config(
        agent_cmd=agent_cmd,
        model=a.model,
        judge_cmd=a.judge_cmd,
        dry_run=a.dry_run,
        kontext_bin=kontext_bin,
        keep_work=a.keep_work,
        turn_timeout=a.turn_timeout,
        fill_scale=a.fill_scale,
        distill_cmd=a.distill_cmd,
        distill_wait=a.distill_wait,
        log=log,
    )
    out, results = runner.run_all(sc, variants, a.seeds, a.out, cfg, first_seed=a.first_seed)
    _, md = report.write(out, sc, results, variants, cfg)
    log(md)
    log(f"Results in {out}")

    return 0


if __name__ == "__main__":
    sys.exit(main())
