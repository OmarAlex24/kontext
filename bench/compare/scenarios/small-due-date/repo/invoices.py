"""Dates of an invoice: when it is due, when to remind, how late it is.

Every function takes and returns `datetime.date` values.
"""

from datetime import date, timedelta

# Saturday and Sunday.
WEEKEND = (5, 6)


def due_date(issued: date, days: int) -> date:
    """The day an invoice issued on `issued` with `days` days of credit is due.

    An invoice issued on March 1 with 30 days of credit is due on March 31.
    """
    if days < 0:
        raise ValueError("days of credit cannot be negative")
    return issued + timedelta(days=days - 1)


def is_business_day(d: date, holidays=()) -> bool:
    """True if `d` is neither a weekend day nor one of `holidays`."""
    return d.weekday() not in WEEKEND and d not in set(holidays)


def next_business_day(d: date, holidays=()) -> date:
    """`d` itself if it is a business day, else the first business day after it."""
    while not is_business_day(d, holidays):
        d += timedelta(days=1)
    return d


def add_business_days(d: date, n: int, holidays=()) -> date:
    """The date `n` business days after `d` (`d` itself does not count)."""
    if n < 0:
        raise ValueError("n cannot be negative")
    while n > 0:
        d += timedelta(days=1)
        if is_business_day(d, holidays):
            n -= 1
    return d


def days_overdue(due: date, today: date) -> int:
    """How many days past `due` `today` is; 0 if it is not overdue yet."""
    return max(0, (today - due).days)


def reminder_dates(due: date, before=(7, 1), holidays=()) -> list[date]:
    """The days to send reminders: `before` days ahead of `due`, each moved
    back to the closest earlier business day, without repeats, soonest first."""
    out = []
    for n in before:
        d = due - timedelta(days=n)
        while not is_business_day(d, holidays):
            d -= timedelta(days=1)
        if d not in out:
            out.append(d)
    return sorted(out)


def month_end(d: date) -> date:
    """The last day of `d`'s month."""
    first_next = date(d.year + (d.month == 12), d.month % 12 + 1, 1)
    return first_next - timedelta(days=1)
