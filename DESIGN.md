# kontext: design

> Status: phases 1 to 6 implemented, plus the human's inbox and the report (§20), and the signed scope (§4.3, principle 7). The benchmark has a harness, but it has not been run. What changed during implementation is noted in "Implemented in phase N" notes.
> Background: the v0 `ctx.py` prototype (ledger with fingerprints, decisions that only a human signs, start hook and distill). This design generalizes it to any project, with several agents and several people.

## 1. The problem

A conversation with an agent that lasts weeks gets lost: it gets compacted, cut off, moved to another session. By the third day, neither the agent nor the person remembers what was said, what was decided or how things stand. The work can keep moving forward and still drift away from the direction that was set at the start.

kontext solves this with one rule: **the conversation is disposable; the truth lives in a ledger.** If a session dies and the next one starts with everything important in view within two seconds, it doesn't matter that the project lasts weeks.

### What it has to achieve

1. You can learn in one minute what happened since the last time you looked and what it expects from you.
2. Each agent starts with the direction and the decisions in force in front of it, without reading the full history.
3. Work that drifts from the direction shows up on its own, without relying on someone discovering it.
4. Several agents and several people can write at the same time without stepping on each other.
5. Measure how much a project drifts with and without kontext, and publish it.

### What it doesn't do

- **It doesn't orchestrate agents.** Launching subagents, choosing models and splitting up work is done by Claude Code, T3 or whichever agent you use. kontext is the memory and the rules. That's why it can be small.
- **It doesn't replace git** and doesn't store code edits. It stores what matters: what was decided, what was found, what was closed and with what evidence.
- **It doesn't replace the ADRs** of a project that already has them. `kontext find` reads them like any other doc.

## 2. Principles

1. **Entries are only ever appended.** Nothing is edited or deleted. To correct something, you add another entry that answers, closes or replaces it. The current state is calculated when reading.
2. **A signed anchor.** The project charter (objective, goals, what is not being done, when it's considered finished) is signed by a human. Each task says which goal it serves. Changing direction is an explicit decision.
3. **One view per role, with a cursor.** Each reader gets only its part and only what is new since the last time.
4. **Agents don't talk to each other; they write to the ledger.** Doubts move up a level only if the current level can't answer them with the charter and the decisions.
5. **What is discovered is saved with its fingerprint.** A fact knows which file and commit it came from, and it expires on its own when that code changes.
6. **No task is closed without evidence.**
7. **Deterministic first; the human signs the scope.** What kontext computes (the state, the views, the plan, the summary the human signs) comes from the ledger, never from a model. Models propose (the distill, the benchmark judge) and change nothing on their own, with one exception the human grants: once they sign the plan, the pm decides inside it, architecture included, and logs why. The scope itself (the charter, the goals, the epics and their features) changes only through a scope change the human signs, on its full summary, bound by its hash; work they already signed keeps moving meanwhile, and the new work waits. Without a signed plan, only an owner decides, as before.

## 3. Concepts

| Concept | What it is |
|---|---|
| Project | A unit of work with a charter. It can span several code repos |
| `kontext-home` | The git repo where a project's ledger lives. It is separate from the code repos |
| Writer | A person on a machine, for example `omar@mac`. Has its own file in the ledger. All the agents on that machine write to that file, and each entry says which agent wrote it |
| Entry | A line in the ledger. Never changes |
| Reader | Whoever reads a view: `omar`, `pm`, `epic:payments`. Has its own cursor |
| Role | `owner` (human who signs), `pm`, `coordinator`, `implementer`, `tester`, `scout` |
| Epic | A group of tasks with a coordinator |

## 4. Storage

### 4.1 Layout of `kontext-home`

```
<kontext-home>/                     git repo; synced with push and pull
  project.toml                  name, owners, repos, roles and budgets
  ledger/
    omar@mac.jsonl              one file per writer; lines are only appended
    omar@t3-machine.jsonl
    ana@laptop.jsonl
  cursors/
    omar/omar@mac.json          cursor of the reader "omar", written from that machine
    pm/omar@t3-machine.json
  .kontext/                         local, in .gitignore
    index.sqlite                index; can be deleted and rebuilt
    locks/
    distill-offsets.json
    sessions/                   what the start hook gave each session (§9.4)
```

Why one file per writer: since nobody writes to someone else's file, git never conflicts when merging, and syncing is just joining files. Beads started with JSONL in git and dropped it because its records were edited and conflicted. Here nothing is edited.

### 4.2 Format of an entry

One JSON line per entry:

```json
{
  "v": 1,
  "id": "D-7f3a9c",
  "writer": "omar@mac",
  "n": 412,
  "prev": "c83f…",
  "hash": "9e02…",
  "hlc": "2026-11-03T18:22:10.123Z.0004",
  "actor": { "type": "agent", "role": "coordinator", "name": "epic:payments", "model": "claude-opus-5-5", "session": "51a2…" },
  "kind": "decision",
  "title": "Charge with Stripe, not Conekta",
  "body": "",
  "why": "Conekta doesn't support recurring charges with 3DS",
  "tags": ["payments"],
  "refs": [],
  "fields": { "epic": "payments", "guard": { "cmd": "! grep -ri conekta src/", "expect": "exit0" } },
  "sig": null
}
```

| Field | Rule |
|---|---|
| `id` | Type prefix plus 6 hex characters of the sha256 of the canonical content (without `id`, `n`, `prev`, `hash`, `sig`). It is stable forever and doesn't clash between writers. The tool accepts any unique prefix (`D-7f3`) |
| `n` | Position in the writer's file, starting at 1 |
| `prev` and `hash` | Chain per file: `hash` is the sha256 of the canonical line, `prev` is the `hash` of the previous line in the same file. `kontext verify` walks each chain |
| `hlc` | Hybrid logical clock: the physical time plus a counter, to order entries from different writers even when their clocks don't fully agree |
| `actor` | `{"type":"human","name":"omar"}` or `{"type":"agent", role, name, model, session}` |
| `sig` | Optional (phase 5): SSH signature (`ssh-keygen -Y sign`) of the `hash`. Required to sign the charter, and to decide whether `project.toml` requires it |

> **Implemented in phase 1:**
> - Canonical JSON: keys sorted, compact, UTF-8 without escaping. `hash` = sha256 of the canonical form without `hash` or `sig`; the first line of each file has `prev` = 64 zeros.
> - The content of the `id` includes `writer` and `hlc`, so two writers who record the same thing get different `id`s. The duplicate case in 7.4 applies when exactly the same entry arrives (reimporting, syncing).
> - If the computed `id` already exists with different content (6 hex characters are 24 bits and occasionally collide), the `hlc` counter is advanced and the id is computed again.
> - The records in the table below (`sign`, `accept`, `claim`…) use the prefix `R`.
> - `hlc`: `YYYY-MM-DDTHH:MM:SS.mmmZ.CCCC`, in UTC with a 4-digit counter. It sorts the same way as text. The last one issued by each writer is stored in `.kontext/hlc-<writer>`, and when writing, the maximum is taken with the highest `hlc` already in the ledger.
> - Fields this version doesn't know are kept as they are: `hash` and `id` are computed over the JSON that was read, not over the struct.

> **Implemented in phase 5:** `sig` is a string: the armored signature from `ssh-keygen -Y sign -n kontext` (`-----BEGIN SSH SIGNATURE-----…`) over the `hash` in hex, with no line break. See §14.

### 4.3 Entry types

Entries with their own identity:

| Type | Prefix | Own fields |
|---|---|---|
| `charter` | `A` | `objective`, `non_goals[]`, `done_when` |
| `goal` | `G` | `charter`, `done_when` |
| `epic` | `E` | `goals[]`, `coordinator` |
| `decision` | `D` | `epic?`, `supersedes[]`, `guard?` |
| `proposal` | `P` | same as decision; written by an agent or the distill |
| `question` | `Q` | `to` (`pm`, `owner`, `epic:x`), `epic?` |
| `fact` | `F` | `fingerprints[]: {repo, path, commit, blob}` |
| `task` | `T` | `epic`, `goals[]`, `depends_on[]`, `acceptance` |
| `journal` | `J` | `task?`, `next?` |
| `feature` | `U` | `epic`, `goals[]`, `acceptance` |

Records that act on another entry (their `refs[0]` is the target):

| Type | Effect when the state is calculated |
|---|---|
| `sign` | The charter or a goal becomes signed. Only valid if the actor is an `owner` |
| `accept` / `reject` | A proposal becomes law or is dismissed. Only `owner`, or `pm` with `approved_by` |
| `answer` | A question becomes answered |
| `supersede` | A goal, decision or fact is replaced by `refs[1]` |
| `assign` / `claim` / `release` | The task gets an owner, or becomes free again |
| `done` | The task is closed. Requires `evidence[]` with at least one element |
| `reopen` | The task is opened again |
| `confirm` | Another agent found the same thing as a fact. Also: someone asks the same thing as an open question (§20), or the human confirms what the distill noted |
| `dismiss` | The human takes a pending item out of their inbox without resolving it (§20) |

`evidence[]`: `{type: "command" | "file" | "pr" | "url", value, exit_code?, excerpt?}`. A command without `exit_code` 0 does not count as evidence.

**"Expired" is not written:** it is calculated when reading. A fact expires if the current `blob` of any of its paths (`git rev-parse <commit>:<path>` against `HEAD`) no longer matches. That way nobody has to mark it, and there is no race to do so.

> **Implemented in phase 2:**
> - "Owner" is a human whose name is in `owners`, or an agent with `actor.role = "pm"` and `fields.approved_by` set to the name of an owner (the v0 rule for coordinator with `approved_by`). It applies to `sign`, `accept`, `reject`, `decision`, and to the `supersede` of charters, goals, decisions and proposals; anyone can write the `supersede` of a fact.
> - What was imported from v0 (`fields.v0_id`) is judged by the v0 rule, which had already accepted it: any human, or `agent:coordinator` with `approved_by`.
> - An entry that doesn't count is given the state `ignored` (if it has its own identity) and raises an alert. The command line also refuses to write it (exit code 2), so such entries only arrive from another machine or an old version.
> - As in v0, an accepted proposal without `--as-decision` counts as a decision in force and replaces what its `fields.supersedes` says. With `--as-decision`, a `decision` is written (`fields.from_proposal`) and the `accept` carries `fields.decision_id`.
> - `kontext charter` puts the signed charters in force into `fields.supersedes`; the replacement counts when the new one is signed. `supersede` also accepts charters.
> - `question` uses `fields.to`; `journal`, `fields.task` and `fields.next`; `charter`, `fields.objective`, `fields.non_goals` and `fields.done_when`.

> **Implemented in phase 3:**
> - `epic` also carries `fields.name`: the short name of the reader `epic:<name>` (taken from the title if not given). The default `coordinator` is `epic:<name>`, the actor name from 4.2. A task stores its epic's `id` in `fields.epic`; a decision or a fact touches an epic if its `fields.epic` is the `id` or the name, if they cite it in `refs`, or if they carry the name as a tag.
> - `assign` carries `fields.to`; whoever holds the task is stored as `agent:<name>` (in a `claim`, the actor) or the `to` as is. `done` carries `fields.evidence` and `fields.since` (the mark). A `question` about a task carries `fields.task` and `fields.epic`.
> - Who counts: `assign`, besides `done` and `release`, only counts if written by the epic's coordinator (by name), the pm (`actor.role = "pm"`, without `approved_by`) or an owner; `done` and `release` also count if written by whoever holds the task. Anything else is stored, doesn't count, and shows up as an alert. `reopen` can be written by anyone, with its reason. A `claim` from whoever already holds the task renews the claim; one from someone else doesn't count and stays in the task's history, not as an alert.
> - A `done` without valid evidence (at least one element; a command needs `exit_code` 0) is stored, doesn't count, and shows up as an alert.
> - `kontext fact` arrives in phase 3 without fingerprints (`fields.fingerprints = []`), so the scout has somewhere to put what it finds.

> **Implemented: features.** A plan made with a large model has features, phases and epics, but kontext had only epics and tasks: a feature the human asked for that never became a task was invisible, because an epic closes when its tasks close. A **feature** is the level in between: what the human would see working ("Refunds"), with that in `fields.acceptance`. Its prefix is `U` (`F` is the fact's).
> - `fields.epic` is the epic's id (as in a task, so `epic_of`, `touches_epic` and the coordinator's "what's new" work unchanged), `fields.goals` like a task's (without `--goal`, the epic's), `refs` = the epic. A task stores `fields.feature`; `kontext task add --feature <U>` takes the epic from the feature (`--epic` is then optional, and one that names another epic is refused with exit 2), and its goals, without `--goal`, from the feature. A task counts toward its feature only if the feature is in the task's own epic and counts: one whose `fields.feature` names another epic's feature (only another machine writes that; the CLI refuses it) does not count toward that feature, does not undo its validation and does not hold its epic, raises `TaskRecordIgnored` (with the feature in `task`), and in its own epic is a task without a feature; so is a task whose feature is missing or not counted. A question with `--feature` stores `fields.feature` and its epic (the name, as for a task), `refs` = the feature, and goes to the epic's coordinator without `--to`; a `confirm` that joins it keeps the feature.
> - **Who counts.** Only the pm (`actor.role = "pm"`, without `approved_by`) or an owner adds a feature or supersedes it with another; the CLI refuses anyone else (exit 2), and one that arrives from another machine is stored with the state `not counted` and the `NotCounting` alert. Validating is `kontext done <U>`: the same record and evidence rules as a task's close, written by the epic's coordinator, the pm or an owner, and only if the feature has at least one task and all of them are closed **at that point of the ledger** (the walk checks it in `hlc` order, with the same helper the CLI uses, so the refusal and the alert give the same reason). A `done` that doesn't meet that is stored, doesn't count, and raises `TaskRecordIgnored` (with the feature in `task`; the inbox calls it "Validation that didn't count"). `--since` is optional for a feature (the coordinator validates what it sees, not an assignment); with it, stale context is checked as for a task, including answers to questions about the feature. `kontext reopen <U> --why` undoes a validation; anyone can write it, as for a task.
> - `kontext supersede <U> <U'>` replaces a feature with another one. Dropping a feature with no replacement is a scope change and is left for later: `supersede` always needs the new entry, so it can't be written. (Now possible: `kontext feature drop`, below.)
> - **Old binaries.** A binary from before features does not know the kind: `kontext verify` stops at the first `feature` line ("unknown entry kind"), so `resume` and the start hook say "CHAIN BROKEN … run `kontext verify` and stop", `sync` merges but warns that what arrived does not pass `verify` and exits with 1, `plan` closes an epic by its tasks alone, `done <U>` is refused ("is feature, not a task") and a validation written by a new binary counts for nothing and shows up in its `standup` as "task closed". The rest reads (an unknown entity is `in force`). That was measured with the binary of 716e888 on a ledger with features. So **every machine of a project upgrades its binary before anyone writes a feature**; an entry's format can't be made readable by older code, and making `feature` look like an existing kind would break the rule that the prefix says the kind.
> - **Newer kinds, from this version on.** Phase 2 adds another kind (`scope`), so this binary already tolerates kinds it does not know. `kontext verify` checks their chain and `hash` as usual, and their `id` only as `<uppercase letters>-<6 hex>` with the hex part equal to the start of `content_digest` (the prefix can't be checked without knowing the kind; a known kind still needs its own prefix). Such an entry is not a problem: `verify` lists it once ("ledger/<writer>.jsonl:<line>: S-… is from a newer kontext version (kind scope): upgrade to read it") and exits with 0, `doctor` shows it once as a note, and `resume`, the start hook and `sync` see a sound chain. The state leaves it out (`State::build` skips any kind without a prefix), so it counts for nothing until the binary is upgraded. Writing one is still refused (`Store::push`: "unknown entry kind"). This does not help binaries of 716e888 and older, which still stop at the first `feature`: the rule above stands for them.

> **Implemented: the signed scope.** A new kind, `scope` (prefix `S`), records the human's signature of the plan or of a change to it. Fields: `type` (`plan` or `change`), `covers` (every id the signature covers: the charter in force, the goals, the epics and features in force, and the decisions in force), `adds` and `removes` (for a change: what entered and left since the last signature), `summary` (the exact text the human read, what `kontext scope summary` printed without its hash line), `summary_hash` (its sha256) and `said` (their words, if given). What a scope covers is the charter (its title, objective, non-goals, done-when and body), each goal (title, done-when, body), each epic's title and each feature's title and acceptance, all in full in the summary; **an epic's or a feature's body is not part of the signed scope** (it is how the work is done, which the pm decides). Only an owner writes it, or the pm with `--approved-by` (the human said yes in the chat); it is in the signed kinds, so `require_signatures` asks for an SSH signature on it too.
> - `kontext scope summary [--json]` is read-only and deterministic: a plan lists the charter, the goals, and per wave each epic with its features and their acceptance, with the counts; a change lists what enters and leaves (with the closed tasks a feature that leaves throws away, and the user's ideas that pointed to it), the epics it touches and their order. Nothing it binds is cut: titles, objective, non-goals, done-when and acceptance come out whole, and a charter's or goal's body line by line. A plan's waves are `kontext plan`'s (`plan::of_state`, the same as the tree), with the finished epics listed under "Done", so closing an epic's last task changes the plan's text and hash; a change compares the **epic order** instead (the order the dependencies give, every epic counted as open, before and now), which closing work does not move. Every summary ends its text with a line about the user's ideas: "Not placed: N of your ideas" (and the list) when `.pm/items.md` was found from where it ran, as `kontext ideas` finds it, or "Ideas not checked: no .pm/items.md found from <dir>"; that line is part of the hashed text, so a summary shown from the repo and a signature run from another folder do not match and the signature is refused. It ends with `Hash: <sha256>`; with nothing to sign it says so and exits 0.
> - `kontext scope sign --hash <h> [--said "…"] [--approved-by <owner>]` recomputes the summary under the lock and refuses (exit 2) if the hash, or a prefix of at least 8 hex characters, does not match ("the scope changed since that summary; show it again"), and refuses a plan while any idea is not placed, listing them. What it stores is the summary it recomputed, which is the one that was shown. `kontext verify` checks every `summary_hash` against its `summary`.
> - **Dropping a feature** is now possible without a replacement: `kontext feature drop <U> --why` writes a `supersede` with `refs = [U]`, no second ref and `fields.drop = true` (an old binary reads it as superseded). A dropped feature no longer holds its epic's closure; its open tasks raise an alert. `kontext reopen <U> --why` puts it back (a `reopen` with `fields.restore = true`), keeping its validation. A dropped feature is **signed out** only by a signature whose `removes` lists it, that is, whose summary showed it leaving. An idea whose only home is such a feature waits ("dropped, waiting for your signature") and counts as discarded, citing that `S` id, once the owner signs. A feature added and dropped between two signatures never entered the scope: no summary mentions it, no signature signs it out, and an idea placed only on it is Not placed ("dropped (it was never in a signed scope)"), in `kontext ideas` and the tree.
> - **Old binaries** (e2cd98f, before this): `verify` lists the `scope` entries as from a newer version and exits 0; `resume`, `plan`, `tree` and `inbox` render. They read a dropped feature as superseded, and one put back stays superseded for them (the `reopen` that restores it is, to them, a reopening of a superseded feature's validation), so their `tree` and `plan` count one feature less. They show the pm's decisions, accepts and rejects made alone as not counted (and those proposals as still pending), a charter or goal signed only by the plan as unsigned, and the pm's own dismissals as not closing anything (also the ones this binary does not count either: a reject or dismiss of the owner's words without `approved_by`, a dismiss of an owner-only item).

> **Implemented in phase 4:**
> - `kontext fact --path <path>…` resolves each path against the git repo of the current folder (or `--repo`) and stores `{repo, path, commit, blob}` with `commit` = `HEAD` and `blob` = `git rev-parse HEAD:<path>` (a folder stores its tree). A path that isn't in the last commit is rejected; one with uncommitted changes is accepted with a warning. The repo name is the one from `[[repos]]` with the same remote, or with the same folder name, or the folder's name (with a warning).
> - Expiry is compared against the `HEAD` of the local checkout (not against `commit`): `git rev-parse HEAD:<path>` being different or missing means "expired", with the short `HEAD` where it was seen; if the repo is not on this machine, "unreviewed". It is calculated after the walk (`facts::annotate`, which adds `State::freshness` and the `FactStale` alert), because it needs git; the pure state doesn't change.
> - Writing a fact with the same title as one that expired writes the new fact and a `supersede` of the old one: that way the alert resolves itself when a scout reads it again.

## 5. Derived state

`kontext` reads the entries from all the files, sorts them by `hlc` (and by `id` if they tie), and walks them applying the rules in the table above. The result:

- **Law in force:** the signed charter, the goals in force and the accepted decisions that haven't been replaced.
- **Open:** unanswered questions (by recipient), unreviewed proposals, tasks by state (ready, claimed, closed, blocked by dependencies).
- **Alerts:**
  - **Drift:** a task without `goals`, or one that points to a replaced goal.
  - **Stale context:** see 7.3.
  - **Expired fact.**
  - **Failing guard:** a decision's test doesn't pass.
  - **Conflict:** two accepted decisions that replace the same thing, two `sign`s of different charters, or a proposal that contradicts a decision. Decision conflicts are never resolved automatically: they go to standup.

> **Implemented in phase 3:**
> - Drift also means citing a goal that doesn't exist, and an epic that cites a replaced goal. Citing an unsigned goal is a softer alert, "unsigned goal". Closed tasks don't generate drift. These alerts, like conflicts, show up in `standup` until they are fixed.
> - Stale context is not an alert that sticks around: it is checked at the moment of `done` (7.3).
> - Two more alerts: a claim that expired without anyone taking it back, and an `assign`, `release` or `done` that doesn't count.
> - The state of each task (ready, blocked, assigned, claimed, closed) is calculated at the end of the walk, with all dependencies seen. A dependency that doesn't exist blocks.

> **Implemented: the state of a feature.** It is derived at the end of the walk (`State::features`, `src/features.rs`), never written: **pending** (no tasks, or none assigned, claimed, closed or reopened, and never validated); **in progress** (some task assigned, claimed, closed or reopened, not all closed, or a validation that no longer holds); **to validate** (at least one task, all closed, no validation that counts); **done** (a validation that counts and holds); **superseded**; **not counted**. A validation stops holding, with no record, when one of the feature's tasks is reopened or a new task is added to it after the validation: the feature goes back to in progress, never pending (or to validate, once those tasks close) with a note that says which task and which validation, and the next validation clears it. That way "done" always means a human-visible check after the last change, and nobody has to remember to undo it. An epic with no features has no entry in `State::features`, and its state and views are what they were (a script compares every view of a project without features against the binary of 716e888).

> **Implemented in phase 2:** conflicts are calculated at the end of the walk. There is a conflict if an entry has more than one replacement in force (two decisions or goals that replace the same thing), or if there is more than one signed charter that isn't replaced (the last signed one counts). They are resolved by writing an entry that replaces all the ones involved. "A proposal that contradicts a decision" is left for when there is a way to detect it without a model. The state (`State`) has a map by type and an `enum Alert` so that phases 3 and 4 can add tasks, drift and expiry.

> **Implemented in phase 5:**
> - "A proposal that contradicts a decision", in its deterministic version: a pending proposal whose `fields.supersedes` cites a decision in force raises an alert ("P-x proposes replacing D-y, which is in force") in `standup` and `resume` until it is accepted or rejected. It doesn't count as a conflict: it is an open question, not two laws at once.
> - With `require_signatures`, an entry that is signed and doesn't carry a signature that verifies gets the alert "invalid or missing signature" (§14); it shows up when new, like the other entries that don't count.
> - A claim that lost to another machine's claim is an alert (§7.2).

> **Implemented after the first import of a large v0 ledger:** a proposal and the decision made from it count as one replacement in the conflict check. The decision was made from the proposal if the proposal's `accept` names it (`fields.decision_id`) or the decision says so (`fields.from_proposal`); `kontext accept --as-decision` writes both. v0 copied a proposal's `supersedes` onto its decision, so the old proposal was replaced by both and showed a conflict with nothing to decide; the rule is in the state, so it holds for what was already imported without rewriting the ledger. When a proposal and the later decision both name an older decision, there was no alert already: a pending proposal never replaces a decision, and after an `accept` with `decision_id` only the decision does.

> **Implemented: what a signed plan changes.** Derived at the end of the walk (`src/scope.rs`); none of it applies until the first `scope` entry, so a project without one reads exactly as before (a script runs 78 commands, views and writes in text and JSON, as the owner, the pm, a coordinator and an implementer, against the binary of e2cd98f, with no difference).
> - **The signed scope** is the `covers` of the last `scope` entry; **the current scope** is the charter in force (the latest one), the goals, and the epics and features in force; **the pending change** is the difference in either direction, which `scope summary` describes and `scope sign` closes. Decisions are covered but are not scope: a new decision is not a change.
> - A charter or goal the signature covers counts as signed (its `signed_by` is the `S` id). The decisions it covers are those already in force, so they stay in force with the owner's signature behind them; a decision that did not count before the plan does not start counting. The "unsigned goal" alert is not raised with a signed plan.
> - **The pm decides alone inside it:** with a signed plan and without `require_signatures` (`scope::pm_acts_alone`), a `decision`, `accept` or `reject` from the pm (`agent` with role `pm`) counts without `approved_by`. Under `require_signatures` nothing changes: the owner signs, and the inbox does not move (§20). Old binaries show those decisions as not counted.
> - **Never over the owner's own words:** an entry that carries them (`origin = user`: what the user said, their rules) may be accepted or confirmed by the pm alone, but a pm `reject` or `dismiss` of it counts only with `approved_by` from an owner, signed plan or not. The CLI refuses it (exit 2, "carries the owner's own words"); one that arrives written is stored, does not count, and shows to the owner as a problem.
> - **New work waits:** a task whose feature or epic entered after the last signature, or is leaving and not yet signed out, is not handed out (`scope::blocked`): `next` skips it and says why, `next --claim` and `assign` refuse it (exit 2), and with nothing else ready `next` refuses listing them ("waiting for your signature: scope change"). Holding comes from the task's feature (the one it counts toward) and its epic. A goal holds a task only when it entered after the signature and the task cites it on its own (`--goal`; `task add` copies the feature's or epic's goals, so a goal absent from both is the task's own); a goal leaving holds no task, and still shows as a pending change. A task added later under an already signed feature is not held. A task already held when its feature leaves keeps going (the least change). Signing the change releases the tasks that entered. The open tasks of a feature out of the scope, dropped or replaced, stay off the board with the same refusal ("U-… Refunds was replaced by U-… (the owner signed it out in S-…); close the task or move its work to a feature in force", or "it was never in a signed scope" for one that left before any signature covered it) and keep their alert until they are closed or moved. A goal added after the plan warns that it enters with the next scope signature, not with `kontext sign`.
> - **The inbox** splits while the pm acts alone (§20), and a pm `dismiss`/`confirm` counts only for what the pm's inbox holds (`inbox::pm_may_close`, the same rule for the CLI and the state): anything else is stored, does not count and shows to the owner. The pending scope signature cannot be dismissed or confirmed from the inbox: it closes when the owner signs it, or when the pm undoes the change.

## 6. SQLite index

- `rusqlite` with SQLite included in the binary, WAL mode, `busy_timeout`.
- Tables: `entries` (columns for filtering plus the full JSON), `refs`, `files` (up to which position each ledger file has been ingested), `entries_fts` and `docs_fts` (FTS5).
- It catches up only when reading: it compares the size of each ledger file with what has been ingested and reads what is new in one transaction.
- It is not the truth. `kontext reindex` deletes it and rebuilds it.

> **Implemented in phase 4:** the schema moves to version 2 (an old database rebuilds itself). New tables: `fact_blobs` (the blob of each `(repo, path, HEAD)`, `NULL` if it doesn't exist; with the same `HEAD` the answer doesn't change, so it is never invalidated), `docs_files` (per file: date in ns, size, status and the ADRs it replaces), `docs_sections`, `docs_repos` (which rules each repo was read with) and `docs_fts`, which stores the already-normalized terms of each section and is used to find candidates and their frequency without reading the whole corpus. A section's id is `file_id << 20 | position`. `kontext reindex` also deletes the docs and the fingerprints.
- The derived state is calculated in memory. For thousands of entries that is enough; if one day it isn't, it will be cached by file positions.

> **Implemented in phase 5:** schema 3, with the table `sig_checks` (`key`, `ok`, `reason`): the result of verifying each signature. The key is the sha256 of the `id`, the `hash`, the signature, the person and the `allowed_signers` built from `[people]`, so it is never invalidated (if the keys change, the cache key changes). `kontext reindex` also deletes it.

## 7. Concurrency

### 7.1 Many writes at once on the same machine

An exclusive lock (`flock`) is taken on `.kontext/locks/<writer>.lock`. Under the lock, the last line of the file is read, the new one is chained, it is written with `O_APPEND`, and `fsync` is called. It takes microseconds, so 30 agents at once don't wait noticeably.

> **Implemented in phase 1:** under the lock, the index is also caught up, to check for `id` collisions and so that `append_if` sees the current state. `fsync` is what takes the most time: about 0.8 ms per entry on `t3-machine`, and 32 processes with 50 entries each finish in 1.3 s. If the writer's file ends with an incomplete line (a write that was cut off), `kontext` refuses to write on top of it and asks you to check with `kontext verify`; it doesn't trim it on its own.

### 7.2 Resources that only one can hold

This only applies to claiming a task.

1. **By design:** the coordinator assigns (`assign`) and each implementer can only close the task assigned to it. One owner per resource.
2. **On the same machine** (`kontext next --claim`): the "still free" check and the write of the `claim` happen under the same lock. Writing only if the condition holds is atomic.
3. **Between machines:** if two `claim`s arrive when syncing, the one with the lower `hlc` wins. Both machines reach the same result on their own, and the loser sees it on its next read. A `claim` expires after 2 h without a `journal` or `done` from its owner (configurable).

> **Implemented in phase 3:**
> - `kontext next --claim` picks the task and writes the `claim` under the same lock: with 16 agents and 5 ready tasks, exactly 5 claims and 11 rejections come out (exit code 2). Whoever already holds a task from the epic gets that one, without another claim.
> - Expiry is applied at two moments: when walking, a `claim` from someone else wins if it arrives when the previous claim had already gone more than `lease` without activity from its owner (`journal` with `fields.task` or `done`); when reading, if nobody took it back and it has expired against the current time, the task goes back to ready and the alert is raised. An `assign` doesn't expire.
> - `KONTEXT_NOW` (for tests) sets the time that `kontext` uses: the one in the `hlc` of what is written, and the one that decides whether a claim has expired.

> **Implemented in phase 5:** a `claim` that loses to an earlier one from **another writer** raises the `ClaimLost` alert ("T-x was claimed first by agent:impl-1 (on omar@mac); agent:impl-2: stop working on it and get another with `kontext next`"). Between `claim`s from the same writer there is no alert: on one machine the claim is atomic and the loser already knew it when claiming. The alert stays in `standup` and in the epic's `brief` while the task is still open and the loser doesn't hold it; `kontext next` for the loser shows it above the assignment, or next to the rejection if no task is left for it.

### 7.3 Stale context on close

`kontext brief` returns an **assignment mark**: the position of each ledger file at that moment, encoded in a short text. `kontext done T-… --since <mark>` checks whether anything touching the task came in after the mark:
- a decision for its epic;
- a replaced goal that the task cites;
- an answer to a question about the task.

If something came in, it rejects the close and shows it. The agent reads it and tries again with a new mark, or leaves a `journal` entry explaining why its work is still valid.

> **Implemented in phase 3:**
> - The mark is given by `kontext next` and `kontext brief`. "A decision for its epic" is a decision that counts and touches the epic (4.3) or cites the task, or the acceptance of a proposal like that. "An answer to a question about the task" is an `answer` to a question with `fields.task` equal to the task.
> - The rejection exits with 3, lists each entry with its `id`, and gives the mark for now so you can retry. Leaving a `journal` entry is not enough by itself: you then have to retry with the new mark.
> - `--since` is mandatory except for a human owner. Who can close and the evidence are checked under the lock, together with the stale context.
> - `kontext done --run "<cmd>"` runs the command with `sh -c` before writing (after checking owner and stale context, so as not to run it for nothing), saves the exit code and the last 20 lines of output as evidence, and only closes if it exits with 0.

### 7.4 Duplicates

- If the content is identical, the `id` is the same and only one entry remains.
- If two facts have the same normalized title or the same fingerprints, the second one is stored as a `confirm` of the first.

> **Implemented in phase 4:** "normalized title" means lowercase, no accents (NFKD), no punctuation, and spaces collapsed; "the same fingerprints" means the same set of `(repo, path, blob)`, and it doesn't count if there are no fingerprints. It is compared against active facts; if the matching one has expired, instead of a `confirm` the new fact is written together with a `supersede` of the old one (4.3). The `confirm` carries `refs[0]` = the fact, the text as the title, its own fingerprints, and `fields.reason` (`same title` or `same fingerprints`). The check runs under the writer's lock; expiry is calculated beforehand, so git doesn't run under the lock.

### 7.5 Cursors

- A cursor is a vector: the position read in each file, for example `{"omar@mac": 412, "ana@laptop": 30}`. "What's new" is exact even when entries arrive late through sync.
- Each cursor is written only by its own reader, in its own file per machine, and it is saved by replacing the file in one go. The effective cursor is the maximum per position across that reader's files.

> **Implemented in phase 2:** a cursor mark in text is `omar@mac:412,ana@laptop:30` (`-` if empty); writer names don't contain `:` or `,`. The cursor file is `{"<writer>": n}`, and when advancing, the maximum with what was already there is saved, so it never goes backwards. The date of "your last visit" in `standup` comes from the modification date of the cursor files. The reader for `standup` is your name (`--as`, the human from `--actor`/`KONTEXT_ACTOR`, or `git config user.name`); for `resume --as <role>` it is the role.

> **Implemented in phase 5:** cursors are uploaded with `kontext sync` like the ledger (each reader on each machine writes only its own file, so they don't conflict). Advancing a cursor now takes the lock of the machine's writer, so `sync` doesn't upload one half-written. What didn't change: "your last visit" comes from the modification date of the files, and when git brings in those from another machine it sets the time of the `sync`; the cursor (what is new) is exact.

## 8. Views

| View | For | Content |
|---|---|---|
| `kontext standup` | You | From your cursor: decisions, closed tasks with their evidence, what is waiting for your answer (questions, proposals, signatures), drift, conflicts and expired facts. At the end it advances your cursor (`--peek` doesn't) |
| `kontext resume --as pm` | Project manager | The charter and the decisions in force, what was escalated to the PM, one line per epic, drift and conflicts. Below, what's new since its cursor |
| `kontext brief epic:<x>` | Coordinator | The charter, the decisions of its epic, the ready tasks, the facts in force for the epic, and the assignment mark |
| `kontext next epic:<x>` | Implementer or tester | One ready task, plus the facts and decisions that touch it and the assignment mark |
| `kontext show <id>` | Anyone | A full entry with its history |
| `kontext tree` | You, the pm | The whole project on one screen: waves, epics, their features, and the ideas not placed |

Each view has a character budget. When it doesn't fit, it trims in this order: old journal, decision bodies (the titles stay), facts, and then the other lists. **The charter and the goals are never trimmed.** The titles of the decisions in force are trimmed only last, when the rest does not leave room for all of them (see "Implemented after the first large ledger" below).

> **Implemented in phase 2:**
> - `standup`: new decisions, what closed (signatures, acceptances, rejections, answers, replacements), what is waiting for your answer (unsigned charters and goals, questions for `owner` or for you, pending proposals; the ones from `agent:distill` are marked `[unreviewed]`), alerts, new journal entries, and a one-line summary. What is waiting for your answer always shows, new or not; conflicts too, until they are resolved; entries that don't count, only when they are new.
> - `resume --as owner|pm|<role>`: header (project, time, chain according to `verify`), charter, goals, decisions in force by their first tag, questions for the role, pending proposals, facts (the 15 newest), journal (the last 10), alerts, and what's new since the cursor. The budget comes from `[budgets]` (9,000 if absent). Trimming: the journal one entry at a time, then decisions and proposals without the "why", then without facts, then the lists cut to 5 with "… N more". If it still doesn't fit, a warning is shown at the end. The per-epic line arrives with phase 3.

> **Implemented in phase 3:**
> - `brief <epic>`: charter, the epic's goals, the epic's decisions (with their why) and the titles of the others in force, the board by state (closed ones with their evidence, blocked ones with what they wait for), questions for `epic:<name>`, the epic's facts, its journal, its alerts, what's new since the cursor (only the epic's, and what changes the law of the project) and the assignment mark. It advances the cursor `epic:<name>` like `resume` (`--peek` doesn't). Trimming: journal, the why of decisions and criteria, facts and then the lists; open tasks and the mark are never trimmed. Budget: `[budgets] coordinator`.
> - `next --epic <epic>`: the task, its acceptance, goals and dependencies, the objective and what is not being done from the charter, the decisions and facts that touch it, the task's questions with their answers, the mark and how to close it. It has no cursor or budget: it is short by design.
> - `resume` has one line per epic for all roles. With `--as pm`, facts and the tasks' journal are also left out, and in "what's new" the entries from the epics' work (tasks, their records, their journal, facts and questions for a coordinator) are summarized in one line. Alerts do show: they only cite ids.
> - `standup` puts the closed tasks (with their evidence) and the reopened ones under "Closed".
> - `show <T>` shows the task's state history (each record, whether it counted and why) and its journal; `show <E>` lists its tasks.

> **Implemented: features in the views.** Only for an epic with at least one feature in force; the others read as before, byte for byte, and their JSON has no new keys.
> - `brief`: the board is grouped by feature, each with its state (`[to validate (3/3 tasks closed)]`), what the human sees when it works, and its tasks under it with their status (`- [ready] T-…`; open ones first, closed ones cut like the rest of the board); a feature to validate says how to validate it; the tasks with no feature (or whose feature is no longer in force) go in a last "No feature" group. The header gains "2/3 features done, 1 to validate." `--json` adds `features` (each with `feature_status`, its tasks and the validation).
> - `next`: the task's feature, its state and what the human sees ("- Feature: U-… Refunds (in progress). When it works, the human sees: …"); `--json` adds `feature`.
> - The per-epic line of `resume` (and the page) starts with the features: "Payments: 2/3 features done, 1 to validate; 3/5 closed, 1 claimed; last activity 2 h ago". A validation (or its reopening) is not task-level work: the pm sees it under "What's new" instead of in the one-line summary of the epics' work.
> - `show <U>`: its state, its tasks with their status, and its validation history (each validation and reopening, whether it counted and why). `show <E>` lists its features with their state; `--json` of a feature adds `feature`. `standup` says "feature validated by …".

> **Implemented: `kontext tree [--json]`.** With features the project has three levels (wave, epic, feature), and "how are we doing?" took `plan`, `resume --as pm` and `ideas` to answer. `kontext tree` draws it in one screen, read-only (it writes nothing and advances no cursor):
> ```
> quoro                            3/9 features · 2 waves to go
> ├─ Done
> │  └─ ✓ Foundations              4/4 tasks
> ├─ Wave 1
> │  ├─ ◐ Payments                 2/3
> │  │   ├─ ✓ Card charge
> │  │   ├─ ◐ Refunds              waiting for you
> │  │   └─ ○ Invoices
> │  └─ ◐ Catalog                  1/2
> ├─ Wave 2
> │  └─ ○ Integration              0/2
> └─ Not placed (2) · 12 of your messages not audited since Nov 12
>     ├─ "que el recibo salga en PDF"  Nov 12
>     └─ "límite de pedidos abiertos"  Nov 12  (Payments, no feature)
> ```
> - **Waves** are exactly `kontext plan`'s (`plan::of_state`): the open epics with the same numbers, and the same "No wave" group with the same reasons (a cycle, a dependency that does not exist or is no longer in force, waiting on one of those). The closed epics go first, one line each, in a "Done" group. A first version laid the closed epics out as if open so a finished wave kept its number; that put ready epics under "No wave" when a closed epic had a dangling `--after`, and drew epics a wave later than `plan` (an epic after a closed one is ready now), so it was dropped.
> - **Epics.** ✓ closed (by `plan`'s rule: its features, or else its tasks), ◐ something started (a feature not pending, or a task assigned, claimed or closed), ○ nothing yet. A closed epic takes one line; an open one lists its features in force. The count is features done/total, or "n/m tasks" for an epic without features. Tasks are never listed.
> - **Features.** ✓ done, ◐ in progress or to validate, ○ pending; then "to validate", and "waiting for you" when an open question for a person (as the inbox decides it: `owner`, an owner, `human:<x>` or someone in `[people]`, not dismissed) cites the feature or one of its tasks (`--feature`, `--task` or its refs).
> - **Header.** "n/m features" across the project, or "n/m epics closed" when there are no features; then "N waves to go" (the plan's waves), "N with no wave" if any, or "all done" when every epic is closed.
> - **Not placed** comes from `kontext ideas` (§12.3): each idea not placed with the user's words (the quote, or else the idea), the date said and what it cites; the header adds the messages not audited. Without `.pm/items.md` the section is absent.
> - `--json` gives the same tree (`done[]`, `waves[].epics[].feature_list[]`, `no_wave`, `not_placed: null | {count, ideas, unaudited}`), which `kontext ui` draws.

> **Implemented: the signed scope in the views.** Only with a signed plan; otherwise every view is byte for byte as before, and the JSON has no new keys.
> - `plan`, `brief`, `resume` and `standup` add one line while a change waits: "Scope change waiting for the owner's signature: enters U-… Invoices; leaves U-… Refunds. Its new work is not handed out until they sign; …" (`pending_scope` in their JSON). While the pm acts alone (no `require_signatures`), `resume --as pm` also says what the pm may do alone and how many items its own inbox has.
> - `next` lists the epic's ready tasks it skipped ("Skipped T-…: waiting for your signature: scope change (U-… Invoices is not signed yet).", `skipped` in JSON).
> - `tree`: the header ends with "plan signed Nov 12", or "1 scope change waiting for you" with a second line "enters Invoices · leaves Refunds"; a feature or epic that entered after the signature says "not signed", and a dropped feature stays listed, uncounted, as "dropping, waiting for you" until the owner signs. `--json` adds `scope: {signed, signed_on, pending?: {enters, leaves}}` and `scope: "not_signed" | "dropping"` on features and epics; `kontext ui` draws the same, red for what waits and a red strike for what is leaving.

> **Implemented in phase 4:**
> - `brief`, `next` and `resume` only list facts in force; the "unreviewed" ones are marked `[unreviewed]`. A fact that expired shows up as an alert in `standup` (until it is replaced) and in the `brief` of its epic, or of all of them if it has no epic. The pm doesn't see facts or those alerts. `show` of a fact lists its fingerprints and its status; `--json` adds `freshness`.
> - `next` has a budget (`--max-chars`, 9,000 by default): first the why of the decisions, then the facts, then the lists (the task's decisions and questions, to 5). The task, its acceptance, the mark and how to close it are never trimmed.

> **Implemented for long memory:**
> - `resume` (all roles, the pm too) has the section **"What the user said"**: facts in force and pending proposals with `fields.origin = "user"` (the distill notes them), each with its id, the date it was said (`fields.said_at`) and, if it updates another, "(supersedes <id>)". It goes right after the decisions, with a line that says how to treat them: in force while nothing replaces them; a decision in force weighs more; the `[proposal]`s become decisions when an owner says so. Those proposals are no longer repeated under "Pending proposals", and nothing in that section is repeated under "What's new". In JSON, `user_said`.
> - A user proposal that is a rule or an instruction (the distill marks it with `fields.user_rule`; decisions that need a signature, like architecture or scope, are not) comes out as "[user rule, in force]", with the instruction to apply it and, if it contradicts a signed decision in force, that the decision prevails and it is raised with `kontext ask`. An agent's proposals don't change.
> - **What the user said applies even if it is unsigned** (after the long-memory benchmark: with two "unsigned" rules, the second replacing the first, opus answered that there was no rate in force). The `resume` header, the "What the user said" section, the hook pointer and the skill all say so: signing is for the charter, not for believing the user. `show` of a pending user rule says "pending signature; it is a user rule and already applies"; `recall`, "user rule, in force (unsigned)"; `standup`, "user rule (already applies; still needs signing for the charter)"; in JSON, `in_force`. The human's inbox (`kontext inbox`) doesn't change: it still asks for the signature.
> - What was replaced is marked as no longer applying, with the new value first: the new line says "(supersedes <id> «old title», which no longer applies)"; under "What's new", `show` and `recall`, the replaced entry says "superseded by <id>, no longer applies".
> - The pm also sees facts: the ones that are not from an epic (an epic's facts are seen by its coordinator), without the user's, the 15 newest.
> - Trimming by priority: charter, goals and decisions → what the user said → open items → what's new → journal. In order: the journal one entry at a time; from "What's new", the replaced entries and those that don't count, then to 10 and then only the count; facts to 5 and then 0; the why of decisions; the open lists to 5; and finally the oldest of what the user said (at 15 and then 5, with a line "… N older ones").
> - The hook's budget stays at 9,000 (the hard cap is still 9,900): Claude Code replaces the text with a path if it goes over 10,000, so there isn't much to gain by raising it; what changes is what gets in first.

> **Implemented after the first large ledger:** a project imported from v0 had 304 decisions in force. Their titles alone took about 45,000 characters, so `resume` ended far over its budget after every trim, and the hook cut the text at 9,900: the session started with the first few dozen titles and without the questions for the pm, the pending proposals, the alerts or what's new, which fit in under 3,000 characters together. Now:
> - When the trims above are not enough, the list of decisions in force is what gets cut last: every other section keeps what the trims left it, and the decisions get what is left of the budget. Output that fits without this cut does not change.
> - The newest decisions in force stay (in ledger order), grouped by first tag as before. A line at the top of the section says how many older ones are not shown, how many of each first tag (the 10 tags with most, if it fits), that the list is partial, and to search with `kontext recall <words>` before proposing or deciding something (`kontext show <id>` reads one). Newest first because a long ledger's recent decisions are the ones the current work touches and the most likely to have replaced an older rule. This is a heuristic: nobody has measured whether agents with a partial list find and respect the rest.
> - `brief` does the same with "Other decisions in force" first and then the epic's own decisions, so a long list of decisions no longer makes the hook cut the assignment mark at the end.
> - If not even the other sections fit, the shorter of the two goes: all the titles, or none with the line that says how many there are; the closing note says which. The hook's hard cap (9,900) stays as a safety net for what the views never cut (a huge charter, for example).
> - `resume --json` and `brief --json` are not cut.
> - `recall` says what replaced an entry ("superseded by F-…"); `show` keeps the full chain, and for an entry that already replaced another it says "Replaces" (not "Would replace"), also if it is a fact or a pending proposal.

## 9. How context reaches the agent

### 9.1 Modes by role

In `project.toml`:

```toml
[context]
pm = "inject"          # inject | pointer | off
coordinator = "inject"
default = "inject"
```

A role's mode is its own, then `default`, and if there is none, `inject`.

| Mode | What the start hook does |
|---|---|
| `inject` | Puts the role's full view, without going over 9,000 characters (Claude Code cuts at 10,000) |
| `pointer` | Puts three lines ("there are 4 updates and 1 question for you; run `kontext resume`"). The skill explains what to look up |
| `off` | Nothing. Only the skill remains |

> **Implemented after incremental injection:** `inject` is the default mode for all roles (before, `default = "pointer"` in the template and `pointer` if there was nothing). In long-memory (`bench/reports/2026-11-04-long-memory.md`, 5 seeds), `inject` remembered more than `pointer` with the small models (haiku 98 % against 89 %, and in class C 93 against 53; sonnet 100 against 97) and almost the same with opus (95 ± 7 against 99 ± 2). What counted against it was the cost, and with incremental injection (§9.4) that was reversed (`2026-11-04-mini-incremental-injection.md`): against having no memory, `inject` costs +3 % with haiku and +5 % with opus, and `pointer`, +12 % with haiku. A value in `[context]` that isn't any mode still falls back to `pointer`: it's a typo, and short is what gets in the way least. Existing projects don't change: their `project.toml` says what it says.

### 9.2 How the hook knows which role it is

In this order: the `KONTEXT_ROLE` variable, then `default_role` in the repo's `.kontext.toml`, and if there is none, `pm`.

> **Implemented with worktrees (§9.7):** in a linked git worktree without a `.kontext.toml` of its own (an untracked one at its root), after `KONTEXT_ROLE` come `worktree_role` and then `default_role` of the repo's file (the main checkout's, or a tracked one in the worktree), then `pm`; the main checkout gets `default_role`. Nothing changes for whoever does not set `worktree_role`.

Implementers, testers and scouts don't use the hook: their context goes in the assignment the coordinator writes to them, which includes the output of `kontext next` or `kontext brief`.

### 9.3 What `kontext install` installs

In the code repo:
- `.kontext.toml` with `project = "<name>"` and `default_role`.
- Hooks in `.claude/settings.json`: `SessionStart` → `kontext hook session-start`; `PreCompact` and `SessionEnd` → `kontext hook distill`. They are merged without touching what is already there, and the diff is shown before writing (as `install-hooks` does today).
- The skill `kontext` (`.claude/skills/kontext/SKILL.md`): when to read, when to write, how to escalate and how to close with evidence.
- The agent files: `.claude/agents/kontext-coordinator.md`, `kontext-implementer.md`, `kontext-tester.md` and `kontext-scout.md` (section 12).

A hook never blocks the session: on any error it exits with 0 and logs it in `.kontext/hooks.log`.

> **Implemented in phase 2:**
> - The start hook doesn't advance any cursor: what it puts in the context may not be read carefully, so what's new stays new until someone runs `resume` or `standup` on purpose.
> - In `inject` the text is cut to 9,900 characters if it goes over. In `pointer` it is three lines at most (since long-memory, the third one says that what the user said applies without a signature and how to invoke `kontext`). Without a project, the hook writes nothing, not even to stderr. The role and the project are looked up from the `cwd` that Claude Code sends.
> - `kontext install` installs only the `SessionStart` hook (`startup|resume|clear|compact`, `timeout` 15) with the absolute path of the binary; if there is already a `kontext` one with another path, its path is changed. `PreCompact` and `SessionEnd` arrive with the distill (phase 4) and the agent files with phase 3; `kontext hook <other event>` already exists and only logs it.
> - `settings.json` keeps the order of its keys; if it changes, it is rewritten with 2-space indentation. The repo is added to `[[repos]]` at the end of `project.toml`, without touching the rest of the file. The skill comes from `templates/skill-kontext.md`.

> **Implemented in phase 3:** the agent files come from `templates/agents/`. If one already exists with different content, it is kept and a warning is shown (it isn't possible to tell whether you changed it or it's from another version); `kontext install --force` replaces it. The skill is always replaced, as in phase 2.

> **Implemented with the `kontext` name (§18.1):** `kontext install` also adds the `Bash(kontext:*)` permission to `permissions.allow` in `.claude/settings.json` (if it isn't there; it doesn't touch the others), so that agents run `kontext` without asking for approval.

> **Implemented for orchestration:** `kontext install` also installs the skill `kontext-orchestrate` (`.claude/skills/kontext-orchestrate/SKILL.md`, from `templates/skill-kontext-orchestrate.md`), in the same place and in the same way as the `kontext` one: it is always replaced. See §12.1.

> **Implemented in phase 4:**
> - The distill hooks are `PreCompact` → `kontext hook pre-compact` and `SessionEnd` → `kontext hook session-end` (without `matcher`, `timeout` 15), not `kontext hook distill`: that way the `hooks.log` record says which event it was. `kontext install` adds them only if `[distill] enabled` (or if there is no `[distill]` section), and as with `SessionStart`, if there is already a `kontext` one with another path, its path is changed.
> - Both run `kontext --project <p> distill --transcript <transcript_path>` in their own process group, with empty stdin and the output in `.kontext/distill.log`, and return without waiting for it. With `KONTEXT_DISTILL_DISABLE` or `enabled = false` they only log it.
> - The start hook of a coordinator with `KONTEXT_EPIC=<epic>` puts that epic's `brief` (without advancing its cursor) instead of the `resume`, if its mode isn't `off`.

> **Implemented with the capture hook (§12.3):** with `[capture] enabled = true`, `kontext install` also adds `UserPromptSubmit` → `kontext hook user-prompt-submit` and `Stop` → `kontext hook stop`, and `SessionEnd` → `kontext hook session-end` if the distill did not already add it: one entry, which then does both jobs, each only if it is on. As with the distill's hooks, turning the setting off later does not remove them (`install` never removed the distill's either): each run checks the setting and does nothing.

> **Implemented with `kontext doctor`:** the hooks record the absolute path of the binary that ran `install`, so a hook installed from `target/debug/kontext` dies silently when `target/` is cleaned. `kontext install` prints a note when the running binary is in a cargo build directory (`target/debug` or `target/release`), in a dry run too; it does not refuse and the exit code does not change. `kontext doctor` (§10) finds it afterwards, with the hooks that point at a missing binary.

> **Implemented with worktrees (§9.7):** `kontext install --local` writes the hooks and the permission into `.claude/settings.local.json` instead of `.claude/settings.json`, and removes kontext's own entries from `settings.json` (its `kontext hook` commands, the groups and event lists that removing them left empty, and the permission; nothing else, and a file with none of them is not touched or created). kontext's entries live in one of the two files: a later `install` without the flag finds them in `settings.local.json`, keeps them there and says so; if they are in both (a copy by hand), it changes only `settings.local.json` and says that `--local` removes the other copy. `--dry-run` shows every file it would touch. `--worktree-role <role>` sets `worktree_role` in `.kontext.toml` (the line replaced, or added after the top-level keys; the rest of the file as it was), and an existing `.kontext.toml` is still never rewritten otherwise, so the key survives every later `install`. `--link-only` writes only `.kontext.toml` in the current checkout (nothing under `.claude/`, no skills, no agent files, no `[[repos]]`): it is how a linked worktree is marked as the PM's. In a linked worktree, a full `install` is refused when the worktree has no `.kontext.toml` of its own and the main checkout has one (the message names the main checkout and says `--link-only` marks this worktree; `--link-only` and `--skills-only` keep working); `--local` and `--worktree-role` are refused there too (Claude Code reads `settings.local.json` from the main checkout's root, and `worktree_role` is read from the repo's file). Where a full `install` proceeds in a linked worktree, `[[repos]]` names the repo after the main checkout's folder (the origin remote is the same in every worktree); in a worktree of a bare repo, which has no main checkout, `[[repos]]` is left as it is and a note says so. When `--local` creates `settings.local.json`, or `--link-only` creates `.kontext.toml` in a linked worktree, and git neither tracks nor ignores it (`git ls-files`, `git check-ignore -q`), a note gives the line to add to `<common git dir>/info/exclude`, which every worktree shares; nothing is written there.

> **Implemented with agent upgrades:** `kontext install` tells an agent file from an older version apart from one changed by hand. `templates/agent-versions.txt` keeps the sha256 of every version of each file in `templates/agents/` that a release installed (taken from main's history, the current one included); older binaries wrote the template verbatim, so the check is a plain match of the file's bytes. A file that matches one of them is replaced without `--force`, like a skill; one that matches none was changed by hand: it is kept with a warning, and `--force` replaces it, as before. `kontext doctor` says which is which: "from an older kontext version" (`kontext install` updates them) or "changed by hand" (`kontext install --force` replaces them). The list is append-only: a test fails when a template changes until its new hash is added. The skills are still always replaced.

### 9.4 The start hook on a `resume`

Claude Code runs the `SessionStart` hook on every `claude --resume`, not only when the session opens, and what it puts in each time stays in the conversation. In the long-memory benchmark each turn was a separate `claude -p --resume`: a one-day session carried 8 nearly identical copies of the `resume` (about 3 thousand tokens each), and each call read them again from the cache.

> **Implemented after long-memory:**
> - The hook reads `source` and `session_id` from what Claude Code sends. With `startup`, `clear` or `compact` (when compacting, by hand or automatically, the context is lost), or if it doesn't know what it gave that session, it puts the full context, as before.
> - With a `resume` of a session it already gave context to, with the same role, mode and epic, it only puts what entered the ledger since last time: `# kontext: what's new since the last context of this session (N)`, a line saying that the rest stays the same, and one line per entry. What the user said comes out as in "What the user said" (with "supersedes <id> "…", which no longer applies"); the rest, as in "What's new" (signatures, acceptances, answers and replacements say which entry they touch; the replaced ones, "no longer applies"). If the chain is broken, it says so. If nothing came in, it puts nothing. The fixed lines (how to invoke `kontext`, who writes, the pm's inbox line) are not repeated. The pm doesn't see the epics' work (one line with how many entries there are), as in `resume`; the coordinator with `KONTEXT_EPIC` sees only what touches its epic or the project's law, as in `brief`. In `pointer` it's the same: what's new instead of the three lines.
> - If what's new goes over 4,500 characters (`inject`, half of the hook's budget) or 1,500 (`pointer`), it saves nothing anymore and the full context goes in.
> - What was given to each session is saved in `.kontext/sessions/<session_id>.json` (local, not synced, and it writes nothing to the ledger): role, mode, epic and the ledger mark (§7.5) taken before building the text, so what enters while the text is being built comes out again in the next `resume` instead of being lost. If it is missing, can't be read, or the `session_id` isn't usable as a file name, the full context goes in. Those older than 30 days are deleted when another one is saved. `hooks.log` says `context=full|delta|none`.
> - The hook still doesn't advance any reader's cursor.

### 9.5 Invariant: kontext doesn't touch the conversation's prefix

Claude Code sends the entire conversation with every request, and Anthropic bills it by prefix: what is already in the cache is read at 0.1× the input price, and what isn't is written at 1.25×. If something changes one byte in the middle of what was already sent, everything after it stops being valid and gets written again: a 100-thousand-token session that breaks at the start costs about 12.5 times more for that request than if it had read from the cache. So:

**kontext never modifies what is already in the conversation's prefix. It only appends to the end.**

- The only thing kontext puts into the conversation is the output of the `SessionStart` hook on `startup`, `resume` and `compact`, which Claude Code adds where the conversation is at that moment. The full block (`resume`, `brief` or the pointer) goes in once per session and after compacting; on a `resume`, only what's new (§9.4). Nothing it already put is rewritten or repeated.
- Nothing dynamic in `CLAUDE.md`, `AGENTS.md`, the agent files, the skills or the system prompt: `kontext install` writes fixed templates (the skills and agent files change only when `install` is run again with another version of kontext), and the project's state never goes there.
- Nothing injected per message or per tool: no `UserPromptSubmit`, `PreToolUse`, `PostToolUse` or `Stop` hook that puts text into the conversation. Hooks on `UserPromptSubmit` and `Stop` may exist only if they put nothing in (no output, exit 0): the capture hooks (§12.3) write the user's message to a file and print nothing. `PreCompact` and `SessionEnd` only start the distill (and the capture) and don't put anything in the conversation.
- What changes (the "Generated" time, how many pending items the human has) can go in the block, because that block is not sent again differently: it stays fixed in the history.

If a change needs to break this, it is discussed first and measured.

> **Implemented with the capture hook (measured 2026-11-09, Claude Code 2.1.321):** in a scratch project with hooks on `UserPromptSubmit` and `Stop` that print nothing and exit with 0, the second request of the session carried exactly the first request's messages plus the new assistant and user messages: no hook text, no attachment, the prefix unchanged. The invariant stays as it was (kontext never modifies the prefix and injects nothing per message); what changed is that such silent hooks are allowed. The capture hook prints nothing on any path: a test checks empty stdout and stderr and exit code 0 when it is off, for another role, outside a git repo, with garbage on stdin, with an unreadable transcript and when the dump cannot be written; a panic is caught and only noted in `hooks.log`, with the panic hook silenced, as for the start hook.

> **Implemented after long-memory (watching):** the harness saves, compressed, the full Claude Code sessions of each run in `runs/<…>/transcripts/claude/` and measures the cache breaks in each one: requests from the main session (without subagents) that read from the cache more than 2,000 tokens less than the prefix of the previous one (read + created), without a `compact_boundary` between them. `run.json` carries them in `cache: {requests, breaks, tokens_rewritten, idle_breaks, idle_tokens}` (the `idle_*` ones are those that arrive more than 5 minutes after the previous one: the cache expired, nobody rewrote anything), and `aggregate.py --costo` shows them per cell. In the large run (2026-11-04, before §9.4) they were 1.1 to 1.5 % of the requests in `none`, `automem` and `kontext-inject`, 0.3 % in `kontext-pointer` and 2.5 % in `notes` (the agent edits its `CLAUDE.md`). The ones of about 3 thousand tokens come from Claude Code, which rewrites the session's first message; with `kontext-inject` they are about 5 to 7 thousand because the start block goes in that message. Of kontext-pointer, the two most expensive were due to a pause (13 and 18 minutes). None comes from something kontext rewrites.

### 9.6 Keeping the main conversation small

§9.5 keeps the prefix valid; it does not keep it short. Measured on a real long project conversation (the session a human talks to, 8 days, one model): 3,939 requests, each re-reading on average 208 thousand tokens of history, and re-reading that history was the largest part of its cost. By characters, as `kontext measure` counts them, the history was 46 % tool results (mostly shell output and thread reads; images and other non-text blocks are not counted), 28 % tool calls, 11 % reports handed back by delegated agents, 9 % the coordinator's own text and 0.2 % what the human typed.

So the installed texts carry four rules, each where its reader is:
- **No bulk reading in the long conversations.** The PM and the coordinator send reading several files, searching, or commands with long output to a subagent (the scout, or `kontext-pm-reader` for `.pm/`) that answers in a few lines; small, targeted reads are fine (`kontext`, `kontext-pm`, `kontext-orchestrate`, `kontext-coordinator`).
- **Short hand-backs.** A delegated agent puts the detail in kontext (`kontext log --task`, `kontext done … --evidence`, `kontext fact`) and hands back about five lines: what was done, the ids, and what it needs from whoever launched it; the brief asks for that (`kontext-implementer`, `kontext-tester`, `kontext-scout`, and the brief-writing parts of `kontext-orchestrate` and `kontext-coordinator`; the scout's report was 15 lines before).
- **Trim output before it enters:** long command output goes through `head`, `tail` or `grep`, or to a file that is summarized (`kontext`; `kontext` itself still goes without pipes, §12).
- **Compact at the end of a phase.** An agent cannot compact its own conversation; only the human can (`/compact`). When a phase closes and its state is in kontext, the PM says once, in one line, that it is a good moment to compact, since kontext gives the context back afterwards (`kontext`, `kontext-pm`).

These are instructions to agents, not mechanisms: nothing enforces them, and no run has measured what they save. The first measurement will be a real project used with them for a week. `kontext measure` (§10) is the tool for that comparison: the same numbers from the transcripts before and after, each cut to its time window.

### 9.7 Working in git worktrees

The first real project moved onto kontext runs every agent conversation in its own linked git worktree (`git worktree add`), the PM's included, and its code repo is public, so `.kontext.toml` and `.claude/` are not committed: they exist only in the main checkout. kontext assumed that sessions run in the main checkout or that those files are committed. Four rules:

1. **The project through the main checkout.** In a linked worktree (its git dir is not the common one), `.kontext.toml` is looked for walking up only to the worktree's root, so a worktree created under the main checkout does not take the main checkout's file as its own; if there is none, it is the one at the root of the main checkout (the first entry of `git worktree list`; a bare repository has none), and only then the walk goes on above the worktree, as before (a `.kontext.toml` in a folder that holds several repos). A file found inside the worktree that git tracks is the repo's, in every worktree, not the worktree's own. `--project` and `KONTEXT_PROJECT` win, and "the only project registered" stays as the last fallback. Callers know which it was (`FoundLink::kind`: `Own`, `Tracked` or `MainCheckout`); `doctor` says so.
2. **`worktree_role`** in the repo's `.kontext.toml` (§9.2): the main checkout's, or a tracked one. A tracked file gives a linked worktree its `worktree_role` and the main checkout its `default_role`. The recommended value for delegated agents whose role is unknown is `agent`: it is neither `pm` nor `coordinator`, so the start hook puts no line about the human's pending items and the capture writes nothing; it gets the `resume` of a non-PM role (the epics' facts and task journal included), its "Questions for agent" are only those addressed `--to agent` (normally none: agents ask `--to pm` or `--to epic:<x>`), and its cursor, which only `resume --as agent` advances, is shared by every session with that role. Its mode and budget follow `[context]` and `[budgets]` (`agent = "pointer"` makes it three lines).
3. **The PM's worktree** has its own `.kontext.toml` at its root with `default_role = "pm"`, not tracked by git (a tracked one is in every worktree, so it marks none); `kontext install --link-only` writes it. Its own file wins over `worktree_role`. With a tracked `.kontext.toml` the PM's worktree cannot be marked: untrack it to mark one (doctor's role note says so for a lane that comes out `pm`).
4. **One `.pm/`** (`ideas::pm_dir`): a main checkout's own; a linked worktree's own with `[capture] worktrees = true`; any other linked worktree, the main checkout's. The capture, `kontext ideas` and the report use it (§12.3).

**The steps, in this order:** `kontext --project <name> install --link-only` in the PM's worktree first (`--project`: the main checkout has no `.kontext.toml` yet), then `kontext install --local --worktree-role agent` in the main checkout. The other way round, the PM's worktree starts as `agent` until it is marked, and its messages are not captured.

**What Claude Code reads in a worktree** is documented, and kontext follows those pages:

- *Settings*, "Where Claude Code keeps the local file in a git repository": `.claude/settings.local.json` is read at the repo root; "in a worktree, it uses the file at the main checkout's root" (since v2.1.211). Not outside a git repo, when the repo root is the home directory, on Windows, or when the repo root or its `.git`/`.claude` entry is not owned by the user. The shared `.claude/settings.json` is read from the session's primary working directory, so a worktree session gets one only if the worktree has it, that is, if git tracks it.
- *Hooks reference*: "If you define the same handler in more than one settings file, it runs once."
- *Worktrees*, "What worktrees share with the main checkout": a worktree with no `.claude/skills` at its root loads the main checkout's project skills (v2.1.277 and later); one with its own `.claude/skills` loads only that copy. The same read-through covers `.claude/agents` and `.claude/commands`, and applies to `git worktree add` worktrees. A desktop-app worktree session reads settings, hooks, skills and agents from the main checkout's root and runs hooks there, with the worktree's path in the hook input's `cwd`.

So `install --local` in the main checkout is what reaches every worktree session when `.claude/` is not committed, and it is also the right file in a public repo (the hook holds an absolute path of one person's machine); the skills and agent files installed there reach worktrees without their own. `doctor`, in a linked worktree, reads the main checkout's `settings.local.json` and counts its hooks and permission as this session's, counts the main checkout's skills and agent files when the worktree has no `.claude/skills` or `.claude/agents` of its own, and notes hooks that are only in the main checkout's `settings.json` when git does not track it (also from a main checkout that has linked worktrees). While kontext's hooks are in both files, the identical handler runs once, but a copy that differs (another binary path) runs as well: the capture writes a prompt delivered twice once (the same `prompt_id`, and the same question-tool answer, through the per-transcript mark under the shared lock, from either checkout), and the start context would go in twice; `doctor` notes hooks in both files. Where kontext's entries live is decided by the hooks alone: a `settings.local.json` with only the permission does not move them.

## 10. Commands

| Command | Phase | What it does |
|---|---|---|
| `kontext init <name> [--remote <git-url>]` | 1 | Creates a `kontext-home` and registers it in `~/.config/kontext/projects.toml` |
| `kontext clone <git-url>` | 5 | Brings in an existing `kontext-home` |
| `kontext install [--dry-run] [--local] [--link-only] [--worktree-role <role>]` | 2 | See 9.3 and 9.7 |
| `kontext verify` | 1 | Checks the chain of each file and that there are no repeated `id`s with different content. An entry of a kind from a newer version is listed, not a problem (§4.3). It also checks that `writer` matches the file name and that there is no incomplete line at the end. If the last lines are deleted, the chain is still valid: that only shows up against another copy (git) |
| `kontext reindex` | 1 | Rebuilds the index |
| `kontext import --from-v0 <path>` | 1 | Migrates the ledger of the v0 `ctx.py` prototype. Checks the v0 chain first (with `--force` it imports even if it is broken). What exists only in v0 goes to `fields`, together with `v0_id` and `v0_hash`. It can be run again: it only brings in what's new |
| `kontext charter` / `kontext goal add` / `kontext sign <id>` | 2 | Charter, goals and signature |
| `kontext decide` / `propose` / `accept` / `reject` / `supersede` | 2 | Decisions |
| `kontext ask --to <reader>` / `kontext answer <id>` | 3 | Questions and escalation |
| `kontext fact [--path <path>...]` | 4 | Facts with a fingerprint (takes the current commit and blob) |
| `kontext log` | 2 | Journal entry |
| `kontext epic add` / `kontext task add` / `assign` / `next [--claim]` / `done --evidence … --since <mark>` / `reopen` | 3 | Tasks |
| `kontext feature add --epic <epic> [--acceptance]` / `task add --feature <U>` / `done <U>` / `reopen <U>` / `ask --feature <U>` | features | Features between an epic and its tasks (§4.3) |
| `kontext epic add --after <epic>` / `kontext plan` | orchestrate | Dependencies between epics and wave order (§12.1) |
| `kontext tree` | human, pm | The project on one screen: waves, epics, features, ideas not placed (§8) |
| `kontext standup [--peek]` / `resume --as <role>` / `brief <epic>` / `show <id>` | 2 | Views |
| `kontext recall <text>` | 2 | Searches the ledger and Claude Code's transcripts (what it does today) |
| `kontext find <topic>` / `--show <path:line>` / `--check` | 4 | Searches the docs of the project's repos, with ranking, synonyms and status in force or historical |
| `kontext distill --transcript <path>` | 4 | Manual distill |
| `kontext changelog [--since <ref>]` | 4 | Technical changelog from the closed tasks and the merged PRs (`gh`). Always comes out the same |
| `kontext sync` | 5 | `git pull --rebase` and `push` of the `kontext-home` |
| `kontext server deploy [--name] [--account-id] [--subdomain] [--replace]` | sync server | Uploads the kontext server to your Cloudflare account (§14.1) and prints its URL; running it again updates it. It refuses a Worker of the same name that kontext did not deploy, unless `--replace` |
| `kontext token create --role <r> [--epic] [--person] [--ttl] [--writer]` / `list` / `revoke <id>` | sync server | An owner's tokens for the project's server (§14.2): one per machine, person or sandbox, with the variables a sandbox needs |
| `kontext ui` | 6 | Local read-only page: the tree at `/`, the ledger at `/ledger`, the report at `/report` |
| `kontext inbox [count\|sign\|answer\|dismiss\|confirm]` / `kontext inbox --everyone` / `kontext report [--html]` | human | The human's inbox, everyone's inbox and the report (§20) |
| `kontext ideas [--mark-audited]` | human | The user's ideas in `.pm/items.md` and the state of each one, from the ledger; which are not placed and how many messages no audit has seen. `--mark-audited` writes `.pm/audited.json` (§12.3) |
| `kontext capture here [--session <id>]` / `list` / `forget <id> \| --all` | human | The conversations the capture hook writes from, on this machine (§12.3). Works with the capture off |
| `kontext hook <event>` | 2 | Entry point for the hooks |
| `kontext doctor` | install | Checks an installation (binary, `PATH`, config, kontext-home, hooks, capture, skills, hook log) and says what fixes each problem. Writes nothing |
| `kontext measure <transcript>…` | cost | Numbers about Claude Code conversations from their transcripts (§9.6). Needs no project |

All read commands accept `--json` for the agents.

> **Implemented in phase 3:** exit codes 0 success, 1 error, 2 rejected (nothing was written), 3 stale context in `done`. `kontext release <T>` also exists. `kontext ask --task <T>` links the question to the task; without `--to`, it goes to its epic. `--to epic:<x>` requires the epic to exist. `kontext fact` was moved up without fingerprints.

> **Implemented in phase 2:** `kontext ask` and `kontext answer` were moved up to phase 2. The commands that write also accept `--json` (they print the new entry) and `--actor`. `recall` searches the ledger with FTS5 (by word prefix and without accents, not by any fragment as v0 did) and the transcripts as v0 did; `KONTEXT_TRANSCRIPTS_GLOB` changes where it looks.

> **Implemented in phase 4:**
> - `kontext find` ranks with BM25 per field (headings, "Read when", path and body; accents ignored, light plurals, at most two sections per file). `examples/find-repo.toml` is a complete configuration, and a golden-query test runs it against the docs in `tests/fixtures/find-docs`. What depends on each repo is configurable: historical paths (`historical`), top-level folders for the cited paths (`roots`, by default the ones git tracks), exclusions (`exclude`) and synonyms (`<kontext-home>/synonyms.toml`). An ADR is any file that starts with `ADR-NNNN`, in any folder. Docs come from walking the repo with `.gitignore` (the `ignore` crate), not from `git ls-files`; "exists" for the cited paths is what git tracks. With several repos, the location is `<repo>:<path>:<start>-<end>` and ADR replacements are calculated per repo. `--show` also accepts what the search prints (`path:310-346`). `--check` accepts `--baseline <file>` and `--update-baseline` (one `<count> <path>` line per doc).
> - `kontext changelog`: the range goes by the `hlc` of the `done` that counts and by `mergedAt`; `--until` with a single date includes the whole day; a git ref counts as its commit date. PRs are grouped by the type in their title (`feat`, `fix`, `perf`, `refactor`, `docs`, `test`, `build`, `ci`, `chore`, `style`, `revert`, then other types by name and finally "other"). `KONTEXT_GH_BIN` changes the `gh` binary.
> - `kontext done` rejects (exit code 2) `file:` evidence that doesn't exist, relative to the current folder (it accepts `path:line`).

> **Implemented in phase 5:**
> - `kontext sync [--include-config]`: `git fetch`, and under the locks of **all** the writers that have a file on the machine (not only its own: a machine can have more than one), `git add` of `ledger/` and `cursors/`, commit `kontext: <writer> <N> entries`, and `git rebase` onto what was fetched; then `git push -u` without the lock. It is the `pull --rebase` in two steps, so as not to hold the lock while using the network. If the `push` loses the race against another, it is repeated (three times). The first `sync` of a repo with no commits uploads everything (`project.toml`, `.gitignore`). Afterwards it catches up the index and reports what was received per writer, the new alerts (those that weren't there before fetching) and the chain problems, first those in the files that arrived (exits with 1 if there are any).
> - `project.toml` and `synonyms.toml` only upload with `--include-config`; without the option, a warning is shown. If they have changes not uploaded and what arrives doesn't touch them, they are set aside during the rebase (`--autostash`); if what arrives also changes them, nothing is fetched or pushed (exit code 2), because git would leave them with conflict markers.
> - If git conflicts, the rebase is aborted and it exits with 2 explaining what conflicted. The local commit stays (it isn't undone): the ledger and the cursors remain exactly as they were, and the next `sync` tries again.
> - `kontext clone <url> [--home] [--name]` deletes what it cloned if it isn't a `kontext-home` (it has no `project.toml`) or if the name is already registered, and warns if there is already a file for this machine's writer (two machines with the same writer would conflict).
> - `kontext status` (also `--json`): project, writer, remote and branch, last sync (`.kontext/last-sync`), own entries not committed and not pushed (against the remote branch of the last `fetch`, without using the network), index, chains, alerts and signatures.

> **Implemented in phase 6:**
> - `kontext ui [--port 4700] [--host 127.0.0.1] [--open] [--as <reader>]`: a minimal HTTP/1.1 server on `std::net::TcpListener` (one thread per connection, `Connection: close`), with no runtime or new crates. Routes: `GET /` (the page, with `templates/ui/{index.html,style.css,app.js}` inside via `include_str!`), `GET /api/state[?before=<hlc>&limit=<n>]` and `GET /api/entry/<id>`. Only `GET` and `HEAD`; nothing writes, not even the cursors.
> - `/api/state` is built from what the views already calculate: `app::state` (with the fact status), `sync::status`, `views::standup` for whoever is looking, `board::epic_line` and `board::counts`. Each reader is a folder in `cursors/`; its line goes at the oldest entry (in `hlc` order) that its cursor doesn't cover, and `new` is what `cursor::new_since` gives. The ledger goes from newest to oldest; a page never splits a group of entries with the same `hlc`, so `before` (strictly lower) neither loses nor repeats entries.
> - What each row says (the box's family, the title of a record, the note, the warning, whether it is struck through, the charter's stamp) is calculated in Rust (`ui::row`); the page only draws. The stamp says "Signed by you" if whoever signed (or the pm's `approved_by`) is the one looking: `--as`, the human from `--actor`/`KONTEXT_ACTOR`, or the machine's, as in `standup`.
> - It listens on loopback by default. With loopback, a request whose `Host` is not `127.0.0.1`, `localhost` or `[::1]` with the page's port gets a 403 (against DNS rebinding). Another address only with `--host`, with a warning on stderr. The response carries a `Content-Security-Policy` with no external origins; the page doesn't cite any external URL (a test checks it).
> - The page queries `/api/state` every 5 s and only redraws if the response changed; the open detail and the earlier pages already loaded are kept. All ledger text goes in with `textContent`.
> - `examples/demo.sh [folder]` builds a realistic sample project (with `KONTEXT_NOW` to spread it over three days and its own `KONTEXT_CONFIG_DIR`) and the `kontext ui` tests run against it.

> **Implemented: the tree in `kontext ui`.** The first thing the human wants from the page is "how are we doing?", and the ledger answers how the memory was written, not that. `/` now shows the tree (§8, `kontext tree`), drawn by `templates/ui/tree.{html,css,js}` from `GET /api/tree`, which returns exactly what `kontext tree --json` prints (the ideas from the `.pm/` found from the folder `kontext ui` started in, read again on each request, as `/report` does). The page reads it every 5 s and redraws only if it changed. The ledger page moved to `GET /ledger` with everything it had (its tagline links back to the tree); `/index.html` is the tree too. `/report`, `/media/`, `/api/state`, `/api/entry/<id>`, the `Host` check and the CSP are unchanged; the tree page asks nothing of the internet and puts all text in with `textContent`. Its look is the ledger's account book: the groups in the margin, left of the double red rule ("Done" with the closed epics first, then `kontext plan`'s waves, wave 1 marked "now", then "No wave"); the marks are the ledger's squares (filled done, half started, hollow not yet), with one small square per feature next to each open epic; red only for "waiting for you" and the messages not audited. Light and dark follow the system.

> **Implemented with `doctor` and `measure`:**
> - `kontext doctor [--json]` exists to diagnose a broken setup, so it does not start with "open the project or fail": each check runs on its own, all are reported, each finding is `ok`, `note` or `problem` (with the command or action that fixes it), and it exits with 1 if there is any problem. It writes nothing: it does not open the `Store` (that creates the SQLite index) nor take locks, it reads the chains with the same verifier as `kontext verify`, what is not committed or pushed with the same code as `kontext status` (`git rev-parse`, `remote`, `symbolic-ref` and `show`, which do not touch the git index), and the skills and agent files with `install`'s plan, without applying it. A test snapshots every file of the config, the kontext-home and the code repo before and after a run, healthy and broken. The hooks are read from the repo's `.claude/settings.json` and `settings.local.json` and from the user's settings, each binary's full path unquoted (the inverse of `install`'s quoting); a problem if one is missing, if they point at different binaries, if one is in a cargo build directory, if there is no `SessionStart` hook, if `PATH` resolves `kontext` to another file, or if the capture is on without its hooks. The hook log is `<RFC 3339 time> <what happened>`, and an error line says `error: `; doctor counts them and gives the time of the last.
> - **With worktrees (§9.7):** in a linked worktree, doctor reads the main checkout's `.claude/settings.local.json` instead of the worktree's (Claude Code's settings docs) and counts its hooks and permission as this session's, and the fixes name the main checkout (`kontext install --local` there). Hooks only in the main checkout's `settings.json`, when git does not track it, are a note (worktree sessions read `settings.json` from their own folder), from a linked worktree and from a main checkout with linked worktrees (`git worktree list`); kontext's hooks in both settings files are a note too (a copy that differs runs as well). It says how the project was found (its own `.kontext.toml`, a tracked one, or the main checkout's) and the role sessions here start with, and from where; in a linked worktree not marked as the PM's whose role comes out `pm` (not from `$KONTEXT_ROLE`) that line is a note, with the fix `kontext install --link-only` in the PM's worktree, then `kontext install --worktree-role agent` in the main checkout. Capture says whether this checkout captures and why (a main checkout; marked as the PM's; `worktrees = true`; another role; an unmarked worktree, a note with `kontext install --link-only`) and prints `.pm/ here: <path>`. In a linked worktree with no `.claude/skills` (or `.claude/agents`) of its own, the main checkout's count (Claude Code's worktrees docs), then the user's folders (`~/.claude/skills`, `~/.claude/agents`, or under `$CLAUDE_CONFIG_DIR`). The extra git commands (`worktree list`, `ls-files`) only read; the writes-nothing snapshot also runs from worktrees.
> - `kontext measure <transcript>… [--since] [--until] [--json]` reads the transcripts a line at a time, without a project or kontext configuration, and prints counts, sizes, tool names and `kontext` subcommand names only, never text, file contents or arguments. The rules (requests by distinct `message.id` with the last usage, sidechains skipped, how each part of the history is counted, how a `kontext` command is recognized, the time window) are in the README, "Measuring conversations". The machine-text rule is the capture hook's (§12.3), so "typed by the human" counts what the capture would write; the `kontext` subcommands come from the CLI definition (clap). No prices.

## 11. Configuration

**`~/.config/kontext/projects.toml`**, per machine:

```toml
writer = "omar@t3-machine"

[projects.billing]
home = "~/kontext-home/billing"
```

**`<kontext-home>/project.toml`**, versioned:

```toml
name = "billing"
owners = ["omar"]                 # who signs the charter and decides
require_signatures = false        # phase 5

[[repos]]
name = "shop"
remote = "github.com/acme/shop"
docs = ["docs/**/*.md", "**/README.md"]

[context]
pm = "inject"
default = "inject"

[budgets]
pm = 9000
coordinator = 9000

[distill]
command = "claude -p --model haiku --output-format json"   # receives the prompt on stdin
enabled = true

[claims]
lease = "2h"
```

**`<repo>/.kontext.toml`**, in each code repo:

```toml
project = "billing"
default_role = "pm"
# worktree_role = "agent"   # sessions in linked git worktrees without their own (untracked) file (§9.7)
```

### Several repos

A project spans the repos in `[[repos]]`. Fact fingerprints store the repo name, so a fact from `shop` only expires when `shop` changes. To find where each repo is on a machine, `projects.toml` (`[projects.x.repos]`) is searched first, and then the `remote` is compared with the repo where the command runs.

> **Implemented in phase 4:**
> - `[projects.<p>.repos]` in `projects.toml` is `<name> = "<path>"`. The repo where the command runs is recognized by its remote (`origin`, or the first one; normalized and lowercased) and, if none of the `[[repos]]` match, by its folder name.
> - `[[repos]]` also accepts `exclude`, `historical` and `roots` (for `kontext find`).
> - `[distill]` accepts `timeout` (`240s` by default) and `model` (the actor's model; if absent, taken from `--model` in `command`). `enabled` is `true` if not stated.

> **Implemented in phase 5:**
> - `projects.toml` accepts `[signing] key = "<path>"` (with `~`): the public or private key the machine signs with (`ssh-keygen` finds the private key next to the public one, or in the agent). `--sign` signs without `[signing]`, with `~/.ssh/id_ed25519.pub`.
> - `project.toml` accepts `[people]`: `<name> = { emails = […], keys = ["ssh-ed25519 AAAA… comment"] }`. Names with spaces or commas don't work for `allowed_signers` and are skipped.

## 12. Roles and agent files

| Role | Where it runs | Reads at startup | Writes |
|---|---|---|---|
| Project manager | The conversation with you | `resume --as pm` (hook) | Charter, goals, epics, decisions with you, answers to escalations |
| Coordinator | Subagent or delegated thread | `brief epic:x` | Tasks, assignments, journal, questions to the PM |
| Implementer or tester | Subagent | `next epic:x` in its assignment | Journal of its task, questions to the coordinator, `done` with evidence |
| Scout | Subagent with a small model, read-only | Only the question | Facts with a fingerprint, and a short report for whoever launched it |

The agent files that `kontext install` writes carry these rules, briefly:
- Before starting, read your view. Don't read the full history.
- If a doubt is answered by the charter or a decision, answer it yourself. If not, `kontext ask --to <your level up>` and move on to another task.
- Don't close without evidence. If `done` rejects it for stale context, read what's new before retrying.
- Don't decide: propose.

The model for each role is not fixed in kontext. The agent file or whoever launches it chooses it. The one exception is the reader of the PM's memory, which a benchmark chose (§12.3).

> **Implemented in phase 3:** the agent files don't carry a `model` (a comment in the body says so). The scout has `tools: Read, Grep, Glob, Bash`; the implementer and the tester also have `Edit` and `Write`; the coordinator has no `tools` so it inherits all of them, because it needs to launch subagents. If it can't launch them from where it runs, it returns the assignments to whoever launched it. The coordinator claims each task on behalf of its implementer (`kontext --actor agent:impl-1 --role implementer next --epic x --claim`) and pastes the output into its assignment.

> **Implemented after long-memory:** within a session, the identity goes in arguments (`--actor`, `--role`, `--model`, global; they take precedence over `KONTEXT_ACTOR`, `KONTEXT_ROLE` and `KONTEXT_MODEL`), not in variables in front of the command. Claude Code's `Bash(kontext:*)` permission only covers commands that start with `kontext`; in `claude -p` nobody approves the others and they don't run. In that run, 153 commands were left this way: `KONTEXT_ACTOR=… kontext …` (the skill asked for it), `export KONTEXT_ACTOR=…` on another line, `timeout N kontext …` and `kontext resume | sed/head/tail`. The hook's pointer and `resume` say how to invoke `kontext` (bare, with nothing in front and no pipes) and, if `KONTEXT_ACTOR` is in the environment, "You write as <actor>". The skill, the agent files, `next` and the board use `kontext --actor … --role …`. In `install`, `--role` (now global) is still the `default_role` of `.kontext.toml`.

### 12.1 Orchestrating: sizes, waves and `kontext plan`

kontext still doesn't orchestrate (§1): it keeps the memory and the contract that all the levels share. Who launches whom is decided by the skill `kontext-orchestrate`, which `kontext install` installs, using the harness's tools where it runs.

- **Size before starting,** with four fixed criteria (epics, open architecture decisions, how much runs in parallel, how much is unknown): S–M is one agent and one task; L, an epic that the main session coordinates; XL, a PM that talks to the user, one coordinator per epic in its worktree, and its workers; XXL (greenfield), the same, in waves. Before launching anything XL or XXL, the skill shows the structure, the models per level and how many agents, and waits for a yes.
- **Waves of a greenfield project:** phase 0 without code (charter, goals and architecture decisions signed by the user, and the epic map); wave 1, the foundations, in series; the independent epics in parallel; the final wave, integration and end-to-end tests. At each boundary, `kontext plan` and `kontext standup` with the user. Within a wave the coordinators don't talk to the user: they escalate with `kontext ask --to pm` and the pm gathers the questions.
- **Harness:** T3 Code (one thread or delegated task per epic, with its worktree), Claude Code with `Workflow` (one script per wave, only if the user asks for multi-agent orchestration), Claude Code with `Agent` (the main session is the PM and coordinator, one epic at a time) or in series. The coordinator lives where the harness can handle long processes; the workers are always the lowest level.
- **Contract:** the same as §7.2 and §7.3 with any harness. The coordinator claims with `next --claim` on behalf of each worker; the worker closes with `done --since <mark>` and evidence; everyone escalates with `ask`; each epic comes out as its stack of PRs.

**Dependencies between epics.** `kontext epic add … --after <epic>` (repeatable) stores `fields.depends_on` on the epic, like a task's `depends_on`: an epic that already exists is referenced by its `id`; a name that doesn't exist yet is stored as is, with a warning, so the map can be written in any order. Without `--after` the field isn't written. As with a task's dependencies, they can't be changed later. `epic add` refuses (exit code 2) to go after itself, an id that isn't any epic's, and anything that would close a cycle.

**`kontext plan`** (and `--json`) calculates the waves without writing anything:
- A current epic is **closed** if it has at least one task and all of them are closed; otherwise it is **open**. A closed dependency is already satisfied.
- Each open epic goes in the wave after that of its last open dependency; those in the same wave don't depend on each other and run in parallel (within the wave, in `hlc` order).
- A dependency that doesn't exist, or an epic that is no longer current, is a problem; a cycle (which only arrives from another machine or version) is too. Those epics, and the ones waiting on them, are left **with no wave** with their reason. If there is any problem, it exits with 1.

> **Implemented: features in `kontext plan`.** An epic with at least one feature in force is closed when all its features in force are done (validated) and every task of the epic without a feature is closed: closing every task is not enough, someone has to check what the human sees, and a task without a feature keeps the meaning it had. The open tasks of a superseded feature don't keep the epic open, but each one raises a standing alert ("open task T-… of superseded feature U-…: close it with evidence or move the work to a feature in force"). An epic without features keeps the rule above. Its line says "(2/3 features done; 4/6 tasks closed)", and `--json` adds `features: {total, done}` only to such epics. A superseded feature doesn't count; a feature added later reopens the epic.

> **Implemented: features in the skills and agent files.** The binary knew features but the agents still planned in epics and tasks, and the pm answered "how are we doing?" with its own summary. Now: `kontext-orchestrate`'s phase 0 writes the features of each epic in the map (`kontext feature add … --acceptance "<what the human sees>"`) and checks it with `kontext tree`; tasks hang from features (`task add --feature`). The coordinator (`templates/agents/kontext-coordinator.md`) gives its tasks a feature, validates a feature when its tasks are closed (itself or through a tester, against its acceptance) with `kontext done <U> --evidence …`, and closes the epic when all its features are done and its tasks without a feature are closed. `kontext-pm` asks every items.md row for a `where` that names a feature, task or decision (or the user's discard), runs `kontext ideas --mark-audited` right before launching the reader for a coverage audit, and answers "how are we doing?" with `kontext tree` in a code block plus at most two sentences. `kontext-show` starts with `kontext tree`; the `kontext` skill mentions `--feature` on `task add` and `ask`. The coordinator file stays under its 60-line limit.

### 12.2 Installing and migrating: the skill `kontext-migrate`

An agent, on its own, installs kontext in a project and migrates the memory that already exists. The skill (`templates/skill-kontext-migrate.md`) does it in six steps: a read-only diagnosis (the binary, a v0 ledger from `ctx.py`, loose memory in `CLAUDE.md`, `AGENTS.md`, notes and Claude Code's automatic memory, and what is already in `.claude/`); installing the binary if it is missing; a single question to the human with everything that can't be deduced (the owner, where the kontext-home goes and whether it has a remote, the cutover day if another system is in use) and the plan with the paths it is going to write to; migrating (`kontext import --from-v0` and the loose memory as **proposals** with their origin, without repeating what `recall` already finds); configuring (`init`, `install`); and checking (`verify`, `resume`, the hook by hand, `inbox count`), with a final summary for the human.

- **Agents don't decide here either.** A rule in `CLAUDE.md` was written by the human, but copying it to the ledger as a decision would be deciding for them which one still applies. It comes in as a proposal (with its origin in the body and `migrated` in the tags) and the human signs it from `kontext inbox`. The source files are not touched.
- **Before there is a binary.** The skill has to be where Claude Code can see it before kontext exists on the machine. `kontext install` installs it in the repo with the others, and `kontext install --skills-only [--global]` installs only the skills (in the repo, or in `~/.claude/skills/`, or `$CLAUDE_CONFIG_DIR/skills/`) without needing a project. For the first start, without a binary, the README says how to download it with `gh` (the repo is private: the same credentials that `cargo install --git` asks for). This was preferred over putting the skill somewhere else (an install script, a separate package): one file and one command, and the skill's version is that of the binary that installs it.

### 12.3 The PM's memory of the user's ideas: the skill `kontext-pm`

The ledger keeps the work once it is a charter, a decision or a task. Before that, the user's ideas live in the conversation with the PM: brain-dumps, remarks in passing, short answers to the PM's questions, over many days. The skill (`templates/skill-kontext-pm.md`) keeps them in `.pm/`, a folder of plain files in the repo, so that no idea is lost without the user dropping it.

- **Layout.** `dumps/<date>.md`, what the user said, verbatim and append-only, one `## <date> <HH:MM>` entry per message (past ~60 KB, the day goes on in `<date>-2.md`, `-3`…, so a file can be Read whole: at the benchmark's largest size one dump went over the Read tool's limit and the readers fell back to Grep); `items.md`, a table with one row per idea (id, idea, source `dumps/<file>:<line>`, state, where it is covered, evidence); `status.md`, short; `shots/`.
- **Only the PM writes,** the main session the user talks to. It saves each message before acting on it, lists back the ideas it took (with their ids) and asks whether it missed one; the answer goes to the dump too. A session that cannot tell it is the PM does not write.
- **Answers keep their questions.** A message that answers the PM's questions is saved under a `**Coordinator asked:**` block that quotes them verbatim, then `**User:**`; a reply to something the PM proposed without a question mark quotes the sentences it replies to. On real data many of the user's messages were numbered answers ("1. yes, approved", "2. merge it yourself") that mean nothing without the question.
- **items.md points; it is not a second roadmap.** When an idea becomes work it is created in kontext (§12.1) and its row says where. Plans and task lists are not copied into it.
- **The coverage audit,** when the user comes back or asks where things stand. The PM does not read the dumps: the `kontext-pm-reader` subagent (`templates/agents/kontext-pm-reader.md`: read-only, `model: haiku`, no `effort`) lists every idea of the user's own text and reports those missing from items.md, each with a verbatim citation. The PM checks each reported idea with one targeted Grep of items.md and of the cited file before adding it as pending, then shows the user, following `kontext-show`: done, in progress, waiting for them, pending and newly found. The same reader answers the value in force of something the user said, so the PM never has to read the dumps for it.
- **Why a Haiku reader with a fixed model.** The reader benchmark (`bench/reports/2026-11-06-reader.md`: one fictional project, English, 5 seeds) found that Opus delegating the reading to Haiku, with prompt v2, matched Opus reading itself (100 % audit recall, no invented citations) at about a fifth of the cost, and that a higher effort added nothing. The skill's two task sections are Audit task v2 and Value task v2, word for word; the agent's body is Reader v2, changed only where this layout needs it (the dumps' entries and split files, the `**Coordinator asked:**` blocks as context, items.md's columns, the later entry of a date winning) plus a comment on its model. `bench/reader.py --prompt skill` reads those texts from `templates/`, so a paid run measures what ships. `model: haiku` is the alias an agent file takes for the current Haiku (it also takes a full id, as the benchmark passed it through `--agents`).
- **Why the PM still checks.** On a first audit of a real project (Spanish, a few dozen ideas), Haiku found every idea but reported one as missing that items.md had in other words, and one citation in a hundred was not verbatim. A Grep per reported idea costs little and keeps both out of items.md.
- **Public repos.** The dumps are the user's words. In a public repo the PM asks before committing `.pm/` and offers to gitignore it.
- **Installing.** `kontext install` writes the skill with the others and the reader with the agent files; `--skills-only` writes only the skill, so the skill says to ask the user before reading the dumps when the reader is not there. A repo that must not get the start hook can copy the two files by hand (README).

> **Implemented: `kontext ideas [--json]`.** An audit of a real project found that ideas were not lost from `.pm/`: the agent kept the `state` column by hand, it went stale, and the human had no view of "my ideas and where each one went". `kontext ideas` reads `.pm/items.md` of the git repo it runs in (or else of the first project repo on this machine; with none, one line that points to the skill, and exit 0) and derives each idea's state from the ledger through the ids in `where` and `evidence`: kontext's ids (or a unique prefix) and v0's (`D-0030`, kept in `fields.v0_id`; `State::resolve` now finds them, so `kontext show D-0030` does too). Seven states, in this order: no destination, waiting for you, pending, in progress, decided, done, discarded. First match wins: written `discarded` (only the user discards); empty `where`; an open question for a person (`owner`, an owner, `human:<x>` or someone in `[people]`) or a pending proposal, not dismissed, imported from v0 or not → waiting for you; tasks or epics cited → their tasks decide (all closed with a `done` that counts → done; something claimed, assigned, started or closed → in progress; otherwise, and an epic with no tasks, pending); otherwise the written state (`done` without evidence shows as in progress with a note; an unknown state as pending with a note). An id with a ledger prefix that is not in the ledger stays as text with a note; other tokens (`CVE-2024`, `PR #12`) are text, and nothing calls the network. The table is read by its header names in any case and order; other columns, lines that are not rows and rows without an id are ignored, and `\|` is a pipe inside a cell. Each idea carries the date it was said (from the dump's name), its source line quoted (≤ 200 characters, absent if the file or line is missing), its evidence split into text and `shots/` images, its notes and whether the ledger or the table gave its state. The logic is in `src/ideas.rs` and returns a plain `Ideas`, separate from the printing, so the report can reuse it.

> **Implemented: ideas that point to features, "Not placed" and audits.** With features (§4.3), an idea's row can say which piece of visible work it became, and the human can ask the one question the states did not answer: "is every idea of mine somewhere?". A state says how far the work went; it does not say whether the idea has a home, and an idea that cites only an epic ("Payments") shows as pending for as long as the epic has an open task, even if no feature or task carries it.
> - **An epic cited** with features is done only when `kontext plan` closes it (§12.1: its features in force done and its tasks without a feature closed); otherwise its tasks decide as below, with "done" shown as in progress. An epic without features is decided by its tasks exactly as before.
> - **A feature cited in `where` or `evidence`** decides like tasks do (rule d): done → done; to validate or in progress → in progress; pending → pending; superseded → the feature that replaced it decides, with a note "U-a was superseded by U-b". Cited with tasks or epics, all done → done, all pending → pending, otherwise in progress. Without a feature cited, the seven states are derived exactly as before.
> - **Placed** (`placed` in `--json`) is apart from the state: an idea is placed if the user discarded it or its `where` cites a feature, a task or a decision that counts: a superseded one if its replacement does, never one that is not counted, ignored or rejected (it keeps its note, and `cites` says its state: "feature U-… not counted"). One that cites only an epic, a question, a proposal, text or nothing is not, and carries what it does cite (`cites`: "Payments, no feature", "question Q-… open", the text). `kontext ideas` prints "Not placed: N (ids)." under the summary and "not placed: …" under each such idea; `--json` adds the list `not_placed` (id, idea, date, quote, cites).
> - **Audits.** The coverage audit had no memory: the PM could not tell which of the user's messages a reader had already gone through. `kontext ideas --mark-audited`, run right before the PM launches the reader, writes `.pm/audited.json` in the `.pm/` that `kontext ideas` and `kontext tree` read (`ideas::locate`: the repo it runs in, then the project repos on this machine, the first with an `items.md`): the time (`KONTEXT_NOW` or the clock) and, per dump in `.pm/dumps/`, how many `## <date> <HH:MM>` entries it had. A message is not audited when it is beyond those counts or in a dump not listed; without the file, every message counts as never audited. `kontext ideas` prints "N of your messages not audited since Nov 12." (or "never audited"; nothing at 0), and `--json` adds `unaudited {count, since}`. It counts entries, not lines, because the dumps are append-only: an earlier entry never moves. With no `.pm/items.md` found, `--mark-audited` is refused with exit 2, says which `.pm/` folders it looked in, and writes nothing.
> - **Unchanged.** The report reads the same `Ideas` and shows nothing new. Of the read-only views of a project without features, only `kontext ideas` changes: its two new lines and the new JSON fields.

> **Implemented: capture.** The dumps depended on the PM remembering to save each message; a model-free hook now writes them (`src/capture.rs`), so the user's words no longer depend on an agent. One writer per message: the skill tells the PM to Grep today's dump for a piece of the message and append it only if it is not there (hook off, or the message was skipped).
> - **Opt-in.** `[capture] enabled = true` in `project.toml`; off by default and with `KONTEXT_CAPTURE_DISABLE`. `kontext install` wires `UserPromptSubmit`, `Stop` and `SessionEnd` only when it is on (§9.3), and each run checks the setting again.
> - **Who.** Only the session the human talks to: the role (`role_for`, §9.2) must be `pm`; an entry with `isSidechain: true` is never read. Otherwise nothing is written and `hooks.log` says why.
> - **Typed messages, at once.** At `UserPromptSubmit`, from the payload's `prompt`, so the dump line exists during the turn and the PM can cite it in items.md. Claude Code sends no field that says whether a human typed it, so machine text is told by its start: a prompt that begins (after leading whitespace) with `<task-notification>`, `[SYSTEM NOTIFICATION`, `<system-reminder`, `<cross-session-message`, `<agent-message`, `<local-command-`, `<command-name>`, `<command-message>`, `<bash-input>`, `<bash-stdout>`, `Caveat: The messages below were generated by the user while running local commands`, `[Request interrupted by user`, `Delegated task node:`, `Update on pull request #` or `Another Claude session sent a message:`, or with a prefix in `[capture] skip_prefixes` (added to those), or that is only a slash command (`^/[\w:-]+$`), or empty, is skipped. When in doubt it is written: a machine message may get in, and `hooks.log` (which names the rule, never the text) tells the user which prefix to add. The same `prompt_id` is written once. `[Request interrupted by user`, `Delegated task node:` and `Update on pull request #` come from a replay: in a real conversation with 846 user turns, 112 were typed by the human, 685 were delegate reports and pull-request updates that a harness posts as user turns with the same metadata, 2 were interruption markers and the other 47 were machine text the earlier prefixes already covered; with them the replay wrote the 112 and no machine text. `Another Claude session sent a message:` is the fixed wrapper Claude Code puts on a message from another session; on a real PM conversation it was not in the list and would have been written as the human's. A prompt that an agent schedules or sends from another thread arrives as a plain user turn with no fixed wrapper: in the PM's own conversation its opening words go in `[capture] skip_prefixes`.
> - **The question it answers.** The last assistant entry with text before the prompt, not a sidechain, matched on the entry's top-level `type` (Claude Code's `api-request-blob` records also carry message text). Only the transcript's tail is read: the last MB, then 4, 16 and 64 MB if it has no assistant text (these files reach hundreds of MB). Its lines that contain `?` go under `**Coordinator asked:**`, verbatim, each with `> `: the last 20 such lines within 2,000 characters (the questions a message ends with are the ones answered). When it has no such line, its last paragraph goes there instead, under the same marker: fenced code blocks (```` ``` ````) are dropped, the text is split into paragraphs on blank lines, and the last one is taken, or the one before it if it is only Markdown headings or has no letter or digit (a horizontal rule). It is quoted verbatim, line by line, within 600 characters (counted as for the questions, without the newlines): the whole lines from the end that fit, or, if the last line alone does not, its last characters from a word boundary after `…`. A replay of a real conversation showed why: only 15 of its 112 typed messages got a block, because in 96 of the other 97 the coordinator's last message asked nothing with a question mark ("Say go and I merge and apply recommendations 2 to 6") and the user answered "go"; in those 96 the last paragraph had a median of 154 characters and a 90th percentile of 358, and 95 were at most 600 (one ended in a fenced code block). No assistant text, nothing left after those rules, or no transcript (the first prompt of a session: the file does not exist yet), no block. Redaction applies to the quoted lines as to the user's text. `kontext ideas` and the report never quote a line under that marker (nor a marker or an entry's header) as the user's words: the idea gets no quote and a note that its source is not the user's own text.
> - **Question-tool answers.** At every trigger (`user-prompt-submit`, `stop`, `session-end`) the entries the transcript gained since last time are scanned for a `user` entry whose `toolUseResult` is an object with `questions` and `answers` (`AskUserQuestion`; other shapes are ignored). Each one becomes one entry: the answered questions under `**Coordinator asked:**` and their answers under `**User:**`, numbered the same way and in question order when there is more than one, dated by the entry's own `timestamp`.
> - **State.** Local, in `.kontext/capture/<hash of the transcript path>.json` (not synced; nothing in the ledger): the byte offset scanned, when the transcript was first seen, and the last 256 `tool_use_id`s and `prompt_id`s written. No backfill: the first time a transcript is seen its mark is its end, so nothing said before capture was on is written. A transcript that does not exist yet (the first prompt) gets the mark 0, so the first turn's answers are not lost, and answers older than that first sight are skipped (a continued session whose file appears with old history). A file shorter than its mark starts again from its end. Files unused for 30 days are deleted, as `.kontext/sessions/` is (the list of named conversations is not under `capture/`, so that expiry never touches it). Everything runs under `.kontext/capture.lock`; each entry is appended with `O_APPEND` in one write, and the state is written to a temporary file and renamed.
> - **Where.** `.pm/dumps/` at the root of the git repo that contains the payload's `cwd` (not in a git repo: nothing, noted). The file is `<YYYY-MM-DD>.md` by local date (jiff's `tz-system` and `tzdb-zoneinfo` features, no new crate), and the entry's header and its file always share that date: an answer given at 23:59 and scanned after midnight goes into the earlier day's file, as `kontext ideas` reads the date from the name. Past 60,000 bytes it goes on in `<date>-2.md`, `-3`… (the last of the date that exists; no empty file is created).
> - **Format.** Exactly the one in the skill's section 2: `## <date> <HH:MM>`, the optional `**Coordinator asked:**` block, `**User:**`, one blank line between entries, a newline at the end. The user's text is verbatim except for secrets, which become `[secret removed]`, in the user's text and in the quoted questions: first a capture-only pass (a token with a vendor prefix, 2 to 8 letters, `_` and 20 or more letters and digits with at least one digit; a vendor prefix and `_` before a UUID; a hex string of exactly 32 characters with a letter and a digit that is a whole run of letters and digits, so a 40-character git SHA or a 64-character hash stays; a Slack token, `xox[abprs]-` and 10 or more letters, digits or `-`), then the distill's redactor (PEM keys, GitHub tokens, `sk-…`, AWS keys, `Bearer …`, `password=`/`token:`… values, long high-entropy strings). The capture-only pass exists because on the same real conversation the distill's redactor removed nothing where a hand audit removed four values; the distill itself does not change and keeps its own `[REDACTED:<kind>]`.
> - **Git.** When the hook creates `.pm/dumps/` and there is no `.pm/.gitignore`, it writes one with `dumps/`. It never touches an existing one, nor the repo's own `.gitignore`.
> - **Quiet and cheap.** No `Store`, no `State`, no ledger read: it resolves the project, reads `[capture]`, appends. Nothing on stdout or stderr on any path, exit 0. `hooks.log` gets one line per run (the event, the session, how many entries, how many characters and where, or why nothing), never the user's or the assistant's text.
> - **Worktrees.** A linked git worktree (its git dir is not the repo's common git dir; one `git rev-parse`, before any lock or state) writes nothing by default: delegated agents run in their own worktree with no `KONTEXT_ROLE`, so they count as `pm`, and their briefs would go in as the user's words and leave an untracked `.pm/` in each one; dumps there would also go when the worktree is removed. With `[capture] worktrees = true` the worktree's own `.pm/` is used, and its dumps go with it.
> - **The PM's worktree (§9.7).** When the PM's own conversation runs in a linked worktree, its messages were saved by hand by the model, which is what the hook exists to avoid. A worktree marked as the PM's (its own untracked `.kontext.toml` with `default_role = "pm"`, written by `kontext install --link-only`) now writes into the main checkout's `.pm/`, the one `kontext ideas` and the report read from every checkout. Any other linked worktree is still skipped, and `hooks.log` says "a linked git worktree not marked as the PM's, nothing written (if the PM works here, run `kontext install --link-only` in it)"; the role check comes first, as before. Two sessions (the main checkout's and the PM's worktree's) append to the same dump under the same `.kontext/capture.lock` of the kontext-home, so their entries do not interleave, and the per-transcript mark writes a prompt delivered twice once. `kontext doctor` says whether a checkout captures and why, and prints the `.pm/` path; the `kontext-pm` skill sends the PM there from a worktree.
> - **Named conversations.** The rules above go by role and checkout, and on the first real project two conversations shared one linked worktree with role `pm`: the PM's and a coordinator's, whose "user" turns are relays and reports from other threads, plain prompts no prefix catches (about three of every four entries would have been agent text recorded as the human's); a second coordinator ran in the main checkout as `pm` too. Only the `session_id` tells them apart. `kontext capture here` names the current conversation (its id from `CLAUDE_CODE_SESSION_ID`, which Claude Code sets in Bash tool and hook subprocesses and which matches the hook input's `session_id`; outside Claude Code, `--session <id>`); `kontext capture list` and `kontext capture forget <id> | --all` show and drop them. Once at least one is named, the hook writes only for named conversations, on both paths (the prompt at `UserPromptSubmit` and the question-tool answers at every trigger); the role and checkout rules still apply first, so a named conversation in an unmarked worktree writes nothing. With none named, nothing changes. A payload without a `session_id` writes nothing while conversations are named. A skipped run logs "this conversation is not one of the N named on this machine, nothing written (`kontext capture here` in it names it)". The list is machine-local, for the project: `.kontext/capture-sessions.json` of the kontext-home (`id`, when, from which folder), written under `.kontext/capture.lock`; not in `.kontext.toml` (git may track it) nor in `project.toml` (synced). A list that cannot be read writes nothing (an error in `hooks.log`), never everything. The id changes on `/clear`: the conversation has to be named again, and the `kontext-pm` skill tells the PM to check `kontext doctor` when the hook missed a message. Claude Code documents that after `--continue`, or `--resume` without an explicit id, the variable may hold the startup id and not the hook's `session_id`: `here` then names an id no hook sends, and the PM's own prompts log "not one of the N named" under another `session=`; `kontext capture here --session <that id>` names the right one. A skipped conversation's mark does not move, as with the role rule: if it is named later, its question-tool answers since it was first seen are written then. The commands work with the capture off (name first, check `doctor`, then turn it on); `here` says why nothing will be captured yet in a checkout or role that does not capture. `doctor` says how many are named and whether this conversation is, with a note when the capture is on, the checkout captures, the role is `pm`, some are named and this one is not. The start hook says nothing about it: the coordinator sharing the folder would read it too.

## 13. Distill

As it is today in `ctx.py`:
- It runs in the background when compacting or closing.
- It removes secrets before sending the text to the model.
- It remembers how far it read each transcript, so it doesn't read the same thing twice.
- It doesn't repeat titles that are already in the ledger.
- It writes only `proposal`, `question`, `fact` (without a fingerprint) and `journal`, always as the actor `agent:distill` and marked as unreviewed.

One thing changes: the model's command is configurable (`[distill].command`) instead of a fixed path to opencode.

> **Implemented in phase 4:**
> - The command runs with `sh -c`, the prompt on stdin, in an empty temporary folder (so it doesn't load the repo's configuration or hooks) and with `KONTEXT_DISTILL_DISABLE=1`. If its output is the `claude --output-format json` envelope, `.result` is taken (and an `is_error` is an error); otherwise the output as is. When `[distill] timeout` is exceeded, its process group is killed. A chunk that fails is retried once, as in v0 (without opencode's ladder of variants).
> - The prompt no longer names the project it was first written for or its owner: it carries the project name and its owners, asks for titles in the language of the conversation, and lists the known titles (decisions, proposals, questions and facts; the last 4,000 characters).
> - Questions go with `fields.to = "owner"`; facts with `fields.fingerprints = []`. Everything carries `fields.source = "distill"` and `fields.transcript`, and the actor `{type: agent, name: distill, model}`. Repeated titles (normalized as in 7.4) are skipped, against the whole ledger and among themselves.
> - Messages are extracted as in v0: text, plus one line per tool (`[tool Bash: …]`, `[tool result: …]`); no reasoning, no `<system-reminder>`, and no `isMeta`, `isCompactSummary` or `isSidechain` records.

> **Implemented for long memory (less noise and data that changes):**
> - The prompt asks only for what the human said, decided or asked in their own messages (`source: user`), or an assistant conclusion that the human accepted (`agent`). Nothing from pasted documents, logs, tool output or long attached text, unless the human says that something from there applies (`document`). Nothing that is read from the code, and no questions the assistant asked itself. Each title carries the concrete value. Each item carries `when`, the time of the message it comes from.
> - What is in force goes into the prompt with its id (decisions in force, pending proposals, open facts and questions; the last 6,000 letters) instead of only the titles: if something updates one of those entries, the model gives its id in `supersedes`. A unique prefix is accepted; an id that is not in force is ignored. A fact that contradicts a decision in force is written as a proposal (an owner decides it).
> - In the state, a fact or a proposal with `fields.supersedes` immediately replaces a fact in force, or a pending proposal from the same actor, from another agent if whoever writes is also an agent, or from anyone if whoever writes meets the owners rule (a human's proposal is only replaced by them or an owner). The distill can cite several ids: all the entries in force with an old value of the same thing. A decision, a charter or a goal can't be replaced this way: that stays with an owner (the proposal remains pending, with its alert, until they accept it).
> - Entries carry `fields.origin` (`user`, `agent` or `document`) and `fields.said_at`. `fields.source` remains `distill`.
> - Cap per run: `[distill] max_entries` (8). If the model returns more, the user's are kept first, then the document's, then the agent's; proposals and facts before questions, and the journal last (one per chunk). When the cap is reached, the remaining chunks only advance the offset.
> - A message longer than `[distill] max_message_chars` (4,000; 0 turns it off) is cut before sending: the beginning and the end are kept (where the person usually says what they want) and in the middle a marker for omitted pasted text.
> - Since the `SessionEnd` hook runs at the end of each `claude -p`, the distill runs after each turn; with the offset per transcript it only reads what is new (checked in the benchmark smoke test: each transcript's byte ranges follow each other, without repeats).

> **Implemented after long-memory (dates):** in the small run the user said "the new one is already in force, 1.8 % monthly" and the distill noted "in force from 2026-11-05": the UTC date of the message, one day after the session's local date. The agent compared it with its "today" and said the 1.8 % was not yet in force. The prompt now says that the times in the headers are UTC, gives the session's date (the date of the chunk's first message, or today's) and asks not to convert relative expressions ("already in force", "from today", "from Monday") into a date unless the human gives one; what already applies is noted as "in force", without a date (`said_at` already says when it was said), and what starts later, with the human's own words.

> **Implemented after long-memory (fewer agent calls):** in 46 `kontext-inject` sessions the agent ran `kontext propose` 109 times and `kontext log` 53 times, almost all to note what the user said or their progress, which the distill already saves; each command is one more round of the model that rereads the whole context. With the distill active (`[distill] enabled` and no `KONTEXT_DISTILL_DISABLE`), the `resume` header and the pointer's second line say: "The kontext distill already saves on its own, when this session closes or compacts, what the user said, decided or asked and the progress: don't run `kontext propose` or `kontext log` to repeat that. Use kontext to look things up (`kontext show <id>`, `kontext recall <topic>`), to ask the human (`kontext ask`), and for what the distill can't pull from the conversation (a finding in the code with `kontext fact --path`, a proposal from you to change a decision)." The `kontext` skill says the same and clarifies that subagents don't go through the distill and keep writing their progress. Without the distill, the text from before.

> **Implemented in the long-memory benchmark:** each distill that a hook starts leaves a mark in `.kontext/distill-pending/` (with the transcript and its pid) and deletes it when it finishes, whether well or badly (`KONTEXT_DISTILL_PENDING` says which one it is; it isn't passed to the model command). `kontext distill --wait [--timeout 600s]` waits until none is left; a mark with a pid that no longer exists, or with no pid and older than 30 s, belongs to a distill that died and is deleted.

## 14. Several people

- **Transport:** the `kontext-home` is a git repo. `kontext sync` does pull and push. Since each writer has its own file, merging never conflicts.
- **Identity:** the writer is `<user>@<machine>`. The human name comes from `git config user.email`, mapped through `project.toml`.
- **Permissions:** only the `owners` can `sign`, `accept`, `reject` and `decide`. An entry of those types written by someone who is not an owner is stored, but doesn't count, and appears as an alert.
- **Signatures (optional):** with `require_signatures = true`, `sign`, `decide` and `accept` need an SSH signature that can be verified against the keys in `project.toml`.
- **Later:** `kontext serve` with the same protocol (joining files), for anyone who doesn't want to use git. It is not in these phases.

> **Implemented in phase 5:**
> - **Identity:** the machine's human name is the person in `[people]` matching the `git config user.email` of the `kontext-home`, or if there is none, the previous one (`git config user.name`). It is used as the default reader for `standup`. If someone writes as `human:<name>` and that email doesn't belong to that person, a warning is shown on stderr; without `[people]` or with `require_signatures`, no warning.
> - **What is signed:** `sign`, `decision`, `accept`, `reject` and `supersede` (all of them, including a fact's), written by a human (the identity is their name) or by the pm with `approved_by` (the identity is that owner: the pm runs on the owner's machine and signs with their key). It is signed if the machine has `[signing] key` or with `--sign`, whether or not it has `require_signatures`.
> - **Verifying:** `ssh-keygen -Y verify -n kontext -f <allowed_signers> -I <person> -s <signature>` with the `hash` on stdin; `allowed_signers` is built from the keys in `[people]` in `.kontext/tmp/`. A key that doesn't belong to that person doesn't verify, even if it belongs to someone else on the project.
> - **`require_signatures = true`:** those types only count if the signature verifies for the person, and the owners rule is the same (§4.3). Those that don't are stored, don't count, and show up as an alert. What was imported from v0 doesn't need a signature (the v0 rule had already accepted it). So as not to write something that wouldn't count, `kontext` refuses (exit code 2) if the machine has no key or if its signature doesn't verify for that person.
> - `kontext verify` checks the signatures in the files (not in the index) and exits with 1 if any doesn't verify; with `require_signatures` it also reports the missing ones.

### 14.1 The server protocol

The server relays ledger lines. It stores them and hands them out; it never derives state. Each project gets its own server, which its owner runs: a Cloudflare Worker (`server/worker.js`, plain JavaScript with no build step, embedded in the binary) with one SQLite-backed Durable Object per project. The local `kontext-home` stays the working copy, and the binary computes everything from it, as before.

**What the Durable Object stores** (tables `WITHOUT ROWID`, keyed by what is looked up, so each stored line is one row written):

| Table | What |
|---|---|
| `lines(writer, n, hash, line)` and `line_chunks(writer, n, i, data)` | Each writer's ledger lines, in order, exactly as the binary wrote them (`line` is the text of the line, without the `\n`). A line over 512 KiB is kept in `line_chunks`, by its UTF-8 bytes in 512 KiB rows, with `line` empty |
| `tails(writer, n, hash)` | The last line of each writer: the server's frontier |
| `cursors(reader, writer, body, stamp, seq)` | The cursor files `cursors/<reader>/<writer>.json`, with their text; last write wins by `stamp` (the file's time in ms) |
| `config(name, version, sha256, body, by, at)` | `project.toml` and `synonyms.toml`, versioned |
| `media(name, sha256, bytes, chunks, seq)` and `media_chunks(name, i, data)` | The files of `media/`, by `<sha256>.<ext>`, in 1 MiB chunks (a row holds at most 8 MB) |
| `tokens(id, hash, person, role, epic, writer, …)` | The tokens, by their sha256 |
| `meta(k, v)` | The project's name, its creation time, and `seq`, a counter that goes up with every change |

**Requests.** Everything is JSON under `/v1/p/<project>/`, with `Authorization: Bearer <token>`; the Worker only routes (it checks that a token is there, so a request without one is never billed to the Durable Object), and the Durable Object authenticates.

| Request | Who | What |
|---|---|---|
| `GET /v1/info` | anyone | `{"kontext_server": 1}` |
| `POST create` `{person, writer}` | the admin secret | Creates the project and its first owner token, bound to `writer`. 409 if it exists |
| `GET whoami` | a token | Its person, role, epic and writer |
| `GET frontier` | a token | `{seq, tails: {writer: {n, hash}}, config: {name: {version, sha256}}, registry}` (`registry` is the registry's version) |
| `POST push` `{writer, lines: [text…]}` | the writer's token, or the admin | Appends (below) |
| `POST pull` `{since: {writer: n}, limit}` | a token | The lines beyond `since`, writer by writer and in order, up to 2,000 lines or 4 MB, `more` if there is more, and the registry's version |
| `GET registry` | a token | Every token's claims by writer, in the order they were made: `{version, writers: {writer: [{person, role, epic, actors, first_n, ord, expires, revoked}]}}`; never an id, a hash, a secret or who made it (§14.2) |
| `GET cursors?since=<seq>` / `POST cursors` `{cursors: [{reader, writer, body, stamp}]}` | a token (only its writer's files) | Cursor files |
| `GET config` / `PUT config/<name>` `{base, body}` | an owner token or the admin to write | The new version only on top of the one the client saw (`base`; 409 with the current one otherwise) |
| `GET media?since=<seq>` / `GET media/<name>` / `PUT media/<name>` | a token | Files by content; the server checks the sha256 in the name |
| `GET ws` (WebSocket) | a token | The frontier when it connects and after every change |

**Appending.** The lines go one by one, in order, and each one must continue the server's chain for its writer:
- same `n` and same `hash` as a line the server has: skipped (`duplicates`), so a retry is always safe;
- a different line at an `n` the server has, or `prev` that is not the hash of the server's last line: refused as `fork`;
- `n` beyond the next one: refused as `gap`;
- `writer` that is not the token's (or a line inside that says another writer): refused as `writer`;
- what the token's role may not write (§14.2): refused as `identity`.

The first refused line stops the batch; the answer carries what was accepted, the server's tail and the refusal (`{index, n, reason, message}`), with 409 for `fork` and `gap` and 403 for `writer` and `identity`. So a fork is never stored. The server reads `writer`, `n`, `prev`, `hash` and `actor` from a copy of the line and stores the text it received; it does not recompute the hash (the binary does that when it pulls).

**Notifications.** `GET ws` opens a hibernatable WebSocket (`ctx.acceptWebSocket`): while nothing changes the object can be evicted and no duration is billed. After each change the server sends `{"type": "frontier", seq, tails, config, registry}` to every socket of the project: the frontier, never the lines. Clients pull what they lack. A socket is tagged with its token, whose id is kept in the socket's attachment (both survive hibernation). Before every send, the token is checked again: a socket whose token was revoked or expired is closed (code 4001) instead of hearing the change, and revoking a token closes its sockets at once.

**Tokens** are `kxt1.<base64url of the claims>.<secret>`, with the claims `{"p": project, "w": writer, "n": person, "r": role, "e"?: epic, "c"?: [names a coordinator writes as], "x"?: expiry in ms}`. The binary reads them without the network: it writes as the token's writer, and it refuses (exit 2) a line the server would refuse (§14.2). The server keeps only the token's sha256 and decides from its own row, never from the claims. A token is bound to a person, a role, optionally an epic, and one writer; no two active tokens share a writer (a revoked or expired one does not count, so a writer can get a new token).

**Limits** (Cloudflare's, 2026-11-13; [Durable Objects limits](https://developers.cloudflare.com/durable-objects/platform/limits/), [pricing](https://developers.cloudflare.com/durable-objects/platform/pricing/), [Workers limits](https://developers.cloudflare.com/workers/platform/limits/)): on the Workers Free plan a Worker gets 10 ms of CPU per request and a Durable Object 30 s, so the Worker only routes and all the work is in the object; 100,000 requests and 100,000 rows written per day, 5 GB stored; a row, string or BLOB holds at most 8 MB, and a statement 100 KB. A push takes at most 1,000 lines per request. **A line holds at most 4 MiB** (`MAX_LINE_BYTES`, the same constant in `server/worker.js` and `src/server.rs`, pinned by a test; a longer one is refused as `too_large`): about a thousand times the largest entry kontext writes (an entry carries text; images go to `media/`), and a push of one such line, JSON-escaped (at most twice its size), stays far below the 100 MB request body and the 128 MB of an isolate. In a project with a server the binary refuses to write a longer line (exit 2, nothing written), so every line it writes can be pushed; without a server there is no such limit. An index adds a row written per indexed write ([D1 pricing](https://developers.cloudflare.com/d1/platform/pricing/)), so the tables have no secondary index (the few tokens are looked up by scanning their hashes).

**Deploying.** `kontext server deploy` uploads the Worker with the Cloudflare REST API (§10), with the tag `kontext-server` in the upload's metadata, which the script list returns (`tags`). A Worker with the same name and without that tag is somebody else's: deploy refuses it (exit 2) and changes nothing, unless `--replace`. For the tests, `server/dev.mjs` runs the same file in workerd through Miniflare, a dev dependency under `server/` that only CI and development need.

### 14.2 Tokens and roles

Every machine, person and sandbox that syncs with the server has its own token, and with it its own writer. An owner makes them with `kontext token create`; the admin secret (set when the server is deployed, kept in `servers.json` of the config folder, mode 0600) only creates projects, and `kontext server connect` and `init --server` use it once to get the first owner token. A copy that started without a server joins one with a token alone (`KONTEXT_TOKEN=<token> kontext server connect <url>`): it uploads its own writer's lines, and the token is saved only once the server takes it. A token goes only to its own project's server: a project that already syncs with one sends neither its saved token nor `$KONTEXT_TOKEN` to another URL.

**What each role may write.** The server checks every pushed line against its token: only identity (who the actor is, and which kinds its role writes), never the project's state. That stays where it was: the binary computes it from the ledger, and an entry that does not count is still stored and shows up as an alert (§4.3).

| Token's role | Writes as | May not write |
|---|---|---|
| `owner` | anyone (an owner can make a token for anyone anyway) | — |
| `pm` | agents (any role, `approved_by` included) | as a human |
| `coordinator` (of an epic) | agents; as a coordinator only with the epic's coordinator names (its `coordinator` and `epic:<name>`, fixed when the token is made) | as a human; as the pm; another epic's coordinator; `approved_by`; `charter`, `goal`, `epic`, `feature`, `scope`, `sign`, `accept`, `reject`, `decision`, `supersede`; `assign` except as one of its names with role `coordinator` |
| `implementer`, `tester` | agents | the above, and as a coordinator (role `coordinator`, or a name `epic:…`); `task`, `assign` |
| `scout` | agents | the above; anything but `fact`, `journal`, `question`, `confirm`, `proposal` |

A `supersede` whose ids are all facts (`F-…`) counts as a fact: it is what any agent writes when it records a stale fact again (§4.3), and the ids say so without the project's state. A coordinator's subagents write through its token: `--actor agent:impl-1 --role implementer next --claim` is allowed, and so are agents with no role (the distill). The epic of an implementer's or tester's token is noted, not enforced (that would need the project's state).

**Whose powers a line has.** A name is not an identity: an epic's coordinator can have any name (`--coordinator lead-pay`), and any token may write as an agent of that name. So with a server, the binary counts a line as the coordinator's (assign, release or close another's task, validate a feature) only if the token it was written under allows it. The server serves every token's claims (`GET registry`; never a secret), and the binary keeps them in `.kontext/tokens.json`, fetched before any pulled line is applied whenever the registry's version moves. Each token records where it starts: `first_n`, its writer's tail on the server when it was made, plus one. Line `n` of a writer is governed by that writer's latest token with `first_n <= n`; everything below was on the server before the token existed. The walk reads only the line's writer and `n` and the registry, never a clock, an expiry or this copy's token, so every copy counts the same. Under no token, or an owner or pm token, the names decide as without a server. Under a coordinator token, the line has the powers only for that token's epic, as one of its names with role `coordinator`. Under any other token it has none. A line that loses powers that way is stored, does not count, and raises an alert that names the token. The binary refuses it first (exit 2) from this copy's own token; the server refuses what the claims alone decide (a coordinator token's `assign`). The epic's own coordinator token and its subagents work as before. Lines written before a token, and a project that started without a server, keep counting as they did: `server connect` creates the owner token before it uploads, so all of that writer's lines fall under it, and the other writers' lines fall under no token. A copy that joins with a token of a narrower role (below) has its earlier lines counted under that token. A project without a server reads no registry.

**Refusing before writing.** A line the server refuses can never be pushed, and nothing after it from that writer can either. So the binary runs the same rules (from the token's claims) under the writer's lock, before the line is in the file, and refuses with exit code 2, naming the token. The server's check is the backstop for a line written before the token, by an old binary, or with a token without claims. If it happens, `kontext status` says so and how to get out: an owner revokes the token and makes one for the same writer with a role that may write the line (`kontext token create --writer <w> --role …`); `KONTEXT_TOKEN=<new token> kontext server connect <url>` on that copy saves it and pushes everything.

**Sandboxes.** `kontext token create` prints the variables a sandbox needs: `KONTEXT_SERVER`, `KONTEXT_PROJECT`, `KONTEXT_TOKEN`, and its actor and role (`KONTEXT_ACTOR=agent:epic:<name>` and `KONTEXT_ROLE=coordinator` for a coordinator). In the sandbox, the first kontext command (the start hook included) sees the token for a project that is not registered and clones it into the config folder (`<config>/homes/<project>`), under a lock so that a coordinator and its subagents starting at once set it up once. It runs with the automatic sync's limits (2.5 s a request, 6 s in all, no media), and a process waits at most 2.5 s for another one's lock; what does not fit comes with the next commands. If the server cannot be reached, no process on that machine tries again for 30 s (`<config>/bootstrap-backoff.json`), so a server that hangs costs each hook and command at most one short timeout. The claims are not signed, so they are not trusted: `Token::parse` takes only a valid project name (letters, digits, `-` `_` `.`, not starting with `.`) before any path or URL is built from it, and the server confirms the project and the writer (`GET whoami`) before anything is written. The set-up writes a marker (`homes/.<project>.bootstrap`) first and removes it when the project is registered; a folder left by a set-up that did not finish is removed only if it carries that marker, is a real directory (not a link), and is directly inside `homes/`. Anything else there stays, and the set-up says so. The variables only set up a project this machine does not have: a registered project always uses its own configuration, its token never goes to `KONTEXT_SERVER`, and a command (never the hook) warns when `KONTEXT_SERVER` names another server than the project's. Nothing goes in the repo: no token, no server URL, no ledger. A token can expire (`--ttl 8h`) and be revoked (`kontext token revoke <id>`), and it stops working at once.

**A second machine of the same person** gets its own token, normally with its own writer; `--writer` reuses one (only one copy at a time may write as a writer: the server refuses the second copy's lines as a fork).

## 15. Benchmark

In a separate repo, `kontext-bench`.

- **Scenarios:** scripted, over several sessions. Each one carries:
  - a charter;
  - planted decisions;
  - days later, "temptations": a turn that suggests the discarded option or asks for something outside the scope.
- **Execution:** with `claude -p`, unattended, killing the session between "days".
- **Variants:** no tool; with `CLAUDE.md` and notes written by hand; with `kontext` in `pointer` mode; with `kontext` in `inject` mode. Optionally, Beads.
- **Measures:**

| Measure | Calculation | How it is measured |
|---|---|---|
| Work outside the charter | % of changed lines that can't be tied to a goal | Deterministic |
| Decisions violated | % of guards that fail at the end | Deterministic |
| Memory | Hits on questions taken from the ledger, asked after N compactions | Model judge, plus a hand-graded sample to check whether it agrees |
| Times you repeat yourself | Scripted turns of "we already decided that" that were needed | Deterministic |
| Cost | Tokens and dollars per scenario | Deterministic |
| Finished | % of the scenario's tests that pass | Deterministic |

- **% drift** = (lines outside the charter + failing guards, weighted) / total. The exact formula is fixed before the first run and isn't changed after seeing results.
- **What gets published:** the scenarios, the harness, the raw transcripts and the table, with several seeds per variant.

> **Implemented in phase 6:**
> - The harness lives for now in this repo, in `bench/` (Python 3.11+ with no dependencies: it's glue). Before publishing it can be moved to `kontext-bench`.
> - Scenario: `bench/scenarios/<name>/scenario.toml` plus `repo/` (the starting code) and the acceptance tests, which are copied at the end so the agent doesn't see them. It carries the charter, goals with `scope` (globs of the paths they apply to), decisions with guards, days with turns (some with `temptation = "decision:<id>"` or `"scope"` and their `correction`), and memory questions with expected words. There is one: `billing` (3 days, 2 decisions, 2 temptations, 3 questions).
> - Variants: `none`, `notes` (a `CLAUDE.md` with the charter and the decisions written by hand, which the agent can edit), `kontext-pointer` and `kontext-inject` (`kontext init` with its own `KONTEXT_CONFIG_DIR`, `[context]` in that mode, distill turned off, `kontext install`, and the charter, goals and decisions written and signed by the owner before day 1). "Optionally, Beads" is not there.
> - Each day is a new session of `--agent-cmd` (by default `claude -p --output-format json --permission-mode acceptEdits`); within the day it continues with `--resume <session_id>`. A temptation "was fulfilled" if that decision's guard passed and no longer does, or if the lines outside the charter grew; then the scripted correction goes in and it counts. The correction reminds, it doesn't order an undo.
> - Work outside the charter is measured per file (`git diff --numstat` against the starting point, with a temporary index to include new files without touching the repo's). **Formula fixed before running:** drift % = 100 × (L_outside + 50 × G_fail) / (L_total + 50 × G_total), in `bench/benchlib/metrics.py` and `bench/README.md`.
> - `--dry-run` swaps the agent for `bench/fake_agent.py`, deterministic and with no model: it follows the scenario's `[[fake.rules]]` and "knows" only what is in its session, `CLAUDE.md` and what the `kontext` hook puts in (it runs the `.claude/settings.json` hook for real). The dry-run judge is keyword-based. `run.py` refuses a real run without `--yes`. In this phase it was only run dry.

> **Implemented after long-memory:** a second scenario, `long-memory` (4 days), for "memory after N compactions": data said in passing (class B), data that changes (C, the one in force counts), and nearly identical distractors seeded in deterministic filler turns (~100k tokens per session, ~140k of window before compacting, scalable with `--fill-scale`), `/compact` in the same session, and questions at four checkpoints. The questions carry `expected` and `forbidden` (asserting the old value or a distractor is worth 0 and counts as a confusion), and some behavior tests grade code that depends on that data. In the kontext variants the distill is on; the harness waits with `kontext distill --wait` and adds its cost. Details in `bench/README.md`.

## 16. Technology

- **Rust,** with a single binary.
- **Crates:**
  - `clap`: commands;
  - `serde` and `serde_json`: JSON;
  - `sha2`: fingerprints;
  - `rusqlite` with `bundled` and FTS5: index;
  - `fs4`: locks. *Phase 1: `std::fs::File::lock` is used instead (stable since Rust 1.89, and it is also `flock` on Linux and macOS), so there is one dependency fewer;*
  - `toml`: configuration;
  - `jiff`: dates;
  - `ignore`: walking docs.
- **Git:** the `git` binary is used as a subprocess, not libgit2, so the binary stays small.

> **Implemented in phase 4:** new crates: `ignore` and `globset` (walking docs and their globs), `regex` (the docs parser and the distill), `unicode-normalization` (removing accents like JavaScript's `normalize("NFD")`; the diacritics table of `\p{Diacritic}` was generated with JavaScript and lives in `src/find/diacritic.rs`) and `libc` (killing the model command's process group).
- **Platforms:** macOS and Linux. Windows is not in these phases (because of `flock`).
- **Tests:**
  - unit tests of the state calculation;
  - views compared against saved outputs;
  - concurrency: N processes writing and claiming tasks at once; at the end the chain verifies and each task has a single owner.
- **CI:** GitHub Actions on `ubuntu-latest` (`.github/workflows/ci.yml`) runs `cargo fmt --check`, `cargo clippy` with `-D warnings` and `cargo test` on each pull request and each push to `main`; development is on macOS, so CI is where Linux gets tested.
- **Installation:** `cargo install --git`, and later binaries in releases.
- **Pending:** the `t3-machine` server doesn't have Rust. Phase 1 installs `rustup` for the user.

## 17. Phases

Each phase is used for a week on a real project before starting the next one.

| Phase | Content | Done when |
|---|---|---|
| 1. Base | Entry format, one file per writer, locks, chain, `verify`, index, `reindex`, `init`, `import --from-v0` | The ledger of the v0 `ctx.py` prototype imports completely, `verify` passes and the concurrency test with 32 processes passes |
| 2. What hurts | Charter, goals, signature, decisions, `log`, cursors, `standup`, `resume`, `recall`, hooks with the three modes, skill, `install` | A week of real use without having to ask the agent "how are we doing?" |
| 3. Teams | Epics, tasks, `assign`, `next --claim`, `done` with evidence and assignment mark, `ask` and `answer`, agent files | A coordinator carries an epic of 5 tasks with 3 implementers in parallel without PM intervention except for escalations |
| 4. Memory | Facts with fingerprint and expiry, `find` in docs, configurable distill, `changelog` | A scout doesn't repeat a search whose fact is still in force; `find` returns the expected top results and status for the golden queries over `tests/fixtures/find-docs` |
| 5. Several people | `sync`, `clone`, permissions, signatures, conflicts | Two machines write at once, sync, and the state comes out identical on both |
| 6. Interface and benchmark harness | `kontext ui` and the `kontext-bench` harness (not run) | `kontext ui` shows the ledger, the cursors and the alerts of a real project; the harness runs a test scenario dry |

## 18. Decisions on the open questions

1. **Name:** the project, the Cargo package and the command are called **kontext**. For a short while the command was `ctx`; it became `kontext` so that everything (the repo, the package, the binary, the skills, the folders) has one name, and because the natural abbreviation already has an owner in the same field. `ktx` was considered, but Kaelio (a YC startup) publishes a `ktx` context CLI for agents, exactly the same thing as this, and Khronos has another `ktx` (textures); with `kontext` there is no confusion when installing or searching for it, and four more letters in each command don't weigh much: agents mostly type them.

   The old name remains only in the `ctx-bench:` seed of the filler (changing it would change the runs' data) and in the dated documents in `bench/reports/`, which keep the names of the time.
2. **Changing a goal:** it's enough for an owner to do the `supersede`. There is no need to sign the whole charter again: `supersede` and `decide` only count if an owner writes them.
3. **Standup:** advances the cursor by default; `--peek` shows it without advancing.
4. **Distill model:** `claude -p --model haiku`, configurable in `[distill].command`.

## 19. Outside these phases

- **Running and publishing the benchmark.** The harness can be written, but each run spends tokens and publishing it means making it public, so it is confirmed first.
- **`kontext serve`** (syncing without git).

## 20. The human as a reader

The human also loses the thread, and doesn't read like an agent: they want to know in a minute what is theirs to do, what went wrong, and how what is being built looks. Listing them what is finished doesn't help. The rule: **if you haven't answered, you haven't read anything.** Showing them something doesn't count as read; only acting on it counts.

**The inbox (`kontext inbox`).** It is derived from the ledger every time; there is no separate store. Four classes:
- **sign**: unsigned charters and goals, proposals from an agent, and the user rules that the distill noted;
- **answer**: open questions for the human (`owner` or their name), grouped by epic (those with no epic, by topic);
- **review**: what the distill noted and nobody confirmed, in a single group;
- **problems**: a `done` that didn't count, a reopened task, an expired or lost claim, drift, an invalid signature, something that doesn't count, a conflict, a dependency that doesn't exist or an epic that doesn't fit in any wave. Each one says why it matters.

A pending item disappears when its cause is resolved in the ledger (someone signs, answers, accepts, rejects or fixes it) or when the human dismisses it (`dismiss`, with its reason) or confirms it (`confirm`). Those two records carry `fields.items` (the keys they close) and only count if written by a human. Seeing it in `kontext inbox`, in the report, in `standup` or in the hook doesn't close or move anything.

**Order by impact:** how many epics and tasks it blocks, directly or transitively (a task blocks those that depend on it, and a blocked epic, or one with a blocked task, blocks those that come after it according to `--after`), and then age, oldest first. A question about a task blocks that task (and that of whoever joined it); a question with no task blocks its epic only with `--blocking`; one marked not urgent blocks nothing. An unsigned goal blocks the epics that cite it; an unsigned charter blocks all of them if there is no other signed one.

**What's new** for the human is what came in after **the last thing they wrote** (their last entry as `human:<name>`), not after their last view. The `standup` cursor still advances when read, but now it only serves to avoid repeating the history ("What's new", "Nothing new since your last visit"); what is pending always shows, and its "(new)" is counted from the last action. That way, looking at `standup` twice doesn't make an unanswered question old. If the reader has never written, their cursor is used as before.

**Questions that merge, and the brake.** `kontext ask` looks for an open question with the same recipient, the same epic (or none) and similar text: words in lowercase, without accents, punctuation or stop words, without the plural `s`; they are similar if they share at least two and Jaccard ≥ 0.6, or if the shorter one (of three or more) fits inside the other by 80 %. If there is one, it writes a `confirm` that is added to it (with the asker's task, which counts for impact and for the stale context of its `done`) instead of another question; `--no-merge` writes it separately. If an agent's question reaches the human and they already have `[inbox] max_pending` pending items (5), it is marked not urgent (`fields.blocking = false`) and `ask` warns "the human has N unread pending items; does this block?"; `--blocking` marks it as blocking. A coordinator's question (`agent:epic:<x>`) carries its epic by itself.

**The hook** of the session that talks to the human (role `pm`, in `inject` or `pointer` mode) adds a line at the top: "omar has 4 pending items in kontext (oldest, from 3 days ago; the first blocks Payments).", and their last action if it was more than a day ago. Nothing if there are none. The line is subtracted from the `resume` budget, so the total stays under 9,000.

> **Implemented after long-memory:** the agent ran `kontext inbox` 86 times in 46 sessions. The hook's line said "Tell them at the start and show them (`kontext inbox`, …)", it came out on every `claude --resume`, and `kontext inbox` run by an agent looked at the human's inbox from `git config` instead of the first owner, which is the one the hook names: the hook said "has 9 pending items" and the inbox said none, and the agent ran it again. Now the line says "Tell them once, at the start. Don't run `kontext inbox` on your own: it's their inbox, not your task list; show it to them only if they ask or if something is blocking work (skill kontext-show). Don't pile new questions on them while they haven't answered.", it isn't repeated on a `resume` (§9.4), and a `kontext inbox` from an agent without `--as` looks at the first owner's, as the hook does. The `kontext` skill no longer tells agents to check `kontext inbox count` (`kontext ask` already brakes on its own).

**Everyone's inbox (`kontext inbox --everyone`).** `kontext inbox` is still a human's inbox (yours; the first owner's if an agent runs it without `--as`). To see everyone's at once, `--everyone` builds one section per human with pending items and a common one at the end, without repeating anything:
- **Who is human:** the owners, in the order of `owners`, and then, by name, the recipients of open questions who are people: `--to human:<x>` or the name of someone in `[people]`. A bare `--to <name>` could be an agent (`--to coder`), so it doesn't open a section. `--to human:<x>` is the same as `--to <x>` in every inbox (before, `kontext inbox --as x` didn't see it).
- **Per person** only the questions addressed to them by name go in. Their heading is the line from `kontext inbox count` and their last action; "new" is counted from that action, as in their own inbox.
- **Common** holds everything that today shows up the same for each owner: sign (charters, goals, proposals, user rules), review, the questions to `owner`, and the problems (which today show up for any human). It goes common and not per person because any owner resolves it, and the first one who acts closes it for everyone: repeating it in each section would make it look like N things and make each person have to do it. "New" is counted from the last action of any owner: if one of them already acted after it came in, someone saw it.
- Each section, like `kontext inbox`: the 5 with the highest impact and "and N more", or all with `--all`. Pending items have a dash and no number, because the numbers in this view aren't anyone's inbox numbers and `kontext inbox sign 2` would act on another item; each one carries its command with ids. Owners with no pending items of their own are shown on one line ("No pending items of their own: ana").
- `--json`: `{project, count, humans: [{human, owner, count, new, line, last_action, items, more}], quiet, common: {…}}`. `kontext inbox count --everyone`: one line per owner (even if they have nothing), one for each other human and one for the common items; with `--json`, the same without `items`.
- It is read-only, like `kontext inbox`. With `--as`, or with `sign`, `answer`, `dismiss` or `confirm`, it exits with 1 and says why: to act, each person uses their own `kontext inbox`.

> **Implemented: the inbox with a signed plan.** Inside a signed scope the pm decides alone (principle 7), so the human's inbox stops being the pm's to-do list. One rule: an item moves to the pm only when the pm can act on it alone, that is, with a signed plan and without `require_signatures` (`scope::pm_acts_alone`). Then an owner's inbox (and the report, the start hook's line and the common section of `--everyone`, which share it) holds only: the pending scope signature (at most one, with the first lines of its summary and the tasks it holds as its impact; `inbox sign` on it refuses and points to `kontext scope summary`, because a scope is signed on its full text), the questions for them, and the problems only an owner resolves: an invalid signature, every conflict (between decisions or charters), a scope signature that does not count (`scope:S-…` for a hash that does not match its summary, `not-counted:S-…` for one written by someone who may not sign), and anything the pm wrote that did not count. The questions for the pm, proposals, user rules, what the distill noted and the other problems go to the pm's inbox, `kontext inbox --as pm`, whose commands say `--as pm`: the pm answers, accepts or rejects, confirms or dismisses alone, except that rejecting or dismissing the owner's own words takes their yes (`--approved-by <owner>`). A pm `dismiss`/`confirm` of anything else, from the CLI (exit 2) or written by another machine (stored, not counted, an alert to the owner), changes nothing. An owner's inbox ends with how many items the pm handles. Without a signed plan, or with `require_signatures`, every inbox is exactly what it was before scopes (`--as pm` is the inbox of the reader `pm`, which the pm does not close), except that a signed plan's pending scope signature shows to the owner.

**Screenshots.** `kontext done --evidence screenshot.png [--screen <screen>]` (PNG, JPG or WebP, checked by their content) copies the image to `media/<sha256>.<ext>` of the kontext-home and saves in the evidence `{type: image, value, sha256, name, screen, bytes}`. The screen, by default, is the file name. Cap per image: `[media] max_size` (2 MB); if it is exceeded, it is rejected with the size and how to shrink it. `kontext decide|propose --attach` does the same with attachments (also an `.html` page: the phase 0 mockup). `kontext sync` uploads `media/` with the ledger (the name is the hash, so it doesn't clash), and `kontext verify` checks that each cited image is there and that its hash matches (the views don't check it: reading images is costly).

**The report (`kontext report --html [-o file]`, and `/report` in `kontext ui`).** A fixed page that comes from the ledger with a template; the agent doesn't design it. The only free part is the "where we are" paragraph, which the agent writes with `kontext log --status "…"` (the last one counts). In this order and nothing else: the status with a traffic light (red if something pending blocks work, amber if there are pending items, green if not); **Yours to do** (all pending items without the problems, from highest to lowest impact, each with its command; the first 5 in view and the rest folded into "and N more"); **Problems** with why they matter; **Your ideas**, only when `.pm/items.md` has ideas (§12.3: what the user said, in their words, and where each one went); **What it looks like** (per epic, the two most recent screens, with the previous screenshot of the same screen beside it, including the approved mockup); and **Progress** (a bar per goal and the wave line). Finished *tasks* are still not listed, but the user's *ideas* that are done or decided are shown with their evidence: the report now also answers "where did each thing I said go". With `-o`, the images are embedded (data URI) up to 12 MB and the rest goes to `<file>_media/`; in `kontext ui` they are served at `/media/<sha256>.<ext>`, which only serves images from `media/` and goes through the same `Host` check and the same CSP. The page is read-only and doesn't request anything from the network: the CSS, the font (Anybody, OFL, embedded) and a small script are inside. It is printed in two inks with ordered dither patterns, in the traffic light's ink; the text is always on a solid background. A fixed sidebar (on a phone, a bar at the top with "Sections" that opens it as a panel; Esc closes it and focus doesn't leave it) carries the project with its traffic light and your last action, the sections with how many there are in each (it marks the one in view; without script they are anchors), and a line per epic with its wave, its progress and a dot if something in it is waiting for you; clicking an epic filters "Yours to do", "Problems" and "What it looks like" by each card's `data-epic` (the epics it touches: its own, those of its tasks and those it blocks). Each pending item carries its command with "Copy" and the other output (reject, dismiss) on a discreet line with a copy icon. The script copies commands, compares before and after with a slider, enlarges screenshots (closed with Esc or a click outside) and shows them dithered in a `<canvas>` and in color on hover, focus or touch; without it the page reads in full. In `kontext ui`, `/report` has a tighter CSP than the page: the style and the script by their hash (without `unsafe-inline`), `img-src 'self' data:`, `font-src data:` and no `connect-src`.

> **Implemented: "Your ideas" in the report.** `kontext ideas` gave the human the list in a terminal; the report is what they actually open. The section comes after "Problems" and before "What it looks like", with a link "Your ideas <count>" after "Your turn" in the sidebar, and it is built from the same `Ideas` as `kontext ideas` (`report::ideas_for`): `kontext report` finds `.pm/` as `kontext ideas` does, and `kontext ui` looks again on every request of `/report`, from the folder it was started in, so an edited `items.md` shows on reload. `report::build` reads no files for it: the caller sets `Report::ideas`. It has a proof strip (one cell per idea, from the least settled state to the most, each a link to its row), a filter per state that has ideas (and "All"), and one group per state with its meaning in one line; each row has the date it was said, the idea, the quote from the dump, where it went (the `where` cell; if it only has ids, what they are), its notes, its evidence as chips and its first screenshot. Each state has its ink: no destination, solid signal ink; waiting for you, dithered signal ink; pending, sparse; in progress, decided and done, denser up to solid; discarded, hollow with the idea struck through. Decided and done go in two columns when they fit. A screenshot is `shots/<file>` of `.pm/` and goes inside the page as a data URI (also in `kontext ui`): a path that is absolute, has `..` or does not resolve inside `.pm/shots/` (symlinks included), a file over `[media] max_size`, one that is not a PNG, JPG or WebP by its content, or one that no longer fits in the page's total, stays as a chip of text. The total is the report's 12 MB for embedded images, counted in raw bytes: with `kontext report --html` the report's own screenshots are counted first, as before, and the ideas' images take what is left, in page order; in `kontext ui`, where the report's screenshots go through `/media/`, the ideas' images have the 12 MB to themselves. The ideas do not belong to an epic, so while an epic filter is on, the section and its link step aside. Its CSS and script go in a second `<style>` and `<script>` only when there are ideas (`/report`'s CSP carries their hashes only then): without `.pm/items.md`, or with no ideas in it, the report, HTML and text, has no such section, and neither its style nor its script. In text, `kontext report` adds "## Your ideas": the summary line, one line per idea that is not settled (no destination, waiting for you, pending, in progress) with where it went, and only the counts of decided, done and discarded, pointing to `kontext ideas`. A table without a `where` column now carries a warning (`Ideas::warnings`, in `--json`, in `kontext ideas` and at the top of the section): every idea then shows as no destination.

**The skill `kontext-show`** (installed by `kontext install`) says when to show (wave boundary, "how are we doing?", when coming back after days, a problem that blocks work; never after every task), how depending on the harness, how to write the paragraph, and how to close with a screenshot. `kontext-orchestrate` adds the phase 0 mockup: before the code, an HTML mockup based on the charter that the human approves and that is kept as a decision with the mockup attached; the coordinators build against it.
