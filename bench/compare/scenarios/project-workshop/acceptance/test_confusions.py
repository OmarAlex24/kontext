"""Confusion probes for the workshop task.

Each probe PASSES only when the code behaves with one specific wrong value: a
distractor from the pottery studio's email, or a value the user replaced or
discarded. A failed requirement whose probe passes counts as a confusion.

Run from the repo root: python3 -m unittest test_confusions
Every probe runs the code through the fixed interface; none reads the source.
"""

import csv
import unittest
from datetime import datetime, timedelta, timezone
from unittest import mock
from zoneinfo import ZoneInfo

from workshop import bookings, export, notify
from workshop.models import Booking, BookingError, Person, WorkshopClass
from workshop.storage import Store

MX = ZoneInfo("America/Mexico_City")
UTC = timezone.utc
PRICE = 40000  # 400 pesos, in cents


def local(year, month, day, hour=10, minute=0):
    """A Mexico City wall-clock time. March 2027: Mon 1, 8, 15; Tue 2, 9, 16."""
    return datetime(year, month, day, hour, minute, tzinfo=MX)


def fresh_store():
    notify.OUTBOX.clear()
    return Store()


def add_class(store, class_id, start, price=PRICE):
    return store.add_class(WorkshopClass(class_id, f"Class {class_id}", "Rosa", start, price=price))


def person(i, member=False):
    return Person(f"Persona {i}", f"persona{i}@example.org", member=member)


def fill_class(store, class_id, seats, booked_at):
    """Puts `seats` confirmed bookings in the class without going through book()."""
    for i in range(seats):
        store.bookings.append(
            Booking(store.next_id(), class_id, f"Asistente {i}", f"asistente{i}@example.org",
                    False, booked_at, price=PRICE)
        )


def direct_booking(store, class_id, booked_at, price=PRICE, **kw):
    b = Booking(store.next_id(), class_id, kw.pop("name", "Ana López"), kw.pop("email", "ana@example.org"),
                kw.pop("member", False), booked_at, price=price, **kw)
    store.bookings.append(b)
    return b


def reminder_store():
    """A Thursday class with one confirmed booking from someone who left a phone."""
    s = fresh_store()
    start = local(2027, 3, 11)  # Thursday
    add_class(s, "c1", start)
    direct_booking(s, "c1", start - timedelta(days=5), phone="+5215550001111")
    return s, start


def minutes_before(start, hours=48):
    t = start - timedelta(hours=hours)
    while t < start:
        yield t
        t += timedelta(minutes=1)


class ConfusionsTest(unittest.TestCase):
    def setUp(self):
        notify.OUTBOX.clear()

    def tearDown(self):
        notify.OUTBOX.clear()

    def assert_full_refund_limit(self, hours):
        """Full refund one hour before the `hours` limit, less than full one hour after it."""
        s = fresh_store()
        start = local(2027, 3, 11)  # Thursday
        cls = add_class(s, "c1", start)
        b = direct_booking(s, "c1", start - timedelta(days=6))
        self.assertEqual(bookings.refund_amount(b, cls, start - timedelta(hours=hours + 1)), PRICE)
        self.assertLess(bookings.refund_amount(b, cls, start - timedelta(hours=hours - 1)), PRICE)

    def test_c01_r01_ten_per_class(self):
        s = fresh_store()
        start = local(2027, 3, 10)  # Wednesday
        add_class(s, "c1", start)
        now = start - timedelta(days=3)
        for i in range(10):
            self.assertEqual(bookings.book(s, "c1", person(i), now).status, "confirmed")
        with self.assertRaises(BookingError):
            bookings.book(s, "c1", person(10), now)

    def test_c02_r02_closed_on_mondays(self):
        s = fresh_store()
        now = local(2027, 3, 6, 12)  # Saturday
        add_class(s, "mon", local(2027, 3, 8))
        add_class(s, "tue", local(2027, 3, 9))
        with self.assertRaises(BookingError):
            bookings.book(s, "mon", person(1), now)
        self.assertEqual(bookings.book(s, "tue", person(2), now).status, "confirmed")

    def test_c03_r06_refund_48h(self):
        self.assert_full_refund_limit(48)

    def test_c04_r06_refund_72h(self):
        self.assert_full_refund_limit(72)

    def test_c05_r07_waitlist_of_five(self):
        s = fresh_store()
        start = local(2027, 3, 11)  # Thursday
        add_class(s, "c1", start)
        now = start - timedelta(days=3)
        fill_class(s, "c1", 12, now - timedelta(days=1))
        for i in range(5):
            bookings.join_waitlist(s, "c1", person(i), now + timedelta(minutes=i))
        with self.assertRaises(BookingError):
            bookings.join_waitlist(s, "c1", person(5), now + timedelta(minutes=5))

    def test_c06_r08_sms_reminders(self):
        s, start = reminder_store()
        channels = set()
        for t in minutes_before(start):
            channels.update(r.channel for r in notify.reminders_due(s, t))
        self.assertIn("sms", channels, "no reminder goes out by SMS")

    def test_c07_r08_sms_sent(self):
        s, start = reminder_store()
        original_sms = notify.send_sms
        with mock.patch.object(notify, "send_sms", side_effect=original_sms) as sms_spy:
            for t in minutes_before(start):
                notify.send_due(s, t)
        sms_in_outbox = [m for m in notify.OUTBOX if m["channel"] == "sms"]
        self.assertTrue(sms_spy.call_count or sms_in_outbox, "no SMS was sent with the reminders")

    def test_c08_r12_comma_csv(self):
        s = fresh_store()
        add_class(s, "c1", local(2027, 3, 10))
        add_class(s, "c2", local(2027, 3, 11))
        direct_booking(s, "c1", local(2027, 3, 5, 12))
        direct_booking(s, "c2", local(2027, 3, 5, 13), name="Luis Pérez", email="luis@example.org",
                       price=34000, member=True)
        text = export.export_csv(s)
        lines = [ln for ln in text.splitlines() if ln.strip()]
        if lines and lines[0].lower().startswith("sep="):
            lines = lines[1:]
        self.assertTrue(lines, "empty export")
        self.assertNotIn(";", lines[0], "the header uses semicolons")
        rows = list(csv.reader(lines, delimiter=","))
        self.assertEqual(len(rows), 3, rows)
        self.assertGreaterEqual(len(rows[0]), 6, rows[0])
        for row in rows[1:]:
            self.assertEqual(len(row), len(rows[0]), row)
        self.assertTrue(any("Luis Pérez" in row for row in rows[1:]), rows)


if __name__ == "__main__":
    unittest.main()
