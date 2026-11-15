import unittest
from decimal import Decimal

import refunds


class RefundTest(unittest.TestCase):
    def test_half_of_an_odd_amount(self):
        # Half of $10.25 is $5.125: the customer gets $5.13.
        self.assertEqual(refunds.partial_refund(Decimal("10.25"), Decimal("0.5")), Decimal("5.13"))

    def test_whole_refund(self):
        self.assertEqual(refunds.partial_refund(Decimal("10.25"), 1), Decimal("10.25"))

    def test_fraction_out_of_range(self):
        with self.assertRaises(ValueError):
            refunds.partial_refund(Decimal("10"), Decimal("1.5"))
