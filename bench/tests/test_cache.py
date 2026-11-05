"""The prompt cache break measure (benchlib/cache.py), and that the runner
saves the Claude Code sessions of each run."""

import gzip
import json
import shutil
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from benchlib import cache, runner  # noqa: E402


def req(mid, read, created, ts="2026-11-04T10:00:00.000Z", sidechain=False):
    """One assistant response line, like Claude Code's."""
    r = {"type": "assistant", "timestamp": ts,
         "message": {"id": mid, "role": "assistant", "content": [{"type": "text", "text": "ok"}],
                     "usage": {"cache_read_input_tokens": read, "cache_creation_input_tokens": created}}}
    if sidechain:
        r["isSidechain"] = True
    return json.dumps(r)


BOUNDARY = json.dumps({"type": "system", "subtype": "compact_boundary",
                       "compactMetadata": {"trigger": "auto", "preTokens": 150000}})


class Breaks(unittest.TestCase):
    def test_counts_breaks_like_the_measured_script(self):
        lines = [
            json.dumps({"type": "user", "message": {"role": "user", "content": "hola"}}),
            req("m1", 0, 15000),
            # Two blocks of the same response: a single request.
            req("m1", 0, 15000),
            req("m2", 15000, 3000),            # grows: fine
            req("m3", 12800, 6000),            # read 5,200 less than 18,000: break
            req("m4", 18800, 100),             # fine
            req("m5", 17000, 1900),            # 1,900 less: under the threshold, not counted
            req("s1", 0, 40000, sidechain=True),  # a subagent: not counted
            req("m6", 18900, 50),              # fine (the subagent did not move the prefix)
            BOUNDARY,
            req("m7", 12000, 9000),            # after compacting it starts from zero
            # A message that mentions compact_boundary does not restart the count.
            json.dumps({"type": "user", "message": {"role": "user", "content": "what is compact_boundary?"}}),
            req("m8", 3000, 18000),            # 21,000 → 3,000: break
            "{no es json",
        ]
        c = cache.session_breaks(lines)
        self.assertEqual(c["requests"], 8)
        self.assertEqual(c["breaks"], 2)
        self.assertEqual(c["tokens_rewritten"], 5200 + 18000)
        self.assertEqual((c["idle_breaks"], c["idle_tokens"]), (0, 0))

    def test_breaks_after_a_pause_are_counted_apart(self):
        lines = [
            req("a", 0, 20000, ts="2026-11-04T10:00:00.000Z"),
            req("b", 20000, 1000, ts="2026-11-04T10:01:00.000Z"),
            # 18 minutes later: the cache expired.
            req("c", 0, 21500, ts="2026-11-04T10:19:00.000Z"),
            req("d", 12000, 9600, ts="2026-11-04T10:19:10.000Z"),
        ]
        c = cache.session_breaks(lines)
        self.assertEqual((c["breaks"], c["tokens_rewritten"]), (2, 21000 + 9500))
        self.assertEqual((c["idle_breaks"], c["idle_tokens"]), (1, 21000))

    def test_reads_gzip_and_sums_sessions(self):
        tmp = Path(tempfile.mkdtemp())
        try:
            (tmp / "a.jsonl").write_text("\n".join([req("1", 0, 9000), req("2", 4000, 6000)]) + "\n")
            with gzip.open(tmp / "b.jsonl.gz", "wt") as f:
                f.write("\n".join([req("1", 0, 9000), req("2", 9000, 10)]) + "\n")
            c = cache.breaks([tmp / "a.jsonl", tmp / "b.jsonl.gz"])
            self.assertEqual((c["requests"], c["breaks"], c["tokens_rewritten"]), (4, 1, 5000))
        finally:
            shutil.rmtree(tmp)

    def test_runner_saves_the_sessions_of_its_repo_compressed(self):
        tmp = Path(tempfile.mkdtemp())
        try:
            repo = tmp / "kontext-bench-kontext-inject-s1-a_b.c" / "repo"
            repo.mkdir(parents=True)
            env = {"CLAUDE_CONFIG_DIR": str(tmp / "cfg")}
            key = runner.claude_project_key(repo)
            # Claude Code changes everything that is not a letter or a digit to `-`.
            self.assertNotIn("_", key)
            self.assertNotIn(".", key)
            src = Path(env["CLAUDE_CONFIG_DIR"]) / "projects" / key
            (src / "s1" / "subagents").mkdir(parents=True)
            body = "\n".join([req("1", 0, 9000), req("2", 4000, 6000)]) * 200 + "\n"
            (src / "s1.jsonl").write_text(body)
            (src / "s1" / "subagents" / "agent-x.jsonl").write_text(req("9", 0, 1) + "\n")
            # The ones of another run, in the same CLAUDE_CONFIG_DIR, are not copied.
            other = Path(env["CLAUDE_CONFIG_DIR"]) / "projects" / (key + "-otra")
            other.mkdir()
            (other / "x.jsonl").write_text(req("1", 0, 1) + "\n")
            dest = tmp / "run" / "transcripts" / "claude"
            saved = runner.save_claude_sessions(env, repo, dest)
            self.assertEqual(saved["files"], 2)
            self.assertTrue((dest / "s1.jsonl.gz").is_file())
            self.assertTrue((dest / "s1" / "subagents" / "agent-x.jsonl.gz").is_file())
            self.assertLess(saved["bytes"], len(body) / 5)
            with gzip.open(dest / "s1.jsonl.gz", "rt") as f:
                self.assertEqual(f.read(), body)
            # No sessions (the folder does not exist): nothing, no error.
            self.assertEqual(runner.save_claude_sessions(env, tmp / "nada", tmp / "d2"), {"files": 0, "bytes": 0})
        finally:
            shutil.rmtree(tmp)


if __name__ == "__main__":
    unittest.main()
