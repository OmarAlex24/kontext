"""The analysis rules (design §5.2), frozen before any model call. See
ANALYSIS.md. The tables apply them mechanically; nothing here is tuned after
seeing results."""

import itertools
import statistics

# L: a difference in requirements kept counts only if the means differ by at
# least this many requirements AND the permutation test gives p < P_MAX.
MIN_REQUIREMENTS_DIFF = 2
P_MAX = 0.05
# Cost and time: differences under this fraction are not claimed.
MIN_COST_TIME_DIFF = 0.20


def permutation_p(a, b):
    """Exact two-sided permutation test on the difference of means (for 5
    against 5, all 252 splits). Returns None if either side is empty."""
    a, b = list(a), list(b)
    if not a or not b:
        return None
    pooled = a + b
    n = len(a)
    observed = abs(statistics.mean(a) - statistics.mean(b))
    total = hits = 0
    for idx in itertools.combinations(range(len(pooled)), n):
        s = set(idx)
        x = [pooled[i] for i in s]
        y = [pooled[i] for i in range(len(pooled)) if i not in s]
        total += 1
        if abs(statistics.mean(x) - statistics.mean(y)) >= observed - 1e-12:
            hits += 1
    return hits / total


def requirements_verdict(a, b):
    """a, b: requirements kept per run (counts out of 14). The verdict text."""
    if len(a) < 2 or len(b) < 2:
        return "too few runs"
    diff = statistics.mean(a) - statistics.mean(b)
    p = permutation_p(a, b)
    if abs(diff) >= MIN_REQUIREMENTS_DIFF and p is not None and p < P_MAX:
        return f"differs ({diff:+.1f} requirements, p = {p:.3f})"
    return f"no claim ({diff:+.1f} requirements, p = {p:.3f})"


def cost_verdict(a, b):
    """a, b: cost (or time) per run. A relative difference of medians."""
    if not a or not b:
        return "–"
    ma, mb = statistics.median(a), statistics.median(b)
    if mb == 0:
        return "–"
    rel = (ma - mb) / mb
    return f"{rel:+.0%}" + ("" if abs(rel) >= MIN_COST_TIME_DIFF else " (no claim)")
