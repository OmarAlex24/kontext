"""The long-memory scenario in dry runs: small filler (--fill-scale 0.02),
/compact, new sessions, kontext's distill for real with a made-up model, and the
measures by class and by checkpoint."""

import io
import json
import shutil
import sys
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path

BENCH = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(BENCH))

import aggregate  # noqa: E402
import run  # noqa: E402
from benchlib import fill, metrics, runner, scenario  # noqa: E402


def jsonl(path):
    return [json.loads(l) for l in path.read_text().splitlines()]


class Pieces(unittest.TestCase):
    def test_scenario_loads(self):
        sc = scenario.load("long-memory")
        self.assertEqual(len(sc.days), 4)
        self.assertTrue(sc.distill)
        kinds = [t.kind for d in sc.days for t in d]
        self.assertEqual(kinds.count("compact"), 3)
        self.assertEqual(kinds.count("fill"), 14)
        probes = sc.all_probes()
        self.assertEqual(len(probes), 20)
        self.assertEqual({p.cls for p in probes}, {"A", "B", "C", "cross"})
        self.assertEqual({p.point for p in probes}, {1, 2, 3, 4})
        self.assertEqual(sum(1 for m in sc.memory if m.get("changed")), 2)
        # The filler puts in about 100k tokens per session with fill-scale 1 (plus ~40k of the rest).
        per_day = [sum(t.tokens for t in d if t.kind == "fill") for d in sc.days]
        self.assertEqual(per_day[:3], [100000, 100000, 80000])

    def test_validation_catches_typos(self):
        tmp = Path(tempfile.mkdtemp())
        try:
            src = scenario.resolve("long-memory")
            shutil.copytree(src, tmp / "x")
            t = tmp / "x" / "scenario.toml"
            t.write_text(t.read_text().replace('said = "we never send', 'said = "we always send'))
            with self.assertRaises(ValueError) as e:
                scenario.load(tmp / "x")
            self.assertIn("sunday", str(e.exception))
        finally:
            shutil.rmtree(tmp)

    def test_fill_is_deterministic_and_scales(self):
        d = ["Ríos Hardware Store has a payment extension until the 20th of each month"]
        a, n = fill.prompt("Summarize.", "minutes", 2000, 7, d)
        b, _ = fill.prompt("Summarize.", "minutes", 2000, 7, d)
        c, _ = fill.prompt("Summarize.", "minutes", 2000, 8, d)
        self.assertEqual(a, b)
        self.assertNotEqual(a, c)
        self.assertIn(d[0], a)
        self.assertGreaterEqual(n, 2000 * fill.CHARS_PER_TOKEN)
        small, m = fill.prompt("Summarize.", "mixed", 2000, 7, d, scale=0.1)
        self.assertLess(m, n)
        self.assertIn(d[0], small)

    def test_cost_is_per_session_not_summed(self):
        # With --resume, Claude Code gives the session's running total in each envelope.
        def env(cost, cr):
            return {"total_cost_usd": cost, "usage": {"cache_read_input_tokens": 1},
                    "modelUsage": {"m": {"cacheReadInputTokens": cr, "outputTokens": 10}}}

        tmp = Path(tempfile.mkdtemp())
        try:
            (tmp / "transcripts").mkdir()
            recs = [("a", env(0.1, 100)), ("a", env(0.25, 300)), ("b", env(0.05, 50))]
            (tmp / "transcripts" / "day-1.jsonl").write_text(
                "".join(json.dumps({"session_id": sid, "envelope": e}) + "\n" for sid, e in recs)
            )
            (tmp / "run.json").write_text(json.dumps({"probes": {"items": [{"usage": {"cost_usd": 0.01}}]}}))
            r = runner.recost(tmp)
            self.assertAlmostEqual(r["usage_by"]["agent"]["cost_usd"], 0.30)
            self.assertEqual(r["usage_by"]["agent"]["cache_read_input_tokens"], 350)
            self.assertAlmostEqual(r["usage"]["cost_usd"], 0.31)
        finally:
            shutil.rmtree(tmp)

    def test_cost_by_kind_takes_each_call(self):
        def env(cost, cr, turns):
            return {"total_cost_usd": cost, "num_turns": turns,
                    "modelUsage": {"m": {"cacheReadInputTokens": cr, "outputTokens": 10}}}

        tmp = Path(tempfile.mkdtemp())
        try:
            (tmp / "transcripts").mkdir()
            recs = [("turn", env(0.1, 100, 5)), ("correction", env(0.15, 160, 2)),
                    ("fill", env(0.45, 400, 1)), ("compact", env(0.6, 900, 0))]
            (tmp / "transcripts" / "day-1.jsonl").write_text(
                "".join(json.dumps({"session_id": "a", "kind": k, "envelope": e}) + "\n" for k, e in recs)
            )
            c = aggregate.cost_by_kind(tmp)
            self.assertEqual(set(c), {"work", "fill", "compact"})
            self.assertEqual(c["work"]["calls"], 2)
            self.assertEqual(c["work"]["cache_read"], 160)
            self.assertEqual(c["work"]["turns"], 7)
            self.assertAlmostEqual(c["work"]["cost_usd"], 0.15)
            self.assertEqual(c["fill"]["cache_read"], 240)
            self.assertEqual(c["compact"]["output"], 0)
            self.assertAlmostEqual(c["compact"]["cost_usd"], 0.15)
        finally:
            shutil.rmtree(tmp)

    def test_distill_command_uses_absolute_paths(self):
        # kontext runs the command in a temporary folder: a relative path would be lost.
        cmd = runner.distill_command(runner.Config(dry_run=True), Path("rel/run"))
        out = cmd.split("--out ", 1)[1].split(" ", 1)[0]
        self.assertTrue(Path(out).is_absolute(), cmd)

    def test_keyword_judge_with_forbidden(self):
        v = metrics.keyword_verdict
        self.assertEqual(v("The rate in force is 1.8 % monthly", ["1.8%"], ["1.5%", "2%"]), (1.0, []))
        self.assertEqual(v("It is 1.5 %", ["1.8%"], ["1.5%"]), (0.0, ["1.5%"]))
        # Expected and forbidden together: 0, and it counts as a confusion.
        self.assertEqual(v("Until the 15, or the 20", ["15"], ["20"]), (0.0, ["20"]))
        # Whole words: 2026 is not "20", and 325000 works if it is accepted.
        self.assertEqual(v("The 15 of 2026", ["15"], ["20"]), (1.0, []))
        self.assertEqual(v("325,000 cents", ["3250|325000"]), (1.0, []))
        self.assertEqual(v("4 days", ["4 days|four days"], ["7 days"]), (1.0, []))


class DryRun(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if not runner.find_kontext():
            raise unittest.SkipTest("no kontext binary: run `cargo build` (or set KONTEXT_BIN)")
        cls.tmp = tempfile.TemporaryDirectory()
        logs = []
        code = run.main(
            ["--dry-run", "--scenario", "long-memory", "--seeds", "1", "--fill-scale", "0.02", "--out", cls.tmp.name],
            log=logs.append,
        )
        assert code == 0, "\n".join(logs)
        cls.out = next(Path(cls.tmp.name).iterdir())
        cls.report = json.loads((cls.out / "report.json").read_text())["table"]
        cls.runs = {v: json.loads((cls.out / "runs" / f"{v}-s1" / "run.json").read_text()) for v in runner.VARIANTS}

    @classmethod
    def tearDownClass(cls):
        cls.tmp.cleanup()

    def test_compacts_and_fills_in_transcripts(self):
        recs = jsonl(self.out / "runs" / "none-s1" / "transcripts" / "day-1.jsonl")
        fills = [r for r in recs if r["kind"] == "fill"]
        self.assertEqual(len(fills), 5)
        for r in fills:
            # The request is saved, not the document: its hash and its size.
            self.assertNotIn("---", r["prompt"])
            self.assertEqual(len(r["fill"]["sha256"]), 64)
            self.assertGreater(r["fill"]["chars"], len(r["prompt"]))
        self.assertEqual([r["prompt"] for r in recs if r["kind"] == "compact"], ["/compact"])
        for v, r in self.runs.items():
            self.assertEqual([c["ok"] for c in r["compacts"]], [True, True, True], v)
            self.assertEqual(len(r["fills"]), 14)

    def test_distill_runs_waits_and_counts(self):
        for v in ("kontext-pointer", "kontext-inject"):
            r = self.runs[v]
            d = r["distill"]
            self.assertTrue(d["enabled"])
            self.assertGreater(d["calls"], 0)
            self.assertEqual((d["failed"], d["wait_errors"]), (0, []))
            titles = {e["title"].lower() for e in d["entries"]}
            self.assertIn("ruiz hardware store has a payment extension until the 15th of each month", titles)
            self.assertIn("the late-fee rate is no longer 1.5 %: the new one, 1.8 % monthly, is already in force", titles)
            # The two facts that change are recorded as replacing the old value.
            self.assertEqual(sum(1 for e in d["entries"] if e["supersedes"]), 2)
            self.assertTrue(all(e["origin"] == "user" for e in d["entries"]))
            self.assertIn("distill", r["usage_by"])
            self.assertTrue(list((self.out / "runs" / f"{v}-s1" / "distill").glob("*.json")))
            recs = jsonl(self.out / "runs" / f"{v}-s1" / "transcripts" / "day-1.jsonl")
            self.assertTrue(all(rec.get("distill_wait_s") is not None for rec in recs))
            # `kontext recall` only sees the transcripts of this run.
            self.assertIn("claude-config/projects/", r["kontext"]["transcripts_glob"])
            self.assertIn("distill_cmd.py", r["kontext"]["distill"])
        for v in ("none", "automem", "notes"):
            self.assertFalse(self.runs[v]["distill"]["enabled"])

    def test_none_forgets_kontext_remembers(self):
        none, inject = self.runs["none"], self.runs["kontext-inject"]
        for k in ("B", "C"):
            self.assertEqual(none["probes"]["by_class"][k]["recall"], 0.0)
            self.assertEqual(inject["probes"]["by_class"][k]["recall"], 1.0)
        self.assertEqual(none["probes"]["by_checkpoint"]["1"]["recall"], 0.0)
        # After compacting, `none` keeps the filler and falls into a distractor.
        self.assertGreaterEqual(none["probes"]["confusions"], 1)
        self.assertEqual(inject["probes"]["confusions"], 0)
        self.assertLess(none["acceptance"]["pct"], 100.0)
        self.assertEqual(inject["acceptance"]["pct"], 100.0)
        self.assertEqual(self.report["notes"]["recall_pct"]["mean"], 100.0)

    def test_reports(self):
        md = (self.out / "report.md").read_text()
        for s in ("Memory % by class", "Memory % by checkpoint", "Behavior %", "Confusions", "distill"):
            self.assertIn(s, md)
        buf = io.StringIO()
        with redirect_stdout(buf):
            self.assertEqual(aggregate.main(["--dry", str(self.out)]), 0)
        text = buf.getvalue()
        self.assertIn("## Scenario long-memory", text)
        self.assertIn("| Model | Variant | Class A | Class B | Class C | Class cross |", text)
        self.assertNotIn("Cost by turn type", text)
        buf = io.StringIO()
        with redirect_stdout(buf):
            self.assertEqual(aggregate.main(["--dry", "--cost", str(self.out)]), 0)
        costo = buf.getvalue()
        # The usual output does not change: the new table goes apart.
        for line in text.splitlines():
            self.assertIn(line, costo)
        self.assertIn("### Cost by turn type", costo)
        self.assertIn("| Model | Variant | Type | Calls | Cache read | Cache created | Output | Turns | USD |", costo)
        for kind in ("work", "fill", "probe", "compact"):
            self.assertIn(f"| kontext-inject | {kind} |", costo)
        self.assertIn("### Prompt cache breaks", costo)
        self.assertIn("| Model | Variant | Requests | Breaks | Breaks % | Tokens rewritten | After pause |", costo)

    def test_sessions_saved_and_cache_measured(self):
        for v, r in self.runs.items():
            claude = self.out / "runs" / f"{v}-s1" / "transcripts" / "claude"
            # One session per day, compressed, in the run (not only in the shared
            # CLAUDE_CONFIG_DIR).
            self.assertEqual(len(list(claude.glob("*.jsonl.gz"))), 4, v)
            self.assertEqual(r["claude_sessions"]["files"], 4, v)
            self.assertGreater(r["cache"]["requests"], 0, v)
            # The made-up agent uses no cache: nothing to break.
            self.assertEqual((r["cache"]["breaks"], r["cache"]["tokens_rewritten"]), (0, 0), v)


if __name__ == "__main__":
    unittest.main()
