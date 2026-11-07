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
of it is in `.pm/items.md`. For the values, the change sentence wins over the
first one.

With `--agents` defining a subagent and its tool in `--tools`, it "delegates":
the main session calls the subagent tool once and the reads happen in the
subagent's transcript. It writes the transcripts like Claude Code
(`$CLAUDE_CONFIG_DIR/projects/<key>/<session>.jsonl` and, for the subagent,
`<session>/subagents/agent-<id>.jsonl`), so the counting code runs end to end.

Knobs, for the tests (environment variables):

- `FAKE_READER_MISS=<n>`: leaves out the first n missing ideas.
- `FAKE_READER_INVENT=1`: adds a citation whose quote is not in the file.
- `FAKE_READER_DISCARD=1`: reports a discarded idea as missing.
- `FAKE_READER_DISTRACT=1`: reports a distractor (another project's idea) as missing.
- `FAKE_READER_STALE=1`: answers the values of the changed ideas with the old value.
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


def audit(spec, folder):
    missing = []
    for i in spec["ideas"]:
        c = cite(folder, i["said"])
        if not c or (i.get("discard") and folder.find(i["discard"])):
            continue
        paraphrases = i.get("items", []) + i.get("items_old", []) + i.get("items_new", [])
        if any(p in folder.items for p in paraphrases):
            continue
        missing.append(c)
    miss = int(knob("MISS") or 0)
    missing = missing[miss:]
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
    return {"missing": missing}


def values(spec, folder):
    ideas = {i["id"]: i for i in spec["ideas"]}
    answers = []
    for q in spec.get("questions", []):
        i = ideas[q["idea"]]
        changed = i.get("change") and folder.find(i["change"]) and not knob("STALE")
        c = cite(folder, i["change"] if changed else i["said"]) or {"quote": "", "file": "", "line": 0}
        answers.append({"id": q["id"], "value": i.get("value_new" if changed else "value", ""), **c})
    return {"answers": answers}


def tool_use(name, inp):
    return {"type": "tool_use", "id": f"toolu_{uuid.uuid4().hex[:24]}", "name": name, "input": inp}


def assistant(model, content, sidechain=False, agent=None):
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
    return r


def write_jsonl(path, records):
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "w") as f:
        for r in records:
            f.write(json.dumps(r, ensure_ascii=False) + "\n")


def reads(folder, model, sidechain=False, agent=None):
    files = [*folder.dumps, folder.root / ".pm" / "items.md"]
    return [assistant(model, [tool_use("Read", {"file_path": str(p)})], sidechain, agent) for p in files]


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
    if delegate:
        sub_model = (agents.get(sub_name) or {}).get("model") or main_model

    if '{"missing"' in prompt:
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
            main_recs.append(assistant(main_model, [call]))
            if knob("BYPASS"):
                main_recs += reads(folder, main_model)
            sub_recs = [{"type": "user", "isSidechain": True, "agentId": agent,
                         "message": {"role": "user", "content": prompt}}]
            sub_recs += reads(folder, sub_model, True, agent)
            sub_recs.append(assistant(sub_model, [{"type": "text", "text": text}], True, agent))
            write_jsonl(tdir / sid / "subagents" / f"agent-{agent}.jsonl", sub_recs)
        else:
            main_recs += reads(folder, main_model)
        main_recs.append(assistant(main_model, [{"type": "text", "text": text}]))
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
