---
name: kontext-show
description: Shows the human how the project is going, with the minimum worth seeing: what is waiting for them (their pending items in kontext), the problems, and screenshots of what is being built. Use it at every wave boundary, when they ask "how are we doing?", when they come back after days (the hook says how many pending items they have), or when something only they can resolve is blocking the work. It also says how to close with screenshots and when to stop asking them questions.
---

# Showing the human how we're doing

The human also loses the thread. Rule: **if they have not answered, they have not read anything.** Showing them something does not count as read; only acting does (signing, answering, confirming, or dismissing). Their pending items pile up in `kontext inbox` until they act.

## When to show

- At every wave boundary (skill `kontext-orchestrate`), before asking for the go-ahead for the next one.
- When they ask "how are we doing?", "where did we leave off?", or something like that.
- When they come back after days: the start hook tells you ("omar has 4 pending items…"). Tell them in your first reply.
- When a problem or a question of theirs is blocking work (`kontext inbox` puts it at the top with "blocks …").

**When not to:** after every task. Finished tasks are not listed to them; the report's "Your ideas" section shows where each of their ideas went, done ones included.

## How, depending on where you run

1. First `kontext tree`, pasted as it is in a code block: the closed epics under "Done", the waves still to go (the same as `kontext plan`), the open epics and their features, what waits for them, and their ideas that are not placed yet.
2. Then the status paragraph (below), logged with `kontext log --status "…"`.
3. Then, with the first option you have:

| If you have | Do |
|---|---|
| T3 Code (`html_preview`, `html_render`; search for them with ToolSearch if they are deferred) | `kontext report --html -o /tmp/kontext-report.html`, and show it with `html_preview`, then `html_render` with that HTML |
| Claude Code with an Artifact tool | A private Artifact with the HTML from `kontext report --html` |
| Claude Code without an Artifact tool | `kontext ui` and give them the path `http://127.0.0.1:4700/report` (the tree is at `/`) |
| Terminal only | Paste the status paragraph and the output of `kontext inbox` (or `kontext report`, which is the same in text) |

The report is fixed: do not redesign it or add sections to it. The only part that is yours is the paragraph.

## The status paragraph

2 or 3 sentences, in this order: what changed that matters to them, what is stuck, and what you need from them. No list of finished things, no ids.

```sh
kontext log --status "Card payments with 3D Secure now work end to end in the test environment. Notifications are stuck because we don't know whether to send SMS in addition to email. I need you to sign the refunds goal and answer the SMS question."
```

## Screenshots

Every task with an interface closes with a screenshot of what it did, in addition to its test:

```sh
kontext done <T> --run "<test>" --since <mark> --evidence payment.png --screen payment
```

- Use the same `--screen` for the same screen: the report puts the earlier one beside it ("before and after").
- Limit per image: 2 MB (`[media] max_size`). Crop to what matters; use JPG or WebP if it is too heavy.
- How to take it: with a headless browser (`npx playwright screenshot --viewport-size=1280,800 <url> payment.png`, or `chromium --headless --screenshot=payment.png --window-size=1280,800 <url>`); in T3 Code, with the preview tools; in a simulator or emulator, with the harness's device tools. If there is no way to take it, say so in the task's `kontext log`; do not make up the screenshot.

## Their pending items

- Before asking them something new, check how many pending items they have (the hook told you at the start; use `kontext inbox count` if a lot of time has passed since then). If they already have several unanswered, **do not pile on more**: move on with whatever does not depend on them, and bundle the rest for when they answer.
- `kontext ask` stops you by itself: if they already have `[inbox] max_pending` pending items (5), your question is marked as not urgent. Use `--blocking` only if it really stops your work.
- If the same question is already open, `kontext ask` joins that one instead of repeating it. Do not reword it to get around this.
- You do not close their pending items: `kontext inbox sign|answer|dismiss|confirm` are the human's.
- If the project has several owners (or there are questions for someone who is not an owner) and you are asked to see everyone's items: `kontext inbox --everyone`, one section per person with their own items and one shared section with what any owner can resolve, plus the problems, shown once; `kontext inbox count --everyone`, one line per person. It is for viewing only: each person acts from their own `kontext inbox`.
