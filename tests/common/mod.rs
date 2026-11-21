#![allow(dead_code)]

use std::sync::Arc;

use serde_json::Value;
use t3_codemode::app::{App, ExecuteRequest, ToolOutput};
use t3_codemode::clock::FakeClock;
use t3_codemode::config::Config;
use t3_codemode::drift::Home;
use t3_codemode::fake::{FakeDesktop, FakeGitHub, FakeRemotes, FakeT3, provider, thread};
use t3_codemode::store::Store;

pub const BOSS: &str = "mcp:boss";
pub const TICKET: &str = "mcp:ticket";
pub const WORKER: &str = "mcp:worker";
pub const T0: &str = "2026-11-17T12:00:00Z";

pub fn at(minutes: i64) -> String {
    let total = 12 * 60 + minutes;
    format!("2026-11-17T{:02}:{:02}:00Z", total / 60, total % 60)
}

pub fn fixture_tools() -> Vec<Value> {
    let raw = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/t3-tools.json")).unwrap();
    serde_json::from_str(&raw).unwrap()
}

pub struct H {
    pub dir: tempfile::TempDir,
    pub fake: Arc<FakeT3>,
    pub clock: Arc<FakeClock>,
    pub app: Arc<App>,
    pub remotes: Arc<FakeRemotes>,
    pub github: Arc<FakeGitHub>,
    pub desktop: Arc<FakeDesktop>,
    config: Config,
}

impl H {
    pub fn new() -> H {
        H::build(Vec::new(), Config::default())
    }

    pub fn with_tools() -> H {
        H::build(fixture_tools(), Config::default())
    }

    pub fn build(tools: Vec<Value>, config: Config) -> H {
        let dir = tempfile::tempdir().unwrap();
        let fake = Arc::new(FakeT3::with_tools(tools));
        for id in [BOSS, TICKET, WORKER] {
            fake.put(thread(id, "idle", T0));
        }
        fake.set_providers(vec![provider("claudeAgent", "claudeAgent", &["claude-opus-5-5", "claude-haiku-5-5"])]);
        let clock = Arc::new(FakeClock::at(T0));
        let store = Arc::new(Store::open(dir.path()).unwrap());
        let remotes = Arc::new(FakeRemotes::default());
        let github = Arc::new(FakeGitHub::default());
        let desktop = Arc::new(FakeDesktop::default());
        let app = App::with_host(
            config.clone(),
            store,
            fake.clone(),
            clock.clone(),
            remotes.clone(),
            github.clone(),
            desktop.clone(),
        );
        app.wf.set_skills_home(Home::under(&dir.path().join("home")));
        app.wf.set_slot_folders(t3_codemode::slots::Folders::under(&dir.path().join("home"), None));
        H { dir, fake, clock, app, remotes, github, desktop, config }
    }

    pub async fn load_catalog(&self) {
        self.app.refresh_catalog().await.unwrap();
    }

    pub fn reopen(&mut self) {
        let store = Arc::new(Store::open(self.dir.path()).unwrap());
        self.app = App::with_host(
            self.config.clone(),
            store,
            self.fake.clone(),
            self.clock.clone(),
            self.remotes.clone(),
            self.github.clone(),
            self.desktop.clone(),
        );
        self.app.wf.set_skills_home(Home::under(&self.dir.path().join("home")));
        self.app.wf.set_slot_folders(t3_codemode::slots::Folders::under(&self.dir.path().join("home"), None));
    }

    pub fn peer(&self) -> Arc<App> {
        let app = App::with_host(
            self.config.clone(),
            Arc::new(Store::open(self.dir.path()).unwrap()),
            self.fake.clone(),
            self.clock.clone(),
            self.remotes.clone(),
            self.github.clone(),
            self.desktop.clone(),
        );
        app.wf.set_skills_home(Home::under(&self.dir.path().join("home")));
        app
    }

    pub async fn exec(&self, code: &str) -> ToolOutput {
        self.run(None, code, None).await
    }

    pub async fn exec_as(&self, caller: &str, code: &str) -> ToolOutput {
        self.run(Some(caller), code, None).await
    }

    pub async fn run(&self, caller: Option<&str>, code: &str, timeout_ms: Option<u64>) -> ToolOutput {
        self.app
            .execute(ExecuteRequest {
                code: code.into(),
                session: None,
                timeout_ms,
                caller: caller.map(String::from),
                reported: Default::default(),
            })
            .await
    }

    pub async fn wf(&self, me: &str, method: &str, args: Value) -> Value {
        match self.app.wf.dispatch(method, Some(me), args).await {
            Ok(v) => v,
            Err(e) => panic!("wf.{method} as {me} failed: {e:#}"),
        }
    }

    pub async fn wf_err(&self, me: &str, method: &str, args: Value) -> String {
        match self.app.wf.dispatch(method, Some(me), args).await {
            Ok(v) => panic!("wf.{method} as {me} should fail, got {v}"),
            Err(e) => format!("{e:#}"),
        }
    }

    pub async fn tick(&self, minutes: i64) -> Vec<String> {
        self.clock.set(&at(minutes));
        self.app.wf.tick().await.unwrap()
    }

    pub fn sent(&self) -> Vec<Value> {
        self.fake.sent()
    }

    pub fn last_message(&self) -> String {
        self.sent().last().expect("a send")["message"].as_str().unwrap().to_string()
    }
}

pub fn ok_json(out: &ToolOutput) -> Value {
    assert!(!out.is_error, "expected success, got: {}", out.text);
    serde_json::from_str(&out.text).unwrap_or_else(|e| panic!("not JSON ({e}): {}", out.text))
}

pub fn result(out: &ToolOutput) -> Value {
    ok_json(out)["result"].clone()
}

pub fn err_text(out: &ToolOutput) -> &str {
    assert!(out.is_error, "expected an error, got: {}", out.text);
    &out.text
}
