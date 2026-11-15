import unittest
from datetime import date

import invoices


class DueDateTest(unittest.TestCase):
    def test_thirty_days(self):
        self.assertEqual(invoices.due_date(date(2027, 3, 1), 30), date(2027, 3, 31))

    def test_negative_days(self):
        with self.assertRaises(ValueError):
            invoices.due_date(date(2027, 3, 1), -1)


class BusinessDaysTest(unittest.TestCase):
    def test_weekend(self):
        self.assertFalse(invoices.is_business_day(date(2027, 3, 6)))
        self.assertTrue(invoices.is_business_day(date(2027, 3, 8)))

    def test_month_end(self):
        self.assertEqual(invoices.month_end(date(2027, 2, 10)), date(2027, 2, 28))
