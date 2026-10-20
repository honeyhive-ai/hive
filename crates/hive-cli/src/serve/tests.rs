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
    let opts = ServeOptions {
        listen: "127.0.0.1:0".parse().unwrap(),
        token_file: serve_dir.join("serve.token"),
        ntfy: Some(ntfy::Ntfy { url: ntfy_url, topic: "hive-test".into(), token: None }),
        public_url: None,
        label: "sff".into(),
        name: Some("Phone".into()),
        pair: false,
        prometheus_url: "http://127.0.0.1:9".into(),
        default_agent: None,
        apply_on_approve: false,
        sync_every: Duration::from_millis(300),
    };
    let handle = start(serve_cfg, opts).await.unwrap();
    let base = format!("http://{}", handle.addr);
    let token = handle.shared.token.clone();
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
    let opts = ServeOptions {
        listen: "127.0.0.1:0".parse().unwrap(),
        token_file: dir.join("serve.token"),
        ntfy: None,
        public_url: None,
        label: "sff".into(),
        name: Some("Phone".into()),
        pair: false,
        prometheus_url: "http://127.0.0.1:9".into(),
        default_agent: None,
        apply_on_approve: false,
        sync_every: Duration::from_millis(300),
    };
    let handle = start(config(&dir, &relay, &room), opts).await.unwrap();
    let base = format!("http://{}", handle.addr);
    let client = reqwest::Client::new();
    for (path, key) in [("/v1/agents", "agents"), ("/v1/hosts", "hosts"), ("/v1/chats", "chats"), ("/v1/proposals", "proposals")] {
        let r = client.get(format!("{base}{path}")).bearer_auth(&handle.shared.token).send().await.unwrap();
        assert_eq!(r.status(), 200, "{path}");
        let v: Value = r.json().await.unwrap();
        assert!(v[key].is_array(), "{path}: {v}");
    }
    assert_eq!(
        client
            .get(format!("{base}/v1/agents"))
            .bearer_auth(&handle.shared.token)
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

    for path in ["/v1/chats", "/v1/agents", "/v1/hosts", "/v1/proposals", "/v1/lab/status", "/v1/events"] {
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
