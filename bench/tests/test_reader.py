"""The reader benchmark in dry runs: the spec and its validation, the fixture
generator and its ground truth, the grader (and the v2 `ideas` diagnostic), the
prompts (the `skill` version: the texts kontext install ships), the made-up
reader and its failure knobs, the transcript counting (effort included), the
variants, the gate per prompt version, and bench/reader.py end to end with
bench/fake_reader.py."""

import contextlib
import copy
import gzip
import hashlib
import io
import json
import os
import re
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
from benchlib.metrics import keyword_hit, normalize  # noqa: E402

SPEC = pmfixture.load_spec()
IDEAS = {i["id"]: i for i in SPEC["ideas"]}
KIND = {x: i["kind"] for x, i in IDEAS.items()}


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
            self.assertEqual(len(omitted), SPEC["omitted_count"])
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

# Words of the user's own text that state something the app should do or follow.
REQUEST_MARKERS = ["should", "i want", "i would like", "i d like", "would be nice", "let s", "we could",
                   "has to", "have to", "needs to", "need to", "must", "we need", "it would"]


def unplanted(spec):
    """The sentences of the user's paragraphs that are not a planted one."""
    out = []
    for d in spec["dumps"]:
        for p in d["paragraphs"]:
            for part in pmfixture.PLACEHOLDER.split(p)[::3]:
                out += [f.strip() for f in re.split(r"(?<=[.!?:])\s+", part) if f.strip()]
    return out


def spec_numbers(o, key=None):
    """Every number in the spec's text (ids aside)."""
    out = set()
    if isinstance(o, dict):
        for k, v in o.items():
            if not k.startswith("_") and k not in ("id", "idea", "near"):
                out |= spec_numbers(v, k)
    elif isinstance(o, list):
        for v in o:
            out |= spec_numbers(v, key)
    elif isinstance(o, str):
        out |= {int(n) for n in re.findall(r"\d+", o)}
    return out


def broken(**edits):
    """A copy of the spec with `edits(spec)` applied; returns validate's message."""
    spec = copy.deepcopy(SPEC)
    for f in edits.values():
        f(spec)
    try:
        pmfixture.validate(spec)
    except ValueError as e:
        return str(e)
    return ""


def idea(spec, ident):
    return next(i for i in spec["ideas"] if i["id"] == ident)


def question(spec, ident):
    return next(q for q in spec["questions"] if q["idea"] == ident)


class Spec(unittest.TestCase):
    def test_version_and_sha_are_recorded(self):
        self.assertEqual(SPEC["version"], 2)
        self.assertEqual(SPEC["_sha256"], hashlib.sha256(pmfixture.SPEC_PATH.read_bytes()).hexdigest())
        tmp = Path(tempfile.mkdtemp())
        try:
            truth = pmfixture.write(SPEC, "s", 1, tmp / "fx")
            self.assertEqual((truth["spec_version"], truth["spec_sha256"]), (2, SPEC["_sha256"]))
        finally:
            shutil.rmtree(tmp)

    def test_what_v2_covers(self):
        kinds = [i["kind"] for i in SPEC["ideas"]]
        self.assertGreaterEqual(kinds.count("passing"), 4)
        qs = {q["idea"]: q for q in SPEC["questions"]}
        self.assertGreaterEqual(len(qs), 6)
        twice = [x for x in qs if len(pmfixture.changes(IDEAS[x])) >= 2]
        relative = [x for x in qs if IDEAS[x].get("relative")]
        passing = [x for x in qs if KIND[x] == "passing"]
        close = [x for x in qs if any(d.get("near") == x and d.get("value") for d in SPEC["distractors"])]
        self.assertTrue(twice and passing)
        self.assertGreaterEqual(len(relative), 1)
        self.assertGreaterEqual(len(close), 2)
        # Every superseded value is forbidden (the validation checks it; spelled out here).
        for x in twice + relative:
            for v in IDEAS[x]["values"][:-1]:
                self.assertTrue(reader_grade.keyword_verdict(v, qs[x]["expected"], qs[x]["forbidden"])[1], (x, v))

    def test_validation_catches_broken_specs(self):
        self.assertEqual(broken(), "")
        cases = {
            "version": dict(a=lambda s: s.pop("version")),
            "has to be used exactly once": dict(a=lambda s: s["dumps"][2]["paragraphs"].pop(2)),
            "later dump": dict(a=lambda s: s["dumps"][1]["paragraphs"].append("{change2:pickup-slots}"),
                               b=lambda s: s["dumps"][2]["paragraphs"].pop(2)),
            "names the value it leads to": dict(
                a=lambda s: idea(s, "order-history").update(change=["Let's show 24 orders per page instead."])),
            "does not state its value": dict(a=lambda s: idea(s, "pickup-slots").pop("relative", None),
                                             b=lambda s: idea(s, "pickup-slots")["change"].__setitem__(
                                                 1, "Rosa finds the slots too loose, let's shorten them.")),
            "is not forbidden": dict(a=lambda s: question(s, "pickup-slots")["forbidden"].remove("30")),
            "not `always`": dict(a=lambda s: next(d for d in s["distractors"] if d["id"] == "flower-orders")
                                 .pop("always")),
            "one value for its sentence": dict(a=lambda s: idea(s, "pickup-slots")["values"].pop()),
            "does not score": dict(a=lambda s: question(s, "order-history").update(expected=["48"])),
            "needs `values`": dict(a=lambda s: idea(s, "cake-notice").pop("values")),
        }
        for what, edits in cases.items():
            self.assertIn(what, broken(**edits), what)

    def test_each_seed_omits_three_and_the_passing_ones_take_turns(self):
        seen = set()
        for seed in range(1, 6):
            omitted = pmfixture.omissions(SPEC, seed)
            for size in ("s", "m"):
                _, plan = pmfixture.build(SPEC, size, seed)
                got = {x for x, i in plan["ideas"].items() if i["state"] == "omitted"}
                self.assertEqual(got, omitted, (size, seed))  # the seed alone decides
            self.assertEqual(len(omitted), 3)
            self.assertTrue(any(KIND[x] == "passing" for x in omitted), seed)
            self.assertTrue(all(KIND[x] in ("clear", "passing") for x in omitted), seed)
            seen |= omitted
        self.assertGreaterEqual(len(seen), 6)
        passing = {x for x, k in KIND.items() if k == "passing"}
        self.assertLessEqual(len(passing), 5)  # so seeds 1-5 can omit each one
        self.assertLessEqual(passing, seen)

    def test_the_users_text_has_no_unplanted_idea(self):
        said = {i["said"] for i in SPEC["ideas"]}
        # The pilot's two: now planted ideas, word for word.
        self.assertIn("Product pages should be simple, a photo, the price and what is inside.", said)
        self.assertIn("The customer picks a slot when ordering and gets a reminder.", said)
        frags = unplanted(SPEC)
        self.assertGreater(len(frags), 20)
        keywords = [k for q in SPEC["questions"] for k in q["expected"] + q.get("forbidden", [])]
        for f in frags:
            n = " " + re.sub(r"[^0-9a-z]+", " ", normalize(f)) + " "
            self.assertEqual([m for m in REQUEST_MARKERS if f" {m} " in n], [], f)
            self.assertEqual([k for k in keywords if keyword_hit(normalize(f), k)], [], f)
        # status.md is not the user's text, but it carries none of the values either.
        for t in pmfixture.STATUS:
            self.assertEqual([k for k in keywords if keyword_hit(normalize(t), k)], [], t)
        # The new passing ideas are facts, not requests.
        for x in ("open-orders-limit", "prices-tax", "monday-closed"):
            n = " " + re.sub(r"[^0-9a-z]+", " ", normalize(IDEAS[x]["said"])) + " "
            self.assertEqual(KIND[x], "passing")
            self.assertEqual([m for m in REQUEST_MARKERS if f" {m} " in n], [], x)

    def test_changes_land_in_later_dumps(self):
        truth_dir = Path(tempfile.mkdtemp())
        try:
            truth = pmfixture.write(SPEC, "s", 3, truth_dir / "fx")
            ss = [s for s in truth["sentences"] if s.get("idea") == "pickup-slots" and s["kind"] in ("said", "change")]
            ss.sort(key=lambda s: s.get("step") or 0)
            self.assertEqual([s.get("step") for s in ss], [None, 1, 2])
            self.assertEqual([s["file"] for s in ss], sorted(s["file"] for s in ss))
            self.assertEqual(len({s["file"] for s in ss}), 3)
        finally:
            shutil.rmtree(truth_dir)


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
        """The value in force of each question, citing the sentence that sets it;
        `stale`: the value before it, citing that sentence."""
        out = []
        for q in self.truth["questions"]:
            vals = IDEAS[q["idea"]]["values"]
            k = len(vals) - 1 - (1 if stale and len(vals) > 1 else 0)
            s = self.sentence(kind="change", idea=q["idea"], step=k) if k else self.sentence(kind="said", idea=q["idea"])
            out.append({"id": q["id"], "value": vals[k], **self.cite(s)})
        return {"answers": out}

    def test_perfect_values(self):
        a = self.perfect_values()
        a["answers"][0]["id"] = " Q1"  # ids are matched loosely
        g = reader_grade.grade_value(self.fx, a, self.truth["questions"])
        self.assertEqual((g["value_pct"], g["confusions"], g["invented"]), (100.0, 0, 0))
        self.assertTrue(all(q["cites_idea"] and q["cites_in_force"] for q in g["questions"]))

    def test_a_stale_value_is_a_forbidden_hit(self):
        g = reader_grade.grade_value(self.fx, self.perfect_values(stale=True), self.truth["questions"])
        changed = [q for q in self.truth["questions"] if IDEAS[q["idea"]].get("change")]
        self.assertEqual(g["confusions"], len(changed))
        self.assertEqual(g["invented"], 0)
        bad = [q for q in g["questions"] if q["forbidden_hits"]]
        self.assertEqual(sorted(q["id"] for q in bad), sorted(q["id"] for q in changed))
        self.assertFalse(any(q["cites_in_force"] for q in bad))
        # A close distractor's value is a forbidden hit too.
        a = self.perfect_values()
        a["answers"][2]["value"] = "9 pm"
        g = reader_grade.grade_value(self.fx, a, self.truth["questions"])
        self.assertEqual((g["questions"][2]["score"], g["questions"][2]["confused"]), (0.0, True))

    def test_relative_and_twice_changed_values(self):
        qs = {q["idea"]: q for q in self.truth["questions"]}

        def score(idea, value):
            q = qs[idea]
            return reader_grade.keyword_verdict(value, q["expected"], q["forbidden"])

        self.assertEqual(score("pickup-slots", "20 minutes"), (1.0, []))
        self.assertEqual(score("pickup-slots", "30 minutes")[0], 0.0)  # the first change
        self.assertEqual(score("pickup-slots", "15 minutes")[0], 0.0)  # the first value
        self.assertEqual(score("pickup-slots", "10-minute slots")[0], 0.0)  # the distractor
        self.assertEqual(score("order-history", "24"), (1.0, []))  # twice as many as 12
        self.assertTrue(score("order-history", "24 (twice the 12)")[1])  # the history is a forbidden hit
        self.assertTrue(score("order-history", "25")[1])
        for v in ("7 pm", "19:00", "7pm"):  # one hour earlier than 8 pm
            self.assertEqual(score("preorder-cutoff", v), (1.0, []), v)
        for v in ("8 pm", "9 pm", "20:00"):
            self.assertTrue(score("preorder-cutoff", v)[1], v)
        self.assertEqual(score("open-orders-limit", "three"), (1.0, []))
        self.assertTrue(score("open-orders-limit", "five")[1])
        self.assertEqual(score("monday-closed", "Mondays"), (1.0, []))
        self.assertTrue(score("monday-closed", "Sunday")[1])

    def test_citing_the_first_change_is_not_the_value_in_force(self):
        a = self.perfect_values()
        q1 = next(x for x in a["answers"] if x["id"] == "q1")
        q1.update(self.cite(self.sentence(kind="change", idea="pickup-slots", step=1)))
        g = reader_grade.grade_value(self.fx, a, self.truth["questions"])
        q = next(x for x in g["questions"] if x["id"] == "q1")
        self.assertEqual((q["score"], q["cites_idea"], q["cites_in_force"]), (1.0, True, False))

    def ideas_answer(self, present=(), skip=(), report=()):
        """A v2 audit answer: every idea sentence of the user, `in_items` from the
        truth except `present` (marked present), `skip` left out, and `missing`
        = `report`."""
        ideas = []
        for x, i in self.truth["ideas"].items():
            if x in skip:
                continue
            c = self.cite(self.sentence(kind="said", idea=x))
            ideas.append({**c, "in_items": i["in_items"] or x in present, "discarded": i["state"] == "discarded"})
        missing = [self.cite(self.sentence(kind="said", idea=x)) for x in report]
        return {"ideas": ideas, "missing": missing}

    def test_the_ideas_diagnostic(self):
        a, b, c = self.truth["omitted"]
        g = reader_grade.grade_ideas(self.fx, self.ideas_answer(present=[b], skip=[a], report=[c]))
        self.assertEqual(g["omitted"][a], {"status": "not enumerated", "reported": False})
        self.assertEqual(g["omitted"][b], {"status": "marked present", "reported": False})
        self.assertEqual(g["omitted"][c], {"status": "marked missing", "reported": True})
        self.assertAlmostEqual(g["enumerated_pct"], 200.0 / 3)
        self.assertEqual((g["marked_present"], g["not_enumerated"], g["invalid"]), (1, 1, 0))
        n = len(self.truth["ideas"])
        self.assertAlmostEqual(g["coverage_pct"], 100.0 * (n - 1) / n)
        self.assertEqual(g["by_kind"][KIND[a]][1], sum(1 for k in KIND.values() if k == KIND[a]))
        # Found: every omitted idea enumerated as missing.
        g = reader_grade.grade_ideas(self.fx, self.ideas_answer(report=[a, b, c]))
        self.assertEqual((g["enumerated_pct"], g["marked_present"], g["coverage_pct"]), (100.0, 0, 100.0))
        # An invented entry is counted apart and never reaches the gate's inputs.
        ans = self.ideas_answer(report=[a, b, c])
        ans["ideas"].append({"quote": "Customers can tip the baker from the app.", "file": ".pm/dumps/2026-10-05.md",
                             "line": 3, "in_items": False})
        self.assertEqual(reader_grade.grade_ideas(self.fx, ans)["invalid"], 1)
        _, v1 = reader_grade.grade(self.fx, "audit", json.dumps(ans), "v1")
        _, v2 = reader_grade.grade(self.fx, "audit", json.dumps(ans), "v2")
        self.assertNotIn("ideas", v1)
        self.assertEqual(v2["ideas"]["enumerated_pct"], 100.0)
        for k in ("recall", "precision", "invented", "confusions", "discarded", "found"):
            self.assertEqual(v1[k], v2[k], k)  # `missing` is graded exactly as in v1
        # No `ideas` list: the diagnostic says so; `missing` still scores.
        _, g = reader_grade.grade(self.fx, "audit", json.dumps({"missing": ans["missing"]}), "v2")
        self.assertEqual((g["ideas"]["parsed"], g["ideas"]["enumerated_pct"], g["recall"]), (False, 0.0, 100.0))

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
        qs = cls.truth["questions"]
        cls.audit = rr.build_call("opus", "audit", sections, qs, rr.DEFAULT_READER_CMD)
        cls.value = rr.build_call("opus", "value", sections, qs, rr.DEFAULT_READER_CMD)
        cls.delegated = rr.build_call("opus+haiku", "audit", sections, qs, rr.DEFAULT_READER_CMD)
        cls.audit2 = rr.build_call("opus", "audit", sections, qs, rr.DEFAULT_READER_CMD, prompt="v2")
        cls.value2 = rr.build_call("opus", "value", sections, qs, rr.DEFAULT_READER_CMD, prompt="v2")
        cls.efforts = rr.build_call("opus+sonnet", "audit", sections, qs, rr.DEFAULT_READER_CMD, effort="high",
                                    prompt="v2", sub_effort="low")
        cls.audit_skill = rr.build_call("opus+haiku", "audit", sections, qs, rr.DEFAULT_READER_CMD, prompt="skill")
        cls.value_skill = rr.build_call("opus+haiku", "value", sections, qs, rr.DEFAULT_READER_CMD, prompt="skill")

    @classmethod
    def tearDownClass(cls):
        shutil.rmtree(cls.tmp)

    def grade(self, call, **knobs):
        argv, prompt = call
        env, stats = run_fake(self.root, prompt, argv[1:], **knobs)
        task = "value" if call in (self.value, self.value2, self.value_skill) else "audit"
        version = ("v2" if call in (self.audit2, self.value2, self.efforts)
                   else "skill" if call in (self.audit_skill, self.value_skill) else "v1")
        return reader_grade.grade(self.fx, task, env["result"], version)[1], env, stats

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

    def test_v2_answers_and_knobs(self):
        g, _, _ = self.grade(self.audit2)
        self.assertEqual((g["recall"], g["invented"], g["confusions"]), (100.0, 0, 0))
        d = g["ideas"]
        self.assertEqual((d["parsed"], d["enumerated_pct"], d["marked_present"], d["invalid"]), (True, 100.0, 0, 0))
        self.assertEqual(d["coverage_pct"], 100.0)  # it lists every idea sentence it finds
        g, _, _ = self.grade(self.value2)
        self.assertEqual((g["value_pct"], g["confusions"]), (100.0, 0))
        n = len(self.truth["omitted"])
        # Never enumerated: hypotheses 1 and 2.
        g, _, _ = self.grade(self.audit2, MISS=1)
        self.assertEqual((g["ideas"]["not_enumerated"], g["ideas"]["marked_present"]), (1, 0))
        self.assertAlmostEqual(g["recall"], 100.0 * (n - 1) / n)
        # Enumerated but marked present: a matching error.
        g, _, _ = self.grade(self.audit2, MARK_PRESENT=1)
        self.assertEqual((g["ideas"]["not_enumerated"], g["ideas"]["marked_present"]), (0, 1))
        self.assertAlmostEqual(g["recall"], 100.0 * (n - 1) / n)
        statuses = sorted(v["status"] for v in g["ideas"]["omitted"].values())
        self.assertEqual(statuses, ["marked missing"] * (n - 1) + ["marked present"])
        # v1 answers keep their shape whatever the knob.
        g, env, _ = self.grade(self.audit, MARK_PRESENT=1)
        self.assertEqual(g["recall"], 100.0)
        self.assertNotIn('"ideas"', env["result"])

    def test_skill_answers_and_knobs(self):
        g, env, stats = self.grade(self.audit_skill)
        self.assertEqual((g["recall"], g["invented"], g["confusions"]), (100.0, 0, 0))
        self.assertEqual(g["ideas"]["enumerated_pct"], 100.0)  # the skill's audit asks for the list
        self.assertEqual(sorted(env["modelUsage"]), ["claude-haiku-5-5", "claude-opus-5-5"])
        self.assertEqual((stats["agent_calls"], stats["main_dump_reads"]), (1, 0))
        g, _, _ = self.grade(self.audit_skill, MARK_PRESENT=1)
        self.assertEqual(g["ideas"]["marked_present"], 1)
        g, _, _ = self.grade(self.value_skill)
        self.assertEqual((g["value_pct"], g["confusions"]), (100.0, 0))
        g, _, _ = self.grade(self.value_skill, STALE=1)
        self.assertGreater(g["confusions"], 0)

    def test_effort_lands_in_the_transcripts(self):
        _, _, stats = self.grade(self.efforts)
        self.assertEqual((stats["effort_seen_main"], stats["effort_seen_sub"]), (["high"], ["low"]))
        _, _, stats = self.grade(self.delegated)
        self.assertEqual((stats["effort_seen_main"], stats["effort_seen_sub"]), (["medium"], ["medium"]))
        # The environment variable overrides both, as in Claude Code.
        argv, prompt = self.efforts
        tmp = Path(tempfile.mkdtemp())
        try:
            project = tmp / "project"
            shutil.copytree(self.root, project)
            env = dict(os.environ, CLAUDE_CONFIG_DIR=str(tmp / "cfg"), CLAUDE_CODE_EFFORT_LEVEL="max")
            subprocess.run([sys.executable, str(rr.FAKE_READER), *argv[1:]], input=prompt, cwd=project, env=env,
                           capture_output=True, text=True, check=True)
            stats = rr.transcript_stats(runner.claude_projects_dir(env) / runner.claude_project_key(project))
            self.assertEqual((stats["effort_seen_main"], stats["effort_seen_sub"]), (["max"], ["max"]))
        finally:
            shutil.rmtree(tmp)


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
        self.assertNotIn("effort", sub)  # no --sub-effort: the agent has no `effort`
        self.assertNotIn("--effort", argv)

    def test_sub_effort_goes_to_the_agents_json(self):
        sections = rr.prompt_sections()
        qs = SPEC["questions"]
        argv, _ = rr.build_call("opus+haiku", "audit", sections, qs, rr.DEFAULT_READER_CMD, effort="low",
                                sub_effort="high")
        sub = json.loads(argv[argv.index("--agents") + 1])[rr.SUBAGENT["name"]]
        self.assertEqual(sub["effort"], "high")
        self.assertEqual(argv[argv.index("--effort") + 1], "low")  # --effort is the main session's
        argv, _ = rr.build_call("opus", "audit", sections, qs, rr.DEFAULT_READER_CMD, sub_effort="high")
        self.assertNotIn("--agents", argv)
        self.assertNotIn("high", argv)
        self.assertEqual(rr.variant_of("opus", "v2", None, "high"), ("opus", "v2", None, None))

    def test_the_calls_never_get_the_effort_variable(self):
        os.environ[rr.EFFORT_ENV] = "low"
        try:
            work = Path(tempfile.mkdtemp())
            for dry in (False, True):
                env = rr.call_env(rr.Options(work, dry_run=dry), work)
                self.assertNotIn(rr.EFFORT_ENV, env)
                self.assertEqual(env["CLAUDE_CODE_DISABLE_AUTO_MEMORY"], "1")
            shutil.rmtree(work)
        finally:
            del os.environ[rr.EFFORT_ENV]
        self.assertNotIn(rr.EFFORT_ENV, (BENCH / "reader.py").read_text().replace("rr.EFFORT_ENV", ""))

    def test_run_names_carry_the_variant(self):
        self.assertEqual(rr.run_name(rr.variant_of("opus+haiku", "v2", None, "high"), "m", 3, "audit"),
                         "opus+haiku-v2-emain_default-esub_high-m-s3-audit")
        self.assertEqual(rr.run_name(rr.variant_of("opus+sonnet"), "s", 1, "value"),
                         "opus+sonnet-v1-emain_default-esub_default-s-s1-value")
        self.assertEqual(rr.run_name(rr.variant_of("opus", "v1", "high"), "l", 2, "audit"),
                         "opus-v1-emain_high-esub_none-l-s2-audit")


# The four v1 sections as the pilot sent them (sha256 of each, as
# prompt_sections returns it): prompt v1 must keep meaning the same text.
V1_SHA256 = {
    "Reader": "8179e7ebd2a3fe8f4d0455dc447ade3b811003d339b447dcfa04ea677e5fb048",
    "Audit task": "007378f3efc8f96215f68fdca9ad0f4623b6b5395a6197144ae2ce76318d9e1f",
    "Value task": "5a3dd5f3b1634bae0c18889fd6e0f8b838e2ad1cb6ddbb0a4351cb5db1ae2458",
    "Delegation": "1eb63f276038ba3b0ca50b93d74f4cfd7989883e5ecd43d62b8a0a453780ed26",
}
# Words of the fixture that the generic v2 prompt must not carry.
FIXTURE_WORDS = ["bakery", "cake", "croissant", "rosa", "oven", "loaf", "pickup", "notice", "crumb", "bread",
                 "dough", "cafe", "slot", "allergen", "stamp", "wholesale", "monday", "twice", "earlier", "half",
                 "hour", "minute"]


class Prompts(unittest.TestCase):
    def test_v1_is_unchanged(self):
        sections = rr.prompt_sections()
        self.assertEqual(set(rr.PROMPTS["v1"].values()), set(V1_SHA256))
        for name, sha in V1_SHA256.items():
            self.assertEqual(hashlib.sha256(sections[name].encode()).hexdigest(), sha, name)
        self.assertEqual(rr.DEFAULT_PROMPT, "v1")

    def test_v2_and_skill_are_generic(self):
        """v2, and the texts that ship (`skill`: the reader agent's body and the
        skill's task sections), carry nothing of the fixture."""
        sections = rr.prompt_sections()
        for version in ("v2", "skill"):
            text = "\n".join(sections[n] for n in dict.fromkeys(rr.PROMPTS[version].values()))
            words = set(re.sub(r"[^0-9a-z]+", " ", normalize(text)).split())
            for w in FIXTURE_WORDS:
                self.assertFalse({x for x in words if x.startswith(w)}, (version, w))
            numbers = {int(n) for n in re.findall(r"\d+", text)}
            self.assertEqual(numbers & spec_numbers(SPEC), set(), version)
            for d in SPEC["dumps"]:
                self.assertNotIn(d["date"], text, version)
            # Nothing of the spec's wording either: no sentence, no 4-word run of one.
            for i in SPEC["ideas"]:
                for _, _, _, t in pmfixture.sentences_of(i):
                    ws = reader_grade.norm_text(t).split()
                    for k in range(len(ws) - 3):
                        self.assertNotIn(" ".join(ws[k:k + 4]), reader_grade.norm_text(text), (version, t))

    def test_v2_asks_for_the_list_and_the_definition(self):
        sections = rr.prompt_sections()
        argv, prompt = rr.build_call("opus", "audit", sections, SPEC["questions"], rr.DEFAULT_READER_CMD, prompt="v2")
        self.assertIn('{"ideas"', prompt)
        self.assertIn('"in_items"', prompt)
        self.assertIn("plain statement of fact", prompt)
        self.assertNotIn(sections["Reader"], prompt)
        argv, prompt = rr.build_call("opus+haiku", "value", sections, SPEC["questions"], rr.DEFAULT_READER_CMD,
                                     prompt="v2")
        sub = json.loads(argv[argv.index("--agents") + 1])[rr.SUBAGENT["name"]]
        self.assertEqual(sub["prompt"], sections["Reader v2"])
        self.assertIn(sections["Value task v2"].split("{questions}")[0], prompt)
        self.assertNotEqual(rr.prompt_set_sha256(sections, "v1"), rr.prompt_set_sha256(sections, "v2"))

    def test_skill_takes_the_shipped_texts(self):
        """`--prompt skill`: the reader is the agent template's body and the tasks
        are the skill's sections, read here without the runner's helpers."""
        body = rr.PM_READER_PATH.read_text().split("\n---\n", 1)[1].strip()
        self.assertTrue(body.startswith("<!--"), body[:80])
        skill = rr.PM_SKILL_PATH.read_text()
        audit = skill.split("\n## Audit task\n", 1)[1].split("\n## Value task\n", 1)[0].strip()
        value = skill.split("\n## Value task\n", 1)[1].strip()
        self.assertIn("{questions}", value)
        sections = rr.prompt_sections()
        self.assertEqual(sections[rr.SHIPPED["reader"]], body)
        qs = SPEC["questions"]
        argv, prompt = rr.build_call("opus+haiku", "audit", sections, qs, rr.DEFAULT_READER_CMD, prompt="skill")
        self.assertEqual(rr.agent_prompt_of(argv), body)
        sub = json.loads(argv[argv.index("--agents") + 1])[rr.SUBAGENT["name"]]
        self.assertEqual((sub["model"], sub["tools"]), ("claude-haiku-5-5", ["Read", "Grep", "Glob"]))
        self.assertNotIn("effort", sub)
        self.assertTrue(prompt.rstrip("\n").endswith(audit))
        self.assertIn(rr.SUBAGENT["name"], prompt)  # the bench's Delegation in front
        self.assertNotIn(body, prompt)
        _, prompt = rr.build_call("opus+haiku", "value", sections, qs, rr.DEFAULT_READER_CMD, prompt="skill")
        self.assertIn(value.split("{questions}")[0], prompt)
        self.assertNotIn("{questions}", prompt)
        for q in qs:
            self.assertIn(f"- {q['id']}: {q['question']}", prompt)
        # A direct configuration reads with the same instructions.
        _, prompt = rr.build_call("opus", "audit", sections, qs, rr.DEFAULT_READER_CMD, prompt="skill")
        self.assertEqual(prompt, body + "\n\n" + audit + "\n")
        # The example entry of the skill (a `## ` line in a fence) is not a section.
        self.assertEqual(rr.markdown_sections("## A\n```\n## not one\n```\ntext\n## B\nx"),
                         {"A": "```\n## not one\n```\ntext", "B": "x"})
        shas = {rr.prompt_set_sha256(sections, p) for p in ("v1", "v2", "skill")}
        self.assertEqual(len(shas), 3)


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
            self.assertEqual((s["effort_seen_main"], s["effort_seen_sub"]), ([], []))
        finally:
            shutil.rmtree(tmp)

    def test_effort_seen(self):
        """`effort` of the assistant records and `output_config.effort` of the
        requests, as in Claude Code 2.1.318 sessions (bench/results/reader-pilot)."""
        tmp = Path(tempfile.mkdtemp())
        try:

            def request(effort, source="sdk", model="claude-opus-5-5"):
                return json.dumps({"type": "api-request", "querySource": source,
                                   "params": {"model": model, "output_config": {"effort": effort}}})

            def answer(tid, effort, **kw):
                r = json.loads(rec(tid, "Read", {"file_path": ".pm/items.md"}, **kw))
                r["effort"] = r["perTurnEffort"] = effort
                return json.dumps(r)

            call = json.loads(rec("t1", "Agent", {"subagent_type": "pm-reader", "effort": "max"}))
            call["effort"] = "high"
            main = [
                json.dumps({"type": "api-request-shape", "shape": {"tools": [{"input_schema": {
                    "properties": {"effort": {"type": "string"}}}}]}}),  # a schema, not an effort
                request("high"),
                json.dumps(call),
                answer("t2", "high"),
                request("low", source="agent:custom:pm-reader", model="claude-haiku-5-5"),  # inline subagent request
                json.dumps({"type": "api-request", "params": {"output_config": None}}),
            ]
            (tmp / "s1.jsonl").write_text("\n".join(main) + "\n")
            sub = tmp / "s1" / "subagents"
            sub.mkdir(parents=True)
            with gzip.open(sub / "agent-a.jsonl.gz", "wt") as f:
                f.write(request("medium", source="agent:custom:pm-reader") + "\n")
                f.write(answer("t3", "medium", sidechain=True, model="claude-haiku-5-5") + "\n")
            s = rr.transcript_stats(tmp)
            self.assertEqual(s["effort_seen_main"], ["high"])
            self.assertEqual(s["effort_seen_sub"], ["low", "medium"])
            self.assertEqual(s["agent_effort_args"], ["max"])
            self.assertEqual(s["agent_calls"], 1)
        finally:
            shutil.rmtree(tmp)


def agg(recall, value, invented=0.0, discarded=0):
    ms = lambda v: {"mean": v, "sd": 0.0}  # noqa: E731
    return {"seeds": 1, "discarded_total": discarded, "recall": ms(recall), "value_pct": ms(value),
            "invented_per_call": ms(invented)}


def V(config, prompt="v1", main=None, sub=None):
    return rr.variant_of(config, prompt, main, sub)


def N(config, prompt="v1", main=None, sub=None):
    return rr.variant_name(V(config, prompt, main, sub))


class Gate(unittest.TestCase):
    def test_verdicts(self):
        table = {
            V("opus"): {"m": agg(90, 100)},
            V("opus+sonnet"): {"m": agg(80, 90, invented=1.0, discarded=1)},
            V("opus+haiku"): {"m": agg(79, 100)},
            V("sonnet"): {"m": agg(10, 10)},
        }
        g = reader_report.gate(table)["prompts"]["v1"]
        self.assertTrue(g["sane"])
        self.assertEqual(g["variants"][N("opus+sonnet")]["verdict"], "viable")
        self.assertEqual(g["variants"][N("opus+haiku")]["verdict"], "not viable")
        self.assertFalse(g["variants"][N("opus+haiku")]["criteria"]["recall"]["ok"])
        self.assertNotIn(N("sonnet"), g["variants"])  # direct configs are diagnosis only
        table[V("opus+haiku")]["m"] = agg(100, 100, invented=1.5)
        self.assertEqual(reader_report.gate(table)["prompts"]["v1"]["variants"][N("opus+haiku")]["verdict"],
                         "not viable")
        table[V("opus+haiku")]["m"] = agg(100, 100, discarded=2)
        self.assertEqual(reader_report.gate(table)["prompts"]["v1"]["variants"][N("opus+haiku")]["verdict"],
                         "not viable")

    def test_ill_posed_and_missing(self):
        g = reader_report.gate({V("opus"): {"m": agg(70, 100)}, V("opus+haiku"): {"m": agg(70, 100)}})
        self.assertFalse(g["prompts"]["v1"]["sane"])
        self.assertIn("ill-posed", g["prompts"]["v1"]["variants"][N("opus+haiku")]["verdict"])
        g = reader_report.gate({V("opus"): {"s": agg(100, 100)}, V("opus+haiku"): {"s": agg(100, 100)}})
        self.assertIn("not judged", g["prompts"]["v1"]["variants"][N("opus+haiku")]["verdict"])

    def test_one_gate_per_prompt_version(self):
        table = {
            V("opus"): {"m": agg(100, 100)},
            V("opus+haiku"): {"m": agg(95, 100)},
            V("opus", "v2"): {"m": agg(85, 100)},
            V("opus", "v2", main="high"): {"m": agg(10, 10)},  # not the baseline: its main effort is not the default
            V("opus+haiku", "v2"): {"m": agg(80, 100)},
            V("opus+haiku", "v2", sub="high"): {"m": agg(74, 100)},
            V("opus+sonnet", "v2", main="low", sub="high"): {"m": agg(76, 100)},
            V("opus+sonnet", "v3"): {"m": agg(100, 100)},  # a version with no baseline
        }
        g = reader_report.gate(table)["prompts"]
        self.assertEqual(sorted(g), ["v1", "v2", "v3"])
        self.assertEqual((g["v1"]["baseline_recall"], g["v2"]["baseline_recall"]), (100, 85))
        self.assertEqual(g["v2"]["baseline_variant"], N("opus", "v2"))
        self.assertEqual(list(g["v1"]["variants"]), [N("opus+haiku")])
        v2 = g["v2"]["variants"]
        self.assertEqual(v2[N("opus+haiku", "v2")]["verdict"], "viable")  # 80 >= 85 - 10
        self.assertEqual(v2[N("opus+haiku", "v2", sub="high")]["verdict"], "not viable")  # 74 < 85 - 10
        self.assertEqual(v2[N("opus+sonnet", "v2", main="low", sub="high")]["verdict"], "viable")
        self.assertIn("not judged", g["v3"]["variants"][N("opus+sonnet", "v3")]["verdict"])
        self.assertIsNone(g["v3"]["baseline_recall"])


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
            self.assertIn(f"| {c} | v1 | default | {'default' if '+' in c else '–'} | s |", md)
            row = self.report["table"][N(c)]["sizes"]["s"]
            self.assertEqual((row["recall"]["mean"], row["value_pct"]["mean"]), (100.0, 100.0))
            self.assertEqual((row["invented_per_call"]["mean"], row["confusions"]["mean"]), (0.0, 0.0))
            self.assertEqual(row["cost_usd"]["mean"], 0.0)
            for t in rr.TASKS:
                r = json.loads((self.out / "runs" / rr.run_name(V(c), "s", 1, t) / "run.json").read_text())
                self.assertEqual(r["run"], rr.run_name(V(c), "s", 1, t))
                self.assertEqual(r["key"], {"config": c, "prompt": "v1", "effort_main": None, "effort_sub": None,
                                            "size": "s", "seed": 1, "task": t})
                self.assertEqual((r["prompt_version"], r["prompt_sha256"]), ("v1", rr.sha256(r["prompt"])))
                self.assertEqual(r["spec"], {"version": 2, "sha256": SPEC["_sha256"]})
                self.assertEqual(r["effort_seen_main"], ["medium"])
                self.assertEqual(r["effort_seen_sub"], ["medium"] if "+" in c else [])
                self.assertEqual(r["agent_prompt_sha256"] is not None, "+" in c)
                self.assertNotIn("ideas", r["grades"])
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

    def test_folders_of_other_runs_are_refused(self):
        for over, why in (({"dry_run": False}, "real"), ({"spec": {"version": 1, "sha256": "x"}}, "another version")):
            out = self.tmp / f"refuse-{why.replace(' ', '-')}"
            src = next((self.out / "runs").glob("*/run.json"))
            dst = out / "runs" / src.parent.name / "run.json"
            dst.parent.mkdir(parents=True)
            dst.write_text(json.dumps({**json.loads(src.read_text()), **over}))
            logs = []
            self.assertEqual(reader.main(["--dry-run", "--sizes", "s", "--out", str(out)], log=logs.append), 2)
            self.assertIn(why, logs[0])
        # Unknown effort levels and prompt versions never reach a run folder's name.
        for bad in (["--effort", "huge"], ["--sub-effort", "huge"], ["--prompt", "v9"]):
            with self.assertRaises(SystemExit), contextlib.redirect_stderr(io.StringIO()):
                reader.parse(bad)


class SkillDryRun(unittest.TestCase):
    """`--prompt skill` end to end: what ships is judged against `opus` reading
    with the same texts."""

    def test_the_skill_gets_its_own_gate(self):
        tmp = Path(tempfile.mkdtemp())
        try:
            out = tmp / "skill"
            logs = []
            code = reader.main(["--dry-run", "--prompt", "skill", "--configs", "opus,opus+haiku", "--sizes", "m",
                                "--seeds", "1", "--out", str(out)], log=logs.append)
            self.assertEqual(code, 0, "\n".join(logs))
            g = json.loads((out / "report.json").read_text())["gate"]["prompts"]
            self.assertEqual(list(g), ["skill"])
            self.assertEqual((g["skill"]["baseline_variant"], g["skill"]["baseline_recall"]),
                             (N("opus", "skill"), 100.0))
            self.assertEqual(g["skill"]["variants"][N("opus+haiku", "skill")]["verdict"], "viable")
            body = rr.PM_READER_PATH.read_text().split("\n---\n", 1)[1].strip()
            r = json.loads((out / "runs" / rr.run_name(V("opus+haiku", "skill"), "m", 1, "audit") / "run.json")
                           .read_text())
            self.assertEqual(r["prompt_version"], "skill")
            self.assertEqual(r["agent_prompt_sha256"], rr.sha256(body))
            self.assertEqual(r["prompt_set_sha256"], rr.prompt_set_sha256(rr.prompt_sections(), "skill"))
            self.assertEqual(r["grades"]["ideas"]["enumerated_pct"], 100.0)
            self.assertIn("### Prompt skill", (out / "report.md").read_text())
        finally:
            shutil.rmtree(tmp)


class MixedVariants(unittest.TestCase):
    """Four invocations into one --out, as the second batch does: the report
    keeps every variant apart and judges each prompt version on its own."""

    @classmethod
    def setUpClass(cls):
        cls.tmp = Path(tempfile.mkdtemp())
        cls.out = cls.tmp / "mixed"
        cls.logs = []
        base = ["--dry-run", "--sizes", "m", "--seeds", "1", "--out", str(cls.out)]

        def run(*args, **knobs):
            env = {f"FAKE_READER_{k}": str(v) for k, v in knobs.items()}
            os.environ.update(env)
            try:
                assert reader.main([*base, *args], log=cls.logs.append) == 0, "\n".join(cls.logs)
            finally:
                for k in env:
                    del os.environ[k]

        run("--prompt", "v1", "--configs", "opus,opus+haiku")
        os.environ[rr.EFFORT_ENV] = "low"  # stripped from the calls
        try:
            run("--prompt", "v2", "--configs", "opus+sonnet,opus+haiku", "--sub-effort", "high", "--tasks", "audit")
        finally:
            del os.environ[rr.EFFORT_ENV]
        run("--prompt", "v2", "--configs", "opus")
        run("--prompt", "v2", "--configs", "opus+haiku", MARK_PRESENT=1)
        cls.report = json.loads((cls.out / "report.json").read_text())
        cls.md = (cls.out / "report.md").read_text()

    @classmethod
    def tearDownClass(cls):
        shutil.rmtree(cls.tmp)

    def test_runs_are_keyed_by_variant(self):
        names = sorted(p.name for p in (self.out / "runs").iterdir())
        self.assertEqual(len(names), 2 * 2 + 2 + 2 + 2)
        self.assertIn("opus+haiku-v2-emain_default-esub_high-m-s1-audit", names)
        self.assertIn("opus+haiku-v2-emain_default-esub_default-m-s1-audit", names)
        self.assertIn("opus+haiku-v1-emain_default-esub_default-m-s1-audit", names)
        self.assertIn("opus-v2-emain_default-esub_none-m-s1-value", names)
        r = json.loads((self.out / "runs" / "opus+haiku-v2-emain_default-esub_high-m-s1-audit" / "run.json").read_text())
        sub = json.loads(r["argv"][r["argv"].index("--agents") + 1])[rr.SUBAGENT["name"]]
        self.assertEqual((sub["effort"], r["effort_sub"], r["effort_main"]), ("high", "high", None))
        # CLAUDE_CODE_EFFORT_LEVEL=low was set, and stripped: the subagent ran at high.
        self.assertEqual((r["effort_seen_main"], r["effort_seen_sub"]), (["medium"], ["high"]))
        self.assertTrue(any(rr.EFFORT_ENV in x for x in self.logs))
        self.assertEqual(r["grades"]["ideas"]["enumerated_pct"], 100.0)

    def test_the_report_groups_by_variant(self):
        self.assertEqual(sorted(self.report["table"]), sorted([
            N("opus"), N("opus+haiku"), N("opus+sonnet", "v2", sub="high"), N("opus+haiku", "v2", sub="high"),
            N("opus", "v2"), N("opus+haiku", "v2"),
        ]))
        t = self.report["table"]
        self.assertEqual(t[N("opus+haiku", "v2")]["sizes"]["m"]["recall"]["mean"], 200.0 / 3)
        self.assertEqual(t[N("opus+haiku")]["sizes"]["m"]["recall"]["mean"], 100.0)
        self.assertEqual(t[N("opus+haiku", "v2")]["sizes"]["m"]["ideas_marked_present"]["mean"], 1.0)
        self.assertNotIn("ideas_enumerated_pct", t[N("opus+haiku")]["sizes"]["m"])
        self.assertIn("| opus+haiku | v2 | default | high | m |", self.md)
        self.assertIn("| opus+haiku | v2 | default | default | m |", self.md)
        self.assertIn("Omitted ideas enumerated %", self.md)
        self.assertIn("Enumerated but marked present", self.md)
        sections = rr.prompt_sections()
        for p in ("v1", "v2"):
            self.assertIn(f"- Prompt {p}: sha256 `{rr.prompt_set_sha256(sections, p)}`", self.md)
        self.assertNotIn("## Warnings", self.md)

    def test_one_gate_per_prompt_version(self):
        g = self.report["gate"]["prompts"]
        self.assertEqual(sorted(g), ["v1", "v2"])
        self.assertEqual(list(g["v1"]["variants"]), [N("opus+haiku")])
        self.assertEqual(g["v1"]["variants"][N("opus+haiku")]["verdict"], "viable")
        v2 = g["v2"]["variants"]
        self.assertEqual(sorted(v2), sorted([N("opus+sonnet", "v2", sub="high"), N("opus+haiku", "v2", sub="high"),
                                             N("opus+haiku", "v2")]))
        self.assertEqual(v2[N("opus+haiku", "v2")]["verdict"], "not viable")  # 67 < 100 - 10
        self.assertIn("lacks a task", v2[N("opus+haiku", "v2", sub="high")]["verdict"])  # audit only
        self.assertIn("### Prompt v1", self.md)
        self.assertIn("### Prompt v2", self.md)


if __name__ == "__main__":
    unittest.main()
