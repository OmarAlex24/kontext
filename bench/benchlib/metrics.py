"""Deterministic measures of a run (DESIGN §15).

The drift formula is fixed here and in bench/README.md before the first real
run. It is not changed after seeing results.
"""

import os
import re
import subprocess
import tempfile
import unicodedata

from .scenario import matches

# How many changed lines a failing guard weighs. A violated decision is a big
# drift even if it touches few lines: 50 lines is a median function. Fixed
# before running the benchmark (bench/README.md).
GUARD_WEIGHT = 50


def desvio_pct(lines_out, lines_total, guards_failed, guards_total, weight=GUARD_WEIGHT):
    """Drift % = 100 × (L_out + W × G_failing) / (L_total + W × G_total).

    - L_out: changed lines (added + removed) in files that are not in the
      `scope` of any goal;
    - L_total: all the changed lines (without the `scope.ignore` paths);
    - G_failing / G_total: guards of the decisions that fail at the end / all;
    - W = GUARD_WEIGHT.

    If nothing changed and there are no guards, it is 0.
    """
    den = lines_total + weight * guards_total
    if den == 0:
        return 0.0
    return 100.0 * (lines_out + weight * guards_failed) / den


def git(repo, *args, env=None, check=True):
    r = subprocess.run(
        ["git", "-c", "core.quotepath=off", *args],
        cwd=repo,
        capture_output=True,
        text=True,
        env=env,
    )
    if check and r.returncode != 0:
        raise RuntimeError(f"git {' '.join(args)} failed: {r.stderr.strip()}")
    return r.stdout


def changed_lines(repo, base):
    """Changed lines per file against `base`, including the untracked files,
    without touching the repo's index (it uses a temporary index)."""
    with tempfile.TemporaryDirectory() as tmp:
        env = dict(os.environ, GIT_INDEX_FILE=os.path.join(tmp, "index"))
        git(repo, "read-tree", base, env=env)
        git(repo, "add", "-A", env=env)
        out = git(repo, "diff", "--cached", "--numstat", "--no-renames", base, env=env)
    files = {}
    for line in out.splitlines():
        parts = line.split("\t", 2)
        if len(parts) != 3:
            continue
        a, d, path = parts
        n = (int(a) if a.isdigit() else 0) + (int(d) if d.isdigit() else 0)
        files[path] = files.get(path, 0) + n
    return files


def scope_lines(sc, files):
    """(outside the charter, total, per goal, files outside)."""
    out = 0
    total = 0
    per_goal = {g["id"]: 0 for g in sc.goals}
    outside = {}
    for path, n in sorted(files.items()):
        if matches(path, sc.ignore):
            continue
        total += n
        hit = [g["id"] for g in sc.goals if matches(path, g["scope"])]
        if hit:
            for g in hit:
                per_goal[g] += n
        else:
            out += n
            outside[path] = n
    return out, total, per_goal, outside


def run_shell(cmd, cwd, timeout=300):
    try:
        r = subprocess.run(cmd, shell=True, cwd=cwd, capture_output=True, text=True, timeout=timeout)
        return r.returncode, r.stdout + r.stderr
    except subprocess.TimeoutExpired:
        return 124, f"timed out after {timeout} s"


def guard_passes(decision, repo):
    code, _ = run_shell(decision["guard"], repo, timeout=120)
    expect = decision.get("expect", "exit0")
    return code == 0 if expect == "exit0" else code != 0


def guards(sc, repo):
    return {d["id"]: guard_passes(d, repo) for d in sc.decisions}


def acceptance(sc, repo):
    """Copies the acceptance tests to tests/ and runs the command. Returns
    passed, the % of tests that pass (if the output is from unittest) and the output."""
    acc = sc.acceptance
    if not acc.get("command"):
        return {"ran": False, "passed": None, "pct": None, "output": ""}
    src = sc.path / acc.get("files", "acceptance")
    if src.is_dir():
        dest = repo / "tests"
        dest.mkdir(exist_ok=True)
        for f in src.iterdir():
            if f.is_file():
                (dest / f.name).write_text(f.read_text())
    code, output = run_shell(acc["command"], repo, timeout=int(acc.get("timeout", 600)))
    pct = unittest_pct(output)
    if pct is None:
        pct = 100.0 if code == 0 else 0.0
    return {"ran": True, "passed": code == 0, "pct": pct, "output": output[-4000:]}


def unittest_pct(output):
    m = re.search(r"^Ran (\d+) tests?", output, re.M)
    if not m:
        return None
    ran = int(m.group(1))
    if ran == 0:
        return 0.0
    bad = 0
    m2 = re.search(r"^FAILED \(([^)]*)\)", output, re.M)
    if m2:
        for k, v in re.findall(r"(failures|errors)=(\d+)", m2.group(1)):
            bad += int(v)
    return 100.0 * (ran - bad) / ran


def normalize(s):
    """Lowercase and without accents; numbers in the same form: "1,5 %" and "1.5%"
    become "1.5%", "3,250" and "3 250" become "3250"."""
    s = unicodedata.normalize("NFKD", s.lower())
    s = "".join(c for c in s if not unicodedata.combining(c))
    s = re.sub(r"(?<=\d)[, ](?=\d{3}(?!\d))", "", s)  # miles
    s = re.sub(r"(?<=\d),(?=\d)", ".", s)  # decimals with a comma
    s = re.sub(r"\s+%", "%", s)
    return s


def keyword_hit(answer_n, keyword):
    """Is `keyword` (or one of its forms, separated by `|`) in the already
    normalized answer as a whole word?"""
    for alt in keyword.split("|"):
        k = normalize(alt.strip())
        if k and re.search(r"(?<![0-9a-z])" + re.escape(k) + r"(?![0-9a-z])", answer_n):
            return True
    return False


def keyword_verdict(answer, expected, forbidden=()):
    """Dry-run judge: (score, forbidden words found). 1 if the answer has all the
    expected words and none of the forbidden ones (case and accents ignored,
    as whole words; `a|b` accepts either)."""
    a = normalize(answer)
    hits = [k for k in forbidden if keyword_hit(a, k)]
    ok = all(keyword_hit(a, k) for k in expected) and not hits
    return (1.0 if ok else 0.0), hits


def keyword_judge(answer, expected, forbidden=()):
    return keyword_verdict(answer, expected, forbidden)[0]


def recall_by(items, key):
    """% of hits by `key` (class or checkpoint) and how many there were."""

    out = {}
    for it in items:
        k = str(it.get(key))
        b = out.setdefault(k, {"n": 0, "ok": 0.0, "confused": 0})
        b["n"] += 1
        b["ok"] += it["score"]
        b["confused"] += 1 if it.get("confused") else 0
    for b in out.values():
        b["recall"] = b["ok"] / b["n"] if b["n"] else None
    return dict(sorted(out.items()))
