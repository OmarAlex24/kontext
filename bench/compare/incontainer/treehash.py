"""Prints a sha256 of a repo's working tree without .git and without tool-state
paths (complib/interventions.TOOL_PATHS, passed as arguments), and the number
of files. The fixed-reply rule compares it before and after a work turn."""

import hashlib
import os
import sys


def main(root, skip):
    h = hashlib.sha256()
    n = 0
    for dirpath, dirnames, filenames in os.walk(root):
        rel_dir = os.path.relpath(dirpath, root)
        rel_dir = "" if rel_dir == "." else rel_dir + "/"
        dirnames[:] = sorted(d for d in dirnames if not skipped(rel_dir + d + "/", skip))
        for f in sorted(filenames):
            rel = rel_dir + f
            if skipped(rel, skip):
                continue
            try:
                with open(os.path.join(dirpath, f), "rb") as fh:
                    data = fh.read()
            except OSError:
                continue
            h.update(rel.encode() + b"\0" + data + b"\0")
            n += 1
    print(h.hexdigest(), n)


def skipped(rel, skip):
    return any(rel == s.rstrip("/") or rel.startswith(s) or ("/" + s) in ("/" + rel) and s.endswith("/") for s in skip)


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2:])
