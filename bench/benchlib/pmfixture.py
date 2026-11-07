"""Fixtures of the reader benchmark: a project memory folder `.pm/` generated
from `bench/reader/spec.toml`, a size and a seed, and its ground truth.

    .pm/dumps/<date>.md   the user's brain-dumps, verbatim, with pasted material
    .pm/items.md          the numbered list of ideas with their state
    .pm/status.md         a short "where we are"

The same `(spec, size, seed)` always gives the same bytes. The seed decides
which ideas `items.md` omits, which changed ideas it carries with the old value,
whether the discarded ones are absent or listed as discarded, the paraphrase of
each item, the order of the user's paragraphs, where the pasted material falls
between them, and which distractors it carries and where.

The ground truth (`truth.json`) is written next to the folder the reader gets,
never inside it. Its line numbers are found in the files once they are written.
"""

import hashlib
import json
import random
import re
import tomllib
from pathlib import Path

from . import fill
from .scenario import BENCH_DIR

SPEC_PATH = BENCH_DIR / "reader" / "spec.toml"

# Characters per token assumed for the fixtures. Not measured: ~4 is the usual
# figure for English prose, and the filler (numbers, folios, names) is probably
# denser (`fill.CHARS_PER_TOKEN` = 2.1 was measured on its Spanish version). The
# report shows the input tokens the envelopes give, to recalibrate after the pilot.
CHARS_PER_TOKEN = 4.0
# Target tokens of the whole `.pm/` folder.
SIZES = {"s": 10_000, "m": 50_000, "l": 150_000}
# Share of the pasted material that goes into each dump, in date order.
DUMP_WEIGHTS = (0.5, 0.3, 0.2)
IDEA_KINDS = ("clear", "changed", "passing", "discarded")
# States of a listed idea that the user has not discarded.
LIVE_STATES = ("pending", "pending", "pending", "in progress", "done")
# What every planted sentence is, in the ground truth.
SENTENCE_KINDS = ("said", "change", "discard", "distractor", "item")

STATUS = [
    "Updated {updated}. Crumb is at the mockup stage. Rosa likes the pre-order flow and the "
    "product pages; the next step is a clickable mockup of ordering and pickup, then a week of "
    "testing it with five regulars.",
    "Updated {updated}. We are still deciding what goes into the first version of Crumb. The "
    "photo session for the product pages is being arranged, and after it comes a clickable "
    "mockup to show Rosa and two of the cafés.",
    "Updated {updated}. The list of ideas is getting long, so the plan is to pick the smallest "
    "first version Rosa can use at the counter, mock it up and try it for a week.",
]

PLACEHOLDER = re.compile(r"\{(said|change|discard):([a-z0-9-]+)\}")


def size_chars(size):
    if size not in SIZES:
        raise ValueError(f"unknown size {size!r} (there are {', '.join(SIZES)})")
    return int(SIZES[size] * CHARS_PER_TOKEN)


def load_spec(path=SPEC_PATH):
    with open(path, "rb") as f:
        spec = tomllib.load(f)
    validate(spec)
    return spec


def validate(spec):
    problems = []
    ideas = {}
    for i in spec.get("ideas", []):
        if i.get("id") in ideas:
            problems.append(f"idea {i.get('id')!r} is repeated")
        ideas[i.get("id")] = i
        kind = i.get("kind")
        if kind not in IDEA_KINDS:
            problems.append(f"idea {i.get('id')}: kind {kind!r} is not one of {', '.join(IDEA_KINDS)}")
        if kind == "changed":
            for k in ("change", "items_old", "items_new"):
                if not i.get(k):
                    problems.append(f"idea {i['id']}: a changed idea needs `{k}`")
            if len(i.get("items_old", [])) != len(i.get("items_new", [])):
                problems.append(f"idea {i['id']}: `items_old` and `items_new` need the same length")
        else:
            if not i.get("items"):
                problems.append(f"idea {i.get('id')}: needs `items` (its paraphrases in items.md)")
        if kind == "discarded" and not i.get("discard"):
            problems.append(f"idea {i['id']}: a discarded idea needs `discard`")
        for k in ("said", "change", "discard"):
            s = i.get(k)
            if s and ("\n" in s or "  " in s or s != s.strip()):
                problems.append(f"idea {i['id']}: `{k}` has to be one line with single spaces")
    used = {}
    for d in spec.get("dumps", []):
        for p in d.get("paragraphs", []):
            if "\n" in p:
                problems.append(f"dump {d.get('date')}: a paragraph has a line break")
            for what, ident in PLACEHOLDER.findall(p):
                used[(what, ident)] = used.get((what, ident), 0) + 1
                if ident not in ideas:
                    problems.append(f"dump {d.get('date')}: {{{what}:{ident}}} is not an idea")
                elif not ideas[ident].get(what):
                    problems.append(f"dump {d.get('date')}: idea {ident} has no `{what}`")
    for ident, i in ideas.items():
        for what in ("said", "change", "discard"):
            if i.get(what) and used.get((what, ident), 0) != 1:
                problems.append(f"idea {ident}: {{{what}:{ident}}} has to be used exactly once in the dumps")
    if len(spec.get("dumps", [])) != len(DUMP_WEIGHTS):
        problems.append(f"there have to be {len(DUMP_WEIGHTS)} dumps")
    if not [i for i in ideas.values() if i.get("kind") == "passing"]:
        problems.append("there has to be at least one `passing` idea")
    if not [i for i in ideas.values() if i.get("kind") == "changed"]:
        problems.append("there has to be at least one `changed` idea")
    dids = [d.get("id") for d in spec.get("distractors", [])]
    if len(set(dids)) != len(dids):
        problems.append("distractor ids are repeated")
    for d in spec.get("distractors", []):
        if d.get("near") and d["near"] not in ideas:
            problems.append(f"distractor {d['id']}: `near` {d['near']!r} is not an idea")
        if d.get("text", "").endswith("."):
            problems.append(f"distractor {d['id']}: no final period (the filler carrier adds it)")
    for q in spec.get("questions", []):
        if q.get("idea") not in ideas:
            problems.append(f"question {q.get('id')}: idea {q.get('idea')!r} does not exist")
        if not q.get("expected"):
            problems.append(f"question {q.get('id')}: needs `expected`")
    if not spec.get("paste_intros"):
        problems.append("needs `paste_intros`")
    if problems:
        raise ValueError("invalid reader spec:\n  " + "\n  ".join(problems))


def dump_path(d):
    return f".pm/dumps/{d['date']}.md"


def render_paragraph(p, ideas):
    return PLACEHOLDER.sub(lambda m: ideas[m.group(2)][m.group(1)], p)


def build(spec, size, seed):
    """The files of the fixture (relative path -> text) and the plan: what the
    seed decided and which sentences were planted, still without line numbers."""
    rng = random.Random(f"kontext-reader:{spec['name']}:{size}:{seed}")
    ideas = {i["id"]: i for i in spec["ideas"]}
    order = [i["id"] for i in spec["ideas"]]
    by_kind = {k: [x for x in order if ideas[x]["kind"] == k] for k in IDEA_KINDS}

    # Which distractors this fixture carries.
    always = [d for d in spec["distractors"] if d.get("always")]
    optional = [d for d in spec["distractors"] if not d.get("always")]
    k = min(int(spec.get("optional_count", 0)), len(optional))
    distractors = always + sorted(rng.sample(optional, k), key=spec["distractors"].index)

    # What items.md leaves out: 2 or 3 live ideas, at least one said in passing.
    # The changed ideas are always listed, so one of them can carry its old value.
    first = rng.choice(by_kind["passing"])
    pool = [x for x in by_kind["clear"] + by_kind["passing"] if x != first]
    omitted = {first, *rng.sample(pool, rng.choice((2, 3)) - 1)}
    stale = set(rng.sample(by_kind["changed"], rng.randint(1, len(by_kind["changed"]))))
    discarded_listed = {x: rng.random() < 0.5 for x in by_kind["discarded"]}

    # The dumps: the user's paragraphs (the first one stays first) and the pasted
    # material between them.
    dumps = []
    for d in spec["dumps"]:
        paras = [render_paragraph(p, ideas) for p in d["paragraphs"]]
        rest = paras[1:]
        rng.shuffle(rest)
        dumps.append([paras[0], *rest])
    user_chars = sum(len(p) + 2 for ps in dumps for p in ps)
    filler_total = max(3000, size_chars(size) - user_chars - 2500)
    chunks = []  # (dump index, gap after paragraph number, chars)
    for di, ps in enumerate(dumps):
        n = len(ps)
        gaps = sorted(rng.sample(range(1, n + 1), rng.randint(min(2, n), n)))
        weights = [rng.uniform(0.5, 1.5) for _ in gaps]
        budget = filler_total * DUMP_WEIGHTS[di]
        for g, w in zip(gaps, weights):
            chunks.append({"dump": di, "gap": g, "chars": max(600, int(budget * w / sum(weights)))})
    for c in chunks:
        c["docs"] = rng.choice(fill.DOCS)
        c["intro"] = rng.choice(spec["paste_intros"])
        c["distractors"] = []
    for d in distractors:
        rng.choice(chunks)["distractors"].append(d)

    files = {}
    for di, (d, ps) in enumerate(zip(spec["dumps"], dumps)):
        out = [f"# Brain-dump, {d['date']}", ""]
        for pi, p in enumerate(ps, 1):
            out += [p, ""]
            for ci, c in enumerate(chunks):
                if c["dump"] != di or c["gap"] != pi:
                    continue
                text = fill.generate(
                    c["docs"],
                    c["chars"],
                    f"reader:{spec['name']}:{size}:{seed}:{di}:{ci}",
                    [x["text"] for x in c["distractors"]],
                )
                out += [c["intro"], "", text, "", "---", ""]
        files[dump_path(d)] = "\n".join(out).rstrip("\n") + "\n"

    # items.md: the listed ideas in the order the dumps first say them.
    first_seen = {}
    for d, ps in zip(spec["dumps"], dumps):
        for p in ps:
            for x in order:
                if ideas[x]["said"] in p:
                    first_seen.setdefault(x, (spec["dumps"].index(d), ps.index(p)))
    items = []
    for x in sorted(order, key=lambda x: first_seen[x]):
        i = ideas[x]
        if x in omitted or (i["kind"] == "discarded" and not discarded_listed[x]):
            continue
        if i["kind"] == "changed":
            j = rng.randrange(len(i["items_old"]))
            text = (i["items_old"] if x in stale else i["items_new"])[j]
        else:
            text = rng.choice(i["items"])
        state = "discarded" if i["kind"] == "discarded" else rng.choice(LIVE_STATES)
        items.append({"idea": x, "text": text, "state": state})
    lines = [
        "# Items",
        "",
        "The user's ideas from the dumps, numbered. State: pending, in progress, done or discarded.",
        "",
    ]
    lines += [f"- #{n} · {it['state']} · {it['text']}" for n, it in enumerate(items, 1)]
    files[".pm/items.md"] = "\n".join(lines) + "\n"
    files[".pm/status.md"] = "# Status\n\n" + rng.choice(STATUS).format(updated=spec["updated"]) + "\n"

    planted = []
    for x in order:
        for what in ("said", "change", "discard"):
            if ideas[x].get(what):
                planted.append({"kind": what, "idea": x, "text": ideas[x][what]})
    for d in distractors:
        planted.append({"kind": "distractor", "distractor": d["id"], "near": d.get("near"), "text": d["text"]})
    for it in items:
        planted.append({"kind": "item", "idea": it["idea"], "text": it["text"], "state": it["state"]})

    listed = {it["idea"]: it for it in items}
    state = {}
    for x in order:
        i = ideas[x]
        if i["kind"] == "discarded":
            state[x] = "discarded"
        elif x in omitted:
            state[x] = "omitted"
        else:
            state[x] = "listed"
    plan = {
        "ideas": {
            x: {
                "kind": ideas[x]["kind"],
                "state": state[x],
                "in_items": x in listed,
                "items_state": listed[x]["state"] if x in listed else None,
                "stale": x in stale if ideas[x]["kind"] == "changed" else None,
            }
            for x in order
        },
        "distractors": [d["id"] for d in distractors],
        "planted": planted,
    }
    return files, plan


def locate(files, planted):
    """The final line range (1-based, inclusive) of each planted sentence in the
    written files. Each one has to appear exactly once in the whole fixture."""
    lines = {p: t.split("\n") for p, t in files.items()}
    out = []
    for s in planted:
        hits = [(p, n) for p, ls in lines.items() for n, l in enumerate(ls, 1) if s["text"] in l]
        if len(hits) != 1:
            raise ValueError(f"planted sentence found {len(hits)} times, expected once: {s['text']!r}")
        p, n = hits[0]
        out.append({**s, "file": p, "lines": [n, n]})
    return out


def digest(files):
    h = hashlib.sha256()
    for p in sorted(files):
        h.update(p.encode() + b"\0" + files[p].encode() + b"\0")
    return h.hexdigest()


def read_tree(root):
    """The fixture's files as {relative path: text}, from a folder that holds `.pm/`."""
    root = Path(root)
    return {
        str(f.relative_to(root)): f.read_text()
        for f in sorted((root / ".pm").rglob("*"))
        if f.is_file()
    }


def write(spec, size, seed, dest):
    """Writes the fixture to `dest/root/.pm/` and its truth to `dest/truth.json`
    (outside `root/`, the folder the reader gets). Returns the truth."""
    dest = Path(dest)
    root = dest / "root"
    files, plan = build(spec, size, seed)
    for rel, text in files.items():
        p = root / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(text)
    # Lines come from the files as written, not from the plan.
    written = read_tree(root)
    sentences = locate(written, plan["planted"])
    chars = sum(len(t) for t in written.values())
    truth = {
        "spec": spec["name"],
        "size": size,
        "seed": seed,
        "sha256": digest(written),
        "chars": chars,
        "chars_per_token": CHARS_PER_TOKEN,
        "est_tokens": round(chars / CHARS_PER_TOKEN),
        "files": {
            p: {"chars": len(t), "lines": t.count("\n"), "sha256": hashlib.sha256(t.encode()).hexdigest()}
            for p, t in written.items()
        },
        "ideas": plan["ideas"],
        "omitted": [x for x, i in plan["ideas"].items() if i["state"] == "omitted"],
        "listed": [x for x, i in plan["ideas"].items() if i["state"] == "listed"],
        "discarded": [x for x, i in plan["ideas"].items() if i["state"] == "discarded"],
        "distractors": plan["distractors"],
        "sentences": sentences,
        "questions": spec.get("questions", []),
    }
    (dest / "truth.json").write_text(json.dumps(truth, ensure_ascii=False, indent=2) + "\n")
    return truth
