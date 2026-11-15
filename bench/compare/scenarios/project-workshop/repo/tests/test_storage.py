import tempfile
import unittest
from datetime import datetime, timezone
from pathlib import Path

from workshop.models import Booking, BookingError, WorkshopClass
from workshop.storage import Store

T = datetime(2027, 3, 3, 17, 0, tzinfo=timezone.utc)


class StoreTest(unittest.TestCase):
    def test_add_and_get_class(self):
        s = Store()
        s.add_class(WorkshopClass("c1", "Wheel throwing", "Rosa", T))
        self.assertEqual(s.get_class("c1").title, "Wheel throwing")
        with self.assertRaises(BookingError):
            s.get_class("nope")

    def test_duplicate_class(self):
        s = Store()
        s.add_class(WorkshopClass("c1", "A", "Rosa", T))
        with self.assertRaises(BookingError):
            s.add_class(WorkshopClass("c1", "B", "Rosa", T))

    def test_ids_are_unique(self):
        s = Store()
        self.assertNotEqual(s.next_id(), s.next_id())

    def test_bookings_for_skips_cancelled(self):
        s = Store()
        s.bookings.append(Booking("b1", "c1", "Ana", "ana@example.org", False, T))
        s.bookings.append(Booking("b2", "c1", "Luis", "luis@example.org", False, T, status="cancelled"))
        self.assertEqual([b.id for b in s.bookings_for("c1")], ["b1"])
        self.assertEqual(len(s.bookings_for("c1", active_only=False)), 2)

    def test_save_and_load(self):
        s = Store()
        s.add_class(WorkshopClass("c1", "Glazing", "Rosa", T))
        s.bookings.append(Booking("b1", "c1", "Ana", "ana@example.org", True, T))
        with tempfile.TemporaryDirectory() as d:
            p = Path(d) / "s.json"
            s.save(p)
            t = Store.load(p)
        self.assertEqual(t.get_class("c1").start, T)
        self.assertEqual(t.get_booking("b1").booked_at, T)
        self.assertTrue(t.get_booking("b1").member)
