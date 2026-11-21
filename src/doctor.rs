use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::agent_link::{self, Agent, LinkState};
use crate::config::Config;
use crate::drift::{self, Home};
use crate::skills::Bundle;
use crate::slots::{self, Enabled, Folders, Inputs, Resolved};
use crate::store::InstanceFolder;

#[derive(Debug, Default)]
pub struct Findings {
    pub problems: Vec<String>,
    pub warnings: Vec<String>,
}

impl Findings {
    pub fn extend(&mut self, other: Findings) {
        self.problems.extend(other.problems);
        self.warnings.extend(other.warnings);
    }
}

pub fn skills(home: &Home, bundle: &Bundle) -> Findings {
    let mut f = Findings::default();
    let d = drift::check(home, bundle);
    if let Some(e) = &d.error {
        f.problems.push(format!("skills: {e}"));
    } else if !d.present {
        f.warnings.push("skills: t3cm has not installed the T3 workflow skills; run t3cm skills install".into());
    } else if !d.current {
        let fix = if d.fixes.is_empty() { "t3cm skills update".to_string() } else { d.fixes.join(", then ") };
        f.problems.push(format!("skills: not current with release {}; fix: {fix}", d.release));
    } else if let Some(p) = &d.problem {
        f.problems.push(format!("setup: {p}"));
    } else if !d.fixes.is_empty() {
        f.warnings.push(format!("setup: {}", d.fixes.join(", then ")));
    }
    f
}

pub fn stale_copies(home: &Home, bundle: &Bundle, codex: &Path) -> Findings {
    let mut f = Findings::default();
    let names = bundle.skills();
    for agent in Agent::all(&home.env) {
        for l in agent_link::check(&agent, &home.paths.dir, &names) {
            if l.state == LinkState::Taken {
                f.problems.push(format!(
                    "skills: {} is a copy, not a link to {}, so {} loads it instead; remove it and run t3cm skills update",
                    l.path.display(),
                    l.target.display(),
                    agent.name
                ));
            }
        }
    }
    for name in &names {
        let path = codex.join("skills").join(name);
        let target = home.paths.dir.join(name);
        if std::fs::symlink_metadata(&path).is_ok() && slots::realpath(&path) != slots::realpath(&target) {
            f.problems.push(format!(
                "skills: {} is a copy, not a link to {}, so Codex loads it instead; remove it",
                path.display(),
                target.display()
            ));
        }
    }
    f
}

fn t3wf_state(folders: &Folders) -> PathBuf {
    let var = |k| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    if let Some(p) = var("T3WF_STATE_DIR") {
        return p;
    }
    var("XDG_STATE_HOME").unwrap_or_else(|| folders.home.join(".local").join("state")).join("t3-workflow")
}

pub fn t3wf_leftovers(folders: &Folders) -> Findings {
    let mut f = Findings::default();
    let units = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| folders.home.join(".config"))
        .join("systemd")
        .join("user");
    for unit in ["t3wf-tick.timer", "cbrip-t3wf-tick.timer"] {
        if units.join(unit).exists() {
            f.warnings.push(format!(
                "t3wf: {} is installed; the t3cm service watches links itself",
                units.join(unit).display()
            ));
        }
    }
    let links = t3wf_state(folders).join("v2").join("links.json");
    if let Ok(raw) = std::fs::read(&links)
        && let Ok(v) = serde_json::from_slice::<Value>(&raw)
    {
        let open = v["links"].as_object().map_or(0, |m| m.values().filter(|l| l["closed"] != true).count());
        let queued = v["outbox"].as_array().map_or(0, Vec::len);
        if open > 0 || queued > 0 {
            f.warnings.push(format!(
                "t3wf: {} has {open} open link(s) and {queued} undelivered message(s); keep it until they close",
                links.display()
            ));
        } else {
            f.warnings
                .push(format!("t3wf: {} has no open links or undelivered messages; safe to delete", links.display()));
        }
    }
    f
}

pub fn t3cm_config(path: &Path) -> (Option<Config>, Findings) {
    let mut f = Findings::default();
    match Config::load(path) {
        Ok(c) => (Some(c), f),
        Err(e) => {
            f.problems.push(format!("t3cm config {}: {e:#}", path.display()));
            (None, f)
        }
    }
}

pub fn pools(eff: &Value, cfg: Option<&Config>) -> Findings {
    let mut f = Findings::default();
    let Some(cfg) = cfg else { return f };
    for (role, m) in eff["models"].as_object().into_iter().flatten() {
        if let Some(pool) = m["spec"]["pool"].as_str()
            && !cfg.pools.contains_key(pool)
        {
            f.problems.push(format!("model {role}: pool {pool} is not in t3cm's config"));
        }
    }
    f
}

pub fn legacy_board(cfg: Option<&Config>) -> Findings {
    let mut f = Findings::default();
    let Some(cfg) = cfg else { return f };
    let keys = &cfg.board.legacy_ticket_keys;
    if !keys.is_empty() {
        let names: Vec<String> = keys.iter().map(|k| format!("board.{k}")).collect();
        let ask_keys: Vec<String> =
            keys.iter().filter(|k| matches!(k.as_str(), "instanceId" | "pool")).map(|k| format!("board.{k}")).collect();
        let (verb, pron) = if names.len() == 1 {
            ("is a legacy ticket launch setting", "it")
        } else {
            ("are legacy ticket launch settings", "them")
        };
        let tail = if ask_keys.is_empty() {
            format!(
                "move {pron} into the models.ticket slot (run setup-t3-workflow or edit ~/.config/t3-workflow/config.json)"
            )
        } else {
            let (ask_pron, ask_verb) =
                if ask_keys.len() == 1 { ("it", "still configures") } else { ("them", "still configure") };
            let ask_list = ask_keys.join(" and ");
            format!(
                "set the models.ticket slot instead (run setup-t3-workflow or edit ~/.config/t3-workflow/config.json), which takes precedence for tickets; keep {ask_list} if you use {ask_pron} for the ask box ({ask_list} {ask_verb} the ask box)",
            )
        };
        f.warnings.push(format!("t3cm config: {} {verb}; {tail}", names.join(", ")));
    }
    f
}

pub fn service(health: Option<&Value>, unreachable: Option<&str>) -> Findings {
    let mut f = Findings::default();
    match (health, unreachable) {
        (_, Some(e)) => f.problems.push(format!("service: {e}")),
        (Some(h), None) if h["t3"]["connected"] != true => {
            let why = h["t3"]["lastError"].as_str().unwrap_or("no T3 connection yet");
            f.problems.push(format!("service: up, but not connected to T3 ({why})"));
        }
        _ => {}
    }
    f
}

fn enabled_instances(caps: &Value) -> Option<Vec<String>> {
    let providers = caps["providers"].as_array()?;
    Some(
        providers
            .iter()
            .filter(|p| {
                !p["constraints"]
                    .as_array()
                    .is_some_and(|c| c.iter().any(|x| x.to_string().to_lowercase().contains("disabled")))
            })
            .filter_map(|p| p["providerInstanceId"].as_str().map(String::from))
            .collect(),
    )
}

pub fn instances(records: &[InstanceFolder], here: &Folders, root: &Path, caps: Option<&Value>) -> Findings {
    let mut f = Findings::default();
    let enabled = caps.and_then(enabled_instances);
    for r in records {
        if enabled.as_ref().is_some_and(|e| !e.contains(&r.instance)) {
            continue;
        }
        let mut folders = here.clone();
        let folder = PathBuf::from(&r.folder);
        if r.driver == "codex" {
            if slots::realpath(&folder) == slots::realpath(&here.codex) {
                continue;
            }
            folders.codex = folder;
        } else {
            if slots::realpath(&folder) == slots::realpath(&here.claude) {
                continue;
            }
            folders.claude = folder;
        }
        let en = Enabled::load(&folders.claude, root);
        let res = Resolved::build(&Inputs { folders: &folders, root, listed: &[], enabled: &en });
        let eff = json!({"review": res.review, "passes": res.passes, "credentials": res.credentials, "delivery": res.delivery});
        let (problems, warnings) = slots::skill_problems(&eff);
        f.problems.extend(problems.into_iter().map(|p| format!("instance {}: {p}", r.instance)));
        f.warnings.extend(warnings.into_iter().map(|w| format!("instance {}: {w}", r.instance)));
    }
    f
}

pub fn lines(eff: &Value, f: &Findings) -> Vec<String> {
    let mut out = slots::report_lines(eff);
    out.extend(f.warnings.iter().map(|w| format!("warn: {w}")));
    out.extend(f.problems.iter().map(|p| format!("PROBLEM: {p}")));
    out.push(if f.problems.is_empty() {
        "doctor: ok".into()
    } else {
        format!("doctor: {} problem(s)", f.problems.len())
    });
    out
}

pub fn resolve_lines(name: &str, resolved: &Value, cands: &[slots::Skill]) -> Vec<String> {
    let mut out = Vec::new();
    if resolved["status"] == "missing" {
        out.push(format!("{name}: not installed{}", if cands.is_empty() { "" } else { "; other matches:" }));
    }
    for c in cands {
        out.push(format!(
            "{}: {}{} ({})",
            c.name,
            c.path.as_deref().unwrap_or("(listed in this session)"),
            if c.inline { " [inline]" } else { "" },
            c.at
        ));
    }
    out
}
