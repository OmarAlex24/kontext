"""Monthly reports."""

from decimal import Decimal

from money import fmt, round_half, to_money


def average_ticket(amounts) -> Decimal:
    """The mean of `amounts`, rounded to the cent; 0 for no amounts."""
    amounts = [to_money(a) for a in amounts]
    if not amounts:
        return Decimal("0.00")
    return round_half(sum(amounts, Decimal("0")) / len(amounts))


def share(part, whole) -> Decimal:
    """`part` as a percentage of `whole`, rounded to the cent."""
    whole = to_money(whole)
    if whole == 0:
        return Decimal("0.00")
    return round_half(to_money(part) * 100 / whole)


def summary_line(label, amounts) -> str:
    return f"{label}: {len(amounts)} tickets, average {fmt(average_ticket(amounts))}"
