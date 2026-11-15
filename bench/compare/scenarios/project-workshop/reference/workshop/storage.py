"""Store: the workshop's data in memory, saved to and loaded from one JSON file."""

import json
from dataclasses import asdict, fields
from datetime import datetime
from pathlib import Path

from .models import Booking, BookingError, WaitlistEntry, WorkshopClass

DATETIME_FIELDS = {"start", "booked_at", "cancelled_at", "confirm_by", "joined_at"}


def _encode(obj):
    out = {}
    for k, v in asdict(obj).items():
        out[k] = v.isoformat() if isinstance(v, datetime) else v
    return out


def _decode(cls, data):
    names = {f.name for f in fields(cls)}
    kw = {}
    for k, v in data.items():
        if k not in names:
            continue
        kw[k] = datetime.fromisoformat(v) if k in DATETIME_FIELDS and v else v
    return cls(**kw)


class Store:
    def __init__(self):
        self.classes: dict[str, WorkshopClass] = {}
        self.bookings: list[Booking] = []
        self.waitlist: dict[str, list[WaitlistEntry]] = {}
        self._seq = 0

    def next_id(self, prefix="b"):
        self._seq += 1
        return f"{prefix}{self._seq}"

    def add_class(self, cls: WorkshopClass):
        if cls.id in self.classes:
            raise BookingError(f"there is already a class {cls.id}")
        self.classes[cls.id] = cls
        return cls

    def get_class(self, class_id) -> WorkshopClass:
        try:
            return self.classes[class_id]
        except KeyError:
            raise BookingError(f"no class {class_id}") from None

    def get_booking(self, booking_id) -> Booking:
        for b in self.bookings:
            if b.id == booking_id:
                return b
        raise BookingError(f"no booking {booking_id}")

    def bookings_for(self, class_id, active_only=True):
        return [
            b
            for b in self.bookings
            if b.class_id == class_id and (not active_only or b.status != "cancelled")
        ]

    def waitlist_for(self, class_id):
        return self.waitlist.setdefault(class_id, [])

    def save(self, path):
        data = {
            "seq": self._seq,
            "classes": [_encode(c) for c in self.classes.values()],
            "bookings": [_encode(b) for b in self.bookings],
            "waitlist": {k: [_encode(e) for e in v] for k, v in self.waitlist.items()},
        }
        Path(path).write_text(json.dumps(data, indent=2, ensure_ascii=False))

    @classmethod
    def load(cls, path):
        store = cls()
        p = Path(path)
        if not p.exists():
            return store
        data = json.loads(p.read_text())
        store._seq = data.get("seq", 0)
        for c in data.get("classes", []):
            store.add_class(_decode(WorkshopClass, c))
        store.bookings = [_decode(Booking, b) for b in data.get("bookings", [])]
        store.waitlist = {
            k: [_decode(WaitlistEntry, e) for e in v] for k, v in data.get("waitlist", {}).items()
        }
        return store
