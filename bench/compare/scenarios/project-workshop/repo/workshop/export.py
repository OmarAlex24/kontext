"""The CSV export for the accountant."""


def export_csv(store, class_id: str | None = None) -> str:
    """The bookings as CSV text, with a header row: one row per booking (of one
    class, or of every class), with the class, its date, the person's name and
    email, the status and the price."""
    raise NotImplementedError
