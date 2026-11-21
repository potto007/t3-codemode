use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use serde_json::{Value, json};

use t3_codemode::app::App;
use t3_codemode::catalog::Catalog;
use t3_codemode::clock::{SystemClock, local_datetime, parse_ts};
use t3_codemode::config::{self, Config};
use t3_codemode::release::{self, Version};
use t3_codemode::slots::{self, Caps, Enabled, Folders, Inputs, Resolved};
use t3_codemode::store::Store;
use t3_codemode::t3_http::HttpT3;
use t3_codemode::wf_slots::Reported;
use t3_codemode::{agent_link, bench, doctor, platform, server, setup, skills, steps, upgrade, watcher};

#[derive(Parser)]
#[command(
    name = "t3cm",
    version,
    about = "Code Mode MCP server for T3 Code: two tools, search and execute",
    arg_required_else_help = true
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(clap::Args, Clone)]
struct StateArgs {
    #[arg(long, env = "T3CM_STATE_DIR")]
    state_dir: Option<PathBuf>,
}

#[derive(clap::Args, Clone)]
struct PortArgs {
    #[arg(long, env = "T3CM_PORT", default_value_t = 47731)]
    port: u16,
}

#[derive(Subcommand)]
enum Cmd {
    /// Serve MCP over HTTP on 127.0.0.1, with the link watcher
    Serve {
        #[command(flatten)]
        port: PortArgs,
        #[command(flatten)]
        state: StateArgs,
        #[arg(long)]
        no_watcher: bool,
        /// Append logs to this file instead of stderr; past 10 MB it rolls to <file>.1 at start.
        /// On Windows it also lets go of a console that t3cm alone holds, so a logon task runs
        /// without a window.
        #[arg(long, value_name = "FILE")]
        log_file: Option<PathBuf>,
    },
    /// Serve MCP over stdio (no watcher unless --watcher)
    Stdio {
        #[command(flatten)]
        state: StateArgs,
        #[arg(long)]
        watcher: bool,
    },
    /// Acquire a fresh T3 OAuth session now; prints only its expiry
    Auth {
        #[command(flatten)]
        state: StateArgs,
    },
    /// Show the running service's /healthz
    Status {
        #[command(flatten)]
        port: PortArgs,
    },
    /// Run a script through the running service's execute tool
    Exec {
        file: String,
        #[command(flatten)]
        port: PortArgs,
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        timeout_ms: Option<u64>,
        #[arg(long, env = "T3CODE_THREAD_ID")]
        thread: Option<String>,
    },
    /// Run a query through the running service's search tool
    Search {
        file: String,
        #[command(flatten)]
        port: PortArgs,
    },
    /// Install the latest release in place and restart the service on it
    #[command(visible_alias = "upgrade")]
    Update {
        /// Only report whether a newer release is out
        #[arg(long)]
        check: bool,
        /// Install this release instead of the latest (vX.Y.Z)
        #[arg(long = "version", value_name = "vX.Y.Z", conflicts_with = "check")]
        tag: Option<String>,
        /// Replace the binary but leave the running service alone
        #[arg(long)]
        no_restart: bool,
        #[command(flatten)]
        port: PortArgs,
        #[command(flatten)]
        state: StateArgs,
    },
    /// Print the T3 workflow slots as the service resolves them for this shell's agent and repository
    Slots {
        /// The repository (default: the current directory's)
        #[arg(long)]
        repo: Option<PathBuf>,
        #[arg(long)]
        json: bool,
        /// Resolve in this process without the service, and skip the model checks
        #[arg(long)]
        no_t3: bool,
        #[command(flatten)]
        port: PortArgs,
        #[arg(long, env = "T3CODE_THREAD_ID")]
        thread: Option<String>,
    },
    /// Check the T3 workflow install: slots, skills, setup steps, t3cm's config and the service; exits 1 on any problem
    Doctor {
        /// The repository (default: the current directory's)
        #[arg(long)]
        repo: Option<PathBuf>,
        #[arg(long)]
        json: bool,
        /// Check without the service: no model, T3 or service checks
        #[arg(long)]
        no_t3: bool,
        /// List every installed skill NAME matches, in search order; exits 1 when none does
        #[arg(long, value_name = "NAME")]
        resolve: Option<String>,
        #[command(flatten)]
        port: PortArgs,
        #[command(flatten)]
        state: StateArgs,
        #[arg(long, env = "T3CODE_THREAD_ID")]
        thread: Option<String>,
    },
    /// Print the generated TypeScript declarations for the catalog
    Types {
        #[arg(long)]
        from: Option<PathBuf>,
        #[command(flatten)]
        state: StateArgs,
    },
    /// Install, update or check the T3 workflow skills this binary carries
    Skills {
        #[command(subcommand)]
        cmd: SkillsCmd,
    },
    /// Detect installed candidates, write slot settings, or install the filed-issue rule
    Setup {
        #[command(subcommand)]
        cmd: SetupCmd,
    },
    #[command(hide = true)]
    Bench {
        #[arg(short, default_value_t = 1000)]
        n: usize,
        #[arg(long)]
        tools: Option<PathBuf>,
    },
    /// Register or remove t3cm stdio in T3 Antigravity profiles' mcp_config.json
    #[command(hide = true)]
    AntigravityMcp {
        #[arg(long)]
        remove: bool,
        #[arg(long)]
        bin: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum SkillsCmd {
    /// Write the skills to ~/.agents/skills, link them into Claude Code's and Antigravity's skills folders, record them in ~/.config/t3-workflow/installed.json, and apply new setup steps
    Install {
        /// Replace skills another tool (npx skills, a copy) installed, and drop them from its lock file
        #[arg(long)]
        take_over: bool,
        /// Overwrite files edited since t3cm installed them
        #[arg(long)]
        force: bool,
        #[arg(long)]
        json: bool,
    },
    /// Bring installed skills to this binary's release, keeping locally edited files, and apply new setup steps
    Update {
        /// Overwrite files edited since t3cm installed them
        #[arg(long)]
        force: bool,
        #[arg(long)]
        json: bool,
    },
    /// Compare the installed skills and setup steps with this binary's release; exits 1 unless all are current
    Status {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum SetupCmd {
    /// Print platform, current slots, installed candidates, T3 providers, rule, steps and t3cm status as JSON
    Detect {
        /// The repository (default: the current directory's)
        #[arg(long)]
        repo: Option<PathBuf>,
        /// Skills listed in this session, comma or space separated
        #[arg(long)]
        skills: Option<String>,
        /// Output JSON (default)
        #[arg(long)]
        json: bool,
        /// Skip querying T3 capabilities through the running service
        #[arg(long)]
        no_t3: bool,
        #[arg(long, env = "T3CM_PORT")]
        port: Option<String>,
        #[command(flatten)]
        state: StateArgs,
        #[arg(long, env = "T3CODE_THREAD_ID")]
        thread: Option<String>,
    },
    /// Merge slot settings from stdin into the user config file and print the resulting slots
    Write {
        /// The repository (default: the current directory's)
        #[arg(long)]
        repo: Option<PathBuf>,
        /// Skills listed in this session, comma or space separated
        #[arg(long)]
        skills: Option<String>,
        /// Drop a slot (or models.ticket / models.worker) so its default applies
        #[arg(long, value_name = "SLOT[.ROLE]")]
        unset: Vec<String>,
        /// Start from an empty config instead of merging with the current file
        #[arg(long)]
        replace: bool,
        /// A slot the user was asked about, kept ones too, recorded as a done setup step
        #[arg(long, value_name = "SLOT[,SLOT]")]
        asked: Vec<String>,
        /// Write to the repository's .t3-workflow.json instead of the user config file
        #[arg(long)]
        repo_config: bool,
        /// Skip querying T3 capabilities through the running service
        #[arg(long)]
        no_t3: bool,
        #[arg(long, env = "T3CM_PORT")]
        port: Option<String>,
        #[command(flatten)]
        state: StateArgs,
        #[arg(long, env = "T3CODE_THREAD_ID")]
        thread: Option<String>,
    },
    /// Install the always-loaded filed-issue rule for Claude Code and Codex
    Rule,
}

fn run_skills(cmd: SkillsCmd) -> Result<ExitCode> {
    let paths = skills::Paths::from_env()?;
    let env = steps::Env::from_env()?;
    let bundle = skills::Bundle::embedded();
    let (opts, json) = match cmd {
        SkillsCmd::Status { json } => {
            let st = skills::status(&paths, &bundle)?;
            let setup = steps::check(&env, &bundle)?;
            let ok = t3_codemode::drift::current(&st, &setup);
            let agents = agent_link::Agent::all(&env);
            let links: Vec<Vec<agent_link::Link>> = agents
                .iter()
                .map(|a| match st.installed {
                    Some(_) => agent_link::check(a, &paths.dir, &bundle.skills()),
                    None => Vec::new(),
                })
                .collect();
            if json {
                let mut v = serde_json::to_value(&st)?;
                v["current"] = Value::Bool(ok);
                v["steps"] = serde_json::to_value(&setup)?;
                for (a, l) in agents.iter().zip(&links) {
                    v[a.key] = serde_json::to_value(l)?;
                }
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else {
                let notes: String = agents.iter().zip(&links).map(|(a, l)| agent_link::describe_check(a, l)).collect();
                print!("{}{}{notes}", skills::describe_status(&st), steps::describe_steps(&setup));
            }
            return Ok(if ok { ExitCode::SUCCESS } else { ExitCode::FAILURE });
        }
        SkillsCmd::Install { take_over, force, json } => {
            (skills::Options { mode: skills::Mode::Install, force, take_over }, json)
        }
        SkillsCmd::Update { force, json } => {
            (skills::Options { mode: skills::Mode::Update, force, take_over: false }, json)
        }
    };
    let retiring_t3wf = paths.dir.join("t3-workflow").join("scripts").join("t3wf.py").exists();
    let open_links = if retiring_t3wf {
        steps::check_open_links(&env, &platform::state_dir())
    } else {
        steps::OpenLinksReport::default()
    };
    let report = skills::sync(&paths, &bundle, opts)?;
    let mut applied = steps::apply(&env, &bundle)?;
    applied.notes.extend(open_links.warnings.clone());
    if retiring_t3wf {
        let mut threads: Vec<String> = open_links.t3cm_threads.clone();
        for t in &open_links.t3wf_threads {
            if !threads.contains(t) {
                threads.push(t.clone());
            }
        }
        if !threads.is_empty() {
            let t3_bin = t3_codemode::t3_http::find_t3_binary();
            let (nudged, nudge_notes) = steps::nudge_threads(t3_bin.as_deref(), &threads);
            applied.nudged = nudged;
            applied.notes.extend(nudge_notes);
        }
    }
    let agents = agent_link::Agent::all(&env);
    let linked: Vec<agent_link::Linked> =
        agents.iter().map(|a| agent_link::link(a, &paths.dir, &bundle.skills())).collect();
    if json {
        let mut v = serde_json::to_value(&report)?;
        v["steps"] = serde_json::to_value(&applied)?;
        for (a, l) in agents.iter().zip(&linked) {
            v[a.key] = serde_json::to_value(l)?;
        }
        println!("{}", serde_json::to_string_pretty(&v)?);
    } else {
        let notes: String = agents.iter().zip(&linked).map(|(a, l)| agent_link::describe(a, l)).collect();
        print!("{}{}{notes}", skills::describe_report(&report), steps::describe_applied(&applied));
    }
    Ok(ExitCode::SUCCESS)
}

fn state_dir(a: &StateArgs) -> PathBuf {
    a.state_dir.clone().unwrap_or_else(platform::state_dir)
}

fn init_logging(log_file: Option<&Path>) -> Result<()> {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    let fmt = tracing_subscriber::fmt().with_env_filter(filter).with_target(false);
    match log_file {
        Some(path) => {
            fmt.with_writer(std::sync::Mutex::new(open_log(path)?)).with_ansi(false).init();
            LOGS_TO_FILE.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        None => fmt.with_writer(std::io::stderr).with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr())).init(),
    }
    Ok(())
}

const LOG_ROLL_BYTES: u64 = 10 << 20;

static LOGS_TO_FILE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn open_log(path: &Path) -> Result<std::fs::File> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    if std::fs::metadata(path).is_ok_and(|m| m.len() > LOG_ROLL_BYTES) {
        let mut rolled = path.as_os_str().to_owned();
        rolled.push(".1");
        std::fs::rename(path, &rolled).with_context(|| format!("rolling {}", path.display()))?;
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))
}

#[cfg(windows)]
fn release_own_console() {
    use windows_sys::Win32::System::Console::{FreeConsole, GetConsoleProcessList};
    let mut pids = [0u32; 2];
    if unsafe { GetConsoleProcessList(pids.as_mut_ptr(), pids.len() as u32) } == 1 {
        unsafe { FreeConsole() };
    }
}

#[cfg(not(windows))]
fn release_own_console() {}

fn not_serving(port: u16) -> String {
    let svc = &platform::SERVICE;
    format!("is t3cm serving on 127.0.0.1:{port}? Start it with `{}`; logs: {}", svc.start, svc.logs)
}

fn build(state: &StateArgs) -> Result<(Arc<App>, Arc<HttpT3>)> {
    let cfg = Config::load(&config::default_path())?;
    let store = Arc::new(Store::open(&state_dir(state))?);
    let clock = Arc::new(SystemClock);
    let t3 = Arc::new(HttpT3::new(store.clone(), clock.clone()));
    Ok((App::new(cfg, store, t3.clone(), clock), t3))
}

async fn warm_up(app: Arc<App>, t3: Arc<HttpT3>) {
    let up = app.clone();
    tokio::spawn(async move {
        let listed = up.upstreams.tools().await;
        tracing::info!(upstreams = listed.len(), "upstream catalogs loaded");
    });
    if let Err(e) = t3.ensure_token(false).await {
        tracing::warn!("T3 auth not ready yet: {e}");
        return;
    }
    match app.refresh_catalog().await {
        Ok(n) => tracing::info!(tools = n, "T3 catalog loaded"),
        Err(e) => tracing::warn!("T3 catalog not loaded yet: {e}"),
    }
    if let Err(e) = app.wf.check_pools().await {
        tracing::error!(
            "pools: {}; every pick from the pool fails until the config is fixed",
            app.t3.redact(format!("{e:#}"))
        );
    }
}

#[cfg(unix)]
async fn os_shutdown() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler");
    term.recv().await;
}

#[cfg(windows)]
async fn os_shutdown() {
    use tokio::signal::windows::{ctrl_break, ctrl_close, ctrl_shutdown};
    let mut brk = ctrl_break().expect("CTRL_BREAK handler");
    let mut close = ctrl_close().expect("CTRL_CLOSE handler");
    let mut shutdown = ctrl_shutdown().expect("CTRL_SHUTDOWN handler");
    tokio::select! {
        _ = brk.recv() => {},
        _ = close.recv() => {},
        _ = shutdown.recv() => {},
    }
}

fn update_check_allowed() -> bool {
    !release::check_disabled(Config::load(&config::default_path()).map(|c| c.update_check).unwrap_or(true))
}

async fn print_update_notice() {
    if !std::io::IsTerminal::is_terminal(&std::io::stderr()) || !update_check_allowed() {
        return;
    }
    let Ok(releases) = release::releases_url() else { return };
    let dir = std::env::var_os("T3CM_STATE_DIR").filter(|d| !d.is_empty()).map(PathBuf::from);
    let dir = dir.unwrap_or_else(platform::state_dir);
    let check = release::known_latest(&dir, &releases, release::CLI_FRESH);
    let Ok(Some(latest)) = tokio::time::timeout(std::time::Duration::from_secs(3), check).await else { return };
    if let Some(n) = release::notice(&Version::current(), &latest, &releases) {
        eprintln!("\n{n}");
    }
}

fn spawn_release_checks(app: Arc<App>, state_dir: PathBuf) {
    if !app.config.update_check || release::check_disabled(true) {
        return;
    }
    tokio::spawn(async move {
        let Ok(releases) = release::releases_url() else { return };
        let every = release::SERVICE_EVERY;
        let mut wait = std::time::Duration::from_secs(5);
        if let Some(c) = release::read_cache(&state_dir)
            && let Some(v) = c.version()
        {
            app.wf.set_release(v);
            if let Some(age) = c.age().and_then(|a| a.try_into().ok()) {
                wait = every.saturating_sub(age).max(wait);
            }
        }
        loop {
            tokio::time::sleep(wait).await;
            wait = every;
            match release::latest(&releases).await {
                Ok(v) => {
                    if let Err(e) = release::write_cache(&state_dir, &release::Checked::now(&v)) {
                        tracing::debug!("update check cache: {e:#}");
                    }
                    if v > Version::current() {
                        tracing::info!(latest = %v, "a newer t3cm release is out; run `t3cm update`");
                    }
                    app.wf.set_release(v);
                }
                Err(e) => tracing::debug!("update check: {e:#}"),
            }
        }
    });
}

async fn shutdown_signal() {
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {},
        _ = os_shutdown() => {},
    }
    tracing::info!("shutting down");
}

fn read_source(file: &str) -> Result<String> {
    if file == "-" {
        let mut s = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut s)?;
        Ok(s)
    } else {
        std::fs::read_to_string(file).with_context(|| format!("reading {file}"))
    }
}

async fn call_service(
    port: u16,
    tool: &str,
    args: Value,
    thread: Option<&str>,
    deadline: Option<std::time::Duration>,
) -> Result<(bool, String)> {
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": tool, "arguments": args}});
    let mut req =
        reqwest::Client::builder().no_proxy().build()?.post(format!("http://127.0.0.1:{port}/mcp")).json(&body);
    if let Some(d) = deadline {
        req = req.timeout(d);
    }
    if let Some(t) = thread {
        req = req.header(server::THREAD_HEADER, t);
    }
    let own = std::env::var("T3CODE_THREAD_ID").ok().filter(|t| !t.is_empty());
    if tool == "execute" && (thread.is_none() || thread == own.as_deref()) {
        for (name, value) in Reported::from_env().headers() {
            req = req.header(name, reqwest::header::HeaderValue::from_bytes(value.as_bytes())?);
        }
    }
    let v: Value = req.send().await.with_context(|| not_serving(port))?.json().await?;
    if let Some(e) = v.get("error") {
        bail!("{}", e["message"]);
    }
    let r = &v["result"];
    let text = r["content"][0]["text"].as_str().unwrap_or_default().to_string();
    Ok((r["isError"] == true, text))
}

async fn exec_value(port: u16, code: String, thread: Option<&str>) -> Result<Value> {
    let (is_error, text) = call_service(
        port,
        "execute",
        json!({"code": code, "timeout_ms": EXEC_BUDGET_MS}),
        thread,
        Some(std::time::Duration::from_millis(EXEC_BUDGET_MS + 10_000)),
    )
    .await?;
    if is_error {
        bail!("{text}");
    }
    let v: Value = serde_json::from_str(&text).with_context(|| format!("the service answered {text}"))?;
    if let Some(note) = v["truncated"].as_str() {
        bail!("the service cut its answer, so it is incomplete ({note}); pass --no-t3 to resolve here in full");
    }
    let Some(result) = v.as_object().and_then(|o| o.get("result")) else {
        bail!("the service answered without a result: {text}");
    };
    Ok(result.clone())
}

async fn service_slots(port: u16, root: &Path, thread: Option<&str>) -> Result<Value> {
    let code = format!("async () => wf.slots({{ repo: {} }})", serde_json::to_string(&root.to_string_lossy())?);
    let v = exec_value(port, code, thread).await.map_err(|e| {
        if format!("{e}").contains("not a function") {
            anyhow::anyhow!(
                "the t3cm service on 127.0.0.1:{port} is older than this t3cm and has no wf.slots; restart it on this binary, or pass --no-t3"
            )
        } else {
            e
        }
    })?;
    if !v.is_object() {
        bail!("the t3cm service on 127.0.0.1:{port} returned unexpected slots data: {v}");
    }
    Ok(v)
}

fn repo_root(repo: Option<&Path>) -> Result<PathBuf> {
    let start = match repo {
        Some(r) => r.to_path_buf(),
        None => std::env::current_dir()?,
    };
    Ok(slots::git_root(&start))
}

fn local_slots(folders: &Folders, root: &Path) -> Resolved {
    let enabled = Enabled::load(&folders.claude, root);
    let r = Resolved::build(&Inputs { folders, root, listed: &[], enabled: &enabled });
    if slots::in_skill_tool_harness() { r } else { r.without_skill_tool() }
}

fn local_view(r: &Resolved, caps: &Caps) -> Value {
    let mut v = r.all(caps);
    v["problems"] = json!(r.problems());
    v["warnings"] = json!([]);
    v
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array().into_iter().flatten().filter_map(Value::as_str).map(String::from).collect()
}

async fn run_slots(
    repo: Option<PathBuf>,
    json: bool,
    no_t3: bool,
    port: u16,
    thread: Option<String>,
) -> Result<ExitCode> {
    let root = repo_root(repo.as_deref())?;
    let v = if no_t3 {
        local_view(&local_slots(&Folders::from_env()?, &root), &Caps::Unchecked)
    } else {
        service_slots(port, &root, thread.as_deref()).await?
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(ExitCode::SUCCESS);
    }
    let mut lines: Vec<String> =
        strings(&v["problems"]).iter().map(|p| format!("config problem (ignored): {p}")).collect();
    lines.extend(slots::report_lines(&v));
    lines.extend(strings(&v["warnings"]).iter().map(|w| format!("warn: {w}")));
    println!("{}", lines.join("\n"));
    Ok(ExitCode::SUCCESS)
}

const EXEC_BUDGET_MS: u64 = 60_000;

async fn health(port: u16) -> Result<Value> {
    Ok(reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(10))
        .build()?
        .get(format!("http://127.0.0.1:{port}/healthz"))
        .send()
        .await
        .with_context(|| not_serving(port))?
        .json()
        .await?)
}

struct DoctorArgs {
    repo: Option<PathBuf>,
    json: bool,
    no_t3: bool,
    resolve: Option<String>,
    port: u16,
    state: StateArgs,
    thread: Option<String>,
}

async fn run_doctor(a: DoctorArgs) -> Result<ExitCode> {
    let folders = Folders::from_env()?;
    let root = repo_root(a.repo.as_deref())?;
    if let Some(name) = a.resolve {
        let enabled = Enabled::load(&folders.claude, &root);
        let resolver = slots::Resolver::new(&slots::search_roots(&folders, &root, &enabled), &[]);
        let cands = resolver.candidates(&name);
        let resolved = resolver.resolve(&name);
        if a.json {
            let list: Vec<Value> = cands.iter().map(slots::Skill::to_json).collect();
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({"ref": name, "resolved": resolved, "candidates": list}))?
            );
        } else {
            println!("{}", doctor::resolve_lines(&name, &resolved, &cands).join("\n"));
        }
        return Ok(if resolved["status"] == "missing" { ExitCode::FAILURE } else { ExitCode::SUCCESS });
    }
    let mut f = doctor::Findings::default();
    let mut caps = None;
    let eff = if a.no_t3 {
        local_view(&local_slots(&folders, &root), &Caps::Unchecked)
    } else {
        match service_slots(a.port, &root, a.thread.as_deref()).await {
            Ok(v) => {
                match health(a.port).await {
                    Ok(h) => f.extend(doctor::service(Some(&h), None)),
                    Err(e) => f.extend(doctor::service(None, Some(&format!("{e:#}")))),
                }
                caps = exec_value(a.port, "async () => t3.orchestrator_capabilities({})".into(), None).await.ok();
                v
            }
            Err(e) => {
                f.extend(doctor::service(None, Some(&format!("{e:#}"))));
                local_view(&local_slots(&folders, &root), &Caps::Failed("the t3cm service is not reachable".into()))
            }
        }
    };
    let (problems, warnings) = slots::doctor_eff(strings(&eff["problems"]), &eff);
    f.problems.splice(0..0, problems);
    f.warnings.splice(0..0, warnings.into_iter().chain(strings(&eff["warnings"])));
    let (cfg, found) = doctor::t3cm_config(&config::default_path());
    f.extend(found);
    f.extend(doctor::pools(&eff, cfg.as_ref()));
    f.extend(doctor::legacy_board(cfg.as_ref()));
    let home = t3_codemode::drift::Home::from_env()?;
    let bundle = skills::Bundle::embedded();
    f.extend(doctor::skills(&home, &bundle));
    f.extend(doctor::stale_copies(&home, &bundle, &folders.codex));
    f.extend(doctor::t3wf_leftovers(&folders));
    let store_dir = state_dir(&a.state);
    let store_file = store_dir.join("store.db");
    if store_file.exists() {
        match Store::open(&store_dir).and_then(|s| s.instance_folders()) {
            Ok(records) => f.extend(doctor::instances(&records, &folders, &root, caps.as_ref())),
            Err(e) => f.warnings.push(format!("instances: cannot read t3cm's store ({e:#})")),
        }
    } else {
        f.warnings.push(format!("instances: no t3cm store at {} yet", store_file.display()));
    }
    if a.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({"problems": f.problems, "warnings": f.warnings, "settings": eff}))?
        );
    } else {
        println!("{}", doctor::lines(&eff, &f).join("\n"));
    }
    Ok(if f.problems.is_empty() { ExitCode::SUCCESS } else { ExitCode::FAILURE })
}

async fn direct_caps(state: &StateArgs) -> Result<Value> {
    use t3_codemode::t3::T3Client;
    let store = Arc::new(Store::open(&state_dir(state))?);
    let clock = Arc::new(SystemClock);
    let t3 = HttpT3::new(store, clock);
    t3.call("orchestrator_capabilities", json!({})).await.map_err(|e| anyhow::anyhow!("{e}"))
}

async fn run_setup(cmd: SetupCmd) -> Result<ExitCode> {
    let folders = Folders::from_env()?;
    let env = steps::Env::from_env()?;
    let bundle = skills::Bundle::embedded();
    match cmd {
        SetupCmd::Detect { repo, skills, json: _, no_t3, port, state, thread } => {
            let root = repo_root(repo.as_deref())?;
            let listed = setup::parse_listed(skills.as_deref());
            let probe = setup::probe_t3cm(&folders.home, port.as_deref()).await;
            let (caps, providers) = if no_t3 {
                (Caps::Unchecked, json!({"error": "T3 unreachable (skipped with --no-t3)"}))
            } else {
                let fetched = if let (Some(p), true) = (probe.port, probe.running) {
                    match exec_value(p, "async () => t3.orchestrator_capabilities({})".into(), thread.as_deref()).await
                    {
                        Ok(c) => Ok(c),
                        Err(_) => direct_caps(&state).await,
                    }
                } else {
                    direct_caps(&state).await
                };
                match fetched {
                    Ok(c) => {
                        let prov = setup::format_providers(&c);
                        (Caps::Got(c), prov)
                    }
                    Err(e) => {
                        let msg = format!("T3 unreachable ({e:#})");
                        (Caps::Failed(msg.clone()), json!({"error": msg}))
                    }
                }
            };
            let out = setup::detect_json(&setup::DetectInputs {
                folders: &folders,
                env: &env,
                bundle: &bundle,
                root: &root,
                listed: &listed,
                caps: &caps,
                providers,
                t3cm: probe.json,
            })?;
            println!("{}", serde_json::to_string_pretty(&out)?);
        }
        SetupCmd::Write { repo, skills, unset, replace, asked, repo_config, no_t3, port, state, thread } => {
            let root = repo_root(repo.as_deref())?;
            let listed = setup::parse_listed(skills.as_deref());
            let mut stdin_buf = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut stdin_buf)?;
            let res = setup::write_config(
                &folders,
                &env,
                &bundle,
                &stdin_buf,
                &setup::WriteOpts { repo: root.clone(), listed: listed.clone(), unset, replace, asked, repo_config },
            )?;
            println!("wrote {}", res.path.display());
            for w in &res.warnings {
                println!("warn: {w}");
            }
            let caps = if no_t3 {
                Caps::Unchecked
            } else {
                let port_num = port
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::parse::<u16>)
                    .transpose()
                    .ok()
                    .flatten()
                    .unwrap_or(setup::DEFAULT_PORT);
                match exec_value(port_num, "async () => t3.orchestrator_capabilities({})".into(), thread.as_deref())
                    .await
                {
                    Ok(c) => Caps::Got(c),
                    Err(_) => match direct_caps(&state).await {
                        Ok(c) => Caps::Got(c),
                        Err(_) => Caps::Unchecked,
                    },
                }
            };
            let enabled = Enabled::load(&folders.claude, &root);
            let resolved =
                Resolved::build(&Inputs { folders: &folders, root: &root, listed: &listed, enabled: &enabled });
            let eff = resolved.all(&caps);
            for p in resolved.problems() {
                println!("config problem (ignored): {p}");
            }
            for line in slots::report_lines(&eff) {
                println!("{line}");
            }
        }
        SetupCmd::Rule => {
            for line in setup::apply_rule(&env, &bundle)? {
                println!("{line}");
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

async fn run(cli: Cli) -> Result<ExitCode> {
    match cli.cmd {
        Cmd::Serve { port, state, no_watcher, log_file } => {
            init_logging(log_file.as_deref())?;
            if log_file.is_some() {
                release_own_console();
            }
            let (app, t3) = build(&state)?;
            app.wf.refresh_skills();
            let (listener, addr) =
                server::bind(port.port).await.with_context(|| format!("binding 127.0.0.1:{}", port.port))?;
            tracing::info!(%addr, store = %app.store.path().display(), "t3-codemode serving MCP at /mcp");
            tokio::spawn(warm_up(app.clone(), t3));
            spawn_release_checks(app.clone(), state_dir(&state));
            if !no_watcher {
                watcher::spawn(app.clone());
            }
            server::serve(app.clone(), listener, shutdown_signal()).await?;
            app.upstreams.shutdown().await;
        }
        Cmd::Stdio { state, watcher: with_watcher } => {
            init_logging(None)?;
            let (app, t3) = build(&state)?;
            app.wf.refresh_skills();
            tokio::spawn(warm_up(app.clone(), t3));
            if with_watcher {
                watcher::spawn(app.clone());
            }
            let caller = std::env::var("T3CODE_THREAD_ID").ok().filter(|s| !s.is_empty());
            server::stdio(app.clone(), caller).await?;
            app.upstreams.shutdown().await;
        }
        Cmd::Auth { state } => {
            let (_, t3) = build(&state)?;
            let tok = t3.force_acquire().await?;
            let when = parse_ts(&tok.expires_at).map(local_datetime).unwrap_or(tok.expires_at);
            println!("T3 session acquired; expires {when}");
        }
        Cmd::Status { port } => {
            let v: Value = reqwest::Client::builder()
                .no_proxy()
                .build()?
                .get(format!("http://127.0.0.1:{}/healthz", port.port))
                .send()
                .await
                .with_context(|| not_serving(port.port))?
                .json()
                .await?;
            println!("{}", serde_json::to_string_pretty(&v)?);
        }
        Cmd::Exec { file, port, session, timeout_ms, thread } => {
            let mut args = json!({"code": read_source(&file)?});
            if let Some(s) = session {
                args["session"] = json!(s);
            }
            if let Some(t) = timeout_ms {
                args["timeout_ms"] = json!(t);
            }
            let (is_error, text) = call_service(port.port, "execute", args, thread.as_deref(), None).await?;
            println!("{text}");
            return Ok(if is_error { ExitCode::FAILURE } else { ExitCode::SUCCESS });
        }
        Cmd::Search { file, port } => {
            let (is_error, text) =
                call_service(port.port, "search", json!({"code": read_source(&file)?}), None, None).await?;
            println!("{text}");
            return Ok(if is_error { ExitCode::FAILURE } else { ExitCode::SUCCESS });
        }
        Cmd::Types { from, state } => {
            let tools: Vec<Value> = match from {
                Some(p) => serde_json::from_str(&std::fs::read_to_string(&p)?)?,
                None => {
                    let store = Store::open(&state_dir(&state))?;
                    let cached = store
                        .meta_get("t3_tools")?
                        .context("no cached T3 tool list; run `t3cm serve` once or pass --from <tools.json>")?;
                    serde_json::from_str(&cached)?
                }
            };
            let cat = Catalog::build(&tools);
            let decls = cat.declarations();
            println!("{decls}");
            let raw = serde_json::to_string(&tools)?.len();
            eprintln!(
                "{} T3 tools + {} built-ins: {} chars of declarations (~{} tokens) vs {} chars of raw tool JSON (~{} tokens)",
                cat.t3_names.len(),
                cat.entries.len() - cat.t3_names.len(),
                decls.len(),
                decls.len() / 4,
                raw,
                raw / 4
            );
        }
        Cmd::Update { check, tag, no_restart, port, state } => {
            upgrade::run(upgrade::Opts {
                check,
                tag,
                port: port.port,
                restart: !no_restart,
                state_dir: state_dir(&state),
            })
            .await?;
        }
        Cmd::Skills { cmd } => return run_skills(cmd),
        Cmd::Setup { cmd } => return run_setup(cmd).await,
        Cmd::Slots { repo, json, no_t3, port, thread } => return run_slots(repo, json, no_t3, port.port, thread).await,
        Cmd::Doctor { repo, json, no_t3, resolve, port, state, thread } => {
            return run_doctor(DoctorArgs { repo, json, no_t3, resolve, port: port.port, state, thread }).await;
        }
        Cmd::Bench { n, tools } => bench::run(n, tools.as_deref()).await?,
        Cmd::AntigravityMcp { remove, bin } => return run_antigravity_mcp(remove, bin),
    }
    Ok(ExitCode::SUCCESS)
}

fn run_antigravity_mcp(remove: bool, bin: Option<PathBuf>) -> Result<ExitCode> {
    let mut env = steps::Env::from_env()?;
    if let Some(b) = bin {
        env.t3cm_bin = b;
    }
    let targets = steps::antigravity_targets(&env);
    if remove {
        let mut failed = false;
        let tools_prefix = format!("mcpServers.{}.tools", steps::MCP_NAME);
        for t in targets {
            match t.status {
                steps::AntigravityState::Current | steps::AntigravityState::Stale => {
                    match steps::remove_antigravity(&t.path) {
                        Ok(true) => println!("Removed {} for Antigravity from {}", steps::MCP_NAME, t.path.display()),
                        Ok(false) => {}
                        Err(e) => {
                            eprintln!(
                                "Warning: could not update Antigravity MCP config at {} ({e:#}); left as is.",
                                t.path.display()
                            );
                            failed = true;
                        }
                    }
                }
                steps::AntigravityState::Missing | steps::AntigravityState::Conflict => {}
                steps::AntigravityState::Problem => {
                    if t.problem.as_deref().is_some_and(|p| p.starts_with(&tools_prefix)) {
                        match steps::remove_antigravity(&t.path) {
                            Ok(true) => {
                                println!("Removed {} for Antigravity from {}", steps::MCP_NAME, t.path.display())
                            }
                            Ok(false) => {}
                            Err(e) => {
                                eprintln!(
                                    "Warning: could not update Antigravity MCP config at {} ({e:#}); left as is.",
                                    t.path.display()
                                );
                                failed = true;
                            }
                        }
                    } else {
                        let why = t.problem.as_deref().unwrap_or("invalid JSON");
                        eprintln!(
                            "Warning: could not update Antigravity MCP config at {} ({why}); left as is.",
                            t.path.display()
                        );
                    }
                }
            }
        }
        return Ok(if failed { ExitCode::from(2) } else { ExitCode::SUCCESS });
    }
    let mut wrote = false;
    for t in targets {
        match t.status {
            steps::AntigravityState::Missing | steps::AntigravityState::Stale => {
                match steps::write_antigravity(&t.path, &env.t3cm_bin) {
                    Ok(true) => {
                        println!("Registered {} for Antigravity in {}", steps::MCP_NAME, t.path.display());
                        wrote = true;
                    }
                    Ok(false) => {}
                    Err(e) => eprintln!(
                        "Warning: could not update Antigravity MCP config at {} ({e:#}); left as is.",
                        t.path.display()
                    ),
                }
            }
            steps::AntigravityState::Conflict => {
                println!(
                    "Antigravity already has an incompatible MCP server named {} in {}; left as is.",
                    steps::MCP_NAME,
                    t.path.display()
                );
            }
            steps::AntigravityState::Problem => {
                let why = t.problem.as_deref().unwrap_or("invalid JSON");
                eprintln!(
                    "Warning: could not update Antigravity MCP config at {} ({why}); left as is.",
                    t.path.display()
                );
            }
            steps::AntigravityState::Current => {}
        }
    }
    if wrote {
        println!("Note: {}", steps::ANTIGRAVITY_CAVEAT);
    }
    Ok(ExitCode::SUCCESS)
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            use clap::error::ErrorKind;
            let shown = matches!(
                e.kind(),
                ErrorKind::DisplayHelp
                    | ErrorKind::DisplayVersion
                    | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
            );
            let _ = e.print();
            if shown {
                print_update_notice().await;
            }
            return ExitCode::from(u8::try_from(e.exit_code()).unwrap_or(2));
        }
    };
    match run(cli).await {
        Ok(code) => code,
        Err(e) => {
            if LOGS_TO_FILE.load(std::sync::atomic::Ordering::Relaxed) {
                tracing::error!("t3cm: {e:#}");
            }
            eprintln!("t3cm: {e:#}");
            ExitCode::FAILURE
        }
    }
}
