use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::clock::{iso, local_clock, parse_ts, plus_minutes};
use crate::store::{Limit, LimitStop, PickRecord};
use crate::usage::{Usage, UsageSource};
use crate::wf::{ACTIVE, Wf};

pub const STRATEGIES: &[&str] = &["weighted", "priority", "cheapest"];
const EFFORT_IDS: &[&str] = &["effort", "reasoningEffort", "thinking_level"];
const THREAD_PAGE: u64 = 200;
const LIMIT_TITLE: &str = "Usage limit reached";
const LOOKBACK_MINUTES: f64 = 24.0 * 60.0;
const STOP_ITEMS: u64 = 20;
const LAST_POSITION: u64 = 1 << 40;

fn weighted() -> String {
    "weighted".into()
}

fn one() -> u32 {
    1
}

fn unit_cost() -> f64 {
    1.0
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PoolConfig {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub mixed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub driver: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default = "weighted")]
    pub strategy: String,
    pub members: Vec<PoolMember>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PoolMember {
    pub instance_id: String,
    #[serde(default = "one")]
    pub weight: u32,
    #[serde(default)]
    pub reserve: f64,
    #[serde(default = "unit_cost")]
    pub cost: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

fn blank(v: &Option<String>) -> bool {
    v.as_deref().is_some_and(|s| s.trim().is_empty())
}

impl PoolConfig {
    pub fn validate(&self) -> Result<()> {
        if self.mixed {
            for (key, set) in [("driver", &self.driver), ("model", &self.model), ("effort", &self.effort)] {
                if set.is_some() {
                    bail!("a mixed pool has no pool-level {key}; set model and effort on each member");
                }
            }
        } else {
            match self.driver.as_deref() {
                None => bail!("driver is missing; set it, or set \"mixed\": true for members of several drivers"),
                Some(d) if d.trim().is_empty() => bail!("driver is empty"),
                Some(_) => {}
            }
            match self.model.as_deref() {
                None => bail!("model is missing"),
                Some(m) if m.trim().is_empty() => bail!("model is empty"),
                Some(_) => {}
            }
            if blank(&self.effort) {
                bail!("effort is empty");
            }
        }
        match self.strategy.as_str() {
            s if STRATEGIES.contains(&s) => {}
            "headroom" => {
                bail!(
                    "strategy headroom needs a usage source, which t3cm does not have yet; use one of {}",
                    STRATEGIES.join(", ")
                )
            }
            s => bail!("unknown strategy {s:?}; use one of {}", STRATEGIES.join(", ")),
        }
        if self.members.is_empty() {
            bail!("a pool needs at least one member");
        }
        for (i, m) in self.members.iter().enumerate() {
            if m.instance_id.trim().is_empty() {
                bail!("member {} has an empty instanceId", i + 1);
            }
            if self.members[..i].iter().any(|o| o.instance_id == m.instance_id) {
                bail!("member {} is listed twice", m.instance_id);
            }
            if m.weight == 0 {
                bail!("member {}: weight must be at least 1", m.instance_id);
            }
            if !(0.0..=100.0).contains(&m.reserve) {
                bail!("member {}: reserve must be between 0 and 100, got {}", m.instance_id, m.reserve);
            }
            if !m.cost.is_finite() || m.cost < 0.0 {
                bail!("member {}: cost must be a finite number of 0 or more, got {}", m.instance_id, m.cost);
            }
            if !self.mixed {
                for (key, set) in [("model", &m.model), ("effort", &m.effort)] {
                    if set.is_some() {
                        bail!(
                            "member {}: only a mixed pool's members set their own {key}; set it on the pool, or set \"mixed\": true",
                            m.instance_id
                        );
                    }
                }
            } else {
                match m.model.as_deref() {
                    None => bail!("member {}: a mixed pool's member needs a model", m.instance_id),
                    Some(s) if s.trim().is_empty() => bail!("member {}: model is empty", m.instance_id),
                    Some(_) => {}
                }
                if blank(&m.effort) {
                    bail!("member {}: effort is empty", m.instance_id);
                }
            }
        }
        Ok(())
    }

    pub fn model_of<'a>(&'a self, m: &'a PoolMember) -> &'a str {
        m.model.as_deref().or(self.model.as_deref()).unwrap_or_default()
    }

    pub fn effort_of<'a>(&'a self, m: &'a PoolMember) -> Option<&'a str> {
        m.effort.as_deref().or(self.effort.as_deref())
    }
}

pub fn check_value(name: &str, raw: &Value) -> Result<PoolConfig> {
    let pool: PoolConfig = serde_json::from_value(raw.clone()).with_context(|| format!("pool {name}"))?;
    pool.validate().with_context(|| format!("pool {name}"))?;
    Ok(pool)
}

fn providers(caps: &Value) -> &[Value] {
    caps["providers"].as_array().map(Vec::as_slice).unwrap_or_default()
}

fn provider<'a>(caps: &'a Value, id: &str) -> Option<&'a Value> {
    providers(caps).iter().find(|p| p["providerInstanceId"] == id)
}

pub fn check_members(name: &str, pool: &PoolConfig, caps: &Value) -> Result<()> {
    for m in &pool.members {
        let Some(p) = provider(caps, &m.instance_id) else {
            let known: Vec<&str> = providers(caps).iter().filter_map(|p| p["providerInstanceId"].as_str()).collect();
            bail!(
                "pool {name}: member {} is not a T3 provider instance; the instances are {}",
                m.instance_id,
                if known.is_empty() { "none".into() } else { known.join(", ") }
            );
        };
        let driver = p["driverKind"].as_str().unwrap_or_default();
        if let Some(want) = pool.driver.as_deref().filter(|w| *w != driver) {
            bail!(
                "pool {name}: member {} is a {driver} instance, not {want}; set \"mixed\": true to mix drivers",
                m.instance_id
            );
        }
    }
    Ok(())
}

fn lists_model(p: &Value, model: &str) -> bool {
    p["models"].as_array().is_some_and(|ms| ms.iter().any(|m| m["id"] == model))
}

fn can_run(pool: &PoolConfig, m: &PoolMember, p: &Value) -> bool {
    let model = pool.model_of(m);
    lists_model(p, model) && effort_options(model, pool.effort_of(m), p).is_some()
}

fn reserve_hint(m: &PoolMember) -> String {
    if m.reserve > 0.0 {
        String::new()
    } else {
        format!(" Set a reserve on {} to keep room for the work only it can run.", m.instance_id)
    }
}

pub fn sole_warnings(pool: &PoolConfig, caps: &Value) -> Vec<String> {
    let mut groups: BTreeMap<&str, Vec<&PoolMember>> = BTreeMap::new();
    for m in &pool.members {
        let Some(p) = provider(caps, &m.instance_id).filter(|p| can_run(pool, m, p)) else { continue };
        let key = if pool.mixed { p["driverKind"].as_str().unwrap_or_default() } else { pool.model_of(m) };
        groups.entry(key).or_default().push(m);
    }
    groups
        .into_iter()
        .filter_map(|(key, ms)| match ms.as_slice() {
            [m] if pool.mixed => Some(format!(
                "{id} is the only {key} member that can run its model, {model}, so the pool runs no {key} launch while {id} is out.{hint}",
                id = m.instance_id,
                model = pool.model_of(m),
                hint = reserve_hint(m)
            )),
            [m] => Some(format!(
                "only {id} can run {key}, so the pool launches nothing while {id} is out.{hint}",
                id = m.instance_id,
                hint = reserve_hint(m)
            )),
            _ => None,
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq)]
enum State {
    Ready,
    Limited(String),
    NoUsage,
    Disabled,
    Constrained(String),
    MissingModel,
    MissingEffort,
}

impl State {
    fn name(&self) -> &'static str {
        match self {
            State::Ready => "ready",
            State::Limited(_) | State::NoUsage => "limited",
            State::Disabled => "disabled",
            State::Constrained(_) => "constrained",
            State::MissingModel => "missing-model",
            State::MissingEffort => "missing-effort",
        }
    }

    fn why(&self, pool: &PoolConfig, m: &PoolMember) -> Option<String> {
        match self {
            State::Ready => None,
            State::Limited(until) => Some(format!("limited until {}", local_clock(until))),
            State::NoUsage => Some("has no usage left".into()),
            State::Disabled => Some("disabled".into()),
            State::Constrained(c) => Some(format!("constrained: {c}")),
            State::MissingModel => Some(format!("does not list model {}", pool.model_of(m))),
            State::MissingEffort => Some(format!(
                "does not offer effort {} for {}",
                pool.effort_of(m).unwrap_or_default(),
                pool.model_of(m)
            )),
        }
    }
}

fn current(usage: BTreeMap<String, Usage>, now: Timestamp) -> BTreeMap<String, Usage> {
    usage.into_iter().filter(|(_, u)| u.resets_at.as_deref().and_then(parse_ts).is_none_or(|r| r > now)).collect()
}

fn active_limit<'a>(limits: &'a BTreeMap<String, Limit>, id: &str, now: Timestamp) -> Option<&'a Limit> {
    limits.get(id).filter(|l| parse_ts(&l.until).is_some_and(|u| u > now))
}

fn member_state(pool: &PoolConfig, m: &PoolMember, p: &Value, limit: Option<&Limit>, usage: Option<&Usage>) -> State {
    let constraints: Vec<&str> = p["constraints"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    if constraints.iter().any(|c| c.to_ascii_lowercase().contains("disabled")) {
        return State::Disabled;
    }
    if !constraints.is_empty() {
        return State::Constrained(constraints.join("; "));
    }
    let model = pool.model_of(m);
    if !lists_model(p, model) {
        return State::MissingModel;
    }
    if effort_options(model, pool.effort_of(m), p).is_none() {
        return State::MissingEffort;
    }
    if let Some(l) = limit {
        return State::Limited(l.until.clone());
    }
    match usage {
        Some(u) if u.left <= 0.0 => u.resets_at.clone().map_or(State::NoUsage, State::Limited),
        _ => State::Ready,
    }
}

fn effort_options(model: &str, effort: Option<&str>, p: &Value) -> Option<Value> {
    let Some(effort) = effort else { return Some(json!([])) };
    let options = p["models"]
        .as_array()
        .and_then(|ms| ms.iter().find(|m| m["id"] == model))
        .and_then(|m| m["options"].as_array())?;
    let option = EFFORT_IDS.iter().find_map(|id| options.iter().find(|o| o["id"] == *id))?;
    let offered = option["values"].as_array().is_some_and(|vs| vs.iter().any(|v| v == effort));
    offered.then(|| json!([{"id": option["id"], "value": effort}]))
}

fn pct(v: f64) -> String {
    format!("{}%", (v * 10.0).round() / 10.0)
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Skipped {
    pub instance_id: String,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Choice {
    pub instance_id: String,
    pub driver: String,
    pub model: String,
    pub effort: Option<String>,
    pub options: Value,
    pub reason: String,
    pub skipped: Vec<Skipped>,
    pub credits: BTreeMap<String, i64>,
}

pub struct Inputs<'a> {
    pub name: &'a str,
    pub pool: &'a PoolConfig,
    pub caps: &'a Value,
    pub running: &'a BTreeMap<String, u64>,
    pub limits: &'a BTreeMap<String, Limit>,
    pub usage: &'a BTreeMap<String, Usage>,
    pub credits: &'a BTreeMap<String, i64>,
    pub now: Timestamp,
}

fn with_skips(base: String, skipped: &[Skipped]) -> String {
    if skipped.is_empty() {
        return base;
    }
    let list: Vec<String> = skipped.iter().map(|s| format!("{} {}", s.instance_id, s.reason)).collect();
    format!("{base}; skipped {}", list.join(", "))
}

fn by_weight(
    pool: &PoolConfig,
    among: &[usize],
    credits: &mut BTreeMap<String, i64>,
    running: &BTreeMap<String, u64>,
) -> (usize, i64) {
    let total: i64 = among.iter().map(|&i| pool.members[i].weight as i64).sum();
    for &i in among {
        let m = &pool.members[i];
        *credits.entry(m.instance_id.clone()).or_default() += m.weight as i64;
    }
    let load = |i: usize| running.get(&pool.members[i].instance_id).copied().unwrap_or(0);
    let credit = |i: usize| credits[&pool.members[i].instance_id];
    let best =
        among
            .iter()
            .copied()
            .reduce(|a, b| {
                if (credit(b), std::cmp::Reverse(load(b))) > (credit(a), std::cmp::Reverse(load(a))) { b } else { a }
            })
            .expect("among is not empty");
    *credits.get_mut(&pool.members[best].instance_id).expect("credited") -= total;
    (best, total)
}

pub fn choose(inp: &Inputs) -> Result<Choice> {
    let Inputs { name, pool, caps, running, limits, usage, credits, now } = *inp;
    check_members(name, pool, caps)?;
    let mut skipped = Vec::new();
    let mut open = Vec::new();
    let mut held = Vec::new();
    for (i, m) in pool.members.iter().enumerate() {
        let p = provider(caps, &m.instance_id).expect("checked members");
        let left = usage.get(&m.instance_id);
        let state = member_state(pool, m, p, active_limit(limits, &m.instance_id, now), left);
        if let Some(why) = state.why(pool, m) {
            skipped.push(Skipped { instance_id: m.instance_id.clone(), reason: why });
            continue;
        }
        match left {
            _ if m.reserve <= 0.0 => open.push(i),
            Some(u) if u.left > m.reserve => open.push(i),
            Some(u) => skipped.push(Skipped {
                instance_id: m.instance_id.clone(),
                reason: format!("held at its {}% reserve with {} left", m.reserve, pct(u.left)),
            }),
            None => held.push(i),
        }
    }
    let last_resort = open.is_empty() && !held.is_empty();
    let candidates = if open.is_empty() { held.clone() } else { open };
    if candidates.is_empty() {
        let list: Vec<String> = skipped.iter().map(|s| format!("{} {}", s.instance_id, s.reason)).collect();
        bail!("pool {name} has no member that can run: {}", list.join("; "));
    }
    if !last_resort {
        for &i in &held {
            let m = &pool.members[i];
            skipped.push(Skipped {
                instance_id: m.instance_id.clone(),
                reason: format!("held at its {}% reserve until every other member is out", m.reserve),
            });
        }
    }
    let mut credits = credits.clone();
    let (chosen, base) = match pool.strategy.as_str() {
        "priority" => (candidates[0], "priority: the first member that can run".to_string()),
        "cheapest" => {
            let cost = |i: usize| pool.members[i].cost;
            let low = candidates.iter().map(|&i| cost(i)).reduce(f64::min).expect("candidates is not empty");
            let group: Vec<usize> = candidates.iter().copied().filter(|&i| cost(i) == low).collect();
            let (best, total) = by_weight(pool, &group, &mut credits, running);
            (best, format!("cheapest: cost {low}, weighted: weight {} of {total}", pool.members[best].weight))
        }
        "headroom" => {
            let left = |i: usize| usage.get(&pool.members[i].instance_id).map(|u| u.left);
            match candidates.iter().filter_map(|&i| left(i)).reduce(f64::max) {
                Some(most) => {
                    let top: Vec<usize> = candidates.iter().copied().filter(|&i| left(i) == Some(most)).collect();
                    let (best, _) = by_weight(pool, &top, &mut credits, running);
                    (best, format!("headroom: most left, {}", pct(most)))
                }
                None => {
                    let (best, total) = by_weight(pool, &candidates, &mut credits, running);
                    let w = pool.members[best].weight;
                    (best, format!("headroom: no member reports usage, so weighted: weight {w} of {total}"))
                }
            }
        }
        _ => {
            let (best, total) = by_weight(pool, &candidates, &mut credits, running);
            (best, format!("weighted: weight {} of {total}", pool.members[best].weight))
        }
    };
    let m = &pool.members[chosen];
    let base = if last_resort {
        format!("last resort: {} holds a {}% reserve and every other member is out; {base}", m.instance_id, m.reserve)
    } else {
        base
    };
    let p = provider(caps, &m.instance_id).expect("checked members");
    let driver = p["driverKind"].as_str().unwrap_or_default().to_string();
    let (model, effort) = (pool.model_of(m), pool.effort_of(m));
    let ran = format!("{driver} {model}, effort {}", effort.unwrap_or("default"));
    Ok(Choice {
        instance_id: m.instance_id.clone(),
        driver,
        model: model.to_string(),
        effort: effort.map(String::from),
        options: effort_options(model, effort, p).expect("a ready member offers the effort"),
        reason: with_skips(format!("{ran}; {base}"), &skipped),
        skipped,
        credits,
    })
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Picked {
    pub pool: String,
    pub instance_id: String,
    pub driver: String,
    pub model: String,
    pub effort: Option<String>,
    pub options: Value,
    pub reason: String,
    pub skipped: Vec<Skipped>,
    pub model_selection: Value,
    #[serde(rename = "pick")]
    pub seq: i64,
}

impl Picked {
    pub fn summary(&self) -> String {
        format!("on {} ({})", self.instance_id, self.reason)
    }
}

pub fn model_selection(instance_id: &str, model: &str, options: &Value) -> Value {
    let mut m = json!({"instanceId": instance_id, "model": model});
    if options.as_array().is_some_and(|o| !o.is_empty()) {
        m["options"] = options.clone();
    }
    m
}

fn snooze_until(t: &Value) -> Option<String> {
    if t["snoozed"] != true {
        return None;
    }
    parse_ts(t["snoozedUntil"].as_str()?).map(iso)
}

fn later(a: &str, b: &str) -> bool {
    matches!((parse_ts(a), parse_ts(b)), (Some(a), Some(b)) if a > b)
}

impl Wf {
    pub(crate) fn pool(&self, name: &str) -> Result<&PoolConfig> {
        self.config.pools.get(name).ok_or_else(|| {
            let known: Vec<&str> = self.config.pools.keys().map(String::as_str).collect();
            anyhow!(
                "no pool {name:?}; configured pools: {}",
                if known.is_empty() { "none".into() } else { known.join(", ") }
            )
        })
    }

    pub fn set_usage_source(&self, source: Option<Arc<dyn UsageSource>>) {
        *self.usage.lock().unwrap_or_else(|e| e.into_inner()) = source;
    }

    async fn usage_of(&self, instances: &[String]) -> BTreeMap<String, Usage> {
        let source = self.usage.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let Some(source) = source else { return BTreeMap::new() };
        match source.usage(instances).await {
            Ok(u) => u,
            Err(e) => {
                tracing::warn!("usage source did not answer, picking without it: {e:#}");
                BTreeMap::new()
            }
        }
    }

    async fn capabilities(&self, model: Option<&str>) -> Result<Value> {
        let input = match model {
            Some(m) => json!({"search": m}),
            None => json!({}),
        };
        Ok(self.t3.call("orchestrator_capabilities", input).await?)
    }

    async fn pool_capabilities(&self, pool: &PoolConfig) -> Result<Value> {
        let models: BTreeSet<&str> = pool.members.iter().map(|m| pool.model_of(m)).collect();
        let mut merged: Option<Value> = None;
        for model in models {
            let caps = self.capabilities(Some(model)).await?;
            let Some(all) = merged.as_mut() else {
                merged = Some(caps);
                continue;
            };
            for p in providers(&caps) {
                let Some(id) = p["providerInstanceId"].as_str() else { continue };
                let Some(have) = all["providers"]
                    .as_array_mut()
                    .and_then(|ps| ps.iter_mut().find(|q| q["providerInstanceId"] == id))
                else {
                    continue;
                };
                if let Some(ms) = have["models"].as_array_mut() {
                    ms.extend(p["models"].as_array().into_iter().flatten().cloned());
                }
            }
        }
        merged.ok_or_else(|| anyhow!("pool has no members"))
    }

    async fn threads_with(&self, statuses: &[&str]) -> Result<Vec<Value>> {
        let mut out = Vec::new();
        let mut cursor: Option<u64> = None;
        loop {
            let mut input = json!({"allProjects": true, "statuses": statuses, "limit": THREAD_PAGE});
            if let Some(c) = cursor {
                input["cursor"] = json!(c);
            }
            let r = self.t3.call("t3_thread_list", input).await?;
            let threads = r["threads"].as_array().cloned().unwrap_or_default();
            let empty = threads.is_empty();
            out.extend(threads);
            match r["nextCursor"].as_u64() {
                Some(c) if !empty && cursor.is_none_or(|p| c > p) => cursor = Some(c),
                _ => break,
            }
        }
        Ok(out)
    }

    async fn running_by_instance(&self) -> Result<BTreeMap<String, u64>> {
        let mut out = BTreeMap::new();
        for t in self.threads_with(ACTIVE).await? {
            if let Some(id) = t["providerInstanceId"].as_str() {
                *out.entry(id.to_string()).or_default() += 1;
            }
        }
        Ok(out)
    }

    pub async fn check_pools(&self) -> Result<()> {
        if self.config.pools.is_empty() {
            return Ok(());
        }
        let caps = match self.capabilities(None).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    "pools not checked at start, T3 did not answer ({}); each pick checks them",
                    self.t3.redact(format!("{e:#}"))
                );
                return Ok(());
            }
        };
        for (name, pool) in &self.config.pools {
            check_members(name, pool, &caps)?;
        }
        Ok(())
    }

    pub async fn pick(&self, name: &str) -> Result<Picked> {
        self.pick_avoiding(name, None).await
    }

    pub(crate) async fn pick_avoiding(&self, name: &str, avoid: Option<(&str, &str)>) -> Result<Picked> {
        let mut pool = self.pool(name)?.clone();
        let caps = self.pool_capabilities(&pool).await?;
        check_members(name, &pool, &caps)?;
        let mut avoided = Vec::new();
        if let Some((id, why)) = avoid {
            let driver_of = |i: &str| provider(&caps, i).and_then(|p| p["driverKind"].as_str()).map(String::from);
            let driver = driver_of(id);
            pool.members.retain(|m| {
                let reason = if m.instance_id == id {
                    Some(why.to_string())
                } else {
                    let d = driver_of(&m.instance_id);
                    driver.as_ref().filter(|want| d.as_ref() != Some(want)).map(|want| {
                        format!("is a {} instance, not {want} like the thread", d.as_deref().unwrap_or("unknown"))
                    })
                };
                let keep = reason.is_none();
                avoided.extend(reason.map(|reason| Skipped { instance_id: m.instance_id.clone(), reason }));
                keep
            });
            if pool.members.is_empty() {
                let like = driver.map(|d| format!(" that is a {d} instance")).unwrap_or_default();
                bail!("pool {name} has no member other than {id}{like}");
            }
        }
        let pool = &pool;
        let running = match pool.strategy.as_str() {
            "priority" => BTreeMap::new(),
            _ => self.running_by_instance().await?,
        };
        let ids: Vec<String> = pool.members.iter().map(|m| m.instance_id.clone()).collect();
        let usage = self.usage_of(&ids).await;
        let _g = self.pool_lock.lock().await;
        let limits = self.store.limits()?;
        let credits = self.store.pool_credits(name)?;
        let now = self.clock.now();
        let usage = current(usage, now);
        let c = choose(&Inputs {
            name,
            pool,
            caps: &caps,
            running: &running,
            limits: &limits,
            usage: &usage,
            credits: &credits,
            now,
        })?;
        let mut c = c;
        for s in avoided {
            let join = if c.skipped.is_empty() { "; skipped" } else { "," };
            c.reason = format!("{}{join} {} {}", c.reason, s.instance_id, s.reason);
            c.skipped.push(s);
        }
        let at = self.now();
        self.store.pool_credits_put(name, &c.credits)?;
        let seq = self.store.pick_add(&PickRecord {
            pool: name.into(),
            instance_id: c.instance_id.clone(),
            model: c.model.clone(),
            strategy: pool.strategy.clone(),
            reason: c.reason.clone(),
            skipped: serde_json::to_value(&c.skipped)?,
            at,
            handle: None,
            thread: None,
            moved_from: None,
        })?;
        Ok(Picked {
            pool: name.into(),
            model_selection: model_selection(&c.instance_id, &c.model, &c.options),
            instance_id: c.instance_id,
            driver: c.driver,
            model: c.model,
            effort: c.effort,
            options: c.options,
            reason: c.reason,
            skipped: c.skipped,
            seq,
        })
    }

    pub fn picked(&self, seq: i64, thread: &str) -> Result<Value> {
        let thread = thread.trim();
        if thread.is_empty() {
            bail!("wf.picked: empty thread");
        }
        let p =
            self.store.pick_get(seq)?.ok_or_else(|| anyhow!("wf.picked: no pick {seq}; pass the pick from wf.pick"))?;
        match p.thread.as_deref() {
            Some(t) if t != thread => {
                bail!("wf.picked: pick {seq} already launched thread {t}; pick again for another launch")
            }
            Some(_) => {}
            None => self.store.pick_bind(seq, p.handle.as_deref(), thread, None)?,
        }
        Ok(json!({"pick": seq, "pool": p.pool, "instanceId": p.instance_id, "thread": thread}))
    }

    fn members(&self) -> BTreeSet<String> {
        self.config.pools.values().flat_map(|p| p.members.iter().map(|m| m.instance_id.clone())).collect()
    }

    async fn stopped_on_limit(&self, t: &Value, thread: &str, run: &str) -> Result<bool> {
        let end = t["itemCount"].as_u64().unwrap_or(LAST_POSITION);
        let r = self
            .t3
            .call(
                "t3_thread_read",
                json!({"threadId": thread, "view": "activity", "beforePosition": end,
                       "limit": STOP_ITEMS, "maxCharsPerItem": 300}),
            )
            .await?;
        let items = r["items"].as_array().map(Vec::as_slice).unwrap_or_default();
        Ok(items.iter().any(|i| i["type"] == "error" && i["title"] == LIMIT_TITLE && i["runId"] == run))
    }

    async fn limit_from_thread(&self, t: &Value, members: &BTreeSet<String>) -> Result<Option<(String, Limit)>> {
        let Some(instance) = t["providerInstanceId"].as_str().filter(|i| members.contains(*i)) else {
            return Ok(None);
        };
        let (Some(thread), Some(run)) = (t["threadId"].as_str(), t["latestRunId"].as_str()) else {
            return Ok(None);
        };
        if self.mins(t["updatedAt"].as_str()).is_none_or(|m| m > LOOKBACK_MINUTES) {
            return Ok(None);
        }
        let now = self.clock.now();
        let stop = match self.store.limit_stop(thread, run)? {
            Some(s) => s,
            None => {
                let limit = self.stopped_on_limit(t, thread, run).await?;
                let hold = self.config.limit_hold_minutes;
                let (until, how) = match snooze_until(t) {
                    _ if !limit => (None, None),
                    Some(u) => (Some(u), Some("reset time from T3's limit snooze".to_string())),
                    None => (
                        Some(iso(plus_minutes(now, hold as f64))),
                        Some(format!("reset time unknown, held {hold} min")),
                    ),
                };
                let s = LimitStop { limit, seen_at: iso(now), until, how };
                self.store.limit_stop_put(thread, run, &s)?;
                s
            }
        };
        let (Some(until), Some(how)) = (stop.until, stop.how) else {
            return Ok(None);
        };
        if parse_ts(&until).is_none_or(|u| u <= now) {
            return Ok(None);
        }
        let title = t["title"].as_str().filter(|s| !s.is_empty()).unwrap_or(thread);
        let limit = Limit {
            until,
            reason: Some(format!("usage limit on thread {title}; {how}")),
            at: iso(now),
            thread: Some(thread.to_string()),
        };
        Ok(Some((instance.to_string(), limit)))
    }

    pub async fn mark_limits(&self) -> Result<Vec<String>> {
        if self.config.pools.is_empty() {
            return Ok(Vec::new());
        }
        let members = self.members();
        let mut want: BTreeMap<String, Limit> = BTreeMap::new();
        let mut keep = |id: String, l: Limit| match want.get(&id) {
            Some(w) if !later(&l.until, &w.until) => {}
            _ => {
                want.insert(id, l);
            }
        };
        let failed = self.threads_with(&["failed"]).await?;
        for t in &failed {
            match self.limit_from_thread(t, &members).await {
                Ok(Some((id, l))) => keep(id, l),
                Ok(None) => {}
                Err(e) => tracing::warn!(
                    "limit check skipped thread {}: {}",
                    t["threadId"].as_str().unwrap_or("?"),
                    self.t3.redact(format!("{e:#}"))
                ),
            }
        }
        let ids: Vec<String> = members.iter().cloned().collect();
        let usage = self.usage_of(&ids).await;
        let now = self.clock.now();
        for (id, u) in current(usage, now) {
            if let Some(until) = u.resets_at.filter(|r| u.left <= 0.0 && parse_ts(r).is_some_and(|r| r > now)) {
                let reason = Some("the usage source reports no usage left".to_string());
                keep(id, Limit { until, reason, at: iso(now), thread: None });
            }
        }
        let mut actions = Vec::new();
        let _g = self.pool_lock.lock().await;
        let stored = self.store.limits()?;
        for (id, l) in &stored {
            if active_limit(&stored, id, now).is_none() {
                self.store.limit_clear(id)?;
                actions.push(format!("limit cleared {id}, reset {} passed", local_clock(&l.until)));
            }
        }
        for (id, l) in want {
            let replace = active_limit(&stored, &id, now).is_none_or(|s| later(&l.until, &s.until));
            if replace {
                actions.push(format!("limited {id} until {}", local_clock(&l.until)));
                self.store.limit_set(&id, &l)?;
            }
        }
        drop(_g);
        actions.extend(self.offer_moves(&failed).await);
        if !actions.is_empty() {
            self.changed();
        }
        Ok(actions)
    }

    pub async fn pools(&self) -> Result<Value> {
        let running = self.running_by_instance().await;
        let limits = self.store.limits()?;
        let reported = self.usage_of(&self.members().into_iter().collect::<Vec<_>>()).await;
        let mut out = Vec::new();
        for (name, pool) in &self.config.pools {
            let caps = match &running {
                Ok(_) => self.pool_capabilities(pool).await,
                Err(e) => Err(anyhow!("{e:#}")),
            };
            let now = self.clock.now();
            let usage = current(reported.clone(), now);
            let error = match &caps {
                Ok(c) => check_members(name, pool, c).err().map(|e| format!("{e:#}")),
                Err(e) => Some(format!("T3 did not answer: {e:#}")),
            };
            let warnings = match (&caps, &error) {
                (Ok(c), None) => sole_warnings(pool, c),
                _ => Vec::new(),
            };
            let stats = self.store.pick_stats(name)?;
            let members: Vec<Value> = pool
                .members
                .iter()
                .map(|m| {
                    let limit = active_limit(&limits, &m.instance_id, now);
                    let left = usage.get(&m.instance_id);
                    let p = caps.as_ref().ok().and_then(|c| provider(c, &m.instance_id));
                    let state = match (p, limit) {
                        (Some(p), _) if error.is_none() => Some(member_state(pool, m, p, limit, left)),
                        (_, Some(l)) => Some(State::Limited(l.until.clone())),
                        _ => None,
                    };
                    let (picks, last) = stats.get(&m.instance_id).cloned().unwrap_or((0, None));
                    let driver = p.and_then(|p| p["driverKind"].as_str()).or(pool.driver.as_deref());
                    json!({
                        "instanceId": m.instance_id,
                        "driver": driver,
                        "model": pool.model_of(m),
                        "effort": pool.effort_of(m),
                        "weight": m.weight,
                        "reserve": m.reserve,
                        "cost": m.cost,
                        "state": state.as_ref().map_or("unknown", State::name),
                        "reason": state.as_ref().and_then(|s| s.why(pool, m)),
                        "limitedUntil": limit.map(|l| l.until.clone()),
                        "limitedUntilLocal": limit.map(|l| local_clock(&l.until)),
                        "limitReason": limit.and_then(|l| l.reason.clone()),
                        "usageLeft": left.map(|u| u.left),
                        "running": running.as_ref().ok().map(|r| r.get(&m.instance_id).copied().unwrap_or(0)),
                        "picks": picks,
                        "lastPick": last,
                        "lastPickLocal": last.as_deref().map(local_clock),
                    })
                })
                .collect();
            out.push(json!({
                "pool": name,
                "mixed": pool.mixed,
                "driver": pool.driver,
                "model": pool.model,
                "effort": pool.effort,
                "strategy": pool.strategy,
                "error": error,
                "warnings": warnings,
                "members": members,
            }));
        }
        Ok(Value::Array(out))
    }
}
