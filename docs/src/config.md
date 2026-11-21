# Configuration

## The config file

The config is optional JSON at `config.json` in the config directory.
`T3CM_CONFIG` points at a different file. Whatever it holds is deep-merged over
the built-in defaults, so a file only needs the keys you change:

| OS | Config and state directory |
|---|---|
| Linux | `$XDG_CONFIG_HOME/t3-codemode/` (default `~/.config/t3-codemode/`) for config, `$XDG_STATE_HOME/t3-codemode/` (default `~/.local/state/t3-codemode/`) for state |
| macOS | `~/Library/Application Support/t3-codemode/` for both |
| Windows | `%LOCALAPPDATA%\t3-codemode\` for both |

The service reads the file at start. Restart it after an edit:

| OS | Restart |
|---|---|
| Linux | `systemctl --user restart t3-codemode` |
| macOS | `launchctl kickstart -k gui/$(id -u)/com.ottoops.t3-codemode` |
| Windows | `Stop-ScheduledTask t3-codemode; Start-ScheduledTask t3-codemode` |

```json
{
  "deny": ["t3_project_delete", "t3_environment_unlink", "t3_thread_interrupt"],
  "defaultTimeoutMs": 120000,
  "board": { "settleAfterHours": 48 }
}
```

A deep merge replaces arrays whole, so this `deny` replaces the default list,
and it has to repeat the two default entries to keep them.

## Keys

### Sandbox and runs

| Key | Default | Does |
|---|---|---|
| `deny` | `["t3_project_delete", "t3_environment_unlink"]` | T3 tools scripts may not call. The host refuses them; see [Security model](security.md). |
| `cpuBudgetMs` | `2000` | JavaScript CPU time per run. Time spent awaiting host calls does not count. |
| `defaultTimeoutMs` | `60000` | Wall-clock limit when a call gives no `timeout_ms` |
| `maxTimeoutMs` | `600000` | The largest `timeout_ms` a call may ask for |
| `maxConcurrentRuns` | `16` | `execute` and `search` runs allowed at once |
| `runQueueMs` | `10000` | How long a run waits for a free slot before it fails with a retryable busy error |

Each run can use up to 64 MB, so the default `maxConcurrentRuns` bounds sandbox
memory at 1 GB, which equals the Linux unit's `MemoryHigh` before the rest of
the service is counted. On Linux, raise it only with `MemoryHigh` and
`MemoryMax`. The launchd agent and the Windows task set no memory limit.

### Workflow layer

| Key | Default | Does |
|---|---|---|
| `watcherIntervalSecs` | `60` | How often the watcher runs a pass |
| `maxHoldMinutes` | `30` | How long a wake to a busy thread waits in the outbox |
| `link` | `{"down": 45, "miss": 2, "stall": 240}` | Default link policy: idle minutes before a nudge, unanswered nudges before an escalation, busy minutes with nothing sent before the parent hears of it |
| `wake` | `["event", "gate", "blocker", "handoff", "closed", "report", "escalate", "reply"]` | Message kinds that wake the receiver. Other kinds are stored as notes. |
| `bodyCap`, `replyCap` | `1500`, `1500` | Characters of a wake or reply body sent inline; the rest stays behind its handle |
| `digestLines` | `6` | Note lines in the digest that rides on the next wake |
| `roles` | see below | Per-role overrides for each child role |

The built-in `roles` give an overseer's `ticket` children `down` 75, `stall`
480, `bodyCap` 600, and a `wake` list without `event` and `reply`, so a
ticket's routine events reach an overseer as notes. `ticket` has `worker`
children with the default policy, and `worker` has none.

### Board

All under `board`:

| Key | Default | Does |
|---|---|---|
| `ticketModel`, `ticketEffort` | `claude-opus-5-5`, `high` | Legacy fallback for ticket launches when `models.ticket` is unset in repo and user workflow configs. Move these into `models.ticket`; `t3cm doctor` warns when they are set. |
| `askModel`, `askEffort` | `claude-haiku-5-5`, unset | Model and effort for the ask box's scratch threads |
| `instanceId` | `claudeAgent` | Legacy fallback T3 provider instance for ticket launches when `models.ticket` is unset, and the default instance for the ask box. `t3cm doctor` warns when it is set and notes that it still applies to the ask box. |
| `pool` | unset | Legacy fallback pool from `pools` for ticket launches when `models.ticket` is unset, and the pool for the ask box. Set ticket pools in `models.ticket` and keep `board.pool` if the ask box uses it; `t3cm doctor` warns when it is set. See [Pools](#pools). |
| `runtimeMode` | `full-access` | Runtime mode for launches. The service has no thread to inherit a mode from. |
| `askPollSecs` | `4` | How often an open ask is checked for its answer |
| `askTimeoutMinutes` | `15` | How long an ask may run |
| `feed` | `50` | Activity feed entries kept on the page |
| `settleAfterHours` | `24` | Hours a done ticket or closed ask stays in the main lists; `0` turns auto-settle off |
| `reconcileActiveMinutes` | `2` | Minutes between GitHub checks while any tracked pull request is queued or waiting to be queued. From 1 to `reconcileIdleMinutes`. |
| `reconcileIdleMinutes` | `10` | Minutes between GitHub checks otherwise; `0` turns the check off, page loads included. See [The board](board.md#github-reconcile). |
| `openLinks` | `desktop` | `desktop`: thread links open in the running T3 Code desktop app, or in a new tab when it is not running. The app check needs Windows, natively or through WSL, so macOS and Linux without WSL always get a tab. `tab`: always a new tab. |

### Pools

`pools` names groups of T3 provider instances, such as two Claude accounts,
each one its own T3 provider instance. A launch from a pool picks one member,
so work spreads across the accounts instead of filling one until it hits its
usage limit. A pool holds instances of one driver and runs one model, unless
it is [mixed](#mixed-pools).

```json
{
  "pools": {
    "opus": {
      "driver": "claudeAgent",
      "model": "claude-opus-5-5",
      "effort": "high",
      "strategy": "weighted",
      "members": [
        { "instanceId": "claude-a", "weight": 1, "reserve": 30 },
        { "instanceId": "claude-b", "weight": 3 }
      ]
    }
  },
  "board": { "pool": "opus" }
}
```

| Key | Default | Does |
|---|---|---|
| `mixed` | `false` | `true` lets members be instances of any driver, each with its own model and effort. See [Mixed pools](#mixed-pools). |
| `driver` | required | The driver every member must be, as `orchestrator_capabilities` reports it in `driverKind`. A mixed pool has none. |
| `model`, `effort` | required, unset | Model and effort for every launch from the pool, so a thread gets the same model whichever account runs it. A mixed pool has none. |
| `strategy` | `weighted` | `weighted` spreads launches by weight. `priority` fills members in order and moves to the next only when one is out. `cheapest` sends launches to the lowest-cost members that can run, by weight among them. `headroom` needs a usage source, so config load rejects it for now. |
| `members[].instanceId` | required | A T3 provider instance |
| `members[].weight` | `1` | The member's share of `weighted` launches. With weights 3 and 1, three of every four launches go to the first member. |
| `members[].reserve` | `0` | A share of the account's usage window, 0-100, held for work that names the instance directly |
| `members[].cost` | `1` | What a launch on the member costs, a number of 0 or more. Only its order against the other members counts, and only `cheapest` reads it. |
| `members[].model`, `members[].effort` | required, unset | Only in a mixed pool: the model and effort for launches on this member |

`weighted` is a smooth weighted round robin. Each member's running credit is
in `store.db`, so a run of launches from the same state picks the same
members, and a restart carries on where it stopped. When two members are
level, the one with fewer running threads wins, then the one listed first.

`cheapest` looks at the members a pick has not skipped, takes those with the
lowest `cost`, and spreads launches across them by the same weighted round
robin. It moves to the next cost only when every cheaper member is out. Give a
flat-rate subscription account cost 0 and metered accounts their relative
prices. The pool then fills the subscription until limit marking takes it out,
and the metered accounts take over after that, cheapest first. Cost never overrides a reserve
or a limit. The pick reason names the cost, as in
`cheapest: cost 0, weighted: weight 1 of 2`. Costs come from config only, since
t3cm cannot see provider pricing;
[ADR 0004](https://github.com/OttoOps/t3-codemode/blob/main/docs/adr/0004-cost-for-pool-picks.md)
records the choice.

A pick skips a member that T3 reports as disabled or constrained, a member
that does not list its model or does not offer its effort, and a member
marked limited until its reset time, as [Limits](#limits) describes. A member
with a `reserve` is a last resort. t3cm has no usage source yet, so it cannot
see how close an account is to its reserve, and the pool picks that member only
when every member without a reserve is out. A launch that names the instance
directly ignores the reserve. When every member is out, the pick fails, nothing
launches, and a board launch leaves an error card that names the pool.

At load, a pool must have at least one member, no member twice, weights of 1
or more, reserves from 0 to 100 and costs of 0 or more, and `board.pool` must name a configured
pool. A pool that is not mixed must not set `model` or `effort` on a member.
A bad pool fails config load with a message that names it. Only T3 knows
whether each member is a real instance and, in a pool that is not mixed,
whether it is an instance of the pool's `driver`. Every pick checks that
against T3's live `orchestrator_capabilities`. A pick from a pool with an
unknown instance fails, and so does a pick from a pool that is not mixed with
a member of another driver. Either failure names the pool and the member. When
T3 answers at start, the service also logs a bad pool then.

The pick's reason starts with the driver, model and effort it chose, such as
`claudeAgent claude-opus-5-5, effort high; weighted: weight 1 of 4`, and
`wf.pick` returns them as `driver`, `model` and `effort`.

#### Sole members

A pool can depend on one account without saying so: when only one member can
run the pool's model, every launch from the pool waits on that member, and the
pool launches nothing while it is limited. `wf.pools()` flags that member in
the pool's `warnings` and suggests a reserve on it:

```text
only claude-b can run claude-opus-5-5, so the pool launches nothing while claude-b is out. Set a reserve on claude-b to keep room for the work only it can run.
```

With a usage source, the reserve is a floor that keeps part of the account's
usage for launches that name it directly. Until t3cm has one, a reserve only
makes the member a last resort, as the pick rules above say, so in this pool,
where nothing else can run the model, it changes no pick yet.

A member can run the model when T3 lists the model for it and offers the
member's effort. Limits and usage do not count, so the warning does not come
and go as members are benched. A mixed pool's members run different models by
design, so there the check groups members by driver: a member that is the only
one of its driver that can run its model is flagged. Once the member has a reserve, the warning drops the suggestion and
stays, since the pool still has one member for that model. A pool that is not
mixed gets no warning when two members can run its model, and a mixed pool
gets none for a driver with two members that can run their models. A pool
whose `error` is set gets no warning until the error is fixed.

#### Mixed pools

A pool with `"mixed": true` takes members of any driver, and each member names
its own model and, when it wants one, its effort. Use one for work where it
matters less which vendor runs a launch than that the launches spread out,
such as a research swarm on cheap models. A capability tier is a mixed pool
named for its class:

```json
{
  "pools": {
    "frontier": {
      "mixed": true,
      "members": [
        { "instanceId": "claude-a", "model": "claude-opus-5-5", "effort": "high" },
        { "instanceId": "codex-a", "model": "gpt-6-astra", "effort": "xhigh" }
      ]
    },
    "cheap": {
      "mixed": true,
      "strategy": "priority",
      "members": [
        { "instanceId": "claude-a", "model": "claude-haiku-5-5" },
        { "instanceId": "codex-a", "model": "gpt-6-astra-mini", "effort": "low" }
      ]
    }
  }
}
```

A mixed pool has no pool-level `driver`, `model` or `effort`, and config load
rejects one that sets them. Every member needs a `model`. t3cm does not group
models into tiers on its own, so a new model joins a tier when you add it to
the pool. The same instance can be a member of several pools, each with its
own model. Weights and credits are per pool, and a usage limit benches the
instance in every pool it belongs to. With a mixed pool active for a board
launch, passing `model` to `wf.launchTicket` overrides the pool, while passing
`effort` alone fails since each member runs its own model. [ADR 0005](https://github.com/OttoOps/t3-codemode/blob/main/docs/adr/0005-tiers-are-mixed-pools.md)
records why tiers work this way.

#### Limits

The watcher pass marks a member limited when a thread on it stops on a usage
limit, and picks skip the member until the reset. T3 shows no reason for a
snooze, so a snooze alone never marks a member. A thread counts as stopped on
a limit when its status is `failed` and its latest run ends with T3's
`Usage limit reached` error item. The pass reads each failed run once, and it
looks only at runs on pool members from the last 24 hours.

The reset time comes from T3's limit snooze. With T3's "snooze limited
threads" setting on, T3 snoozes a thread that stops on a limit until its
account resets, and t3cm takes that time when it first sees the stop. With the
setting off, t3cm cannot see the reset, so it holds the member for
`limitHoldMinutes` from when it saw the stop. Turn the setting on in T3 to get
exact reset times.

| Key | Default | Does |
|---|---|---|
| `limitHoldMinutes` | `60` | How long a member stays limited after a limit stop whose reset time T3 does not show, from 1 to 10080 (a week) |

A stop with a later reset extends a member's mark, and one with an earlier
reset leaves it as it is. The
pass clears a mark once its reset passes, and the member is picked again.
Marks live in `store.db`, so they survive a restart. `wf.pools()` shows each
mark's reset time and the thread that set it. [ADR 0001](https://github.com/OttoOps/t3-codemode/blob/main/docs/adr/0001-usage-signal-for-pools.md)
records why t3cm reads limits this way.

Board ticket launches and `wf.launchTicket` resolve their pool or model from
the `models.ticket` slot first, checking repo `.t3-workflow.json` and
`AGENTS.md` before user `~/.config/t3-workflow/config.json`, and fall back to
legacy `board.pool` only when `models.ticket` is unset. `model` or `effort`
passed to `wf.launchTicket` takes precedence over the slot and legacy board
keys; passing `effort` alone over a mixed pool fails because members run
different models. `board.pool` still covers the ask box, running the picked
member's model in place of `askModel`. Skills pick from a pool with
[`wf.pick`](globals.md#wf).
The workflow skills do that for each launch when their `model worker` or
`model ticket` slot names a pool, as `"worker": {"pool": "opus"}`; see
[A pool on a model line](https://github.com/OttoOps/t3-codemode/blob/main/skills/t3-workflow/slots.md#a-pool-on-a-model-line).
Pools are per T3 environment, since instance ids belong to one T3 server.

### Updates

| Key | Default | Does |
|---|---|---|
| `updateCheck` | `true` | Check GitHub for a newer release: the service every 12 hours, the CLI at most once a day. `false` turns off the CLI notice, the board's update button and the service's checks; `t3cm update` still works. See [Update](getting-started.md#update). |

### Upstreams

`upstreams` lists other MCP servers to wrap. It has its own page:
[Upstreams](upstreams.md).

## Environment

| Variable | Does |
|---|---|
| `T3CM_CONFIG` | Path of the config file |
| `T3CM_STATE_DIR` | State directory, same as `--state-dir` |
| `T3CM_PORT` | Port for `serve`, `status`, `exec` and `search` (default `47731`) |
| `T3CM_T3_BIN` | The `t3` binary used to mint pairing codes; see [Security model](security.md#t3-sign-in) |
| `T3CM_T3_ORIGIN` | T3's origin, when `server-runtime.json` has the wrong one |
| `T3CODE_HOME` | T3's home directory (default `~/.t3`) |
| `T3CM_TZ` | Time zone for times the board and `wf` show (default `America/Denver`; an unknown zone falls back to UTC) |
| `T3CM_NO_UPDATE_CHECK` | Any value but `0` or `false` turns the release checks off, like `updateCheck: false`. `CI` does the same. |
| `T3CM_RELEASES_URL` | Where `t3cm update` and the checks look for releases (default `https://github.com/OttoOps/t3-codemode/releases`); https, or plain http on `127.0.0.1` or `localhost` only, and with no user name or password in it. The installers read it too. |
| `RUST_LOG` | Log level |

## State

State lives in SQLite at `store.db` in the state directory above
(`--state-dir` or `T3CM_STATE_DIR` override it). It holds the T3 session, `mem`
and `vars`, the workflow links and messages, the board's cards and feed, pool
member credits, limits and pick history, and the cached T3 catalog for when T3
is down. Everything survives restarts. Next to it, `update-check.json` holds
the latest release the last check found and when.

On Linux and macOS the directory is `0700` and the database `0600`. On Windows
t3cm sets no ACL of its own: the store relies on the per-user ACL that
`%LOCALAPPDATA%` already carries, which grants access to you, SYSTEM and
Administrators, so a `--state-dir` outside your profile gets whatever its parent
grants.

Uninstalling (`install.sh --uninstall` or `install.ps1 -Uninstall`) keeps the
store. Delete the state directory to drop memory and links.
