"""Refunds, whole or partial."""

from decimal import Decimal

from money import round_half, to_money


def partial_refund(amount, fraction) -> Decimal:
    """`fraction` of `amount` (0 to 1), rounded to the cent."""
    f = to_money(fraction)
    if not 0 <= f <= 1:
        raise ValueError("fraction must be between 0 and 1")
    return round_half(to_money(amount) * f)


def refund_lines(lines, returned) -> list:
    """For each (quantity, unit_price) line, the refund for `returned[i]` units."""
    out = []
    for (q, p), r in zip(lines, returned):
        if r > q:
            raise ValueError("cannot return more than was bought")
        out.append(round_half(to_money(r) * to_money(p)))
    return out
