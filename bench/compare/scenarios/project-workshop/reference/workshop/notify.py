"""Sending messages, and the reminders that are due.

There is no real gateway here: every message goes to OUTBOX, which the tests read.
"""

from datetime import datetime, time, timedelta
from zoneinfo import ZoneInfo

from .models import Reminder  # noqa: F401

TZ = ZoneInfo("America/Mexico_City")
# The reminder goes out the day before the class, at this local hour.
REMINDER_HOUR = 18
# A reminder is due from its send time until this long after it.
DUE_WINDOW = timedelta(hours=1)

OUTBOX: list[dict] = []


def send_email(to, subject, body):
    OUTBOX.append({"channel": "email", "to": to, "subject": subject, "body": body})


def send_sms(to, body):
    OUTBOX.append({"channel": "sms", "to": to, "body": body})


def reminders_due(store, now: datetime) -> list:
    """The reminders (Reminder objects) that should go out at time `now`."""
    due = []
    for b in store.bookings:
        if b.status != "confirmed":
            continue
        cls = store.get_class(b.class_id)
        send_at = reminder_time(cls.start)
        if send_at <= now < send_at + DUE_WINDOW and now < cls.start:
            due.append(Reminder(b.id, b.class_id, b.email, "email", send_at))
    return due


def reminder_time(start: datetime) -> datetime:
    """The day before `start`, at REMINDER_HOUR, in the workshop's time zone."""
    day = start.astimezone(TZ).date() - timedelta(days=1)
    return datetime.combine(day, time(REMINDER_HOUR, 0), tzinfo=TZ)


def send_due(store, now: datetime):
    """Sends every reminder that is due at `now`. Returns how many it sent."""
    sent = 0
    for r in reminders_due(store, now):
        cls = store.get_class(r.class_id)
        text = f"Reminder: {cls.title} starts at {cls.start:%H:%M} on {cls.start:%Y-%m-%d}."
        if r.channel == "sms":
            send_sms(r.to, text)
        else:
            send_email(r.to, f"Reminder: {cls.title}", text)
        sent += 1
    return sent
