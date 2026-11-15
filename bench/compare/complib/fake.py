"""The made-up agent's rules for dry runs (bench/fake_agent.py reads them from
BENCH_SCENARIO). Built here from the scenario's own files, never frozen, and
copied only into dry-run containers.

They make a dry run exercise every path of the harness without a model: a work
turn that asks instead of acting (the fixed reply), code written on the day-3
and day-4 turns, and memory that matters: on day 3 the made-up agent writes the
current refund rule only if it "knows" the day-2 change (from this session,
CLAUDE.md, or what a SessionStart hook prints, e.g. kontext's distilled notes);
otherwise it writes the old 48-hour rule, which the hidden check rejects and
the confusion probe catches.
"""

import json
import tomllib


def _s(v):
    return json.dumps(v, ensure_ascii=False)


def to_toml(fake, memory):
    out = []
    for k, v in fake.items():
        if not isinstance(v, list):
            out.append(f"{k} = {_s(v)}")
    out.append("")
    for m in memory:
        out.append("[[memory]]")
        out += [f"{k} = {_s(v)}" for k, v in m.items()]
        out.append("")
    for r in fake.get("rules", []):
        out.append("[[fake.rules]]")
        for k, v in r.items():
            if k != "write":
                out.append(f"{k} = {_s(v)}")
        for w in r.get("write", []):
            out.append("[[fake.rules.write]]")
            out += [f"{k} = {_s(v)}" for k, v in w.items()]
        out.append("")
    text = "[fake]\n" + "\n".join(out)
    tomllib.loads(text)
    return text


def note(path, line):
    return {"path": path, "mode": "append", "content": f"\n# {line}\n"}


def build(sc):
    """The fake.toml text for scenario `sc`."""
    rules, memory = [], []
    fake = {"compact_keeps": 0}
    if sc.task == "smoke":
        memory.append({"id": "codename", "class": "B", "said": "this project's release codename is Blue Heron, and releases always go out on a Thursday"})
        rules.append({"when": "farewell", "reply": "Added farewell() with a test.",
                      "write": [{"path": "greet.py", "mode": "append",
                                 "content": '\n\ndef farewell(name):\n    return f"Goodbye, {name}"\n'}]})
    elif sc.task == "S1":
        fixed = (sc.repo_dir / "invoices.py").read_text().replace("days=days - 1", "days=days")
        rules.append({"when": "due date on invoices", "reply": "Fixed due_date().",
                      "write": [{"path": "invoices.py", "content": fixed}]})
    elif sc.task == "S2":
        fixed = (sc.repo_dir / "money.py").read_text().replace("ROUND_HALF_DOWN", "ROUND_HALF_UP")
        rules.append({"when": "partial refunds", "reply": "Fixed round_half() in money.py.",
                      "write": [{"path": "money.py", "content": fixed}]})
    elif sc.task == "L":
        reqs = tomllib.loads((sc.path / "requirements.toml").read_text())["requirement"]
        for r in reqs:
            m = {"id": r["id"], "class": r["class"], "said": r.get("clause") or r["said"]}
            if r.get("changed"):
                m["changed"] = r["changed"]
            memory.append(m)
        ref = sc.path / "reference" / "workshop"
        good = (ref / "bookings.py").read_text()
        mutants = {m.get("confusion"): m for m in tomllib.loads((sc.path / "mutants.toml").read_text())["mutant"]}
        m48 = mutants["test_c03_r06_refund_48h"]
        old = good.replace(m48["old"], m48["new"])
        knows_24 = next(r for r in reqs if r["id"] == "R6")["changed"]
        # Day 1, first turn: asks instead of acting; the fixed reply makes it act.
        rules.append({"when": "short docstring that says what it represents",
                      "reply": "Before I change anything: should the docstrings follow Google style or plain sentences?"})
        rules.append({"when": "best judgement", "reply": "Done.",
                      "write": [note("workshop/models.py", "docstrings: plain sentences")]})
        for when, path in [("teachers()", "workshop/storage.py"), ("classes_by_teacher", "workshop/schedule.py"),
                           ("write the file safely", "workshop/storage.py"), ("--version", "workshop/cli.py"),
                           ("add-class", "workshop/cli.py"), ("show <class_id>", "workshop/cli.py"),
                           ("--json", "workshop/cli.py"), ("Commands", "README.md")]:
            rules.append({"when": when, "reply": "Done.", "write": [note(path, f"made-up change for: {when}")]})
        rules.append({"when": "class listing comes out in the wrong order", "reply": "Fixed the order.",
                      "write": [{"path": "workshop/schedule.py",
                                 "content": (sc.repo_dir / "workshop/schedule.py").read_text().replace("key=lambda c: c.title", "key=lambda c: c.start")}]})
        rules.append({"when": "implement `cancel()`", "if_known": [knows_24], "reply": "Implemented with the 24-hour rule.",
                      "write": [{"path": "workshop/bookings.py", "content": good}]})
        rules.append({"when": "implement `cancel()`", "reply": "Implemented with the 48-hour rule.",
                      "write": [{"path": "workshop/bookings.py", "content": old}]})
        rules.append({"when": "reminders_due()", "reply": "Implemented.",
                      "write": [{"path": "workshop/notify.py", "content": (ref / "notify.py").read_text()}]})
        rules.append({"when": "export_csv()", "reply": "Implemented.",
                      "write": [{"path": "workshop/export.py", "content": (ref / "export.py").read_text()}]})
        rules.append({"when": "implement `book()`", "reply": "Implemented.",
                      "write": [note("workshop/bookings.py", "book() and price_for() reviewed")]})
    fake["rules"] = rules
    return to_toml(fake, memory)
