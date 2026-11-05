#!/usr/bin/env python3
"""A made-up distill model, to run the harness in dry runs.

`kontext distill` sends it the prompt on stdin (with the piece of transcript
between BEGIN/END and the list of the entries in force with their id). It
answers with the envelope of `claude -p --output-format json`: one fact said by
the user for each sentence of the scenario's `[[memory]]` (`said` or `changed`)
that appears word for word in the piece. If it is a `changed` and the `said`
sentence of the same fact is in force, it says it replaces it (`supersedes`). It
does not see the distractors (they are not in `[[memory]]`) and invents nothing,
not even dates (the prompt asks not to turn "already in force" into a date; `when`
is empty and kontext uses the time of the entry). It is deterministic and costs
nothing.
"""


import json
import os
import re
import sys
import tomllib
import unicodedata


def norm(s):
    s = unicodedata.normalize("NFKD", s.lower())
    return " ".join("".join(c for c in s if not unicodedata.combining(c)).split())


def main():
    prompt = sys.stdin.read()
    begin, end = "BEGIN TRANSCRIPT EXCERPT", "END TRANSCRIPT EXCERPT"
    head, _, rest = prompt.partition(begin)
    excerpt_n = norm(rest.rsplit(end, 1)[0])
    # The entries in force: "- <id> [<type>] <title>".
    active = {}
    for line in head.splitlines():
        m = re.match(r"- (\S+) \[[^\]]+\] (.*)$", line.strip())
        if m:
            active[norm(m.group(2))] = m.group(1)
    facts = []
    path = os.environ.get("BENCH_SCENARIO")
    if path:
        with open(path, "rb") as f:
            memory = tomllib.load(f).get("memory", [])
        for m in memory:
            for k in ("said", "changed"):
                phrase = m.get(k)
                if not phrase or norm(phrase) not in excerpt_n:
                    continue
                item = {"text": phrase[0].upper() + phrase[1:], "source": "user", "when": "", "tags": ["dato"]}
                old = active.get(norm(m.get("said", "")))
                if k == "changed" and old:
                    item["supersedes"] = old
                facts.append(item)
    reply = {"decisions": [], "questions": [], "facts": facts, "journal": []}
    print(
        json.dumps(
            {
                "type": "result",
                "subtype": "success",
                "is_error": False,
                "result": json.dumps(reply, ensure_ascii=False),
                "session_id": "fake-distill",
                "total_cost_usd": 0,
                "usage": {
                    "input_tokens": 0,
                    "output_tokens": 0,
                    "cache_creation_input_tokens": 0,
                    "cache_read_input_tokens": 0,
                },
            },
            ensure_ascii=False,
        )
    )


if __name__ == "__main__":
    main()
