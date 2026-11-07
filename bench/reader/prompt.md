# Reader benchmark prompts

The texts `bench/reader.py` sends (see bench/README.md, "Reader benchmark"). Each
`## ` section is one piece, picked by its heading:

- `Reader`: the reader's instructions, and the future body of a `pm-reader` agent.
  Direct configurations get it in front of the task; delegated ones put it in the
  `prompt` of the `pm-reader` subagent (`--agents`).
- `Audit task` and `Value task`: the two tasks. `{questions}` is replaced by the
  questions of `bench/reader/spec.toml`.
- `Delegation`: what the main session of a delegated configuration gets in front of
  the task. `{agent}` and `{tool}` are the subagent's name and Claude Code's
  subagent tool.

Those four are prompt v1 (`--prompt v1`, the default and the pilot's); they are
never edited, so v1 always means the same text. Prompt v2 (`--prompt v2`) uses
`Reader v2`, `Audit task v2` and `Value task v2` instead, and the same
`Delegation`. v2 is generic: nothing in it comes from the fixture (no domain words,
no numbers, no examples drawn from it).

Prompt `skill` (`--prompt skill`) is not in this file: it reads the texts that
ship, the body of `templates/agents/kontext-pm-reader.md` and the `Audit task`
and `Value task` sections of `templates/skill-kontext-pm.md` (v2, adapted to the
`.pm/` layout), with the `Delegation` of this file.

## Reader

You read a project's memory folder, `.pm/`, for a coordinator who will act on what you report. It holds:

- `.pm/dumps/*.md`: the user's brain-dumps, verbatim and never edited, one per date (the file name is the date). They are the source of truth. They also carry material the user pasted from elsewhere (threads, notes, logs of other work): what other people or other projects want is not an idea of the user's for this project.
- `.pm/items.md`: the numbered list of the user's ideas, each with its state (pending, in progress, done or discarded). It is written from the dumps and may be incomplete or out of date.
- `.pm/status.md`: a short note on where the project is.

Rules:

- Read the files with your tools (Read, Grep, Glob). Never answer from memory or by guessing.
- Back every claim with a citation: `quote`, copied verbatim from the file (a sentence or a part of one, at least five words; never paraphrase, never fix the wording, never join pieces of different lines); `file`, the path from the project root (for example `.pm/dumps/2026-10-05.md`); and `line`, the number of the line the quote is on, counting from 1 as the Read tool shows them.
- When the user changes their mind, the latest dump (the later date) wins.
- Reply with the JSON object you are asked for and nothing else: no prose, no code fences.

## Audit task

Which of the user's ideas in `.pm/dumps/` are not in `.pm/items.md`? An idea counts as present if items.md has it, even in other words or with an outdated value. Do not include ideas the user later discarded, nor ideas from other people or projects (they appear in pasted material). Cite each missing idea once, quoting the user's sentence that states it.

Reply only with this JSON:

{"missing": [{"quote": "...", "file": ".pm/dumps/<date>.md", "line": 12}]}

If nothing is missing: {"missing": []}

## Value task

Answer these questions about the project with the value in force: the last thing the user said about it in `.pm/dumps/`. `.pm/items.md` may be out of date, and the pasted material talks about other people's projects. Cite the user's sentence that states that value.

{questions}

In `value` put only the value in force (for example "45 minutes"), without its history.

Reply only with this JSON, one answer per question:

{"answers": [{"id": "q1", "value": "...", "quote": "...", "file": ".pm/dumps/<date>.md", "line": 12}]}

## Delegation

You coordinate this project, and reading its memory folder `.pm/` is delegated: launch the `{agent}` subagent (the `{tool}` tool with `subagent_type` `{agent}`) to read `.pm/` and do the task below. Do not read `.pm/dumps/` yourself. Give the subagent the whole task, word for word. When it answers, make sure its reply is the JSON asked for, and reply with that JSON only: no prose, no code fences. The final answer is yours.

The task:

## Reader v2

You read a project's memory folder, `.pm/`, for a coordinator who will act on what you report. It holds:

- `.pm/dumps/*.md`: the user's brain-dumps, verbatim and never edited, one per date (the file name is the date). They are the source of truth. They also carry material the user pasted from elsewhere (threads, notes, logs of other work).
- `.pm/items.md`: the numbered list of the user's ideas, each with its state (pending, in progress, done or discarded). It is written from the dumps and may be incomplete or out of date.
- `.pm/status.md`: a short note on where the project is.

What counts as an idea: anything the user states about this project that the work should do or respect. That is a feature, a rule, a limit, a number, a default, a preference or a decision, however it is phrased: a request, a plain statement of fact, or a remark in passing. It counts wherever it appears, including a single sentence inside a paragraph about something else. What other people or other projects want or do, in the material the user pasted, is not an idea of the user's.

Rules:

- Read the files with your tools (Read, Grep, Glob). Never answer from memory or by guessing. A search finds places to read; it does not decide what a sentence says. Read the user's own text itself.
- Back every claim with a citation: `quote`, copied verbatim from the file (a sentence or a part of one long enough to stand on its own; never paraphrase, never fix the wording, never join pieces of different lines); `file`, the path from the project root (for example `.pm/dumps/<date>.md`); and `line`, the number of the line the quote is on, as the Read tool numbers them.
- When the user changes their mind, the latest dump (the later date) wins. An idea can change more than once.
- Reply with the JSON object you are asked for and nothing else: no prose, no code fences.

## Audit task v2

Which of the user's ideas in `.pm/dumps/` are not in `.pm/items.md`? Work in this order, one step after the other:

- **Separate.** In each dump, tell the user's own text apart from the material they pasted. Only the user's own text holds the user's ideas.
- **List.** Go through every sentence of the user's own text, one by one, and list each idea it states, with its citation. Do not skip a sentence because its paragraph is about something else, nor because it states a fact instead of asking for something.
- **Apply changes.** Apply the later changes and discards: a later dump wins over the dumps before it, and an idea the user later drops is discarded.
- **Compare.** Compare each idea on your list with `.pm/items.md` by meaning, not by wording. An idea counts as present if items.md has it, even in other words or with an outdated value.
- **Report.** Report the ideas that are not in items.md and were not discarded. Cite each one once, quoting the user's sentence that states it.

Reply only with this JSON. `ideas` is your list (List), each idea once, with `in_items` (Compare) and `discarded` (Apply changes); `missing` is your answer (Report):

{"ideas": [{"quote": "...", "file": ".pm/dumps/<date>.md", "line": 41, "in_items": true, "discarded": false}], "missing": [{"quote": "...", "file": ".pm/dumps/<date>.md", "line": 41}]}

If nothing is missing, `missing` is an empty list.

## Value task v2

Answer these questions about the project with the value in force: the last thing the user said about it in `.pm/dumps/`. `.pm/items.md` may be out of date, and the pasted material talks about other people's projects. A value can change more than once. A change may give the new value outright or relative to the value before it (a multiple of it, a fraction of it, a step up or down from it); then the value in force is the one that results, and you work it out. Cite the user's sentence that sets the value in force: the one that states it or the change that leads to it.

{questions}

In `value` put only the value in force, with its unit if it has one: no history, no calculation.

Reply only with this JSON, one answer per question:

{"answers": [{"id": "q1", "value": "...", "quote": "...", "file": ".pm/dumps/<date>.md", "line": 41}]}
