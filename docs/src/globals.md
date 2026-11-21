# Execute globals

These are the only globals inside `execute`. There is no fs, require, process,
fetch, import or timers.

| Global | What it is |
|---|---|
| `t3` | Every T3 tool by exact name (`t3.t3_thread_list(input)`), an alias without the `t3_` prefix, and `t3.call(name, input)`. A failed call throws an `Error` with T3's message. |
| `<ns>` | One per enabled upstream, named by its `name`, present even before its tool list is known: each allowed tool by exact name (`cbm.search_graph(input)`, `cbm["get-thing"](input)`), a sanitized alias when it is free (`cbm.get_thing`), and `cbm.call(name, input)`. A tool named `call` is reached with `cbm.call("call", input)`. |
| `tools` | `tools.call("ns.tool", input)` for any namespace, `t3` included (which keeps the `t3_` alias and the deny list). The name splits at the first dot, so `tools.call("cbm.a.b")` calls tool `a.b`. |
| `mem` | `get(key)`, `set(key, value)`, `del(key)`, `list(prefix?)` on the session namespace; `mem.ns(name)` opens a shared one (for example `work:widgets#41`). JSON values, 1 MB each. |
| `vars` | A plain object restored before the run and saved after it (also after a thrown error; a CPU or wall kill loses that run's changes). REPL state across calls and restarts. |
| `wf` | The workflow layer, acting as the calling thread. |
| `console` | `log`/`info`/`debug`/`warn`/`error`, captured to `logs` (50 lines / 4,000 chars). |

The full signatures are on the [API reference](api.md) page.

## t3

```js
async () => {
  const { threads } = await t3.thread_list({ allProjects: true, limit: 20 });
  return threads.filter(t => t.status === "running").map(t => t.link);
}
```

`t3.thread_list` and `t3.t3_thread_list` are the same tool. A tool on the
config's `deny` list throws without reaching T3.

## Upstream namespaces

Each enabled upstream adds one frozen global named after it. With the `cbm`
upstream from the [Upstreams](upstreams.md) example:

```js
async () => {
  try {
    return await cbm.list_projects({});
  } catch (e) {
    return `cbm failed: ${e.message}`;
  }
}
```

`tools.call("cbm.list_projects", {})` reaches the same tool by its qualified
name. Use `search` to find an upstream's tool names; `catalog` is not a global
inside `execute`.

## mem and vars

`mem` is a key-value store on the session namespace. The session defaults to
`thread:<caller>`, so each thread gets its own, and `mem.ns(name)` opens a
namespace any thread can share:

```js
async () => {
  await mem.ns("work:widgets#41").set("handoff:pr1", "PR 1 merged at d4db97c");
  return await mem.ns("work:widgets#41").get("handoff:pr1");
}
```

`vars` is a plain object. Whatever a run leaves in it is there at the start of
the next run in the same session, across service restarts:

```js
async () => {
  vars.runs = (vars.runs ?? 0) + 1;
  return vars.runs;
}
```

## wf

`wf` is the thread-workflow layer. Threads form a tree of links, each between a
parent and a child, and the service keeps every link and message in its store.

- `wf.init({role, label?, root?})` starts a thread with no parent link. A role
  that `roles.overseer.children` lists, which is only `ticket` by default, joins
  the board the way `wf.join({ parent: "board", label, role: "ticket" })` does,
  under the board launch's label, `label`, or its T3 title, in that order. The
  result names `parent: "board"` and the label. `root: true` keeps the thread
  off the board as a standalone root. Every other role starts as a root.
  `wf.init` is for a ticket started on its own, but a ticket launched from the
  board that calls it instead of its brief's `wf.join` still clears its launch
  from the board's pending list. The service matches the launch by the thread
  T3 created for it, never by label, so a ticket started by hand under the
  same title leaves another launch on the pending list.
- `wf.openChild({role, label})` returns a `brief` block the parent pastes into
  the child's launch brief; the child runs the
  `wf.join({parent, label, role, down, miss, stall})` call it contains. When
  the parent launches the child with `t3.t3_thread_launch` in `execute`, the
  service records the new thread on its launch in the pending list, and the
  watcher's never-joined report links to it.
- `wf.send({kind, body, gate?, wake?, options?})`, kinds `note | event | gate |
  blocker | report | handoff | closed`, to the caller's parent. Returns
  `{handle, delivery}`: `noted`, `sent`, `queued` (the parent is mid-turn, or
  delivery failed; the watcher retries), or `board` when the parent is the
  board. When the message opened a decision card, the result also carries the
  `card` id and the card's reply buttons as `options`, plus a `warning` when a
  gate's card has no buttons. `gate` also opens a gate, which stops nudges
  until the sender clears it. `handoff` and `closed` close the link.
- `options`, a list of answers on a `gate`, `blocker` or `report`, sets the
  board card's buttons in place of the ones parsed from the body. The limits
  and the parsing rules are in [Reply buttons](board.md#reply-buttons). A bad
  entry fails the send. A thread parent gets only the body.
- `wf.reply`, `wf.gate`, `wf.close`, `wf.inbox`, `wf.show`, `wf.board`,
  `wf.adopt` and `wf.sweep` cover the rest of a link's life.
- `wf.launchTicket`, `wf.cards`, `wf.answer` and `wf.feed` work with the
  [board](board.md).
- `wf.issue({ url, title?, note? })` reports a GitHub issue the calling thread
  just filed. The board lists it under [new issues](board.md#new-issues) with
  the filing thread and the time. The call needs the caller's thread id but no
  parent link, so any thread can make it. It returns `{ added, message, issue }`.
  When the issue is already on the list, dismissed or not, or already has a
  board ticket, the call adds nothing. Its result then has `added: false`, a
  `reason` of `reported` or `ticket`, the existing `issue` or `ticket`, and a
  `message` that says which.
- `wf.pick({ pool })` picks a member of a configured [pool](config.md#pools)
  for one launch and returns `{ pool, pick, instanceId, driver, model, effort,
  options, reason, skipped, modelSelection }`. Pass `modelSelection` to
  `t3_thread_launch`. `reason` names the driver, model and effort chosen and
  says why that member, and `skipped` lists each member left out with
  its reason, such as `limited until 14:00 MST`. Under the `cheapest`
  strategy, `reason` names the cost, as in
  `claudeAgent claude-opus-5-5, effort high; cheapest: cost 0, weighted: weight 1 of 2`.
  The service records each pick, and `pick` is its id. When no member can
  run, it throws and names the pool.
- `wf.picked({ pick, thread })` records the thread a pick launched. The board
  offers to move a thread to another member when it stops on a usage limit,
  as [Moving a limited thread](config.md#moving-a-limited-thread) describes,
  and only for a thread with a recorded pick. Board launches record theirs on
  their own. It throws when the pick is unknown or already launched another
  thread.
- `wf.pools()` lists each pool, with `mixed` and the pool's `driver`, `model`
  and `effort` (`null` in a mixed pool), and its members with their `driver`,
  `model`, `effort`, `weight`, `reserve`, `cost`, `state` (`ready`, `limited`,
  `disabled`, `constrained`, `missing-model`, `missing-effort`, or `unknown`
  when T3 did not answer), `reason`, `limitedUntil`, `limitedUntilLocal`,
  `limitReason`, `usageLeft`, `running` threads, `picks` and `lastPick`.
  `limitReason` names the thread whose limit stop set the mark and where its
  reset time came from. `usageLeft` stays `null` until t3cm has a usage
  source. The watcher sets and clears the marks, as
  [Limits](config.md#limits) describes. Each pool also has `warnings`, which
  flags a member that is the only one that can run the pool's model, or in a
  mixed pool the only one of its driver that can run its model, as
  [Sole members](config.md#sole-members) describes.
- `wf.status()` returns the service's state as the board header shows it:
  `version`, `t3`, `tokenExpiresAt`, `watcher`, `openLinks`, `update` and
  `skills`, the check of the installed T3 workflow skills and setup steps
  described in [Install the skills with t3cm](getting-started.md#install-the-skills-with-t3cm).
  It needs no caller and reads the skills fresh.
- `wf.slot(name, { repo?, instance?, skills?, files? })` returns one
  [slot](getting-started.md#slots), `review`, `passes`, `delivery`, `models` or
  `credentials`, resolved for the caller's T3 instance and project: its
  `source` layer, each skill with `how` to run it (`Skill tool`, or
  `inline: read <path> and apply it`, or why it is skipped), a one-line `line`,
  and `warnings`. For `delivery`, `files` filters steps with `paths` globs
  against changed repo-relative paths and returns unmatched steps in `skipped`.
  For an Antigravity thread, which has no Skill tool, every
  skill found on disk is `inline`. For `models`, each role carries a
  `modelSelection` for `t3_thread_launch`, or the pool to pass to `wf.pick`. `repo` overrides the
  project's workspace root, for example with a worktree.
- `wf.renewal({ output, repo? })` matches a failed command's output against
  the `credentials` patterns and returns `{ match, skill, how, skipped }`, or
  `null` when none matches.
- `wf.slots({ repo?, instance? })` returns every slot at once, as
  `t3cm slots --json` prints it, with the caller `context` and `warnings`.
- `wf.waiting.add`, `wf.waiting.resolve` and `wf.waiting.list` keep an overseer
  thread's waiting list.
- `wf.merge({repo, pr, head, method?})` merges a GitHub pull request at the
  approved head commit. `repo` is `owner/name`, `head` the full 40-character
  SHA the user approved, and `method` one of `merge`, `squash` or `rebase`.
  See [wf.merge](#wfmerge).

```js
async () => wf.send({ kind: "note", body: "tests green at d4db97c" })
```

The service stores a note without waking anyone, and the note reaches the
parent as a digest line on its next wake. The kinds in the config's `wake` list
wake the receiver. `wake: true` on a send forces a wake for any kind, notes
included, and `wake: false` suppresses one, except that `handoff` and `closed`
always wake a thread parent. A send to the board ignores `wake`, since the board
turns messages into cards and feed entries. A wake is one compact
`t3_thread_send` (mode `queue`) of the form
`[t3cm <kind> from=<label> msg=<handle>]`, with the body capped at `bodyCap`.
`wf.show(handle)` returns the rest.

Wakes to a busy thread wait in an outbox and go out as one combined message
when it is idle, held at most `maxHoldMinutes` (30). A watcher task runs every
`watcherIntervalSecs` (60 s): it nudges idle children, reports stalls and
pending requests once, escalates after repeated misses, closes links whose
thread was deleted or archived, and marks [pool](config.md#limits) members
limited when one of their threads stops on a usage limit.

```js
async () => {
  const p = await wf.pick({ pool: "opus" });
  const r = await t3.thread_launch({ projectId, title, message, modelSelection: p.modelSelection });
  return wf.picked({ pick: p.pick, thread: r.threadId });
}
```

### wf.merge

`wf.merge` reads the pull request in one GraphQL query, then does one of these.
GitHub computes the merge state lazily, so while an open pull request reports
`UNKNOWN`, the query runs again, at most three times, one second apart.

| The pull request | What `wf.merge` does | Returns |
|---|---|---|
| Is at a head other than `head` | Nothing | `refused` |
| Is a draft or closed | Nothing | `refused` |
| Is already merged | Nothing | `merged` with its merge commit |
| Is already in the merge queue | Nothing | `queued` with its position and `approval` id |
| Has a merge queue and is mergeable | `enqueuePullRequest` with `expectedHeadOid = head` | `queued` with its position and `approval` id |
| Has a merge queue and is not mergeable yet | Records it as waiting to be queued | `waiting` with its `mergeState` and `approval` id |
| Has no merge queue and is mergeable | `mergePullRequest` with `method`, else the repo's default, and `expectedHeadOid = head` | `merged` with the commit |
| Has no merge queue and is not mergeable | Nothing | `refused` |
| Has a check on its head that failed | Nothing | `refused`, naming the check |

Mergeable means a `mergeStateStatus` of `CLEAN` or `HAS_HOOKS`, or `UNSTABLE`
when every check on the head that did not pass ended `CANCELLED`, `SKIPPED` or
`NEUTRAL`. GitHub's own merge button accepts any `UNSTABLE` pull request, but
`UNSTABLE` also covers a failed optional check, so `wf.merge` reads the
checks. A check that ended `FAILURE`, `ERROR`, `TIMED_OUT`, `ACTION_REQUIRED`,
`STALE` or any other outcome makes it refuse, and a check still running makes
the pull request not mergeable yet. [ADR 0003](https://github.com/OttoOps/t3-codemode/blob/main/docs/adr/0003-unstable-merge-state.md)
has the reasoning. `wf.merge` never turns on
auto-merge: on a merge-queue repo, auto-merge set on an already mergeable pull
request never joins the queue. Every result carries `status`, `repo`, `pr` and
`head`; `refused` adds a `reason`. When `gh` cannot reach GitHub at all, the
call throws.

A waiting record holds the repo, the pull request, the approved head, who
asked (`thread` and `label`), and the merge state it saw. A `queued` result
keeps the record too, marked as sent, so the board follows the pull request
until it merges or leaves the queue. The board's
[GitHub reconcile](board.md#github-reconcile) queues a waiting pull request
once GitHub shows it mergeable at the approved head, and opens a new merge gate
if the head moves first. A merge, here or on GitHub, drops the record. A
repeat call replaces the record, and the board acts only on the latest one.

The service runs `gh api graphql` with the `gh` login of the user it runs as,
so `gh` must be on the service's `PATH` and signed in. `execute` code never
reaches `gh` or the network itself; see [Security](security.md#github).

```js
async () => wf.merge({ repo: "acme/widgets", pr: 81, head: "d4db97c0e1f2a3b4c5d6e7f8091a2b3c4d5e6f70" })
```

## console

```js
async () => {
  console.log("starting");
  console.warn("this goes to logs too");
  return "done";
}
```

returns `{"result": "done", "logs": [...]}` with both lines.
