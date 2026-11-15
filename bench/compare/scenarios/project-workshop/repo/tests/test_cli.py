import io
import tempfile
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path

from workshop import cli, schedule
from workshop.storage import Store


class CliTest(unittest.TestCase):
    def test_list_prints_upcoming(self):
        s = Store()
        schedule.add_class(s, "c1", "Wheel throwing", "Rosa", datetime.now(timezone.utc) + timedelta(days=2))
        with tempfile.TemporaryDirectory() as d:
            p = Path(d) / "s.json"
            s.save(p)
            out = io.StringIO()
            self.assertEqual(cli.main(["--store", str(p), "list"], out=out), 0)
        self.assertIn("Wheel throwing", out.getvalue())

    def test_list_empty_store(self):
        with tempfile.TemporaryDirectory() as d:
            out = io.StringIO()
            self.assertEqual(cli.main(["--store", str(Path(d) / "none.json"), "list"], out=out), 0)
        self.assertEqual(out.getvalue(), "")
