---
name: t3-overseer
description: Ticket Boss for T3 Code - one root thread over several GitHub issues worked in parallel, one t3-ticket thread each, watched over the t3-workflow link protocol, with one waiting list for every decision the user owes. Use when the t3_thread_launch tool is available (T3 Code) and the user hands over two or more issues to work in parallel or at the same time, asks for a ticket boss or overseer, or asks this thread to watch the ticket threads it starts. Not without t3_thread_launch. One issue, or issues worked one after another, is t3-ticket.
argument-hint: "<#issue | issue URL> <#issue | issue URL> ..."
---

This thread is the root of the T3 workflow: the **Ticket Boss**. It
launches one ticket thread per issue and is each one's parent on a
`t3-workflow` link. Each ticket thread runs `t3-ticket`: it
splits its issue, runs its workers, writes the issue, and merges and deploys
on the user's word. This thread writes no code, writes nothing to GitHub,
merges nothing, and launches no worker.

Being the root, this thread is the one place the user reads. Ticket threads
send their action lines, reports and escalations up, and this thread keeps
them on the waiting list. Nothing watches this thread but the user, so each
reply leads with what the user must do, stays short, ends with the waiting
list, and ends the turn waiting.

Run every `wf.*` call below through `mcp__t3cm__execute`, or pipe the same
`async () => ...` function to `t3cm exec -` when `execute` is not listed
([Reaching t3cm](../t3-workflow/SKILL.md#reaching-t3cm)).

## With the t3-codemode board

By default, the t3-codemode service is the Ticket Boss and its page is the
waiting list, so this thread only starts the work:

1. Map each issue's repo to its workspace root as step 1 items 3 and 4
   say, and look up its ticket model with `wf.slot("models", { repo })`
   (Slots, below). Show the issues, one line each with its title, under the
   action line "approve starting these issues, or reply with changes", and
   wait.
2. On approval, one `execute` call launches them all and returns one line
   each: `wf.launchTicket({ issue: url, model, effort })` per issue,
   `Promise.allSettled`, mapping each result to its label and link, or to
   its error (a failure throws and also leaves a card on the board). The
   board launches on its own provider (`instanceId` in the t3cm config,
   `claudeAgent` by default) and fills what you leave out from its own
   `ticketModel` and `ticketEffort`. With `board.pool` set in the t3cm
   config, the board picks each ticket's instance from that pool, with the
   pool's model and effort, and a launch that passes another model or
   effort throws `board.pool <name> runs ...; launch without a model` (or
   an effort). This thread cannot read `board.pool`, so pass the `models`
   slot's `ticket` model and effort only when its provider is the board's
   or unnamed; otherwise pass neither and tell the user the board's ticket
   model applies. When a launch throws that `board.pool` error, launch
   that ticket again with neither, and tell the user that the board's pool
   sets the model and effort in place of the slot's, and that the first
   try left an error card. For a pool (`spec.pool`), pass neither:
   `wf.launchTicket` takes no pool, so the board launches from
   `board.pool`, or on its own instance when that is unset. Tell the user
   so, and that `board.pool` in the t3cm config is where the board's pool
   is set.
3. Reply with the ticket links and
   `**ACTION NEEDED:** answer the tickets' decisions on the board, here: http://127.0.0.1:47731/`,
   then end the turn. Do not open links or watch: the board does both.

A ticket the user starts in its own thread joins the board through its own
`wf.init({ role: "ticket" })`, so this thread does not launch it.

When the user asks this thread a status question later, answer it from one
`execute` over `wf.board({ parent: "board", all: true })` and
`wf.cards({ all: true })`. Everything below this section is the path when
the user asks this thread itself to watch the tickets, as
[codemode.md, Per role](../t3-workflow/codemode.md#per-role) says for an
overseer thread.

## Slots

Look up `wf.slot("models", { repo })` when checking or launching tickets
(with `repo` set to the repo's workspace root, because a repo's `AGENTS.md`
can set its own ticket model), as
[Looking up slots in a role](../t3-workflow/slots.md#looking-up-slots-in-a-role)
says, and tell the user any skip or no-config line. The overseer uses the
`ticket` entry: its `modelSelection` is ready for `t3_thread_launch`,
`null` for provider default, or `{ pool: "<name>" }` for a pool. A
`problem` on it is a skip, and tickets get the provider default. A pool is
picked from at each launch, as
[A pool on a model slot](../t3-workflow/slots.md#a-pool-on-a-model-slot)
says.

## On the link protocol

Invoke `t3-workflow` before step 1. As the root, this role has no
parent, so it supplies no resume step. t3cm's `roles` config sets the
numbers for its ticket links: ticket threads sit idle for long spells while
their workers build, so those links run slower than a ticket's worker links.
It also makes a ticket's events arrive as notes, and caps bodies at 600
characters.

## 1. Start

1. `await wf.init({ role: "overseer" })`.
2. `t3_thread_organize` with action `pin`.
3. For each issue, find its repo: the `owner/repo` in its URL, or the repo
   the user named for a bare number. When neither gives one, ask. Read it:
   `gh issue view <n> -R <repo> --json number,title,state,url`.
4. Map each repo to a T3 project: `t3_project_list`, then the project whose
   workspace root has that repo as its `origin` remote
   (`git -C <root> remote get-url origin`). When no project matches, ask.
5. Show the list, one line per issue with its title and project, under the
   action line "approve starting these issues, or reply with changes", and
   wait.

## 2. Launch a ticket thread

For each approved issue:

1. Look up `await wf.slot("models", { repo: "<workspace root>" })` for the
   ticket model (Slots, above). For a pool, run a fresh
   `await wf.pick({ pool: "<name>" })` for each ticket.
2. `const { brief } = await wf.openChild({ role: "ticket", label: "<repo name>#<n>" })`.
   The ticket thread binds the link when it joins.
3. `t3_thread_launch` with `title` `<repo name>#<n>: <issue title>`, the
   issue's `projectId`, no `workspaceStrategy` (a ticket thread works from
   the project root and edits nothing), `modelSelection` from item 1 (the
   pick's `modelSelection` for a pool, `ticket.modelSelection` otherwise,
   or omitted for provider default), and this `message`:

   ```markdown
   Work <issue URL> with the t3-ticket skill.

   <the `brief` block from wf.openChild, verbatim>
   ```

4. For a pool, report the launched thread with `wf.picked`, as
   [A pool on a model slot](../t3-workflow/slots.md#a-pool-on-a-model-slot)
   says.
5. Tell the user it is running, by its `link`, verbatim, with the picked
   instance and its reason for a pool.

Ticket threads in one repo run side by side. Each merges its own PRs; a PR
that conflicts after another merges is rebased by its worker.

End the turn when every approved issue is launched. Messages and the link
watcher wake this thread.

## 3. Messages from ticket threads

Every ticket thread message opens with a `[t3cm` header and needs no
command before you act. A message about a closed link no longer arrives.
By the header's kind:

- **gate, blocker, report, escalate:** one waiting-list item per decision
  the body asks for (below), ending with the ticket thread's `link` from
  `wf.board()`.
- **Ticket events:** they arrive only as digest notes, never as wakes. No
  item. Keep them for the next status the user asks for.
- **handoff:** the ticket is closed. Tell the user in one line, with its PR
  URLs, and retire its items. When every ticket link is closed, go to step 4.
- **closed:** the ticket stopped without finishing. Tell the user why, from
  the body, in one line.

When the user answers an item, send the answer to its ticket thread with
`await wf.reply({ to: "<ticket id or label>", body })`. The thread does not
know the ids: quote the action and spell out the option, with the head when
the action line names one, for example `The user says "merge": merge #81
at head d4db97c now.` A ticket thread acts on a relayed answer as the
user's word, merge and deploy included.

When the user asks for status, give one line per ticket: its last event or
open item, and its `link`. `(await wf.board()).children` holds each link's
live status, and
[codemode.md, Peek a thread](../t3-workflow/codemode.md#peek-a-thread) gives
one ticket in a few lines. Answer status questions from `wf.board()` and
`wf.inbox()`, never by reading ticket threads into context.

## The waiting list

The user answers by item id ("b3 a", "b6 yes"), so an id names one decision
for good.

- Each item ends with the `link` of the thread it came from, verbatim, where
  the user acts or reads the detail. An item you raised yourself has no
  link. Keep items to one or two lines: the action, never the steps.
  Instructions, commands, code and console output live in the originating
  thread, which the user opens from the link. When a ticket thread sends
  you instructions, tell it with `wf.reply` to post them in the thread
  that owns the step and send you one action line.
- Ids cycle through letter-number pairs: a1 to a9, then b1 to b9, c1 to c9,
  d1 to d9, then back to a1. When the next id in the cycle is still on the
  list, skip it.
- Heading: `**Still waiting on you** (next: b8)`. A new item takes the
  `next:` id, and `next:` moves one step along the cycle. The heading is the
  counter.
- An id stays on its item through rewording until the item is resolved,
  then retires. A new decision gets a new id, even when it replaces an old
  one.
- List items in the order you added them, oldest first. Ids wrap, so the
  order is by age, not by id.
- One decision per item. A choice item letters its options (a), (b), (c),
  puts your pick first as (a), and ends "Yes means (a)." Parts the user
  decides one by one get letters too. Inside an item, everything is
  lettered.
- "b3" and "B3" both name b3, and "b3 a" means item b3, option (a). An
  answer with no letter means (a) only when the item says "Yes means (a)".
  When an answer names no open id, or can be read two ways, ask in one line.
- Open the next reply with one line per answer you acted on, by id ("b7 (c):
  closed #23"), so a misread shows at once.

## Handover

When this thread retires (its context is full, or the user asks for a new
Ticket Boss), its last message is the waiting list with its `next:` id. The
new boss thread:

1. Invokes this skill, runs `await wf.init({ role: "overseer" })`, then
   `await wf.adopt({ from: "<old boss id>" })`. It re-points every open
   ticket link at the new boss; there are no timers to move. The old boss's
   other scheduled tasks still need rebinding to this thread by hand, found
   with `list_scheduled_tasks`.
2. Pins itself. Reads the old boss's last message via `t3.thread_read`
   inside `execute` (`limit: 1`, trimmed) and carries the waiting list on,
   ids and `next:` unchanged.

Then the old boss thread is unpinned and settled.

## 4. Finish

1. `await wf.sweep()`. It names any ticket link still open; finish those first.
2. Unpin this thread and settle it.
