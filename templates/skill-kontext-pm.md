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

- `**Coordinator asked:**` only when the message answers something you said: the questions you asked, quoted verbatim, as you asked them; or, when your message asked nothing with a question mark ("Say go and I merge it"), the sentence or sentences it replies to, verbatim (the last paragraph of your message). Without it, a numbered answer or a "go" means nothing on another day.
- The markers (`## <date> <HH:MM>`, `**Coordinator asked:**`, `**User:**`) are always these, in English; the content stays in the user's language.
- The only change you may make to the user's text: a secret (token, password, key) becomes `[secret removed]`. Pasted material stays as pasted.

One writer per message. When kontext's capture hook is on, kontext appends each user message to today's dump by itself, in this same format, before you see it: do not append it again. Check with one Grep of today's dump for a piece of the message, and append it yourself only if it is not there (the hook is off, or it skipped the message). Answers given through a question tool reach the dump at the end of the turn; if you need to cite one before that, append it yourself in this format.

If this is the conversation the human types into, name it once with `kontext capture here`: once any conversation is named, the hook captures only named ones, so a coordinator sharing your folder does not get into the dumps. A conversation that gets its messages from other agents never names itself. If the Grep shows the hook did not write a message, append it yourself as above and check `kontext doctor`: the conversation's id changes on `/clear`, and it has to be named again.

In a linked git worktree, `.pm/` is the main checkout's, not the worktree's: run `kontext doctor` once and use the path on its `.pm/ here:` line (under Capture) wherever this skill says `.pm/`, and put it in one line before the task text you give `kontext-pm-reader`. If doctor says this checkout captures, the hook saves each message: do not save the dumps by hand. If it says the worktree is not marked as the PM's, tell the user its fix.

## 3. Confirm

After saving, list back the ideas you took from the message, with the ids you gave them in items.md (section 4), and ask whether you missed anything. Their answer is a user message like any other: it goes to the dump too (section 2, with your question under `**Coordinator asked:**`).

## 4. items.md is a pointer table, not a second roadmap

```markdown
| id | idea | source | state | where | evidence |
|---|---|---|---|---|---|
| P1 | CSV export of invoices | dumps/2026-11-07.md:14 | in progress | U-4c1 | |
| P2 | drop the old export | dumps/2026-11-07.md:23 | done | T-9a0 | PR #12 |
| P3 | invoices in PDF | dumps/2026-11-07.md:30 | decided | D-77e | |
```

- One row per idea: `id` (`P1`, `P2`…, never reused), the idea in a few words, `source` (`dumps/<file>:<line>`), `state` (pending, in progress, decided, done or discarded; decided: a decision settled it and there is nothing to build), `where` (the id of the kontext feature, task or decision that covers it; a PR or a doc may follow it) and `evidence` (for done: a PR, a commit, a test run or a `shots/` path).
- Every row's `where` points to a feature (`U-…`), a task (`T-…`) or a decision (`D-…`), unless the user discarded the idea. An epic, a question or plain text is not enough: `kontext ideas` and `kontext tree` list such a row under "Not placed" until it has one. If the idea is something the human will see working, give it a feature (`kontext feature add "<title>" --epic <name> --acceptance "<what the human sees>"`); if it is a piece of work inside one, a task.
- `discarded` only when the user says so: cite where in `evidence` (`dumps/<file>:<line>`).
- When an idea becomes work, create it in kontext as the `kontext-orchestrate` skill says, and write its id under `where`.
- Do not copy plans, task lists or acceptance criteria into items.md: they live in kontext.
- `kontext ideas` shows each idea's state as the ledger has it, through the ids in `where`: an open question or a pending proposal is waiting for the user; tasks or epics are pending, in progress or done by their tasks; a feature by its own state. Otherwise your `state` stands. It also lists the rows that are not placed and how many of the user's messages no audit has seen.

## 5. Coming back: the coverage audit

When the user comes back after time away, or asks where things stand:

1. **Do not read the dumps yourself**: that is the point, your context stays small. Run `kontext ideas --mark-audited` right before you launch the reader (it records which messages this audit covers, so the next `kontext ideas` counts only the newer ones). Then launch the `kontext-pm-reader` subagent (in Claude Code, the `Agent` tool with `subagent_type` `kontext-pm-reader`) and give it the text under "Audit task" below, word for word. It answers with JSON: `ideas` (every idea it found) and `missing` (those not in items.md), each with `quote`, `file` and `line`.
2. **Verify each idea in `missing`** before you believe it: one targeted Grep of items.md (and of the places its rows point to) for that idea, and one Grep of the cited file for a piece of the quote. Drop the ones that are covered: on real data the reader flags an idea that items.md has in other words, and now and then a quote is not verbatim.
3. **Add the confirmed ones** to items.md as pending, with the reader's citation as their source (`dumps/<file>:<line>`, without the `.pm/` in front), and give each a `where` (section 4) as soon as it has a feature, task or decision.
4. **Show the user where things stand**, following the `kontext-show` skill for how to show it (do not repeat its rules here). The ideas newly found in step 3 show under "Not placed" in `kontext tree` until they have a `where`.

When the user asks "how are we doing?", answer with the output of `kontext tree` in a code block and at most two sentences of your own (what changed, what you need from them).

If `kontext-pm-reader` is not among your agents (`kontext install` installs it; `kontext install --skills-only` brings only the skills), say so and ask the user before reading the dumps yourself.

## 6. Values

When you need the value in force of something the user said (a limit, a name, a default, whether they changed their mind), do not read the dumps: launch `kontext-pm-reader` with the text under "Value task" below, word for word, with `{questions}` replaced by your questions, one per line (`- q1: <question>`, `- q2: …`). Grep the quote of each answer in its file before you act on it.

## 7. status.md

Rewrite it at the end of each block of work: ~40 lines at most. Where things are, what waits for the user, what comes next. It is not a log: the dumps and kontext have the history.

## 8. Public repos

If the repo is public (check its remote, or ask), ask the user before committing `.pm/`: the dumps are their words, verbatim. Offer to add `.pm/` to `.gitignore` instead. The capture hook gitignores `dumps/` inside `.pm/` by default (`.pm/.gitignore`).

## 9. Keep this conversation small

This is the conversation the user talks to, and it runs for days: every later request re-reads whatever entered it, a file you read, a command's output or an agent's report. On a real project, tool results were most of that history and what the user typed a tiny part of it.

- Bulk reading and long shell work go to a subagent that returns a short answer: `kontext-scout` for the code and the docs, `kontext-pm-reader` for `.pm/` (sections 5 and 6). A small, targeted read (the file you are about to change, one Grep) is fine here; the rule is about volume.
- Ask every agent you launch to hand back a few lines and to put the detail in kontext (the `kontext-orchestrate` skill says how).
- When a phase closes (a wave or an epic done, its state in kontext), tell the user in one line that this is a good moment to compact (`/compact`): only they can, and kontext gives the context back afterwards. Once per closed phase, not again until the next one closes.

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
