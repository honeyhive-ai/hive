//! End to end: a real `hive serve` with remote access on, a minimal fake
//! relay, and a fake phone (snow initiator) pairing, then GET + SSE through
//! the tunnel, renewal, and revocation (included from `serve/tests.rs` for
//! its harness).
//!
//! Set `HIVE_TUNNEL_INTEROP_RELAY=http://127.0.0.1:<port>` to run
//! `interop_against_a_real_relay` against a real relay binary started with
//! `HIVE_RELAY_TUNNEL=open`.

use std::time::Duration;

use serde_json::json;

use super::*;
use crate::serve::tunnel::frame::{self, Frame, ResponseHead};
use crate::serve::tunnel::testkit::{self, FakeRelay, HostInfo, PhoneKeys};
use crate::serve::tunnel::{self as tun, cred, PairingSecret, RelaySettings};

async fn serve_with_relay(relay: &str) -> Harness {
    let (endpoint, _) = spawn_model(vec![]).await;
    let relay = relay.to_string();
    harness_with(&endpoint, "m", &[("README.md", "hi\n")], move |o| {
        o.remote = Some(RelaySettings::new(&relay, None, true).unwrap());
    })
    .await
}

fn host_info(h: &Harness, relay: &str) -> HostInfo {
    let t = h.handle.shared.tunnel.as_ref().expect("tunnel on");
    HostInfo { relay: relay.to_string(), host_id: t.host_id().to_string(), hs: *t.keys.static_pub() }
}

/// The phone's side of a v2 link: (relay, hid, hs, ps).
fn parse_link(link: &str) -> (String, String, [u8; 32], PairingSecret) {
    let param = |k: &str| link.split(['?', '&']).find_map(|kv| kv.strip_prefix(&format!("{k}="))).unwrap().to_string();
    assert_eq!(param("v"), "2");
    let relay = String::from_utf8(tun::b64u_decode(&param("relay")).unwrap()).unwrap();
    (relay, param("hid"), tun::b64u_fixed(&param("hs")).unwrap(), PairingSecret::from_bytes(tun::b64u_fixed(&param("ps")).unwrap()))
}

/// A v2 link the way `hive pair` makes one: window file + link.
fn pair_link(h: &Harness) -> String {
    let t = h.handle.shared.tunnel.as_ref().unwrap();
    let (secret, exp) = tun::create_window(&t.dir, tun::PAIR_WINDOW_SECS).unwrap();
    tun::pairing_link_v2(Some(("http://100.101.102.104:8787", &h.token)), &t.keys, &t.settings.relay, &secret.ps, exp)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tunnel_end_to_end_pair_get_sse_renew_revoke() {
    let relay = FakeRelay::start().await;
    let h = serve_with_relay(&relay.url).await;
    relay.wait_connected(Duration::from_secs(10)).await.unwrap();
    let t = h.handle.shared.tunnel.clone().unwrap();

    // Pair: `hive pair` leaves a window in pending/, serve announces it.
    let link = pair_link(&h);
    let (relay_url, hid, hs, ps) = parse_link(&link);
    assert_eq!((relay_url.as_str(), hid.as_str(), hs), (relay.url.as_str(), t.host_id(), *t.keys.static_pub()));
    let announced = relay.wait_for("enroll", |m| m["eid"] == ps.eid, Duration::from_secs(5)).await.unwrap();
    assert_eq!(announced["tagHash"], json!(ps.tag_hash()));
    let info = host_info(&h, &relay.url);
    let phone = PhoneKeys::generate();
    let (mut c, pid, credential) = testkit::phone_enroll(&info, &phone, &ps, "Test iPhone").await.unwrap();
    assert!(t.registry.is_active(&pid));
    relay.wait_for("enrollClose", |m| m["eid"] == ps.eid, Duration::from_secs(5)).await.unwrap();
    assert!(!tun::pending_dir(&t.dir).join(format!("enroll-{}.json", ps.eid)).exists());
    // The enrollment connection already carries the API.
    let health = c.request("GET", "/v1/health", None).await.unwrap();
    assert_eq!(health.status, 200);
    assert_eq!(health.json()["workspace"], json!(h.handle.shared.cfg.room), "tunnel peer counts as authorized");
    drop(c);

    // The same ps can't enroll twice (T-qr-reuse).
    assert!(testkit::phone_enroll(&info, &PhoneKeys::generate(), &ps, "again").await.is_err());

    // Session: GET.
    let mut c = testkit::phone_session(&info, &phone, &credential).await.unwrap();
    let chats = c.request("GET", "/v1/chats", None).await.unwrap();
    assert_eq!(chats.status, 200, "{}", String::from_utf8_lossy(&chats.body));
    assert!(chats.json()["chats"].is_array());

    // SSE: the hub's events arrive as DATA on an open stream.
    let sse = c.open("GET", "/v1/events", &[("accept", "text/event-stream")], None, true).await.unwrap();
    let head = c.recv_timeout(Duration::from_secs(5)).await.unwrap();
    assert_eq!((head.ty, head.stream), (frame::RESPONSE, sse));
    let head: ResponseHead = serde_json::from_slice(&head.payload).unwrap();
    assert_eq!(head.status, 200);
    assert!(head.headers.iter().any(|(k, v)| k == "content-type" && v.starts_with("text/event-stream")));
    h.handle.shared.hub.emit("turn.status", json!({ "canary": "tunnel-sse" }));
    let mut text = String::new();
    while !text.contains("tunnel-sse") {
        let f = c.recv_timeout(Duration::from_secs(5)).await.unwrap();
        if f.ty == frame::DATA && f.stream == sse {
            text.push_str(&String::from_utf8_lossy(&f.payload));
        }
    }
    assert!(text.contains("event: turn.status"), "{text}");
    c.send(Frame::reset(sse, frame::reset::CANCEL, "")).await.unwrap();

    // Renewal (tunnel-only): a fresh credential for the same pid and key.
    let renewed = c.request("POST", "/v1/tunnel/credential", None).await.unwrap();
    assert_eq!(renewed.status, 200);
    let new_cred = renewed.json()["credential"].as_str().unwrap().to_string();
    let claims = cred::verify(&new_cred, t.host_id(), Some((t.host_id(), &t.keys.sign_pub())), tun::now()).unwrap();
    assert_eq!((claims.pid.as_str(), claims.pk), (pid.as_str(), phone.auth.verifying_key().to_bytes()));
    // Push preference: previews default on; "previews": false turns them off;
    // a body without the field keeps the stored value.
    let r = c.request("PUT", "/v1/tunnel/push", Some(json!({ "enabled": true }))).await.unwrap();
    assert_eq!((r.status, r.json()), (200, json!({ "enabled": true, "previews": true })));
    assert!(t.registry.get(&pid).unwrap().push_enabled && t.registry.get(&pid).unwrap().push_previews);
    let r = c.request("PUT", "/v1/tunnel/push", Some(json!({ "enabled": true, "previews": false }))).await.unwrap();
    assert_eq!(r.json(), json!({ "enabled": true, "previews": false }));
    let r = c.request("PUT", "/v1/tunnel/push", Some(json!({ "enabled": true }))).await.unwrap();
    assert_eq!(r.json(), json!({ "enabled": true, "previews": false }), "absent field keeps the stored value");
    // A push to this phone is sender-only now; with previews back on it
    // carries the first 120 characters, end-to-end encrypted.
    t.push_available.store(true, std::sync::atomic::Ordering::SeqCst);
    let key: [u8; 32] = tun::b64u_fixed(&t.registry.get(&pid).unwrap().push_key).unwrap();
    let content = tun::PushContent {
        kind: "message.done".into(),
        chat_id: Some("c1".into()),
        title: "coder replied".into(),
        sender: Some("coder".into()),
        preview: Some(format!("secret plan {}", "z".repeat(300))),
        ..Default::default()
    };
    // The SSE stream above was cancelled: the app is "in the background".
    wait_background(&t, &pid).await;
    for (previews, want_preview) in [(false, false), (true, true)] {
        c.request("PUT", "/v1/tunnel/push", Some(json!({ "enabled": true, "previews": previews }))).await.unwrap();
        let seq = t.registry.get(&pid).unwrap().push_seq;
        // A different chat each time: the same (kind, chat) within 10 s is
        // coalesced by the host-side limiter.
        let content = tun::PushContent { chat_id: Some(format!("c-{previews}")), ..content.clone() };
        assert_eq!(t.push_all(&content), 1);
        let m = relay
            .wait_for(
                "push",
                |m| {
                    let ct = tun::b64u_decode(m["ct"].as_str().unwrap_or_default()).unwrap_or_default();
                    m["pid"] == json!(pid) && ct.len() > 28 && testkit::open_push(&key, t.host_id(), &pid, &ct)["seq"] == json!(seq + 1)
                },
                Duration::from_secs(5),
            )
            .await
            .unwrap();
        let ct = tun::b64u_decode(m["ct"].as_str().unwrap()).unwrap();
        assert!(ct.len() <= tun::PUSH_CT_MAX);
        let pt = testkit::open_push(&key, t.host_id(), &pid, &ct);
        assert_eq!(pt["sender"], "coder");
        if want_preview {
            assert!(pt["body"].as_str().unwrap().starts_with("secret plan zzz") && pt["body"].as_str().unwrap().ends_with('…'));
            assert_eq!(pt["body"].as_str().unwrap().chars().count(), tun::PREVIEW_CHARS + 1);
        } else {
            assert_eq!(pt["body"], "");
            assert!(!pt.to_string().contains("secret"));
        }
    }
    t.push_available.store(false, std::sync::atomic::Ordering::SeqCst);
    // …and 404 on the direct path, even with the right bearer token.
    let direct = h.client.post(format!("{}/v1/tunnel/credential", h.base)).bearer_auth(&h.token).send().await.unwrap();
    assert_eq!(direct.status(), 404);

    // Revocation from another process (`hive phones revoke`): the live SSE
    // session gets GOAWAY revoked within ~1 s of serve noticing (≤ 2 s poll).
    let sse = c.open("GET", "/v1/events", &[], None, true).await.unwrap();
    let _ = c.recv_timeout(Duration::from_secs(5)).await.unwrap();
    let data_dir = h.handle.shared.cfg.data_dir.clone();
    tun::cli::phones_revoke(&data_dir, Some(&pid), &h.handle.shared.opts.token_file, false).unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
    loop {
        let f = tokio::time::timeout_at(deadline, c.recv()).await.expect("GOAWAY").unwrap();
        if f.ty == frame::GOAWAY {
            assert_eq!(f.code(), Some(frame::goaway::REVOKED));
            break;
        }
        assert_eq!(f.stream, sse);
    }
    let rv = relay.wait_for("revoke", |m| m["pid"] == json!(pid), Duration::from_secs(5)).await.unwrap();
    assert!(rv["exp"].as_u64().unwrap() >= claims.exp, "revocation lasts until the latest credential's exp");
    // Acked → the pending file is gone; reconnect is refused at the relay.
    for _ in 0..50 {
        if !tun::pending_dir(&t.dir).join(format!("revoke-{pid}.json")).exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(!tun::pending_dir(&t.dir).join(format!("revoke-{pid}.json")).exists());
    let err = testkit::phone_session(&info, &phone, &new_cred).await.err().expect("revoked");
    assert_eq!(testkit::close_code(&err), Some(4003));
    // The direct-path token was rotated, and serve picked it up.
    let old_token = h.token.clone();
    let mut refused = false;
    for _ in 0..40 {
        let r = h.client.get(format!("{}/v1/chats", h.base)).bearer_auth(&old_token).send().await.unwrap();
        if r.status() == 401 {
            refused = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(refused, "old bearer token still accepted after revoke");
    let new_token = h.handle.shared.token();
    assert_ne!(new_token, old_token);
    assert_eq!(h.client.get(format!("{}/v1/chats", h.base)).bearer_auth(&new_token).send().await.unwrap().status(), 200);
}

/// Until the phone's cancelled `/v1/events` streams are gone.
async fn wait_background(t: &tun::TunnelHost, pid: &str) {
    for _ in 0..100 {
        if t.foreground_streams(pid) == 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the events stream still counts as foreground");
}

/// The four notification kinds, end to end: a real serve projection change
/// (agent reply, mention, proposal, workflow gate) → push on the fake relay →
/// decrypts with the phone's push key; and none while the app is open.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pushes_reach_the_relay_encrypted_and_skip_a_foreground_phone() {
    use hive_core::{workflow, ActionProposal, NodeRunStatus, ProposalKind};
    let relay = FakeRelay::start().await;
    let h = serve_with_relay(&relay.url).await;
    relay.wait_connected(Duration::from_secs(10)).await.unwrap();
    let t = h.handle.shared.tunnel.clone().unwrap();
    let (_, _, _, ps) = parse_link(&pair_link(&h));
    relay.wait_for("enroll", |m| m["eid"] == ps.eid, Duration::from_secs(5)).await.unwrap();
    let info = host_info(&h, &relay.url);
    let phone = PhoneKeys::generate();
    let (_c, pid, credential) = testkit::phone_enroll(&info, &phone, &ps, "Push phone").await.unwrap();
    drop(_c);
    let mut c = testkit::phone_session(&info, &phone, &credential).await.unwrap();
    assert_eq!(c.request("PUT", "/v1/tunnel/push", Some(json!({ "enabled": true }))).await.unwrap().status, 200);
    t.push_available.store(true, std::sync::atomic::Ordering::SeqCst);
    let key: [u8; 32] = tun::b64u_fixed(&t.registry.get(&pid).unwrap().push_key).unwrap();
    let shared = h.handle.shared.clone();
    let ws = shared.ws;
    let chat = h.send(reqwest::Method::POST, "/v1/chats", None).await;
    let chat_id = uuid::Uuid::parse_str(chat["chat"]["id"].as_str().unwrap()).unwrap();

    // Foreground: an open events stream → a finished reply pushes nothing.
    let sse = c.open("GET", "/v1/events", &[("accept", "text/event-stream")], None, true).await.unwrap();
    let _ = c.recv_timeout(Duration::from_secs(5)).await.unwrap();
    assert_eq!(t.foreground_streams(&pid), 1);
    let seq0 = t.registry.get(&pid).unwrap().push_seq;
    let mid = shared.lock().begin_assistant_message(chat_id, ws, "coder".to_string(), "gw".to_string(), Some(h.agent_id), None).unwrap();
    shared.lock().complete_assistant_message(chat_id, ws, mid, "all done").unwrap();
    shared.refresh();
    assert_eq!(t.registry.get(&pid).unwrap().push_seq, seq0, "no push while the app is open");

    // Background: the app cancelled the stream.
    c.send(Frame::reset(sse, frame::reset::CANCEL, "")).await.unwrap();
    wait_background(&t, &pid).await;
    let next = |seq: u64| {
        let relay = &relay;
        let pid = pid.clone();
        let hid = t.host_id().to_string();
        async move {
            let m = relay
                .wait_for(
                    "push",
                    |m| {
                        let ct = tun::b64u_decode(m["ct"].as_str().unwrap_or_default()).unwrap_or_default();
                        m["pid"] == json!(pid) && ct.len() > 28 && testkit::open_push(&key, &hid, &pid, &ct)["seq"] == json!(seq)
                    },
                    Duration::from_secs(5),
                )
                .await
                .unwrap();
            assert_eq!(m["prio"], "high");
            assert!(m["collapse"].as_str().is_some_and(|c| !c.contains('-')), "collapse never reveals the chat id");
            testkit::open_push(&key, &hid, &pid, &tun::b64u_decode(m["ct"].as_str().unwrap()).unwrap())
        }
    };

    // An agent reply that mentions the user → one `mention` push, not two.
    let me = shared.lock().author().display_name.clone();
    let mid = shared.lock().begin_assistant_message(chat_id, ws, "coder".to_string(), "gw".to_string(), Some(h.agent_id), None).unwrap();
    shared.lock().complete_assistant_message(chat_id, ws, mid, format!("@{me} the build is green")).unwrap();
    shared.refresh();
    let pt = next(seq0 + 1).await;
    assert_eq!((pt["kind"].as_str(), pt["title"].as_str(), pt["sender"].as_str()), (Some("mention"), Some("coder mentioned you"), Some("coder")));
    assert_eq!(pt["chatId"], json!(chat_id.to_string()));
    assert_eq!(pt["body"], json!(format!("@{me} the build is green")));
    assert_eq!(t.registry.get(&pid).unwrap().push_seq, seq0 + 1, "exactly one push for the mentioning reply");

    // A proposal waiting for review.
    let p = ActionProposal::new("coder proposed changes to 1 file", ProposalKind::FileDiff, "agent-x");
    let proposal_id = p.id;
    shared.lock().upsert_proposal(chat_id, ws, p).unwrap();
    shared.refresh();
    let pt = next(seq0 + 2).await;
    assert_eq!((pt["kind"].as_str(), pt["title"].as_str()), (Some("proposal.created"), Some("Change ready for review")));
    assert_eq!((pt["chatId"].clone(), pt["proposalId"].clone()), (json!(chat_id.to_string()), json!(proposal_id.to_string())));

    // A workflow gate waiting for approval (the desktop's run engine writes
    // the gate's proposal, then the run): one `workflow.approval` push.
    let def = workflow::preset_review_gate();
    let mut run = workflow::new_run(&def, "build it", "u1");
    let gate = ActionProposal::new("Approve?", ProposalKind::Decision, "");
    let st = run.node_state_mut("approval").unwrap();
    st.status = NodeRunStatus::AwaitingApproval;
    st.proposal_id = Some(gate.id);
    let gate_id = gate.id;
    shared.lock().upsert_proposal(chat_id, ws, gate).unwrap();
    shared.refresh();
    shared.lock().upsert_workflow_run(chat_id, ws, run.clone()).unwrap();
    shared.refresh();
    let pt = next(seq0 + 3).await;
    assert_eq!(pt["kind"], "workflow.approval");
    assert_eq!(pt["title"], json!(format!("{} needs approval", def.name)));
    assert_eq!(pt["workflowRunId"], json!(run.id.to_string()));
    assert_eq!(pt["workflowId"], json!(def.id.to_string()));
    assert_eq!(pt["proposalId"], json!(gate_id.to_string()));
    assert_eq!(pt["body"], "Approval");
    assert_eq!(t.registry.get(&pid).unwrap().push_seq, seq0 + 3, "the gate's proposal didn't push on its own");
    t.push_available.store(false, std::sync::atomic::Ordering::SeqCst);
    h.handle.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tunnel_request_after_revocation_on_open_session_gets_401() {
    // The per-request re-check (§9), independent of the GOAWAY: a request
    // racing the revocation is refused by the auth middleware.
    let relay = FakeRelay::start().await;
    let h = serve_with_relay(&relay.url).await;
    relay.wait_connected(Duration::from_secs(10)).await.unwrap();
    let t = h.handle.shared.tunnel.clone().unwrap();
    let (_, _, _, ps) = parse_link(&pair_link(&h));
    relay.wait_for("enroll", |m| m["eid"] == ps.eid, Duration::from_secs(5)).await.unwrap();
    let info = host_info(&h, &relay.url);
    let phone = PhoneKeys::generate();
    let (_c, pid, _) = testkit::phone_enroll(&info, &phone, &ps, "p").await.unwrap();
    // Mark revoked in the registry only (no broadcast yet), then call the
    // router the way the connector does.
    t.registry.revoke(&pid).unwrap();
    let req = axum::http::Request::builder().uri("/v1/chats").body(axum::body::Body::empty()).unwrap();
    let mut req = req;
    req.extensions_mut().insert(tun::TunnelPeer { pid });
    use tower::ServiceExt;
    let resp = crate::serve::api::router(h.handle.shared.clone()).oneshot(req).await.unwrap();
    assert_eq!(resp.status(), 401);
}

/// Interop with a real relay (the Go `honeyhive-ai/relay` with
/// `HIVE_RELAY_TUNNEL=open`). Ignored unless `HIVE_TUNNEL_INTEROP_RELAY` is set.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn interop_against_a_real_relay() {
    let Ok(relay) = std::env::var("HIVE_TUNNEL_INTEROP_RELAY") else {
        eprintln!("skipped: set HIVE_TUNNEL_INTEROP_RELAY=http://127.0.0.1:<port>");
        return;
    };
    let h = serve_with_relay(&relay).await;
    let t = h.handle.shared.tunnel.clone().unwrap();
    for _ in 0..100 {
        if t.live_status().connected {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(t.live_status().connected, "host never connected: {:?}", t.live_status());
    let (_, _, _, ps) = parse_link(&pair_link(&h));
    // Let serve pick up and announce the window.
    tokio::time::sleep(Duration::from_secs(3)).await;
    let info = host_info(&h, &relay);
    let phone = PhoneKeys::generate();
    let (mut c, pid, credential) = testkit::phone_enroll(&info, &phone, &ps, "Interop").await.expect("enroll via the real relay");
    assert_eq!(c.request("GET", "/v1/health", None).await.unwrap().status, 200);
    drop(c);
    let mut c = testkit::phone_session(&info, &phone, &credential).await.expect("session via the real relay");
    assert_eq!(c.request("GET", "/v1/chats", None).await.unwrap().status, 200);
    let sse = c.open("GET", "/v1/events", &[], None, true).await.unwrap();
    let _ = c.recv_timeout(Duration::from_secs(5)).await.unwrap();
    h.handle.shared.hub.emit("turn.status", json!({ "canary": "interop" }));
    let mut text = String::new();
    while !text.contains("interop") {
        let f = c.recv_timeout(Duration::from_secs(5)).await.unwrap();
        if f.ty == frame::DATA && f.stream == sse {
            text.push_str(&String::from_utf8_lossy(&f.payload));
        }
    }
    t.revoke(&pid).unwrap();
    loop {
        let f = c.recv_timeout(Duration::from_secs(5)).await.unwrap();
        if f.ty == frame::GOAWAY {
            assert_eq!(f.code(), Some(frame::goaway::REVOKED));
            break;
        }
    }
    for _ in 0..50 {
        if t.pending_revocations().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(t.pending_revocations().is_empty(), "relay never acked the revoke");
    let err = testkit::phone_session(&info, &phone, &credential).await.err().expect("revoked at the relay");
    assert_eq!(testkit::close_code(&err), Some(4003), "{err:#}");
}
