"""Invoices and which ones are overdue."""

from dataclasses import dataclass
from datetime import date


@dataclass(frozen=True)
class Invoice:
    customer: str
    number: str
    balance_cents: int
    due: date


def overdue(invoices, today):
    """The invoices with a balance whose due date has already passed."""
    return [f for f in invoices if f.balance_cents > 0 and f.due < today]


def by_customer(invoices):
    """Groups the invoices by customer, in order of number."""
    groups = {}
    for f in sorted(invoices, key=lambda f: f.number):
        groups.setdefault(f.customer, []).append(f)
    return groups
