import unittest
from decimal import Decimal

import invoices


class InvoiceTest(unittest.TestCase):
    def test_line_total(self):
        self.assertEqual(invoices.line_total(3, Decimal("2.50")), Decimal("7.50"))

    def test_invoice_total(self):
        self.assertEqual(invoices.invoice_total([(1, Decimal("100"))]), Decimal("116.00"))
