"""Sets `[distill] command` of the project to the default command wrapped in
distill_cmd.py, so each distill's envelope is saved and its cost counted. The
model command itself stays kontext's default (`claude -p --model haiku
--output-format json`), or the made-up model in dry runs."""

import json
import os
import re
import sys
from pathlib import Path

toml = Path.home() / "kontext-home" / "bench" / "project.toml"
text = toml.read_text()
default = re.search(r'(?m)^command = "(.*)"', text)
model = default.group(1) if default else "claude -p --model haiku --output-format json"
if os.environ.get("BENCH_DRY") == "1":
    model = "python3 /opt/bench/fake_distill.py"
out = os.path.join(os.environ["BENCH_OUT"], "distill")
cmd = f"python3 /opt/bench/distill_cmd.py --out {out} -- {model}"
text, n = re.subn(r"(?m)^command = .*$", lambda _: "command = " + json.dumps(cmd), text, count=1)
if not n:
    sys.exit("project.toml has no [distill] command line")
toml.write_text(text)
print(f"BENCH_STEP harness distill command wrapped: {cmd}")
