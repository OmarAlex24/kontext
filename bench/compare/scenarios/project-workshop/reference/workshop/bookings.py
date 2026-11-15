"""Booking, prices, cancelling, refunds and the waitlist.

The signatures are fixed: the rest of the code and the tests call them as they
are. Every function that refuses something raises BookingError.

Money is always an int number of cents.
"""

from datetime import datetime, timedelta
from zoneinfo import ZoneInfo

from .models import Booking, BookingError, Person, WaitlistEntry, WorkshopClass  # noqa: F401

TZ = ZoneInfo("America/Mexico_City")
CAPACITY = 12
CLOSED_WEEKDAY = 1  # Monday is 0: the space is closed on Tuesdays
MEMBER_DISCOUNT_PERCENT = 15
MIN_HOURS_BEFORE_START = 2
FULL_REFUND_HOURS = 24
WAITLIST_SIZE = 10
CONFIRM_HOURS = 2
MEMBER_DAYS_AHEAD = 14
OTHERS_DAYS_AHEAD = 7


def _same_name(a: str, b: str) -> bool:
    return a.casefold() == b.casefold()


def _active(store, class_id):
    return [b for b in store.bookings_for(class_id) if b.status in ("confirmed", "offered")]


def book(store, class_id: str, person: Person, now: datetime) -> Booking:
    """Books one seat in a class for `person` at time `now` and returns the
    confirmed Booking (stored in `store.bookings`), with its price set."""
    cls = store.get_class(class_id)
    if cls.start.astimezone(TZ).weekday() == CLOSED_WEEKDAY:
        raise BookingError("there are no classes on Tuesdays")
    if now > cls.start - timedelta(hours=MIN_HOURS_BEFORE_START):
        raise BookingError(f"bookings close {MIN_HOURS_BEFORE_START} hours before the class")
    days_ahead = MEMBER_DAYS_AHEAD if person.member else OTHERS_DAYS_AHEAD
    if cls.start - now > timedelta(days=days_ahead):
        raise BookingError(f"this class opens for booking {days_ahead} days before it starts")
    for b in _active(store, class_id):
        if _same_name(b.name, person.name):
            # Someone offered a seat from the waitlist confirms it by booking.
            if b.status == "offered" and b.confirm_by is not None and now <= b.confirm_by:
                b.status = "confirmed"
                b.confirm_by = None
                return b
            raise BookingError(f"{person.name} already has a seat in this class")
    if len(_active(store, class_id)) >= CAPACITY:
        raise BookingError("the class is full")
    booking = Booking(
        store.next_id(),
        class_id,
        person.name,
        person.email,
        person.member,
        now,
        price=price_for(cls, person),
        phone=person.phone,
    )
    store.bookings.append(booking)
    return booking


def price_for(cls: WorkshopClass, person: Person):
    """What `person` pays for one seat in `cls`."""
    price = int(cls.price or 0)
    if person.member:
        return (price * (100 - MEMBER_DISCOUNT_PERCENT) + 50) // 100
    return price


def cancel(store, booking_id: str, now: datetime) -> Booking:
    """Cancels a booking at time `now` and returns it. A seat that frees up
    goes through promote_waitlist()."""
    booking = store.get_booking(booking_id)
    if booking.status == "cancelled":
        raise BookingError("this booking is already cancelled")
    cls = store.get_class(booking.class_id)
    if now >= cls.start:
        raise BookingError("the class has already started")
    booking.status = "cancelled"
    booking.cancelled_at = now
    promote_waitlist(store, booking.class_id, now)
    return booking


def refund_amount(booking: Booking, cls: WorkshopClass, now: datetime):
    """How much is refunded if `booking` is cancelled at time `now`."""
    paid = int(booking.price if booking.price is not None else (cls.price or 0))
    if cls.start - now >= timedelta(hours=FULL_REFUND_HOURS):
        return paid
    return 0


def join_waitlist(store, class_id: str, person: Person, now: datetime) -> WaitlistEntry:
    """Puts `person` on the waitlist of a class and returns the entry."""
    cls = store.get_class(class_id)
    if now >= cls.start:
        raise BookingError("the class has already started")
    waiting = store.waitlist_for(class_id)
    if any(_same_name(e.name, person.name) for e in waiting):
        raise BookingError(f"{person.name} is already on the waitlist")
    if len(waiting) >= WAITLIST_SIZE:
        raise BookingError("the waitlist is full")
    entry = WaitlistEntry(class_id, person.name, person.email, person.member, now, person.phone)
    waiting.append(entry)
    return entry


def promote_waitlist(store, class_id: str, now: datetime) -> Booking | None:
    """Called when a seat in the class frees up at time `now`: offers it to the
    next person on the waitlist (a Booking with status "offered", stored in
    `store.bookings`) and returns it, or None if nobody is waiting."""
    waiting = store.waitlist_for(class_id)
    if not waiting:
        return None
    cls = store.get_class(class_id)
    entry = waiting.pop(0)
    person = Person(entry.name, entry.email, entry.phone, entry.member)
    offer = Booking(
        store.next_id(),
        class_id,
        entry.name,
        entry.email,
        entry.member,
        now,
        status="offered",
        price=price_for(cls, person),
        confirm_by=now + timedelta(hours=CONFIRM_HOURS),
        phone=entry.phone,
    )
    store.bookings.append(offer)
    return offer
