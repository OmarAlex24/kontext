"""The command line: python3 -m workshop.cli --store data.json <command>."""

import argparse
import sys
from datetime import datetime, timezone

from . import schedule
from .storage import Store


def main(argv=None, out=sys.stdout):
    p = argparse.ArgumentParser(prog="workshop")
    p.add_argument("--store", default="workshop.json")
    sub = p.add_subparsers(dest="command", required=True)
    sub.add_parser("list", help="the upcoming classes")
    a = p.parse_args(argv)
    store = Store.load(a.store)
    if a.command == "list":
        now = datetime.now(timezone.utc)
        for c in schedule.upcoming(store, now):
            print(f"{c.id}\t{c.start.isoformat()}\t{c.title}\t{c.teacher}", file=out)
    return 0


if __name__ == "__main__":
    sys.exit(main())
