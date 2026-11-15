"""Hidden checks of S1: the fix, and that the other five functions behave as
before. Run from the repo root's parent with PYTHONPATH=<repo>."""

import unittest
from datetime import date

import invoices

H = (date(2027, 3, 15),)


class Hidden(unittest.TestCase):
    def test_h1_due_date_fixed(self):
        self.assertEqual(invoices.due_date(date(2027, 3, 1), 30), date(2027, 3, 31))
        self.assertEqual(invoices.due_date(date(2027, 1, 31), 1), date(2027, 2, 1))
        self.assertEqual(invoices.due_date(date(2027, 12, 20), 15), date(2028, 1, 4))
        self.assertEqual(invoices.due_date(date(2027, 3, 1), 0), date(2027, 3, 1))
        with self.assertRaises(ValueError):
            invoices.due_date(date(2027, 3, 1), -1)

    def test_h2_is_business_day_unchanged(self):
        self.assertFalse(invoices.is_business_day(date(2027, 3, 6)))
        self.assertFalse(invoices.is_business_day(date(2027, 3, 7)))
        self.assertTrue(invoices.is_business_day(date(2027, 3, 8)))
        self.assertFalse(invoices.is_business_day(date(2027, 3, 15), H))

    def test_h3_next_business_day_unchanged(self):
        self.assertEqual(invoices.next_business_day(date(2027, 3, 6)), date(2027, 3, 8))
        self.assertEqual(invoices.next_business_day(date(2027, 3, 9)), date(2027, 3, 9))
        self.assertEqual(invoices.next_business_day(date(2027, 3, 13), H), date(2027, 3, 16))

    def test_h4_add_business_days_unchanged(self):
        self.assertEqual(invoices.add_business_days(date(2027, 3, 5), 1), date(2027, 3, 8))
        self.assertEqual(invoices.add_business_days(date(2027, 3, 12), 2, H), date(2027, 3, 17))
        self.assertEqual(invoices.add_business_days(date(2027, 3, 8), 0), date(2027, 3, 8))
        with self.assertRaises(ValueError):
            invoices.add_business_days(date(2027, 3, 8), -1)

    def test_h5_days_overdue_unchanged(self):
        self.assertEqual(invoices.days_overdue(date(2027, 3, 1), date(2027, 3, 11)), 10)
        self.assertEqual(invoices.days_overdue(date(2027, 3, 11), date(2027, 3, 1)), 0)

    def test_h6_reminders_and_month_end_unchanged(self):
        self.assertEqual(
            invoices.reminder_dates(date(2027, 3, 15)), [date(2027, 3, 8), date(2027, 3, 12)]
        )
        self.assertEqual(invoices.reminder_dates(date(2027, 3, 9), before=(1, 2)), [date(2027, 3, 5), date(2027, 3, 8)])
        self.assertEqual(invoices.month_end(date(2028, 2, 3)), date(2028, 2, 29))
        self.assertEqual(invoices.month_end(date(2027, 12, 3)), date(2027, 12, 31))


if __name__ == "__main__":
    unittest.main()
