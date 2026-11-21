use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow, bail};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::board::{BOARD, LaunchArgs, NewCard, Remotes, checked_options};
use crate::clock::{Clock, iso, local_clock, minutes_since};
use crate::config::{CLOSING, Config, LinkParams, UP_KINDS};
use crate::github::GitHub;
use crate::issues::IssueArgs;
use crate::merge::MergeArgs;
use crate::store::{Bind, Link, Pending, Store};
use crate::t3::{T3Client, T3Error};
use crate::usage::UsageSource;

pub const ACTIVE: &[&str] = &["preparing", "queued", "starting", "running"];
pub const ENDED_BADLY: &[&str] = &["failed", "interrupted", "cancelled"];
pub const WATCHER: &str = "t3cm";
const OPTION_KINDS: &[&str] = &["gate", "blocker", "report"];
pub const LAST_PASS: &str = "watcher_last_pass";
const NO_CALLER: &str = "wf needs the caller's T3 thread id: connect with the X-T3-Thread header \
(Claude Code: X-T3-Thread: ${T3CODE_THREAD_ID}) or pass `thread` to execute";

pub fn waiting_ids() -> Vec<String> {
    ['a', 'b', 'c', 'd'].iter().flat_map(|c| (1..=9).map(move |n| format!("{c}{n}"))).collect()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InitArgs {
    role: String,
    label: Option<String>,
    #[serde(default)]
    root: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenChildArgs {
    role: String,
    label: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JoinArgs {
    parent: String,
    label: String,
    role: String,
    down: Option<i64>,
    miss: Option<i64>,
    stall: Option<i64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SendArgs {
    kind: String,
    body: String,
    gate: Option<String>,
    wake: Option<bool>,
    options: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplyArgs {
    to: String,
    body: String,
    #[serde(default)]
    reopen: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GateArgs {
    open: Option<String>,
    #[serde(default)]
    clear: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChildArgs {
    child: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HandleArgs {
    handle: String,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct BoardArgs {
    #[serde(default)]
    all: bool,
    parent: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AnswerArgs {
    id: String,
    text: String,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct CardsArgs {
    #[serde(default)]
    all: bool,
    context: Option<String>,
    projects: Option<Vec<String>>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct FeedArgs {
    limit: Option<usize>,
    context: Option<String>,
    projects: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PickArgs {
    pool: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PickedArgs {
    pick: i64,
    thread: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AdoptArgs {
    from: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WaitAddArgs {
    text: String,
    from: Option<String>,
    link: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WaitResolveArgs {
    id: String,
    note: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

fn parse<T: DeserializeOwned>(method: &str, args: Value) -> Result<T> {
    let args = if args.is_null() { json!({}) } else { args };
    serde_json::from_value(args).map_err(|e| anyhow!("wf.{method}: {e}"))
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

pub(crate) fn count(v: &Value) -> i64 {
    match v {
        Value::Array(a) => a.len() as i64,
        Value::Number(n) => n.as_i64().unwrap_or(0),
        other => truthy(other) as i64,
    }
}

fn quote_label(label: &str) -> String {
    if label.is_empty() || label.chars().any(|c| c.is_whitespace() || c == '"' || c == ']') {
        Value::String(label.into()).to_string()
    } else {
        label.into()
    }
}

pub fn header(kind: &str, from: &str, handle: &str, gate: Option<&str>) -> String {
    let gate = gate.map(|g| format!(" gate={g}")).unwrap_or_default();
    format!("[t3cm {kind} from={} msg={handle}{gate}]", quote_label(from))
}

pub fn join_target(message: &str) -> Option<(String, String)> {
    let json_string = |s: &str| {
        let mut it = serde_json::Deserializer::from_str(s).into_iter::<String>();
        let v = it.next()?.ok()?;
        Some((v, s[it.byte_offset()..].to_string()))
    };
    let (_, block) = message.rsplit_once("## t3-codemode link\n")?;
    let (_, rest) = block.split_once("async () => wf.join({ parent: ")?;
    let (parent, rest) = json_string(rest)?;
    let (label, _) = json_string(rest.strip_prefix(", label: ")?)?;
    Some((parent, label))
}

pub fn capped(body: &str, handle: &str, cap: usize) -> String {
    let n = body.chars().count();
    if n <= cap {
        return body.to_string();
    }
    let head: String = body.chars().take(cap).collect();
    format!("{}\n... ({} more chars; full: wf.show(\"{handle}\"))", head.trim_end(), n - cap)
}

fn gate_name(s: &str) -> String {
    let joined = s.split_whitespace().collect::<Vec<_>>().join("_");
    if joined.is_empty() { "user".into() } else { joined }
}

pub struct Wf {
    pub(crate) store: Arc<Store>,
    pub(crate) t3: Arc<dyn T3Client>,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) config: Arc<Config>,
    pub(crate) lock: tokio::sync::Mutex<()>,
    pub(crate) remotes: Arc<dyn Remotes>,
    pub(crate) github: Arc<dyn GitHub>,
    pub(crate) live: Mutex<HashMap<String, Value>>,
    pub(crate) closed_prs: Mutex<HashMap<String, ClosedPrs>>,
    pub(crate) known_projects: Mutex<HashMap<String, Value>>,
    pub(crate) changes: tokio::sync::broadcast::Sender<()>,
    pub(crate) asks_lock: tokio::sync::Mutex<()>,
    pub(crate) pool_lock: tokio::sync::Mutex<()>,
    pub(crate) reconcile_lock: tokio::sync::Mutex<()>,
    pub(crate) usage: Mutex<Option<Arc<dyn UsageSource>>>,
    pub(crate) release: Mutex<Option<crate::release::Version>>,
    pub(crate) skills: crate::drift::Cache,
    pub(crate) slot_folders: Mutex<Option<crate::slots::Folders>>,
    pub(crate) drivers: Mutex<HashMap<String, String>>,
}

const MAX_PR_PAGES: usize = 20;
const PR_LIST_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

type ClosedPrs = ((u64, u64), Value);

type Consumed = Vec<(String, Vec<String>)>;

struct Flush {
    sent: Vec<(String, Vec<String>)>,
    errors: Vec<String>,
}

impl Wf {
    pub fn new(
        store: Arc<Store>,
        t3: Arc<dyn T3Client>,
        clock: Arc<dyn Clock>,
        config: Arc<Config>,
        remotes: Arc<dyn Remotes>,
        github: Arc<dyn GitHub>,
    ) -> Wf {
        if store.node_role(BOARD).ok().flatten().as_deref() != Some("overseer") {
            let _ = store.node_set(BOARD, "overseer", &iso(clock.now()));
        }
        if store.meta_get(crate::reconcile::SINCE).ok().flatten().is_none() {
            let _ = store.meta_set(crate::reconcile::SINCE, &iso(clock.now()));
        }
        Wf {
            store,
            t3,
            clock,
            config,
            lock: tokio::sync::Mutex::new(()),
            remotes,
            github,
            live: Mutex::new(HashMap::new()),
            closed_prs: Mutex::new(HashMap::new()),
            known_projects: Mutex::new(HashMap::new()),
            changes: tokio::sync::broadcast::channel(64).0,
            asks_lock: tokio::sync::Mutex::new(()),
            pool_lock: tokio::sync::Mutex::new(()),
            reconcile_lock: tokio::sync::Mutex::new(()),
            usage: Mutex::new(None),
            release: Mutex::new(None),
            skills: crate::drift::Cache::default(),
            slot_folders: Mutex::new(None),
            drivers: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn now(&self) -> String {
        iso(self.clock.now())
    }

    pub(crate) fn mins(&self, s: Option<&str>) -> Option<f64> {
        minutes_since(&*self.clock, s)
    }

    pub fn watcher_alive(&self) -> bool {
        let last = self.store.meta_get(LAST_PASS).ok().flatten();
        let limit = 3.0 * self.config.watcher_interval_secs.max(60) as f64 / 60.0;
        self.mins(last.as_deref()).is_some_and(|m| m <= limit)
    }

    fn watcher_line(&self) -> String {
        let last = self.store.meta_get(LAST_PASS).ok().flatten();
        match self.mins(last.as_deref()) {
            Some(m) if self.watcher_alive() => format!("running (last pass {m:.0} min ago)"),
            Some(m) => format!("stale (last pass {m:.0} min ago); check `{}`", crate::platform::SERVICE.status),
            None => "no pass recorded yet".into(),
        }
    }

    fn mark_pass(&self) -> Result<()> {
        self.store.meta_set(LAST_PASS, &self.now())
    }

    pub(crate) async fn read_thread(&self, id: &str) -> Result<Value, T3Error> {
        match self.t3.call("t3_thread_read", json!({"threadId": id, "limit": 1, "maxCharsPerItem": 1})).await {
            Ok(r) => {
                let th = self.with_closed_prs(id, r["thread"].clone()).await;
                self.note_live(id, &th);
                Ok(th)
            }
            Err(e) => {
                if e.is_not_found() {
                    self.note_live(id, &json!({"status": "gone"}));
                }
                Err(e)
            }
        }
    }

    async fn with_closed_prs(&self, id: &str, mut th: Value) -> Value {
        let counts = th["pullRequestCount"].as_u64().zip(th["openPullRequestCount"].as_u64());
        let Some(counts) = counts.filter(|(all, open)| all > open) else {
            self.closed_prs.lock().unwrap_or_else(|e| e.into_inner()).remove(id);
            return th;
        };
        let cached = self.closed_prs.lock().unwrap_or_else(|e| e.into_inner()).get(id).cloned();
        let prs = match cached {
            Some((at, prs))
                if at == counts && prs.as_array().is_some_and(|a| a.iter().all(|p| p["state"] == "merged")) =>
            {
                prs
            }
            _ => match tokio::time::timeout(PR_LIST_DEADLINE, self.list_prs(id)).await.ok().flatten() {
                Some(prs) => {
                    self.closed_prs.lock().unwrap_or_else(|e| e.into_inner()).insert(id.into(), (counts, prs.clone()));
                    prs
                }
                None => {
                    th["pullRequestsStale"] = json!(true);
                    return th;
                }
            },
        };
        th["pullRequests"] = prs;
        th
    }

    async fn list_prs(&self, id: &str) -> Option<Value> {
        let mut prs = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_PR_PAGES {
            let mut args = json!({"threadId": id});
            if let Some(c) = &cursor {
                args["cursor"] = json!(c);
            }
            let r = self.t3.call("list_thread_pull_requests", args).await.ok()?;
            prs.extend(r["pullRequests"].as_array()?.iter().cloned());
            cursor = r["nextCursor"].as_str().filter(|c| !c.is_empty()).map(String::from);
            if cursor.is_none() {
                return Some(json!(prs));
            }
        }
        None
    }

    pub async fn dispatch(&self, method: &str, caller: Option<&str>, args: Value) -> Result<Value> {
        self.dispatch_from(method, caller, &crate::wf_slots::Reported::default(), args).await
    }

    pub async fn dispatch_from(
        &self,
        method: &str,
        caller: Option<&str>,
        reported: &crate::wf_slots::Reported,
        args: Value,
    ) -> Result<Value> {
        if matches!(method, "slot" | "slots" | "renewal") {
            return self.slot_call(method, caller.filter(|c| !c.is_empty()), reported, args).await;
        }
        let out = self.dispatch_inner(method, caller, args).await;
        if out.is_ok()
            && !matches!(method, "show" | "inbox" | "board" | "cards" | "feed" | "waiting.list" | "pools" | "status")
        {
            self.changed();
        }
        out
    }

    async fn dispatch_inner(&self, method: &str, caller: Option<&str>, args: Value) -> Result<Value> {
        match method {
            "show" => return self.show(&parse::<HandleArgs>(method, args)?.handle),
            "cards" => {
                let a: CardsArgs = parse(method, args)?;
                let filter = self.project_filter(a.context.as_deref(), a.projects)?;
                return Ok(json!(self.cards_in(a.all, filter.as_ref())?));
            }
            "feed" => {
                let a: FeedArgs = parse(method, args)?;
                let filter = self.project_filter(a.context.as_deref(), a.projects)?;
                return Ok(json!(self.feed_in(a.limit.unwrap_or(self.config.board.feed), filter.as_ref())?));
            }
            "answer" => {
                let a: AnswerArgs = parse(method, args)?;
                return self.answer(&a.id, &a.text).await;
            }
            "pick" => return Ok(serde_json::to_value(self.pick(&parse::<PickArgs>(method, args)?.pool).await?)?),
            "picked" => {
                let a: PickedArgs = parse(method, args)?;
                return self.picked(a.pick, &a.thread);
            }
            "pools" => {
                parse::<Empty>(method, args)?;
                return self.pools().await;
            }
            "status" => {
                parse::<Empty>(method, args)?;
                self.refresh_skills();
                return Ok(self.service_view());
            }
            "merge" => return self.merge(caller.filter(|c| !c.is_empty()), parse::<MergeArgs>(method, args)?).await,
            "launchTicket" => {
                let r = self.launch_ticket(parse::<LaunchArgs>(method, args)?).await?;
                if r["ok"] == false {
                    bail!("{} (recorded as board card {})", r["error"].as_str().unwrap_or("launch failed"), r["card"]);
                }
                return Ok(r);
            }
            _ => {}
        }
        let board_args = if method == "board" { Some(parse::<BoardArgs>(method, args.clone())?) } else { None };
        if let Some(BoardArgs { all, parent: Some(root) }) = board_args {
            return self.board_view(&root, all).await;
        }
        let me = caller.filter(|c| !c.is_empty()).ok_or_else(|| anyhow!(NO_CALLER))?;
        match method {
            "init" => self.init(me, parse(method, args)?).await,
            "openChild" => {
                let a: OpenChildArgs = parse(method, args)?;
                self.open_child(me, &a.role, &a.label).await
            }
            "join" => self.join(me, parse(method, args)?).await,
            "issue" => self.report_issue(me, parse::<IssueArgs>(method, args)?).await,
            "send" => self.send(me, parse(method, args)?).await,
            "reply" => self.reply(me, parse(method, args)?).await,
            "gate" => self.gate(me, parse(method, args)?).await,
            "close" => self.close(me, &parse::<ChildArgs>(method, args)?.child).await,
            "inbox" => {
                parse::<Empty>(method, args)?;
                self.inbox(me).await
            }
            "board" => self.board_of(me, board_args.unwrap_or_default().all).await,
            "adopt" => self.adopt(me, &parse::<AdoptArgs>(method, args)?.from).await,
            "sweep" => {
                parse::<Empty>(method, args)?;
                self.sweep(me).await
            }
            "waiting.add" => {
                let a: WaitAddArgs = parse(method, args)?;
                self.waiting_add(me, &a.text, a.from, a.link)
            }
            "waiting.resolve" => {
                let a: WaitResolveArgs = parse(method, args)?;
                self.waiting_resolve(me, &a.id, a.note.as_deref())
            }
            "waiting.list" => {
                parse::<Empty>(method, args)?;
                Ok(json!(self.store.waiting_open(me)?))
            }
            other => bail!("wf has no method {other}; search the catalog with kind \"wf\""),
        }
    }

    async fn init(&self, me: &str, a: InitArgs) -> Result<Value> {
        self.config.check_role(&a.role)?;
        let guard = self.lock.lock().await;
        let open = self.store.link_get(me)?.filter(|l| !l.closed);
        if !a.root && open.is_none() && self.config.board_oversees(&a.role) {
            drop(guard);
            let thread = self.read_thread(me).await.ok();
            let usable = |l: &str| Some(l.trim().to_string()).filter(|l| !l.is_empty());
            let label = a
                .label
                .as_deref()
                .and_then(usable)
                .or_else(|| thread.as_ref().and_then(|t| t["title"].as_str()).and_then(usable))
                .unwrap_or_else(|| me.to_string());
            let args = JoinArgs {
                parent: BOARD.into(),
                label: label.clone(),
                role: a.role.clone(),
                down: None,
                miss: None,
                stall: None,
            };
            let joined = self.bind(me, args, thread.as_ref(), true).await?;
            return Ok(json!({"thread": me, "role": a.role, "root": false, "parent": joined["parent"],
                "label": joined["label"], "watcher": self.watcher_line()}));
        }
        self.store.node_set(me, &a.role, &self.now())?;
        let mut out = json!({"thread": me, "role": a.role, "root": open.is_none(), "watcher": self.watcher_line()});
        if let Some(l) = open {
            out["parent"] = json!(l.parent);
            out["label"] = json!(l.label);
        }
        Ok(out)
    }

    async fn open_child(&self, me: &str, role: &str, label: &str) -> Result<Value> {
        if label.trim().is_empty() {
            bail!("wf.openChild: label must not be empty");
        }
        let _g = self.lock.lock().await;
        let my_role = self
            .store
            .node_role(me)?
            .ok_or_else(|| anyhow!("call wf.init({{role}}) (no parent) or your brief's wf.join(...) first"))?;
        let params = self.config.link_params(&my_role, role)?;
        self.store.pending_put(&Pending {
            parent: me.into(),
            label: label.into(),
            role: role.into(),
            params,
            at: self.now(),
            reported: false,
            thread: None,
            link: None,
        })?;
        let call = format!(
            "async () => wf.join({{ parent: {}, label: {}, role: {}, down: {}, miss: {}, stall: {} }})",
            json!(me),
            json!(label),
            json!(role),
            params.down,
            params.miss,
            params.stall
        );
        let brief = format!(
            "## t3-codemode link\n\
             This thread is a child on a t3-codemode link. Before anything else, call the t3cm `execute` tool with:\n\
             {call}\n\
             Report routine progress with wf.send({{ kind: \"note\", body }}) (it never wakes the parent); \
             event, gate, blocker and handoff wake it. Finish with wf.send({{ kind: \"handoff\", body }})."
        );
        Ok(json!({"brief": brief, "params": params}))
    }

    pub async fn note_launch(&self, parent: &str, label: &str, launched: &Value) -> Result<()> {
        let Some(thread) = launched["threadId"].as_str() else {
            return Ok(());
        };
        let _g = self.lock.lock().await;
        let row = self.store.pending_all()?.into_iter().find(|p| p.parent == parent && p.label == label);
        if let Some(mut p) = row.filter(|p| p.thread.is_none()) {
            p.thread = Some(thread.into());
            p.link = launched["link"].as_str().map(String::from);
            self.store.pending_put(&p)?;
        }
        Ok(())
    }

    async fn join(&self, me: &str, a: JoinArgs) -> Result<Value> {
        self.config.check_role(&a.role)?;
        let thread = self.read_thread(me).await.ok();
        self.bind(me, a, thread.as_ref(), false).await
    }

    async fn bind(&self, me: &str, a: JoinArgs, thread: Option<&Value>, auto: bool) -> Result<Value> {
        let field = |k: &str| thread.and_then(|t| t[k].as_str()).map(String::from);
        let (link_url, project) = (field("link"), field("projectId"));
        let _g = self.lock.lock().await;
        let parent_role = self.store.node_role(&a.parent)?;
        let base =
            parent_role.as_deref().and_then(|r| self.config.link_params(r, &a.role).ok()).unwrap_or(self.config.link);
        let params = LinkParams {
            down: a.down.unwrap_or(base.down),
            miss: a.miss.unwrap_or(base.miss),
            stall: a.stall.unwrap_or(base.stall),
        };
        params.validate().map_err(|e| anyhow!("the brief's link numbers are invalid: {e}"))?;
        let now = self.now();
        let launched_by = auto.then_some(BOARD);
        let bound = self.store.link_bind_launched(me, !auto, launched_by, |current, launch| {
            if auto && let Some(l) = current.as_ref().filter(|l| !l.closed) {
                return Bind::Kept(l.clone());
            }
            let old = current.filter(|l| l.parent == a.parent);
            Bind::Saved(Link {
                child: me.into(),
                parent: a.parent.clone(),
                label: launch.map(|p| p.label.clone()).unwrap_or_else(|| a.label.clone()),
                role: a.role.clone(),
                parent_role,
                params,
                link: link_url.or_else(|| old.as_ref().and_then(|o| o.link.clone())),
                opened: old.as_ref().map(|o| o.opened.clone()).unwrap_or_else(|| now.clone()),
                last_up: now.clone(),
                last_seen: None,
                last_nudge: None,
                nudges: 0,
                escalated: false,
                reported: old.as_ref().filter(|_| !auto).map(|o| o.reported.clone()).unwrap_or_default(),
                gate: old.as_ref().filter(|_| !auto).and_then(|o| o.gate.clone()),
                gate_since: old.as_ref().filter(|_| !auto).and_then(|o| o.gate_since.clone()),
                closed: false,
                closed_at: None,
                closed_how: None,
                unread: old.as_ref().map(|o| o.unread.clone()).unwrap_or_default(),
                bad_run: None,
                settled_at: None,
                restored_at: None,
                project: project.or_else(|| old.as_ref().and_then(|o| o.project.clone())),
            })
        })?;
        let ln = match bound {
            Bind::Kept(l) => return Ok(json!({"parent": l.parent, "label": l.label})),
            Bind::Saved(l) => l,
        };
        Ok(json!({
            "parent": ln.parent, "label": ln.label, "params": params,
            "message": format!("Joined. The watcher nudges this thread after {} idle minutes with no gate open; \
                there are no timers or check-ins to keep. Routine progress: wf.send({{kind: \"note\"}}).", params.down)
        }))
    }

    async fn send(&self, me: &str, a: SendArgs) -> Result<Value> {
        if !UP_KINDS.contains(&a.kind.as_str()) {
            bail!("wf.send: kind must be one of {}", UP_KINDS.join(", "));
        }
        if a.body.trim().is_empty() {
            bail!("wf.send: empty body");
        }
        let options = match &a.options {
            Some(_) if !OPTION_KINDS.contains(&a.kind.as_str()) => {
                bail!("wf.send: options apply only to kinds {}", OPTION_KINDS.join(", "))
            }
            Some(o) => Some(checked_options(o)?),
            None => None,
        };
        let _g = self.lock.lock().await;
        let mut ln = self
            .store
            .link_get(me)?
            .ok_or_else(|| anyhow!("this thread has no parent link; it reports to the user directly"))?;
        if ln.closed {
            bail!("the link to your parent is closed; nothing to send on it");
        }
        if CLOSING.contains(&a.kind.as_str())
            && let Err(e) = self.read_thread(me).await
            && !e.is_not_found()
        {
            self.live.lock().unwrap_or_else(|e| e.into_inner()).remove(me);
        }
        let now = self.now();
        if a.kind == "gate" {
            ln.gate = Some(gate_name(a.gate.as_deref().unwrap_or("user")));
            ln.gate_since = Some(now.clone());
        }
        let handle = self.store.msg_new(me, &ln.parent, &a.kind, &a.body, &now)?;
        ln.last_up = now.clone();
        ln.nudges = 0;
        ln.escalated = false;
        let (wake_kinds, cap) = self.config.link_policy(ln.parent_role.as_deref(), &ln.role);
        let mut wake = a.wake.unwrap_or_else(|| wake_kinds.contains(&a.kind));
        let closing = CLOSING.contains(&a.kind.as_str());
        if closing {
            ln.closed = true;
            ln.closed_at = Some(now.clone());
            ln.closed_how = Some(a.kind.clone());
            ln.gate = None;
            wake = true;
        }
        if ln.parent == BOARD {
            self.store.link_save(&ln)?;
            return self.board_receive(&ln, &a.kind, &handle, &a.body, options);
        }
        if !wake {
            ln.unread.push(handle.clone());
            self.store.link_save(&ln)?;
            return Ok(json!({"handle": handle, "delivery": "noted",
                "detail": "no wake; your parent sees it in the digest of its next wake"}));
        }
        let text =
            format!("{}\n{}", header(&a.kind, &ln.label, &handle, ln.gate.as_deref()), capped(&a.body, &handle, cap));
        self.store.link_save(&ln)?;
        let mut out = self.send_wake(&ln.parent, &text, &handle).await?;
        if closing {
            out["closed"] = json!(true);
        }
        Ok(out)
    }

    async fn reply(&self, me: &str, a: ReplyArgs) -> Result<Value> {
        if a.body.trim().is_empty() {
            bail!("wf.reply: empty body");
        }
        let _g = self.lock.lock().await;
        let ln = self.find_child(me, &a.to)?;
        if ln.closed && !a.reopen {
            bail!("the link to {} is closed; pass reopen: true to send anyway (it starts a new turn there)", ln.label);
        }
        let handle = self.store.msg_new(me, &ln.child, "reply", &a.body, &self.now())?;
        let text = format!(
            "{}\n{}",
            header("reply", "parent", &handle, None),
            capped(&a.body, &handle, self.config.reply_cap)
        );
        self.send_wake(&ln.child, &text, &handle).await
    }

    async fn gate(&self, me: &str, a: GateArgs) -> Result<Value> {
        if a.clear == a.open.is_some() {
            bail!("wf.gate takes {{open: \"<what>\"}} or {{clear: true}}");
        }
        let _g = self.lock.lock().await;
        let mut ln = self
            .store
            .link_get(me)?
            .filter(|l| !l.closed)
            .ok_or_else(|| anyhow!("this thread has no open parent link to hold a gate on"))?;
        ln.gate = a.open.as_deref().map(gate_name);
        ln.gate_since = ln.gate.as_ref().map(|_| self.now());
        ln.nudges = 0;
        ln.escalated = false;
        self.store.link_save(&ln)?;
        if a.clear && ln.parent == BOARD {
            self.resolve_cards(
                |c| c.kind == "gate" && c.ticket.as_deref() == Some(ln.child.as_str()),
                "gate cleared in the ticket",
            )?;
        }
        Ok(json!({"gate": ln.gate}))
    }

    fn find_child(&self, me: &str, reference: &str) -> Result<Link> {
        self.store
            .children(me, false)?
            .into_iter()
            .rev()
            .find(|l| l.child == reference || l.label == reference)
            .ok_or_else(|| anyhow!("{reference:?} is not a child link of this thread; wf.board() lists them"))
    }

    async fn close(&self, me: &str, child: &str) -> Result<Value> {
        let _g = self.lock.lock().await;
        let mut ln = self.find_child(me, child)?;
        ln.closed = true;
        ln.closed_at = Some(self.now());
        ln.closed_how = Some("closed-by-parent".into());
        self.store.link_save(&ln)?;
        Ok(json!({"closed": ln.label}))
    }

    async fn inbox(&self, me: &str) -> Result<Value> {
        let _g = self.lock.lock().await;
        let mut out = Vec::new();
        for mut ln in self.store.children(me, false)? {
            if ln.unread.is_empty() {
                continue;
            }
            for h in &ln.unread {
                if let Some(m) = self.store.msg_get(h)? {
                    out.push(json!({"handle": h, "at": local_clock(&m.at), "label": ln.label, "kind": m.kind, "head": m.head(160)}));
                }
            }
            ln.unread.clear();
            self.store.link_save(&ln)?;
        }
        Ok(Value::Array(out))
    }

    fn show(&self, handle: &str) -> Result<Value> {
        let m = self
            .store
            .msg_get(handle)?
            .ok_or_else(|| anyhow!("no message {handle:?} (only the most recent 5000 are kept)"))?;
        Ok(json!(m))
    }

    pub(crate) async fn board_of(&self, me: &str, all: bool) -> Result<Value> {
        let (parent, kids) = {
            let _g = self.lock.lock().await;
            (self.store.link_get(me)?, self.store.children(me, !all)?)
        };
        let mut children = Vec::new();
        for k in kids {
            let live = if k.closed { None } else { self.read_thread(&k.child).await.ok() };
            let status = match (&live, k.closed) {
                (_, true) => k.closed_how.clone().unwrap_or_else(|| "closed".into()),
                (Some(th), _) => th["status"].as_str().unwrap_or("?").to_string(),
                (None, _) => "unreachable".into(),
            };
            let live = live.unwrap_or(Value::Null);
            let last = self.store.msg_last_from(&k.child)?;
            children.push(json!({
                "label": k.label, "role": k.role, "thread": k.child, "link": k.link.clone().or_else(|| live["link"].as_str().map(String::from)),
                "status": status, "asks": count(&live["pendingRequestCount"]), "background": count(&live["pendingBackgroundTasks"]),
                "gate": k.gate, "lastUp": local_clock(&k.last_up), "nudges": k.nudges, "unread": k.unread.len(),
                "settled": k.settled_at.is_some(),
                "last": last.map(|m| json!({"handle": m.handle, "kind": m.kind, "head": m.head(90)})),
            }));
        }
        let parent = parent.map(|p| json!({"thread": p.parent, "label": p.label, "closed": p.closed, "gate": p.gate}));
        Ok(json!({"parent": parent, "children": children}))
    }

    async fn adopt(&self, me: &str, from: &str) -> Result<Value> {
        let _g = self.lock.lock().await;
        let mut adopted = Vec::new();
        for mut ln in self.store.children(from, true)? {
            ln.parent = me.into();
            self.store.link_save(&ln)?;
            adopted.push(ln.label);
        }
        let pending = self.store.pending_retarget(from, me)?;
        self.store.outbox_retarget(from, me)?;
        Ok(json!({"adopted": adopted, "pending": pending}))
    }

    async fn sweep(&self, me: &str) -> Result<Value> {
        let _g = self.lock.lock().await;
        let open: Vec<String> = self.store.children(me, true)?.into_iter().map(|l| l.label).collect();
        let mut dropped = Vec::new();
        for p in self.store.pending_all()?.into_iter().filter(|p| p.parent == me) {
            self.store.pending_remove(&p.parent, &p.label)?;
            dropped.push(p.label);
        }
        let parent_open = self.store.link_get(me)?.is_some_and(|l| !l.closed);
        Ok(json!({"open": open, "dropped": dropped, "parentOpen": parent_open}))
    }

    fn waiting_add(&self, me: &str, text: &str, from: Option<String>, link: Option<String>) -> Result<Value> {
        if text.trim().is_empty() {
            bail!("wf.waiting.add: empty text");
        }
        Ok(json!({"id": self.alloc_waiting(me, text, from, link, None)?}))
    }

    fn waiting_resolve(&self, me: &str, id: &str, note: Option<&str>) -> Result<Value> {
        match self.store.waiting_resolve(me, id, note, &self.now())? {
            Some(text) => Ok(json!({"id": id, "text": text, "resolved": true})),
            None => bail!("no open waiting-list item {id:?}; wf.waiting.list() shows the open ones"),
        }
    }

    pub(crate) async fn flush_to(&self, to: &str) -> Result<()> {
        let _g = self.lock.lock().await;
        self.flush_outbox(Some(to), !self.watcher_alive()).await.map(|_| ())
    }

    pub(crate) async fn send_wake(&self, to: &str, text: &str, handle: &str) -> Result<Value> {
        self.store.outbox_push(to, text, handle, &self.now())?;
        let force = !self.watcher_alive();
        let f = self.flush_outbox(Some(to), force).await?;
        if !f.sent.is_empty() {
            return Ok(json!({"handle": handle, "delivery": "sent"}));
        }
        let detail = match f.errors.first() {
            Some(e) => format!("delivery failed ({e}); the watcher retries"),
            None => "the recipient is mid-turn; the watcher delivers it, merged with anything else queued, when the turn ends".into(),
        };
        Ok(json!({"handle": handle, "delivery": "queued", "detail": detail}))
    }

    fn digest_for(&self, parent: &str, exclude: &[String]) -> Result<(String, Consumed)> {
        let mut lines = Vec::new();
        let mut consumed = Vec::new();
        let mut overflow = 0;
        for ln in self.store.children(parent, false)? {
            let mut unread = Vec::new();
            for h in ln.unread.iter().filter(|h| !exclude.contains(h)) {
                if let Some(m) = self.store.msg_get(h)? {
                    unread.push(m);
                }
            }
            let Some(last) = unread.last() else { continue };
            if lines.len() >= self.config.digest_lines {
                overflow += 1;
                continue;
            }
            let more = if unread.len() > 1 { format!(" (+{} earlier)", unread.len() - 1) } else { String::new() };
            lines.push(format!("- {} {} {} [{}]{more}", ln.label, local_clock(&last.at), last.head(120), last.handle));
            consumed.push((ln.child.clone(), unread.iter().map(|m| m.handle.clone()).collect()));
        }
        if overflow > 0 {
            lines.push(format!("- ... {overflow} more threads with notes: wf.inbox()"));
        }
        if lines.is_empty() {
            return Ok((String::new(), consumed));
        }
        Ok((
            format!("\n\nNotes since your last wake (wf.show(\"<msg>\") for one in full):\n{}", lines.join("\n")),
            consumed,
        ))
    }

    async fn flush_outbox(&self, only_to: Option<&str>, force: bool) -> Result<Flush> {
        self.store.outbox_drop_to(BOARD)?;
        let mut groups: Vec<(String, Vec<crate::store::OutboxItem>)> = Vec::new();
        for item in self.store.outbox_all()? {
            match groups.iter_mut().find(|(to, _)| *to == item.to) {
                Some((_, g)) => g.push(item),
                None => groups.push((item.to.clone(), vec![item])),
            }
        }
        let mut f = Flush { sent: Vec::new(), errors: Vec::new() };
        for (to, items) in groups {
            if only_to.is_some_and(|o| o != to) {
                continue;
            }
            if !force && let Ok(th) = self.read_thread(&to).await {
                let held = self.mins(Some(&items[0].queued)).unwrap_or(0.0);
                if ACTIVE.contains(&th["status"].as_str().unwrap_or("")) && held < self.config.max_hold_minutes {
                    continue;
                }
            }
            let handles: Vec<String> = items.iter().map(|i| i.handle.clone()).collect();
            let mut text = items.iter().map(|i| i.text.as_str()).collect::<Vec<_>>().join("\n\n");
            let (digest, consumed) = self.digest_for(&to, &handles)?;
            text.push_str(&digest);
            match self.t3.call("t3_thread_send", json!({"threadId": to, "message": text, "mode": "queue"})).await {
                Err(e) => f.errors.push(format!("{to}: {e}")),
                Ok(_) => {
                    for (child, done) in consumed {
                        if let Some(mut l) = self.store.link_get(&child)? {
                            l.unread.retain(|h| !done.contains(h));
                            self.store.link_save(&l)?;
                        }
                    }
                    self.store.outbox_delete(&items.iter().map(|i| i.id).collect::<Vec<_>>())?;
                    f.sent.push((to, handles));
                }
            }
        }
        Ok(f)
    }

    fn report_once(&self, ln: &mut Link, key: String, kind: &str, text: &str, actions: &mut Vec<String>) -> Result<()> {
        if ln.reported.contains(&key) {
            return Ok(());
        }
        ln.reported.push(key.clone());
        let handle = self.store.msg_new(WATCHER, &ln.parent, kind, text, &self.now())?;
        self.deliver_up(ln, kind, &handle, text)?;
        actions.push(format!("{kind} to {} about {}: {key}", ln.parent, ln.label));
        Ok(())
    }

    fn deliver_up(&self, ln: &Link, kind: &str, handle: &str, body: &str) -> Result<()> {
        if ln.parent == BOARD {
            self.board_card(NewCard {
                kind,
                label: Some(&ln.label),
                ticket: Some(&ln.child),
                link: ln.link.as_deref(),
                body,
                handle: Some(handle),
                options: None,
                pr: None,
                approval: None,
                offer: None,
            })?;
            return Ok(());
        }
        let msg = format!("{}\n{body}", header(kind, &ln.label, handle, None));
        self.store.outbox_push(&ln.parent, &msg, handle, &self.now())
    }

    fn where_is(ln: &Link) -> String {
        ln.link.clone().unwrap_or_else(|| ln.child.clone())
    }

    async fn tick_link(&self, ln: &mut Link, actions: &mut Vec<String>) -> Result<()> {
        let p = ln.params;
        let now = self.now();
        let th = match self.read_thread(&ln.child).await {
            Ok(th) => th,
            Err(e) if e.is_not_found() => {
                ln.closed = true;
                ln.closed_at = Some(now.clone());
                ln.closed_how = Some("thread-gone".into());
                let body = format!("{}'s thread is gone; its link is closed.", ln.label);
                let h = self.store.msg_new(WATCHER, &ln.parent, "note", &body, &now)?;
                if ln.parent == BOARD {
                    self.resolve_ticket_cards(&ln.child, "ticket thread gone")?;
                } else {
                    ln.unread.push(h);
                }
                actions.push(format!("closed {}: thread gone", ln.label));
                return Ok(());
            }
            Err(_) => return Ok(()),
        };
        if th["archived"] == true {
            ln.closed = true;
            ln.closed_at = Some(now);
            ln.closed_how = Some("archived".into());
            if ln.parent == BOARD {
                self.resolve_ticket_cards(&ln.child, "ticket archived")?;
            }
            actions.push(format!("closed {}: archived", ln.label));
            return Ok(());
        }
        if ln.link.is_none() {
            ln.link = th["link"].as_str().map(String::from);
        }
        let updated = th["updatedAt"].as_str();
        ln.last_seen = updated.map(String::from);
        let status = th["status"].as_str().unwrap_or("");
        let busy =
            ACTIVE.contains(&status) || truthy(&th["blockedOnThreadWait"]) || truthy(&th["pendingBackgroundTasks"]);
        let idle = self.mins(updated).unwrap_or(0.0);
        let latest_run = th["latestRunId"].as_str().map(String::from);
        if truthy(&th["pendingRequestCount"]) || status == "waiting" {
            if idle >= p.down as f64 {
                let text = format!(
                    "{} is waiting on an answer to a question or approval in its own thread for {idle:.0} min: {}",
                    ln.label,
                    Self::where_is(ln)
                );
                let key = format!("asks@{}", latest_run.as_deref().unwrap_or("-"));
                self.report_once(ln, key, "report", &text, actions)?;
            }
            return Ok(());
        }
        if busy {
            let quiet = self.mins(Some(&ln.last_up)).unwrap_or(0.0);
            if ln.gate.is_none() && quiet >= p.stall as f64 {
                let text = format!(
                    "{} has been busy but sent nothing for {quiet:.0} min. t3.t3_thread_read shows where it is: {}",
                    ln.label,
                    Self::where_is(ln)
                );
                let key = format!("stall@{}", ln.last_up);
                self.report_once(ln, key, "report", &text, actions)?;
            }
            return Ok(());
        }
        if ln.gate.is_some() {
            return Ok(());
        }
        let bad_end = ENDED_BADLY.contains(&status) && ln.bad_run != latest_run;
        let since_nudge = ln.last_nudge.as_deref().and_then(|t| self.mins(Some(t)));
        let due = bad_end || (idle >= p.down as f64 && since_nudge.is_none_or(|s| s >= p.down as f64));
        if !due {
            return Ok(());
        }
        if ln.nudges >= p.miss {
            if !ln.escalated {
                ln.escalated = true;
                let body = format!(
                    "{} has sat idle for {idle:.0} min through {} nudges, with no gate open. Look at it \
                     (t3.t3_thread_read), then wf.reply, relaunch it, or wf.close({{child: {}}}). Its thread: {}",
                    ln.label,
                    ln.nudges,
                    json!(ln.child),
                    Self::where_is(ln)
                );
                let handle = self.store.msg_new(WATCHER, &ln.parent, "escalate", &body, &now)?;
                self.deliver_up(ln, "escalate", &handle, &body)?;
                actions.push(format!("escalate {} to {}", ln.label, ln.parent));
            }
            return Ok(());
        }
        ln.nudges += 1;
        ln.last_nudge = Some(now.clone());
        if bad_end {
            ln.bad_run = latest_run;
        }
        let why = if bad_end {
            format!("Your last run ended {status}.")
        } else {
            format!("This thread has been idle {idle:.0} min with its link to its parent open and no gate.")
        };
        let handle = self.store.msg_new(WATCHER, &ln.child, "nudge", &why, &now)?;
        let text = format!(
            "{}\n{why} Run your role's resume step, then tell your parent what changed with wf.send \
             (kind \"note\" for routine progress). Waiting on a person: wf.gate({{open: \"<what>\"}}). \
             Done: wf.send({{kind: \"handoff\", body}}). Stopped for good: wf.send({{kind: \"closed\", body}}). \
             Nudge {} of {}.",
            header("nudge", "watcher", &handle, None),
            ln.nudges,
            p.miss
        );
        actions.push(format!("nudge {} ({}/{})", ln.label, ln.nudges, p.miss));
        let sent =
            self.t3.call("t3_thread_send", json!({"threadId": ln.child, "message": text, "mode": "queue"})).await;
        if sent.is_err() {
            self.store.outbox_push(&ln.child, &text, &handle, &now)?;
        }
        Ok(())
    }

    pub async fn tick(&self) -> Result<Vec<String>> {
        let _g = self.lock.lock().await;
        let mut actions = Vec::new();
        let links = self.store.links_open()?;
        let pending = self.store.pending_all()?;
        if links.is_empty() && pending.is_empty() && self.store.outbox_all()?.is_empty() {
            self.mark_pass()?;
            self.changed();
            return Ok(actions);
        }
        for mut ln in links {
            self.tick_link(&mut ln, &mut actions).await?;
            self.store.link_save(&ln)?;
        }
        for mut p in pending.into_iter().filter(|p| !p.reported) {
            let age = self.mins(Some(&p.at)).unwrap_or(0.0);
            if age < p.params.down as f64 {
                continue;
            }
            p.reported = true;
            let thread = p
                .link
                .as_deref()
                .filter(|_| p.parent != BOARD)
                .map(|l| format!(" Its thread: {l}."))
                .unwrap_or_default();
            let body = format!(
                "{} was opened {age:.0} min ago and never joined.{thread} Check its launch; wf.sweep() drops it.",
                p.label
            );
            let now = self.now();
            let handle = self.store.msg_new(WATCHER, &p.parent, "report", &body, &now)?;
            if p.parent == BOARD {
                self.board_card(NewCard {
                    kind: "report",
                    label: Some(&p.label),
                    ticket: p.thread.as_deref(),
                    link: p.link.as_deref(),
                    body: &body,
                    handle: Some(&handle),
                    options: None,
                    pr: None,
                    approval: None,
                    offer: None,
                })?;
            } else {
                let msg = format!("{}\n{body}", header("report", &p.label, &handle, None));
                self.store.outbox_push(&p.parent, &msg, &handle, &now)?;
            }
            self.store.pending_put(&p)?;
            actions.push(format!("report never-joined {}", p.label));
        }
        let f = self.flush_outbox(None, false).await?;
        actions.extend(f.sent.iter().map(|(to, h)| format!("delivered {} to {to}", h.join("+"))));
        actions.extend(f.errors.iter().map(|e| format!("delivery failed: {e}")));
        self.mark_pass()?;
        self.changed();
        Ok(actions)
    }
}
