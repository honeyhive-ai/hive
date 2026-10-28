//! Conformance against `docs/relay-tunnel/vectors.json` (§19.2) and mux /
//! session behaviour over an in-memory WebSocket (no relay).

use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::Request;
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Extension, Router};
use serde_json::{json, Value};

use super::frame::{self, Frame, RequestHead, ResponseHead};
use super::testkit::{self, HostInfo, PhoneConn, PhoneKeys};
use super::*;

fn vectors() -> Value {
    let p = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/relay-tunnel/vectors.json");
    serde_json::from_str(&std::fs::read_to_string(p).expect("vectors.json")).unwrap()
}

fn h(v: &Value) -> Vec<u8> {
    hex::decode(v.as_str().unwrap()).unwrap()
}
fn h32(v: &Value) -> [u8; 32] {
    h(v).try_into().unwrap()
}
fn b(v: &Value) -> Vec<u8> {
    b64u_decode(v.as_str().unwrap()).unwrap()
}

fn vector_keys(v: &Value) -> HostKeys {
    HostKeys::from_parts(h32(&v["keys"]["hostSignSeedHex"]), h32(&v["keys"]["hostStaticPrivHex"]))
}

// ── V-keys ───────────────────────────────────────────────────────────────────

#[test]
fn v_keys_host_id_and_challenge() {
    let v = vectors();
    let k = vector_keys(&v);
    assert_eq!(b64u(k.sign_pub().as_bytes()), v["keys"]["hostSignPub"]);
    assert_eq!(b64u(k.static_pub()), v["keys"]["hostStaticPub"]);
    assert_eq!(b64u(keys::x25519_pub(&h32(&v["keys"]["phoneStaticPrivHex"]))), v["keys"]["phoneStaticPub"]);
    assert_eq!(k.host_id(), v["hostId"]);
    let nonce: [u8; 32] = b(&v["challenge"]["nonce"]).try_into().unwrap();
    assert_eq!(hex::encode(ctx("host-auth", &[k.host_id().as_bytes(), &nonce])), v["challenge"]["hostAuthInputHex"]);
    assert_eq!(b64u(k.sign_challenge(&nonce)), v["challenge"]["hostAuthSig"]);
    let phone = ed25519_dalek::SigningKey::from_bytes(&h32(&v["keys"]["phoneAuthSeedHex"]));
    use ed25519_dalek::Signer;
    let pa = ctx("phone-auth", &[k.host_id().as_bytes(), &nonce]);
    assert_eq!(hex::encode(&pa), v["challenge"]["phoneAuthInputHex"]);
    assert_eq!(b64u(phone.sign(&pa).to_bytes()), v["challenge"]["phoneAuthSig"]);
}

#[test]
fn v_keys_pairing_derivations_and_link() {
    let v = vectors();
    let p = &v["pairing"];
    let s = PairingSecret::from_bytes(b(&p["pairingSecret"]).try_into().unwrap());
    assert_eq!(s.eid, p["eid"]);
    assert_eq!(b64u(s.tag), p["tag"]);
    assert_eq!(s.tag_hash(), p["tagHash"]);
    assert_eq!(hex::encode(s.psk), p["pskHex"]);
    // The full v2 link, from the same inputs (url/token read back from it).
    let link = p["link"].as_str().unwrap();
    let param = |k: &str| link.split(['?', '&']).find_map(|kv| kv.strip_prefix(&format!("{k}="))).unwrap().to_string();
    let url = String::from_utf8(b64u_decode(&param("url")).unwrap()).unwrap();
    let exp: u64 = param("exp").parse().unwrap();
    let built = pairing_link_v2(Some((&url, &param("token"))), &vector_keys(&v), "https://relay.example.com", &s.ps, exp);
    assert_eq!(built, link);
    // Relay-only drops url/token, keeps the rest.
    let relay_only = pairing_link_v2(None, &vector_keys(&v), "https://relay.example.com/", &s.ps, exp);
    assert!(relay_only.starts_with("hive://pair?v=2&hid="), "{relay_only}");
    assert!(!relay_only.contains("token="));
}

#[test]
fn b64u_is_strict() {
    assert!(b64u_decode("AAAA").is_ok());
    assert!(b64u_decode("AAA=").is_err());
    assert!(b64u_decode("AA+A").is_err());
    assert!(b64u_decode("AA/A").is_err());
}

// ── V-cred ───────────────────────────────────────────────────────────────────

#[test]
fn v_cred_issue_and_verify() {
    let v = vectors();
    let k = vector_keys(&v);
    let c = &v["credential"];
    let claims = cred::Claims {
        hid: v["hostId"].as_str().unwrap().into(),
        pid: v["pid"].as_str().unwrap().into(),
        pk: b(&v["keys"]["phoneAuthPub"]).try_into().unwrap(),
        iat: 1_793_000_000,
        exp: 1_793_000_000 + CRED_LIFETIME_SECS,
    };
    assert_eq!(claims.to_json(), c["claimsJson"]);
    let token = cred::sign(&k, &claims);
    assert_eq!(token, c["token"]);
    assert_eq!(cred::issue(&k, &claims.pid, &claims.pk, claims.iat), (token.clone(), claims.exp));

    let hid = k.host_id();
    let key = k.sign_pub();
    let reg = Some((hid, &key));
    let now = claims.iat + 10;
    assert_eq!(cred::verify(&token, hid, reg, now), Ok(claims.clone()));
    use cred::CredError::*;
    assert_eq!(cred::verify(v["negative"]["credentialTamperedClaims"].as_str().unwrap(), hid, reg, now), Err(Unauthorized));
    assert_eq!(cred::verify(&token, hid, reg, claims.exp), Err(Expired));
    assert_eq!(cred::verify(&token, "AAAAAAAAAAAAAAAAAAAAAA", reg, now), Err(Unauthorized), "hid != host");
    assert_eq!(cred::verify(&token, hid, None, now), Err(HostUnknown));
    // Padding added to a segment.
    let parts: Vec<&str> = token.split('.').collect();
    let padded = format!("{}.{}=.{}", parts[0], parts[1], parts[2]);
    assert_eq!(cred::verify(&padded, hid, reg, now), Err(Unauthorized));
    // A validly signed token with a duplicate "exp".
    let dup = c["claimsJson"].as_str().unwrap().replace("}", ",\"exp\":9999999999}");
    let signed = format!("hpc1.{}", b64u(&dup));
    let dup_token = format!("{signed}.{}", b64u(k.sign(signed.as_bytes())));
    assert_eq!(cred::verify(&dup_token, hid, reg, now), Err(Unauthorized));
    // iat too far in the future; lifetime above the relay cap.
    let future = cred::sign(&k, &cred::Claims { iat: now + 1000, exp: now + 2000, ..claims.clone() });
    assert_eq!(cred::verify(&future, hid, reg, now), Err(Unauthorized));
    let long = cred::sign(&k, &cred::Claims { exp: claims.iat + cred::MAX_LIFETIME_SECS + 1, ..claims.clone() });
    assert_eq!(cred::verify(&long, hid, reg, now), Err(Unauthorized));
}

// ── V-noise ──────────────────────────────────────────────────────────────────

fn check_transcript(v: &Value, t: &Value) {
    let keys = &v["keys"];
    let host = vector_keys(v);
    let psk: Option<[u8; 32]> = t.get("pskHex").map(h32);
    assert_eq!(hex::encode(noise::prologue(host.host_id())), t["prologueHex"]);
    assert_eq!(t["protocol"], if psk.is_some() { noise::IK_PSK1 } else { noise::IK });
    let mut ini = noise::initiator(
        &h32(&keys["phoneStaticPrivHex"]),
        host.static_pub(),
        host.host_id(),
        psk.as_ref(),
        Some(&h32(&t["phoneEphemeralPrivHex"])),
    )
    .unwrap();
    let mut res = noise::responder_with(host.static_priv(), host.host_id(), psk.as_ref(), Some(&h32(&t["hostEphemeralPrivHex"]))).unwrap();
    let (mut buf, mut out) = (vec![0u8; 65535], vec![0u8; 65535]);
    let n = ini.write_message(t["msg1Payload"].as_str().unwrap().as_bytes(), &mut buf).unwrap();
    assert_eq!(hex::encode(&buf[..n]), t["msg1Hex"]);
    let m = res.read_message(&h(&t["msg1Hex"]), &mut out).unwrap();
    assert_eq!(&out[..m], t["msg1Payload"].as_str().unwrap().as_bytes());
    assert_eq!(b64u(res.get_remote_static().unwrap()), keys["phoneStaticPub"]);
    let n = res.write_message(t["msg2Payload"].as_str().unwrap().as_bytes(), &mut buf).unwrap();
    assert_eq!(hex::encode(&buf[..n]), t["msg2Hex"]);
    let m = ini.read_message(&buf[..n], &mut out).unwrap();
    assert_eq!(&out[..m], t["msg2Payload"].as_str().unwrap().as_bytes());
    assert_eq!(hex::encode(res.get_handshake_hash()), t["handshakeHashHex"]);
    assert_eq!(hex::encode(ini.get_handshake_hash()), t["handshakeHashHex"]);
    if let Some(pk) = t.get("pushKeyHex") {
        assert_eq!(hex::encode(hkdf(res.get_handshake_hash(), "push-key", 32)), *pk);
    }
    let mut ini = ini.into_transport_mode().unwrap();
    let mut res = res.into_transport_mode().unwrap();
    for msg in t["transport"].as_array().unwrap() {
        let pt = h(&msg["plaintextHex"]);
        // V-mux: every vector plaintext is one frame that round-trips.
        let f = Frame::decode(&pt).unwrap();
        assert_eq!(f.encode(), pt);
        let phone_to_host = msg["dir"] == "phone->host";
        let n = if phone_to_host { ini.write_message(&pt, &mut buf) } else { res.write_message(&pt, &mut buf) }.unwrap();
        assert_eq!(hex::encode(&buf[..n]), msg["wireHex"], "{}", msg["note"]);
        let m = if phone_to_host { res.read_message(&buf[..n], &mut out) } else { ini.read_message(&buf[..n], &mut out) }.unwrap();
        assert_eq!(&out[..m], &pt[..]);
    }
}

#[test]
fn v_noise_enrollment_transcript() {
    let v = vectors();
    check_transcript(&v, &v["enrollment"]);
}

#[test]
fn v_noise_session_transcript() {
    let v = vectors();
    check_transcript(&v, &v["session"]);
}

#[test]
fn v_noise_negative_vectors_rejected() {
    let v = vectors();
    let host = vector_keys(&v);
    let mut out = vec![0u8; 65535];
    let mut res = noise::responder(host.static_priv(), host.host_id(), None).unwrap();
    assert!(res.read_message(&h(&v["negative"]["sessionMsg1OtherHostPrologueHex"]), &mut out).is_err());
    let s = &v["session"];
    let mut ini = noise::initiator(
        &h32(&v["keys"]["phoneStaticPrivHex"]),
        host.static_pub(),
        host.host_id(),
        None,
        Some(&h32(&s["phoneEphemeralPrivHex"])),
    )
    .unwrap();
    let mut buf = vec![0u8; 65535];
    ini.write_message(br#"{"v":1}"#, &mut buf).unwrap();
    assert!(ini.read_message(&h(&v["negative"]["sessionMsg2TamperedHex"]), &mut out).is_err());
    // T-wrong-prologue: right hs, wrong hid.
    let mut wrong = noise::initiator(&h32(&v["keys"]["phoneStaticPrivHex"]), host.static_pub(), "AAAAAAAAAAAAAAAAAAAAAA", None, None).unwrap();
    let n = wrong.write_message(br#"{"v":1}"#, &mut buf).unwrap();
    let mut res = noise::responder(host.static_priv(), host.host_id(), None).unwrap();
    assert!(res.read_message(&buf[..n], &mut out).is_err());
}

#[test]
fn v_noise_all_zero_dh_aborts() {
    // Initiator with a peer static of 32 zero bytes: `es` is all-zero.
    let mut ini = noise::initiator(&random_bytes(), &[0u8; 32], "AAAAAAAAAAAAAAAAAAAAAA", None, None).unwrap();
    let mut buf = vec![0u8; 65535];
    assert!(matches!(ini.write_message(b"{}", &mut buf), Err(snow::Error::Dh)));
    // Responder fed an all-zero ephemeral.
    let host = HostKeys::generate();
    let mut ini = noise::initiator(&random_bytes(), host.static_pub(), host.host_id(), None, None).unwrap();
    let n = ini.write_message(br#"{"v":1}"#, &mut buf).unwrap();
    buf[..32].fill(0);
    let mut res = noise::responder(host.static_priv(), host.host_id(), None).unwrap();
    let mut out = vec![0u8; 65535];
    assert!(matches!(res.read_message(&buf[..n], &mut out), Err(snow::Error::Dh)));
}

// ── V-mux codec ──────────────────────────────────────────────────────────────

#[test]
fn v_mux_frame_codec() {
    let head = RequestHead { method: "GET".into(), path: "/v1/health".into(), headers: vec![("accept".into(), "application/json".into())] };
    assert_eq!(
        hex::encode(Frame::open(1, &head).encode()),
        "01000000017b226d6574686f64223a22474554222c2270617468223a222f76312f6865616c7468222c2268656164657273223a5b5b22616363657074222c226170706c69636174696f6e2f6a736f6e225d5d7d"
    );
    let resp = ResponseHead { status: 200, headers: vec![("content-type".into(), "application/json".into())] };
    assert!(hex::encode(Frame::response(1, &resp).encode()).starts_with("0500000001"));
    assert_eq!(hex::encode(Frame::fin(1).encode()), "0300000001");
    assert_eq!(hex::encode(Frame::ping([0, 1, 2, 3, 4, 5, 6, 7]).encode()), "07000000000001020304050607");
    let r = Frame::reset(7, frame::reset::FLOW_CONTROL, "x");
    assert_eq!(hex::encode(r.encode()), "04000000070000000378");
    let d = Frame::decode(&r.encode()).unwrap();
    assert_eq!((d.ty, d.stream, d.code()), (frame::RESET, 7, Some(3)));
    assert_eq!(Frame::decode(&[1, 0, 0, 0]), Err(frame::ShortFrame));
    assert_eq!(Frame::window(3, 65536).code(), Some(65536));
    assert_eq!(Frame::goaway(frame::goaway::REVOKED, "").encode(), vec![9, 0, 0, 0, 0, 0, 0, 0, 1]);
}

// ── V-push ───────────────────────────────────────────────────────────────────

#[test]
fn v_push_vector() {
    use chacha20poly1305::aead::{Aead, KeyInit, Payload};
    let v = vectors();
    let key = h32(&v["enrollment"]["pushKeyHex"]);
    let p = &v["push"];
    let (hid, pid) = (v["hostId"].as_str().unwrap(), v["pid"].as_str().unwrap());
    assert_eq!(hex::encode(ctx("push", &[hid.as_bytes(), pid.as_bytes()])), p["adHex"]);
    let nonce: [u8; 12] = h(&p["nonceHex"]).try_into().unwrap();
    let ct = push_encrypt(&key, hid, pid, &nonce, p["plaintext"].as_str().unwrap().as_bytes());
    assert_eq!(b64u(&ct), p["ct"]);
    let cipher = chacha20poly1305::ChaCha20Poly1305::new((&key).into());
    let ad = h(&p["adHex"]);
    let dec = |ct: &[u8], ad: &[u8]| cipher.decrypt(ct[..12].into(), Payload { msg: &ct[12..], aad: ad });
    assert_eq!(dec(&ct, &ad).unwrap(), p["plaintext"].as_str().unwrap().as_bytes());
    let mut flipped = ct.clone();
    flipped[20] ^= 1;
    assert!(dec(&flipped, &ad).is_err());
    let mut bad_ad = ad.clone();
    bad_ad[0] ^= 1;
    assert!(dec(&ct, &bad_ad).is_err());
    assert_eq!(push_collapse(&key, "chat").len(), 16);
}

// ── Push previews (§14) ──────────────────────────────────────────────────────

use super::testkit::open_push;

fn reply(text: &str) -> PushContent {
    PushContent {
        kind: "message.done".into(),
        chat_id: Some("chat-1".into()),
        title: "coder replied".into(),
        sender: Some("coder".into()),
        preview: Some(text.into()),
        ..Default::default()
    }
}

#[test]
fn preview_text_cuts_on_char_boundaries() {
    assert_eq!(preview_text("  hello\n\n  world\t! "), "hello world !");
    assert_eq!(preview_text("a\u{0}b\u{7}c"), "abc");
    let long = "é".repeat(200);
    let p = preview_text(&long);
    assert_eq!(p.chars().count(), PREVIEW_CHARS + 1);
    assert!(p.ends_with('…') && p.starts_with("éé"));
    let exact = "x".repeat(PREVIEW_CHARS);
    assert_eq!(preview_text(&exact), exact, "no ellipsis when nothing is cut");
    // Emoji (4-byte scalars) are never split.
    let emoji = "🐝".repeat(150);
    let p = preview_text(&emoji);
    assert_eq!(p.chars().filter(|c| *c == '🐝').count(), PREVIEW_CHARS);
    assert!(p.len() <= 512);
}

#[test]
fn push_plaintext_previews_on_and_off() {
    let c = reply("Done: refactored the parser.\nAll tests pass.");
    let on = push_plaintext(&c, true, 7, 1_800_000_000);
    assert_eq!(on["body"], "Done: refactored the parser. All tests pass.");
    assert_eq!((on["title"].as_str(), on["sender"].as_str(), on["preview"].as_bool()), (Some("coder replied"), Some("coder"), Some(true)));
    assert_eq!((on["seq"].as_u64(), on["chatId"].as_str(), on["kind"].as_str()), (Some(7), Some("chat-1"), Some("message.done")));
    let off = push_plaintext(&c, false, 8, 1_800_000_000);
    assert_eq!(off["body"], "", "sender only");
    assert_eq!(off["title"], "coder replied");
    assert_eq!(off["sender"], "coder");
    assert!(off.get("preview").is_none());
    assert!(!off.to_string().contains("parser"), "no message text with previews off");
    // A non-content body (phone.paired) is shown either way.
    let paired = PushContent { kind: "phone.paired".into(), title: "New phone paired".into(), fixed_body: "Ana's iPhone".into(), ..Default::default() };
    assert_eq!(push_plaintext(&paired, false, 1, 0)["body"], "Ana's iPhone");
    assert_eq!(push_plaintext(&paired, true, 1, 0)["body"], "Ana's iPhone");
}

#[test]
fn sealed_push_stays_within_the_spec_limit() {
    let key = [9u8; 32];
    let (hid, pid) = ("2LGZFj2lCKlo49_pwPzGjw", "lg7S2X1oS18S3AUMgTzRgQ");
    // Worst cases for JSON size: quotes/backslashes (escaped ×2), control
    // characters in the title and sender (\u00XX, ×6), 4-byte scalars.
    for text in ["\"\\".repeat(500), "🐝".repeat(1000), "\u{1}".repeat(1000) + &"x".repeat(1000)] {
        let mut c = reply(&text);
        c.title = "\u{1}".repeat(64);
        c.sender = Some("\"".repeat(64));
        let ct = seal_push(&key, hid, pid, &c, true, u64::MAX, u64::MAX);
        assert!(ct.len() <= PUSH_CT_MAX, "{} bytes", ct.len());
        let pt = open_push(&key, hid, pid, &ct);
        assert_eq!(pt["preview"], true);
        assert!(pt["body"].as_str().unwrap().chars().count() <= PREVIEW_CHARS + 1);
    }
}

// ── Files ────────────────────────────────────────────────────────────────────

#[cfg(unix)]
#[test]
fn state_files_are_private() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let host = TunnelHost::open(tmp.path(), RelaySettings::new("https://relay.example.com", None, false).unwrap()).unwrap();
    let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&host.dir), 0o700);
    assert_eq!(mode(&HostKeys::file(&host.dir)), 0o600);
    // Reloads to the same identity.
    let again = HostKeys::load(&host.dir).unwrap().unwrap();
    assert_eq!(again.host_id(), host.host_id());
    assert_eq!(again.static_pub(), host.keys.static_pub());
    host.registry.activate(record("p", &PhoneKeys::generate())).unwrap();
    assert_eq!(mode(&Registry::file(&host.dir)), 0o600);
    // An http relay needs the explicit opt-in.
    assert!(RelaySettings::new("http://relay.example.com", None, false).is_err());
    assert!(RelaySettings::new("http://127.0.0.1:1", None, true).is_ok());
    assert!(RelaySettings::new("ftp://x", None, true).is_err());
}

// ── Session + mux over an in-memory WebSocket ───────────────────────────────

fn record(pid: &str, phone: &PhoneKeys) -> PhoneRecord {
    PhoneRecord {
        pid: pid.into(),
        name: "test phone".into(),
        auth_pub: b64u(phone.auth.verifying_key().as_bytes()),
        static_pub: b64u(phone.static_pub()),
        push_key: b64u([7u8; 32]),
        enrolled_at: now(),
        cred_exp: now() + CRED_LIFETIME_SECS,
        revoked_at: None,
        push_enabled: false,
        push_previews: true,
        push_seq: 0,
        last_seen: None,
    }
}

async fn hello(peer: Option<Extension<TunnelPeer>>, headers: HeaderMap) -> axum::Json<Value> {
    axum::Json(json!({ "pid": peer.map(|p| p.0.pid), "authorization": headers.contains_key("authorization") }))
}

fn test_router() -> Router {
    Router::new()
        .route("/v1/hello", get(hello))
        .route("/v1/echo", post(|b: Bytes| async move { b }))
        .route("/v1/big", get(|| async { vec![b'x'; 600_000] }))
        .route("/v1/sink", post(|_req: Request| async { "ignored" }))
        .route(
            "/v1/sse",
            get(|| async {
                let s = futures_util::stream::unfold(0u64, |n| async move {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    Some((Ok::<_, std::convert::Infallible>(Bytes::from(format!("data: {n}\n\n"))), n + 1))
                });
                axum::response::Response::builder().header("content-type", "text/event-stream").body(Body::from_stream(s)).unwrap()
            }),
        )
}

struct Rig {
    _tmp: tempfile::TempDir,
    host: Arc<TunnelHost>,
    phone: PhoneKeys,
    info: HostInfo,
    stop: tokio::sync::watch::Sender<bool>,
}

fn rig() -> Rig {
    let tmp = tempfile::tempdir().unwrap();
    let host = TunnelHost::open(tmp.path(), RelaySettings::new("https://relay.example.com", None, false).unwrap()).unwrap();
    let phone = PhoneKeys::generate();
    host.registry.activate(record("phone-1", &phone)).unwrap();
    let info = HostInfo { relay: String::new(), host_id: host.host_id().into(), hs: *host.keys.static_pub() };
    let (stop, _) = tokio::sync::watch::channel(false);
    Rig { _tmp: tmp, host, phone, info, stop }
}

type MemWs = tokio_tungstenite::WebSocketStream<tokio::io::DuplexStream>;

impl Rig {
    /// Start a host session for `mode`; returns the phone's raw WebSocket and
    /// the session task.
    async fn accept(&self, mode: session::Mode, t: session::Timeouts) -> (MemWs, tokio::task::JoinHandle<anyhow::Result<()>>) {
        let (client, server) = testkit::ws_pair().await;
        let task = tokio::spawn(session::run(self.host.clone(), test_router(), server, mode, self.stop.subscribe(), t));
        (client, task)
    }

    async fn session(&self) -> PhoneConn<MemWs> {
        let (ws, _) = self.accept(session::Mode::Session { pid: "phone-1".into() }, Default::default()).await;
        PhoneConn::handshake(ws, &self.phone, &self.info).await.unwrap()
    }
}

#[tokio::test]
async fn mux_requests_reach_the_router_with_tunnel_peer() {
    let r = rig();
    let mut c = r.session().await;
    // A bearer header from the phone is dropped; the peer is the Noise static.
    let id = c.open("GET", "/v1/hello", &[("authorization", "Bearer nope"), ("connection", "x")], None, true).await.unwrap();
    let resp = c.read_response(id).await.unwrap();
    assert_eq!(resp.status, 200);
    assert_eq!(resp.json(), json!({ "pid": "phone-1", "authorization": false }));
    assert!(resp.headers.iter().all(|(k, _)| !frame::is_hop_by_hop(k)));
    // Bodies both ways, larger than the initial window.
    let big: Vec<u8> = (0..300_000u32).map(|i| i as u8).collect();
    let id = c.open("POST", "/v1/echo", &[], Some(&big[..200_000]), true).await.unwrap();
    assert_eq!(c.read_response(id).await.unwrap().body, &big[..200_000]);
    let resp = c.request("GET", "/v1/big", None).await.unwrap();
    assert_eq!(resp.body.len(), 600_000);
    // Outside /v1/: 404 without touching the router.
    assert_eq!(c.request("GET", "/admin", None).await.unwrap().status, 404);
    // PING → PONG.
    c.send(Frame::ping(*b"12345678")).await.unwrap();
    let f = c.recv().await.unwrap();
    assert_eq!((f.ty, f.payload.as_slice()), (frame::PONG, &b"12345678"[..]));
}

#[tokio::test]
async fn mux_refuses_even_ids_reused_ids_and_bad_heads() {
    let r = rig();
    let mut c = r.session().await;
    c.send(Frame::new(frame::OPEN, 2, br#"{"method":"GET","path":"/v1/hello"}"#.to_vec())).await.unwrap();
    let f = c.recv_timeout(Duration::from_secs(5)).await.unwrap();
    assert_eq!((f.ty, f.stream, f.code()), (frame::RESET, 2, Some(frame::reset::REFUSED)));
    let id = c.open("GET", "/v1/hello", &[], None, true).await.unwrap();
    c.read_response(id).await.unwrap();
    c.send(Frame::new(frame::OPEN, id, br#"{"method":"GET","path":"/v1/hello"}"#.to_vec())).await.unwrap();
    let f = c.recv_timeout(Duration::from_secs(5)).await.unwrap();
    assert_eq!((f.ty, f.stream, f.code()), (frame::RESET, id, Some(frame::reset::PROTOCOL)));
    c.send(Frame::new(frame::OPEN, 101, b"not json".to_vec())).await.unwrap();
    let f = c.recv_timeout(Duration::from_secs(5)).await.unwrap();
    assert_eq!((f.ty, f.stream, f.code()), (frame::RESET, 101, Some(frame::reset::PROTOCOL)));
    // Unknown frame types are ignored; the session still works.
    c.send(Frame::new(0x7f, 0, vec![1, 2, 3])).await.unwrap();
    c.send(Frame::ping([9; 8])).await.unwrap();
    assert_eq!(c.recv().await.unwrap().ty, frame::PONG);
}

#[tokio::test]
async fn mux_data_beyond_the_window_is_reset_flow_control() {
    let r = rig();
    let mut c = r.session().await;
    // /v1/sink never reads the body, so no WINDOW comes back.
    let id = c.open("POST", "/v1/sink", &[], None, false).await.unwrap();
    let chunk = vec![0u8; frame::MAX_DATA];
    for _ in 0..(frame::INITIAL_WINDOW as usize / frame::MAX_DATA + 1) {
        c.send(Frame::data(id, &chunk)).await.unwrap();
    }
    loop {
        let f = c.recv_timeout(Duration::from_secs(5)).await.unwrap();
        if f.ty == frame::RESET {
            assert_eq!((f.stream, f.code()), (id, Some(frame::reset::FLOW_CONTROL)));
            break;
        }
    }
}

#[tokio::test]
async fn mux_send_window_holds_the_response_until_window_frames() {
    let r = rig();
    let mut c = r.session().await;
    let id = c.open("GET", "/v1/big", &[], None, true).await.unwrap();
    let mut got = 0usize;
    // Without WINDOW the host stops at exactly the initial window.
    while got < frame::INITIAL_WINDOW as usize {
        let f = c.recv_timeout(Duration::from_secs(5)).await.unwrap();
        if f.ty == frame::DATA {
            got += f.payload.len();
        }
    }
    assert_eq!(got, frame::INITIAL_WINDOW as usize);
    assert!(c.recv_timeout(Duration::from_millis(300)).await.is_err(), "host sent past the window");
    c.send(Frame::window(id, 1000)).await.unwrap();
    let f = c.recv_timeout(Duration::from_secs(5)).await.unwrap();
    assert_eq!((f.ty, f.payload.len()), (frame::DATA, 1000));
}

#[tokio::test]
async fn mux_streams_sse_and_cancels_with_reset() {
    let r = rig();
    let mut c = r.session().await;
    let id = c.open("GET", "/v1/sse", &[("accept", "text/event-stream")], None, true).await.unwrap();
    let head = c.recv_timeout(Duration::from_secs(5)).await.unwrap();
    assert_eq!(head.ty, frame::RESPONSE);
    let head: ResponseHead = serde_json::from_slice(&head.payload).unwrap();
    assert!(head.headers.contains(&("content-type".into(), "text/event-stream".into())));
    let mut text = String::new();
    while !text.contains("data: 3") {
        let f = c.recv_timeout(Duration::from_secs(5)).await.unwrap();
        assert_eq!(f.ty, frame::DATA);
        text.push_str(&String::from_utf8_lossy(&f.payload));
    }
    c.send(Frame::reset(id, frame::reset::CANCEL, "")).await.unwrap();
    // After the cancel the stream goes quiet (allow frames already in flight).
    tokio::time::sleep(Duration::from_millis(100)).await;
    while let Ok(f) = c.recv_timeout(Duration::from_millis(100)).await {
        assert_eq!(f.stream, id);
    }
    assert!(c.recv_timeout(Duration::from_millis(200)).await.is_err());
}

#[tokio::test]
async fn mux_caps_concurrent_streams_at_32() {
    let r = rig();
    let mut c = r.session().await;
    let mut ids = Vec::new();
    for _ in 0..frame::MAX_STREAMS {
        ids.push(c.open("GET", "/v1/sse", &[], None, true).await.unwrap());
    }
    let extra = c.open("GET", "/v1/hello", &[], None, true).await.unwrap();
    loop {
        let f = c.recv_timeout(Duration::from_secs(5)).await.unwrap();
        if f.stream == extra {
            assert_eq!((f.ty, f.code()), (frame::RESET, Some(frame::reset::REFUSED)));
            break;
        }
    }
}

#[tokio::test]
async fn session_rejects_unknown_static_and_pid_mismatch_without_msg2() {
    let r = rig();
    // S1: a static the host never paired.
    let (ws, task) = r.accept(session::Mode::Session { pid: "phone-1".into() }, Default::default()).await;
    let err = PhoneConn::handshake(ws, &PhoneKeys::generate(), &r.info).await.err().expect("must fail");
    assert!(testkit::close_code(&err).is_some(), "{err:#}");
    assert!(task.await.unwrap().is_err());
    // S2: the right static, but the relay said another pid.
    let (ws, task) = r.accept(session::Mode::Session { pid: "someone-else".into() }, Default::default()).await;
    assert!(PhoneConn::handshake(ws, &r.phone, &r.info).await.is_err());
    assert!(task.await.unwrap().unwrap_err().to_string().contains("inconsistent"));
    // A revoked phone is S1-rejected too.
    r.host.registry.revoke("phone-1").unwrap();
    let (ws, _) = r.accept(session::Mode::Session { pid: "phone-1".into() }, Default::default()).await;
    assert!(PhoneConn::handshake(ws, &r.phone, &r.info).await.is_err());
}

#[tokio::test]
async fn session_replayed_msg1_executes_nothing() {
    // T-replay: a recorded msg1 gets a msg2, but with no transport message
    // there is no request and no state change.
    let r = rig();
    let (mut ws, task) = r.accept(session::Mode::Session { pid: "phone-1".into() }, Default::default()).await;
    let mut hs = noise::initiator(&r.phone.static_priv, &r.info.hs, &r.info.host_id, None, None).unwrap();
    let mut buf = vec![0u8; 65535];
    let n = hs.write_message(br#"{"v":1}"#, &mut buf).unwrap();
    use futures_util::{SinkExt, StreamExt};
    ws.send(tokio_tungstenite::tungstenite::Message::Binary(buf[..n].to_vec())).await.unwrap();
    assert!(matches!(ws.next().await, Some(Ok(tokio_tungstenite::tungstenite::Message::Binary(_)))));
    // A tampered transport message ends the session (P2).
    ws.send(tokio_tungstenite::tungstenite::Message::Binary(vec![0u8; 40])).await.unwrap();
    let res = tokio::time::timeout(Duration::from_secs(5), task).await.unwrap().unwrap();
    assert!(res.unwrap_err().to_string().contains("decrypt"));
}

#[tokio::test]
async fn session_idles_out_and_ends_at_lifetime() {
    let r = rig();
    let t = session::Timeouts { idle: Duration::from_millis(300), ..Default::default() };
    let (ws, task) = r.accept(session::Mode::Session { pid: "phone-1".into() }, t).await;
    let mut c = PhoneConn::handshake(ws, &r.phone, &r.info).await.unwrap();
    // An open stream keeps an otherwise quiet session alive.
    let id = c.open("GET", "/v1/sse", &[], None, true).await.unwrap();
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(!task.is_finished());
    c.send(Frame::reset(id, 0, "")).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), task).await.expect("idle close").unwrap().unwrap();

    let t = session::Timeouts { lifetime: Duration::from_millis(300), ..Default::default() };
    let (ws, _) = r.accept(session::Mode::Session { pid: "phone-1".into() }, t).await;
    let mut c = PhoneConn::handshake(ws, &r.phone, &r.info).await.unwrap();
    loop {
        let f = c.recv_timeout(Duration::from_secs(3)).await.unwrap();
        if f.ty == frame::GOAWAY {
            assert_eq!(f.code(), Some(frame::goaway::LIFETIME));
            break;
        }
    }
}

#[tokio::test]
async fn revocation_ends_live_sessions_with_goaway() {
    let r = rig();
    let mut c = r.session().await;
    c.open("GET", "/v1/sse", &[], None, true).await.unwrap();
    c.recv_timeout(Duration::from_secs(5)).await.unwrap();
    assert!(r.host.revoke("phone-1").unwrap());
    assert!(!r.host.registry.is_active("phone-1"));
    assert!(r.host.pending_revocations().contains(&"phone-1".to_string()));
    assert!(pending_dir(&r.host.dir).join("revoke-phone-1.json").exists());
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    loop {
        let f = tokio::time::timeout_at(deadline, c.recv()).await.expect("GOAWAY within 1 s").unwrap();
        if f.ty == frame::GOAWAY {
            assert_eq!(f.code(), Some(frame::goaway::REVOKED));
            break;
        }
    }
}

#[tokio::test]
async fn enrollment_binds_window_and_activates_on_first_transport_message() {
    let r = rig();
    let ps = PairingSecret::generate();
    let eid = ps.eid.clone();
    r.host.add_window(PairingSecret::from_bytes(ps.ps), now() + 600);
    let newbie = PhoneKeys::generate();
    // Wrong pairing secret: msg1 fails and counts an attempt.
    let (ws, task) = r.accept(session::Mode::Enroll { eid: eid.clone() }, Default::default()).await;
    let wrong = PairingSecret::generate();
    assert!(PhoneConn::enroll_handshake(ws, &newbie, &r.info, &wrong, "x").await.is_err());
    assert!(task.await.unwrap().is_err());
    // msg1 replayed without a transport message: no record (E4).
    {
        let (mut ws, task) = r.accept(session::Mode::Enroll { eid: eid.clone() }, Default::default()).await;
        let mut hs = noise::initiator(&newbie.static_priv, &r.info.hs, &r.info.host_id, Some(&ps.psk), None).unwrap();
        let mut buf = vec![0u8; 65535];
        let payload = json!({ "v": 1, "name": "n", "pk": b64u(newbie.auth.verifying_key().as_bytes()) }).to_string();
        let n = hs.write_message(payload.as_bytes(), &mut buf).unwrap();
        use futures_util::{SinkExt, StreamExt};
        ws.send(tokio_tungstenite::tungstenite::Message::Binary(buf[..n].to_vec())).await.unwrap();
        assert!(matches!(ws.next().await, Some(Ok(tokio_tungstenite::tungstenite::Message::Binary(_)))));
        drop(ws);
        let _ = task.await;
        assert!(r.host.registry.active_by_static(&newbie.static_pub()).is_none());
    }
    // E2: another static can't use the window now bound to `newbie`.
    let (ws, _) = r.accept(session::Mode::Enroll { eid: eid.clone() }, Default::default()).await;
    assert!(PhoneConn::enroll_handshake(ws, &PhoneKeys::generate(), &r.info, &ps, "thief").await.is_err());
    // The bound phone completes; PING confirms (E4).
    let (ws, _) = r.accept(session::Mode::Enroll { eid: eid.clone() }, Default::default()).await;
    let (mut c, pid, credential) = PhoneConn::enroll_handshake(ws, &newbie, &r.info, &ps, "New Phone").await.unwrap();
    let rec = r.host.registry.get(&pid).expect("record active after PING");
    assert_eq!(rec.name, "New Phone");
    assert_eq!(rec.static_pub, b64u(newbie.static_pub()));
    let claims = cred::verify(&credential, r.host.host_id(), Some((r.host.host_id(), &r.host.keys.sign_pub())), now()).unwrap();
    assert_eq!((claims.pid.as_str(), claims.pk), (pid.as_str(), newbie.auth.verifying_key().to_bytes()));
    assert_eq!(rec.cred_exp, claims.exp);
    // The window is consumed: a second enrollment with the same ps fails.
    assert!(r.host.window_psk(&eid).is_none());
    // And the API works on the enrollment connection.
    let resp = c.request("GET", "/v1/hello", None).await.unwrap();
    assert_eq!(resp.json()["pid"], json!(pid));
}

#[tokio::test]
async fn window_closes_after_five_failed_attempts() {
    let r = rig();
    let ps = PairingSecret::generate();
    r.host.add_window(PairingSecret::from_bytes(ps.ps), now() + 600);
    for _ in 0..MAX_WINDOW_ATTEMPTS {
        let (ws, task) = r.accept(session::Mode::Enroll { eid: ps.eid.clone() }, Default::default()).await;
        let _ = PhoneConn::enroll_handshake(ws, &PhoneKeys::generate(), &r.info, &PairingSecret::generate(), "x").await;
        let _ = task.await;
    }
    assert!(r.host.window_psk(&ps.eid).is_none());
}

#[test]
fn pending_hand_off_picks_up_windows_and_revocations() {
    let tmp = tempfile::tempdir().unwrap();
    let host = TunnelHost::open(tmp.path(), RelaySettings::new("https://relay.example.com", None, false).unwrap()).unwrap();
    let (secret, _) = create_window(&host.dir, 600).unwrap();
    host.poll_pending();
    assert!(host.window_psk(&secret.eid).is_some());
    // An expired window file is dropped.
    write_json(&pending_dir(&host.dir).join("enroll-old.json"), &WindowFile { ps: b64u([1u8; 32]), exp: 1 }).unwrap();
    host.poll_pending();
    assert!(!pending_dir(&host.dir).join("enroll-old.json").exists());
    // `hive phones revoke` from another process.
    let phone = PhoneKeys::generate();
    host.registry.activate(record("abc", &phone)).unwrap();
    let token = tmp.path().join("serve.token");
    crate::serve::pair::load_or_create_token(&token).unwrap();
    let before = std::fs::read_to_string(&token).unwrap();
    cli::phones_revoke(tmp.path(), Some("test phone"), &token, false).unwrap();
    assert_ne!(std::fs::read_to_string(&token).unwrap(), before, "token rotated");
    assert!(host.registry.is_active("abc"), "in-memory until picked up");
    host.poll_pending();
    assert!(!host.registry.is_active("abc"));
    assert_eq!(host.pending_revocations(), vec!["abc".to_string()]);
    // --keep-token leaves the token alone.
    host.registry.activate(record("def", &PhoneKeys::generate())).unwrap();
    let t2 = std::fs::read_to_string(&token).unwrap();
    cli::phones_revoke(tmp.path(), Some("def"), &token, true).unwrap();
    assert_eq!(std::fs::read_to_string(&token).unwrap(), t2);
}
