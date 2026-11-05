These runs used the Spanish version of the scenario; the scenario was translated later.

# Long-term memory: long-memory scenario, 2026-11-04

Internal run, **not publishable yet**: it is a single scenario, and the harness was written by the same team as kontext (see "Limits").

**How it works.** The user says things at the start, and then the context window fills with a lot of material: meeting notes, tickets and logs of about 20 thousand tokens per block, with similar data that serves as distractors. Then `/compact` runs and the agent is asked questions.

| | |
|---|---|
| Models | `claude-haiku-5-5`, `claude-sonnet-5-5` and `claude-opus-5-5` |
| Judge | `claude-sonnet-5-5`, no tools and no configuration, for all |
| Session | 4 days, 14 fillers and 3 `/compact` (the context reached 135–155 thousand tokens before each one) |
| Questions | 20, at 4 checkpoints |
| Behavior tests | 9 hidden tests: the code has to apply what was said |
| Runs | 5 variants × 3 models × 5 seeds = 75 runs, **204.56 USD** billed through the API |

The questions are grouped into four classes of data:

| Class | What it is | Questions |
|---|---|---:|
| A | What was recorded in the charter | 2 |
| B | What the user said in passing | 12 |
| C | Something that changes halfway through the session | 3 |
| cross | Several pieces of data have to be combined | 3 |

**Agent and environment.** The agent uses the same command and the same clean environment as in [billing](2026-11-04-billing.md). Claude Code's automatic memory is off except in `automem`.

## Table

Mean ± standard deviation over 5 seeds. The cost includes the agent, the judge and the distill.

| Model | Variant | Memory % | Confusions | Behavior % | Cost USD | Distill USD |
|---|---|---:|---:|---:|---:|---:|
| haiku | none | 27 ± 4 | 0.0 | 0 ± 0 | 0.59 | – |
| haiku | automem | 55 ± 28 | 2.0 | 51 ± 48 | 0.59 | – |
| haiku | notes | 72 ± 26 | 1.2 | 64 ± 49 | 0.62 | – |
| haiku | kontext-pointer | 89 ± 7 | 2.2 | 100 ± 0 | 0.68 | 0.031 |
| haiku | kontext-inject | **98 ± 4** | 0.4 | 100 ± 0 | 0.69 | 0.032 |
| sonnet | none | 25 ± 0 | 0.0 | 0 ± 0 | 2.34 | – |
| sonnet | automem | 87 ± 12 | 0.8 | 96 ± 6 | 2.46 | – |
| sonnet | notes | 97 ± 7 | 0.6 | 100 ± 0 | 2.48 | – |
| sonnet | kontext-pointer | 97 ± 7 | 0.6 | 100 ± 0 | 2.50 | 0.029 |
| sonnet | kontext-inject | **100 ± 0** | 0.0 | 100 ± 0 | 2.63 | 0.030 |
| opus | none | 26 ± 2 | 0.0 | 0 ± 0 | 4.57 | – |
| opus | automem | 90 ± 12 | 1.8 | 84 ± 17 | 4.92 | – |
| opus | notes | 85 ± 10 | 2.8 | 91 ± 12 | 4.83 | – |
| opus | kontext-pointer | **99 ± 2** | 0.0 | 100 ± 0 | 5.47 | 0.031 |
| opus | kontext-inject | 95 ± 7 | 0.4 | 100 ± 0 | 5.53 | 0.032 |

What each column means:
- **Confusions:** how many times per run the agent answered with something forbidden, such as an old value or a distractor's data.
- **Behavior:** the percentage of the 9 hidden tests that the code passes.

The full table, with drift, tokens and errors, comes out of `python3 bench/aggregate.py bench/results/memoria-20261104`.

### By data class

| Model | Variant | A | B | C | cross |
|---|---|---:|---:|---:|---:|
| haiku | none | 50 | 33 | 7 | 7 |
| haiku | automem | 50 | 70 | 13 | 40 |
| haiku | notes | 100 | 70 | 73 | 60 |
| haiku | kontext-pointer | 100 | 100 | 53 | 73 |
| haiku | kontext-inject | 100 | 100 | 93 | 93 |
| sonnet | none | 50 | 33 | 0 | 0 |
| sonnet | automem | 70 | 95 | 73 | 80 |
| sonnet | notes | 100 | 100 | 87 | 93 |
| sonnet | kontext-pointer | 100 | 100 | 87 | 93 |
| sonnet | kontext-inject | 100 | 100 | 100 | 100 |
| opus | none | 50 | 35 | 0 | 0 |
| opus | automem | 100 | 97 | 60 | 87 |
| opus | notes | 100 | 100 | 33 | 67 |
| opus | kontext-pointer | 100 | 98 | 100 | 100 |
| opus | kontext-inject | 100 | 100 | 73 | 93 |

### By checkpoint

Each checkpoint falls after a `/compact`, except checkpoint 1, which comes before the first one.

| Model | Variant | 1 | 2 | 3 | 4 |
|---|---|---:|---:|---:|---:|
| haiku | none | 100 | 0 | 17 | 4 |
| haiku | automem | 100 | 50 | 37 | 48 |
| haiku | notes | 90 | 60 | 71 | 68 |
| haiku | kontext-pointer | 100 | 100 | 77 | 88 |
| haiku | kontext-inject | 100 | 100 | 94 | 100 |
| sonnet | none | 100 | 0 | 14 | 0 |
| sonnet | automem | 100 | 95 | 71 | 92 |
| sonnet | notes | 100 | 100 | 94 | 96 |
| sonnet | kontext-pointer | 100 | 100 | 94 | 96 |
| sonnet | kontext-inject | 100 | 100 | 100 | 100 |
| opus | none | 100 | 0 | 17 | 0 |
| opus | automem | 100 | 95 | 86 | 84 |
| opus | notes | 100 | 100 | 69 | 84 |
| opus | kontext-pointer | 100 | 95 | 100 | 100 |
| opus | kontext-inject | 100 | 100 | 91 | 92 |

## What the results show

- **Without memory, `/compact` erases almost everything.** The three models get 100 % before the first `/compact` and between 0 and 17 % after it. Only the two data points in the charter survive, because the agent reads them again from the repo.
- **kontext is the only variant that doesn't depend on the model.**
  - `kontext-inject` reaches 95–100 % on the three models and passes all the behavior tests.
  - With `automem` and `notes`, sonnet and opus hold up well, at 85 to 97 %, but haiku falls to 55 and 72 %, and its behavior drops to 51 and 64 %, with a lot of variance.
- **The difference is in the data that changes (class C).**
  - `automem` gets 13–73 % right and `notes` 33–87 %.
  - kontext gets 93–100 % right with `kontext-inject` on haiku and sonnet, and 100 % with `kontext-pointer` on opus. What helps is the distill's `supersedes`.
  - `notes` with opus stays at 33 %: the model writes the new value in the notes, but does not delete the old one.
- **`kontext-pointer` costs haiku**: 53 % in class C, against 93 % with `kontext-inject`. When kontext only tells it that the memory exists, haiku doesn't always go and consult it. For small models it's better to inject.
- **It costs more, not less.** Per run, kontext comes out between 7 and 21 % more expensive than `none`, and between 2 and 17 % more than `automem`.
  - haiku: from 0.59 to 0.69 USD.
  - sonnet: from 2.34 to 2.63 USD.
  - opus: from 4.57 to 5.53 USD.

  The distill costs about 0.03 USD per run; the rest comes from the agent reading and writing in kontext. In [billing](2026-11-04-billing.md) it came out a little cheaper because it avoided corrections; here there are no corrections to avoid.

## Which failures remain with kontext, and why

Of the 22 failures with kontext, 20 are three questions about the surcharge rate:

| Question | Class |
|---|---|
| `p3-tasa` | C |
| `p4-tasa` | C |
| `p3-recargo` | cross |

In almost all of them, the agent **does remember** the change from 1.5 % to 1.8 %. It fails for two reasons that are not about memory:

1. **The scenario is ambiguous.** The user says "from Monday it's 1.8 %". Since the run was on a Wednesday, 4 November, the models understand "starting Monday the 9th" and answer that 1.5 % is still in force today. The judge's note expected it to be in force from Monday the 2nd. Without those three questions, kontext gets 99–100 % on the three models, `automem` gets 65, 93 and 96 %, and `notes` 76, 100 and 95 %.
2. **"Unsigned" slows opus down.** What the user says enters the ledger as a pending proposal. In one seed, opus answered "there is no rate in force yet" because both rates were proposals not yet accepted. That same caution costs `kontext-inject` with opus: 73 % in class C.

A side note: in several runs the agent tried to write to kontext and Claude Code asked for approval, which nobody gives in `-p`. The distill saved it anyway, so memory was not affected, but the `Bash(ctx:*)` permission does not cover every way the agent invokes kontext.

## What needs fixing

- Scenario: change "from Monday" to a date ("from Monday the 2nd") before the next run. This run used the ambiguous version.
- kontext: a user rule with `origin = user` should count as in force when answering, even if it is not signed. Signing it is for the charter, not so that the agent believes the user.
- Bench: find out which way of invoking kontext `Bash(ctx:*)` does not cover (`cd … && ctx …`, pipes), and widen the permission.

Done, and checked with a small benchmark of 11 runs ([results](2026-11-04-mini-after-fixes.md)):

- Scenario: done in `679231f`. The circular now says "the surcharge rate is no longer 1.5 %: the new one is in force, 1.8 % per month", and that both changes apply to everything calculated from that day on. It does not depend on the day the run happens. The judge's notes say the same.
- kontext: done in `3f6a65c`. A user rule that nothing replaces comes out as "[user rule, in force]", and what replaces it says "(supersedes <id> "old value", which no longer applies)". The header of `resume`, the hook pointer and the skill say that what the user said applies even if it is unsigned. The human's inbox does not change.
- Bench: done in `7856a6f` (kontext) and `6aaff73` (bench). Of 153 kontext commands without permission, 104 were writes with `CTX_ACTOR=… ctx …` or `export CTX_ACTOR=…` and 49 were reads with `| sed`, `| head`, `| tail` or `( … )`. kontext now accepts `--role` and `--model` as it does `--actor`, and the hook, `resume` and the skill tell the agent how to invoke it. The agent's default command includes the missing permissions. Result in [the small benchmark](2026-11-04-mini-after-fixes.md).

## Limits, before publishing

- It is a single scenario, with 5 seeds per cell. With haiku the variance of `automem` and `notes` is large (±26–48).
- The judge is sonnet for all, including sonnet as the agent. There were 1500 verdicts and none fell back to the keyword check.
- The harness, the scenario and kontext were written by the same team.
- **Incidents in the run:**
  - Two jobs crashed when the judge went past 300 s. Now it retries.
  - Six crashed at 16:37, when the T3 session that had launched them was restarted.
  - The lost seeds were repeated with `--first-seed`.
  - No finished run was discarded, and all cells have n = 5.
