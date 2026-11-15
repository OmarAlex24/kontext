"""The freeze (design §2 and §6.2 point 1): fixtures, scripted turns, hidden
checks, the fixed-reply rule and the analysis rules go in one commit before any
model call. FREEZE.json records that commit and a sha256 over the frozen
files' bytes; run.py refuses a paid cell if the files differ from it.

The digest is what is checked (it survives a squash merge, which would change
the commit); the commit is recorded so a reader can find it.
"""

import hashlib
import json
import subprocess
from pathlib import Path

COMPARE_DIR = Path(__file__).resolve().parent.parent
BENCH_DIR = COMPARE_DIR.parent
FREEZE_FILE = COMPARE_DIR / "FREEZE.json"
# Relative to bench/. Everything that decides what the agent is told, what is
# checked, and how a difference is judged.
FROZEN = [
    "compare/scenarios",
    "compare/ANALYSIS.md",
    "compare/complib/interventions.py",
    "compare/complib/analysis.py",
    "benchlib/fill.py",
]
SKIP = ("__pycache__", ".pyc", ".DS_Store")


def files(root=BENCH_DIR):
    root = Path(root)
    out = []
    for rel in FROZEN:
        p = root / rel
        if p.is_file():
            out.append(p)
        elif p.is_dir():
            out += [f for f in sorted(p.rglob("*")) if f.is_file() and not any(s in str(f.relative_to(root)) for s in SKIP)]
    return sorted(out)


def digest(root=BENCH_DIR):
    """sha256 over the frozen files under `root` (a bench/ folder): their
    paths and bytes, so an added, removed or changed file changes it."""
    root = Path(root)
    h = hashlib.sha256()
    for f in files(root):
        h.update(str(f.relative_to(root)).encode() + b"\0")
        h.update(f.read_bytes())
        h.update(b"\0")
    return h.hexdigest()


def recorded():
    if not FREEZE_FILE.exists():
        return None
    return json.loads(FREEZE_FILE.read_text())


def status():
    """{"frozen": bool, "digest", "recorded_digest", "commit", "reason"}."""
    rec = recorded()
    now = digest()
    out = {"digest": now, "recorded_digest": rec and rec.get("digest"), "commit": rec and rec.get("commit")}
    if not rec:
        out.update(frozen=False, reason="no FREEZE.json")
    elif rec.get("digest") != now:
        out.update(frozen=False, reason="the frozen files differ from FREEZE.json")
    else:
        out.update(frozen=True, reason=None)
    return out


def write(commit, note=""):
    FREEZE_FILE.write_text(
        json.dumps(
            {"commit": commit, "digest": digest(), "paths": FROZEN, "files": len(files()), "note": note}, indent=2
        )
        + "\n"
    )


def git_head():
    r = subprocess.run(["git", "-C", str(BENCH_DIR), "rev-parse", "HEAD"], capture_output=True, text=True)
    return r.stdout.strip() if r.returncode == 0 else None
