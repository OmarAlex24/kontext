"""The reader benchmark in dry runs: the fixture generator and its ground truth,
the grader, the made-up reader and its failure knobs, the transcript counting,
the gate, and bench/reader.py end to end with bench/fake_reader.py."""

import gzip
import json
import os
import shlex
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

BENCH = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(BENCH))

import reader  # noqa: E402
from benchlib import pmfixture, reader_grade, reader_report, runner  # noqa: E402
from benchlib import reader_runner as rr  # noqa: E402

SPEC = pmfixture.load_spec()
IDEAS = {i["id"]: i for i in SPEC["ideas"]}


def item_lines(files):
    """{idea: state} of the ideas items.md lists, found by their paraphrases."""
    out = {}
    for line in files[".pm/items.md"].splitlines():
        for x, i in IDEAS.items():
            if any(p in line for p in i.get("items", []) + i.get("items_old", []) + i.get("items_new", [])):
                out[x] = line.split(" · ")[1]
    return out


class Fixture(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = Path(tempfile.mkdtemp())

    @classmethod
    def tearDownClass(cls):
        shutil.rmtree(cls.tmp)

    def test_same_inputs_give_the_same_bytes(self):
        a = pmfixture.write(SPEC, "s", 3, self.tmp / "a")
        b = pmfixture.write(SPEC, "s", 3, self.tmp / "b")
        self.assertEqual(a, b)
        self.assertEqual(pmfixture.read_tree(self.tmp / "a" / "root"), pmfixture.read_tree(self.tmp / "b" / "root"))
        self.assertEqual((self.tmp / "a" / "truth.json").read_bytes(), (self.tmp / "b" / "truth.json").read_bytes())
        # Nothing absolute or dated by the machine in the truth.
        self.assertNotIn(str(self.tmp), (self.tmp / "a" / "truth.json").read_text())

    def test_seeds_give_different_fixtures(self):
        omitted, distractors, orders, paraphrases, places = set(), set(), set(), set(), set()
        for seed in range(1, 9):
            files, plan = pmfixture.build(SPEC, "s", seed)
            omitted.add(tuple(sorted(x for x, i in plan["ideas"].items() if i["state"] == "omitted")))
            distractors.add(tuple(plan["distractors"]))
            dump1 = files[".pm/dumps/2026-10-05.md"]
            orders.add(tuple(sorted(IDEAS, key=lambda x: dump1.find(IDEAS[x]["said"]))))
            paraphrases.add(files[".pm/items.md"])
            lines = dump1.split("\n")
            places.add(tuple(n for n, l in enumerate(lines) if IDEAS["preorder-cutoff"]["said"] in l))
        for what in (omitted, distractors, orders, paraphrases, places):
            self.assertGreater(len(what), 1)
        a, _ = pmfixture.build(SPEC, "s", 1)
        b, _ = pmfixture.build(SPEC, "s", 2)
        self.assertNotEqual(a, b)

    def test_ranges_contain_their_sentences(self):
        for size in ("s", "m"):
            for seed in (1, 2, 3):
                dest = self.tmp / f"{size}-{seed}"
                truth = pmfixture.write(SPEC, size, seed, dest)
                root = dest / "root"
                # The truth is outside the folder the reader gets.
                self.assertEqual(sorted(p.name for p in root.iterdir()), [".pm"])
                kinds = {s["kind"] for s in truth["sentences"]}
                self.assertEqual(kinds, set(pmfixture.SENTENCE_KINDS))
                for s in truth["sentences"]:
                    lines = (root / s["file"]).read_text().split("\n")
                    a, b = s["lines"]
                    self.assertIn(s["text"], "\n".join(lines[a - 1 : b]), (size, seed, s))
                for x in IDEAS:
                    self.assertTrue(any(s.get("idea") == x and s["kind"] == "said" for s in truth["sentences"]))
                ds = [s for s in truth["sentences"] if s["kind"] == "distractor"]
                self.assertEqual(len(ds), len(truth["distractors"]))
                self.assertTrue(all(s["file"].startswith(".pm/dumps/") for s in ds))

    def test_sizes_are_near_their_target(self):
        for size in ("s", "m"):
            files, _ = pmfixture.build(SPEC, size, 1)
            chars = sum(len(t) for t in files.values())
            target = pmfixture.size_chars(size)
            self.assertLess(abs(chars - target) / target, 0.15, (size, chars, target))

    def test_items_never_show_a_discarded_idea_as_pending(self):
        absent = listed = 0
        for seed in range(1, 25):
            files, plan = pmfixture.build(SPEC, "s", seed)
            states = item_lines(files)
            ideas = plan["ideas"]
            omitted = [x for x, i in ideas.items() if i["state"] == "omitted"]
            self.assertIn(len(omitted), (2, 3))
            self.assertTrue(any(IDEAS[x]["kind"] == "passing" for x in omitted), seed)
            for x in omitted:
                self.assertNotIn(x, states)
                self.assertNotEqual(IDEAS[x]["kind"], "discarded")
            for x, i in IDEAS.items():
                if i["kind"] == "discarded":
                    if x in states:
                        self.assertEqual(states[x], "discarded", seed)
                        listed += 1
                    else:
                        absent += 1
                elif x not in omitted:
                    self.assertIn(x, states)
                    self.assertNotEqual(states[x], "discarded")
            # At least one changed idea is listed with its old value.
            text = files[".pm/items.md"]
            stale = [x for x, i in IDEAS.items() if i["kind"] == "changed" and any(p in text for p in i["items_old"])]
            self.assertTrue(stale, seed)
            self.assertEqual(sorted(stale), sorted(x for x, i in ideas.items() if i["stale"]))
            # items.md paraphrases: never the dump's sentence.
            for i in IDEAS.values():
                self.assertNotIn(i["said"], text)
        self.assertGreater(absent, 0)
        self.assertGreater(listed, 0)


class Grader(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = Path(tempfile.mkdtemp())
        cls.truth = pmfixture.write(SPEC, "s", 2, cls.tmp / "fx")
        cls.fx = reader_grade.Fixture(cls.tmp / "fx" / "root", cls.truth)

    @classmethod
    def tearDownClass(cls):
        shutil.rmtree(cls.tmp)

    def sentence(self, **kv):
        return next(s for s in self.truth["sentences"] if all(s.get(k) == v for k, v in kv.items()))

    def cite(self, s, **over):
        return {"quote": s["text"], "file": s["file"], "line": s["lines"][0], **over}

    def perfect(self):
        return {"missing": [self.cite(self.sentence(kind="said", idea=x)) for x in self.truth["omitted"]]}

    def test_perfect_audit(self):
        g = reader_grade.grade_audit(self.fx, self.perfect())
        self.assertEqual((g["recall"], g["precision"], g["invented"], g["confusions"]), (100.0, 100.0, 0, 0))

    def test_a_repeated_idea_counts_once(self):
        a = self.perfect()
        a["missing"].append(dict(a["missing"][0]))
        g = reader_grade.grade_audit(self.fx, a)
        self.assertEqual((g["recall"], g["precision"], g["valid"]), (100.0, 100.0, len(self.truth["omitted"])))

    def test_quotes_are_normalized_and_lines_have_slack(self):
        s = self.sentence(kind="said", idea=self.truth["omitted"][0])
        quote = "**" + s["text"].upper().replace("'", "’") + "**"
        g = reader_grade.grade_audit(self.fx, {"missing": [self.cite(s, quote=quote, line=s["lines"][0] + 2,
                                                                     file="/tmp/x/" + s["file"])]})
        self.assertEqual((g["recall"] > 0, g["invented"]), (True, 0))
        # A part of the sentence is fine; three lines off is not.
        part = " ".join(s["text"].split()[:6])
        self.assertTrue(reader_grade.check_citation(self.fx, self.cite(s, quote=part))["valid"])
        far = reader_grade.check_citation(self.fx, self.cite(s, line=s["lines"][0] + 3))
        self.assertEqual((far["valid"], far["reason"]), (False, "wrong line"))
        short = reader_grade.check_citation(self.fx, self.cite(s, quote="pre-order"))
        self.assertEqual(short["reason"], "too short")

    def test_a_fabricated_quote_is_invented(self):
        a = self.perfect()
        a["missing"].append({"quote": "Customers can tip the baker from the app.", "file": ".pm/dumps/2026-10-05.md",
                             "line": 3})
        a["missing"].append({"quote": "The bakery wants a drive-through window.", "file": ".pm/nope.md", "line": 1})
        g = reader_grade.grade_audit(self.fx, a)
        self.assertEqual((g["recall"], g["invented"], g["confusions"]), (100.0, 2, 0))
        reasons = [c["reason"] for c in g["citations"] if not c["valid"]]
        self.assertEqual(reasons, ["not in the file", "no such file"])

    def test_a_discarded_idea_is_a_confusion(self):
        for kind in ("said", "discard"):
            a = self.perfect()
            a["missing"].append(self.cite(self.sentence(kind=kind, idea="dark-mode")))
            g = reader_grade.grade_audit(self.fx, a)
            self.assertEqual((g["confusions"], g["discarded"], g["invented"]), (1, 1, 0), kind)
            self.assertLess(g["precision"], 100.0)

    def test_a_distractor_is_a_confusion(self):
        a = self.perfect()
        d = self.sentence(kind="distractor")
        self.assertIn(d["distractor"], self.truth["distractors"])
        a["missing"].append(self.cite(d))
        g = reader_grade.grade_audit(self.fx, a)
        self.assertEqual((g["confusions"], g["distractors"], g["invented"]), (1, 1, 0))

    def test_a_listed_idea_is_a_false_positive(self):
        a = self.perfect()
        a["missing"].append(self.cite(self.sentence(kind="said", idea=self.truth["listed"][0])))
        g = reader_grade.grade_audit(self.fx, a)
        self.assertEqual((g["recall"], g["listed"], g["confusions"]), (100.0, 1, 0))
        n = len(self.truth["omitted"])
        self.assertAlmostEqual(g["precision"], 100.0 * n / (n + 1))

    def test_filler_next_to_a_distractor_is_not_the_distractor(self):
        planted = {(s["file"], s["lines"][0]) for s in self.truth["sentences"]}
        near = []
        for d in self.truth["sentences"]:
            if d["kind"] != "distractor":
                continue
            lines = (self.fx.root / d["file"]).read_text().split("\n")
            for n in range(d["lines"][0] - 2, d["lines"][0] + 3):
                if (d["file"], n) not in planted and len(reader_grade.norm_text(lines[n - 1])) >= 20:
                    near.append({"quote": lines[n - 1], "file": d["file"], "line": n})
        self.assertTrue(near)
        g = reader_grade.grade_audit(self.fx, {"missing": near})
        self.assertEqual((g["invented"], g["confusions"]), (0, 0))
        self.assertEqual(g["other"], g["valid"])

    def perfect_values(self, stale=False):
        out = []
        for q in self.truth["questions"]:
            i = IDEAS[q["idea"]]
            use_new = i.get("change") and not stale
            s = self.sentence(kind="change" if use_new else "said", idea=q["idea"])
            out.append({"id": q["id"], "value": i["value_new"] if use_new else i["value"], **self.cite(s)})
        return {"answers": out}

    def test_perfect_values(self):
        a = self.perfect_values()
        a["answers"][0]["id"] = " Q1"  # ids are matched loosely
        g = reader_grade.grade_value(self.fx, a, self.truth["questions"])
        self.assertEqual((g["value_pct"], g["confusions"], g["invented"]), (100.0, 0, 0))
        self.assertTrue(all(q["cites_idea"] for q in g["questions"]))

    def test_a_stale_value_is_a_forbidden_hit(self):
        g = reader_grade.grade_value(self.fx, self.perfect_values(stale=True), self.truth["questions"])
        changed = [q for q in self.truth["questions"] if IDEAS[q["idea"]].get("change")]
        self.assertEqual(g["confusions"], len(changed))
        self.assertEqual(g["invented"], 0)
        bad = [q for q in g["questions"] if q["forbidden_hits"]]
        self.assertEqual(sorted(q["id"] for q in bad), sorted(q["id"] for q in changed))
        # A close distractor's value is a forbidden hit too.
        a = self.perfect_values()
        a["answers"][2]["value"] = "9 pm"
        g = reader_grade.grade_value(self.fx, a, self.truth["questions"])
        self.assertEqual((g["questions"][2]["score"], g["questions"][2]["confused"]), (0.0, True))

    def test_json_extraction(self):
        obj = {"missing": [{"quote": "x", "file": "y", "line": 1}]}
        for text in (
            json.dumps(obj),
            "```json\n" + json.dumps(obj) + "\n```",
            "Here it is:\n```json\n" + json.dumps(obj, indent=2) + "\n```\nThat's all.",
            "Sure. " + json.dumps(obj) + " Done.",
        ):
            self.assertEqual(reader_grade.extract_json(text), (obj, None), text)
        for text in ("", "no json here", "[1, 2]", "{not json}"):
            v, err = reader_grade.extract_json(text)
            self.assertIsNone(v)
            self.assertTrue(err)

    def test_unparseable_answer_scores_zero_and_is_flagged(self):
        _, g = reader_grade.grade(self.fx, "audit", "I could not do it.")
        self.assertEqual((g["recall"], g["parsed"]), (0.0, False))
        self.assertTrue(g["parse_error"])
        _, g = reader_grade.grade(self.fx, "value", '{"something": 1}')
        self.assertEqual(g["value_pct"], 0.0)
        self.assertIn("shape", g["parse_error"])

    def test_paths(self):
        for p in (".pm/dumps/a.md", "./.pm/dumps/a.md", "/private/tmp/x/.pm/dumps/a.md", "dumps/a.md"):
            self.assertEqual(reader_grade.norm_path(p), ".pm/dumps/a.md")
        self.assertEqual(reader_grade.norm_path("items.md"), ".pm/items.md")


def run_fake(fixture_root, prompt, argv=(), **knobs):
    """Runs bench/fake_reader.py in a copy of the fixture, like the runner does."""
    tmp = Path(tempfile.mkdtemp())
    try:
        project = tmp / "project"
        shutil.copytree(fixture_root, project)
        env = dict(os.environ, CLAUDE_CONFIG_DIR=str(tmp / "cfg"))
        env.update({f"FAKE_READER_{k}": str(v) for k, v in knobs.items()})
        r = subprocess.run(
            [sys.executable, str(rr.FAKE_READER), *argv], input=prompt, cwd=project, env=env,
            capture_output=True, text=True, check=True,
        )
        env_out = runner.parse_envelope(r.stdout)
        stats = rr.transcript_stats(runner.claude_projects_dir(env) / runner.claude_project_key(project))
        return env_out, stats
    finally:
        shutil.rmtree(tmp)


class FakeReader(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = Path(tempfile.mkdtemp())
        cls.truth = pmfixture.write(SPEC, "s", 4, cls.tmp / "fx")
        cls.root = cls.tmp / "fx" / "root"
        cls.fx = reader_grade.Fixture(cls.root, cls.truth)
        sections = rr.prompt_sections()
        cls.audit = rr.build_call("opus", "audit", sections, cls.truth["questions"], rr.DEFAULT_READER_CMD)
        cls.value = rr.build_call("opus", "value", sections, cls.truth["questions"], rr.DEFAULT_READER_CMD)
        cls.delegated = rr.build_call("opus+haiku", "audit", sections, cls.truth["questions"], rr.DEFAULT_READER_CMD)

    @classmethod
    def tearDownClass(cls):
        shutil.rmtree(cls.tmp)

    def grade(self, call, **knobs):
        argv, prompt = call
        env, stats = run_fake(self.root, prompt, argv[1:], **knobs)
        task = "audit" if call is self.audit or call is self.delegated else "value"
        return reader_grade.grade(self.fx, task, env["result"])[1], env, stats

    def test_answers_right_without_knobs(self):
        g, env, stats = self.grade(self.audit)
        self.assertEqual((g["recall"], g["invented"], g["confusions"]), (100.0, 0, 0))
        self.assertEqual(list(env["modelUsage"]), ["claude-opus-5-5"])
        self.assertEqual((stats["agent_calls"], stats["main_read_dumps"]), (0, True))
        g, _, _ = self.grade(self.value)
        self.assertEqual((g["value_pct"], g["confusions"], g["invented"]), (100.0, 0, 0))

    def test_each_knob_is_caught(self):
        g, _, _ = self.grade(self.audit, MISS=1)
        n = len(self.truth["omitted"])
        self.assertAlmostEqual(g["recall"], 100.0 * (n - 1) / n)
        g, _, _ = self.grade(self.audit, INVENT=1)
        self.assertEqual((g["recall"], g["invented"]), (100.0, 1))
        g, _, _ = self.grade(self.audit, DISCARD=1)
        self.assertEqual((g["discarded"], g["confusions"]), (1, 1))
        g, _, _ = self.grade(self.audit, DISTRACT=1)
        self.assertEqual((g["distractors"], g["confusions"]), (1, 1))
        g, _, _ = self.grade(self.value, STALE=1)
        self.assertLess(g["value_pct"], 100.0)
        self.assertGreater(g["confusions"], 0)
        g, _, _ = self.grade(self.audit, GARBLE=1)
        self.assertEqual(g["recall"], 0.0)
        self.assertTrue(g["parse_error"])

    def test_delegates_when_given_a_subagent(self):
        g, env, stats = self.grade(self.delegated)
        self.assertEqual(g["recall"], 100.0)
        self.assertEqual(sorted(env["modelUsage"]), ["claude-haiku-5-5", "claude-opus-5-5"])
        self.assertEqual((stats["agent_calls"], stats["agent_types"]), (1, [rr.SUBAGENT["name"]]))
        self.assertEqual((stats["main_dump_reads"], stats["sub_files"]), (0, 1))
        self.assertEqual(stats["sub_models"], ["claude-haiku-5-5"])
        _, _, stats = self.grade(self.delegated, BYPASS=1)
        self.assertTrue(stats["main_read_dumps"])


class Calls(unittest.TestCase):
    def test_argv_of_each_kind_of_config(self):
        sections = rr.prompt_sections()
        qs = SPEC["questions"]
        argv, prompt = rr.build_call("opus", "audit", sections, qs, rr.DEFAULT_READER_CMD, effort="high")
        self.assertEqual(argv[:2], ["claude", "-p"])
        self.assertEqual(argv[argv.index("--setting-sources") + 1], "")
        self.assertIn("--strict-mcp-config", argv)
        self.assertEqual(argv[argv.index("--model") + 1], "claude-opus-5-5")
        self.assertEqual(argv[argv.index("--effort") + 1], "high")
        self.assertEqual(argv[argv.index("--tools") + 1], "Read,Grep,Glob")
        self.assertNotIn("--agents", argv)
        self.assertIn(sections["Reader"], prompt)
        self.assertIn('{"missing"', prompt)

        argv, prompt = rr.build_call("opus+sonnet", "value", sections, qs, rr.DEFAULT_READER_CMD)
        self.assertEqual(argv[argv.index("--model") + 1], "claude-opus-5-5")
        self.assertEqual(argv[argv.index("--tools") + 1], f"Read,Grep,Glob,{rr.SUBAGENT['tool']}")
        agents = json.loads(argv[argv.index("--agents") + 1])
        sub = agents[rr.SUBAGENT["name"]]
        self.assertEqual(sub["model"], "claude-sonnet-5-5")
        self.assertEqual(sub["tools"], ["Read", "Grep", "Glob"])
        self.assertEqual(sub["prompt"], sections["Reader"])
        self.assertTrue(sub["description"])
        self.assertNotIn(sections["Reader"], prompt)
        self.assertIn(rr.SUBAGENT["name"], prompt)
        self.assertNotIn("{agent}", prompt)
        for q in qs:
            self.assertIn(q["question"], prompt)
        self.assertNotIn("{questions}", prompt)
        # The argv survives a round trip through the shell, for a copy-paste.
        self.assertEqual(shlex.split(shlex.join(argv)), argv)


def rec(tid, name, inp, sidechain=False, model="claude-opus-5-5", mid="m", ctx=0):
    usage = {"input_tokens": 3, "cache_creation_input_tokens": ctx, "cache_read_input_tokens": 100}
    r = {"type": "assistant", "message": {"id": mid, "model": model, "role": "assistant", "usage": usage,
                                          "content": [{"type": "tool_use", "id": tid, "name": name, "input": inp}]}}
    if sidechain:
        r["isSidechain"] = True
    return json.dumps(r)


class Transcripts(unittest.TestCase):
    def test_counts_tools_delegations_and_dump_reads(self):
        tmp = Path(tempfile.mkdtemp())
        try:
            main = [
                json.dumps({"type": "user", "message": {"role": "user", "content": "go"}}),
                rec("t1", "Agent", {"subagent_type": "pm-reader", "prompt": "…"}),
                rec("t1", "Agent", {"subagent_type": "pm-reader", "prompt": "…"}),  # same block twice
                rec("t2", "Read", {"file_path": "/tmp/p/.pm/dumps/2026-10-05.md"}),
                rec("t3", "Glob", {"pattern": "*.md", "path": "/tmp/p/.pm/dumps"}),  # lists, does not read
                rec("t4", "Grep", {"pattern": "slot", "path": ".pm"}),
                rec("t5", "Read", {"file_path": "/tmp/p/.pm/items.md"}, ctx=40000),
                rec("t6", "Read", {"file_path": "/tmp/p/.pm/dumps/x.md"}, sidechain=True),  # inline subagent
                "{broken",
            ]
            (tmp / "s1.jsonl").write_text("\n".join(main) + "\n")
            sub = tmp / "s1" / "subagents"
            sub.mkdir(parents=True)
            with gzip.open(sub / "agent-a.jsonl.gz", "wt") as f:
                f.write(rec("t7", "Read", {"file_path": ".pm/dumps/a.md"}, model="claude-haiku-5-5") + "\n")
                f.write(rec("t8", "Grep", {"pattern": "x"}, model="claude-haiku-5-5") + "\n")
            (sub / "agent-b.jsonl.gz").write_bytes(b"not gzip")
            s = rr.transcript_stats(tmp)
            self.assertEqual(s["main_tools"], {"Agent": 1, "Read": 2, "Glob": 1, "Grep": 1})
            self.assertEqual(s["sub_tools"], {"Read": 2, "Grep": 1})
            self.assertEqual((s["agent_calls"], s["agent_types"]), (1, ["pm-reader"]))
            self.assertEqual((s["main_dump_reads"], s["main_read_dumps"]), (2, True))
            self.assertEqual((s["main_files"], s["sub_files"]), (1, 2))
            self.assertEqual((s["main_models"], s["sub_models"]), (["claude-opus-5-5"], ["claude-haiku-5-5", "claude-opus-5-5"]))
            self.assertEqual((s["main_peak_context"], s["sub_peak_context"]), (40103, 103))
            empty = rr.transcript_stats(tmp / "missing")
            self.assertEqual((empty["agent_calls"], empty["main_files"], empty["main_read_dumps"]), (0, 0, False))
        finally:
            shutil.rmtree(tmp)


def agg(recall, value, invented=0.0, discarded=0):
    ms = lambda v: {"mean": v, "sd": 0.0}  # noqa: E731
    return {"seeds": 1, "discarded_total": discarded, "recall": ms(recall), "value_pct": ms(value),
            "invented_per_call": ms(invented)}


class Gate(unittest.TestCase):
    def test_verdicts(self):
        table = {
            "opus": {"m": agg(90, 100)},
            "opus+sonnet": {"m": agg(80, 90, invented=1.0, discarded=1)},
            "opus+haiku": {"m": agg(79, 100)},
            "sonnet": {"m": agg(10, 10)},
        }
        g = reader_report.gate(table)
        self.assertTrue(g["sane"])
        self.assertEqual(g["configs"]["opus+sonnet"]["verdict"], "viable")
        self.assertEqual(g["configs"]["opus+haiku"]["verdict"], "not viable")
        self.assertFalse(g["configs"]["opus+haiku"]["criteria"]["recall"]["ok"])
        self.assertNotIn("sonnet", g["configs"])  # direct configs are diagnosis only
        table["opus+haiku"]["m"] = agg(100, 100, invented=1.5)
        self.assertEqual(reader_report.gate(table)["configs"]["opus+haiku"]["verdict"], "not viable")
        table["opus+haiku"]["m"] = agg(100, 100, discarded=2)
        self.assertEqual(reader_report.gate(table)["configs"]["opus+haiku"]["verdict"], "not viable")

    def test_ill_posed_and_missing(self):
        g = reader_report.gate({"opus": {"m": agg(70, 100)}, "opus+haiku": {"m": agg(70, 100)}})
        self.assertFalse(g["sane"])
        self.assertIn("ill-posed", g["configs"]["opus+haiku"]["verdict"])
        g = reader_report.gate({"opus": {"s": agg(100, 100)}, "opus+haiku": {"s": agg(100, 100)}})
        self.assertIn("not judged", g["configs"]["opus+haiku"]["verdict"])


class DryRun(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = Path(tempfile.mkdtemp())
        cls.out = cls.tmp / "r"
        logs = []
        code = reader.main(["--dry-run", "--sizes", "s", "--seeds", "1", "--jobs", "2", "--out", str(cls.out)],
                           log=logs.append)
        assert code == 0, "\n".join(logs)
        cls.report = json.loads((cls.out / "report.json").read_text())

    @classmethod
    def tearDownClass(cls):
        shutil.rmtree(cls.tmp)

    def test_report_and_runs(self):
        md = (self.out / "report.md").read_text()
        self.assertIn("Dry run", md)
        self.assertIn("## Gate", md)
        for c in rr.DEFAULT_CONFIGS:
            self.assertIn(f"| {c} | s |", md)
            row = self.report["table"][c]["s"]
            self.assertEqual((row["recall"]["mean"], row["value_pct"]["mean"]), (100.0, 100.0))
            self.assertEqual((row["invented_per_call"]["mean"], row["confusions"]["mean"]), (0.0, 0.0))
            self.assertEqual(row["cost_usd"]["mean"], 0.0)
            for t in rr.TASKS:
                r = json.loads((self.out / "runs" / f"{c}-s-s1-{t}" / "run.json").read_text())
                self.assertEqual(r["argv"][r["argv"].index("--model") + 1], "claude-opus-5-5")
                self.assertTrue(r["fixture"]["unchanged"])
                self.assertIsNotNone(r["answer"])
                self.assertTrue(r["usage_by_model"])
                self.assertGreaterEqual(r["claude_sessions"]["files"], 1)
                delegated = "+" in c
                self.assertEqual(r["transcript"]["agent_calls"], 1 if delegated else 0)
                self.assertEqual(r["transcript"]["main_read_dumps"], not delegated)
                self.assertEqual(sorted(r["usage_by_model"]), sorted(m for m in r["models"].values() if m))
        # The truth stays out of the folder the reader gets; all configs read one fixture.
        self.assertTrue((self.out / "fixtures" / "s-s1" / "truth.json").exists())
        shas = {json.loads(p.read_text())["fixture"]["sha256"] for p in (self.out / "runs").glob("*/run.json")}
        self.assertEqual(len(shas), 1)

    def test_real_run_needs_confirmation(self):
        logs = []
        self.assertEqual(reader.main(["--sizes", "s", "--out", str(self.tmp / "x")], log=logs.append), 2)
        self.assertIn("spends tokens", logs[0])
        self.assertFalse((self.tmp / "x").exists())
        self.assertEqual(reader.main(["--dry-run", "--configs", "gpt"], log=logs.append), 2)

    def test_a_bypassed_delegation_is_reported(self):
        out = self.tmp / "bypass"
        os.environ["FAKE_READER_BYPASS"] = "1"
        try:
            code = reader.main(["--dry-run", "--sizes", "s", "--configs", "opus+haiku", "--tasks", "audit",
                                "--out", str(out)], log=lambda *_: None)
        finally:
            del os.environ["FAKE_READER_BYPASS"]
        self.assertEqual(code, 0)
        self.assertIn("read `.pm/dumps/` from the main session", (out / "report.md").read_text())


if __name__ == "__main__":
    unittest.main()
