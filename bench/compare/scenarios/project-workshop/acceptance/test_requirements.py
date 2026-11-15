"""Hidden acceptance checks for the workshop's requirements R1-R14.

Run from the repo root: python3 -m unittest test_requirements
Every check runs the code through the fixed interface; none reads the source.
"""

import csv
import io
import unittest
from datetime import datetime, timedelta, timezone
from unittest import mock
from zoneinfo import ZoneInfo

from workshop import bookings, export, notify
from workshop.models import Booking, BookingError, Person, WaitlistEntry, WorkshopClass
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


def reminders_for(store, booking_id, now):
    return [r for r in notify.reminders_due(store, now) if r.booking_id == booking_id]


class RequirementsTest(unittest.TestCase):
    def setUp(self):
        notify.OUTBOX.clear()

    def tearDown(self):
        notify.OUTBOX.clear()

    def test_r01_twelve_per_class(self):
        s = fresh_store()
        start = local(2027, 3, 10)  # Wednesday
        add_class(s, "c1", start)
        now = start - timedelta(days=3)
        for i in range(12):
            b = bookings.book(s, "c1", person(i), now)
            self.assertEqual(b.status, "confirmed", f"seat {i + 1} of 12")
        with self.assertRaises(BookingError):
            bookings.book(s, "c1", person(12), now)

    def test_r02_closed_on_tuesdays(self):
        s = fresh_store()
        now = local(2027, 3, 6, 12)  # Saturday
        # Monday and Wednesday are open days.
        add_class(s, "mon", local(2027, 3, 8))
        add_class(s, "wed", local(2027, 3, 10))
        self.assertEqual(bookings.book(s, "mon", person(1), now).status, "confirmed")
        self.assertEqual(bookings.book(s, "wed", person(2), now).status, "confirmed")
        # Tuesday in Mexico City, in the morning and late in the evening (already
        # Wednesday in UTC): booking either is refused.
        for cid, start in (("tue-am", local(2027, 3, 9, 10)), ("tue-pm", local(2027, 3, 9, 19, 30))):
            try:
                add_class(s, cid, start)
            except BookingError:
                continue
            with self.assertRaises(BookingError, msg=cid):
                bookings.book(s, cid, person(3), now)

    def test_r03_money_in_cents(self):
        s = fresh_store()
        start = local(2027, 3, 11)  # Thursday
        cls = add_class(s, "c1", start, price=33335)
        b = direct_booking(s, "c1", start - timedelta(days=6), price=33335)
        early = bookings.refund_amount(b, cls, start - timedelta(days=5))
        self.assertIs(type(early), int)
        self.assertEqual(early, 33335)
        late = bookings.refund_amount(b, cls, start - timedelta(hours=1))
        self.assertIs(type(late), int)
        self.assertTrue(0 <= late <= 33335)

    def test_r04_member_discount(self):
        s = fresh_store()
        start = local(2027, 3, 12)  # Friday
        cls = add_class(s, "c1", start)
        member = Person("Marta Ruiz", "marta@example.org", member=True)
        guest = Person("Pablo Gil", "pablo@example.org")
        self.assertEqual(bookings.price_for(cls, member), 34000)
        self.assertEqual(bookings.price_for(cls, guest), 40000)
        now = start - timedelta(days=3)
        self.assertEqual(bookings.book(s, "c1", member, now).price, 34000)
        self.assertEqual(bookings.book(s, "c1", guest, now).price, 40000)

    def test_r05_two_hours_before(self):
        s = fresh_store()
        start = local(2027, 3, 10)  # Wednesday 10:00
        add_class(s, "c1", start)
        with self.assertRaises(BookingError):
            bookings.book(s, "c1", person(1), start - timedelta(minutes=119))
        b = bookings.book(s, "c1", person(2), start - timedelta(minutes=121))
        self.assertEqual(b.status, "confirmed")

    def test_r06_full_refund_24h(self):
        s = fresh_store()
        start = local(2027, 3, 11)  # Thursday
        cls = add_class(s, "c1", start)
        b = direct_booking(s, "c1", start - timedelta(days=6))
        self.assertEqual(bookings.refund_amount(b, cls, start - timedelta(hours=25)), PRICE)
        self.assertLess(bookings.refund_amount(b, cls, start - timedelta(hours=23)), PRICE)

    def test_r07_waitlist_of_ten(self):
        s = fresh_store()
        start = local(2027, 3, 11)  # Thursday
        add_class(s, "c1", start)
        now = start - timedelta(days=3)
        fill_class(s, "c1", 12, now - timedelta(days=1))
        for i in range(10):
            bookings.join_waitlist(s, "c1", person(i), now + timedelta(minutes=i))
        with self.assertRaises(BookingError):
            bookings.join_waitlist(s, "c1", person(10), now + timedelta(minutes=10))

    def test_r08_email_only_reminders(self):
        s = fresh_store()
        start = local(2027, 3, 11)  # Thursday
        add_class(s, "c1", start)
        direct_booking(s, "c1", start - timedelta(days=5), phone="+5215550001111")
        channels = set()
        original_sms = getattr(notify, "send_sms", None)
        patch = (mock.patch.object(notify, "send_sms", side_effect=original_sms)
                 if original_sms is not None else mock.MagicMock())
        with patch as sms_spy:
            t = start - timedelta(hours=48)
            while t < start:
                channels.update(r.channel for r in notify.reminders_due(s, t))
                notify.send_due(s, t)
                t += timedelta(minutes=1)
        if original_sms is not None:
            self.assertEqual(sms_spy.call_count, 0, "an SMS was sent")
        self.assertNotIn("sms", channels)
        self.assertFalse([m for m in notify.OUTBOX if m["channel"] == "sms"])
        emails = [m for m in notify.OUTBOX if m["channel"] == "email" and m["to"] == "ana@example.org"]
        self.assertTrue(emails, "no reminder email went out in the two days before the class")

    def test_r09_reminder_day_before_at_six_pm(self):
        start = local(2027, 3, 11, 10)  # Thursday 10:00
        expected = local(2027, 3, 10, 18)  # Wednesday 18:00
        early = fresh_store()
        add_class(early, "c1", start)
        b = direct_booking(early, "c1", start - timedelta(days=6))
        self.assertEqual(reminders_for(early, b.id, local(2027, 3, 8, 12)), [])
        s = fresh_store()
        add_class(s, "c1", start)
        b = direct_booking(s, "c1", start - timedelta(days=6))
        due = reminders_for(s, b.id, expected)
        self.assertTrue(due, "no reminder due the day before at 18:00")
        self.assertTrue(any(r.send_at == expected for r in due), [r.send_at for r in due])

    def test_r10_mexico_city_time(self):
        s = fresh_store()
        # Given in UTC: 02:00 UTC Friday is 20:00 Thursday in Mexico City.
        start = datetime(2027, 3, 12, 2, 0, tzinfo=UTC)
        expected = local(2027, 3, 10, 18)  # Wednesday 18:00 in Mexico City
        add_class(s, "c1", start)
        b = direct_booking(s, "c1", start - timedelta(days=6))
        due = reminders_for(s, b.id, expected)
        self.assertTrue(due, "no reminder due at 18:00 Mexico City time the day before")
        self.assertTrue(any(r.send_at == expected for r in due), [r.send_at for r in due])

    def test_r11_names_case_insensitive(self):
        s = fresh_store()
        start = local(2027, 3, 12)  # Friday
        add_class(s, "c1", start)
        now = start - timedelta(days=3)
        first = bookings.book(s, "c1", Person("Ana López", "ana@example.org"), now)
        self.assertEqual(first.name, "Ana López")
        with self.assertRaises(BookingError):
            bookings.book(s, "c1", Person("ANA LÓPEZ", "ana.lopez@trabajo.example"), now)
        other = bookings.book(s, "c1", Person("luis PÉREZ", "luis@example.org"), now)
        self.assertEqual(other.name, "luis PÉREZ")

    def test_r12_csv_semicolons(self):
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
        rows = list(csv.reader(lines, delimiter=";"))
        self.assertEqual(len(rows), 3, rows)
        header = rows[0]
        self.assertGreaterEqual(len(header), 6, header)
        for row in rows[1:]:
            self.assertEqual(len(row), len(header), row)
        self.assertTrue(any("ana@example.org" in row for row in rows[1:]), rows)
        self.assertTrue(any("Luis Pérez" in row for row in rows[1:]), rows)

    def test_r13_freed_seat_offered_for_two_hours(self):
        s = fresh_store()
        start = local(2027, 3, 11)  # Thursday
        add_class(s, "c1", start)
        now = start - timedelta(days=3)
        fill_class(s, "c1", 12, now - timedelta(days=2))
        waiting = s.waitlist_for("c1")
        waiting.append(WaitlistEntry("c1", "Primera Fila", "primera@example.org", False, now - timedelta(hours=5)))
        waiting.append(WaitlistEntry("c1", "Segunda Fila", "segunda@example.org", False, now - timedelta(hours=4)))
        bookings.cancel(s, s.bookings[0].id, now)
        offered = [b for b in s.bookings if b.class_id == "c1" and b.status == "offered"]
        self.assertEqual([b.name for b in offered], ["Primera Fila"])
        self.assertEqual(offered[0].confirm_by, now + timedelta(hours=2))
        # promote_waitlist() itself, on a class with a free seat.
        t = fresh_store()
        add_class(t, "c2", start)
        t.waitlist_for("c2").append(WaitlistEntry("c2", "Primera Fila", "primera@example.org", False, now))
        t.waitlist_for("c2").append(WaitlistEntry("c2", "Segunda Fila", "segunda@example.org", False, now))
        offer = bookings.promote_waitlist(t, "c2", now)
        self.assertIsNotNone(offer)
        self.assertEqual((offer.name, offer.status), ("Primera Fila", "offered"))
        self.assertEqual(offer.confirm_by, now + timedelta(hours=2))
        self.assertIn(offer, t.bookings)

    def test_r14_booking_window(self):
        s = fresh_store()
        now = local(2027, 3, 3, 12)  # Wednesday 12:00; every class below is at 10:00
        add_class(s, "d7", local(2027, 3, 10))  # Wednesday, 7 days ahead (6 d 22 h)
        add_class(s, "d8", local(2027, 3, 11))  # Thursday, 8 days ahead (7 d 22 h)
        add_class(s, "d14", local(2027, 3, 17))  # Wednesday, 14 days ahead (13 d 22 h)
        add_class(s, "d15", local(2027, 3, 18))  # Thursday, 15 days ahead (14 d 22 h)
        member = Person("Marta Ruiz", "marta@example.org", member=True)
        guest = Person("Pablo Gil", "pablo@example.org")
        self.assertEqual(bookings.book(s, "d7", guest, now).status, "confirmed")
        with self.assertRaises(BookingError):
            bookings.book(s, "d8", guest, now)
        self.assertEqual(bookings.book(s, "d8", member, now).status, "confirmed")
        self.assertEqual(bookings.book(s, "d14", member, now).status, "confirmed")
        with self.assertRaises(BookingError):
            bookings.book(s, "d15", member, now)


if __name__ == "__main__":
    unittest.main()
