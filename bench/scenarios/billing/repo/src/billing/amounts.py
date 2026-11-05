"""Amounts in cents (integers). Never float."""


def to_cents(text):
    """'1,250.50' -> 125050"""
    whole, _, dec = text.replace(",", "").replace("$", "").strip().partition(".")
    return int(whole or "0") * 100 + int((dec + "00")[:2])


def format_amount(cents):
    """125050 -> '$1,250.50'"""
    sign = "-" if cents < 0 else ""
    cents = abs(cents)
    return f"{sign}${cents // 100:,}.{cents % 100:02d}"
