//! One phone session on an accept connection: the Noise responder (§7), then
//! the stream mux (§8) carrying HTTP requests into the axum router (§9).

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use axum::body::{Body, Bytes};
use axum::http::{self, StatusCode};
use axum::Router;
use futures_util::{Sink, SinkExt, Stream, StreamExt};
use serde::Deserialize;
use serde_json::json;
use snow::TransportState;
use tokio::sync::{mpsc, watch, Semaphore};
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tower::ServiceExt;

use super::frame::{self, goaway, reset, Frame, RequestHead, ResponseHead};
use super::{b64u, b64u_fixed, cred, hkdf, noise, now, random_id, PhoneRecord, TunnelHost, TunnelPeer};

/// Handshake must complete within this long of `ready` (§10).
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// Close a session with no frame from the phone (and no open stream) (§10).
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(90);
/// Session lifetime (§7.4).
pub const MAX_LIFETIME: Duration = Duration::from_secs(24 * 3600);
const MAX_TRANSPORT_MESSAGES: u64 = 1 << 32;

/// What the relay's `open` asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Session { pid: String },
    Enroll { eid: String },
}

/// Knobs the tests shorten.
#[derive(Debug, Clone, Copy)]
pub struct Timeouts {
    pub handshake: Duration,
    pub idle: Duration,
    pub lifetime: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self { handshake: HANDSHAKE_TIMEOUT, idle: IDLE_TIMEOUT, lifetime: MAX_LIFETIME }
    }
}

/// An enrollment that becomes a phone record at the first transport message (E4).
struct PendingEnroll {
    rec: PhoneRecord,
    eid: String,
}

#[derive(Deserialize)]
struct EnrollHello {
    v: u32,
    #[serde(default)]
    name: String,
    pk: String,
}

#[derive(Deserialize)]
struct SessionHello {
    v: u32,
}

async fn next_binary<S>(rx: &mut S) -> Result<Vec<u8>>
where
    S: Stream<Item = Result<Message, WsError>> + Unpin,
{
    loop {
        match rx.next().await {
            Some(Ok(Message::Binary(b))) => return Ok(b),
            Some(Ok(Message::Close(c))) => bail!("closed before the handshake ({c:?})"),
            Some(Ok(_)) => continue,
            Some(Err(e)) => return Err(e.into()),
            None => bail!("closed before the handshake"),
        }
    }
}

/// Run one accepted connection to the end. Errors are already logged-worthy
/// descriptions; the connection is closed either way.
pub async fn run<S>(host: Arc<TunnelHost>, router: Router, ws: S, mode: Mode, stop: watch::Receiver<bool>, t: Timeouts) -> Result<()>
where
    S: Stream<Item = Result<Message, WsError>> + Sink<Message, Error = WsError> + Unpin + Send + 'static,
{
    let (mut tx, mut rx) = ws.split();
    let res = async {
        let msg1 = tokio::time::timeout(t.handshake, next_binary(&mut rx)).await.map_err(|_| anyhow!("handshake timed out"))??;
        let keys = &host.keys;
        let mut buf = vec![0u8; noise::MAX_MSG];
        match &mode {
            Mode::Session { pid } => {
                let mut hs = noise::responder(keys.static_priv(), keys.host_id(), None)?;
                let n = hs.read_message(&msg1, &mut buf).map_err(|e| anyhow!("session msg1 rejected: {e}"))?;
                let hello: SessionHello = serde_json::from_slice(&buf[..n]).context("session msg1 payload")?;
                if hello.v != 1 {
                    bail!("unsupported session version {}", hello.v);
                }
                let rs: [u8; 32] = hs.get_remote_static().and_then(|s| s.try_into().ok()).ok_or_else(|| anyhow!("no remote static"))?;
                // S1: the static must belong to an active phone.
                let rec = host.registry.active_by_static(&rs).ok_or_else(|| anyhow!("unknown or revoked phone static {}", b64u(rs)))?;
                // S2: and match the pid the relay admitted.
                if rec.pid != *pid {
                    bail!("relay opened pid {pid} but the Noise static belongs to {}: relay or credential inconsistent", rec.pid);
                }
                let n = hs.write_message(br#"{"v":1}"#, &mut buf)?;
                tx.send(Message::Binary(buf[..n].to_vec())).await?;
                let ts = hs.into_transport_mode()?;
                Ok((ts, rec.pid, None))
            }
            Mode::Enroll { eid } => {
                // E1: an open, unexpired window.
                let psk = host.window_psk(eid).ok_or_else(|| anyhow!("no open pairing window {eid}"))?;
                let mut hs = noise::responder(keys.static_priv(), keys.host_id(), Some(&psk))?;
                let n = match hs.read_message(&msg1, &mut buf) {
                    Ok(n) => n,
                    Err(e) => {
                        host.window_failed(eid);
                        bail!("enrollment msg1 rejected (wrong pairing secret?): {e}");
                    }
                };
                let rs: [u8; 32] = hs.get_remote_static().and_then(|s| s.try_into().ok()).ok_or_else(|| anyhow!("no remote static"))?;
                // E2: bound to the first static.
                if !host.window_bind(eid, rs) {
                    host.window_failed(eid);
                    bail!("pairing window {eid} is bound to another phone");
                }
                let hello: EnrollHello = match serde_json::from_slice(&buf[..n]) {
                    Ok(h) => h,
                    Err(e) => {
                        host.window_failed(eid);
                        bail!("enrollment payload: {e}");
                    }
                };
                let pk: [u8; 32] = b64u_fixed(&hello.pk).context("enrollment pk")?;
                if hello.v != 1 {
                    bail!("unsupported enrollment version {}", hello.v);
                }
                let name: String = hello.name.trim().chars().filter(|c| !c.is_control()).take(64).collect();
                // E3: same static as an active phone → keep its pid.
                let pid = host.registry.active_by_static(&rs).map(|r| r.pid).unwrap_or_else(random_id);
                let (credential, exp) = cred::issue(keys, &pid, &pk, now());
                let payload = format!(
                    "{{\"v\":1,\"pid\":{},\"cred\":{}}}",
                    serde_json::Value::from(pid.as_str()),
                    serde_json::Value::from(credential.as_str())
                );
                let n = hs.write_message(payload.as_bytes(), &mut buf)?;
                let push_key = hkdf(hs.get_handshake_hash(), "push-key", 32);
                tx.send(Message::Binary(buf[..n].to_vec())).await?;
                let ts = hs.into_transport_mode()?;
                let rec = PhoneRecord {
                    pid: pid.clone(),
                    name: if name.is_empty() { "iPhone".into() } else { name },
                    auth_pub: b64u(pk),
                    static_pub: b64u(rs),
                    push_key: b64u(push_key),
                    enrolled_at: now(),
                    cred_exp: exp,
                    revoked_at: None,
                    push_enabled: false,
                    push_seq: 0,
                    last_seen: None,
                };
                Ok((ts, pid, Some(PendingEnroll { rec, eid: eid.clone() })))
            }
        }
    }
    .await;
    let (ts, pid, pending) = match res {
        Ok(v) => v,
        Err(e) => {
            // Close without msg2 (S1, E1): a normal close, nothing more to say.
            let _ = tx.send(Message::Close(Some(CloseFrame { code: CloseCode::Normal, reason: "".into() }))).await;
            return Err(e);
        }
    };
    if pending.is_none() {
        host.registry.touch(&pid);
    }
    mux(host, router, tx, rx, ts, pid, pending, stop, t).await
}

enum Out {
    Frame(Frame),
    Close,
}

/// Per-stream state the reader keeps.
struct StreamCtl {
    recv_window: Arc<AtomicI64>,
    send_window: Arc<Semaphore>,
    body_tx: Option<mpsc::UnboundedSender<Bytes>>,
    task: tokio::task::AbortHandle,
    req_fin: bool,
    resp_done: bool,
}

#[allow(clippy::too_many_arguments)]
async fn mux<Tx, Rx>(
    host: Arc<TunnelHost>,
    router: Router,
    mut tx: Tx,
    mut rx: Rx,
    ts: TransportState,
    pid: String,
    mut pending: Option<PendingEnroll>,
    mut stop: watch::Receiver<bool>,
    t: Timeouts,
) -> Result<()>
where
    Tx: Sink<Message, Error = WsError> + Unpin + Send + 'static,
    Rx: Stream<Item = Result<Message, WsError>> + Unpin + Send + 'static,
{
    let ts = Arc::new(Mutex::new(ts));
    // Writer: encrypts in send order, one Noise message per WebSocket message.
    let (out, mut out_rx) = mpsc::channel::<Out>(64);
    let wts = ts.clone();
    let writer = tokio::spawn(async move {
        let mut buf = vec![0u8; noise::MAX_MSG];
        let mut sent: u64 = 0;
        while let Some(o) = out_rx.recv().await {
            match o {
                Out::Frame(f) => {
                    let pt = f.encode();
                    let n = match wts.lock().unwrap_or_else(|e| e.into_inner()).write_message(&pt, &mut buf) {
                        Ok(n) => n,
                        Err(_) => break,
                    };
                    if tx.send(Message::Binary(buf[..n].to_vec())).await.is_err() {
                        return;
                    }
                    sent += 1;
                    if sent >= MAX_TRANSPORT_MESSAGES - 1 {
                        break;
                    }
                }
                Out::Close => break,
            }
        }
        let _ = tx.send(Message::Close(Some(CloseFrame { code: CloseCode::Normal, reason: "".into() }))).await;
        let _ = tx.close().await;
    });

    let mut streams: HashMap<u32, StreamCtl> = HashMap::new();
    let (done_tx, mut done_rx) = mpsc::unbounded_channel::<u32>();
    let mut revoked = host.revoked.subscribe();
    let mut last_open: u32 = 0;
    let mut last_activity = Instant::now();
    let lifetime = tokio::time::sleep(t.lifetime);
    tokio::pin!(lifetime);
    let mut received: u64 = 0;
    let mut buf = vec![0u8; noise::MAX_MSG];
    let mut goaway_code: Option<(u32, &str)> = None;
    let mut result = Ok(());

    'session: loop {
        let idle = tokio::time::sleep_until(last_activity + t.idle);
        tokio::select! {
            m = rx.next() => {
                let bytes = match m {
                    Some(Ok(Message::Binary(b))) => b,
                    Some(Ok(Message::Close(_))) | None => break 'session,
                    Some(Ok(_)) => continue,
                    Some(Err(e)) => { result = Err(anyhow!("websocket: {e}")); break 'session; }
                };
                last_activity = Instant::now();
                // P2: any decrypt failure ends the session.
                let n = match ts.lock().unwrap_or_else(|e| e.into_inner()).read_message(&bytes, &mut buf) {
                    Ok(n) => n,
                    Err(e) => { result = Err(anyhow!("transport decrypt failed: {e}")); break 'session; }
                };
                received += 1;
                if received >= MAX_TRANSPORT_MESSAGES - 1 {
                    goaway_code = Some((goaway::LIFETIME, "message limit"));
                    break 'session;
                }
                // E4: the first valid transport message confirms an enrollment.
                if let Some(p) = pending.take() {
                    let name = p.rec.name.clone();
                    if let Err(e) = host.registry.activate(p.rec) {
                        result = Err(e.context("saving the new phone"));
                        break 'session;
                    }
                    host.close_window(&p.eid);
                    println!("remote: new phone paired: {name} (pid {pid})");
                    host.push_all("phone.paired", None, "New phone paired", &name);
                }
                let Ok(f) = Frame::decode(&buf[..n]) else {
                    goaway_code = Some((goaway::PROTOCOL, "short frame"));
                    break 'session;
                };
                match handle_frame(f, &mut streams, &mut last_open, &out, &router, &pid, &done_tx).await {
                    Flow::Continue => {}
                    Flow::Goaway(code, why) => { goaway_code = Some((code, why)); break 'session; }
                    Flow::Close => break 'session,
                }
            }
            Some(id) = done_rx.recv() => {
                if let Some(s) = streams.get_mut(&id) {
                    s.resp_done = true;
                    if s.req_fin {
                        streams.remove(&id);
                        last_activity = Instant::now();
                    }
                }
            }
            r = revoked.recv() => {
                let gone = match r {
                    Ok(p) => p == pid,
                    Err(_) => !host.registry.is_active(&pid) && pending.is_none(),
                };
                if gone {
                    goaway_code = Some((goaway::REVOKED, "revoked"));
                    break 'session;
                }
            }
            _ = &mut lifetime => { goaway_code = Some((goaway::LIFETIME, "lifetime")); break 'session; }
            _ = idle, if streams.is_empty() => { goaway_code = Some((goaway::NORMAL, "idle")); break 'session; }
            _ = super::stopped(&mut stop) => { goaway_code = Some((goaway::SHUTTING_DOWN, "shutting down")); break 'session; }
        }
    }
    for (_, s) in streams.drain() {
        s.task.abort();
    }
    if let Some((code, why)) = goaway_code {
        let _ = out.send(Out::Frame(Frame::goaway(code, why))).await;
    }
    let _ = out.send(Out::Close).await;
    drop(out);
    let _ = tokio::time::timeout(Duration::from_secs(5), writer).await;
    // Erase the session keys (§7.4): drop the last reference now.
    drop(ts);
    result
}

enum Flow {
    Continue,
    Goaway(u32, &'static str),
    Close,
}

async fn send(out: &mpsc::Sender<Out>, f: Frame) {
    let _ = out.send(Out::Frame(f)).await;
}

fn drop_stream(streams: &mut HashMap<u32, StreamCtl>, id: u32) {
    if let Some(s) = streams.remove(&id) {
        s.task.abort();
    }
}

async fn handle_frame(
    f: Frame,
    streams: &mut HashMap<u32, StreamCtl>,
    last_open: &mut u32,
    out: &mpsc::Sender<Out>,
    router: &Router,
    pid: &str,
    done_tx: &mpsc::UnboundedSender<u32>,
) -> Flow {
    use frame::*;
    let id = f.stream;
    let stream_frame = matches!(f.ty, OPEN | DATA | FIN | RESET | RESPONSE | WINDOW);
    if stream_frame && id == 0 {
        return Flow::Goaway(goaway::PROTOCOL, "stream frame on stream 0");
    }
    match f.ty {
        OPEN => {
            if id & 1 == 0 {
                send(out, Frame::reset(id, reset::REFUSED, "even stream ids are reserved")).await;
                return Flow::Continue;
            }
            if id <= *last_open {
                send(out, Frame::reset(id, reset::PROTOCOL, "stream id reused")).await;
                return Flow::Continue;
            }
            *last_open = id;
            if streams.len() >= MAX_STREAMS {
                send(out, Frame::reset(id, reset::REFUSED, "too many streams")).await;
                return Flow::Continue;
            }
            let head: RequestHead = match (f.payload.len() <= MAX_HEAD).then(|| serde_json::from_slice(&f.payload)) {
                Some(Ok(h)) => h,
                _ => {
                    send(out, Frame::reset(id, reset::PROTOCOL, "bad request head")).await;
                    return Flow::Continue;
                }
            };
            let recv_window = Arc::new(AtomicI64::new(INITIAL_WINDOW as i64));
            let send_window = Arc::new(Semaphore::new(INITIAL_WINDOW as usize));
            let (body_tx, body_rx) = mpsc::unbounded_channel();
            let task = tokio::spawn(serve_stream(
                id,
                head,
                router.clone(),
                pid.to_string(),
                out.clone(),
                body_rx,
                recv_window.clone(),
                send_window.clone(),
                done_tx.clone(),
            ));
            streams.insert(
                id,
                StreamCtl { recv_window, send_window, body_tx: Some(body_tx), task: task.abort_handle(), req_fin: false, resp_done: false },
            );
        }
        DATA => {
            let Some(s) = streams.get_mut(&id) else { return Flow::Continue };
            let len = f.payload.len();
            if len == 0 || len > MAX_DATA || s.req_fin {
                drop_stream(streams, id);
                send(out, Frame::reset(id, reset::PROTOCOL, "bad DATA")).await;
                return Flow::Continue;
            }
            let before = s.recv_window.fetch_sub(len as i64, Ordering::SeqCst);
            if before < len as i64 {
                drop_stream(streams, id);
                send(out, Frame::reset(id, reset::FLOW_CONTROL, "window exceeded")).await;
                return Flow::Continue;
            }
            if let Some(b) = &s.body_tx {
                let _ = b.send(Bytes::from(f.payload));
            }
        }
        FIN => {
            let Some(s) = streams.get_mut(&id) else { return Flow::Continue };
            s.req_fin = true;
            s.body_tx = None;
            if s.resp_done {
                streams.remove(&id);
            }
        }
        RESET => drop_stream(streams, id),
        WINDOW => {
            let Some(s) = streams.get(&id) else { return Flow::Continue };
            let inc = match (f.payload.len(), f.code()) {
                (4, Some(inc)) if (1..=0x7fff_ffff).contains(&inc) => inc as usize,
                _ => {
                    drop_stream(streams, id);
                    send(out, Frame::reset(id, reset::PROTOCOL, "bad WINDOW")).await;
                    return Flow::Continue;
                }
            };
            if s.send_window.available_permits() + inc > 0x7fff_ffff {
                drop_stream(streams, id);
                send(out, Frame::reset(id, reset::FLOW_CONTROL, "window overflow")).await;
                return Flow::Continue;
            }
            s.send_window.add_permits(inc);
        }
        RESPONSE => {
            if streams.contains_key(&id) {
                drop_stream(streams, id);
                send(out, Frame::reset(id, reset::PROTOCOL, "RESPONSE from the phone")).await;
            }
        }
        PING => {
            if id != 0 || f.payload.len() != 8 {
                return Flow::Goaway(goaway::PROTOCOL, "bad PING");
            }
            send(out, Frame::pong(&f.payload)).await;
        }
        GOAWAY => return Flow::Close,
        // PONG, and unknown types (forward compatibility).
        _ => {}
    }
    Flow::Continue
}

fn simple_response(status: StatusCode, msg: &str) -> http::Response<Body> {
    http::Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Body::from(json!({ "error": msg }).to_string()))
        .expect("static response")
}

/// Build the in-process request for a head; `None` = malformed.
fn build_request(head: &RequestHead, body: Body, pid: &str) -> Option<http::Request<Body>> {
    let method = http::Method::from_bytes(head.method.as_bytes()).ok()?;
    let uri: http::Uri = head.path.parse().ok()?;
    let mut b = http::Request::builder().method(method).uri(uri);
    for (k, v) in &head.headers {
        let k = k.to_ascii_lowercase();
        // The phone authenticates by its Noise static, never a bearer token.
        if frame::is_hop_by_hop(&k) || k == "authorization" {
            continue;
        }
        b = b.header(k, v);
    }
    let mut req = b.body(body).ok()?;
    req.extensions_mut().insert(TunnelPeer { pid: pid.to_string() });
    Some(req)
}

#[allow(clippy::too_many_arguments)]
async fn serve_stream(
    id: u32,
    head: RequestHead,
    router: Router,
    pid: String,
    out: mpsc::Sender<Out>,
    body_rx: mpsc::UnboundedReceiver<Bytes>,
    recv_window: Arc<AtomicI64>,
    send_window: Arc<Semaphore>,
    done_tx: mpsc::UnboundedSender<u32>,
) {
    // Request body: DATA as it arrives; WINDOW back once half is consumed.
    let wout = out.clone();
    let body = futures_util::stream::unfold((body_rx, 0u32), move |(mut rx, mut consumed)| {
        let wout = wout.clone();
        let win = recv_window.clone();
        async move {
            let chunk = rx.recv().await?;
            consumed += chunk.len() as u32;
            if consumed >= frame::INITIAL_WINDOW / 2 {
                win.fetch_add(consumed as i64, Ordering::SeqCst);
                let _ = wout.send(Out::Frame(Frame::window(id, consumed))).await;
                consumed = 0;
            }
            Some((Ok::<_, Infallible>(chunk), (rx, consumed)))
        }
    });
    // §9: anything outside /v1/ is a 404; the router is never consulted.
    let resp = if !head.path.starts_with("/v1/") {
        simple_response(StatusCode::NOT_FOUND, "not found")
    } else {
        match build_request(&head, Body::from_stream(body), &pid) {
            Some(req) => match router.oneshot(req).await {
                Ok(r) => r,
                Err(e) => match e {},
            },
            None => simple_response(StatusCode::BAD_REQUEST, "malformed request head"),
        }
    };
    let (parts, body) = resp.into_parts();
    let headers = parts
        .headers
        .iter()
        .filter(|(k, _)| !frame::is_hop_by_hop(k.as_str()))
        .filter_map(|(k, v)| Some((k.as_str().to_string(), v.to_str().ok()?.to_string())))
        .collect();
    let head = ResponseHead { status: parts.status.as_u16(), headers };
    if out.send(Out::Frame(Frame::response(id, &head))).await.is_err() {
        return;
    }
    let mut body = body.into_data_stream();
    while let Some(chunk) = body.next().await {
        let chunk = match chunk {
            Ok(c) => c,
            Err(_) => {
                let _ = out.send(Out::Frame(Frame::reset(id, reset::INTERNAL, "response body failed"))).await;
                let _ = done_tx.send(id);
                return;
            }
        };
        let mut rest = &chunk[..];
        while !rest.is_empty() {
            // Per-stream flow control: wait for at least one byte of window,
            // then send as much as the window (and a DATA frame) allows.
            let Ok(first) = send_window.acquire().await else { return };
            first.forget();
            let want = rest.len().min(frame::MAX_DATA) - 1;
            let more = want.min(send_window.available_permits());
            if more > 0 {
                match send_window.try_acquire_many(more as u32) {
                    Ok(p) => p.forget(),
                    Err(_) => return,
                }
            }
            let (piece, tail) = rest.split_at(1 + more);
            rest = tail;
            if out.send(Out::Frame(Frame::data(id, piece))).await.is_err() {
                return;
            }
        }
    }
    let _ = out.send(Out::Frame(Frame::fin(id))).await;
    let _ = done_tx.send(id);
}
