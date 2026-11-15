"""One cell run: one arm, one task, one model, one seed, end to end, in
containers (design §2.3, §6.1).

    network + remotes volume (+ kontext sidecar)
    machine A: fixture, bare remote, arm setup, days on A
    (end of day 3) commit and push, handoff_a; machine B: clone, handoff_b
    days on B; settle; collect transcripts; teardown
    hidden checks on each day's snapshot, in a container with no network
    run.json, with every secret redacted and the results folder scanned
"""

import gzip
import io
import json
import secrets as _secrets
import shlex
import tarfile
import time
from pathlib import Path

from benchlib import cache, container, metrics, runner

from . import fake, interventions
from .arms import ARMS
from .keys import Secrets, scan

COMPARE_DIR = Path(__file__).resolve().parent.parent
BENCH_DIR = COMPARE_DIR.parent
HOME = container.HOME
REPO = f"{HOME}/work/repo"
OUT = f"{HOME}/bench-out"
OPT = "/opt/bench"


class CellConfig:
    def __init__(
        self,
        dry_run=True,
        model=None,
        keys=None,
        fill_scale=1.0,
        turn_timeout=1200,
        settle_timeout=600,
        images=None,
        freeze_status=None,
        keep_containers=False,
        measure=True,
        log=print,
        docker=None,
    ):
        self.dry_run = dry_run
        self.model = model
        # name -> value, from run.py's environment; never in a dry run.
        self.keys = {} if dry_run else dict(keys or {})
        self.fill_scale = fill_scale
        self.turn_timeout = turn_timeout
        self.settle_timeout = settle_timeout
        self.images = images or {}
        self.freeze_status = freeze_status
        self.keep_containers = keep_containers
        self.measure = measure
        self.log = log
        self.docker = docker or container.Docker(log=log)


def model_tag(model, dry_run):
    if dry_run:
        return "fake"
    return (model or "default").replace("claude-", "").replace(".", "-")


# ------------------------------------------------------------ parsing a call


def parse_stream(stdout):
    """(envelope, init, hooks) from `claude -p --output-format stream-json
    --verbose --include-hook-events` (or the single JSON of the made-up agent)."""
    envelope, init, hooks = {}, None, {}
    for line in (stdout or "").splitlines():
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            v = json.loads(line)
        except json.JSONDecodeError:
            continue
        if not isinstance(v, dict):
            continue
        t, st = v.get("type"), v.get("subtype")
        if t == "result":
            envelope = v
        elif t == "system" and st == "init" and init is None:
            init = {
                k: v.get(k)
                for k in ("apiKeySource", "model", "claude_code_version", "permissionMode", "cwd", "output_style")
            }
            init["tools"] = len(v.get("tools") or [])
            init["mcp_servers"] = v.get("mcp_servers")
            init["plugins"] = v.get("plugins")
            init["skills"] = len(v.get("skills") or [])
            init["slash_commands"] = len(v.get("slash_commands") or [])
            init["agents"] = v.get("agents")
        elif t == "system" and st and st.startswith("hook"):
            key = f"{v.get('hook_event') or v.get('hook_event_name') or '?'}:{v.get('hook_name') or v.get('name') or '?'}"
            h = hooks.setdefault(key, {"started": 0, "responses": 0, "nonzero_exit": 0})
            if st == "hook_started":
                h["started"] += 1
            else:
                h["responses"] += 1
                if v.get("exit_code") not in (0, None):
                    h["nonzero_exit"] += 1
    if not envelope:
        envelope = runner.parse_envelope(stdout or "")
    return envelope, init, hooks


def denials(envelope):
    d = envelope.get("permission_denials") or []
    return [x.get("tool_name") if isinstance(x, dict) else str(x) for x in d]


# ------------------------------------------------------------ the run


class Cell:
    def __init__(self, sc, arm_name, seed, run_dir, cfg):
        self.sc = sc
        self.arm = ARMS[arm_name]
        self.seed = seed
        self.cfg = cfg
        self.run_dir = Path(run_dir)
        self.tag = model_tag(cfg.model, cfg.dry_run)
        slug = f"{sc.task}-{arm_name}-{self.tag}-s{seed}".lower()
        self.run_id = f"{slug}-{_secrets.token_hex(3)}"
        self.prefix = f"bench-{self.run_id}"
        self.docker = cfg.docker
        self.secrets = Secrets({k: v for k, v in cfg.keys.items() if k in self.arm.secret_names(cfg.dry_run)})
        self.log = cfg.log
        self.calls = []
        self.steps = []
        self.settles = []
        self.interventions = []
        self.snapshots = {}
        self.cumulative = {}
        self.errors = []
        self.handoff_secret = {}

    # ---------------------------------------------------------- helpers

    def env(self):
        e = {
            "REPO": REPO,
            "BENCH_RUN": self.run_id,
            "BENCH_REMOTE": f"file://{container.REMOTES}/{self.run_id}.git",
            "BENCH_OUT": OUT,
            "BENCH_DRY": "1" if self.cfg.dry_run else "0",
            "BENCH_SEED": str(self.seed),
            **self.arm.run_env(self.run_id),
        }
        if self.arm.sidecar:
            e["KONTEXT_SERVER_URL"] = self.sidecar.url
        if self.cfg.dry_run:
            e["BENCH_SCENARIO"] = f"{OPT}/fake.toml"
            e["BENCH_FAKE_STATE"] = f"{HOME}/.bench-fake"
            # The made-up agent writes its transcripts only under an explicit
            # CLAUDE_CONFIG_DIR; this is Claude Code's default folder anyway.
            e["CLAUDE_CONFIG_DIR"] = f"{HOME}/.claude"
        return e

    def machine(self, letter):
        return container.Machine(
            self.docker,
            f"{self.prefix}-{letter.lower()}",
            self.arm.image,
            f"{self.prefix}-net",
            f"{self.prefix}-remotes",
            self.run_id,
            hostname=f"machine-{letter.lower()}",
            env=self.env(),
            secret_env=self.secrets.values_for(self.arm.secret_names(self.cfg.dry_run)),
        )

    def install_harness_files(self, m):
        files = {}
        for p in sorted((COMPARE_DIR / "arms").rglob("*")):
            if p.is_file() and "__pycache__" not in p.parts:
                files[f"arms/{p.relative_to(COMPARE_DIR / 'arms')}"] = p.read_bytes()
        files["treehash.py"] = (COMPARE_DIR / "incontainer" / "treehash.py").read_bytes()
        files["distill_cmd.py"] = (BENCH_DIR / "distill_cmd.py").read_bytes()
        if self.cfg.dry_run:
            files["fake_agent.py"] = (BENCH_DIR / "fake_agent.py").read_bytes()
            files["fake_distill.py"] = (BENCH_DIR / "fake_distill.py").read_bytes()
            files["fake.toml"] = fake.build(self.sc).encode()
        m.put_files(files, OPT)
        m.sh(f"mkdir -p {OUT} {HOME}/work")

    def run_script(self, m, name, secret_env=None, timeout=900):
        """An arm script; returns (ok, seconds, stdout). BENCH_STEP lines are
        counted, BENCH_SECRET lines are kept in memory only."""
        path = self.arm.script(name)
        if not path:
            return True, 0.0, ""
        t0 = time.monotonic()
        r = m.exec(["bash", f"{OPT}/arms/{self.arm.name}/{name}.sh"], secret_env=secret_env, workdir=HOME, timeout=timeout)
        secs = round(time.monotonic() - t0, 2)
        kept = []
        for line in (r.stdout or "").splitlines():
            if line.startswith("BENCH_SECRET "):
                k, _, v = line[len("BENCH_SECRET ") :].partition("=")
                self.handoff_secret[k.strip()] = v.strip()
                self.secrets.add(k.strip(), v.strip())
                kept.append(f"BENCH_SECRET {k.strip()}=<redacted>")
                continue
            if line.startswith("BENCH_STEP "):
                kind, _, text = line[len("BENCH_STEP ") :].partition(" ")
                self.steps.append({"script": name, "machine": m.hostname, "kind": kind, "step": text})
            kept.append(line)
        out = self.secrets.redact_text("\n".join(kept))
        err = self.secrets.redact_text(r.stderr or "")
        self.steps_log(name, m, r.returncode, secs, out, err)
        if r.returncode != 0:
            self.errors.append({"where": name, "machine": m.hostname, "exit_code": r.returncode, "stderr": err[-1500:]})
        return r.returncode == 0, secs, out

    def steps_log(self, name, m, code, secs, out, err):
        d = self.run_dir / "arm-scripts"
        d.mkdir(parents=True, exist_ok=True)
        (d / f"{m.hostname}-{name}.log").write_text(f"exit {code}, {secs} s\n--- stdout\n{out}\n--- stderr\n{err}\n")

    def git(self, m, script, check=True):
        return m.sh(f"set -e; cd {REPO}; {script}", check=check)

    def treehash(self, m):
        skip = " ".join(shlex.quote(p) for p in interventions.TOOL_PATHS)
        r = m.sh(f"python3 {OPT}/treehash.py {REPO} {skip}", check=False)
        return (r.stdout or "").strip()

    def settle(self, m, when):
        if not self.arm.script("settle"):
            return
        ok, secs, out = self.run_script(m, "settle", timeout=self.cfg.settle_timeout + 60)
        rec = {"when": when, "machine": m.hostname, "seconds": secs, "ok": ok}
        rec["report"] = None
        for text in (out.strip(), (out.strip().splitlines() or [""])[-1]):
            try:
                rec["report"] = json.loads(text)
                break
            except json.JSONDecodeError:
                rec["report"] = out[-300:] or None
        self.settles.append(rec)

    # ---------------------------------------------------------- the agent

    def agent_argv(self, session):
        if self.cfg.dry_run:
            argv = ["python3", f"{OPT}/fake_agent.py", "-p", "--output-format", "json"]
        else:
            argv = [
                "claude", "-p", "--output-format", "stream-json", "--verbose", "--include-hook-events",
                "--permission-mode", "acceptEdits",
                "--allowedTools", ",".join(self.arm.allowed_tools()),
            ]  # fmt: skip
            if self.cfg.model:
                argv += ["--model", self.cfg.model]
        if session:
            argv += ["--resume", session]
        return argv

    def call(self, m, day, kind, prompt, session, meta):
        argv = self.agent_argv(session)
        before = self.treehash(m) if kind in ("work", "reply") else None
        t0 = time.monotonic()
        r = m.exec(argv, input=prompt, workdir=REPO, timeout=self.cfg.turn_timeout)
        secs = round(time.monotonic() - t0, 2)
        envelope, init, hooks = parse_stream(r.stdout)
        sid = envelope.get("session_id") or session
        cum = runner.usage_of(envelope)
        usage = runner.session_delta(cum, self.cumulative.get(sid))
        self.cumulative[sid] = cum
        after = self.treehash(m) if before is not None else None
        result = envelope.get("result") if isinstance(envelope.get("result"), str) else ""
        n = len(self.calls) + 1
        stream_dir = self.run_dir / "transcripts" / "stream"
        stream_dir.mkdir(parents=True, exist_ok=True)
        with gzip.open(stream_dir / f"{n:03d}-day{day.day}-{kind}.jsonl.gz", "wb") as f:
            f.write(self.secrets.redact_bytes((r.stdout or "").encode()))
        rec = {
            "n": n,
            "day": day.day,
            "machine": day.machine,
            "kind": kind,
            **meta,
            "argv": argv,
            "exit_code": r.returncode,
            "seconds": secs,
            "session_id": sid,
            "resumed": bool(session),
            "is_error": bool(envelope.get("is_error")) or r.returncode != 0,
            "result": result,
            "num_turns": envelope.get("num_turns"),
            "usage": usage,
            "permission_denials": denials(envelope),
            "init": init,
            "hooks": hooks,
            "changed": (before != after) if before is not None else None,
            "stderr": (r.stderr or "")[-1500:],
        }
        self.calls.append(rec)
        return rec

    def run_turn(self, m, day, t_index, turn, session):
        prompt = turn.prompt(self.cfg.fill_scale)
        meta = {"turn": t_index}
        if turn.kind == "fill":
            meta["fill"] = {"tokens_asked": int(turn.tokens * self.cfg.fill_scale), "chars": len(prompt), "seed": turn.seed}
            meta["prompt"] = turn.text
        elif turn.kind == "probe":
            meta["prompt"] = turn.text
            meta["probe"] = turn.id
        else:
            meta["prompt"] = prompt
        rec = self.call(m, day, turn.kind, prompt, session, meta)
        session = rec["session_id"]
        if turn.kind == "probe":
            score, _ = metrics.keyword_verdict(rec["result"], turn.expected)
            rec["probe_score"] = score
        # The fixed-reply rule (design §2.2).
        for _ in range(interventions.MAX_PER_TURN):
            last = self.calls[-1]
            why = interventions.needs_reply(turn.kind, any(c.get("changed") for c in self.calls if c["day"] == day.day and c.get("turn") == t_index), last["result"], last["is_error"])
            if not why:
                break
            self.interventions.append({"day": day.day, "turn": t_index, "kind": why})
            rec = self.call(m, day, "reply", interventions.FIXED_REPLY, session, {"turn": t_index, "prompt": interventions.FIXED_REPLY, "intervention": why})
            session = rec["session_id"]
        return session

    # ---------------------------------------------------------- collecting

    def collect(self, m, tag):
        """Transcripts (~/.claude/projects) and BENCH_OUT, redacted, into the run folder."""
        for src, dest in ((f"{HOME}/.claude/projects", f"transcripts/claude-{tag}"), (OUT, f"out-{tag}")):
            data = m.get_tar(src)
            if data:
                self.extract(data, self.run_dir / dest, gz=dest.startswith("transcripts"))

    def extract(self, data, dest, gz):
        with tarfile.open(fileobj=io.BytesIO(data)) as t:
            for ti in t.getmembers():
                if not ti.isfile():
                    continue
                rel = Path(ti.name)
                if rel.is_absolute() or ".." in rel.parts:
                    continue
                body = self.secrets.redact_bytes(t.extractfile(ti).read())
                out = dest / rel
                out.parent.mkdir(parents=True, exist_ok=True)
                if gz and out.suffix == ".jsonl":
                    with gzip.open(out.with_name(out.name + ".gz"), "wb") as f:
                        f.write(body)
                else:
                    out.write_bytes(body)

    def snapshot(self, m, day_n):
        data = m.get_tar(REPO, exclude=(".git", "__pycache__", "node_modules"))
        d = self.run_dir / "snapshots"
        d.mkdir(parents=True, exist_ok=True)
        (d / f"day-{day_n}.tar.gz").write_bytes(gzip.compress(self.secrets.redact_bytes(data)))
        self.snapshots[day_n] = d / f"day-{day_n}.tar.gz"

    # ---------------------------------------------------------- the whole cell

    def run(self):
        self.run_dir.mkdir(parents=True, exist_ok=True)
        t_start = time.monotonic()
        d = self.docker
        current = None
        self.sidecar = None
        try:
            d.run(container.argv_network_create(f"{self.prefix}-net", self.run_id))
            d.run(container.argv_volume_create(f"{self.prefix}-remotes", self.run_id))
            if self.arm.sidecar:
                self.sidecar = container.Sidecar(d, f"{self.prefix}-sidecar", "bench-sidecar:latest", f"{self.prefix}-net", self.run_id)
                self.sidecar.start()
                self.secrets.add("KONTEXT_ADMIN_SECRET", self.sidecar.admin_secret)
            current = self.machine(self.sc.days[0].machine)
            current.start()
            self.install_harness_files(current)
            current.put_dir(self.sc.repo_dir, REPO)
            remote = self.env()["BENCH_REMOTE"]
            self.git(
                current,
                f"git init -q -b main && git add -A && git commit -q -m 'scenario: starting point' && "
                f"git init -q --bare {remote[len('file://'):]} && git remote add origin {remote} && git push -q origin main",
            )
            setup_secret = {"KONTEXT_ADMIN_SECRET": self.sidecar.admin_secret} if self.sidecar else None
            ok, secs, _ = self.run_script(current, "setup", secret_env=setup_secret)
            self.setup = {"ok": ok, "seconds": secs}
            self.git(current, "git add -A && git commit -q --allow-empty -m 'bench: arm setup' && git push -q origin main")
            self.base = self.git(current, "git rev-parse HEAD").stdout.strip()
            prev_machine = current_letter = self.sc.days[0].machine
            for i, day in enumerate(self.sc.days):
                if i > 0:
                    self.settle(current, f"before day {day.day}")
                if day.machine != current_letter:
                    current = self.move(current, day)
                    current_letter = day.machine
                prev_machine = day.machine
                session = None
                for t, turn in enumerate(day.turns, 1):
                    session = self.run_turn(current, day, t, turn, session)
                self.snapshot(current, day.day)
            self.settle(current, "end")
            self.collect(current, prev_machine.lower())
            self.run_script(current, "teardown", timeout=900)
            self.collect(current, prev_machine.lower())
        except Exception as e:  # a broken run is recorded, never hidden
            self.errors.append({"where": "run", "error": self.secrets.redact_text(f"{type(e).__name__}: {e}")[-2000:]})
            if current is not None and current.started:
                try:
                    self.collect(current, "error")
                except Exception:
                    pass
        finally:
            if not self.cfg.keep_containers:
                container.cleanup_run(d, self.run_id)
        wall = round(time.monotonic() - t_start, 1)
        checks = self.verify()
        measured = self.measure() if self.cfg.measure else None
        result = self.result(wall, checks, measured)
        self.write(result)
        return result

    def move(self, a, day):
        """The A → B handoff (design §2.3): the code through the bare remote for
        every arm, the memory only through the arm's own documented path."""
        self.git(a, f"git add -A && git commit -q --allow-empty -m 'bench: end of day {day.day - 1}' && git push -q origin main")
        self.run_script(a, "handoff_a")
        self.collect(a, a.hostname.split("-")[-1])
        a.remove()
        b = self.machine(day.machine)
        b.start()
        self.install_harness_files(b)
        b.sh(f"git clone -q {self.env()['BENCH_REMOTE']} {REPO}")
        self.run_script(b, "handoff_b", secret_env=dict(self.handoff_secret) or None)
        return b

    # ---------------------------------------------------------- after the run

    def verify(self):
        mods = self.sc.check_modules()
        if not mods or not self.snapshots:
            return {}
        days = sorted(self.snapshots)
        if self.sc.acceptance.get("snapshots") != "every-day":
            days = days[-1:]
        out = {}
        for n in days:
            v = container.Verifier(self.docker, f"{self.prefix}-verify-{n}", "bench-verify:latest", self.run_id)
            try:
                out[n] = v.check(gzip.decompress(self.snapshots[n].read_bytes()), self.sc.checks_dir, mods)
            except container.DockerError as e:
                out[n] = {"error": str(e)[-1000:]}
        return out

    def measure(self):
        """`kontext measure --json` over the main-session transcripts, in a
        container of the kontext image with no network (any arm)."""
        files = {}
        for p in sorted((self.run_dir / "transcripts").glob("claude-*/*/*.jsonl.gz")):
            files[f"{p.parent.parent.name}-{p.name[:-3]}"] = gzip.decompress(p.read_bytes())
        if not files:
            return None
        name = f"{self.prefix}-measure"
        try:
            self.docker.run(container.argv_run_verify(name, "bench-kontext:latest", self.run_id))
            m = container.Machine(self.docker, name, "bench-kontext:latest", "none", None, self.run_id, "measure")
            m.put_files(files, f"{HOME}/measure")
            r = m.exec(["bash", "-c", f"cd {HOME}/measure && kontext measure --json *.jsonl"], timeout=300)
            try:
                return json.loads(r.stdout)
            except json.JSONDecodeError:
                return {"error": (r.stderr or r.stdout)[-500:]}
        except container.DockerError as e:
            return {"error": str(e)[-500:]}
        finally:
            self.docker.run(container.argv_rm([name]), check=False)

    def result(self, wall, checks, measured):
        agent = runner.zero_usage()
        for c in self.calls:
            runner.add_usage(agent, c["usage"])
        distill = runner.zero_usage()
        distill_calls = distill_failed = 0
        for p in sorted(self.run_dir.glob("out-*/distill/*.json")):
            try:
                rec = json.loads(p.read_text())
            except json.JSONDecodeError:
                continue
            distill_calls += 1
            if rec.get("exit_code") != 0 or (rec.get("envelope") or {}).get("is_error"):
                distill_failed += 1
            runner.add_usage(distill, runner.usage_of(rec.get("envelope") or {}))
        mem0 = None
        ev = sorted(self.run_dir.glob("out-*/mem0/evidence.json"))
        if ev:
            mem0 = [json.loads(p.read_text()).get("counts") for p in ev]
        main = [p for p in (self.run_dir / "transcripts").glob("claude-*/*/*.jsonl.gz")]
        req_stats = cache.breaks(sorted(main)) if main else None
        totals = runner.zero_usage()
        runner.add_usage(totals, agent)
        runner.add_usage(totals, distill)
        session_seconds = sum(c["seconds"] for c in self.calls)
        out = {
            "scenario": self.sc.name,
            "task": self.sc.task,
            "arm": self.arm.name,
            "model": self.cfg.model,
            "model_tag": self.tag,
            "seed": self.seed,
            "dry_run": self.cfg.dry_run,
            "run_id": self.run_id,
            "images": {k: v for k, v in self.cfg.images.items() if k in (self.arm.image, "bench-base:latest", "bench-verify:latest", "bench-sidecar:latest")},
            "freeze": self.cfg.freeze_status,
            "keys_passed": self.arm.secret_names(self.cfg.dry_run),
            "setup": getattr(self, "setup", None),
            "steps": self.steps,
            "wall_seconds": wall,
            "session_seconds": round(session_seconds, 1),
            "settle": {"waits": self.settles, "seconds": round(sum(s["seconds"] for s in self.settles), 1)},
            "turns": {
                "scripted": sum(1 for c in self.calls if c["kind"] != "reply"),
                "calls": len(self.calls),
                "num_turns": sum(c.get("num_turns") or 0 for c in self.calls),
                "model_requests": (req_stats or {}).get("requests"),
            },
            "interventions": {
                "count": len(self.interventions),
                "by_kind": _count(i["kind"] for i in self.interventions),
                "items": self.interventions,
            },
            "permission_denials": {
                "count": sum(len(c["permission_denials"]) for c in self.calls),
                "by_tool": _count(t for c in self.calls for t in c["permission_denials"]),
            },
            "usage": totals,
            "usage_by": {"agent": agent, "distill": distill},
            "distill": {"calls": distill_calls, "failed": distill_failed},
            "mem0": mem0,
            "cache": req_stats,
            "init": [c["init"] for c in self.calls if c.get("init") and not c["resumed"]],
            "hooks": _merge_hooks(c["hooks"] for c in self.calls),
            "kontext_measure": measured,
            "checks": checks,
            "errors": self.errors,
            "agent_errors": sum(1 for c in self.calls if c["is_error"]),
        }
        out.update(score(self.sc, checks, self.calls))
        return out

    def write(self, result):
        t = self.run_dir / "transcripts"
        t.mkdir(parents=True, exist_ok=True)
        with open(t / "calls.jsonl", "w") as f:
            for c in self.calls:
                f.write(json.dumps(c, ensure_ascii=False) + "\n")
        text = self.secrets.redact_text(json.dumps(result, ensure_ascii=False, indent=2))
        (self.run_dir / "run.json").write_text(text)
        # Redact the call log too, then scan everything that was written.
        p = t / "calls.jsonl"
        p.write_text(self.secrets.redact_text(p.read_text()))
        hits = scan(self.run_dir, self.secrets.values)
        if hits:
            # Never leave a file with a secret: remove it and say which one.
            for path, _ in hits:
                Path(path).unlink(missing_ok=True)
            result["errors"].append({"where": "secret scan", "removed": [{"file": h[0], "secret": h[1]} for h in hits]})
            (self.run_dir / "run.json").write_text(self.secrets.redact_text(json.dumps(result, ensure_ascii=False, indent=2)))
        self.secret_hits = hits


def _count(xs):
    out = {}
    for x in xs:
        out[x] = out.get(x, 0) + 1
    return out


def _merge_hooks(hs):
    out = {}
    for h in hs:
        for k, v in (h or {}).items():
            o = out.setdefault(k, {"started": 0, "responses": 0, "nonzero_exit": 0})
            for kk in o:
                o[kk] += v.get(kk, 0)
    return out


def _passed(day_checks, mod, test):
    tests = ((day_checks or {}).get(mod) or {}).get("tests") or {}
    return tests.get(test) == "ok"


def req_class(c):
    c = c.lower()
    if "answer" in c:
        return "answer"
    if c.startswith("discarded"):
        return "discarded"
    return c.split(",")[0].strip().upper()


def score(sc, checks, calls):
    """The task's own scores (design §2, §4)."""
    if sc.task == "smoke":
        probes = [{"day": c["day"], "machine": c["machine"], "probe": c.get("probe"), "score": c.get("probe_score"), "answer": c["result"][:400]} for c in calls if c["kind"] == "probe"]
        return {"smoke": {"probes": probes, "recalled": sum(p["score"] or 0 for p in probes), "of": len(probes)}}
    if not checks:
        return {"hidden": None}
    if sc.task in ("S1", "S2"):
        day = max(checks)
        tests = ((checks[day] or {}).get("test_hidden") or {}).get("tests") or {}
        ok = sum(1 for v in tests.values() if v == "ok")
        return {"hidden": {"passed": ok, "total": len(tests), "pct": round(100 * ok / len(tests), 1) if tests else None, "tests": tests}}
    reqs = sc.requirements()
    conf = sc.confusions()
    probes_by_req = {}
    for name, c in conf.items():
        probes_by_req.setdefault(c["requirement"], []).append({"probe": name, **c})
    mod = sc.acceptance["modules"][0]
    cmod = sc.acceptance.get("confusions")
    last = max(checks)
    items = []
    for r in reqs:
        sd = r["scored_on"]
        item = {
            "id": r["id"],
            "class": req_class(r["class"]),
            "scored_on": sd,
            "passed": _passed(checks.get(sd), mod, r["test"]),
            "by_day": {d: _passed(checks.get(d), mod, r["test"]) for d in sorted(checks)},
            "coded_before_loss": _passed(checks.get(2), mod, r["test"]),
            "final": _passed(checks.get(last), mod, r["test"]),
        }
        item["regressed"] = bool(sd == 3 and item["passed"] and not _passed(checks.get(4), mod, r["test"]) and 4 in checks)
        took = [p["probe"] for p in probes_by_req.get(r["id"], []) if cmod and _passed(checks.get(sd), cmod, p["probe"])]
        item["confusion"] = took if (took and not item["passed"]) else []
        items.append(item)
    kept = sum(1 for i in items if i["passed"])
    by_class, by_day = {}, {}
    for i in items:
        for key, bucket in ((i["class"], by_class), (i["scored_on"], by_day)):
            b = bucket.setdefault(str(key), {"kept": 0, "of": 0})
            b["of"] += 1
            b["kept"] += int(i["passed"])
    return {
        "requirements": {
            "kept": kept,
            "of": len(items),
            "pct": round(100 * kept / len(items), 1) if items else None,
            "by_class": by_class,
            "by_day": by_day,
            "coded_before_loss": [i["id"] for i in items if i["coded_before_loss"]],
            "regressed": [i["id"] for i in items if i["regressed"]],
            "confusions": sum(1 for i in items if i["confusion"]),
            "confused": {i["id"]: i["confusion"] for i in items if i["confusion"]},
            "failed": [i["id"] for i in items if not i["passed"]],
            "items": items,
        }
    }
