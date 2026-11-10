---
name: kontext
description: Project memory (charter, goals, decisions in force, open questions, facts, and the distill log). Use it before starting or resuming work, when you need to know what was decided, why, or what is still pending, when a question can be answered by the charter or a decision, and when you have something to save that the distill does not record on its own (a question for the owner or the pm, a finding in the code, a proposal of yours, progress on a task).
---

# kontext: the project's memory

The conversation is disposable: it gets compacted, cut off, or switched to another session. Whatever is not in `kontext` is lost. Whatever is in `kontext` is the truth, even if this conversation says otherwise.

## Before working: read

- `kontext resume --as <role>`: the charter, the goals, the decisions in force, what is open, and what is new since your last read. Read it at startup if the hook did not already put it in your context.
- `kontext show <id>`: a full entry with its history (why it was decided, what it replaced, who answered). Accepts prefixes: `kontext show D-7f3`.
- `kontext recall <words>`: searches the ledger and the earlier conversations. Use it before asking something that may already have been discussed.
- `kontext find <topic>`: searches the docs of the project's repos (ADRs, READMEs) and says which section to read; `kontext find --show <path:line>` prints only that section.

Do not read the whole ledger or the full history: your view already has what matters. Add `--json` if you are going to process the output. The same holds for everything that enters this conversation, because every later request re-reads it. If you are the pm or a coordinator, send bulk reading and long shell work (several files, searches, commands with long output) to a subagent that answers in a few lines (`kontext-scout`, or `kontext-pm-reader` for `.pm/`); a small, targeted read is fine. Trim long output before it enters: pipe a command through `head`, `tail` or `grep`, or write it to a file and summarize it (not `kontext` itself, which goes without pipes: see below).

## How much each thing weighs

- **Decisions in force** (`D-…`, or an accepted proposal): these are the rule. If what you are asked contradicts them, say so and propose the change.
- **What the user said**: the person said it in an earlier conversation, and the distill recorded it with its date. **It applies even when unsigned**: the signature is for the charter, not for believing the user. Use it as current data, without asking them to confirm it again and without saying "nothing is in force yet", for as long as nothing replaces it. If it clashes with a decision in force, the decision wins and you ask the user. A **`[user rule, in force]`** is a rule or instruction the user gave: apply it as stated; if it contradicts a signed decision in force, the decision wins and you raise it with `kontext ask`. Items marked `[proposal]` (scope, architecture) are still proposals that need an owner's signature, but the user stated them: do not treat them as an agent's idea.
- **Superseded things no longer apply.** When the user changes a value, the new line says "supersedes <id> "old value", which no longer applies": answer with the new value and, if relevant, mention the old one only as the previous one.
- **`[unreviewed]` proposals from an agent**: ideas, not rules. Mention them if relevant; do not apply them as if they were law.
- **Facts**: what someone found. Facts with a fingerprint expire on their own when their file changes; the distill records the `[unreviewed]` ones itself.
- Anything that `kontext show` or `kontext recall` marks as **superseded** is no longer valid: follow the chain to the current one.

## While working: write

**With the distill on** (the hook or `kontext resume` says: "The kontext distill already saves on its own…"), when the conversation with the human is closed or compacted, the distill records what the user said, decided, or asked, and the progress. Do not run `kontext propose` or `kontext log` to repeat that: it gets recorded twice, and each command is one more round for the model that rereads the whole context. In that conversation, kontext is for consulting (`show`, `recall`), for asking the human (`ask`), and for what the distill cannot pull out of the conversation: a finding in the code (`kontext fact --path`), a proposal of yours to change a decision, and the work on a task (`kontext log --task`, `kontext done`). Subagents (coordinator, implementer, tester, scout) do not go through the distill: they write their own progress. The commands:
- `kontext log "<what you did or found>" --next "<what comes next>"`: a progress entry. Write it when you finish an important step and before the session can be cut off (with the distill on, only for a task's work).
- `kontext propose "<title>" --why "<why>" --body "<detail>" [--supersedes <D-id>]`: when you think something needs a decision or a decision needs to change.
- `kontext ask "<question>" --to owner|pm|epic:<x> [--task <T>] [--body "<context>"] [--blocking]`: when you need an answer that is not in the charter or the decisions. If there is already an open question that says the same thing, `ask` joins that one instead of repeating it. If the human already has many pending items, your question is marked as not urgent; use `--blocking` only if it really stops your work.
- `kontext answer <Q-id> "<answer>"`: if you are the one to answer.
- `kontext fact "<finding>" --path <file>`: something you found in the code, with the file's fingerprint; it expires on its own when that file changes.

Write with your agent identity. If the hook says "Writing as agent:…", it is already in the environment and nothing else is needed. If not, pass it in the arguments: `kontext --actor agent:<your-name> --role <role> [--model <model>] log …`. Without it, you show up as `agent:unknown`.

## How to invoke kontext

Invoke `kontext` as a bare command that starts with `kontext`. Claude Code usually grants permission with `Bash(kontext:*)`, and that permission covers only that. In a session where nobody can approve (`claude -p`), anything else does not run:

- Nothing in front: no `KONTEXT_ACTOR=… kontext …`, no `export KONTEXT_ACTOR=…` on another line, no `timeout 60 kontext …`. The identity goes in `--actor`, `--role`, and `--model`.
- Nothing around it: no `cd <repo> && kontext …`, no `( kontext … )`, no `kontext … && cat …`. One command per call, from the repo.
- No pipes: no `kontext resume … | head`, no `| sed -n '/What's new/,$p'`, no `2>&1 | tail`. `resume` is already trimmed to its budget; if you only want what is new, use `kontext standup`; to process the output, use `--json`. The text goes in quotes in the argument (`kontext log "…" --next "…"`), not in a heredoc.

## Never decide

Decisions, charter and goal signatures, and accepting or rejecting proposals belong to a human owner. `kontext decide`, `kontext sign`, `kontext accept`, and `kontext reject` will refuse you, and that is fine: propose with `kontext propose` and move on.

The only exception is the pm, when an owner has already approved it in the conversation: `kontext --role pm decide … --approved-by <owner>`. Do not use it to guess what the owner would want.

## How to escalate

1. If the question is answered by the charter or by a decision in force, answer it yourself and move on.
2. If not, ask one level up (`kontext ask --to pm`, or `--to owner` if you are the pm) and move on to something else while the answer is pending.
3. **If the human does not answer, they have not read anything.** Do not pile more questions on them: move on with whatever does not depend on them. `kontext ask` already warns you if they have many pending items; the skill `kontext-show` says when and how to show them.
4. If what you are asked contradicts the charter or a decision in force, do not go along with it silently: say so, cite the id (`D-…`), and propose the change with `kontext propose --supersedes <id>`.

## Teamwork (epics and tasks)

If you are the pm and the work is bigger than one session:

1. Make sure the charter and its goals are signed (`kontext resume --as pm` says so).
2. `kontext epic add "<title>" --goal <G-…> [--name <short>]`: one epic per front of work.
3. `kontext task add "<title>" --epic <name> --acceptance "<how it is checked>" [--depends-on <T-…>]`. Without `--goal`, the task serves the goals of its epic.
4. Launch the `kontext-coordinator` subagent with the output of `kontext brief <name>` in its brief. It distributes the work with `kontext next --epic <name> --claim` and escalates to you with `kontext ask --to pm`.
5. You see one line per epic in `kontext resume --as pm`, not the tasks. Answer what was escalated with `kontext answer`.

If there are several epics or a project from scratch, follow the skill `kontext-orchestrate`: measure the size, order the epics with `--after` and `kontext plan`, and confirm with the user before launching.

If you are an implementer: your brief is the output of `kontext next`, with its mark. You close with `kontext done <T> --run "<test>" --since <mark>`; if it exits with 3 (stale context), read what is new before retrying.

## What is waiting for the human

`kontext inbox` is their inbox: what they have to sign, answer, and review, plus the problems, sorted by what they block. It is derived from the ledger. Looking at it closes nothing; only the human closes items (`kontext inbox sign|answer|dismiss|confirm`). You do not close them on their behalf. It is not your task list: do not check it as a routine. If you are the pm, the hook already tells you at the start how many pending items they have; tell them once, and show them the inbox when they ask or when the skill `kontext-show` says so. Every task with an interface closes with a screenshot: `kontext done <T> … --evidence screenshot.png --screen <screen>`.

## When closing

Before you finish or the conversation gets compacted, leave a `kontext log` with what was done, what is left, and the next step. The next session starts from there, not from this conversation. With the distill on, you do not need it in the conversation with the human: the distill records it. Only the human can compact a conversation (`/compact`). If you are the pm, when a phase closes and its state is in kontext, tell them in one line that this is a good moment to compact, because kontext gives the context back afterwards; once per closed phase, not more.
