"""Money helpers shared by invoices, refunds and reports."""

from decimal import ROUND_HALF_DOWN, Decimal

CENT = Decimal("0.01")


def to_money(value) -> Decimal:
    """A Decimal from an int, a str or a Decimal (never a float)."""
    if isinstance(value, float):
        raise TypeError("use str or Decimal for money, not float")
    return Decimal(value)


def round_half(value: Decimal) -> Decimal:
    """Rounds to the cent; a half cent goes up (away from zero), as the bank does."""
    return to_money(value).quantize(CENT, rounding=ROUND_HALF_DOWN)


def fmt(value: Decimal) -> str:
    """`$1,234.50`."""
    v = round_half(value)
    sign = "-" if v < 0 else ""
    return f"{sign}${abs(v):,.2f}"
