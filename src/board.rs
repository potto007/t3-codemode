use std::process::Stdio;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::clock::{local_clock, parse_ts};
use crate::contexts::ProjectFilter;
use crate::issues::issue_key;
use crate::moves::Offer;
use crate::pools::Picked;
use crate::reconcile::{CardPr, card_pr};
use crate::store::{Link, Pending};
use crate::t3::{BoxFut, truncate_chars};
use crate::wf::{ACTIVE, ENDED_BADLY, WATCHER, Wf, capped, count, header};

pub const BOARD: &str = "board";
pub const CARD_KINDS: &[&str] = &["gate", "blocker", "report", "escalate"];
const FEED_KINDS: &[&str] = &["note", "event"];
const DONE_SHOWN: usize = 20;
const FEED_MAX: usize = 200;
const ASKS_SHOWN: usize = 8;
const SETTLED_SHOWN: usize = 200;
const MAX_OPTIONS: usize = 8;
const MAX_OPTION_CHARS: usize = 60;
const NO_BUTTONS: &str = "this gate's card has no reply buttons, so the user must type an answer; \
    send it again with options: [\"<answer>\", ...], or quote each answer after reply on its **ACTION NEEDED:** line";
const MAX_TEXT: usize = 2000;
const ANSWER_CAP: usize = 1500;
const PROJECT_RETRY_MINS: f64 = 30.0;

pub trait Remotes: Send + Sync {
    fn origin<'a>(&'a self, root: &'a str) -> BoxFut<'a, Option<String>>;
}

pub struct GitRemotes;

impl Remotes for GitRemotes {
    fn origin<'a>(&'a self, root: &'a str) -> BoxFut<'a, Option<String>> {
        Box::pin(async move {
            let run = tokio::process::Command::new("git")
                .args(["-C", root, "remote", "get-url", "origin"])
                .env("GIT_TERMINAL_PROMPT", "0")
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .output();
            let out = tokio::time::timeout(Duration::from_secs(5), run).await.ok()?.ok()?;
            let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
            (out.status.success() && !url.is_empty()).then_some(url)
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Issue {
    pub owner: String,
    pub repo: String,
    pub number: u64,
}

impl Issue {
    pub fn url(&self) -> String {
        format!("https://github.com/{}/{}/issues/{}", self.owner, self.repo, self.number)
    }

    pub fn label(&self) -> String {
        format!("{}#{}", self.repo, self.number)
    }
}

pub(crate) fn gh_name(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix).then(|| &s[prefix.len()..])
}

pub fn parse_issue(raw: &str) -> Option<Issue> {
    parse_ref(raw, true)
}

pub fn parse_issue_only(raw: &str) -> Option<Issue> {
    parse_ref(raw, false)
}

fn parse_ref(raw: &str, allow_pull: bool) -> Option<Issue> {
    let s = raw.trim().trim_end_matches('/');
    let make = |owner: &str, repo: &str, n: &str| {
        let number: u64 = n.parse().ok().filter(|n| *n > 0)?;
        (gh_name(owner) && gh_name(repo)).then(|| Issue { owner: owner.into(), repo: repo.into(), number })
    };
    if !s.contains("://")
        && strip_prefix_ci(s, "github.com/").is_none()
        && let Some((path, n)) = s.split_once('#')
    {
        let (owner, repo) = path.split_once('/')?;
        return make(owner, repo, n);
    }
    let rest = strip_prefix_ci(s, "https://").or_else(|| strip_prefix_ci(s, "http://")).unwrap_or(s);
    let rest = strip_prefix_ci(rest, "www.").unwrap_or(rest);
    let rest = strip_prefix_ci(rest, "github.com/")?;
    let rest = rest.split(['?', '#']).next()?;
    let parts: Vec<&str> = rest.split('/').filter(|p| !p.is_empty()).collect();
    match parts.as_slice() {
        [owner, repo, "issues", n, ..] => make(owner, repo, n),
        [owner, repo, "pull", n, ..] if allow_pull => make(owner, repo, n),
        _ => None,
    }
}

pub fn parse_remote(url: &str) -> Option<(String, String)> {
    let url = url.trim();
    let (host, path) = match url.split_once("://") {
        Some((_, rest)) => {
            let (authority, path) = rest.split_once('/')?;
            let host = authority.rsplit('@').next()?;
            (host.split(':').next()?, path)
        }
        None => {
            let (left, path) = url.split_once(':')?;
            (left.rsplit('@').next()?, path)
        }
    };
    if !host.eq_ignore_ascii_case("github.com") {
        return None;
    }
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    match path.split('/').collect::<Vec<_>>().as_slice() {
        [owner, repo] if gh_name(owner) && gh_name(repo) => Some((owner.to_string(), repo.to_string())),
        _ => None,
    }
}

const ACTION_MARK: &str = "**ACTION NEEDED:**";
const REPLY_VERBS: &[&str] = &["reply", "answer", "say"];

pub fn card_text(body: &str) -> String {
    let line = body
        .lines()
        .find_map(|l| l.find(ACTION_MARK).map(|i| l[i + ACTION_MARK.len()..].trim().to_string()))
        .filter(|l| !l.is_empty())
        .or_else(|| body.lines().map(str::trim).find(|l| !l.is_empty()).map(String::from))
        .unwrap_or_default();
    truncate_chars(&line, 400)
}

pub fn reply_options(body: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let action_lines: Vec<&str> =
        body.lines().filter_map(|l| l.find(ACTION_MARK).map(|i| &l[i + ACTION_MARK.len()..])).collect();
    if action_lines.is_empty() {
        reply_phrases(body, &mut out);
    } else {
        for line in action_lines {
            quoted_after_verb(line, &mut out);
        }
    }
    out
}

fn option_fault(out: &[String], opt: &str) -> Option<&'static str> {
    if opt.is_empty() {
        Some("is empty")
    } else if opt.contains('\n') {
        Some("runs onto a second line")
    } else if opt.chars().count() > MAX_OPTION_CHARS {
        Some("is over 60 characters")
    } else if out.iter().any(|o| o.to_lowercase() == opt.to_lowercase()) {
        Some("repeats an earlier option")
    } else {
        None
    }
}

fn add_option(out: &mut Vec<String>, opt: &str) {
    let opt = opt.trim();
    if out.len() < MAX_OPTIONS && option_fault(out, opt).is_none() {
        out.push(opt.to_string());
    }
}

pub fn checked_options(opts: &[String]) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    let mut faults = Vec::new();
    for (i, opt) in opts.iter().enumerate() {
        let opt = opt.trim();
        match option_fault(&out, opt) {
            Some(why) => faults.push(format!("options[{i}] {} {why}", json!(opt))),
            None => out.push(opt.to_string()),
        }
    }
    if opts.len() > MAX_OPTIONS {
        faults.push(format!("{} options given, and a card shows at most {MAX_OPTIONS}", opts.len()));
    }
    if !faults.is_empty() {
        bail!(
            "wf.send: {}; each option is one line of 1 to {MAX_OPTION_CHARS} characters, unique ignoring case",
            faults.join("; ")
        );
    }
    Ok(out)
}

fn after_word_break(s: &str, i: usize) -> bool {
    !s[..i].chars().next_back().is_some_and(char::is_alphanumeric)
}

fn reply_phrases(body: &str, out: &mut Vec<String>) {
    for (i, _) in body.char_indices() {
        let Some(rest) = strip_prefix_ci(&body[i..], "reply") else { continue };
        if !after_word_break(body, i) {
            continue;
        }
        let rest = rest.trim_start_matches([' ', '\t']);
        let mut chars = rest.chars();
        let close = match chars.next() {
            Some('"') => '"',
            Some('\'') => '\'',
            Some('\u{201c}') => '\u{201d}',
            Some('\u{2018}') => '\u{2019}',
            _ => continue,
        };
        let inner = chars.as_str();
        if let Some(end) = inner.find(close) {
            add_option(out, &inner[..end]);
        }
    }
}

fn quoted_after_verb(line: &str, out: &mut Vec<String>) {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let is_verb = |i: usize| {
        !line[..i].chars().next_back().is_some_and(word)
            && REPLY_VERBS.iter().any(|v| strip_prefix_ci(&line[i..], v).is_some_and(|rest| !rest.starts_with(word)))
    };
    let mut armed = false;
    let mut i = 0;
    while let Some(c) = line[i..].chars().next() {
        let from = i + c.len_utf8();
        let close = match c {
            '"' => Some(('"', false)),
            '\u{201c}' => Some(('\u{201d}', false)),
            '\'' if after_word_break(line, i) => Some(('\'', true)),
            '\u{2018}' if after_word_break(line, i) => Some(('\u{2019}', true)),
            _ => None,
        };
        let end = close.and_then(|(close, single)| {
            line[from..]
                .match_indices(close)
                .map(|(e, _)| from + e)
                .find(|&e| !single || !line[e + close.len_utf8()..].starts_with(char::is_alphanumeric))
                .map(|e| (e, e + close.len_utf8()))
        });
        match end {
            Some((end, next)) => {
                if armed {
                    add_option(out, &line[from..end]);
                }
                i = next;
            }
            None => {
                armed |= is_verb(i);
                i = from;
            }
        }
    }
}

#[derive(Default)]
pub struct CardRef<'a> {
    pub at: Option<&'a str>,
    pub token: Option<&'a str>,
}

fn same_card(card: &Card, id: &str, want: &CardRef) -> Result<()> {
    let stale_at = want.at.is_some_and(|at| at != card.at);
    let stale_token = want.token.is_some_and(|t| t != card.token);
    if stale_at || stale_token {
        bail!("no open board card {id:?} for that decision; a newer decision has that id, so nothing was done");
    }
    Ok(())
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Card {
    pub id: String,
    pub kind: String,
    pub label: Option<String>,
    pub ticket: Option<String>,
    pub link: Option<String>,
    pub text: String,
    pub handle: Option<String>,
    pub options: Vec<String>,
    pub at: String,
    pub at_local: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    pub token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval: Option<String>,
    #[serde(skip)]
    pub seq: i64,
    #[serde(skip)]
    pub pr: Option<CardPr>,
    #[serde(skip)]
    pub offer: Option<Offer>,
}

pub struct NewCard<'a> {
    pub kind: &'a str,
    pub label: Option<&'a str>,
    pub ticket: Option<&'a str>,
    pub link: Option<&'a str>,
    pub body: &'a str,
    pub handle: Option<&'a str>,
    pub options: Option<Vec<String>>,
    pub pr: Option<CardPr>,
    pub approval: Option<&'a str>,
    pub offer: Option<&'a Offer>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchArgs {
    pub issue: String,
    pub project: Option<String>,
    pub title: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
}

pub(crate) fn random_id() -> String {
    let mut b = [0u8; 12];
    let _ = getrandom::fill(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn nonempty(s: Option<String>) -> Option<String> {
    s.map(|x| x.trim().to_string()).filter(|x| !x.is_empty())
}

pub fn ticket_brief(url: &str, label: &str) -> String {
    format!(
        "Work {url} with the t3-ticket skill.\n\n\
         ## t3-codemode link\n\
         This thread reports to the t3-codemode board, not to a parent thread. Before anything else, call \
         mcp__t3cm__execute with:\n\
         async () => {{ await wf.join({{ parent: \"board\", label: {}, role: \"ticket\" }}); return \"joined\"; }}\n\
         Send gates, blockers and the handoff with wf.send; the user answers them on the board and the answer \
         arrives here as a [t3cm reply]. Progress is wf.send({{ kind: \"note\" }}).",
        json!(label)
    )
}

pub fn ask_brief(question: &str) -> String {
    format!(
        "Answer this question about the t3-codemode board, then stop:\n\n{question}\n\n\
         Use only the mcp__t3cm__execute tool, and only to read. These calls read the board:\n\
         - wf.board({{ parent: \"board\", all: true }}): every ticket, finished ones included, with its status, gate, link and workers\n\
         - wf.cards({{ all: true }}): every decision card, open or resolved, with how each was resolved\n\
         - wf.feed(): the latest notes and events\n\
         - wf.show(\"<handle>\"): one stored message in full\n\
         For example: async () => ({{ board: await wf.board({{ parent: \"board\", all: true }}), cards: await wf.cards({{ all: true }}) }})\n\
         Answer in at most 6 lines of plain text. Name a thread by its link. Do not message, launch, answer or \
         change anything."
    )
}

fn kept(mut items: Vec<Value>, filter: Option<&ProjectFilter>) -> Vec<Value> {
    if let Some(f) = filter {
        items.retain(|v| f.keeps(v));
    }
    items
}

pub fn pr_of(th: &Value) -> Value {
    let last = th["pullRequests"].as_array().and_then(|a| a.last()).cloned();
    let pr = last.or_else(|| Some(th["linkedPullRequest"].clone()).filter(Value::is_object));
    match pr {
        Some(p) if p["url"].is_string() => json!({
            "number": p["number"], "url": p["url"], "state": p.get("state").cloned().unwrap_or(Value::Null),
            "repository": p["repository"],
        }),
        _ => Value::Null,
    }
}

pub fn chip(live: Option<&Value>) -> &'static str {
    let Some(th) = live else { return "unknown" };
    let status = th["status"].as_str().unwrap_or("");
    if status == "gone" {
        "gone"
    } else if ACTIVE.contains(&status) {
        "running"
    } else if status == "waiting" || th["asks"].as_i64().unwrap_or(0) > 0 {
        "waiting"
    } else if ENDED_BADLY.contains(&status) {
        "failed"
    } else {
        "idle"
    }
}

fn web_link(v: Option<String>) -> Option<String> {
    v.filter(|l| l.starts_with("https://") || l.starts_with("http://"))
}

fn owner_repo((owner, repo): (String, String)) -> String {
    format!("{owner}/{repo}")
}

pub fn identity_repo(project: &Value) -> Option<String> {
    let id = &project["repositoryIdentity"];
    match (id["owner"].as_str(), id["name"].as_str()) {
        (Some(o), Some(n)) if gh_name(o) && gh_name(n) => Some(format!("{o}/{n}")),
        _ => id["locator"]["remoteUrl"].as_str().and_then(parse_remote).map(owner_repo),
    }
}

impl Wf {
    pub fn changed(&self) {
        let _ = self.changes.send(());
    }

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<()> {
        self.changes.subscribe()
    }

    pub fn set_release(&self, latest: crate::release::Version) {
        let mut slot = self.release.lock().unwrap();
        if slot.as_ref() != Some(&latest) {
            *slot = Some(latest);
            drop(slot);
            self.changed();
        }
    }

    pub fn set_skills_home(&self, home: crate::drift::Home) {
        self.skills.set_home(home);
        self.refresh_skills();
    }

    pub fn skills_view(&self) -> Value {
        self.skills.view()
    }

    pub fn refresh_skills(&self) -> Value {
        let (v, changed) = self.skills.refresh();
        if changed {
            self.changed();
        }
        v
    }

    pub fn update_view(&self) -> Value {
        let latest = self.release.lock().unwrap().clone();
        let releases = crate::release::releases_url().unwrap_or_else(|_| crate::release::RELEASES.to_string());
        crate::release::view(&crate::release::Version::current(), latest.as_ref(), &releases)
    }

    pub(crate) fn note_live(&self, id: &str, th: &Value) {
        let v = json!({
            "status": th["status"], "asks": count(&th["pendingRequestCount"]), "link": th["link"],
            "pr": pr_of(th), "title": th["title"], "prStale": th["pullRequestsStale"] == true,
            "prs": th["pullRequests"].as_array().map(|a| a.iter().filter_map(|p| p["url"].as_str()).collect::<Vec<_>>()),
            "project": th["projectId"],
        });
        let mut live = self.live.lock().unwrap_or_else(|e| e.into_inner());
        if live.get(id) != Some(&v) {
            live.insert(id.to_string(), v);
            drop(live);
            self.changed();
        }
    }

    pub(crate) fn live_of(&self, id: &str) -> Option<Value> {
        self.live.lock().unwrap_or_else(|e| e.into_inner()).get(id).cloned()
    }

    fn known_project(&self, id: &str) -> Option<Value> {
        self.known_projects.lock().unwrap_or_else(|e| e.into_inner()).get(id).cloned()
    }

    fn remember_project(&self, id: &str, title: &Value, repo: Option<String>) -> bool {
        let tried = repo.is_none().then(|| self.now());
        let v = json!({"title": title, "repo": repo, "triedAt": tried});
        let mut known = self.known_projects.lock().unwrap_or_else(|e| e.into_inner());
        if known.get(id) == Some(&v) {
            return false;
        }
        known.insert(id.to_string(), v);
        true
    }

    fn place(&self, project: Option<&str>, live: &Value) -> (Value, Value) {
        let id = project.or_else(|| live["project"].as_str());
        let known = id.and_then(|i| self.known_project(i)).unwrap_or(Value::Null);
        let pr_repo = live["pr"]["repository"].as_str().filter(|r| r.contains('/'));
        let repo = known["repo"].as_str().or(pr_repo);
        let project = id.map_or(Value::Null, |i| json!({"id": i, "title": known["title"]}));
        (json!(repo), project)
    }

    fn place_of(&self, thread: &str) -> (Value, Value) {
        let ln = self.store.link_get(thread).ok().flatten();
        let live = self.live_of(thread).unwrap_or(Value::Null);
        self.place(ln.as_ref().and_then(|l| l.project.as_deref()), &live)
    }

    fn placed(&self, mut v: Value, thread: Option<&str>) -> Value {
        let (repo, project) = thread.map_or((Value::Null, Value::Null), |t| self.place_of(t));
        v["repo"] = repo;
        v["project"] = project;
        v
    }

    async fn origin_repo(&self, project: &Value) -> Option<String> {
        let root = project["workspaceRoot"].as_str()?;
        self.remotes.origin(root).await.as_deref().and_then(parse_remote).map(owner_repo)
    }

    async fn hydrate(&self, ln: &Link) -> Result<Option<String>> {
        if self.live_of(&ln.child).is_none() {
            let _ = self.read_thread(&ln.child).await;
        }
        if ln.project.is_some() {
            return Ok(ln.project.clone());
        }
        let Some(project) = self.live_of(&ln.child).and_then(|v| v["project"].as_str().map(String::from)) else {
            return Ok(None);
        };
        let _g = self.lock.lock().await;
        if let Some(mut fresh) = self.store.link_get(&ln.child)?
            && fresh.project.is_none()
        {
            fresh.project = Some(project.clone());
            self.store.link_save(&fresh)?;
        }
        Ok(Some(project))
    }

    pub(crate) async fn learn_projects(&self) -> Result<()> {
        let mut links = Vec::new();
        for t in self.store.children(BOARD, false)? {
            links.extend(self.store.children(&t.child, false)?);
            links.push(t);
        }
        let mut ids: Vec<String> = Vec::new();
        for ln in links {
            ids.extend(self.hydrate(&ln).await?);
        }
        ids.extend(
            self.live
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .values()
                .filter_map(|v| v["project"].as_str())
                .map(String::from),
        );
        ids.sort();
        ids.dedup();
        ids.retain(|i| match self.known_project(i) {
            None => true,
            Some(k) => k["repo"].is_null() && self.mins(k["triedAt"].as_str()).is_none_or(|m| m >= PROJECT_RETRY_MINS),
        });
        if ids.is_empty() {
            return Ok(());
        }
        let mut changed = false;
        let mut seen: Vec<String> = Vec::new();
        for p in self.project_list().await? {
            let Some(id) = p["id"].as_str().filter(|i| ids.iter().any(|w| w == i)) else { continue };
            seen.push(id.to_string());
            let repo = match identity_repo(&p) {
                Some(r) => Some(r),
                None => self.origin_repo(&p).await,
            };
            changed |= self.remember_project(id, &p["title"], repo);
        }
        for id in ids.iter().filter(|i| !seen.contains(i)) {
            self.remember_project(id, &Value::Null, None);
        }
        if changed {
            self.changed();
        }
        Ok(())
    }

    pub(crate) fn alloc_waiting(
        &self,
        owner: &str,
        text: &str,
        from: Option<String>,
        link: Option<String>,
        data: Option<&Value>,
    ) -> Result<String> {
        let ids = crate::wf::waiting_ids();
        let open: Vec<String> = self.store.waiting_open(owner)?.into_iter().map(|w| w.id).collect();
        let key = format!("waiting_cursor:{owner}");
        let cursor: Option<usize> = self.store.meta_get(&key)?.and_then(|s| s.parse().ok());
        let start = cursor.map(|c| c + 1).unwrap_or(0);
        let idx = (0..ids.len())
            .map(|step| (start + step) % ids.len())
            .find(|i| !open.contains(&ids[*i]))
            .ok_or_else(|| anyhow!("the waiting list is full ({} open items); resolve some first", ids.len()))?;
        let item = crate::store::WaitingItem { id: ids[idx].clone(), text: text.into(), from, link, at: self.now() };
        self.store.waiting_insert(owner, &item, data)?;
        self.store.meta_set(&key, &idx.to_string())?;
        Ok(item.id)
    }

    pub fn cards(&self) -> Result<Vec<Card>> {
        self.cards_with(false)
    }

    pub fn cards_with(&self, all: bool) -> Result<Vec<Card>> {
        Ok(self
            .store
            .waiting_data(BOARD, all)?
            .into_iter()
            .map(|(w, d, done, seq)| {
                let (resolved, resolution) = match done {
                    Some((at, note)) => (Some(local_clock(&at)), note),
                    None => (None, None),
                };
                let d = d.unwrap_or(Value::Null);
                let s = |k: &str| d[k].as_str().map(String::from);
                Card {
                    id: w.id,
                    kind: s("kind").unwrap_or_else(|| "item".into()),
                    label: w.from,
                    ticket: s("ticket"),
                    link: w.link,
                    text: w.text,
                    handle: s("handle"),
                    options: d["options"]
                        .as_array()
                        .map(|a| a.iter().filter_map(|o| o.as_str().map(String::from)).collect())
                        .unwrap_or_default(),
                    at_local: local_clock(&w.at),
                    at: w.at,
                    resolved,
                    resolution,
                    token: seq.to_string(),
                    approval: s("approval"),
                    seq,
                    pr: serde_json::from_value(d["pr"].clone()).ok().flatten(),
                    offer: serde_json::from_value(d["move"].clone()).ok().flatten(),
                }
            })
            .collect())
    }

    pub fn cards_in(&self, all: bool, filter: Option<&ProjectFilter>) -> Result<Vec<Value>> {
        let cards = self.cards_with(all)?.iter().map(|c| self.placed(json!(c), c.ticket.as_deref())).collect();
        Ok(kept(cards, filter))
    }

    pub(crate) fn board_places(&self) -> Result<Vec<Value>> {
        let mut settled = self.settled_tickets()?;
        settled.truncate(SETTLED_SHOWN);
        let mut out = self.tree()?;
        out.extend(settled);
        out.extend(self.cards_in(false, None)?);
        out.extend(self.feed(self.config.board.feed)?);
        Ok(out)
    }

    pub(crate) fn card_add(&self, c: NewCard) -> Result<String> {
        let options = c.options.unwrap_or_else(|| reply_options(c.body));
        let pr = c.pr.or_else(|| card_pr(c.body));
        let mut data = json!({"kind": c.kind, "ticket": c.ticket, "handle": c.handle, "options": options, "pr": pr});
        if let Some(a) = c.approval {
            data["approval"] = json!(a);
        }
        if let Some(o) = c.offer {
            data["move"] = json!(o);
        }
        let id = self.alloc_waiting(
            BOARD,
            &card_text(c.body),
            c.label.map(String::from),
            c.link.map(String::from),
            Some(&data),
        )?;
        self.changed();
        Ok(id)
    }

    pub(crate) fn resolve_cards(&self, pred: impl Fn(&Card) -> bool, note: &str) -> Result<Vec<String>> {
        let mut done = Vec::new();
        for c in self.cards()?.into_iter().filter(|c| pred(c)) {
            if self.store.waiting_resolve(BOARD, &c.id, Some(note), &self.now())?.is_some() {
                done.push(c.id);
            }
        }
        if !done.is_empty() {
            self.changed();
        }
        Ok(done)
    }

    pub(crate) fn resolve_ticket_cards(&self, ticket: &str, note: &str) -> Result<Vec<String>> {
        self.resolve_cards(|c| c.ticket.as_deref() == Some(ticket), note)
    }

    pub(crate) fn board_card(&self, c: NewCard) -> Result<String> {
        if c.kind == "gate"
            && let Some(t) = c.ticket
        {
            self.resolve_cards(|o| o.kind == "gate" && o.ticket.as_deref() == Some(t), "superseded by a newer gate")?;
        }
        self.card_add(c)
    }

    pub(crate) fn board_receive(
        &self,
        ln: &Link,
        kind: &str,
        handle: &str,
        body: &str,
        options: Option<Vec<String>>,
    ) -> Result<Value> {
        if ln.closed {
            self.resolve_ticket_cards(&ln.child, "ticket finished")?;
            self.changed();
            return Ok(json!({"handle": handle, "delivery": "board", "closed": true,
                "detail": "the board marks this ticket done"}));
        }
        if CARD_KINDS.contains(&kind) {
            let options = options.unwrap_or_else(|| reply_options(body));
            let id = self.board_card(NewCard {
                kind,
                label: Some(&ln.label),
                ticket: Some(&ln.child),
                link: ln.link.as_deref(),
                body,
                handle: Some(handle),
                options: Some(options.clone()),
                pr: None,
                approval: None,
                offer: None,
            })?;
            let mut out = json!({"handle": handle, "delivery": "board", "card": id, "options": options,
                "detail": format!("on the user's board as {id}; the answer arrives here as a [t3cm reply]")});
            if kind == "gate" && options.is_empty() {
                out["warning"] = json!(NO_BUTTONS);
            }
            return Ok(out);
        }
        self.changed();
        Ok(json!({"handle": handle, "delivery": "noted", "detail": "on the board's activity feed"}))
    }

    pub async fn answer(&self, id: &str, text: &str) -> Result<Value> {
        self.answer_at(id, text, &CardRef::default()).await
    }

    pub async fn answer_at(&self, id: &str, text: &str, want: &CardRef<'_>) -> Result<Value> {
        let text = text.trim();
        if text.is_empty() {
            bail!("wf.answer: empty answer text");
        }
        if text.chars().count() > MAX_TEXT {
            bail!("wf.answer: the answer is over {MAX_TEXT} chars");
        }
        let _g = self.lock.lock().await;
        let card = self
            .cards()?
            .into_iter()
            .find(|c| c.id == id)
            .ok_or_else(|| anyhow!("no open board card {id:?}; wf.cards() lists the open ones"))?;
        same_card(&card, id, want)?;
        if let Some(offer) = card.offer.clone() {
            return self.answer_offer(&card, &offer, text).await;
        }
        let ticket = card
            .ticket
            .clone()
            .ok_or_else(|| anyhow!("card {id} has no ticket thread to answer; dismiss it instead"))?;
        let body = format!("The user says {} about {id}: {}", json!(text), card.text);
        let handle = self.store.msg_new(BOARD, &ticket, "reply", &body, &self.now())?;
        let msg =
            format!("{}\n{}", header("reply", BOARD, &handle, None), capped(&body, &handle, self.config.reply_cap));
        let mut out = self.send_wake(&ticket, &msg, &handle).await?;
        self.store.waiting_resolve_seq(BOARD, card.seq, Some(&format!("answered: {text}")), &self.now())?;
        self.changed();
        out["card"] = json!(id);
        Ok(out)
    }

    pub fn dismiss(&self, id: &str) -> Result<Value> {
        match self.store.waiting_resolve(BOARD, id, Some("dismissed"), &self.now())? {
            Some(_) => {
                self.changed();
                Ok(json!({"card": id, "dismissed": true}))
            }
            None => bail!("no open board card {id:?}"),
        }
    }

    pub async fn dismiss_at(&self, id: &str, want: &CardRef<'_>) -> Result<Value> {
        let _g = self.lock.lock().await;
        let card =
            self.cards()?.into_iter().find(|c| c.id == id).ok_or_else(|| anyhow!("no open board card {id:?}"))?;
        same_card(&card, id, want)?;
        if !self.store.waiting_resolve_seq(BOARD, card.seq, Some("dismissed"), &self.now())? {
            bail!("no open board card {id:?}");
        }
        self.changed();
        Ok(json!({"card": id, "dismissed": true}))
    }

    pub fn feed(&self, limit: usize) -> Result<Vec<Value>> {
        let mut labels: Vec<(String, String, String)> = Vec::new();
        let mut done: Vec<String> = Vec::new();
        for t in self.store.children(BOARD, false)? {
            if t.closed {
                done.push(t.child.clone());
            }
            for w in self.store.children(&t.child, false)? {
                labels.push((w.child.clone(), w.label, t.child.clone()));
            }
            labels.push((t.child.clone(), t.label, t.child.clone()));
        }
        let mut to: Vec<String> = labels.iter().map(|(c, _, _)| c.clone()).collect();
        to.push(BOARD.into());
        let label_of = |id: &str| -> String {
            if id == WATCHER {
                return "watcher".into();
            }
            labels.iter().find(|(c, _, _)| c == id).map(|(_, l, _)| l.clone()).unwrap_or_else(|| id.to_string())
        };
        let ticket_of = |id: &str| labels.iter().find(|(c, _, _)| c == id).map(|(_, _, t)| t.clone());
        Ok(self
            .store
            .msgs_recent(FEED_KINDS, &to, limit.clamp(1, FEED_MAX))?
            .into_iter()
            .map(|m| {
                let ticket = ticket_of(&m.from).or_else(|| ticket_of(&m.to));
                let parent = ticket.as_deref().map(|t| {
                    let live = self.live_of(t);
                    let status = if done.iter().any(|d| d == t) { "done" } else { chip(live.as_ref()) };
                    (label_of(t), live.unwrap_or(Value::Null)["title"].clone(), status)
                });
                let mut v = json!({"handle": m.handle, "at": local_clock(&m.at), "iso": m.at, "label": label_of(&m.from),
                       "kind": m.kind, "head": truncate_chars(&m.head(usize::MAX), 200), "ticket": ticket,
                       "ticketLabel": parent.as_ref().map(|p| &p.0), "ticketTitle": parent.as_ref().map(|p| &p.1),
                       "ticketStatus": parent.as_ref().map(|p| p.2)});
                if m.from == BOARD
                    && let Ok(Some(p)) = self.store.pick_by_handle(&m.handle)
                {
                    v["instanceId"] = json!(p.instance_id);
                    v["reason"] = json!(p.reason);
                }
                self.placed(v, ticket.as_deref())
            })
            .collect())
    }

    pub fn feed_in(&self, limit: usize, filter: Option<&ProjectFilter>) -> Result<Vec<Value>> {
        let Some(f) = filter else { return self.feed(limit) };
        let mut rows = kept(self.feed(FEED_MAX)?, Some(f));
        rows.truncate(limit.clamp(1, FEED_MAX));
        Ok(rows)
    }

    fn ensure_not_on_board(&self, label: &str) -> Result<()> {
        if self.store.children(BOARD, true)?.iter().any(|l| l.label == label) {
            bail!("{label} is already on the board; close or finish it before launching it again");
        }
        if self.store.pending_all()?.iter().any(|p| p.parent == BOARD && p.label == label) {
            bail!("{label} was launched and has not joined yet; the board reports it if it never does");
        }
        Ok(())
    }

    async fn projects(&self) -> Result<Vec<Value>> {
        let all = self.project_list().await?;
        Ok(all.into_iter().filter(|p| p["deletedAt"].is_null() && p["archivedAt"].is_null()).collect())
    }

    pub(crate) async fn project_list(&self) -> Result<Vec<Value>> {
        let mut out = Vec::new();
        let mut cursor: Option<u64> = None;
        for _ in 0..20 {
            let mut input = json!({"limit": 100});
            if let Some(c) = cursor {
                input["cursor"] = json!(c);
            }
            let r = self.t3.call("t3_project_list", input).await?;
            out.extend(r["projects"].as_array().cloned().unwrap_or_default());
            match r["nextCursor"].as_u64() {
                Some(c) if Some(c) != cursor => cursor = Some(c),
                _ => break,
            }
        }
        Ok(out)
    }

    async fn find_project(&self, issue: &Issue, wanted: Option<&str>) -> Result<Option<Value>> {
        let projects = self.projects().await?;
        if let Some(w) = wanted {
            return projects
                .into_iter()
                .find(|p| p["id"] == w || p["title"].as_str().is_some_and(|t| t.eq_ignore_ascii_case(w)))
                .map(Some)
                .ok_or_else(|| anyhow!("no T3 project with id or title {w:?}"));
        }
        for p in projects {
            let Some(root) = p["workspaceRoot"].as_str() else { continue };
            let Some(url) = self.remotes.origin(root).await else { continue };
            if let Some((owner, repo)) = parse_remote(&url)
                && owner.eq_ignore_ascii_case(&issue.owner)
                && repo.eq_ignore_ascii_case(&issue.repo)
            {
                return Ok(Some(p));
            }
        }
        Ok(None)
    }

    async fn pool_pick(&self, model: Option<&str>, effort: Option<&str>) -> Result<Option<Picked>> {
        let Some(name) = &self.config.board.pool else { return Ok(None) };
        let pool = self.pool(name)?;
        if pool.mixed
            && let Some(what) = model.map(|m| format!("model {m}")).or(effort.map(|e| format!("effort {e}")))
        {
            bail!(
                "board.pool {name} is a mixed pool, where each member runs its own model and effort; launch without a model or effort, or unset board.pool to launch {what}"
            );
        }
        let pooled = pool.model.as_deref().unwrap_or_default();
        if let Some(m) = model.filter(|m| *m != pooled) {
            bail!("board.pool {name} runs {pooled}; launch without a model, or unset board.pool to launch {m}");
        }
        if let Some(e) = effort.filter(|e| Some(*e) != pool.effort.as_deref()) {
            bail!(
                "board.pool {name} runs effort {}; launch without an effort, or unset board.pool to launch effort {e}",
                pool.effort.as_deref().unwrap_or("default")
            );
        }
        Ok(Some(self.pick(name).await?))
    }

    fn note_pool_launch(&self, out: &mut Value, picked: &Picked, what: &str, thread: &str) {
        out["instanceId"] = json!(picked.instance_id);
        out["reason"] = json!(picked.reason);
        let body = format!("{what} launched {}", picked.summary());
        let recorded = self
            .store
            .msg_new(BOARD, BOARD, "note", &body, &self.now())
            .and_then(|handle| self.store.pick_launched(picked.seq, Some(&handle), thread));
        if let Err(e) = recorded {
            tracing::warn!("{what} launched on {}, but recording the pick failed: {e:#}", picked.instance_id);
            out["warning"] = json!(format!("launched, but recording the pick failed: {e:#}"));
        }
    }

    fn model_selection(&self, model: &str, effort: Option<&str>) -> Value {
        let mut m = json!({"instanceId": self.config.board.instance_id, "model": model});
        if let Some(e) = effort {
            m["options"] = json!([{"id": "effort", "value": e}]);
        }
        m
    }

    fn launch_failed(&self, label: &str, text: &str) -> Result<Value> {
        let card = self.card_add(NewCard {
            kind: "error",
            label: Some(label),
            ticket: None,
            link: None,
            body: text,
            handle: None,
            options: None,
            pr: None,
            approval: None,
            offer: None,
        })?;
        Ok(json!({"ok": false, "label": label, "error": text, "card": card}))
    }

    pub async fn launch_ticket(&self, a: LaunchArgs) -> Result<Value> {
        let issue = parse_issue(&a.issue).ok_or_else(|| {
            anyhow!(
                "{:?} is not a GitHub issue; use https://github.com/<owner>/<repo>/issues/<n> or <owner>/<repo>#<n>",
                a.issue
            )
        })?;
        let label = issue.label();
        self.ensure_not_on_board(&label)?;
        let wanted = nonempty(a.project);
        let model_arg = nonempty(a.model);
        let effort_arg = nonempty(a.effort);
        let found = match self.find_project(&issue, wanted.as_deref()).await {
            Ok(found) => found,
            Err(e) => return self.launch_failed(&label, &format!("Cannot launch {label}: {e:#}")),
        };
        let project = match found {
            Some(p) => p,
            None => {
                let text = format!(
                    "Cannot launch {label}: no T3 project has github.com/{}/{} as its origin remote. \
                     Add the repository as a T3 project, then launch it again.",
                    issue.owner, issue.repo
                );
                return self.launch_failed(&label, &text);
            }
        };
        let project_id = project["id"].as_str().unwrap_or_default().to_string();
        let repo = match identity_repo(&project) {
            Some(r) => Some(r),
            None => self.origin_repo(&project).await,
        };
        if let Some(repo) = repo {
            self.remember_project(&project_id, &project["title"], Some(repo));
        }
        let picked = match self.pool_pick(model_arg.as_deref(), effort_arg.as_deref()).await {
            Ok(p) => p,
            Err(e) => return self.launch_failed(&label, &format!("Cannot launch {label}: {e:#}")),
        };
        let params = self.config.link_params("overseer", "ticket")?;
        {
            let _g = self.lock.lock().await;
            self.ensure_not_on_board(&label)?;
            self.store.pending_put(&Pending {
                parent: BOARD.into(),
                label: label.clone(),
                role: "ticket".into(),
                params,
                at: self.now(),
                reported: false,
                thread: None,
                link: None,
            })?;
        }
        self.changed();
        let b = &self.config.board;
        let title = format!("{label}: {}", nonempty(a.title).unwrap_or_else(|| issue.url()));
        let selection = match &picked {
            Some(p) => p.model_selection.clone(),
            None => {
                let effort = effort_arg.or_else(|| b.ticket_effort.clone());
                self.model_selection(&model_arg.unwrap_or_else(|| b.ticket_model.clone()), effort.as_deref())
            }
        };
        let mut input = json!({
            "projectId": project_id,
            "title": title,
            "modelSelection": selection,
            "message": ticket_brief(&issue.url(), &label),
            "clientRequestId": format!("t3cm-board-{}", random_id()),
        });
        if let Some(m) = &b.runtime_mode {
            input["runtimeMode"] = json!(m);
        }
        let launched = self.t3.call("t3_thread_launch", input).await;
        let r = match launched {
            Ok(r) if r["threadId"].is_string() => r,
            other => {
                let why = match other {
                    Err(e) => e.to_string(),
                    Ok(r) => format!("t3_thread_launch returned no threadId: {}", truncate_chars(&r.to_string(), 200)),
                };
                {
                    let _g = self.lock.lock().await;
                    self.store.pending_remove(BOARD, &label)?;
                }
                return self.launch_failed(&label, &format!("Launching {label} failed: {why}"));
            }
        };
        let thread = r["threadId"].as_str().unwrap_or_default().to_string();
        let link = r["link"].as_str().map(String::from);
        {
            let _g = self.lock.lock().await;
            let early = self.store.link_get(&thread)?.filter(|l| l.parent == BOARD);
            if let Some(mut ln) = early {
                self.store.pending_remove(BOARD, &label)?;
                ln.label = label.clone();
                self.store.link_save(&ln)?;
            } else if let Some(mut p) =
                self.store.pending_all()?.into_iter().find(|p| p.parent == BOARD && p.label == label)
            {
                p.thread = Some(thread.clone());
                p.link = link.clone();
                self.store.pending_put(&p)?;
            }
        }
        self.note_live(&thread, &json!({"status": r["status"], "link": link, "title": title, "projectId": project_id}));
        if let Err(e) = self.store.new_issue_remove(&issue_key(&issue)) {
            tracing::warn!("{label} launched, but taking it off the new issues failed: {e:#}");
        }
        let mut out = json!({"ok": true, "label": label, "thread": thread, "link": link, "projectId": project_id, "title": title});
        if let Some(p) = &picked {
            self.note_pool_launch(&mut out, p, &label, &thread);
        }
        self.changed();
        Ok(out)
    }

    pub async fn ask(&self, question: &str) -> Result<Value> {
        let q = question.trim();
        if q.is_empty() {
            bail!("empty question");
        }
        if q.chars().count() > MAX_TEXT {
            bail!("the question is over {MAX_TEXT} chars");
        }
        let b = &self.config.board;
        let picked = match self.pool_pick(None, None).await {
            Ok(p) => p,
            Err(e) => {
                let text = format!("Cannot ask the board question: {e:#}");
                let card = self.launch_failed("ask", &text)?;
                bail!("{text} (recorded as board card {})", card["card"]);
            }
        };
        let selection = match &picked {
            Some(p) => p.model_selection.clone(),
            None => self.model_selection(&b.ask_model, b.ask_effort.as_deref()),
        };
        let mut input = json!({
            "scratch": true,
            "title": format!("Board question: {}", truncate_chars(&q.split_whitespace().collect::<Vec<_>>().join(" "), 50)),
            "modelSelection": selection,
            "message": ask_brief(q),
            "clientRequestId": format!("t3cm-ask-{}", random_id()),
        });
        if let Some(m) = &b.runtime_mode {
            input["runtimeMode"] = json!(m);
        }
        let r = self.t3.call("t3_thread_launch", input).await?;
        let thread = r["threadId"].as_str().ok_or_else(|| anyhow!("t3_thread_launch returned no threadId"))?;
        let data = json!({"question": q, "thread": thread, "link": r["link"], "status": "running",
                          "answer": null, "at": self.now(), "answeredAt": null, "archived": false});
        let seq = self.store.ask_insert(thread, &data)?;
        let mut out = ask_view(seq, &data);
        if let Some(p) = &picked {
            self.note_pool_launch(&mut out, p, "Board question", thread);
        }
        self.changed();
        Ok(out)
    }

    async fn last_answer(&self, thread: &str) -> Result<(Value, Option<String>)> {
        let mut input = json!({"threadId": thread, "view": "messages", "limit": 20, "maxCharsPerItem": 6000});
        let mut th = Value::Null;
        for _ in 0..2 {
            let r = self.t3.call("t3_thread_read", input.clone()).await?;
            if th.is_null() {
                th = r["thread"].clone();
            }
            let found = r["items"].as_array().and_then(|items| {
                items
                    .iter()
                    .rev()
                    .filter(|i| i["type"] == "assistant_message")
                    .find_map(|i| i["text"].as_str().map(str::trim).filter(|t| !t.is_empty()).map(String::from))
            });
            if found.is_some() {
                return Ok((th, found));
            }
            match r["earlierPosition"].as_u64() {
                Some(p) => input["beforePosition"] = json!(p),
                None => break,
            }
        }
        Ok((th, None))
    }

    pub async fn check_asks(&self, only: Option<i64>) -> Result<bool> {
        let _g = self.asks_lock.lock().await;
        let mut still_open = false;
        for (seq, mut data) in self.store.asks_open()? {
            if only.is_some_and(|o| o != seq) {
                continue;
            }
            let thread = data["thread"].as_str().unwrap_or_default().to_string();
            let age = self.mins(data["at"].as_str()).unwrap_or(0.0);
            let (th, answer) = match self.last_answer(&thread).await {
                Ok(x) => x,
                Err(e) if e.downcast_ref::<crate::t3::T3Error>().is_some_and(|t| t.is_not_found()) => {
                    data["status"] = json!("gone");
                    self.store.ask_update(seq, false, &data)?;
                    self.changed();
                    continue;
                }
                Err(_) => {
                    still_open = true;
                    continue;
                }
            };
            let status = th["status"].as_str().unwrap_or("");
            let busy = ACTIVE.contains(&status) || status == "waiting";
            let timeout = age >= self.config.board.ask_timeout_minutes as f64;
            let close = match (&answer, busy) {
                (Some(text), false) => {
                    data["status"] = json!("answered");
                    data["answer"] = json!(truncate_chars(text, ANSWER_CAP));
                    data["answeredAt"] = json!(self.now());
                    let archived =
                        self.t3.call("t3_thread_organize", json!({"threadId": thread, "action": "archive"})).await;
                    data["archived"] = json!(archived.is_ok());
                    true
                }
                (None, false) if ENDED_BADLY.contains(&status) => {
                    data["status"] = json!("failed");
                    true
                }
                (None, false) if age >= 2.0 => {
                    data["status"] = json!("no answer");
                    true
                }
                _ if timeout => {
                    data["status"] = json!("timed out");
                    true
                }
                _ => {
                    let s = if status == "waiting" { "waiting" } else { "running" };
                    if data["status"] != s {
                        data["status"] = json!(s);
                        self.store.ask_update(seq, true, &data)?;
                        self.changed();
                    }
                    false
                }
            };
            if close {
                self.store.ask_update(seq, false, &data)?;
                self.changed();
            } else {
                still_open = true;
            }
        }
        Ok(still_open)
    }

    pub async fn board_tick(&self) -> Result<()> {
        let launched: Vec<String> =
            self.store.pending_all()?.into_iter().filter(|p| p.parent == BOARD).filter_map(|p| p.thread).collect();
        for t in launched {
            let _ = self.read_thread(&t).await;
        }
        for ticket in self.store.children(BOARD, false)?.into_iter().filter(|t| t.settled_at.is_none()) {
            for w in self.store.children(&ticket.child, false)? {
                let can_change =
                    |l: &Value| l["prStale"] == true || l["pr"]["state"] == "open" || l["pr"]["state"] == "closed";
                if w.closed && self.live_of(&w.child).is_none_or(|l| can_change(&l)) {
                    let _ = self.read_thread(&w.child).await;
                }
            }
        }
        self.check_asks(None).await?;
        match self.reconcile_if_due().await {
            Ok(a) if !a.is_empty() => tracing::info!(actions = ?a, "github reconcile"),
            Ok(_) => {}
            Err(e) => tracing::warn!("github reconcile failed: {e:#}"),
        }
        self.mirror_t3_settles().await?;
        if let Err(e) = self.learn_projects().await {
            tracing::debug!("board projects not refreshed: {e:#}");
        }
        let hours = self.config.board.settle_after_hours;
        if hours > 0 {
            self.settle_closed(Some(hours as f64 * 60.0)).await?;
        }
        Ok(())
    }

    async fn mirror_t3_settles(&self) -> Result<()> {
        let unsettled: Vec<Link> =
            self.store.children(BOARD, false)?.into_iter().filter(|l| l.closed && l.settled_at.is_none()).collect();
        let mut changed = false;
        for ln in unsettled {
            let newer = |at: Option<&str>| match ln.restored_at.as_deref() {
                None => true,
                Some(r) => matches!((at.and_then(parse_ts), parse_ts(r)), (Some(a), Some(b)) if a > b),
            };
            let mirror = match self.read_thread(&ln.child).await {
                Ok(th) if th["settled"] == true => newer(th["settledAt"].as_str()),
                Ok(th) if th["archived"] == true => ln.restored_at.is_none(),
                Ok(_) => false,
                Err(e) => e.is_not_found(),
            };
            if !mirror {
                continue;
            }
            let _g = self.lock.lock().await;
            let Some(mut fresh) = self.store.link_get(&ln.child)? else { continue };
            if fresh.closed && fresh.settled_at.is_none() && fresh.restored_at == ln.restored_at {
                fresh.settled_at = Some(self.now());
                self.store.link_save(&fresh)?;
                changed = true;
            }
        }
        if changed {
            self.changed();
        }
        Ok(())
    }

    fn board_ticket(&self, thread: &str) -> Result<Link> {
        self.store.link_get(thread)?.filter(|l| l.parent == BOARD).ok_or_else(|| anyhow!("no board ticket {thread:?}"))
    }

    async fn organize(&self, thread: &str, action: &str) -> Value {
        match self.t3.call("t3_thread_organize", json!({"threadId": thread, "action": action})).await {
            Ok(_) => json!({"ok": true}),
            Err(e) => {
                let why = self.t3.redact(e.to_string());
                tracing::warn!("t3_thread_organize {action} {thread} failed: {why}");
                json!({"ok": false, "error": why})
            }
        }
    }

    pub async fn settle_ticket(&self, thread: &str) -> Result<Value> {
        {
            let _g = self.lock.lock().await;
            let mut ln = self.board_ticket(thread)?;
            if !ln.closed {
                bail!("{} is not done; only done tickets can be settled", ln.label);
            }
            if ln.settled_at.is_some() {
                bail!("{} is already settled", ln.label);
            }
            ln.settled_at = Some(self.now());
            self.store.link_save(&ln)?;
        }
        let t3 = self.organize(thread, "settle").await;
        self.changed();
        Ok(json!({"thread": thread, "settled": true, "t3": t3}))
    }

    pub async fn restore_ticket(&self, thread: &str) -> Result<Value> {
        {
            let _g = self.lock.lock().await;
            let mut ln = self.board_ticket(thread)?;
            if ln.settled_at.is_none() {
                bail!("{} is not settled", ln.label);
            }
            ln.settled_at = None;
            ln.restored_at = Some(self.now());
            self.store.link_save(&ln)?;
        }
        let t3 = self.organize(thread, "unsettle").await;
        self.changed();
        Ok(json!({"thread": thread, "settled": false, "t3": t3}))
    }

    fn ask_seq(id: &str) -> Result<i64> {
        id.strip_prefix('q').and_then(|n| n.parse().ok()).ok_or_else(|| anyhow!("no ask {id:?}; ask ids look like q12"))
    }

    fn ask_row(&self, id: &str) -> Result<(i64, bool, Value)> {
        let seq = Self::ask_seq(id)?;
        let (open, data) = self.store.ask_get(seq)?.ok_or_else(|| anyhow!("no ask {id:?}"))?;
        Ok((seq, open, data))
    }

    pub fn settle_ask(&self, id: &str) -> Result<Value> {
        let (seq, open, mut data) = self.ask_row(id)?;
        if open {
            bail!("ask {id} is still open; only closed asks can be settled");
        }
        if !data["settledAt"].is_null() {
            bail!("ask {id} is already settled");
        }
        data["settledAt"] = json!(self.now());
        self.store.ask_update(seq, false, &data)?;
        self.changed();
        Ok(json!({"ask": id, "settled": true}))
    }

    pub fn restore_ask(&self, id: &str) -> Result<Value> {
        let (seq, _, mut data) = self.ask_row(id)?;
        if data["settledAt"].is_null() {
            bail!("ask {id} is not settled");
        }
        data["settledAt"] = Value::Null;
        data["restoredAt"] = json!(self.now());
        self.store.ask_update(seq, false, &data)?;
        self.changed();
        Ok(json!({"ask": id, "settled": false}))
    }

    pub async fn settle_closed(&self, older_than_mins: Option<f64>) -> Result<Value> {
        let due = |since: Option<&str>| match older_than_mins {
            None => true,
            Some(min) => self.mins(since).is_some_and(|age| age >= min),
        };
        let mut tickets = 0;
        for ln in self.store.children(BOARD, false)? {
            if ln.closed
                && ln.settled_at.is_none()
                && due(ln.closed_since())
                && self.settle_ticket(&ln.child).await.is_ok()
            {
                tickets += 1;
            }
        }
        let mut asks = 0;
        for (seq, d) in self.store.asks_closed_unsettled()? {
            let since = d["restoredAt"].as_str().or(d["answeredAt"].as_str()).or(d["at"].as_str());
            if due(since) && self.settle_ask(&format!("q{seq}")).is_ok() {
                asks += 1;
            }
        }
        Ok(json!({"tickets": tickets, "asks": asks}))
    }

    fn node_view(&self, ln: &Link) -> Value {
        let live = self.live_of(&ln.child);
        let status = if ln.closed { "done" } else { chip(live.as_ref()) };
        let live = live.unwrap_or(Value::Null);
        let (repo, project) = self.place(ln.project.as_deref(), &live);
        json!({
            "title": live["title"], "repo": repo, "project": project,
            "thread": ln.child, "label": ln.label, "status": status, "t3Status": live["status"],
            "gate": if ln.closed { None } else { ln.gate.clone() },
            "link": web_link(ln.link.clone().or_else(|| live["link"].as_str().map(String::from))),
            "pr": self.pr_view(&live["pr"]), "done": ln.closed, "closedHow": ln.closed_how, "lastUp": local_clock(&ln.last_up),
            "upAt": ln.last_up,
        })
    }

    fn settled_tickets(&self) -> Result<Vec<Value>> {
        let mut settled: Vec<Link> =
            self.store.children(BOARD, false)?.into_iter().filter(|l| l.settled_at.is_some()).collect();
        settled.sort_by(|a, b| b.settled_at.cmp(&a.settled_at));
        Ok(settled
            .iter()
            .map(|l| {
                let mut v = self.node_view(l);
                v["settledAt"] = json!(l.settled_at.as_deref().map(local_clock));
                v["settledIso"] = json!(l.settled_at);
                v
            })
            .collect())
    }

    pub fn tree(&self) -> Result<Vec<Value>> {
        let all = self.store.children(BOARD, false)?;
        let (open, mut closed): (Vec<Link>, Vec<Link>) =
            all.into_iter().filter(|l| l.settled_at.is_none()).partition(|l| !l.closed);
        closed.sort_by(|a, b| b.closed_since().cmp(&a.closed_since()));
        closed.truncate(DONE_SHOWN);
        let mut out = Vec::new();
        let mut dismissed: Option<Vec<String>> = None;
        for t in open.iter().chain(closed.iter()) {
            let mut v = self.node_view(t);
            let workers: Vec<Value> = self.store.children(&t.child, false)?.iter().map(|w| self.node_view(w)).collect();
            v["workers"] = json!(workers);
            v["ticket"] = json!(t.child);
            v["joined"] = json!(true);
            v["last"] = match self.store.msg_last_from(&t.child)? {
                Some(m) => {
                    let mut last = json!({"handle": m.handle, "kind": m.kind, "head": truncate_chars(&m.head(usize::MAX), 140), "at": local_clock(&m.at)});
                    if m.kind == "gate" {
                        if dismissed.is_none() {
                            dismissed = Some(
                                self.cards_with(true)?
                                    .into_iter()
                                    .filter(|c| c.resolution.as_deref() == Some("dismissed"))
                                    .filter_map(|c| c.handle)
                                    .collect(),
                            );
                        }
                        if dismissed.as_ref().is_some_and(|d| d.contains(&m.handle)) {
                            last["dismissed"] = json!(true);
                        }
                    }
                    last
                }
                None => Value::Null,
            };
            out.push(v);
        }
        let mut pending = Vec::new();
        for p in self.store.pending_all()?.into_iter().filter(|p| p.parent == BOARD) {
            let live = p.thread.as_deref().and_then(|t| self.live_of(t));
            let status = if p.thread.is_none() { "launching" } else { chip(live.as_ref()) };
            let live = live.unwrap_or(Value::Null);
            let (repo, project) = self.place(None, &live);
            pending.push(json!({
                "title": live["title"], "repo": repo, "project": project,
                "ticket": p.thread, "thread": p.thread, "label": p.label, "status": status,
                "t3Status": live["status"], "gate": null,
                "link": web_link(p.link.clone().or_else(|| live["link"].as_str().map(String::from))),
                "pr": live["pr"], "done": false, "closedHow": null, "joined": false, "workers": [],
                "lastUp": local_clock(&p.at), "upAt": p.at, "last": null,
            }));
        }
        out.splice(open.len()..open.len(), pending);
        Ok(out)
    }

    pub fn asks(&self) -> Result<Vec<Value>> {
        Ok(self.store.asks_unsettled(ASKS_SHOWN)?.iter().map(|(seq, d)| ask_view(*seq, d)).collect())
    }

    fn settled(&self) -> Result<(Value, usize)> {
        let mut tickets = self.settled_tickets()?;
        let total = tickets.len() + self.store.asks_settled_count()?;
        tickets.truncate(SETTLED_SHOWN);
        let asks: Vec<Value> =
            self.store.asks_settled(SETTLED_SHOWN)?.iter().map(|(seq, d)| ask_view(*seq, d)).collect();
        Ok((json!({"tickets": tickets, "asks": asks}), total))
    }

    pub fn snapshot(&self) -> Result<Value> {
        let cards: Vec<Value> = self.cards()?.iter().map(|c| self.placed(json!(c), c.ticket.as_deref())).collect();
        let tree = self.tree()?;
        let feed = self.feed(self.config.board.feed)?;
        let asks = self.asks()?;
        let (settled, settled_total) = self.settled()?;
        let contexts = self.contexts()?;
        let (new_issues, dismissed_issues) = self.new_issues()?;
        let open = tree.iter().filter(|t| t["done"] == false).count();
        let running = tree.iter().filter(|t| t["status"] == "running").count();
        Ok(json!({
            "cards": cards,
            "tree": tree,
            "feed": feed,
            "asks": asks,
            "settled": settled,
            "contexts": contexts,
            "newIssues": new_issues,
            "dismissedIssues": dismissed_issues,
            "counts": {"cards": cards.len(), "tickets": open, "running": running, "settled": settled_total,
                       "newIssues": new_issues.len()},
            "service": self.service_view(),
        }))
    }

    pub fn service_view(&self) -> Value {
        let t3 = self.t3.status();
        let last = self.store.meta_get(crate::wf::LAST_PASS).ok().flatten();
        json!({
            "version": env!("CARGO_PKG_VERSION"),
            "t3": {"connected": t3.connected, "lastError": t3.last_error},
            "tokenExpiresAt": t3.token_expires_at.as_deref().map(local_clock_date),
            "watcher": {"lastPass": last.as_deref().map(local_clock), "lastPassAt": last, "alive": self.watcher_alive()},
            "openLinks": self.config.board.open_links,
            "update": self.update_view(),
            "skills": self.skills_view(),
        })
    }

    pub async fn board_view(&self, root: &str, all: bool) -> Result<Value> {
        let mut v = self.board_of(root, all).await?;
        if let Some(kids) = v["children"].as_array_mut() {
            for k in kids.iter_mut() {
                let thread = k["thread"].as_str().unwrap_or_default().to_string();
                k["workers"] = self.board_of(&thread, all).await?["children"].take();
            }
        }
        let pending: Vec<Value> = self
            .store
            .pending_all()?
            .into_iter()
            .filter(|p| p.parent == root)
            .map(|p| json!({"label": p.label, "thread": p.thread, "link": p.link, "since": local_clock(&p.at)}))
            .collect();
        v["pending"] = json!(pending);
        if root == BOARD {
            v["cards"] = json!(self.cards()?.len());
        }
        Ok(v)
    }
}

fn local_clock_date(s: &str) -> String {
    crate::clock::parse_ts(s).map(crate::clock::local_datetime).unwrap_or_else(|| s.to_string())
}

pub fn ask_view(seq: i64, d: &Value) -> Value {
    let mut v = d.clone();
    v["id"] = json!(format!("q{seq}"));
    v["seq"] = json!(seq);
    v["atLocal"] = json!(d["at"].as_str().map(local_clock));
    v
}
