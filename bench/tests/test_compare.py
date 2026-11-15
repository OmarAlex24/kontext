"""The comparison benchmark (bench/compare) without Docker: the argv rules
(no bind mount, keys by name only), the fixed-reply rule, secret redaction and
scan, stream parsing, the scenarios and their hidden checks (fixture fails,
reference passes, every mutant is caught), the freeze, the analysis rules,
and that nothing harness-side ships a hidden check into an agent container."""

import gzip
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import tomllib
import unittest
from pathlib import Path

BENCH = Path(__file__).resolve().parent.parent
COMPARE = BENCH / "compare"
sys.path.insert(0, str(BENCH))
sys.path.insert(0, str(COMPARE))

from benchlib import container  # noqa: E402
from complib import analysis, arms, cell, fake, freeze, interventions, keys, script  # noqa: E402

WORKSHOP = COMPARE / "scenarios" / "project-workshop"
CANARY = "sk-ant-canary-0123456789abcdef"
CHECKED = ["book(", "book()", "price_for", "cancel(", "refund_amount", "join_waitlist", "promote_waitlist", "reminders_due", "export_csv"]
TOOL_WORDS = re.compile(r"\b(kontext|mem0|beads|bd|openspec|engram|gentle-ai|gentle|claude|/compact|/mem0|slash|skill)\b", re.I)


def run_checks(pkg_dir, module, mutate=None, extra_files=None):
    """{test: ok|FAIL|ERROR} of an acceptance module against a copy of `pkg_dir`."""
    d = Path(tempfile.mkdtemp())
    try:
        shutil.copytree(pkg_dir, d / "repo")
        if mutate:
            f = d / "repo" / mutate["file"]
            s = f.read_text()
            assert s.count(mutate["old"]) == 1, mutate
            f.write_text(s.replace(mutate["old"], mutate["new"]))
        env = {"PYTHONPATH": str(d / "repo"), "BENCH_REPO": str(d / "repo"), "PYTHONDONTWRITEBYTECODE": "1", "PATH": os.environ.get("PATH", "")}
        r = subprocess.run(
            [sys.executable, "-m", "unittest", "-v", module], cwd=pkg_dir.parent / "acceptance" if (pkg_dir.parent / "acceptance").exists() else extra_files,
            env=env, capture_output=True, text=True, timeout=120,
        )  # fmt: skip
        return container.parse_unittest_verbose(r.stderr)
    finally:
        shutil.rmtree(d, ignore_errors=True)


class Argv(unittest.TestCase):
    def all_argv(self):
        return [
            container.argv_run_machine("m", "bench-plain:latest", "net", "vol", "r", "machine-a", env={"A": "1"}, secret_names=("ANTHROPIC_API_KEY", "MEM0_API_KEY")),
            container.argv_run_sidecar("s", "bench-sidecar:latest", "net", "r"),
            container.argv_run_verify("v", "bench-verify:latest", "r"),
            container.argv_exec("m", ["true"], env={"X": "y"}, secret_names=("KONTEXT_TOKEN",), workdir="/w"),
        ]  # fmt: skip

    def test_no_bind_mount_ever(self):
        for argv in self.all_argv():
            self.assertNotIn("-v", argv)
            self.assertNotIn("--volume", argv)
            for i, a in enumerate(argv):
                if a == "--mount":
                    self.assertIn("type=volume", argv[i + 1])
                    self.assertNotIn("type=bind", argv[i + 1])

    def test_secrets_by_name_only(self):
        argv = self.all_argv()[0]
        self.assertIn("ANTHROPIC_API_KEY", argv)
        self.assertIn("MEM0_API_KEY", argv)
        self.assertFalse(any(a.startswith("ANTHROPIC_API_KEY=") for a in argv))
        with self.assertRaises(ValueError):
            container.argv_exec("m", ["true"], env={"ANTHROPIC_API_KEY": CANARY})
        with self.assertRaises(ValueError):
            container.argv_exec("m", ["true"], env={"MEM0_API_KEY": CANARY})

    def test_machine_keys_go_on_each_exec_not_on_run(self):
        class Rec:
            def __init__(self):
                self.calls = []

            def run(self, argv, input=None, secret_env=None, **k):
                self.calls.append((argv, dict(secret_env or {})))

        d = Rec()
        m = container.Machine(d, "m", "bench-plain:latest", "net", "vol", "r", "machine-a", secret_env={"ANTHROPIC_API_KEY": CANARY})
        m.start()
        m.exec(["true"])
        (run_argv, run_env), (exec_argv, exec_env) = d.calls
        self.assertNotIn("ANTHROPIC_API_KEY", run_argv)
        self.assertEqual(run_env, {})
        self.assertIn("ANTHROPIC_API_KEY", exec_argv)
        self.assertEqual(exec_env, {"ANTHROPIC_API_KEY": CANARY})
        self.assertFalse(any(CANARY in arg for argv, _ in d.calls for arg in argv))

    def test_verify_has_no_network(self):
        argv = container.argv_run_verify("v", "bench-verify:latest", "r")
        self.assertEqual(argv[argv.index("--network") + 1], "none")
        self.assertFalse(any("KEY" in a or "SECRET" in a for a in argv))

    def test_docker_env_strips_keys(self):
        old = dict(os.environ)
        try:
            os.environ.update(ANTHROPIC_API_KEY=CANARY, ANTHROPIC_BASE_URL="http://x", MEM0_API_KEY=CANARY, KONTEXT_TOKEN=CANARY)
            env = container.docker_env()
            self.assertFalse(any(k.startswith(("ANTHROPIC_", "MEM0_", "KONTEXT_")) for k in env))
            self.assertEqual(container.docker_env({"MEM0_API_KEY": CANARY})["MEM0_API_KEY"], CANARY)
        finally:
            os.environ.clear()
            os.environ.update(old)

    def test_keys_go_only_to_arms_that_need_them(self):
        self.assertEqual(arms.ARMS["plain"].secret_names(False), ["ANTHROPIC_API_KEY"])
        self.assertEqual(arms.ARMS["mem0"].secret_names(False), ["ANTHROPIC_API_KEY", "MEM0_API_KEY"])
        self.assertEqual(arms.ARMS["kontext"].secret_names(False), ["ANTHROPIC_API_KEY"])
        for a in arms.ARMS.values():
            self.assertEqual(a.secret_names(True), [], "a dry run passes no key")

    def test_the_deployed_server_appears_nowhere(self):
        for p in COMPARE.rglob("*"):
            if p.is_file() and p.name not in ("DESIGN.md",) and "results" not in p.parts and p.suffix in (".py", ".sh", ".toml", ".md", ".txt", ".mjs", ""):
                self.assertNotIn("workers.dev/", p.read_text(errors="replace").replace("<subdomain>.workers.dev", ""), p)
                self.assertNotIn("omarespinosafernandez", p.read_text(errors="replace"), p)


class Interventions(unittest.TestCase):
    def test_asks(self):
        self.assertEqual(interventions.asks("I looked at it. Should I go ahead and change the schema?"), "approval")
        self.assertEqual(interventions.asks("Which approach do you prefer, A or B?"), "question")
        self.assertEqual(interventions.asks("The plan is ready. Please confirm before I proceed."), "approval")
        self.assertEqual(interventions.asks("Needs your decision: keep or drop the column."), "approval")
        self.assertIsNone(interventions.asks("Done: fixed the order and the test passes."))

    def test_rule(self):
        self.assertEqual(interventions.needs_reply("work", False, "Do you want me to fix it?"), "question")
        self.assertIsNone(interventions.needs_reply("work", True, "Do you want me to fix it?"))
        self.assertIsNone(interventions.needs_reply("fill", False, "What was the topic?"))
        self.assertIsNone(interventions.needs_reply("work", False, "Should I?", is_error=True))

    def test_tool_paths_are_not_work(self):
        for p in ("openspec/changes/x/proposal.md", ".beads/issues.jsonl", ".claude/settings.json", "CLAUDE.md", ".engram/manifest.json", ".kontext.toml"):
            self.assertTrue(interventions.is_tool_path(p), p)
        self.assertFalse(interventions.is_tool_path("workshop/bookings.py"))


class Secrets(unittest.TestCase):
    def test_redact_and_scan(self):
        s = keys.Secrets({"ANTHROPIC_API_KEY": CANARY})
        self.assertNotIn(CANARY, s.redact_text(f"key={CANARY}"))
        self.assertNotIn(CANARY.encode(), s.redact_bytes(f"key={CANARY}".encode()))
        with tempfile.TemporaryDirectory() as d:
            Path(d, "a.txt").write_text("nothing")
            with gzip.open(Path(d, "b.jsonl.gz"), "wb") as f:
                f.write(f"leak {CANARY}".encode())
            hits = keys.scan(d, {"ANTHROPIC_API_KEY": CANARY})
            self.assertEqual(len(hits), 1)
            self.assertTrue(hits[0][0].endswith("b.jsonl.gz"))
            self.assertNotIn(CANARY, json.dumps(hits))

    def test_short_secret_refused(self):
        with self.assertRaises(ValueError):
            keys.Secrets({"X": "abc"})


class Parsing(unittest.TestCase):
    def test_stream_json(self):
        lines = [
            {"type": "system", "subtype": "init", "apiKeySource": "ANTHROPIC_API_KEY", "model": "claude-sonnet-5-5", "tools": ["Read"], "mcp_servers": [{"name": "engram", "status": "connected"}], "plugins": []},
            {"type": "system", "subtype": "hook_started", "hook_event": "SessionStart", "hook_name": "SessionStart:startup"},
            {"type": "system", "subtype": "hook_response", "hook_event": "SessionStart", "hook_name": "SessionStart:startup", "exit_code": 0},
            {"type": "assistant", "message": {}},
            {"type": "result", "session_id": "s1", "result": "Done.", "total_cost_usd": 0.12, "permission_denials": [{"tool_name": "Bash"}], "num_turns": 3},
        ]  # fmt: skip
        env, init, hooks = cell.parse_stream("\n".join(json.dumps(x) for x in lines))
        self.assertEqual(env["session_id"], "s1")
        self.assertEqual(init["apiKeySource"], "ANTHROPIC_API_KEY")
        self.assertEqual(init["tools"], 1)
        self.assertEqual(hooks["SessionStart:SessionStart:startup"], {"started": 1, "responses": 1, "nonzero_exit": 0})
        self.assertEqual(cell.denials(env), ["Bash"])

    def test_fake_agent_envelope(self):
        env, init, hooks = cell.parse_stream(json.dumps({"type": "result", "session_id": "x", "result": "ok"}))
        self.assertEqual(env["result"], "ok")
        self.assertIsNone(init)

    def test_unittest_verbose(self):
        text = "test_a (m.T.test_a) ... ok\ntest_b (m.T.test_b)\nDocstring ... FAIL\ntest_c (m.T.test_c) ... ERROR\n"
        self.assertEqual(container.parse_unittest_verbose(text), {"test_a": "ok", "test_b": "FAIL", "test_c": "ERROR"})


class Scenarios(unittest.TestCase):
    def test_workshop_shape(self):
        sc = script.load("L")
        self.assertEqual([len(d.turns) for d in sc.days], [5, 8, 3, 3])
        self.assertEqual([d.machine for d in sc.days], ["A", "A", "A", "B"])
        self.assertEqual(sum(1 for d in sc.days for t in d.turns if t.kind == "compact"), 1)
        self.assertEqual(len(sc.requirements()), 14)
        self.assertTrue(all(r["scored_on"] in (3, 4) for r in sc.requirements()))

    def test_every_requirement_sentence_is_said_verbatim(self):
        sc = script.load("L")
        text = "\n".join(t.text for d in sc.days for t in d.turns)
        for r in sc.requirements():
            for k in ("said", "changed", "discarded"):
                if r.get(k):
                    self.assertIn(r[k], text, f"{r['id']} {k}")
                    self.assertEqual(text.count(r[k]), 1, f"{r['id']} {k} said twice")

    def test_no_checked_function_is_asked_for_before_day_3(self):
        sc = script.load("L")
        for d in sc.days[:2]:
            for t in d.turns:
                for name in CHECKED:
                    self.assertNotIn(name, t.text, f"day {d.day} names {name}")

    def test_turns_never_name_a_tool(self):
        for task in ("smoke", "S1", "S2", "L"):
            for d in script.load(task).days:
                for t in d.turns:
                    if t.kind != "compact":
                        self.assertIsNone(TOOL_WORDS.search(t.text), f"{task} day {d.day}: {TOOL_WORDS.search(t.text)}")

    def test_first_prompt_of_each_session_is_long_enough_for_mem0(self):
        for task in ("smoke", "S1", "S2", "L"):
            for d in script.load(task).days:
                self.assertGreaterEqual(len(d.turns[0].prompt(0.05)), 20)

    def test_fill_is_deterministic(self):
        t = [t for d in script.load("L").days for t in d.turns if t.kind == "fill"][0]
        self.assertEqual(t.prompt(0.1), t.prompt(0.1))


class HiddenChecks(unittest.TestCase):
    def test_workshop_fixture_fails_every_check(self):
        res = run_checks(WORKSHOP / "repo", "test_requirements")
        self.assertEqual(len(res), 14)
        self.assertTrue(all(v != "ok" for v in res.values()), res)

    def test_workshop_reference_passes_every_check(self):
        d = Path(tempfile.mkdtemp())
        try:
            shutil.copytree(WORKSHOP / "reference" / "workshop", d / "pkg" / "workshop")
            res = run_checks(d / "pkg", "test_requirements", extra_files=WORKSHOP / "acceptance")
        finally:
            shutil.rmtree(d, ignore_errors=True)
        self.assertEqual(set(res.values()), {"ok"})
        self.assertEqual(len(res), 14)

    def test_every_mutant_fails_its_check_and_trips_its_probe(self):
        reqs = {r["id"]: r["test"] for r in tomllib.loads((WORKSHOP / "requirements.toml").read_text())["requirement"]}
        d = Path(tempfile.mkdtemp())
        try:
            shutil.copytree(WORKSHOP / "reference" / "workshop", d / "pkg" / "workshop")
            for m in tomllib.loads((WORKSHOP / "mutants.toml").read_text())["mutant"]:
                res = run_checks(d / "pkg", "test_requirements", mutate=m, extra_files=WORKSHOP / "acceptance")
                failed = sorted(k for k, v in res.items() if v != "ok")
                self.assertEqual(failed, [reqs[m["requirement"]]], m)
                if m.get("confusion"):
                    probes = run_checks(d / "pkg", "test_confusions", mutate=m, extra_files=WORKSHOP / "acceptance")
                    self.assertEqual(probes[m["confusion"]], "ok", m)
        finally:
            shutil.rmtree(d, ignore_errors=True)

    def test_probes_fail_on_the_reference(self):
        d = Path(tempfile.mkdtemp())
        try:
            shutil.copytree(WORKSHOP / "reference" / "workshop", d / "pkg" / "workshop")
            res = run_checks(d / "pkg", "test_confusions", extra_files=WORKSHOP / "acceptance")
        finally:
            shutil.rmtree(d, ignore_errors=True)
        self.assertEqual(len(res), 8)
        self.assertTrue(all(v != "ok" for v in res.values()))

    def test_small_tasks(self):
        for task, fix in (("small-due-date", ("invoices.py", "days=days - 1", "days=days")),
                          ("small-rounding", ("money.py", "ROUND_HALF_DOWN", "ROUND_HALF_UP"))):  # fmt: skip
            path = COMPARE / "scenarios" / task
            before = run_checks(path / "repo", "test_hidden")
            self.assertEqual(len(before), 6)
            self.assertIn("FAIL", before.values())
            after = run_checks(path / "repo", "test_hidden", mutate={"file": fix[0], "old": fix[1], "new": fix[2]} if fix[1] != "ROUND_HALF_DOWN" else None)
            if fix[1] == "ROUND_HALF_DOWN":
                d = Path(tempfile.mkdtemp())
                shutil.copytree(path / "repo", d / "repo")
                f = d / "repo" / fix[0]
                f.write_text(f.read_text().replace(fix[1], fix[2]))
                after = run_checks(d / "repo", "test_hidden", extra_files=path / "acceptance")
                shutil.rmtree(d, ignore_errors=True)
            self.assertEqual(set(after.values()), {"ok"}, task)


class HiddenChecksStayHidden(unittest.TestCase):
    """The hidden checks reach only the verification container."""

    MARKERS = [b"test_r01_twelve_per_class", b"def test_r14_", b"def test_c01_", b"def test_c08_", b"def test_h1_", b"def test_h5_"]

    def test_images_build_contexts_carry_no_check(self):
        di = (COMPARE / "images" / ".dockerignore").read_text().split()
        self.assertEqual(di, ["*", "!sidecar.mjs"])
        for f in (COMPARE / "images").glob("Dockerfile.*"):
            for line in f.read_text().splitlines():
                if line.startswith(("COPY", "ADD")):
                    self.assertRegex(line, r"^COPY (server/|sidecar\.mjs)", f)

    def test_harness_files_sent_to_agents_have_no_check(self):
        class Rec:
            def __init__(self):
                self.files = {}

            def put_files(self, files, dest):
                self.files.update(files)

            def sh(self, *a, **k):
                pass

        for task in ("smoke", "S1", "S2", "L"):
            for dry in (True, False):
                c = cell.Cell.__new__(cell.Cell)
                c.sc = script.load(task)
                c.cfg = cell.CellConfig(dry_run=dry, docker=object())
                m = Rec()
                c.install_harness_files(m)
                self.assertTrue(m.files)
                for name, data in m.files.items():
                    self.assertFalse(name.startswith(("acceptance", "reference")), name)
                    for marker in self.MARKERS:
                        self.assertNotIn(marker, data, f"{task} {name}")
                self.assertEqual("fake.toml" in m.files, dry)

    def test_the_fixture_has_no_check(self):
        for task in ("small-due-date", "small-rounding", "project-workshop", "smoke"):
            for p in (COMPARE / "scenarios" / task / "repo").rglob("*"):
                if p.is_file():
                    for marker in self.MARKERS:
                        self.assertNotIn(marker, p.read_bytes(), p)

    def test_fake_rules_carry_code_not_checks(self):
        text = fake.build(script.load("L")).encode()
        for marker in self.MARKERS:
            self.assertNotIn(marker, text)


class Freeze(unittest.TestCase):
    def test_the_frozen_files_match_freeze_json(self):
        st = freeze.status()
        self.assertTrue(st["frozen"], f"{st['reason']}: re-freeze only before any real run (see README)")

    def test_digest_is_stable(self):
        self.assertEqual(freeze.digest(), freeze.digest())

    def test_any_change_to_a_frozen_file_changes_the_digest(self):
        with tempfile.TemporaryDirectory() as d:
            for rel in freeze.FROZEN:
                src, dst = BENCH / rel, Path(d) / rel
                dst.parent.mkdir(parents=True, exist_ok=True)
                (shutil.copytree if src.is_dir() else shutil.copy)(src, dst)
            base = freeze.digest(d)
            self.assertEqual(base, freeze.digest())
            f = Path(d, "compare/scenarios/project-workshop/acceptance/test_requirements.py")
            data = f.read_bytes()
            f.write_bytes(data[:-1] + bytes([data[-1] ^ 1]))
            self.assertNotEqual(freeze.digest(d), base, "a flipped byte")
            f.write_bytes(data)
            self.assertEqual(freeze.digest(d), base)
            Path(d, "compare/scenarios/smoke/extra.txt").write_text("x")
            self.assertNotEqual(freeze.digest(d), base, "an added file")
            Path(d, "compare/scenarios/smoke/extra.txt").unlink()
            Path(d, "compare/ANALYSIS.md").unlink()
            self.assertNotEqual(freeze.digest(d), base, "a removed file")
            Path(d, "compare/scenarios/smoke/__pycache__").mkdir()
            Path(d, "compare/scenarios/smoke/__pycache__/x.pyc").write_bytes(b"x")
            shutil.copy(BENCH / "compare/ANALYSIS.md", Path(d, "compare/ANALYSIS.md"))
            self.assertEqual(freeze.digest(d), base, "bytecode is not frozen")

    def test_frozen_paths_exist(self):
        for rel in freeze.FROZEN:
            self.assertTrue((BENCH / rel).exists(), rel)


class Analysis(unittest.TestCase):
    def test_permutation(self):
        self.assertAlmostEqual(analysis.permutation_p([14, 14, 13, 14, 14], [9, 8, 10, 9, 7]), 2 / 252)
        self.assertEqual(analysis.permutation_p([5, 5, 5], [5, 5, 5]), 1.0)

    def test_verdicts(self):
        self.assertTrue(analysis.requirements_verdict([14, 14, 13, 14, 14], [9, 8, 10, 9, 7]).startswith("differs"))
        self.assertTrue(analysis.requirements_verdict([14, 13, 14, 13, 14], [13, 13, 14, 12, 13]).startswith("no claim"))
        self.assertIn("no claim", analysis.cost_verdict([1.1, 1.0], [1.0, 1.0]))
        self.assertNotIn("no claim", analysis.cost_verdict([2.0, 2.0], [1.0, 1.0]))


class RunPy(unittest.TestCase):
    def setUp(self):
        # bench/run.py is also a module named "run": load this one by path.
        import importlib.util

        spec = importlib.util.spec_from_file_location("compare_run", COMPARE / "run.py")
        self.run = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.run)
        self.logs = []

    def test_refuses_without_yes(self):
        self.assertEqual(self.run.main(["--tasks", "S1", "--arms", "plain", "--seeds", "1"], log=self.logs.append), 2)
        self.assertIn("--yes", self.logs[-1])

    def test_refuses_unfrozen(self):
        real = freeze.status
        try:
            freeze.status = lambda: {"frozen": False, "reason": "changed", "digest": "a", "recorded_digest": "b"}
            self.assertIs(self.run.freeze, freeze)
            self.assertEqual(self.run.main(["--tasks", "S1", "--arms", "plain", "--seeds", "1", "--yes"], log=self.logs.append), 2)
            self.assertIn("refusing a paid run", self.logs[-1])
        finally:
            freeze.status = real

    def test_refuses_without_keys(self):
        old = dict(os.environ)
        try:
            for k in ("ANTHROPIC_API_KEY", "MEM0_API_KEY"):
                os.environ.pop(k, None)
            if not freeze.status()["frozen"]:
                self.skipTest("not frozen yet")
            self.assertEqual(self.run.main(["--tasks", "S1", "--arms", "mem0", "--seeds", "1", "--yes"], log=self.logs.append), 2)
            self.assertIn("ANTHROPIC_API_KEY", self.logs[-1])
            self.assertIn("MEM0_API_KEY", self.logs[-1])
        finally:
            os.environ.clear()
            os.environ.update(old)


if __name__ == "__main__":
    unittest.main()
