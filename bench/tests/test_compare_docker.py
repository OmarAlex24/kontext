"""The comparison benchmark against real containers, model-free. Needs Docker
and the images from bench/compare/images/build.sh; runs only with
BENCH_DOCKER_TESTS=1 (it takes a few minutes):

    BENCH_DOCKER_TESTS=1 python3 -m unittest bench.tests.test_compare_docker

- a dry cell end to end with every docker call recorded: anything that carries
  a hidden check goes only to the verification container;
- no image holds a hidden check;
- a key passed by name reaches the container but never a docker argv, and a
  run folder with a planted key is cleaned by the scan.
"""

import gzip
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

BENCH = Path(__file__).resolve().parent.parent
COMPARE = BENCH / "compare"
sys.path.insert(0, str(BENCH))
sys.path.insert(0, str(COMPARE))

from benchlib import container  # noqa: E402
from complib import cell, keys, script  # noqa: E402
from complib.arms import ARM_NAMES  # noqa: E402

ENABLED = os.environ.get("BENCH_DOCKER_TESTS") == "1"
CANARY = "sk-ant-canary-" + "f00dfeed" * 4
# The checks' code. Their names do appear in the results (which check passed).
CODE = [b"def test_r01_", b"def test_r14_", b"def test_c01_", b"def test_c08_", b"def test_h1_", b"def test_h6_"]
# What may not reach an agent container at all: the code, and the names too.
MARKERS = CODE + [b"test_r01_twelve_per_class", b"test_r14_booking_window"]
IMAGES = ["bench-base:latest", "bench-sidecar:latest", "bench-verify:latest"] + [f"bench-{a}:latest" for a in ARM_NAMES]


class RecordingDocker(container.Docker):
    """Every call, with whether its stdin carried a hidden check and the names
    of the secrets its process environment held."""

    def __init__(self):
        super().__init__(log=lambda *a: None)
        self.carrying = []
        self.with_secrets = []

    def run(self, argv, input=None, secret_env=None, timeout=None, check=True, text=True):
        self.with_secrets.append((list(argv), sorted(secret_env or {})))
        if input is not None:
            data = input if isinstance(input, bytes) else input.encode()
            if any(m in data for m in MARKERS):
                self.carrying.append(list(argv))
        return super().run(argv, input=input, secret_env=secret_env, timeout=timeout, check=check, text=text)


@unittest.skipUnless(ENABLED, "set BENCH_DOCKER_TESTS=1 (needs Docker and the bench images)")
class HiddenChecksReachOnlyTheVerifier(unittest.TestCase):
    def run_cell(self, task, arm, fill_scale=0.02):
        d = RecordingDocker()
        out = Path(tempfile.mkdtemp(prefix="bench-cmp-test-"))
        cfg = cell.CellConfig(dry_run=True, fill_scale=fill_scale, measure=False, docker=d, log=lambda *a: None)
        r = cell.Cell(script.load(task), arm, 1, out / "run", cfg).run()
        return d, r, out

    def check(self, task, arm, n_checks):
        d, r, out = self.run_cell(task, arm)
        self.assertEqual(r["errors"], [], json.dumps(r["errors"])[:2000])
        self.assertTrue(d.carrying, "no docker call carried a check: the test would prove nothing")
        for argv in d.carrying:
            target = [a for a in argv if a.startswith("bench-") and "-verify" in a]
            self.assertTrue(target, f"a hidden check went to {argv[:8]}")
        # The run's own files keep no copy of the checks' code either.
        for p in out.rglob("*"):
            if p.is_file():
                data = p.read_bytes()
                if p.suffix == ".gz":
                    data = gzip.decompress(data)
                for m in CODE:
                    self.assertNotIn(m, data, p)
        last = r["checks"][max(r["checks"], key=int)]
        tests = {t: v for mod in last.values() for t, v in mod["tests"].items() if t.startswith(("test_r", "test_h"))}
        self.assertEqual(len(tests), n_checks)

    def test_smoke_kontext(self):
        # Sidecar, day 3 on machine B, the settle and the handoff.
        d, r, _ = self.run_cell("smoke", "kontext")
        self.assertEqual(r["errors"], [], json.dumps(r["errors"])[:2000])
        self.assertFalse(d.carrying)

    def test_workshop_plain(self):
        self.check("L", "plain", 14)

    def test_small_task_mem0(self):
        self.check("S1", "mem0", 6)


@unittest.skipUnless(ENABLED, "set BENCH_DOCKER_TESTS=1 (needs Docker and the bench images)")
class KeysThroughACell(unittest.TestCase):
    """A dry cell with a canary forced in as the arm's key (no model is called:
    the agent is still fake_agent.py): the key reaches every exec into the
    machines and nothing else, and no file of the run holds it."""

    def test_canary(self):
        arm = cell.ARMS["plain"]

        d = RecordingDocker()
        out = Path(tempfile.mkdtemp(prefix="bench-cmp-key-"))
        try:
            arm.secret_names = lambda dry_run: ["ANTHROPIC_API_KEY"]
            cfg = cell.CellConfig(dry_run=True, measure=True, docker=d, log=lambda *a: None)
            cfg.keys = {"ANTHROPIC_API_KEY": CANARY}
            c = cell.Cell(script.load("smoke"), "plain", 1, out / "run", cfg)
            r = c.run()
        finally:
            del arm.secret_names
        self.assertEqual(r["errors"], [], json.dumps(r["errors"])[:2000])
        agent_calls = 0
        for argv, names in d.with_secrets:
            self.assertFalse(any(CANARY in a for a in argv), argv[:6])
            if argv[1] == "exec" and any(a.endswith(("-a", "-b")) and a.startswith("bench-") for a in argv):
                self.assertEqual(names, ["ANTHROPIC_API_KEY"], argv[:8])
                self.assertIn("ANTHROPIC_API_KEY", argv)
                agent_calls += "fake_agent.py" in " ".join(argv)
            else:
                self.assertEqual(names, [], f"a key went to {argv[:8]}")
        self.assertGreater(agent_calls, 0)
        # smoke reaches machine B (day 3), where the key must arrive the same way.
        self.assertTrue(any(a[1] == "exec" and any(x.endswith("-b") for x in a) for a, _ in d.with_secrets))
        for p in out.rglob("*"):
            if p.is_file():
                data = p.read_bytes()
                if p.suffix == ".gz":
                    data = gzip.decompress(data)
                self.assertNotIn(CANARY.encode(), data, p)


@unittest.skipUnless(ENABLED, "set BENCH_DOCKER_TESTS=1 (needs Docker and the bench images)")
class ImagesHoldNoCheck(unittest.TestCase):
    def test_no_image_holds_a_check(self):
        pattern = "|".join(m.decode() for m in CODE)
        for image in IMAGES:
            # A check planted in /planted proves the search would find one.
            r = subprocess.run(
                [container.DOCKER, "run", "--rm", "--network", "none", "--user", "0", "--entrypoint", "sh", image, "-c",
                 f"echo '{CODE[0].decode()}x' > /planted; grep -rlE '{pattern}' / --exclude-dir=proc --exclude-dir=sys --exclude-dir=dev 2>/dev/null; true"],
                capture_output=True, text=True, timeout=600,
            )  # fmt: skip
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertEqual(r.stdout.split(), ["/planted"], f"{image}: {r.stdout.strip()}")


@unittest.skipUnless(ENABLED, "set BENCH_DOCKER_TESTS=1 (needs Docker and the bench images)")
class KeysByName(unittest.TestCase):
    def test_key_reaches_the_container_but_no_argv(self):
        d = container.Docker(log=lambda *a: None)
        run_id = "keytest-" + os.urandom(3).hex()
        name = f"bench-{run_id}-a"
        try:
            d.run(container.argv_network_create(f"bench-{run_id}-net", run_id))
            d.run(container.argv_volume_create(f"bench-{run_id}-remotes", run_id))
            m = container.Machine(
                d, name, "bench-plain:latest", f"bench-{run_id}-net", f"bench-{run_id}-remotes", run_id, "machine-a",
                secret_env={"ANTHROPIC_API_KEY": CANARY},
            )  # fmt: skip
            m.start()
            r = m.exec(["sh", "-c", 'printf %s "$ANTHROPIC_API_KEY" | sha256sum'], check=False)
            import hashlib

            self.assertEqual(r.stdout.split()[0], hashlib.sha256(CANARY.encode()).hexdigest())
            for argv in d.calls:
                self.assertFalse(any(CANARY in a for a in argv), argv)
            # The key never lands in the container's stored config.
            insp = d.run([container.DOCKER, "inspect", name]).stdout
            self.assertNotIn(CANARY, insp)
            self.assertNotIn("ANTHROPIC_API_KEY", insp)
        finally:
            container.cleanup_run(d, run_id)

    def test_planted_key_is_removed_by_the_scan(self):
        with tempfile.TemporaryDirectory() as tmp:
            Path(tmp, "transcript.jsonl").write_text(f'{{"text": "{CANARY}"}}\n')
            Path(tmp, "clean.txt").write_text("nothing")
            hits = keys.scan(tmp, {"ANTHROPIC_API_KEY": CANARY})
            self.assertEqual([Path(h[0]).name for h in hits], ["transcript.jsonl"])


if __name__ == "__main__":
    unittest.main()
