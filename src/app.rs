use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::board::{GitRemotes, Remotes};
use crate::catalog::{Catalog, aliases};
use crate::clock::{Clock, iso};
use crate::config::{Config, OpenLinks};
use crate::desktop::{Desktop, WindowsDesktop, app_link};
use crate::github::{GhCli, GitHub};
use crate::sandbox::{self, Host, Limits, NoHost, Run};
use crate::store::Store;
use crate::t3::{BoxFut, T3Client};
use crate::upstream::{T3_NAMESPACE, UpstreamManager, qualified, redact_json, split_qualified};
use crate::wf::{LAST_PASS, Wf, join_target};
use crate::{js, truncate};

pub const VARS_KEY: &str = "__vars__";
pub const EXEC_GLOBALS: &str = "t3, mem, vars, wf, console";
const EXEC_TAIL: &str = "tools, mem, vars, wf, console";
pub const SEARCH_GLOBALS: &str = "catalog, console";
const TOOLS_CACHE: &str = "t3_tools";
const MAX_VALUE: usize = 1024 * 1024;
const REFRESH_BACKOFF: Duration = Duration::from_secs(30);
pub const RUN_CONNECT_WAIT: Duration = Duration::from_secs(2);

const EXEC_PRELUDE: &str = r#"
(() => {
  const host = globalThis.__host;
  const names = globalThis.__names;
  const upstreams = globalThis.__upstreams;
  delete globalThis.__host;
  delete globalThis.__names;
  delete globalThis.__upstreams;
  const call = async (op, args) => {
    const r = JSON.parse(await host(op, JSON.stringify(args === undefined ? null : args)));
    if (!r.ok) throw new Error(r.error);
    return r.value;
  };
  const input = (x) => (x === undefined ? null : x);
  const tool = (ns, name, x) => call("call", { ns, name: String(name), input: input(x) });
  const define = (k, v) =>
    Object.defineProperty(globalThis, k, { value: v, writable: false, enumerable: true, configurable: false });
  const t3 = { call: (name, x) => tool("t3", name, x) };
  for (const n of names) t3[n] = (x) => tool("t3", n, x);
  for (const n of names) {
    if (n.startsWith("t3_") && !(n.slice(3) in t3)) t3[n.slice(3)] = t3[n];
  }
  define("t3", Object.freeze(t3));
  for (const [ns, members] of upstreams) {
    const api = {};
    const put = (k, v) => Object.defineProperty(api, k, { value: v, enumerable: true });
    put("call", (name, x) => tool(ns, name, x));
    for (const [n, alias] of members) {
      if (n === "call") continue;
      const f = (x) => tool(ns, n, x);
      put(n, f);
      if (alias !== null) put(alias, f);
    }
    define(ns, Object.freeze(api));
  }
  define("tools", Object.freeze({ call: (name, x) => call("call", { qualified: String(name), input: input(x) }) }));
  const memApi = (ns) => Object.freeze({
    get: (key) => call("mem.get", { ns, key }),
    set: (key, value) => call("mem.set", { ns, key, value }),
    del: (key) => call("mem.del", { ns, key }),
    list: (prefix) => call("mem.list", { ns, prefix }),
    ns: (name) => memApi(String(name)),
  });
  const m = (name) => (x) => call("wf." + name, input(x));
  const wf = Object.freeze({
    init: m("init"), openChild: m("openChild"), join: m("join"), send: m("send"), reply: m("reply"),
    gate: m("gate"), close: m("close"), inbox: m("inbox"), board: m("board"), adopt: m("adopt"), sweep: m("sweep"),
    show: (handle) => call("wf.show", { handle }),
    launchTicket: m("launchTicket"), cards: m("cards"), feed: m("feed"), merge: m("merge"),
    pick: m("pick"), picked: m("picked"), pools: m("pools"), issue: m("issue"), status: m("status"),
    slot: (name, x) => call("wf.slot", Object.assign({}, input(x) || {}, { name })),
    slots: m("slots"), renewal: m("renewal"),
    answer: (id, text) => call("wf.answer", { id, text }),
    waiting: Object.freeze({
      add: m("waiting.add"),
      list: m("waiting.list"),
      resolve: (id, note) => call("wf.waiting.resolve", note === undefined ? { id } : { id, note }),
    }),
  });
  define("mem", memApi(null));
  define("wf", wf);
})();
"#;

const SEARCH_PRELUDE: &str = "delete globalThis.__host;";

pub struct ExecuteRequest {
    pub code: String,
    pub session: Option<String>,
    pub timeout_ms: Option<u64>,
    pub caller: Option<String>,
    pub reported: crate::wf_slots::Reported,
}

#[derive(Debug, Clone)]
pub struct ToolOutput {
    pub is_error: bool,
    pub text: String,
}

pub struct App {
    pub config: Arc<Config>,
    pub store: Arc<Store>,
    pub t3: Arc<dyn T3Client>,
    pub clock: Arc<dyn Clock>,
    pub wf: Arc<Wf>,
    pub upstreams: Arc<UpstreamManager>,
    pub desktop: Arc<dyn Desktop>,
    pub csrf: String,
    pub stopping: tokio::sync::watch::Sender<bool>,
    built: RwLock<Built>,
    last_refresh: Mutex<Option<Instant>>,
    started: Instant,
    runs: Arc<Semaphore>,
    queued: AtomicUsize,
}

struct Built {
    catalog: Arc<Catalog>,
    t3_tools: Arc<Vec<Value>>,
    generation: u64,
}

fn exec_globals(namespaces: &[String]) -> String {
    if namespaces.is_empty() { EXEC_GLOBALS.to_string() } else { format!("t3, {}, {EXEC_TAIL}", namespaces.join(", ")) }
}

fn object_input(qualified: &str, input: Option<&Value>) -> Result<Value, String> {
    match input {
        Some(v @ Value::Object(_)) => Ok(v.clone()),
        Some(Value::Null) | None => Ok(json!({})),
        Some(_) => Err(format!("{qualified}(input): input must be an object")),
    }
}

fn csrf_token() -> String {
    let mut b = [0u8; 24];
    getrandom::fill(&mut b).expect("system randomness");
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn nonempty(s: Option<String>) -> Option<String> {
    s.map(|x| x.trim().to_string()).filter(|x| !x.is_empty())
}

fn error_output(msg: &str, logs: Vec<String>, globals: &str) -> ToolOutput {
    let mut text = format!("Error: {msg}");
    if msg.contains("is not defined") {
        text.push_str(&format!("\nAvailable globals: {globals}."));
    }
    if !logs.is_empty() {
        text.push_str("\nLogs:\n");
        text.push_str(&logs.join("\n"));
    }
    ToolOutput { is_error: true, text }
}

struct Outside {
    remotes: Arc<dyn Remotes>,
    github: Arc<dyn GitHub>,
    desktop: Arc<dyn Desktop>,
}

impl App {
    pub fn new(config: Config, store: Arc<Store>, t3: Arc<dyn T3Client>, clock: Arc<dyn Clock>) -> Arc<App> {
        let upstreams = Arc::new(UpstreamManager::new(config.upstreams.clone()));
        App::build(config, store, t3, clock, Arc::new(GitRemotes), upstreams)
    }

    pub fn with_upstreams(
        config: Config,
        store: Arc<Store>,
        t3: Arc<dyn T3Client>,
        clock: Arc<dyn Clock>,
        upstreams: Arc<UpstreamManager>,
    ) -> Arc<App> {
        App::build(config, store, t3, clock, Arc::new(GitRemotes), upstreams)
    }

    pub fn with_remotes(
        config: Config,
        store: Arc<Store>,
        t3: Arc<dyn T3Client>,
        clock: Arc<dyn Clock>,
        remotes: Arc<dyn Remotes>,
    ) -> Arc<App> {
        App::with_host(config, store, t3, clock, remotes, Arc::new(GhCli), Arc::new(WindowsDesktop))
    }

    pub fn with_host(
        config: Config,
        store: Arc<Store>,
        t3: Arc<dyn T3Client>,
        clock: Arc<dyn Clock>,
        remotes: Arc<dyn Remotes>,
        github: Arc<dyn GitHub>,
        desktop: Arc<dyn Desktop>,
    ) -> Arc<App> {
        let upstreams = Arc::new(UpstreamManager::new(config.upstreams.clone()));
        App::assemble(config, store, t3, clock, upstreams, Outside { remotes, github, desktop })
    }

    pub fn build(
        config: Config,
        store: Arc<Store>,
        t3: Arc<dyn T3Client>,
        clock: Arc<dyn Clock>,
        remotes: Arc<dyn Remotes>,
        upstreams: Arc<UpstreamManager>,
    ) -> Arc<App> {
        App::assemble(
            config,
            store,
            t3,
            clock,
            upstreams,
            Outside { remotes, github: Arc::new(GhCli), desktop: Arc::new(WindowsDesktop) },
        )
    }

    fn assemble(
        config: Config,
        store: Arc<Store>,
        t3: Arc<dyn T3Client>,
        clock: Arc<dyn Clock>,
        upstreams: Arc<UpstreamManager>,
        Outside { remotes, github, desktop }: Outside,
    ) -> Arc<App> {
        let config = Arc::new(config);
        let wf = Arc::new(Wf::new(store.clone(), t3.clone(), clock.clone(), config.clone(), remotes, github));
        let cached: Vec<Value> =
            store.meta_get(TOOLS_CACHE).ok().flatten().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        let generation = upstreams.generation();
        let catalog = Arc::new(Catalog::build_with(&cached, &upstreams.cached_tools()));
        let runs = Arc::new(Semaphore::new(config.max_concurrent_runs));
        Arc::new(App {
            config,
            store,
            t3,
            clock,
            wf,
            upstreams,
            desktop,
            csrf: csrf_token(),
            stopping: tokio::sync::watch::channel(false).0,
            built: RwLock::new(Built { catalog, t3_tools: Arc::new(cached), generation }),
            last_refresh: Mutex::new(None),
            started: Instant::now(),
            runs,
            queued: AtomicUsize::new(0),
        })
    }

    pub async fn open_link(&self, link: &str) -> anyhow::Result<Value> {
        let url = app_link(link).ok_or_else(|| anyhow::anyhow!("not a T3 thread link: {link:?}"))?;
        let opened = self.config.board.open_links == OpenLinks::Desktop && self.desktop.open(&url).await;
        Ok(json!({"opened": opened}))
    }

    pub fn catalog(&self) -> Arc<Catalog> {
        self.built.read().unwrap().catalog.clone()
    }

    fn rebuild(&self, t3_tools: Option<Vec<Value>>) -> Arc<Catalog> {
        let generation = self.upstreams.generation();
        let upstream_tools = self.upstreams.cached_tools();
        let t3_tools = t3_tools.map(Arc::new).unwrap_or_else(|| self.built.read().unwrap().t3_tools.clone());
        let catalog = Arc::new(Catalog::build_with(&t3_tools, &upstream_tools));
        *self.built.write().unwrap() = Built { catalog: catalog.clone(), t3_tools, generation };
        catalog
    }

    async fn refresh_t3(&self) -> Result<usize, String> {
        *self.last_refresh.lock().unwrap() = Some(Instant::now());
        let tools = self.t3.list_tools().await.map_err(|e| e.to_string())?;
        if tools.is_empty() {
            return Ok(self.catalog().t3_names.len());
        }
        let _ = self.store.meta_set(TOOLS_CACHE, &Value::Array(tools.clone()).to_string());
        Ok(self.rebuild(Some(tools)).t3_names.len())
    }

    fn rebuild_if_stale(&self) {
        if self.built.read().unwrap().generation != self.upstreams.generation() {
            self.rebuild(None);
        }
    }

    pub async fn refresh_catalog(&self) -> Result<usize, String> {
        let (_, t3) = tokio::join!(self.upstreams.refresh(), self.refresh_t3());
        self.rebuild_if_stale();
        t3
    }

    pub async fn refresh_t3_catalog(&self) -> Result<usize, String> {
        let t3 = self.refresh_t3().await;
        self.rebuild_if_stale();
        t3
    }

    pub async fn refresh_upstreams(&self) {
        for (name, r) in self.upstreams.refresh().await {
            if let Err(e) = r {
                tracing::debug!(upstream = %name, "upstream refresh failed: {e}");
            }
        }
        self.rebuild_if_stale();
    }

    async fn catalog_for_run(&self) -> Arc<Catalog> {
        if self.catalog().t3_names.is_empty() {
            let stale = self.last_refresh.lock().unwrap().is_none_or(|t| t.elapsed() >= REFRESH_BACKOFF);
            if stale {
                let _ = self.refresh_t3().await;
            }
        }
        if !self.upstreams.namespaces().is_empty() {
            let _ = self.upstreams.tools_for_run(RUN_CONNECT_WAIT).await;
        }
        let built = self.built.read().unwrap();
        if built.generation == self.upstreams.generation() {
            return built.catalog.clone();
        }
        drop(built);
        self.rebuild(None)
    }

    pub fn redact(&self, text: String) -> String {
        self.upstreams.redact(self.t3.redact(text))
    }

    fn redact_value(&self, value: Value) -> Value {
        let r = self.upstreams.redactor();
        redact_json(value, &|s| r.apply(self.t3.redact(s)))
    }

    fn limits(&self, timeout_ms: Option<u64>) -> Limits {
        let ms = timeout_ms.unwrap_or(self.config.default_timeout_ms).clamp(100, self.config.max_timeout_ms);
        Limits {
            memory: sandbox::MEMORY_LIMIT,
            cpu: Duration::from_millis(self.config.cpu_budget_ms),
            wall: Duration::from_millis(ms),
        }
    }

    async fn run_slot(&self) -> Result<OwnedSemaphorePermit, String> {
        if let Ok(p) = self.runs.clone().try_acquire_owned() {
            return Ok(p);
        }
        self.queued.fetch_add(1, Ordering::Relaxed);
        let wait = Duration::from_millis(self.config.run_queue_ms);
        let got = tokio::time::timeout(wait, self.runs.clone().acquire_owned()).await;
        self.queued.fetch_sub(1, Ordering::Relaxed);
        match got {
            Ok(Ok(p)) => Ok(p),
            _ => Err(format!(
                "t3cm is busy: {} runs are in progress and none finished within {} ms; retry shortly",
                self.config.max_concurrent_runs, self.config.run_queue_ms
            )),
        }
    }

    pub fn session_ns(session: Option<String>, caller: Option<&str>) -> String {
        nonempty(session).unwrap_or_else(|| match caller {
            Some(c) => format!("thread:{c}"),
            None => "default".into(),
        })
    }

    pub async fn execute(self: &Arc<Self>, req: ExecuteRequest) -> ToolOutput {
        let namespaces = self.upstreams.namespaces();
        let globals = exec_globals(&namespaces);
        let js = match js::prepare(&req.code) {
            Ok(j) => j,
            Err(e) => return error_output(&e, Vec::new(), &globals),
        };
        let caller = nonempty(req.caller);
        let ns = App::session_ns(req.session, caller.as_deref());
        let vars =
            self.store.kv_get(&ns, VARS_KEY).ok().flatten().filter(Value::is_object).unwrap_or_else(|| json!({}));
        let catalog = self.catalog_for_run().await;
        let names = serde_json::to_string(&catalog.t3_names).unwrap_or_else(|_| "[]".into());
        let upstreams: Vec<Value> = namespaces.iter().map(|n| json!([n, aliases(&catalog.tools_of(n))])).collect();
        let host = Arc::new(ExecHost { app: self.clone(), caller, reported: req.reported, ns: ns.clone(), catalog });
        let _slot = match self.run_slot().await {
            Ok(p) => p,
            Err(e) => return error_output(&e, Vec::new(), &globals),
        };
        let out = sandbox::run(Run {
            js,
            prelude: EXEC_PRELUDE,
            globals: vec![
                ("__names", names),
                ("__upstreams", Value::Array(upstreams).to_string()),
                ("vars", vars.to_string()),
            ],
            host,
            limits: self.limits(req.timeout_ms),
            keep_vars: true,
        })
        .await;
        let mut logs: Vec<String> = out.logs.into_iter().map(|l| self.redact(l)).collect();
        if let Some(v) = out.vars {
            if v.to_string().len() > MAX_VALUE {
                logs.push("[t3cm] vars not saved: over 1 MB serialized".into());
            } else if let Err(e) = self.store.kv_set(&ns, VARS_KEY, &v, &iso(self.clock.now())) {
                logs.push(format!("[t3cm] vars not saved: {e}"));
            }
        }
        let output = match out.result {
            Ok(v) => ToolOutput { is_error: false, text: truncate::render(self.redact_value(v), logs) },
            Err(e) => error_output(&self.redact(e), logs, &globals),
        };
        ToolOutput { text: self.redact(output.text), ..output }
    }

    pub async fn search(self: &Arc<Self>, code: &str) -> ToolOutput {
        let js = match js::prepare(code) {
            Ok(j) => j,
            Err(e) => return error_output(&e, Vec::new(), SEARCH_GLOBALS),
        };
        let catalog = self.catalog_for_run().await;
        let _slot = match self.run_slot().await {
            Ok(p) => p,
            Err(e) => return error_output(&e, Vec::new(), SEARCH_GLOBALS),
        };
        let out = sandbox::run(Run {
            js,
            prelude: SEARCH_PRELUDE,
            globals: vec![("catalog", catalog.json.clone())],
            host: Arc::new(NoHost),
            limits: self.limits(None),
            keep_vars: false,
        })
        .await;
        let logs: Vec<String> = out.logs.into_iter().map(|l| self.redact(l)).collect();
        let output = match out.result {
            Ok(v) => ToolOutput { is_error: false, text: truncate::render(self.redact_value(v), logs) },
            Err(e) => error_output(&self.redact(e), logs, SEARCH_GLOBALS),
        };
        ToolOutput { text: self.redact(output.text), ..output }
    }

    pub async fn ask(self: &Arc<Self>, question: &str) -> anyhow::Result<Value> {
        let v = self.wf.ask(question).await?;
        let seq = v["seq"].as_i64().unwrap_or_default();
        let app = self.clone();
        let every = Duration::from_secs(self.config.board.ask_poll_secs.max(1));
        let until = Instant::now() + Duration::from_secs(self.config.board.ask_timeout_minutes.max(1) * 60 + 60);
        tokio::spawn(async move {
            while Instant::now() < until {
                tokio::time::sleep(every).await;
                match app.wf.check_asks(Some(seq)).await {
                    Ok(false) => break,
                    Ok(true) => {}
                    Err(e) => tracing::warn!("ask q{seq} check failed: {}", app.t3.redact(e.to_string())),
                }
            }
        });
        Ok(v)
    }

    pub fn health(&self) -> Value {
        let last_pass = self.store.meta_get(LAST_PASS).ok().flatten();
        json!({
            "version": env!("CARGO_PKG_VERSION"),
            "uptimeSecs": self.started.elapsed().as_secs(),
            "t3": self.t3.status(),
            "tools": self.catalog().t3_names.len(),
            "watcher": {
                "lastPass": last_pass,
                "alive": self.wf.watcher_alive(),
                "intervalSecs": self.config.watcher_interval_secs,
            },
            "runs": {
                "running": self.config.max_concurrent_runs - self.runs.available_permits(),
                "queued": self.queued.load(Ordering::Relaxed),
                "max": self.config.max_concurrent_runs,
            },
            "store": self.store.path().display().to_string(),
            "upstreams": self.upstreams.status(),
            "update": self.wf.update_view(),
            "skills": self.wf.skills_view(),
        })
    }

    async fn host_op(
        &self,
        op: &str,
        args: Value,
        caller: Option<&str>,
        reported: &crate::wf_slots::Reported,
        ns: &str,
        catalog: &Catalog,
    ) -> Result<Value, String> {
        let pick_ns = |args: &Value| -> Result<String, String> {
            match &args["ns"] {
                Value::Null => Ok(ns.to_string()),
                Value::String(s) if !s.trim().is_empty() && s.len() <= 200 => Ok(s.clone()),
                _ => Err("mem.ns(name): name must be a non-empty string of at most 200 chars".into()),
            }
        };
        let key = |args: &Value, method: &str| -> Result<String, String> {
            match args["key"].as_str() {
                Some(k) if !k.is_empty() && k.len() <= 512 => Ok(k.to_string()),
                _ => Err(format!("mem.{method}(key): key must be a non-empty string of at most 512 chars")),
            }
        };
        let store_err = |e: anyhow::Error| format!("store: {e}");
        match op {
            "call" => {
                let (ns, tool) = match args["qualified"].as_str() {
                    Some(q) => split_qualified(q).ok_or_else(|| {
                        format!(
                            "tools.call(name): name must be \"<namespace>.<tool>\", got {q:?}; namespaces are {}",
                            self.namespace_list()
                        )
                    })?,
                    None => (args["ns"].as_str().unwrap_or_default(), args["name"].as_str().unwrap_or_default()),
                };
                self.call_tool(ns, tool, args.get("input"), catalog).await
            }
            "mem.get" => {
                let (ns, key) = (pick_ns(&args)?, key(&args, "get")?);
                self.store.kv_get(&ns, &key).map(|v| v.unwrap_or(Value::Null)).map_err(store_err)
            }
            "mem.set" => {
                let (ns, key) = (pick_ns(&args)?, key(&args, "set")?);
                if key == VARS_KEY {
                    return Err(format!("{VARS_KEY} is reserved for vars; assign to vars instead"));
                }
                let value =
                    args.get("value").ok_or("mem.set(key, value): value must be JSON-serializable, not undefined")?;
                if value.to_string().len() > MAX_VALUE {
                    return Err("mem.set: value is over 1 MB serialized".into());
                }
                self.store.kv_set(&ns, &key, value, &iso(self.clock.now())).map_err(store_err)?;
                Ok(Value::Null)
            }
            "mem.del" => {
                let (ns, key) = (pick_ns(&args)?, key(&args, "del")?);
                self.store.kv_del(&ns, &key).map(Value::Bool).map_err(store_err)
            }
            "mem.list" => {
                let ns = pick_ns(&args)?;
                let prefix = args["prefix"].as_str().unwrap_or("");
                let keys = self.store.kv_list(&ns, prefix).map_err(store_err)?;
                Ok(json!(keys.into_iter().filter(|k| k != VARS_KEY).collect::<Vec<_>>()))
            }
            other => match other.strip_prefix("wf.") {
                Some(method) => self.wf.dispatch_from(method, caller, reported, args).await.map_err(|e| e.to_string()),
                None => Err(format!("unknown host operation {other}")),
            },
        }
    }
}

impl App {
    fn namespace_list(&self) -> String {
        let mut all = vec![T3_NAMESPACE.to_string()];
        all.extend(self.upstreams.namespaces());
        all.join(", ")
    }

    async fn call_tool(&self, ns: &str, tool: &str, input: Option<&Value>, catalog: &Catalog) -> Result<Value, String> {
        if ns == T3_NAMESPACE {
            return self.call_t3(tool, input, catalog).await;
        }
        if !self.upstreams.is_namespace(ns) {
            return Err(format!("unknown namespace {ns:?}; namespaces are {}", self.namespace_list()));
        }
        let input = object_input(&qualified(ns, tool), input)?;
        let value = self.upstreams.call(ns, tool, input).await.map_err(|e| e.to_string())?;
        Ok(self.redact_value(value))
    }

    async fn call_t3(&self, name: &str, input: Option<&Value>, catalog: &Catalog) -> Result<Value, String> {
        let resolved = if catalog.t3_names.is_empty() || catalog.t3_names.contains(name) {
            name.to_string()
        } else if catalog.t3_names.contains(&format!("t3_{name}")) {
            format!("t3_{name}")
        } else {
            return Err(format!("unknown T3 tool {name:?}; use the search tool to find the right name"));
        };
        if self.config.deny.contains(&resolved) {
            return Err(format!("{resolved} is denied by the t3-codemode deny list; do not retry it"));
        }
        let input = object_input(&format!("t3.{resolved}"), input)?;
        let child = (resolved == "t3_thread_launch").then(|| input["message"].as_str().and_then(join_target)).flatten();
        let value = self.t3.call(&resolved, input).await.map_err(|e| e.to_string())?;
        if let Some((parent, label)) = child
            && let Err(e) = self.wf.note_launch(&parent, &label, &value).await
        {
            tracing::warn!("t3_thread_launch succeeded, but recording its thread on a pending link failed: {e:#}");
        }
        Ok(self.redact_value(value))
    }
}

struct ExecHost {
    app: Arc<App>,
    caller: Option<String>,
    reported: crate::wf_slots::Reported,
    ns: String,
    catalog: Arc<Catalog>,
}

impl Host for ExecHost {
    fn call(&self, op: String, args: Value) -> BoxFut<'static, Result<Value, String>> {
        let app = self.app.clone();
        let caller = self.caller.clone();
        let reported = self.reported.clone();
        let ns = self.ns.clone();
        let catalog = self.catalog.clone();
        Box::pin(async move {
            app.host_op(&op, args, caller.as_deref(), &reported, &ns, &catalog).await.map_err(|e| app.redact(e))
        })
    }
}
