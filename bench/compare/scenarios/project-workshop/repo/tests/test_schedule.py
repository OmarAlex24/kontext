import unittest
from datetime import datetime, timedelta, timezone

from workshop import schedule
from workshop.models import Booking, BookingError
from workshop.storage import Store

NOW = datetime(2027, 3, 1, 12, 0, tzinfo=timezone.utc)


class ScheduleTest(unittest.TestCase):
    def setUp(self):
        self.s = Store()

    def test_start_needs_timezone(self):
        with self.assertRaises(BookingError):
            schedule.add_class(self.s, "c1", "A", "Rosa", datetime(2027, 3, 3, 17, 0))

    def test_upcoming_skips_started(self):
        schedule.add_class(self.s, "old", "Old", "Rosa", NOW - timedelta(hours=1))
        schedule.add_class(self.s, "new", "New", "Rosa", NOW + timedelta(hours=1))
        self.assertEqual([c.id for c in schedule.upcoming(self.s, NOW)], ["new"])

    def test_upcoming_soonest_first(self):
        schedule.add_class(self.s, "c1", "Alpha", "Rosa", NOW + timedelta(days=3))
        schedule.add_class(self.s, "c2", "Beta", "Rosa", NOW + timedelta(days=1))
        self.assertEqual([c.id for c in schedule.upcoming(self.s, NOW)], ["c2", "c1"])

    def test_seats_taken(self):
        schedule.add_class(self.s, "c1", "A", "Rosa", NOW + timedelta(days=1))
        self.s.bookings.append(Booking("b1", "c1", "Ana", "a@example.org", False, NOW))
        self.s.bookings.append(Booking("b2", "c1", "Bo", "b@example.org", False, NOW, status="offered"))
        self.s.bookings.append(Booking("b3", "c1", "Cy", "c@example.org", False, NOW, status="cancelled"))
        self.assertEqual(schedule.seats_taken(self.s, "c1"), 2)
