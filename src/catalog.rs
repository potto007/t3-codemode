use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use serde_json::{Value, json};

use crate::types::{doc_block, is_identifier, property_key, sanitize_identifier, signature};
use crate::upstream::{T3_NAMESPACE, ToolDef, qualified};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub name: String,
    pub kind: &'static str,
    pub server: String,
    pub description: String,
    pub signature: String,
    pub input_schema: Value,
}

pub struct Catalog {
    pub entries: Vec<Entry>,
    pub t3_names: BTreeSet<String>,
    pub upstream_names: BTreeMap<String, BTreeSet<String>>,
    namespaces: Vec<String>,
    pub json: String,
}

const WAKE_RESULT: &str = r#"{ handle: string; delivery: "noted" | "sent" | "queued" | "board"; detail?: string; card?: string; options?: string[]; warning?: string; closed?: boolean }"#;
const MERGE_SIGNATURE: &str = r#"wf.merge(input: { repo: string; pr: number; head: string; method?: "merge" | "squash" | "rebase" }): Promise<{ repo: string; pr: number; head: string } & ({ status: "queued"; position: number | null; approval: string } | { status: "merged"; commit: string | null; method?: "merge" | "squash" | "rebase" } | { status: "waiting"; mergeState: string; approval: string } | { status: "refused"; reason: string })>;"#;
const CARD: &str = r#"{ id: string; kind: string; label: string | null; ticket: string | null; link: string | null; text: string; handle: string | null; options: string[]; at: string; atLocal: string; resolved?: string; resolution?: string | null; token: string; approval?: string; repo: string | null; project: { id: string; title: string | null } | null }"#;

const NEW_ISSUE: &str = "{ id: string; url: string; repo: string; number: number; label: string; title: string | null; note: string | null; thread: string; threadLink: string | null; threadTitle: string | null; at: string; atLocal: string; dismissed: boolean; dismissedAt: string | null; dismissedAtLocal: string | null; restoredAt: string | null }";
const SKIPPED: &str = "{ instanceId: string; reason: string }";

fn builtin(name: &str, kind: &'static str, description: &str, sig: &str, input: Value) -> Entry {
    Entry {
        name: name.into(),
        kind,
        server: kind.into(),
        description: description.into(),
        signature: format!("{}{sig}", doc_block(description, "")),
        input_schema: input,
    }
}

fn obj(props: Value, required: &[&str]) -> Value {
    json!({"type": "object", "properties": props, "required": required, "additionalProperties": false})
}

pub fn builtins() -> Vec<Entry> {
    let kinds = json!(["note", "event", "gate", "blocker", "report", "handoff", "closed"]);
    let link = obj(
        json!({"parent": {"type": "string"}, "label": {"type": "string"}, "role": {"type": "string"},
               "down": {"type": "integer"}, "miss": {"type": "integer"}, "stall": {"type": "integer"}}),
        &["parent", "label", "role"],
    );
    vec![
        builtin(
            "mem.get",
            "mem",
            "Read a JSON value from this session's namespace; null when absent.",
            "mem.get(key: string): Promise<unknown | null>;",
            obj(json!({"key": {"type": "string"}}), &["key"]),
        ),
        builtin(
            "mem.set",
            "mem",
            "Store a JSON value (up to 1 MB) under key. Survives restarts.",
            "mem.set(key: string, value: unknown): Promise<null>;",
            obj(json!({"key": {"type": "string"}, "value": {}}), &["key", "value"]),
        ),
        builtin(
            "mem.del",
            "mem",
            "Delete a key; true when it existed.",
            "mem.del(key: string): Promise<boolean>;",
            obj(json!({"key": {"type": "string"}}), &["key"]),
        ),
        builtin(
            "mem.list",
            "mem",
            "Keys in the namespace, sorted, optionally starting with prefix.",
            "mem.list(prefix?: string): Promise<string[]>;",
            obj(json!({"prefix": {"type": "string"}}), &[]),
        ),
        builtin(
            "mem.ns",
            "mem",
            "The same get/set/del/list API on another namespace, shared across threads (for example \"work:widgets#41\"). Synchronous.",
            "mem.ns(name: string): { get; set; del; list };",
            obj(json!({"name": {"type": "string"}}), &["name"]),
        ),
        builtin(
            "vars",
            "mem",
            "A plain object restored before each execute and saved after it, per session (default: your thread). Assign fields; keep it JSON-serializable.",
            "vars: Record<string, unknown>;",
            json!({}),
        ),
        builtin(
            "wf.init",
            "wf",
            "Start this thread with a role: overseer, ticket or worker. A role the overseer role lists as a child (ticket by default) with no open link joins the board (parent \"board\"), labeled from its board launch, `label`, or its T3 title, so its gates become board cards; root: true keeps it a standalone root. Other roles start as roots. A thread the board launched clears its own pending launch, matched by thread id.",
            "wf.init(input: { role: string; label?: string; root?: boolean }): Promise<{ thread: string; role: string; root: boolean; parent?: string; label?: string; watcher: string }>;",
            obj(
                json!({"role": {"type": "string"}, "label": {"type": "string"}, "root": {"type": "boolean"}}),
                &["role"],
            ),
        ),
        builtin(
            "wf.openChild",
            "wf",
            "Prepare a link to a child thread you are about to launch. Paste the returned brief, verbatim, at the end of the child's launch brief; the child binds the link with wf.join.",
            "wf.openChild(input: { role: string; label: string }): Promise<{ brief: string; params: { down: number; miss: number; stall: number } }>;",
            obj(json!({"role": {"type": "string"}, "label": {"type": "string"}}), &["role", "label"]),
        ),
        builtin(
            "wf.join",
            "wf",
            "Join the link a parent opened (the call is in your launch brief). The watcher then nudges you after `down` idle minutes with no gate open.",
            "wf.join(input: { parent: string; label: string; role: string; down?: number; miss?: number; stall?: number }): Promise<{ parent: string; label: string; params: { down: number; miss: number; stall: number } }>;",
            link,
        ),
        builtin(
            "wf.send",
            "wf",
            "Send to your parent. note: stored, never wakes, reaches the parent as a digest line at its next wake. event/gate/blocker/report wake it per link policy (overseer links turn events into notes). gate also opens a gate (no nudges until cleared). handoff/closed close the link. wake overrides the policy. With parent \"board\": note/event go to the board's feed, gate/blocker/report become a decision card (delivery \"board\", card id, and the card's reply buttons as options), handoff/closed mark the ticket done. options (gate/blocker/report only) sets the card's buttons in place of parsing the body: each one line, 1-60 chars, unique ignoring case, at most 8; a bad entry fails the send. A gate card with no buttons also returns a warning.",
            &format!(
                "wf.send(input: {{ kind: \"note\" | \"event\" | \"gate\" | \"blocker\" | \"report\" | \"handoff\" | \"closed\"; body: string; gate?: string; wake?: boolean; options?: string[] }}): Promise<{WAKE_RESULT}>;"
            ),
            obj(
                json!({"kind": {"enum": kinds}, "body": {"type": "string"}, "gate": {"type": "string"}, "wake": {"type": "boolean"},
                    "options": {"type": "array", "items": {"type": "string"}, "maxItems": 8}}),
                &["kind", "body"],
            ),
        ),
        builtin(
            "wf.reply",
            "wf",
            "Send to one of your children (by label or thread id). A closed link needs reopen: true.",
            &format!("wf.reply(input: {{ to: string; body: string; reopen?: boolean }}): Promise<{WAKE_RESULT}>;"),
            obj(
                json!({"to": {"type": "string"}, "body": {"type": "string"}, "reopen": {"type": "boolean"}}),
                &["to", "body"],
            ),
        ),
        builtin(
            "wf.gate",
            "wf",
            "Open a gate while you wait on a person ({open: \"merge\"}); the watcher does not nudge you until {clear: true}.",
            "wf.gate(input: { open: string } | { clear: true }): Promise<{ gate: string | null }>;",
            obj(json!({"open": {"type": "string"}, "clear": {"type": "boolean"}}), &[]),
        ),
        builtin(
            "wf.close",
            "wf",
            "Close a child link without a handoff (by label or thread id); the watcher stops watching it.",
            "wf.close(input: { child: string }): Promise<{ closed: string }>;",
            obj(json!({"child": {"type": "string"}}), &["child"]),
        ),
        builtin(
            "wf.inbox",
            "wf",
            "Unread notes from your children, oldest first; marks them read.",
            "wf.inbox(): Promise<{ handle: string; at: string; label: string; kind: string; head: string }[]>;",
            obj(json!({}), &[]),
        ),
        builtin(
            "wf.show",
            "wf",
            "One stored message in full, by handle (\"m12\").",
            "wf.show(handle: string): Promise<{ handle: string; kind: string; from: string; to: string; at: string; body: string }>;",
            obj(json!({"handle": {"type": "string"}}), &["handle"]),
        ),
        builtin(
            "wf.board",
            "wf",
            "Your parent link and one object per child link, with live T3 status. all: include closed links. parent: show that node's tree instead, each child with its workers (parent: \"board\" is the whole t3-codemode board, no caller needed).",
            "wf.board(input?: { all?: boolean; parent?: string }): Promise<{ parent: { thread: string; label: string; closed: boolean; gate: string | null } | null; children: { label: string; role: string; thread: string; link: string | null; status: string; asks: number; background: number; gate: string | null; lastUp: string; nudges: number; unread: number; last: { handle: string; kind: string; head: string } | null }[] }>;",
            obj(json!({"all": {"type": "boolean"}, "parent": {"type": "string"}}), &[]),
        ),
        builtin(
            "wf.adopt",
            "wf",
            "Take over a retiring thread's open child links, pending links and queued messages.",
            "wf.adopt(input: { from: string }): Promise<{ adopted: string[]; pending: string[] }>;",
            obj(json!({"from": {"type": "string"}}), &["from"]),
        ),
        builtin(
            "wf.sweep",
            "wf",
            "Before finishing: open child links, never-joined links (dropped now), and whether your own parent link is still open.",
            "wf.sweep(): Promise<{ open: string[]; dropped: string[]; parentOpen: boolean }>;",
            obj(json!({}), &[]),
        ),
        builtin(
            "wf.launchTicket",
            "wf",
            "Launch a t3-ticket thread for a GitHub issue that reports to the t3-codemode board (the root overseer). The T3 project is the one whose workspace origin remote is the issue's repo, unless project (id or title) is given. The model resolves through the models.ticket slot for the issue's project in this order: launch arguments (model/effort), the repo's .t3-workflow.json and AGENTS.md, the user config's models.ticket, legacy board.* ticket keys, and the built-in default. A pool value picks through that pool (as wf.pick does) and adds pool and reason to the result. The result always carries instanceId, model, effort and modelSource. An unmapped repo, an invalid or unavailable model/instance/effort/pool, or a failed launch throws and leaves an error card on the board, never falling back to the built-in default.",
            "wf.launchTicket(input: { issue: string; project?: string; title?: string; model?: string; effort?: string }): Promise<{ ok: true; label: string; thread: string; link: string | null; projectId: string; title: string; instanceId: string; model: string; effort: string | null; modelSource: \"argument\" | \".t3-workflow.json\" | \"AGENTS.md\" | \"user\" | \"board\" | \"default\"; pool?: string; reason?: string }>;",
            obj(
                json!({"issue": {"type": "string", "description": "https://github.com/<owner>/<repo>/issues/<n> or <owner>/<repo>#<n>"},
                       "project": {"type": "string"}, "title": {"type": "string"}, "model": {"type": "string"}, "effort": {"type": "string"}}),
                &["issue"],
            ),
        ),
        builtin(
            "wf.issue",
            "wf",
            "Report a GitHub issue this thread just filed, so it shows on the board's new-issues list with a link back to this thread and when it was filed. Needs no parent link. Reporting an issue already on the list, dismissed or not, or one that already has a board ticket, adds nothing: added is false, reason says which, and message says so. From the list the user launches a ticket for it (the same path as wf.launchTicket) or dismisses it.",
            &format!(
                "wf.issue(input: {{ url: string; title?: string; note?: string }}): Promise<{{ added: boolean; reason?: \"reported\" | \"ticket\"; message: string; issue?: {NEW_ISSUE}; ticket?: {{ label: string; thread: string | null; link: string | null; done: boolean }} }}>;"
            ),
            obj(
                json!({"url": {"type": "string", "description": "https://github.com/<owner>/<repo>/issues/<n> or <owner>/<repo>#<n>"},
                       "title": {"type": "string", "maxLength": 300}, "note": {"type": "string", "maxLength": 2000}}),
                &["url"],
            ),
        ),
        builtin(
            "wf.pick",
            "wf",
            "Pick a provider instance from a configured pool for one launch. The result and its reason name the driver, model and effort chosen: the pool's model and effort, or in a mixed pool the chosen member's own. Skips members T3 reports disabled or constrained, members that do not list their model or offer their effort, and members marked limited; a member with a reserve is a last resort, picked only when every other member is out. Under strategy cheapest, the lowest-cost members that can run take every launch, by weight among themselves. Pass modelSelection to t3_thread_launch. Throws, naming the pool, when no member can run or, in a pool that is not mixed, a member is not an instance of the pool's driver; nothing is launched.",
            &format!(
                "wf.pick(input: {{ pool: string }}): Promise<{{ pool: string; instanceId: string; driver: string; model: string; effort: string | null; options: {{ id: string; value: string }}[]; reason: string; skipped: {SKIPPED}[]; modelSelection: {{ instanceId: string; model: string; options?: {{ id: string; value: string }}[] }} }}>;"
            ),
            obj(json!({"pool": {"type": "string"}}), &["pool"]),
        ),
        builtin(
            "wf.pools",
            "wf",
            "Each configured pool with its members: driver, model and effort, weight, reserve, cost, state (ready, limited, disabled, constrained, missing-model, missing-effort, or unknown when T3 did not answer), why, the limit's reset time (ISO and local), running threads, and pick count and last pick. A mixed pool has no pool-level driver, model or effort (null), and in a mixed pool a member's driver is null when T3 did not answer. error names a member that is not an instance of the pool's driver, or says T3 did not answer. warnings flags a member that is the only one able to run the pool's model, or in a mixed pool the only one of its driver able to run its model, and suggests a reserve on it when it has none.",
            "wf.pools(): Promise<{ pool: string; mixed: boolean; driver: string | null; model: string | null; effort: string | null; strategy: \"weighted\" | \"priority\" | \"cheapest\"; error: string | null; warnings: string[]; members: { instanceId: string; driver: string | null; model: string; effort: string | null; weight: number; reserve: number; cost: number; state: string; reason: string | null; limitedUntil: string | null; limitedUntilLocal: string | null; limitReason: string | null; usageLeft: number | null; running: number | null; picks: number; lastPick: string | null; lastPickLocal: string | null }[] }[]>;",
            obj(json!({}), &[]),
        ),
        builtin(
            "wf.status",
            "wf",
            "The service's own state, as the board header shows it: version, T3 connection, watcher, the release check, and skills, which compares the installed T3 workflow skills and recorded setup steps against this release. skills.current is false when the skills are behind, edited, missing, installed by npx skills, or an automatic setup step is missing; present is false when no skills are installed at all; fixes lists the commands to run, in order. Reads the skills fresh on each call.",
            &format!(
                "wf.status(): Promise<{{ version: string; t3: {{ connected: boolean; lastError: string | null }}; tokenExpiresAt: string | null; watcher: {{ lastPass: string | null; lastPassAt: string | null; alive: boolean }}; openLinks: \"desktop\" | \"tab\"; update: {{ current: string; latest: string; available: boolean; notes: string; command: string }} | null; skills: {SKILLS} }}>;"
            ),
            obj(json!({}), &[]),
        ),
        builtin(
            "wf.slot",
            "wf",
            "One T3 workflow slot, looked up by the step that uses it: review before opening a PR, passes when publishing, delivery for delivery stages and gates, models when launching. The service resolves it for your thread's T3 instance and project, the way your own session would see it: defaults, the user config, the repo's AGENTS.md Agent workflow block, the repo's .t3-workflow.json, and the skills your agent's config folder has installed and enabled. Each skill carries how to run it (the Skill tool, or the inline SKILL.md to read) or why it is skipped. For delivery, files filters steps with paths globs against changed repo-relative file paths and reports unmatched steps in skipped. line is the one-line summary; warnings names config problems and a connection that reported no config folder. repo overrides the project's workspace root, for example with your worktree.",
            &format!(
                "wf.slot(name: \"review\" | \"passes\" | \"delivery\" | \"models\" | \"credentials\", input?: {{ repo?: string; instance?: string; skills?: string[]; files?: string[] }}): Promise<{}>;",
                slot_result()
            ),
            obj(
                json!({"name": {"enum": ["review", "passes", "delivery", "models", "credentials"]}, "repo": {"type": "string"},
                       "instance": {"type": "string"}, "skills": {"type": "array", "items": {"type": "string"}},
                       "files": {"type": "array", "items": {"type": "string"}}}),
                &["name"],
            ),
        ),
        builtin(
            "wf.renewal",
            "wf",
            "A command failed: pass its output, and the service matches it against the credentials slot's patterns. Returns the renewal skill to run, then retry the command once, or null when no pattern matches. skipped is true when the matching renewal skill no longer resolves; carry on as if none were configured.",
            &format!(
                "wf.renewal(input: {{ output: string; repo?: string; instance?: string }}): Promise<{{ match: string; skill: {SLOT_SKILL}; how: string; skipped: boolean; reason?: string; warnings: string[] }} | null>;"
            ),
            obj(
                json!({"output": {"type": "string"}, "repo": {"type": "string"}, "instance": {"type": "string"}}),
                &["output"],
            ),
        ),
        builtin(
            "wf.slots",
            "wf",
            "Every T3 workflow slot at once, as setup and the board need them, in the shape `t3cm slots --json` prints. A role looks up the one slot its step needs with wf.slot instead.",
            "wf.slots(input?: { repo?: string; instance?: string; skills?: string[] }): Promise<{ repo: string; config: string | null; repoConfig?: string | null; review: object; passes: object; credentials: object; delivery: object; models: { ticket: object; worker: object }; problems: string[]; warnings: string[]; context: { instance: string | null; driver: string | null; claudeDir: string; codexHome: string; folderFrom: \"reported\" | \"recorded\" | \"default\"; config: string } }>;",
            obj(
                json!({"repo": {"type": "string"}, "instance": {"type": "string"}, "skills": {"type": "array", "items": {"type": "string"}}}),
                &[],
            ),
        ),
        builtin(
            "wf.merge",
            "wf",
            "Merge a GitHub pull request through the service's gh login, only at the approved head commit (refused when the PR's head differs). On a repo with a merge queue: a mergeable PR is enqueued (never auto-merge); one not yet mergeable is recorded as waiting to be queued and returns waiting. Without a merge queue: merged directly with method, or the repo's default method. Throws when gh cannot reach GitHub.",
            MERGE_SIGNATURE,
            obj(
                json!({"repo": {"type": "string", "description": "owner/name"}, "pr": {"type": "integer"},
                       "head": {"type": "string", "description": "the approved head commit, full 40-character SHA"},
                       "method": {"enum": ["merge", "squash", "rebase"]}}),
                &["repo", "pr", "head"],
            ),
        ),
        builtin(
            "wf.cards",
            "wf",
            "The board's open decision cards, oldest first: gates, blockers, reports and escalations from ticket threads, with their reply options (the sender's options, else those parsed from their text) and their ticket's repo and project. all: include resolved cards, each with resolved (local time) and resolution. context (a saved context's id or name) or projects (project ids, or old repo or title values) keeps only the cards of those projects, matched as the board page matches them; an empty list keeps every card.",
            &format!(
                "wf.cards(input?: {{ all?: boolean; context?: string; projects?: string[] }}): Promise<{CARD}[]>;"
            ),
            obj(
                json!({"all": {"type": "boolean"}, "context": {"type": "string"}, "projects": {"type": "array", "items": {"type": "string"}}}),
                &[],
            ),
        ),
        builtin(
            "wf.answer",
            "wf",
            "Answer a board card for the user: sends the ticket a [t3cm reply] 'The user says \"<text>\" about <id>: <card text>' and resolves the card. Same as a button on the board page.",
            &format!("wf.answer(id: string, text: string): Promise<{WAKE_RESULT}>;"),
            obj(json!({"id": {"type": "string"}, "text": {"type": "string"}}), &["id", "text"]),
        ),
        builtin(
            "wf.feed",
            "wf",
            "The board's activity feed: the latest notes and events from ticket and worker threads, newest first, each with its ticket's repo and project. context (a saved context's id or name) or projects (project ids, or old repo or title values) keeps only the rows of those projects, matched as the board page matches them: up to limit matching rows from the latest 200.",
            "wf.feed(input?: { limit?: number; context?: string; projects?: string[] }): Promise<{ handle: string; at: string; iso: string; label: string; kind: string; head: string; ticket: string | null; repo: string | null; project: { id: string; title: string | null } | null }[]>;",
            obj(
                json!({"limit": {"type": "integer"}, "context": {"type": "string"}, "projects": {"type": "array", "items": {"type": "string"}}}),
                &[],
            ),
        ),
        builtin(
            "wf.waiting.add",
            "wf",
            "Add an item to your waiting list (decisions the user owes). Ids cycle a1..a9, b1..d9, skipping open ones.",
            "wf.waiting.add(input: { text: string; from?: string; link?: string }): Promise<{ id: string }>;",
            obj(json!({"text": {"type": "string"}, "from": {"type": "string"}, "link": {"type": "string"}}), &["text"]),
        ),
        builtin(
            "wf.waiting.resolve",
            "wf",
            "Resolve an open waiting-list item.",
            "wf.waiting.resolve(id: string, note?: string): Promise<{ id: string; text: string; resolved: true }>;",
            obj(json!({"id": {"type": "string"}, "note": {"type": "string"}}), &["id"]),
        ),
        builtin(
            "wf.waiting.list",
            "wf",
            "Your open waiting-list items, oldest first.",
            "wf.waiting.list(): Promise<{ id: string; text: string; from: string | null; link: string | null; at: string }[]>;",
            obj(json!({}), &[]),
        ),
    ]
}

const SKILLS: &str = "{ release: string; installed: string | null; present: boolean; current: boolean; fix: string | null; foreign: { tool: string; lock: string | null; commit: string | null; skills: string[]; matchesRelease: boolean; differingFiles: number } | null; missingSteps: { id: string; kind: \"auto\" | \"interview\"; since: string; slot?: string; fix: string }[]; configState: \"absent\" | \"legacy\" | \"recorded\" | \"problem\" | null; problem?: string; fixes: string[]; error?: string }";

const SLOT_SKILL: &str = "{ ref: string; status: \"ok\" | \"ambiguous\" | \"missing\"; name?: string; path?: string | null; where?: string; invoke?: \"skill\" | \"inline\"; others?: string[]; how?: string } | null";

const SLOT_MODEL: &str = "{ slot: string; source: string; spec: { model?: string; effort?: string; provider?: string; pool?: string } | null; note?: string | null; problem?: string; modelSelection: { instanceId: string; model: string; options?: { id: string; value: string }[] } | { pool: string; pick: string } | null }";

const DELIVERY_STEP: &str = "{ id: string; kind: \"builtin\" | \"skill\" | \"manual\"; skill?: SlotSkill; prompt?: string; gate: \"always\" | \"chain\" | \"never\"; scope: \"pr\" | \"ticket\" | \"post-handoff\"; paths?: string[]; watch?: boolean }";

fn slot_result() -> String {
    "{ slot: string; source?: string; line: string; warnings: string[]; skill?: SlotSkill; note?: string; skills?: (SlotSkill)[]; renewals?: { match: string; skill: SlotSkill }[]; steps?: DeliveryStep[]; closeAfter?: \"pr\" | \"ticket\" | \"all\"; skipped?: { id: string; reason: string; paths?: string[] }[]; mode?: string; problem?: string; ticket?: SlotModel; worker?: SlotModel }"
        .replace("DeliveryStep", DELIVERY_STEP)
        .replace("SlotSkill", SLOT_SKILL)
        .replace("SlotModel", SLOT_MODEL)
}

const CALL_SIGNATURE: &str = "call(name: string, input?: object): Promise<unknown>;";

fn display_name(ns: &str, tool: &str) -> String {
    if is_identifier(tool) { format!("{ns}.{tool}") } else { format!("{ns}[{}]", property_key(tool)) }
}

fn push_member(s: &mut String, signature: &str, display: &str, member: &str) {
    for line in signature.lines() {
        s.push_str("  ");
        match line.strip_prefix(display) {
            Some(rest) => {
                s.push_str(member);
                s.push_str(rest);
            }
            None => s.push_str(line),
        }
        s.push('\n');
    }
}

pub fn aliases(tools: &[&str]) -> Vec<(String, Option<String>)> {
    let exact: BTreeSet<&str> = tools.iter().copied().collect();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let sanitized: Vec<String> = tools.iter().map(|t| sanitize_identifier(t)).collect();
    for (t, a) in tools.iter().zip(&sanitized) {
        if a != t {
            *counts.entry(a.clone()).or_default() += 1;
        }
    }
    tools
        .iter()
        .zip(sanitized)
        .map(|(t, a)| {
            let free = a != *t && counts[&a] == 1 && !exact.contains(a.as_str()) && a != "call";
            (t.to_string(), free.then_some(a))
        })
        .collect()
}

impl Catalog {
    pub fn build(t3_tools: &[Value]) -> Catalog {
        Catalog::build_with(t3_tools, &[])
    }

    pub fn build_with(t3_tools: &[Value], upstreams: &[(String, Vec<ToolDef>)]) -> Catalog {
        let upstream_count: usize = upstreams.iter().map(|(_, t)| t.len()).sum();
        let mut entries = Vec::with_capacity(t3_tools.len() + upstream_count + 24);
        let mut t3_names = BTreeSet::new();
        for tool in t3_tools {
            let Some(name) = tool["name"].as_str() else { continue };
            let description = tool["description"].as_str().unwrap_or_default().to_string();
            let input = tool.get("inputSchema").cloned().unwrap_or_else(|| json!({"type": "object"}));
            let display = format!("t3.{}", sanitize_identifier(name));
            let sig = signature(&display, &description, &input, tool.get("outputSchema"));
            t3_names.insert(name.to_string());
            entries.push(Entry {
                name: name.into(),
                kind: "t3",
                server: T3_NAMESPACE.into(),
                description,
                signature: sig,
                input_schema: input,
            });
        }
        let mut upstream_names: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut namespaces = Vec::new();
        for (ns, tools) in upstreams {
            if ns == T3_NAMESPACE || upstream_names.contains_key(ns) {
                continue;
            }
            namespaces.push(ns.clone());
            let names = upstream_names.entry(ns.clone()).or_default();
            for tool in tools {
                if !names.insert(tool.name.clone()) {
                    continue;
                }
                let display = display_name(ns, &tool.name);
                let mut sig = signature(&display, &tool.description, &tool.input_schema, tool.output_schema.as_ref());
                if tool.name == "call" {
                    sig.insert_str(doc_block(&tool.description, "").len() + display.len() + 1, "\"call\", ");
                }
                entries.push(Entry {
                    name: qualified(ns, &tool.name),
                    kind: "mcp",
                    server: ns.clone(),
                    description: tool.description.clone(),
                    signature: sig,
                    input_schema: tool.input_schema.clone(),
                });
            }
        }
        entries.extend(builtins());
        let json = serde_json::to_string(&entries).expect("catalog serializes");
        Catalog { entries, t3_names, upstream_names, namespaces, json }
    }

    pub fn tools_of(&self, ns: &str) -> Vec<&str> {
        let prefix_len = ns.len() + 1;
        self.entries.iter().filter(|e| e.kind == "mcp" && e.server == ns).map(|e| &e.name[prefix_len..]).collect()
    }

    pub fn declarations(&self) -> String {
        let mut s = String::from("declare const t3: {\n");
        for e in self.entries.iter().filter(|e| e.kind == "t3") {
            for line in e.signature.lines() {
                s.push_str("  ");
                s.push_str(line.strip_prefix("t3.").unwrap_or(line));
                s.push('\n');
            }
        }
        s.push_str(&format!("  {CALL_SIGNATURE}\n}};\n"));
        for ns in &self.namespaces {
            s.push_str(&format!("declare const {ns}: {{\n"));
            for e in self.entries.iter().filter(|e| e.kind == "mcp" && e.server == *ns) {
                let tool = &e.name[ns.len() + 1..];
                if tool == "call" {
                    continue;
                }
                push_member(&mut s, &e.signature, &display_name(ns, tool), &property_key(tool));
            }
            s.push_str(&format!("  {CALL_SIGNATURE}\n}};\n"));
        }
        s.push_str(&format!("declare const tools: {{\n  {CALL_SIGNATURE}\n}};\n"));
        for e in self.entries.iter().filter(|e| e.kind != "t3" && e.kind != "mcp") {
            s.push_str(&e.signature);
            s.push('\n');
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_result_parenthesizes_nullable_skill_array() {
        let sig = slot_result();
        assert!(!sig.contains("| null[]"), "{sig}");
        assert!(sig.contains(&format!("skills?: ({SLOT_SKILL})[]")), "{sig}");
    }
}
