"""Keys and other secrets: read by name from run.py's environment, kept in
memory, redacted from everything copied out, and a scan that fails the run if
a value is still in the results folder (rule: no key in a file, a log, a
transcript copy or the results)."""

import gzip
import os
from pathlib import Path

from .arms import KEY_NAMES

MIN_LEN = 8


def from_env(names=None, environ=None):
    environ = os.environ if environ is None else environ
    names = names or list(KEY_NAMES)
    return {n: environ[n] for n in names if environ.get(n)}


class Secrets:
    """Every secret value of a run: the keys, the sidecar's admin secret, a
    handoff token. Values shorter than MIN_LEN are refused (they would redact
    ordinary words)."""

    def __init__(self, values=None):
        self.values = {}
        for k, v in (values or {}).items():
            self.add(k, v)

    def add(self, name, value):
        if not value:
            return
        if len(value) < MIN_LEN:
            raise ValueError(f"secret {name} is too short to redact safely")
        self.values[name] = value

    def values_for(self, names):
        return {n: self.values[n] for n in names if n in self.values}

    def redact_text(self, s):
        for name, v in self.values.items():
            if v in s:
                s = s.replace(v, f"<redacted:{name}>")
        return s

    def redact_bytes(self, b):
        for name, v in self.values.items():
            vb = v.encode()
            if vb in b:
                b = b.replace(vb, f"<redacted:{name}>".encode())
        return b


def scan(folder, values):
    """[(file, secret name)] for every file under `folder` that holds a value
    (gzip files are read decompressed). Never returns the value."""
    hits = []
    needles = {n: v.encode() for n, v in values.items() if v}
    if not needles:
        return hits
    for p in sorted(Path(folder).rglob("*")):
        if not p.is_file():
            continue
        try:
            data = p.read_bytes()
            if p.suffix == ".gz":
                try:
                    data = gzip.decompress(data)
                except OSError:
                    pass
        except OSError:
            continue
        for n, v in needles.items():
            if v in data:
                hits.append((str(p), n))
    return hits
