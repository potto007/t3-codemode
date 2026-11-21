use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::pools::PoolConfig;
use crate::upstream::UpstreamConfig;

pub const UP_KINDS: &[&str] = &["note", "event", "gate", "blocker", "report", "handoff", "closed"];
pub const CLOSING: &[&str] = &["handoff", "closed"];
const EXTRA_WAKE_KINDS: &[&str] = &["escalate", "reply"];

pub const MAX_LIMIT_HOLD_MINUTES: u64 = 7 * 24 * 60;
pub const LEGACY_BOARD_TICKET_KEYS: &[&str] = &["instanceId", "ticketModel", "ticketEffort", "pool"];

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]

pub struct LinkParams {
    pub down: i64,
    pub miss: i64,
    pub stall: i64,
}

impl LinkParams {
    pub fn validate(&self) -> Result<()> {
        for (k, v) in [("down", self.down), ("miss", self.miss), ("stall", self.stall)] {
            if v < 1 {
                bail!("{k} must be a positive integer, got {v}");
            }
        }
        if self.stall <= self.down {
            bail!("stall ({}) must exceed down ({})", self.stall, self.down);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChildPolicy {
    pub down: Option<i64>,
    pub miss: Option<i64>,
    pub stall: Option<i64>,
    pub wake: Option<Vec<String>>,
    pub body_cap: Option<usize>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleConfig {
    #[serde(default)]
    pub children: BTreeMap<String, ChildPolicy>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoardConfig {
    pub instance_id: String,
    #[serde(default)]
    pub pool: Option<String>,
    pub ticket_model: String,
    pub ticket_effort: Option<String>,
    pub ask_model: String,
    pub ask_effort: Option<String>,
    pub runtime_mode: Option<String>,
    pub ask_poll_secs: u64,
    pub ask_timeout_minutes: u64,
    pub feed: usize,
    pub settle_after_hours: u64,
    pub reconcile_active_minutes: u64,
    pub reconcile_idle_minutes: u64,
    pub open_links: OpenLinks,
    #[serde(skip)]
    pub legacy_ticket_keys: Vec<String>,
}

impl BoardConfig {
    pub fn has_legacy_ticket_keys(&self) -> bool {
        !self.legacy_ticket_keys.is_empty()
    }

    pub fn has_legacy_ticket_key(&self, key: &str) -> bool {
        self.legacy_ticket_keys.iter().any(|k| k == key)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OpenLinks {
    Desktop,
    Tab,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    pub deny: Vec<String>,
    pub watcher_interval_secs: u64,
    pub max_hold_minutes: f64,
    pub cpu_budget_ms: u64,
    pub default_timeout_ms: u64,
    pub max_timeout_ms: u64,
    pub max_concurrent_runs: usize,
    pub run_queue_ms: u64,
    pub link: LinkParams,
    pub wake: Vec<String>,
    pub body_cap: usize,
    pub reply_cap: usize,
    pub digest_lines: usize,
    pub roles: BTreeMap<String, RoleConfig>,
    pub board: BoardConfig,
    #[serde(default)]
    pub upstreams: Vec<UpstreamConfig>,
    #[serde(default)]
    pub pools: BTreeMap<String, PoolConfig>,
    pub limit_hold_minutes: u64,
    pub update_check: bool,
}

pub fn builtin_defaults() -> Value {
    json!({
        "deny": ["t3_project_delete", "t3_environment_unlink"],
        "watcherIntervalSecs": 60,
        "maxHoldMinutes": 30,
        "cpuBudgetMs": 2000,
        "defaultTimeoutMs": 60000,
        "maxTimeoutMs": 600000,
        "maxConcurrentRuns": 16,
        "runQueueMs": 10000,
        "link": {"down": 45, "miss": 2, "stall": 240},
        "wake": ["event", "gate", "blocker", "handoff", "closed", "report", "escalate", "reply"],
        "bodyCap": 1500,
        "replyCap": 1500,
        "digestLines": 6,
        "limitHoldMinutes": 60,
        "updateCheck": true,
        "roles": {
            "overseer": {"children": {"ticket": {
                "down": 75, "stall": 480,
                "wake": ["gate", "blocker", "handoff", "closed", "report", "escalate"],
                "bodyCap": 600
            }}},
            "ticket": {"children": {"worker": {}}},
            "worker": {"children": {}}
        },
        "board": {
            "instanceId": "claudeAgent",
            "pool": null,
            "ticketModel": "claude-opus-5-5",
            "ticketEffort": "high",
            "askModel": "claude-haiku-5-5",
            "askEffort": null,
            "runtimeMode": "full-access",
            "askPollSecs": 4,
            "askTimeoutMinutes": 15,
            "feed": 50,
            "settleAfterHours": 24,
            "reconcileActiveMinutes": 2,
            "reconcileIdleMinutes": 10,
            "openLinks": "desktop"
        }
    })
}

fn merge(base: &mut Value, over: Value) {
    match (base, over) {
        (Value::Object(b), Value::Object(o)) => {
            for (k, v) in o {
                match b.get_mut(&k) {
                    Some(slot) => merge(slot, v),
                    None => {
                        b.insert(k, v);
                    }
                }
            }
        }
        (slot, v) => *slot = v,
    }
}

pub fn default_path() -> PathBuf {
    if let Ok(p) = std::env::var("T3CM_CONFIG") {
        return PathBuf::from(p);
    }
    crate::platform::config_dir().join("config.json")
}

impl Default for Config {
    fn default() -> Self {
        Config::from_value(Value::Null).expect("built-in defaults are valid")
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Config> {
        let over = match std::fs::read_to_string(path) {
            Ok(s) => serde_json::from_str(&s).with_context(|| format!("{} is not valid JSON", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Value::Null,
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        Config::from_value(over).with_context(|| format!("config {}", path.display()))
    }

    pub fn from_value(over: Value) -> Result<Config> {
        let legacy_ticket_keys = over
            .get("board")
            .and_then(Value::as_object)
            .map(|b| {
                LEGACY_BOARD_TICKET_KEYS
                    .iter()
                    .filter(|&&k| match b.get(k) {
                        Some(v) if k == "pool" => !v.is_null(),
                        Some(_) => true,
                        None => false,
                    })
                    .map(|&k| k.to_string())
                    .collect()
            })
            .unwrap_or_default();
        let mut v = builtin_defaults();
        if !over.is_null() {
            merge(&mut v, over);
        }
        if let Some(pools) = v.get("pools") {
            let pools = pools.as_object().ok_or_else(|| anyhow!("pools must be an object of named pools"))?;
            for (name, raw) in pools {
                crate::pools::check_value(name, raw)?;
            }
        }
        let mut cfg: Config = serde_json::from_value(v)?;
        cfg.board.legacy_ticket_keys = legacy_ticket_keys;
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<()> {
        if self.max_concurrent_runs == 0 {
            bail!("maxConcurrentRuns must be at least 1");
        }
        self.link.validate().context("link")?;
        let (active, idle) = (self.board.reconcile_active_minutes, self.board.reconcile_idle_minutes);
        if idle > 0 && !(1..=idle).contains(&active) {
            bail!("board.reconcileActiveMinutes must be from 1 to reconcileIdleMinutes ({idle}), got {active}");
        }
        if !self.roles.get("overseer").is_some_and(|r| r.children.contains_key("ticket")) {
            bail!("the board needs the overseer role to open ticket links");
        }
        check_wake(&self.wake).context("wake")?;
        for (role, rc) in &self.roles {
            for child in rc.children.keys() {
                if !self.roles.contains_key(child) {
                    bail!("role {role} opens links to unknown role {child}");
                }
                self.link_params(role, child)?;
                if let Some(w) = &rc.children[child].wake {
                    check_wake(w).with_context(|| format!("{role} -> {child} wake"))?;
                }
                if let Some(c) = rc.children[child].body_cap
                    && c < 200
                {
                    bail!("{role} -> {child}: bodyCap must be at least 200");
                }
            }
        }
        crate::upstream::config::validate(&self.upstreams).context("upstreams")?;
        if !(1..=MAX_LIMIT_HOLD_MINUTES).contains(&self.limit_hold_minutes) {
            bail!("limitHoldMinutes must be between 1 and {MAX_LIMIT_HOLD_MINUTES} (a week)");
        }
        for (name, pool) in &self.pools {
            if name.trim().is_empty() {
                bail!("a pool needs a name");
            }
            pool.validate().with_context(|| format!("pool {name}"))?;
        }
        if let Some(p) = &self.board.pool
            && !self.pools.contains_key(p)
        {
            bail!("board.pool names {p:?}, which is not in pools");
        }
        Ok(())
    }

    pub fn check_role(&self, role: &str) -> Result<()> {
        if self.roles.contains_key(role) {
            Ok(())
        } else {
            let known: Vec<&str> = self.roles.keys().map(String::as_str).collect();
            Err(anyhow!("unknown role {role:?}; roles are {}", known.join(", ")))
        }
    }

    pub fn board_oversees(&self, role: &str) -> bool {
        self.roles.get("overseer").is_some_and(|r| r.children.contains_key(role))
    }

    pub fn link_params(&self, parent_role: &str, child_role: &str) -> Result<LinkParams> {
        self.check_role(parent_role)?;
        let child = self.roles[parent_role]
            .children
            .get(child_role)
            .ok_or_else(|| anyhow!("role {parent_role:?} opens no links to {child_role:?}"))?;
        let p = LinkParams {
            down: child.down.unwrap_or(self.link.down),
            miss: child.miss.unwrap_or(self.link.miss),
            stall: child.stall.unwrap_or(self.link.stall),
        };
        p.validate().with_context(|| format!("{parent_role} -> {child_role} link numbers"))?;
        Ok(p)
    }

    pub fn link_policy(&self, parent_role: Option<&str>, child_role: &str) -> (Vec<String>, usize) {
        let child = parent_role.and_then(|r| self.roles.get(r)).and_then(|rc| rc.children.get(child_role));
        let wake = child.and_then(|c| c.wake.clone()).unwrap_or_else(|| self.wake.clone());
        let cap = child.and_then(|c| c.body_cap).unwrap_or(self.body_cap);
        (wake, cap)
    }
}

fn check_wake(kinds: &[String]) -> Result<()> {
    for k in kinds {
        if k == "note" {
            bail!("a note never wakes its parent; take it out of wake");
        }
        if !UP_KINDS.contains(&k.as_str()) && !EXTRA_WAKE_KINDS.contains(&k.as_str()) {
            bail!("unknown wake kind {k:?}");
        }
    }
    Ok(())
}
