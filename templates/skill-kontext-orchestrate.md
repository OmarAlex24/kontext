---
name: kontext-orchestrate
description: Sets up and coordinates a team of agents on kontext when the work is big. Use it when starting an idea or a project from scratch (greenfield), when the work spans several epics or fronts, when the user says "this is a lot of work", or when they ask you to set up, orchestrate, or coordinate several agents. It measures the size, proposes the structure (PM → one coordinator per epic → scouts, implementers, and testers) with its cost, waits for the go-ahead, and sets it up with the harness tools where you run.
---

# Orchestrating with kontext

`kontext` does not orchestrate: it keeps the memory and the contract that everyone shares. You decide how much structure is needed and set it up with what the harness offers. To read and write in `kontext`, follow the `kontext` skill.

## 1. Measure the size before starting

Check four criteria. The highest one wins:

| Criterion | S–M | L | XL | XXL |
|---|---|---|---|---|
| Epics | none new: one task | one | two or more | several, and there is no code yet |
| Open architecture decisions | none | one or two, inside the epic | one or more per epic | the foundational ones: stack, data, deployment |
| How much can run in parallel | nothing | tasks within one epic | independent epics | independent epics, after some foundations |
| How much is unknown | little: one scout is enough | bounded to the epic | how the epics fit together | almost everything: the charter is missing |

| Size | Structure |
|---|---|
| **S–M** | You alone. One task in `kontext`, in the right epic, closed with evidence; a scout if something needs to be looked up. No extra confirmation |
| **L** (one epic) | This session coordinates the epic. Implementers and testers in parallel; scouts with a small model |
| **XL** (several epics) | PM (this session, the only one that talks to the user) → one coordinator per epic, each in its own worktree → their workers. In waves, following `kontext plan` |
| **XXL** (greenfield) | Like XL, with phase 0 before any code |

## 2. Before launching anything XL or XXL: confirm

Set up the structure (section 3), run `kontext plan`, and send this message. Launch nothing until the user says yes:

```
This is <XL|XXL>: <n> epics; <the highest criterion> set the size.
Waves (kontext plan):
  1. <foundations> (in series)
  2. <payments>, <catalog>, <notifications> (in parallel)
  3. <integration and end-to-end tests>
Structure:
  - PM: this session (<large model>). Talks to you and bundles the questions.
  - Coordinators: <n>, one per epic, each in its own worktree (<large model>), via <tool>.
  - Workers per epic: ~<n> implementers (<medium model>), ~1 tester (<medium>), ~<n> scouts (<small>).
Total: ~<N> agents. The expensive ones are the coordinators and the implementers.
I'll pause at each wave boundary for a standup with you. Shall I start?
```

Count like this: one coordinator per epic; as many implementers as tasks ready at once, up to 3 per epic; one tester per epic; one or two scouts per epic. If the user or the project already fixes models, use those.

## 3. Waves

**Phase 0, no code (XXL).** With the user, in the conversation: the charter (`kontext charter`), the goals (`kontext goal add`), and the architecture decisions (`kontext decide`). They are signed together with the plan, at the end of phase 0 (below), not one by one; a decision you write before then needs `--approved-by`, only once they have approved it in the conversation. Then, the epic map, with the features of each epic: what the human will see working, each with its acceptance written as what they see:

```sh
kontext epic add "Foundations" --goal <G> --name foundations
kontext epic add "Payments" --goal <G> --name payments --after foundations
kontext feature add "Card charge" --epic payments --acceptance "<what the human sees>"
kontext feature add "Refunds" --epic payments --acceptance "the owner refunds an order from the admin and the customer gets the money back"
kontext epic add "Integration" --goal <G> --name integration --after payments --after catalog
kontext plan            # exits with 1 if there is a cycle or an epic that does not exist: fix it before going on
kontext tree            # the map as the user will see it: waves, epics and features
```

`--after` accepts an epic that does not exist yet, so the map can be written in any order. Tasks hang from features (`kontext task add "<title>" --feature <U> --acceptance "<how it is checked>"`); an epic with features closes when all of them are done. Only the pm or an owner adds features.

**Mockup, if there is an interface.** Before any code, make an HTML mockup of the main screens from the charter (a single file, no dependencies), take a screenshot of it, and show it to the user (skill `kontext-show`). Adjust until they say yes. Then register it as the reference, with its signature:

```sh
kontext decide "The screens follow the approved mockup" --epic <epic> \
  --attach mockup.html --attach mockup.png --screen <screen> --approved-by <owner>
```

The coordinators build against that reference (it appears in "How it looks" in the report), and each worker closes with a screenshot of the same screen (`--evidence screenshot.png --screen <screen>`), so the human sees the before and after.

**Sign the plan: phase 0 ends here.** Show the user `kontext scope summary` verbatim, run from the repo that has `.pm/` (elsewhere it says "Ideas not checked", and a signature run from another folder than the summary is refused): the charter, the goals, each wave with its epics, features and acceptance, and the ideas not placed (a plan with any is refused: place or discard them first). On their yes ("sí" is enough), sign it with the hash it ends with: `kontext scope sign --hash <hash> --approved-by <owner> --said "<their words>"`. From then on you decide alone inside it (section 10 of the `kontext-pm` skill), and a change of features, epics, goals or the charter is a scope change they sign the same way; its new work is not handed out until they do. Launch no wave before the plan is signed.

- **Wave 1:** the foundations epic, in series (one coordinator).
- **Middle waves:** the independent epics in parallel, one coordinator each (what `kontext plan` puts in the same wave).
- **Final wave:** integration and end-to-end tests.

**Within a wave**, the coordinators do not talk to the user: they escalate with `kontext ask --to pm`. You answer what the charter or a decision answers (`kontext answer`), and bundle the rest for the boundary. Only interrupt the user if the whole wave is blocked. If the user has unanswered pending items (`kontext inbox count`), do not add more: the wave keeps going with whatever does not depend on them.

**At every wave boundary:** `kontext plan` (what closed and what comes next) and `kontext tree`, the report with the `kontext-show` skill (status paragraph, what is waiting for them, problems, and screenshots), their answers to what you bundled (`kontext inbox`, `kontext answer`, signed decisions), and their go-ahead for the next wave.

In XL there is no phase 0 if the charter is already signed: start from the epic map.

## 4. Translation by harness

Check which tools you have in this session (including the deferred ones) and use the first row that applies:

| If you have | Coordinators | Workers |
|---|---|---|
| T3 Code: `delegate_task`, `t3_thread_launch`, and the other `t3_thread_*` | One thread (`t3_thread_launch` with `workspaceStrategy` `worktree`) or one delegated task (`delegate_task` with `isolation: "worktree"`) per epic. Follow them with `t3_thread_wait` or `task_status`, and with `kontext resume --as pm` | Each coordinator launches its own (`Agent` or `delegate_task`) |
| Claude Code with the `Workflow` tool, **and the user asked for multi-agent orchestration** | One script per wave: one step per epic in the wave | Steps in the script, with the `kontext-*` roles |
| Claude Code with `Agent` | This session acts as PM and coordinator, one epic at a time, in the order of `kontext plan` | `Agent` with `subagent_type` `kontext-implementer`, `kontext-tester`, or `kontext-scout` (installed by `kontext install`); several in the same message to run in parallel |
| Codex or another agent CLI | Only if the user names it | Same |
| No way to launch agents | In series: you play each role in turn, with the same structure of epics and tasks in `kontext` | You, one task at a time |

A coordinator in a worktree of this machine shares the kontext-home and needs nothing more. One that runs elsewhere (a cloud thread, a sandbox, another machine) needs the project on a kontext server: make it a token with `kontext token create --role coordinator --epic <name> --ttl 8h` and give it the printed variables through the harness's environment or secrets, never in the repo (and in the prompt only if there is no other way: prompts are kept). With them, its first kontext command sets the project up, and everything it writes reaches you on your next command; its workers write through the same token.

Do not depend on subagents that launch subagents. The coordinator lives where the harness can keep long processes running (a thread, a worktree, a workflow); workers are always the lowest level and do not launch anyone. If a coordinator cannot launch workers, it returns the briefs and you launch them yourself.

## 5. The contract, the same with any harness

| Level | Identity | Starts with | Closes with |
|---|---|---|---|
| PM | `--role pm` | `kontext resume --as pm` | `kontext plan` and `kontext standup` at each boundary |
| Coordinator | `--actor agent:epic:<x> --role coordinator` (and `KONTEXT_EPIC=<x>` in the environment you launch it with, for its hook) | `kontext brief <x>` | every task closed with evidence, the epic's PR stack, a `kontext log` with the summary, and its report to the PM |
| Implementer or tester | `--actor agent:<x>-impl-1 --role implementer` | its brief: the output of `kontext next --epic <x> --claim` (with the mark) | `kontext done <T> --run "<test>" --since <mark>` |
| Scout | `--actor agent:scout --role scout` | only the question | `kontext fact "…" --path <path>` and a hand-back of about five lines |

- The identity goes in the arguments of each command (`kontext --actor … --role … <command>`), not in variables in front: a `Bash(kontext:*)` permission does not cover `KONTEXT_ACTOR=… kontext …` (the `kontext` skill, "How to invoke kontext").
- The coordinator takes each task on behalf of its worker (`kontext --actor agent:<x>-impl-1 --role implementer next --epic <x> --claim`) and pastes the full output into its brief.
- Everyone escalates with `kontext ask` one level up (`--task <T>` for the worker, `--to pm` for the coordinator). Nobody decides: they propose.
- Each epic comes out as its own PR stack, on its own branch or worktree.

**Keep the PM's and the coordinators' conversations small.** They run longest, and every later request re-reads whatever entered them. Do not do bulk reading or long shell work there (several files, searches, commands with long output): give it to a scout, which answers in a few lines; a small, targeted read is fine. In every brief you write, ask for a short hand-back: the detail goes into kontext (`kontext log "…" --task <T>`, `kontext done <T> --run "…" --evidence …`, `kontext fact`), and the report back is about five lines at most: what was done, the ids, and what is needed from whoever launched it.

## 6. When something fails

- **`done` exits with 3 (stale context):** something that touches the task came in after the mark. The worker reads it (`kontext show <id>`), adjusts, and retries with the new mark that `done` gives it. If the worker has already finished, the coordinator launches it again with those entries and the new mark; it never closes the task itself by skipping what is new.
- **Lost or expired claim** ("stop working on it", or the task went back to ready): the worker stops; the coordinator hands it out again with another `next --claim`.
- **A worker fails or does not finish:** `kontext release <T> --why "…"` and another worker with a new `next --claim`. If it fails twice, split the task or ask with `kontext ask --to pm`.
- **A blocked coordinator** (`next --claim` exits with 2 and nothing moves): leave a `kontext log` with the status, ask `kontext ask --to pm` what is blocking it, and report back "blocked: Q-…". Answer if the charter or a decision allows it; if not, it goes to the boundary, or you bring the standup forward if the whole wave is stuck.
- **A coordinator or thread was cut off:** launch another one with the same identity; `kontext brief <x>` gives it the board. The claims of the workers that died expire on their own (2 h) or are released with `kontext release`.
- **`kontext plan` exits with 1:** do not launch the wave. Create the missing epic, or ask the user to replace the one that closes the cycle (`kontext supersede`).
