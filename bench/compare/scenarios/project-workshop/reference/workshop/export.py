"""The CSV export for the accountant."""

import csv
import io
from zoneinfo import ZoneInfo

TZ = ZoneInfo("America/Mexico_City")
DELIMITER = ";"
HEADER = ["class", "date", "name", "email", "status", "price"]


def _money(cents):
    if cents is None:
        return ""
    cents = int(cents)
    return f"{cents // 100}.{cents % 100:02d}"


def export_csv(store, class_id: str | None = None) -> str:
    """The bookings as CSV text, with a header row: one row per booking (of one
    class, or of every class), with the class, its date, the person's name and
    email, the status and the price."""
    out = io.StringIO()
    w = csv.writer(out, delimiter=DELIMITER, lineterminator="\n")
    w.writerow(HEADER)
    for b in store.bookings:
        if class_id is not None and b.class_id != class_id:
            continue
        cls = store.get_class(b.class_id)
        w.writerow([cls.title, cls.start.astimezone(TZ).isoformat(), b.name, b.email, b.status, _money(b.price)])
    return out.getvalue()
