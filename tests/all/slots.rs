use std::path::{Path, PathBuf};

use crate::common::{self, H, ok_json};
use serde_json::{Value, json};
use t3_codemode::app::{ExecuteRequest, ToolOutput};
use t3_codemode::fake::{project, provider, thread};
use t3_codemode::slots::Folders;
use t3_codemode::wf_slots::Reported;

const ME: &str = "mcp:slot-user";

struct S {
    h: H,
    home: PathBuf,
    repo: PathBuf,
}

fn own(dir: &Path) -> Reported {
    Reported { own: Some(dir.to_string_lossy().into_owned()), ..Reported::default() }
}

fn skill(base: &Path, name: &str, inline: bool) -> PathBuf {
    let d = base.join(name);
    std::fs::create_dir_all(&d).unwrap();
    let extra = if inline { "disable-model-invocation: true\n" } else { "" };
    let p = d.join("SKILL.md");
    std::fs::write(&p, format!("---\nname: {name}\ndescription: test\n{extra}---\n\nBody.\n")).unwrap();
    p
}

impl S {
    fn new(instance: &str) -> S {
        let h = H::new();
        let home = h.dir.path().join("home");
        let repo = h.dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        h.app.wf.set_slot_folders(Folders::under(&home, None));
        h.fake.set_providers(vec![
            provider("claudeAgent", "claudeAgent", &["claude-opus-5-5"]),
            provider("claude-2", "claudeAgent", &["claude-opus-5-5"]),
            provider("codex", "codex", &["gpt-6.2-sol"]),
        ]);
        h.fake.set_projects(vec![project("proj:widgets", &repo.to_string_lossy())]);
        let s = S { h, home, repo };
        s.thread(ME, instance);
        s
    }

    fn thread(&self, id: &str, instance: &str) {
        let mut th = thread(id, "running", common::T0);
        th["providerInstanceId"] = json!(instance);
        th["projectId"] = json!("proj:widgets");
        self.h.fake.put(th);
    }

    fn config(&self, cfg: Value) {
        let p = self.home.join(".config/t3-workflow/config.json");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, cfg.to_string()).unwrap();
    }

    async fn run(&self, caller: Option<&str>, code: &str, reported: Reported) -> ToolOutput {
        self.h
            .app
            .execute(ExecuteRequest {
                code: code.into(),
                session: None,
                timeout_ms: None,
                caller: caller.map(String::from),
                reported,
            })
            .await
    }

    async fn call(&self, code: &str, reported: Reported) -> Value {
        ok_json(&self.run(Some(ME), code, reported).await)["result"].clone()
    }
}

#[tokio::test]
async fn a_reported_config_folder_resolves_that_agents_skills_and_is_recorded() {
    let s = S::new("claude-2");
    let mine = s.home.join(".claude-2");
    let path = skill(&mine.join("skills"), "unslop", true);
    skill(&s.home.join(".claude").join("skills"), "other-only", false);
    s.config(json!({"passes": ["unslop"]}));
    let v = s.call("async () => wf.slot(\"passes\")", own(&mine)).await;
    let first = &v["skills"][0];
    assert_eq!((first["status"].as_str(), first["path"].as_str()), (Some("ok"), path.to_str()));
    assert_eq!(first["how"], format!("inline: read {} and apply it", path.display()));
    assert_eq!(v["warnings"], json!([]));
    let rec = s.h.app.store.instance_folder("claude-2").unwrap().unwrap();
    assert_eq!((rec.driver.as_str(), PathBuf::from(&rec.folder)), ("claudeAgent", mine.clone()));

    let other = s.call("async () => wf.slots({ instance: \"claudeAgent\" })", own(&mine)).await;
    assert_eq!(other["context"]["folderFrom"], "default", "a report belongs to the caller's own instance");
    assert!(s.h.app.store.instance_folder("claudeAgent").unwrap().is_none());

    let later = s.call("async () => wf.slots()", Reported::default()).await;
    assert_eq!(later["passes"]["skills"][0]["path"].as_str(), path.to_str(), "falls back to the recorded folder");
    assert_eq!(later["context"]["folderFrom"], "recorded");
    let warning = later["warnings"][0].as_str().unwrap();
    assert!(warning.contains("recorded for this instance") && warning.contains("t3cm skills update"), "{warning}");
}

#[tokio::test]
async fn an_empty_header_means_the_default_folder_and_no_header_warns() {
    let s = S::new("claudeAgent");
    let path = skill(&s.home.join(".claude").join("skills"), "pr", false);
    s.config(json!({"passes": ["pr"]}));
    let reported = Reported { own: Some(String::new()), ..Reported::default() };
    let v = s.call("async () => wf.slots()", reported).await;
    assert_eq!(v["passes"]["skills"][0]["path"].as_str(), path.to_str());
    assert_eq!((v["context"]["folderFrom"].as_str(), v["warnings"].clone()), (Some("reported"), json!([])));

    s.thread("mcp:fresh", "claude-3");
    let out = s.run(Some("mcp:fresh"), "async () => wf.slots()", Reported::default()).await;
    let v = ok_json(&out)["result"].clone();
    assert_eq!(v["context"]["folderFrom"], "default");
    assert!(v["warnings"][0].as_str().unwrap().contains("the default folder"), "{}", v["warnings"]);
}

#[tokio::test]
async fn an_antigravity_thread_reads_every_skill_on_disk_inline() {
    let s = S::new("antigravity");
    s.h.fake.set_providers(vec![
        provider("claudeAgent", "claudeAgent", &["claude-opus-5-5"]),
        provider("antigravity", "antigravity", &["gemini-4-neon"]),
    ]);
    let review = skill(&s.home.join(".gemini/config/skills"), "sol-loop", false);
    let pass = skill(&s.home.join(".agents/skills"), "pr", false);
    s.config(json!({"review": "sol-loop", "passes": ["pr"]}));
    let v = s.call("async () => wf.slots()", Reported::default()).await;
    assert_eq!(v["context"]["driver"], "antigravity");
    assert_eq!(
        (v["review"]["skill"]["invoke"].as_str(), v["review"]["skill"]["path"].as_str()),
        (Some("inline"), review.to_str())
    );
    assert_eq!(v["passes"]["skills"][0]["invoke"], "inline");
    let one = s.call(r#"async () => wf.slot("passes")"#, Reported::default()).await;
    assert_eq!(one["skills"][0]["how"], format!("inline: read {} and apply it", pass.display()));

    s.thread("mcp:claude-user", "claudeAgent");
    let out = s.run(Some("mcp:claude-user"), "async () => wf.slots()", own(&s.home.join(".claude"))).await;
    let v = &ok_json(&out)["result"];
    assert_eq!(v["passes"]["skills"][0]["invoke"], "skill");
    assert_eq!(v["review"]["skill"]["invoke"], "inline", "the Skill tool cannot load from Antigravity's folder");
}

#[tokio::test]
async fn a_reported_gemini_home_resolves_that_profiles_skills() {
    let s = S::new("antigravity");
    s.h.fake.set_providers(vec![provider("antigravity", "antigravity", &["gemini-4-neon"])]);
    let profile = s.home.join("profile");
    let mine = skill(&profile.join("config/skills"), "profile-only", false);
    s.config(json!({"review": "profile-only"}));
    let v = s.call("async () => wf.slots()", Reported::default()).await;
    assert_eq!(v["review"]["skill"]["status"], "missing");
    let reported = Reported { gemini: Some(profile.to_string_lossy().into_owned()), ..Reported::default() };
    let v = s.call("async () => wf.slots()", reported).await;
    assert_eq!(v["review"]["skill"]["path"].as_str(), mine.to_str());
    assert_eq!(v["context"]["geminiHome"], profile.to_string_lossy().as_ref());
    let empty = Reported { gemini: Some(String::new()), ..Reported::default() };
    let v = s.call("async () => wf.slots()", empty).await;
    assert_eq!(v["context"]["geminiHome"], s.home.join(".gemini").to_string_lossy().as_ref());
}

#[tokio::test]
async fn t3cm_exec_reports_both_folders_and_a_codex_instance_uses_its_own() {
    let s = S::new("codex");
    let codex = s.home.join("custom-codex");
    let claude = s.home.join("custom-claude");
    let c = skill(&codex.join("skills"), "from-codex", false);
    let k = skill(&claude.join("skills"), "from-claude", false);
    s.config(json!({"passes": ["from-codex", "from-claude"]}));
    let reported = Reported {
        own: None,
        claude: Some(claude.to_string_lossy().into_owned()),
        codex: Some(codex.to_string_lossy().into_owned()),
        gemini: None,
    };
    let v = s.call("async () => wf.slots()", reported).await;
    let paths: Vec<&str> =
        v["passes"]["skills"].as_array().unwrap().iter().map(|e| e["path"].as_str().unwrap()).collect();
    assert_eq!(paths, vec![c.to_str().unwrap(), k.to_str().unwrap()]);
    assert_eq!(v["context"]["driver"], "codex");
    let rec = s.h.app.store.instance_folder("codex").unwrap().unwrap();
    assert_eq!((rec.driver.as_str(), PathBuf::from(rec.folder)), ("codex", codex));

    let v = s.call("async () => wf.slots()", own(&s.home.join("header-codex"))).await;
    assert_eq!(v["context"]["codexHome"], s.home.join("header-codex").to_string_lossy().as_ref());
}

fn install_plugin(claude: &Path, plugin: &str, skill_name: &str) {
    let root = claude.join("plugins/cache/acme").join(plugin).join("1.0.0");
    skill(&root.join("skills"), skill_name, false);
    let path = claude.join("plugins/installed_plugins.json");
    let mut data: Value = std::fs::read(&path)
        .ok()
        .and_then(|r| serde_json::from_slice(&r).ok())
        .unwrap_or(json!({"version": 2, "plugins": {}}));
    data["plugins"][format!("{plugin}@acme")] = json!([{"scope": "user", "installPath": root.to_string_lossy()}]);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, data.to_string()).unwrap();
}

#[tokio::test]
async fn disabled_plugins_do_not_resolve() {
    let s = S::new("claudeAgent");
    let claude = s.home.join(".claude");
    install_plugin(&claude, "on", "voice");
    install_plugin(&claude, "off", "review-it");
    install_plugin(&claude, "unlisted", "extra");
    std::fs::write(
        claude.join("settings.json"),
        json!({"enabledPlugins": {"on@acme": true, "off@acme": false}}).to_string(),
    )
    .unwrap();
    s.config(json!({"passes": ["on:voice", "off:review-it", "unlisted:extra"]}));
    let v = s.call("async () => wf.slot(\"passes\")", own(&claude)).await;
    let status: Vec<&str> = v["skills"].as_array().unwrap().iter().map(|e| e["status"].as_str().unwrap()).collect();
    assert_eq!(status, vec!["ok", "missing", "missing"]);
    assert!(v["line"].as_str().unwrap().contains("SKIP off:review-it: no longer resolves"), "{}", v["line"]);

    std::fs::create_dir_all(s.repo.join(".claude")).unwrap();
    std::fs::write(
        s.repo.join(".claude/settings.local.json"),
        json!({"enabledPlugins": {"off@acme": true}}).to_string(),
    )
    .unwrap();
    let v = s.call("async () => wf.slot(\"passes\")", own(&claude)).await;
    assert_eq!(v["skills"][1]["status"], "ok", "the project's local settings turn it back on");
}

#[tokio::test]
async fn the_store_is_reused_until_an_input_changes() {
    let s = S::new("claudeAgent");
    let claude = s.home.join(".claude");
    s.config(json!({"passes": ["pr"]}));
    let review = |v: &Value| v["passes"]["skills"][0]["status"].as_str().unwrap().to_string();
    let v = s.call("async () => wf.slots()", own(&claude)).await;
    assert_eq!(review(&v), "missing");

    let key = "project:proj:widgets|claudeAgent";
    let (fp, mut data) = s.h.app.store.slot_resolution(key).unwrap().expect("stored");
    data["passes"]["skills"][0]["status"] = json!("from-the-store");
    s.h.app.store.slot_resolution_set(key, &fp, &data, common::T0).unwrap();
    let v = s.call("async () => wf.slots()", own(&claude)).await;
    assert_eq!(review(&v), "from-the-store", "an unchanged fingerprint reuses the stored resolution");

    skill(&claude.join("skills"), "pr", false);
    let v = s.call("async () => wf.slots()", own(&claude)).await;
    assert_eq!(review(&v), "ok", "a skill installed mid-ticket shows up on the next lookup");

    let md = claude.join("skills/pr/SKILL.md");
    let padded = |head: &str| format!("{head}{}", "x".repeat(120 - head.len()));
    std::fs::write(&md, padded("---\nname: pr\ndescription: test\n---\n\n")).unwrap();
    let v = s.call("async () => wf.slots()", own(&claude)).await;
    assert_eq!(v["passes"]["skills"][0]["invoke"], "skill");
    let mtime = std::fs::metadata(&md).unwrap().modified().unwrap();
    std::fs::write(&md, padded("---\nname: pr\ndisable-model-invocation: true\n---\n\n")).unwrap();
    std::fs::File::options().write(true).open(&md).unwrap().set_modified(mtime).unwrap();
    let v = s.call("async () => wf.slots()", own(&claude)).await;
    assert_eq!(v["passes"]["skills"][0]["invoke"], "inline", "a same-size SKILL.md edit that keeps the mtime rebuilds");

    s.config(json!({"passes": ["pr", "unslop"]}));
    let v = s.call("async () => wf.slots()", own(&claude)).await;
    assert_eq!(v["passes"]["skills"].as_array().unwrap().len(), 2, "a config change rebuilds");

    let cfg = s.home.join(".config/t3-workflow/config.json");
    let mtime = std::fs::metadata(&cfg).unwrap().modified().unwrap();
    s.config(json!({"passes": ["pr", "unslap"]}));
    std::fs::File::options().write(true).open(&cfg).unwrap().set_modified(mtime).unwrap();
    let v = s.call("async () => wf.slots()", own(&claude)).await;
    assert_eq!(v["passes"]["skills"][1]["ref"], "unslap", "a same-size rewrite that keeps the mtime rebuilds");

    std::fs::write(s.repo.join("AGENTS.md"), "## Agent workflow\n\n- Passes: none\n").unwrap();
    let v = s.call("async () => wf.slots()", own(&claude)).await;
    assert_eq!((v["passes"]["source"].as_str(), v["passes"]["skills"].clone()), (Some("AGENTS.md"), json!([])));

    install_plugin(&claude, "cbrip", "sol-loop");
    std::fs::write(s.repo.join("AGENTS.md"), "## Agent workflow\n\n- Review: cbrip:sol-loop\n").unwrap();
    let v = s.call("async () => wf.slots()", own(&claude)).await;
    assert_eq!(v["review"]["skill"]["status"], "ok");
    std::fs::write(claude.join("settings.json"), json!({"enabledPlugins": {"cbrip@acme": false}}).to_string()).unwrap();
    let v = s.call("async () => wf.slots()", own(&claude)).await;
    assert_eq!(v["review"]["skill"]["status"], "missing", "an enabledPlugins change rebuilds");
}

#[tokio::test]
async fn each_slot_comes_back_with_how_to_run_it() {
    let s = S::new("claude-2");
    let mine = s.home.join(".claude-2");
    skill(&mine.join("skills"), "sol-loop", false);
    skill(&mine.join("skills"), "deploy", false);
    s.config(json!({"review": "sol-loop", "delivery": {"mode": "tag-deploy", "skill": "deploy"},
                    "models": {"ticket": {"model": "claude-opus-5-5", "effort": "high"}, "worker": {"pool": "opus"}}}));
    let mut codex = provider("codex", "codex", &["gpt-6.2-sol"]);
    codex["models"][0]["options"] = json!([{"id": "reasoningEffort", "values": ["low", "high"]}]);
    s.h.fake.set_providers(vec![
        provider("claudeAgent", "claudeAgent", &["claude-opus-5-5"]),
        provider("claude-2", "claudeAgent", &["claude-opus-5-5"]),
        codex,
    ]);
    let r = s.call("async () => wf.slot(\"review\")", own(&mine)).await;
    assert_eq!((r["source"].as_str(), r["skill"]["how"].as_str()), (Some("user"), Some("Skill tool")));
    assert_eq!(r["line"], "review: sol-loop (Skill tool) [user]");
    let d = s.call("async () => wf.slot(\"delivery\")", own(&mine)).await;
    assert_eq!((d["mode"].as_str(), d["skill"]["name"].as_str()), (Some("tag-deploy"), Some("deploy")));
    let m = s.call("async () => wf.slot(\"models\")", own(&mine)).await;
    assert_eq!(
        m["ticket"]["modelSelection"],
        json!({"instanceId": "claude-2", "model": "claude-opus-5-5", "options": [{"id": "effort", "value": "high"}]})
    );
    assert_eq!(m["worker"]["modelSelection"]["pool"], "opus");
    s.config(json!({"models": {"ticket": {"provider": "codex", "model": "gpt-6.2-sol", "effort": "high"},
                               "worker": {"provider": "claudeAgent", "model": "claude-opus-5-5"}}}));
    let m = s.call("async () => wf.slot(\"models\")", own(&mine)).await;
    assert_eq!(
        m["ticket"]["modelSelection"],
        json!({"instanceId": "codex", "model": "gpt-6.2-sol", "options": [{"id": "reasoningEffort", "value": "high"}]})
    );
    assert_eq!(
        m["worker"]["modelSelection"]["instanceId"], "claude-2",
        "a driver name picks the caller's instance when it offers the model"
    );
    let mut medium = provider("claude-2", "claudeAgent", &["claude-opus-5-5"]);
    medium["models"][0]["options"] = json!([{"id": "effort", "values": ["medium"]}]);
    s.h.fake.set_providers(vec![medium, provider("claudeAgent", "claudeAgent", &["claude-opus-5-5"])]);
    s.config(json!({"models": {"ticket": {"model": "claude-opus-5-5", "effort": "high"}}}));
    let m = s.call("async () => wf.slot(\"models\")", own(&mine)).await;
    assert_eq!(m["ticket"]["modelSelection"]["instanceId"], "claudeAgent", "an instance that offers the effort wins");
    assert!(m["ticket"].get("problem").is_none(), "{}", m["ticket"]);
    let out = s.run(Some(ME), "async () => wf.slot(\"reviewer\")", own(&mine)).await;
    assert!(out.is_error && out.text.contains("no slot \"reviewer\""), "{}", out.text);
}

#[tokio::test]
async fn renewal_matches_the_failure_text_in_code() {
    let s = S::new("claudeAgent");
    let claude = s.home.join(".claude");
    skill(&claude.join("skills"), "aws-sso-login", false);
    s.config(json!({"credentials": [
        {"match": "(?<=token )expired", "skill": "gone-login"},
        {"match": "ExpiredToken|SSO session", "skill": "aws-sso-login"}]}));
    let hit = s
        .call("async () => wf.renewal({ output: \"An error occurred (ExpiredToken) when calling\" })", own(&claude))
        .await;
    assert_eq!(
        (hit["match"].as_str(), hit["skill"]["name"].as_str()),
        (Some("ExpiredToken|SSO session"), Some("aws-sso-login"))
    );
    assert_eq!((hit["skipped"].as_bool(), hit["how"].as_str()), (Some(false), Some("Skill tool")));

    let none = s.call("async () => wf.renewal({ output: \"connection refused\" })", own(&claude)).await;
    assert!(none.is_null(), "{none}");

    let skipped = s.call("async () => wf.renewal({ output: \"the token expired\" })", own(&claude)).await;
    assert_eq!((skipped["skipped"].as_bool(), skipped["skill"]["status"].as_str()), (Some(true), Some("missing")));
    assert!(skipped["reason"].as_str().unwrap().contains("gone-login"));
}

#[tokio::test]
async fn repo_overrides_the_project_and_finds_its_git_root() {
    let s = S::new("claudeAgent");
    let other = s.h.dir.path().join("worktree");
    std::fs::create_dir_all(other.join("sub")).unwrap();
    let git = std::process::Command::new("git").arg("init").arg("-q").arg(&other).status();
    if !git.is_ok_and(|g| g.success()) {
        return;
    }
    std::fs::write(other.join("AGENTS.md"), "## Agent workflow\n\n- Delivery: tag-deploy\n").unwrap();
    let code = format!("async () => wf.slot(\"delivery\", {{ repo: {} }})", json!(other.join("sub")));
    let d = s.call(&code, own(&s.home.join(".claude"))).await;
    assert_eq!((d["mode"].as_str(), d["source"].as_str()), (Some("tag-deploy"), Some("AGENTS.md")));
    assert_eq!(d["problem"], "tag-deploy names no deploy skill");
}

#[tokio::test]
async fn without_a_caller_the_repo_is_required() {
    let s = S::new("claudeAgent");
    let out = s.run(None, "async () => wf.slots()", Reported::default()).await;
    assert!(out.is_error && out.text.contains("pass { repo }"), "{}", out.text);
    let code = format!("async () => (await wf.slots({{ repo: {} }})).repo", json!(s.repo));
    let v = ok_json(&s.run(None, &code, Reported::default()).await)["result"].clone();
    assert_eq!(v.as_str(), s.repo.to_str());
}

async fn serve(s: &S) -> (u16, tokio::sync::oneshot::Sender<()>) {
    let (listener, addr) = t3_codemode::server::bind(0).await.unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let app = s.h.app.clone();
    tokio::spawn(async move {
        t3_codemode::server::serve(app, listener, async move {
            let _ = rx.await;
        })
        .await
        .unwrap();
    });
    (addr.port(), tx)
}

fn t3cm(home: &Path) -> std::process::Command {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_t3cm"));
    cmd.env("HOME", home)
        .env("USERPROFILE", home)
        .env("T3CM_CONFIG", home.join("none.json"))
        .env("T3CM_NO_UPDATE_CHECK", "1")
        .env("T3CM_STATE_DIR", home.join("state"))
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_STATE_HOME")
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CODEX_HOME")
        .env_remove("GEMINI_HOME")
        .env_remove("ANTIGRAVITY_AGENT")
        .env_remove("T3WF_CONFIG")
        .env_remove("T3CODE_THREAD_ID");
    cmd
}

#[test]
fn the_cli_in_an_antigravity_shell_reads_skills_inline() {
    let home = tempfile::tempdir().unwrap();
    let repo = home.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let path = skill(&home.path().join(".gemini/config/skills"), "sol-loop", false);
    let cfg = home.path().join(".config/t3-workflow/config.json");
    std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
    std::fs::write(&cfg, json!({"review": "sol-loop"}).to_string()).unwrap();
    let out = t3cm(home.path())
        .args(["slots", "--no-t3", "--repo"])
        .arg(&repo)
        .env("ANTIGRAVITY_AGENT", "1")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{text}");
    assert!(
        text.contains(&format!("review: sol-loop (inline: read {} and apply it) [user]", path.display())),
        "{text}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_http_header_and_t3cm_exec_report_the_config_folder() {
    let s = S::new("claude-2");
    let (port, _stop) = serve(&s).await;
    let mine = s.home.join(".claude-2");
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                      "params": {"name": "execute", "arguments": {"code": "async () => (await wf.slots()).context"}}});
    let r: Value = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .post(format!("http://127.0.0.1:{port}/mcp"))
        .header("X-T3-Thread", ME)
        .header("X-T3-Config-Dir", mine.to_string_lossy().as_ref())
        .json(&body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ctx: Value = serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(ctx["result"]["claudeDir"], mine.to_string_lossy().as_ref());
    assert_eq!(ctx["result"]["folderFrom"], "reported");

    let home = s.home.clone();
    let codex = s.home.join("cx");
    let out = tokio::task::spawn_blocking(move || {
        t3cm(&home)
            .args(["exec", "-", "--port", &port.to_string()])
            .env("T3CODE_THREAD_ID", ME)
            .env("CLAUDE_CONFIG_DIR", home.join("from-exé"))
            .env("CODEX_HOME", &codex)
            .env("GEMINI_HOME", home.join("profile"))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut c| {
                use std::io::Write;
                c.stdin.take().unwrap().write_all(b"async () => (await wf.slots()).context")?;
                c.wait_with_output()
            })
            .unwrap()
    })
    .await
    .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["result"]["claudeDir"], s.home.join("from-exé").to_string_lossy().as_ref());
    assert_eq!(v["result"]["codexHome"], s.home.join("cx").to_string_lossy().as_ref());
    assert_eq!(v["result"]["geminiHome"], s.home.join("profile").to_string_lossy().as_ref());

    s.thread("mcp:other", "claude-3");
    let home = s.home.clone();
    let out = tokio::task::spawn_blocking(move || {
        t3cm(&home)
            .args(["exec", "-", "--port", &port.to_string(), "--thread", "mcp:other"])
            .env("T3CODE_THREAD_ID", ME)
            .env("CLAUDE_CONFIG_DIR", home.join("not-yours"))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut c| {
                use std::io::Write;
                c.stdin.take().unwrap().write_all(b"async () => (await wf.slots()).context")?;
                c.wait_with_output()
            })
            .unwrap()
    })
    .await
    .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["result"]["folderFrom"], "default", "a shell reports its folder only for its own thread");
    assert!(s.h.app.store.instance_folder("claude-3").unwrap().is_none());
}

#[test]
fn the_cli_resolves_offline_and_doctor_names_a_removed_skill() {
    let home = tempfile::tempdir().unwrap();
    let repo = home.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let path = skill(&home.path().join(".agents/skills"), "sol-loop", false);
    let cfg = home.path().join(".config/t3-workflow/config.json");
    std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
    std::fs::write(&cfg, json!({"review": "sol-loop", "passes": ["gone"]}).to_string()).unwrap();

    let out = t3cm(home.path()).args(["slots", "--no-t3", "--repo"]).arg(&repo).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{text}");
    assert!(text.contains("review: sol-loop (Skill tool) [user]"), "{text}");
    assert!(text.contains("passes: SKIP gone: no longer resolves [user]"), "{text}");

    let out =
        t3cm(home.path()).args(["doctor", "--resolve", "sol-loop", "--json", "--repo"]).arg(&repo).output().unwrap();
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["resolved"]["path"].as_str(), path.to_str());
    assert_eq!(v["candidates"][0]["where"], "~/.agents/skills");
    let out = t3cm(home.path()).args(["doctor", "--resolve", "nothing-here", "--repo"]).arg(&repo).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stdout).contains("nothing-here: not installed"));

    let out = t3cm(home.path()).args(["doctor", "--no-t3", "--repo"]).arg(&repo).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "{text}");
    assert!(text.contains("PROBLEM: passes[0]: skill gone no longer resolves"), "{text}");
    assert!(text.contains("doctor: "), "{text}");

    std::fs::write(&cfg, json!({"credentials": [{"match": "(", "skill": "x"}]}).to_string()).unwrap();
    let out = t3cm(home.path()).args(["doctor", "--no-t3", "--json", "--repo"]).arg(&repo).output().unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let problems: Vec<&str> = v["problems"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    assert!(problems.iter().any(|p| p.contains("credentials[0].match: not a regular expression")), "{problems:?}");
    assert!(!home.path().join("state").exists(), "offline doctor must not create a missing state directory");
}

#[tokio::test]
async fn model_checks_search_catalog_validate_codex_effort_and_default_effort() {
    let s = S::new("claudeAgent");
    let claude = s.home.join(".claude");
    // Unfiltered capabilities omit catalog-only model (short_list = 1); search returns it.
    s.h.fake.set_providers(vec![provider("claudeAgent", "claudeAgent", &["claude-opus-5-5", "claude-catalog-only"])]);
    s.h.fake.set_short_list(1);
    s.config(json!({"models": {"worker": {"model": "claude-catalog-only", "effort": "high"}}}));
    let m = s.call("async () => wf.slot(\"models\")", own(&claude)).await;
    assert!(m["worker"].get("problem").is_none(), "{}", m["worker"]);
    assert_eq!(m["worker"]["modelSelection"]["model"], "claude-catalog-only");

    // Codex reasoningEffort is validated consistently: high passes, turbo fails.
    let mut codex = provider("codex", "codex", &["gpt-6.2-sol"]);
    codex["models"][0]["options"] = json!([{"id": "reasoningEffort", "values": ["low", "high"]}]);
    s.h.fake.set_providers(vec![provider("claudeAgent", "claudeAgent", &["claude-opus-5-5"]), codex]);
    s.h.fake.set_short_list(0);
    s.config(json!({"models": {
        "ticket": {"provider": "codex", "model": "gpt-6.2-sol", "effort": "high"},
        "worker": {"provider": "codex", "model": "gpt-6.2-sol", "effort": "turbo"}
    }}));
    let m = s.call("async () => wf.slot(\"models\")", own(&claude)).await;
    assert!(m["ticket"].get("problem").is_none(), "{}", m["ticket"]);
    assert!(
        m["worker"]["problem"].as_str().is_some_and(|p| p.contains("effort turbo is not offered")),
        "{}",
        m["worker"]
    );

    // Default model does not advertise high effort when Claude only offers low.
    let mut low_only = provider("claudeAgent", "claudeAgent", &["claude-opus-5-5"]);
    low_only["models"][0]["options"] = json!([{"id": "effort", "values": ["low"]}]);
    s.h.fake.set_providers(vec![low_only]);
    s.config(json!({}));
    let m = s.call("async () => wf.slot(\"models\")", own(&claude)).await;
    assert!(m["worker"]["spec"].is_null(), "{}", m["worker"]);
    assert!(
        m["worker"]["problem"].as_str().is_some_and(|p| p.contains("effort high is not offered")),
        "{}",
        m["worker"]
    );

    // Default model also requires high effort to be explicitly offered, not omitted from options metadata.
    let mut no_options = provider("claudeAgent", "claudeAgent", &["claude-opus-5-5"]);
    no_options["models"][0]["options"] = json!([]);
    s.h.fake.set_providers(vec![no_options]);
    let m = s.call("async () => wf.slot(\"models\")", own(&claude)).await;
    assert!(m["worker"]["spec"].is_null(), "{}", m["worker"]);
    assert!(
        m["worker"]["problem"].as_str().is_some_and(|p| p.contains("effort high is not offered")),
        "{}",
        m["worker"]
    );
}

#[tokio::test]
async fn unreadable_agents_md_and_malformed_plugin_records_are_handled_safely() {
    let s = S::new("claudeAgent");
    let claude = s.home.join(".claude");
    // A directory at AGENTS.md is unreadable as a file and must report a problem.
    std::fs::create_dir(s.repo.join("AGENTS.md")).unwrap();
    let v = s.call("async () => wf.slots()", own(&claude)).await;
    assert!(
        v["problems"].as_array().unwrap().iter().any(|p| p.as_str().is_some_and(|s| s.contains("AGENTS.md"))),
        "{}",
        v["problems"]
    );
    std::fs::remove_dir(s.repo.join("AGENTS.md")).unwrap();

    // A null record or malformed projectPath in installed_plugins.json must be skipped
    // without dropping projectPath scoping or turning malformed project records into global ones.
    let other_repo = s.h.dir.path().join("other-repo");
    std::fs::create_dir_all(&other_repo).unwrap();
    let scoped_root = claude.join("plugins/cache/acme/scoped/1.0.0");
    skill(&scoped_root.join("skills"), "secret-skill", false);
    let false_path_root = claude.join("plugins/cache/acme/badproj/1.0.0");
    skill(&false_path_root.join("skills"), "leaked-skill", false);
    let user_root = claude.join("plugins/cache/acme/userok/1.0.0");
    skill(&user_root.join("skills"), "allowed-skill", false);
    let path = claude.join("plugins/installed_plugins.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        json!({
            "version": 2,
            "plugins": {
                "broken@acme": [null],
                "scoped@acme": [{"scope": "project", "projectPath": other_repo.to_string_lossy(), "installPath": scoped_root.to_string_lossy()}],
                "badproj@acme": [{"scope": "project", "projectPath": false, "installPath": false_path_root.to_string_lossy()}],
                "userok@acme": [{"scope": "user", "installPath": user_root.to_string_lossy()}]
            }
        })
        .to_string(),
    )
    .unwrap();
    s.config(json!({"passes": ["scoped:secret-skill", "badproj:leaked-skill", "userok:allowed-skill"]}));
    let v = s.call("async () => wf.slot(\"passes\")", own(&claude)).await;
    assert_eq!(
        v["skills"][0]["status"], "missing",
        "project-scoped plugin for another repo must not leak via cache fallback"
    );
    assert_eq!(
        v["skills"][1]["status"], "missing",
        "project-scoped plugin with projectPath: false must not be treated as unscoped"
    );
    assert_eq!(v["skills"][2]["status"], "ok", "valid user-scoped plugin without projectPath still resolves");
}

#[tokio::test]
async fn real_shaped_config_with_deploying_repo_overrides_resolves_for_default_and_custom_instances() {
    const DEPLOYING_BLOCK: &str = "# Repo\n\n## Agent workflow\n\n\
        - Tracker: github-issues (see Backlog)\n\
        - Delivery: tag-deploy (see Deploying)\n\
        - Deploy skill: `deploy` (`content/v*` when only `training/` changed, else `deploy/v*`)\n\
        - Merge method: rebase\n\
        - PR format: the `pr` skill\n\
        - Checks: `npm test` (runs `tsc` first) and `npm run build`\n\
        - In-progress label: none\n\
        - Identity: the user's own; no bot\n";

    for (instance, custom_folder) in [("claudeAgent", false), ("claude-2", true)] {
        let s = S::new(instance);
        let claude = if custom_folder { s.home.join(".claude-2") } else { s.home.join(".claude") };
        let cbrip = claude.join("plugins/cache/acme/cbrip/1.40.0");
        let sol_path = skill(&cbrip.join("skills"), "sol-loop", false);
        let aws_path = skill(&cbrip.join("skills"), "aws-sso-login", false);
        let deploy_path = skill(&cbrip.join("skills"), "deploy", false);
        let voice = claude.join("plugins/cache/acme/paul-otto/1.2.0");
        let voice_path = skill(&voice.join("skills"), "paul-voice", false);
        let installed = claude.join("plugins/installed_plugins.json");
        std::fs::create_dir_all(installed.parent().unwrap()).unwrap();
        std::fs::write(
            &installed,
            json!({
                "version": 2,
                "plugins": {
                    "cbrip@acme": [{"scope": "user", "installPath": cbrip.to_string_lossy()}],
                    "paul-otto@acme": [{"scope": "user", "installPath": voice.to_string_lossy()}]
                }
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            claude.join("settings.json"),
            json!({"enabledPlugins": {"cbrip@acme": true, "paul-otto@acme": true}}).to_string(),
        )
        .unwrap();
        let agents = s.home.join(".agents/skills");
        let pr_path = skill(&agents, "pr", false);
        let unslop_path = skill(&agents, "unslop", true);
        s.config(json!({
            "version": 1,
            "writtenBy": "0.6.0",
            "steps": ["slot:review", "slot:passes", "slot:credentials", "slot:delivery", "slot:models", "rule:filed-issues"],
            "review": "cbrip:sol-loop",
            "passes": ["pr", "paul-otto:paul-voice", "unslop"],
            "credentials": [{"match": "ExpiredToken|Token has expired|SSO session|aws sso login", "skill": "cbrip:aws-sso-login"}],
            "delivery": {"mode": "merge"},
            "models": {
                "ticket": {"provider": "claude-2", "model": "claude-opus-5-5", "effort": "high"},
                "worker": {"provider": "claude-2", "model": "claude-opus-5-5", "effort": "high"}
            }
        }));
        let reported =
            if custom_folder { own(&claude) } else { Reported { own: Some(String::new()), ..Reported::default() } };

        let plain = s.call("async () => wf.slots()", reported.clone()).await;
        assert_eq!(plain["problems"], json!([]));
        assert_eq!(plain["warnings"], json!([]));
        assert_eq!(plain["review"]["source"], "user");
        assert_eq!(plain["review"]["skill"]["path"].as_str(), sol_path.to_str());
        assert_eq!(plain["review"]["skill"]["invoke"], "skill");
        let passes = plain["passes"]["skills"].as_array().unwrap();
        assert_eq!(passes.len(), 3);
        assert_eq!((passes[0]["path"].as_str(), passes[0]["invoke"].as_str()), (pr_path.to_str(), Some("skill")));
        assert_eq!((passes[1]["path"].as_str(), passes[1]["invoke"].as_str()), (voice_path.to_str(), Some("skill")));
        assert_eq!((passes[2]["path"].as_str(), passes[2]["invoke"].as_str()), (unslop_path.to_str(), Some("inline")));
        assert_eq!(plain["credentials"]["renewals"][0]["skill"]["path"].as_str(), aws_path.to_str());
        assert_eq!(plain["delivery"]["mode"], "merge");
        assert_eq!(plain["delivery"]["source"], "user");
        assert_eq!(plain["models"]["ticket"]["spec"]["provider"], "claude-2");
        assert_eq!(plain["models"]["worker"]["spec"]["model"], "claude-opus-5-5");

        std::fs::write(s.repo.join("AGENTS.md"), DEPLOYING_BLOCK).unwrap();
        let deploying = s.call("async () => wf.slots()", reported).await;
        assert_eq!(deploying["problems"], json!([]));
        assert_eq!(deploying["warnings"], json!([]));
        assert_eq!(deploying["delivery"]["mode"], "tag-deploy");
        assert_eq!(deploying["delivery"]["source"], "AGENTS.md");
        assert_eq!(deploying["delivery"]["skill"]["path"].as_str(), deploy_path.to_str());
        assert_eq!(deploying["delivery"]["skill"]["invoke"], "skill");
        assert_eq!(deploying["review"]["skill"]["path"].as_str(), sol_path.to_str());
    }
}

#[tokio::test]
async fn legacy_delivery_configs_normalize_with_full_parity_to_steps() {
    let s = S::new("claudeAgent");
    let claude = s.home.join(".claude");
    let deploy_path = skill(&claude.join("skills"), "deploy", false);
    let ship_path = skill(&claude.join("skills"), "ship", true);

    let canonical_merge = json!({
        "steps": [{"id": "merge", "kind": "builtin", "gate": "always", "scope": "pr"}],
        "closeAfter": "ticket",
    });
    let canonical_tag_deploy = |skill_name: &str, path: &Path, invoke: &str| {
        let how = if invoke == "inline" {
            format!("inline: read {} and apply it", path.display())
        } else {
            "Skill tool".into()
        };
        json!({
            "steps": [
                {"id": "merge", "kind": "builtin", "gate": "always", "scope": "pr"},
                {
                    "id": "deploy",
                    "kind": "skill",
                    "skill": {
                        "ref": skill_name,
                        "name": skill_name,
                        "path": path.to_string_lossy(),
                        "where": "~/.claude/skills",
                        "invoke": invoke,
                        "status": "ok",
                        "how": how,
                    },
                    "gate": "chain",
                    "scope": "pr",
                    "watch": true,
                }
            ],
            "closeAfter": "ticket",
        })
    };

    let shape = |d: &Value| json!({"steps": d["steps"], "closeAfter": d["closeAfter"]});

    // 1. Default (no config)
    let d = s.call("async () => wf.slot(\"delivery\")", own(&claude)).await;
    assert_eq!(d["source"], "default");
    assert_eq!(shape(&d), canonical_merge);

    // 2. User config {"mode": "merge"}
    s.config(json!({"delivery": {"mode": "merge"}}));
    let d = s.call("async () => wf.slot(\"delivery\")", own(&claude)).await;
    assert_eq!(d["source"], "user");
    assert_eq!(shape(&d), canonical_merge);

    // 3. AGENTS.md `- Delivery: merge`
    std::fs::write(s.repo.join("AGENTS.md"), "## Agent workflow\n\n- Delivery: merge\n").unwrap();
    let d = s.call("async () => wf.slot(\"delivery\")", own(&claude)).await;
    assert_eq!(d["source"], "AGENTS.md");
    assert_eq!(shape(&d), canonical_merge);

    // 4. Explicit equivalent steps in .t3-workflow.json
    std::fs::write(
        s.repo.join(".t3-workflow.json"),
        json!({"delivery": {"steps": [{"id": "merge", "kind": "builtin", "gate": "always", "scope": "pr"}]}})
            .to_string(),
    )
    .unwrap();
    let d_explicit = s.call("async () => wf.slot(\"delivery\")", own(&claude)).await;
    assert_eq!(d_explicit["source"], ".t3-workflow.json");
    assert_eq!(shape(&d_explicit), canonical_merge);
    std::fs::remove_file(s.repo.join(".t3-workflow.json")).unwrap();
    std::fs::remove_file(s.repo.join("AGENTS.md")).unwrap();

    // 5. User config {"mode": "tag-deploy", "skill": "deploy"}
    let want_deploy = canonical_tag_deploy("deploy", &deploy_path, "skill");
    s.config(json!({"delivery": {"mode": "tag-deploy", "skill": "deploy"}}));
    let d_user = s.call("async () => wf.slot(\"delivery\")", own(&claude)).await;
    assert_eq!(d_user["source"], "user");
    assert_eq!(shape(&d_user), want_deploy);

    // 6. AGENTS.md Delivery: tag-deploy + Deploy skill: deploy
    s.config(json!({}));
    std::fs::write(s.repo.join("AGENTS.md"), "## Agent workflow\n\n- Delivery: tag-deploy\n- Deploy skill: `deploy`\n")
        .unwrap();
    let d_agents = s.call("async () => wf.slot(\"delivery\")", own(&claude)).await;
    assert_eq!(d_agents["source"], "AGENTS.md");
    assert_eq!(shape(&d_agents), want_deploy);

    // 7. Explicit equivalent steps in .t3-workflow.json matches legacy tag-deploy byte-for-byte
    std::fs::write(
        s.repo.join(".t3-workflow.json"),
        json!({
            "delivery": {
                "steps": [
                    {"id": "merge", "kind": "builtin", "gate": "always", "scope": "pr"},
                    {"id": "deploy", "skill": "deploy", "gate": "chain", "scope": "pr", "watch": true}
                ]
            }
        })
        .to_string(),
    )
    .unwrap();
    let d_steps = s.call("async () => wf.slot(\"delivery\")", own(&claude)).await;
    assert_eq!(d_steps["source"], ".t3-workflow.json");
    assert_eq!(shape(&d_steps), want_deploy);
    assert_eq!(d_steps["mode"], "tag-deploy");
    std::fs::remove_file(s.repo.join(".t3-workflow.json")).unwrap();

    // 8. Partial AGENTS.md `- Deploy skill: ship` overrides user's tag-deploy skill
    s.config(json!({"delivery": {"mode": "tag-deploy", "skill": "deploy"}}));
    std::fs::write(s.repo.join("AGENTS.md"), "## Agent workflow\n\n- Deploy skill: `ship`\n").unwrap();
    let d_override = s.call("async () => wf.slot(\"delivery\")", own(&claude)).await;
    assert_eq!(d_override["source"], "AGENTS.md");
    assert_eq!(shape(&d_override), canonical_tag_deploy("ship", &ship_path, "inline"));

    // 9. AGENTS.md `- Delivery: merge` overrides user's tag-deploy cleanly without leaking deploy skill
    std::fs::write(s.repo.join("AGENTS.md"), "## Agent workflow\n\n- Delivery: merge\n").unwrap();
    let d_merge_override = s.call("async () => wf.slot(\"delivery\")", own(&claude)).await;
    assert_eq!(d_merge_override["source"], "AGENTS.md");
    assert_eq!(shape(&d_merge_override), canonical_merge);
    assert_eq!(d_merge_override["mode"], "merge");
    assert!(d_merge_override.get("skill").is_none());
}

#[tokio::test]
async fn lms_three_stage_config_resolves_filters_paths_and_respects_layer_precedence() {
    let s = S::new("claudeAgent");
    let claude = s.home.join(".claude");
    let deploy_path = skill(&claude.join("skills"), "deploy", false);
    let publish_path = skill(&claude.join("skills"), "lms-publish", true);
    skill(&claude.join("skills"), "sol-loop", false);
    skill(&claude.join("skills"), "pr", false);

    // Layer 2 (user)
    s.config(json!({
        "review": "sol-loop",
        "passes": ["pr"],
        "delivery": {"mode": "merge"}
    }));
    // Layer 3 (AGENTS.md)
    std::fs::write(
        s.repo.join("AGENTS.md"),
        "## Agent workflow\n\n- Review: none\n- Delivery: tag-deploy\n- Deploy skill: `deploy`\n",
    )
    .unwrap();
    // Layer 4 (.t3-workflow.json): overrides delivery only; review stays from AGENTS.md, passes from user
    let lms_cfg = json!({
        "version": 1,
        "delivery": {
            "steps": [
                {"id": "merge",   "kind": "builtin", "gate": "always", "scope": "pr"},
                {"id": "deploy",  "skill": "deploy", "gate": "chain",  "scope": "ticket", "watch": true},
                {"id": "publish", "skill": "lms-publish", "gate": "always", "scope": "post-handoff", "paths": ["training/**"]}
            ],
            "closeAfter": "ticket"
        }
    });
    let repo_cfg_path = s.repo.join(".t3-workflow.json");
    std::fs::write(&repo_cfg_path, lms_cfg.to_string()).unwrap();

    // Slot-by-slot precedence across all 4 layers
    let all = s.call("async () => wf.slots()", own(&claude)).await;
    assert_eq!(all["problems"], json!([]));
    assert_eq!(all["warnings"], json!([]));
    assert_eq!(all["repoConfig"].as_str(), repo_cfg_path.to_str());
    assert_eq!(all["passes"]["source"], "user");
    assert_eq!(all["review"]["source"], "AGENTS.md");
    assert!(all["review"]["skill"].is_null());
    assert_eq!(all["delivery"]["source"], ".t3-workflow.json");
    assert_eq!(all["delivery"]["closeAfter"], "ticket");
    assert!(all["delivery"].get("mode").is_none());

    // Unfiltered wf.slot("delivery"): all 3 steps present with `how`
    let d_all = s.call("async () => wf.slot(\"delivery\")", own(&claude)).await;
    let steps = d_all["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 3);
    assert_eq!(
        (steps[0]["id"].as_str(), steps[0]["kind"].as_str(), steps[0]["gate"].as_str(), steps[0]["scope"].as_str()),
        (Some("merge"), Some("builtin"), Some("always"), Some("pr"))
    );
    assert_eq!(
        (
            steps[1]["id"].as_str(),
            steps[1]["kind"].as_str(),
            steps[1]["gate"].as_str(),
            steps[1]["scope"].as_str(),
            steps[1]["watch"].as_bool()
        ),
        (Some("deploy"), Some("skill"), Some("chain"), Some("ticket"), Some(true))
    );
    assert_eq!(steps[1]["skill"]["path"].as_str(), deploy_path.to_str());
    assert_eq!(steps[1]["skill"]["how"], "Skill tool");
    assert_eq!(
        (steps[2]["id"].as_str(), steps[2]["kind"].as_str(), steps[2]["gate"].as_str(), steps[2]["scope"].as_str()),
        (Some("publish"), Some("skill"), Some("always"), Some("post-handoff"))
    );
    assert_eq!(steps[2]["skill"]["path"].as_str(), publish_path.to_str());
    assert_eq!(steps[2]["skill"]["how"], format!("inline: read {} and apply it", publish_path.display()));
    assert_eq!(steps[2]["paths"], json!(["training/**"]));
    assert!(d_all.get("skipped").is_none());

    // Filtered with non-training files: publish is skipped deterministically
    let d_code =
        s.call("async () => wf.slot(\"delivery\", { files: [\"src/app.ts\", \"package.json\"] })", own(&claude)).await;
    let code_ids: Vec<&str> = d_code["steps"].as_array().unwrap().iter().map(|st| st["id"].as_str().unwrap()).collect();
    assert_eq!(code_ids, vec!["merge", "deploy"]);
    assert_eq!(
        d_code["skipped"],
        json!([{"id": "publish", "reason": "no files matched paths", "paths": ["training/**"]}])
    );
    assert!(d_code["line"].as_str().unwrap().contains("skipped: publish"), "{}", d_code["line"]);

    // Filtered with training files: all 3 steps match, skipped is empty
    let d_training =
        s.call("async () => wf.slot(\"delivery\", { files: [\"training/course-1/lesson.md\"] })", own(&claude)).await;
    let training_ids: Vec<&str> =
        d_training["steps"].as_array().unwrap().iter().map(|st| st["id"].as_str().unwrap()).collect();
    assert_eq!(training_ids, vec!["merge", "deploy", "publish"]);
    assert_eq!(d_training["skipped"], json!([]));

    // Filtered with empty files array: publish skipped
    let d_empty = s.call("async () => wf.slot(\"delivery\", { files: [] })", own(&claude)).await;
    assert_eq!(d_empty["steps"].as_array().unwrap().len(), 2);
    assert_eq!(d_empty["skipped"][0]["id"], "publish");

    // Passing `files` to a non-delivery slot errors cleanly
    let bad = s.run(Some(ME), "async () => wf.slot(\"review\", { files: [\"a.rs\"] })", own(&claude)).await;
    assert!(bad.is_error && bad.text.contains("only supported for the delivery slot"), "{}", bad.text);

    // Modifying .t3-workflow.json (same size & mtime) invalidates cache
    let mtime = std::fs::metadata(&repo_cfg_path).unwrap().modified().unwrap();
    let updated = json!({
        "version": 1,
        "delivery": {
            "steps": [
                {"id": "merge",   "kind": "builtin", "gate": "always", "scope": "pr"},
                {"id": "deploy",  "skill": "deploy", "gate": "chain",  "scope": "ticket", "watch": true},
                {"id": "publish", "skill": "lms-publish", "gate": "always", "scope": "post-handoff", "paths": ["courses/**"]}
            ],
            "closeAfter": "ticket"
        }
    });
    std::fs::write(&repo_cfg_path, updated.to_string()).unwrap();
    std::fs::File::options().write(true).open(&repo_cfg_path).unwrap().set_modified(mtime).unwrap();
    let d_updated = s.call("async () => wf.slot(\"delivery\", { files: [\"courses/intro.md\"] })", own(&claude)).await;
    assert_eq!(d_updated["steps"].as_array().unwrap().len(), 3);
    assert_eq!(d_updated["steps"][2]["paths"], json!(["courses/**"]));

    // Manual step resolves with prompt and closeAfter: "all"
    std::fs::write(
        &repo_cfg_path,
        json!({
            "delivery": {
                "steps": [
                    {"id": "merge", "kind": "builtin", "gate": "always", "scope": "pr"},
                    {"id": "announce", "kind": "manual", "prompt": "Post notes in #releases", "gate": "always", "scope": "post-handoff"}
                ],
                "closeAfter": "all"
            }
        })
        .to_string(),
    )
    .unwrap();
    let d_manual = s.call("async () => wf.slot(\"delivery\")", own(&claude)).await;
    assert_eq!(d_manual["closeAfter"], "all");
    assert_eq!(d_manual["steps"][1]["kind"], "manual");
    assert_eq!(d_manual["steps"][1]["prompt"], "Post notes in #releases");
    assert!(d_manual["line"].as_str().unwrap().contains("announce (manual)"), "{}", d_manual["line"]);
    assert!(d_manual["line"].as_str().unwrap().contains("closeAfter all"), "{}", d_manual["line"]);
}

#[test]
fn doctor_validates_repo_workflow_json_and_surfaces_skipped_or_bad_delivery_steps() {
    let home = tempfile::tempdir().unwrap();
    let repo = home.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    skill(&home.path().join(".claude/skills"), "deploy", false);
    skill(&home.path().join(".claude/skills"), "lms-publish", false);

    let lms_cfg = json!({
        "version": 1,
        "delivery": {
            "steps": [
                {"id": "merge",   "kind": "builtin", "gate": "always", "scope": "pr"},
                {"id": "deploy",  "skill": "deploy", "gate": "chain",  "scope": "ticket", "watch": true},
                {"id": "publish", "skill": "lms-publish", "gate": "always", "scope": "post-handoff", "paths": ["training/**"]}
            ],
            "closeAfter": "ticket"
        }
    });
    std::fs::write(repo.join(".t3-workflow.json"), lms_cfg.to_string()).unwrap();

    // Valid lms config: no delivery or config problems
    let out = t3cm(home.path()).args(["doctor", "--no-t3", "--json", "--repo"]).arg(&repo).output().unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let problems: Vec<&str> = v["problems"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    assert!(!problems.iter().any(|p| p.contains("delivery") || p.contains(".t3-workflow.json")), "{problems:?}");
    assert_eq!(v["settings"]["delivery"]["source"], ".t3-workflow.json");
    assert_eq!(v["settings"]["delivery"]["steps"].as_array().unwrap().len(), 3);

    // Missing skill in a delivery step: doctor reports PROBLEM and surfaces SKIP in summary line
    std::fs::remove_dir_all(home.path().join(".claude/skills/lms-publish")).unwrap();
    let out = t3cm(home.path()).args(["doctor", "--no-t3", "--repo"]).arg(&repo).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "{text}");
    assert!(text.contains("SKIP lms-publish: no longer resolves"), "{text}");
    assert!(text.contains("PROBLEM: delivery.publish: skill lms-publish no longer resolves"), "{text}");

    // Bad .t3-workflow.json (merge not first, invalid glob, chain without always, bad closeAfter)
    std::fs::write(
        repo.join(".t3-workflow.json"),
        json!({
            "delivery": {
                "steps": [
                    {"id": "deploy", "skill": "deploy", "gate": "chain", "scope": "ticket", "paths": ["training/[bad"]},
                    {"id": "merge", "kind": "builtin", "gate": "always", "scope": "pr"}
                ],
                "closeAfter": "bogus"
            }
        })
        .to_string(),
    )
    .unwrap();
    let out = t3cm(home.path()).args(["doctor", "--no-t3", "--json", "--repo"]).arg(&repo).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let problems: Vec<&str> = v["problems"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    assert!(problems.iter().any(|p| p.contains("closeAfter")), "{problems:?}");
    assert!(problems.iter().any(|p| p.contains("requires a preceding step with gate 'always'")), "{problems:?}");
    assert!(problems.iter().any(|p| p.contains("invalid glob")), "{problems:?}");
    assert!(problems.iter().any(|p| p.contains("builtin merge step must be first")), "{problems:?}");
    assert!(problems.iter().any(|p| p.contains("cannot follow a broader scope")), "{problems:?}");
    // Broken .t3-workflow.json is ignored so delivery falls back to default merge cleanly
    assert_eq!(v["settings"]["delivery"]["source"], "default");
    assert_eq!(v["settings"]["delivery"]["steps"].as_array().unwrap().len(), 1);
}

#[test]
fn doctor_warns_when_legacy_board_ticket_keys_are_set() {
    let home = tempfile::tempdir().unwrap();
    let repo = home.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let cfg_path = home.path().join("t3cm.json");

    // Non-legacy board keys and null pool produce no legacy ticket warning.
    std::fs::write(&cfg_path, json!({"board": {"settleAfterHours": 48, "pool": null}}).to_string()).unwrap();
    let out = t3cm(home.path())
        .env("T3CM_CONFIG", &cfg_path)
        .args(["doctor", "--no-t3", "--json", "--repo"])
        .arg(&repo)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let warnings: Vec<&str> = v["warnings"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    assert!(!warnings.iter().any(|w| w.contains("legacy ticket launch")), "{warnings:?}");

    // Setting legacy board ticket keys emits a warning pointing to models.ticket without failing doctor.
    std::fs::write(
        &cfg_path,
        json!({
            "pools": {
                "opus": {
                    "driver": "claudeAgent",
                    "model": "claude-opus-5-5",
                    "members": [{"instanceId": "claude-a"}]
                }
            },
            "board": {
                "instanceId": "claude-2",
                "ticketModel": "claude-opus-5-5",
                "ticketEffort": "medium",
                "pool": "opus"
            }
        })
        .to_string(),
    )
    .unwrap();
    let out = t3cm(home.path())
        .env("T3CM_CONFIG", &cfg_path)
        .args(["doctor", "--no-t3", "--json", "--repo"])
        .arg(&repo)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let warnings: Vec<&str> = v["warnings"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    let legacy = warnings.iter().find(|w| w.contains("legacy ticket launch")).expect("legacy board warning");
    for key in ["board.instanceId", "board.ticketModel", "board.ticketEffort", "board.pool", "models.ticket"] {
        assert!(legacy.contains(key), "missing {key} in {legacy}");
    }
    assert!(legacy.contains("ask box"), "expected ask-box caveat for instanceId/pool: {legacy}");

    // When only ticketModel/ticketEffort are set, doctor advises moving them without the ask-box caveat.
    std::fs::write(&cfg_path, json!({"board": {"ticketModel": "claude-opus-5-5"}}).to_string()).unwrap();
    let out = t3cm(home.path())
        .env("T3CM_CONFIG", &cfg_path)
        .args(["doctor", "--no-t3", "--json", "--repo"])
        .arg(&repo)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let warnings: Vec<&str> = v["warnings"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    let legacy = warnings.iter().find(|w| w.contains("legacy ticket launch")).expect("legacy board warning");
    assert!(legacy.contains("board.ticketModel") && legacy.contains("move it into the models.ticket slot"), "{legacy}");
    assert!(!legacy.contains("ask box"), "{legacy}");
}
