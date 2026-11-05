#!/usr/bin/env python3
"""The distill command of the runs: it wraps the real one so its cost counts.

    distill_cmd.py --out <folder> -- claude -p --model … --output-format json

`kontext distill` runs it with `sh -c`, the prompt on stdin, in a temporary
folder and with the agent's environment (inherited from the hook). It runs the
command as is, with the same stdin and environment, returns its output and exit
code, and saves the JSON envelope (with `usage` and `total_cost_usd`) in
`<folder>/<time>-<pid>.json`, so the harness adds it to the run's cost.
"""

import json
import os
import subprocess
import sys
import time
from pathlib import Path


def main(argv):
    if len(argv) < 4 or argv[0] != "--out" or argv[2] != "--":
        print("usage: distill_cmd.py --out <folder> -- <command…>", file=sys.stderr)

        return 2
    out_dir = Path(argv[1])
    cmd = argv[3:]
    prompt = sys.stdin.read()
    t0 = time.monotonic()
    r = subprocess.run(cmd, input=prompt, capture_output=True, text=True)
    envelope = {}
    try:
        v = json.loads(r.stdout.strip() or "{}")
        envelope = v if isinstance(v, dict) else {}
    except json.JSONDecodeError:
        pass
    out_dir.mkdir(parents=True, exist_ok=True)
    rec = {
        "argv": cmd,
        "exit_code": r.returncode,
        "seconds": round(time.monotonic() - t0, 2),
        "prompt_chars": len(prompt),
        "envelope": envelope,
        "stdout": "" if envelope else r.stdout[-2000:],
        "stderr": r.stderr[-2000:],
    }
    name = f"{time.time_ns()}-{os.getpid()}.json"
    (out_dir / name).write_text(json.dumps(rec, ensure_ascii=False))
    sys.stdout.write(r.stdout)
    sys.stderr.write(r.stderr)
    return r.returncode


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
