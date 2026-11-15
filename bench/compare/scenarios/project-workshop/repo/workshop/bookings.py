"""Booking, prices, cancelling, refunds and the waitlist.

The signatures are fixed: the rest of the code and the tests call them as they
are. Every function that refuses something raises BookingError.
"""

from datetime import datetime

from .models import Booking, BookingError, Person, WaitlistEntry, WorkshopClass  # noqa: F401


def book(store, class_id: str, person: Person, now: datetime) -> Booking:
    """Books one seat in a class for `person` at time `now` and returns the
    confirmed Booking (stored in `store.bookings`), with its price set."""
    raise NotImplementedError


def price_for(cls: WorkshopClass, person: Person):
    """What `person` pays for one seat in `cls`."""
    raise NotImplementedError


def cancel(store, booking_id: str, now: datetime) -> Booking:
    """Cancels a booking at time `now` and returns it. A seat that frees up
    goes through promote_waitlist()."""
    raise NotImplementedError


def refund_amount(booking: Booking, cls: WorkshopClass, now: datetime):
    """How much is refunded if `booking` is cancelled at time `now`."""
    raise NotImplementedError


def join_waitlist(store, class_id: str, person: Person, now: datetime) -> WaitlistEntry:
    """Puts `person` on the waitlist of a class and returns the entry."""
    raise NotImplementedError


def promote_waitlist(store, class_id: str, now: datetime) -> Booking | None:
    """Called when a seat in the class frees up at time `now`: offers it to the
    next person on the waitlist (a Booking with status "offered", stored in
    `store.bookings`) and returns it, or None if nobody is waiting."""
    raise NotImplementedError
