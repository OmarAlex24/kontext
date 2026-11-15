# workshop

A small booking library for a community workshop: classes, bookings, a waitlist,
cancellations and refunds, reminders, and a CSV export for the accountant.

Python 3.11, standard library only.

```sh
python3 -m unittest discover -s tests -t .
python3 -m workshop.cli --store data.json list
```

## Layout

| Module | What it holds |
|---|---|
| `workshop/models.py` | The data: classes, people, bookings, waitlist entries, reminders |
| `workshop/storage.py` | `Store`: everything in memory, saved to and loaded from one JSON file |
| `workshop/schedule.py` | Adding and listing classes |
| `workshop/bookings.py` | Booking, prices, cancelling, refunds, the waitlist |
| `workshop/notify.py` | Sending messages and the reminders that are due |
| `workshop/export.py` | The CSV export |
| `workshop/cli.py` | The command line |

Several functions in `bookings.py`, `notify.py` and `export.py` are still stubs
that raise `NotImplementedError`; their signatures are fixed, the rest of the
code and the tests call them as they are.
