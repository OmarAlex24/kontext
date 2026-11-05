#!/usr/bin/env python3
"""A made-up agent to run the harness in dry runs (--dry-run).

It is invoked like `claude -p`: the turn arrives on stdin, it accepts `--resume <id>`,
`--model`, `--output-format json` and `--permission-mode` (it ignores them except
`--resume`), and it prints a JSON envelope with the same shape as Claude Code's
(`result`, `session_id`, `usage`, `total_cost_usd` at 0).

It uses no model. It follows the scenario's `[[fake.rules]]` (`BENCH_SCENARIO`
points at its scenario.toml): the first one whose `when` appears in the turn (and
whose `if_known` phrases it knows all) applies; with `unless_known`, if any of
those words is in what the agent "knows", it refuses and writes nothing. What it
knows is the same as a real agent would know without searching the code:

- what was said in this session since the last compaction (the previous turns;
  not the current one). `/compact` forgets everything, except the last
  `[fake] compact_keeps` turns;
- the repo's CLAUDE.md, if it exists (`notes` variant). With CLAUDE.md, it notes
  there the `[[memory]]` sentences of the scenario that it hears, as an agent
  that keeps its notes would;
- what the SessionStart hook in `.claude/settings.json` prints, which runs on
  every call (`startup`, `resume` or `compact`, as in Claude Code). As in Claude
  Code, what each call adds is added to the earlier calls of the same session (on
  a `resume`, kontext only adds what is new) and `/compact` clears it. If the hook
  only gives a pointer to `kontext resume`, it follows it and runs that command.

Like Claude Code, it runs the PreCompact hooks (on `/compact`) and SessionEnd (when
each call ends) with the path of its transcript, which it writes to
`$CLAUDE_CONFIG_DIR/projects/<repo>/<session>.jsonl` in the same shape (and a
`compact_boundary` when compacting). That way kontext's distill really runs.

If no rule applies, it looks in `[[fake.answers]]` (the first one whose `when` is
in the question; of its `options`, the first whose `known` phrases it knows all),
and otherwise it answers with the lines of what it knows that share words with the
question (that is how it answers the memory questions).

It is deterministic. An odd `BENCH_SEED` adds a comment line to what it writes, so
the seeds don't come out identical.
"""

import json
import os
import re
import subprocess
import sys
import time
import tomllib
import unicodedata
import uuid
from pathlib import Path


def norm(s):
    s = unicodedata.normalize("NFKD", s.lower())
    return "".join(c for c in s if not unicodedata.combining(c))


STOP = set(
    "about above after again against because before being below between both during "
    "each from further have having here into more most other over same should some such "
    "than that their them then there these they this those through under until very what "
    "when where which while whom with would your find code project".split()
)


def words(s):
    return {w for w in re.findall(r"[a-z0-9_]+", norm(s)) if len(w) >= 4 and w not in STOP}


def parse_args(argv):
    resume = None
    i = 0
    while i < len(argv):
        a = argv[i]
        if a in ("--resume", "-r") and i + 1 < len(argv):
            resume = argv[i + 1]
            i += 2
            continue
        if a in ("--model", "--output-format", "--permission-mode") and i + 1 < len(argv):
            i += 2
            continue
        i += 1
    return resume


def state_dir():
    base = os.environ.get("BENCH_FAKE_STATE") or os.path.join(
        os.environ.get("TMPDIR", "/tmp"), "bench-fake"
    )
    p = Path(base)
    p.mkdir(parents=True, exist_ok=True)
    return p


def project_key(repo):
    return re.sub(r"[^A-Za-z0-9]", "-", str(Path(repo).resolve()))


def transcript_path(repo, sid):
    base = os.environ.get("CLAUDE_CONFIG_DIR")
    if not base:
        return None
    p = Path(base) / "projects" / project_key(repo) / f"{sid}.jsonl"
    p.parent.mkdir(parents=True, exist_ok=True)
    return p


def append_transcript(tp, records):
    if not tp:
        return
    with open(tp, "a") as f:
        for r in records:
            f.write(json.dumps(r, ensure_ascii=False) + "\n")


def run_hooks(repo, event, payload):
    """Runs the `event` hooks of `.claude/settings.json` with `payload` on
    stdin. Returns their outputs."""
    settings = repo / ".claude" / "settings.json"
    if not settings.exists():
        return []
    try:
        data = json.loads(settings.read_text())
    except json.JSONDecodeError:
        return []
    outs = []
    for group in data.get("hooks", {}).get(event, []):
        for h in group.get("hooks", []):
            cmd = h.get("command")
            if not cmd:
                continue
            body = json.dumps({"hook_event_name": event, "cwd": str(repo), **payload})
            r = subprocess.run(
                cmd, shell=True, input=body, capture_output=True, text=True, cwd=repo, timeout=60
            )
            outs.append((cmd, r.stdout))
    return outs


def hook_context(repo, source="startup", sid=None, tp=None):
    """What the SessionStart hook would print (and, if it is a pointer, `kontext resume`)."""
    out = []
    payload = {"source": source, "session_id": sid, "transcript_path": str(tp) if tp else None}
    for cmd, text in run_hooks(repo, "SessionStart", payload):
        try:
            text = json.loads(text)["hookSpecificOutput"]["additionalContext"]
        except (json.JSONDecodeError, KeyError, TypeError):
            pass
        out.append(text)
        # Pointer mode: the hook says what to run; an obedient agent runs it.
        m = re.search(r"`(kontext resume --as \S+?)`", text)
        if m:
            exe = cmd.split(" hook ")[0].strip()
            role = m.group(1).split("--as", 1)[1].strip()
            r = subprocess.run(
                f"{exe} resume --as {role} --peek",
                shell=True,
                capture_output=True,
                text=True,
                cwd=repo,
                timeout=60,
            )
            out.append(r.stdout)
    return "\n".join(out)


def load_fake():
    path = os.environ.get("BENCH_SCENARIO")
    if not path:
        return {}, []
    with open(path, "rb") as f:
        data = tomllib.load(f)
    memory = [m[k] for m in data.get("memory", []) for k in ("said", "changed") if m.get(k)]
    return data.get("fake", {}), memory


def any_in(whens, text_n):
    whens = [whens] if isinstance(whens, str) else whens
    return any(norm(w) in text_n for w in whens)


def answer_from(answers, prompt_n, known_n):
    for a in answers:
        if not any_in(a["when"], prompt_n):
            continue
        for o in a.get("options", []):
            if all(norm(k) in known_n for k in o.get("known", [])):
                return o["reply"]
        return a.get("default", "I don't know.")
    return None


def apply_writes(repo, rule, prompt_n, seed, log):
    written = []
    for w in rule.get("write", []):
        if w.get("only_if") and norm(w["only_if"]) not in prompt_n:
            continue
        p = repo / w["path"]
        if w.get("if_missing") and p.exists():
            continue
        p.parent.mkdir(parents=True, exist_ok=True)
        content = w["content"]
        if seed % 2 == 1 and not written:
            content = content.rstrip("\n") + f"\n# seed {seed}\n"
        if w.get("mode") == "append" and p.exists():
            with p.open("a") as f:
                f.write(content)
        else:
            p.write_text(content)
        written.append(w["path"])
    if written:
        log.append({"wrote": written})
    return written


def now():
    return time.strftime("%Y-%m-%dT%H:%M:%S.000Z", time.gmtime())


def main():
    resume = parse_args(sys.argv[1:])
    prompt = sys.stdin.read()
    repo = Path.cwd()
    seed = int(os.environ.get("BENCH_SEED", "0") or 0)
    sd = state_dir()
    fake, memory = load_fake()

    if resume and (sd / f"{resume}.json").exists():
        session = json.loads((sd / f"{resume}.json").read_text())
        source = "resume"
    else:
        session = {"id": str(uuid.uuid4()), "context": "", "turns": []}
        source = "startup"
    sid = session["id"]
    tp = transcript_path(repo, sid)
    # Like Claude Code: the start hook runs on every call and what it prints
    # stays in the conversation (on a `resume`, kontext only adds what is new).
    added = hook_context(repo, source, sid, tp)
    session["context"] = "\n".join(x for x in (session.get("context", ""), added) if x.strip())

    reply = None
    log = []
    notes_path = repo / "CLAUDE.md"
    if prompt.strip() == "/compact":
        pre = len(json.dumps(session["turns"])) // 4
        run_hooks(repo, "PreCompact", {"trigger": "manual", "session_id": sid, "transcript_path": str(tp) if tp else None})
        append_transcript(
            tp,
            [
                {"type": "system", "subtype": "compact_boundary", "timestamp": now(),
                 "compactMetadata": {"trigger": "manual", "preTokens": pre}},
                {"type": "user", "isCompactSummary": True, "timestamp": now(),
                 "message": {"role": "user", "content": "(made-up summary: it keeps nothing)"}},
            ],
        )
        # The made-up agent forgets what was said before compacting, except the
        # last `[fake] compact_keeps` turns (a summary keeps the recent ones,
        # which in a filler scenario is the filler).
        keep = int(fake.get("compact_keeps", 0))
        session["turns"] = session["turns"][-keep:] if keep else []
        # Compacting also takes away what the hook printed: it puts it back.
        session["context"] = hook_context(repo, "compact", sid, tp)
        reply = ""
        log.append({"compacted": True, "pre_tokens": pre})
    else:
        notes = notes_path.read_text() if notes_path.exists() else ""
        known = "\n".join([session["context"], notes] + [t["prompt"] for t in session["turns"]])
        known_n = norm(known)
        prompt_n = norm(prompt)
        for rule in fake.get("rules", []):
            if norm(rule["when"]) not in prompt_n:
                continue
            if not all(norm(k) in known_n for k in rule.get("if_known", [])):
                continue
            blockers = [k for k in rule.get("unless_known", []) if norm(k) in known_n]
            if blockers:
                reply = rule.get("refuse", "I won't do it: it contradicts what we already agreed.")
                log.append({"refused": rule["when"], "because": blockers})
            else:
                apply_writes(repo, rule, prompt_n, seed, log)
                reply = rule.get("reply", "Done.")
                if notes and log:
                    with notes_path.open("a") as f:
                        f.write(f"\n- Done: {reply}\n")
            break
        if reply is None:
            reply = answer_from(fake.get("answers", []), prompt_n, known_n)
        if reply is None:
            # A question: what it knows that shares words with it.
            q = words(prompt)
            lines = [l.strip(" -#*") for l in re.split(r"\n|(?<=\.)\s", known) if l.strip()]
            hits = [l for l in lines if q & words(l)]
            reply = " ".join(dict.fromkeys(hits[:6])) or "I don't know."
        if notes_path.exists():
            # It keeps its notes: what it hears from [[memory]] goes in them.
            notes = notes_path.read_text()
            new = [m for m in memory if norm(m) in prompt_n and norm(m) not in norm(notes)]
            if new:
                with notes_path.open("a") as f:
                    f.write("".join(f"\n- {m}" for m in new) + "\n")
                log.append({"noted": len(new)})
        session["turns"].append({"prompt": prompt, "reply": reply, "log": log})
        append_transcript(
            tp,
            [
                {"type": "user", "timestamp": now(), "message": {"role": "user", "content": prompt}},
                # With `id` and `usage` (at zero) like Claude Code's, so that the
                # prompt cache measure counts its requests.
                {"type": "assistant", "timestamp": now(),
                 "message": {"id": f"msg_{uuid.uuid4().hex}", "role": "assistant",
                             "content": [{"type": "text", "text": reply}],
                             "usage": {"input_tokens": 0, "output_tokens": 0,
                                       "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0}}},
            ],
        )

    (sd / f"{sid}.json").write_text(json.dumps(session, ensure_ascii=False))
    # When each `claude -p` ends, the session ends: SessionEnd runs.
    run_hooks(repo, "SessionEnd", {"reason": "other", "session_id": sid, "transcript_path": str(tp) if tp else None})
    tokens_in = len(prompt.split()) + len(session["context"].split())
    print(
        json.dumps(
            {
                "type": "result",
                "subtype": "success",
                "is_error": False,
                "result": reply,
                "session_id": sid,
                "num_turns": 1,
                "total_cost_usd": 0,
                "usage": {
                    "input_tokens": 0,
                    "output_tokens": 0,
                    "cache_creation_input_tokens": 0,
                    "cache_read_input_tokens": 0,
                },
                "fake": {"log": log, "known_words": tokens_in},
            },
            ensure_ascii=False,
        )
    )


if __name__ == "__main__":
    main()
