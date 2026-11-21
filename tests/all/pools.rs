use crate::common::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;
use t3_codemode::board::BOARD;
use t3_codemode::clock::local_clock;
use t3_codemode::config::Config;

use t3_codemode::app::App;
use t3_codemode::fake::{FakeUsage, disabled_provider, project, provider, thread, usage_limit_item};
use t3_codemode::pools::{Inputs, PoolConfig, PoolMember, choose};
use t3_codemode::store::{Limit, Store};
use t3_codemode::usage::Usage;

const OPUS: &str = "claude-opus-5-5";

fn config(strategy: &str, members: Value, board_pool: bool) -> Config {
    let mut c = json!({"pools": {"opus": {"driver": "claudeAgent", "model": OPUS, "effort": "high",
                                          "strategy": strategy, "members": members}}});
    if board_pool {
        c["board"] = json!({"pool": "opus"});
    }
    Config::from_value(c).unwrap()
}

fn pooled(strategy: &str, members: Value) -> H {
    let h = H::build(Vec::new(), config(strategy, members, true));
    h.fake.set_providers(vec![
        provider("claude-a", "claudeAgent", &[OPUS, "claude-haiku-5-5"]),
        provider("claude-b", "claudeAgent", &[OPUS]),
        provider("claude-c", "claudeAgent", &["claude-haiku-5-5"]),
        disabled_provider("claude-off", "claudeAgent"),
        provider("codex", "codex", &["gpt-6-astra"]),
    ]);
    h.fake.set_projects(vec![project("p-widgets", "/src/widgets")]);
    h.remotes.set("/src/widgets", "git@github.com:acme/widgets.git");
    h
}

fn two(wa: u32, wb: u32) -> Value {
    json!([{"instanceId": "claude-a", "weight": wa}, {"instanceId": "claude-b", "weight": wb}])
}

async fn launch(h: &H, n: u64) -> Value {
    let out = h.exec(&format!("async () => wf.launchTicket({{ issue: 'acme/widgets#{n}' }})")).await;
    result(&out)
}

async fn launches(h: &H, from: u64, count: u64) -> Vec<String> {
    let mut out = Vec::new();
    for n in from..from + count {
        out.push(launch(h, n).await["instanceId"].as_str().unwrap().to_string());
    }
    out
}

fn tally(picks: &[String], id: &str) -> usize {
    picks.iter().filter(|p| *p == id).count()
}

fn limit(h: &H, id: &str, until: &str) {
    h.app
        .store
        .limit_set(id, &Limit { until: until.into(), reason: Some("usage limit".into()), at: T0.into(), thread: None })
        .unwrap();
}

async fn pick(h: &H) -> Value {
    result(&h.exec("async () => wf.pick({ pool: 'opus' })").await)
}

#[tokio::test]
async fn weighted_one_to_one_splits_evenly_and_repeats_from_the_same_state() {
    let first = pooled("weighted", two(1, 1));
    let a = launches(&first, 1, 8).await;
    assert_eq!((tally(&a, "claude-a"), tally(&a, "claude-b")), (4, 4), "{a:?}");
    let second = pooled("weighted", two(1, 1));
    assert_eq!(launches(&second, 1, 8).await, a);
    let l = &first.fake.launched()[0];
    assert_eq!(
        l["modelSelection"],
        json!({"instanceId": a[0], "model": OPUS, "options": [{"id": "effort", "value": "high"}]})
    );
}

#[tokio::test]
async fn weights_three_to_one_lean_toward_the_heavier_member() {
    let h = pooled("weighted", two(1, 3));
    let picks = launches(&h, 1, 8).await;
    assert_eq!((tally(&picks, "claude-a"), tally(&picks, "claude-b")), (2, 6), "{picks:?}");
}

#[tokio::test]
async fn priority_fills_the_first_member_until_it_is_out() {
    let h = pooled("priority", two(1, 1));
    assert_eq!(launches(&h, 1, 3).await, ["claude-a"; 3]);
    limit(&h, "claude-a", "2026-11-17T21:00:00Z");
    let r = launch(&h, 4).await;
    assert_eq!(r["instanceId"], "claude-b");
    let reason = r["reason"].as_str().unwrap();
    assert!(reason.starts_with(&format!("claudeAgent {OPUS}, effort high; priority")), "{reason}");
    assert!(reason.contains(&format!("claude-a limited until {}", local_clock("2026-11-17T21:00:00Z"))), "{reason}");
}

#[tokio::test]
async fn a_priority_pick_reads_no_thread_counts() {
    let h = pooled("priority", two(1, 1));
    assert_eq!(pick(&h).await["instanceId"], "claude-a");
    assert!(!h.fake.calls().iter().any(|c| c == "t3_thread_list"), "{:?}", h.fake.calls());
}

#[tokio::test]
async fn a_limited_member_is_skipped_and_named_with_its_reset_time() {
    let h = pooled("weighted", two(1, 1));
    limit(&h, "claude-a", "2026-11-17T21:00:00Z");
    let picks = launches(&h, 1, 4).await;
    assert_eq!(picks, ["claude-b"; 4]);
    let r = pick(&h).await;
    let reason = r["reason"].as_str().unwrap();
    let until = local_clock("2026-11-17T21:00:00Z");
    assert!(reason.contains(&format!("claude-a limited until {until}")), "{reason}");
    assert_eq!(r["skipped"], json!([{"instanceId": "claude-a", "reason": format!("limited until {until}")}]));
    h.clock.set("2026-11-17T21:00:01Z");
    assert_eq!(pick(&h).await["instanceId"], "claude-a");
}

#[tokio::test]
async fn every_member_out_fails_with_an_error_card_and_launches_nothing() {
    let h = pooled(
        "weighted",
        json!([{"instanceId": "claude-a"}, {"instanceId": "claude-off"}, {"instanceId": "claude-c"}]),
    );
    limit(&h, "claude-a", "2026-11-17T21:00:00Z");
    let out = h.exec("async () => wf.launchTicket({ issue: 'acme/widgets#1' })").await;
    let e = err_text(&out);
    assert!(e.contains("pool opus has no member that can run"), "{e}");
    assert!(e.contains("claude-off disabled"), "{e}");
    assert!(e.contains(&format!("claude-c does not list model {OPUS}")), "{e}");
    let cards = h.app.wf.cards().unwrap();
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0].kind, "error");
    assert!(cards[0].text.contains("pool opus"), "{}", cards[0].text);
    assert!(h.fake.launched().is_empty());
    assert!(h.app.store.pending_all().unwrap().is_empty());
    let ask = h.app.wf.ask("Anything waiting?").await.unwrap_err();
    assert!(format!("{ask:#}").contains("pool opus has no member that can run"), "{ask:#}");
    assert_eq!(h.app.wf.cards().unwrap().len(), 2);
    assert!(h.fake.launched().is_empty());
    let direct = h.exec("async () => wf.pick({ pool: 'opus' })").await;
    assert!(err_text(&direct).contains("pool opus has no member that can run"), "{}", direct.text);
}

#[tokio::test]
async fn a_member_missing_the_model_is_skipped_with_that_reason() {
    let h = pooled("weighted", json!([{"instanceId": "claude-c"}, {"instanceId": "claude-b"}]));
    let r = pick(&h).await;
    assert_eq!(r["instanceId"], "claude-b");
    assert_eq!(r["skipped"], json!([{"instanceId": "claude-c", "reason": format!("does not list model {OPUS}")}]));
}

#[tokio::test]
async fn a_member_without_the_pools_effort_is_skipped() {
    let h = pooled("weighted", two(1, 1));
    let mut a = provider("claude-a", "claudeAgent", &[OPUS]);
    a["models"][0]["options"][0]["values"] = json!(["low", "medium"]);
    h.fake.set_providers(vec![a, provider("claude-b", "claudeAgent", &[OPUS])]);
    let r = pick(&h).await;
    assert_eq!(r["instanceId"], "claude-b");
    assert_eq!(
        r["skipped"],
        json!([{"instanceId": "claude-a", "reason": format!("does not offer effort high for {OPUS}")}])
    );
    let mut a = provider("claude-a", "claudeAgent", &[OPUS]);
    a["models"][0]["options"] = json!([{"id": "effort"}]);
    h.fake.set_providers(vec![a, provider("claude-b", "claudeAgent", &[OPUS])]);
    assert_eq!(pick(&h).await["instanceId"], "claude-b");
}

#[tokio::test]
async fn a_reserved_member_is_a_last_resort_while_no_usage_source_exists() {
    let h =
        pooled("weighted", json!([{"instanceId": "claude-a", "weight": 5, "reserve": 30}, {"instanceId": "claude-b"}]));
    let picks = launches(&h, 1, 4).await;
    assert_eq!(picks, ["claude-b"; 4]);
    let r = pick(&h).await;
    assert!(r["reason"].as_str().unwrap().contains("claude-a held at its 30% reserve"), "{r}");
    limit(&h, "claude-b", "2026-11-17T21:00:00Z");
    let r = launch(&h, 5).await;
    assert_eq!(r["instanceId"], "claude-a");
    let reason = r["reason"].as_str().unwrap();
    assert!(
        reason.starts_with(&format!("claudeAgent {OPUS}, effort high; last resort: claude-a holds a 30% reserve")),
        "{reason}"
    );
    assert!(reason.contains("claude-b limited until"), "{reason}");
}

#[tokio::test]
async fn a_launch_naming_a_reserved_instance_directly_still_goes_through() {
    let h = pooled("weighted", json!([{"instanceId": "claude-a", "reserve": 30}, {"instanceId": "claude-b"}]));
    let out = h
        .exec(
            "async () => t3.call('t3_thread_launch', { projectId: 'p-widgets', title: 'direct', message: 'go',
               modelSelection: { instanceId: 'claude-a', model: 'claude-opus-5-5' } })",
        )
        .await;
    assert_eq!(result(&out)["modelSelection"]["instanceId"], "claude-a");
    assert_eq!(h.fake.launched().len(), 1);
}

#[tokio::test]
async fn wf_pick_returns_a_model_selection_and_records_the_pick() {
    let h = pooled("weighted", two(1, 1));
    let r = pick(&h).await;
    assert_eq!(r["pool"], "opus");
    assert_eq!(r["model"], OPUS);
    assert_eq!(r["options"], json!([{"id": "effort", "value": "high"}]));
    assert_eq!(r["modelSelection"], json!({"instanceId": r["instanceId"], "model": OPUS, "options": r["options"]}));
    assert_eq!((&r["driver"], &r["effort"]), (&json!("claudeAgent"), &json!("high")));
    assert!(
        r["reason"].as_str().unwrap().starts_with(&format!("claudeAgent {OPUS}, effort high; weighted: weight 1 of 2")),
        "{r}"
    );
    let launched = h
        .exec(
            "async () => { const p = await wf.pick({ pool: 'opus' });
               return t3.call('t3_thread_launch', { projectId: 'p-widgets', title: 't', message: 'm', modelSelection: p.modelSelection }); }",
        )
        .await;
    assert_eq!(result(&launched)["modelSelection"]["model"], OPUS);
    let pools = result(&h.exec("async () => wf.pools()").await);
    let members = pools[0]["members"].as_array().unwrap();
    assert_eq!(members.iter().map(|m| m["picks"].as_u64().unwrap()).sum::<u64>(), 2);
    let unknown = h.exec("async () => wf.pick({ pool: 'nope' })").await;
    assert!(err_text(&unknown).contains("no pool \"nope\"; configured pools: opus"), "{}", unknown.text);
}

#[tokio::test]
async fn wf_pools_reports_state_reset_time_and_running_count() {
    let h = pooled(
        "weighted",
        json!([{"instanceId": "claude-a"}, {"instanceId": "claude-b"}, {"instanceId": "claude-off"}, {"instanceId": "claude-c"}]),
    );
    launch(&h, 1).await;
    limit(&h, "claude-b", "2026-11-17T21:00:00Z");
    let pools = result(&h.exec("async () => wf.pools()").await);
    let p = &pools[0];
    assert_eq!(p["pool"], "opus");
    assert_eq!(p["strategy"], "weighted");
    assert_eq!(p["error"], Value::Null);
    let m: Vec<&Value> = p["members"].as_array().unwrap().iter().collect();
    assert_eq!(m[0]["state"], "ready");
    assert_eq!(m[0]["running"], 1);
    assert_eq!(m[0]["picks"], 1);
    assert_eq!(m[1]["state"], "limited");
    assert_eq!(m[1]["limitedUntil"], "2026-11-17T21:00:00Z");
    assert_eq!(m[1]["limitedUntilLocal"], local_clock("2026-11-17T21:00:00Z"));
    assert_eq!(m[1]["running"], 0);
    assert_eq!(m[2]["state"], "disabled");
    assert_eq!(m[3]["state"], "missing-model");
}

#[tokio::test]
async fn running_counts_read_every_page_of_threads() {
    let h = pooled("weighted", two(1, 1));
    for n in 0..2001 {
        let mut t = thread(&format!("busy:{n:04}"), "running", T0);
        t["providerInstanceId"] = json!("claude-b");
        h.fake.put(t);
    }
    let pools = result(&h.exec("async () => wf.pools()").await);
    assert_eq!(pools[0]["members"][1]["running"], 2001);
}

#[tokio::test]
async fn fewer_running_threads_break_a_weighted_tie() {
    let h = pooled("weighted", two(1, 1));
    let mut t = thread("busy:a", "running", T0);
    t["providerInstanceId"] = json!("claude-a");
    h.fake.put(t);
    assert_eq!(pick(&h).await["instanceId"], "claude-b");
}

#[tokio::test]
async fn the_feed_shows_which_instance_a_pool_launch_went_to() {
    let h = pooled("weighted", two(1, 1));
    let r = launch(&h, 41).await;
    let feed = h.app.wf.feed(10).unwrap();
    let entry = feed.iter().find(|f| f["instanceId"].is_string()).expect("a pool launch entry");
    assert_eq!(entry["instanceId"], r["instanceId"]);
    assert_eq!(entry["reason"], r["reason"]);
    assert_eq!(entry["label"], BOARD);
    assert!(
        entry["head"]
            .as_str()
            .unwrap()
            .starts_with(&format!("widgets#41 launched on {}", r["instanceId"].as_str().unwrap()))
    );
    let a = h.app.wf.ask("Which tickets wait on me?").await.unwrap();
    assert!(a["instanceId"].is_string(), "{a}");
    assert_eq!(h.fake.launched()[1]["modelSelection"]["model"], OPUS);
}

#[tokio::test]
async fn a_pool_launch_refuses_a_different_model() {
    let h = pooled("weighted", two(1, 1));
    let out = h.exec("async () => wf.launchTicket({ issue: 'acme/widgets#1', model: 'claude-haiku-5-5' })").await;
    assert!(err_text(&out).contains("board.pool opus runs claude-opus-5-5"), "{}", out.text);
    assert!(h.fake.launched().is_empty());
}

#[tokio::test]
async fn limits_credits_and_pick_history_survive_a_restart() {
    let mut steady = pooled("weighted", two(1, 3));
    let all = launches(&steady, 1, 6).await;
    let mut h = pooled("weighted", two(1, 3));
    let before = launches(&h, 1, 3).await;
    limit(&h, "claude-a", "2026-11-17T21:00:00Z");
    h.reopen();
    let limits = h.app.store.limits().unwrap();
    assert_eq!(limits["claude-a"].until, "2026-11-17T21:00:00Z");
    assert_eq!(h.app.store.picks("opus", 10).unwrap().len(), 3);
    h.app.store.limit_clear("claude-a").unwrap();
    let after = launches(&h, 4, 3).await;
    assert_eq!([before, after].concat(), all);
    steady.reopen();
    assert_eq!(steady.app.store.picks("opus", 100).unwrap().len(), 6);
}

#[tokio::test]
async fn no_pool_set_launches_exactly_as_before() {
    let h = H::new();
    h.fake.set_projects(vec![project("p-widgets", "/src/widgets")]);
    h.remotes.set("/src/widgets", "git@github.com:acme/widgets.git");
    let r = launch(&h, 1).await;
    assert!(r.get("instanceId").is_none() && r.get("reason").is_none(), "{r}");
    let l = &h.fake.launched()[0];
    assert_eq!(
        l["modelSelection"],
        json!({"instanceId": "claudeAgent", "model": OPUS, "options": [{"id": "effort", "value": "high"}]})
    );
    assert!(!h.fake.calls().iter().any(|c| c == "orchestrator_capabilities"));
    assert!(h.app.wf.feed(10).unwrap().is_empty());
}

#[tokio::test]
async fn a_member_of_another_driver_or_unknown_instance_is_rejected() {
    let h = pooled("weighted", json!([{"instanceId": "claude-a"}, {"instanceId": "codex"}]));
    let out = h.exec("async () => wf.launchTicket({ issue: 'acme/widgets#1' })").await;
    assert!(
        err_text(&out).contains("pool opus: member codex is a codex instance, not claudeAgent; set \"mixed\": true"),
        "{}",
        out.text
    );
    assert!(h.fake.launched().is_empty());
    assert!(h.app.wf.check_pools().await.is_err());
    let h = pooled("weighted", json!([{"instanceId": "claude-a"}, {"instanceId": "claude-z"}]));
    let e = format!("{:#}", h.app.wf.check_pools().await.unwrap_err());
    assert!(e.contains("pool opus: member claude-z is not a T3 provider instance"), "{e}");
    let pools = result(&h.exec("async () => wf.pools()").await);
    assert!(pools[0]["error"].as_str().unwrap().contains("claude-z"), "{pools}");
}

#[test]
fn config_load_rejects_bad_pools_by_name() {
    let bad = |pool: Value| {
        let e = Config::from_value(json!({"pools": {"opus": pool}})).unwrap_err();
        format!("{e:#}")
    };
    let base = || json!({"driver": "claudeAgent", "model": OPUS, "members": [{"instanceId": "claude-a"}]});
    let mut p = base();
    p["members"] = json!([]);
    assert!(bad(p).contains("pool opus: a pool needs at least one member"));
    let mut p = base();
    p["members"][0]["reserve"] = json!(101);
    assert!(bad(p).contains("pool opus: member claude-a: reserve must be between 0 and 100"));
    let mut p = base();
    p["members"][0]["reserve"] = json!(-1);
    assert!(bad(p).contains("reserve must be between 0 and 100"));
    let mut p = base();
    p["strategy"] = json!("headroom");
    assert!(bad(p).contains("pool opus: strategy headroom needs a usage source"));
    let mut p = base();
    p["members"] = json!([{"instanceId": "claude-a"}, {"instanceId": "claude-a"}]);
    assert!(bad(p).contains("member claude-a is listed twice"));
    let mut p = base();
    p["members"][0]["wieght"] = json!(2);
    let e = bad(p);
    assert!(e.starts_with("pool opus: unknown field `wieght`"), "{e}");
    let e = Config::from_value(json!({"board": {"pool": "missing"}})).unwrap_err();
    assert!(format!("{e:#}").contains("board.pool names \"missing\""));
    let ok = Config::from_value(json!({"pools": {"opus": base()}, "board": {"pool": "opus"}})).unwrap();
    assert_eq!(ok.pools["opus"].strategy, "weighted");
    assert_eq!(ok.pools["opus"].members[0].weight, 1);
    assert!(Config::default().pools.is_empty() && Config::default().board.pool.is_none());
}

const RESET: &str = "2026-11-17T15:00:00Z";

fn stopped(h: &H, id: &str, instance: &str, status: &str, updated: &str, limit_item: bool) {
    let mut t = thread(id, status, updated);
    t["providerInstanceId"] = json!(instance);
    t["itemCount"] = json!(10);
    h.fake.put(t);
    if limit_item {
        let text = "You've hit your usage limit. Try again later.";
        h.fake.set_items(id, vec![usage_limit_item(&format!("run:{id}:1"), text)]);
    }
}

fn snooze(h: &H, id: &str, until: &str) {
    h.fake.update(id, json!({"snoozed": true, "snoozedUntil": until}));
}

async fn mark(h: &H) -> Vec<String> {
    h.app.wf.mark_limits().await.unwrap()
}

fn reads(h: &H) -> usize {
    h.fake.calls().iter().filter(|c| *c == "t3_thread_read").count()
}

#[tokio::test]
async fn a_thread_stopped_on_a_usage_limit_benches_its_member_until_the_reset() {
    let h = pooled("weighted", two(1, 1));
    stopped(&h, "stop:a", "claude-a", "failed", T0, true);
    snooze(&h, "stop:a", RESET);
    let until = local_clock(RESET);
    assert_eq!(mark(&h).await, [format!("limited claude-a until {until}")]);
    assert_eq!(launches(&h, 1, 4).await, ["claude-b"; 4]);
    let r = pick(&h).await;
    let reason = r["reason"].as_str().unwrap();
    assert!(reason.contains(&format!("claude-a limited until {until}")), "{reason}");
    let pools = result(&h.exec("async () => wf.pools()").await);
    let a = &pools[0]["members"][0];
    assert_eq!(a["state"], "limited");
    assert_eq!(a["limitedUntil"], RESET);
    assert_eq!(a["limitReason"], "usage limit on thread stop:a; reset time from T3's limit snooze");
    assert!(mark(&h).await.is_empty());
    h.clock.set("2026-11-17T15:00:01Z");
    assert_eq!(mark(&h).await, [format!("limit cleared claude-a, reset {until} passed")]);
    assert!(h.app.store.limits().unwrap().is_empty());
    let back = launches(&h, 5, 2).await;
    assert!(back.contains(&"claude-a".to_string()), "{back:?}");
    assert!(mark(&h).await.is_empty());
}

#[tokio::test]
async fn a_snooze_without_a_limit_stop_marks_nothing() {
    let h = pooled("weighted", two(1, 1));
    stopped(&h, "done:a", "claude-a", "completed", T0, true);
    snooze(&h, "done:a", RESET);
    stopped(&h, "error:a", "claude-a", "failed", T0, false);
    snooze(&h, "error:a", RESET);
    h.fake.set_items("error:a", vec![json!({"type": "error", "title": "Provider error", "runId": "run:error:a:1"})]);
    stopped(&h, "rerun:b", "claude-b", "failed", T0, false);
    h.fake.set_items("rerun:b", vec![usage_limit_item("run:rerun:b:0", "an earlier run's limit")]);
    assert!(mark(&h).await.is_empty());
    assert!(h.app.store.limits().unwrap().is_empty());
}

#[tokio::test]
async fn a_limit_stop_without_a_reset_time_holds_the_member_for_limit_hold_minutes() {
    let mut h = pooled("weighted", two(1, 1));
    stopped(&h, "stop:b", "claude-b", "failed", T0, true);
    let held = at(60);
    assert_eq!(mark(&h).await, [format!("limited claude-b until {}", local_clock(&held))]);
    let limits = h.app.store.limits().unwrap();
    assert_eq!(limits["claude-b"].until, held);
    assert_eq!(
        limits["claude-b"].reason.as_deref(),
        Some("usage limit on thread stop:b; reset time unknown, held 60 min")
    );
    snooze(&h, "stop:b", "2026-11-18T09:00:00Z");
    h.reopen();
    assert!(mark(&h).await.is_empty());
    assert_eq!(h.app.store.limits().unwrap()["claude-b"].until, held);
    assert_eq!(reads(&h), 1);
    h.clock.set(&at(61));
    assert_eq!(mark(&h).await.len(), 1);
    assert!(h.app.store.limits().unwrap().is_empty());
    assert_eq!(reads(&h), 1);
    let mut longer = config("weighted", two(1, 1), true);
    longer.limit_hold_minutes = 120;
    h.app = App::with_host(
        longer,
        Arc::new(Store::open(h.dir.path()).unwrap()),
        h.fake.clone(),
        h.clock.clone(),
        h.remotes.clone(),
        h.github.clone(),
        h.desktop.clone(),
    );
    h.clock.set(&at(70));
    assert!(mark(&h).await.is_empty());
    assert!(h.app.store.limits().unwrap().is_empty());
}

#[tokio::test]
async fn limit_marking_skips_other_instances_and_old_stops() {
    let h = pooled("weighted", two(1, 1));
    stopped(&h, "stop:codex", "codex", "failed", T0, true);
    stopped(&h, "stop:old", "claude-b", "failed", "2026-11-15T12:00:00Z", true);
    assert!(mark(&h).await.is_empty());
    assert_eq!(reads(&h), 0);
    let none = H::new();
    stopped(&none, "stop:a", "claude-a", "failed", T0, true);
    assert!(mark(&none).await.is_empty());
    assert!(!none.fake.calls().iter().any(|c| c == "t3_thread_list"));
}

#[tokio::test]
async fn a_later_stop_extends_a_limit_and_an_earlier_one_does_not_shorten_it() {
    let h = pooled("weighted", two(1, 1));
    stopped(&h, "stop:1", "claude-a", "failed", T0, true);
    snooze(&h, "stop:1", RESET);
    mark(&h).await;
    stopped(&h, "stop:2", "claude-a", "failed", T0, true);
    snooze(&h, "stop:2", "2026-11-17T14:00:00Z");
    assert!(mark(&h).await.is_empty());
    assert_eq!(h.app.store.limits().unwrap()["claude-a"].until, RESET);
    stopped(&h, "stop:3", "claude-a", "failed", T0, true);
    snooze(&h, "stop:3", "2026-11-17T16:00:00Z");
    assert_eq!(mark(&h).await.len(), 1);
    assert_eq!(h.app.store.limits().unwrap()["claude-a"].thread.as_deref(), Some("stop:3"));
    h.fake.update("stop:3", json!({"latestRunId": "run:stop:3:2", "snoozed": false, "snoozedUntil": null}));
    h.fake.set_items("stop:3", vec![usage_limit_item("run:stop:3:2", "again")]);
    assert!(mark(&h).await.is_empty());
    assert_eq!(h.app.store.limits().unwrap()["claude-a"].until, "2026-11-17T16:00:00Z");
}

#[tokio::test]
async fn a_reset_that_already_passed_marks_nothing() {
    let h = pooled("weighted", two(1, 1));
    stopped(&h, "stop:a", "claude-a", "failed", T0, true);
    snooze(&h, "stop:a", &at(-1));
    assert!(mark(&h).await.is_empty());
    assert!(h.app.store.limits().unwrap().is_empty());
}

fn usage(h: &H) -> Arc<FakeUsage> {
    let u = Arc::new(FakeUsage::default());
    h.app.wf.set_usage_source(Some(u.clone()));
    u
}

#[tokio::test]
async fn a_usage_source_turns_a_reserve_into_a_floor() {
    let h = pooled("weighted", json!([{"instanceId": "claude-a", "reserve": 30}, {"instanceId": "claude-b"}]));
    let u = usage(&h);
    u.set("claude-a", 50.0, None);
    let picks = launches(&h, 1, 4).await;
    assert_eq!(tally(&picks, "claude-a"), 2, "{picks:?}");
    u.set("claude-a", 20.0, None);
    let r = pick(&h).await;
    assert_eq!(r["instanceId"], "claude-b");
    assert_eq!(r["skipped"], json!([{"instanceId": "claude-a", "reason": "held at its 30% reserve with 20% left"}]));
    limit(&h, "claude-b", RESET);
    let out = h.exec("async () => wf.pick({ pool: 'opus' })").await;
    assert!(err_text(&out).contains("claude-a held at its 30% reserve with 20% left"), "{}", out.text);
    let pools = result(&h.exec("async () => wf.pools()").await);
    assert_eq!(pools[0]["members"][0]["usageLeft"], 20.0);
}

#[tokio::test]
async fn a_usage_source_with_nothing_left_limits_the_member() {
    let h = pooled("weighted", two(1, 1));
    let u = usage(&h);
    u.set("claude-a", 0.0, Some(RESET));
    let r = pick(&h).await;
    assert_eq!(r["instanceId"], "claude-b");
    assert!(r["reason"].as_str().unwrap().contains(&format!("claude-a limited until {}", local_clock(RESET))));
    assert_eq!(mark(&h).await, [format!("limited claude-a until {}", local_clock(RESET))]);
    let limits = h.app.store.limits().unwrap();
    assert_eq!(limits["claude-a"].reason.as_deref(), Some("the usage source reports no usage left"));
    u.set("claude-b", 0.0, None);
    let out = h.exec("async () => wf.pick({ pool: 'opus' })").await;
    assert!(err_text(&out).contains("claude-b has no usage left"), "{}", out.text);
    u.set_fails(true);
    h.app.store.limit_clear("claude-a").unwrap();
    assert_eq!(pick(&h).await["instanceId"], "claude-a");
    u.set_fails(false);
    u.set("claude-a", 0.0, Some(&at(-60)));
    u.set("claude-b", 0.0, Some(&at(-60)));
    let r = pick(&h).await;
    assert!(r["skipped"].as_array().is_none_or(|s| s.is_empty()), "{r}");
    assert!(mark(&h).await.is_empty());
}

fn headroom_choice(usage: &[(&str, f64)], credits: &BTreeMap<String, i64>) -> (String, String, BTreeMap<String, i64>) {
    let pool = PoolConfig {
        mixed: false,
        driver: Some("claudeAgent".into()),
        model: Some(OPUS.into()),
        effort: None,
        strategy: "headroom".into(),
        members: ["claude-a", "claude-b", "claude-c"]
            .iter()
            .map(|id| PoolMember {
                instance_id: id.to_string(),
                weight: 1,
                reserve: 0.0,
                cost: 1.0,
                model: None,
                effort: None,
            })
            .collect(),
    };
    let providers: Vec<Value> =
        ["claude-a", "claude-b", "claude-c"].map(|id| provider(id, "claudeAgent", &[OPUS])).into();
    let caps = json!({ "providers": providers });
    let usage: BTreeMap<String, Usage> =
        usage.iter().map(|(id, left)| (id.to_string(), Usage { left: *left, resets_at: None })).collect();
    let c = choose(&Inputs {
        name: "opus",
        pool: &pool,
        caps: &caps,
        running: &BTreeMap::new(),
        limits: &BTreeMap::new(),
        usage: &usage,
        credits,
        now: "2026-11-17T12:00:00Z".parse().unwrap(),
    })
    .unwrap();
    (c.instance_id, c.reason, c.credits)
}

#[test]
fn headroom_picks_the_most_room_and_breaks_ties_by_weight() {
    let none = BTreeMap::new();
    let (id, reason, _) = headroom_choice(&[("claude-a", 40.0), ("claude-b", 62.0), ("claude-c", 10.0)], &none);
    let ran = format!("claudeAgent {OPUS}, effort default; ");
    assert_eq!((id.as_str(), reason.as_str()), ("claude-b", format!("{ran}headroom: most left, 62%").as_str()));
    let tie = [("claude-a", 70.0), ("claude-b", 70.0), ("claude-c", 10.0)];
    let (first, _, credits) = headroom_choice(&tie, &none);
    let (second, _, _) = headroom_choice(&tie, &credits);
    assert_eq!([first.as_str(), second.as_str()], ["claude-a", "claude-b"]);
    let (_, reason, _) = headroom_choice(&[], &none);
    assert_eq!(reason, format!("{ran}headroom: no member reports usage, so weighted: weight 1 of 3"));
}

#[test]
fn limit_hold_minutes_must_be_positive() {
    for bad in [0, 10081, u64::MAX] {
        let e = Config::from_value(json!({"limitHoldMinutes": bad})).unwrap_err();
        assert!(format!("{e:#}").contains("limitHoldMinutes must be between 1 and 10080"), "{e:#}");
    }
    assert_eq!(Config::from_value(json!({"limitHoldMinutes": 10080})).unwrap().limit_hold_minutes, 10080);
    assert_eq!(Config::default().limit_hold_minutes, 60);
}

const HIGH: &str = "claudeAgent claude-opus-5-5, effort high; ";
const DEFAULT: &str = "claudeAgent claude-opus-5-5, effort default; ";

fn costed(costs: &[(&str, f64)]) -> Value {
    costs.iter().map(|(id, cost)| json!({"instanceId": id, "cost": cost})).collect()
}

#[tokio::test]
async fn cheapest_sends_every_launch_to_the_cheapest_ready_member() {
    let h = pooled("cheapest", costed(&[("claude-a", 2.0), ("claude-b", 0.0)]));
    assert_eq!(launches(&h, 1, 4).await, ["claude-b"; 4]);
    let r = pick(&h).await;
    assert_eq!(r["reason"], format!("{HIGH}cheapest: cost 0, weighted: weight 1 of 1"));
    limit(&h, "claude-b", "2026-11-17T21:00:00Z");
    let r = launch(&h, 5).await;
    assert_eq!(r["instanceId"], "claude-a");
    let reason = r["reason"].as_str().unwrap();
    let until = local_clock("2026-11-17T21:00:00Z");
    assert_eq!(
        reason,
        format!("{HIGH}cheapest: cost 2, weighted: weight 1 of 1; skipped claude-b limited until {until}")
    );
    h.clock.set("2026-11-17T21:00:01Z");
    assert_eq!(pick(&h).await["instanceId"], "claude-b");
}

#[tokio::test]
async fn cheapest_spreads_by_weight_among_members_of_the_same_cost() {
    let h = pooled(
        "cheapest",
        json!([{"instanceId": "claude-a", "cost": 0.5}, {"instanceId": "claude-b", "cost": 0.5, "weight": 3},
               {"instanceId": "claude-d", "cost": 0.25, "weight": 9}]),
    );
    h.fake.set_providers(vec![
        provider("claude-a", "claudeAgent", &[OPUS]),
        provider("claude-b", "claudeAgent", &[OPUS]),
        provider("claude-d", "claudeAgent", &["claude-haiku-5-5"]),
    ]);
    let picks = launches(&h, 1, 8).await;
    assert_eq!((tally(&picks, "claude-a"), tally(&picks, "claude-b")), (2, 6), "{picks:?}");
    let r = pick(&h).await;
    let reason = r["reason"].as_str().unwrap();
    assert!(reason.starts_with(&format!("{HIGH}cheapest: cost 0.5, weighted: weight ")), "{reason}");
    assert!(reason.ends_with(" of 4; skipped claude-d does not list model claude-opus-5-5"), "{reason}");
}

#[tokio::test]
async fn a_cheap_member_held_at_its_reserve_never_wins_over_a_ready_one() {
    let h =
        pooled("cheapest", json!([{"instanceId": "claude-a", "cost": 0, "reserve": 30}, {"instanceId": "claude-b"}]));
    assert_eq!(launches(&h, 1, 3).await, ["claude-b"; 3]);
    let r = pick(&h).await;
    assert_eq!(
        r["reason"],
        format!(
            "{HIGH}cheapest: cost 1, weighted: weight 1 of 1; skipped claude-a held at its 30% reserve until every other member is out"
        )
    );
    let u = usage(&h);
    u.set("claude-a", 20.0, None);
    let r = pick(&h).await;
    assert_eq!(r["instanceId"], "claude-b");
    assert_eq!(r["skipped"], json!([{"instanceId": "claude-a", "reason": "held at its 30% reserve with 20% left"}]));
    u.set("claude-a", 50.0, None);
    assert_eq!(pick(&h).await["instanceId"], "claude-a");
    u.set("claude-a", 20.0, None);
    limit(&h, "claude-b", "2026-11-17T21:00:00Z");
    let out = h.exec("async () => wf.pick({ pool: 'opus' })").await;
    assert!(err_text(&out).contains("claude-a held at its 30% reserve with 20% left"), "{}", out.text);
}

#[tokio::test]
async fn a_cheap_member_with_a_reserve_is_still_the_last_resort() {
    let h =
        pooled("cheapest", json!([{"instanceId": "claude-a", "cost": 0, "reserve": 30}, {"instanceId": "claude-b"}]));
    limit(&h, "claude-b", "2026-11-17T21:00:00Z");
    let r = pick(&h).await;
    assert_eq!(r["instanceId"], "claude-a");
    let reason = r["reason"].as_str().unwrap();
    assert!(
        reason.starts_with(&format!(
            "{HIGH}last resort: claude-a holds a 30% reserve and every other member is out; cheapest: cost 0"
        )),
        "{reason}"
    );
}

#[tokio::test]
async fn weighted_and_priority_ignore_cost() {
    let members = costed(&[("claude-a", 5.0), ("claude-b", 0.0)]);
    let h = pooled("weighted", members.clone());
    let picks = launches(&h, 1, 4).await;
    assert_eq!((tally(&picks, "claude-a"), tally(&picks, "claude-b")), (2, 2), "{picks:?}");
    let h = pooled("priority", members);
    assert_eq!(launches(&h, 1, 2).await, ["claude-a"; 2]);
}

#[tokio::test]
async fn wf_pools_shows_each_members_cost() {
    let h = pooled("cheapest", json!([{"instanceId": "claude-a", "cost": 0}, {"instanceId": "claude-b"}]));
    let pools = result(&h.exec("async () => wf.pools()").await);
    assert_eq!(pools[0]["strategy"], "cheapest");
    let costs: Vec<f64> = pools[0]["members"].as_array().unwrap().iter().map(|m| m["cost"].as_f64().unwrap()).collect();
    assert_eq!(costs, [0.0, 1.0]);
}

#[test]
fn config_load_rejects_a_bad_cost_by_member() {
    let pool =
        |members: Value| json!({"pools": {"opus": {"driver": "claudeAgent", "model": OPUS, "members": members}}});
    let e = Config::from_value(pool(json!([{"instanceId": "claude-a", "cost": -0.5}]))).unwrap_err();
    assert!(
        format!("{e:#}").contains("pool opus: member claude-a: cost must be a finite number of 0 or more, got -0.5"),
        "{e:#}"
    );
    let e = Config::from_value(pool(json!([{"instanceId": "claude-a", "cost": "free"}]))).unwrap_err();
    assert!(format!("{e:#}").starts_with("pool opus: invalid type"), "{e:#}");
    let mut p = Config::from_value(pool(json!([{"instanceId": "claude-a"}]))).unwrap().pools["opus"].clone();
    assert_eq!(p.members[0].cost, 1.0);
    for bad in [f64::INFINITY, f64::NAN] {
        p.members[0].cost = bad;
        assert!(format!("{:#}", p.validate().unwrap_err()).contains("member claude-a: cost must be a finite number"));
    }
    let e = Config::from_value(json!({"pools": {"opus": {"driver": "claudeAgent", "model": OPUS, "strategy": "cheap",
                                                          "members": [{"instanceId": "claude-a"}]}}}))
    .unwrap_err();
    assert!(format!("{e:#}").contains("unknown strategy \"cheap\"; use one of weighted, priority, cheapest"), "{e:#}");
}

fn cheapest_choice(
    costs: &[f64],
    limited: &[&str],
    credits: &BTreeMap<String, i64>,
) -> (String, String, BTreeMap<String, i64>) {
    let ids = ["claude-a", "claude-b", "claude-c"];
    let pool = PoolConfig {
        mixed: false,
        driver: Some("claudeAgent".into()),
        model: Some(OPUS.into()),
        effort: None,
        strategy: "cheapest".into(),
        members: ids
            .iter()
            .zip(costs)
            .map(|(id, cost)| PoolMember {
                instance_id: id.to_string(),
                weight: 1,
                reserve: 0.0,
                cost: *cost,
                model: None,
                effort: None,
            })
            .collect(),
    };
    let providers: Vec<Value> = ids.map(|id| provider(id, "claudeAgent", &[OPUS])).into();
    let caps = json!({ "providers": providers });
    let limits: BTreeMap<String, Limit> = limited
        .iter()
        .map(|id| {
            (id.to_string(), Limit { until: "2026-11-17T21:00:00Z".into(), reason: None, at: T0.into(), thread: None })
        })
        .collect();
    let c = choose(&Inputs {
        name: "opus",
        pool: &pool,
        caps: &caps,
        running: &BTreeMap::new(),
        limits: &limits,
        usage: &BTreeMap::new(),
        credits,
        now: "2026-11-17T12:00:00Z".parse().unwrap(),
    })
    .unwrap();
    (c.instance_id, c.reason, c.credits)
}

#[test]
fn cheapest_takes_the_lowest_cost_group_and_moves_up_only_when_it_is_out() {
    let none = BTreeMap::new();
    let costs = [0.0, 0.0, 3.0];
    let (first, reason, credits) = cheapest_choice(&costs, &[], &none);
    assert_eq!((first.as_str(), reason), ("claude-a", format!("{DEFAULT}cheapest: cost 0, weighted: weight 1 of 2")));
    let (second, _, credits) = cheapest_choice(&costs, &[], &credits);
    assert_eq!(second, "claude-b");
    let (one_out, _, _) = cheapest_choice(&costs, &["claude-a"], &credits);
    assert_eq!(one_out, "claude-b");
    let (id, reason, _) = cheapest_choice(&costs, &["claude-a", "claude-b"], &credits);
    let until = local_clock("2026-11-17T21:00:00Z");
    assert_eq!(id, "claude-c");
    assert_eq!(
        reason,
        format!(
            "{DEFAULT}cheapest: cost 3, weighted: weight 1 of 1; skipped claude-a limited until {until}, claude-b limited until {until}"
        )
    );
    let (id, _, _) = cheapest_choice(&[2.0, 1.5, 1.0], &[], &none);
    assert_eq!(id, "claude-c");
}

const ASTRA: &str = "gpt-6-astra";

fn codex(id: &str) -> Value {
    let mut p = provider(id, "codex", &[ASTRA]);
    p["models"][0]["options"] = json!([{"id": "reasoningEffort", "values": ["low", "medium", "xhigh"]}]);
    p
}

fn mixed(members: Value, board_pool: bool) -> H {
    let mut c = json!({"pools": {"frontier": {"mixed": true, "members": members}}});
    if board_pool {
        c["board"] = json!({"pool": "frontier"});
    }
    let h = H::build(Vec::new(), Config::from_value(c).unwrap());
    h.fake.set_providers(vec![
        provider("claude-a", "claudeAgent", &[OPUS, "claude-haiku-5-5"]),
        codex("codex-a"),
        codex("codex-b"),
    ]);
    h.fake.set_projects(vec![project("p-widgets", "/src/widgets")]);
    h.remotes.set("/src/widgets", "git@github.com:acme/widgets.git");
    h
}

fn claude_and_codex() -> Value {
    json!([{"instanceId": "claude-a", "model": OPUS, "effort": "high"},
           {"instanceId": "codex-a", "model": ASTRA, "effort": "xhigh"}])
}

async fn pick_frontier(h: &H) -> Value {
    result(&h.exec("async () => wf.pick({ pool: 'frontier' })").await)
}

#[tokio::test]
async fn a_mixed_pool_picks_each_member_with_its_own_model_and_effort() {
    let h = mixed(claude_and_codex(), false);
    let mut picks = BTreeMap::new();
    for _ in 0..2 {
        let r = pick_frontier(&h).await;
        picks.insert(r["instanceId"].as_str().unwrap().to_string(), r);
    }
    let claude = &picks["claude-a"];
    assert_eq!(
        (&claude["driver"], &claude["model"], &claude["effort"]),
        (&json!("claudeAgent"), &json!(OPUS), &json!("high"))
    );
    assert_eq!(
        claude["modelSelection"],
        json!({"instanceId": "claude-a", "model": OPUS, "options": [{"id": "effort", "value": "high"}]})
    );
    assert!(
        claude["reason"].as_str().unwrap().starts_with(&format!("claudeAgent {OPUS}, effort high; weighted")),
        "{claude}"
    );
    let codex = &picks["codex-a"];
    assert_eq!(
        (&codex["driver"], &codex["model"], &codex["effort"]),
        (&json!("codex"), &json!(ASTRA), &json!("xhigh"))
    );
    assert_eq!(
        codex["modelSelection"],
        json!({"instanceId": "codex-a", "model": ASTRA, "options": [{"id": "reasoningEffort", "value": "xhigh"}]})
    );
    assert!(
        codex["reason"].as_str().unwrap().starts_with(&format!("codex {ASTRA}, effort xhigh; weighted")),
        "{codex}"
    );
}

#[tokio::test]
async fn a_mixed_member_is_checked_against_its_own_model_and_effort() {
    let h = mixed(
        json!([{"instanceId": "claude-a", "model": ASTRA},
               {"instanceId": "codex-a", "model": ASTRA, "effort": "high"},
               {"instanceId": "codex-b", "model": ASTRA}]),
        false,
    );
    let r = pick_frontier(&h).await;
    assert_eq!(r["instanceId"], "codex-b");
    assert_eq!((&r["effort"], &r["options"]), (&Value::Null, &json!([])));
    assert_eq!(r["modelSelection"], json!({"instanceId": "codex-b", "model": ASTRA}));
    assert!(r["reason"].as_str().unwrap().starts_with(&format!("codex {ASTRA}, effort default; ")), "{r}");
    assert_eq!(
        r["skipped"],
        json!([{"instanceId": "claude-a", "reason": format!("does not list model {ASTRA}")},
               {"instanceId": "codex-a", "reason": format!("does not offer effort high for {ASTRA}")}])
    );
}

#[tokio::test]
async fn wf_pools_shows_each_members_driver_model_and_effort() {
    let h = mixed(claude_and_codex(), false);
    let pools = result(&h.exec("async () => wf.pools()").await);
    let p = &pools[0];
    assert_eq!((&p["pool"], &p["mixed"], &p["error"]), (&json!("frontier"), &json!(true), &Value::Null));
    assert_eq!((&p["driver"], &p["model"], &p["effort"]), (&Value::Null, &Value::Null, &Value::Null));
    let seen: Vec<(Value, Value, Value, Value)> = p["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| (m["driver"].clone(), m["model"].clone(), m["effort"].clone(), m["state"].clone()))
        .collect();
    assert_eq!(
        seen,
        [
            (json!("claudeAgent"), json!(OPUS), json!("high"), json!("ready")),
            (json!("codex"), json!(ASTRA), json!("xhigh"), json!("ready")),
        ]
    );
    let same = pooled("weighted", two(1, 1));
    let pools = result(&same.exec("async () => wf.pools()").await);
    let m = &pools[0]["members"][1];
    assert_eq!(pools[0]["mixed"], false);
    assert_eq!((&m["driver"], &m["model"], &m["effort"]), (&json!("claudeAgent"), &json!(OPUS), &json!("high")));
}

#[tokio::test]
async fn a_mixed_board_pool_launches_on_the_members_model_and_refuses_one_named() {
    let h = mixed(claude_and_codex(), true);
    let r = launch(&h, 1).await;
    let id = r["instanceId"].as_str().unwrap();
    let model = if id == "claude-a" { OPUS } else { ASTRA };
    assert_eq!(h.fake.launched()[0]["modelSelection"]["model"], model);
    let out = h.exec(&format!("async () => wf.launchTicket({{ issue: 'acme/widgets#2', model: '{OPUS}' }})")).await;
    assert!(err_text(&out).contains("board.pool frontier is a mixed pool"), "{}", out.text);
    let out = h.exec("async () => wf.launchTicket({ issue: 'acme/widgets#3', effort: 'high' })").await;
    assert!(err_text(&out).contains("unset board.pool to launch effort high"), "{}", out.text);
    assert_eq!(h.fake.launched().len(), 1);
}

#[test]
fn config_load_keeps_mixed_and_single_driver_pools_apart() {
    let bad = |pool: Value| format!("{:#}", Config::from_value(json!({"pools": {"p": pool}})).unwrap_err());
    let single = json!({"driver": "claudeAgent", "model": OPUS,
                        "members": [{"instanceId": "claude-a", "model": "claude-haiku-5-5"}]});
    assert!(bad(single).contains("pool p: member claude-a: only a mixed pool's members set their own model"));
    let single =
        json!({"driver": "claudeAgent", "model": OPUS, "members": [{"instanceId": "claude-a", "effort": "low"}]});
    assert!(bad(single).contains("only a mixed pool's members set their own effort"));
    let e = bad(json!({"model": OPUS, "members": [{"instanceId": "claude-a"}]}));
    assert!(e.contains("pool p: driver is missing; set it, or set \"mixed\": true"), "{e}");
    for key in ["driver", "model", "effort"] {
        let mut p = json!({"mixed": true, "members": [{"instanceId": "claude-a", "model": OPUS}]});
        p[key] = json!("x");
        assert!(bad(p).contains(&format!("pool p: a mixed pool has no pool-level {key}")));
    }
    let e = bad(json!({"mixed": true, "members": [{"instanceId": "claude-a", "effort": "high"}]}));
    assert!(e.contains("pool p: member claude-a: a mixed pool's member needs a model"), "{e}");
    let ok = Config::from_value(json!({"pools": {"p": {"mixed": true, "members": claude_and_codex()}}})).unwrap();
    let p = &ok.pools["p"];
    assert_eq!((p.model_of(&p.members[1]), p.effort_of(&p.members[1])), (ASTRA, Some("xhigh")));
}

#[tokio::test]
async fn a_single_driver_pool_still_rejects_another_driver_beside_a_mixed_one() {
    let c = json!({"pools": {
        "frontier": {"mixed": true, "members": claude_and_codex()},
        "opus": {"driver": "claudeAgent", "model": OPUS, "members": [{"instanceId": "claude-a"}, {"instanceId": "codex-a"}]}
    }});
    let h = H::build(Vec::new(), Config::from_value(c).unwrap());
    h.fake.set_providers(vec![provider("claude-a", "claudeAgent", &[OPUS]), codex("codex-a")]);
    let out = h.exec("async () => wf.pick({ pool: 'opus' })").await;
    assert!(err_text(&out).contains("pool opus: member codex-a is a codex instance, not claudeAgent"), "{}", out.text);
    assert_eq!(pick_frontier(&h).await["pool"], "frontier");
    let e = format!("{:#}", h.app.wf.check_pools().await.unwrap_err());
    assert!(e.contains("pool opus: member codex-a"), "{e}");
}

#[tokio::test]
async fn a_mixed_pool_finds_a_member_model_beyond_t3s_short_list() {
    let h = mixed(json!([{"instanceId": "claude-a", "model": "claude-haiku-5-5"}]), false);
    h.fake.set_short_list(1);
    let r = pick_frontier(&h).await;
    assert_eq!((&r["instanceId"], &r["model"]), (&json!("claude-a"), &json!("claude-haiku-5-5")));
    let pools = result(&h.exec("async () => wf.pools()").await);
    assert_eq!(pools[0]["members"][0]["state"], "ready");
}

#[tokio::test]
async fn a_mixed_cheapest_pool_sends_launches_to_the_cheapest_member_on_its_own_model() {
    let c = json!({"pools": {"cheap": {"mixed": true, "strategy": "cheapest", "members": [
        {"instanceId": "claude-a", "model": "claude-haiku-5-5", "cost": 2},
        {"instanceId": "codex-a", "model": ASTRA, "effort": "low", "cost": 0.5}
    ]}}});
    let h = H::build(Vec::new(), Config::from_value(c).unwrap());
    h.fake.set_providers(vec![provider("claude-a", "claudeAgent", &[OPUS, "claude-haiku-5-5"]), codex("codex-a")]);
    let pick = || async { result(&h.exec("async () => wf.pick({ pool: 'cheap' })").await) };
    let r = pick().await;
    assert_eq!(
        r["modelSelection"],
        json!({"instanceId": "codex-a", "model": ASTRA, "options": [{"id": "reasoningEffort", "value": "low"}]})
    );
    assert_eq!(r["reason"], format!("codex {ASTRA}, effort low; cheapest: cost 0.5, weighted: weight 1 of 1"));
    limit(&h, "codex-a", "2026-11-17T21:00:00Z");
    let r = pick().await;
    assert_eq!(r["modelSelection"], json!({"instanceId": "claude-a", "model": "claude-haiku-5-5"}));
    assert!(
        r["reason"].as_str().unwrap().starts_with("claudeAgent claude-haiku-5-5, effort default; cheapest: cost 2"),
        "{r}"
    );
}

async fn warnings(h: &H) -> Vec<String> {
    let pools = result(&h.exec("async () => wf.pools()").await);
    serde_json::from_value(pools[0]["warnings"].clone()).unwrap()
}

#[tokio::test]
async fn wf_pools_warns_when_only_one_member_can_run_the_model() {
    let h = pooled("weighted", json!([{"instanceId": "claude-b"}, {"instanceId": "claude-c"}]));
    assert_eq!(
        warnings(&h).await,
        [format!(
            "only claude-b can run {OPUS}, so the pool launches nothing while claude-b is out. \
             Set a reserve on claude-b to keep room for the work only it can run."
        )]
    );
}

#[tokio::test]
async fn wf_pools_stays_quiet_when_two_members_offer_the_model() {
    let h =
        pooled("weighted", json!([{"instanceId": "claude-a"}, {"instanceId": "claude-b"}, {"instanceId": "claude-c"}]));
    assert!(warnings(&h).await.is_empty());
}

#[tokio::test]
async fn a_sole_member_with_a_reserve_is_flagged_without_the_suggestion() {
    let h = pooled("weighted", json!([{"instanceId": "claude-b", "reserve": 20}, {"instanceId": "claude-off"}]));
    assert_eq!(
        warnings(&h).await,
        [format!("only claude-b can run {OPUS}, so the pool launches nothing while claude-b is out.")]
    );
}

#[tokio::test]
async fn a_pool_with_a_bad_member_reports_the_error_and_no_warnings() {
    let h = pooled("weighted", json!([{"instanceId": "claude-b"}, {"instanceId": "codex"}]));
    let pools = result(&h.exec("async () => wf.pools()").await);
    assert!(pools[0]["error"].as_str().unwrap().contains("codex is a codex instance"), "{pools}");
    assert_eq!(pools[0]["warnings"], json!([]));
}

#[tokio::test]
async fn a_mixed_pool_warns_about_the_only_member_of_a_driver() {
    let h = mixed(
        json!([{"instanceId": "claude-a", "model": OPUS, "effort": "high"},
               {"instanceId": "codex-a", "model": ASTRA, "effort": "xhigh"},
               {"instanceId": "codex-b", "model": ASTRA, "effort": "low"}]),
        false,
    );
    assert_eq!(
        warnings(&h).await,
        [format!(
            "claude-a is the only claudeAgent member that can run its model, {OPUS}, so the pool runs no \
             claudeAgent launch while claude-a is out. Set a reserve on claude-a to keep room for the work only it can run."
        )]
    );
}

#[tokio::test]
async fn a_mixed_pool_counts_only_members_that_can_run_their_model() {
    let h = mixed(
        json!([{"instanceId": "codex-a", "model": ASTRA, "effort": "xhigh"},
               {"instanceId": "codex-b", "model": ASTRA, "effort": "high"}]),
        false,
    );
    let w = warnings(&h).await;
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].starts_with("codex-a is the only codex member"), "{w:?}");
}

fn limit_stop_on(h: &H, id: &str) {
    h.fake.update(id, json!({"status": "failed", "itemCount": 10, "updatedAt": T0}));
    let text = "You've hit your usage limit. Try again later.";
    h.fake.set_items(id, vec![usage_limit_item(&format!("run:{id}:1"), text)]);
}

fn move_cards(h: &H) -> Vec<t3_codemode::board::Card> {
    h.app.wf.cards().unwrap().into_iter().filter(|c| c.offer.is_some()).collect()
}

#[tokio::test]
async fn a_limited_pool_ticket_moves_to_another_ready_member_on_one_card() {
    let h = pooled("weighted", two(1, 1));
    let ticket = launch(&h, 41).await;
    let (old, first) = (ticket["thread"].as_str().unwrap().to_string(), ticket["instanceId"].as_str().unwrap());
    let other = if first == "claude-a" { "claude-b" } else { "claude-a" };
    h.wf(&old, "join", json!({"parent": BOARD, "label": "widgets#41", "role": "ticket"})).await;
    let worker = h.wf(&old, "openChild", json!({"role": "worker", "label": "widgets#41 PR 1/1"})).await;
    assert!(worker["brief"].is_string(), "{worker}");
    h.wf("mcp:worker-41", "join", json!({"parent": old, "label": "widgets#41 PR 1/1", "role": "worker"})).await;
    h.wf(&old, "send", json!({"kind": "gate", "gate": "plan", "body": "**ACTION NEEDED:** reply \"go\" to start"}))
        .await;
    let running = launch(&h, 42).await["thread"].as_str().unwrap().to_string();
    let direct = h
        .exec(&format!(
            "async () => t3.call('t3_thread_launch', {{ projectId: 'p-widgets', title: 'direct', message: 'm',
               modelSelection: {{ instanceId: '{first}', model: '{OPUS}' }} }})"
        ))
        .await;
    let direct = result(&direct)["threadId"].as_str().unwrap().to_string();
    limit_stop_on(&h, &old);
    limit_stop_on(&h, &direct);
    limit_stop_on(&h, &running);
    h.fake.update(&running, json!({"status": "running"}));

    let actions = mark(&h).await;
    let cards = move_cards(&h);
    assert_eq!(cards.len(), 1, "{actions:?} {cards:?}");
    let card = &cards[0];
    assert_eq!(card.ticket.as_deref(), Some(old.as_str()));
    assert_eq!(card.label.as_deref(), Some("widgets#41"));
    assert_eq!(card.options, ["move", "wait"]);
    assert!(card.text.contains(&format!("widgets#41 stopped on a usage limit on {first}")), "{}", card.text);
    assert!(
        actions.iter().any(|a| a == &format!("offered to move widgets#41 off {first} as {}", card.id)),
        "{actions:?}"
    );
    mark(&h).await;
    assert_eq!(move_cards(&h).len(), 1);

    let moved = h.app.wf.answer(&card.id, "move").await.unwrap();
    let fork = moved["thread"].as_str().unwrap().to_string();
    assert_eq!(moved["instanceId"], other, "{moved}");
    assert_eq!(moved["from"], old.as_str());
    let f = h.fake.thread(&fork).unwrap();
    assert_eq!(f["forkedFrom"], old.as_str());
    assert_eq!(f["providerInstanceId"], other);
    assert_eq!(f["model"], OPUS);
    assert_eq!(
        h.fake.configured().last().unwrap()["modelSelection"],
        json!({"instanceId": other, "model": OPUS, "options": [{"id": "effort", "value": "high"}]})
    );

    let ln = h.app.store.link_get(&fork).unwrap().expect("the ticket link moved to the fork");
    assert_eq!((ln.label.as_str(), ln.parent.as_str(), ln.closed), ("widgets#41", BOARD, false));
    assert!(h.app.store.link_get(&old).unwrap().is_none());
    assert_eq!(h.app.store.link_get("mcp:worker-41").unwrap().unwrap().parent, fork);
    let gate = h.app.wf.cards().unwrap().into_iter().find(|c| c.offer.is_none() && c.text.contains("\"go\"")).unwrap();
    assert_eq!(gate.ticket.as_deref(), Some(fork.as_str()), "{gate:?}");
    assert_eq!(gate.link.as_deref(), Some(format!("https://t3/{fork}").as_str()), "{gate:?}");

    let pick = &h.app.store.picks("opus", 1).unwrap()[0];
    assert_eq!(pick.instance_id, other);
    assert_eq!(pick.thread.as_deref(), Some(fork.as_str()));
    assert_eq!(pick.moved_from.as_deref(), Some(old.as_str()));
    assert!(pick.reason.contains(&format!("skipped {first} is the member the thread stopped on")), "{}", pick.reason);
    let feed = h.app.wf.feed(20).unwrap();
    let entry = feed.iter().find(|f| f["head"].as_str().unwrap().starts_with("moved ")).expect("a move entry");
    assert_eq!(entry["instanceId"], other);
    assert!(entry["head"].as_str().unwrap().contains(&format!("from {first} to {fork}")), "{entry}");

    let resume = h.sent().into_iter().find(|s| s["threadId"] == fork.as_str()).expect("the fork was resumed");
    assert!(resume["message"].as_str().unwrap().contains(&format!("now runs on {other}")), "{resume}");
    assert!(h.fake.organized().iter().any(|o| o["threadId"] == old.as_str() && o["action"] == "settle"));
    assert!(move_cards(&h).is_empty());
    assert!(
        h.app
            .wf
            .cards_with(true)
            .unwrap()
            .iter()
            .any(|c| c.id == card.id && c.resolution.as_deref().is_some_and(|r| r.starts_with("answered: move")))
    );
    mark(&h).await;
    assert!(move_cards(&h).is_empty());
}

#[tokio::test]
async fn a_worker_reported_with_wf_picked_is_offered_and_wait_leaves_it() {
    let h = pooled("priority", two(1, 1));
    let launched = h
        .exec(
            "async () => { const p = await wf.pick({ pool: 'opus' });
               const r = await t3.call('t3_thread_launch', { projectId: 'p-widgets', title: 'worker', message: 'm',
                 modelSelection: p.modelSelection });
               return wf.picked({ pick: p.pick, thread: r.threadId }); }",
        )
        .await;
    let r = result(&launched);
    let thread = r["thread"].as_str().unwrap().to_string();
    assert_eq!((r["pool"].as_str(), r["instanceId"].as_str()), (Some("opus"), Some("claude-a")), "{r}");
    let again = h.wf_err(&thread, "picked", json!({"pick": r["pick"], "thread": "launch:99"})).await;
    assert!(again.contains(&format!("already launched thread {thread}")), "{again}");
    assert!(h.wf_err(&thread, "picked", json!({"pick": 999, "thread": thread})).await.contains("no pick 999"));

    limit_stop_on(&h, &thread);
    mark(&h).await;
    let cards = move_cards(&h);
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0].label.as_deref(), Some("worker"));
    let e = format!("{:#}", h.app.wf.answer(&cards[0].id, "later").await.unwrap_err());
    assert!(e.contains("takes \"move\" or \"wait\""), "{e}");
    let w = h.app.wf.answer(&cards[0].id, "wait").await.unwrap();
    assert_eq!(w["moved"], false);
    assert!(move_cards(&h).is_empty());
    assert!(h.fake.configured().is_empty());
    mark(&h).await;
    assert!(move_cards(&h).is_empty());
}

#[tokio::test]
async fn a_thread_that_ran_again_is_not_moved_and_its_card_goes() {
    let h = pooled("weighted", two(1, 1));
    let old = launch(&h, 43).await["thread"].as_str().unwrap().to_string();
    limit_stop_on(&h, &old);
    mark(&h).await;
    let id = move_cards(&h)[0].id.clone();
    h.fake.update(&old, json!({"status": "running", "activeRunId": format!("run:{old}:2")}));
    let e = format!("{:#}", h.app.wf.answer(&id, "move").await.unwrap_err());
    assert!(e.contains("ran again after its usage limit, so nothing was moved"), "{e}");
    assert!(h.fake.configured().is_empty());
    mark(&h).await;
    assert!(move_cards(&h).is_empty());
}

#[tokio::test]
async fn a_move_in_a_mixed_pool_keeps_the_thread_on_its_driver() {
    let members = json!([{"instanceId": "codex-a", "model": ASTRA, "effort": "xhigh"},
                         {"instanceId": "claude-a", "model": OPUS, "effort": "high"},
                         {"instanceId": "codex-b", "model": ASTRA, "effort": "xhigh"}]);
    let c = json!({"pools": {"frontier": {"mixed": true, "strategy": "priority", "members": members}},
                   "board": {"pool": "frontier"}});
    let h = H::build(Vec::new(), Config::from_value(c).unwrap());
    h.fake.set_providers(vec![
        codex("codex-a"),
        provider("claude-a", "claudeAgent", &[OPUS, "claude-haiku-5-5"]),
        codex("codex-b"),
    ]);
    h.fake.set_projects(vec![project("p-widgets", "/src/widgets")]);
    h.remotes.set("/src/widgets", "git@github.com:acme/widgets.git");
    let ticket = launch(&h, 44).await;
    assert_eq!(ticket["instanceId"], "codex-a", "{ticket}");
    let old = ticket["thread"].as_str().unwrap().to_string();
    limit_stop_on(&h, &old);
    mark(&h).await;
    let id = move_cards(&h)[0].id.clone();
    let out = h.app.wf.answer(&id, "move").await.unwrap();
    assert_eq!(out["instanceId"], "codex-b", "{out}");
    let reason = out["reason"].as_str().unwrap();
    assert!(reason.contains("claude-a is a claudeAgent instance, not codex like the thread"), "{reason}");
    assert!(reason.contains("codex-a is the member the thread stopped on"), "{reason}");
}

#[tokio::test]
async fn a_ticket_limited_before_it_joins_moves_its_launch_row_and_a_board_question_gets_no_card() {
    let h = pooled("priority", two(1, 1));
    let old = launch(&h, 45).await["thread"].as_str().unwrap().to_string();
    let ask = h.app.wf.ask("Which tickets wait on me?").await.unwrap();
    let ask = ask["thread"].as_str().unwrap().to_string();
    limit_stop_on(&h, &old);
    limit_stop_on(&h, &ask);
    mark(&h).await;
    let cards = move_cards(&h);
    assert_eq!(cards.len(), 1, "{cards:?}");
    assert_eq!(cards[0].ticket.as_deref(), Some(old.as_str()));
    let moved = h.app.wf.answer(&cards[0].id, "move").await.unwrap();
    let fork = moved["thread"].as_str().unwrap();
    let rows = h.app.store.pending_all().unwrap();
    assert_eq!(rows.iter().filter_map(|p| p.thread.as_deref()).collect::<Vec<_>>(), [fork], "{rows:?}");
}
