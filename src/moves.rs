use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::board::{BOARD, Card, NewCard};
use crate::clock::local_clock;
use crate::store::{Link, Pending};
use crate::wf::Wf;

pub const MOVE: &str = "move";
pub const WAIT: &str = "wait";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Offer {
    pub thread: String,
    pub run: String,
    pub pool: String,
    pub instance_id: String,
    pub title: String,
}

fn offered_key(thread: &str, run: &str) -> String {
    format!("move_offer:{thread}:{run}")
}

impl Wf {
    pub(crate) async fn offer_moves(&self, failed: &[Value]) -> Vec<String> {
        let mut actions = Vec::new();
        if let Err(e) = self.retire_offers(failed) {
            tracing::warn!("move offers not checked: {e:#}");
        }
        for t in failed {
            match self.offer_move(t).await {
                Ok(Some(a)) => actions.push(a),
                Ok(None) => {}
                Err(e) => tracing::warn!(
                    "move offer skipped thread {}: {}",
                    t["threadId"].as_str().unwrap_or("?"),
                    self.t3.redact(format!("{e:#}"))
                ),
            }
        }
        actions
    }

    fn retire_offers(&self, failed: &[Value]) -> Result<()> {
        let still =
            |o: &Offer| failed.iter().any(|t| t["threadId"] == o.thread.as_str() && t["latestRunId"] == o.run.as_str());
        self.resolve_cards(|c| c.offer.as_ref().is_some_and(|o| !still(o)), "the thread ran again")?;
        Ok(())
    }

    async fn offer_move(&self, t: &Value) -> Result<Option<String>> {
        let (Some(thread), Some(run)) = (t["threadId"].as_str(), t["latestRunId"].as_str()) else {
            return Ok(None);
        };
        let Some(instance) = t["providerInstanceId"].as_str() else { return Ok(None) };
        if t["status"] != "failed" || t["archived"] == true {
            return Ok(None);
        }
        if !self.store.limit_stop(thread, run)?.is_some_and(|s| s.limit) {
            return Ok(None);
        }
        let Some(pick) = self.store.pick_by_thread(thread)? else { return Ok(None) };
        if self.store.ask_thread(thread)? {
            return Ok(None);
        }
        if !self.config.pools.contains_key(&pick.pool) {
            return Ok(None);
        }
        let key = offered_key(thread, run);
        let _g = self.lock.lock().await;
        if self.store.meta_get(&key)?.is_some() {
            return Ok(None);
        }
        let link = self.store.link_get(thread)?.filter(|l| !l.closed);
        let title = t["title"].as_str().filter(|s| !s.is_empty()).unwrap_or(thread).to_string();
        let label = link.as_ref().map_or(title.clone(), |l| l.label.clone());
        let url = t["link"].as_str().map(String::from).or_else(|| link.as_ref().and_then(|l| l.link.clone()));
        let reset =
            self.store.limits()?.get(instance).map(|l| format!(" until {}", local_clock(&l.until))).unwrap_or_default();
        let here = url.as_deref().map(|u| format!(", here: {u}")).unwrap_or_default();
        let body = format!(
            "**ACTION NEEDED:** {label} stopped on a usage limit on {instance}{reset}: reply \"{MOVE}\" to move it \
             to a ready member of pool {}, or \"{WAIT}\" to leave it until the reset{here}",
            pick.pool
        );
        let offer =
            Offer { thread: thread.into(), run: run.into(), pool: pick.pool, instance_id: instance.into(), title };
        let id = self.card_add(NewCard {
            kind: "report",
            label: Some(&label),
            ticket: Some(thread),
            link: url.as_deref(),
            body: &body,
            handle: None,
            options: Some(vec![MOVE.into(), WAIT.into()]),
            pr: None,
            approval: None,
            offer: Some(&offer),
        })?;
        self.store.meta_set(&key, &id)?;
        Ok(Some(format!("offered to move {label} off {instance} as {id}")))
    }

    pub(crate) async fn answer_offer(&self, card: &Card, offer: &Offer, text: &str) -> Result<Value> {
        let note = match text.to_ascii_lowercase().as_str() {
            WAIT => {
                self.store.waiting_resolve_seq(BOARD, card.seq, Some("answered: wait"), &self.now())?;
                self.changed();
                return Ok(json!({"card": card.id, "moved": false}));
            }
            MOVE => self.move_thread(offer).await?,
            _ => bail!("card {} takes \"{MOVE}\" or \"{WAIT}\"", card.id),
        };
        self.store.waiting_resolve_seq(
            BOARD,
            card.seq,
            Some(&format!("answered: move; {}", note["summary"].as_str().unwrap_or_default())),
            &self.now(),
        )?;
        self.changed();
        let mut out = note;
        out["card"] = json!(card.id);
        Ok(out)
    }

    async fn move_thread(&self, o: &Offer) -> Result<Value> {
        let old = &o.thread;
        let th = self.t3.call("t3_thread_read", json!({"threadId": old, "limit": 1, "maxCharsPerItem": 1})).await?;
        let th = &th["thread"];
        if th["status"] != "failed" || th["latestRunId"] != o.run.as_str() || !th["activeRunId"].is_null() {
            bail!("{} ran again after its usage limit, so nothing was moved", o.title);
        }
        let why = "is the member the thread stopped on";
        let picked = self.pick_avoiding(&o.pool, Some((&o.instance_id, why))).await?;
        let fork =
            self.t3.call("t3_thread_transfer", json!({"action": "fork", "threadId": old, "title": o.title})).await?;
        let new = fork["targetThreadId"]
            .as_str()
            .ok_or_else(|| anyhow!("t3_thread_transfer returned no fork thread for {}", o.title))?
            .to_string();
        self.t3
            .call("t3_thread_configure", json!({"threadId": new, "modelSelection": picked.model_selection}))
            .await
            .map_err(|e| {
            anyhow!("forked {} as {new}, but moving the fork to {} failed: {e}", o.title, picked.instance_id)
        })?;
        let f = self.t3.call("t3_thread_read", json!({"threadId": new, "limit": 1, "maxCharsPerItem": 1})).await?;
        let f = &f["thread"];
        if f["providerInstanceId"] != picked.instance_id.as_str() {
            bail!(
                "forked {} as {new}, but the fork runs on {} instead of {}",
                o.title,
                f["providerInstanceId"].as_str().unwrap_or("an unknown instance"),
                picked.instance_id
            );
        }
        let link = f["link"].as_str().map(String::from);
        let rebound = self.rebind(old, &new, link.as_deref())?;
        self.note_live(&new, f);
        let summary = format!("moved {} from {} to {new} {}", o.title, o.instance_id, picked.summary());
        let handle = self.store.msg_new(BOARD, BOARD, "note", &summary, &self.now())?;
        self.store.pick_bind(picked.seq, Some(&handle), &new, Some(old))?;
        let mut warnings = Vec::new();
        let resume = format!(
            "This thread is a fork of {}, which stopped on a usage limit on {}. It now runs on {}{}. \
             The last run may have stopped partway: check where the work stands, then carry on from there.",
            th["link"].as_str().unwrap_or(old),
            o.instance_id,
            picked.instance_id,
            if rebound { ", and its workflow link moved with it" } else { "" }
        );
        if let Err(e) =
            self.t3.call("t3_thread_send", json!({"threadId": new, "message": resume, "mode": "auto"})).await
        {
            warnings.push(format!("resuming the fork failed, so send it a message by hand: {e}"));
        }
        if let Err(e) = self.t3.call("t3_thread_organize", json!({"threadId": old, "action": "settle"})).await {
            warnings.push(format!("settling the old thread failed: {e}"));
        }
        let mut out = json!({"moved": true, "from": old, "thread": new, "link": link, "instanceId": picked.instance_id,
                             "reason": picked.reason, "pick": picked.seq, "summary": summary});
        if !warnings.is_empty() {
            out["warnings"] = json!(warnings);
        }
        Ok(out)
    }

    fn rebind(&self, old: &str, new: &str, link: Option<&str>) -> Result<bool> {
        let mut rebound = false;
        if let Some(ln) = self.store.link_get(old)?.filter(|l| !l.closed) {
            let moved = Link {
                child: new.into(),
                link: link.map(String::from).or(ln.link.clone()),
                last_nudge: None,
                nudges: 0,
                escalated: false,
                bad_run: None,
                settled_at: None,
                ..ln
            };
            self.store.link_delete(old)?;
            self.store.link_save(&moved)?;
            rebound = true;
        }
        for mut ln in self.store.children(old, true)? {
            ln.parent = new.into();
            self.store.link_save(&ln)?;
            rebound = true;
        }
        if let Some(role) = self.store.node_role(old)? {
            self.store.node_set(new, &role, &self.now())?;
        }
        self.store.pending_retarget(old, new)?;
        for p in self.store.pending_all()?.into_iter().filter(|p| p.thread.as_deref() == Some(old)) {
            let link = link.map(String::from).or(p.link.clone());
            self.store.pending_put(&Pending { thread: Some(new.into()), link, ..p })?;
            rebound = true;
        }
        for (_, data, _, seq) in self.store.waiting_data(BOARD, false)? {
            if let Some(mut d) = data.filter(|d| d["ticket"] == old && d["move"].is_null()) {
                d["ticket"] = json!(new);
                self.store.waiting_set_data(BOARD, seq, &d)?;
            }
        }
        self.store.outbox_retarget(old, new)?;
        Ok(rebound)
    }
}
