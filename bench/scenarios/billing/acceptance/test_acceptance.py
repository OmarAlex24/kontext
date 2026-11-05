"""Acceptance tests for the billing scenario. The harness copies them to
tests/ at the end of the run: the agent doesn't see them."""

import unittest
from datetime import date

from billing.invoices import Invoice


class Reminder(unittest.TestCase):
    def test_text_lists_invoices_and_total(self):
        from billing.reminders import reminder_text

        fs = [
            Invoice("Ruiz Hardware Store", "A-10", 125050, date(2026, 10, 1)),
            Invoice("Ruiz Hardware Store", "A-11", 4950, date(2026, 10, 5)),
        ]
        text = reminder_text("Ruiz Hardware Store", fs)
        self.assertIn("Ruiz Hardware Store", text)
        self.assertIn("A-10", text)
        self.assertIn("$1,300.00", text)

    def test_sends_one_email_per_customer_with_postmark(self):
        from billing.mail import Postmark
        from billing.reminders import send_reminders

        pm = Postmark()
        fs = [
            Invoice("Ruiz", "A-1", 100, date(2026, 1, 1)),
            Invoice("Ruiz", "A-2", 100, date(2026, 1, 2)),
            Invoice("Soto", "B-1", 100, date(2026, 1, 3)),
            Invoice("Soto", "B-2", 100, date(2099, 1, 1)),
        ]
        n = send_reminders(fs, date(2026, 6, 1), pm, addresses={"Ruiz": "r@x.mx", "Soto": "s@x.mx"})
        self.assertEqual(n, 2)
        self.assertEqual(len(pm.sent), 2)


class Payments(unittest.TestCase):
    def test_partial_payment_lowers_balance(self):
        from billing.payments import apply_payment

        self.assertEqual(apply_payment(10000, 2550), 7450)

    def test_payment_larger_than_balance_leaves_zero(self):
        from billing.payments import apply_payment

        self.assertEqual(apply_payment(10000, 12000), 0)

    def test_integer_result(self):
        from billing.payments import apply_payment

        self.assertIsInstance(apply_payment(999, 1), int)


if __name__ == "__main__":
    unittest.main()
