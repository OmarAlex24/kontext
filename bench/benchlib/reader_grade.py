"""Grading of the reader benchmark: deterministic, no model judge.

A citation is `{quote, file, line}`. It is valid if its quote, normalized (case,
accents, whitespace, quotes and markdown punctuation), has at least
`MIN_QUOTE_CHARS` characters and is inside lines [line - 2, line + 2] of the
cited file. Anything else is an invented citation. A valid citation maps to the
planted sentence of the ground truth whose range it overlaps (± 2 lines) and
whose text it shares (one contains the other, or most of the words).

The v2 audit answer also lists every idea the reader found (`ideas`); its grade
is a diagnostic, never an input of the gate: whether each omitted idea was
enumerated at all and, if it was, whether it was marked as present in items.md.

The gate is fixed here and in bench/README.md before any real run. It is not
changed after seeing results.
"""

import json
import re
from pathlib import Path

from .metrics import keyword_verdict, normalize

MIN_QUOTE_CHARS = 15
LINE_SLACK = 2
# A quote that is not a substring of a planted sentence (nor the other way
# round) still maps to it if it shares this share of the shorter one's words.
WORD_OVERLAP = 0.6

# ------------------------------------------------------------------ the gate
#
# At size GATE_SIZE, mean over the seeds, against GATE_BASELINE: a delegated
# configuration `opus+X` is viable if
#   1. audit recall  >= baseline audit recall - GATE_RECALL_DROP points,
#   2. value %       >= baseline value %      - GATE_VALUE_DROP points,
#   3. invented citations per run (one call: one task of one seed, both tasks
#      counted) <= GATE_MAX_INVENTED, on average,
#   4. discarded ideas reported as missing, in total over the seeds,
#      <= GATE_MAX_DISCARDED.
# If the baseline's own audit recall is below GATE_SANITY_RECALL, the task is
# ill-posed and no configuration is judged. Size `l` is reported, not gated.
GATE_SIZE = "m"
GATE_BASELINE = "opus"
GATE_RECALL_DROP = 10.0
GATE_VALUE_DROP = 10.0
GATE_MAX_INVENTED = 1.0
GATE_MAX_DISCARDED = 1
GATE_SANITY_RECALL = 80.0

FENCE_START = re.compile(r"^```[a-zA-Z]*\s*")
FENCE_END = re.compile(r"\s*```\s*$")
ELLIPSIS = re.compile(r"\.\.\.|…")


def extract_json(text):
    """The JSON object of a reply, even inside ``` or with text around it (as
    `parse_distill_json` in src/distill.rs). Returns (object, None) or (None, error)."""
    if not isinstance(text, str) or not text.strip():
        return None, "empty reply"
    t = FENCE_END.sub("", FENCE_START.sub("", text.strip()))
    a, b = t.find("{"), t.rfind("}")
    if a < 0 or b <= a:
        return None, "the reply has no JSON object"
    try:
        v = json.loads(t[a : b + 1])
    except json.JSONDecodeError as e:
        return None, f"invalid JSON ({e})"
    if not isinstance(v, dict):
        return None, "the reply is not a JSON object"
    return v, None


def norm_text(s):
    """Lowercase, without accents, numbers in one form (metrics.normalize), and
    only letters and digits separated by single spaces: quotes, markdown and
    punctuation don't count."""
    s = normalize(str(s))
    s = re.sub(r"['’‘`´]", "", s)  # don't -> dont, on both sides alike
    s = re.sub(r"[^0-9a-z]+", " ", s)
    return s.strip()


def words(s):
    return {w for w in norm_text(s).split() if len(w) >= 3}


def norm_path(p):
    """`.pm/...` from whatever the reader wrote (absolute, `./.pm/...`, `dumps/...`)."""
    p = str(p or "").strip().replace("\\", "/")
    i = p.find(".pm/")
    if i >= 0:
        return p[i:]
    p = p.lstrip("./")
    if p.startswith("dumps/") or p in ("items.md", "status.md"):
        return ".pm/" + p
    return p


def as_line(v):
    if isinstance(v, bool):
        return None
    if isinstance(v, int):
        return v
    m = re.match(r"\s*L?(\d+)", str(v or ""))
    return int(m.group(1)) if m else None


class Fixture:
    """The fixture's files (read from the folder that holds `.pm/`) and its truth."""

    def __init__(self, root, truth):
        self.root = Path(root)
        self.truth = truth
        self._lines = {}

    def lines(self, rel):
        if rel not in self.truth["files"]:
            return None
        if rel not in self._lines:
            self._lines[rel] = (self.root / rel).read_text().split("\n")
        return self._lines[rel]


def check_citation(fx, c):
    """{valid, reason, file, line}: whether the quote is in the cited place."""
    if not isinstance(c, dict):
        return {"valid": False, "reason": "not an object"}
    quote, rel, line = c.get("quote"), norm_path(c.get("file")), as_line(c.get("line"))
    out = {"file": rel, "line": line}
    if not isinstance(quote, str) or not quote.strip() or not rel or line is None:
        return {**out, "valid": False, "reason": "incomplete"}
    pieces = [norm_text(x) for x in ELLIPSIS.split(quote)]
    pieces = [x for x in pieces if x]
    if sum(len(x) for x in pieces) < MIN_QUOTE_CHARS:
        return {**out, "valid": False, "reason": "too short"}
    lines = fx.lines(rel)
    if lines is None:
        return {**out, "valid": False, "reason": "no such file"}
    lo, hi = max(1, line - LINE_SLACK), min(len(lines), line + LINE_SLACK)
    window = norm_text(" ".join(lines[lo - 1 : hi])) if lo <= hi else ""
    if found_in_order(pieces, window):
        return {**out, "valid": True, "reason": None}
    whole = norm_text(" ".join(lines))
    reason = "wrong line" if found_in_order(pieces, whole) else "not in the file"
    return {**out, "valid": False, "reason": reason}


def found_in_order(pieces, text):
    at = 0
    for p in pieces:
        i = text.find(p, at)
        if i < 0:
            return False
        at = i + len(p)
    return True


def text_match(quote, sentence):
    q, s = norm_text(quote), norm_text(sentence)
    if q and s and (q in s or s in q):
        return 1.0
    qw, sw = words(quote), words(sentence)
    if not qw or not sw:
        return 0.0
    return len(qw & sw) / min(len(qw), len(sw))


def map_citation(fx, c, check):
    """The planted sentence a valid citation points at, or None."""
    best = None
    for s in fx.truth["sentences"]:
        if s["file"] != check["file"]:
            continue
        a, b = s["lines"]
        if check["line"] + LINE_SLACK < a or check["line"] - LINE_SLACK > b:
            continue
        score = text_match(c["quote"], s["text"])
        if score < WORD_OVERLAP:
            continue
        key = (score, -abs(check["line"] - a))
        if best is None or key > best[0]:
            best = (key, s)
    return best[1] if best else None


def classify_audit(fx, s):
    """tp, listed, discarded, distractor or other."""
    if s is None:
        return "other", None
    if s["kind"] == "distractor":
        return "distractor", s["distractor"]
    idea = s["idea"]
    state = fx.truth["ideas"][idea]["state"]
    if s["kind"] == "discard" or state == "discarded":
        return "discarded", idea
    if state == "omitted":
        return "tp", idea
    return "listed", idea


def grade_audit(fx, answer):
    """Recall, precision, invented citations and confusions of an audit answer."""
    omitted = fx.truth["omitted"]
    items = answer.get("missing") if isinstance(answer, dict) else None
    if not isinstance(items, list):
        return {
            "parsed": False,
            "recall": 0.0,
            "precision": None,
            "valid": 0,
            "invented": 0,
            "confusions": 0,
            "discarded": 0,
            "distractors": 0,
            "listed": 0,
            "other": 0,
            "found": [],
            "missed": list(omitted),
            "citations": [],
        }
    cites = []
    targets = {}  # deduped valid citations: target -> class
    for c in items:
        chk = check_citation(fx, c)
        rec = {"citation": c, **chk}
        if chk["valid"]:
            s = map_citation(fx, c, chk)
            cls, ident = classify_audit(fx, s)
            rec.update(cls=cls, target=ident, sentence=s["kind"] if s else None)
            key = (cls, ident) if ident else ("other", chk["file"], norm_text(c.get("quote")))
            targets.setdefault(key, cls)
        cites.append(rec)
    found = sorted({k[1] for k, cls in targets.items() if cls == "tp"})

    def count(cls):
        return sum(1 for v in targets.values() if v == cls)

    return {
        "parsed": True,
        "recall": 100.0 * len(found) / len(omitted) if omitted else 100.0,
        "precision": 100.0 * len(found) / len(targets) if targets else None,
        "valid": len(targets),
        "invented": sum(1 for r in cites if not r["valid"]),
        "confusions": count("discarded") + count("distractor"),
        "discarded": count("discarded"),
        "distractors": count("distractor"),
        "listed": count("listed"),
        "other": count("other"),
        "found": found,
        "missed": [x for x in omitted if x not in found],
        "citations": cites,
    }


def truthy(v):
    return v is True or (isinstance(v, str) and v.strip().lower() == "true")


def grade_ideas(fx, answer):
    """The diagnostic of a v2 audit's `ideas` list (not part of the gate). Per
    omitted idea: `not enumerated` (no valid entry of `ideas` points at one of
    its sentences), `marked present` (every entry that does says `in_items:
    true`: a matching error) or `marked missing`; and whether it made it to
    `missing`. Also how many of all the user's ideas were enumerated, by kind."""
    omitted = fx.truth["omitted"]
    items = answer.get("ideas") if isinstance(answer, dict) else None
    out = {
        "parsed": isinstance(items, list),
        "entries": 0,
        "invalid": 0,
        "unmapped": 0,
        "omitted": {},
        "enumerated_pct": 0.0,
        "marked_present": 0,
        "not_enumerated": 0,
        "coverage_pct": 0.0,
        "by_kind": {},
    }
    marks = {}  # idea -> [in_items of each entry]
    for c in items if out["parsed"] else []:
        out["entries"] += 1
        chk = check_citation(fx, c)
        if not chk["valid"]:
            out["invalid"] += 1
            continue
        s = map_citation(fx, c, chk)
        if s is None or not s.get("idea") or s["kind"] not in ("said", "change", "discard"):
            out["unmapped"] += 1
            continue
        marks.setdefault(s["idea"], []).append(truthy(c.get("in_items")))
    reported = set(grade_audit(fx, answer)["found"]) if out["parsed"] else set()
    for x in omitted:
        m = marks.get(x)
        status = "not enumerated" if not m else ("marked present" if all(m) else "marked missing")
        out["omitted"][x] = {"status": status, "reported": x in reported}
    n = len(omitted)
    enumerated = [x for x, v in out["omitted"].items() if v["status"] != "not enumerated"]
    out["enumerated_pct"] = 100.0 * len(enumerated) / n if n else 100.0
    out["marked_present"] = sum(1 for v in out["omitted"].values() if v["status"] == "marked present")
    out["not_enumerated"] = n - len(enumerated)
    ideas = fx.truth["ideas"]
    out["coverage_pct"] = 100.0 * len(set(marks) & set(ideas)) / len(ideas) if ideas else 0.0
    for x, i in ideas.items():
        k = out["by_kind"].setdefault(i["kind"], [0, 0])
        k[0] += 1 if x in marks else 0
        k[1] += 1
    return out


def in_force(fx, idea):
    """The planted sentence that sets an idea's value in force: its last change,
    or its own sentence if it never changed."""
    ss = [s for s in fx.truth["sentences"] if s.get("idea") == idea and s["kind"] in ("said", "change")]
    return max(ss, key=lambda s: s.get("step") or 0) if ss else None


def grade_value(fx, answer, questions):
    """Per question, `keyword_verdict` on `value` (a forbidden word is a
    confusion) and whether its citation is valid; value % is the mean score."""
    items = answer.get("answers") if isinstance(answer, dict) else None
    parsed = isinstance(items, list)
    by_id = {}
    for a in items if parsed else []:
        if isinstance(a, dict):
            by_id.setdefault(str(a.get("id")).strip().lower(), a)
    out = []
    for q in questions:
        a = by_id.get(q["id"].strip().lower())
        if a is None:
            out.append({"id": q["id"], "answered": False, "score": 0.0, "forbidden_hits": [],
                        "confused": False, "citation_valid": False, "invented": False})
            continue
        value = a.get("value")
        value = value if isinstance(value, str) else json.dumps(value)
        score, hits = keyword_verdict(value, q["expected"], q.get("forbidden", []))
        chk = check_citation(fx, a)
        s = map_citation(fx, a, chk) if chk["valid"] else None
        out.append(
            {
                "id": q["id"],
                "answered": True,
                "value": value,
                "score": score,
                "forbidden_hits": hits,
                "confused": bool(hits),
                "citation_valid": chk["valid"],
                "citation_reason": chk["reason"],
                "invented": not chk["valid"],
                "cites": ({"kind": s["kind"], "idea": s.get("idea"), "distractor": s.get("distractor")} if s else None),
                "cites_idea": bool(s and s.get("idea") == q["idea"] and s["kind"] in ("said", "change")),
                "cites_in_force": bool(s and s == in_force(fx, q["idea"])),
            }
        )
    n = len(questions)
    return {
        "parsed": parsed,
        "value_pct": 100.0 * sum(x["score"] for x in out) / n if n else 0.0,
        "confusions": sum(1 for x in out if x["confused"]),
        "invented": sum(1 for x in out if x["invented"]),
        "citations_valid": sum(1 for x in out if x["citation_valid"]),
        "answered": sum(1 for x in out if x["answered"]),
        "questions": out,
    }


def grade(fx, task, text, prompt="v1"):
    """Extracts the JSON from the reply text and grades it. An unparseable
    reply scores 0 and is flagged (`parse_error`). A v2 audit also gets the
    `ideas` diagnostic; its `missing` is graded exactly as in v1."""
    answer, err = extract_json(text)
    if task == "audit":
        g = grade_audit(fx, answer)
        if prompt != "v1":
            g["ideas"] = grade_ideas(fx, answer)
    elif task == "value":
        g = grade_value(fx, answer, fx.truth["questions"])
    else:
        raise ValueError(f"unknown task {task!r}")
    if err is None and not g["parsed"]:
        err = "the JSON does not have the shape asked for"
    return answer, {**g, "parse_error": err}
