# Analysis rules (frozen)

Fixed before any model call, with the fixtures, the scripted turns and the
hidden checks (FREEZE.json). They are applied mechanically by
`complib/analysis.py` and `complib/tables.py`; none is changed after seeing
results. From design §5.2.

1. **No composite score.** Every measure is reported raw, per task × model × arm.
2. **Requirements (L).** A run's score is the number of the 14 hidden checks
   that pass on the snapshot of the day the requirement is scored on (day 3
   for R3, R6, R7, R8, R9, R10, R13; day 4 for the rest; `scored_on` in the
   script). A difference between two arms counts as meaningful only if the
   means differ by **≥ 2 requirements** and an exact two-sided permutation test
   over the runs gives **p < 0.05**. Otherwise the table says "no claim".
3. **Cost and time.** Differences of medians under **20 %** are not claimed.
4. **Small tasks.** Hidden checks are expected near 100 % for every arm; only
   cost, time and turns are compared, by median and range.
5. **Reported apart:** requirements already passing at the end of day 2
   ("coded before the loss", a leak in the script if any), day-3 requirements
   that regress by day 4, confusions (a failed requirement whose confusion
   probe passes: the code took an old or a distractor value), interventions
   (fixed replies sent, by kind) and permission denials.
6. **Costs** are what `claude -p` reports (`total_cost_usd` and `modelUsage`
   of the envelope, as the difference from the previous call of the same
   session), plus kontext's distill envelopes. mem0's server-side extraction is
   shown as $0 (free plan) and at the BYOK rate per add. The design's price
   table is never used for a result.
7. **Fixed reply.** `complib/interventions.py`: a work turn with no change to
   the code (tool-state paths aside) and a reply that asks gets "Use your best
   judgement with what I've told you so far, and go ahead.", at most twice.
