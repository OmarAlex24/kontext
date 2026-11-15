"""The mem0 plugin's local evidence (evidence.sqlite3 in its data folder):
settle (wait for no flush in flight), export (counts, and the run's memories
from the API), forget (delete them with the plugin's own command).

Uses only the plugin's files and its CLI; the key comes from MEM0_API_KEY in
the environment and is never printed or written.
"""

import argparse
import glob
import json
import os
import sqlite3
import subprocess
import sys
import time
from pathlib import Path

HOME = Path.home()
FINAL = ("semantic-succeeded", "explicitly-stored", "gave-up")


def data_dirs():
    cands = glob.glob(str(HOME / ".claude/plugins/data/*mem0*")) + glob.glob(str(HOME / ".mem0/*"))
    return [Path(c) for c in cands if (Path(c) / "evidence.sqlite3").exists()]


def plugin_root():
    roots = sorted(glob.glob(str(HOME / ".claude/plugins/cache/mem0-plugins/mem0/*")))
    return Path(roots[-1]) if roots else None


def counts():
    out = {"data_dirs": [], "flushes": {}, "operations": {}, "retrievals": 0, "events": 0}
    for d in data_dirs():
        out["data_dirs"].append(str(d))
        c = sqlite3.connect(d / "evidence.sqlite3")
        for status, n in c.execute("SELECT status, COUNT(*) FROM flushes GROUP BY status"):
            out["flushes"][status] = out["flushes"].get(status, 0) + n
        for op, ok, n, items in c.execute(
            "SELECT operation, success, COUNT(*), SUM(item_count) FROM operations GROUP BY operation, success"
        ):
            key = f"{op}:{'ok' if ok else 'failed'}"
            prev = out["operations"].get(key, {"calls": 0, "items": 0})
            out["operations"][key] = {"calls": prev["calls"] + n, "items": prev["items"] + (items or 0)}
        out["retrievals"] += c.execute("SELECT COUNT(*) FROM retrievals").fetchone()[0]
        out["events"] += c.execute("SELECT COUNT(*) FROM events").fetchone()[0]
        c.close()
    out["pending"] = sum(n for s, n in out["flushes"].items() if s not in FINAL)
    return out


def settle(timeout):
    t0 = time.monotonic()
    seen = counts()
    while seen["pending"] and time.monotonic() - t0 < timeout:
        time.sleep(5)
        seen = counts()
    seen["waited_s"] = round(time.monotonic() - t0, 1)
    seen["timed_out"] = bool(seen["pending"])
    print(json.dumps(seen))
    return 0


def cli(*args):
    root = plugin_root()
    if not root:
        return None
    data = data_dirs()
    extra = ["--plugin-data-dir", str(data[0])] if data else []
    return subprocess.run(
        [sys.executable, str(root / "core/memory_cli.py"), *extra, *args],
        capture_output=True,
        text=True,
        timeout=300,
    )


def export(out):
    out = Path(out)
    out.mkdir(parents=True, exist_ok=True)
    rec = {"counts": counts()}
    if os.environ.get("BENCH_DRY") != "1":
        r = cli("status", "--json")
        if r is not None:
            try:
                rec["status"] = json.loads(r.stdout)
            except json.JSONDecodeError:
                rec["status_error"] = r.stderr[-500:]
        rec["memories"] = list_memories()
    (out / "evidence.json").write_text(json.dumps(rec, indent=2, ensure_ascii=False))
    print(json.dumps(rec["counts"]))
    return 0


def list_memories():
    """This run's memories (its user and the repo's shared project memory),
    with the plugin's own list helpers."""
    root = plugin_root()
    if not root:
        return {"error": "no plugin"}
    sys.path.insert(0, str(root / "core"))
    try:
        import memory_core as mc  # noqa: E402

        repo = mc.resolve_repo(os.getcwd())
        key = mc.api_key()
        url = os.environ.get("MEM0_API_URL", mc.DEFAULT_API_URL).rstrip("/")
        items = []
        filters = [{"user_id": mc.user_id()}] + [{"agent_id": p} for p in mc._shared_project_ids(repo)]
        for f in filters:
            parsed, _, _ = mc._request_json(f"{url}/v2/memories/?page=1&page_size=100", key, {"filters": f}, 30)
            got = parsed.get("results") if isinstance(parsed, dict) else parsed
            items += [{"filter": f, **m} for m in (got or []) if isinstance(m, dict)]
        return items
    except Exception as e:  # the export must never stop the teardown
        return {"error": type(e).__name__ + ": " + str(e)[:300]}


def forget():
    r = cli("forget", "--remote", "--yes", "--include-project-memory")
    print(json.dumps({"forget_exit": None if r is None else r.returncode, "stdout": (r.stdout[-500:] if r else "")}))
    return 0


def main(argv):
    p = argparse.ArgumentParser()
    sub = p.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("settle")
    s.add_argument("--timeout", type=float, default=600)
    e = sub.add_parser("export")
    e.add_argument("--out", required=True)
    sub.add_parser("forget")
    sub.add_parser("counts")
    a = p.parse_args(argv)
    if a.cmd == "settle":
        return settle(a.timeout)
    if a.cmd == "export":
        return export(a.out)
    if a.cmd == "forget":
        return forget()
    print(json.dumps(counts()))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
