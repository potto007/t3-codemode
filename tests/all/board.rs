use std::time::Duration;

use crate::common::*;
use serde_json::{Value, json};
use t3_codemode::board::{BOARD, card_text, parse_issue, parse_remote, reply_options};
use t3_codemode::config::Config;
use t3_codemode::fake::{project, pull_request, thread};
use t3_codemode::server;

const GATE: &str = "PR #81 is green and reviewed.\n\
**ACTION NEEDED:** reply \"merge\" to merge #81, or reply \"merge and deploy\" to ship it too, here: https://github.com/o/widgets/pull/81";

async fn join_board(h: &H, who: &str, label: &str) {
    h.wf(who, "join", json!({"parent": BOARD, "label": label, "role": "ticket"})).await;
}

fn cards(h: &H) -> Vec<Value> {
    h.app.wf.cards().unwrap().into_iter().map(|c| serde_json::to_value(c).unwrap()).collect()
}

fn sent_to(h: &H, id: &str) -> Vec<Value> {
    h.sent().into_iter().filter(|s| s["threadId"] == id).collect()
}

#[test]
fn parsing_issues_remotes_and_options() {
    let i = parse_issue("https://github.com/Acme/widgets/issues/41#issuecomment-1").unwrap();
    assert_eq!((i.owner.as_str(), i.repo.as_str(), i.number), ("Acme", "widgets", 41));
    assert_eq!(i.label(), "widgets#41");
    assert_eq!(parse_issue("Acme/widgets#7").unwrap().number, 7);
    assert_eq!(parse_issue("github.com/o/r/issues/3").unwrap().repo, "r");
    assert!(parse_issue("https://gitlab.com/o/r/issues/3").is_none());
    assert!(parse_issue("o/r#0").is_none());
    for url in [
        "git@github.com:Acme/WIDGETS.git",
        "https://github.com/acme/widgets",
        "https://x-access-token:abc@github.com/Acme/widgets.git",
        "ssh://git@github.com:22/Acme/widgets.git",
    ] {
        let (o, r) = parse_remote(url).unwrap_or_else(|| panic!("{url}"));
        assert!(o.eq_ignore_ascii_case("acme") && r.eq_ignore_ascii_case("widgets"), "{url}");
    }
    assert!(parse_remote("git@gitlab.com:o/r.git").is_none());
    assert_eq!(reply_options(GATE), ["merge", "merge and deploy"]);
    assert_eq!(reply_options("Reply 'yes' or reply \u{201c}no\u{201d}; prereply \"x\""), ["yes", "no"]);
    assert!(card_text(GATE).starts_with("reply \"merge\" to merge #81"));
    assert_eq!(card_text("\n  first line\nsecond"), "first line");
}

#[test]
fn every_quoted_answer_on_an_action_line_is_an_option() {
    let corpus: &[(&str, &[&str])] = &[
        (
            "**ACTION NEEDED:** reply how to track the finding (...): \"advisory\" for a private \
             security advisory, \"issue\" for a public issue, or \"nothing\", here: https://github.com/acme/widgets/pull/37",
            &["advisory", "issue", "nothing"],
        ),
        (
            "**ACTION NEEDED:** reply \"file follow-ups\" to open issues for the 3 items deferred from #39 (...), or \"no follow-ups\", here: acme/widgets#39",
            &["file follow-ups", "no follow-ups"],
        ),
        (
            "**ACTION NEEDED:** reply \"merge\" to merge #81, or review it first, here: https://github.com/acme/widgets/pull/81",
            &["merge"],
        ),
        (
            "**ACTION NEEDED:** reply \"merge and deploy\" to merge #81 and push content/v1.4.2, or \"merge\" alone, \
             or review it first, here: https://github.com/acme/widgets/pull/81",
            &["merge and deploy", "merge"],
        ),
        (
            "**ACTION NEEDED:** reply \"deploy\" to push content/v1.4.2, or review it first, here: https://github.com/acme/widgets/pull/81",
            &["deploy"],
        ),
        (
            "**ACTION NEEDED:** reopen #81, then reply \"merge\" to merge it, here: https://github.com/acme/widgets/pull/81",
            &["merge"],
        ),
        (
            "**ACTION NEEDED:** reply \"advisory\" to open a private security advisory, reply \"issue\" to file a \
             public issue, or reply \"nothing\" to leave it, here: https://github.com/acme/widgets/pull/37",
            &["advisory", "issue", "nothing"],
        ),
        ("**ACTION NEEDED:** answer \"A\", \"B\", or \"C\"", &["A", "B", "C"]),
        ("**ACTION NEEDED:** Say \u{201c}ship it\u{201d} or \u{201c}hold\u{201d}", &["ship it", "hold"]),
        (
            "**ACTION NEEDED:** reply 'keep' or \u{2018}drop\u{2019} for the user's draft, here: acme/widgets#5",
            &["keep", "drop"],
        ),
        ("**ACTION NEEDED:** reply 'the user's call' or 'skip'", &["the user's call", "skip"]),
        ("**ACTION NEEDED:** don't reply 'yet', it's \u{2018}pending\u{2019}", &["yet", "pending"]),
        ("**ACTION NEEDED:** in \"draft\" mode, reply \"go\"", &["go"]),
        ("**ACTION NEEDED:** replying \"x\" or essay \"y\" does nothing", &[]),
        ("**ACTION NEEDED:** change \"no reply\" to \"pending\", then reply \"done\" or \"skip\"", &["done", "skip"]),
        ("**ACTION NEEDED:** set reply_mode \"on\" or auto_reply \"x\", then answer \"ok\"", &["ok"]),
        ("**ACTION NEEDED:** start the code-mode service, here: `t3cm serve`", &[]),
        ("**ACTION NEEDED:** reply \"\", \"   \", or \"Merge\" or \"merge\"", &["Merge"]),
    ];
    for (line, want) in corpus {
        assert_eq!(reply_options(line), *want, "{line}");
    }
}

#[test]
fn quoted_text_off_the_action_line_makes_no_button() {
    let body = "Earlier the user said \"ship it\", and the reviewer asked us to reply \"later\".\n\
        **ACTION NEEDED:** reply \"merge\" to merge #81, here: https://github.com/acme/widgets/pull/81\n\
        Notes: \"draft\" mode was off.";
    assert_eq!(reply_options(body), ["merge"]);
    let none = "We reply \"merge\" on green.\n**ACTION NEEDED:** review #81 first, here: acme/widgets#81";
    assert!(reply_options(none).is_empty());
    let two = "**ACTION NEEDED:** reply \"merge\" to merge #81\n**ACTION NEEDED:** answer \"retry\" or \"skip\" for the flaky check";
    assert_eq!(reply_options(two), ["merge", "retry", "skip"]);
    let many: String = (1..=12).map(|n| format!(" \"o{n}\"")).collect();
    assert_eq!(reply_options(&format!("**ACTION NEEDED:** reply{many}")).len(), 8);
    let long = format!("**ACTION NEEDED:** reply \"{}\" or \"ok\"", "x".repeat(61));
    assert_eq!(reply_options(&long), ["ok"]);
}

#[tokio::test]
async fn a_gate_to_the_board_becomes_a_card_with_its_reply_buttons() {
    let h = H::new();
    join_board(&h, TICKET, "widgets#41").await;
    let r = h.wf(TICKET, "send", json!({"kind": "gate", "gate": "merge", "body": GATE})).await;
    assert_eq!(r["delivery"], "board");
    assert_eq!(r["card"], "a1");
    let c = cards(&h);
    assert_eq!(c.len(), 1);
    assert_eq!(c[0]["id"], "a1");
    assert_eq!(c[0]["kind"], "gate");
    assert_eq!(c[0]["label"], "widgets#41");
    assert_eq!(c[0]["ticket"], TICKET);
    assert_eq!(c[0]["link"], "https://t3/mcp:ticket");
    assert_eq!(c[0]["handle"], r["handle"]);
    assert_eq!(c[0]["options"], json!(["merge", "merge and deploy"]));
    assert!(c[0]["text"].as_str().unwrap().starts_with("reply \"merge\""));
    assert!(h.sent().is_empty(), "nothing goes to T3: {:?}", h.sent());
}

#[tokio::test]
async fn blockers_and_reports_become_cards_and_events_only_feed() {
    let h = H::new();
    join_board(&h, TICKET, "widgets#41").await;
    h.wf(TICKET, "send", json!({"kind": "blocker", "body": "Need an AWS SSO login"})).await;
    h.wf(TICKET, "send", json!({"kind": "report", "body": "Worker 2 died twice"})).await;
    let r = h.wf(TICKET, "send", json!({"kind": "event", "body": "PR 1/2 merged", "wake": true})).await;
    assert_eq!(r["delivery"], "noted");
    h.wf(TICKET, "send", json!({"kind": "note", "body": "starting PR 2/2"})).await;
    let kinds: Vec<Value> = cards(&h).iter().map(|c| c["kind"].clone()).collect();
    assert_eq!(kinds, [json!("blocker"), json!("report")]);
    let feed = h.wf(TICKET, "feed", json!({})).await;
    let heads: Vec<&str> = feed.as_array().unwrap().iter().map(|f| f["head"].as_str().unwrap()).collect();
    assert_eq!(heads, ["starting PR 2/2", "PR 1/2 merged"]);
    assert_eq!(feed[0]["label"], "widgets#41");
    assert!(h.sent().is_empty());
    assert!(h.app.store.outbox_all().unwrap().is_empty());
}

#[tokio::test]
async fn a_handoff_resolves_the_tickets_cards_and_marks_it_done() {
    let h = H::new();
    join_board(&h, TICKET, "widgets#41").await;
    join_board(&h, WORKER, "widgets#42").await;
    h.wf(TICKET, "send", json!({"kind": "gate", "body": GATE})).await;
    h.wf(TICKET, "send", json!({"kind": "blocker", "body": "stuck"})).await;
    h.wf(WORKER, "send", json!({"kind": "blocker", "body": "other ticket"})).await;
    let r = h.wf(TICKET, "send", json!({"kind": "handoff", "body": "merged and deployed"})).await;
    assert_eq!(r["closed"], true);
    let left = cards(&h);
    assert_eq!(left.len(), 1);
    assert_eq!(left[0]["label"], "widgets#42");
    let tree = h.app.wf.tree().unwrap();
    let t = tree.iter().find(|t| t["label"] == "widgets#41").unwrap();
    assert_eq!((t["done"].clone(), t["status"].clone()), (json!(true), json!("done")));
    assert!(h.sent().is_empty());
}

#[tokio::test]
async fn an_ask_can_see_finished_tickets_and_resolved_cards() {
    let h = H::new();
    join_board(&h, TICKET, "widgets#41").await;
    h.wf(TICKET, "send", json!({"kind": "gate", "body": GATE})).await;
    h.wf(TICKET, "send", json!({"kind": "handoff", "body": "merged and deployed"})).await;
    let open = result(
        &h.exec(
            "async () => ({ b: (await wf.board({ parent: 'board' })).children.length, c: (await wf.cards()).length })",
        )
        .await,
    );
    assert_eq!(open, json!({"b": 0, "c": 0}));
    let all = result(
        &h.exec(
            "async () => ({ b: (await wf.board({ parent: 'board', all: true })).children.map(c => c.label), \
             c: (await wf.cards({ all: true })).map(c => [c.id, c.kind, typeof c.resolved, c.resolution]) })",
        )
        .await,
    );
    assert_eq!(all["b"], json!(["widgets#41"]));
    assert_eq!(all["c"][0][0], "a1");
    assert_eq!(all["c"][0][1], "gate");
    assert_eq!(all["c"][0][2], "string");
    assert!(all["c"][0][3].is_string());
    assert!(t3_codemode::board::ask_brief("q").contains("all: true"));
}

#[tokio::test]
async fn clearing_the_gate_in_the_ticket_resolves_its_gate_card() {
    let h = H::new();
    join_board(&h, TICKET, "widgets#41").await;
    h.wf(TICKET, "send", json!({"kind": "gate", "body": GATE})).await;
    h.wf(TICKET, "send", json!({"kind": "blocker", "body": "also this"})).await;
    h.wf(TICKET, "gate", json!({"clear": true})).await;
    let left = cards(&h);
    assert_eq!(left.len(), 1);
    assert_eq!(left[0]["kind"], "blocker");
}

#[tokio::test]
async fn a_newer_gate_supersedes_the_old_gate_card() {
    let h = H::new();
    join_board(&h, TICKET, "widgets#41").await;
    h.wf(TICKET, "send", json!({"kind": "gate", "body": "reply \"go\" to split"})).await;
    let r = h.wf(TICKET, "send", json!({"kind": "gate", "body": GATE})).await;
    assert_eq!(r["card"], "a2");
    let left = cards(&h);
    assert_eq!(left.len(), 1);
    assert_eq!(left[0]["id"], "a2");
}

#[tokio::test]
async fn execute_can_list_answer_and_see_the_whole_board() {
    let h = H::new();
    join_board(&h, TICKET, "widgets#41").await;
    h.wf(TICKET, "openChild", json!({"role": "worker", "label": "#41 PR 1/2"})).await;
    h.wf(WORKER, "join", json!({"parent": TICKET, "label": "#41 PR 1/2", "role": "worker"})).await;
    h.wf(TICKET, "send", json!({"kind": "gate", "body": GATE})).await;
    let out = h.exec("async () => (await wf.cards()).map(c => c.id + ':' + c.options.join('|'))").await;
    assert_eq!(result(&out), json!(["a1:merge|merge and deploy"]));
    let b = result(&h.exec("async () => wf.board({ parent: 'board' })").await);
    assert_eq!(b["children"][0]["label"], "widgets#41");
    assert_eq!(b["children"][0]["workers"][0]["label"], "#41 PR 1/2");
    assert_eq!(b["cards"], 1);
    let r = result(&h.exec("async () => wf.answer('a1', 'merge')").await);
    assert_eq!(r["delivery"], "sent");
    assert_eq!(sent_to(&h, TICKET).len(), 1);
    assert!(cards(&h).is_empty());
}

#[tokio::test]
async fn launch_ticket_maps_the_repo_to_a_project_and_preregisters_the_link() {
    let h = H::new();
    h.fake.set_projects(vec![project("p-docs", "/src/docs"), project("p-widgets", "/src/widgets")]);
    h.remotes.set("/src/docs", "git@github.com:Acme/docs.git");
    h.remotes.set("/src/widgets", "https://github.com/acme/WIDGETS.git");
    let out = h
        .exec("async () => wf.launchTicket({ issue: 'https://github.com/Acme/widgets/issues/41', title: 'Fix the grader' })")
        .await;
    let r = result(&out);
    assert_eq!(r["ok"], true);
    assert_eq!(r["label"], "widgets#41");
    assert_eq!(r["projectId"], "p-widgets");
    let l = &h.fake.launched()[0];
    assert_eq!(l["projectId"], "p-widgets");
    assert_eq!(l["title"], "widgets#41: Fix the grader");
    assert!(l.get("workspaceStrategy").is_none());
    assert_eq!(l["modelSelection"]["model"], "claude-opus-5-5");
    assert_eq!(l["modelSelection"]["options"], json!([{"id": "effort", "value": "high"}]));
    let msg = l["message"].as_str().unwrap();
    assert!(msg.starts_with("Work https://github.com/Acme/widgets/issues/41 with the t3-ticket skill."), "{msg}");
    assert!(msg.contains(r#"wf.join({ parent: "board", label: "widgets#41", role: "ticket" })"#), "{msg}");
    let pending = h.app.store.pending_all().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].parent, BOARD);
    assert_eq!(pending[0].thread.as_deref(), Some("launch:1"));
    let tree = h.app.wf.tree().unwrap();
    assert_eq!(tree[0]["joined"], false);
    assert_eq!(tree[0]["status"], "running");
    assert_eq!(tree[0]["repo"], "acme/WIDGETS");
    assert_eq!(tree[0]["project"], json!({"id": "p-widgets", "title": "p-widgets"}));
    assert_eq!(tree[0]["title"], "widgets#41: Fix the grader");
    let again = h.exec("async () => wf.launchTicket({ issue: 'Acme/widgets#41' })").await;
    assert!(err_text(&again).contains("has not joined yet"), "{}", again.text);
    h.wf("launch:1", "join", json!({"parent": BOARD, "label": "widgets#41", "role": "ticket"})).await;
    assert!(h.app.store.pending_all().unwrap().is_empty());
    assert_eq!(h.app.store.link_get("launch:1").unwrap().unwrap().params.down, 75);
}

#[tokio::test]
async fn an_unmapped_repo_leaves_an_error_card() {
    let h = H::new();
    h.fake.set_projects(vec![project("p-docs", "/src/docs")]);
    h.remotes.set("/src/docs", "git@github.com:Acme/docs.git");
    let out = h.exec("async () => wf.launchTicket({ issue: 'Acme/widgets#41' })").await;
    assert!(err_text(&out).contains("no T3 project has github.com/Acme/widgets"), "{}", out.text);
    let c = cards(&h);
    assert_eq!(c.len(), 1);
    assert_eq!(c[0]["kind"], "error");
    assert_eq!(c[0]["ticket"], Value::Null);
    assert!(h.fake.launched().is_empty());
    assert!(h.app.store.pending_all().unwrap().is_empty());
}

#[tokio::test]
async fn a_failed_project_lookup_leaves_an_error_card() {
    let h = H::new();
    h.fake.set_projects_fail(true);
    let out = h.exec("async () => wf.launchTicket({ issue: 'Acme/widgets#41' })").await;
    assert!(err_text(&out).contains("Cannot launch widgets#41: "), "{}", out.text);
    let c = cards(&h);
    assert_eq!(c.len(), 1);
    assert_eq!(c[0]["kind"], "error");
    assert_eq!(c[0]["label"], "widgets#41");
    assert!(h.fake.launched().is_empty());
    assert!(h.app.store.pending_all().unwrap().is_empty());
}

#[tokio::test]
async fn a_ticket_that_joins_before_its_launch_returns_is_not_reported() {
    let h = H::new();
    h.fake.set_projects(vec![project("p-widgets", "/src/widgets")]);
    h.remotes.set("/src/widgets", "git@github.com:o/widgets.git");
    h.wf("launch:1", "init", json!({"role": "ticket"})).await;
    h.exec("async () => wf.launchTicket({ issue: 'o/widgets#9' })").await;
    let again = h.exec("async () => wf.launchTicket({ issue: 'o/widgets#9' })").await;
    assert!(err_text(&again).contains("widgets#9 is already on the board"), "{}", again.text);
    assert!(!h.tick(76).await.iter().any(|a| a.starts_with("report never-joined")));
    assert!(cards(&h).iter().all(|c| c["kind"] != "report"), "{:?}", cards(&h));
}

#[tokio::test]
async fn a_launched_ticket_keeps_its_issue_label_over_a_custom_one() {
    let h = H::new();
    h.fake.set_projects(vec![project("p-widgets", "/src/widgets")]);
    h.remotes.set("/src/widgets", "git@github.com:o/widgets.git");
    h.exec("async () => wf.launchTicket({ issue: 'o/widgets#9' })").await;
    let r = result(&h.exec_as("launch:1", "async () => wf.init({ role: 'ticket', label: 'Custom' })").await);
    assert_eq!(r["label"], "widgets#9", "{r}");
    let again = h.exec("async () => wf.launchTicket({ issue: 'o/widgets#9' })").await;
    assert!(err_text(&again).contains("widgets#9 is already on the board"), "{}", again.text);
}

#[tokio::test]
async fn a_ticket_that_finishes_before_its_launch_returns_is_not_reported() {
    let h = H::new();
    h.fake.set_projects(vec![project("p-widgets", "/src/widgets")]);
    h.remotes.set("/src/widgets", "git@github.com:o/widgets.git");
    h.wf("launch:1", "init", json!({"role": "ticket"})).await;
    h.wf("launch:1", "send", json!({"kind": "handoff", "body": "done"})).await;
    h.exec("async () => wf.launchTicket({ issue: 'o/widgets#9' })").await;
    assert!(!h.tick(76).await.iter().any(|a| a.starts_with("report never-joined")));
    assert!(cards(&h).iter().all(|c| c["kind"] != "report"), "{:?}", cards(&h));
}

#[tokio::test]
async fn a_launched_ticket_that_calls_init_clears_its_own_launch() {
    let h = H::new();
    h.fake.set_projects(vec![project("p-widgets", "/src/widgets")]);
    h.remotes.set("/src/widgets", "git@github.com:o/widgets.git");
    h.exec("async () => wf.launchTicket({ issue: 'o/widgets#9', title: 'Fix the grader' })").await;
    let r = result(&h.exec_as("launch:1", "async () => wf.init({ role: 'ticket' })").await);
    assert_eq!(r["parent"], BOARD, "{r}");
    assert_eq!(r["label"], "widgets#9", "{r}");
    let tree = h.app.wf.tree().unwrap();
    assert_eq!(tree.len(), 1, "{tree:?}");
    assert_eq!(tree[0]["joined"], true);
    assert!(!h.tick(76).await.iter().any(|a| a.starts_with("report never-joined")));
    assert!(cards(&h).iter().all(|c| c["kind"] != "report"), "{:?}", cards(&h));
}

#[tokio::test]
async fn a_never_joined_board_ticket_becomes_a_card() {
    let h = H::new();
    h.fake.set_projects(vec![project("p-widgets", "/src/widgets")]);
    h.remotes.set("/src/widgets", "git@github.com:o/widgets.git");
    h.app.wf.launch_ticket(serde_json::from_value(json!({"issue": "o/widgets#9"})).unwrap()).await.unwrap();
    assert!(h.tick(70).await.is_empty());
    assert!(h.tick(76).await.contains(&"report never-joined widgets#9".to_string()));
    let c = cards(&h);
    assert_eq!(c.len(), 1);
    assert_eq!(c[0]["kind"], "report");
    assert_eq!(c[0]["ticket"], "launch:1");
    assert_eq!(c[0]["link"], "https://t3/launch:1");
    assert!(c[0]["text"].as_str().unwrap().contains("never joined"));
    assert!(sent_to(&h, BOARD).is_empty());
}

#[tokio::test]
async fn watcher_escalations_about_board_children_are_cards_never_sends() {
    let h = H::new();
    join_board(&h, TICKET, "widgets#41").await;
    assert_eq!(h.tick(75).await, vec!["nudge widgets#41 (1/2)"]);
    assert_eq!(h.tick(150).await, vec!["nudge widgets#41 (2/2)"]);
    let out = h.tick(225).await;
    assert!(out.iter().any(|a| a.starts_with("escalate widgets#41")), "{out:?}");
    let c = cards(&h);
    assert_eq!(c.len(), 1);
    assert_eq!(c[0]["kind"], "escalate");
    assert_eq!(c[0]["ticket"], TICKET);
    h.fake.update(TICKET, json!({"status": "waiting", "pendingRequestCount": 1, "latestRunId": "run:2"}));
    h.tick(400).await;
    assert_eq!(cards(&h).len(), 2);
    assert!(sent_to(&h, BOARD).is_empty());
    assert!(h.sent().iter().all(|s| s["threadId"] == TICKET));
}

#[tokio::test]
async fn an_ask_launches_a_scratch_thread_and_captures_its_answer() {
    let h = H::new();
    let a = h.app.wf.ask("Which tickets wait on me?").await.unwrap();
    assert_eq!(a["id"], "q1");
    let l = &h.fake.launched()[0];
    assert_eq!(l["scratch"], true);
    assert!(l.get("projectId").is_none());
    assert_eq!(l["modelSelection"]["model"], "claude-haiku-5-5");
    let msg = l["message"].as_str().unwrap();
    assert!(msg.contains("Which tickets wait on me?") && msg.contains("mcp__t3cm__execute"), "{msg}");
    assert!(h.app.wf.check_asks(None).await.unwrap(), "still running");
    h.fake.update("launch:1", json!({"status": "completed"}));
    h.fake.set_items(
        "launch:1",
        vec![
            json!({"type": "user_message", "position": 0, "text": msg}),
            json!({"type": "assistant_message", "position": 3, "text": "widgets#41 waits on a merge: a1."}),
        ],
    );
    assert!(!h.app.wf.check_asks(None).await.unwrap());
    let asks = h.app.wf.asks().unwrap();
    assert_eq!(asks[0]["status"], "answered");
    assert_eq!(asks[0]["answer"], "widgets#41 waits on a merge: a1.");
    assert_eq!(asks[0]["archived"], true);
    assert_eq!(h.fake.organized(), vec![json!({"threadId": "launch:1", "action": "archive"})]);
    assert!(h.sent().is_empty());
}

struct Srv {
    base: String,
    http: reqwest::Client,
    h: H,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Drop for Srv {
    fn drop(&mut self) {
        if let Some(s) = self.stop.take() {
            let _ = s.send(());
        }
    }
}

async fn start(h: H) -> Srv {
    let (listener, addr) = server::bind(0).await.unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let app = h.app.clone();
    tokio::spawn(async move {
        server::serve(app, listener, async move {
            let _ = rx.await;
        })
        .await
        .unwrap();
    });
    Srv {
        base: format!("http://{addr}"),
        http: reqwest::Client::builder().no_proxy().build().unwrap(),
        h,
        stop: Some(tx),
    }
}

impl Srv {
    async fn post(&self, path: &str, body: Value, csrf: Option<&str>, origin: Option<&str>) -> reqwest::Response {
        let mut req = self.http.post(format!("{}{path}", self.base)).json(&body);
        if let Some(c) = csrf {
            req = req.header("X-T3CM-CSRF", c);
        }
        if let Some(o) = origin {
            req = req.header("Origin", o);
        }
        req.send().await.unwrap()
    }

    fn csrf(&self) -> String {
        self.h.app.csrf.clone()
    }

    async fn get(&self, path: &str) -> Value {
        self.http.get(format!("{}{path}", self.base)).send().await.unwrap().json().await.unwrap()
    }

    async fn context(&self, method: reqwest::Method, id: &str, body: Value, csrf: Option<&str>) -> (u16, Value) {
        let url = format!("{}/api/contexts/{}", self.base, id.replace(' ', "%20"));
        let mut req = self.http.request(method, url).json(&body).header("Origin", &self.base);
        if let Some(c) = csrf {
            req = req.header("X-T3CM-CSRF", c);
        }
        let r = req.send().await.unwrap();
        (r.status().as_u16(), r.json().await.unwrap_or(Value::Null))
    }

    async fn restart(mut self) -> Srv {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        let mut h = std::mem::replace(&mut self.h, H::new());
        h.reopen();
        start(h).await
    }
}

#[tokio::test]
async fn the_answer_endpoint_sends_one_reply_and_resolves_the_card() {
    let s = start(H::new()).await;
    join_board(&s.h, TICKET, "widgets#41").await;
    s.h.wf(TICKET, "send", json!({"kind": "gate", "body": GATE})).await;
    let r = s.post("/api/cards/a1/answer", json!({"text": "merge and deploy"}), Some(&s.csrf()), Some(&s.base)).await;
    assert_eq!(r.status(), 200);
    let sent = sent_to(&s.h, TICKET);
    assert_eq!(sent.len(), 1);
    let msg = sent[0]["message"].as_str().unwrap();
    assert!(msg.starts_with("[t3cm reply from=board msg="), "{msg}");
    assert!(msg.contains("The user says \"merge and deploy\" about a1: reply \"merge\" to merge #81"), "{msg}");
    assert!(cards(&s.h).is_empty());
    let r = s.post("/api/cards/a1/answer", json!({"text": "again"}), Some(&s.csrf()), None).await;
    assert_eq!(r.status(), 404);
    assert_eq!(sent_to(&s.h, TICKET).len(), 1);
}

#[tokio::test]
async fn card_actions_for_an_older_decision_with_the_same_id_do_nothing() {
    let s = start(H::new()).await;
    join_board(&s.h, TICKET, "widgets#41").await;
    s.h.wf(TICKET, "send", json!({"kind": "gate", "body": GATE})).await;
    let old = cards(&s.h)[0].clone();
    assert_eq!(old["id"], "a1");
    let want = |c: &Value| json!({"at": c["at"], "token": c["token"]});
    let r = s.post("/api/cards/a1/dismiss", want(&old), Some(&s.csrf()), None).await;
    assert_eq!(r.status(), 200);
    for batch in [2..=19, 20..=36] {
        let issues: Vec<String> = batch.map(|n| format!("o/nowhere#{n}")).collect();
        assert_eq!(s.post("/api/launch", json!({"issues": issues}), Some(&s.csrf()), None).await.status(), 200);
    }
    assert_eq!(cards(&s.h).len(), 35);
    s.h.wf(TICKET, "send", json!({"kind": "gate", "body": GATE})).await;
    let new = cards(&s.h).into_iter().find(|c| c["id"] == "a1").unwrap();
    assert_eq!(new["at"], old["at"]);
    assert_ne!(new["token"], old["token"]);
    let mut stale = want(&old);
    stale["text"] = json!("merge");
    let r = s.post("/api/cards/a1/answer", stale, Some(&s.csrf()), None).await;
    assert_eq!(r.status(), 404);
    let r = s.post("/api/cards/a1/dismiss", want(&old), Some(&s.csrf()), None).await;
    assert_eq!(r.status(), 404);
    assert!(sent_to(&s.h, TICKET).is_empty());
    assert!(cards(&s.h).iter().any(|c| c["id"] == "a1"));
    let mut fresh = want(&new);
    fresh["text"] = json!("merge");
    let r = s.post("/api/cards/a1/answer", fresh, Some(&s.csrf()), None).await;
    assert_eq!(r.status(), 200);
    assert_eq!(sent_to(&s.h, TICKET).len(), 1);
}

#[tokio::test]
async fn a_reply_in_flight_resolves_only_the_card_it_answered() {
    let s = start(H::new()).await;
    join_board(&s.h, TICKET, "widgets#41").await;
    s.h.wf(TICKET, "send", json!({"kind": "gate", "body": GATE})).await;
    let old = cards(&s.h)[0].clone();
    s.h.fake.set_delay(Duration::from_millis(400));
    let body = json!({"text": "merge", "at": old["at"], "token": old["token"]});
    let csrf = s.csrf();
    let reply = s.post("/api/cards/a1/answer", body, Some(&csrf), None);
    let reuse = async {
        while s.h.app.store.outbox_all().unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        s.h.app.store.waiting_resolve(BOARD, "a1", Some("resolved elsewhere"), "2026-11-17T12:00:00Z").unwrap();
        let item = t3_codemode::store::WaitingItem {
            id: "a1".into(),
            text: "a newer gate".into(),
            from: None,
            link: None,
            at: old["at"].as_str().unwrap().into(),
        };
        s.h.app.store.waiting_insert(BOARD, &item, Some(&json!({"kind": "gate", "ticket": TICKET}))).unwrap();
    };
    let (r, ()) = tokio::join!(reply, reuse);
    s.h.fake.set_delay(Duration::ZERO);
    assert_eq!(r.status(), 200);
    let open = cards(&s.h);
    assert_eq!(open.len(), 1);
    assert_eq!(open[0]["id"], "a1");
    assert_eq!(open[0]["text"], "a newer gate");
    assert_ne!(open[0]["token"], old["token"]);
}

#[tokio::test]
async fn posts_need_the_csrf_token_and_the_boards_own_origin() {
    let s = start(H::new()).await;
    join_board(&s.h, TICKET, "widgets#41").await;
    s.h.wf(TICKET, "send", json!({"kind": "gate", "body": GATE})).await;
    let body = json!({"text": "merge"});
    assert_eq!(s.post("/api/cards/a1/answer", body.clone(), None, None).await.status(), 403);
    assert_eq!(s.post("/api/cards/a1/answer", body.clone(), Some("nope"), None).await.status(), 403);
    let foreign = s.post("/api/cards/a1/answer", body.clone(), Some(&s.csrf()), Some("https://evil.example")).await;
    assert_eq!(foreign.status(), 403);
    let other_port = s.post("/api/cards/a1/answer", body.clone(), Some(&s.csrf()), Some("http://127.0.0.1:1")).await;
    assert_eq!(other_port.status(), 403);
    assert_eq!(s.post("/api/launch", json!({"issues": ["o/r#1"]}), None, None).await.status(), 403);
    assert_eq!(s.post("/api/ask", json!({"question": "x"}), Some("nope"), None).await.status(), 403);
    let r = s.http.get(format!("{}/api/state", s.base)).header("Origin", "https://evil.example").send().await.unwrap();
    assert_eq!(r.status(), 403);
    let r = s.http.get(format!("{}/api/state", s.base)).header("Host", "evil.example:80").send().await.unwrap();
    assert_eq!(r.status(), 403);
    assert!(s.h.sent().is_empty());
    assert_eq!(cards(&s.h).len(), 1);
}

#[tokio::test]
async fn dismiss_resolves_without_messaging() {
    let s = start(H::new()).await;
    join_board(&s.h, TICKET, "widgets#41").await;
    s.h.wf(TICKET, "send", json!({"kind": "blocker", "body": "stuck"})).await;
    let r = s.post("/api/cards/a1/dismiss", json!({}), Some(&s.csrf()), Some(&s.base)).await;
    assert_eq!(r.status(), 200);
    assert!(cards(&s.h).is_empty());
    assert!(s.h.sent().is_empty());
}

#[tokio::test]
async fn state_launch_and_message_endpoints() {
    let s = start(H::new()).await;
    s.h.fake.set_projects(vec![project("p-widgets", "/src/widgets")]);
    s.h.remotes.set("/src/widgets", "git@github.com:o/widgets.git");
    let r =
        s.post("/api/launch", json!({"issues": ["o/widgets#1", "o/other#2", "nonsense"]}), Some(&s.csrf()), None).await;
    assert_eq!(r.status(), 200);
    let res: Value = r.json().await.unwrap();
    let ok: Vec<Value> = res["results"].as_array().unwrap().iter().map(|x| x["ok"].clone()).collect();
    assert_eq!(ok, [json!(true), json!(false), json!(false)]);
    assert_eq!(res["results"][1]["card"], "a1");
    join_board(&s.h, TICKET, "widgets#41").await;
    let sent = s.h.wf(TICKET, "send", json!({"kind": "gate", "body": GATE})).await;
    s.h.fake.put(thread("launch:1", "running", T0));
    let st: Value = s.http.get(format!("{}/api/state", s.base)).send().await.unwrap().json().await.unwrap();
    assert_eq!(st["counts"]["cards"], 2);
    assert_eq!(st["cards"][1]["options"], json!(["merge", "merge and deploy"]));
    let labels: Vec<Value> = st["tree"].as_array().unwrap().iter().map(|t| t["label"].clone()).collect();
    assert_eq!(labels, [json!("widgets#41"), json!("widgets#1")]);
    assert_eq!(st["service"]["t3"]["connected"], true);
    let handle = sent["handle"].as_str().unwrap();
    let m: Value = s.http.get(format!("{}/api/msg/{handle}", s.base)).send().await.unwrap().json().await.unwrap();
    assert_eq!(m["body"], GATE);
}

#[tokio::test]
async fn the_event_stream_pushes_state_when_a_card_appears() {
    let s = start(H::new()).await;
    join_board(&s.h, TICKET, "widgets#41").await;
    let mut r = s.http.get(format!("{}/api/events", s.base)).send().await.unwrap();
    assert_eq!(r.status(), 200);
    assert!(r.headers()["content-type"].to_str().unwrap().starts_with("text/event-stream"));
    let first = tokio::time::timeout(Duration::from_secs(5), r.chunk()).await.unwrap().unwrap().unwrap();
    let first = String::from_utf8_lossy(&first).to_string();
    assert!(first.starts_with("event: state\ndata: {"), "{first}");
    assert!(first.contains("\"cards\":[]"), "{first}");
    s.h.wf(TICKET, "send", json!({"kind": "gate", "body": GATE})).await;
    let mut seen = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !seen.contains("\"id\":\"a1\"") {
        let chunk =
            tokio::time::timeout_at(deadline, r.chunk()).await.expect("a state event in time").unwrap().unwrap();
        seen.push_str(&String::from_utf8_lossy(&chunk));
    }
    assert!(seen.contains("event: state"), "{seen}");
    assert!(seen.contains("merge and deploy"), "{seen}");
}

#[tokio::test]
async fn the_page_embeds_the_token_and_loads_nothing_external() {
    let s = start(H::new()).await;
    let r = s.http.get(format!("{}/", s.base)).send().await.unwrap();
    assert_eq!(r.status(), 200);
    assert!(r.headers()["content-type"].to_str().unwrap().starts_with("text/html"));
    assert!(r.headers()["content-security-policy"].to_str().unwrap().contains("default-src 'none'"));
    let html = r.text().await.unwrap();
    assert!(html.contains(&format!("const CSRF = \"{}\";", s.csrf())));
    assert!(!html.contains("{{CSRF}}"));
    let lower = html.to_lowercase();
    let css = &lower[lower.find("<style>").unwrap()..lower.find("</style>").unwrap()];
    for (i, _) in css.match_indices("url(") {
        assert!(css[i..].starts_with("url(\"data:"), "external load: {}", &css[i..i + 40]);
    }
    for tag in ["<script", "<link", "<img", "<iframe", "@import"] {
        for (i, _) in lower.match_indices(tag) {
            let end = lower[i..].find('>').map(|e| i + e).unwrap_or(lower.len());
            let t = &lower[i..end];
            assert!(!t.contains("http://") && !t.contains("https://") && !t.contains("src="), "external load: {t}");
        }
    }
    assert!(!html.contains('\u{2014}'));
}

fn settle_calls(h: &H, action: &str) -> Vec<Value> {
    h.fake.organized().into_iter().filter(|o| o["action"] == action).collect()
}

async fn done_ticket(h: &H, who: &str, label: &str) {
    join_board(h, who, label).await;
    h.wf(who, "send", json!({"kind": "handoff", "body": "merged and deployed"})).await;
}

async fn closed_ask(h: &H, question: &str) -> String {
    let a = h.app.wf.ask(question).await.unwrap();
    let thread = a["thread"].as_str().unwrap().to_string();
    h.fake.update(&thread, json!({"status": "completed"}));
    h.fake.set_items(&thread, vec![json!({"type": "assistant_message", "position": 1, "text": "all quiet"})]);
    h.app.wf.check_asks(None).await.unwrap();
    a["id"].as_str().unwrap().to_string()
}

fn snapshot(h: &H) -> Value {
    h.app.wf.snapshot().unwrap()
}

fn labels(v: &Value) -> Vec<Value> {
    v.as_array().unwrap().iter().map(|t| t["label"].clone()).collect()
}

#[tokio::test]
async fn done_tickets_and_closed_asks_settle_once_the_threshold_passes() {
    let h = H::new();
    done_ticket(&h, TICKET, "widgets#41").await;
    join_board(&h, WORKER, "widgets#42").await;
    let ask = closed_ask(&h, "What is waiting?").await;
    h.clock.advance_minutes(24 * 60 - 1);
    h.app.wf.board_tick().await.unwrap();
    let st = snapshot(&h);
    assert_eq!(labels(&st["tree"]), [json!("widgets#42"), json!("widgets#41")]);
    assert_eq!(st["asks"][0]["id"], ask.as_str());
    assert_eq!(st["counts"]["settled"], 0);
    assert!(settle_calls(&h, "settle").is_empty());
    h.clock.advance_minutes(1);
    h.app.wf.board_tick().await.unwrap();
    let st = snapshot(&h);
    assert_eq!(labels(&st["tree"]), [json!("widgets#42")]);
    assert_eq!(st["asks"], json!([]));
    assert_eq!(labels(&st["settled"]["tickets"]), [json!("widgets#41")]);
    assert_eq!(st["settled"]["tickets"][0]["done"], true);
    assert!(st["settled"]["tickets"][0]["settledAt"].as_str().unwrap().contains(':'));
    assert_eq!(st["settled"]["asks"][0]["id"], ask.as_str());
    assert!(st["settled"]["asks"][0]["settledAt"].is_string());
    assert_eq!(st["counts"]["settled"], 2);
    assert_eq!(settle_calls(&h, "settle"), vec![json!({"threadId": TICKET, "action": "settle"})]);
    h.app.wf.board_tick().await.unwrap();
    assert_eq!(settle_calls(&h, "settle").len(), 1);
    let board = h.wf(BOSS, "board", json!({"parent": BOARD, "all": true})).await;
    assert_eq!(board["children"][0]["settled"], true);
    assert_eq!(board["children"][1]["settled"], false);
}

#[tokio::test]
async fn a_failed_ask_settles_from_when_it_was_asked() {
    let h = H::new();
    let a = h.app.wf.ask("Will this fail?").await.unwrap();
    h.fake.update(a["thread"].as_str().unwrap(), json!({"status": "failed"}));
    h.app.wf.check_asks(None).await.unwrap();
    assert!(h.app.wf.asks().unwrap()[0]["answeredAt"].is_null());
    h.clock.advance_minutes(24 * 60 + 1);
    h.app.wf.board_tick().await.unwrap();
    assert_eq!(snapshot(&h)["settled"]["asks"][0]["status"], "failed");
}

#[tokio::test]
async fn zero_settle_after_hours_turns_auto_settle_off() {
    let config = Config::from_value(json!({"board": {"settleAfterHours": 0}})).unwrap();
    let h = H::build(Vec::new(), config);
    done_ticket(&h, TICKET, "widgets#41").await;
    closed_ask(&h, "What is waiting?").await;
    h.clock.advance_minutes(30 * 24 * 60);
    h.app.wf.board_tick().await.unwrap();
    let st = snapshot(&h);
    assert_eq!(labels(&st["tree"]), [json!("widgets#41")]);
    assert_eq!(st["asks"].as_array().unwrap().len(), 1);
    assert_eq!(st["counts"]["settled"], 0);
    assert!(settle_calls(&h, "settle").is_empty());
}

#[tokio::test]
async fn a_ticket_settled_or_archived_in_t3_settles_on_the_board() {
    let h = H::new();
    done_ticket(&h, TICKET, "widgets#41").await;
    done_ticket(&h, WORKER, "widgets#42").await;
    done_ticket(&h, BOSS, "widgets#43").await;
    h.app.wf.board_tick().await.unwrap();
    assert_eq!(snapshot(&h)["counts"]["settled"], 0);
    h.fake.update(TICKET, json!({"settled": true, "settledAt": "2026-11-17T12:05:00.123Z"}));
    h.fake.update(WORKER, json!({"archived": true, "updatedAt": "2026-11-17T12:05:00.000Z"}));
    h.clock.advance_minutes(10);
    h.app.wf.board_tick().await.unwrap();
    let st = snapshot(&h);
    assert_eq!(labels(&st["tree"]), [json!("widgets#43")]);
    let mut settled = labels(&st["settled"]["tickets"]);
    settled.sort_by_key(|v| v.to_string());
    assert_eq!(settled, [json!("widgets#41"), json!("widgets#42")]);
    assert!(settle_calls(&h, "settle").is_empty());
}

#[tokio::test]
async fn a_ticket_whose_thread_is_gone_settles_on_the_board() {
    let h = H::new();
    done_ticket(&h, TICKET, "widgets#41").await;
    h.fake.remove(TICKET);
    h.app.wf.board_tick().await.unwrap();
    assert_eq!(labels(&snapshot(&h)["settled"]["tickets"]), [json!("widgets#41")]);
    assert!(settle_calls(&h, "settle").is_empty());
}

#[tokio::test]
async fn a_restored_ticket_ignores_a_t3_settle_older_than_the_restore() {
    let h = H::new();
    done_ticket(&h, TICKET, "widgets#41").await;
    h.app.wf.settle_ticket(TICKET).await.unwrap();
    h.clock.advance_minutes(10);
    h.fake.set_organize_fails(true);
    h.app.wf.restore_ticket(TICKET).await.unwrap();
    h.fake.update(TICKET, json!({"settled": true, "settledAt": "2026-11-17T12:00:00.000Z"}));
    h.app.wf.board_tick().await.unwrap();
    assert_eq!(labels(&snapshot(&h)["tree"]), [json!("widgets#41")]);
    h.clock.advance_minutes(10);
    h.fake.update(TICKET, json!({"settledAt": "2026-11-17T12:15:00.000Z"}));
    h.app.wf.board_tick().await.unwrap();
    assert_eq!(labels(&snapshot(&h)["settled"]["tickets"]), [json!("widgets#41")]);
}

#[tokio::test]
async fn a_restored_ticket_stays_restored_while_its_thread_is_archived() {
    let h = H::new();
    done_ticket(&h, TICKET, "widgets#41").await;
    h.fake.update(TICKET, json!({"archived": true}));
    h.app.wf.board_tick().await.unwrap();
    h.clock.advance_minutes(10);
    h.app.wf.restore_ticket(TICKET).await.unwrap();
    h.fake.update(TICKET, json!({"updatedAt": "2026-11-17T12:30:00.000Z"}));
    h.clock.advance_minutes(30);
    h.app.wf.board_tick().await.unwrap();
    assert_eq!(labels(&snapshot(&h)["tree"]), [json!("widgets#41")]);
}

#[tokio::test]
async fn a_restored_ticket_is_not_settled_again_at_once() {
    let h = H::new();
    done_ticket(&h, TICKET, "widgets#41").await;
    let ask = closed_ask(&h, "What is waiting?").await;
    h.clock.advance_minutes(25 * 60);
    h.app.wf.board_tick().await.unwrap();
    assert_eq!(snapshot(&h)["counts"]["settled"], 2);
    h.app.wf.restore_ticket(TICKET).await.unwrap();
    h.app.wf.restore_ask(&ask).unwrap();
    h.clock.advance_minutes(60);
    h.app.wf.board_tick().await.unwrap();
    let st = snapshot(&h);
    assert_eq!(labels(&st["tree"]), [json!("widgets#41")]);
    assert_eq!(st["asks"][0]["id"], ask.as_str());
    assert_eq!(st["counts"]["settled"], 0);
    h.clock.advance_minutes(24 * 60);
    h.app.wf.board_tick().await.unwrap();
    assert_eq!(snapshot(&h)["counts"]["settled"], 2);
}

#[tokio::test]
async fn settle_and_restore_work_through_the_routes_and_call_t3() {
    let s = start(H::new()).await;
    done_ticket(&s.h, TICKET, "widgets#41").await;
    let ask = closed_ask(&s.h, "What is waiting?").await;
    let ticket_settle = format!("/api/tickets/{TICKET}/settle");
    assert_eq!(s.post(&ticket_settle, json!({}), None, None).await.status(), 403);
    assert_eq!(s.post(&ticket_settle, json!({}), Some("nope"), None).await.status(), 403);
    let ask_settle = format!("/api/asks/{ask}/settle");
    assert_eq!(s.post(&ask_settle, json!({}), None, None).await.status(), 403);
    assert_eq!(s.post("/api/settle-done", json!({}), None, None).await.status(), 403);
    assert_eq!(snapshot(&s.h)["counts"]["settled"], 0);
    assert!(settle_calls(&s.h, "settle").is_empty());

    let r = s.post(&ticket_settle, json!({}), Some(&s.csrf()), Some(&s.base)).await;
    assert_eq!(r.status(), 200);
    let body: Value = r.json().await.unwrap();
    assert_eq!((body["settled"].clone(), body["t3"]["ok"].clone()), (json!(true), json!(true)));
    assert_eq!(s.post(&ask_settle, json!({}), Some(&s.csrf()), None).await.status(), 200);
    let st = snapshot(&s.h);
    assert_eq!(st["tree"], json!([]));
    assert_eq!(st["asks"], json!([]));
    assert_eq!(st["settled"]["tickets"][0]["thread"], TICKET);
    assert_eq!(st["settled"]["asks"][0]["id"], ask.as_str());
    assert_eq!(st["counts"]["settled"], 2);
    assert_eq!(settle_calls(&s.h, "settle"), vec![json!({"threadId": TICKET, "action": "settle"})]);
    assert_eq!(s.post(&ticket_settle, json!({}), Some(&s.csrf()), None).await.status(), 400);

    let ticket_restore = format!("/api/tickets/{TICKET}/restore");
    let ask_restore = format!("/api/asks/{ask}/restore");
    assert_eq!(s.post(&ticket_restore, json!({}), None, None).await.status(), 403);
    assert_eq!(s.post(&ticket_restore, json!({}), Some(&s.csrf()), None).await.status(), 200);
    assert_eq!(s.post(&ask_restore, json!({}), Some(&s.csrf()), None).await.status(), 200);
    let st = snapshot(&s.h);
    assert_eq!(labels(&st["tree"]), [json!("widgets#41")]);
    assert_eq!(st["asks"][0]["id"], ask.as_str());
    assert_eq!(st["counts"]["settled"], 0);
    assert_eq!(settle_calls(&s.h, "unsettle"), vec![json!({"threadId": TICKET, "action": "unsettle"})]);
    assert_eq!(s.post(&ticket_restore, json!({}), Some(&s.csrf()), None).await.status(), 400);
    assert_eq!(s.post(&ask_restore, json!({}), Some(&s.csrf()), None).await.status(), 400);
    let missing = s.post("/api/tickets/nope/settle", json!({}), Some(&s.csrf()), None).await;
    assert_eq!(missing.status(), 404);
    assert_eq!(s.post("/api/asks/q99/settle", json!({}), Some(&s.csrf()), None).await.status(), 404);
    assert_eq!(s.post("/api/asks/bogus/settle", json!({}), Some(&s.csrf()), None).await.status(), 404);
}

#[tokio::test]
async fn open_tickets_and_open_asks_cannot_be_settled() {
    let s = start(H::new()).await;
    join_board(&s.h, TICKET, "widgets#41").await;
    let a = s.h.app.wf.ask("Still working?").await.unwrap();
    let r = s.post(&format!("/api/tickets/{TICKET}/settle"), json!({}), Some(&s.csrf()), None).await;
    assert_eq!(r.status(), 400);
    let err: Value = r.json().await.unwrap();
    assert!(err["error"].as_str().unwrap().contains("only done tickets"), "{err}");
    let r = s.post(&format!("/api/asks/{}/settle", a["id"].as_str().unwrap()), json!({}), Some(&s.csrf()), None).await;
    assert_eq!(r.status(), 400);
    let err: Value = r.json().await.unwrap();
    assert!(err["error"].as_str().unwrap().contains("only closed asks"), "{err}");
    let st = snapshot(&s.h);
    assert_eq!(labels(&st["tree"]), [json!("widgets#41")]);
    assert_eq!(st["counts"]["settled"], 0);
    assert!(s.h.fake.organized().is_empty());
}

#[tokio::test]
async fn a_t3_error_does_not_fail_settle_or_restore() {
    let s = start(H::new()).await;
    done_ticket(&s.h, TICKET, "widgets#41").await;
    s.h.fake.set_organize_fails(true);
    let r = s.post(&format!("/api/tickets/{TICKET}/settle"), json!({}), Some(&s.csrf()), None).await;
    assert_eq!(r.status(), 200);
    let body: Value = r.json().await.unwrap();
    assert_eq!(body["settled"], true);
    assert_eq!(body["t3"]["ok"], false);
    assert!(body["t3"]["error"].as_str().unwrap().contains("unreachable"), "{body}");
    assert_eq!(snapshot(&s.h)["counts"]["settled"], 1);
    let r = s.post(&format!("/api/tickets/{TICKET}/restore"), json!({}), Some(&s.csrf()), None).await;
    assert_eq!(r.status(), 200);
    assert_eq!(r.json::<Value>().await.unwrap()["t3"]["ok"], false);
    assert_eq!(snapshot(&s.h)["counts"]["settled"], 0);
}

#[tokio::test]
async fn settle_done_settles_every_finished_item_and_returns_counts() {
    let s = start(H::new()).await;
    done_ticket(&s.h, TICKET, "widgets#41").await;
    join_board(&s.h, WORKER, "widgets#42").await;
    s.h.fake.put(thread("mcp:third", "idle", T0));
    done_ticket(&s.h, "mcp:third", "widgets#43").await;
    closed_ask(&s.h, "First?").await;
    s.h.app.wf.ask("Second?").await.unwrap();
    assert_eq!(s.post("/api/settle-done", json!({}), None, None).await.status(), 403);
    let r = s.post("/api/settle-done", json!({}), Some(&s.csrf()), None).await;
    assert_eq!(r.status(), 200);
    assert_eq!(r.json::<Value>().await.unwrap(), json!({"tickets": 2, "asks": 1}));
    let st = snapshot(&s.h);
    assert_eq!(labels(&st["tree"]), [json!("widgets#42")]);
    assert_eq!(st["asks"].as_array().unwrap().len(), 1);
    assert_eq!(st["asks"][0]["status"], "running");
    assert_eq!(st["counts"]["settled"], 3);
    assert_eq!(settle_calls(&s.h, "settle").len(), 2);
    let again = s.post("/api/settle-done", json!({}), Some(&s.csrf()), None).await;
    assert_eq!(again.json::<Value>().await.unwrap(), json!({"tickets": 0, "asks": 0}));
}

#[tokio::test]
async fn settled_items_are_newest_first_and_capped() {
    let h = H::new();
    for n in 0..205 {
        let id = format!("mcp:t{n}");
        h.fake.put(thread(&id, "idle", T0));
        done_ticket(&h, &id, &format!("widgets#{n}")).await;
        h.clock.advance_minutes(1);
        h.app.wf.settle_ticket(&id).await.unwrap();
    }
    for n in 0..205 {
        closed_ask(&h, &format!("Question {n}?")).await;
        h.clock.advance_minutes(1);
        h.app.wf.settle_ask(&format!("q{}", n + 1)).unwrap();
    }
    let st = snapshot(&h);
    assert_eq!(st["settled"]["tickets"].as_array().unwrap().len(), 200);
    assert_eq!(st["settled"]["asks"].as_array().unwrap().len(), 200);
    assert_eq!(st["settled"]["tickets"][0]["label"], "widgets#204");
    assert_eq!(st["settled"]["asks"][0]["id"], "q205");
    assert_eq!(st["counts"]["settled"], 410);
    assert_eq!(st["asks"], json!([]));
}

#[tokio::test]
async fn links_stored_before_settling_still_load() {
    let h = H::new();
    join_board(&h, TICKET, "widgets#1").await;
    let ln = h.app.store.link_get(TICKET).unwrap().unwrap();
    let mut raw = serde_json::to_value(&ln).unwrap();
    let keys = raw.as_object_mut().unwrap();
    assert!(keys.remove("settledAt").is_some() && keys.remove("restoredAt").is_some());
    let old: t3_codemode::store::Link = serde_json::from_value(raw).unwrap();
    assert_eq!(old, ln);
    assert_eq!((old.settled_at, old.restored_at), (None, None));
}

fn run_page(html: &str, scenario: &str) -> Value {
    let open = html.find("<script>").unwrap() + "<script>".len();
    let close = html.rfind("</script>").unwrap();
    let src =
        format!("{}\n{}\nJSON.stringify(({scenario})())", include_str!("../support/board_dom.js"), &html[open..close]);
    let rt = rquickjs::Runtime::new().unwrap();
    let ctx = rquickjs::Context::full(&rt).unwrap();
    ctx.with(|ctx| {
        use rquickjs::CatchResultExt;
        let out: String = ctx.eval(src).catch(&ctx).unwrap_or_else(|e| panic!("{e}"));
        serde_json::from_str(&out).unwrap()
    })
}

#[tokio::test]
async fn typing_in_a_card_reply_survives_state_events() {
    let s = start(H::new()).await;
    join_board(&s.h, TICKET, "widgets#41").await;
    join_board(&s.h, WORKER, "widgets#42").await;
    s.h.wf(TICKET, "send", json!({"kind": "gate", "gate": "merge", "body": GATE})).await;
    s.h.wf(WORKER, "send", json!({"kind": "gate", "gate": "merge", "body": GATE})).await;
    let html = s.http.get(format!("{}/", s.base)).send().await.unwrap().text().await.unwrap();
    let st: Value = s.http.get(format!("{}/api/state", s.base)).send().await.unwrap().json().await.unwrap();
    let ids: Vec<Value> = st["cards"].as_array().unwrap().iter().map(|c| c["id"].clone()).collect();
    assert_eq!(ids, [json!("a1"), json!("a2")]);
    let scenario = format!(
        r#"() => {{
  const base = {st};
  const input = (id) => document.querySelector('#cards [data-id="' + id + '"] input');
  const changed = (id, text) => ({{ ...base, cards: base.cards.map((c) => c.id === id ? {{ ...c, text }} : c) }});
  emitState(base);
  const first = input("a1");
  first.focus();
  typeKeys("hello");
  first.setSelectionRange(2, 2);
  emitState(base);
  typeKeys("X");
  emitState(changed("a2", "the other card changed"));
  typeKeys("Y");
  emitState({{ ...base, cards: [{{ ...base.cards[1], id: "a9" }}, ...base.cards] }});
  typeKeys("Z");
  const kept = input("a1") === first;
  emitState(changed("a1", "this card changed"));
  typeKeys("W");
  const rebuilt = input("a1");
  rebuilt.setSelectionRange(1, 4, "backward");
  emitState(changed("a1", "this card changed again"));
  const last = input("a1");
  return {{
    kept,
    same: rebuilt === first && last === first,
    value: last.value,
    focused: document.activeElement === last,
    selection: [last.selectionStart, last.selectionEnd, last.selectionDirection],
    order: [...document.getElementById("cards").children].map((c) => c.dataset.id),
  }};
}}"#
    );
    let r = run_page(&html, &scenario);
    assert_eq!(r["kept"], true, "{r}");
    assert_eq!(r["same"], true, "{r}");
    assert_eq!(r["value"], "heXYZWllo", "{r}");
    assert_eq!(r["focused"], true, "{r}");
    assert_eq!(r["selection"], json!([1, 4, "backward"]), "{r}");
    assert_eq!(r["order"], json!(["a1", "a2"]), "{r}");
}

const THREAD_LINK: &str = "https://app.t3.codes/threads/env-1/mcp%3Alaunch%3Aabc123";
const APP_LINK: &str = "t3code://threads/env-1/mcp%3Alaunch%3Aabc123";

#[test]
fn only_hosted_thread_links_map_to_the_desktop_scheme() {
    use t3_codemode::desktop::app_link;
    assert_eq!(app_link(THREAD_LINK).as_deref(), Some(APP_LINK));
    for bad in [
        "https://github.com/o/widgets/pull/81",
        "http://app.t3.codes/threads/env-1/t1",
        "https://app.t3.codes/threads/env-1",
        "https://app.t3.codes/threads/env-1/t1/",
        "https://app.t3.codes/threads/env-1/t1/extra",
        "https://app.t3.codes/threads//t1",
        "https://app.t3.codes/threads/env-1/t1?x=1",
        "https://app.t3.codes/threads/env-1/t\"1",
        "https://app.t3.codes.evil/threads/env-1/t1",
    ] {
        assert_eq!(app_link(bad), None, "{bad}");
    }
}

#[tokio::test]
async fn a_thread_link_opens_in_the_running_desktop_app() {
    let s = start(H::new()).await;
    s.h.desktop.running.store(true, std::sync::atomic::Ordering::SeqCst);
    let r = s.post("/api/open", json!({"link": THREAD_LINK}), Some(&s.csrf()), Some(&s.base)).await;
    assert_eq!(r.status(), 200);
    assert_eq!(r.json::<Value>().await.unwrap(), json!({"opened": true}));
    assert_eq!(*s.h.desktop.opened.lock().unwrap(), [APP_LINK]);
}

#[tokio::test]
async fn with_the_app_closed_the_page_is_told_to_open_a_tab() {
    let s = start(H::new()).await;
    let r = s.post("/api/open", json!({"link": THREAD_LINK}), Some(&s.csrf()), None).await;
    assert_eq!(r.json::<Value>().await.unwrap(), json!({"opened": false}));
    assert!(s.h.desktop.opened.lock().unwrap().is_empty());
}

#[tokio::test]
async fn the_tab_setting_never_asks_the_desktop_app() {
    let config = Config::from_value(json!({"board": {"openLinks": "tab"}})).unwrap();
    let s = start(H::build(Vec::new(), config)).await;
    s.h.desktop.running.store(true, std::sync::atomic::Ordering::SeqCst);
    let st: Value = s.http.get(format!("{}/api/state", s.base)).send().await.unwrap().json().await.unwrap();
    assert_eq!(st["service"]["openLinks"], "tab");
    let r = s.post("/api/open", json!({"link": THREAD_LINK}), Some(&s.csrf()), None).await;
    assert_eq!(r.json::<Value>().await.unwrap(), json!({"opened": false}));
    assert!(s.h.desktop.opened.lock().unwrap().is_empty());
}

#[tokio::test]
async fn the_open_route_refuses_other_links_and_missing_csrf() {
    let s = start(H::new()).await;
    s.h.desktop.running.store(true, std::sync::atomic::Ordering::SeqCst);
    let st: Value = s.http.get(format!("{}/api/state", s.base)).send().await.unwrap().json().await.unwrap();
    assert_eq!(st["service"]["openLinks"], "desktop");
    let pr = s.post("/api/open", json!({"link": "https://github.com/o/widgets/pull/81"}), Some(&s.csrf()), None).await;
    assert_eq!(pr.status(), 400);
    assert_eq!(s.post("/api/open", json!({"link": THREAD_LINK}), None, None).await.status(), 403);
    let foreign =
        s.post("/api/open", json!({"link": THREAD_LINK}), Some(&s.csrf()), Some("https://evil.example")).await;
    assert_eq!(foreign.status(), 403);
    assert!(s.h.desktop.opened.lock().unwrap().is_empty());
}

#[test]
fn open_links_accepts_only_desktop_or_tab() {
    assert!(Config::from_value(json!({"board": {"openLinks": "window"}})).is_err());
    assert_eq!(Config::default().board.open_links, t3_codemode::config::OpenLinks::Desktop);
}

async fn two_gate_page() -> (String, Value) {
    let s = start(H::new()).await;
    join_board(&s.h, TICKET, "widgets#41").await;
    join_board(&s.h, WORKER, "widgets#42").await;
    s.h.wf(TICKET, "send", json!({"kind": "gate", "gate": "merge", "body": GATE})).await;
    s.h.wf(WORKER, "send", json!({"kind": "gate", "gate": "merge", "body": GATE})).await;
    let html = s.http.get(format!("{}/", s.base)).send().await.unwrap().text().await.unwrap();
    let st: Value = s.http.get(format!("{}/api/state", s.base)).send().await.unwrap().json().await.unwrap();
    (html, st)
}

const PAGE_HELPERS: &str = r##"
const q = (sel) => document.querySelector(sel);
const card = (id) => q('#cards [data-key="' + id + '"]');
const ids = () => [...document.getElementById("cards").children].map((c) => c.dataset.key);
const answers = () => fetches.filter((f) => /\/(answer|dismiss)$/.test(f.url));
"##;

fn run_page_async(html: &str, scenario: &str) -> Value {
    run_page_async_with(html, "", scenario)
}

fn run_page_async_with(html: &str, prelude: &str, scenario: &str) -> Value {
    let open = html.find("<script>").unwrap() + "<script>".len();
    let close = html.rfind("</script>").unwrap();
    let src = format!(
        "{prelude}\n{}\n{}\nPromise.resolve(({scenario})()).then((v) => {{ globalThis.__out = JSON.stringify(v); }}, (e) => {{ globalThis.__out = JSON.stringify({{ error: String(e) }}); }});",
        include_str!("../support/board_dom.js"),
        &html[open..close]
    );
    let rt = rquickjs::Runtime::new().unwrap();
    let ctx = rquickjs::Context::full(&rt).unwrap();
    ctx.with(|ctx| {
        use rquickjs::CatchResultExt;
        ctx.eval::<(), _>(src).catch(&ctx).unwrap_or_else(|e| panic!("{e}"));
    });
    while rt.is_job_pending() {
        rt.execute_pending_job().unwrap();
    }
    ctx.with(|ctx| {
        let out: String = ctx.eval("globalThis.__out").unwrap();
        serde_json::from_str(&out).unwrap()
    })
}

fn page_scenario(html: &str, st: &Value, body: &str) -> Value {
    run_page(html, &format!("() => {{\n{PAGE_HELPERS}\nconst base = {st};\n{body}\n}}"))
}

#[tokio::test]
async fn the_page_is_a_fixed_shell_with_panels_tabs_and_skip_links() {
    let (html, _) = two_gate_page().await;
    for needle in [
        "height: 100dvh",
        "html, body { height: 100%; margin: 0; overflow: hidden; }",
        r#"role="tablist" aria-label="Ticket lists""#,
        r#"id="tab-open""#,
        r#"id="tab-done""#,
        r#"id="tab-settled""#,
        r#"id="new-cards""#,
        r#"id="new-tix""#,
        r#"id="new-feed""#,
        r#"aria-live="polite""#,
        r#"role="alert""#,
        "Skip to Needs you",
        "Skip to Launch",
        "Skip to Ask",
        "Skip to Tickets",
        "Skip to Activity",
        r#"aria-label="Launch and Ask""#,
    ] {
        assert!(html.contains(needle), "missing {needle}");
    }
    assert!(html.find("Skip to Needs you").unwrap() < html.find("Skip to Launch").unwrap());
    assert!(html.find("Skip to Ask").unwrap() < html.find("Skip to Tickets").unwrap());
    for banned in ["gradient", "btn primary", "backdrop-filter", "requestPermission();\n}, { once: true }"] {
        assert!(!html.contains(banned), "found {banned}");
    }
    for line in html.lines().filter(|l| l.contains("box-shadow")) {
        assert!(line.contains("box-shadow: none"), "{line}");
    }
}

#[tokio::test]
async fn new_cards_wait_behind_the_count_while_the_reader_is_in_the_panel() {
    let (html, st) = two_gate_page().await;
    let r = page_scenario(
        &html,
        &st,
        r##"
emitState(base);
const input = card("a1").querySelector("input");
input.focus();
const blocker = { ...base.cards[1], id: "a9", kind: "blocker" };
emitState({ ...base, cards: [...base.cards, blocker] });
const held = { order: ids(), slot: q("#new-cards").textContent, shown: !q("#new-cards").classList.contains("off"), focused: document.activeElement === input };
q("#new-cards").dispatch("click");
return { held, after: ids(), focus: document.activeElement.dataset.key, slotAfter: q("#new-cards").classList.contains("off") };
"##,
    );
    assert_eq!(
        r["held"],
        json!({"order": ["a1", "a2"], "slot": "1 new - 1 blocker", "shown": true, "focused": true}),
        "{r}"
    );
    assert_eq!(r["after"], json!(["a9", "a1", "a2"]), "{r}");
    assert_eq!(r["focus"], "a9", "{r}");
    assert_eq!(r["slotAfter"], true, "{r}");
}

#[tokio::test]
async fn a_removed_card_keeps_a_placeholder_until_the_panel_is_free() {
    let (html, st) = two_gate_page().await;
    let r = page_scenario(
        &html,
        &st,
        r##"
emitState(base);
card("a2").querySelector("input").focus();
emitState({ ...base, cards: [base.cards[1]] });
const a1 = card("a1");
const held = { gone: a1.classList.contains("gone"), buttons: a1.querySelectorAll("button").length, text: a1.textContent, order: ids() };
document.activeElement = document.body;
emitState({ ...base, cards: [base.cards[1]] });
return { held, after: ids() };
"##,
    );
    assert_eq!(r["held"], json!({"gone": true, "buttons": 0, "text": "a1 resolved", "order": ["a1", "a2"]}), "{r}");
    assert_eq!(r["after"], json!(["a2"]), "{r}");
}

#[tokio::test]
async fn a_reply_leaves_after_five_seconds_and_undo_sends_nothing() {
    let (html, st) = two_gate_page().await;
    let r = page_scenario(
        &html,
        &st,
        r##"
emitState(base);
card("a1").querySelector('[data-opt="1"]').dispatch("click");
const what = card("a1").querySelector('[data-part="what"]').textContent;
const at0 = answers().length;
advance(4999);
const at4999 = answers().length;
advance(1);
const sent = answers().map((f) => [f.url, JSON.parse(f.body)]);
card("a2").querySelector('[data-opt="2"]').dispatch("click");
advance(1000);
card("a2").querySelector('[data-part="undo"]').dispatch("click");
advance(10000);
return { what, at0, at4999, sent, after: answers().length, back: !card("a2").querySelector('[data-part="act"]').hidden };
"##,
    );
    assert_eq!(r["what"], "Sending \u{201c}merge\u{201d} to widgets#41", "{r}");
    assert_eq!((r["at0"].as_i64(), r["at4999"].as_i64()), (Some(0), Some(0)), "{r}");
    assert_eq!(
        r["sent"],
        json!([["/api/cards/a1/answer", {"text": "merge", "at": st["cards"][0]["at"], "token": st["cards"][0]["token"]}]]),
        "{r}"
    );
    assert_eq!(r["after"], 1, "{r}");
    assert_eq!(r["back"], true, "{r}");
}

#[tokio::test]
async fn a_reply_to_a_card_resolved_in_the_undo_window_sends_nothing() {
    let (html, st) = two_gate_page().await;
    let r = page_scenario(
        &html,
        &st,
        r##"
emitState(base);
card("a1").querySelector('[data-opt="1"]').dispatch("click");
emitState({ ...base, cards: base.cards.filter((c) => c.id !== "a1") });
advance(6000);
const c = card("a1");
return {
  sent: answers().length,
  kept: !!c,
  sending: c ? !c.querySelector('[data-part="sending"]').hidden : null,
  focused: document.activeElement === c,
  disabled: c ? [...c.querySelectorAll('[data-part="act"] button[data-opt]')].map((b) => b.getAttribute("aria-disabled")) : null,
  live: document.getElementById("live").textContent,
};
"##,
    );
    assert_eq!(r["sent"], 0, "{r}");
    assert_eq!(r["kept"], true, "{r}");
    assert_eq!(r["sending"], false, "{r}");
    assert_eq!(r["focused"], true, "{r}");
    assert_eq!(r["disabled"], json!(["true", "true"]), "{r}");
    assert_eq!(r["live"], "a1 was already resolved. Nothing was sent.", "{r}");
}

#[tokio::test]
async fn a_reply_waiting_on_undo_is_not_sent_after_the_stream_drops() {
    let (html, st) = two_gate_page().await;
    let r = page_scenario(
        &html,
        &st,
        r##"
emitState(base);
card("a1").querySelector('[data-opt="1"]').dispatch("click");
advance(1000);
eventSource.onerror();
advance(5000);
const sent = answers().length;
const alert = document.getElementById("alert").textContent;
emitState(base);
return { sent, alert, again: !card("a1").querySelector('[data-part="act"]').hidden };
"##,
    );
    assert_eq!(r["sent"], 0, "{r}");
    assert_eq!(r["alert"], "a1 was not sent because the board is offline. Send it again after it reconnects.", "{r}");
    assert_eq!(r["again"], true, "{r}");
}

#[tokio::test]
async fn a_reply_is_not_sent_to_a_new_card_that_reuses_the_id() {
    let (html, st) = two_gate_page().await;
    let r = page_scenario(
        &html,
        &st,
        r##"
emitState(base);
card("a1").querySelector('[data-opt="1"]').dispatch("click");
emitState({ ...base, cards: base.cards.map((c) => c.id === "a1" ? { ...c, at: "2099-01-01T00:00:00Z" } : c) });
advance(6000);
return { sent: answers().length, live: document.getElementById("live").textContent };
"##,
    );
    assert_eq!(r["sent"], 0, "{r}");
    assert_eq!(r["live"], "a1 was already resolved. Nothing was sent.", "{r}");
}

#[tokio::test]
async fn a_reply_is_not_sent_when_the_card_gets_a_new_handle() {
    let (html, st) = two_gate_page().await;
    let r = page_scenario(
        &html,
        &st,
        r##"
emitState(base);
card("a1").querySelector('[data-opt="1"]').dispatch("click");
emitState({ ...base, cards: base.cards.map((c) => c.id === "a1" ? { ...c, handle: "m99" } : c) });
const act = !card("a1").querySelector('[data-part="act"]').hidden;
const focused = document.activeElement === card("a1");
advance(6000);
return { act, focused, sent: answers().length, live: document.getElementById("live").textContent };
"##,
    );
    assert_eq!(r["act"], true, "{r}");
    assert_eq!(r["focused"], true, "{r}");
    assert_eq!(r["sent"], 0, "{r}");
    assert_eq!(r["live"], "a1 was already resolved. Nothing was sent.", "{r}");
}

#[tokio::test]
async fn closing_an_action_error_returns_focus_to_the_action_or_the_composer() {
    let (html, st) = two_gate_page().await;
    let r = run_page_async(
        &html,
        &format!(
            "async () => {{\n{PAGE_HELPERS}\nconst base = {st};\n{}\n}}",
            r##"
emitState(base);
fetchReply = () => Promise.reject(new Error("settle failed"));
const composer = document.getElementById("composer");
const b = document.createElement("button");
composer.append(b);
const err = document.getElementById("composer-err");
const close = document.getElementById("composer-err-close");
await act(b, "/api/asks/q1/settle");
const shown = !err.hidden;
close.focus();
close.dispatch("click");
const back = document.activeElement === b;
await act(b, "/api/asks/q1/settle");
b.disabled = true;
close.focus();
close.dispatch("click");
const unfocusable = document.activeElement === document.getElementById("issues");
b.disabled = false;
await act(b, "/api/asks/q1/settle");
b.remove();
close.focus();
close.dispatch("click");
return { shown, back, unfocusable, hidden: err.hidden, fallback: document.activeElement === document.getElementById("issues") };
"##
        ),
    );
    assert_eq!(r["shown"], true, "{r}");
    assert_eq!(r["back"], true, "{r}");
    assert_eq!(r["unfocusable"], true, "{r}");
    assert_eq!(r["hidden"], true, "{r}");
    assert_eq!(r["fallback"], true, "{r}");
}

#[tokio::test]
async fn a_late_message_for_an_old_handle_never_replaces_the_new_one() {
    let (html, st) = two_gate_page().await;
    let r = run_page_async(
        &html,
        &format!(
            "async () => {{\n{PAGE_HELPERS}\nconst base = {st};\n{}\n}}",
            r##"
const waiting = [];
fetchReply = () => new Promise((resolve) => waiting.push(resolve));
const reply = (body) => ({ ok: true, json: async () => ({ body }) });
const tick = async () => { for (let i = 0; i < 5; i++) await Promise.resolve(); };
emitState(base);
const c = card("a1");
const btn = c.querySelector('[data-part="fullbtn"]');
const pre = c.querySelector('[data-part="full"]').firstChild;
btn.dispatch("click");
emitState({ ...base, cards: base.cards.map((x) => x.id === "a1" ? { ...x, handle: "m99" } : x) });
btn.dispatch("click");
waiting[1](reply("new body"));
await tick();
waiting[0](reply("old body"));
await tick();
const shown = pre.textContent;
btn.dispatch("click");
btn.dispatch("click");
return { shown, again: pre.textContent, msgs: fetches.filter((f) => f.url.startsWith("/api/msg/")).map((f) => f.url) };
"##
        ),
    );
    assert_eq!(r["shown"], "new body", "{r}");
    assert_eq!(r["again"], "new body", "{r}");
    assert_eq!(r["msgs"].as_array().unwrap().len(), 2, "{r}");
    assert_eq!(r["msgs"][1], "/api/msg/m99", "{r}");
}

#[tokio::test]
async fn a_card_id_reused_by_a_new_decision_drops_the_old_draft_and_message() {
    let (html, st) = two_gate_page().await;
    let r = page_scenario(
        &html,
        &st,
        r##"
emitState(base);
const c = card("a1");
c.querySelector('[data-part="replybtn"]').dispatch("click");
typeKeys("for the old gate");
c.querySelector('[data-part="fullbtn"]').dispatch("click");
const reused = { ...base, cards: base.cards.map((x) => x.id === "a1" ? { ...x, at: "2099-01-01T00:00:00Z", handle: "m99" } : x) };
emitState(reused);
const after = {
  same: card("a1") === c,
  value: c.querySelector("input").value,
  reply: c.querySelector('[data-part="reply"]').hidden,
  full: c.querySelector('[data-part="full"]').hidden,
  expanded: c.querySelector('[data-part="fullbtn"]').getAttribute("aria-expanded"),
};
emitState(reused);
c.querySelector('[data-part="fullbtn"]').dispatch("click");
return { ...after, kept: c.querySelector("input").value, msgs: fetches.filter((f) => f.url.startsWith("/api/msg/")).map((f) => f.url) };
"##,
    );
    assert_eq!(r["same"], true, "{r}");
    assert_eq!(r["value"], "", "{r}");
    assert_eq!(r["reply"], true, "{r}");
    assert_eq!(r["full"], true, "{r}");
    assert_eq!(r["expanded"], "false", "{r}");
    assert_eq!(r["kept"], "", "{r}");
    assert_eq!(r["msgs"].as_array().unwrap().len(), 2, "{r}");
    assert_eq!(r["msgs"][1], "/api/msg/m99", "{r}");
}

#[tokio::test]
async fn a_sent_reply_does_not_hold_a_new_card_that_reuses_the_id() {
    let (html, st) = two_gate_page().await;
    let r = page_scenario(
        &html,
        &st,
        r##"
emitState(base);
card("a1").querySelector('[data-opt="1"]').dispatch("click");
advance(5000);
const sent = answers().length;
emitState({ ...base, cards: base.cards.map((c) => c.id === "a1" ? { ...c, at: "2099-01-01T00:00:00Z" } : c) });
return { sent, act: !card("a1").querySelector('[data-part="act"]').hidden };
"##,
    );
    assert_eq!(r["sent"], 1, "{r}");
    assert_eq!(r["act"], true, "{r}");
}

#[tokio::test]
async fn a_restore_that_finishes_while_its_row_is_stale_stays_disabled() {
    let (html, st) = two_gate_page().await;
    let r = run_page_async(
        &html,
        &format!(
            "async () => {{\n{PAGE_HELPERS}\nconst base = {st};\n{}\n}}",
            r##"
const waiting = [];
fetchReply = () => new Promise((resolve) => waiting.push(resolve));
const T = { thread: "th-7", label: "acme/widgets#7", settledAt: "2026-01-01T00:00:00Z" };
const withT = { ...base, settled: { tickets: [T], asks: [] } };
emitState(withT);
const b = q('#settled [data-key="t:th-7"] [data-part="restore"]');
b.focus();
b.dispatch("click");
emitState({ ...base, settled: { tickets: [], asks: [] } });
waiting[0]({ ok: true, json: async () => ({}) });
for (let i = 0; i < 5; i++) await Promise.resolve();
const stale = b.getAttribute("aria-disabled");
emitState(withT);
return { stale, back: b.getAttribute("aria-disabled") };
"##
        ),
    );
    assert_eq!(r["stale"], "true", "{r}");
    assert_eq!(r["back"], serde_json::Value::Null, "{r}");
}

#[tokio::test]
async fn a_card_replaced_while_typing_a_reply_keeps_keyboard_focus() {
    let (html, st) = two_gate_page().await;
    let r = page_scenario(
        &html,
        &st,
        r##"
emitState(base);
const c = card("a1");
c.querySelector('[data-part="replybtn"]').dispatch("click");
typeKeys("for the old gate");
emitState({ ...base, cards: base.cards.map((x) => x.id === "a1" ? { ...x, at: "2099-01-01T00:00:00Z" } : x) });
return { focused: document.activeElement === c, value: c.querySelector("input").value };
"##,
    );
    assert_eq!(r["focused"], true, "{r}");
    assert_eq!(r["value"], "", "{r}");
}

#[tokio::test]
async fn a_resolution_note_stays_with_the_decision_that_produced_it() {
    let (html, st) = two_gate_page().await;
    let r = run_page_async(
        &html,
        &format!(
            "async () => {{\n{PAGE_HELPERS}\nconst base = {st};\n{}\n}}",
            r##"
fetchReply = async () => ({ ok: true, json: async () => ({}) });
emitState(base);
card("a1").querySelector('[data-opt="1"]').dispatch("click");
advance(5000);
for (let i = 0; i < 5; i++) await Promise.resolve();
emitState({ ...base, cards: base.cards.map((x) => x.id === "a1" ? { ...x, at: "2099-01-01T00:00:00Z" } : x) });
card("a2").focus();
emitState({ ...base, cards: base.cards.filter((x) => x.id !== "a1") });
return { note: card("a1").textContent };
"##
        ),
    );
    assert_eq!(r["note"], "a1 resolved", "{r}");
}

#[tokio::test]
async fn a_pending_launch_row_is_not_given_to_one_of_two_threads_with_its_label() {
    let (html, st) = two_gate_page().await;
    let r = page_scenario(
        &html,
        &st,
        r##"
const P = { ...base.tree[0], thread: null, label: "acme/widgets#9", status: "running", done: false, gate: null, last: null };
emitState({ ...base, tree: [...base.tree, P] });
const row = q('#open [data-key="pending:acme/widgets#9"]');
q("#open .tk").focus();
emitState({ ...base, tree: [...base.tree, { ...P, thread: "th-9" }, { ...P, thread: "th-10" }] });
return { key: row.getAttribute("data-key"), gone: row.classList.contains("gone") };
"##,
    );
    assert_eq!(r["key"], "pending:acme/widgets#9", "{r}");
    assert_eq!(r["gone"], true, "{r}");
}

#[tokio::test]
async fn the_composer_error_strip_moves_with_the_dock() {
    let (html, _) = two_gate_page().await;
    let dock = html.find(r#"id="dock""#).unwrap();
    let err = html.find(r#"id="composer-err""#).unwrap();
    let forms = html.find(r#"class="forms""#).unwrap();
    assert!(dock < err && err < forms);
}

#[tokio::test]
async fn a_settled_row_that_comes_back_can_be_restored_again() {
    let (html, st) = two_gate_page().await;
    let r = page_scenario(
        &html,
        &st,
        r##"
const T = { thread: "th-7", label: "acme/widgets#7", settledAt: "2026-01-01T00:00:00Z" };
const withT = { ...base, settled: { tickets: [T], asks: [] } };
emitState(withT);
const b = q('#settled [data-key="t:th-7"] [data-part="restore"]');
b.focus();
emitState({ ...base, settled: { tickets: [], asks: [] } });
const stale = b.getAttribute("aria-disabled");
emitState(withT);
b.dispatch("click");
return { stale, after: b.getAttribute("aria-disabled"), posts: fetches.filter((f) => /\/restore$/.test(f.url)).length };
"##,
    );
    assert_eq!(r["stale"], "true", "{r}");
    assert_eq!(r["posts"], 1, "{r}");
}

#[tokio::test]
async fn a_launch_keeps_its_row_when_it_gets_a_thread() {
    let (html, st) = two_gate_page().await;
    let r = page_scenario(
        &html,
        &st,
        r##"
const P = { ...base.tree[0], thread: null, label: "acme/widgets#9", status: "running", done: false, gate: null, last: null };
emitState({ ...base, tree: [...base.tree, P] });
const row = q('#open [data-key="pending:acme/widgets#9"]');
q("#open .tk").focus();
emitState({ ...base, tree: [...base.tree, { ...P, thread: "th-9" }] });
return {
  same: q('#open [data-key="th-9"]') === row,
  gone: row.classList.contains("gone"),
  held: !q("#new-tix").classList.contains("off"),
};
"##,
    );
    assert_eq!(r["same"], true, "{r}");
    assert_eq!(r["gone"], false, "{r}");
    assert_eq!(r["held"], false, "{r}");
}

#[tokio::test]
async fn board_shortcuts_wait_while_the_shortcuts_dialog_is_open() {
    let (html, st) = two_gate_page().await;
    let r = page_scenario(
        &html,
        &st,
        r##"
emitState(base);
const before = document.getElementById("app").dataset.panel;
document.getElementById("keys").open = true;
const close = document.getElementById("keys-close");
for (const key of ["g", "t"]) for (const f of document.listeners.keydown) f({ key, target: close, preventDefault() {} });
return { before, after: document.getElementById("app").dataset.panel };
"##,
    );
    assert_eq!(r["after"], r["before"], "{r}");
}

#[tokio::test]
async fn options_have_equal_weight_and_digits_only_focus_them() {
    let (html, st) = two_gate_page().await;
    let r = page_scenario(
        &html,
        &st,
        r##"
emitState(base);
const opts = [...document.querySelectorAll("#cards button[data-opt]")];
card("a1").focus();
for (const f of document.listeners.keydown) f({ key: "2", target: card("a1"), preventDefault() {} });
advance(10000);
return {
  classes: [...new Set(opts.map((b) => b.className))],
  names: opts.map((b) => b.getAttribute("aria-label")),
  focused: document.activeElement.textContent,
  sent: answers().length,
};
"##,
    );
    assert_eq!(r["classes"], json!(["btn answer"]), "{r}");
    assert_eq!(r["names"][0], "merge, reply to a1 widgets#41", "{r}");
    assert_eq!(r["names"][3], "merge and deploy, reply to a2 widgets#42", "{r}");
    assert_eq!(r["focused"], "merge and deploy", "{r}");
    assert_eq!(r["sent"], 0, "{r}");
}

#[tokio::test]
async fn ticket_rows_update_in_place_and_finished_tickets_move_to_done() {
    let (html, st) = two_gate_page().await;
    let r = page_scenario(
        &html,
        &st,
        &format!(
            r##"
const T = "{TICKET}";
const row = (list) => q("#" + list + ' [data-thread="' + T + '"]');
emitState(base);
const label = row("open").querySelector('[data-part="label"]');
const noted = base.tree.map((t) => t.thread === T ? {{ ...t, last: {{ handle: "m9", kind: "note", head: "**ACTION NEEDED:** build green", at: "10:15 MST" }} }} : t);
emitState({{ ...base, cards: [], tree: noted }});
const kept = row("open").querySelector('[data-part="label"]') === label;
const note = row("open").querySelector('[data-part="note"]').textContent;
q("#open").children.find((el) => el.dataset.key && el.dataset.thread !== T).focus();
const done = noted.map((t) => t.thread === T ? {{ ...t, done: true, status: "done" }} : t);
emitState({{ ...base, cards: [], tree: done }});
return {{
  kept, note,
  placeholder: row("open").textContent,
  inDone: !!row("done"),
  settle: row("done").querySelector('[data-part="settle"]').getAttribute("aria-label"),
  doneCount: q("#n-done").textContent,
}};
"##
        ),
    );
    assert_eq!(r["kept"], true, "{r}");
    assert_eq!(r["note"], "Build green", "{r}");
    assert_eq!(r["placeholder"], "widgets#41 finished and moved to Done", "{r}");
    assert_eq!(r["inDone"], true, "{r}");
    assert_eq!(r["settle"], "Settle widgets#41", "{r}");
    assert_eq!(r["doneCount"], "1", "{r}");
}

#[tokio::test]
async fn a_needs_you_row_links_to_its_card() {
    let (html, st) = two_gate_page().await;
    let r = page_scenario(
        &html,
        &st,
        &format!(
            r##"
emitState(base);
const needs = q('#open [data-thread="{TICKET}"] [data-part="needs"]');
needs.dispatch("click");
const toCard = document.activeElement.dataset.key;
card("a1").querySelector('[data-part="id"]').dispatch("click");
return {{ text: needs.textContent, hidden: needs.hidden, toCard, back: document.activeElement.dataset.thread }};
"##
        ),
    );
    assert_eq!(r["text"], "Choose", "{r}");
    assert_eq!(r["hidden"], false, "{r}");
    assert_eq!(r["toCard"], "a1", "{r}");
    assert_eq!(r["back"], TICKET, "{r}");
}

#[tokio::test]
async fn a_dropped_stream_marks_the_board_stale_and_holds_replies() {
    let (html, st) = two_gate_page().await;
    let r = page_scenario(
        &html,
        &st,
        r##"
emitState(base);
eventSource.onerror();
const opts = [...document.querySelectorAll("#cards [data-opt]")];
opts[0].dispatch("click");
advance(6000);
const during = {
  stale: document.body.classList.contains("stale"),
  header: q("#svc-text").textContent,
  disabled: opts.every((b) => b.getAttribute("aria-disabled") === "true"),
  sent: answers().length,
};
emitState(base);
return { during, after: { stale: document.body.classList.contains("stale"), header: q("#svc-text").textContent } };
"##,
    );
    assert_eq!(r["during"]["stale"], true, "{r}");
    assert!(r["during"]["header"].as_str().unwrap().starts_with("Disconnected"), "{r}");
    assert_eq!(r["during"]["disabled"], true, "{r}");
    assert_eq!(r["during"]["sent"], 0, "{r}");
    assert_eq!(r["after"]["stale"], false, "{r}");
    assert!(r["after"]["header"].as_str().unwrap().starts_with("T3 connected"), "{r}");
}

fn worker_pr(h: &H) -> Value {
    let tree = h.app.wf.tree().unwrap();
    let t = tree.iter().find(|t| t["label"] == "widgets#41").unwrap();
    t["workers"][0]["pr"].clone()
}

fn pr_lists(h: &H) -> usize {
    h.fake.calls().iter().filter(|c| *c == "list_thread_pull_requests").count()
}

async fn ticket_with_worker(h: &H) {
    join_board(h, TICKET, "widgets#41").await;
    h.wf(WORKER, "join", json!({"parent": TICKET, "label": "#41 PR 1/2", "role": "worker"})).await;
}

#[tokio::test]
async fn a_finished_workers_merged_pr_shows_on_the_board() {
    let h = H::new();
    ticket_with_worker(&h).await;
    h.fake.set_prs(WORKER, vec![pull_request("acme/widgets", 81, "open")]);
    h.tick(1).await;
    assert_eq!(worker_pr(&h)["state"], "open");
    assert_eq!(pr_lists(&h), 0);
    h.fake.set_prs(WORKER, vec![pull_request("acme/widgets", 81, "merged")]);
    h.wf(WORKER, "send", json!({"kind": "handoff", "body": "merged at abc123"})).await;
    let pr = worker_pr(&h);
    assert_eq!(
        (pr["number"].clone(), pr["state"].clone(), pr["url"].clone()),
        (json!(81), json!("merged"), json!("https://github.com/acme/widgets/pull/81"))
    );
    let tree = h.app.wf.tree().unwrap();
    assert_eq!(tree[0]["workers"][0]["done"], true);
    h.tick(2).await;
    h.app.wf.board_tick().await.unwrap();
    assert_eq!(pr_lists(&h), 1);
}

#[tokio::test]
async fn a_finished_workers_closed_pr_shows_after_a_restart() {
    let mut h = H::new();
    ticket_with_worker(&h).await;
    h.fake.set_prs(WORKER, vec![pull_request("acme/widgets", 82, "closed")]);
    h.wf(WORKER, "send", json!({"kind": "closed", "body": "abandoned"})).await;
    assert_eq!(worker_pr(&h)["state"], "closed");
    h.reopen();
    assert_eq!(worker_pr(&h), Value::Null);
    h.app.wf.board_tick().await.unwrap();
    assert_eq!((worker_pr(&h)["number"].clone(), worker_pr(&h)["state"].clone()), (json!(82), json!("closed")));
}

#[tokio::test]
async fn a_reopened_pr_is_listed_again_when_it_closes() {
    let h = H::new();
    ticket_with_worker(&h).await;
    h.fake.set_prs(WORKER, vec![pull_request("acme/widgets", 83, "closed")]);
    h.tick(3).await;
    assert_eq!(worker_pr(&h)["state"], "closed");
    h.fake.set_prs(WORKER, vec![pull_request("acme/widgets", 83, "open")]);
    h.tick(4).await;
    assert_eq!(worker_pr(&h)["state"], "open");
    h.fake.set_prs(WORKER, vec![pull_request("acme/widgets", 83, "merged")]);
    h.tick(5).await;
    assert_eq!(worker_pr(&h)["state"], "merged");
    assert_eq!(pr_lists(&h), 2);
}

#[tokio::test]
async fn a_failed_pr_lookup_is_retried_on_the_next_board_tick() {
    let h = H::new();
    ticket_with_worker(&h).await;
    h.fake.set_prs(WORKER, vec![pull_request("acme/widgets", 84, "merged")]);
    h.fake.set_prs_fail(true);
    h.wf(WORKER, "send", json!({"kind": "handoff", "body": "merged at abc123"})).await;
    assert_eq!(worker_pr(&h), Value::Null);
    h.app.wf.board_tick().await.unwrap();
    assert_eq!(worker_pr(&h), Value::Null);
    h.fake.set_prs_fail(false);
    h.app.wf.board_tick().await.unwrap();
    assert_eq!(worker_pr(&h)["state"], "merged");
    h.app.wf.board_tick().await.unwrap();
    assert_eq!(pr_lists(&h), 3);
}

#[tokio::test]
async fn a_failed_read_at_handoff_is_retried_on_the_next_board_tick() {
    let h = H::new();
    ticket_with_worker(&h).await;
    h.fake.set_prs(WORKER, vec![pull_request("acme/widgets", 85, "open")]);
    h.tick(1).await;
    assert_eq!(worker_pr(&h)["state"], "open");
    h.fake.set_prs(WORKER, vec![pull_request("acme/widgets", 85, "merged")]);
    h.fake.set_read_fails(true);
    h.wf(WORKER, "send", json!({"kind": "handoff", "body": "merged at abc123"})).await;
    h.fake.set_read_fails(false);
    h.app.wf.board_tick().await.unwrap();
    assert_eq!(worker_pr(&h)["state"], "merged");
}

#[tokio::test]
async fn a_closed_pr_that_reopens_and_merges_between_reads_shows_merged() {
    let h = H::new();
    ticket_with_worker(&h).await;
    h.fake.set_prs(WORKER, vec![pull_request("acme/widgets", 86, "closed")]);
    h.tick(1).await;
    assert_eq!(worker_pr(&h)["state"], "closed");
    h.fake.set_prs(WORKER, vec![pull_request("acme/widgets", 86, "merged")]);
    h.tick(2).await;
    assert_eq!(worker_pr(&h)["state"], "merged");
    h.tick(3).await;
    assert_eq!(pr_lists(&h), 2);
}

#[tokio::test]
async fn worker_prs_are_read_across_every_page() {
    let h = H::new();
    ticket_with_worker(&h).await;
    h.fake.set_pr_page(2);
    h.fake.set_prs(
        WORKER,
        vec![
            pull_request("acme/widgets", 87, "merged"),
            pull_request("acme/widgets", 88, "merged"),
            pull_request("acme/widgets", 89, "merged"),
        ],
    );
    h.tick(1).await;
    assert_eq!((worker_pr(&h)["number"].clone(), worker_pr(&h)["state"].clone()), (json!(89), json!("merged")));
    assert_eq!(pr_lists(&h), 2);
}

#[tokio::test(start_paused = true)]
async fn a_stalled_pr_lookup_gives_up_and_is_retried() {
    let h = H::new();
    ticket_with_worker(&h).await;
    h.fake.set_prs(WORKER, vec![pull_request("acme/widgets", 90, "merged")]);
    h.fake.set_delay(Duration::from_secs(3600));
    h.tick(1).await;
    assert_eq!(worker_pr(&h), Value::Null);
    h.fake.set_delay(Duration::ZERO);
    h.tick(2).await;
    assert_eq!(worker_pr(&h)["state"], "merged");
}

#[tokio::test]
async fn a_pr_that_reopens_between_the_read_and_the_list_is_listed_again() {
    let h = H::new();
    ticket_with_worker(&h).await;
    h.fake.set_prs(WORKER, vec![pull_request("acme/widgets", 91, "closed")]);
    h.fake.set_prs_before_list(WORKER, vec![pull_request("acme/widgets", 91, "open")]);
    h.tick(1).await;
    assert_eq!(worker_pr(&h)["state"], "open");
    h.fake.set_prs(WORKER, vec![pull_request("acme/widgets", 91, "merged")]);
    h.tick(2).await;
    assert_eq!(worker_pr(&h)["state"], "merged");
}

#[tokio::test]
async fn a_worker_that_finishes_with_an_open_pr_shows_its_merge() {
    let h = H::new();
    ticket_with_worker(&h).await;
    h.fake.set_prs(WORKER, vec![pull_request("acme/widgets", 92, "closed")]);
    h.fake.set_prs_before_list(WORKER, vec![pull_request("acme/widgets", 92, "open")]);
    h.wf(WORKER, "send", json!({"kind": "handoff", "body": "ready to merge"})).await;
    assert_eq!(worker_pr(&h)["state"], "open");
    h.fake.set_prs(WORKER, vec![pull_request("acme/widgets", 92, "merged")]);
    h.app.wf.board_tick().await.unwrap();
    assert_eq!(worker_pr(&h)["state"], "merged");
    let reads = |h: &H| h.fake.calls().iter().filter(|c| *c == "t3_thread_read").count();
    let before = reads(&h);
    h.app.wf.board_tick().await.unwrap();
    assert_eq!(reads(&h), before);
}

fn by_label<'a>(items: &'a Value, label: &str) -> &'a Value {
    items.as_array().unwrap().iter().find(|t| t["label"] == label).unwrap_or_else(|| panic!("no {label} in {items}"))
}

#[tokio::test]
async fn the_state_carries_each_tickets_repo_project_and_title() {
    let mut h = H::new();
    let mut gadgets = project("p-gadgets", "/src/gadgets");
    gadgets["title"] = json!("Gadgets");
    gadgets["repositoryIdentity"] = json!({
        "canonicalKey": "github.com/acme/gadgets", "owner": "acme", "name": "gadgets",
        "locator": {"source": "git-remote", "remoteName": "origin", "remoteUrl": "https://github.com/acme/gadgets.git"},
    });
    h.fake.set_projects(vec![project("p-widgets", "/src/widgets"), gadgets]);
    h.remotes.set("/src/widgets", "git@github.com:acme/widgets.git");
    h.fake.update(TICKET, json!({"projectId": "p-widgets", "title": "widgets#41: Fix the grader"}));
    h.fake.update(WORKER, json!({"projectId": "p-gadgets"}));
    join_board(&h, TICKET, "widgets#41").await;
    join_board(&h, WORKER, "gadgets#7").await;
    h.wf(TICKET, "send", json!({"kind": "gate", "gate": "merge", "body": GATE})).await;
    h.wf(WORKER, "send", json!({"kind": "event", "body": "PR opened"})).await;
    h.app.wf.board_tick().await.unwrap();
    let st = snapshot(&h);
    let w = by_label(&st["tree"], "widgets#41");
    assert_eq!(w["repo"], "acme/widgets", "{w}");
    assert_eq!(w["project"], json!({"id": "p-widgets", "title": "p-widgets"}));
    assert_eq!(w["title"], "widgets#41: Fix the grader");
    let g = by_label(&st["tree"], "gadgets#7");
    assert_eq!(g["repo"], "acme/gadgets", "{g}");
    assert_eq!(g["project"], json!({"id": "p-gadgets", "title": "Gadgets"}));
    assert_eq!(st["cards"][0]["ticket"], TICKET);
    assert_eq!(st["cards"][0]["repo"], "acme/widgets");
    assert_eq!(st["cards"][0]["project"]["id"], "p-widgets");
    let f = by_label(&st["feed"], "gadgets#7");
    assert_eq!((&f["ticket"], &f["repo"]), (&json!(WORKER), &json!("acme/gadgets")), "{f}");
    assert_eq!((&f["ticketLabel"], &f["ticketTitle"]), (&json!("gadgets#7"), &json!(WORKER)), "{f}");
    h.reopen();
    assert_eq!(by_label(&snapshot(&h)["tree"], "gadgets#7")["project"]["id"], "p-gadgets");
    h.app.wf.board_tick().await.unwrap();
    let again = snapshot(&h);
    assert_eq!(by_label(&again["tree"], "gadgets#7")["repo"], "acme/gadgets");
    assert_eq!(by_label(&again["tree"], "widgets#41")["repo"], "acme/widgets");
    assert_eq!(by_label(&again["tree"], "widgets#41")["title"], "widgets#41: Fix the grader");
}

const FILTER_HELPERS: &str = r##"
const $$ = (id) => document.getElementById(id);
const keysOf = (id) => [...$$(id).children].filter((e) => e.dataset.key && !e.classList.contains("gone")).map((e) => e.dataset.key);
const facet = (key) => q('[data-facet="' + key + '"]');
const pick = (key, val) => {
  if (facet(key).querySelector('[data-part="fpop"]').hidden) facet(key).querySelector('[data-part="fbtn"]').dispatch("click");
  const box = facet(key).querySelector('[data-val="' + val + '"] input');
  box.checked = !box.checked;
  box.dispatch("change");
};
const view = () => ({
  cards: ids(), open: keysOf("open"), done: keysOf("done"), feed: keysOf("feed"), settled: keysOf("settled"),
  n: ["n-cards", "n-open", "n-done", "n-feed", "n-settled"].map((id) => $$(id).textContent),
});
"##;

fn filter_scenario(html: &str, st: &Value, body: &str) -> Value {
    page_scenario(html, st, &format!("{}\n{body}", filter_setup()))
}

fn filter_setup() -> String {
    format!(
        r##"{FILTER_HELPERS}
const T = "{TICKET}", W = "{WORKER}";
const repoOf = (t) => t === T ? "acme/widgets" : "acme/gadgets";
const place = (t) => ({{ repo: repoOf(t), project: {{ id: "p-" + repoOf(t).slice(5), title: repoOf(t).slice(5) }} }});
const s0 = {{
  ...base,
  tree: base.tree.map((t) => ({{ ...t, ...place(t.thread), title: t.thread === T ? "widgets#41: Fix the grader" : "widgets#42: Ship the gadget" }})),
  cards: base.cards.map((c) => ({{ ...c, ...place(c.ticket) }})),
  feed: [
    {{ handle: "m91", at: "10:00 MST", iso: "2026-11-17T17:00:00Z", label: "widgets#41", kind: "note", head: "tests green", ticket: T, ...place(T) }},
    {{ handle: "m92", at: "10:01 MST", iso: "2026-11-17T17:01:00Z", label: "widgets#42", kind: "event", head: "PR opened", ticket: W, ...place(W) }},
  ],
  settled: {{
    tickets: [{{ thread: "th-s", label: "gadgets#3", status: "done", done: true, settledAt: "09:00 MST", pr: null, link: null, ...place(W) }}],
    asks: [{{ id: "q1", seq: 1, question: "Which tickets wait?", answer: "None", status: "answered" }}],
  }},
  counts: {{ ...base.counts, settled: 2 }},
}};"##
    )
}

#[tokio::test]
async fn a_project_filter_narrows_every_list_and_shows_visible_counts() {
    let (html, st) = two_gate_page().await;
    let r = filter_scenario(
        &html,
        &st,
        r##"
emitState(s0);
const all = view();
facet("projects").querySelector('[data-part="fbtn"]').dispatch("click");
const opts = [...facet("projects").querySelectorAll(".fopt")].map((o) => o.dataset.val);
const find = facet("projects").querySelector('[data-part="ffind"]');
find.value = "gadg";
find.dispatch("input");
const found = [...facet("projects").querySelectorAll(".fopt")].filter((o) => !o.hidden).map((o) => o.dataset.val);
find.value = "";
find.dispatch("input");
pick("projects", "p-widgets");
const one = { ...view(), btn: facet("projects").querySelector('[data-part="fbtn"]').textContent, clear: !$$("filter-clear").hidden };
pick("projects", "p-gadgets");
const two = view();
$$("filter-clear").dispatch("click");
return { all, opts, found, one, two, after: view(), btn: facet("projects").querySelector('[data-part="fbtn"]').textContent };
"##,
    );
    let t = json!(TICKET);
    let w = json!(WORKER);
    assert_eq!(r["all"]["cards"], json!(["a1", "a2"]), "{r}");
    assert_eq!(r["all"]["n"], json!(["2", "2", "0", "2", "2"]), "{r}");
    assert_eq!(r["opts"], json!(["p-gadgets", "p-widgets"]), "{r}");
    assert_eq!(r["found"], json!(["p-gadgets"]), "{r}");
    assert_eq!(r["one"]["cards"], json!(["a1"]), "{r}");
    assert_eq!(r["one"]["open"], json!([t]), "{r}");
    assert_eq!(r["one"]["feed"], json!(["m91"]), "{r}");
    assert_eq!(r["one"]["settled"], json!([]), "{r}");
    assert_eq!(r["one"]["n"], json!(["1/2", "1/2", "0/0", "1/2", "0/2"]), "{r}");
    assert_eq!(r["one"]["btn"], "Project: acme/widgets", "{r}");
    assert_eq!(r["one"]["clear"], true, "{r}");
    assert_eq!(r["two"]["cards"], json!(["a1", "a2"]), "{r}");
    assert_eq!(r["two"]["settled"], json!(["t:th-s"]), "{r}");
    assert_eq!(r["after"]["open"].as_array().unwrap().len(), 2, "{r}");
    assert!(r["after"]["open"].as_array().unwrap().contains(&w), "{r}");
    assert_eq!(r["after"]["n"], r["all"]["n"], "{r}");
    assert_eq!(r["btn"], "Project", "{r}");
}

#[tokio::test]
async fn the_search_box_narrows_after_a_pause_and_esc_clears_it() {
    let (html, st) = two_gate_page().await;
    let r = filter_scenario(
        &html,
        &st,
        r##"
emitState(s0);
const box = $$("filter-q");
box.focus();
typeKeys("42");
const typing = ids();
advance(200);
const number = view();
box.value = "";
typeKeys("GRADER fix");
advance(200);
const words = view();
box.value = "";
typeKeys("gadgets");
advance(200);
const repo = view();
const old = { handle: "m93", at: "09:00 MST", iso: "2026-11-17T16:00:00Z", label: "PR 1/2", kind: "note", head: "rebased", ticket: "th-old", ticketLabel: "gizmos#5", ticketTitle: "gizmos#5: Tune the gizmo", repo: null, project: null };
emitState({ ...s0, feed: [...s0.feed, old] });
box.value = "";
typeKeys("tune");
advance(200);
const parent = view().feed;
box.dispatch("keydown", { key: "Escape", stopPropagation() {} });
return { typing, number, words, repo, parent, cleared: { value: box.value, ...view() }, focused: document.activeElement === box };
"##,
    );
    let t = json!(TICKET);
    let w = json!(WORKER);
    assert_eq!(r["typing"], json!(["a1", "a2"]), "{r}");
    assert_eq!(r["number"]["cards"], json!(["a2"]), "{r}");
    assert_eq!(r["number"]["open"], json!([w]), "{r}");
    assert_eq!(r["number"]["feed"], json!(["m92"]), "{r}");
    assert_eq!(r["words"]["cards"], json!(["a1"]), "{r}");
    assert_eq!(r["words"]["open"], json!([t]), "{r}");
    assert_eq!(r["repo"]["cards"], json!(["a2"]), "{r}");
    assert_eq!(r["repo"]["settled"], json!(["t:th-s"]), "{r}");
    assert_eq!(r["parent"], json!(["m93"]), "{r}");
    assert_eq!(r["cleared"]["value"], "", "{r}");
    assert_eq!(r["cleared"]["cards"], json!(["a1", "a2"]), "{r}");
    assert_eq!(r["cleared"]["settled"], json!(["t:th-s", "a:q1"]), "{r}");
    assert_eq!(r["focused"], true, "{r}");
}

#[tokio::test]
async fn a_new_card_outside_the_filter_shows_as_hidden() {
    let (html, st) = two_gate_page().await;
    let r = filter_scenario(
        &html,
        &st,
        r##"
emitState(s0);
pick("projects", "p-widgets");
const before = { hidden: $$("hidden-new").hidden };
const a3 = { ...s0.cards[1], id: "a3", at: "2026-11-17T12:30:00Z", kind: "blocker", token: "t-a3" };
emitState({ ...s0, cards: [...s0.cards, a3] });
const after = { hidden: $$("hidden-new").hidden, text: $$("hidden-new").textContent, urgent: $$("hidden-new").getAttribute("data-urgent"), cards: ids(), n: $$("n-cards").textContent };
emitState({ ...s0, cards: [s0.cards[0], { ...s0.cards[1], token: "t-new" }, a3] });
const replaced = $$("hidden-new").textContent;
$$("hidden-new").dispatch("click");
return { before, after, replaced, shown: ids(), still: $$("hidden-new").hidden };
"##,
    );
    assert_eq!(r["before"]["hidden"], true, "{r}");
    assert_eq!(
        r["after"],
        json!({"hidden": false, "text": "1 hidden", "urgent": "1", "cards": ["a1"], "n": "1/3"}),
        "{r}"
    );
    assert_eq!(r["replaced"], "2 hidden", "{r}");
    assert_eq!(r["shown"], json!(["a3", "a1", "a2"]), "{r}");
    assert_eq!(r["still"], true, "{r}");
}

#[tokio::test]
async fn status_and_kind_filters_pick_tickets_and_cards() {
    let (html, st) = two_gate_page().await;
    let r = filter_scenario(
        &html,
        &st,
        r##"
const a3 = { ...s0.cards[0], id: "a3", kind: "blocker", token: "t-a3" };
const old = { handle: "m94", at: "08:00 MST", iso: "2026-11-17T15:00:00Z", label: "gizmos#5", kind: "note", head: "merged", ticket: "th-old", ticketStatus: "done", repo: null, project: null };
const s1 = { ...s0, cards: [...s0.cards, a3], feed: [...s0.feed, old], tree: s0.tree.map((t) => t.thread === W ? { ...t, done: true, status: "done", gate: null } : t) };
emitState(s1);
pick("statuses", "done");
const done = view();
pick("statuses", "done");
pick("statuses", "blocked");
const blocked = view();
pick("statuses", "blocked");
pick("statuses", "waiting");
const needs = view();
pick("statuses", "waiting");
pick("kinds", "gate");
const gates = view();
pick("kinds", "gate");
emitState({ ...s1, cards: s0.cards, tree: s1.tree.map((x) => x.thread === T ? { ...x, status: "failed" } : x) });
pick("statuses", "blocked");
const failedGate = view();
return { done, blocked, needs, gates, failedGate };
"##,
    );
    let t = json!(TICKET);
    let w = json!(WORKER);
    assert_eq!(r["done"]["cards"], json!(["a2"]), "{r}");
    assert_eq!(r["done"]["open"], json!([]), "{r}");
    assert_eq!(r["done"]["done"], json!([w]), "{r}");
    assert_eq!(r["done"]["settled"], json!(["t:th-s"]), "{r}");
    assert_eq!(r["done"]["feed"], json!(["m92", "m94"]), "{r}");
    assert_eq!(r["blocked"]["cards"], json!(["a3", "a1"]), "{r}");
    assert_eq!(r["blocked"]["open"], json!([t]), "{r}");
    assert_eq!(r["blocked"]["feed"], json!(["m91"]), "{r}");
    assert_eq!(
        (&r["needs"]["cards"], &r["needs"]["open"]),
        (&json!(["a3", "a1"]), &json!([t])),
        "a blocked ticket with cards also needs you: {r}"
    );
    assert_eq!(r["gates"]["cards"], json!(["a1", "a2"]), "{r}");
    assert_eq!(r["gates"]["open"], json!([t]), "{r}");
    assert_eq!(r["failedGate"]["open"], json!([t]), "a failed ticket with only a gate is still blocked: {r}");
}

#[tokio::test]
async fn the_f_key_focuses_search_only_with_single_key_shortcuts_on() {
    let (html, st) = two_gate_page().await;
    let r = page_scenario(
        &html,
        &st,
        r##"
emitState(base);
const press = (key) => { for (const f of document.listeners.keydown) f({ key, target: document.body, preventDefault() {} }); };
press("f");
const on = document.activeElement === document.getElementById("filter-q");
document.activeElement = document.body;
const box = document.getElementById("keys-on");
box.checked = false;
box.dispatch("change");
press("f");
return { on, off: document.activeElement === document.getElementById("filter-q") };
"##,
    );
    assert_eq!(r, json!({"on": true, "off": false}));
}

#[tokio::test]
async fn cards_and_feed_filter_by_a_context_or_projects_as_the_page_does() {
    let h = H::new();
    let mut gadgets = project("p-gadgets", "/src/gadgets");
    gadgets["title"] = json!("Gadgets");
    h.fake.set_projects(vec![project("p-widgets", "/src/widgets"), gadgets]);
    h.remotes.set("/src/widgets", "git@github.com:acme/widgets.git");
    h.remotes.set("/src/gadgets", "git@github.com:acme/gadgets.git");
    h.fake.update(TICKET, json!({"projectId": "p-widgets"}));
    h.fake.update(WORKER, json!({"projectId": "p-gadgets"}));
    h.fake.set_prs(BOSS, vec![pull_request("Acme/Widgets", 90, "open")]);
    join_board(&h, TICKET, "widgets#41").await;
    join_board(&h, WORKER, "gadgets#7").await;
    join_board(&h, BOSS, "widgets#90").await;
    for who in [TICKET, WORKER, BOSS] {
        h.wf(who, "send", json!({"kind": "gate", "body": GATE})).await;
        h.wf(who, "send", json!({"kind": "note", "body": format!("{who} is green")})).await;
    }
    h.app.wf.board_tick().await.unwrap();
    for (id, name, projects) in [
        ("c-ids", "Widget work", json!(["p-widgets"])),
        ("c-repo", "Old repo", json!(["ACME/Gadgets"])),
        ("c-title", "Old title", json!(["gadgets"])),
        ("c-none", "Waiting", json!([])),
    ] {
        let body =
            serde_json::from_value(json!({"name": name, "spec": {"projects": projects, "statuses": ["waiting"]}}));
        h.app.wf.put_context(id, body.unwrap()).unwrap();
    }
    let r = result(
        &h.exec(
            r#"async () => {
const cards = async (x) => (await wf.cards(x)).map((c) => c.label + " " + (c.project && c.project.id));
const feed = async (x) => (await wf.feed(x)).map((f) => f.label);
const err = async (f) => { try { await f(); return null; } catch (e) { return String(e.message || e); } };
return {
  all: await cards(),
  ids: await cards({ context: "c-ids" }),
  byName: await cards({ context: "widget WORK" }),
  repo: await cards({ context: "c-repo" }),
  title: await feed({ context: "c-title" }),
  none: (await wf.cards({ context: "c-none" })).length,
  picked: await feed({ projects: [" p-gadgets ", "p-gizmos"] }),
  limited: await feed({ projects: ["p-widgets"], limit: 1 }),
  empty: (await wf.feed({ projects: [] })).length,
  missing: await err(() => wf.cards({ context: "c-gone" })),
  both: await err(() => wf.feed({ context: "c-ids", projects: ["p-widgets"] })),
};
}"#,
        )
        .await,
    );
    let widgets = json!(["widgets#41 p-widgets", "widgets#90 null"]);
    assert_eq!(r["all"], json!(["widgets#41 p-widgets", "gadgets#7 p-gadgets", "widgets#90 null"]), "{r}");
    assert_eq!(r["ids"], widgets, "{r}");
    assert_eq!(r["byName"], widgets, "{r}");
    assert_eq!(r["repo"], json!(["gadgets#7 p-gadgets"]), "{r}");
    assert_eq!(r["title"], json!(["gadgets#7"]), "{r}");
    assert_eq!(r["none"], 3, "{r}");
    assert_eq!(r["picked"], json!(["gadgets#7"]), "{r}");
    assert_eq!(r["limited"], json!(["widgets#90"]), "{r}");
    assert_eq!(r["empty"], 3, "{r}");
    assert_eq!(r["missing"], "no saved context with the id or name \"c-gone\"", "{r}");
    assert_eq!(r["both"], "give a context or projects, not both", "{r}");
}

#[tokio::test]
async fn the_filter_learns_a_projects_repo_from_the_items_the_page_sees_first() {
    let h = H::new();
    h.fake.set_projects(vec![project("p-widgets", "/src/widgets")]);
    h.fake.update(TICKET, json!({"projectId": "p-widgets"}));
    h.fake.update(WORKER, json!({"projectId": "p-widgets"}));
    h.fake.set_prs(TICKET, vec![pull_request("acme/old-widgets", 1, "merged")]);
    h.fake.set_prs(WORKER, vec![pull_request("acme/widgets", 2, "open")]);
    h.fake.set_prs(BOSS, vec![pull_request("acme/widgets", 3, "open")]);
    join_board(&h, TICKET, "old-widgets#1").await;
    h.wf(TICKET, "send", json!({"kind": "handoff", "body": "done"})).await;
    join_board(&h, WORKER, "widgets#2").await;
    join_board(&h, BOSS, "widgets#3").await;
    h.wf(BOSS, "send", json!({"kind": "gate", "body": GATE})).await;
    h.app.wf.board_tick().await.unwrap();
    let r = result(&h.exec("async () => (await wf.cards({ projects: ['p-widgets'] })).map((c) => c.label)").await);
    assert_eq!(r, json!(["widgets#3"]));
}

#[tokio::test]
async fn a_ticket_joined_before_projects_were_stored_gets_its_project_back() {
    let mut h = H::new();
    h.fake.set_projects(vec![project("p-widgets", "/src/widgets")]);
    h.remotes.set("/src/widgets", "git@github.com:acme/widgets.git");
    join_board(&h, TICKET, "widgets#41").await;
    h.wf(TICKET, "send", json!({"kind": "handoff", "body": "done"})).await;
    assert_eq!(h.app.store.link_get(TICKET).unwrap().unwrap().project, None);
    h.fake.update(TICKET, json!({"projectId": "p-widgets"}));
    h.reopen();
    h.app.wf.board_tick().await.unwrap();
    assert_eq!(h.app.store.link_get(TICKET).unwrap().unwrap().project.as_deref(), Some("p-widgets"));
    let st = snapshot(&h);
    let t = by_label(&st["tree"], "widgets#41");
    assert_eq!((&t["repo"], &t["project"]["id"]), (&json!("acme/widgets"), &json!("p-widgets")), "{t}");
}

#[tokio::test]
async fn a_reply_waiting_on_undo_stays_visible_when_the_filter_changes() {
    let (html, st) = two_gate_page().await;
    let r = filter_scenario(
        &html,
        &st,
        r##"
emitState(s0);
card("a1").querySelector("[data-opt]").dispatch("click");
pick("projects", "p-gadgets");
const during = { cards: ids(), undo: !card("a1").querySelector('[data-part="undo"]').hidden };
advance(6000);
const sent = answers().map((f) => f.url);
card("a2").querySelector("[data-opt]").dispatch("click");
pick("projects", "p-gadgets");
pick("projects", "p-widgets");
const held = ids();
card("a2").querySelector('[data-part="undo"]').dispatch("click");
document.activeElement = document.body;
advance(10);
return { during, sent, held, undone: ids(), after: answers().length };
"##,
    );
    assert_eq!(r["during"], json!({"cards": ["a1", "a2"], "undo": true}), "{r}");
    assert_eq!(r["sent"], json!(["/api/cards/a1/answer"]), "{r}");
    assert_eq!(r["held"], json!(["a1", "a2"]), "{r}");
    assert_eq!(r["undone"], json!(["a1"]), "{r}");
    assert_eq!(r["after"], 1, "{r}");
}

#[tokio::test]
async fn a_project_whose_repo_was_unknown_is_looked_up_again_later() {
    let h = H::new();
    h.fake.set_projects(vec![project("p-widgets", "/src/widgets")]);
    h.fake.update(TICKET, json!({"projectId": "p-widgets"}));
    join_board(&h, TICKET, "widgets#41").await;
    h.app.wf.board_tick().await.unwrap();
    assert_eq!(by_label(&snapshot(&h)["tree"], "widgets#41")["repo"], Value::Null);
    h.remotes.set("/src/widgets", "git@github.com:acme/widgets.git");
    h.clock.advance_minutes(10);
    h.app.wf.board_tick().await.unwrap();
    assert_eq!(by_label(&snapshot(&h)["tree"], "widgets#41")["repo"], Value::Null);
    h.clock.advance_minutes(25);
    h.app.wf.board_tick().await.unwrap();
    assert_eq!(by_label(&snapshot(&h)["tree"], "widgets#41")["repo"], "acme/widgets");
}

#[tokio::test]
async fn a_gate_with_options_shows_exactly_those_buttons() {
    let h = H::new();
    join_board(&h, TICKET, "widgets#41").await;
    let opts = json!(["merge", " hold for review ", "close it"]);
    let r = h.wf(TICKET, "send", json!({"kind": "gate", "gate": "merge", "body": GATE, "options": opts})).await;
    assert_eq!(r["delivery"], "board");
    assert_eq!(r["options"], json!(["merge", "hold for review", "close it"]));
    assert!(r.get("warning").is_none(), "{r}");
    assert_eq!(cards(&h)[0]["options"], r["options"]);
}

#[tokio::test]
async fn a_board_send_reports_the_parsed_buttons() {
    let h = H::new();
    join_board(&h, TICKET, "widgets#41").await;
    let r = h.wf(TICKET, "send", json!({"kind": "gate", "body": GATE})).await;
    assert_eq!(r["options"], json!(["merge", "merge and deploy"]));
    assert!(r.get("warning").is_none(), "{r}");
    let r = h.wf(TICKET, "send", json!({"kind": "blocker", "body": "Need an AWS SSO login"})).await;
    assert_eq!(r["options"], json!([]));
    assert!(r.get("warning").is_none(), "a blocker needs no buttons: {r}");
}

#[tokio::test]
async fn a_gate_with_no_buttons_comes_back_with_a_warning() {
    let h = H::new();
    join_board(&h, TICKET, "widgets#41").await;
    let body = "**ACTION NEEDED:** merge 'feat/x' or hold it, here: https://github.com/o/widgets/pull/81";
    let r = h.wf(TICKET, "send", json!({"kind": "gate", "body": body})).await;
    assert_eq!(r["options"], json!([]));
    let w = r["warning"].as_str().unwrap();
    assert!(w.contains("options") && w.contains("reply"), "{w}");
    let r = h.wf(TICKET, "send", json!({"kind": "gate", "body": GATE, "options": []})).await;
    assert_eq!(r["options"], json!([]));
    assert!(r["warning"].is_string(), "{r}");
    assert_eq!(cards(&h)[0]["options"], json!([]));
}

#[tokio::test]
async fn bad_options_are_rejected_and_nothing_is_sent() {
    let h = H::new();
    join_board(&h, TICKET, "widgets#41").await;
    let long = "x".repeat(61);
    let e = h
        .wf_err(
            TICKET,
            "send",
            json!({"kind": "gate", "body": GATE, "options": ["merge", "", long, "two\nlines", "MERGE"]}),
        )
        .await;
    for want in ["options[1]", "empty", "options[2]", "60", "options[3]", "line", "options[4]", "repeats"] {
        assert!(e.contains(want), "{want:?} missing from {e}");
    }
    assert!(!e.contains("options[0]"), "{e}");
    let nine: Vec<String> = (1..=9).map(|n| format!("option {n}")).collect();
    let e = h.wf_err(TICKET, "send", json!({"kind": "gate", "body": GATE, "options": nine})).await;
    assert!(e.contains("at most 8"), "{e}");
    let e = h
        .wf_err(TICKET, "send", json!({"kind": "gate", "body": GATE, "options": ["\u{c9}t\u{e9}", "\u{e9}T\u{c9}"]}))
        .await;
    assert!(e.contains("options[1]") && e.contains("repeats"), "{e}");
    let e = h.wf_err(TICKET, "send", json!({"kind": "note", "body": "hi", "options": ["ok"]})).await;
    assert!(e.contains("gate, blocker, report"), "{e}");
    assert!(cards(&h).is_empty());
    assert!(h.wf(TICKET, "feed", json!({})).await.as_array().unwrap().is_empty());
}

fn context_names(v: &Value) -> Vec<Value> {
    v["contexts"].as_array().unwrap().iter().map(|c| c["name"].clone()).collect()
}

#[tokio::test]
async fn contexts_are_created_renamed_edited_and_deleted_and_survive_a_restart() {
    use reqwest::Method;
    let s = start(H::new()).await;
    let csrf = s.csrf();
    let spec = json!({"projects": ["p-widgets", " p-gadgets ", "P-Widgets", "p-widgets"], "statuses": ["done", "waiting"], "kinds": [], "q": "  grader "});
    let (code, made) = s.context(Method::PUT, "c1", json!({"name": " Widgets ", "spec": spec}), Some(&csrf)).await;
    assert_eq!(code, 201, "{made}");
    let made = &made["context"];
    assert_eq!((&made["id"], &made["name"]), (&json!("c1"), &json!("Widgets")));
    assert_eq!(
        made["spec"],
        json!({"projects": ["P-Widgets", "p-gadgets", "p-widgets"], "statuses": ["waiting", "done"], "kinds": [], "q": "grader"})
    );
    assert_eq!((&made["createdAt"], &made["updatedAt"]), (&json!(T0), &json!(T0)));
    s.h.clock.advance_minutes(5);
    let (code, renamed) = s.context(Method::PUT, "c1", json!({"name": "Widget work"}), Some(&csrf)).await;
    assert_eq!(code, 200, "{renamed}");
    assert_eq!(renamed["context"]["name"], "Widget work");
    assert_eq!(renamed["context"]["spec"], made["spec"]);
    assert_eq!((&renamed["context"]["createdAt"], &renamed["context"]["updatedAt"]), (&json!(T0), &json!(at(5))));
    let (code, edited) = s.context(Method::PUT, "c1", json!({"spec": {"kinds": ["gate"]}}), Some(&csrf)).await;
    assert_eq!(code, 200, "{edited}");
    assert_eq!(edited["context"]["name"], "Widget work");
    assert_eq!(edited["context"]["spec"], json!({"projects": [], "statuses": [], "kinds": ["gate"]}));
    assert_eq!(s.context(Method::PUT, "c2", json!({"name": "AI research", "spec": {}}), Some(&csrf)).await.0, 201);
    assert_eq!(context_names(&s.get("/api/contexts").await), [json!("AI research"), json!("Widget work")]);

    let s = s.restart().await;
    let listed = s.get("/api/contexts").await;
    assert_eq!(context_names(&listed), [json!("AI research"), json!("Widget work")]);
    assert_eq!(listed["contexts"][1], edited["context"]);
    assert_eq!(context_names(&s.get("/api/state").await), [json!("AI research"), json!("Widget work")]);
    let csrf = s.csrf();
    let (code, gone) = s.context(Method::DELETE, "c1", json!({}), Some(&csrf)).await;
    assert_eq!((code, gone), (200, json!({"deleted": "c1"})));
    let (code, again) = s.context(Method::DELETE, "c1", json!({}), Some(&csrf)).await;
    assert_eq!((code, &again["error"]), (404, &json!("no context \"c1\"")));

    let s = s.restart().await;
    assert_eq!(context_names(&s.get("/api/contexts").await), [json!("AI research")]);
}

#[tokio::test]
async fn context_routes_check_the_token_the_origin_and_the_input() {
    use reqwest::Method;
    let s = start(H::new()).await;
    let csrf = s.csrf();
    let ok = json!({"name": "Widgets", "spec": {}});
    assert_eq!(s.context(Method::PUT, "c1", ok.clone(), None).await.0, 403);
    assert_eq!(s.context(Method::PUT, "c1", ok.clone(), Some("nope")).await.0, 403);
    let foreign = s
        .http
        .put(format!("{}/api/contexts/c1", s.base))
        .json(&ok)
        .header("X-T3CM-CSRF", &csrf)
        .header("Origin", "https://evil.example")
        .send()
        .await
        .unwrap();
    assert_eq!(foreign.status(), 403);
    let r =
        s.http.get(format!("{}/api/contexts", s.base)).header("Origin", "https://evil.example").send().await.unwrap();
    assert_eq!(r.status(), 403);
    let long = "x".repeat(61);
    let bad = [
        ("c 1", ok.clone(), "context ids are 1 to 64"),
        ("c1", json!({"name": "  ", "spec": {}}), "a context needs a name"),
        ("c1", json!({"name": "All", "spec": {}}), "\"All\" is the built-in context"),
        ("c1", json!({"name": long, "spec": {}}), "at most 60 characters"),
        ("c1", json!({"name": "W", "spec": {"statuses": ["stuck"]}}), "unknown status \"stuck\""),
        ("c1", json!({"name": "W", "spec": {"kinds": ["note"]}}), "unknown card kind \"note\""),
        ("c1", json!({"name": "W", "spec": {"projects": [""]}}), "each project is one line"),
        ("c1", json!({"name": "W", "spec": {"project": ["a/b"]}}), "unknown field `project`"),
    ];
    for (id, body, want) in bad {
        let (code, r) = s.context(Method::PUT, id, body.clone(), Some(&csrf)).await;
        assert_eq!(code, 400, "{body}: {r}");
        assert!(r["error"].as_str().unwrap().contains(want), "{body}: {r}");
    }
    let gone = json!("no context \"c1\"; give a name and a spec to create one");
    let (code, r) = s.context(Method::PUT, "c1", json!({"name": "W"}), Some(&csrf)).await;
    assert_eq!((code, &r["error"]), (404, &gone));
    let (code, r) = s.context(Method::PUT, "c1", json!({"spec": {}}), Some(&csrf)).await;
    assert_eq!((code, &r["error"]), (404, &gone));
    let (code, r) = s.context(Method::PUT, "c1", json!({}), Some(&csrf)).await;
    assert_eq!((code, &r["error"]), (400, &json!("give a name, a spec or both")));
    assert_eq!(s.context(Method::PUT, "c1", ok.clone(), Some(&csrf)).await.0, 201);
    let (code, r) = s.context(Method::PUT, "c2", json!({"name": "WIDGETS", "spec": {}}), Some(&csrf)).await;
    assert_eq!((code, &r["error"]), (409, &json!("a context named \"Widgets\" exists")));
    assert_eq!(s.context(Method::DELETE, "c1", json!({}), None).await.0, 403);
    assert_eq!(s.context(Method::DELETE, "c9", json!({}), Some(&csrf)).await.0, 404);
    assert_eq!(context_names(&s.get("/api/contexts").await), [json!("Widgets")]);
    for n in 2..=t3_codemode::contexts::MAX_CONTEXTS {
        let body = serde_json::from_value(json!({"name": format!("n{n}"), "spec": {}})).unwrap();
        s.h.app.wf.put_context(&format!("c{n}"), body).unwrap();
    }
    let (code, r) = s.context(Method::PUT, "c999", json!({"name": "One more", "spec": {}}), Some(&csrf)).await;
    assert_eq!((code, &r["error"]), (400, &json!("the board keeps at most 100 contexts")));
    assert_eq!(s.context(Method::PUT, "c1", json!({"name": "Renamed", "spec": {}}), Some(&csrf)).await.0, 200);
}

#[tokio::test]
async fn context_names_that_match_under_full_case_folding_are_taken() {
    use reqwest::Method;
    let s = start(H::new()).await;
    let csrf = s.csrf();
    for (id, name) in [("c1", "Σ"), ("c2", "Straße")] {
        assert_eq!(s.context(Method::PUT, id, json!({"name": name, "spec": {}}), Some(&csrf)).await.0, 201);
    }
    for (id, name, taken) in
        [("c3", "ς", "Σ"), ("c3", "σ", "Σ"), ("c3", "STRASSE", "Straße"), ("c1", "strasse", "Straße")]
    {
        let (code, r) = s.context(Method::PUT, id, json!({"name": name, "spec": {}}), Some(&csrf)).await;
        assert_eq!((code, &r["error"]), (409, &json!(format!("a context named {taken:?} exists"))), "{name}");
    }
    assert_eq!(s.context(Method::PUT, "c1", json!({"name": "ς"}), Some(&csrf)).await.0, 200);
    assert_eq!(context_names(&s.get("/api/contexts").await), [json!("Straße"), json!("ς")]);
}

async fn next_state(r: &mut reqwest::Response, buf: &mut Vec<u8>) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        while let Some(end) = buf.windows(2).position(|w| w == b"\n\n") {
            let raw: Vec<u8> = buf.drain(..end + 2).collect();
            let event = String::from_utf8(raw).unwrap();
            if event.lines().any(|l| l == "event: state") {
                let data: String = event.lines().filter_map(|l| l.strip_prefix("data: ")).collect();
                return serde_json::from_str(&data).unwrap();
            }
        }
        let chunk =
            tokio::time::timeout_at(deadline, r.chunk()).await.expect("a state event in time").unwrap().unwrap();
        buf.extend_from_slice(&chunk);
    }
}

#[tokio::test]
async fn a_saved_context_reaches_every_open_board() {
    let s = start(H::new()).await;
    let mut r = s.http.get(format!("{}/api/events", s.base)).send().await.unwrap();
    let mut buf = Vec::new();
    assert_eq!(next_state(&mut r, &mut buf).await["contexts"], json!([]));
    let body = json!({"name": "Widgets", "spec": {"projects": ["p-widgets"]}});
    assert_eq!(s.context(reqwest::Method::PUT, "c1", body, Some(&s.csrf())).await.0, 201);
    let st = next_state(&mut r, &mut buf).await;
    assert_eq!(context_names(&st), [json!("Widgets")], "{st}");
    assert_eq!(st["contexts"][0]["spec"]["projects"], json!(["p-widgets"]), "{st}");
}

const SAVED_CONTEXTS: &str = r##"[
  { id: "c1", name: "Widgets", spec: { projects: ["p-widgets"], statuses: [], kinds: [] }, createdAt: "2026-11-17T12:00:00Z", updatedAt: "2026-11-17T12:00:00Z" },
  { id: "c2", name: "Gadget gates", spec: { projects: ["p-gadgets"], statuses: [], kinds: ["gate"], q: "gadget" }, createdAt: "2026-11-17T12:00:00Z", updatedAt: "2026-11-17T12:00:00Z" },
]"##;

const CONTEXT_HELPERS: &str = r##"
const ctxBtn = () => $$("ctx-btn");
const ctxPop = () => $$("ctx-pop");
const ctxOpts = () => [...ctxPop().querySelectorAll(".ctxopt")].map((o) => [o.dataset.ctx, o.textContent, o.getAttribute("aria-pressed")]);
const openCtxMenu = () => { if (ctxPop().hidden) ctxBtn().dispatch("click"); };
const choose = (id) => { openCtxMenu(); ctxPop().querySelector('[data-ctx="' + id + '"]').dispatch("click"); };
const ctxAct = (a) => ctxPop().querySelector('[data-act="' + a + '"]');
const stored = () => localStorage.getItem("t3board.context");
const flush = async () => { for (let i = 0; i < 20; i++) await null; };
"##;

fn context_scenario(html: &str, st: &Value, storage: Value, body: &str) -> Value {
    context_scenario_with(html, st, storage, "", body)
}

fn context_scenario_with(html: &str, st: &Value, storage: Value, prelude: &str, body: &str) -> Value {
    run_page_async_with(
        html,
        &format!("globalThis.__storage = {storage};\n{prelude}"),
        &format!(
            "async () => {{\n{PAGE_HELPERS}\nconst base = {st};\n{}\n{CONTEXT_HELPERS}\nconst ctxs = {SAVED_CONTEXTS};\nconst s1 = {{ ...s0, contexts: ctxs }};\n{body}\n}}",
            filter_setup()
        ),
    )
}

#[tokio::test]
async fn switching_contexts_applies_their_filters_and_all_clears_them() {
    let (html, st) = two_gate_page().await;
    let r = context_scenario(
        &html,
        &st,
        json!({}),
        r##"
emitState({ ...s0, contexts: [] });
$$("settled-filter").value = "gadgets";
const before = { btn: ctxBtn().textContent, cards: ids() };
openCtxMenu();
const lonely = ctxOpts();
emitState(s1);
const live = ctxOpts();
choose("c1");
const one = { ...view(), btn: ctxBtn().textContent, stored: stored(), menu: !ctxPop().hidden, proj: facet("projects").querySelector('[data-part="fbtn"]').textContent };
choose("c2");
const two = { cards: ids(), q: $$("filter-q").value, kinds: facet("kinds").querySelector('[data-part="fbtn"]').textContent, btn: ctxBtn().textContent };
pick("statuses", "waiting");
const edited = ctxBtn().textContent;
openCtxMenu();
const acts = ["new", "update", "rename", "delete"].filter((a) => !ctxAct(a).hidden);
choose("");
const all = { ...view(), btn: ctxBtn().textContent, stored: stored(), q: $$("filter-q").value, find: $$("settled-filter").value };
pick("projects", "p-widgets");
const loose = ctxBtn().textContent;
choose("c1");
$$("filter-clear").dispatch("click");
return { before, lonely, live, one, two, edited, acts, all, loose, cleared: { btn: ctxBtn().textContent, stored: stored(), cards: ids() } };
"##,
    );
    let t = json!(TICKET);
    assert_eq!(r["before"], json!({"btn": "Context: All", "cards": ["a1", "a2"]}), "{r}");
    assert_eq!(r["lonely"], json!([["", "All", "true"]]), "{r}");
    assert_eq!(
        r["live"],
        json!([["", "All", "true"], ["c2", "Gadget gates", "false"], ["c1", "Widgets", "false"]]),
        "{r}"
    );
    assert_eq!(r["one"]["cards"], json!(["a1"]), "{r}");
    assert_eq!(r["one"]["open"], json!([t]), "{r}");
    assert_eq!(r["one"]["feed"], json!(["m91"]), "{r}");
    assert_eq!(r["one"]["btn"], "Context: Widgets", "{r}");
    assert_eq!(r["one"]["stored"], "c1", "{r}");
    assert_eq!(r["one"]["menu"], false, "{r}");
    assert_eq!(r["one"]["proj"], "Project: acme/widgets", "{r}");
    assert_eq!(
        r["two"],
        json!({"cards": ["a2"], "q": "gadget", "kinds": "Card kind: gate", "btn": "Context: Gadget gates"}),
        "{r}"
    );
    assert_eq!(r["edited"], "Context: Gadget gates (edited)", "{r}");
    assert_eq!(r["acts"], json!(["new", "update", "rename", "delete"]), "{r}");
    assert_eq!(r["all"]["cards"], json!(["a1", "a2"]), "{r}");
    assert_eq!(r["all"]["n"], json!(["2", "2", "0", "2", "2"]), "{r}");
    assert_eq!(
        (&r["all"]["btn"], &r["all"]["stored"], &r["all"]["q"]),
        (&json!("Context: All"), &Value::Null, &json!(""))
    );
    assert_eq!(r["all"]["find"], "", "{r}");
    assert_eq!(r["loose"], "Context: Unsaved filter", "{r}");
    assert_eq!(r["cleared"], json!({"btn": "Context: All", "stored": null, "cards": ["a1", "a2"]}), "{r}");
}

#[tokio::test]
async fn the_active_context_comes_back_after_a_reload_or_falls_back_to_all() {
    let (html, st) = two_gate_page().await;
    let body = r##"
emitState(s1);
return { ...view(), btn: ctxBtn().textContent, stored: stored(), q: $$("filter-q").value };
"##;
    let kept = context_scenario(&html, &st, json!({"t3board.context": "c2"}), body);
    assert_eq!(kept["cards"], json!(["a2"]), "{kept}");
    assert_eq!(
        (&kept["btn"], &kept["stored"], &kept["q"]),
        (&json!("Context: Gadget gates"), &json!("c2"), &json!("gadget"))
    );
    let stale = context_scenario(&html, &st, json!({"t3board.context": "c-gone"}), body);
    assert_eq!(stale["cards"], json!(["a1", "a2"]), "{stale}");
    assert_eq!((&stale["btn"], &stale["stored"]), (&json!("Context: All"), &Value::Null));
}

#[tokio::test]
async fn a_context_changed_or_deleted_elsewhere_updates_this_page() {
    let (html, st) = two_gate_page().await;
    let r = context_scenario(
        &html,
        &st,
        json!({"t3board.context": "c1"}),
        r##"
emitState(s1);
const first = ids();
const a3 = { ...s0.cards[0], id: "a3", at: "2026-11-17T12:30:00Z", kind: "blocker", token: "t-a3" };
emitState({ ...s1, cards: [...s1.cards, a3], contexts: [ctxs[1], { ...ctxs[0], spec: { projects: ["p-gadgets"], statuses: [], kinds: [] } }] });
const edited = { cards: ids(), btn: ctxBtn().textContent, hidden: $$("hidden-new").textContent };
const find = $$("settled-filter");
find.value = "gadgets";
find.dispatch("input");
emitState({ ...s1, contexts: [ctxs[1]] });
return { first, edited, gone: { cards: ids(), btn: ctxBtn().textContent, stored: stored(), find: find.value, settled: keysOf("settled") } };
"##,
    );
    assert_eq!(r["first"], json!(["a1"]), "{r}");
    assert_eq!(r["edited"], json!({"cards": ["a2"], "btn": "Context: Widgets", "hidden": "1 hidden"}), "{r}");
    assert_eq!(
        r["gone"],
        json!({"cards": ["a1", "a2"], "btn": "Context: All", "stored": null, "find": "", "settled": ["t:th-s", "a:q1"]}),
        "{r}"
    );
}

#[tokio::test]
async fn contexts_are_saved_renamed_edited_and_deleted_from_the_page() {
    let (html, st) = two_gate_page().await;
    let r = context_scenario(
        &html,
        &st,
        json!({}),
        r##"
emitState({ ...s0, contexts: [] });
let reply = null;
fetchReply = (url, opts) => reply(url, opts);
let lastSpec = null, lastName = null;
const echo = (status) => async (url, opts) => {
  const b = JSON.parse(opts.body);
  if (b.spec) lastSpec = b.spec;
  if (b.name) lastName = b.name;
  return { ok: true, status, json: async () => ({ context: { id: decodeURIComponent(url.split("/").pop()), name: lastName, spec: lastSpec, createdAt: "x", updatedAt: "x" } }) };
};
const writes = () => fetches.filter((f) => f.method !== "GET").map((f) => [f.method, f.url, f.body ? JSON.parse(f.body) : null]);
openCtxMenu();
const idle = ["new", "update", "rename", "delete"].filter((a) => !ctxAct(a).hidden);
pick("projects", "p-widgets");
openCtxMenu();
ctxAct("new").dispatch("click");
const name = ctxPop().querySelector('[data-part="cname"]');
const focused = document.activeElement === name;
name.value = "  ";
name.dispatch("keydown", { key: "Enter" });
const empty = ctxPop().querySelector('[data-part="cerr"]').textContent;
name.value = "Widgets";
reply = echo(201);
name.dispatch("keydown", { key: "Enter" });
await flush();
const made = writes()[0];
const id = made[1].split("/").pop();
const saved = { btn: ctxBtn().textContent, stored: stored() === id, closed: ctxPop().hidden, cards: ids() };
emitState({ ...s0, contexts: [{ id, name: "Widgets", spec: made[2].spec }] });
const pushed = { btn: ctxBtn().textContent, cards: ids() };
openCtxMenu();
ctxAct("rename").dispatch("click");
const prefill = name.value;
name.value = "Widget work";
ctxPop().querySelector('[data-part="cok"]').dispatch("click");
await flush();
const renamed = ctxBtn().textContent;
pick("statuses", "waiting");
openCtxMenu();
const editedActs = ["new", "update", "rename", "delete"].filter((a) => !ctxAct(a).hidden);
ctxAct("update").dispatch("click");
await flush();
const updated = ctxBtn().textContent;
openCtxMenu();
ctxAct("new").dispatch("click");
name.value = "widget WORK";
reply = async () => ({ ok: false, status: 409, json: async () => ({ error: "a context named \"Widget work\" exists" }) });
name.dispatch("keydown", { key: "Enter" });
await flush();
const taken = { err: ctxPop().querySelector('[data-part="cerr"]').textContent, open: !ctxPop().hidden, alert: $$("alert").textContent };
ctxPop().dispatch("keydown", { key: "Escape", stopPropagation() {} });
const formClosed = { form: ctxPop().querySelector('[data-part="cform"]').hidden, open: !ctxPop().hidden };
ctxAct("delete").dispatch("click");
const armed = { text: ctxAct("delete").textContent, writes: writes().length };
reply = async () => ({ ok: true, status: 200, json: async () => ({ deleted: id }) });
ctxAct("delete").dispatch("click");
await flush();
const deleted = { btn: ctxBtn().textContent, stored: stored(), cards: ids(), q: facet("projects").querySelector('[data-part="fbtn"]').textContent };
return { idle, focused, empty, made, saved, pushed, prefill, renamed, editedActs, updated, taken, formClosed, armed, deleted, writes: writes().slice(1).map((w) => [w[0], w[1] === made[1], w[2]]) };
"##,
    );
    assert_eq!(r["idle"], json!([]), "{r}");
    assert_eq!(r["focused"], true, "{r}");
    assert_eq!(r["empty"], "Name the context first.", "{r}");
    assert!(r["made"][1].as_str().unwrap().starts_with("/api/contexts/c"), "{r}");
    assert_eq!(r["made"][0], "PUT", "{r}");
    assert_eq!(
        r["made"][2],
        json!({"name": "Widgets", "spec": {"projects": ["p-widgets"], "statuses": [], "kinds": [], "q": ""}}),
        "{r}"
    );
    assert_eq!(r["saved"], json!({"btn": "Context: Widgets", "stored": true, "closed": true, "cards": ["a1"]}), "{r}");
    assert_eq!(r["pushed"], json!({"btn": "Context: Widgets", "cards": ["a1"]}), "{r}");
    assert_eq!(r["prefill"], "Widgets", "{r}");
    assert_eq!(r["renamed"], "Context: Widget work", "{r}");
    assert_eq!(r["editedActs"], json!(["new", "update", "rename", "delete"]), "{r}");
    assert_eq!(r["updated"], "Context: Widget work", "{r}");
    assert_eq!(r["taken"]["err"], "a context named \"Widget work\" exists", "{r}");
    assert_eq!(r["taken"]["open"], true, "{r}");
    assert_eq!(r["formClosed"], json!({"form": true, "open": true}), "{r}");
    assert_eq!(r["armed"], json!({"text": "Click again to delete Widget work", "writes": 4}), "{r}");
    assert_eq!(
        r["deleted"],
        json!({"btn": "Context: All", "stored": null, "cards": ["a1", "a2"], "q": "Project"}),
        "{r}"
    );
    let waiting = json!({"projects": ["p-widgets"], "statuses": ["waiting"], "kinds": [], "q": ""});
    assert_eq!(
        r["writes"],
        json!([
            ["PUT", true, {"name": "Widget work"}],
            ["PUT", true, {"spec": waiting}],
            ["PUT", false, {"name": "widget WORK", "spec": waiting}],
            ["DELETE", true, {}],
        ]),
        "{r}"
    );
}

#[tokio::test]
async fn a_context_saved_before_the_repo_resolves_still_matches_after_it_arrives() {
    let (html, st) = two_gate_page().await;
    let r = context_scenario(
        &html,
        &st,
        json!({}),
        r##"
const early = (x) => x.thread === T || x.ticket === T ? { ...x, repo: null } : x;
const s2 = { ...s0, tree: s0.tree.map(early), cards: s0.cards.map(early), feed: s0.feed.map(early), contexts: [] };
emitState(s2);
fetchReply = async (url, opts) => ({ ok: true, status: 201, json: async () => ({ context: { id: decodeURIComponent(url.split("/").pop()), ...JSON.parse(opts.body), createdAt: "x", updatedAt: "x" } }) });
facet("projects").querySelector('[data-part="fbtn"]').dispatch("click");
const opts = [...facet("projects").querySelectorAll(".fopt")].map((o) => [o.dataset.val, o.textContent]);
pick("projects", "p-widgets");
openCtxMenu();
ctxAct("new").dispatch("click");
const name = ctxPop().querySelector('[data-part="cname"]');
name.value = "Widgets";
name.dispatch("keydown", { key: "Enter" });
await flush();
const put = JSON.parse(fetches.filter((f) => f.method === "PUT")[0].body);
const before = { ...view(), btn: ctxBtn().textContent, proj: facet("projects").querySelector('[data-part="fbtn"]').textContent };
emitState({ ...s0, contexts: [{ id: stored(), name: "Widgets", spec: put.spec }] });
const after = { ...view(), btn: ctxBtn().textContent, proj: facet("projects").querySelector('[data-part="fbtn"]').textContent };
return { opts, spec: put.spec.projects, before, after };
"##,
    );
    let t = json!(TICKET);
    assert_eq!(r["opts"], json!([["p-gadgets", "acme/gadgets"], ["p-widgets", "widgets"]]), "{r}");
    assert_eq!(r["spec"], json!(["p-widgets"]), "{r}");
    assert_eq!(r["before"]["cards"], json!(["a1"]), "{r}");
    assert_eq!(r["before"]["open"], json!([t]), "{r}");
    assert_eq!(r["before"]["proj"], "Project: widgets", "{r}");
    assert_eq!(r["after"]["cards"], json!(["a1"]), "{r}");
    assert_eq!(r["after"]["open"], json!([t]), "{r}");
    assert_eq!(r["after"]["feed"], json!(["m91"]), "{r}");
    assert_eq!(r["after"]["btn"], "Context: Widgets", "{r}");
    assert_eq!(r["after"]["proj"], "Project: acme/widgets", "{r}");
}

#[tokio::test]
async fn contexts_saved_under_a_repo_or_a_title_still_match() {
    let (html, st) = two_gate_page().await;
    let r = context_scenario(
        &html,
        &st,
        json!({}),
        r##"
const old = [
  { id: "c-repo", name: "Old repo", spec: { projects: ["acme/gadgets"], statuses: [], kinds: [] } },
  { id: "c-title", name: "Old title", spec: { projects: ["widgets"], statuses: [], kinds: [] } },
  { id: "c-gone", name: "Old gizmos", spec: { projects: ["acme/gizmos", "widgets"], statuses: [], kinds: [] } },
];
emitState({ ...s0, contexts: old });
const optsOf = () => {
  if (facet("projects").querySelector('[data-part="fpop"]').hidden) facet("projects").querySelector('[data-part="fbtn"]').dispatch("click");
  return [...facet("projects").querySelectorAll(".fopt")].map((o) => [o.dataset.val, o.querySelector("input").checked]);
};
choose("c-repo");
const repo = { ...view(), btn: ctxBtn().textContent, proj: facet("projects").querySelector('[data-part="fbtn"]').textContent, opts: optsOf() };
choose("c-title");
const title = { ...view(), btn: ctxBtn().textContent, proj: facet("projects").querySelector('[data-part="fbtn"]').textContent, opts: optsOf() };
choose("c-gone");
const gone = { cards: ids(), opts: optsOf() };
pick("projects", "p-widgets");
const unpicked = { cards: ids(), opts: optsOf(), btn: ctxBtn().textContent, spec: filterSpec().projects };
return { repo, title, gone, unpicked };
"##,
    );
    let t = json!(TICKET);
    let w = json!(WORKER);
    assert_eq!(r["repo"]["cards"], json!(["a2"]), "{r}");
    assert_eq!(r["repo"]["open"], json!([w]), "{r}");
    assert_eq!(r["repo"]["btn"], "Context: Old repo", "{r}");
    assert_eq!(r["repo"]["proj"], "Project: acme/gadgets", "{r}");
    assert_eq!(r["repo"]["opts"], json!([["p-gadgets", true], ["p-widgets", false]]), "{r}");
    assert_eq!(r["title"]["cards"], json!(["a1"]), "{r}");
    assert_eq!(r["title"]["open"], json!([t]), "{r}");
    assert_eq!(r["title"]["btn"], "Context: Old title", "{r}");
    assert_eq!(r["title"]["proj"], "Project: acme/widgets", "{r}");
    assert_eq!(r["title"]["opts"], json!([["p-gadgets", false], ["p-widgets", true]]), "{r}");
    assert_eq!(r["gone"]["cards"], json!(["a1"]), "{r}");
    assert_eq!(r["gone"]["opts"], json!([["p-gadgets", false], ["acme/gizmos", true], ["p-widgets", true]]), "{r}");
    assert_eq!(r["unpicked"]["cards"], json!([]), "{r}");
    assert_eq!(
        r["unpicked"]["opts"],
        json!([["p-gadgets", false], ["acme/gizmos", true], ["p-widgets", false]]),
        "{r}"
    );
    assert_eq!(r["unpicked"]["btn"], "Context: Old gizmos (edited)", "{r}");
    assert_eq!(r["unpicked"]["spec"], json!(["acme/gizmos"]), "{r}");
}

#[tokio::test]
async fn notifications_skip_cards_outside_the_active_context() {
    let (html, st) = two_gate_page().await;
    let r = context_scenario(
        &html,
        &st,
        json!({}),
        r##"
const notes = [];
globalThis.Notification = class { constructor(title) { notes.push(title.match(/\((a\d+)\)$/)[1]); } };
Notification.permission = "granted";
document.hidden = true;
emitState(s1);
choose("c1");
const a3 = { ...s0.cards[1], id: "a3", at: "2026-11-17T12:30:00Z", kind: "blocker", token: "t-a3" };
const a4 = { ...s0.cards[0], id: "a4", at: "2026-11-17T12:31:00Z", kind: "blocker", token: "t-a4" };
emitState({ ...s1, cards: [...s1.cards, a3, a4] });
const inside = { notes: notes.slice(), hidden: $$("hidden-new").textContent, cards: ids() };
choose("");
const a5 = { ...s0.cards[1], id: "a5", at: "2026-11-17T12:32:00Z", token: "t-a5" };
emitState({ ...s1, cards: [...s1.cards, a3, a4, a5] });
return { inside, all: notes };
"##,
    );
    assert_eq!(r["inside"], json!({"notes": ["a4"], "hidden": "1 hidden", "cards": ["a4", "a1"]}), "{r}");
    assert_eq!(r["all"], json!(["a4", "a5"]), "{r}");
}

#[tokio::test]
async fn a_save_that_finishes_after_a_switch_keeps_the_new_context() {
    let (html, st) = two_gate_page().await;
    let r = context_scenario(
        &html,
        &st,
        json!({}),
        r##"
emitState(s1);
const waiting = [];
fetchReply = () => new Promise((resolve) => waiting.push(resolve));
const answer = () => {
  const f = fetches[fetches.length - 1];
  const b = JSON.parse(f.body);
  const id = f.url.split("/").pop();
  const name = b.name || ctxs.find((c) => c.id === id).name;
  waiting.shift()({ ok: true, status: b.name ? 201 : 200, json: async () => ({ context: { id, name, spec: b.spec, createdAt: "x", updatedAt: "x" } }) });
};
pick("projects", "p-widgets");
openCtxMenu();
ctxAct("new").dispatch("click");
const name = ctxPop().querySelector('[data-part="cname"]');
name.value = "Mine";
name.dispatch("keydown", { key: "Enter" });
choose("c2");
openCtxMenu();
ctxAct("rename").dispatch("click");
name.value = "Gadgets draft";
answer();
await flush();
const draft = { form: !ctxPop().querySelector('[data-part="cform"]').hidden, value: name.value };
ctxPop().dispatch("keydown", { key: "Escape", stopPropagation() {} });
const saved = { btn: ctxBtn().textContent, stored: stored(), cards: ids(), names: ctxOpts().map((o) => o[1]) };
choose("c1");
pick("statuses", "waiting");
openCtxMenu();
ctxAct("update").dispatch("click");
choose("c2");
pick("statuses", "waiting");
answer();
await flush();
const c1 = contexts.list.find((c) => c.id === "c1");
const updatedC1 = { name: c1.name, statuses: c1.spec.statuses };
emitState({ ...s1, contexts: [...ctxs, { id: "mine", name: "Mine", spec: { projects: ["p-widgets"], statuses: [], kinds: [] } }] });
const updated = ctxBtn().textContent;
choose("c1");
pick("statuses", "running");
openCtxMenu();
ctxAct("update").dispatch("click");
const sent = JSON.parse(fetches[fetches.length - 1].body).spec;
pick("kinds", "gate");
emitState({ ...s1, contexts: [{ ...ctxs[0], spec: sent }, ctxs[1]] });
const echoed = { kinds: facet("kinds").querySelector('[data-part="fbtn"]').textContent, btn: ctxBtn().textContent };
answer();
await flush();
return { draft, saved, updatedC1, updated, echoed };
"##,
    );
    assert_eq!(r["draft"], json!({"form": true, "value": "Gadgets draft"}), "{r}");
    assert_eq!(
        r["saved"],
        json!({"btn": "Context: Gadget gates", "stored": "c2", "cards": ["a2"], "names": ["All", "Gadget gates", "Mine", "Widgets"]}),
        "{r}"
    );
    assert_eq!(r["updatedC1"], json!({"name": "Widgets", "statuses": ["waiting"]}), "{r}");
    assert_eq!(r["updated"], "Context: Gadget gates (edited)", "{r}");
    assert_eq!(r["echoed"], json!({"kinds": "Card kind: gate", "btn": "Context: Widgets (edited)"}), "{r}");
}

#[tokio::test]
async fn a_failed_first_state_request_keeps_the_saved_context() {
    let (html, st) = two_gate_page().await;
    let r = context_scenario_with(
        &html,
        &st,
        json!({"t3board.context": "c1"}),
        r#"globalThis.__fetchReply = async () => ({ ok: false, status: 500, json: async () => ({ error: "store is busy" }) });"#,
        r##"
await flush();
const early = { stored: stored(), btn: ctxBtn().textContent };
emitState(s1);
return { early, cards: ids(), btn: ctxBtn().textContent, stored: stored() };
"##,
    );
    assert_eq!(r["early"], json!({"stored": "c1", "btn": "Context: All"}), "{r}");
    assert_eq!(r["cards"], json!(["a1"]), "{r}");
    assert_eq!((&r["btn"], &r["stored"]), (&json!("Context: Widgets"), &json!("c1")), "{r}");
}

#[tokio::test]
async fn the_state_carries_iso_times_for_relative_ages() {
    let h = H::new();
    join_board(&h, TICKET, "widgets#41").await;
    h.wf(TICKET, "send", json!({"kind": "note", "body": "tests green"})).await;
    let st = snapshot(&h);
    let t = by_label(&st["tree"], "widgets#41");
    assert!(t["upAt"].as_str().is_some_and(|s| s.ends_with('Z') && s.contains('T')), "{t}");
}

#[tokio::test]
async fn a_dismissed_gate_is_marked_on_the_ticket_last_message() {
    let h = H::new();
    join_board(&h, TICKET, "widgets#41").await;
    h.wf(TICKET, "send", json!({"kind": "gate", "gate": "merge", "body": GATE})).await;
    let last = |h: &H| by_label(&snapshot(h)["tree"], "widgets#41")["last"].clone();
    assert!(last(&h).get("dismissed").is_none(), "{}", last(&h));
    let id = cards(&h)[0]["id"].as_str().unwrap().to_string();
    h.app.wf.dismiss(&id).unwrap();
    assert_eq!(last(&h)["dismissed"], true, "{}", last(&h));
}

const ROWS: &str = r##"
const W = (label, status, extra) => ({ thread: "w-" + label, label, status, done: status === "done", gate: null, pr: null, link: null, lastUp: "10:00 MST", ...extra });
const T = (label, extra) => ({ thread: "t-" + label, ticket: "t-" + label, label, status: "idle", done: false, gate: null, pr: null, link: null, joined: true, lastUp: "10:00 MST", last: null, workers: [], ...extra });
const row = (label) => q('#open [data-thread="t-' + label + '"]');
const groups = () => [...document.getElementById("open").children].filter((e) => e.classList.contains("tgroup")).map((g) => g.textContent);
"##;

#[tokio::test]
async fn ticket_state_comes_from_its_workers_and_rows_group_by_state() {
    let (html, st) = two_gate_page().await;
    let r = page_scenario(
        &html,
        &st,
        &format!(
            r##"{ROWS}
const c1 = {{ ...base.cards[0], ticket: "t-acme/a#1", label: "acme/a#1" }};
const tree = [
  T("acme/d#4", {{ workers: [W("#4 PR 1/1", "idle")] }}),
  T("acme/b#2", {{ workers: [W("#2 PR 1/2", "done"), W("#2 PR 2/2", "running")] }}),
  T("acme/c#3", {{ workers: [W("#3 PR 1/1", "failed")] }}),
  T("acme/a#1", {{ gate: "merge", workers: [W("#1 PR 1/1", "idle", {{ gate: "merge" }})] }}),
  T("acme/e#5", {{ workers: [W("#5 PR 1/1", "waiting")] }}),
];
emitState({{ ...base, cards: [c1], tree, counts: {{ ...base.counts, cards: 1, running: 0 }} }});
const keys = [...document.getElementById("open").children].filter((e) => e.dataset.key).map((e) => e.dataset.key);
return {{
  keys, groups: groups(),
  states: keys.map((k) => q('#open [data-key="' + k + '"]').dataset.state),
  marks: ["acme/a#1", "acme/b#2"].map((l) => row(l).querySelector(".mk .mark").className),
  worker: row("acme/a#1").querySelector(".wi .mark").className,
  running: q("#counts").children[1].firstChild.textContent,
  need: q("#counts").children[0].firstChild.textContent,
  lead: (() => {{
    emitState({{ ...base, cards: [c1, {{ ...base.cards[1], id: "a9", kind: "blocker", ticket: "t-acme/a#1", label: "acme/a#1" }}], tree, counts: {{ ...base.counts, cards: 2, running: 0 }} }});
    return row("acme/a#1").querySelector('[data-part="needs"]').textContent;
  }})(),
}};
"##
        ),
    );
    assert_eq!(r["keys"], json!(["t-acme/a#1", "t-acme/c#3", "t-acme/b#2", "t-acme/e#5", "t-acme/d#4"]), "{r}");
    assert_eq!(r["states"], json!(["need", "fail", "run", "wait", "idle"]), "{r}");
    assert_eq!(r["lead"], "Blocked +1", "a newer blocker leads the row like it leads the cards: {r}");
    assert_eq!(r["groups"], json!(["Needs you1", "Failed1", "Running1", "Waiting1", "Idle1"]), "{r}");
    assert_eq!(r["marks"], json!(["mark s-need", "mark s-run"]), "{r}");
    assert_eq!(r["worker"], "mark s-need", "{r}");
    assert_eq!((&r["running"], &r["need"]), (&json!("1"), &json!("1")), "{r}");
}

#[tokio::test]
async fn rows_show_a_readable_title_and_say_when_a_gate_was_answered() {
    let (html, st) = two_gate_page().await;
    let r = page_scenario(
        &html,
        &st,
        &format!(
            r##"{ROWS}
const tree = [
  T("acme/a#1", {{ title: "acme/a#1: Fix the grader" }}),
  T("acme/b#2", {{ gate: "acme/api#86_owns_the_next_deploy" }}),
  T("acme/c#3", {{ title: "acme/c#3: https://github.com/acme/c/issues/3", gate: "windows-signing",
    last: {{ kind: "gate", head: '**ACTION NEEDED:** reply "approve" to approve the 2-PR split, or reply with changes, here: https://github.com/acme/c/issues/3', at: "10:00 MST" }} }}),
  T("acme/e#5", {{ last: {{ kind: "gate", dismissed: true, head: '**ACTION NEEDED:** reply "merge" to merge #12, here: https://github.com/acme/e/pull/12', at: "10:00 MST" }} }}),
  T("acme/d#4", {{ last: {{ kind: "event", head: "PR 1/2 opened: https://github.com/acme/d/pull/9, head 0123456789abcdef0123 Publishing: template", at: "10:00 MST" }} }}),
];
emitState({{ ...base, cards: [], tree }});
const text = (l, p) => row(l).querySelector('[data-part="' + p + '"]').textContent;
return {{
  titles: ["acme/a#1", "acme/b#2", "acme/c#3"].map((l) => text(l, "title")),
  answered: text("acme/c#3", "note"),
  dismissed: text("acme/e#5", "note"),
  needsHidden: row("acme/c#3").querySelector('[data-part="needs"]').hidden,
  state: row("acme/c#3").dataset.state,
  note: text("acme/d#4", "note"),
}};
"##
        ),
    );
    assert_eq!(r["titles"], json!(["Fix the grader", "Acme/api#86 owns the next deploy", "Windows signing"]), "{r}");
    assert_eq!(r["answered"], "Answered: approve the 2-PR split", "{r}");
    assert_eq!(r["dismissed"], "Dismissed: merge #12", "{r}");
    assert_eq!((&r["needsHidden"], &r["state"]), (&json!(true), &json!("idle")), "{r}");
    assert_eq!(r["note"], "PR 1/2 opened: #9, head 0123456", "{r}");
}

#[tokio::test]
async fn a_card_leads_with_the_ask_and_keeps_the_context_below_it() {
    let (html, st) = two_gate_page().await;
    let r = page_scenario(
        &html,
        &st,
        r##"
const one = { ...base.cards[0], options: ["merge"], text: 'reply "merge" to merge #64, or review it first, here: https://github.com/o/widgets/pull/64 . Verified: approved at 905fcaa7d21dfe19.' };
const blocker = { ...base.cards[1], kind: "blocker", options: [], text: base.cards[1].label + " worker cannot reach the registry; fix the token, here: https://github.com/o/widgets/actions/runs/7" };
emitState({ ...base, cards: [one, blocker] });
const p = (id, part) => card(id).querySelector('[data-part="' + part + '"]');
const out = {
  order: ids(),
  ask: [p("a1", "ask").textContent, p("a2", "ask").textContent],
  word: [p("a1", "kw").hidden, p("a2", "kw").textContent],
  detail: p("a1", "detail").textContent,
  review: p("a1", "rev").textContent,
  open: p("a2", "rev").textContent,
  cls: card("a2").className,
  title: p("a1", "q").getAttribute("title").slice(0, 13),
  label: p("a1", "id").textContent,
};
emitState({ ...base, cards: [{ ...one, text: 'reopen #81, then reply "merge" to merge it, here: https://github.com/o/widgets/pull/81' }] });
out.then = p("a1", "ask").textContent;
emitState({ ...base, cards: [{ ...one, options: [], text: "**ACTION NEEDED:** start the code-mode service, here: `t3cm serve`, then reply once it is healthy" }] });
out.cmd = p("a1", "ask").textContent;
emitState({ ...base, cards: [{ ...one, text: 'reply "merge" to merge #81, here: `https://github.com/o/widgets/pull/81`.' }] });
const rev = p("a1", "rev").querySelector("a");
out.href = rev && rev.getAttribute("href");
emitState({ ...base, cards: [{ ...one, options: [], text: "Restart job 12345678 after the fix" }] });
out.num = p("a1", "ask").textContent;
emitState({ ...base, cards: [{ ...one, options: [], text: "Rebase head 390604428 (390604428) onto main" }] });
out.digits = p("a1", "ask").textContent;
emitState({ ...base, cards: [{ ...one, options: [], text: "**ACTION NEEDED:** run the check here: npm test" }] });
out.bare = p("a1", "ask").textContent;
emitState({ ...base, cards: [{ ...one, options: [], text: "**ACTION NEEDED:** run `printf '%s' deadbeef0123` on the host" }] });
out.code = p("a1", "ask").textContent;
return out;
"##,
    );
    assert_eq!(r["order"], json!(["a2", "a1"]), "{r}");
    assert_eq!(r["ask"], json!(["Merge #64", "Worker cannot reach the registry; fix the token"]), "{r}");
    assert_eq!(r["word"], json!([true, "Blocked:"]), "{r}");
    assert_eq!(r["detail"], "Verified: approved at 905fcaa.", "{r}");
    assert_eq!((&r["review"], &r["open"]), (&json!("Review #64"), &json!("Open link")), "{r}");
    assert_eq!(r["cls"], "card k-fail", "{r}");
    assert_eq!(r["title"], "reply \"merge\"", "{r}");
    assert_eq!(r["label"], "widgets#41", "{r}");
    assert_eq!(r["then"], "Reopen #81, then reply \"merge\" to merge it", "{r}");
    assert_eq!(r["cmd"], "Start the code-mode service: t3cm serve, then reply once it is healthy", "{r}");
    assert_eq!(r["href"], "https://github.com/o/widgets/pull/81", "{r}");
    assert_eq!(r["num"], "Restart job 12345678 after the fix", "{r}");
    assert_eq!(r["digits"], "Rebase head 3906044 (3906044) onto main", "{r}");
    assert_eq!(r["bare"], "Run the check here: npm test", "{r}");
    assert_eq!(r["code"], "Run printf '%s' deadbeef0123 on the host", "inline code keeps its hex: {r}");
}

#[tokio::test]
async fn activity_groups_runs_and_shows_the_message_without_its_kind() {
    let (html, st) = two_gate_page().await;
    let r = page_scenario(
        &html,
        &st,
        r##"
const F = (n, label, kind, head) => ({ handle: "m" + n, at: "10:0" + n + " MST", iso: "2026-11-17T17:0" + n + ":00Z", label, kind, head, ticket: null, ticketLabel: "acme/w#7", ticketTitle: null });
const feed = [
  F(5, "#7 PR 3/3", "event", "PR 3/3 updated: #12, head 4f2a9c1e0b9d3d6a1f00c2b7e1a8d55e2c9f0a11. Fixed both findings"),
  F(4, "#7 PR 3/3", "note", "#12: review requested changes"),
  F(3, "#7 PR 3/3", "event", "PR 3/3 opened: https://github.com/acme/w/pull/12, head 1c0ffee Publishing: template filled"),
  F(2, "acme/w#7", "note", "split approved"),
];
emitState({ ...base, feed });
const li = () => [...document.getElementById("feed").children];
const first = li()[0];
const before = { rows: li().map((e) => e.dataset.key), who: first.querySelector(".who").textContent, sub: first.querySelector(".fsub").textContent,
  msg: first.querySelector(".msg").textContent, more: first.querySelector(".more").textContent, earlier: first.querySelector(".earlier").hidden };
first.querySelector(".more").dispatch("click");
const open = [...first.querySelector(".earlier").children].map((e) => e.querySelector(".msg").textContent);
emitState({ ...base, feed });
const kept = { still: !first.querySelector(".earlier").hidden, toggle: first.querySelector(".more").getAttribute("aria-expanded") };
emitState({ ...base, feed: feed.filter((f) => f.handle !== "m3") });
kept.evicted = !first.querySelector(".earlier").hidden;
emitState({ ...base, feed: [feed[0], { ...feed[1], ticket: "t-x", ticketLabel: "acme/x#7" }] });
return { before, open, ...kept, split: li().length };
"##,
    );
    assert_eq!(r["before"]["rows"], json!(["m5", "m2"]), "{r}");
    assert_eq!((&r["before"]["who"], &r["before"]["sub"]), (&json!("acme/w#7"), &json!("PR 3/3")), "{r}");
    assert_eq!(r["before"]["msg"], "Updated: #12, head 4f2a9c1. Fixed both findings", "{r}");
    assert_eq!(r["before"]["more"], "+ 2 earlier from PR 3/3", "{r}");
    assert_eq!(r["before"]["earlier"], true, "{r}");
    assert_eq!(r["open"], json!(["#12: review requested changes", "Opened: #12, head 1c0ffee"]), "{r}");
    assert_eq!((&r["still"], &r["toggle"]), (&json!(true), &json!("true")), "{r}");
    assert_eq!(r["split"], 2, "same worker label on another ticket must not fold: {r}");
    assert_eq!(r["evicted"], true, "an open group stays open when its oldest row leaves the feed: {r}");
}

#[tokio::test]
async fn an_age_tooltip_names_the_zone_its_clock_reads_in() {
    let (html, st) = two_gate_page().await;
    let r = page_scenario(
        &html,
        &st,
        &format!(
            r##"{ROWS}
const tree = [T("acme/a#1", {{ status: "running", lastUp: "16:21 MST", upAt: "2026-11-18T23:21:00Z" }}), T("acme/b#2", {{ lastUp: "16:00 MST" }})];
const tip = () => row("acme/a#1").querySelector('[data-part="tm"]').getAttribute("title");
globalThis.Intl = {{ DateTimeFormat: function () {{ return {{ formatToParts: () => [{{ type: "hour", value: "23" }}, {{ type: "timeZoneName", value: "UTC" }}] }}; }} }};
emitState({{ ...base, cards: [], tree }});
const browser = tip();
delete globalThis.Intl;
emitState({{ ...base, cards: [], tree: [{{ ...tree[0], upAt: "2026-11-18T23:22:00Z" }}, tree[1]] }});
return {{ browser, bare: tip() }};
"##
        ),
    );
    let browser = r["browser"].as_str().unwrap_or_default();
    assert!(browser.ends_with(" UTC"), "the label names the browser zone the time is read in: {r}");
    let bare = r["bare"].as_str().unwrap_or_default();
    assert!(!bare.contains("MST") && !bare.contains("UTC"), "no zone label without Intl: {r}");
}

#[tokio::test]
async fn the_check_age_in_the_header_keeps_counting() {
    let (html, st) = two_gate_page().await;
    let r = page_scenario(
        &html,
        &st,
        r##"
const t0 = Date.parse("2026-11-19T17:00:00Z");
Date.now = () => t0;
const service = { ...base.service, watcher: { ...base.service.watcher, alive: true, lastPassAt: "2026-11-19T17:00:00Z" } };
emitState({ ...base, service });
const fresh = document.getElementById("svc-text").textContent;
Date.now = () => t0 + 5 * 60000;
for (const f of intervals) f();
return { fresh, later: document.getElementById("svc-text").textContent };
"##,
    );
    assert!(r["fresh"].as_str().unwrap_or_default().contains("checked just now"), "{r}");
    assert!(r["later"].as_str().unwrap_or_default().contains("checked 5m ago"), "the timer repaints the age: {r}");
}

#[tokio::test]
async fn group_headings_wait_while_the_ticket_list_is_held() {
    let (html, st) = two_gate_page().await;
    let r = page_scenario(
        &html,
        &st,
        &format!(
            r##"{ROWS}
const tree = [T("acme/a#1", {{ status: "running" }}), T("acme/b#2", {{ status: "running" }})];
emitState({{ ...base, cards: [], tree }});
const shape = () => [...document.getElementById("open").children].map((e) => e.dataset.key || e.textContent);
const before = shape();
row("acme/b#2").focus();
const c1 = {{ ...base.cards[0], ticket: "t-acme/a#1", label: "acme/a#1" }};
emitState({{ ...base, cards: [c1], tree }});
const held = shape();
document.activeElement = document.body;
emitState({{ ...base, cards: [c1], tree }});
return {{ before, held, live: shape() }};
"##
        ),
    );
    assert_eq!(r["held"], r["before"], "{r}");
    assert_eq!(r["live"], json!(["Needs you1", "t-acme/a#1", "Running1", "t-acme/b#2"]), "{r}");
}

#[tokio::test]
async fn done_workers_fold_into_a_count_that_expands() {
    let (html, st) = two_gate_page().await;
    let r = page_scenario(
        &html,
        &st,
        &format!(
            r##"{ROWS}
const tree = [T("acme/b#2", {{ workers: [W("#2 PR 1/4", "done"), W("#2 PR 2/4", "done"), W("#2 PR 3/4", "idle"), W("#2 PR 4/4", "running")] }})];
emitState({{ ...base, cards: [], tree }});
const shown = () => [...row("acme/b#2").querySelectorAll(".wl .wi")].map((w) => w.textContent);
const more = row("acme/b#2").querySelector('[data-part="wmore"]');
const done = row("acme/b#2").querySelector('[data-part="wdone"]');
const before = {{ shown: shown(), more: more.textContent, done: done.textContent }};
more.dispatch("click");
const after = {{ shown: shown(), less: more.textContent, expanded: more.getAttribute("aria-expanded"), doneHidden: done.hidden }};
more.dispatch("click");
done.dispatch("click");
const viaDone = shown().length;
const focusMoved = document.activeElement === more;
more.dispatch("click");
return {{ before, after, viaDone, focusMoved, focusStays: document.activeElement === more && !more.hidden }};
"##
        ),
    );
    assert_eq!(r["before"], json!({"shown": ["PR 4/4"], "more": "+1 worker", "done": "2 done"}), "{r}");
    assert_eq!(
        r["after"],
        json!({"shown": ["PR 1/4", "PR 2/4", "PR 3/4", "PR 4/4"], "less": "Less", "expanded": "true", "doneHidden": true}),
        "{r}"
    );
    assert_eq!(r["viaDone"], 4, "{r}");
    assert_eq!((&r["focusMoved"], &r["focusStays"]), (&json!(true), &json!(true)), "{r}");
}

async fn new_issue_page() -> (String, Value) {
    let s = start(H::new()).await;
    join_board(&s.h, TICKET, "widgets#41").await;
    let report = "async () => wf.issue({ url: 'https://github.com/Acme/widgets/issues/43', title: 'Fix the grader', note: 'found while testing' })";
    result(&s.h.exec_as(WORKER, report).await);
    result(&s.h.exec_as(WORKER, "async () => wf.issue({ url: 'Acme/gadgets#7' })").await);
    let html = s.http.get(format!("{}/", s.base)).send().await.unwrap().text().await.unwrap();
    let st: Value = s.http.get(format!("{}/api/state", s.base)).send().await.unwrap().json().await.unwrap();
    (html, st)
}

const NEW_HELPERS: &str = r##"
const row = (id) => q('#nis [data-key="' + id + '"]');
const rows = () => [...document.getElementById("nis").children].map((r) => r.dataset.key);
const btn = (id, p) => row(id).querySelector('[data-part="' + p + '"]');
const posts = () => fetches.filter((f) => f.method === "POST").map((f) => f.url);
const sec = () => document.getElementById("p-new");
const flush = async () => { for (let i = 0; i < 20; i++) await null; };
const answer = (body) => async () => ({ ok: true, json: async () => body });
"##;

fn new_scenario(html: &str, st: &Value, body: &str) -> Value {
    run_page_async(html, &format!("async () => {{\n{PAGE_HELPERS}\n{NEW_HELPERS}\nconst base = {st};\n{body}\n}}"))
}

#[tokio::test]
async fn a_project_merges_what_every_item_knows_and_catches_repo_only_issues() {
    let (html, st) = new_issue_page().await;
    let r = run_page_async(
        &html,
        &format!(
            r##"async () => {{
{PAGE_HELPERS}
{NEW_HELPERS}
{FILTER_HELPERS}
const base = {st};
const proj = {{ id: "p-widgets", title: "widgets" }};
const s0 = {{
  ...base,
  tree: base.tree.map((t) => ({{ ...t, repo: null, project: proj }})),
  feed: [{{ handle: "m91", at: "10:00 MST", iso: "2026-11-17T17:00:00Z", label: "widgets#41", kind: "note", head: "tests green", ticket: base.tree[0].thread, repo: "acme/widgets", project: proj }}],
  contexts: [{{ id: "c-old", name: "Old", spec: {{ projects: ["acme/widgets"], statuses: [], kinds: [] }} }}],
}};
emitState(s0);
const all = rows();
facet("projects").querySelector('[data-part="fbtn"]').dispatch("click");
const opts = [...facet("projects").querySelectorAll(".fopt")].map((o) => [o.dataset.val, o.textContent]);
pick("projects", "p-widgets");
const picked = rows();
pick("projects", "p-widgets");
$$("ctx-btn").dispatch("click");
$$("ctx-pop").querySelector('[data-ctx="c-old"]').dispatch("click");
facet("projects").querySelector('[data-part="fbtn"]').dispatch("click");
const legacy = [...facet("projects").querySelectorAll(".fopt")].map((o) => [o.dataset.val, o.querySelector("input").checked]);
return {{ all, opts, picked, legacy, legacyRows: rows(), tickets: [...keysOf("open"), ...keysOf("done")], feed: keysOf("feed") }};
}}"##
        ),
    );
    assert_eq!(r["all"].as_array().unwrap().len(), 2, "{r}");
    assert_eq!(r["opts"], json!([["p-widgets", "acme/widgets"]]), "{r}");
    assert_eq!(r["picked"].as_array().unwrap().len(), 1, "{r}");
    assert_eq!(r["legacy"], json!([["p-widgets", true]]), "{r}");
    assert_eq!(r["legacyRows"], r["picked"], "{r}");
    assert_eq!(r["tickets"], json!([TICKET]), "{r}");
    assert_eq!(r["feed"], json!(["m91"]), "{r}");
}

#[tokio::test]
async fn the_event_stream_pushes_an_issue_as_soon_as_a_thread_reports_it() {
    let s = start(H::new()).await;
    let mut r = s.http.get(format!("{}/api/events", s.base)).send().await.unwrap();
    let mut buf = Vec::new();
    assert_eq!(next_state(&mut r, &mut buf).await["newIssues"], json!([]));
    result(&s.h.exec_as(WORKER, "async () => wf.issue({ url: 'Acme/widgets#43', title: 'Fix the grader' })").await);
    let st = next_state(&mut r, &mut buf).await;
    assert_eq!(st["newIssues"][0]["label"], "widgets#43", "{st}");
    assert_eq!(st["counts"]["newIssues"], 1, "{st}");
}

#[tokio::test]
async fn new_issues_sit_above_tickets_with_their_links_and_a_count() {
    let (html, st) = new_issue_page().await;
    assert!(html.find(r#"id="p-new""#).unwrap() < html.find(r#"id="p-tix""#).unwrap());
    let r = new_scenario(
        &html,
        &st,
        r##"
emitState(base);
const one = row("n1");
const label = btn("n1", "label").querySelector("a");
const thread = btn("n1", "th").querySelector("a");
const shown = {
  rows: rows(), chip: q("#n-new").textContent, empty: sec().hasAttribute("data-empty"),
  label: [label.textContent, label.getAttribute("href")], title: btn("n1", "title").textContent,
  verb: btn("n1", "verb").textContent, at: btn("n1", "when").getAttribute("data-at"),
  thread: [thread.textContent, thread.getAttribute("href")],
  names: [btn("n1", "launch").getAttribute("aria-label"), btn("n1", "dismiss").getAttribute("aria-label")],
  gadget: btn("n2", "title").textContent, view: q("#new-view").hidden,
};
emitState({ ...base, newIssues: [], counts: { ...base.counts, newIssues: 0 } });
return { shown, after: { rows: rows(), chip: q("#n-new").textContent, empty: sec().hasAttribute("data-empty") } };
"##,
    );
    assert_eq!(r["shown"]["rows"], json!(["n2", "n1"]), "{r}");
    assert_eq!(r["shown"]["chip"], "2", "{r}");
    assert_eq!(r["shown"]["empty"], false, "{r}");
    assert_eq!(r["shown"]["label"], json!(["widgets#43", "https://github.com/Acme/widgets/issues/43"]), "{r}");
    assert_eq!(r["shown"]["title"], "Fix the grader", "{r}");
    assert_eq!(r["shown"]["verb"], "filed", "{r}");
    assert_eq!(r["shown"]["at"], T0, "{r}");
    assert_eq!(r["shown"]["thread"], json!([WORKER, format!("https://t3/{WORKER}")]), "{r}");
    assert_eq!(r["shown"]["names"], json!(["Launch a ticket for widgets#43", "Dismiss widgets#43"]), "{r}");
    assert_eq!(r["shown"]["gadget"], "", "{r}");
    assert_eq!(r["shown"]["view"], true, "{r}");
    assert_eq!(r["after"], json!({"rows": [], "chip": "", "empty": true}), "{r}");
}

#[tokio::test]
async fn launch_and_dismiss_post_to_the_entry_and_a_failed_launch_says_why() {
    let (html, st) = new_issue_page().await;
    let r = new_scenario(
        &html,
        &st,
        r##"
emitState(base);
fetchReply = answer({ ok: true, label: "widgets#43", thread: "launch:1" });
btn("n1", "launch").dispatch("click");
const busy = [btn("n1", "launch").textContent, btn("n1", "launch").getAttribute("aria-disabled")];
await flush();
const launched = { posts: posts(), err: q("#new-err").hidden, label: btn("n1", "launch").textContent };
fetchReply = answer({ ok: true });
btn("n2", "dismiss").dispatch("click");
await flush();
const dismissed = posts().slice(1);
fetchReply = answer({ ok: false, label: "widgets#43", error: "Cannot launch widgets#43: no T3 project", card: "a3" });
btn("n1", "launch").dispatch("click");
await flush();
const failed = { err: q("#new-err").hidden, text: q("#new-err-text").textContent, enabled: btn("n1", "launch").getAttribute("aria-disabled") };
return { busy, launched, dismissed, failed };
"##,
    );
    assert_eq!(r["busy"], json!(["Launching...", "true"]), "{r}");
    assert_eq!(r["launched"], json!({"posts": ["/api/new-issues/n1/launch"], "err": true, "label": "Launch"}), "{r}");
    assert_eq!(r["dismissed"], json!(["/api/new-issues/n2/dismiss"]), "{r}");
    assert_eq!(
        r["failed"],
        json!({"err": false, "text": "Cannot launch widgets#43: no T3 project (card a3)", "enabled": null}),
        "{r}"
    );
}

#[tokio::test]
async fn a_launched_issue_leaves_a_placeholder_while_the_reader_is_in_the_column() {
    let (html, st) = new_issue_page().await;
    let r = new_scenario(
        &html,
        &st,
        r##"
emitState(base);
q("#tixcol").dispatch("pointerenter");
fetchReply = answer({ ok: true, label: "widgets#43" });
btn("n1", "launch").dispatch("click");
await flush();
emitState({ ...base, newIssues: base.newIssues.filter((i) => i.id !== "n1") });
const held = { rows: rows(), note: row("n1").textContent, chip: q("#n-new").textContent };
q("#tixcol").dispatch("pointerleave");
advance(10);
return { held, after: rows() };
"##,
    );
    assert_eq!(r["held"], json!({"rows": ["n2", "n1"], "note": "widgets#43 launched", "chip": "1"}), "{r}");
    assert_eq!(r["after"], json!(["n2"]), "{r}");
}

#[tokio::test]
async fn a_new_issue_waits_behind_the_count_while_a_ticket_has_focus() {
    let (html, st) = new_issue_page().await;
    let r = new_scenario(
        &html,
        &st,
        r##"
const none = { ...base, newIssues: [], counts: { ...base.counts, newIssues: 0 } };
document.getElementById("tixcol").append(document.getElementById("open"));
emitState(none);
q("#open .tk").focus();
emitState(base);
const held = { rows: rows(), slot: q("#new-nis").textContent, off: q("#new-nis").classList.contains("off") };
q("#new-nis").dispatch("click");
return { held, after: rows(), off: q("#new-nis").classList.contains("off") };
"##,
    );
    assert_eq!(r["held"], json!({"rows": [], "slot": "2 new", "off": false}), "{r}");
    assert_eq!(r["after"], json!(["n2", "n1"]), "{r}");
    assert_eq!(r["off"], true, "{r}");
}

#[tokio::test]
async fn show_dismissed_lists_dismissed_issues_and_restore_brings_one_back() {
    let s = start(H::new()).await;
    result(&s.h.exec_as(WORKER, "async () => wf.issue({ url: 'Acme/widgets#43', title: 'Fix the grader' })").await);
    result(&s.h.exec_as(WORKER, "async () => wf.issue({ url: 'Acme/widgets#44' })").await);
    s.h.app.wf.dismiss_new_issue("n1").await.unwrap();
    let html = s.http.get(format!("{}/", s.base)).send().await.unwrap().text().await.unwrap();
    let st: Value = s.http.get(format!("{}/api/state", s.base)).send().await.unwrap().json().await.unwrap();
    let r = new_scenario(
        &html,
        &st,
        r##"
emitState(base);
const open = { rows: rows(), view: [q("#new-view").hidden, q("#new-view-t").textContent + " " + q("#n-dismissed").textContent, q("#new-view").getAttribute("aria-pressed")] };
q("#new-view").dispatch("click");
const view = {
  rows: rows(), head: q("#h-new-t").textContent, chip: q("#n-new").textContent,
  launch: btn("n1", "launch").hidden, restore: [btn("n1", "dismiss").textContent, btn("n1", "dismiss").getAttribute("aria-label")],
  verb: btn("n1", "verb").textContent, toggle: [q("#new-view-t").textContent, q("#n-dismissed").textContent, q("#new-view").getAttribute("aria-pressed")],
};
fetchReply = answer({ id: "n1", dismissed: false });
btn("n1", "dismiss").dispatch("click");
await flush();
const restored = { ...base.dismissedIssues[0], dismissed: false, dismissedAt: null };
emitState({ ...base, newIssues: [restored, ...base.newIssues], dismissedIssues: [] });
return { open, view, posts: posts(), back: { rows: rows(), head: q("#h-new-t").textContent, toggle: q("#new-view").hidden } };
"##,
    );
    assert_eq!(r["open"], json!({"rows": ["n2"], "view": [false, "Show dismissed 1", "false"]}), "{r}");
    assert_eq!(
        r["view"],
        json!({"rows": ["n1"], "head": "Dismissed issues", "chip": "", "launch": true,
               "restore": ["Restore", "Restore widgets#43"], "verb": "dismissed", "toggle": ["Show new", "", "true"]}),
        "{r}"
    );
    assert_eq!(r["posts"], json!(["/api/new-issues/n1/restore"]), "{r}");
    assert_eq!(r["back"], json!({"rows": ["n1", "n2"], "head": "New issues", "toggle": true}), "{r}");
}

#[tokio::test]
async fn the_board_offers_a_newer_release_and_a_reload_after_one() {
    let s = start(H::new()).await;
    let st: Value = s.http.get(format!("{}/api/state", s.base)).send().await.unwrap().json().await.unwrap();
    assert!(st["service"]["update"].is_null(), "no release is known before a check: {st}");
    s.h.app.wf.set_release(t3_codemode::release::Version::parse("9.9.9").unwrap());
    let st: Value = s.http.get(format!("{}/api/state", s.base)).send().await.unwrap().json().await.unwrap();
    let u = &st["service"]["update"];
    assert_eq!((u["latest"].as_str(), u["available"].as_bool()), (Some("9.9.9"), Some(true)), "{st}");
    assert!(u["notes"].as_str().unwrap().ends_with("/releases/tag/v9.9.9"), "{u}");
    let health: Value = s.http.get(format!("{}/healthz", s.base)).send().await.unwrap().json().await.unwrap();
    assert_eq!(health["update"]["latest"], "9.9.9");
    let html = s.http.get(format!("{}/", s.base)).send().await.unwrap().text().await.unwrap();
    assert!(html.contains(&format!("const PAGE_VERSION = \"{}\"", env!("CARGO_PKG_VERSION"))));
    let scenario = format!(
        r#"() => {{
  const base = {st};
  const $ = (id) => document.getElementById(id);
  const withUpdate = (latest) => ({{ ...base, service: {{ ...base.service, update: {{ ...base.service.update, latest, available: true }} }} }});
  const out = {{}};
  emitState(base);
  out.offered = !$("upd").hidden && $("upd-text").textContent;
  $("upd-btn").dispatch("click");
  out.opened = !$("upd-pop").hidden && $("upd-btn").getAttribute("aria-expanded");
  out.cmd = $("upd-cmd").textContent;
  out.notes = $("upd-notes").getAttribute("href");
  $("upd-hide").dispatch("click");
  out.hidden = $("upd").hidden && $("upd-pop").hidden;
  out.stored = localStorage.getItem("t3board.update.hidden");
  emitState(base);
  out.stillHidden = $("upd").hidden;
  emitState(withUpdate("9.9.10"));
  out.newer = !$("upd").hidden && $("upd-text").textContent;
  emitState({{ ...base, service: {{ ...base.service, version: "9.9.10", update: null }} }});
  out.reload = !$("upd").hidden && $("upd-text").textContent;
  out.go = $("upd-btn").classList.contains("go");
  emitState({{ ...base, service: {{ ...base.service, update: {{ ...base.service.update, available: false }} }} }});
  out.current = $("upd").hidden;
  return out;
}}"#
    );
    let r = run_page(&html, &scenario);
    assert_eq!(r["offered"], "Update to 9.9.9", "{r}");
    assert_eq!(r["opened"], "true", "{r}");
    assert_eq!(r["cmd"], t3_codemode::release::UPDATE_COMMAND, "{r}");
    assert!(r["notes"].as_str().unwrap().ends_with("/releases/tag/v9.9.9"), "{r}");
    assert_eq!((r["hidden"].as_bool(), r["stored"].as_str()), (Some(true), Some("9.9.9")), "{r}");
    assert_eq!(r["stillHidden"], true, "{r}");
    assert_eq!(r["newer"], "Update to 9.9.10", "{r}");
    assert_eq!(r["reload"], "Reload for 9.9.10", "{r}");
    assert_eq!(r["go"], true, "{r}");
    assert_eq!(r["current"], true, "{r}");
}

const LAYOUT_PRELUDE: &str = r##"
const __mq = { wide: false, ls: [] };
globalThis.matchMedia = (query) => ({
  get matches() { return /min-width: 1200px/.test(query) ? __mq.wide : false; },
  addEventListener: (_, f) => __mq.ls.push(f),
});
"##;

const LAYOUT_HELPERS: &str = r##"
const setWide = (on) => { __mq.wide = on; for (const f of __mq.ls) f(); };
const heads = () => {
  for (const id of ["p-dec", "p-new", "p-tix", "p-act"]) {
    const ph = document.createElement("div"); ph.className = "ph"; document.getElementById(id).append(ph);
  }
  const form = document.getElementById("launch");
  const ph = document.createElement("div"); ph.className = "ph"; form.append(ph);
  document.getElementById("composer").append(form);
};
const main = () => document.getElementById("main");
const where = () => main().children.filter((c) => c.classList.contains("lcol")).map((c) => c.children.filter((x) => x.id).map((x) => x.id));
const grip = (id) => q("#" + id + " .grip");
const fold = (id) => q("#" + id + " .fold");
const key = (el, k) => el.dispatch("keydown", { key: k, stopPropagation() {} });
const saved = () => JSON.parse(localStorage.getItem("t3board.layout") || "null");
"##;

fn layout_scenario(html: &str, st: &Value, storage: &str, body: &str) -> Value {
    let prelude = format!("globalThis.__storage = {storage};\n{LAYOUT_PRELUDE}");
    run_page_async_with(
        html,
        &prelude,
        &format!("async () => {{\n{PAGE_HELPERS}\n{LAYOUT_HELPERS}\nconst base = {st};\nemitState(base);\n{body}\n}}"),
    )
}

#[tokio::test]
async fn panels_move_and_collapse_from_the_keyboard_and_the_layout_is_saved() {
    let (html, st) = new_issue_page().await;
    let r = layout_scenario(
        &html,
        &st,
        "{}",
        r##"
heads();
setWide(true);
const start = { where: where(), grips: document.querySelectorAll(".grip").length, reset: q("#lay-reset").hidden, saved: saved() };
grip("p-dec").focus();
key(grip("p-dec"), "ArrowRight");
const moved = { where: where(), focus: document.activeElement === grip("p-dec") };
advance(100);
moved.said = q("#live").textContent;
key(grip("p-dec"), "ArrowLeft");
key(grip("p-dec"), "ArrowLeft");
advance(100);
const edge = { where: where(), said: q("#live").textContent };
key(grip("p-act"), "ArrowUp");
fold("p-new").dispatch("click");
advance(100);
return {
  start, moved, edge,
  after: {
    where: where(), up: where()[2],
    collapsed: q("#p-new").classList.contains("collapsed"),
    expanded: fold("p-new").getAttribute("aria-expanded"),
    label: fold("p-new").getAttribute("aria-label"),
    said: q("#live").textContent,
    reset: q("#lay-reset").hidden,
    saved: saved(),
  },
};
"##,
    );
    assert_eq!(
        r["start"],
        json!({"where": [["p-dec"], ["p-new", "p-tix"], ["composer", "p-act"]], "grips": 5, "reset": true, "saved": null}),
        "{r}"
    );
    assert_eq!(r["moved"]["where"], json!([[], ["p-dec", "p-new", "p-tix"], ["composer", "p-act"]]), "{r}");
    assert_eq!(r["moved"]["focus"], true, "{r}");
    assert_eq!(r["moved"]["said"], "Needs you moved to column 2, position 1 of 3.", "{r}");
    assert_eq!(r["edge"]["where"], json!([["p-dec"], ["p-new", "p-tix"], ["composer", "p-act"]]), "{r}");
    assert_eq!(r["edge"]["said"], "Needs you is already at column 1, position 1 of 1.", "{r}");
    assert_eq!(r["after"]["up"], json!(["p-act", "composer"]), "{r}");
    assert_eq!(r["after"]["collapsed"], true, "{r}");
    assert_eq!(r["after"]["expanded"], "false", "{r}");
    assert_eq!(r["after"]["label"], "Expand New issues", "{r}");
    assert_eq!(r["after"]["said"], "New issues collapsed.", "{r}");
    assert_eq!(r["after"]["reset"], false, "{r}");
    assert_eq!(
        r["after"]["saved"],
        json!({"cols": [{"w": null, "p": ["dec"]}, {"w": null, "p": ["new", "tix"]}, {"w": null, "p": ["act", "cmd"]}], "h": {}, "c": {"new": true}}),
        "{r}"
    );
}

#[tokio::test]
async fn a_saved_layout_comes_back_and_a_bad_one_falls_back_to_the_default() {
    let (html, st) = two_gate_page().await;
    let good = r#"{"t3board.layout": "{\"cols\":[{\"w\":null,\"p\":[]},{\"w\":900,\"p\":[\"new\",\"dec\",\"tix\"]},{\"w\":400,\"p\":[\"act\",\"cmd\"]}],\"h\":{\"dec\":500,\"bogus\":3},\"c\":{\"act\":true,\"tix\":\"yes\"}}"}"#;
    let r = layout_scenario(
        &html,
        &st,
        good,
        r##"
heads();
setWide(true);
const back = {
  where: where(), act: q("#p-act").classList.contains("collapsed"), tix: q("#p-tix").classList.contains("collapsed"),
  flex: [q("#p-dec").style.flex, main().children.filter((c) => c.classList.contains("lcol"))[1].style.flex],
  vacant: main().children.filter((c) => c.classList.contains("lcol"))[0].classList.contains("vacant"),
  reset: q("#lay-reset").hidden,
};
q("#lay-reset").dispatch("click");
return { back, reset: { where: where(), act: q("#p-act").classList.contains("collapsed"), saved: saved(), hidden: q("#lay-reset").hidden } };
"##,
    );
    assert_eq!(
        r["back"],
        json!({"where": [[], ["p-new", "p-dec", "p-tix"], ["p-act", "composer"]], "act": true, "tix": false,
               "flex": ["500 1 0px", "900 1 0px"], "vacant": true, "reset": false}),
        "{r}"
    );
    assert_eq!(
        r["reset"],
        json!({"where": [["p-dec"], ["p-new", "p-tix"], ["composer", "p-act"]], "act": false, "saved": null, "hidden": true}),
        "{r}"
    );
    for bad in [
        r#"{"t3board.layout": "{\"cols\":[{\"p\":[\"dec\",\"dec\"]},{\"p\":[\"new\",\"tix\"]},{\"p\":[\"cmd\"]}]}"}"#,
        r#"{"t3board.layout": "{\"cols\":[{\"p\":[\"dec\",\"new\",\"tix\",\"cmd\",\"act\"]}]}"}"#,
        r#"{"t3board.layout": "not json"}"#,
    ] {
        let r = layout_scenario(&html, &st, bad, "heads();\nsetWide(true);\nreturn where();");
        assert_eq!(r, json!([["p-dec"], ["p-new", "p-tix"], ["composer", "p-act"]]), "{bad}");
    }
}

#[tokio::test]
async fn narrow_widths_get_the_fixed_layout_back_and_wide_restores_the_saved_one() {
    let (html, st) = two_gate_page().await;
    let r = layout_scenario(
        &html,
        &st,
        "{}",
        r##"
heads();
setWide(true);
key(grip("p-act"), "ArrowLeft");
fold("p-tix").dispatch("click");
const wide = where();
setWide(false);
const narrow = {
  cols: main().children.filter((c) => c.classList.contains("lcol")).length,
  tixcol: q("#tixcol").children.map((x) => x.id),
  top: main().children.filter((x) => ["p-dec", "tixcol", "p-act", "composer"].includes(x.id)).map((x) => x.id),
  collapsed: document.querySelectorAll(".collapsed").length,
  splits: document.querySelectorAll(".vsplit").length + document.querySelectorAll(".hsplit").length,
  lay: main().classList.contains("lay"),
  reset: q("#lay-reset").hidden,
};
setWide(true);
return { wide, narrow, again: where(), tix: q("#p-tix").classList.contains("collapsed") };
"##,
    );
    assert_eq!(r["wide"], json!([["p-dec"], ["p-new", "p-act", "p-tix"], ["composer"]]), "{r}");
    assert_eq!(
        r["narrow"],
        json!({"cols": 0, "tixcol": ["p-new", "p-tix"], "top": ["p-dec", "tixcol", "p-act", "composer"],
               "collapsed": 0, "splits": 0, "lay": false, "reset": true}),
        "{r}"
    );
    assert_eq!(r["again"], r["wide"], "{r}");
    assert_eq!(r["tix"], true, "{r}");
}

#[tokio::test]
async fn a_shortcut_to_a_collapsed_panel_expands_it_and_resizing_is_keyboard_driven() {
    let (html, st) = new_issue_page().await;
    let r = layout_scenario(
        &html,
        &st,
        "{}",
        r##"
heads();
setWide(true);
fold("p-act").dispatch("click");
const shut = q("#p-act").classList.contains("collapsed");
jump("activity");
const splits = { v: document.querySelectorAll(".vsplit").map((s) => [s.getAttribute("role"), s.getAttribute("aria-orientation"), s.getAttribute("aria-label")]),
  h: document.querySelectorAll(".hsplit").filter((s) => !s.classList.contains("fixed")).map((s) => s.getAttribute("aria-label")) };
const hs = document.querySelectorAll(".hsplit").filter((s) => !s.classList.contains("fixed"))[0];
key(hs, "ArrowDown");
const flat = saved();
q("#p-new").rect = { height: 200 };
q("#p-tix").rect = { height: 150 };
key(hs, "ArrowDown");
const small = saved();
q("#p-tix").rect = { height: 500 };
key(hs, "ArrowDown");
key(hs, "ArrowUp");
key(hs, "ArrowDown");
return { shut, open: q("#p-act").classList.contains("collapsed"), splits, flat, small, sized: saved().h, flex: [q("#p-new").style.flex, q("#p-tix").style.flex] };
"##,
    );
    assert_eq!(r["shut"], true, "{r}");
    assert_eq!(r["open"], false, "{r}");
    assert_eq!(
        r["splits"],
        json!({"v": [["separator", "vertical", "Resize column 1 and column 2"], ["separator", "vertical", "Resize column 2 and column 3"]],
               "h": ["Resize New issues and Tickets", "Resize Launch and Ask and Activity"]}),
        "{r}"
    );
    assert_eq!(r["flat"], Value::Null, "{r}");
    assert_eq!(
        r["small"],
        json!({"cols": [{"w": null, "p": ["dec"]}, {"w": null, "p": ["new", "tix"]}, {"w": null, "p": ["cmd", "act"]}], "h": {"new": 224, "tix": 126}, "c": {}}),
        "{r}"
    );
    assert_eq!(r["sized"], json!({"new": 224, "tix": 476}), "{r}");
    assert_eq!(r["flex"], json!(["224 1 0px", "476 1 0px"]), "{r}");
}

#[tokio::test]
async fn layout_controls_stay_inert_on_narrow_screens_and_keep_columns_usable() {
    let (html, st) = two_gate_page().await;
    let r = layout_scenario(
        &html,
        &st,
        "{}",
        r##"
heads();
setWide(true);
setWide(false);
q("#p-dec .ph").dispatch("dblclick");
q("#p-dec .ph").dispatch("pointerdown", { button: 0, isPrimary: true, clientX: 5, clientY: 5 });
const narrow = { cols: main().children.filter((c) => c.classList.contains("lcol")).length, collapsed: document.querySelectorAll(".collapsed").length };
setWide(true);
const cols = main().children.filter((c) => c.classList.contains("lcol"));
cols[0].rect = { width: 400 }; cols[1].rect = { width: 700 }; cols[2].rect = { width: 340 };
key(document.querySelectorAll(".vsplit")[0], "ArrowRight");
key(grip("p-new"), "ArrowLeft");
key(grip("p-tix"), "ArrowRight");
const emptied = where()[1];
key(grip("p-tix"), "ArrowLeft");
const back = { where: where()[1], flex: cols[1].style.flex, min: cols[1].style.minWidth, w: saved().cols.map((c) => c.w) };
return { narrow, emptied, back };
"##,
    );
    assert_eq!(html.matches(r#"class="lay-only" hidden"#).count(), 8);
    assert_eq!(r["narrow"], json!({"cols": 0, "collapsed": 0}), "{r}");
    assert_eq!(r["emptied"], json!([]), "{r}");
    assert_eq!(
        r["back"],
        json!({"where": ["p-tix"], "flex": "382 1 0px", "min": "320px", "w": [424, 382, 340]}),
        "{r}"
    );
}

#[tokio::test]
async fn new_issues_hold_follows_their_column_and_jumps_open_collapsed_panels() {
    let (html, st) = two_gate_page().await;
    let r = layout_scenario(
        &html,
        &st,
        "{}",
        r##"
heads();
setWide(true);
key(grip("p-act"), "ArrowLeft");
const cols = main().children.filter((c) => c.classList.contains("lcol"));
cols[1].dispatch("pointerenter");
const inNewColumn = newView.colPointer;
cols[1].dispatch("pointerleave");
cols[2].dispatch("pointerenter");
const elsewhere = newView.colPointer;
cols[2].dispatch("pointerleave");
const item = document.createElement("li");
q("#p-act").append(item);
item.focus();
const focusHolds = !newLive(panels.newIssues);
fold("p-dec").dispatch("click");
jumpToCard(base.cards[0].id);
const decOpen = !q("#p-dec").classList.contains("collapsed");
fold("p-tix").dispatch("click");
jumpToTicket(base.cards[0].ticket || "nope");
const tixOpen = !q("#p-tix").classList.contains("collapsed");
const hs = document.querySelectorAll(".hsplit").filter((s) => !s.classList.contains("fixed"))[0];
hs.focus();
hs.dispatch("dblclick");
return { inNewColumn, elsewhere, focusHolds, decOpen, tixOpen, splitKept: document.activeElement === hs && hs.isConnected };
"##,
    );
    assert_eq!(
        r,
        json!({"inNewColumn": true, "elsewhere": false, "focusHolds": true, "decOpen": true, "tixOpen": true, "splitKept": true}),
        "{r}"
    );
}

#[tokio::test]
async fn collapsing_keeps_scroll_and_focus_and_narrowing_keeps_focus() {
    let (html, st) = two_gate_page().await;
    let r = layout_scenario(
        &html,
        &st,
        "{}",
        r##"
heads();
setWide(true);
const hint = q("#lay-hint").hidden;
const list = document.createElement("ol"); list.className = "scroll";
const item = document.createElement("li"); list.append(item);
const body = document.createElement("div"); body.className = "body"; body.append(list);
q("#p-act").append(body);
list.scrollTop = 300;
item.focus();
fold("p-act").dispatch("click");
const shut = { focus: document.activeElement === fold("p-act"), saved: lay.scroll.act };
list.scrollTop = 0;
fold("p-act").dispatch("click");
const open = list.scrollTop;
key(grip("p-new"), "ArrowRight");
const cols = main().children.filter((c) => c.classList.contains("lcol"));
cols[1].dispatch("pointerenter");
const holdOld = newPointer();
cols[1].dispatch("pointerleave");
cols[2].dispatch("pointerenter");
const holdNew = newPointer();
const nlist = document.createElement("ol"); nlist.className = "scroll";
const nbody = document.createElement("div"); nbody.className = "body"; nbody.append(nlist);
q("#p-new").append(nbody);
nlist.scrollTop = 120;
fold("p-new").dispatch("click");
const heldShut = !newLive(panels.newIssues);
fold("p-new").dispatch("click");
nlist.scrollTop = 0;
const ask = document.getElementById("ask");
const question = document.getElementById("question");
ask.append(question);
q("#composer").append(ask);
question.focus();
setWide(false);
return { hint, shut, open, holdOld, holdNew, heldShut, narrowFocus: document.activeElement === question, mode: q("#composer").getAttribute("data-mode"), narrowHint: q("#lay-hint").hidden };
"##,
    );
    assert_eq!(
        r,
        json!({"hint": false, "shut": {"focus": true, "saved": [300]}, "open": 300, "holdOld": false, "holdNew": true, "heldShut": true,
               "narrowFocus": true, "mode": "ask", "narrowHint": true}),
        "{r}"
    );
}

#[tokio::test]
async fn with_no_new_issues_nothing_renders_above_tickets() {
    let (html, st) = new_issue_page().await;
    assert!(html.contains(r#"data-view="open" data-empty="" hidden>"#));
    let r = new_scenario(
        &html,
        &st,
        r##"
const none = { ...base, newIssues: [], counts: { ...base.counts, newIssues: 0 } };
emitState(none);
const empty = { sec: sec().hidden, route: q("#dismissed-open").hidden };
emitState(base);
const shown = { sec: sec().hidden, route: q("#dismissed-open").hidden, view: q("#new-view").hidden };
emitState(none);
return { empty, shown, again: sec().hidden };
"##,
    );
    assert_eq!(r["empty"], json!({"sec": true, "route": true}), "{r}");
    assert_eq!(r["shown"], json!({"sec": false, "route": true, "view": true}), "{r}");
    assert_eq!(r["again"], true, "{r}");
}

#[tokio::test]
async fn only_issues_and_tickets_that_arrive_live_flash_and_the_flash_fades() {
    let (html, st) = new_issue_page().await;
    let r = new_scenario(
        &html,
        &st,
        r##"
const lit = (el) => !!el && el.classList.contains("arrive");
const tk = (k) => q('#open [data-key="' + k + '"]');
emitState(base);
const atLoad = { rows: rows().map((k) => lit(row(k))), ticket: lit(tk(base.tree[0].thread)) };
const issue = { ...base.newIssues[0], id: "n9", label: "widgets#49", number: 49, url: "https://github.com/Acme/widgets/issues/49" };
const ticket = { ...base.tree[0], thread: "t-new", label: "widgets#50" };
emitState({ ...base, newIssues: [issue, ...base.newIssues], tree: [...base.tree, ticket] });
const arrived = { issue: lit(row("n9")), ticket: lit(tk("t-new")), old: lit(row("n1")) };
advance(4000);
const fading = lit(row("n9")) && lit(tk("t-new"));
advance(200);
return { atLoad, arrived, fading, faded: lit(row("n9")) || lit(tk("t-new")) };
"##,
    );
    assert_eq!(r["atLoad"], json!({"rows": [false, false], "ticket": false}), "{r}");
    assert_eq!(r["arrived"], json!({"issue": true, "ticket": true, "old": false}), "{r}");
    assert_eq!(r["fading"], true, "{r}");
    assert_eq!(r["faded"], false, "{r}");
    assert!(html.contains(".nrow.arrive, .tk.arrive { animation: flash 4s ease-in; }"));
}

#[tokio::test]
async fn an_issue_held_behind_the_count_flashes_when_revealed() {
    let (html, st) = new_issue_page().await;
    let r = new_scenario(
        &html,
        &st,
        r##"
emitState(base);
q("#tixcol").dispatch("pointerenter");
const issue = { ...base.newIssues[0], id: "n9", label: "widgets#49", number: 49 };
emitState({ ...base, newIssues: [issue, ...base.newIssues] });
const held = { rows: rows(), slot: q("#new-nis").textContent };
q("#new-nis").dispatch("click");
return { held, rows: rows(), lit: row("n9").classList.contains("arrive") };
"##,
    );
    assert_eq!(r["held"], json!({"rows": ["n2", "n1"], "slot": "1 new"}), "{r}");
    assert_eq!(r["rows"], json!(["n9", "n2", "n1"]), "{r}");
    assert_eq!(r["lit"], true, "{r}");
}

#[tokio::test]
async fn reduced_motion_gets_a_short_static_tint() {
    let (html, st) = new_issue_page().await;
    assert!(html.contains("  .nrow.arrive, .tk.arrive { background-color: var(--flash); }\n}"));
    let prelude = r#"globalThis.matchMedia = (m) => ({ matches: /reduce/.test(m), addEventListener() {} });"#;
    let body = r##"
emitState(base);
const issue = { ...base.newIssues[0], id: "n9", label: "widgets#49", number: 49 };
emitState({ ...base, newIssues: [issue, ...base.newIssues] });
const lit = row("n9").classList.contains("arrive");
advance(1500);
return { lit, after: row("n9").classList.contains("arrive") };
"##;
    let r = run_page_async_with(
        &html,
        prelude,
        &format!("async () => {{\n{PAGE_HELPERS}\n{NEW_HELPERS}\nconst base = {st};\n{body}\n}}"),
    );
    assert_eq!(r, json!({"lit": true, "after": false}));
}

#[tokio::test]
async fn dismissed_issues_stay_reachable_from_settled_while_none_are_new() {
    let s = start(H::new()).await;
    result(&s.h.exec_as(WORKER, "async () => wf.issue({ url: 'Acme/widgets#43', title: 'Fix the grader' })").await);
    s.h.app.wf.dismiss_new_issue("n1").await.unwrap();
    let html = s.http.get(format!("{}/", s.base)).send().await.unwrap().text().await.unwrap();
    let st: Value = s.http.get(format!("{}/api/state", s.base)).send().await.unwrap().json().await.unwrap();
    let r = new_scenario(
        &html,
        &st,
        r##"
emitState(base);
const hidden = { sec: sec().hidden, route: q("#dismissed-open").hidden, n: q("#n-dismissed-s").textContent,
  name: q("#dismissed-open").getAttribute("aria-label") };
sec().append(q("#new-view"));
q("#dismissed-open").dispatch("click");
const view = { sec: sec().hidden, rows: rows(), head: q("#h-new-t").textContent, route: q("#dismissed-open").hidden,
  focus: document.activeElement === q("#new-view") };
q("#new-view").dispatch("click");
return { hidden, view, back: sec().hidden, refocus: document.activeElement === q("#dismissed-open") };
"##,
    );
    assert_eq!(r["hidden"], json!({"sec": true, "route": false, "n": "1", "name": "Show 1 dismissed issues"}), "{r}");
    assert_eq!(
        r["view"],
        json!({"sec": false, "rows": ["n1"], "head": "Dismissed issues", "route": true, "focus": true}),
        "{r}"
    );
    assert_eq!((&r["back"], &r["refocus"]), (&json!(true), &json!(true)), "{r}");
}

#[tokio::test]
async fn clearing_the_filter_over_tickets_brings_issues_back_at_once_without_a_flash() {
    let (html, st) = new_issue_page().await;
    let r = new_scenario(
        &html,
        &st,
        r##"
emitState(base);
q("#tixcol").dispatch("pointerenter");
const box = document.getElementById("filter-q");
box.focus();
typeKeys("gadgets");
advance(200);
const narrowed = rows();
box.value = "";
box.dispatch("input");
advance(200);
return { narrowed, rows: rows(), slot: q("#new-nis").textContent, lit: rows().map((k) => row(k).classList.contains("arrive")) };
"##,
    );
    assert_eq!(r, json!({"narrowed": ["n2"], "rows": ["n2", "n1"], "slot": "", "lit": [false, false]}));
}

#[tokio::test]
async fn the_wide_layout_drops_the_divider_above_tickets_while_new_issues_hides() {
    let (html, st) = new_issue_page().await;
    let none = json!({"newIssues": [], "counts": {"newIssues": 0}});
    let r = layout_scenario(
        &html,
        &st,
        "{}",
        &format!(
            r##"
const none = {{ ...base, ...{none}, counts: {{ ...base.counts, newIssues: 0 }} }};
heads();
setWide(true);
const col = () => main().children.filter((c) => c.classList.contains("lcol"))[1];
const kids = () => col().children.map((x) => x.id || x.className);
const shown = kids();
emitState(none);
const hidden = {{ kids: kids(), vacant: col().classList.contains("vacant") }};
emitState(base);
return {{ shown, hidden, back: kids() }};
"##
        ),
    );
    assert_eq!(r["shown"], json!(["p-new", "hsplit", "p-tix"]), "{r}");
    assert_eq!(r["hidden"], json!({"kids": ["p-new", "p-tix"], "vacant": false}), "{r}");
    assert_eq!(r["back"], json!(["p-new", "hsplit", "p-tix"]), "{r}");
}

fn skills_home(h: &H) -> std::path::PathBuf {
    h.dir.path().join("home")
}

fn install_skills(home: &std::path::Path, release: &str) {
    use t3_codemode::skills::{self, Bundle, Mode, Options, Paths};
    let emb = Bundle::embedded();
    let files: Vec<(String, Vec<u8>)> =
        emb.expected().keys().map(|p| (p.clone(), emb.raw(p).unwrap().to_vec())).collect();
    let opts = Options { mode: Mode::Install, force: false, take_over: false };
    skills::sync(&Paths::under(home, None), &Bundle::new(release, files), opts).unwrap();
}

fn legacy_setup(home: &std::path::Path) {
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    let cfg = home.join(".config").join("t3-workflow").join("config.json");
    std::fs::write(cfg, r#"{"version":1,"passes":["pr"]}"#).unwrap();
}

async fn state_and_health(s: &Srv) -> (Value, Value) {
    let st: Value = s.http.get(format!("{}/api/state", s.base)).send().await.unwrap().json().await.unwrap();
    let health: Value = s.http.get(format!("{}/healthz", s.base)).send().await.unwrap().json().await.unwrap();
    (st, health)
}

const DRIFT_SCENARIO: &str = r#"() => {
  const $ = (id) => document.getElementById(id);
  emitState(base);
  const out = { shown: !$("drift").hidden, label: $("drift-text").textContent, aria: $("drift-btn").getAttribute("aria-label") };
  if (!out.shown) return out;
  $("drift-btn").dispatch("click");
  out.opened = !$("drift-pop").hidden && $("drift-btn").getAttribute("aria-expanded");
  out.msg = $("drift-msg").textContent;
  out.cmds = $("drift-fixes").querySelectorAll("code").map((c) => c.textContent);
  for (const f of document.listeners.click) f({ type: "click", target: $("counts") });
  out.outside = $("drift-pop").hidden;
  emitState({ ...base, service: { ...base.service, skills: { ...base.service.skills, fixes: [], missingSteps: [], fix: null, foreign: null, current: true, installed: base.service.skills.release } } });
  out.cleared = $("drift").hidden && $("drift-pop").hidden;
  return out;
}"#;

fn drift_page(html: &str, st: &Value) -> Value {
    run_page(html, &format!("(() => {{ const base = {st}; return ({DRIFT_SCENARIO})(); }})"))
}

#[tokio::test]
async fn stale_skills_and_a_missing_rule_show_in_healthz_and_the_header() {
    let s = start(H::new()).await;
    let home = skills_home(&s.h);
    install_skills(&home, "0.0.1");
    legacy_setup(&home);
    s.h.app.wf.refresh_skills();
    let (st, health) = state_and_health(&s).await;
    let k = &health["skills"];
    assert_eq!(k, &st["service"]["skills"], "/healthz and /api/state report one view");
    assert_eq!(
        (k["release"].as_str(), k["installed"].as_str()),
        (Some(env!("CARGO_PKG_VERSION")), Some("0.0.1")),
        "{k}"
    );
    assert_eq!((k["present"].as_bool(), k["current"].as_bool()), (Some(true), Some(false)), "{k}");
    assert_eq!(k["fixes"], json!(["t3cm skills update"]), "{k}");
    let missing: Vec<(&str, &str)> = k["missingSteps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| (m["id"].as_str().unwrap(), m["fix"].as_str().unwrap()))
        .collect();
    assert_eq!(missing, [("rule:filed-issues", "t3cm skills update")], "{k}");
    let html = s.http.get(format!("{}/", s.base)).send().await.unwrap().text().await.unwrap();
    let r = drift_page(&html, &st);
    assert_eq!(r["label"], "Skills out of date", "{r}");
    assert_eq!(r["aria"], "Skills out of date. Fix: t3cm skills update", "{r}");
    assert_eq!(r["opened"], "true", "{r}");
    let msg = r["msg"].as_str().unwrap();
    assert!(msg.contains("are at 0.0.1; t3cm runs") && msg.contains("The filed-issue rule is missing"), "{r}");
    assert_eq!(r["cmds"], json!(["t3cm skills update"]), "{r}");
    assert_eq!(r["outside"], true, "a click elsewhere closes the panel: {r}");
    assert_eq!(r["cleared"], true, "{r}");
}

#[tokio::test]
async fn the_rule_alone_missing_is_a_setup_step_and_clears_after_update() {
    let s = start(H::new()).await;
    let home = skills_home(&s.h);
    install_skills(&home, env!("CARGO_PKG_VERSION"));
    legacy_setup(&home);
    let st = s.h.exec("async () => wf.status()").await;
    let k = result(&st)["skills"].clone();
    assert_eq!((k["fix"].is_null(), k["current"].as_bool()), (true, Some(false)), "{k}");
    assert_eq!(k["fixes"], json!(["t3cm skills update"]), "{k}");
    let (state, health) = state_and_health(&s).await;
    assert_eq!(health["skills"], k);
    let html = s.http.get(format!("{}/", s.base)).send().await.unwrap().text().await.unwrap();
    let r = drift_page(&html, &state);
    assert_eq!(r["label"], "Setup step missing", "{r}");
    assert_eq!(r["cmds"], json!(["t3cm skills update"]), "{r}");
    t3_codemode::steps::apply(&t3_codemode::steps::Env::under(&home, None), &t3_codemode::skills::Bundle::embedded())
        .unwrap();
    let (_, health) = state_and_health(&s).await;
    assert_eq!(health["skills"]["current"], false, "a read between watcher passes is served from the cache");
    let mut events = s.h.app.wf.subscribe();
    let fresh = result(&s.h.exec("async () => (await wf.status()).skills").await);
    assert_eq!((fresh["current"].as_bool(), &fresh["fixes"]), (Some(true), &json!([])), "{fresh}");
    assert!(events.try_recv().is_ok(), "a changed report pushes a board update");
    let (state, health) = state_and_health(&s).await;
    assert_eq!(health["skills"], fresh);
    assert_eq!(drift_page(&html, &state)["shown"], false);
}

#[tokio::test]
async fn an_npx_skills_install_shows_as_skew_with_the_take_over_fix() {
    let s = start(H::new()).await;
    let home = skills_home(&s.h);
    install_skills(&home, env!("CARGO_PKG_VERSION"));
    std::fs::remove_file(home.join(".config").join("t3-workflow").join("installed.json")).unwrap();
    let lock = json!({"version": 3, "skills": {"t3-worker": {"source": "OttoOps/t3-codemode", "commit": "8e179c3"}}});
    std::fs::write(home.join(".agents").join(".skill-lock.json"), lock.to_string()).unwrap();
    s.h.app.wf.refresh_skills();
    let (st, health) = state_and_health(&s).await;
    let k = &health["skills"];
    assert_eq!(k["foreign"]["tool"], "npx skills", "{k}");
    assert_eq!(k["installed"], Value::Null, "{k}");
    assert_eq!(k["fixes"][0], "t3cm skills install --take-over", "{k}");
    let html = s.http.get(format!("{}/", s.base)).send().await.unwrap().text().await.unwrap();
    let r = drift_page(&html, &st);
    assert_eq!(r["label"], "Skills not managed", "{r}");
    assert!(r["msg"].as_str().unwrap().contains("installed by npx skills and match t3cm"), "{r}");
    assert_eq!(r["cmds"][0], "t3cm skills install --take-over", "{r}");
}

#[tokio::test]
async fn a_machine_without_the_skills_shows_no_skills_chip() {
    let s = start(H::new()).await;
    let (st, health) = state_and_health(&s).await;
    let k = &health["skills"];
    assert_eq!((k["present"].as_bool(), k["current"].as_bool()), (Some(false), Some(false)), "{k}");
    assert_eq!(k["fixes"][0], "t3cm skills install", "{k}");
    let html = s.http.get(format!("{}/", s.base)).send().await.unwrap().text().await.unwrap();
    assert_eq!(drift_page(&html, &st)["shown"], false);
}

#[tokio::test]
async fn a_new_slot_or_a_broken_config_names_the_setup_skill() {
    let s = start(H::new()).await;
    let home = skills_home(&s.h);
    install_skills(&home, env!("CARGO_PKG_VERSION"));
    legacy_setup(&home);
    let env = t3_codemode::steps::Env::under(&home, None);
    t3_codemode::steps::apply(&env, &t3_codemode::skills::Bundle::embedded()).unwrap();
    let mut cfg: Value = serde_json::from_str(&std::fs::read_to_string(&env.config).unwrap()).unwrap();
    cfg["steps"].as_array_mut().unwrap().retain(|id| id != "slot:models");
    std::fs::write(&env.config, cfg.to_string()).unwrap();
    let k = result(&s.h.exec("async () => (await wf.status()).skills").await);
    assert_eq!(
        (k["current"].as_bool(), &k["fixes"]),
        (Some(true), &json!(["run setup-t3-workflow: new slot models"])),
        "{k}"
    );
    let (st, _) = state_and_health(&s).await;
    let html = s.http.get(format!("{}/", s.base)).send().await.unwrap().text().await.unwrap();
    let r = drift_page(&html, &st);
    assert_eq!(r["label"], "Setup step missing", "{r}");
    assert!(r["msg"].as_str().unwrap().contains("Setup has a new slot to pick: models."), "{r}");
    std::fs::write(&env.config, "{not json").unwrap();
    let k = result(&s.h.exec("async () => (await wf.status()).skills").await);
    assert_eq!(k["configState"], "problem", "{k}");
    let fix = k["fixes"][0].as_str().unwrap();
    assert!(fix.starts_with("fix ") && fix.ends_with("config.json, then run setup-t3-workflow"), "{k}");
    let (st, _) = state_and_health(&s).await;
    let r = drift_page(&html, &st);
    assert_eq!(r["label"], "Workflow config ignored", "{r}");
    assert!(r["msg"].as_str().unwrap().contains("The T3 workflow config is ignored: not JSON"), "{r}");
}

fn write_workflow_config(h: &H, cfg: Value) {
    let p = h.dir.path().join("home/.config/t3-workflow/config.json");
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, cfg.to_string()).unwrap();
}

fn codex_provider(id: &str, models: &[&str]) -> Value {
    let mut p = t3_codemode::fake::provider(id, "codex", models);
    for m in p["models"].as_array_mut().unwrap() {
        m["options"] = json!([{"id": "reasoningEffort", "values": ["low", "medium", "high", "xhigh"]}]);
    }
    p
}

#[tokio::test]
async fn board_launch_resolves_user_config_repo_agents_md_and_precedence() {
    use t3_codemode::fake::provider;

    let s = start(H::new()).await;
    let repo = s.h.dir.path().join("widgets");
    std::fs::create_dir_all(&repo).unwrap();
    s.h.fake.set_projects(vec![project("p-widgets", &repo.to_string_lossy())]);
    s.h.remotes.set(&repo.to_string_lossy(), "git@github.com:acme/widgets.git");
    s.h.fake.set_providers(vec![
        provider("claudeAgent", "claudeAgent", &["claude-opus-5-5", "claude-haiku-5-5", "claude-catalog-only"]),
        provider("claude-2", "claudeAgent", &["claude-opus-5-5", "claude-haiku-5-5", "claude-catalog-only"]),
        codex_provider("codex", &["gpt-6.2-sol"]),
    ]);

    // 1. User config sets instance, model and effort (even though claudeAgent is listed first).
    write_workflow_config(
        &s.h,
        json!({"models": {"ticket": {"provider": "claude-2", "model": "claude-opus-5-5", "effort": "medium"}}}),
    );
    let r = s.post("/api/launch", json!({"issues": ["acme/widgets#1"]}), Some(&s.csrf()), None).await;
    assert_eq!(r.status(), 200);
    let body: Value = r.json().await.unwrap();
    let res = &body["results"][0];
    assert_eq!(res["ok"], true, "{res}");
    assert_eq!(res["instanceId"], "claude-2");
    assert_eq!(res["model"], "claude-opus-5-5");
    assert_eq!(res["effort"], "medium");
    assert_eq!(res["modelSource"], "user");
    assert_eq!(
        s.h.fake.launched()[0]["modelSelection"],
        json!({"instanceId": "claude-2", "model": "claude-opus-5-5", "options": [{"id": "effort", "value": "medium"}]})
    );

    // 2. Repo AGENTS.md overrides effort while keeping user's provider for the same model.
    std::fs::write(repo.join("AGENTS.md"), "## Agent workflow\n\n- Ticket model: claude-opus-5-5, low effort\n")
        .unwrap();
    let res = result(&s.h.exec("async () => wf.launchTicket({ issue: 'acme/widgets#2' })").await);
    assert_eq!(res["instanceId"], "claude-2");
    assert_eq!(res["model"], "claude-opus-5-5");
    assert_eq!(res["effort"], "low");
    assert_eq!(res["modelSource"], "AGENTS.md");
    assert_eq!(
        s.h.fake.launched()[1]["modelSelection"],
        json!({"instanceId": "claude-2", "model": "claude-opus-5-5", "options": [{"id": "effort", "value": "low"}]})
    );

    // 3. Repo AGENTS.md switches to a Codex model (no restart needed).
    std::fs::write(repo.join("AGENTS.md"), "## Agent workflow\n\n- Ticket model: gpt-6.2-sol, xhigh effort\n").unwrap();
    let res = result(&s.h.exec("async () => wf.launchTicket({ issue: 'acme/widgets#3' })").await);
    assert_eq!(res["instanceId"], "codex");
    assert_eq!(res["model"], "gpt-6.2-sol");
    assert_eq!(res["effort"], "xhigh");
    assert_eq!(res["modelSource"], "AGENTS.md");
    assert_eq!(
        s.h.fake.launched()[2]["modelSelection"],
        json!({"instanceId": "codex", "model": "gpt-6.2-sol", "options": [{"id": "reasoningEffort", "value": "xhigh"}]})
    );

    // 4. Repo .t3-workflow.json overrides AGENTS.md.
    std::fs::write(
        repo.join(".t3-workflow.json"),
        json!({"models": {"ticket": {"provider": "claude-2", "model": "claude-opus-5-5", "effort": "high"}}})
            .to_string(),
    )
    .unwrap();
    let res = result(&s.h.exec("async () => wf.launchTicket({ issue: 'acme/widgets#4' })").await);
    assert_eq!(res["instanceId"], "claude-2");
    assert_eq!(res["model"], "claude-opus-5-5");
    assert_eq!(res["effort"], "high");
    assert_eq!(res["modelSource"], ".t3-workflow.json");

    // 5. Launch arg overrides .t3-workflow.json (both model+effort and effort-only).
    let res = result(
        &s.h.exec("async () => wf.launchTicket({ issue: 'acme/widgets#5', model: 'gpt-6.2-sol', effort: 'medium' })")
            .await,
    );
    assert_eq!(res["instanceId"], "codex");
    assert_eq!(res["model"], "gpt-6.2-sol");
    assert_eq!(res["effort"], "medium");
    assert_eq!(res["modelSource"], "argument");

    let res = result(&s.h.exec("async () => wf.launchTicket({ issue: 'acme/widgets#6', effort: 'low' })").await);
    assert_eq!(res["instanceId"], "claude-2");
    assert_eq!(res["model"], "claude-opus-5-5");
    assert_eq!(res["effort"], "low");
    assert_eq!(res["modelSource"], "argument");

    // 6. Catalog-only model beyond T3's short list resolves via orchestrator_capabilities search.
    s.h.fake.set_short_list(1);
    std::fs::write(
        repo.join(".t3-workflow.json"),
        json!({"models": {"ticket": {"provider": "claude-2", "model": "claude-catalog-only", "effort": "high"}}})
            .to_string(),
    )
    .unwrap();
    let res = result(&s.h.exec("async () => wf.launchTicket({ issue: 'acme/widgets#7' })").await);
    assert_eq!(res["instanceId"], "claude-2");
    assert_eq!(res["model"], "claude-catalog-only");
    assert_eq!(res["effort"], "high");
    assert_eq!(res["modelSource"], ".t3-workflow.json");
}

#[tokio::test]
async fn legacy_board_ticket_keys_sit_below_user_slot_and_above_default() {
    use t3_codemode::fake::provider;

    let cfg = Config::from_value(json!({
        "board": {"instanceId": "claude-2", "ticketModel": "claude-haiku-5-5", "ticketEffort": "low"}
    }))
    .unwrap();
    let h = H::build(Vec::new(), cfg);
    let repo = h.dir.path().join("widgets");
    std::fs::create_dir_all(&repo).unwrap();
    h.fake.set_projects(vec![project("p-widgets", &repo.to_string_lossy())]);
    h.remotes.set(&repo.to_string_lossy(), "git@github.com:acme/widgets.git");
    h.fake.set_providers(vec![
        provider("claudeAgent", "claudeAgent", &["claude-opus-5-5", "claude-haiku-5-5"]),
        provider("claude-2", "claudeAgent", &["claude-opus-5-5", "claude-haiku-5-5"]),
    ]);

    // With no slot set, legacy board.* keys apply.
    let res = result(&h.exec("async () => wf.launchTicket({ issue: 'acme/widgets#1' })").await);
    assert_eq!(res["instanceId"], "claude-2");
    assert_eq!(res["model"], "claude-haiku-5-5");
    assert_eq!(res["effort"], "low");
    assert_eq!(res["modelSource"], "board");

    // Once user config sets models.ticket, it outranks legacy board.* keys.
    write_workflow_config(
        &h,
        json!({"models": {"ticket": {"provider": "claudeAgent", "model": "claude-opus-5-5", "effort": "high"}}}),
    );
    let res = result(&h.exec("async () => wf.launchTicket({ issue: 'acme/widgets#2' })").await);
    assert_eq!(res["instanceId"], "claudeAgent");
    assert_eq!(res["model"], "claude-opus-5-5");
    assert_eq!(res["effort"], "high");
    assert_eq!(res["modelSource"], "user");
}

#[tokio::test]
async fn board_launch_picks_through_slot_pool_from_user_config_and_agents_md() {
    use t3_codemode::fake::provider;

    let cfg = Config::from_value(json!({
        "pools": {
            "opus": {
                "driver": "claudeAgent",
                "model": "claude-opus-5-5",
                "effort": "high",
                "members": [{"instanceId": "claude-a"}, {"instanceId": "claude-b"}]
            },
            "frontier": {
                "mixed": true,
                "strategy": "priority",
                "members": [{"instanceId": "codex-a", "model": "gpt-6.2-sol", "effort": "xhigh"}]
            }
        }
    }))
    .unwrap();
    let s = start(H::build(Vec::new(), cfg)).await;
    let repo = s.h.dir.path().join("widgets");
    std::fs::create_dir_all(&repo).unwrap();
    s.h.fake.set_projects(vec![project("p-widgets", &repo.to_string_lossy())]);
    s.h.remotes.set(&repo.to_string_lossy(), "git@github.com:acme/widgets.git");
    s.h.fake.set_providers(vec![
        provider("claude-a", "claudeAgent", &["claude-opus-5-5"]),
        provider("claude-b", "claudeAgent", &["claude-opus-5-5"]),
        codex_provider("codex-a", &["gpt-6.2-sol"]),
    ]);

    // User config pool picks via wf.pick and records the pick on the feed.
    write_workflow_config(&s.h, json!({"models": {"ticket": {"pool": "opus"}}}));
    let r = s.post("/api/launch", json!({"issues": ["acme/widgets#1", "acme/widgets#2"]}), Some(&s.csrf()), None).await;
    let body: Value = r.json().await.unwrap();
    let r1 = &body["results"][0];
    let r2 = &body["results"][1];
    assert_eq!(r1["ok"], true, "{r1}");
    assert_eq!(r1["pool"], "opus");
    assert_eq!(r1["model"], "claude-opus-5-5");
    assert_eq!(r1["effort"], "high");
    assert_eq!(r1["modelSource"], "user");
    assert!(r1["reason"].as_str().is_some_and(|s| s.contains("claude-opus-5-5")), "{r1}");
    assert_ne!(r1["instanceId"], r2["instanceId"], "weighted 1:1 alternates pool members");
    assert!(s.h.app.wf.feed(10).unwrap().iter().any(|f| f["instanceId"] == r1["instanceId"]));

    // Matching effort arg over a non-mixed pool still picks through the pool.
    let res = result(&s.h.exec("async () => wf.launchTicket({ issue: 'acme/widgets#4', effort: 'high' })").await);
    assert_eq!(res["pool"], "opus");
    assert_eq!(res["modelSource"], "argument");
    assert!(res["reason"].is_string(), "{res}");

    // Repo AGENTS.md pool overrides user config pool (and an invalid Worker model line does not block tickets).
    std::fs::write(repo.join("AGENTS.md"), "## Agent workflow\n\n- Ticket model: pool `frontier`\n- Worker model: \n")
        .unwrap();
    let res = result(&s.h.exec("async () => wf.launchTicket({ issue: 'acme/widgets#3' })").await);
    assert_eq!(res["instanceId"], "codex-a");
    assert_eq!(res["model"], "gpt-6.2-sol");
    assert_eq!(res["effort"], "xhigh");
    assert_eq!(res["pool"], "frontier");
    assert_eq!(res["modelSource"], "AGENTS.md");
    assert_eq!(
        s.h.fake.launched()[3]["modelSelection"],
        json!({"instanceId": "codex-a", "model": "gpt-6.2-sol", "options": [{"id": "reasoningEffort", "value": "xhigh"}]})
    );
}

#[tokio::test]
async fn board_launch_fails_with_error_card_on_invalid_slot_choices_never_falling_back() {
    use t3_codemode::fake::{disabled_provider, provider};

    let s = start(H::new()).await;
    let repo = s.h.dir.path().join("widgets");
    std::fs::create_dir_all(&repo).unwrap();
    s.h.fake.set_projects(vec![project("p-widgets", &repo.to_string_lossy())]);
    s.h.remotes.set(&repo.to_string_lossy(), "git@github.com:acme/widgets.git");
    s.h.fake.set_providers(vec![
        provider("claudeAgent", "claudeAgent", &["claude-opus-5-5"]),
        disabled_provider("claude-off", "claudeAgent"),
    ]);

    let expect_error_card = |res: &Value, needle: &str| {
        assert_eq!(res["ok"], false, "{res}");
        assert!(res["card"].is_string(), "{res}");
        assert!(res["error"].as_str().is_some_and(|e| e.contains(needle)), "expected {needle:?} in {res}");
    };

    // 1. Instance gone.
    write_workflow_config(
        &s.h,
        json!({"models": {"ticket": {"provider": "claude-gone", "model": "claude-opus-5-5", "effort": "high"}}}),
    );
    let body: Value =
        s.post("/api/launch", json!({"issues": ["acme/widgets#1"]}), Some(&s.csrf()), None).await.json().await.unwrap();
    expect_error_card(&body["results"][0], "claude-gone");

    // 2. Instance disabled.
    write_workflow_config(
        &s.h,
        json!({"models": {"ticket": {"provider": "claude-off", "model": "claude-opus-5-5", "effort": "high"}}}),
    );
    let body: Value =
        s.post("/api/launch", json!({"issues": ["acme/widgets#2"]}), Some(&s.csrf()), None).await.json().await.unwrap();
    expect_error_card(&body["results"][0], "claude-off");

    // 3. Model not listed (via AGENTS.md).
    std::fs::write(repo.join("AGENTS.md"), "## Agent workflow\n\n- Ticket model: claude-missing, high effort\n")
        .unwrap();
    let out = s.h.exec("async () => wf.launchTicket({ issue: 'acme/widgets#3' })").await;
    assert!(err_text(&out).contains("claude-missing"), "{}", out.text);

    // 4. Effort not offered.
    write_workflow_config(
        &s.h,
        json!({"models": {"ticket": {"provider": "claudeAgent", "model": "claude-opus-5-5", "effort": "high"}}}),
    );
    std::fs::write(repo.join("AGENTS.md"), "## Agent workflow\n\n- Ticket model: claude-opus-5-5, max effort\n")
        .unwrap();
    let out = s.h.exec("async () => wf.launchTicket({ issue: 'acme/widgets#4' })").await;
    assert!(err_text(&out).contains("effort max is not offered"), "{}", out.text);

    // 5. Unknown pool.
    std::fs::write(repo.join("AGENTS.md"), "## Agent workflow\n\n- Ticket model: pool `ghost`\n").unwrap();
    let body: Value =
        s.post("/api/launch", json!({"issues": ["acme/widgets#5"]}), Some(&s.csrf()), None).await.json().await.unwrap();
    expect_error_card(&body["results"][0], "no pool \"ghost\"");

    // 6. Broken user workflow config never falls back silently to default.
    std::fs::remove_file(repo.join("AGENTS.md")).unwrap();
    std::fs::write(s.h.dir.path().join("home/.config/t3-workflow/config.json"), "{broken json").unwrap();
    let out = s.h.exec("async () => wf.launchTicket({ issue: 'acme/widgets#6' })").await;
    assert!(err_text(&out).contains("workflow config problem"), "{}", out.text);

    // 7. No config and T3 offers no Claude -> fails with error card instead of sending claudeAgent.
    std::fs::remove_file(s.h.dir.path().join("home/.config/t3-workflow/config.json")).unwrap();
    s.h.fake.set_providers(vec![codex_provider("codex", &["gpt-6.2-sol"])]);
    let out = s.h.exec("async () => wf.launchTicket({ issue: 'acme/widgets#7' })").await;
    assert!(err_text(&out).contains("no enabled Claude provider"), "{}", out.text);

    // 8. T3 capabilities unreachable -> fails with error card.
    s.h.fake.set_caps_fail(true);
    let out = s.h.exec("async () => wf.launchTicket({ issue: 'acme/widgets#8' })").await;
    assert!(err_text(&out).contains("T3 capabilities unreachable"), "{}", out.text);

    // Nothing launched and no pending links left; every failure left an error card on the board.
    assert!(s.h.fake.launched().is_empty(), "no thread should launch on invalid choices");
    assert!(s.h.app.store.pending_all().unwrap().is_empty());
    let board_cards = cards(&s.h);
    assert_eq!(board_cards.len(), 8);
    assert!(board_cards.iter().all(|c| c["kind"] == "error"), "{board_cards:?}");

    // 9. Legacy board.instanceId disabled does not silently switch to another instance of the same driver.
    let cfg = Config::from_value(json!({"board": {"instanceId": "claudeAgent"}})).unwrap();
    let h2 = H::build(Vec::new(), cfg);
    h2.fake.set_projects(vec![project("p-widgets", &repo.to_string_lossy())]);
    h2.remotes.set(&repo.to_string_lossy(), "git@github.com:acme/widgets.git");
    h2.fake.set_providers(vec![
        disabled_provider("claudeAgent", "claudeAgent"),
        provider("claude-2", "claudeAgent", &["claude-opus-5-5"]),
    ]);
    let out = h2.exec("async () => wf.launchTicket({ issue: 'acme/widgets#9' })").await;
    assert!(err_text(&out).contains("claudeAgent"), "{}", out.text);
    assert!(h2.fake.launched().is_empty());
    assert_eq!(cards(&h2).len(), 1);
    assert_eq!(cards(&h2)[0]["kind"], "error");
}
