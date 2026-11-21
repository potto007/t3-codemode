# The board

The service is the root of the link tree, in place of an LLM overseer thread. A
fixed virtual node, `board`, always exists, and ticket threads join it with
`wf.join({ parent: "board", label, role: "ticket" })`, or with
`wf.init({ role: "ticket" })` when a thread starts a ticket on its own. No
overseer thread runs, so no ticket's messages land in any model's context until
someone asks.

The page is `http://127.0.0.1:47731/` on the default port; use the port you
gave `--port` or `T3CM_PORT` if you changed it. It is one embedded HTML file with no
external requests, in light and dark, and it raises a browser notification for
each new card while the tab is hidden.

## What the page shows

The page has three panels under a header that counts the cards waiting on you
and the threads running. Only the lists scroll.

- **Needs you.** Gates, blockers, reports and the watcher's escalations each
  become a card, blockers first. A card opens with the ask itself ("Merge
  #64"), then the ticket, its issue title and how long the card has waited.
  Its answers are the filled buttons, as [Reply buttons](#reply-buttons)
  describes, followed by a link to the PR or issue the message names and the
  quieter Reply, Dismiss and full-message controls. The ticket label jumps to
  the ticket's row.
  Answering a card sends the ticket one reply and resolves the card. A ticket
  that clears its gate, or sends a newer `gate` message, resolves its older
  gate cards, and its handoff resolves all of its open cards.
- **New issues,** at the top of the Tickets column: the issues threads have
  filed and reported with `wf.issue` that have no ticket yet, newest first.
  Each row shows the issue's `repo#n` linked to GitHub, its title, how long ago
  it was filed and a link to the thread that filed it, with Launch and Dismiss.
  A count beside the heading says how many wait, and the section is gone
  while none do. Show dismissed, beside the heading when any are dismissed,
  swaps the list for the dismissed issues, each with Restore. [New issues](#new-issues) covers what each button
  does.
- **Tickets,** in Open, Done and Settled tabs, grouped by state. A ticket
  takes the most urgent state of its own thread and its workers, so a ticket
  whose worker runs reads running:

  | Mark | State |
  |---|---|
  | filled diamond | needs you: it has an open card |
  | cross | failed, or blocked |
  | half-filled ring | running |
  | dashed ring | waiting |
  | hollow ring | idle |
  | check | done |

  Amber marks only the tickets with an open card. A row shows the ticket label,
  its issue title (or its gate in words when there is no title), and, for a
  ticket that needs you, a `Reply "merge"` link to its card. Other rows show
  the last message, and a gate already answered reads "Answered: ...", or "Dismissed: ..." when you dismissed its card. The
  workers sit at the end of the row, with finished ones folded into a count
  that expands.
- **Activity.** Notes and events from tickets, the latest `board.feed` (50)
  of them, under the ticket and worker that sent them. Runs from the same
  sender fold into one row that expands. The board drops kind prefixes and
  `Publishing:` tails, and shortens hashes to 7 characters and GitHub links to
  `#65`. When a launch picks from a pool in `models.ticket` or `board.pool`,
  each launch adds an entry that names the instance it went to and why.
- **Launch and Ask,** pinned above Activity. Launch takes GitHub issue URLs and
  makes each one a ticket thread. A question in Ask starts a short-lived Haiku
  thread that reads the board, writes its answer under Ask, and then gets
  archived. With `board.pool` set, the thread runs on a pool member with the
  pool's model, or in a mixed pool the member's own.

Times show as ages (`36m`, `1h 32m`); hover one for the local time.

A `handoff` or `closed` message marks a ticket done.

## Updates

When a newer t3cm release is out, the header shows **Update to vX.Y.Z**. It
opens a panel with the `t3cm update` command, a button that copies it, and a
link to the release notes. **Hide until the next release** hides the button
until a later release comes out; the board remembers that per browser. After
the service restarts on a new version, the page reconnects by itself. Once the
service runs a newer version than the loaded page, the button reads **Reload
for vX.Y.Z**, and a click loads the new board.

When the T3 workflow skills are behind this release, edited, installed by
`npx skills`, or a setup step is missing, the header shows a warning button
such as **Skills out of date** or **Setup step missing**. Its panel says what
drifted and lists each command that fixes it, such as `t3cm skills update` or
`run setup-t3-workflow: new slot models`, each with a copy button. The button
goes away on the next check after the fix. A machine with no skills installed
shows nothing.

## Reply buttons

A `gate`, `blocker` or `report` sent with `options` gets exactly those
buttons, and the board does not read the body for them:

```js
async () => wf.send({
  kind: "gate", gate: "merge",
  body: '**ACTION NEEDED:** reply "merge" to merge #81, or "hold" to wait, here: https://github.com/acme/widgets/pull/81',
  options: ["merge", "hold"],
})
```

Each option must be a single line of 1 to 60 characters once trimmed, no two
may match ignoring case, and a send carries at most 8. A send that breaks any
of these fails, names each bad entry by its index, and leaves no card. A
`note`, `event`, `handoff` or `closed` send with `options` fails too. A send
with an empty list gets a card with no buttons.

Without `options`, the board reads the buttons from the body.

When a message has an `**ACTION NEEDED:**` line, its buttons come from that
line alone, or from each such line when it has several. A quoted phrase
elsewhere in the message never becomes a button. On an action line, each quoted
answer after the first `reply`, `answer` or `say` becomes a button. The first
line below gives two buttons and the second gives three:

```text
**ACTION NEEDED:** reply "merge and deploy" to merge #81, or "merge" alone, here: https://github.com/acme/widgets/pull/81
**ACTION NEEDED:** reply how to track it: "advisory", "issue", or "nothing", here: https://github.com/acme/widgets/pull/37
```

Double quotes count whether they are straight or curly. Single quotes count
only when they open after a space or punctuation mark and close before one or
at the end of the line. That rule keeps the apostrophe in `user's` from
starting or ending an answer.

A message with no action line gets one button per `reply "X"` phrase anywhere
in it.

The board trims each answer and drops one that runs past 60 characters or onto
a second line. Answers that differ only in case share one button, and a card
shows at most 8.

A send that opens a card returns that card's buttons as `options`, so the
sender sees what the user will see. A gate whose card has no buttons also
returns a `warning`: the user can still type an answer, but the sender should
send it again with `options`, or quote each answer after `reply` on its action
line.

### Delivery gates and chains

The board keeps no delivery wording of its own. `t3-ticket` reads ordered
stages from `wf.slot("delivery", { files })` and passes explicit `options` on
each `wf.send({ kind: "gate", gate, body, options })` call. The builtin merge
stage keeps `gate: "merge"` so GitHub reconcile tracks the PR, holds approval
to `head <sha>`, and clears the merge gate once the PR lands. Every later
gated stage uses `gate: "delivery:<id>"`, such as `gate: "delivery:deploy"` or
`gate: "delivery:publish"`.

When a gated stage is followed by a consecutive run of `gate: "chain"` stages,
`t3-ticket` passes one button per composable prefix, longest first, within the
board's 8-button and 60-character limits. For a pipeline with `merge`
(`always`), `deploy` (`chain`), and `verify` (`chain`), the merge gate sends:

```js
async () => wf.send({
  kind: "gate",
  gate: "merge",
  body: '**ACTION NEEDED:** reply "merge, deploy and verify", "merge and deploy", or "merge" to merge #81 at head d4db97c, here: https://github.com/acme/widgets/pull/81',
  options: ["merge, deploy and verify", "merge and deploy", "merge"],
})
```

Picking a shorter prefix runs that prefix and opens the next stage's own
`gate: "delivery:<id>"` card when reached, such as
`options: ["deploy and verify", "deploy", "skip deploy"]`. Stages with
`gate: "never"` run automatically once their predecessor succeeds. Stages with
`scope: "post-handoff"` run outside the ticket. Under the default
`closeAfter: "ticket"`, the ticket posts a note on the issue and includes the
remaining checklist in a final `report` and its `handoff` body without keeping
the ticket open. Under `closeAfter: "all"`, each remaining `post-handoff`
stage opens a `delivery:<id>` gate before the ticket closes.
[ADR 0007](https://github.com/OttoOps/t3-codemode/blob/main/docs/adr/0007-composable-delivery.md)
records the composable delivery design.

## Filtering the page

The bar under the header narrows the page to the work you pick. Each filter
applies to the decision cards, the ticket tree, the activity feed and the
Settled section at once.

- **Project, Status and Card kind** are multi-select dropdowns. Type in one to
  narrow its list, and Enter toggles the first value left. A project shows as
  the ticket's `owner/repo`, or as its T3 project's title while the repo is
  not known. The filter keeps the T3 project id, so a pick still holds once
  the repo arrives, and
  [ADR 0004](https://github.com/OttoOps/t3-codemode/blob/main/docs/adr/0004-context-projects-by-id.md)
  has the reasoning. Status is running, needs you, blocked or done. Card
  kind narrows only the cards.
- **Search** narrows the page while you type, with no Enter. Every word must
  appear somewhere in the ticket label, the issue title or number, the repo, or
  the card or feed text. `Esc` clears it, and `f` focuses it when single-key
  shortcuts are on.

While a filter is on, each count reads visible/total. A new card the filter
hides shows up as "N hidden", and clicking it clears the filter.

### Contexts

A context is a named filter: a set of projects, statuses and card kinds, plus a
search if you want one. The service stores contexts, so every browser that
opens the board sees the same ones, and a change made in one browser reaches the
others as it happens.

The **Context** menu at the start of the filter bar switches between them.
Picking one applies its filter. **All** clears every filter, and so do **Clear
filters** and the "N hidden" button. The same menu edits contexts:

- **Save filter as a new context** names the filter you have on.
- **Save filter to** a context replaces its filter with the one you have on. A
  context whose filter you changed reads "(edited)" until you save or switch.
- **Rename** and **Delete** act on the context you are in. Delete asks for a
  second click.

Each browser remembers its own context and comes back to it after a reload. If
that context was deleted, the page falls back to All. A context changed in
another browser applies here too.

While a context is on, a browser notification goes out only for a new card
inside it. A card outside it still counts toward "N hidden".

## From a thread

| Call | Does |
|---|---|
| `wf.launchTicket({ issue, project?, title?, model?, effort? })` | Launch a ticket thread for a GitHub issue |
| `wf.cards({ all?, context?, projects? })` | The open cards, oldest first, each with its `repo` and `project`; `all` includes resolved ones |
| `wf.answer(id, text)` | Answer a card for the user, the same as a button on the page |
| `wf.feed({ limit?, context?, projects? })` | The latest notes and events, each with its `ticket`, `ticketLabel`, `ticketTitle`, `ticketStatus`, `repo` and `project` |
| `wf.board({ parent: "board", all: true })` | Every ticket on the board, with live status |
| `wf.issue({ url, title?, note? })` | Put an issue this thread just filed on the [new issues](#new-issues) list |

```js
async () => (await wf.cards()).map(c => `${c.id} ${c.label}: ${c.options.join(" | ")}`)
```

`context` and `projects` narrow `wf.cards` and `wf.feed` to the same projects
the page would show. `context` takes a saved [context](#contexts)'s id or its
name, and uses only its projects. Its statuses, card kinds and search stay on
the page. `projects` takes a list of T3 project ids. A repo or title value
from an older context works there too. Both follow the page's rule from
[ADR 0004](https://github.com/OttoOps/t3-codemode/blob/main/docs/adr/0004-context-projects-by-id.md),
so a card or row matches by its project id, by the repo or title the board
knows for that id, or, with no project id, by a repo the board ties to the
picked id. A card or row with no project and no repo matches nothing. An
empty list, or a context with no projects, keeps everything. Giving both, or
a context that does not exist, throws. With a filter, `wf.feed` scans the
latest 200 rows and returns up to `limit` of the ones that match.

```js
async () => (await wf.cards({ context: "Widget work" })).map(c => `${c.id} ${c.repo}: ${c.text}`)
```

`wf.launchTicket` finds the T3 project whose workspace `origin` remote is the
issue's GitHub repo, unless `project` names one, resolves `models.ticket` for
that project through the service's slot resolver, and launches a thread that
works the issue and reports to the board. Precedence for the launch model is:

1. `model` or `effort` passed to `wf.launchTicket`, reported as `modelSource: "argument"`.
2. The repository's `.t3-workflow.json` or `AGENTS.md`, reported as `".t3-workflow.json"` or `"AGENTS.md"`.
3. The user config's `models.ticket`, reported as `"user"`.
4. Legacy `board.instanceId`, `ticketModel`, `ticketEffort` and `pool` in
   t3cm's config, reported as `"board"`.
5. The built-in default, reported as `"default"`.

The result always carries `instanceId`, `model`, `effort`, which is `null` when
none applies, and `modelSource`. When the resolved choice is a
[pool](config.md#pools) in `models.ticket` or legacy `board.pool`, the launch
picks a provider instance through `wf.pick`, adds `pool` and `reason` to the
result, and logs a feed entry with `instanceId` and `reason`. Every launch
checks its choice against T3's `orchestrator_capabilities`. An unmapped repo, a
missing or disabled instance, an unlisted model, an unsupported effort, an
unknown or exhausted pool, or a failed launch throws and leaves an error card,
never falling back to the built-in default. The launched thread is told to use
the `t3-ticket` skill, which lives in this repo's `skills/` folder and is
installed as an agent skill, apart from the service.
[The workflow skills](getting-started.md#the-workflow-skills) covers installing
them. Without the skills installed, launched threads do not know the workflow.

## New issues

A thread that files a GitHub issue reports it with `wf.issue`, and the board
keeps it on a list of new issues until someone acts on it. Threads know to
report because `setup-t3-workflow` installs a short rule that Claude Code and
Codex load in every session, so it reaches threads that run none of the
[workflow skills](getting-started.md#the-workflow-skills). Each entry records
the issue's URL, `repo` and `number`, and the `title` and `note` the thread
passed. It also records the filing `thread`, with its T3 `threadLink` and
`threadTitle`, and `at`, the time the thread reported it. An entry's id looks
like `n3`, and the list survives restarts. If T3 can't give the thread's link
and title when the issue is reported, the entry keeps them empty. The next
report of that issue fills them in.

An issue goes on the list once. Reporting it again, from any thread, changes
nothing. Neither does reporting an issue that already has a board ticket, open,
done or still waiting to join. The board matches tickets by their `repo#n`
label, ignoring case. The label leaves out the owner, so an issue in another
owner's repo of the same name counts as having that ticket.

- **Launch** runs `wf.launchTicket` for the entry's issue, with its title as
  the ticket's title. When the launch succeeds, the entry leaves the list and
  the ticket shows under Tickets. A launch that fails leaves the entry in
  place and an error card, as any failed launch does. Launching the same issue
  from the Launch tickets box also takes its entry off the list.
- **Dismiss** takes an entry off the list without touching GitHub. Dismissed
  entries stay in the store, and **Restore** puts one back.

The page gets a reported issue within seconds, on the same event stream as
everything else. While the pointer or focus is in the Tickets column, a new
entry waits behind an "N new" button in the section's heading, and an entry
that leaves stays as a placeholder ("widgets#43 launched") until you move
away, so nothing under your pointer shifts.

While no entry waits, nothing shows above Tickets, not even the heading. When
dismissed entries exist then, the Settled tab carries a Dismissed issues
button that opens the section on the dismissed list, and Show new closes it
again.

A new-issue row or ticket row that arrives on the event stream gets a green
tint that fades on its own over about four seconds. An entry held behind "N
new" gets it when you reveal it. Rows already there when the page loads, and
rows a filter change brings back, don't flash. With reduced motion set, the
tint holds still for a moment and then goes, with no fade.

## GitHub reconcile

The board checks what it believes against GitHub on a timer, so a pull request
merged, closed or dropped from a merge queue outside T3 still reaches the board
and its ticket. It tracks:

- each pull request of an open worker link,
- each pull request an open card names, read once when the card opens: from
  its action line, or else from its whole message, and only when it names
  exactly one,
- each pull request `wf.merge` saved as waiting to be queued, or sent to
  GitHub,
- each pull request it last saw in a merge queue, until 30 minutes after it
  leaves, so a removal that GitHub records late still gets its report.

It reads them with `gh` as the user the service runs as, in one GraphQL query
for every 100 pull requests it tracks, and makes no GitHub call when it tracks
nothing. A check runs every `board.reconcileActiveMinutes`, 2 by default, while
a tracked pull request is queued or waiting, and every
`board.reconcileIdleMinutes`, 10 by default, otherwise. Loading the page runs
one too, at most once every 30 seconds.
`docs/adr/0002-github-reconcile.md` in the repository has the reasoning.

Each check compares what GitHub shows now with the board's state now, and
makes them agree:

| GitHub shows | The board |
|---|---|
| Merged | Resolves the pull request's cards opened before the merge, with the note "merged on GitHub at `<sha>`, `<time>`". Clears the ticket's `merge` gate when no other gate card stays open for it (later delivery stages use `gate: "delivery:<id>"`, so reconcile leaves those untouched). Sends each ticket tracking the pull request one `event` to run its post-merge steps |
| Closed without merging | Resolves its cards opened before the close and sends each ticket one `event`. Reopening and closing it again sends another |
| Dropped from the merge queue for `merge_conflict`, `failed_checks` or `manual` removal | Opens a `report` card per ticket that gives the reason, with `requeue` and `hold` buttons, or one card with no buttons and the steps to take by hand when no ticket tracks it. An approval older than the removal is dropped, so the board never queues the pull request again on its own |
| Waiting to be queued, now mergeable at the approved head | Sends `enqueuePullRequest` with the approved head as `expectedHeadOid`. If the repository has turned its merge queue off since, sends `mergePullRequest` the same way, with the approval's method or else the repository's default |
| Waiting to be queued, with a check at the approved head that failed | Holds the approval and opens one `report` card per approval that names the failed checks, with a `fix` button. Queues the pull request and resolves the card once the check is re-run and passes |
| Approved at one head, now at another | Opens one new card per ticket: a `merge` gate for the new head, or a `report` when GitHub already has the new head in its queue. Resolves the old gate and drops the old approval |

A gate names its head with the word `head` and the SHA, such as
`head d4db97c`. The board holds a gate that names none to the head the first
check sees, so a push before that check goes unnoticed.

The board records each wake and each removal report per ticket, under the
merge commit, the close time or the removal time, so a later check never
repeats it. It saves a wake and its delivery in one transaction, so a T3 outage
delays a wake but never loses it. An event from before the board started tracking the pull
request, or from before this version of the service first started, stays
quiet.

When GitHub refuses the enqueue or the merge, or cannot find the pull request,
the approval stays for the next check. After 3 failures in a row the board
opens a `report` card with GitHub's last message and drops the approval. A pull
request sent to the queue that leaves it with no removal GitHub recorded gets
the same 3 checks. The board never sends an approval whose number GitHub now
resolves to another pull request, such as after a repository rename. It drops
that approval and opens a `report` card. Every card the reconcile raises for an
approval carries that `approval` id, so `wf.cards()` callers can match it to
the `wf.merge` call that recorded it. Every fix goes on the activity feed
from `github`.

The tree shows a pull request in the merge queue as `queued`, next to `open`,
`merged` and `closed`. When `gh` cannot reach GitHub, the check logs a warning,
changes nothing, and runs again at the next interval. When one pull request
cannot be fixed, for example because the waiting list is full, the check still
fixes the others, and that pull request keeps its approval for the next check.
A read error for one pull request, other than GitHub's `NOT_FOUND`, changes
nothing for it either.

## Settling

The board settles finished tickets and answered asks and never deletes them. A ticket that
has been done, or an ask that has been closed, for `board.settleAfterHours`
leaves the main lists on the next watcher pass. The board also moves a settled
ticket off the thread list in T3 with `t3_thread_organize` and `settle`. That
step is best effort, so a T3 error goes to the log and the board state still
changes.

It works the other way too. A done ticket whose thread you settle or archive in
T3, or whose thread is gone, settles on the board at the next watcher pass. A
ticket you restored on the board ignores a T3 settle older than the restore.

The **Settled** tab keeps them, with a filter and a
**Restore** button on each one. Restoring a ticket sends `unsettle` and
restarts its settle clock. **Settle** on a row and **Settle all done** do it by
hand.

## HTTP routes

| Route | Does |
|---|---|
| `GET /` | The page, with a per-process CSRF token embedded |
| `GET /api/state` | `{cards, tree, feed, asks, settled, contexts, newIssues, dismissedIssues, counts, service}`, with `counts.newIssues`; tickets, cards and feed items carry `repo` (`owner/repo`) and `project` (`{id, title}`), and tickets their thread `title`, with `last.dismissed` when the last message is a gate whose card you dismissed. Times come as local clock strings with an ISO twin for ages: a ticket's `lastUp` and `upAt`, a settled ticket's `settledAt` and `settledIso`, and `service.watcher.lastPass` and `lastPassAt`. `service.update` is `null` until a release check has answered, then `{current, latest, available, notes, command}`. `service.skills` is the skills check that `/healthz` also reports ([getting started](getting-started.md#install-the-skills-with-t3cm)) |
| `GET /api/events` | SSE: a `state` event on every change |
| `GET /api/msg/{handle}` | One stored message in full |
| `POST /api/cards/{id}/answer` `{text}` | Reply to the card's ticket and resolve it |
| `POST /api/cards/{id}/dismiss` | Resolve without messaging |
| `POST /api/launch` `{issues: [...]}` | `wf.launchTicket` for each (at most 20) |
| `POST /api/ask` `{question}` | Ask a scratch thread about the board |
| `POST /api/tickets/{thread}/settle`, `/restore` | Settle a done ticket, or bring a settled one back (400 if open, or not settled) |
| `POST /api/asks/{id}/settle`, `/restore` | The same for a closed ask; `id` is `q<n>` |
| `GET /api/new-issues` | `{issues, dismissed}`, newest first; an issue that has a board ticket is in neither |
| `POST /api/new-issues/{id}/launch` | Launch a ticket for the entry, as `POST /api/launch` does for one issue; the result adds `id` (404 for no such entry) |
| `POST /api/new-issues/{id}/dismiss`, `/restore` | Dismiss the entry, or bring a dismissed one back (400 if already dismissed, or not dismissed) |
| `POST /api/settle-done` | Settle every done ticket and closed ask; returns `{tickets, asks}` counts |
| `GET /api/contexts` | `{contexts: [...]}`, sorted by name; each is `{id, name, spec, createdAt, updatedAt}`, and `spec` is `{projects, statuses, kinds, q?}`. Each project is a T3 project id, or, for an item with no project id, its lowercased `owner/repo` or title. A context saved before project ids holds an `owner/repo` or a project title instead, and an item matches it ignoring case |
| `PUT /api/contexts/{id}` `{name, spec}` | Create the context (201) or replace its name and filter (200). Leave out `spec` to rename an existing context, or `name` to change only its filter; either form needs the context to exist (404 if it does not). `id` is 1 to 64 letters, digits, `-` or `_`. Names are unique ignoring case (409), at most 60 characters, and never "All". Statuses are `running`, `waiting`, `blocked` and `done`, and kinds are the card kinds. The board keeps at most 100 contexts |
| `DELETE /api/contexts/{id}` | Delete the context (404 if there is none) |
| `POST /api/open` `{link}` | Open a T3 thread link in the running Windows desktop app (on Windows, or from WSL); returns `{opened}`, and the page opens a new tab when it is `false` (400 for a link that is not a T3 thread) |

Every board route requires `Host` to be `127.0.0.1:<port>` or
`localhost:<port>` and refuses any other `Origin`. POST, PUT and DELETE also
need the `X-T3CM-CSRF` header, which the page reads from its embedded token.
