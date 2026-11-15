#!/usr/bin/env python3
"""The friction axis (design §3), re-executed: for each setup, the terminal
steps from a clean machine A to "machine B sees what A saved", in two fresh
containers of the base image (Claude Code and git, nothing else). Steps that
need an account, a host or a browser are counted, not executed. No model is
called and no key is used.

    python3 bench/compare/friction/run.py                  # every setup
    python3 bench/compare/friction/run.py --setups kontext,beads
    python3 bench/compare/friction/run.py --table-only <friction.json>

Writes friction.json and friction.md (the regenerated §3 table, each step
marked executed or doc-only, and where it differs from the design) into
--out (default bench/compare/friction/results/<stamp>/).
"""

import argparse
import json
import re
import secrets as _secrets
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parent.parent))

from benchlib import container  # noqa: E402
from setups import SETUPS  # noqa: E402

CLEAN_PATH = "/usr/local/bin:/usr/bin:/bin"


def run_setup(docker, setup, log):
    run_id = f"friction-{setup.name}-{_secrets.token_hex(3)}"
    prefix = f"bench-{run_id}"
    net, vol = f"{prefix}-net", f"{prefix}-remotes"
    results = []
    held = {}  # secrets handed from one step to another, in memory only
    try:
        if any(s.run for s in setup.steps):
            docker.run(container.argv_network_create(net, run_id))
            docker.run(container.argv_volume_create(vol, run_id))
        env = {"BENCH_REMOTE": f"file:///remotes/{run_id}.git"}
        sidecar = None
        if setup.sidecar:
            sidecar = container.Sidecar(docker, f"{prefix}-sidecar", "bench-sidecar:latest", net, run_id)
            sidecar.start()
            env["KONTEXT_SERVER_URL"] = sidecar.url
            held["KONTEXT_ADMIN_SECRET"] = sidecar.admin_secret
        machines, paths = {}, {}

        def machine(letter):
            if letter not in machines:
                m = container.Machine(docker, f"{prefix}-{letter.lower()}", "bench-base:latest", net, vol, run_id, f"machine-{letter.lower()}", env=env)
                m.start()
                machines[letter] = m
                paths[letter] = CLEAN_PATH
            return machines[letter]

        for i, s in enumerate(setup.steps, 1):
            rec = {
                "n": i, "where": s.where, "kind": s.kind, "text": s.text, "conditional": s.conditional,
                "account": s.account, "secret": s.secret,
            }  # fmt: skip
            if not s.run:
                rec.update(status="doc-only", why=s.doc)
                results.append(rec)
                continue
            letter = s.where if s.where in ("A", "B") else "A"
            m = machine(letter)
            secret_env = {k: held[k] for k in s.secret_in if k in held}
            t0 = time.monotonic()
            r = m.exec(
                ["bash", "-o", "pipefail", "-c", s.run],
                env={"PATH": paths[letter]} | ({"HOME": "/root"} if s.root else {}),
                secret_env=secret_env, workdir=container.HOME, timeout=s.timeout, user=0 if s.root else None,
            )  # fmt: skip
            secs = round(time.monotonic() - t0, 1)
            out = r.stdout or ""
            if s.secret_out:
                for line in out.splitlines():
                    if line.startswith(s.secret_out + "="):
                        held[s.secret_out] = line.split("=", 1)[1].strip()
                out = "\n".join(x for x in out.splitlines() if not x.startswith(s.secret_out + "=")) + "\n(the secret line is kept in memory only)"
            for v in held.values():
                out = out.replace(v, "<redacted>")
            err = r.stderr or ""
            for v in held.values():
                err = err.replace(v, "<redacted>")
            if s.probe:
                # A conditional manual step: exit 0 means a new terminal already
                # finds the tool, so the step was not needed on this machine.
                rec.update(status="executed", needed=r.returncode != 0, seconds=secs)
                # After it, the user's shell has the folder (that is the step).
                paths[letter] = f"{container.HOME}/.local/bin:" + paths[letter]
            else:
                rec.update(status="executed" if r.returncode == 0 else "failed", exit_code=r.returncode, seconds=secs)
            rec["output_tail"] = (out + err)[-600:]
            log(f"  {setup.name} {i:2d} {s.where:3s} {rec['status']:8s} {s.text[:70]}")
            results.append(rec)
    except container.DockerError as e:
        results.append({"n": len(results) + 1, "kind": "harness", "text": "harness error", "status": "failed", "output_tail": str(e)[-600:]})
    finally:
        container.cleanup_run(docker, run_id)
    return results


def counts(setup_name, steps):
    def n(pred):
        return sum(1 for s in steps if pred(s))

    def by(where):
        return n(lambda s: s["kind"] == "command" and not s["conditional"] and where in s["where"].split("+"))

    cmd_a, cmd_b = by("A"), by("B")
    cmd_cond = n(lambda s: s["kind"] == "command" and s["conditional"])
    manual = n(lambda s: s["kind"] == "manual" and not s["conditional"])
    manual_cond = n(lambda s: s["kind"] == "manual" and s["conditional"])
    needed = n(lambda s: s["kind"] == "manual" and s["conditional"] and s.get("needed"))
    host = n(lambda s: s["kind"] == "command" and s["where"] == "host")
    return {
        "commands": f"{cmd_a} + {cmd_b}" + (f" (+{host} on a host)" if host else "") + (f" (+{cmd_cond} conditional)" if cmd_cond else ""),
        "manual": f"{manual}–{manual + manual_cond}" if manual_cond else str(manual),
        "manual_needed_here": manual + needed,
        "accounts": n(lambda s: s.get("account")),
        "secrets": n(lambda s: s.get("secret")),
        "executed": n(lambda s: s["status"] == "executed"),
        "failed": n(lambda s: s["status"] == "failed"),
        "doc_only": n(lambda s: s["status"] == "doc-only"),
        "b_sees_a": next((s["status"] for s in steps if s["kind"] == "check"), "no check"),
    }


def table(report):
    md = ["# Install-and-sync friction, re-executed", ""]
    md.append(
        "From a clean machine A with Claude Code and git to \"machine B sees what A saved\". Every terminal step "
        "below was re-executed in two fresh containers of the base image unless marked doc-only; steps that need "
        "an account, a host or a browser are counted, not executed. Harness steps (the code clone, the stand-in "
        "for the agent saving something) are not counted. Generated by bench/compare/friction/run.py on "
        f"{report['date']}."
    )
    md.append("")
    md.append("| Setup | Commands A + B | Manual steps | New accounts | Secrets made or shown once | After setup | B sees what A saved | Steps executed / doc-only / failed | Design §3 |")
    md.append("|---|---|---|---|---|---|---|---|---|")
    for s in report["setups"]:
        c = s["counts"]
        d = s["design"]
        md.append(
            f"| **{s['title']}** | {c['commands']} | {c['manual']} (here: {c['manual_needed_here']}) | {c['accounts']} | {c['secrets']} | {s['after']} | "
            f"{c['b_sees_a']} | {c['executed']} / {c['doc_only']} / {c['failed']} | {d['commands']}; {d['manual']}; {d['accounts']}; {d['secrets']} |"
        )
    md.append("")
    md.append("## Where it differs from the design's §3")
    md.append("")
    for s in report["setups"]:
        diffs = s.get("differences") or []
        md.append(f"- **{s['title']}**: " + ("; ".join(diffs) if diffs else "same counts as the design."))
    md.append("")
    for s in report["setups"]:
        if not s["steps"]:
            continue
        md.append(f"## {s['title']}")
        md.append("")
        md.append("| # | Where | Step | Kind | Status |")
        md.append("|---|---|---|---|---|")
        for st in s["steps"]:
            kind = st["kind"] + (", conditional" if st.get("conditional") else "") + (", account" if st.get("account") else "") + (", secret" if st.get("secret") else "")
            status = st["status"]
            if st.get("why"):
                status += f" ({st['why']})"
            if "needed" in st:
                status += ": needed here" if st["needed"] else ": not needed here (a new login shell already finds it)"
            if st.get("exit_code"):
                status += f", exit {st['exit_code']}"
            md.append(f"| {st['n']} | {st['where']} | {st['text'].replace('|', '\\|')} | {kind} | {status} |")
        md.append("")
    return "\n".join(md)


_RANGE = re.compile(r"^(\d+)(?:–(\d+))?(?: \+ (\d+)(?:–(\d+))?)?(?: \(\+(\d+)(?: conditional)?\))?$")


def command_range(text):
    """(fewest, most) commands of a count like "5 + 3 (+1 conditional)",
    "8 (+1)" or "2 + 0–1"; the text itself when it is not a plain count."""
    m = _RANGE.match(text.strip())
    if not m:
        return text
    a_lo, a_hi, b_lo, b_hi, cond = m.groups()
    lo = int(a_lo) + int(b_lo or 0)
    hi = int(a_hi or a_lo) + int(b_hi or b_lo or 0) + int(cond or 0)
    return (lo, hi)


def differences(setup, c):
    d = setup.design
    out = []

    def norm(x):
        return str(x).replace(" ", "")

    # "count": the design's commands as a plain count, where its text is not one.
    ours, theirs = command_range(c["commands"]), command_range(d.get("count", d["commands"]))
    if setup.name not in ("gentle-ai-cloud", "claude-mem") and ours != theirs:
        out.append(f"commands {c['commands']} against the design's {d['commands']}")
    if norm(c["manual"]) != norm(d["manual"]) and d["manual"][0].isdigit():
        out.append(f"manual steps {c['manual']} against {d['manual']}")
    if d["accounts"][0].isdigit() and str(c["accounts"]) != d["accounts"].split()[0]:
        out.append(f"accounts {c['accounts']} against {d['accounts']}")
    if d["secrets"][0].isdigit() and str(c["secrets"]) != d["secrets"].rstrip("+"):
        out.append(f"secrets {c['secrets']} against {d['secrets']}")
    return out


def summarize(setup, steps):
    c = counts(setup.name, steps)
    diffs = differences(setup, c)
    for st in steps:
        if st["status"] == "failed":
            diffs.append(f"step {st['n']} ({st['text']}) failed when executed")
        if st.get("needed") is False:
            diffs.append(f"step {st['n']} ({st['text']}) was not needed on Debian (its ~/.profile puts ~/.local/bin on PATH in a new login shell)")
    return {"name": setup.name, "title": setup.title, "after": setup.after, "design": setup.design, "counts": c, "differences": diffs, "steps": steps}


def main(argv=None, log=print):
    p = argparse.ArgumentParser(prog="bench/compare/friction/run.py")
    p.add_argument("--setups", default=",".join(s.name for s in SETUPS))
    p.add_argument("--out", default=None)
    p.add_argument("--table-only", default=None, help="regenerate friction.md from a friction.json")
    a = p.parse_args(argv)
    if a.table_only:
        rep = json.loads(Path(a.table_only).read_text())
        by_name = {s.name: s for s in SETUPS}
        rep["setups"] = [summarize(by_name[s["name"]], s["steps"]) if s["name"] in by_name else s for s in rep["setups"]]
        Path(a.table_only).write_text(json.dumps(rep, indent=2, ensure_ascii=False))
        Path(a.table_only).with_name("friction.md").write_text(table(rep))
        return 0
    want = [x.strip() for x in a.setups.split(",") if x.strip()]
    docker = container.Docker(log=log)
    out = Path(a.out) if a.out else HERE / "results" / time.strftime("%Y%m%d-%H%M%S")
    out.mkdir(parents=True, exist_ok=True)
    report = {"date": time.strftime("%Y-%m-%d"), "base_image": docker.image_id("bench-base:latest"), "setups": []}
    for setup in [s for s in SETUPS if s.name in want]:
        log(f"friction: {setup.name}")
        steps = run_setup(docker, setup, log)
        report["setups"].append(summarize(setup, steps))
    (out / "friction.json").write_text(json.dumps(report, indent=2, ensure_ascii=False))
    (out / "friction.md").write_text(table(report))
    log(f"friction: results in {out}")
    return 0 if all(s["counts"]["failed"] == 0 for s in report["setups"]) else 1


if __name__ == "__main__":
    sys.exit(main())
