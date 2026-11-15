"""Scenarios of the comparison: `bench/compare/scenarios/<name>/script.toml`,
its `repo/` (the fixture) and its `acceptance/` (the hidden checks)."""

import sys
import tomllib
from dataclasses import dataclass, field
from pathlib import Path

COMPARE_DIR = Path(__file__).resolve().parent.parent
SCENARIOS_DIR = COMPARE_DIR / "scenarios"
sys.path.insert(0, str(COMPARE_DIR.parent))

from benchlib import fill  # noqa: E402

TURN_KINDS = ("work", "fill", "compact", "probe")
TASKS = {"smoke": "smoke", "S1": "small-due-date", "S2": "small-rounding", "L": "project-workshop"}


@dataclass
class Turn:
    kind: str
    text: str = ""
    tokens: int = 0
    docs: str = "mixed"
    seed: int = 0
    id: str | None = None
    expected: list = field(default_factory=list)

    def prompt(self, fill_scale=1.0):
        """The bytes sent to the agent (a filler document is generated here)."""
        if self.kind == "compact":
            return "/compact"
        if self.kind == "fill":
            return fill.prompt(self.text, self.docs, self.tokens, self.seed, [], fill_scale)[0]
        return self.text


@dataclass
class Day:
    day: int
    machine: str
    turns: list


@dataclass
class Scenario:
    name: str
    task: str
    path: Path
    data: dict
    days: list
    acceptance: dict

    @property
    def repo_dir(self):
        return self.path / "repo"

    @property
    def checks_dir(self):
        return self.path / "acceptance"

    @property
    def machines(self):
        return sorted({d.machine for d in self.days})

    def requirements(self):
        """L only: [{id, class, test, day, scored_on, ...}] from requirements.toml."""
        rel = self.acceptance.get("requirements")
        if not rel:
            return []
        reqs = tomllib.loads((self.path / rel).read_text())["requirement"]
        scored = self.acceptance.get("scored_on", {})
        for r in reqs:
            r["scored_on"] = scored.get(r["id"])
        return reqs

    def confusions(self):
        p = self.path / "confusions.toml"
        return tomllib.loads(p.read_text()) if p.exists() else {}

    def check_modules(self):
        mods = list(self.acceptance.get("modules", []))
        if self.acceptance.get("confusions"):
            mods.append(self.acceptance["confusions"])
        return mods

    def calls(self):
        return sum(len(d.turns) for d in self.days)


def load(name_or_task):
    name = TASKS.get(name_or_task, name_or_task)
    path = SCENARIOS_DIR / name
    data = tomllib.loads((path / "script.toml").read_text())
    days = []
    for i, d in enumerate(data["days"], 1):
        turns = []
        for t in d["turns"]:
            if t["kind"] not in TURN_KINDS:
                raise ValueError(f"{name}: unknown turn kind {t['kind']!r}")
            turns.append(
                Turn(
                    kind=t["kind"],
                    text=t.get("text", ""),
                    tokens=int(t.get("tokens", 0)),
                    docs=t.get("docs", "mixed"),
                    seed=int(t.get("seed", 0)),
                    id=t.get("id"),
                    expected=list(t.get("expected", [])),
                )
            )
        days.append(Day(day=int(d.get("day", i)), machine=d.get("machine", "A"), turns=turns))
    return Scenario(
        name=data["name"], task=data["task"], path=path, data=data, days=days, acceptance=data.get("acceptance", {})
    )
