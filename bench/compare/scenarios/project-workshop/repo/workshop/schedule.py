"""Adding and listing classes."""

from datetime import datetime

from .models import BookingError, WorkshopClass


def add_class(store, class_id, title, teacher, start: datetime, minutes=120, price=None):
    if start.tzinfo is None:
        raise BookingError("a class start needs a timezone")
    return store.add_class(WorkshopClass(class_id, title, teacher, start, minutes, price))


def upcoming(store, now: datetime):
    """The classes that have not started yet, the soonest first."""
    return sorted((c for c in store.classes.values() if c.start > now), key=lambda c: c.title)


def seats_taken(store, class_id):
    """Confirmed bookings and seats offered from the waitlist."""
    return sum(1 for b in store.bookings_for(class_id) if b.status in ("confirmed", "offered"))
