//! Integration tests for `hive serve`: a real server against the in-process
//! test relay (`hive-runtime/src/test_relay.rs`), a fake OpenAI-compatible
//! model server and a fake ntfy, driven over HTTP + SSE like the phone.
//!
//! The env-gated live test runs the same flow against the lab gateway:
//!
//! ```text
//! HIVE_SERVE_LIVE_ENDPOINT=http://100.101.102.103:8080/v1/chat/completions \
//! HIVE_SERVE_LIVE_MODELS=gpt-oss:20b,gemma4:12b \
//!   cargo test -p hive-cli serve::tests::live -- --nocapture
//! ```

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::post;
use axum::Router;
use futures_util::StreamExt;
use hive_core::{workspace_config_session_id, ModelProviderKind, WorkspaceAgent, WorkspaceRuntime};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use uuid::Uuid;

use super::*;
use crate::repos::RepoBindings;

#[path = "../../../hive-runtime/src/test_relay.rs"]
#[allow(dead_code)]
mod test_relay;

async fn spawn(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{addr}")
}

// ── Fake model server ──────────────────────────────────────────────────────

#[derive(Clone, Default)]
struct FakeModel {
    bodies: Arc<Mutex<Vec<Value>>>,
    rounds: Arc<Vec<Vec<String>>>,
}

fn delta(d: Value) -> String {
    format!("data: {}", json!({"choices": [{"index": 0, "delta": d}]}))
}

fn streamed_call(id: &str, name: &str, args: &str) -> Vec<String> {
    let (a, b) = args.split_at(args.len() / 2);
    vec![
        delta(json!({"tool_calls": [{"index": 0, "id": id, "type": "function", "function": {"name": name, "arguments": ""}}]})),
        delta(json!({"tool_calls": [{"index": 0, "function": {"arguments": a}}]})),
        delta(json!({"tool_calls": [{"index": 0, "function": {"arguments": b}}]})),
    ]
}

async fn fake_chat(State(f): State<FakeModel>, body: String) -> axum::response::Response {
    let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    let n = {
        let mut b = f.bodies.lock().unwrap();
        b.push(v);
        b.len() - 1
    };
    let lines = f.rounds.get(n).or_else(|| f.rounds.last()).cloned().unwrap_or_default();
    let mut out: String = lines.iter().map(|l| format!("{l}\n\n")).collect();
    out.push_str("data: [DONE]\n\n");
    axum::response::Response::builder().header("content-type", "text/event-stream").body(Body::from(out)).unwrap()
}

async fn spawn_model(rounds: Vec<Vec<String>>) -> (String, FakeModel) {
    let f = FakeModel { bodies: Default::default(), rounds: Arc::new(rounds) };
    let base = spawn(Router::new().route("/v1/chat/completions", post(fake_chat)).with_state(f.clone())).await;
    (format!("{base}/v1/chat/completions"), f)
}

// ── Fake ntfy ──────────────────────────────────────────────────────────────

type Pushes = Arc<Mutex<Vec<(HeaderMap, String)>>>;

async fn spawn_ntfy() -> (String, Pushes) {
    let pushes: Pushes = Default::default();
    let p = pushes.clone();
    let router = Router::new().route(
        "/:topic",
        post(move |headers: HeaderMap, body: String| {
            let p = p.clone();
            async move {
                p.lock().unwrap().push((headers, body));
                "{}"
            }
        }),
    );
    (spawn(router).await, pushes)
}

// ── Harness ────────────────────────────────────────────────────────────────

fn git(dir: &Path, args: &[&str]) {
    let out = hive_core::process_util::command("git").arg("-C").arg(dir).args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

fn make_repo(dir: &Path, files: &[(&str, &str)]) {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q"]);
    git(dir, &["config", "user.email", "t@t"]);
    git(dir, &["config", "user.name", "t"]);
    for (name, body) in files {
        std::fs::write(dir.join(name), body).unwrap();
    }
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-qm", "init"]);
}

fn config(dir: &Path, relay: &str, room: &str) -> Config {
    Config {
        data_dir: dir.to_path_buf(),
        relay_url: Some(relay.to_string()),
        token: None,
        github_token: None,
        room: room.to_string(),
        key: Some("serve-test-passphrase".into()),
    }
}

struct Harness {
    _tmp: tempfile::TempDir,
    desktop: Config,
    handle: ServeHandle,
    base: String,
    token: String,
    client: reqwest::Client,
    agent_id: Uuid,
    ntfy: Pushes,
}

/// A desktop that owns the workspace (founder) with one agent `@coder` on a
/// Custom runtime at `endpoint` / `model`, synced through the relay; then a
/// serve host joined to it with a repo bound.
async fn harness(endpoint: &str, model: &str, repo_files: &[(&str, &str)]) -> Harness {
    harness_with(endpoint, model, repo_files, |_| {}).await
}

async fn harness_with(
    endpoint: &str,
    model: &str,
    repo_files: &[(&str, &str)],
    tweak: impl FnOnce(&mut ServeOptions),
) -> Harness {
    let tmp = tempfile::tempdir().unwrap();
    let relay = spawn(test_relay::router()).await;
    let room = format!("serve-test-{}", Uuid::new_v4());
    let ws = crate::uuid_of_room(&room);

    let desktop = config(&tmp.path().join("desktop"), &relay, &room);
    let agent_id = {
        let mut svc = open_service(&desktop).unwrap();
        svc.ensure_self_member(workspace_config_session_id(ws), ws).unwrap();
        let mut wr = WorkspaceRuntime::new("gw", "Lab gateway", ModelProviderKind::Custom, model);
        wr.endpoint = endpoint.to_string();
        svc.add_workspace_runtime(ws, wr).unwrap();
        let mut agent = WorkspaceAgent::new("coder", "gw");
        agent.owner_actor_id = svc.author().id.clone();
        let id = agent.id;
        svc.add_agent(ws, ws, agent).unwrap();
        id
    };
    sync_once(&desktop).await.unwrap();

    let serve_dir = tmp.path().join("serve");
    let serve_cfg = config(&serve_dir, &relay, &room);
    let repo = tmp.path().join("repo");
    make_repo(&repo, repo_files);
    RepoBindings { workspace: Some(repo.to_string_lossy().into_owned()), ..Default::default() }.save(&serve_dir).unwrap();

    let (ntfy_url, ntfy) = spawn_ntfy().await;
    let dir_for_dashboard = serve_dir.clone();
    let opts = ServeOptions {
        listen: "127.0.0.1:0".parse().unwrap(),
        token_file: serve_dir.join("serve.token"),
        ntfy: Some(ntfy::Ntfy { url: ntfy_url, topic: "hive-test".into(), token: None }),
        public_url: None,
        label: "sff".into(),
        name: Some("Phone".into()),
        pair: false,
        prometheus_url: "http://127.0.0.1:9".into(),
        prometheus_configured: false,
        dashboard_file: dir_for_dashboard.join("dashboard.toml"),
        default_agent: None,
        apply_on_approve: false,
        sync_every: Duration::from_millis(300),
        remote: None,
        appearance: Default::default(),
    };
    let mut opts = opts;
    tweak(&mut opts);
    let handle = start(serve_cfg, opts).await.unwrap();
    let base = format!("http://{}", handle.addr);
    let token = handle.shared.token();
    Harness { _tmp: tmp, desktop, handle, base, token, client: reqwest::Client::new(), agent_id, ntfy }
}

impl Harness {
    async fn get(&self, path: &str) -> Value {
        let r = self.client.get(format!("{}{path}", self.base)).bearer_auth(&self.token).send().await.unwrap();
        assert!(r.status().is_success(), "GET {path}: {}", r.status());
        r.json().await.unwrap()
    }

    async fn send(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Value {
        let mut req = self.client.request(method.clone(), format!("{}{path}", self.base)).bearer_auth(&self.token);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let r = req.send().await.unwrap();
        let status = r.status();
        let v: Value = r.json().await.unwrap_or(Value::Null);
        assert!(status.is_success(), "{method} {path}: {status} {v}");
        v
    }

    /// Open the SSE stream; events arrive as (name, data, id) on the channel.
    async fn events(&self, last_id: Option<u64>) -> mpsc::UnboundedReceiver<(String, Value, u64)> {
        let mut req = self.client.get(format!("{}/v1/events", self.base)).bearer_auth(&self.token);
        if let Some(id) = last_id {
            req = req.header("Last-Event-ID", id.to_string());
        }
        let resp = req.send().await.unwrap();
        assert!(resp.status().is_success());
        assert!(resp.headers()["content-type"].to_str().unwrap().starts_with("text/event-stream"));
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let mut buf = String::new();
            let mut stream = resp.bytes_stream();
            while let Some(Ok(chunk)) = stream.next().await {
                buf.push_str(&String::from_utf8_lossy(&chunk));
                while let Some(end) = buf.find("\n\n") {
                    let frame: String = buf.drain(..end + 2).collect();
                    let (mut name, mut data, mut id) = (String::new(), String::new(), 0u64);
                    for line in frame.lines() {
                        if let Some(v) = line.strip_prefix("event:") {
                            name = v.trim().to_string();
                        } else if let Some(v) = line.strip_prefix("data:") {
                            data.push_str(v.trim_start());
                        } else if let Some(v) = line.strip_prefix("id:") {
                            id = v.trim().parse().unwrap_or(0);
                        }
                    }
                    if !name.is_empty() && tx.send((name, serde_json::from_str(&data).unwrap_or(Value::Null), id)).is_err() {
                        return;
                    }
                }
            }
        });
        rx
    }
}

/// Collect events until `done` says so (or time runs out).
async fn collect_until(
    rx: &mut mpsc::UnboundedReceiver<(String, Value, u64)>,
    secs: u64,
    mut done: impl FnMut(&[(String, Value, u64)]) -> bool,
) -> Vec<(String, Value, u64)> {
    let mut seen = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    while !done(&seen) {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Some(ev)) => seen.push(ev),
            _ => break,
        }
    }
    seen
}

async fn this_host(h: &Harness) -> String {
    let hosts = h.get("/v1/hosts").await;
    let me = hosts["hosts"].as_array().unwrap().iter().find(|x| x["isThis"] == json!(true)).expect("this host listed");
    assert_eq!(me["label"], "sff");
    assert_eq!(me["online"], json!(true));
    me["id"].as_str().unwrap().to_string()
}

// ── Tests ──────────────────────────────────────────────────────────────────

/// The phone's pairing check calls `GET /v1/agents`; a box whose workspace has
/// no agents or config yet must answer with empty lists, not 404.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn empty_workspace_lists_are_empty_not_404() {
    let tmp = tempfile::tempdir().unwrap();
    let relay = spawn(test_relay::router()).await;
    let room = format!("serve-empty-{}", Uuid::new_v4());
    let dir = tmp.path().join("serve");
    let dir_for_dashboard = dir.clone();
    let opts = ServeOptions {
        listen: "127.0.0.1:0".parse().unwrap(),
        token_file: dir.join("serve.token"),
        ntfy: None,
        public_url: None,
        label: "sff".into(),
        name: Some("Phone".into()),
        pair: false,
        prometheus_url: "http://127.0.0.1:9".into(),
        prometheus_configured: false,
        dashboard_file: dir_for_dashboard.join("dashboard.toml"),
        default_agent: None,
        apply_on_approve: false,
        sync_every: Duration::from_millis(300),
        remote: None,
        appearance: Default::default(),
    };
    let handle = start(config(&dir, &relay, &room), opts).await.unwrap();
    let base = format!("http://{}", handle.addr);
    let client = reqwest::Client::new();
    for (path, key) in [("/v1/agents", "agents"), ("/v1/hosts", "hosts"), ("/v1/chats", "chats"), ("/v1/proposals", "proposals")] {
        let r = client.get(format!("{base}{path}")).bearer_auth(handle.shared.token()).send().await.unwrap();
        assert_eq!(r.status(), 200, "{path}");
        let v: Value = r.json().await.unwrap();
        assert!(v[key].is_array(), "{path}: {v}");
    }
    assert_eq!(
        client
            .get(format!("{base}/v1/agents"))
            .bearer_auth(handle.shared.token())
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap()["agents"],
        json!([])
    );
    handle.shutdown();
}

/// People, channels and chat details: avatars and host kinds ride the roster,
/// chats carry their channel and recent speakers, and details mirror the
/// desktop's right-hand panel (agents, people, default responder, context,
/// workflow runs, skills, vaults).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn people_channels_and_chat_details() {
    let (endpoint, _model) = spawn_model(vec![vec![delta(json!({"content": "hi"}))]]).await;
    let h = harness(&endpoint, "gpt-4o", &[("README.md", "# demo\n")]).await;
    let ws = crate::uuid_of_room(&h.handle.shared.cfg.room);

    // The desktop files a channel, gives @coder a picture, loads a skill and
    // runs a workflow in the channel's chat.
    let avatar = "data:image/png;base64,iVBORw0KGgo=";
    let (channel_id, chat_id) = {
        let mut svc = open_service(&h.desktop).unwrap();
        let (ch, chat) = svc.create_channel(ws, "design", Some("UI and UX".into()), "gw").unwrap();
        svc.set_agent_avatar(chat.id, ws, h.agent_id, Some(avatar.into()), Some("#336699".into())).unwrap();
        let mut skill = hive_core::SkillProfile::new("Reviewer", "# Review code\nLook for bugs.");
        skill.source_url = Some("https://example.com/skill.md".into());
        svc.add_skill(chat.id, ws, skill).unwrap();
        let def = hive_core::WorkflowDefinition {
            id: Uuid::new_v4(),
            name: "Ship it".into(),
            description: String::new(),
            input_label: None,
            nodes: vec![hive_core::workflow::WorkflowNode {
                id: "draft".into(),
                name: "Draft".into(),
                depends_on: vec![],
                kind: hive_core::WorkflowNodeKind::Agent { agent_id: None, prompt_template: "go".into() },
                x: None,
                y: None,
            }],
            created_at: hive_core::Timestamp::now(),
        };
        let me = svc.author().id.clone();
        svc.upsert_workflow_run(chat.id, ws, hive_core::workflow::new_run(&def, "now", me)).unwrap();
        svc.post_user_message(chat.id, ws, "kicking off the design pass").unwrap();
        (ch.id, chat.id)
    };
    sync_once(&h.desktop).await.unwrap();

    // The serve box syncs on its own loop; wait for the channel to arrive.
    let mut channels = Value::Null;
    for _ in 0..50 {
        channels = h.get("/v1/channels").await;
        if channels["channels"].as_array().is_some_and(|c| c.iter().any(|c| c["id"] == json!(channel_id))) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let design = channels["channels"].as_array().unwrap().iter().find(|c| c["id"] == json!(channel_id)).expect("channel synced");
    assert_eq!(design["name"], "design");
    assert_eq!(design["purpose"], "UI and UX");
    assert_eq!(design["archived"], json!(false));
    assert_eq!(design["defaultChatId"], json!(chat_id.to_string()));

    // Wait for the avatar edit and the message too.
    let mut chat = Value::Null;
    for _ in 0..50 {
        let chats = h.get("/v1/chats").await;
        chat = chats["chats"].as_array().unwrap().iter().find(|c| c["id"] == json!(chat_id)).cloned().unwrap_or(Value::Null);
        if chat["participants"].as_array().is_some_and(|p| !p.is_empty()) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert_eq!(chat["channelId"], json!(channel_id));
    let speaker = &chat["participants"][0];
    assert_eq!(speaker["kind"], "person");
    assert!(speaker.get("avatarUrl").is_none(), "chat rows never carry avatars");
    let desktop_actor = speaker["id"].as_str().unwrap().to_string();

    // Messages name a person's actor.
    let msgs = h.get(&format!("/v1/chats/{chat_id}/messages")).await;
    let m = msgs["messages"].as_array().unwrap().iter().find(|m| m["role"] == "user").unwrap();
    assert_eq!(m["actorId"], json!(desktop_actor));

    // Agents carry the avatar, color and where they run.
    let agents = h.get("/v1/agents").await;
    let coder = agents["agents"].as_array().unwrap().iter().find(|a| a["name"] == "coder").unwrap().clone();
    assert_eq!(coder["avatarUrl"], avatar);
    assert_eq!(coder["avatarColorHex"], "#336699");
    assert_eq!(coder["hostKind"], "ownerDevice");
    let host = this_host(&h).await;
    let moved = h
        .send(reqwest::Method::PUT, &format!("/v1/agents/{}/host", h.agent_id), Some(json!({ "hostId": host })))
        .await;
    assert_eq!(moved["agent"]["hostKind"], "thisHost");
    assert_eq!(moved["agent"]["hostOnline"], json!(true));

    // People: the desktop founder (with role) and this box.
    let people = h.get("/v1/people").await;
    let list = people["people"].as_array().unwrap();
    let founder = list.iter().find(|p| p["actorId"] == json!(desktop_actor)).expect("founder listed");
    assert_eq!(founder["role"], "owner");
    assert_eq!(founder["isSelf"], json!(false));

    // Details.
    let d = h.get(&format!("/v1/chats/{chat_id}/details")).await;
    assert_eq!(d["chat"]["id"], json!(chat_id));
    assert_eq!(d["channel"]["name"], "design");
    let coder = d["agents"].as_array().unwrap().iter().find(|a| a["name"] == "coder").unwrap();
    assert_eq!(coder["inChat"], json!(false));
    assert_eq!(coder["avatarUrl"], avatar);
    let founder = d["people"].as_array().unwrap().iter().find(|p| p["actorId"] == json!(desktop_actor)).unwrap();
    assert_eq!(founder["inChat"], json!(true));
    assert_eq!(d["defaultResponder"]["kind"], "primary");
    assert_eq!(d["defaultResponder"]["name"], "hive");
    assert_eq!(d["defaultResponder"]["runtimeLabel"], "Lab gateway");
    assert_eq!(d["defaultResponder"]["model"], "gpt-4o");
    assert_eq!(d["context"]["estimated"], json!(true));
    assert_eq!(d["context"]["model"], "gpt-4o");
    assert!(d["context"]["windowTokens"].as_u64().unwrap() >= 8192);
    assert!(d["context"]["usedTokens"].as_u64().unwrap() > 0);
    assert!(d["context"]["skillTokens"].as_u64().unwrap() > 0);
    let run = &d["workflowRuns"][0];
    assert_eq!(run["name"], "Ship it");
    assert_eq!(run["status"], "running");
    assert_eq!(run["stepsDone"], json!(0));
    assert_eq!(run["stepsTotal"], json!(1));
    let skill = &d["skills"][0];
    assert_eq!(skill["name"], "Reviewer");
    assert_eq!(skill["summary"], "Review code");
    assert_eq!(skill["sourceUrl"], "https://example.com/skill.md");
    assert_eq!(d["vaults"], json!([]));

    // An unknown chat is a 404.
    let r = h.client.get(format!("{}/v1/chats/{}/details", h.base, Uuid::new_v4())).bearer_auth(&h.token).send().await.unwrap();
    assert_eq!(r.status(), 404);

    // A phone-made chat lands in the first channel (sidebar order).
    let made = h.send(reqwest::Method::POST, "/v1/chats", Some(json!({ "title": "From the phone" }))).await;
    assert_eq!(made["chat"]["channelId"], json!(channel_id));
    h.handle.shutdown();
}

/// The phone's URLSession only returns the response once body bytes arrive, so
/// a quiet event stream must send something at once — not wait for the 15 s
/// heartbeat (the app showed "Connecting" that long after every connect).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn event_stream_sends_bytes_immediately() {
    let (endpoint, _) = spawn_model(vec![vec![delta(json!({"content": "hi"}))]]).await;
    let h = harness(&endpoint, "m", &[("README.md", "x\n")]).await;
    let resp = h.client.get(format!("{}/v1/events", h.base)).bearer_auth(&h.token).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let mut body = resp.bytes_stream();
    let first = tokio::time::timeout(Duration::from_secs(2), body.next())
        .await
        .expect("a first frame within 2 s")
        .expect("stream open")
        .unwrap();
    assert!(String::from_utf8_lossy(&first).starts_with(':'), "an SSE comment: {first:?}");
    h.handle.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn auth_is_required_except_for_bare_health() {
    let (endpoint, _) = spawn_model(vec![vec![delta(json!({"content": "hi"}))]]).await;
    let h = harness(&endpoint, "m", &[("README.md", "x\n")]).await;

    let anon: Value = h.client.get(format!("{}/v1/health", h.base)).send().await.unwrap().json().await.unwrap();
    assert_eq!(anon["ok"], json!(true));
    assert!(anon["version"].is_string());
    assert!(anon.get("workspace").is_none() && anon.get("deviceLabel").is_none(), "{anon}");

    let authed = h.get("/v1/health").await;
    assert!(authed["workspace"].as_str().unwrap().starts_with("serve-test-"));
    assert_eq!(authed["deviceLabel"], "sff");

    for path in ["/v1/chats", "/v1/agents", "/v1/hosts", "/v1/proposals", "/v1/lab/status", "/v1/dashboard", "/v1/appearance", "/v1/events"] {
        let none = h.client.get(format!("{}{path}", h.base)).send().await.unwrap();
        assert_eq!(none.status(), 401, "{path} without a token");
        let wrong = h.client.get(format!("{}{path}", h.base)).bearer_auth("nope").send().await.unwrap();
        assert_eq!(wrong.status(), 401, "{path} with a wrong token");
    }
    let post = h.client.post(format!("{}/v1/chats", h.base)).json(&json!({})).send().await.unwrap();
    assert_eq!(post.status(), 401);

    // Lab status is best-effort: an unreachable Prometheus is an empty snapshot.
    let lab = h.get("/v1/lab/status").await;
    assert_eq!(lab["workers"], json!([]));
    assert!(lab["updatedAt"].is_string());
    let r = h.client.get(format!("{}/v1/lab/status", h.base)).bearer_auth(&h.token).send().await.unwrap();
    assert_eq!(r.headers()["deprecation"], "true", "lab status is deprecated");

    // No dashboard.toml and no HIVE_PROMETHEUS_URL: an empty dashboard (the
    // app hides its tab).
    let d = h.get("/v1/dashboard").await;
    assert_eq!(d["sections"], json!([]), "{d}");
    assert_eq!(d["title"], "Dashboard");
    assert_eq!(d["source"], "none");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dashboard_serves_the_hosts_config_with_per_panel_errors() {
    use axum::routing::get;
    let json_src = spawn(Router::new().route("/status", get(|| async { axum::Json(json!({"gpu": {"temp": 61.5}, "state": "ok"})) }))).await;
    let (endpoint, _model) = spawn_model(vec![vec![delta(json!({"content": "hi"}))]]).await;
    let dashboard_toml = format!(
        r#"
title = "Home"
[[section]]
title = "Box"
[[section.panel]]
id = "gpu-temp"
title = "GPU temp"
kind = "gauge"
unit = "°C"
url = "{json_src}/status"
path = "/gpu/temp"
max = 110
thresholds = [{{ value = 0, level = "ok" }}, {{ value = 80, level = "critical" }}]
[[section.panel]]
title = "Gateway"
kind = "status"
url = "{json_src}/status"
path = "/state"
[[section.panel]]
title = "Power"
query = "sum(power_watts)"
"#
    );
    let h = harness_with(&endpoint, "gemma4:12b", &[("README.md", "# demo\n")], |o| {
        std::fs::create_dir_all(o.dashboard_file.parent().unwrap()).unwrap();
        std::fs::write(&o.dashboard_file, &dashboard_toml).unwrap();
    })
    .await;
    let d = h.get("/v1/dashboard").await;
    assert_eq!(d["title"], "Home");
    assert_eq!(d["source"], "config");
    let panels = d["sections"][0]["panels"].as_array().unwrap();
    assert_eq!(panels[0]["id"], "gpu-temp");
    assert_eq!(panels[0]["value"], json!(61.5));
    assert_eq!(panels[0]["max"], json!(110.0));
    assert!(panels[0].get("error").is_none(), "{}", panels[0]);
    assert_eq!(panels[1]["rows"], json!([{"label": "Gateway", "up": true, "text": "ok"}]));
    // Prometheus at 127.0.0.1:9 is unreachable: that panel errors, the rest stand.
    assert!(panels[2]["error"].as_str().unwrap().contains("Prometheus"), "{}", panels[2]);
    assert!(panels[2].get("value").is_none());
}

/// "Match host" on the phone: `/v1/appearance` reports the desktop app's saved
/// theme on this machine, live (no restart), and serve config pins a field.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn appearance_follows_the_desktop_file_and_config() {
    let (endpoint, _) = spawn_model(vec![vec![delta(json!({"content": "hi"}))]]).await;
    let desk = tempfile::tempdir().unwrap();
    let dir = desk.path().to_path_buf();
    let h = harness_with(&endpoint, "m", &[("README.md", "x\n")], |o| {
        o.appearance = appearance::AppearanceConfig { desktop_dir: Some(dir), ..Default::default() };
    })
    .await;
    assert_eq!(h.get("/v1/appearance").await, json!({"palette": null, "mode": null, "source": "none"}));

    std::fs::write(desk.path().join(appearance::FILE_NAME), r#"{"palette":"harbor","mode":"dark"}"#).unwrap();
    assert_eq!(h.get("/v1/appearance").await, json!({"palette": "harbor", "mode": "dark", "source": "desktop"}));
    h.handle.shutdown();

    let pinned = harness_with(&endpoint, "m", &[("README.md", "x\n")], |o| {
        o.appearance = appearance::AppearanceConfig { palette: Some("obsidian".into()), mode: None, desktop_dir: None };
    })
    .await;
    assert_eq!(pinned.get("/v1/appearance").await, json!({"palette": "obsidian", "mode": null, "source": "config"}));
    pinned.handle.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn phone_message_to_hosted_agent_streams_reply_and_files_a_proposal() {
    let mut rounds = vec![streamed_call("call_1", "write_file", r#"{"path":"hello.txt","content":"hello from the phone\n"}"#)];
    rounds.push(vec![delta(json!({"content": "Done: "})), delta(json!({"content": "added hello.txt"}))]);
    let (endpoint, model) = spawn_model(rounds).await;
    let h = harness(&endpoint, "gemma4:12b", &[("README.md", "# demo\n")]).await;

    // The desktop's agent is visible, on its owner's device until sent here.
    let agents = h.get("/v1/agents").await;
    let coder = agents["agents"].as_array().unwrap().iter().find(|a| a["name"] == "coder").expect("coder listed").clone();
    assert_eq!(coder["id"], json!(h.agent_id));
    assert_eq!(coder["runtimeLabel"], "Lab gateway");
    assert_eq!(coder["model"], "gemma4:12b");
    assert!(coder.get("hostId").is_none());

    let host = this_host(&h).await;
    let moved = h
        .send(reqwest::Method::PUT, &format!("/v1/agents/{}/host", h.agent_id), Some(json!({ "hostId": host })))
        .await;
    assert_eq!(moved["agent"]["hostId"], json!(host));
    assert_eq!(moved["agent"]["hostLabel"], "sff (this host)");
    // An unknown host is refused.
    let bad = h
        .client
        .put(format!("{}/v1/agents/{}/host", h.base, h.agent_id))
        .bearer_auth(&h.token)
        .json(&json!({ "hostId": "not-a-host" }))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 400);

    let chat = h.send(reqwest::Method::POST, "/v1/chats", Some(json!({ "title": "From the phone" }))).await;
    let chat_id = chat["chat"]["id"].as_str().unwrap().to_string();
    assert_eq!(chat["chat"]["title"], "From the phone");

    let mut rx = h.events(None).await;
    let posted = h
        .send(
            reqwest::Method::POST,
            &format!("/v1/chats/{chat_id}/messages"),
            Some(json!({ "body": "@coder please add a hello file" })),
        )
        .await;
    assert_eq!(posted["message"]["role"], "user");
    assert_eq!(posted["message"]["author"], "Phone");
    assert_eq!(posted["message"]["chatId"], json!(chat_id));

    let seen = collect_until(&mut rx, 30, |evs| {
        evs.iter().any(|(n, _, _)| n == "proposal.created")
            && evs.iter().any(|(n, d, _)| n == "turn.status" && d["state"] == "done")
            && evs.iter().any(|(n, d, _)| n == "message.done" && d["message"]["role"] == "agent")
    })
    .await;
    let names: Vec<&str> = seen.iter().map(|(n, _, _)| n.as_str()).collect();
    let find = |name: &str| seen.iter().find(|(n, _, _)| n == name).map(|(_, d, _)| d.clone());

    let created = seen
        .iter()
        .find(|(n, d, _)| n == "message.created" && d["message"]["role"] == "agent")
        .map(|(_, d, _)| d.clone())
        .unwrap_or_else(|| panic!("agent message.created missing: {names:?}"));
    assert_eq!(created["message"]["streaming"], json!(true));
    assert_eq!(created["message"]["agentId"], json!(h.agent_id));
    let reply_id = created["message"]["id"].clone();

    let deltas: String = seen
        .iter()
        .filter(|(n, d, _)| n == "message.delta" && d["messageId"] == reply_id)
        .map(|(_, d, _)| d["text"].as_str().unwrap_or("").to_string())
        .collect();
    assert!(deltas.contains("added hello.txt"), "deltas: {deltas:?} / {names:?}");
    let delta_ev = find("message.delta").unwrap();
    assert_eq!(delta_ev["chatId"], json!(chat_id));

    let done = seen
        .iter()
        .find(|(n, d, _)| n == "message.done" && d["message"]["id"] == reply_id)
        .map(|(_, d, _)| d.clone())
        .unwrap_or_else(|| panic!("message.done missing: {names:?}"));
    assert_eq!(done["message"]["streaming"], json!(false));
    assert_eq!(done["message"]["body"], "Done: added hello.txt");
    assert!(seen.iter().any(|(n, d, _)| n == "turn.status" && d["state"] == "running" && d["agentId"] == json!(h.agent_id)));
    assert!(names.contains(&"chat.updated"));
    // Event ids increase.
    assert!(seen.windows(2).all(|w| w[0].2 < w[1].2));

    let proposal = find("proposal.created").unwrap()["proposal"].clone();
    assert_eq!(proposal["kind"], "fileDiff");
    assert_eq!(proposal["status"], "open");
    assert_eq!(proposal["chatId"], json!(chat_id));
    assert_eq!(proposal["author"], "@coder");
    assert!(proposal["diff"].as_str().unwrap().contains("+hello from the phone"));

    // The model was offered the built-in tools on the first round.
    {
        let bodies = model.bodies.lock().unwrap();
        let tools: Vec<&str> = bodies[0]["tools"]
            .as_array()
            .expect("tools offered")
            .iter()
            .filter_map(|t| t["function"]["name"].as_str())
            .collect();
        assert!(tools.contains(&"write_file") && tools.contains(&"read_file"), "{tools:?}");
        assert!(!tools.contains(&"run_command"), "run_command is off by default");
    }

    // REST views agree.
    let chats = h.get("/v1/chats").await;
    let listed = chats["chats"].as_array().unwrap().iter().find(|c| c["id"] == json!(chat_id)).unwrap().clone();
    assert_eq!(listed["preview"], "Done: added hello.txt");
    assert_eq!(listed["unread"], json!(1), "the agent's reply is unread until fetched");
    let msgs = h.get(&format!("/v1/chats/{chat_id}/messages")).await;
    let roles: Vec<&str> = msgs["messages"].as_array().unwrap().iter().map(|m| m["role"].as_str().unwrap()).collect();
    assert_eq!(roles, vec!["user", "agent"]);
    let after_user = msgs["messages"][0]["id"].as_str().unwrap();
    let page = h.get(&format!("/v1/chats/{chat_id}/messages?after={after_user}&limit=5")).await;
    assert_eq!(page["messages"].as_array().unwrap().len(), 1);
    let chats = h.get("/v1/chats").await;
    assert_eq!(chats["chats"][0]["unread"], json!(0), "fetching marked it read");

    // The edit lives only in the proposal: the checkout is untouched.
    let open = h.get("/v1/proposals").await;
    assert_eq!(open["proposals"].as_array().unwrap().len(), 1);
    let pid = open["proposals"][0]["id"].as_str().unwrap().to_string();
    let approved = h.send(reqwest::Method::POST, &format!("/v1/proposals/{pid}/approve"), None).await;
    assert_eq!(approved["proposal"]["status"], "approved");
    assert!(h.get("/v1/proposals?status=open").await["proposals"].as_array().unwrap().is_empty());
    assert_eq!(h.get("/v1/proposals?status=all").await["proposals"][0]["status"], "approved");

    // ntfy heard about the reply and the proposal, with a deep link and no content.
    tokio::time::sleep(Duration::from_millis(500)).await;
    {
        let pushes = h.ntfy.lock().unwrap();
        assert!(pushes.len() >= 2, "ntfy pushes: {}", pushes.len());
        for (headers, body) in pushes.iter() {
            assert_eq!(headers["click"].to_str().unwrap(), format!("hive://chat/{chat_id}"));
            assert!(!body.contains("hello from the phone"));
        }
        assert!(pushes.iter().any(|(hd, _)| hd["title"].to_str().unwrap() == "coder replied"));
        assert!(pushes.iter().any(|(hd, _)| hd["title"].to_str().unwrap() == "Change ready for review"));
    }

    // The desktop sees the phone's chat, the agent's reply and the approval
    // through the relay (E2EE, verified on ingest).
    let chat_uuid = Uuid::parse_str(&chat_id).unwrap();
    let mut synced = None;
    for _ in 0..40 {
        sync_once(&h.desktop).await.unwrap();
        let svc = open_service(&h.desktop).unwrap();
        if let Some(s) = svc.load(chat_uuid).unwrap() {
            let replied = s.messages.iter().any(|m| m.body == "Done: added hello.txt" && !m.is_streaming);
            let approved = s.proposals.iter().any(|p| p.status == hive_core::ProposalStatus::Approved);
            if replied && approved {
                synced = Some(s);
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    assert!(synced.is_some(), "desktop never saw the reply + approval");
    h.handle.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reject_with_reason_and_last_event_id_replay() {
    let (endpoint, _) = spawn_model(vec![vec![delta(json!({"content": "hi"}))]]).await;
    let h = harness(&endpoint, "m", &[("README.md", "x\n")]).await;
    let chat = h.send(reqwest::Method::POST, "/v1/chats", None).await;
    let chat_id = Uuid::parse_str(chat["chat"]["id"].as_str().unwrap()).unwrap();
    assert_eq!(chat["chat"]["title"], "Untitled");

    // A proposal filed the way the worker files one.
    let pid = {
        let mut svc = h.handle.shared.lock();
        let ws = h.handle.shared.ws;
        let p = hive_core::ActionProposal::file_diff("@coder proposed changes to 1 file", "", "diff --git a/x b/x\n".into(), vec!["x".into()]);
        let id = p.id;
        svc.upsert_proposal(chat_id, ws, p).unwrap();
        id
    };
    h.handle.shared.after_write();

    let rejected = h
        .send(reqwest::Method::POST, &format!("/v1/proposals/{pid}/reject"), Some(json!({ "reason": "not like this" })))
        .await;
    assert_eq!(rejected["proposal"]["status"], "rejected");
    {
        let svc = h.handle.shared.lock();
        let s = svc.load(chat_id).unwrap().unwrap();
        let p = s.proposals.iter().find(|p| p.id == pid).unwrap();
        assert_eq!(p.refinements.len(), 1);
        assert_eq!(p.refinements[0].text, "not like this");
    }
    let missing = h
        .client
        .post(format!("{}/v1/proposals/{}/approve", h.base, Uuid::new_v4()))
        .bearer_auth(&h.token)
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), 404);

    // A reconnecting client replays what it missed after Last-Event-ID 0.
    let mut rx = h.events(Some(0)).await;
    let seen = collect_until(&mut rx, 5, |evs| evs.iter().any(|(n, _, _)| n == "proposal.created")).await;
    let names: Vec<&str> = seen.iter().map(|(n, _, _)| n.as_str()).collect();
    assert!(names.contains(&"chat.updated"), "{names:?}");
    let p = seen.iter().find(|(n, _, _)| n == "proposal.created").expect("replayed proposal.created");
    assert_eq!(p.1["proposal"]["id"], json!(pid));
    assert!(seen[0].2 >= 1);
    h.handle.shutdown();
}

/// File a proposal on a fresh chat the way the worker does (authorless, from
/// `@coder`), returning (chat, proposal).
async fn filed_proposal(h: &Harness, producing: Option<Uuid>) -> (Uuid, Uuid) {
    let chat = h.send(reqwest::Method::POST, "/v1/chats", None).await;
    let chat_id = Uuid::parse_str(chat["chat"]["id"].as_str().unwrap()).unwrap();
    let pid = {
        let mut svc = h.handle.shared.lock();
        let mut p = hive_core::ActionProposal::file_diff(
            "@coder proposed changes to 1 file",
            "",
            "diff --git a/x b/x\n".into(),
            vec!["x".into()],
        );
        p.producing_agent_id = producing;
        let id = p.id;
        svc.upsert_proposal(chat_id, h.handle.shared.ws, p).unwrap();
        id
    };
    h.handle.shared.after_write();
    (chat_id, pid)
}

async fn status_of(h: &Harness, method: reqwest::Method, path: &str, body: Option<Value>) -> (u16, Value) {
    let mut req = h.client.request(method, format!("{}{path}", h.base)).bearer_auth(&h.token);
    if let Some(b) = body {
        req = req.json(&b);
    }
    let r = req.send().await.unwrap();
    let code = r.status().as_u16();
    (code, r.json().await.unwrap_or(Value::Null))
}

/// The desktop's Review card, from the phone: comments and change requests
/// land on the proposal's refinement thread, a vote is tagged with the voter's
/// name and role, and each change reaches the stream as `proposal.updated`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn comments_and_votes_show_who_and_stream_updates() {
    let (endpoint, _) = spawn_model(vec![vec![delta(json!({"content": "hi"}))]]).await;
    let h = harness(&endpoint, "m", &[("README.md", "x\n")]).await;
    let (_, pid) = filed_proposal(&h, Some(h.agent_id)).await;
    let mut rx = h.events(None).await;

    let listed = h.get("/v1/proposals").await["proposals"][0].clone();
    assert_eq!(listed["commentCount"], 0);
    assert_eq!(listed["requiredApprovals"], 1);
    assert_eq!(listed["qualifyingApprovals"], 0);
    assert_eq!(listed["votes"], json!([]));
    assert_eq!(listed["myVote"], Value::Null);
    assert_eq!(listed["canVote"], true);

    let empty = status_of(&h, reqwest::Method::POST, &format!("/v1/proposals/{pid}/comments"), Some(json!({ "text": "  " }))).await;
    assert_eq!(empty.0, 400);

    let c = h
        .send(reqwest::Method::POST, &format!("/v1/proposals/{pid}/comments"), Some(json!({ "text": "Looks close" })))
        .await;
    assert_eq!(c["comment"]["text"], "Looks close");
    assert_eq!(c["comment"]["requestChanges"], false);
    assert_eq!(c["comment"]["author"], "Phone");
    assert_eq!(c["proposal"]["commentCount"], 1);
    let cr = h
        .send(
            reqwest::Method::POST,
            &format!("/v1/proposals/{pid}/comments"),
            Some(json!({ "text": "Rename the helper", "requestChanges": true })),
        )
        .await;
    assert_eq!(cr["proposal"]["changeRequestCount"], 1);
    let thread = h.get(&format!("/v1/proposals/{pid}/comments")).await;
    let texts: Vec<&str> = thread["comments"].as_array().unwrap().iter().map(|c| c["text"].as_str().unwrap()).collect();
    assert_eq!(texts, vec!["Looks close", "Rename the helper"]);

    let approved = h.send(reqwest::Method::POST, &format!("/v1/proposals/{pid}/approve"), None).await;
    let p = &approved["proposal"];
    assert_eq!(p["status"], "approved");
    assert_eq!(p["myVote"], "approve");
    assert_eq!(p["qualifyingApprovals"], 1);
    assert_eq!(p["quorumMet"], true);
    assert_eq!(p["votes"][0]["voter"], "Phone");
    assert_eq!(p["votes"][0]["approved"], true);
    assert!(["owner", "admin", "contributor"].contains(&p["votes"][0]["role"].as_str().unwrap()), "{p}");
    // A later reject replaces this voter's vote (latest wins), as on the desktop.
    let rejected = h.send(reqwest::Method::POST, &format!("/v1/proposals/{pid}/reject"), None).await;
    assert_eq!(rejected["proposal"]["myVote"], "reject");
    assert_eq!(rejected["proposal"]["votes"].as_array().unwrap().len(), 1);

    let one = h.get(&format!("/v1/proposals/{pid}")).await;
    assert_eq!(one["proposal"]["status"], "rejected");
    assert_eq!(one["proposal"]["comments"].as_array().unwrap().len(), 2);

    let seen = collect_until(&mut rx, 5, |evs| {
        evs.iter().filter(|(n, d, _)| n == "proposal.updated" && d["proposal"]["id"] == json!(pid)).count() >= 4
    })
    .await;
    let updates: Vec<&Value> = seen.iter().filter(|(n, _, _)| n == "proposal.updated").map(|(_, d, _)| &d["proposal"]).collect();
    assert!(updates.len() >= 4, "{seen:?}");
    assert_eq!(updates.last().unwrap()["status"], "rejected");

    // And it synced: the desktop sees the thread and the vote.
    sync_once(&h.desktop).await.unwrap();
    let mut desktop_seen = false;
    for _ in 0..30 {
        sync_once(&h.desktop).await.unwrap();
        let svc = open_service(&h.desktop).unwrap();
        let found = svc
            .store()
            .list_session_ids()
            .unwrap()
            .into_iter()
            .filter_map(|id| svc.load(id).ok().flatten())
            .flat_map(|s| s.proposals)
            .find(|p| p.id == pid);
        if found.as_ref().is_some_and(|p| p.refinements.len() == 2 && p.approvals.len() == 1) {
            desktop_seen = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(desktop_seen, "the desktop never saw the phone's notes and vote");
    h.handle.shutdown();
}

/// "Send to agent to revise" from the phone: the change request rides the
/// thread, the instruction is posted to the producing agent as a mention, and
/// the revision that agent files comes back linked to the original.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn request_changes_asks_the_agent_and_links_the_revision() {
    let (endpoint, _) = spawn_model(vec![vec![delta(json!({"content": "hi"}))]]).await;
    let h = harness(&endpoint, "m", &[("README.md", "x\n")]).await;
    let (chat_id, pid) = filed_proposal(&h, Some(h.agent_id)).await;

    // Nothing on the thread yet: nothing to forward.
    let bare = status_of(&h, reqwest::Method::POST, &format!("/v1/proposals/{pid}/request-changes"), None).await;
    assert_eq!(bare.0, 400, "{}", bare.1);

    let res = h
        .send(
            reqwest::Method::POST,
            &format!("/v1/proposals/{pid}/request-changes"),
            Some(json!({ "note": "Use a constant, not a literal" })),
        )
        .await;
    let body = res["message"]["body"].as_str().unwrap();
    assert!(body.starts_with("@coder Please revise your proposal"), "{body}");
    assert!(body.contains("- Use a constant, not a literal"), "{body}");
    let thread: Vec<&str> =
        res["proposal"]["comments"].as_array().unwrap().iter().map(|c| c["text"].as_str().unwrap()).collect();
    assert_eq!(thread, vec!["Use a constant, not a literal", "Sent to the agent to revise."]);
    assert!(res["proposal"]["comments"].as_array().unwrap().iter().all(|c| c["requestChanges"] == true));
    {
        let svc = h.handle.shared.lock();
        let s = svc.load(chat_id).unwrap().unwrap();
        let pending = hive_runtime::chat_service::pending_mentions(&s);
        assert!(pending.iter().any(|m| m.agent_id == h.agent_id), "the revision request is queued work for @coder");
    }

    // The agent's turn files a new version (wherever it ran).
    let mut rx = h.events(None).await;
    let child = {
        let mut svc = h.handle.shared.lock();
        let mut p = hive_core::ActionProposal::file_diff(
            "@coder proposed changes to 1 file",
            "",
            "diff --git a/y b/y\n".into(),
            vec!["y".into()],
        );
        p.producing_agent_id = Some(h.agent_id);
        let id = p.id;
        svc.upsert_proposal(chat_id, h.handle.shared.ws, p).unwrap();
        id
    };
    h.handle.shared.after_write();
    let all = h.get("/v1/proposals?status=all").await;
    let find = |id: Uuid| all["proposals"].as_array().unwrap().iter().find(|p| p["id"] == json!(id)).unwrap().clone();
    assert_eq!(find(child)["parentId"], json!(pid));
    assert_eq!(find(pid)["supersededBy"], json!(child));
    let open: Vec<Value> = h.get("/v1/proposals").await["proposals"].as_array().unwrap().clone();
    assert_eq!(open.len(), 1, "the superseded version left the inbox");
    assert_eq!(open[0]["id"], json!(child));
    let seen = collect_until(&mut rx, 5, |evs| {
        evs.iter().any(|(n, d, _)| n == "proposal.updated" && d["proposal"]["supersededBy"] == json!(child))
    })
    .await;
    assert!(seen.iter().any(|(n, _, _)| n == "proposal.created"), "{seen:?}");

    // A revised proposal can't be sent back again.
    let again = status_of(&h, reqwest::Method::POST, &format!("/v1/proposals/{pid}/request-changes"), Some(json!({ "note": "x" }))).await;
    assert_eq!(again.0, 409);

    // One from the chat's primary runtime has no agent the phone can address.
    let (_, primary) = filed_proposal(&h, None).await;
    let res = status_of(&h, reqwest::Method::POST, &format!("/v1/proposals/{primary}/request-changes"), Some(json!({ "note": "x" }))).await;
    assert_eq!(res.0, 409, "{}", res.1);
    h.handle.shutdown();
}

/// Role gates hold exactly as on the desktop: a viewer can't vote, comment or
/// send a proposal back — the core's authz refuses the event and the phone
/// gets 403 — and the proposal JSON says so up front (`canVote: false`).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_viewer_cannot_vote_comment_or_request_changes() {
    let (endpoint, _) = spawn_model(vec![vec![delta(json!({"content": "hi"}))]]).await;
    let h = harness(&endpoint, "m", &[("README.md", "x\n")]).await;
    let (chat_id, pid) = filed_proposal(&h, Some(h.agent_id)).await;
    let me = h.handle.shared.my_actor_id();
    let ws = h.handle.shared.ws;

    // Wait for the box's own membership to reach the desktop, then the owner
    // demotes it to viewer.
    let mut demoted = false;
    for _ in 0..50 {
        sync_once(&h.desktop).await.unwrap();
        let mut svc = open_service(&h.desktop).unwrap();
        let cfg = workspace_config_session_id(ws);
        let known = svc.load(cfg).unwrap().is_some_and(|s| s.members.iter().any(|m| m.id == me));
        if known {
            svc.set_member_role(cfg, ws, me.clone(), hive_core::WorkspaceRole::Viewer).unwrap();
            drop(svc);
            sync_once(&h.desktop).await.unwrap();
            demoted = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(demoted, "the box never joined the desktop's roster");
    let mut is_viewer = false;
    for _ in 0..50 {
        let s = h.handle.shared.lock().load(chat_id).unwrap().unwrap();
        if s.role_of(&me) == hive_core::WorkspaceRole::Viewer {
            is_viewer = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(is_viewer, "the demotion never synced to the box");

    let p = h.get(&format!("/v1/proposals/{pid}")).await;
    assert_eq!(p["proposal"]["canVote"], false);
    assert_eq!(p["proposal"]["myRole"], "viewer");
    for (path, body) in [
        (format!("/v1/proposals/{pid}/approve"), None),
        (format!("/v1/proposals/{pid}/reject"), None),
        (format!("/v1/proposals/{pid}/comments"), Some(json!({ "text": "hi" }))),
        (format!("/v1/proposals/{pid}/request-changes"), Some(json!({ "note": "redo" }))),
    ] {
        let (code, v) = status_of(&h, reqwest::Method::POST, &path, body).await;
        assert_eq!(code, 403, "{path}: {v}");
    }
    let s = h.handle.shared.lock().load(chat_id).unwrap().unwrap();
    let p = s.proposals.iter().find(|p| p.id == pid).unwrap();
    assert!(p.approvals.is_empty() && p.refinements.is_empty(), "a denied write left a trace");
    assert_eq!(p.status, hive_core::ProposalStatus::Open);
    drop(s);
    h.handle.shutdown();
}

/// Approving from the phone must not apply a proposal a qualifying reviewer
/// already rejected: quorum stays met after a reject, but the folded status is
/// Rejected, and Applied would be terminal on every peer.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn apply_on_approve_never_overrides_a_rejection() {
    let (endpoint, _) = spawn_model(vec![vec![delta(json!({"content": "hi"}))]]).await;
    let h = harness_with(&endpoint, "m", &[("README.md", "x\n")], |o| o.apply_on_approve = true).await;
    let chat = h.send(reqwest::Method::POST, "/v1/chats", None).await;
    let chat_id = Uuid::parse_str(chat["chat"]["id"].as_str().unwrap()).unwrap();
    let ws = h.handle.shared.ws;
    let diff = |name: &str| {
        format!("diff --git a/{name} b/{name}\nnew file mode 100644\n--- /dev/null\n+++ b/{name}\n@@ -0,0 +1 @@\n+hi\n")
    };
    let file = |name: &str| {
        let mut p = hive_core::ActionProposal::file_diff(format!("add {name}"), "", diff(name), vec![name.into()]);
        p.producing_agent_id = Some(h.agent_id);
        p
    };
    let (rejected, control) = (file("rejected.txt"), file("approved.txt"));
    let (rid, cid) = (rejected.id, control.id);
    {
        let mut svc = h.handle.shared.lock();
        svc.upsert_proposal(chat_id, ws, rejected).unwrap();
        svc.upsert_proposal(chat_id, ws, control).unwrap();
        // An Admin rejected it on the desktop (synced in).
        svc.vote_on_proposal(chat_id, ws, rid, "desktop-admin", hive_core::WorkspaceRole::Admin, false).unwrap();
    }
    h.handle.shared.after_write();

    let res = h.send(reqwest::Method::POST, &format!("/v1/proposals/{rid}/approve"), None).await;
    assert_eq!(res["proposal"]["status"], "rejected", "{res}");
    let wt = h._tmp.path().join("repo").join(".hive").join("worktrees").join(format!("chat-{chat_id}"));
    assert!(!wt.join("rejected.txt").exists(), "a rejected proposal was applied");
    {
        let svc = h.handle.shared.lock();
        let s = svc.load(chat_id).unwrap().unwrap();
        let p = s.proposals.iter().find(|p| p.id == rid).unwrap();
        assert_eq!(p.status, hive_core::ProposalStatus::Rejected);
    }

    // Control: the same flow without a rejection applies and commits.
    let res = h.send(reqwest::Method::POST, &format!("/v1/proposals/{cid}/approve"), None).await;
    assert_eq!(res["proposal"]["status"], "applied", "{res}");
    assert!(wt.join("approved.txt").exists(), "the approved proposal was applied in the chat worktree");
    assert!(!wt.join("rejected.txt").exists());
    h.handle.shutdown();
}

/// A turn cut off by a restart (placeholder still streaming, stamped as the
/// mention's reply) is finished as interrupted on the next start instead of
/// staying "streaming" forever and never being answered.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restart_finishes_a_reply_cut_off_mid_stream() {
    let (endpoint, _) = spawn_model(vec![vec![delta(json!({"content": "hi"}))]]).await;
    let h = harness(&endpoint, "m", &[("README.md", "x\n")]).await;
    let chat = h.send(reqwest::Method::POST, "/v1/chats", None).await;
    let chat_id = Uuid::parse_str(chat["chat"]["id"].as_str().unwrap()).unwrap();
    let posted = h.send(reqwest::Method::POST, &format!("/v1/chats/{chat_id}/messages"), Some(json!({ "body": "@coder hi" }))).await;
    let mention = Uuid::parse_str(posted["message"]["id"].as_str().unwrap()).unwrap();
    let ws = h.handle.shared.ws;
    let reply = {
        let mut svc = h.handle.shared.lock();
        let mid = svc
            .begin_assistant_message(chat_id, ws, "coder".to_string(), "gw".to_string(), Some(h.agent_id), Some(mention))
            .unwrap();
        svc.append_chunk(chat_id, ws, mid, "partial answer".to_string()).unwrap();
        mid
    };
    assert!(h.handle.stop(Duration::from_secs(5)).await, "an idle worker stops at once");

    // Same box, same data dir: start again.
    let serve_cfg = config(&h._tmp.path().join("serve"), h.desktop.relay_url.as_deref().unwrap(), &h.desktop.room);
    let mut opts = ServeOptions::from_flags(&serve_cfg, Some("127.0.0.1:0".into()), None, None, None, None, Some("sff".into()), None, false)
        .unwrap();
    opts.prometheus_url = "http://127.0.0.1:9".into();
    let again = start(serve_cfg, opts).await.unwrap();
    {
        let svc = again.shared.lock();
        let s = svc.load(chat_id).unwrap().unwrap();
        let m = s.messages.iter().find(|m| m.id == reply).unwrap();
        assert!(!m.is_streaming, "still streaming after a restart");
        assert!(m.body.starts_with("partial answer"), "{}", m.body);
        assert!(m.body.ends_with(crate::worker::INTERRUPTED_NOTE), "{}", m.body);
    }
    let base = format!("http://{}", again.addr);
    let msgs: Value = h
        .client
        .get(format!("{base}/v1/chats/{chat_id}/messages"))
        .bearer_auth(again.shared.token())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let m = msgs["messages"].as_array().unwrap().iter().find(|m| m["id"] == json!(reply)).unwrap().clone();
    assert_eq!(m["streaming"], json!(false));
    again.shutdown();
}

/// refresh() runs at once from the sync loop, the worker and API handlers. A
/// refresh that loaded its projection before a newer one was diffed must not
/// roll the differ back, or the next refresh re-emits message.done (and a
/// second ntfy push). Deterministic: the test holds the differ lock while a
/// refresh is in flight and advances the differ itself in between.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stale_refresh_never_rolls_the_differ_back() {
    let (endpoint, _) = spawn_model(vec![vec![delta(json!({"content": "hi"}))]]).await;
    let h = harness(&endpoint, "m", &[("README.md", "x\n")]).await;
    let chat = h.send(reqwest::Method::POST, "/v1/chats", None).await;
    let chat_id = Uuid::parse_str(chat["chat"]["id"].as_str().unwrap()).unwrap();
    let shared = h.handle.shared.clone();
    let ws = shared.ws;
    let mid = shared
        .lock()
        .begin_assistant_message(chat_id, ws, "coder".to_string(), "gw".to_string(), Some(h.agent_id), None)
        .unwrap();
    shared.refresh(); // the differ knows the reply is streaming

    let (_, mut rx) = shared.hub.subscribe(None);
    let guard = shared.differ.lock().unwrap();
    // A: a refresh that starts while the reply is still streaming.
    let slow = {
        let shared = shared.clone();
        std::thread::spawn(move || shared.refresh())
    };
    std::thread::sleep(Duration::from_millis(300));
    // The reply completes and a newer refresh diffs it first.
    shared.lock().complete_assistant_message(chat_id, ws, mid, "done").unwrap();
    let mut differ = guard;
    let changes = differ.diff(&shared.workspace_chats().unwrap());
    assert!(changes.contains(&hub::Change::MessageDone { chat: chat_id, message: mid }));
    drop(differ);
    slow.join().unwrap();
    shared.refresh();

    let mut repeats = 0;
    while let Ok(ev) = rx.try_recv() {
        if ev.name == "message.done" && ev.data["message"]["id"] == json!(mid) {
            repeats += 1;
        }
    }
    assert_eq!(repeats, 0, "message.done was emitted again after the differ had seen it");
    h.handle.shutdown();
}

/// A fresh box whose first sync fails must not seed its own workspace config
/// log (naming itself Owner); it refuses to start until the relay answers.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fresh_box_with_unreachable_relay_refuses_to_start() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("serve");
    let room = format!("serve-offline-{}", Uuid::new_v4());
    let cfg = config(&dir, "http://127.0.0.1:9", &room);
    let mut opts = ServeOptions::from_flags(&cfg, Some("127.0.0.1:0".into()), None, None, None, None, Some("sff".into()), None, false)
        .unwrap();
    opts.prometheus_url = "http://127.0.0.1:9".into();
    let err = start(cfg.clone(), opts).await.err().expect("must not start");
    assert!(err.to_string().contains("no copy of the workspace"), "{err}");
    let ws = crate::uuid_of_room(&room);
    assert!(open_service(&cfg).unwrap().load(workspace_config_session_id(ws)).unwrap().is_none(), "no config log seeded");
}

#[test]
fn serve_options_refuse_public_binds_and_need_both_ntfy_parts() {
    let cfg = Config {
        data_dir: PathBuf::from("/tmp/x"),
        relay_url: None,
        token: None,
        github_token: None,
        room: "r".into(),
        key: None,
    };
    let opts = |listen: &str, url: Option<&str>, topic: Option<&str>| {
        ServeOptions::from_flags(
            &cfg,
            Some(listen.into()),
            None,
            url.map(Into::into),
            topic.map(Into::into),
            None,
            Some("sff".into()),
            None,
            false,
        )
    };
    assert!(opts("0.0.0.0:8787", None, None).is_err());
    assert!(opts("100.101.102.104:8787", Some("http://n"), None).is_err());
    let ok = opts("100.101.102.104:8787", Some("http://n:8095"), Some("t")).unwrap();
    assert_eq!(ok.server_url(), "http://100.101.102.104:8787");
    assert_eq!(ok.token_file, PathBuf::from("/tmp/x/serve.token"));
    assert_eq!(ok.ntfy.unwrap().endpoint(), "http://n:8095/t");
}

/// Live: one agent turn end to end against the lab gateway with a built-in
/// tool call (`read_file` of a planted note). Skipped unless
/// `HIVE_SERVE_LIVE_ENDPOINT` is set.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_gateway_turn_reads_a_file() {
    let Some(endpoint) = std::env::var("HIVE_SERVE_LIVE_ENDPOINT").ok().filter(|s| !s.trim().is_empty()) else {
        eprintln!("skipping: HIVE_SERVE_LIVE_ENDPOINT unset");
        return;
    };
    let models = std::env::var("HIVE_SERVE_LIVE_MODELS").unwrap_or_else(|_| "gpt-oss:20b,gemma4:12b".into());
    for model in models.split(',').map(str::trim).filter(|m| !m.is_empty()) {
        let word = format!("lantern{}", &Uuid::new_v4().simple().to_string()[..6]);
        let note = format!("The secret word is {word}.\n");
        let h = harness(&endpoint, model, &[("NOTE.txt", note.as_str()), ("README.md", "# notes\n")]).await;
        let host = this_host(&h).await;
        h.send(reqwest::Method::PUT, &format!("/v1/agents/{}/host", h.agent_id), Some(json!({ "hostId": host }))).await;
        let chat = h.send(reqwest::Method::POST, "/v1/chats", Some(json!({ "title": "live" }))).await;
        let chat_id = chat["chat"]["id"].as_str().unwrap().to_string();
        let mut rx = h.events(None).await;
        let started = std::time::Instant::now();
        h.send(
            reqwest::Method::POST,
            &format!("/v1/chats/{chat_id}/messages"),
            Some(json!({ "body": "@coder Use the read_file tool to read NOTE.txt, then reply with only the secret word it contains." })),
        )
        .await;
        let seen = collect_until(&mut rx, 300, |evs| {
            evs.iter().any(|(n, d, _)| n == "message.done" && d["message"]["role"] == "agent")
        })
        .await;
        let done = seen
            .iter()
            .find(|(n, d, _)| n == "message.done" && d["message"]["role"] == "agent")
            .unwrap_or_else(|| panic!("{model}: no reply within the timeout"));
        let body = done.1["message"]["body"].as_str().unwrap_or("");
        let deltas = seen.iter().filter(|(n, _, _)| n == "message.delta").count();
        eprintln!("{model}: {:.1}s, {deltas} deltas, reply: {body:?}", started.elapsed().as_secs_f64());
        assert!(body.contains(&word), "{model}: the reply should carry the word read from the file: {body:?}");
        h.handle.shutdown();
    }
}

#[path = "tunnel/e2e_tests.rs"]
mod tunnel_e2e;
