"""Hidden checks of S2: every caller of the shared rounding rounds a half cent
up, and no caller grew its own copy of the rounding. Run with PYTHONPATH=<repo>
and BENCH_REPO=<repo>."""

import ast
import os
import unittest
from decimal import Decimal
from pathlib import Path

import invoices
import money
import refunds
import reports

REPO = Path(os.environ.get("BENCH_REPO", "."))
CALLERS = ("invoices.py", "refunds.py", "reports.py")


class Hidden(unittest.TestCase):
    def test_h1_round_half_up(self):
        self.assertEqual(money.round_half(Decimal("2.345")), Decimal("2.35"))
        self.assertEqual(money.round_half(Decimal("-2.345")), Decimal("-2.35"))
        self.assertEqual(money.round_half(Decimal("2.344")), Decimal("2.34"))
        self.assertEqual(money.round_half(Decimal("2.346")), Decimal("2.35"))

    def test_h2_invoices_round_half_up(self):
        # 3 × 0.125 = 0.375 -> 0.38; 16 % of 0.50 = 0.08 exactly.
        self.assertEqual(invoices.line_total(3, Decimal("0.125")), Decimal("0.38"))
        self.assertEqual(invoices.vat(Decimal("0.5")), Decimal("0.08"))

    def test_h3_refunds_round_half_up(self):
        self.assertEqual(refunds.partial_refund(Decimal("10.25"), Decimal("0.5")), Decimal("5.13"))
        self.assertEqual(refunds.refund_lines([(4, Decimal("0.125"))], [1]), [Decimal("0.13")])

    def test_h4_reports_round_half_up(self):
        self.assertEqual(reports.average_ticket([Decimal("1.00"), Decimal("1.01")]), Decimal("1.01"))
        self.assertEqual(reports.share(Decimal("1"), Decimal("8")), Decimal("12.50"))
        self.assertEqual(reports.share(Decimal("1"), Decimal("16")), Decimal("6.25"))
        self.assertEqual(reports.share(Decimal("1"), Decimal("32")), Decimal("3.13"))

    def test_h5_no_copy_of_the_rounding(self):
        # No caller rounds on its own: no quantize, no ROUND_* mode, no round().
        for name in CALLERS:
            tree = ast.parse((REPO / name).read_text())
            for node in ast.walk(tree):
                if isinstance(node, ast.Attribute):
                    self.assertNotEqual(node.attr, "quantize", f"{name} calls quantize")
                if isinstance(node, ast.Name):
                    self.assertFalse(node.id.startswith("ROUND_"), f"{name} uses {node.id}")
                if isinstance(node, ast.alias):
                    self.assertFalse(node.name.startswith("ROUND_"), f"{name} imports {node.name}")
                if isinstance(node, ast.Call) and isinstance(node.func, ast.Name):
                    self.assertNotEqual(node.func.id, "round", f"{name} calls round()")

    def test_h6_callers_still_use_money(self):
        for name in CALLERS:
            self.assertIn("round_half", (REPO / name).read_text(), f"{name} no longer uses round_half")


if __name__ == "__main__":
    unittest.main()
