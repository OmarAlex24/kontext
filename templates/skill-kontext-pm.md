---
name: kontext-pm
description: Keeps a memory of the user's ideas in the project's .pm/ folder so none gets silently lost across days, and audits it with the kontext-pm-reader subagent. Use it when the user brain-dumps ideas or answers the coordinator's questions (save them verbatim and confirm what you took); when the user comes back after time away or asks where things stand (run the coverage audit, then show them); and when the coordinator needs the value in force of something the user said (ask the reader instead of reading the dumps).
---

# The PM's memory of the user's ideas

The user says many things over many days, some in passing, many as short answers to your questions. Each one has to end up done, in progress, waiting for them, or discarded by them; never forgotten. `.pm/`, in the project's root, keeps what they said word for word and where each idea went. kontext (the `kontext` skill) keeps the work; `.pm/` only points at it.

```
.pm/
  dumps/<YYYY-MM-DD>.md   what the user said, verbatim, append-only, never edited
  items.md                one row per idea: where it went and with what evidence
  status.md               where things stand, short
  shots/                  screenshots used as evidence
```

When the current dump file passes ~60 KB (`wc -c`), go on in `<date>-2.md`, then `-3`…, so that each file can be Read whole.

## 1. Who writes

Only the main session the user talks to, acting as PM. Orchestrators, coordinators, implementers and subagents never write `.pm/`: they report to you, and you write. If you cannot tell that you are the PM, do not write.

## 2. Capture

Append each user message that carries ideas, decisions or answers to today's dump, verbatim, before you act on it. Never edit or reorder what is already there. Each entry:

```markdown
## 2026-11-07 10:42

**Coordinator asked:**

> 1. Do we keep the old export or drop it?
> 2. Who merges the PRs, you or me?

**User:**

1. drop it
2. merge them yourself
```

- `**Coordinator asked:**` only when the message answers questions you asked: quote those questions verbatim, as you asked them. Without it, a numbered answer means nothing on another day.
- The markers (`## <date> <HH:MM>`, `**Coordinator asked:**`, `**User:**`) are always these, in English; the content stays in the user's language.
- The only change you may make to the user's text: a secret (token, password, key) becomes `[secret removed]`. Pasted material stays as pasted.

## 3. Confirm

After saving, list back the ideas you took from the message, with the ids you gave them in items.md (section 4), and ask whether you missed anything. Their answer is a user message like any other: save it to the dump too (section 2, with your question under `**Coordinator asked:**`).

## 4. items.md is a pointer table, not a second roadmap

```markdown
| id | idea | source | state | where | evidence |
|---|---|---|---|---|---|
| P1 | CSV export of invoices | dumps/2026-11-07.md:14 | in progress | T-4c1 | |
| P2 | drop the old export | dumps/2026-11-07.md:23 | done | T-9a0 | PR #12 |
```

- One row per idea: `id` (`P1`, `P2`…, never reused), the idea in a few words, `source` (`dumps/<file>:<line>`), `state` (pending, in progress, done or discarded), `where` (the kontext task or epic, the decision, the PR or the doc that covers it) and `evidence` (for done: a PR, a commit, a test run or a `shots/` path).
- `discarded` only when the user says so: cite where in `evidence` (`dumps/<file>:<line>`).
- When an idea becomes work, create it in kontext as the `kontext-orchestrate` skill says, and write its id under `where`.
- Do not copy plans, task lists or acceptance criteria into items.md: they live in kontext.

## 5. Coming back: the coverage audit

When the user comes back after time away, or asks where things stand:

1. **Do not read the dumps yourself**: that is the point, your context stays small. Launch the `kontext-pm-reader` subagent (in Claude Code, the `Agent` tool with `subagent_type` `kontext-pm-reader`) and give it the text under "Audit task" below, word for word. It answers with JSON: `ideas` (every idea it found) and `missing` (those not in items.md), each with `quote`, `file` and `line`.
2. **Verify each idea in `missing`** before you believe it: one targeted Grep of items.md (and of the places its rows point to) for that idea, and one Grep of the cited file for a piece of the quote. Drop the ones that are covered: on real data the reader flags an idea that items.md has in other words, and now and then a quote is not verbatim.
3. **Add the confirmed ones** to items.md as pending, with the reader's citation as their source (`dumps/<file>:<line>`, without the `.pm/` in front).
4. **Show the user where things stand**, following the `kontext-show` skill for how to show it (do not repeat its rules here). The `.pm` part, each with its evidence: done; in progress; waiting for the user; pending; and newly found missing (the ones from step 3).

If `kontext-pm-reader` is not among your agents (`kontext install` installs it; `kontext install --skills-only` brings only the skills), say so and ask the user before reading the dumps yourself.

## 6. Values

When you need the value in force of something the user said (a limit, a name, a default, whether they changed their mind), do not read the dumps: launch `kontext-pm-reader` with the text under "Value task" below, word for word, with `{questions}` replaced by your questions, one per line (`- q1: <question>`, `- q2: …`). Grep the quote of each answer in its file before you act on it.

## 7. status.md

Rewrite it at the end of each block of work: ~40 lines at most. Where things are, what waits for the user, what comes next. It is not a log: the dumps and kontext have the history.

## 8. Public repos

If the repo is public (check its remote, or ask), ask the user before committing `.pm/`: the dumps are their words, verbatim. Offer to add `.pm/` to `.gitignore` instead.

## Audit task

Which of the user's ideas in `.pm/dumps/` are not in `.pm/items.md`? Work in this order, one step after the other:

- **Separate.** In each dump, tell the user's own text apart from the material they pasted. Only the user's own text holds the user's ideas.
- **List.** Go through every sentence of the user's own text, one by one, and list each idea it states, with its citation. Do not skip a sentence because its paragraph is about something else, nor because it states a fact instead of asking for something.
- **Apply changes.** Apply the later changes and discards: a later dump wins over the dumps before it, and an idea the user later drops is discarded.
- **Compare.** Compare each idea on your list with `.pm/items.md` by meaning, not by wording. An idea counts as present if items.md has it, even in other words or with an outdated value.
- **Report.** Report the ideas that are not in items.md and were not discarded. Cite each one once, quoting the user's sentence that states it.

Reply only with this JSON. `ideas` is your list (List), each idea once, with `in_items` (Compare) and `discarded` (Apply changes); `missing` is your answer (Report):

{"ideas": [{"quote": "...", "file": ".pm/dumps/<date>.md", "line": 41, "in_items": true, "discarded": false}], "missing": [{"quote": "...", "file": ".pm/dumps/<date>.md", "line": 41}]}

If nothing is missing, `missing` is an empty list.

## Value task

Answer these questions about the project with the value in force: the last thing the user said about it in `.pm/dumps/`. `.pm/items.md` may be out of date, and the pasted material talks about other people's projects. A value can change more than once. A change may give the new value outright or relative to the value before it (a multiple of it, a fraction of it, a step up or down from it); then the value in force is the one that results, and you work it out. Cite the user's sentence that sets the value in force: the one that states it or the change that leads to it.

{questions}

In `value` put only the value in force, with its unit if it has one: no history, no calculation.

Reply only with this JSON, one answer per question:

{"answers": [{"id": "q1", "value": "...", "quote": "...", "file": ".pm/dumps/<date>.md", "line": 41}]}
