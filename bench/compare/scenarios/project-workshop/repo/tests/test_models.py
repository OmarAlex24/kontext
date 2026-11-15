import unittest
from datetime import datetime, timezone

from workshop.models import Booking, BookingError, Person


class ModelsTest(unittest.TestCase):
    def test_booking_defaults(self):
        b = Booking("b1", "c1", "Ana", "ana@example.org", False, datetime(2027, 3, 1, tzinfo=timezone.utc))
        self.assertEqual(b.status, "confirmed")
        self.assertIsNone(b.cancelled_at)

    def test_person_defaults(self):
        self.assertFalse(Person("Ana", "ana@example.org").member)

    def test_booking_error_is_value_error(self):
        self.assertTrue(issubclass(BookingError, ValueError))
