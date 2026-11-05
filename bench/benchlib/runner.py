"""Runs a scenario: for each variant and seed, a new temporary repo, a day = a
new agent session, and the measures at the end."""

import gzip
import json
import os
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

from . import cache, fill, metrics
from .scenario import BENCH_DIR

# `automem` is `none` with Claude Code's automatic memory turned on; the other
# variants turn it off (CLAUDE_CODE_DISABLE_AUTO_MEMORY=1) so it doesn't leak in.
# `kontext-pointer` and `kontext-inject` are the keys of the results aggregate.py
# compares.
VARIANTS = ["none", "automem", "notes", "kontext-pointer", "kontext-inject"]
# What the agent runs without anyone approving it: in `claude -p` nobody does, and
# a command that is not allowed does not run. Besides plain `kontext`, the ways of
# calling it that the command's permission did not cover in long-memory
# (2026-11-04): variables in front (`KONTEXT_ACTOR=… kontext …`),
# `export KONTEXT_…` on another line, `timeout N kontext …`, and pipes into head,
# tail, sed -n or echo. kontext already tells the agent not to do those (the hook,
# `resume` and the skill); this is the safety net. It is given to every variant alike.
AGENT_ALLOWED_TOOLS = [
    "Bash(kontext:*)",
    "Bash(KONTEXT_* kontext *)",
    "Bash(export KONTEXT_*)",
    "Bash(timeout * kontext *)",
    "Bash(python3:*)",
    "Bash(PYTHONPATH=src python3:*)",
    "Bash(git:*)",
    "Bash(ls:*)",
    "Bash(cat:*)",
    "Bash(grep:*)",
    "Bash(head:*)",
    "Bash(tail:*)",
    "Bash(sed -n:*)",
    "Bash(echo:*)",
    "Bash(wc:*)",
    "Bash(sort:*)",
    "Bash(uniq:*)",
]
DEFAULT_AGENT_CMD = (
    "claude -p --output-format json --permission-mode acceptEdits "
    f"--allowedTools {shlex.quote(','.join(AGENT_ALLOWED_TOOLS))}"
)
FAKE_AGENT = BENCH_DIR / "fake_agent.py"
FAKE_DISTILL = BENCH_DIR / "fake_distill.py"
DISTILL_WRAPPER = BENCH_DIR / "distill_cmd.py"
# The distill model of the kontext variants when the scenario turns it on.
# No tools, no configuration and a short system prompt: Claude Code's own is
# ~16k tokens per call, and the distill does not need it.
DEFAULT_DISTILL_CMD = (
    'claude -p --model claude-haiku-5-5 --output-format json --tools "" --setting-sources "" '
    '--system-prompt "You take notes from transcripts. Reply only with the JSON you are asked for."'
)
USAGE_KEYS = ["input_tokens", "output_tokens", "cache_creation_input_tokens", "cache_read_input_tokens"]
GIT_ID = ["-c", "user.name=bench", "-c", "user.email=bench@example.com"]


class Config:
    def __init__(
        self,
        agent_cmd=DEFAULT_AGENT_CMD,
        model=None,
        judge_cmd=None,
        dry_run=False,
        kontext_bin=None,
        keep_work=False,
        turn_timeout=900,
        fill_scale=1.0,
        distill_cmd=None,
        distill_wait=900,
        log=print,
    ):
        self.agent_cmd = agent_cmd
        self.model = model
        self.judge_cmd = judge_cmd
        self.dry_run = dry_run
        self.kontext_bin = kontext_bin
        self.keep_work = keep_work
        self.turn_timeout = turn_timeout
        self.fill_scale = fill_scale
        # Without --distill-cmd: the made-up model in dry runs, DEFAULT_DISTILL_CMD for real.
        self.distill_cmd = distill_cmd
        self.distill_wait = distill_wait
        self.log = log

    def distill_model(self):
        if self.distill_cmd:
            return self.distill_cmd
        if self.dry_run:
            return shlex.join([python(), str(FAKE_DISTILL)])
        return DEFAULT_DISTILL_CMD


def find_kontext(explicit=None):
    """The kontext binary: --kontext-bin, $KONTEXT_BIN, the repo's
    target/{debug,release}/kontext, or `kontext` from the PATH."""
    cands = [explicit, os.environ.get("KONTEXT_BIN")]
    root = BENCH_DIR.parent
    cands += [str(root / "target" / "debug" / "kontext"), str(root / "target" / "release" / "kontext")]
    for c in cands:
        if c and Path(c).is_file() and os.access(c, os.X_OK):
            return str(Path(c).resolve())
    return shutil.which("kontext")


def git(repo, *args):
    r = subprocess.run(["git", *GIT_ID, *args], cwd=repo, capture_output=True, text=True)
    if r.returncode != 0:
        raise RuntimeError(f"git {' '.join(args)} failed: {r.stderr.strip()}")
    return r.stdout.strip()


# ------------------------------------------------------------ preparation


def notes_text(sc):
    """The CLAUDE.md of the `notes` variant: the same thing kontext records before
    day 1, written by hand, and the instruction to keep it up to date."""
    c = sc.charter
    lines = ["# Project notes", "", "## Charter", "", f"Objective: {c['objective']}", ""]
    lines.append("Goals:")
    lines += [f"- {g['title']}" for g in sc.goals]
    lines += ["", "Not doing: " + "; ".join(c.get("non_goals", []))]
    if c.get("done_when"):
        lines += ["", f"Done when: {c['done_when']}"]
    lines += ["", "## Decisions", ""]
    lines += [f"- {d['title']} (why: {d.get('why', '')})" for d in sc.decisions]
    lines += [
        "",
        "## How to use this file",
        "",
        "Read it before changing anything. When we decide something or finish something, write it here.",
        "",
    ]
    return "\n".join(lines)


class KontextSetup:
    """A kontext-home and its machine configuration, isolated in the run's folder
    (KONTEXT_CONFIG_DIR): it never touches ~/.config/kontext."""

    def __init__(self, kontext_bin, work, repo, mode, distill=None):
        self.bin = kontext_bin
        self.config = work / "kontext-config"
        self.home = work / "kontext-home"
        self.repo = repo
        self.mode = mode
        self.project = "bench"
        # The `[distill] command` (already wrapped), or None: off.
        self.distill = distill

    def env(self, actor="human:owner"):
        e = dict(os.environ)
        e.update(
            KONTEXT_CONFIG_DIR=str(self.config),
            KONTEXT_PROJECT=self.project,
            KONTEXT_ACTOR=actor,
            KONTEXT_DISTILL_DISABLE="1",
        )
        e.pop("KONTEXT_ROLE", None)
        return e

    def run(self, *args, cwd=None, actor="human:owner"):
        r = subprocess.run(
            [self.bin, *args],
            cwd=cwd or self.repo,
            env=self.env(actor),
            capture_output=True,
            text=True,
        )
        if r.returncode != 0:
            raise RuntimeError(f"kontext {' '.join(args)} failed ({r.returncode}): {r.stderr.strip()}")
        return r.stdout

    def id_of(self, *args):
        return json.loads(self.run("--json", *args))["id"]

    def setup(self, sc):
        self.config.mkdir(parents=True)
        (self.config / "projects.toml").write_text('writer = "bench@harness"\n')
        self.run("init", self.project, "--home", str(self.home), cwd=self.config)
        toml = self.home / "project.toml"
        text = toml.read_text()
        text = re.sub(r"(?m)^owners = .*$", 'owners = ["owner"]', text)
        text = re.sub(r'(?m)^pm = "\w+"$', f'pm = "{self.mode}"', text, count=1)
        text = re.sub(r'(?m)^coordinator = "\w+"$', f'coordinator = "{self.mode}"', text)
        text = re.sub(r'(?m)^default = "\w+"$', f'default = "{self.mode}"', text)
        if self.distill:
            # The PreCompact and SessionEnd hooks distill with this command; the
            # harness waits for them to finish (`kontext distill --wait`).
            text = re.sub(r"(?m)^command = .*$", lambda _: "command = " + toml_str(self.distill), text, count=1)
            text = re.sub(r"(?m)^enabled = .*$", 'enabled = true\ntimeout = "300s"', text, count=1)
        else:
            text = re.sub(r"(?m)^enabled = true$", "enabled = false", text)
        toml.write_text(text)
        self.run("install", "--role", "pm")
        c = sc.charter
        args = ["charter", "--objective", c["objective"], "--title", c.get("title", c["objective"])]
        for n in c.get("non_goals", []):
            args += ["--non-goal", n]
        if c.get("done_when"):
            args += ["--done-when", c["done_when"]]
        charter = self.id_of(*args)
        self.run("sign", charter)
        goal_ids = {}
        for g in sc.goals:
            a = ["goal", "add", g["title"]]
            if g.get("done_when"):
                a += ["--done-when", g["done_when"]]
            gid = self.id_of(*a)
            self.run("sign", gid)
            goal_ids[g["id"]] = gid
        decision_ids = {}
        for d in sc.decisions:
            decision_ids[d["id"]] = self.id_of(
                "decide", d["title"], "--why", d.get("why", ""), "--guard-cmd", d["guard"]
            )
        return {"charter": charter, "goals": goal_ids, "decisions": decision_ids}

    def agent_env(self):
        e = self.env(actor="agent:pm")
        e["KONTEXT_ROLE"] = "pm"
        if self.distill:
            e.pop("KONTEXT_DISTILL_DISABLE", None)
        return e

    def wait_distill(self, env, timeout):
        """`kontext distill --wait`: until the distills the hooks started have
        finished. Returns the seconds it waited (or None if it failed)."""
        r = subprocess.run(
            [self.bin, "--json", "distill", "--wait", "--timeout", f"{int(timeout)}s"],
            cwd=self.repo,
            env=env,
            capture_output=True,
            text=True,
        )
        if r.returncode != 0:
            return None, r.stderr.strip()[-500:]
        try:
            return json.loads(r.stdout)["waited_ms"] / 1000, None
        except (json.JSONDecodeError, KeyError):
            return None, r.stdout[-500:]


def toml_str(s):
    return json.dumps(s, ensure_ascii=False)


def claude_projects_dir(env):
    """Where Claude Code keeps the transcripts: `$CLAUDE_CONFIG_DIR/projects`
    or `~/.claude/projects`."""
    base = env.get("CLAUDE_CONFIG_DIR") or os.path.join(env.get("HOME", str(Path.home())), ".claude")
    return Path(base) / "projects"


def claude_project_key(repo):
    """The name of a repo's folder in `projects/`: the path with every character
    that is not a letter or a digit changed to `-`."""
    return re.sub(r"[^A-Za-z0-9]", "-", str(Path(repo).resolve()))


def claude_transcript(env, repo, session):
    if not session:
        return None
    p = claude_projects_dir(env) / claude_project_key(repo) / f"{session}.jsonl"
    return p if p.exists() else None


def save_claude_sessions(env, repo, dest):
    """Copies, gzip-compressed, the full Claude Code sessions of this repo
    (`$CLAUDE_CONFIG_DIR/projects/<repo>/`, with the subagents' ones if there are
    any) to `dest`, keeping the subfolders. In the shared `CLAUDE_CONFIG_DIR` they
    are mixed with other runs' sessions; so each run saves its own. Returns how
    many files and how many compressed bytes."""
    src = claude_projects_dir(env) / claude_project_key(repo)
    files, size = 0, 0
    if not src.is_dir():
        return {"files": 0, "bytes": 0}
    for f in sorted(src.rglob("*.jsonl")):
        out = Path(dest) / f.relative_to(src)
        out = out.with_name(out.name + ".gz")
        out.parent.mkdir(parents=True, exist_ok=True)
        with open(f, "rb") as fi, gzip.open(out, "wb", compresslevel=6) as fo:
            shutil.copyfileobj(fi, fo)
        files += 1
        size += out.stat().st_size
    return {"files": files, "bytes": size}


def compactions(path):
    """The `compact_boundary` records of a Claude Code transcript."""
    out = []
    if not path:
        return out
    with open(path, errors="replace") as f:
        for line in f:
            if "compact_boundary" not in line:
                continue
            try:
                r = json.loads(line)
            except json.JSONDecodeError:
                continue
            if r.get("subtype") == "compact_boundary":
                m = r.get("compactMetadata") or {}
                out.append({"trigger": m.get("trigger"), "pre_tokens": m.get("preTokens")})
    return out


def distill_command(cfg, run_dir):
    """The `[distill] command`: the model, wrapped in bench/distill_cmd.py so each
    envelope is saved in `<run>/distill/`."""
    model = shlex.split(cfg.distill_model())
    # Absolute path: kontext runs the command in a temporary folder.
    out = Path(run_dir).resolve() / "distill"
    return shlex.join([python(), str(DISTILL_WRAPPER), "--out", str(out), "--", *model])


def prepare(sc, variant, work, cfg, run_dir=None):
    """Copies the scenario's repo, prepares the variant and leaves the base
    commit. Returns (repo, base, agent environment, preparation notes,
    KontextSetup or None)."""
    repo = work / "repo"
    shutil.copytree(sc.repo_dir, repo)
    git(repo, "init", "-q", "-b", "main")
    git(repo, "add", "-A")
    git(repo, "commit", "-q", "-m", "scenario: starting point")
    env = dict(os.environ)
    info = {}
    setup = None
    if variant == "notes":
        (repo / "CLAUDE.md").write_text(notes_text(sc))
    elif variant in ("kontext-pointer", "kontext-inject"):
        if not cfg.kontext_bin:
            raise RuntimeError("the variant needs kontext: build it with cargo build or use --kontext-bin")
        mode = variant.split("-", 1)[1]
        distill = distill_command(cfg, run_dir or work) if sc.distill else None
        setup = KontextSetup(cfg.kontext_bin, work, repo, mode, distill=distill)
        info = setup.setup(sc)
        info["distill"] = distill
        env = setup.agent_env()
    elif variant not in ("none", "automem"):
        raise ValueError(f"unknown variant {variant!r}")
    git(repo, "add", "-A")
    git(repo, "commit", "-q", "--allow-empty", "-m", f"bench: variant {variant}")
    base = git(repo, "rev-parse", "HEAD")
    return repo, base, env, info, setup


# ------------------------------------------------------------ the agent


def agent_argv(cfg, session):
    argv = shlex.split(cfg.agent_cmd)
    if cfg.model:
        argv += ["--model", cfg.model]
    if session:
        argv += ["--resume", session]
    return argv


def parse_envelope(stdout):
    """The JSON envelope of `claude -p --output-format json` (or the last JSON
    line of the output)."""
    text = stdout.strip()
    try:
        v = json.loads(text)
        if isinstance(v, list):  # stream-json in a list
            v = next((x for x in reversed(v) if isinstance(x, dict) and x.get("type") == "result"), {})
        return v if isinstance(v, dict) else {}
    except json.JSONDecodeError:
        for line in reversed(text.splitlines()):
            try:
                v = json.loads(line)
                if isinstance(v, dict):
                    return v
            except json.JSONDecodeError:
                continue
    return {}


MODEL_USAGE_KEYS = {
    "input_tokens": "inputTokens",
    "output_tokens": "outputTokens",
    "cache_creation_input_tokens": "cacheCreationInputTokens",
    "cache_read_input_tokens": "cacheReadInputTokens",
}


def usage_of(env):
    """The usage of the envelope of `claude -p --output-format json`.

    Careful: with `--resume`, `total_cost_usd` and `modelUsage` are the session's
    running total, not the call's (and `usage` does not count the `/compact`
    call). This returns the running total of `modelUsage` if there is one; for
    one call's usage, see `session_delta`."""
    mu = env.get("modelUsage")
    if isinstance(mu, dict) and mu:
        out = {k: sum(int((m or {}).get(v) or 0) for m in mu.values()) for k, v in MODEL_USAGE_KEYS.items()}
    else:
        u = env.get("usage") or {}
        out = {k: int(u.get(k) or 0) for k in USAGE_KEYS}
    out["cost_usd"] = float(env.get("total_cost_usd") or env.get("cost_usd") or 0)
    return out


def session_delta(cum, prev):
    """One call's share: its running session total minus the running total of
    the previous call in the same session."""
    return {k: max(0, cum[k] - (prev or {}).get(k, 0)) for k in cum}


def day_transcripts(run_dir):
    """The per-day transcripts of a run: `transcripts/day-N.jsonl`."""
    return sorted((Path(run_dir) / "transcripts").glob("day-*.jsonl"))


def call_agent(cfg, prompt, session, repo, env):
    argv = agent_argv(cfg, session)
    t0 = time.monotonic()
    try:
        r = subprocess.run(
            argv,
            input=prompt,
            cwd=repo,
            env=env,
            capture_output=True,
            text=True,
            timeout=cfg.turn_timeout,
        )
        code, stdout, stderr = r.returncode, r.stdout, r.stderr
    except subprocess.TimeoutExpired as e:
        code, stdout, stderr = 124, e.stdout or "", f"timed out after {cfg.turn_timeout} s"
    except FileNotFoundError as e:
        code, stdout, stderr = 127, "", str(e)
    envelope = parse_envelope(stdout if isinstance(stdout, str) else stdout.decode())
    return {
        "argv": argv,
        "exit_code": code,
        "seconds": round(time.monotonic() - t0, 2),
        "session_id": envelope.get("session_id") or session,
        "result": envelope.get("result", "") if isinstance(envelope.get("result", ""), str) else "",
        "is_error": bool(envelope.get("is_error")) or code != 0,
        "usage": usage_of(envelope),
        "envelope": envelope,
        "stderr": (stderr or "")[-2000:],
    }


JUDGE_PROMPT = """You are the strict judge of a memory benchmark. A person asked a code agent
something about their project, and the agent answered. Grade the answer.

Question: {question}
Expected (ALL these ideas must be there; "a | b" are equivalent forms): {expected}
Forbidden (if the answer asserts any of these as current or correct, it scores 0): {forbidden}
{note}Agent's answer:
<<<
{answer}
>>>

Rules:
- Score 1 if the answer gives all the expected ideas as the answer to the question (even if it uses
  other words or number formats) and doesn't assert any forbidden one as current or correct.
- Warnings don't invalidate it: if it gives the expected as what applies and also says that it still
  has to be confirmed, that it is an unreviewed proposal, or that the code doesn't use it yet, it scores 1.
- A forbidden idea mentioned as history or to dismiss it ("it used to be X, now it is Y", "X was
  replaced", "don't confuse it with X") does not count as asserted.
- It is forbidden (and scores 0) if it presents a forbidden idea as the one in force, or if it leaves
  the expected and the forbidden as equally valid without favoring the expected.
- Scores 0, without being forbidden, if it says it doesn't know, that there is no data, or if the
  expected only appears in passing without being given as the answer.

Reply only with JSON: {{"score": 1 or 0, "forbidden": true or false, "why": "one sentence"}}"""


def judge(cfg, probe, answer):
    """1.0 or 0.0, and whether it fell into a forbidden idea. In dry runs (or
    without --judge-cmd), by keywords."""
    kw_score, kw_hits = metrics.keyword_verdict(answer, probe.expected, probe.forbidden)
    if cfg.dry_run or not cfg.judge_cmd:
        return {"score": kw_score, "confused": bool(kw_hits), "forbidden_hits": kw_hits, "by": "words"}
    prompt = JUDGE_PROMPT.format(
        question=probe.question,
        expected=", ".join(probe.expected),
        forbidden=", ".join(probe.forbidden) if probe.forbidden else "(nothing)",
        note=f"Context for the judge: {probe.note}\n" if probe.note else "",
        answer=answer,
    )
    # With several runs at once the judge sometimes hangs: it is retried and, if it
    # never answers, the question is graded by words instead of killing the run.
    r = None
    for _ in range(3):
        try:
            r = subprocess.run(
                shlex.split(cfg.judge_cmd), input=prompt, capture_output=True, text=True, timeout=300
            )
            break
        except subprocess.TimeoutExpired:
            cfg.log("judge: no answer in 300 s, retrying")
    if r is None:
        return {"score": kw_score, "confused": bool(kw_hits), "forbidden_hits": kw_hits, "by": "words (judge hung)"}
    env = parse_envelope(r.stdout)
    text = env.get("result") if isinstance(env.get("result"), str) else r.stdout
    m = re.search(r"\{[^{}]*\"score\"[^{}]*\}", text or "")
    try:
        v = json.loads(m.group(0)) if m else {}
        score = 1.0 if float(v.get("score", 0)) >= 0.5 else 0.0
    except (ValueError, AttributeError):
        v, score = {}, 0.0
    confused = bool(v.get("forbidden")) if isinstance(v, dict) else False
    if confused:
        score = 0.0
    return {
        "score": score,
        "confused": confused,
        "by": "model",
        "why": v.get("why") if isinstance(v, dict) else None,
        "usage": usage_of(env),
        "keywords": kw_score,
        "keywords_forbidden": kw_hits,
    }


# ------------------------------------------------------------ one run


def complied(sc, turn, repo, base, before):
    """Did the agent do what the temptation asked? (= the correction was needed)."""
    kind, _, ident = turn.temptation.partition(":")
    if kind == "decision":
        return before and not metrics.guard_passes(sc.decision(ident), repo)
    out, _, _, _ = metrics.scope_lines(sc, metrics.changed_lines(repo, base))
    return out > before


def tempt_before(sc, turn, repo, base):
    kind, _, ident = turn.temptation.partition(":")
    if kind == "decision":
        return metrics.guard_passes(sc.decision(ident), repo)
    return metrics.scope_lines(sc, metrics.changed_lines(repo, base))[0]


def default_correction(sc, turn):
    kind, _, ident = turn.temptation.partition(":")
    if kind == "decision":
        d = sc.decision(ident)
        return f"We already decided that: {d['title']} ({d.get('why', '')}). Keep it in mind from now on."
    return "That is outside the charter. Keep it in mind from now on."


def zero_usage():
    return {**{k: 0 for k in USAGE_KEYS}, "cost_usd": 0.0}


def add_usage(acc, u):
    for k in acc:
        acc[k] += u.get(k, 0)


def tokens_of(u):
    return sum(u[k] for k in USAGE_KEYS)


def distill_usage(run_dir):
    """The sum of the envelopes bench/distill_cmd.py saved, and how many calls
    there were (and how many failed)."""
    total = zero_usage()
    calls = failed = 0
    for p in sorted((run_dir / "distill").glob("*.json")):
        try:
            rec = json.loads(p.read_text())
        except json.JSONDecodeError:
            continue
        calls += 1
        if rec.get("exit_code") != 0 or (rec.get("envelope") or {}).get("is_error"):
            failed += 1
        add_usage(total, usage_of(rec.get("envelope") or {}))
    return total, calls, failed


def probe_record(p, d, answer, verdict):
    return {
        "day": d,
        "id": p.id,
        "class": p.cls,
        "checkpoint": p.point,
        "question": p.question,
        "expected": p.expected,
        "forbidden": p.forbidden,
        "answer": answer,
        **verdict,
    }


def run_one(sc, variant, seed, run_dir, cfg):
    run_dir.mkdir(parents=True, exist_ok=True)
    (run_dir / "transcripts").mkdir(exist_ok=True)
    work = Path(tempfile.mkdtemp(prefix=f"kontext-bench-{variant}-s{seed}-"))
    try:
        repo, base, env, info, setup = prepare(sc, variant, work, cfg, run_dir)
        env = dict(env)
        env["BENCH_SEED"] = str(seed)
        env["BENCH_VARIANT"] = variant
        if variant == "automem":
            env.pop("CLAUDE_CODE_DISABLE_AUTO_MEMORY", None)
        else:
            env["CLAUDE_CODE_DISABLE_AUTO_MEMORY"] = "1"
        # The made-up distill reads the scenario's facts.
        env["BENCH_SCENARIO"] = str(sc.toml_path)
        if cfg.dry_run:
            env["BENCH_FAKE_STATE"] = str(work / "fake-state")
            # The made-up agent writes its transcripts the way Claude Code does.
            env["CLAUDE_CONFIG_DIR"] = str(work / "claude-config")
        if setup:
            # `kontext recall` searches the transcripts of this repo, not the ones in
            # ~/.claude (the user's) or the other runs'.
            env["KONTEXT_TRANSCRIPTS_GLOB"] = str(claude_projects_dir(env) / claude_project_key(repo) / "*.jsonl")
            info["transcripts_glob"] = env["KONTEXT_TRANSCRIPTS_GLOB"]
        waits = setup is not None and bool(setup.distill)
        usage = {"agent": zero_usage(), "judge": zero_usage(), "distill": zero_usage()}
        corrections = []
        temptations = 0
        probes = []
        compacts = []
        fills = []
        errors = 0
        distill_errors = []

        cumulative = {}  # session_id -> running usage that the last envelope reported
        for d, day in enumerate(sc.days, 1):
            session = None
            transcript = []

            def say(prompt, kind, extra=None, record_prompt=None):
                nonlocal session, errors
                out = call_agent(cfg, prompt, session, repo, env)
                session = out["session_id"]
                # The envelope carries the session's running total: the difference is added.
                cum = out["usage"]
                out["usage"] = session_delta(cum, cumulative.get(session))
                cumulative[session] = cum
                add_usage(usage["agent"], out["usage"])
                if out["is_error"]:
                    errors += 1
                rec = {"day": d, "kind": kind, "prompt": prompt if record_prompt is None else record_prompt}
                rec.update(extra or {})
                rec.update(out)
                if waits:
                    # The PreCompact and SessionEnd hooks start the distill in the
                    # background; the next turn does not go out until it finishes.
                    secs, err = setup.wait_distill(env, cfg.distill_wait)
                    rec["distill_wait_s"] = secs
                    if err:
                        rec["distill_wait_error"] = err
                        distill_errors.append({"day": d, "kind": kind, "error": err})
                transcript.append(rec)
                return out

            def ask(p):
                out = say(p.question, "probe", {"probe": p.id, "class": p.cls, "checkpoint": p.point})
                verdict = judge(cfg, p, out["result"])
                if "usage" in verdict:
                    add_usage(usage["judge"], verdict["usage"])
                probes.append(probe_record(p, d, out["result"], verdict))

            for t, turn in enumerate(day, 1):
                if turn.kind == "probe":
                    ask(turn.probe)
                elif turn.kind == "fill":
                    prompt, doc_chars = fill.prompt(
                        turn.text,
                        turn.docs,
                        turn.tokens,
                        turn.seed,
                        [sc.distractors[x] for x in turn.distractors],
                        cfg.fill_scale,
                    )
                    meta = {
                        "turn": t,
                        "fill": {
                            "docs": turn.docs,
                            "seed": turn.seed,
                            "tokens_asked": int(turn.tokens * cfg.fill_scale),
                            "chars": len(prompt),
                            "doc_chars": doc_chars,
                            "sha256": fill.sha256(prompt),
                            "distractors": turn.distractors,
                        },
                    }
                    # The transcript keeps the request, not the document.
                    say(prompt, "fill", meta, record_prompt=turn.text)
                    fills.append({"day": d, **meta["fill"]})
                elif turn.kind == "compact":
                    out = say("/compact", "compact", {"turn": t})
                    found = compactions(claude_transcript(env, repo, out["session_id"]))
                    compacts.append(
                        {
                            "day": d,
                            "turn": t,
                            "ok": bool(found),
                            "pre_tokens": found[-1]["pre_tokens"] if found else None,
                            "trigger": found[-1]["trigger"] if found else None,
                            "exit_code": out["exit_code"],
                        }
                    )
                elif turn.temptation:
                    temptations += 1
                    before = tempt_before(sc, turn, repo, base)
                    say(turn.text, "turn", {"turn": t, "temptation": turn.temptation})
                    if complied(sc, turn, repo, base, before):
                        text = turn.correction or default_correction(sc, turn)
                        corrections.append({"day": d, "turn": t, "temptation": turn.temptation})
                        say(text, "correction", {"turn": t, "temptation": turn.temptation})
                else:
                    say(turn.text, "turn", {"turn": t})
            for p in [p for p in sc.probes if p.day == d]:
                ask(p)
            with open(run_dir / "transcripts" / f"day-{d}.jsonl", "w") as f:
                for rec in transcript:
                    f.write(json.dumps(rec, ensure_ascii=False) + "\n")

        # The full Claude Code sessions, to measure the prompt cache (and whatever
        # else is wanted later) without depending on the shared CLAUDE_CONFIG_DIR.
        claude_dir = run_dir / "transcripts" / "claude"
        sessions = save_claude_sessions(env, repo, claude_dir)
        cache_stats = cache.breaks(sorted(claude_dir.glob("*.jsonl.gz")))

        dist_usage, dist_calls, dist_failed = distill_usage(run_dir)
        usage["distill"] = dist_usage
        totals = zero_usage()
        for u in usage.values():
            add_usage(totals, u)
        files = metrics.changed_lines(repo, base)
        out_lines, total_lines, per_goal, outside = metrics.scope_lines(sc, files)
        g = metrics.guards(sc, repo)
        failed = [k for k, ok in g.items() if not ok]
        acc = metrics.acceptance(sc, repo)
        result = {
            "scenario": sc.name,
            "variant": variant,
            "seed": seed,
            "dry_run": cfg.dry_run,
            "agent_cmd": cfg.agent_cmd,
            "model": cfg.model,
            "fill_scale": cfg.fill_scale,
            "kontext": info,
            "lines": {
                "out_of_charter": out_lines,
                "total": total_lines,
                "per_goal": per_goal,
                "outside_files": outside,
                "files": files,
            },
            "guards": {"results": g, "failed": failed, "total": len(g)},
            "desvio_pct": round(
                metrics.desvio_pct(out_lines, total_lines, len(failed), len(g)), 2
            ),
            "corrections": {"count": len(corrections), "temptations": temptations, "turns": corrections},
            "probes": {
                "recall": (sum(p["score"] for p in probes) / len(probes)) if probes else None,
                "confusions": sum(1 for p in probes if p.get("confused")),
                "by_class": metrics.recall_by(probes, "class"),
                "by_checkpoint": metrics.recall_by(probes, "checkpoint"),
                "items": probes,
            },
            "compacts": compacts,
            "fills": fills,
            "distill": {
                "enabled": waits,
                "calls": dist_calls,
                "failed": dist_failed,
                "wait_errors": distill_errors,
                "entries": distilled_entries(setup) if waits else None,
            },
            "cache": cache_stats,
            "claude_sessions": sessions,
            "usage": totals,
            "usage_by": usage,
            "tokens": tokens_of(totals),
            "acceptance": acc,
            "agent_errors": errors,
            "work": str(work) if cfg.keep_work else None,
        }
        (run_dir / "run.json").write_text(json.dumps(result, ensure_ascii=False, indent=2))
        return result
    finally:
        if not cfg.keep_work:
            shutil.rmtree(work, ignore_errors=True)


def distilled_entries(setup):
    """What the distill wrote to the ledger (id, type, title, origin and what it
    replaces), read from the files in `ledger/`."""
    out = []
    for f in sorted((setup.home / "ledger").glob("*.jsonl")):
        for line in f.read_text().splitlines():
            try:
                e = json.loads(line)
            except json.JSONDecodeError:
                continue
            a = e.get("actor") or {}
            if isinstance(a, dict) and a.get("name") == "distill":
                f = e.get("fields") or {}
                out.append(
                    {
                        "id": e.get("id"),
                        "kind": e.get("kind"),
                        "title": e.get("title"),
                        "origin": f.get("origin"),
                        "supersedes": f.get("supersedes") or [],
                    }
                )
    return out


def run_all(sc, variants, seeds, out_root, cfg, first_seed=1):
    stamp = time.strftime("%Y%m%d-%H%M%S")
    out = Path(out_root).resolve() / f"{stamp}-{sc.name}{'-dry' if cfg.dry_run else ''}"
    out.parent.mkdir(parents=True, exist_ok=True)
    base, n = out.name, 1
    while True:
        # Two runs that start in the same second share the stamp: the second one gets -2.
        try:
            out.mkdir()
            break
        except FileExistsError:
            n += 1
            out = out.with_name(f"{base}-{n}")
    results = []
    for v in variants:
        for s in range(first_seed, first_seed + seeds):
            cfg.log(f"running {sc.name} · {v} · seed {s}")
            results.append(run_one(sc, v, s, out / "runs" / f"{v}-s{s}", cfg))
    return out, results


def python():
    return sys.executable or "python3"
