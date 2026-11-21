use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Duration;

use serde_json::{Value, json};

use crate::t3::{BoxFut, T3Client, T3Error};
use crate::usage::{Usage, UsageSource};

#[derive(Default)]
struct State {
    threads: BTreeMap<String, Value>,
    items: BTreeMap<String, Vec<Value>>,
    prs: BTreeMap<String, Vec<Value>>,
    prs_before_list: BTreeMap<String, Vec<Value>>,
    projects: Vec<Value>,
    providers: Vec<Value>,
    launched: Vec<Value>,
    organized: Vec<Value>,
    configured: Vec<Value>,
    forks: u64,
    sent: Vec<Value>,
    calls: Vec<String>,
    send_fails: bool,
    prs_fail: bool,
    projects_fail: bool,
    read_fails: bool,
    caps_fail: bool,
    pr_page: usize,
    short_list: usize,
    organize_fails: bool,
    delay: Duration,
}

#[derive(Default)]
pub struct FakeT3 {
    state: Mutex<State>,
    tools: Vec<Value>,
}

#[derive(Default)]
pub struct FakeRemotes(Mutex<BTreeMap<String, String>>);

impl FakeRemotes {
    pub fn set(&self, root: &str, url: &str) {
        self.0.lock().unwrap().insert(root.into(), url.into());
    }
}

impl crate::board::Remotes for FakeRemotes {
    fn origin<'a>(&'a self, root: &'a str) -> BoxFut<'a, Option<String>> {
        Box::pin(async move { self.0.lock().unwrap().get(root).cloned() })
    }
}

#[derive(Default)]
pub struct FakeDesktop {
    pub running: std::sync::atomic::AtomicBool,
    pub opened: Mutex<Vec<String>>,
}

impl crate::desktop::Desktop for FakeDesktop {
    fn open<'a>(&'a self, app_link: &'a str) -> BoxFut<'a, bool> {
        Box::pin(async move {
            let running = self.running.load(std::sync::atomic::Ordering::SeqCst);
            if running {
                self.opened.lock().unwrap().push(app_link.to_string());
            }
            running
        })
    }
}

pub fn project(id: &str, root: &str) -> Value {
    json!({"id": id, "title": id, "workspaceRoot": root, "defaultModelSelection": null, "scripts": [],
           "createdAt": "2026-11-01T00:00:00Z", "updatedAt": "2026-11-01T00:00:00Z", "deletedAt": null})
}

pub fn pull_request(repository: &str, number: u64, state: &str) -> Value {
    json!({
        "host": "github.com", "repository": repository, "number": number,
        "url": format!("https://github.com/{repository}/pull/{number}"), "source": "agent",
        "watching": state == "open", "state": state, "title": format!("PR {number}"),
        "headBranch": format!("fix/{number}"), "baseBranch": "main", "isDraft": false, "stack": null
    })
}

pub fn provider(id: &str, driver: &str, models: &[&str]) -> Value {
    let models: Vec<Value> = models
        .iter()
        .map(|m| json!({"id": m, "options": [{"id": "effort", "values": ["low", "medium", "high"], "default": "medium"}]}))
        .collect();
    json!({"providerInstanceId": id, "driverKind": driver, "displayName": id, "models": models,
           "canRunChildTask": true, "canRunCrossProviderChildTask": true, "constraints": []})
}

pub fn disabled_provider(id: &str, driver: &str) -> Value {
    json!({"providerInstanceId": id, "driverKind": driver, "displayName": id, "models": [],
           "canRunChildTask": false, "canRunCrossProviderChildTask": false,
           "constraints": ["Provider instance is disabled."]})
}

pub fn usage_limit_item(run: &str, text: &str) -> Value {
    json!({"position": 9, "itemId": format!("error:{run}"), "runId": run, "type": "error", "status": "failed",
           "title": "Usage limit reached", "text": text, "createdBy": null, "creationSource": null})
}

#[derive(Default)]
pub struct FakeUsage {
    usage: Mutex<BTreeMap<String, Usage>>,
    fails: Mutex<bool>,
}

impl FakeUsage {
    pub fn set(&self, instance: &str, left: f64, resets_at: Option<&str>) {
        let u = Usage { left, resets_at: resets_at.map(String::from) };
        self.usage.lock().unwrap().insert(instance.into(), u);
    }

    pub fn set_fails(&self, fails: bool) {
        *self.fails.lock().unwrap() = fails;
    }
}

impl UsageSource for FakeUsage {
    fn usage<'a>(&'a self, instances: &'a [String]) -> BoxFut<'a, anyhow::Result<BTreeMap<String, Usage>>> {
        Box::pin(async move {
            if *self.fails.lock().unwrap() {
                anyhow::bail!("usage source unreachable (fake)");
            }
            let all = self.usage.lock().unwrap();
            Ok(instances.iter().filter_map(|i| all.get(i).map(|u| (i.clone(), u.clone()))).collect())
        })
    }
}

pub fn thread(id: &str, status: &str, updated: &str) -> Value {
    json!({
        "threadId": id, "status": status, "updatedAt": updated, "link": format!("https://t3/{id}"),
        "pendingBackgroundTasks": [], "blockedOnThreadWait": false, "pendingRequestCount": 0,
        "latestRunId": format!("run:{id}:1"), "archived": false, "title": id
    })
}

impl FakeT3 {
    pub fn new() -> FakeT3 {
        FakeT3::default()
    }

    pub fn with_tools(tools: Vec<Value>) -> FakeT3 {
        FakeT3 { tools, ..FakeT3::default() }
    }

    fn st(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }

    pub fn put(&self, thread: Value) {
        let id = thread["threadId"].as_str().expect("threadId").to_string();
        self.st().threads.insert(id, thread);
    }

    pub fn update(&self, id: &str, patch: Value) {
        let mut st = self.st();
        let th = st.threads.get_mut(id).expect("known thread");
        for (k, v) in patch.as_object().expect("object patch") {
            th[k] = v.clone();
        }
    }

    pub fn remove(&self, id: &str) {
        self.st().threads.remove(id);
    }

    pub fn set_send_fails(&self, fails: bool) {
        self.st().send_fails = fails;
    }

    pub fn set_prs_fail(&self, fails: bool) {
        self.st().prs_fail = fails;
    }

    pub fn set_projects_fail(&self, fails: bool) {
        self.st().projects_fail = fails;
    }

    pub fn set_pr_page(&self, size: usize) {
        self.st().pr_page = size;
    }

    pub fn set_read_fails(&self, fails: bool) {
        self.st().read_fails = fails;
    }

    pub fn set_organize_fails(&self, fails: bool) {
        self.st().organize_fails = fails;
    }

    pub fn set_delay(&self, delay: Duration) {
        self.st().delay = delay;
    }

    pub fn set_items(&self, id: &str, items: Vec<Value>) {
        self.st().items.insert(id.into(), items);
    }

    pub fn set_prs(&self, id: &str, prs: Vec<Value>) {
        self.st().prs.insert(id.into(), prs);
    }

    pub fn set_prs_before_list(&self, id: &str, prs: Vec<Value>) {
        self.st().prs_before_list.insert(id.into(), prs);
    }

    pub fn set_projects(&self, projects: Vec<Value>) {
        self.st().projects = projects;
    }

    pub fn set_caps_fail(&self, fails: bool) {
        self.st().caps_fail = fails;
    }

    pub fn set_providers(&self, providers: Vec<Value>) {
        self.st().providers = providers;
    }

    pub fn set_short_list(&self, models: usize) {
        self.st().short_list = models;
    }

    pub fn launched(&self) -> Vec<Value> {
        self.st().launched.clone()
    }

    pub fn organized(&self) -> Vec<Value> {
        self.st().organized.clone()
    }

    pub fn sent(&self) -> Vec<Value> {
        self.st().sent.clone()
    }

    pub fn configured(&self) -> Vec<Value> {
        self.st().configured.clone()
    }

    pub fn thread(&self, id: &str) -> Option<Value> {
        self.st().threads.get(id).cloned()
    }

    pub fn calls(&self) -> Vec<String> {
        self.st().calls.clone()
    }

    fn handle(&self, tool: &str, args: &Value) -> Result<Value, T3Error> {
        let mut st = self.st();
        st.calls.push(tool.to_string());
        match tool {
            "t3_thread_read" => {
                if st.read_fails {
                    return Err(T3Error::Unavailable("T3 unreachable (fake)".into()));
                }
                let id = args["threadId"].as_str().unwrap_or_default();
                let items = st.items.get(id).cloned().unwrap_or_default();
                match st.threads.get(id) {
                    Some(th) => {
                        let mut th = th.clone();
                        if let Some(prs) = st.prs.get(id) {
                            let open: Vec<&Value> = prs.iter().filter(|p| p["state"] == "open").collect();
                            th["openPullRequestCount"] = json!(open.len());
                            th["pullRequestCount"] = json!(prs.len());
                            th["pullRequests"] = json!(open);
                        }
                        Ok(json!({"thread": th, "items": items, "earlierPosition": null}))
                    }
                    None => Err(T3Error::Tool(format!("t3_thread_read: thread {id} not found"))),
                }
            }
            "t3_thread_send" => {
                if st.send_fails {
                    return Err(T3Error::Unavailable("T3 unreachable (fake)".into()));
                }
                st.sent.push(args.clone());
                let n = st.sent.len();
                Ok(json!({"threadId": args["threadId"], "messageId": format!("msg:{n}"),
                          "runId": format!("run:{n}"), "status": "queued", "delivery": "queued"}))
            }
            "t3_thread_list" => {
                let wanted: Option<Vec<&str>> =
                    args["statuses"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect());
                let limit = args["limit"].as_u64().unwrap_or(u64::MAX) as usize;
                let start = args["cursor"].as_u64().unwrap_or(0) as usize;
                let matching: Vec<&Value> = st
                    .threads
                    .values()
                    .filter(|t| wanted.as_ref().is_none_or(|w| w.contains(&t["status"].as_str().unwrap_or(""))))
                    .collect();
                let threads: Vec<Value> = matching.iter().skip(start).take(limit).map(|t| (*t).clone()).collect();
                let next = (start.saturating_add(limit) < matching.len()).then(|| start + limit);
                Ok(json!({"threads": threads, "nextCursor": next, "total": matching.len()}))
            }
            "list_thread_pull_requests" => {
                if st.prs_fail {
                    return Err(T3Error::Unavailable("T3 unreachable (fake)".into()));
                }
                let id = args["threadId"].as_str().unwrap_or_default();
                if let Some(prs) = st.prs_before_list.remove(id) {
                    st.prs.insert(id.into(), prs);
                }
                let wanted: Option<Vec<&str>> =
                    args["states"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect());
                let prs: Vec<Value> = st
                    .prs
                    .get(id)
                    .into_iter()
                    .flatten()
                    .filter(|p| wanted.as_ref().is_none_or(|w| w.contains(&p["state"].as_str().unwrap_or(""))))
                    .cloned()
                    .collect();
                let size = if st.pr_page == 0 { prs.len().max(1) } else { st.pr_page };
                let start: usize = args["cursor"].as_str().and_then(|c| c.parse().ok()).unwrap_or(0);
                let end = (start + size).min(prs.len());
                let next = (end < prs.len()).then(|| end.to_string());
                Ok(json!({"pullRequests": prs[start.min(end)..end], "chains": [], "nextCursor": next}))
            }
            "t3_project_list" if st.projects_fail => Err(T3Error::Unavailable("T3 unreachable (fake)".into())),
            "t3_project_list" => Ok(json!({"projects": st.projects, "nextCursor": null})),
            "orchestrator_capabilities" if st.caps_fail => Err(T3Error::Unavailable("T3 unreachable (fake)".into())),
            "orchestrator_capabilities" => {
                let search = args["search"].as_str();
                let providers: Vec<Value> = st
                    .providers
                    .iter()
                    .map(|p| {
                        let mut p = p.clone();
                        if search.is_none() && st.short_list > 0 {
                            let models: Vec<Value> =
                                p["models"].as_array().into_iter().flatten().take(st.short_list).cloned().collect();
                            p["models"] = json!(models);
                        }
                        if let Some(q) = search {
                            let models: Vec<Value> = p["models"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .filter(|m| m["id"].as_str().is_some_and(|id| id.contains(q)))
                                .cloned()
                                .collect();
                            p["models"] = json!(models);
                        }
                        p
                    })
                    .collect();
                Ok(json!({"parentThreadId": null, "runtimeMode": "full-access", "providers": providers}))
            }
            "t3_thread_launch" => {
                st.launched.push(args.clone());
                let id = format!("launch:{}", st.launched.len());
                let mut th = thread(&id, "running", "2026-11-17T12:00:00Z");
                th["title"] = args["title"].clone();
                th["providerInstanceId"] = args["modelSelection"]["instanceId"].clone();
                th["model"] = args["modelSelection"]["model"].clone();
                st.threads.insert(id.clone(), th);
                Ok(json!({"threadId": id, "link": format!("https://t3/{id}"), "projectId": args["projectId"],
                          "modelSelection": args["modelSelection"], "runId": format!("run:{id}:1"), "status": "running"}))
            }
            "t3_thread_transfer" if args["action"] == "fork" => {
                let id = args["threadId"].as_str().unwrap_or_default();
                let Some(src) = st.threads.get(id).cloned() else {
                    return Err(T3Error::Tool(format!("t3_thread_transfer: thread {id} not found")));
                };
                st.forks += 1;
                let new = format!("fork:{}", st.forks);
                let mut th = thread(&new, "idle", "2026-11-17T12:00:00Z");
                for k in ["providerInstanceId", "model", "projectId", "branch", "worktreePath"] {
                    th[k] = src[k].clone();
                }
                th["title"] = args.get("title").cloned().unwrap_or_else(|| src["title"].clone());
                th["latestRunId"] = Value::Null;
                th["forkedFrom"] = json!(id);
                st.threads.insert(new.clone(), th);
                Ok(json!({"sequence": st.forks, "targetThreadId": new}))
            }
            "t3_thread_configure" => {
                let id = args["threadId"].as_str().unwrap_or_default().to_string();
                st.configured.push(args.clone());
                let Some(th) = st.threads.get_mut(&id) else {
                    return Err(T3Error::Tool(format!("t3_thread_configure: thread {id} not found")));
                };
                let m = &args["modelSelection"];
                if m.is_object() {
                    th["providerInstanceId"] = m["instanceId"].clone();
                    th["model"] = m["model"].clone();
                }
                Ok(json!({"sequence": 1}))
            }
            "t3_thread_organize" => {
                if st.organize_fails {
                    return Err(T3Error::Unavailable("T3 unreachable (fake)".into()));
                }
                st.organized.push(args.clone());
                let id = args["threadId"].as_str().unwrap_or_default().to_string();
                if args["action"] == "archive"
                    && let Some(th) = st.threads.get_mut(&id)
                {
                    th["archived"] = json!(true);
                }
                Ok(Value::Null)
            }
            "echo" => Ok(args.clone()),
            "fail" => {
                Err(T3Error::Tool(format!("fail: {}", args["message"].as_str().unwrap_or("the fake tool failed"))))
            }
            other => Err(T3Error::Tool(format!("fake T3 has no tool {other}"))),
        }
    }
}

impl T3Client for FakeT3 {
    fn call<'a>(&'a self, tool: &'a str, args: Value) -> BoxFut<'a, Result<Value, T3Error>> {
        Box::pin(async move {
            let delay = self.st().delay;
            let delay = match tool {
                "sleep" => Duration::from_millis(args["ms"].as_u64().unwrap_or(0)),
                _ => delay,
            };
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            if tool == "sleep" {
                self.st().calls.push("sleep".into());
                return Ok(args);
            }
            self.handle(tool, &args)
        })
    }

    fn list_tools(&self) -> BoxFut<'_, Result<Vec<Value>, T3Error>> {
        Box::pin(async move { Ok(self.tools.clone()) })
    }
}

#[derive(Default)]
struct GhState {
    repos: BTreeMap<String, Value>,
    prs: BTreeMap<(String, u64), Value>,
    calls: Vec<Value>,
    errors: BTreeMap<String, String>,
    read_errors: BTreeMap<(String, u64), String>,
    field_errors: BTreeMap<(String, u64), (String, String)>,
    down: bool,
    queue: u64,
    merges: u64,
    unknown_reads: usize,
}

#[derive(Default)]
pub struct FakeGitHub(Mutex<GhState>);

pub fn github_pr(head: &str, merge_state: &str, queue: bool) -> Value {
    json!({
        "state": "OPEN", "isDraft": false, "headRefOid": head, "mergeStateStatus": merge_state,
        "isMergeQueueEnabled": queue, "isInMergeQueue": false, "mergeQueueEntry": null, "mergeCommit": null,
        "mergedAt": null, "closedAt": null, "timelineItems": {"nodes": []}, "commits": head_commit(head, json!([]))
    })
}

pub fn head_commit(head: &str, checks: Value) -> Value {
    json!({"nodes": [{"commit": {"oid": head, "statusCheckRollup": {
        "contexts": {"pageInfo": {"hasNextPage": false}, "nodes": checks}
    }}}]})
}

pub fn check_run(name: &str, conclusion: &str) -> Value {
    json!({"__typename": "CheckRun", "name": name, "status": "COMPLETED", "conclusion": conclusion})
}

fn gh_error(message: &str) -> Value {
    json!({"data": null, "errors": [{"type": "UNPROCESSABLE", "message": message}]})
}

impl FakeGitHub {
    fn st(&self) -> std::sync::MutexGuard<'_, GhState> {
        self.0.lock().unwrap()
    }

    pub fn set_pr(&self, repo: &str, number: u64, pr: Value) {
        let mut st = self.st();
        let mut pr = pr;
        pr["id"] = json!(format!("PR_{repo}_{number}"));
        st.repos.entry(repo.into()).or_insert_with(|| json!({"viewerDefaultMergeMethod": "MERGE"}));
        st.prs.insert((repo.into(), number), pr);
    }

    pub fn update_pr(&self, repo: &str, number: u64, patch: Value) {
        let mut st = self.st();
        let pr = st.prs.get_mut(&(repo.into(), number)).expect("known pull request");
        for (k, v) in patch.as_object().expect("object patch") {
            pr[k] = v.clone();
        }
    }

    pub fn set_checks(&self, repo: &str, number: u64, checks: Value) {
        let head = self.pr(repo, number)["headRefOid"].as_str().unwrap_or_default().to_string();
        self.update_pr(repo, number, json!({"commits": head_commit(&head, checks)}));
    }

    pub fn pr(&self, repo: &str, number: u64) -> Value {
        self.st().prs[&(repo.into(), number)].clone()
    }

    pub fn remove_pr(&self, repo: &str, number: u64) {
        self.st().prs.remove(&(repo.into(), number));
    }

    pub fn drop_from_queue(&self, repo: &str, number: u64, reason: &str, at: &str) {
        self.update_pr(
            repo,
            number,
            json!({"isInMergeQueue": false, "mergeQueueEntry": null,
                   "timelineItems": {"nodes": [{"reason": reason, "createdAt": at}]}}),
        );
    }

    pub fn set_default_method(&self, repo: &str, method: &str) {
        self.st().repos.insert(repo.into(), json!({"viewerDefaultMergeMethod": method}));
    }

    pub fn set_queue_length(&self, n: u64) {
        self.st().queue = n;
    }

    pub fn fail_operation(&self, operation: &str, message: &str) {
        self.st().errors.insert(operation.into(), message.into());
    }

    pub fn fail_read(&self, repo: &str, number: u64, message: &str) {
        self.st().read_errors.insert((repo.into(), number), message.into());
    }

    pub fn fail_field(&self, repo: &str, number: u64, field: &str, message: &str) {
        self.st().field_errors.insert((repo.into(), number), (field.into(), message.into()));
    }

    pub fn clear_read_errors(&self) {
        let mut st = self.st();
        st.read_errors.clear();
        st.field_errors.clear();
    }

    pub fn set_unknown_reads(&self, n: usize) {
        self.st().unknown_reads = n;
    }

    pub fn set_down(&self, down: bool) {
        self.st().down = down;
    }

    pub fn calls(&self) -> Vec<Value> {
        self.st().calls.clone()
    }

    pub fn operations(&self) -> Vec<String> {
        self.calls().iter().map(|c| c["operation"].as_str().unwrap_or_default().to_string()).collect()
    }
}

fn operation(query: &str) -> &'static str {
    if query.starts_with("query Reconcile(") {
        return "reconcile";
    }
    for op in ["enqueuePullRequest", "mergePullRequest", "enablePullRequestAutoMerge"] {
        if query.contains(op) {
            return op;
        }
    }
    "read"
}

impl crate::github::GitHub for FakeGitHub {
    fn graphql<'a>(&'a self, query: &'a str, variables: Value) -> BoxFut<'a, anyhow::Result<Value>> {
        Box::pin(async move {
            let mut st = self.st();
            let op = operation(query);
            st.calls.push(json!({"operation": op, "variables": variables}));
            if st.down {
                anyhow::bail!("gh api graphql failed: gh: not logged in");
            }
            if let Some(message) = st.errors.get(op) {
                return Ok(gh_error(message));
            }
            if op == "reconcile" {
                let mut data = serde_json::Map::new();
                let mut errs = Vec::new();
                for i in 0.. {
                    let Some(owner) = variables[format!("o{i}")].as_str() else { break };
                    let repo = format!("{owner}/{}", variables[format!("r{i}")].as_str().unwrap()).to_ascii_lowercase();
                    let number = variables[format!("n{i}")].as_u64().unwrap();
                    let alias = format!("p{i}");
                    if !st.repos.contains_key(&repo) {
                        errs.push(
                            json!({"type": "NOT_FOUND", "path": [alias], "message": format!("no repository {repo}")}),
                        );
                        data.insert(alias, Value::Null);
                        continue;
                    }
                    let mut pr = st.prs.get(&(repo.clone(), number)).cloned().unwrap_or(Value::Null);
                    if let Some(message) = st.read_errors.get(&(repo.clone(), number)) {
                        errs.push(json!({"path": [alias, "pullRequest"], "message": message}));
                        pr = Value::Null;
                    } else if let Some((field, message)) = st.field_errors.get(&(repo.clone(), number)) {
                        errs.push(json!({"path": [alias, "pullRequest", field], "message": message}));
                        pr[field.as_str()] = Value::Null;
                    } else if pr.is_null() {
                        errs.push(
                            json!({"type": "NOT_FOUND", "path": [alias, "pullRequest"], "message": "no pull request"}),
                        );
                    }
                    let mut repository = st.repos[&repo].clone();
                    repository["pullRequest"] = pr;
                    data.insert(alias, repository);
                }
                let mut out = json!({"data": data});
                if !errs.is_empty() {
                    out["errors"] = json!(errs);
                }
                return Ok(out);
            }
            if op == "read" {
                let repo = format!("{}/{}", variables["owner"].as_str().unwrap(), variables["name"].as_str().unwrap())
                    .to_ascii_lowercase();
                let Some(meta) = st.repos.get(&repo).cloned() else {
                    return Ok(gh_error(&format!("Could not resolve to a Repository with the name '{repo}'.")));
                };
                let number = variables["number"].as_u64().unwrap();
                let mut pr = st.prs.get(&(repo, number)).cloned().unwrap_or(Value::Null);
                if st.unknown_reads > 0 && !pr.is_null() {
                    st.unknown_reads -= 1;
                    pr["mergeStateStatus"] = json!("UNKNOWN");
                }
                let mut repository = meta;
                repository["pullRequest"] = pr;
                return Ok(json!({"data": {"repository": repository}}));
            }
            let id = variables["id"].as_str().unwrap().to_string();
            let head = variables["head"].as_str().unwrap().to_string();
            let queue = st.queue;
            st.merges += 1;
            let merges = st.merges;
            let Some(pr) = st.prs.values_mut().find(|p| p["id"] == id.as_str()) else {
                return Ok(gh_error("Could not resolve to a node"));
            };
            if pr["headRefOid"] != head.as_str() {
                return Ok(gh_error("Head branch was modified. Review and try the merge again."));
            }
            match op {
                "enqueuePullRequest" => {
                    let position = queue + 1;
                    pr["isInMergeQueue"] = json!(true);
                    pr["mergeQueueEntry"] = json!({"position": position});
                    Ok(json!({"data": {"enqueuePullRequest": {"mergeQueueEntry": {"position": position}}}}))
                }
                "mergePullRequest" => {
                    let oid = format!("{merges:040x}");
                    pr["state"] = json!("MERGED");
                    pr["mergeCommit"] = json!({"oid": oid});
                    Ok(json!({"data": {"mergePullRequest": {"pullRequest": {"mergeCommit": {"oid": oid}}}}}))
                }
                _ => Ok(gh_error("the fake does not support this mutation")),
            }
        })
    }
}
