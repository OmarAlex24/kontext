"""Scenarios: `bench/scenarios/<name>/scenario.toml` and its `repo/`."""

import re
import tomllib
from dataclasses import dataclass, field
from pathlib import Path

BENCH_DIR = Path(__file__).resolve().parent.parent
SCENARIOS_DIR = BENCH_DIR / "scenarios"


# Classes of a memory question: A (recorded before day 1), B (said in passing
# in the conversation), C (a fact that changed; the one in force counts), cross
# (joins several facts). The `[[probes]]` without a class stay at "?".
PROBE_CLASSES = ("A", "B", "C", "cross", "?")
TURN_KINDS = ("work", "fill", "compact", "probe")


@dataclass
class Probe:
    day: int  # 1-based
    question: str
    expected: list[str]
    forbidden: list[str] = field(default_factory=list)
    cls: str = "?"
    checkpoint: int | None = None  # by default, the day
    id: str | None = None
    note: str | None = None  # context for the judge (what changed, what is a distractor)

    @property
    def point(self):
        return self.checkpoint if self.checkpoint is not None else self.day


@dataclass
class Turn:
    text: str = ""
    temptation: str | None = None  # "decision:<id>" o "scope"
    correction: str | None = None
    kind: str = "work"  # work | fill | compact | probe
    # fill: a generated document (benchlib/fill.py) and a request about it
    tokens: int = 0
    seed: int = 0
    docs: str = "mixed"
    distractors: list[str] = field(default_factory=list)
    # probe: a memory question at this point of the day
    probe: Probe | None = None


@dataclass
class Scenario:
    name: str
    path: Path  # the scenario folder
    data: dict
    charter: dict
    goals: list[dict]
    decisions: list[dict]
    days: list[list[Turn]]
    probes: list[Probe]
    ignore: list[str]
    acceptance: dict = field(default_factory=dict)
    # `[[memory]]`: the facts that are measured (id, class, said, changed)
    memory: list[dict] = field(default_factory=list)
    # `[[distractors]]`: id -> sentence almost identical to a fact, for the filler
    distractors: dict = field(default_factory=dict)
    # `[bench]`: distill (turn on kontext's distill in the kontext variants), etc.
    bench: dict = field(default_factory=dict)

    @property
    def distill(self):
        return bool(self.bench.get("distill", False))

    def all_probes(self):
        """The questions in order: the ones in the turns and the `[[probes]]`
        (these last, at the end of their day)."""
        out = []
        for d, day in enumerate(self.days, 1):
            out += [t.probe for t in day if t.kind == "probe"]
            out += [p for p in self.probes if p.day == d]
        return out

    @property
    def toml_path(self):
        return self.path / "scenario.toml"

    @property
    def repo_dir(self):
        return self.path / "repo"

    def decision(self, ident):
        for d in self.decisions:
            if d["id"] == ident:
                return d
        raise KeyError(f"the scenario has no decision {ident!r}")


def resolve(name_or_path):
    p = Path(name_or_path)
    if p.is_file():
        return p.parent
    if p.is_dir() and (p / "scenario.toml").exists():
        return p
    q = SCENARIOS_DIR / name_or_path
    if (q / "scenario.toml").exists():
        return q
    raise FileNotFoundError(f"no scenario {name_or_path!r} (looked in {q})")


def probe_of(p, day):
    return Probe(
        day=day,
        question=p["question"],
        expected=list(p.get("expected", [])),
        forbidden=list(p.get("forbidden", [])),
        cls=p.get("class", "?"),
        checkpoint=p.get("checkpoint"),
        id=p.get("id"),
        note=p.get("note"),
    )


def turn_of(t, day):
    kind = t.get("kind", "work")
    turn = Turn(
        text=t.get("text", t.get("ask", "")),
        temptation=t.get("temptation"),
        correction=t.get("correction"),
        kind=kind,
        tokens=int(t.get("tokens", 0)),
        seed=int(t.get("seed", 0)),
        docs=t.get("docs", "mixed"),
        distractors=list(t.get("distractors", [])),
    )
    if kind == "compact":
        turn.text = "/compact"
    if kind == "probe":
        turn.probe = probe_of(t, day)
        turn.text = turn.probe.question
    return turn


def load(name_or_path):
    path = resolve(name_or_path)
    with open(path / "scenario.toml", "rb") as f:
        data = tomllib.load(f)
    days = []
    for i, d in enumerate(data.get("days", []), 1):
        days.append([turn_of(t, i) for t in d.get("turns", [])])
    probes = [probe_of(p, int(p["day"])) for p in data.get("probes", [])]
    sc = Scenario(
        name=data.get("name", path.name),
        path=path,
        data=data,
        charter=data["charter"],
        goals=data.get("goals", []),
        decisions=data.get("decisions", []),
        days=days,
        probes=probes,
        ignore=data.get("scope", {}).get("ignore", []),
        acceptance=data.get("acceptance", {}),
        memory=data.get("memory", []),
        distractors={d["id"]: d["text"] for d in data.get("distractors", [])},
        bench=data.get("bench", {}),
    )
    validate(sc)
    return sc


def validate(sc):
    problems = []
    ids = {d["id"] for d in sc.decisions}
    from .fill import DOCS

    turn_texts = "\n".join(t.text for day in sc.days for t in day if t.kind == "work")
    for i, day in enumerate(sc.days, 1):
        for t in day:
            if t.kind not in TURN_KINDS:
                problems.append(f"day {i}: unknown turn kind {t.kind!r}")
            if t.kind == "fill":
                if t.tokens <= 0:
                    problems.append(f"day {i}: fill without `tokens`")
                if t.docs not in DOCS:
                    problems.append(f"day {i}: fill {t.docs!r} unknown (there are {', '.join(DOCS)})")
                if not t.text.strip():
                    problems.append(f"day {i}: fill without `ask` (what is asked about the text)")
                for x in t.distractors:
                    if x not in sc.distractors:
                        problems.append(f"day {i}: the fill uses the distractor {x!r}, which is not in [[distractors]]")
            if t.temptation is None:
                continue
            if t.temptation == "scope":
                continue
            kind, _, ident = t.temptation.partition(":")
            if kind != "decision" or ident not in ids:
                problems.append(f"day {i}: invalid temptation {t.temptation!r}")
    for p in sc.all_probes():
        if not 1 <= p.day <= len(sc.days):
            problems.append(f"question {p.question!r}: day {p.day} does not exist")
        if not p.expected:
            problems.append(f"question {p.question!r}: no expected words")
        if p.cls not in PROBE_CLASSES:
            problems.append(f"question {p.question!r}: class {p.cls!r} (there are {', '.join(PROBE_CLASSES)})")
    probe_ids = [p.id for p in sc.all_probes() if p.id]
    for dup in sorted({i for i in probe_ids if probe_ids.count(i) > 1}):
        problems.append(f"question {dup!r}: repeated id")
    for m in sc.memory:
        for k in ("said", "changed"):
            if m.get(k) and m[k] not in turn_texts:
                problems.append(f"fact {m.get('id')}: `{k}` does not appear word for word in any turn")
    for d in sc.decisions:
        if not d.get("guard"):
            problems.append(f"decision {d['id']}: no guard")
    for g in sc.goals:
        if not g.get("scope"):
            problems.append(f"goal {g['id']}: no `scope`")
    if not sc.repo_dir.is_dir():
        problems.append(f"missing {sc.repo_dir}")
    if problems:
        raise ValueError("invalid scenario:\n  " + "\n  ".join(problems))


def glob_regex(glob):
    """A path glob with `**` as a regex: `src/**/*.py`, `tests/test_*.py`."""

    out = []
    i = 0
    while i < len(glob):
        c = glob[i]
        if glob.startswith("**/", i):
            out.append("(?:.*/)?")
            i += 3
        elif glob.startswith("**", i):
            out.append(".*")
            i += 2
        elif c == "*":
            out.append("[^/]*")
            i += 1
        elif c == "?":
            out.append("[^/]")
            i += 1
        else:
            out.append(re.escape(c))
            i += 1
    return re.compile("^" + "".join(out) + "$")


def matches(path, globs):
    return any(glob_regex(g).match(path) for g in globs)
