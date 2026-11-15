"""Invoice lines and totals."""

from decimal import Decimal

from money import round_half, to_money

VAT = Decimal("0.16")


def line_total(quantity, unit_price) -> Decimal:
    """Quantity times unit price, rounded to the cent."""
    return round_half(to_money(quantity) * to_money(unit_price))


def vat(subtotal) -> Decimal:
    return round_half(to_money(subtotal) * VAT)


def invoice_total(lines) -> Decimal:
    """`lines` is a list of (quantity, unit_price); subtotal plus VAT."""
    subtotal = sum((line_total(q, p) for q, p in lines), Decimal("0"))
    return subtotal + vat(subtotal)
