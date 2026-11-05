"""Prompt cache breaks in Claude Code sessions.

Claude Code sends the whole conversation in every request, and Anthropic
charges cheaply (0.1×) for what was already in the cache and dearly (1.25×) for
what it writes anew. As long as nothing changes what is already in the
conversation, each request reads from the cache everything the previous one
had (what it read plus what it created). If it reads much less, something
rewrote the prefix: that is a **break**, and what it stopped reading gets
written again.

The requests of the main session are walked in order (one per `message.id`;
Claude Code writes one line per block of the same response), without the
sidechains (subagents). A request breaks the cache if it reads more than
`THRESHOLD` tokens less than the previous request's prefix. A
`compact_boundary` starts from zero: compacting changes the conversation on
purpose.

Also counted apart (`idle_breaks`, `idle_tokens`, already included in `breaks`
and `tokens_rewritten`) are the ones that arrive more than `IDLE_SECONDS` after
the previous request: the 5-minute cache expired, and nobody rewrote anything.
In the big run, the two most expensive kontext-pointer breaks were like that,
after 13 and 18 minutes of waiting.

It is the logic of `/tmp/cache_breaks.py`, which measured the big run, except
that the `compact_boundary` is recognized by its record
(`subtype == "compact_boundary"`) and not by the text of the line, so that a
message that mentions it does not restart the count.
"""

import gzip
import json
from datetime import datetime
from pathlib import Path

THRESHOLD = 2000
IDLE_SECONDS = 300
KEYS = ("requests", "breaks", "tokens_rewritten", "idle_breaks", "idle_tokens")


def _open(path):
    path = Path(path)
    if path.suffix == ".gz":
        return gzip.open(path, "rt", errors="replace")
    return open(path, errors="replace")


def _ts(x):
    try:
        return datetime.fromisoformat(str(x.get("timestamp")).replace("Z", "+00:00"))
    except ValueError:
        return None


def session_breaks(lines):
    """`{requests, breaks, tokens_rewritten, idle_breaks, idle_tokens}` for the
    lines of one session."""
    out = dict.fromkeys(KEYS, 0)
    prev = None
    prev_ts = None
    seen = set()
    for line in lines:
        if "compact_boundary" not in line and '"assistant"' not in line:
            continue
        try:
            x = json.loads(line)
        except json.JSONDecodeError:
            continue
        if not isinstance(x, dict):
            continue
        if x.get("subtype") == "compact_boundary":
            prev = None
            continue
        msg = x.get("message") or {}
        if x.get("type") != "assistant" or not isinstance(msg, dict) or "usage" not in msg:
            continue
        mid = msg.get("id")
        if mid in seen:
            continue
        seen.add(mid)
        if x.get("isSidechain"):
            continue
        u = msg.get("usage") or {}
        cr = u.get("cache_read_input_tokens", 0) or 0
        cc = u.get("cache_creation_input_tokens", 0) or 0
        ts = _ts(x)
        out["requests"] += 1
        if prev is not None and cr + THRESHOLD < prev:
            out["breaks"] += 1
            out["tokens_rewritten"] += prev - cr
            if ts and prev_ts and (ts - prev_ts).total_seconds() > IDLE_SECONDS:
                out["idle_breaks"] += 1
                out["idle_tokens"] += prev - cr
        prev = cr + cc
        prev_ts = ts
    return out


def breaks(paths):
    """The same, summed over several sessions (`.jsonl` or `.jsonl.gz`)."""

    total = dict.fromkeys(KEYS, 0)
    for p in paths:
        with _open(p) as f:
            for k, v in session_breaks(f).items():
                total[k] += v
    return total
