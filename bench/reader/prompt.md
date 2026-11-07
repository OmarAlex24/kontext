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
