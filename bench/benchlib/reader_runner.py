"""Runs the reader benchmark: for each configuration, size, seed and task, one
fresh `claude -p` in a fresh copy of the fixture, graded against its truth.

The fixture of a `(size, seed)` is generated once (`<out>/fixtures/`), so every
configuration reads the same bytes; each call copies it to its own temporary
folder, so calls never share files nor a Claude Code project key.

A run is keyed by its variant (configuration, prompt version, main effort,
subagent effort), size, seed and task: the key names its folder and is in its
run.json, so runs of different variants share one results folder and never mix.
"""

import hashlib
import json
import os
import shlex
import shutil
import subprocess
import tempfile
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

from . import cache, pmfixture, reader_grade, runner
from .scenario import BENCH_DIR

MODELS = {"opus": "claude-opus-5-5", "sonnet": "claude-sonnet-5-5", "haiku": "claude-haiku-5-5"}
# name -> (model of the main session, model of the pm-reader subagent or None)
CONFIGS = {
    "opus": ("opus", None),
    "opus+sonnet": ("opus", "sonnet"),
    "opus+haiku": ("opus", "haiku"),
    # Diagnosis only: that model reads by itself.
    "sonnet": ("sonnet", None),
    "haiku": ("haiku", None),
}
DEFAULT_CONFIGS = ["opus", "opus+sonnet", "opus+haiku"]
TASKS = ["audit", "value"]
READER_TOOLS = ["Read", "Grep", "Glob"]
# No user, project or local settings and no MCP servers. Run it under the clean
# environment of bench/README.md, which also gives it its own CLAUDE_CONFIG_DIR.
DEFAULT_READER_CMD = 'claude -p --output-format json --setting-sources "" --strict-mcp-config'
FAKE_READER = BENCH_DIR / "fake_reader.py"
PROMPT_PATH = BENCH_DIR / "reader" / "prompt.md"
# What `kontext install` ships: the reader agent (its body is the reader's
# instructions) and the skill (its `## Audit task` and `## Value task` sections
# are the task texts the PM passes word for word).
TEMPLATES = BENCH_DIR.parent / "templates"
PM_READER_PATH = TEMPLATES / "agents" / "kontext-pm-reader.md"
PM_SKILL_PATH = TEMPLATES / "skill-kontext-pm.md"
# Their names among the sections (`prompt_sections`), next to prompt.md's.
SHIPPED = {
    "reader": "templates/agents/kontext-pm-reader.md (body)",
    "audit": "templates/skill-kontext-pm.md, Audit task",
    "value": "templates/skill-kontext-pm.md, Value task",
}
# The sections each prompt version uses. v1 is the pilot's; `skill` is what
# ships, with the bench's own `Delegation` in front of the task.
PROMPTS = {
    "v1": {"reader": "Reader", "audit": "Audit task", "value": "Value task", "delegation": "Delegation"},
    "v2": {"reader": "Reader v2", "audit": "Audit task v2", "value": "Value task v2", "delegation": "Delegation"},
    "skill": {**SHIPPED, "delegation": "Delegation"},
}
DEFAULT_PROMPT = "v1"
# Effort levels, as the subagent tool's `effort` parameter lists them in Claude
# Code 2.1.318 transcripts. `--effort` and `--sub-effort` take one of them.
EFFORT_LEVELS = ("low", "medium", "high", "xhigh", "max")
# Overrides both the session's and the subagent's effort: the calls never get it.
EFFORT_ENV = "CLAUDE_CODE_EFFORT_LEVEL"

# How a delegated configuration declares its subagent. Best effort for Claude
# Code 2.1.318, not checked against a model: `claude --help` documents
# `--agents '{"name": {"description": …, "prompt": …}}'`; `tools` and `model`
# are the fields of an agent file's frontmatter, and `Agent` is the name of the
# subagent tool in local 2.1.x transcripts (older versions called it `Task`).
# Whether `model` takes a full id or only an alias (`sonnet`; the Agent tool's
# own `model` parameter showed an alias in a local transcript) is unknown: the
# pilot checks `modelUsage` and the subagent transcripts. Fix it here
# (`model_as`: "id" or "alias"). The pilot confirmed the full id: the subagent's
# requests ran on it. `effort` (`--sub-effort`) is the subagent's own effort
# level, which overrides the session's; only `CLAUDE_CODE_EFFORT_LEVEL` would
# override it, so the calls never get that variable (`call_env`). The effort
# each session really ran at is read back from its transcript. With `--prompt
# skill` the entry's `prompt` is the shipped agent's body, but its name,
# description and model stay these (the shipped file says `kontext-pm-reader`
# and `model: haiku`, the alias of the same Haiku), so the configurations and
# the gate compare like for like.
SUBAGENT = {
    "tool": "Agent",
    "name": "pm-reader",
    "description": "Reads the project's memory folder .pm/ and answers with verbatim citations (quote, file, line).",
    "fields": {"description": "description", "prompt": "prompt", "tools": "tools", "model": "model", "effort": "effort"},
    "model_as": "id",
}
# Tool names counted as a delegation in the main session.
AGENT_TOOLS = {SUBAGENT["tool"], "Task"}


def config_models(name):
    main, sub = CONFIGS[name]
    return MODELS[main], (MODELS[sub] if sub else None)


def markdown_sections(text):
    """The `## ` sections of a markdown text, by heading. A `## ` line inside a
    fenced block (```) is text, not a heading."""
    out, cur, fenced = {}, None, False
    for line in text.split("\n"):
        if line.startswith("```"):
            fenced = not fenced
        if line.startswith("## ") and not fenced:
            cur = line[3:].strip()
            out[cur] = []
        elif cur:
            out[cur].append(line)
    return {k: "\n".join(v).strip() for k, v in out.items()}


def agent_body(text):
    """The body of an agent file: what follows its frontmatter (`---` … `---`),
    the subagent's system prompt."""
    if not text.startswith("---\n"):
        return text.strip()
    end = text.find("\n---\n", 4)
    if end < 0:
        raise ValueError("the agent file's frontmatter is not closed")
    return text[end + len("\n---\n") :].strip()


def prompt_sections(path=PROMPT_PATH, skill=PM_SKILL_PATH, agent=PM_READER_PATH):
    """The `## ` sections of prompt.md, by heading, plus the shipped texts
    (`SHIPPED`): the reader agent's body and the skill's two task sections."""
    out = markdown_sections(Path(path).read_text())
    skill_sections = markdown_sections(Path(skill).read_text())
    out[SHIPPED["reader"]] = agent_body(Path(agent).read_text())
    out[SHIPPED["audit"]] = skill_sections["Audit task"]
    out[SHIPPED["value"]] = skill_sections["Value task"]
    return out


def section(sections, prompt, piece):
    """The text of one piece (`reader`, `audit`, `value`, `delegation`) of a prompt version."""
    return sections[PROMPTS[prompt][piece]]


def prompt_set_sha256(sections, prompt):
    """The sha256 of every section a prompt version uses, with their headings:
    it changes if any of them changes."""
    h = hashlib.sha256()
    for name in dict.fromkeys(PROMPTS[prompt].values()):
        h.update(f"## {name}\n\n{sections[name]}\n\n".encode())
    return h.hexdigest()


def sha256(text):
    return hashlib.sha256(text.encode()).hexdigest() if text is not None else None


def task_text(sections, task, questions, prompt=DEFAULT_PROMPT):
    if task == "audit":
        return section(sections, prompt, "audit")
    qs = "\n".join(f"- {q['id']}: {q['question']}" for q in questions)
    return section(sections, prompt, "value").replace("{questions}", qs)


def agents_json(sub_model, reader_prompt, effort=None):
    f = SUBAGENT["fields"]
    agent = {
        f["description"]: SUBAGENT["description"],
        f["prompt"]: reader_prompt,
        f["tools"]: READER_TOOLS,
        f["model"]: sub_model if SUBAGENT["model_as"] == "id" else model_alias(sub_model),
    }
    if effort:
        agent[f["effort"]] = effort
    return json.dumps({SUBAGENT["name"]: agent}, ensure_ascii=False)


def build_call(config, task, sections, questions, reader_cmd, effort=None, prompt=DEFAULT_PROMPT, sub_effort=None):
    """(argv, the prompt for stdin). The argv is recorded as is in run.json.
    `effort` is the main (or only) session's; `sub_effort`, the subagent's
    (delegated configurations only)."""
    main, sub = config_models(config)
    argv = shlex.split(reader_cmd) + ["--model", main]
    if effort:
        argv += ["--effort", effort]
    task_prompt = task_text(sections, task, questions, prompt)
    reader = section(sections, prompt, "reader")
    if sub is None:
        argv += ["--tools", ",".join(READER_TOOLS)]
        text = reader + "\n\n" + task_prompt
    else:
        argv += ["--tools", ",".join([*READER_TOOLS, SUBAGENT["tool"]])]
        argv += ["--agents", agents_json(sub, reader, sub_effort)]
        lead = section(sections, prompt, "delegation")
        lead = lead.replace("{agent}", SUBAGENT["name"]).replace("{tool}", SUBAGENT["tool"])
        text = lead + "\n\n" + task_prompt
    return argv, text + "\n"


def agent_prompt_of(argv):
    """The subagent's prompt in an argv's `--agents`, or None."""
    if "--agents" not in argv:
        return None
    try:
        agents = json.loads(argv[argv.index("--agents") + 1])
    except (IndexError, json.JSONDecodeError):
        return None
    return (agents.get(SUBAGENT["name"]) or {}).get(SUBAGENT["fields"]["prompt"])


# ------------------------------------------------------------ variants


def is_delegated(config):
    return CONFIGS[config][1] is not None


def variant_of(config, prompt=DEFAULT_PROMPT, effort_main=None, effort_sub=None):
    """(config, prompt, main effort, subagent effort): None is the default
    effort (not passed). A direct configuration has no subagent effort."""
    return (config, prompt, effort_main or None, (effort_sub or None) if is_delegated(config) else None)


def variant_name(variant):
    """`<config>-<prompt>-emain_<level>-esub_<level>`: `default` when not passed,
    `none` for the subagent of a direct configuration."""
    config, prompt, em, es = variant
    sub = es or ("default" if is_delegated(config) else "none")
    return f"{config}-{prompt}-emain_{em or 'default'}-esub_{sub}"


def run_name(variant, size, seed, task):
    return f"{variant_name(variant)}-{size}-s{seed}-{task}"


def model_alias(model_id):
    for a in MODELS:
        if a in str(model_id).lower():
            return a
    return str(model_id)


def usage_by_model(envelope):
    """`modelUsage` of the envelope, per model: tokens by type and `costUSD`."""
    out = {}
    mu = envelope.get("modelUsage")
    if isinstance(mu, dict):
        for model, m in mu.items():
            m = m if isinstance(m, dict) else {}
            u = {k: int(m.get(v) or 0) for k, v in runner.MODEL_USAGE_KEYS.items()}
            u["cost_usd"] = float(m.get("costUSD") or 0)
            out[model] = u
    return out


def prompt_tokens(u):
    """Every input token of a call: uncached, written to the cache and read from it."""
    return u["input_tokens"] + u["cache_creation_input_tokens"] + u["cache_read_input_tokens"]


# ------------------------------------------------------------ transcripts


def touches_dumps(name, inp):
    """Did this tool call read `.pm/dumps/`? A Read of a file in it, or a Grep
    over it (or over all of `.pm/`). Glob only lists names: it does not count."""
    if not isinstance(inp, dict) or name not in ("Read", "Grep"):
        return False
    vals = [str(inp.get(k) or "") for k in ("file_path", "path", "glob")]
    if any(".pm/dumps" in v for v in vals):
        return True
    p = str(inp.get("path") or "").rstrip("/")
    return name == "Grep" and (p == ".pm" or p.endswith("/.pm"))


def record_effort(r):
    """The effort a session record shows: `effort` of an assistant record, or
    `params.output_config.effort` of an api-request record; else None."""
    if r.get("type") == "assistant":
        e = r.get("effort")
    elif r.get("type") == "api-request":
        params = r.get("params") if isinstance(r.get("params"), dict) else {}
        oc = params.get("output_config") if isinstance(params.get("output_config"), dict) else {}
        e = oc.get("effort")
    else:
        return None
    return e if isinstance(e, str) and e else None


def transcript_stats(folder):
    """From the saved Claude Code sessions of a call (`<sid>.jsonl[.gz]` the main
    one, `<sid>/subagents/*.jsonl[.gz]` the subagents'; a record with
    `isSidechain`, or a request whose `querySource` is `agent:…`, also counts as
    a subagent's): tool calls by name in the main session and in subagents,
    delegations, whether the main session read `.pm/dumps/`, and the effort
    levels each side ran at. Missing or broken files count as nothing."""
    out = {
        "main_files": 0,
        "sub_files": 0,
        "main_tools": {},
        "sub_tools": {},
        "agent_calls": 0,
        "agent_types": [],
        "main_dump_reads": 0,
        "main_read_dumps": False,
        "main_models": [],
        "sub_models": [],
        # The largest single request (uncached + cache writes + cache reads): about
        # the system prompt plus everything read so far, unlike the envelope's sum.
        "main_peak_context": 0,
        "sub_peak_context": 0,
        # Effort levels seen in the records (`record_effort`), and the `effort`
        # the main session passed in its calls of the subagent tool, if any.
        "effort_seen_main": [],
        "effort_seen_sub": [],
        "agent_effort_args": [],
    }
    folder = Path(folder)
    if not folder.is_dir():
        return out
    seen_ids = set()
    for f in sorted(folder.rglob("*")):
        if not f.is_file() or not (f.name.endswith(".jsonl") or f.name.endswith(".jsonl.gz")):
            continue
        nested = f.parent != folder
        out["sub_files" if nested else "main_files"] += 1
        try:
            with cache._open(f) as fh:
                lines = list(fh)
        except (OSError, EOFError):
            continue
        for line in lines:
            if '"assistant"' not in line and '"effort"' not in line:
                continue
            try:
                r = json.loads(line)
            except json.JSONDecodeError:
                continue
            if not isinstance(r, dict):
                continue
            side = nested or bool(r.get("isSidechain")) or str(r.get("querySource") or "").startswith("agent:")
            effort = record_effort(r)
            seen = out["effort_seen_sub" if side else "effort_seen_main"]
            if effort and effort not in seen:
                seen.append(effort)
            msg = r.get("message")
            if r.get("type") != "assistant" or not isinstance(msg, dict):
                continue
            model = msg.get("model")
            models = out["sub_models" if side else "main_models"]
            if isinstance(model, str) and model not in models and not model.startswith("<"):
                models.append(model)
            u = msg.get("usage") if isinstance(msg.get("usage"), dict) else {}
            ctx = sum(int(u.get(k) or 0) for k in ("input_tokens", "cache_creation_input_tokens", "cache_read_input_tokens"))
            peak = "sub_peak_context" if side else "main_peak_context"
            out[peak] = max(out[peak], ctx)
            content = msg.get("content")
            for i, b in enumerate(content if isinstance(content, list) else []):
                if not isinstance(b, dict) or b.get("type") != "tool_use":
                    continue
                bid = b.get("id") or (str(f), msg.get("id"), i)
                if bid in seen_ids:
                    continue
                seen_ids.add(bid)
                name = str(b.get("name") or "?")
                tools = out["sub_tools" if side else "main_tools"]
                tools[name] = tools.get(name, 0) + 1
                if side:
                    continue
                inp = b.get("input") if isinstance(b.get("input"), dict) else {}
                if name in AGENT_TOOLS:
                    out["agent_calls"] += 1
                    out["agent_types"].append(inp.get("subagent_type"))
                    if inp.get("effort"):
                        out["agent_effort_args"].append(inp["effort"])
                if touches_dumps(name, inp):
                    out["main_dump_reads"] += 1
    out["main_read_dumps"] = out["main_dump_reads"] > 0
    for k in ("main_models", "sub_models", "effort_seen_main", "effort_seen_sub"):
        out[k].sort()
    return out


# ------------------------------------------------------------ one call


class Options:
    def __init__(
        self,
        out,
        reader_cmd=DEFAULT_READER_CMD,
        dry_run=False,
        effort=None,
        sub_effort=None,
        prompt=DEFAULT_PROMPT,
        timeout=900,
        keep_work=False,
        jobs=1,
        spec_path=pmfixture.SPEC_PATH,
        log=print,
    ):
        self.out = Path(out)
        self.reader_cmd = reader_cmd
        self.dry_run = dry_run
        self.effort = effort
        self.sub_effort = sub_effort
        self.prompt = prompt
        self.timeout = timeout
        self.keep_work = keep_work
        self.jobs = jobs
        self.spec_path = Path(spec_path)
        self.log = log


def fake_reader_cmd():
    return shlex.join([runner.python(), str(FAKE_READER)]) + " " + DEFAULT_READER_CMD.split(" ", 1)[1]


def prepare_fixtures(spec, sizes, seeds, out):
    """Generates each `(size, seed)` once: `<out>/fixtures/<size>-s<seed>/root/.pm/`
    and, next to `root/`, `truth.json`."""
    fixtures = {}
    for size in sizes:
        for seed in seeds:
            dest = Path(out) / "fixtures" / f"{size}-s{seed}"
            if dest.exists():
                shutil.rmtree(dest)
            fixtures[(size, seed)] = (dest, pmfixture.write(spec, size, seed, dest))
    return fixtures


def run_dir_of(out, variant, size, seed, task):
    return Path(out) / "runs" / run_name(variant, size, seed, task)


def call_env(opts, work):
    """The environment of one call: this one's, without `CLAUDE_CODE_EFFORT_LEVEL`
    (it would override `--effort` and the subagent's `effort`), and with
    auto-memory off. A dry run also points the made-up reader at its own
    Claude Code folder and at the spec."""
    env = dict(os.environ)
    env.pop(EFFORT_ENV, None)
    env["CLAUDE_CODE_DISABLE_AUTO_MEMORY"] = "1"
    if opts.dry_run:
        # The made-up reader writes its transcripts the way Claude Code does,
        # and reads the spec (never the truth) to know what to look for.
        env["CLAUDE_CONFIG_DIR"] = str(Path(work) / "claude-config")
        env["BENCH_READER_SPEC"] = str(opts.spec_path)
    return env


def call_reader(argv, prompt, cwd, env, timeout):
    t0 = time.monotonic()
    timed_out = False
    try:
        r = subprocess.run(argv, input=prompt, cwd=cwd, env=env, capture_output=True, text=True, timeout=timeout)
        code, stdout, stderr = r.returncode, r.stdout, r.stderr
    except subprocess.TimeoutExpired as e:
        timed_out = True
        stdout = e.stdout.decode(errors="replace") if isinstance(e.stdout, bytes) else (e.stdout or "")
        code, stderr = 124, f"timed out after {timeout} s"
    except FileNotFoundError as e:
        code, stdout, stderr = 127, "", str(e)
    return code, stdout, stderr, round(time.monotonic() - t0, 2), timed_out


def run_call(opts, sections, config, size, seed, task, fixture_dir, truth):
    """One call: copy the fixture, run the reader, grade it, save its sessions
    and `run.json`. Returns the record."""
    variant = variant_of(config, opts.prompt, opts.effort, opts.sub_effort)
    _, prompt_version, effort_main, effort_sub = variant
    name = run_name(variant, size, seed, task)
    run_dir = run_dir_of(opts.out, variant, size, seed, task)
    if run_dir.exists():
        shutil.rmtree(run_dir)
    run_dir.mkdir(parents=True)
    work = Path(tempfile.mkdtemp(prefix=f"kontext-reader-{name.replace('+', '-')}-"))
    try:
        project = work / "project"
        shutil.copytree(Path(fixture_dir) / "root", project)
        env = call_env(opts, work)
        argv, prompt = build_call(
            config, task, sections, truth["questions"], opts.reader_cmd, effort_main, prompt_version, effort_sub
        )
        code, stdout, stderr, seconds, timed_out = call_reader(argv, prompt, project, env, opts.timeout)
        envelope = runner.parse_envelope(stdout)
        result = envelope.get("result") if isinstance(envelope.get("result"), str) else ""
        fx = reader_grade.Fixture(Path(fixture_dir) / "root", truth)
        answer, grades = reader_grade.grade(fx, task, result, prompt_version)
        sessions = runner.save_claude_sessions(env, project, run_dir / "transcripts" / "claude")
        stats = transcript_stats(run_dir / "transcripts" / "claude")
        total = runner.usage_of(envelope)
        by_model = usage_by_model(envelope)
        main, sub = config_models(config)
        record = {
            "run": name,
            "variant": variant_name(variant),
            "key": {
                "config": config,
                "prompt": prompt_version,
                "effort_main": effort_main,
                "effort_sub": effort_sub,
                "size": size,
                "seed": seed,
                "task": task,
            },
            "config": config,
            "size": size,
            "seed": seed,
            "task": task,
            "dry_run": opts.dry_run,
            "models": {"main": main, "subagent": sub},
            "spec": {"version": truth.get("spec_version"), "sha256": truth.get("spec_sha256")},
            "prompt_version": prompt_version,
            # The exact text on stdin, the subagent's prompt in `--agents`, and
            # every section of the prompt version (the report's header).
            "prompt_sha256": sha256(prompt),
            "agent_prompt_sha256": sha256(agent_prompt_of(argv)),
            "prompt_set_sha256": prompt_set_sha256(sections, prompt_version),
            # Requested (None: not passed) and seen in the transcripts.
            "effort_main": effort_main,
            "effort_sub": effort_sub,
            "effort_seen_main": stats["effort_seen_main"],
            "effort_seen_sub": stats["effort_seen_sub"],
            "argv": argv,
            "prompt": prompt,
            "fixture": {
                "dir": str(Path(fixture_dir).resolve()),
                "sha256": truth["sha256"],
                "chars": truth["chars"],
                "est_tokens": truth["est_tokens"],
                "unchanged": pmfixture.digest(pmfixture.read_tree(project)) == truth["sha256"],
            },
            "exit_code": code,
            "timed_out": timed_out,
            "is_error": bool(envelope.get("is_error")) or code != 0,
            "seconds": seconds,
            "num_turns": envelope.get("num_turns"),
            "envelope": envelope,
            "stderr": (stderr or "")[-2000:],
            "result": result,
            "answer": answer,
            "grades": grades,
            "usage": {**total, "prompt_tokens": prompt_tokens(total)},
            "usage_by_model": {m: {**u, "prompt_tokens": prompt_tokens(u)} for m, u in by_model.items()},
            "claude_sessions": sessions,
            "transcript": stats,
            "work": str(work) if opts.keep_work else None,
        }
        if not envelope:
            record["stdout"] = (stdout or "")[-4000:]
        (run_dir / "run.json").write_text(json.dumps(record, ensure_ascii=False, indent=2))
        return record
    finally:
        if not opts.keep_work:
            shutil.rmtree(work, ignore_errors=True)


def run_all(opts, configs, sizes, seeds, tasks):
    """Every call of configs × sizes × seeds × tasks, `opts.jobs` at a time.
    Returns the records in a fixed order."""
    spec = pmfixture.load_spec(opts.spec_path)
    sections = prompt_sections()
    fixtures = prepare_fixtures(spec, sizes, seeds, opts.out)
    jobs = [(c, z, s, t) for z in sizes for s in seeds for c in configs for t in tasks]
    lock = threading.Lock()
    done = [0]

    def one(job):
        c, z, s, t = job
        d, truth = fixtures[(z, s)]
        rec = run_call(opts, sections, c, z, s, t, d, truth)
        with lock:
            done[0] += 1
            g = rec["grades"]
            score = f"recall {g['recall']:.0f}%" if t == "audit" else f"value {g['value_pct']:.0f}%"
            if g.get("ideas"):
                score += f", omitted enumerated {g['ideas']['enumerated_pct']:.0f}%"
            opts.log(
                f"[{done[0]}/{len(jobs)}] {rec['variant']} · {z} · seed {s} · {t}: {score}, "
                f"invented {g['invented']}, {rec['seconds']:.1f} s, {rec['usage']['cost_usd']:.3f} USD"
                + (" (error)" if rec["is_error"] else "")
            )
        return rec

    if opts.jobs <= 1:
        return [one(j) for j in jobs]
    with ThreadPoolExecutor(max_workers=opts.jobs) as ex:
        return list(ex.map(one, jobs))


def load_runs(out):
    """Every `runs/*/run.json` of a results folder (this run's and earlier ones')."""
    recs = []
    for p in sorted((Path(out) / "runs").glob("*/run.json")):
        try:
            recs.append(json.loads(p.read_text()))
        except json.JSONDecodeError:
            continue
    return recs
