#!/usr/bin/env python3
"""A made-up reader for the reader benchmark's dry runs (bench/reader.py --dry-run).

It is invoked like `claude -p`: the prompt arrives on stdin; it accepts the same
flags (`--model`, `--tools`, `--agents`, `--effort`, `--output-format`,
`--setting-sources`, `--strict-mcp-config`) and ignores them except `--model`
and `--agents`, and it prints an envelope of the same shape as Claude Code's,
with usage and cost at 0 (`modelUsage` carries the main model and, when it
delegates, the subagent's).

It uses no model and never reads the ground truth. It reads the spec
(`$BENCH_READER_SPEC`, by default bench/reader/spec.toml) and the fixture in
the working folder: it looks for the spec's idea sentences in `.pm/dumps/` and
answers from what it finds. For the audit, an idea is missing if its sentence is
in a dump, it has no discard sentence there, and none of the spec's paraphrases
of it is in `.pm/items.md`. When the prompt asks for the v2 answer (`{"ideas"`),
it also lists every idea sentence it found, with `in_items` and `discarded`. For
the values, the last of an idea's sentences found in the dumps (its own, then
each change) gives the value in force, from the idea's `values`.

Effort: the assistant records carry `effort` like Claude Code's. The main
session's is `--effort` (default `medium`); the subagent's is the `effort` of
its `--agents` entry, else the session's; `CLAUDE_CODE_EFFORT_LEVEL`, when set,
overrides both, as it does in Claude Code.

With `--agents` defining a subagent and its tool in `--tools`, it "delegates":
the main session calls the subagent tool once and the reads happen in the
subagent's transcript. It writes the transcripts like Claude Code
(`$CLAUDE_CONFIG_DIR/projects/<key>/<session>.jsonl` and, for the subagent,
`<session>/subagents/agent-<id>.jsonl`), so the counting code runs end to end.

Knobs, for the tests (environment variables):

- `FAKE_READER_MISS=<n>`: leaves out the first n missing ideas (in v2, from the
  `ideas` list too: never enumerated).
- `FAKE_READER_MARK_PRESENT=1`: v2 only; enumerates the first missing idea but
  marks it `in_items: true` and leaves it out of `missing` (a matching error).
- `FAKE_READER_INVENT=1`: adds a citation whose quote is not in the file.
- `FAKE_READER_DISCARD=1`: reports a discarded idea as missing.
- `FAKE_READER_DISTRACT=1`: reports a distractor (another project's idea) as missing.
- `FAKE_READER_STALE=1`: answers the values of the changed ideas with the value
  before the one in force.
- `FAKE_READER_BYPASS=1`: when delegating, the main session also reads the dumps.
- `FAKE_READER_GARBLE=1`: replies with prose and no JSON.
"""

import json
import os
import re
import sys
import tomllib
import uuid
from pathlib import Path

SPEC = Path(__file__).resolve().parent / "reader" / "spec.toml"
VALUE_FLAGS = ("--model", "--tools", "--agents", "--effort", "--output-format", "--setting-sources")
INVENTED = "Customers can tip the baker from the app after every order."


def parse_args(argv):
    opts = {}
    i = 0
    while i < len(argv):
        a = argv[i]
        if a in VALUE_FLAGS and i + 1 < len(argv):
            opts[a[2:]] = argv[i + 1]
            i += 2
            continue
        i += 1
    return opts


def knob(name):
    return os.environ.get(f"FAKE_READER_{name}", "")


def project_key(root):
    return re.sub(r"[^A-Za-z0-9]", "-", str(Path(root).resolve()))


class Folder:
    def __init__(self, root):
        self.root = root
        self.dumps = sorted((root / ".pm" / "dumps").glob("*.md"))
        self.lines = {f".pm/dumps/{p.name}": p.read_text().split("\n") for p in self.dumps}
        items = root / ".pm" / "items.md"
        self.items = items.read_text() if items.exists() else ""

    def find(self, text):
        """(file, line) of the first line of a dump that has `text`, or None."""
        for rel, lines in self.lines.items():
            for n, line in enumerate(lines, 1):
                if text in line:
                    return rel, n
        return None


def cite(folder, text):
    loc = folder.find(text)
    return {"quote": text, "file": loc[0], "line": loc[1]} if loc else None


def changes(idea):
    c = idea.get("change") or []
    return [c] if isinstance(c, str) else list(c)


def audit(spec, folder, v2=False):
    found = []  # (citation, in_items, discarded) of each idea sentence in the dumps
    for i in spec["ideas"]:
        c = cite(folder, i["said"])
        if not c:
            continue
        paraphrases = i.get("items", []) + i.get("items_old", []) + i.get("items_new", [])
        found.append((c, any(p in folder.items for p in paraphrases), bool(i.get("discard") and folder.find(i["discard"]))))
    missing = [c for c, listed, dropped in found if not listed and not dropped]
    miss = int(knob("MISS") or 0)
    gone, missing = missing[:miss], missing[miss:]
    present = missing[:1] if v2 and knob("MARK_PRESENT") else []
    missing = missing[len(present):]
    ideas = [
        {**c, "in_items": listed or c in present, "discarded": dropped}
        for c, listed, dropped in found
        if c not in gone
    ]
    if knob("DISCARD"):
        for i in spec["ideas"]:
            c = cite(folder, i["said"]) if i.get("discard") else None
            if c:
                missing.append(c)
                break
    if knob("DISTRACT"):
        for d in spec["distractors"]:
            c = cite(folder, d["text"])
            if c:
                missing.append(c)
                break
    if knob("INVENT") and folder.lines:
        missing.append({"quote": INVENTED, "file": next(iter(folder.lines)), "line": 3})
    return {"ideas": ideas, "missing": missing} if v2 else {"missing": missing}


def values(spec, folder):
    ideas = {i["id"]: i for i in spec["ideas"]}
    answers = []
    for q in spec.get("questions", []):
        i = ideas[q["idea"]]
        vals = i.get("values") or []
        sentences = [i["said"], *changes(i)]
        # The last sentence found sets the value in force; STALE takes the one before.
        k = max((n for n, t in enumerate(sentences) if folder.find(t)), default=0)
        if knob("STALE") and k > 0:
            k -= 1
        c = cite(folder, sentences[k]) or {"quote": "", "file": "", "line": 0}
        answers.append({"id": q["id"], "value": vals[k] if k < len(vals) else "", **c})
    return {"answers": answers}


def tool_use(name, inp):
    return {"type": "tool_use", "id": f"toolu_{uuid.uuid4().hex[:24]}", "name": name, "input": inp}


def assistant(model, content, sidechain=False, agent=None, effort=None):
    r = {
        "type": "assistant",
        "message": {
            "id": f"msg_{uuid.uuid4().hex}",
            "role": "assistant",
            "model": model,
            "content": content,
            "usage": {"input_tokens": 0, "output_tokens": 0, "cache_read_input_tokens": 0,
                      "cache_creation_input_tokens": 0},
        },
    }
    if sidechain:
        r["isSidechain"] = True
        r["agentId"] = agent
    if effort:
        r["effort"] = effort
    return r


def write_jsonl(path, records):
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "w") as f:
        for r in records:
            f.write(json.dumps(r, ensure_ascii=False) + "\n")


def reads(folder, model, sidechain=False, agent=None, effort=None):
    files = [*folder.dumps, folder.root / ".pm" / "items.md"]
    return [assistant(model, [tool_use("Read", {"file_path": str(p)})], sidechain, agent, effort) for p in files]


def main():
    opts = parse_args(sys.argv[1:])
    prompt = sys.stdin.read()
    root = Path.cwd()
    with open(os.environ.get("BENCH_READER_SPEC") or SPEC, "rb") as f:
        spec = tomllib.load(f)
    folder = Folder(root)
    main_model = opts.get("model") or "fake-model"
    agents = {}
    try:
        agents = json.loads(opts["agents"]) if opts.get("agents") else {}
    except json.JSONDecodeError:
        pass
    tools = [t.strip() for t in re.split(r"[,\s]+", opts.get("tools", "")) if t.strip()]
    sub_name = next(iter(agents), None)
    sub_tool = next((t for t in tools if t not in ("Read", "Grep", "Glob")), None)
    delegate = bool(sub_name and sub_tool)
    sub_model = None
    forced = os.environ.get("CLAUDE_CODE_EFFORT_LEVEL")
    main_effort = forced or opts.get("effort") or "medium"
    sub_effort = None
    if delegate:
        sub = agents.get(sub_name) or {}
        sub_model = sub.get("model") or main_model
        sub_effort = forced or sub.get("effort") or main_effort

    if '{"ideas"' in prompt:
        answer = audit(spec, folder, v2=True)
    elif '{"missing"' in prompt:
        answer = audit(spec, folder)
    elif '{"answers"' in prompt:
        answer = values(spec, folder)
    else:
        answer = {}
    if knob("GARBLE"):
        text = "I read the folder, but I am not sure what to report."
    else:
        text = "Here is what I found:\n```json\n" + json.dumps(answer, ensure_ascii=False, indent=2) + "\n```"

    sid = str(uuid.uuid4())
    base = os.environ.get("CLAUDE_CONFIG_DIR")
    if base:
        tdir = Path(base) / "projects" / project_key(root)
        main_recs = [{"type": "user", "message": {"role": "user", "content": prompt}}]
        if delegate:
            agent = uuid.uuid4().hex[:17]
            call = tool_use(sub_tool, {"subagent_type": sub_name, "description": "Read .pm/", "prompt": prompt})
            main_recs.append(assistant(main_model, [call], effort=main_effort))
            if knob("BYPASS"):
                main_recs += reads(folder, main_model, effort=main_effort)
            sub_recs = [{"type": "user", "isSidechain": True, "agentId": agent,
                         "message": {"role": "user", "content": prompt}}]
            sub_recs += reads(folder, sub_model, True, agent, sub_effort)
            sub_recs.append(assistant(sub_model, [{"type": "text", "text": text}], True, agent, sub_effort))
            write_jsonl(tdir / sid / "subagents" / f"agent-{agent}.jsonl", sub_recs)
        else:
            main_recs += reads(folder, main_model, effort=main_effort)
        main_recs.append(assistant(main_model, [{"type": "text", "text": text}], effort=main_effort))
        write_jsonl(tdir / f"{sid}.jsonl", main_recs)

    zero = {"inputTokens": 0, "outputTokens": 0, "cacheReadInputTokens": 0, "cacheCreationInputTokens": 0,
            "costUSD": 0}
    model_usage = {main_model: dict(zero)}
    if delegate:
        model_usage[sub_model] = dict(zero)
    print(
        json.dumps(
            {
                "type": "result",
                "subtype": "success",
                "is_error": False,
                "result": text,
                "session_id": sid,
                "num_turns": 1,
                "total_cost_usd": 0,
                "usage": {"input_tokens": 0, "output_tokens": 0, "cache_creation_input_tokens": 0,
                          "cache_read_input_tokens": 0},
                "modelUsage": model_usage,
            },
            ensure_ascii=False,
        )
    )


if __name__ == "__main__":
    main()
