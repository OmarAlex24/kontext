"""The fixed-reply rule (design §2.2), the same for every arm. Frozen with the
scripts (FREEZE.json): never tuned after a real run.

A work turn whose agent changed no code (tool-state paths aside) and whose
reply asks something instead (a question, or a request for approval or a
signature) gets FIXED_REPLY, at most MAX_PER_TURN times, then the script moves
on. Each reply is an intervention, counted by kind.
"""

import re

FIXED_REPLY = "Use your best judgement with what I've told you so far, and go ahead."
MAX_PER_TURN = 2

# Paths that are a tool's own state, not the code: a change there alone (an
# OpenSpec proposal, a Beads issue, a kontext note) is not "doing the work".
TOOL_PATHS = (
    ".git/",
    ".claude/",
    "CLAUDE.md",
    "AGENTS.md",
    ".beads/",
    ".engram/",
    "openspec/",
    ".kontext",
    ".pm/",
    ".mem0",
    "__pycache__/",
)

APPROVAL = re.compile(
    r"\b(approve|approval|sign[- ]?off|sign the|signature|your (?:ok|okay|go-ahead|decision)|"
    r"needs? your decision|confirm (?:that|whether|if|the|before)|please confirm|"
    r"shall I (?:proceed|go ahead|start|continue)|should I (?:proceed|go ahead|start|continue)|"
    r"before I (?:proceed|start|continue|implement)|waiting for (?:your )?(?:approval|confirmation))\b",
    re.I,
)
QUESTION = re.compile(
    r"(\?\s*$)|\b(should I|shall I|do you want|would you like|want me to|let me know|"
    r"which (?:option|approach|one)|can you (?:confirm|clarify|tell me)|could you (?:confirm|clarify))\b",
    re.I | re.M,
)


def tail(text, chars=1200):
    """The end of the reply, where an agent asks."""
    return (text or "").strip()[-chars:]


def asks(text):
    """None, "approval" or "question"."""
    t = tail(text)
    if not t:
        return None
    if APPROVAL.search(t):
        return "approval"
    if QUESTION.search(t):
        return "question"
    return None


def needs_reply(turn_kind, changed, reply_text, is_error=False):
    """The kind of intervention due, or None."""
    if turn_kind != "work" or changed or is_error:
        return None
    return asks(reply_text)


def is_tool_path(path):
    p = path
    while p.startswith("./"):
        p = p[2:]
    return any(p == t.rstrip("/") or p.startswith(t) for t in TOOL_PATHS)
