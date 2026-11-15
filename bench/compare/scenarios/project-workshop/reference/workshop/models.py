"""The workshop's data: classes, people, bookings, waitlist entries and reminders.

Every datetime is timezone-aware.
"""

from dataclasses import dataclass, field
from datetime import datetime


class BookingError(ValueError):
    """A booking, cancellation or waitlist operation that is not allowed."""


@dataclass
class WorkshopClass:
    id: str
    title: str
    teacher: str
    start: datetime
    minutes: int = 120
    # The price of one seat. How money is represented is still to be agreed.
    price: object = None


@dataclass
class Person:
    name: str
    email: str
    phone: str = ""
    member: bool = False


@dataclass
class Booking:
    id: str
    class_id: str
    name: str
    email: str
    member: bool
    booked_at: datetime
    # "confirmed", "offered" (a seat offered from the waitlist) or "cancelled"
    status: str = "confirmed"
    price: object = None
    cancelled_at: datetime | None = None
    # For an offered seat: until when the person can take it.
    confirm_by: datetime | None = None
    phone: str = ""


@dataclass
class WaitlistEntry:
    class_id: str
    name: str
    email: str
    member: bool
    joined_at: datetime
    phone: str = ""


@dataclass
class Reminder:
    booking_id: str
    class_id: str
    to: str
    # "email" or "sms"
    channel: str
    send_at: datetime
    extra: dict = field(default_factory=dict)
