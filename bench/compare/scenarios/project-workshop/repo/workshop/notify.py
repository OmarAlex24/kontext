"""Sending messages, and the reminders that are due.

There is no real gateway here: every message goes to OUTBOX, which the tests read.
"""

from datetime import datetime

from .models import Reminder  # noqa: F401

OUTBOX: list[dict] = []


def send_email(to, subject, body):
    OUTBOX.append({"channel": "email", "to": to, "subject": subject, "body": body})


def send_sms(to, body):
    OUTBOX.append({"channel": "sms", "to": to, "body": body})


def reminders_due(store, now: datetime) -> list:
    """The reminders (Reminder objects) that should go out at time `now`."""
    raise NotImplementedError


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
