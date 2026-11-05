"""Behavior tests for the long-memory scenario. The harness copies them to
tests/ at the end of the run: the agent doesn't see them.

Each one depends on a fact that was only said in the conversation (class B) or
that changed later (class C, the one in force counts):

- late-fee rate: 1.5 % monthly on day 1, 1.8 % from day 2 (distractor: the 2 % of the 2023 draft);
- grace days: 4 (distractor: 7 from the 2023 policy);
- never sent on a Sunday (distractor: the North branch doesn't send on Saturdays);
- sending time: 9:40 (distractor: the 9:30 cash cut-off);
- threshold: 3,250 pesos on day 1, 4,100 from day 2 (distractor: 41,000 for legal);
- extension: Ruiz Hardware Store until the 15th of each month (distractor: Ríos Hardware Store until the 20th).
"""

import unittest
from datetime import date


class LateFee(unittest.TestCase):
    def fee(self, *a):
        from billing.fees import late_fee

        return late_fee(*a)

    def test_rate_in_force_one_month(self):
        # 34 days overdue = 30 days after the 4 grace days: one month at 1.8 %.
        self.assertAlmostEqual(self.fee(100000, 34), 1800, delta=1)

    def test_grace_days(self):
        self.assertEqual(self.fee(100000, 4), 0)
        self.assertEqual(self.fee(100000, 0), 0)
        # 9 days = 5 chargeable: 300000 × 1.8 % × 5 / 30 = 900.
        self.assertAlmostEqual(self.fee(300000, 9), 900, delta=1)

    def test_integer(self):
        self.assertIsInstance(self.fee(123457, 40), int)


class ScheduleSend(unittest.TestCase):
    def schedule(self, d):
        from billing.sending import schedule_send

        return schedule_send(d)

    def test_agreed_time(self):
        r = self.schedule(date(2026, 11, 4))  # Wednesday
        self.assertEqual((r.date(), r.hour, r.minute), (date(2026, 11, 4), 9, 40))

    def test_sunday_moves_to_monday(self):
        r = self.schedule(date(2026, 11, 8))  # Sunday
        self.assertEqual((r.date(), r.hour, r.minute), (date(2026, 11, 9), 9, 40))

    def test_saturday_is_sent(self):
        r = self.schedule(date(2026, 11, 7))  # Saturday
        self.assertEqual(r.date(), date(2026, 11, 7))


class ShouldRemind(unittest.TestCase):
    def should(self, *a):
        from billing.reminders import should_remind

        return bool(should_remind(*a))

    def test_threshold_in_force(self):
        today = date(2026, 11, 20)
        self.assertFalse(self.should("Centauro Paints", 400000, today))  # 4,000 < 4,100
        self.assertFalse(self.should("Centauro Paints", 410000, today))  # has to be over 4,100
        self.assertTrue(self.should("Centauro Paints", 420000, today))

    def test_ruiz_extension(self):
        self.assertFalse(self.should("Ruiz Hardware Store", 900000, date(2026, 11, 10)))
        self.assertFalse(self.should("Ruiz Hardware Store", 900000, date(2026, 11, 15)))
        self.assertTrue(self.should("Ruiz Hardware Store", 900000, date(2026, 11, 16)))

    def test_rios_has_no_extension(self):
        self.assertTrue(self.should("Ríos Hardware Store", 900000, date(2026, 11, 17)))


if __name__ == "__main__":
    unittest.main()
