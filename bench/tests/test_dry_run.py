"""The harness end to end, in dry runs: the billing scenario with the four
variants and 2 seeds, with bench/fake_agent.py instead of a model."""

import json
import sys
import tempfile
import unittest
from pathlib import Path

BENCH = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(BENCH))

import run  # noqa: E402
from benchlib import metrics, runner, scenario  # noqa: E402


class Formula(unittest.TestCase):
    def test_drift(self):
        self.assertEqual(metrics.desvio_pct(0, 0, 0, 0), 0.0)
        self.assertEqual(metrics.desvio_pct(0, 100, 0, 2), 0.0)
        # 20 lines outside of 100, one of two guards fails: (20 + 50) / (100 + 100).
        self.assertAlmostEqual(metrics.desvio_pct(20, 100, 1, 2), 35.0)
        self.assertEqual(metrics.desvio_pct(10, 10, 1, 1), 100.0)

    def test_globs(self):
        self.assertTrue(scenario.matches("src/a/b.py", ["src/**/*.py"]))
        self.assertTrue(scenario.matches("src/b.py", ["src/**/*.py"]))
        self.assertFalse(scenario.matches("srcx/b.py", ["src/**"]))
        self.assertTrue(scenario.matches(".claude/settings.json", [".claude/**"]))
        self.assertTrue(scenario.matches("x/__pycache__/y.pyc", ["**/__pycache__/**"]))
        self.assertFalse(scenario.matches("src/a/b.py", ["src/*.py"]))

    def test_unittest_pct(self):
        self.assertEqual(metrics.unittest_pct("Ran 4 tests in 0.1s\n\nOK\n"), 100.0)
        self.assertEqual(metrics.unittest_pct("Ran 4 tests\n\nFAILED (failures=1, errors=1)\n"), 50.0)
        self.assertIsNone(metrics.unittest_pct("nothing"))

    def test_default_agent_may_run_kontext_as_agents_call_it(self):
        # In `claude -p` nobody approves: the default command carries the
        # permissions, including the ways of calling kontext that `Bash(kontext:*)`
        # did not cover in long-memory.
        import shlex

        argv = shlex.split(runner.DEFAULT_AGENT_CMD)
        allowed = argv[argv.index("--allowedTools") + 1].split(",")
        self.assertEqual(allowed, runner.AGENT_ALLOWED_TOOLS)
        for rule in ("Bash(kontext:*)", "Bash(KONTEXT_* kontext *)", "Bash(export KONTEXT_*)", "Bash(sed -n:*)", "Bash(head:*)"):
            self.assertIn(rule, allowed)

    def test_scenario_loads(self):
        sc = scenario.load("billing")
        self.assertEqual(len(sc.days), 3)
        self.assertEqual(len(sc.decisions), 2)
        self.assertEqual(sum(1 for d in sc.days for t in d if t.temptation), 2)
        self.assertEqual(len(sc.probes), 3)


class DryRun(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if not runner.find_kontext():
            raise unittest.SkipTest("no kontext binary: run `cargo build` (or set KONTEXT_BIN)")
        cls.tmp = tempfile.TemporaryDirectory()
        logs = []
        code = run.main(["--dry-run", "--seeds", "2", "--out", cls.tmp.name], log=logs.append)
        assert code == 0, "\n".join(logs)
        cls.out = next(Path(cls.tmp.name).iterdir())
        cls.report = json.loads((cls.out / "report.json").read_text())["table"]

    @classmethod
    def tearDownClass(cls):
        cls.tmp.cleanup()

    def test_report_and_runs_exist(self):
        md = (self.out / "report.md").read_text()
        self.assertIn("Dry run", md)
        for v in runner.VARIANTS:
            self.assertIn(f"| {v} |", md)
            for s in (1, 2):
                run_dir = self.out / "runs" / f"{v}-s{s}"
                self.assertTrue((run_dir / "run.json").exists(), run_dir)
                self.assertEqual(len(list((run_dir / "transcripts").glob("day-*.jsonl"))), 3)

    def test_none_drifts_more_than_kontext_inject(self):
        none = self.report["none"]
        inject = self.report["kontext-inject"]
        self.assertGreater(none["desvio_pct"]["mean"], inject["desvio_pct"]["mean"])
        self.assertGreater(none["corrections"]["mean"], inject["corrections"]["mean"])
        self.assertGreaterEqual(inject["recall_pct"]["mean"], none["recall_pct"]["mean"])
        self.assertEqual(inject["acceptance_pct"]["mean"], 100.0)

    def test_dry_run_costs_nothing(self):
        for v in runner.VARIANTS:
            self.assertEqual(self.report[v]["tokens"]["mean"], 0)
            self.assertEqual(self.report[v]["cost_usd"]["mean"], 0)

    def test_real_run_needs_confirmation(self):
        logs = []
        self.assertEqual(run.main(["--seeds", "1", "--out", self.tmp.name], log=logs.append), 2)
        self.assertIn("spends tokens", logs[0])


if __name__ == "__main__":
    unittest.main()
