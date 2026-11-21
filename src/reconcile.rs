use std::collections::BTreeMap;

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::board::{BOARD, Card, NewCard, gh_name};
use crate::clock::{local_clock, parse_ts};
use crate::github::errors;
use crate::merge::{CHECKS, ENQUEUE_PR, MERGE_PR, Ready, ready};
use crate::store::{MergeWait, PrSeen, WaitStatus, Wake};
use crate::wf::{Wf, header};

pub const GITHUB: &str = "github";
pub const LAST_CHECK: &str = "reconcile_last";
pub const SINCE: &str = "reconcile_since";
const BATCH: usize = 100;
const PAGE_LOAD_GAP_MINS: f64 = 0.5;
const MAX_FAILURES: u32 = 3;
const EXIT_WINDOW_MINS: f64 = 30.0;
const PR_FIELDS: &str = "fragment F on PullRequest { id state isDraft headRefOid mergeStateStatus \
isMergeQueueEnabled isInMergeQueue mergeQueueEntry { position } mergeCommit { oid } mergedAt closedAt \
timelineItems(last: 1, itemTypes: [REMOVED_FROM_MERGE_QUEUE_EVENT]) { nodes { ... on RemovedFromMergeQueueEvent { \
reason createdAt } } } ...Checks }";
const HELD: &str = "has a failing check at its approved head";
const NO_TICKET: &str =
    "no ticket tracks it to take a reply, so act on it on GitHub or with wf.merge and then dismiss this card";

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CardPr {
    pub repo: String,
    pub number: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
}

pub fn pr_refs(text: &str) -> Vec<(String, String, u64)> {
    let mut out: Vec<(String, String, u64)> = Vec::new();
    for (i, m) in text.match_indices("github.com/") {
        if !at_host_start(&text[..i]) {
            continue;
        }
        let rest = &text[i + m.len()..];
        let end = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_' | '.')))
            .unwrap_or(rest.len());
        let parts: Vec<&str> = rest[..end].split('/').collect();
        if let [owner, repo, "pull", n, ..] = parts.as_slice()
            && gh_name(owner)
            && gh_name(repo)
            && let Some(n) = n.trim_end_matches('.').parse::<u64>().ok().filter(|n| (1..=i32::MAX as u64).contains(n))
            && !out.iter().any(|(o, r, k)| o.eq_ignore_ascii_case(owner) && r.eq_ignore_ascii_case(repo) && *k == n)
        {
            out.push((owner.to_string(), repo.to_string(), n));
        }
    }
    out
}

fn at_host_start(before: &str) -> bool {
    let before = before.strip_suffix("www.").unwrap_or(before);
    before.ends_with("://")
        || before
            .chars()
            .last()
            .is_none_or(|c| !(c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '-' | '_' | '@')))
}

fn approved_head(body: &str) -> Option<String> {
    let words: Vec<&str> = body.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()).collect();
    words.windows(2).find_map(|w| {
        let sha = w[1];
        let hex = sha.chars().all(|c| c.is_ascii_hexdigit()) && sha.chars().any(|c| c.is_ascii_digit());
        (w[0].eq_ignore_ascii_case("head") && (7..=40).contains(&sha.len()) && hex).then(|| sha.to_ascii_lowercase())
    })
}

pub fn card_pr(body: &str) -> Option<CardPr> {
    let action: Vec<&str> = body.lines().filter(|l| l.contains("ACTION NEEDED")).collect();
    let mut refs = pr_refs(&action.join("\n"));
    if refs.len() != 1 {
        refs = pr_refs(body);
    }
    let [(owner, name, number)] = refs.as_slice() else { return None };
    Some(CardPr { repo: format!("{owner}/{name}").to_ascii_lowercase(), number: *number, head: approved_head(body) })
}

fn short(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

fn why_dropped(reason: &str) -> String {
    match reason {
        "merge_conflict" => "a merge conflict".into(),
        "failed_checks" => "failed checks".into(),
        "manual" => "removed by hand".into(),
        "" => "no reason given".into(),
        other => other.replace('_', " "),
    }
}

fn later(event: &str, since: &str) -> bool {
    match (parse_ts(event), parse_ts(since)) {
        (Some(e), Some(s)) => e >= s,
        _ => false,
    }
}

struct Obs {
    state: String,
    head: String,
    queued: bool,
    draft: bool,
    merge_state: String,
    ready: Ready,
    queue: bool,
    id: String,
    commit: String,
    ended: Option<String>,
    removal: Option<(String, String)>,
    method: Option<String>,
}

impl Obs {
    fn read(repo: &Value) -> Obs {
        let pr = &repo["pullRequest"];
        let s = |k: &str| pr[k].as_str().unwrap_or_default().to_string();
        let removal = &pr["timelineItems"]["nodes"][0];
        let merged = pr["state"] == "MERGED";
        Obs {
            state: s("state"),
            head: s("headRefOid").to_ascii_lowercase(),
            queued: pr["isInMergeQueue"] == true,
            draft: pr["isDraft"] == true,
            merge_state: pr["mergeStateStatus"].as_str().unwrap_or("UNKNOWN").into(),
            ready: ready(pr),
            queue: pr["isMergeQueueEnabled"] == true,
            id: s("id"),
            commit: pr["mergeCommit"]["oid"].as_str().unwrap_or_default().into(),
            ended: pr[if merged { "mergedAt" } else { "closedAt" }].as_str().map(String::from),
            removal: removal["createdAt"]
                .as_str()
                .map(|at| (removal["reason"].as_str().unwrap_or_default().to_string(), at.to_string())),
            method: repo["viewerDefaultMergeMethod"].as_str().map(String::from),
        }
    }
}

#[derive(Default)]
struct Tracked {
    repo: String,
    number: u64,
    since: String,
    tickets: Vec<(String, String)>,
    cards: Vec<i64>,
    approval: Option<String>,
}

impl Tracked {
    fn label(&self) -> String {
        format!("{}#{}", self.repo, self.number)
    }

    fn url(&self) -> String {
        format!("https://github.com/{}/pull/{}", self.repo, self.number)
    }

    fn add(&mut self, ticket: Option<&str>, since: &str) {
        if since.is_empty() {
            return;
        }
        if self.since.is_empty() || since < self.since.as_str() {
            self.since = since.into();
        }
        let Some(t) = ticket.filter(|t| *t != BOARD && !t.is_empty()) else { return };
        match self.tickets.iter_mut().find(|(x, _)| x == t) {
            Some((_, s)) if since < s.as_str() => *s = since.into(),
            Some(_) => {}
            None => self.tickets.push((t.into(), since.into())),
        }
    }

    fn pr(&self, head: &str) -> CardPr {
        CardPr { repo: self.repo.clone(), number: self.number, head: Some(head.into()) }
    }
}

struct Ask {
    what: String,
    reply: &'static str,
    context: String,
}

#[derive(Default)]
struct Pass {
    actions: Vec<String>,
    woken: Vec<String>,
}

fn add<'a>(map: &'a mut BTreeMap<(String, u64), Tracked>, repo: &str, n: u64) -> &'a mut Tracked {
    let repo = repo.to_ascii_lowercase();
    map.entry((repo.clone(), n)).or_insert_with(|| Tracked { repo, number: n, ..Tracked::default() })
}

fn batch_query(chunk: &[&Tracked]) -> (String, Value) {
    let mut params = Vec::new();
    let mut fields = Vec::new();
    let mut vars = serde_json::Map::new();
    for (i, t) in chunk.iter().enumerate() {
        let (owner, name) = t.repo.split_once('/').unwrap_or_default();
        params.push(format!("$o{i}: String!, $r{i}: String!, $n{i}: Int!"));
        fields.push(format!(
            "p{i}: repository(owner: $o{i}, name: $r{i}) {{ viewerDefaultMergeMethod pullRequest(number: $n{i}) {{ ...F }} }}"
        ));
        vars.insert(format!("o{i}"), json!(owner));
        vars.insert(format!("r{i}"), json!(name));
        vars.insert(format!("n{i}"), json!(t.number));
    }
    let query = format!("query Reconcile({}) {{ {} }} {PR_FIELDS} {CHECKS}", params.join(", "), fields.join(" "));
    (query, Value::Object(vars))
}

impl Wf {
    pub fn pr_view(&self, pr: &Value) -> Value {
        let mut pr = pr.clone();
        if pr["state"] == "open"
            && let Some((o, r, n)) = pr["url"].as_str().and_then(|u| pr_refs(u).into_iter().next())
            && let Ok(Some(seen)) = self.store.pr_seen_get(&format!("{o}/{r}").to_ascii_lowercase(), n)
            && seen.queued
            && seen.state == "OPEN"
        {
            pr["state"] = json!("queued");
        }
        pr
    }

    pub async fn reconcile_if_due(&self) -> Result<Vec<String>> {
        let b = &self.config.board;
        if b.reconcile_idle_minutes == 0 {
            return Ok(Vec::new());
        }
        let active = !self.store.merge_waits()?.is_empty() || self.store.pr_seen_any_queued()?;
        let every = if active { b.reconcile_active_minutes } else { b.reconcile_idle_minutes } as f64;
        self.reconcile_after(every).await
    }

    pub async fn reconcile_soon(&self) -> Result<Vec<String>> {
        if self.config.board.reconcile_idle_minutes == 0 {
            return Ok(Vec::new());
        }
        self.reconcile_after(PAGE_LOAD_GAP_MINS).await
    }

    pub async fn reconcile(&self) -> Result<Vec<String>> {
        self.reconcile_after(0.0).await
    }

    async fn reconcile_after(&self, gap_mins: f64) -> Result<Vec<String>> {
        let Ok(_running) = self.reconcile_lock.try_lock() else { return Ok(Vec::new()) };
        if gap_mins > 0.0 && self.mins(self.store.meta_get(LAST_CHECK)?.as_deref()).is_some_and(|m| m < gap_mins) {
            return Ok(Vec::new());
        }
        self.store.meta_set(LAST_CHECK, &self.now())?;
        self.learn_card_prs()?;
        let floor = self.store.meta_get(SINCE)?.unwrap_or_default();
        let tracked = self.tracked(&floor)?;
        let mut pass = Pass::default();
        let mut failed = Vec::new();
        let list: Vec<&Tracked> = tracked.values().collect();
        for chunk in list.chunks(BATCH) {
            let (query, vars) = batch_query(chunk);
            let resp = self.github.graphql(&query, vars).await?;
            if !resp["data"].is_object() {
                bail!("GitHub reconcile query failed: {}", errors(&resp).unwrap_or_else(|| "no data".into()));
            }
            for (i, t) in chunk.iter().enumerate() {
                let alias = format!("p{i}");
                let repo = &resp["data"][&alias];
                let errs: Vec<&Value> = resp["errors"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|e| e["path"][0] == alias.as_str())
                    .collect();
                let why = errs.iter().filter_map(|e| e["message"].as_str()).collect::<Vec<_>>().join("; ");
                let found = repo["pullRequest"].is_object();
                let done = if found && errs.is_empty() {
                    self.fix(t, repo, &mut pass).await
                } else if !found && errs.iter().any(|e| e["type"] == "NOT_FOUND") {
                    self.missing(t, &why, &mut pass).await
                } else {
                    Err(anyhow!(
                        "GitHub returned an incomplete read: {}",
                        if why.is_empty() { "no error given" } else { &why }
                    ))
                };
                if let Err(e) = done {
                    failed.push(format!("{}: {e:#}", t.label()));
                }
            }
        }
        for row in self.store.pr_seen_all()? {
            if !tracked.contains_key(&(row.repo.clone(), row.pr)) {
                self.store.pr_seen_remove(&row.repo, row.pr)?;
            }
        }
        pass.woken.sort();
        pass.woken.dedup();
        for t in &pass.woken {
            self.flush_to(t).await?;
        }
        if !pass.actions.is_empty() {
            self.changed();
        }
        if !failed.is_empty() {
            if !pass.actions.is_empty() {
                tracing::info!(actions = ?pass.actions, "github reconcile");
            }
            bail!("GitHub reconcile failed for {}", failed.join("; "));
        }
        Ok(pass.actions)
    }

    fn learn_card_prs(&self) -> Result<()> {
        for (_, data, _, seq) in self.store.waiting_data(BOARD, false)? {
            let Some(mut data) = data.filter(|d| d.get("pr").is_none()) else { continue };
            let body = match data["handle"].as_str().map(|h| self.store.msg_get(h)).transpose()?.flatten() {
                Some(m) => m.body,
                None => continue,
            };
            data["pr"] = json!(card_pr(&body));
            self.store.waiting_set_data(BOARD, seq, &data)?;
        }
        Ok(())
    }

    fn tracked(&self, floor: &str) -> Result<BTreeMap<(String, u64), Tracked>> {
        let mut map = BTreeMap::new();
        for ln in self.store.links_open()?.into_iter().filter(|l| l.role == "worker") {
            let live = self.live.lock().unwrap_or_else(|e| e.into_inner()).get(&ln.child).cloned();
            let urls: Vec<String> = live
                .as_ref()
                .map(|l| {
                    let prs = l["prs"].as_array().into_iter().flatten();
                    prs.chain([&l["pr"]["url"]]).filter_map(|u| u.as_str().map(String::from)).collect()
                })
                .unwrap_or_default();
            for (o, r, n) in urls.iter().flat_map(|u| pr_refs(u)) {
                add(&mut map, &format!("{o}/{r}"), n).add(Some(&ln.parent), &ln.opened);
            }
        }
        for card in self.cards()? {
            if let Some(p) = &card.pr {
                let t = add(&mut map, &p.repo, p.number);
                t.add(card.ticket.as_deref(), &card.at);
                t.cards.push(card.seq);
            }
        }
        for w in self.store.merge_waits()? {
            let t = add(&mut map, &w.repo, w.pr);
            t.add(w.thread.as_deref(), &w.at);
            t.approval = Some(w.approval);
        }
        for row in self.store.pr_seen_all()? {
            let leaving = row.left.as_deref().is_some_and(|l| self.mins(Some(l)).is_some_and(|m| m < EXIT_WINDOW_MINS));
            let live = (row.queued || leaving) && row.state == "OPEN";
            if !live && !map.contains_key(&(row.repo.clone(), row.pr)) {
                continue;
            }
            let t = add(&mut map, &row.repo, row.pr);
            t.add(None, &row.since);
            for (ticket, since) in &row.tickets {
                t.add(Some(ticket), since);
            }
        }
        for t in map.values_mut() {
            if t.since.as_str() < floor {
                t.since = floor.into();
            }
            for (_, s) in t.tickets.iter_mut().filter(|(_, s)| s.as_str() < floor) {
                *s = floor.into();
            }
        }
        Ok(map)
    }

    async fn fix(&self, t: &Tracked, repo: &Value, pass: &mut Pass) -> Result<()> {
        let o = Obs::read(repo);
        let advance = {
            let _g = self.lock.lock().await;
            let cards: Vec<Card> = self.cards()?.into_iter().filter(|c| t.cards.contains(&c.seq)).collect();
            let wait = self.wait(t)?;
            let advance = match o.state.as_str() {
                "MERGED" | "CLOSED" => {
                    self.finished(t, &o, &cards, wait.as_ref(), pass)?;
                    None
                }
                "OPEN" => self.open(t, &o, &cards, wait, pass)?,
                _ => None,
            };
            let prev = self.store.pr_seen_get(&t.repo, t.number)?;
            let left = match &prev {
                Some(p) if o.state == "OPEN" && !o.queued && p.queued => Some(self.now()),
                Some(p) if o.state == "OPEN" && !o.queued => p.left.clone(),
                _ => None,
            };
            if prev.as_ref().map(|p| (p.state.as_str(), p.queued)) != Some((o.state.as_str(), o.queued)) {
                self.changed();
            }
            self.store.pr_seen_put(&PrSeen {
                repo: t.repo.clone(),
                pr: t.number,
                state: o.state.clone(),
                head: o.head.clone(),
                queued: o.queued,
                since: t.since.clone(),
                tickets: t.tickets.clone(),
                at: self.now(),
                left,
            })?;
            advance
        };
        match advance {
            Some(w) => self.advance(t, &o, w, pass).await,
            None => Ok(()),
        }
    }

    async fn missing(&self, t: &Tracked, why: &str, pass: &mut Pass) -> Result<()> {
        let _g = self.lock.lock().await;
        self.store.pr_seen_remove(&t.repo, t.number)?;
        if let Some(w) = self.wait(t)? {
            self.failed(t, w, why, pass)?;
        }
        Ok(())
    }

    fn wait(&self, t: &Tracked) -> Result<Option<MergeWait>> {
        let w = self.store.merge_wait_get(&t.repo, t.number)?;
        Ok(w.filter(|w| t.approval.as_ref() == Some(&w.approval)))
    }

    fn finished(&self, t: &Tracked, o: &Obs, cards: &[Card], wait: Option<&MergeWait>, pass: &mut Pass) -> Result<()> {
        let merged = o.state == "MERGED";
        let end = o.ended.clone().unwrap_or_else(|| self.now());
        let when = local_clock(&end);
        let note = if merged {
            format!("merged on GitHub at {}, {when}", short(&o.commit))
        } else {
            format!("closed on GitHub without merging, {when}")
        };
        let stale: Vec<&Card> =
            cards.iter().filter(|c| parse_ts(&c.at).zip(parse_ts(&end)).is_none_or(|(a, e)| a <= e)).collect();
        let event = match (merged, o.commit.as_str()) {
            (true, "") => format!("merged:{end}"),
            (true, commit) => format!("merged:{commit}"),
            (false, _) => format!("closed:{end}"),
        };
        let body = if merged {
            format!(
                "{} merged on GitHub at {}, {when} (merge commit {}). Run your post-merge steps. {}",
                t.label(),
                short(&o.commit),
                o.commit,
                t.url()
            )
        } else {
            format!("{} was closed on GitHub without merging, {when}. {}", t.label(), t.url())
        };
        let mut told = 0;
        for (ticket, _) in t.tickets.iter().filter(|(_, s)| later(&end, s)) {
            if self.store.link_get(ticket)?.is_some_and(|l| !l.closed) {
                told += self.wake(t, &event, ticket, "event", &body, pass)? as usize;
            }
        }
        let on_feed = told == 0
            && later(&end, &t.since)
            && (wait.is_some() || !stale.is_empty())
            && self.store.pr_event_mark(&t.repo, t.number, &event, BOARD, &self.now())?;
        let seqs: Vec<i64> = stale.iter().map(|c| c.seq).collect();
        let resolved = self.resolve_cards(|c| seqs.contains(&c.seq), &note)?;
        if !resolved.is_empty() {
            self.release_gates(stale.iter().filter(|c| c.kind == "gate").filter_map(|c| c.ticket.as_deref()))?;
        }
        if let Some(w) = wait {
            self.store.merge_wait_retire(w)?;
        }
        if on_feed {
            self.feed_note(None, &body)?;
        } else if told == 0 && !resolved.is_empty() {
            self.feed_note(t.tickets.first().map(|(x, _)| x.as_str()), &format!("{}: {note}", t.label()))?;
        }
        if told > 0 || on_feed || !resolved.is_empty() {
            pass.actions.push(format!("{} {note}", t.label()));
        }
        Ok(())
    }

    fn release_gates<'a>(&self, tickets: impl Iterator<Item = &'a str>) -> Result<()> {
        let open = self.cards()?;
        for ticket in tickets {
            if let Some(mut l) = self.store.link_get(ticket)?
                && !l.closed
                && l.gate.as_deref() == Some("merge")
                && !open.iter().any(|c| c.kind == "gate" && c.ticket.as_deref() == Some(ticket))
            {
                l.gate = None;
                l.gate_since = None;
                self.store.link_save(&l)?;
            }
        }
        Ok(())
    }

    fn open(
        &self,
        t: &Tracked,
        o: &Obs,
        cards: &[Card],
        mut wait: Option<MergeWait>,
        pass: &mut Pass,
    ) -> Result<Option<MergeWait>> {
        if self.dropped(t, o, wait.as_ref(), pass)?
            && let Some(w) = wait.take()
        {
            self.store.merge_wait_retire(&w)?;
        }
        if o.queued || o.ready == Ready::Yes {
            let this_pr = |c: &Card| c.pr.as_ref().is_some_and(|p| p.repo == t.repo && p.number == t.number);
            let note = if o.queued { "it is in the merge queue" } else { "it is ready to merge" };
            self.resolve_cards(|c| c.kind == "report" && this_pr(c) && c.text.contains(HELD), note)?;
        }
        let mut moved: Vec<(Option<String>, String, Vec<i64>)> = Vec::new();
        for c in cards.iter().filter(|c| c.kind == "gate") {
            let Some(ticket) = c.ticket.as_deref() else { continue };
            if !self.store.link_get(ticket)?.is_some_and(|l| !l.closed && l.gate.as_deref() == Some("merge")) {
                continue;
            }
            let Some(base) = c.pr.as_ref().and_then(|p| p.head.clone()) else {
                self.set_card_pr(c, t.pr(&o.head))?;
                continue;
            };
            if o.head.starts_with(&base) {
                continue;
            }
            match moved.iter_mut().find(|(x, _, _)| x.as_deref() == Some(ticket)) {
                Some((_, _, seqs)) => seqs.push(c.seq),
                None => moved.push((Some(ticket.into()), base, vec![c.seq])),
            }
        }
        if let Some(w) = &wait
            && w.head != o.head
            && !moved.iter().any(|(x, _, _)| *x == w.thread)
        {
            moved.push((w.thread.clone(), w.head.clone(), Vec::new()));
        }
        for (ticket, base, seqs) in &moved {
            if wait.as_ref().is_some_and(|w| w.thread == *ticket && w.head == o.head) {
                self.resolve_cards(|c| seqs.contains(&c.seq), "superseded by a newer merge approval")?;
                continue;
            }
            let approval = wait.as_ref().filter(|w| w.thread == *ticket).map(|w| w.approval.as_str());
            self.moved(t, o, ticket.as_deref(), base, seqs, approval, pass)?;
            if let Some(w) = wait.take_if(|w| w.thread == *ticket) {
                self.store.merge_wait_retire(&w)?;
            }
        }
        let Some(mut w) = wait.filter(|w| w.head == o.head) else { return Ok(None) };
        match (w.status, o.queued) {
            (WaitStatus::Waiting, true) => {
                w.status = WaitStatus::Sent;
                if self.store.merge_wait_swap(&w)? {
                    self.feed_note(
                        w.thread.as_deref(),
                        &format!("{} is in the merge queue at its approved head", t.label()),
                    )?;
                }
                Ok(None)
            }
            (WaitStatus::Waiting, false) if !o.draft && o.ready != Ready::Yes => {
                if let Ready::Failing(why) = &o.ready {
                    self.held(t, o, &w, why, pass)?;
                }
                Ok(None)
            }
            (WaitStatus::Waiting, false) if !o.draft => {
                if !w.pull_request_id.is_empty() && w.pull_request_id == o.id {
                    return Ok(Some(w));
                }
                let ask = Ask {
                    what: format!("{} is no longer the pull request that was approved", t.label()),
                    reply: "reply \"merge\" to approve it again, or \"hold\" to leave it",
                    context: format!(
                        "The approval was for GitHub node \"{}\". That number now resolves to \"{}\".",
                        w.pull_request_id, o.id
                    ),
                };
                self.raise(t, w.thread.as_deref(), "report", &ask, &o.head, None, Some(&w.approval), pass)?;
                self.store.merge_wait_retire(&w)?;
                Ok(None)
            }
            (WaitStatus::Sent, true) if w.failures > 0 => {
                w.failures = 0;
                w.last_error = None;
                self.store.merge_wait_swap(&w)?;
                Ok(None)
            }
            (WaitStatus::Sent, false) => {
                self.failed(t, w, "it left the merge queue and GitHub recorded no reason", pass)?;
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    fn held(&self, t: &Tracked, o: &Obs, w: &MergeWait, why: &str, pass: &mut Pass) -> Result<()> {
        let event = format!("checks:{}", w.approval);
        let ticket = w.thread.as_deref();
        if self.store.pr_event_seen(&t.repo, t.number, &event, ticket.unwrap_or(BOARD))? {
            return Ok(());
        }
        let ask = Ask {
            what: format!("{} {HELD} {}: {why}", t.label(), short(&o.head)),
            reply: "reply \"fix\" to send it back to the worker, or re-run the check on GitHub",
            context: "The approval stays. The board queues or merges the pull request once every check at this \
head has passed or ended cancelled, skipped or neutral. A push drops the approval."
                .into(),
        };
        self.raise(t, ticket, "report", &ask, &o.head, Some(&event), Some(&w.approval), pass)?;
        let text = format!("{} is held: a check failed at its approved head {}", t.label(), short(&o.head));
        self.feed_note(ticket, &text)?;
        pass.actions.push(text);
        Ok(())
    }

    fn dropped(&self, t: &Tracked, o: &Obs, wait: Option<&MergeWait>, pass: &mut Pass) -> Result<bool> {
        let Some((reason, at)) = o.removal.as_ref().filter(|(r, _)| !o.queued && r != "merged") else {
            return Ok(false);
        };
        let newest = self.store.merge_wait_get(&t.repo, t.number)?;
        if !later(at, &t.since) || newest.is_some_and(|w| parse_ts(&w.at).zip(parse_ts(at)).is_some_and(|(w, r)| w > r))
        {
            return Ok(false);
        }
        let event = format!("removed:{at}");
        let why = why_dropped(reason);
        let ask = Ask {
            what: format!("{} was dropped from the merge queue ({why}) at {}", t.label(), local_clock(at)),
            reply: "reply \"requeue\" to queue it again, or \"hold\" to leave it out",
            context: String::new(),
        };
        let mut to: Vec<Option<&str>> =
            t.tickets.iter().filter(|(_, s)| later(at, s)).map(|(x, _)| Some(x.as_str())).collect();
        if to.is_empty() {
            to.push(None);
        }
        for ticket in to {
            if self.store.pr_event_seen(&t.repo, t.number, &event, ticket.unwrap_or(BOARD))? {
                continue;
            }
            let approval = wait.filter(|w| w.thread.as_deref() == ticket).map(|w| w.approval.as_str());
            self.raise(t, ticket, "report", &ask, &o.head, Some(&event), approval, pass)?;
            self.feed_note(ticket, &format!("{} was dropped from the merge queue: {why}", t.label()))?;
            pass.actions.push(format!("{} dropped from the merge queue: {why}", t.label()));
        }
        Ok(true)
    }

    #[allow(clippy::too_many_arguments)]
    fn moved(
        &self,
        t: &Tracked,
        o: &Obs,
        ticket: Option<&str>,
        base: &str,
        old: &[i64],
        approval: Option<&str>,
        pass: &mut Pass,
    ) -> Result<()> {
        let label = t.label();
        let (kind, ask) = if o.queued {
            let ask = Ask {
                what: format!(
                    "{label} is in the merge queue at head {}, not at its approved head {}",
                    short(&o.head),
                    short(base)
                ),
                reply: "reply \"ok\" to let it merge at that head, or take it out of the queue on GitHub",
                context: format!("The approval was for head {base}. The queued head is {}.", o.head),
            };
            ("report", ask)
        } else {
            let ask = Ask {
                what: format!("{label} moved to head {} after it was approved at {}", short(&o.head), short(base)),
                reply: "reply \"merge\" to merge it at that head, or \"hold\" to wait",
                context: format!(
                    "The approval was for head {base}, so it no longer applies. The new head is {}.",
                    o.head
                ),
            };
            ("gate", ask)
        };
        self.raise(t, ticket, kind, &ask, &o.head, None, approval, pass)?;
        let note = format!("head moved to {} after this approval; replaced by a new card", short(&o.head));
        self.resolve_cards(|c| old.contains(&c.seq), &note)?;
        if kind == "report" {
            self.release_gates(ticket.into_iter())?;
        }
        self.feed_note(ticket, &format!("{label} merge approval replaced: the head moved to {}", short(&o.head)))?;
        pass.actions.push(format!("{label} merge approval replaced: head moved to {}", short(&o.head)));
        Ok(())
    }

    async fn advance(&self, t: &Tracked, o: &Obs, w: MergeWait, pass: &mut Pass) -> Result<()> {
        let (query, vars) = match o.queue {
            true => (ENQUEUE_PR, json!({"id": o.id, "head": w.head})),
            false => {
                let method = w.method.clone().or_else(|| o.method.clone()).unwrap_or_else(|| "MERGE".into());
                (MERGE_PR, json!({"id": o.id, "head": w.head, "method": method}))
            }
        };
        let resp = self.github.graphql(query, vars).await;
        let _g = self.lock.lock().await;
        let failure = match &resp {
            Err(e) => Some(format!("{e:#}")),
            Ok(r) => errors(r),
        };
        if let Some(e) = failure {
            return self.failed(t, w, &e, pass);
        }
        let mut sent = w.clone();
        sent.status = WaitStatus::Sent;
        sent.failures = 0;
        sent.last_error = None;
        if !self.store.merge_wait_swap(&sent)? {
            return Ok(());
        }
        if o.queue
            && let Some(mut seen) = self.store.pr_seen_get(&t.repo, t.number)?
        {
            seen.queued = true;
            self.store.pr_seen_put(&seen)?;
            self.changed();
        }
        let r = resp.unwrap_or_default();
        let text = match o.queue {
            true => format!(
                "{} queued at its approved head {} (position {}) once it was {}",
                t.label(),
                short(&w.head),
                r["data"]["enqueuePullRequest"]["mergeQueueEntry"]["position"],
                o.merge_state
            ),
            false => {
                format!("{} merged at its approved head {} once it was {}", t.label(), short(&w.head), o.merge_state)
            }
        };
        self.feed_note(w.thread.as_deref(), &text)?;
        pass.actions.push(text);
        Ok(())
    }

    fn failed(&self, t: &Tracked, w: MergeWait, why: &str, pass: &mut Pass) -> Result<()> {
        let mut next = w.clone();
        next.failures += 1;
        next.last_error = Some(why.into());
        if next.failures < MAX_FAILURES {
            self.store.merge_wait_swap(&next)?;
            return Ok(());
        }
        let verb = if w.status == WaitStatus::Sent { "keep" } else { "queue" };
        let ask = Ask {
            what: format!("GitHub would not {verb} {} ({why})", t.label()),
            reply: "reply \"merge\" to try again, or \"hold\" to leave it",
            context: format!("It was approved at head {}. GitHub refused {MAX_FAILURES} checks in a row.", w.head),
        };
        self.raise(t, w.thread.as_deref(), "report", &ask, &w.head, None, Some(&w.approval), pass)?;
        self.store.merge_wait_retire(&w)?;
        self.feed_note(w.thread.as_deref(), &format!("{} could not be queued: {why}", t.label()))?;
        pass.actions.push(format!("{} could not be queued: {why}", t.label()));
        Ok(())
    }

    fn wake(&self, t: &Tracked, event: &str, ticket: &str, kind: &str, body: &str, pass: &mut Pass) -> Result<bool> {
        let now = self.now();
        let wake = Wake { repo: &t.repo, pr: t.number, event: Some(event), ticket, from: GITHUB, kind, body, at: &now };
        let fresh = self.store.pr_event_wake(&wake, |h| format!("{}\n{body}", header(kind, GITHUB, h, None)))?;
        if fresh.is_some() {
            pass.woken.push(ticket.into());
        }
        Ok(fresh.is_some())
    }

    #[allow(clippy::too_many_arguments)]
    fn raise(
        &self,
        t: &Tracked,
        ticket: Option<&str>,
        kind: &str,
        ask: &Ask,
        head: &str,
        event: Option<&str>,
        approval: Option<&str>,
        pass: &mut Pass,
    ) -> Result<()> {
        let link = ticket.map(|x| self.store.link_get(x)).transpose()?.flatten().filter(|l| !l.closed);
        let act = if link.is_some() { ask.reply } else { NO_TICKET };
        let tail = if ask.context.is_empty() { String::new() } else { format!("\n\n{}", ask.context) };
        let body = format!("**ACTION NEEDED:** {}: {act}, here: {}{tail}", ask.what, t.url());
        let mark = |to: &str| match event {
            Some(e) => self.store.pr_event_mark(&t.repo, t.number, e, to, &self.now()).map(|_| ()),
            None => Ok(()),
        };
        match link {
            Some(mut l) if l.parent == BOARD => {
                let handle = self.store.msg_new(GITHUB, &l.child, kind, &body, &self.now())?;
                self.card_add(NewCard {
                    kind,
                    label: Some(&l.label),
                    ticket: Some(&l.child),
                    link: l.link.as_deref(),
                    body: &body,
                    handle: Some(&handle),
                    options: None,
                    pr: Some(t.pr(head)),
                    approval,
                    offer: None,
                })?;
                if kind == "gate" && l.gate.as_deref().is_none_or(|g| g == "merge") {
                    l.gate = Some("merge".into());
                    l.gate_since = Some(self.now());
                    l.nudges = 0;
                    l.escalated = false;
                    self.store.link_save(&l)?;
                }
                mark(&l.child)
            }
            Some(l) => {
                let now = self.now();
                let wake = Wake {
                    repo: &t.repo,
                    pr: t.number,
                    event,
                    ticket: &l.child,
                    from: GITHUB,
                    kind,
                    body: &body,
                    at: &now,
                };
                if self.store.pr_event_wake(&wake, |h| format!("{}\n{body}", header(kind, GITHUB, h, None)))?.is_some()
                {
                    pass.woken.push(l.child.clone());
                }
                Ok(())
            }
            None => {
                let handle = self.store.msg_new(GITHUB, BOARD, "report", &body, &self.now())?;
                self.card_add(NewCard {
                    kind: "report",
                    label: Some(GITHUB),
                    ticket: None,
                    link: None,
                    body: &body,
                    handle: Some(&handle),
                    options: Some(Vec::new()),
                    pr: Some(t.pr(head)),
                    approval,
                    offer: None,
                })?;
                mark(ticket.unwrap_or(BOARD))
            }
        }
    }

    fn set_card_pr(&self, c: &Card, pr: CardPr) -> Result<()> {
        let row = self.store.waiting_data(BOARD, false)?.into_iter().find(|(_, _, _, seq)| *seq == c.seq);
        let Some((_, Some(mut data), _, seq)) = row else { return Ok(()) };
        data["pr"] = json!(pr);
        self.store.waiting_set_data(BOARD, seq, &data)
    }

    fn feed_note(&self, ticket: Option<&str>, text: &str) -> Result<()> {
        let on_board = |t: &&str| self.store.link_get(t).ok().flatten().is_some_and(|l| l.parent == BOARD);
        let to = ticket.filter(on_board).unwrap_or(BOARD);
        self.store.msg_new(GITHUB, to, "note", text, &self.now())?;
        self.changed();
        Ok(())
    }
}
