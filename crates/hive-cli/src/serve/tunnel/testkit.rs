//! Test-only peers for the tunnel: a fake phone (Noise initiator via snow +
//! the mux client side) and a minimal fake relay (control, connect, accept
//! and the splice), both over real WebSockets.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use futures_util::{Sink, SinkExt, Stream, StreamExt};
use serde_json::{json, Value};
use snow::TransportState;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::WebSocketStream;

use super::frame::{self, Frame, RequestHead, ResponseHead};
use super::keys::{host_id_for, x25519_pub};
use super::{b64u, b64u_fixed, cred, ctx, noise, random_bytes, random_id, sha256, PairingSecret, SUBPROTOCOL};

/// A relay close: `(code, reason)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Closed(pub u16, pub String);

impl std::fmt::Display for Closed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "closed {} {}", self.0, self.1)
    }
}
impl std::error::Error for Closed {}

pub fn close_code(e: &anyhow::Error) -> Option<u16> {
    e.downcast_ref::<Closed>().map(|c| c.0)
}

// ── Fake phone ───────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct PhoneKeys {
    pub auth: SigningKey,
    pub static_priv: [u8; 32],
}

impl PhoneKeys {
    pub fn generate() -> Self {
        Self { auth: SigningKey::from_bytes(&random_bytes()), static_priv: random_bytes() }
    }
    pub fn static_pub(&self) -> [u8; 32] {
        x25519_pub(&self.static_priv)
    }
}

/// What the phone learned from a pairing link.
#[derive(Clone)]
pub struct HostInfo {
    pub relay: String,
    pub host_id: String,
    pub hs: [u8; 32],
}

pub struct Response1 {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response1 {
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

/// A phone end of an established Noise session, over any WebSocket.
pub struct PhoneConn<S> {
    pub ws: S,
    ts: TransportState,
    next_id: u32,
    buf: Vec<u8>,
}

async fn next_text<S>(ws: &mut S) -> Result<Value>
where
    S: Stream<Item = Result<Message, WsError>> + Unpin,
{
    loop {
        match ws.next().await {
            Some(Ok(Message::Text(t))) => return Ok(serde_json::from_str(&t)?),
            Some(Ok(Message::Close(Some(f)))) => return Err(Closed(f.code.into(), f.reason.to_string()).into()),
            Some(Ok(Message::Close(None))) | None => return Err(Closed(1005, String::new()).into()),
            Some(Ok(_)) => {}
            Some(Err(e)) => return Err(e.into()),
        }
    }
}

async fn next_bin<S>(ws: &mut S) -> Result<Vec<u8>>
where
    S: Stream<Item = Result<Message, WsError>> + Unpin,
{
    loop {
        match ws.next().await {
            Some(Ok(Message::Binary(b))) => return Ok(b),
            Some(Ok(Message::Close(Some(f)))) => return Err(Closed(f.code.into(), f.reason.to_string()).into()),
            Some(Ok(Message::Close(None))) | None => return Err(Closed(1005, String::new()).into()),
            Some(Ok(_)) => {}
            Some(Err(e)) => return Err(e.into()),
        }
    }
}

/// Wait for `ready` (or the relay's error close).
async fn await_ready<S>(ws: &mut S) -> Result<()>
where
    S: Stream<Item = Result<Message, WsError>> + Unpin,
{
    loop {
        let m = next_text(ws).await?;
        if m["t"] == "ready" {
            return Ok(());
        }
    }
}

impl<S> PhoneConn<S>
where
    S: Stream<Item = Result<Message, WsError>> + Sink<Message, Error = WsError> + Unpin,
{
    /// Session handshake (IK) on an already-spliced WebSocket.
    pub async fn handshake(mut ws: S, keys: &PhoneKeys, host: &HostInfo) -> Result<Self> {
        let mut hs = noise::initiator(&keys.static_priv, &host.hs, &host.host_id, None, None)?;
        let mut buf = vec![0u8; noise::MAX_MSG];
        let n = hs.write_message(br#"{"v":1}"#, &mut buf)?;
        ws.send(Message::Binary(buf[..n].to_vec())).await?;
        let msg2 = next_bin(&mut ws).await?;
        let mut out = vec![0u8; noise::MAX_MSG];
        hs.read_message(&msg2, &mut out).map_err(|e| anyhow!("msg2: {e}"))?;
        Ok(Self { ws, ts: hs.into_transport_mode()?, next_id: 1, buf })
    }

    /// Enrollment handshake (IKpsk1). Returns the conn, pid and credential.
    pub async fn enroll_handshake(mut ws: S, keys: &PhoneKeys, host: &HostInfo, ps: &PairingSecret, name: &str) -> Result<(Self, String, String)> {
        let mut hs = noise::initiator(&keys.static_priv, &host.hs, &host.host_id, Some(&ps.psk), None)?;
        let mut buf = vec![0u8; noise::MAX_MSG];
        let payload = json!({ "v": 1, "name": name, "pk": b64u(keys.auth.verifying_key().as_bytes()) }).to_string();
        let n = hs.write_message(payload.as_bytes(), &mut buf)?;
        ws.send(Message::Binary(buf[..n].to_vec())).await?;
        let msg2 = next_bin(&mut ws).await?;
        let mut out = vec![0u8; noise::MAX_MSG];
        let m = hs.read_message(&msg2, &mut out).map_err(|e| anyhow!("msg2: {e}"))?;
        let v: Value = serde_json::from_slice(&out[..m])?;
        let pid = v["pid"].as_str().ok_or_else(|| anyhow!("no pid"))?.to_string();
        let credential = v["cred"].as_str().ok_or_else(|| anyhow!("no cred"))?.to_string();
        let mut conn = Self { ws, ts: hs.into_transport_mode()?, next_id: 1, buf };
        // E4: the first transport message confirms the enrollment.
        conn.send(Frame::ping(*b"pairing!")).await?;
        let pong = conn.recv().await?;
        if pong.ty != frame::PONG || pong.payload != b"pairing!" {
            bail!("expected PONG, got {pong:?}");
        }
        Ok((conn, pid, credential))
    }

    pub async fn send(&mut self, f: Frame) -> Result<()> {
        let n = self.ts.write_message(&f.encode(), &mut self.buf)?;
        self.ws.send(Message::Binary(self.buf[..n].to_vec())).await?;
        Ok(())
    }

    pub async fn recv(&mut self) -> Result<Frame> {
        let b = next_bin(&mut self.ws).await?;
        let mut out = vec![0u8; noise::MAX_MSG];
        let n = self.ts.read_message(&b, &mut out).map_err(|e| anyhow!("decrypt: {e}"))?;
        Frame::decode(&out[..n]).map_err(|_| anyhow!("short frame"))
    }

    pub async fn recv_timeout(&mut self, d: Duration) -> Result<Frame> {
        tokio::time::timeout(d, self.recv()).await.map_err(|_| anyhow!("no frame within {d:?}"))?
    }

    /// Open a stream: OPEN (+ DATA) (+ FIN). Returns its id.
    pub async fn open(&mut self, method: &str, path: &str, headers: &[(&str, &str)], body: Option<&[u8]>, fin: bool) -> Result<u32> {
        let id = self.next_id;
        self.next_id += 2;
        let head = RequestHead {
            method: method.into(),
            path: path.into(),
            headers: headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        };
        self.send(Frame::open(id, &head)).await?;
        if let Some(b) = body {
            for c in b.chunks(frame::MAX_DATA) {
                self.send(Frame::data(id, c)).await?;
            }
        }
        if fin {
            self.send(Frame::fin(id)).await?;
        }
        Ok(id)
    }

    /// Read stream `id` to its FIN, granting WINDOW as the body arrives.
    pub async fn read_response(&mut self, id: u32) -> Result<Response1> {
        let mut head: Option<ResponseHead> = None;
        let mut body = Vec::new();
        let mut consumed = 0u32;
        loop {
            let f = self.recv_timeout(Duration::from_secs(10)).await?;
            if f.stream != id {
                continue;
            }
            match f.ty {
                frame::RESPONSE => head = Some(serde_json::from_slice(&f.payload)?),
                frame::DATA => {
                    body.extend_from_slice(&f.payload);
                    consumed += f.payload.len() as u32;
                    if consumed >= frame::INITIAL_WINDOW / 2 {
                        self.send(Frame::window(id, consumed)).await?;
                        consumed = 0;
                    }
                }
                frame::FIN => break,
                frame::RESET => bail!("stream {id} reset with code {:?}", f.code()),
                _ => {}
            }
        }
        let head = head.ok_or_else(|| anyhow!("FIN before RESPONSE"))?;
        Ok(Response1 { status: head.status, headers: head.headers, body })
    }

    pub async fn request(&mut self, method: &str, path: &str, body: Option<Value>) -> Result<Response1> {
        let b = body.map(|v| v.to_string());
        let headers: &[(&str, &str)] = if b.is_some() { &[("content-type", "application/json")] } else { &[] };
        let id = self.open(method, path, headers, b.as_deref().map(str::as_bytes), true).await?;
        self.read_response(id).await
    }
}

pub type ClientWs = WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>;

/// `GET /v1/tunnel/connect?host=…` up to the challenge nonce.
async fn phone_dial(host: &HostInfo) -> Result<(ClientWs, [u8; 32])> {
    let base = host.relay.replacen("http://", "ws://", 1).replacen("https://", "wss://", 1);
    let mut ws = super::control::dial(&format!("{base}/v1/tunnel/connect?host={}", host.host_id)).await?;
    let ch = next_text(&mut ws).await?;
    let nonce = b64u_fixed(ch["nonce"].as_str().unwrap_or_default())?;
    Ok((ws, nonce))
}

/// Enroll through the relay with the pairing secret from a link.
pub async fn phone_enroll(host: &HostInfo, keys: &PhoneKeys, ps: &PairingSecret, name: &str) -> Result<(PhoneConn<ClientWs>, String, String)> {
    let (mut ws, _) = phone_dial(host).await?;
    ws.send(Message::Text(json!({ "t": "enroll", "eid": ps.eid, "tag": b64u(ps.tag) }).to_string())).await?;
    await_ready(&mut ws).await?;
    PhoneConn::enroll_handshake(ws, keys, host, ps, name).await
}

/// Open a session through the relay with a credential.
pub async fn phone_session(host: &HostInfo, keys: &PhoneKeys, credential: &str) -> Result<PhoneConn<ClientWs>> {
    let (mut ws, nonce) = phone_dial(host).await?;
    let sig = keys.auth.sign(&ctx("phone-auth", &[host.host_id.as_bytes(), &nonce]));
    ws.send(Message::Text(json!({ "t": "auth", "cred": credential, "sig": b64u(sig.to_bytes()) }).to_string())).await?;
    await_ready(&mut ws).await?;
    PhoneConn::handshake(ws, keys, host).await
}

// ── In-memory WebSocket pair (no relay) ──────────────────────────────────────

/// Two connected WebSockets over an in-memory pipe: (client side, server side).
pub async fn ws_pair() -> (WebSocketStream<tokio::io::DuplexStream>, WebSocketStream<tokio::io::DuplexStream>) {
    use tokio_tungstenite::tungstenite::protocol::Role;
    let (a, b) = tokio::io::duplex(1 << 20);
    let client = WebSocketStream::from_raw_socket(a, Role::Client, None).await;
    let server = WebSocketStream::from_raw_socket(b, Role::Server, None).await;
    (client, server)
}

// ── Fake relay ───────────────────────────────────────────────────────────────

struct HostConn {
    host_id: String,
    key: VerifyingKey,
    tx: mpsc::UnboundedSender<String>,
}

#[derive(Default)]
pub struct RelayState {
    host: Mutex<Option<HostConn>>,
    accepts: Mutex<HashMap<String, oneshot::Sender<WebSocketStream<TcpStream>>>>,
    windows: Mutex<HashMap<String, String>>,
    pub revoked: Mutex<HashSet<String>>,
    /// Every JSON message a host sent on its control connection.
    pub log: Mutex<Vec<Value>>,
}

pub struct FakeRelay {
    pub url: String,
    pub state: Arc<RelayState>,
    log_tx: broadcast::Sender<Value>,
}

impl FakeRelay {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(RelayState::default());
        let (log_tx, _) = broadcast::channel(256);
        let (st, lt) = (state.clone(), log_tx.clone());
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let (st, lt) = (st.clone(), lt.clone());
                tokio::spawn(async move {
                    let _ = conn(tcp, st, lt).await;
                });
            }
        });
        Self { url, state, log_tx }
    }

    pub fn host_connected(&self) -> bool {
        self.state.host.lock().unwrap().is_some()
    }

    pub fn messages(&self, t: &str) -> Vec<Value> {
        self.state.log.lock().unwrap().iter().filter(|m| m["t"] == t).cloned().collect()
    }

    /// Wait for the host to send a `t` message matching `pred`.
    pub async fn wait_for(&self, t: &str, pred: impl Fn(&Value) -> bool, within: Duration) -> Result<Value> {
        let mut rx = self.log_tx.subscribe();
        if let Some(m) = self.messages(t).into_iter().find(|m| pred(m)) {
            return Ok(m);
        }
        let deadline = tokio::time::Instant::now() + within;
        loop {
            let m = tokio::time::timeout_at(deadline, rx.recv()).await.map_err(|_| anyhow!("no {t} within {within:?}"))??;
            if m["t"] == t && pred(&m) {
                return Ok(m);
            }
        }
    }

    pub async fn wait_connected(&self, within: Duration) -> Result<()> {
        let deadline = tokio::time::Instant::now() + within;
        while !self.host_connected() {
            if tokio::time::Instant::now() > deadline {
                bail!("host never connected");
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        Ok(())
    }
}

async fn close_with(ws: &mut WebSocketStream<TcpStream>, code: u16, name: &str) {
    let _ = ws.send(Message::Text(json!({ "t": "error", "code": name, "message": name }).to_string())).await;
    let _ = ws.send(Message::Close(Some(CloseFrame { code: CloseCode::from(code), reason: name.to_string().into() }))).await;
}

async fn conn(tcp: TcpStream, st: Arc<RelayState>, log_tx: broadcast::Sender<Value>) -> Result<()> {
    let target = Arc::new(Mutex::new(String::new()));
    let t2 = target.clone();
    let cb = move |req: &Request, mut resp: Response| {
        *t2.lock().unwrap() = req.uri().to_string();
        let offered = req.headers().get("sec-websocket-protocol").and_then(|v| v.to_str().ok()).unwrap_or("");
        if offered.split(',').any(|p| p.trim() == SUBPROTOCOL) {
            resp.headers_mut().insert("sec-websocket-protocol", HeaderValue::from_static(SUBPROTOCOL));
        }
        Ok(resp)
    };
    let mut ws = tokio_tungstenite::accept_hdr_async(tcp, cb).await?;
    let target = target.lock().unwrap().clone();
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    let param = |k: &str| query.split('&').find_map(|kv| kv.strip_prefix(&format!("{k}="))).map(str::to_string);
    match path {
        "/v1/tunnel/host" => control_conn(ws, st, log_tx).await,
        "/v1/tunnel/accept" => {
            let tx = param("sid").and_then(|sid| st.accepts.lock().unwrap().remove(&sid));
            match tx {
                Some(tx) => {
                    let _ = tx.send(ws);
                }
                None => close_with(&mut ws, 4001, "unauthorized").await,
            }
            Ok(())
        }
        "/v1/tunnel/connect" => phone_conn(ws, st, param("host").unwrap_or_default()).await,
        _ => {
            close_with(&mut ws, 4000, "protocol").await;
            Ok(())
        }
    }
}

async fn control_conn(mut ws: WebSocketStream<TcpStream>, st: Arc<RelayState>, log_tx: broadcast::Sender<Value>) -> Result<()> {
    let nonce: [u8; 32] = random_bytes();
    ws.send(Message::Text(json!({ "t": "challenge", "v": 1, "nonce": b64u(nonce) }).to_string())).await?;
    let hello = next_text(&mut ws).await?;
    let host_pub: [u8; 32] = b64u_fixed(hello["hostPub"].as_str().unwrap_or_default())?;
    let host_id = hello["hostId"].as_str().unwrap_or_default().to_string();
    let key = VerifyingKey::from_bytes(&host_pub)?;
    let sig = ed25519_dalek::Signature::from_slice(&super::b64u_decode(hello["sig"].as_str().unwrap_or_default())?)?;
    if host_id_for(&host_pub) != host_id || key.verify_strict(&ctx("host-auth", &[host_id.as_bytes(), &nonce]), &sig).is_err() {
        close_with(&mut ws, 4001, "unauthorized").await;
        return Ok(());
    }
    ws.send(Message::Text(json!({ "t": "welcome", "status": "active", "maxSessions": 8, "push": false }).to_string())).await?;
    let (tx, mut rx) = mpsc::unbounded_channel();
    *st.host.lock().unwrap() = Some(HostConn { host_id, key, tx });
    loop {
        tokio::select! {
            m = next_text(&mut ws) => {
                let Ok(m) = m else { break };
                match m["t"].as_str() {
                    Some("enroll") => {
                        st.windows.lock().unwrap().insert(m["eid"].as_str().unwrap_or_default().into(), m["tagHash"].as_str().unwrap_or_default().into());
                    }
                    Some("enrollClose") => {
                        st.windows.lock().unwrap().remove(m["eid"].as_str().unwrap_or_default());
                    }
                    Some("revoke") => {
                        st.revoked.lock().unwrap().insert(m["pid"].as_str().unwrap_or_default().into());
                    }
                    Some("reject") => {
                        st.accepts.lock().unwrap().remove(m["sid"].as_str().unwrap_or_default());
                    }
                    _ => {}
                }
                if let Some(id) = m["id"].as_str() {
                    ws.send(Message::Text(json!({ "t": "ack", "id": id, "ok": true }).to_string())).await?;
                }
                st.log.lock().unwrap().push(m.clone());
                let _ = log_tx.send(m);
            }
            Some(out) = rx.recv() => ws.send(Message::Text(out)).await?,
        }
    }
    *st.host.lock().unwrap() = None;
    Ok(())
}

async fn phone_conn(mut ws: WebSocketStream<TcpStream>, st: Arc<RelayState>, host: String) -> Result<()> {
    let nonce: [u8; 32] = random_bytes();
    ws.send(Message::Text(json!({ "t": "challenge", "v": 1, "nonce": b64u(nonce) }).to_string())).await?;
    let m = next_text(&mut ws).await?;
    let (hid, key, tx) = match st.host.lock().unwrap().as_ref() {
        Some(h) => (h.host_id.clone(), h.key, h.tx.clone()),
        None => (String::new(), VerifyingKey::from_bytes(&[0u8; 32]).unwrap_or_else(|_| SigningKey::from_bytes(&[1; 32]).verifying_key()), mpsc::unbounded_channel().0),
    };
    let open = match m["t"].as_str() {
        Some("auth") => {
            let claims = match cred::verify(m["cred"].as_str().unwrap_or_default(), &host, (!hid.is_empty()).then_some((hid.as_str(), &key)), super::now()) {
                Ok(c) => c,
                Err(e) => {
                    let code = match e {
                        cred::CredError::Unauthorized => 4001,
                        cred::CredError::HostUnknown => 4004,
                        cred::CredError::Expired => 4002,
                    };
                    close_with(&mut ws, code, e.code()).await;
                    return Ok(());
                }
            };
            if st.revoked.lock().unwrap().contains(&claims.pid) {
                close_with(&mut ws, 4003, "revoked").await;
                return Ok(());
            }
            let pk = VerifyingKey::from_bytes(&claims.pk)?;
            let sig = ed25519_dalek::Signature::from_slice(&super::b64u_decode(m["sig"].as_str().unwrap_or_default())?)?;
            if pk.verify_strict(&ctx("phone-auth", &[host.as_bytes(), &nonce]), &sig).is_err() {
                close_with(&mut ws, 4001, "unauthorized").await;
                return Ok(());
            }
            json!({ "t": "open", "mode": "session", "pid": claims.pid })
        }
        Some("enroll") => {
            let eid = m["eid"].as_str().unwrap_or_default();
            let tag = super::b64u_decode(m["tag"].as_str().unwrap_or_default()).unwrap_or_default();
            let ok = st.windows.lock().unwrap().get(eid).is_some_and(|h| *h == b64u(sha256(&tag)));
            if !ok {
                close_with(&mut ws, 4001, "unauthorized").await;
                return Ok(());
            }
            json!({ "t": "open", "mode": "enroll", "eid": eid })
        }
        _ => {
            close_with(&mut ws, 4000, "protocol").await;
            return Ok(());
        }
    };
    if hid.is_empty() {
        close_with(&mut ws, 4008, "host_offline").await;
        return Ok(());
    }
    let sid = random_id();
    let (atx, arx) = oneshot::channel();
    st.accepts.lock().unwrap().insert(sid.clone(), atx);
    let mut open = open;
    open["sid"] = json!(sid);
    let _ = tx.send(open.to_string());
    let host_ws = match tokio::time::timeout(Duration::from_secs(10), arx).await {
        Ok(Ok(w)) => w,
        Ok(Err(_)) => {
            close_with(&mut ws, 4010, "host_rejected").await;
            return Ok(());
        }
        Err(_) => {
            close_with(&mut ws, 4012, "timeout").await;
            return Ok(());
        }
    };
    ws.send(Message::Text(json!({ "t": "ready", "sid": sid }).to_string())).await?;
    splice(ws, host_ws).await;
    Ok(())
}

/// Copy binary messages both ways, one at a time; a close ends both sides.
async fn splice(a: WebSocketStream<TcpStream>, b: WebSocketStream<TcpStream>) {
    let (mut a_tx, mut a_rx) = a.split();
    let (mut b_tx, mut b_rx) = b.split();
    loop {
        tokio::select! {
            m = a_rx.next() => match m {
                Some(Ok(Message::Binary(x))) => { if b_tx.send(Message::Binary(x)).await.is_err() { break; } }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                _ => {}
            },
            m = b_rx.next() => match m {
                Some(Ok(Message::Binary(x))) => { if a_tx.send(Message::Binary(x)).await.is_err() { break; } }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                _ => {}
            },
        }
    }
    let _ = a_tx.send(Message::Close(None)).await;
    let _ = b_tx.send(Message::Close(None)).await;
}

/// Decrypt a sealed push (§14) with the spec's AD and parse the plaintext.
pub fn open_push(key: &[u8; 32], hid: &str, pid: &str, ct: &[u8]) -> Value {
    use chacha20poly1305::aead::{Aead, KeyInit, Payload};
    let cipher = chacha20poly1305::ChaCha20Poly1305::new(key.into());
    let ad = ctx("push", &[hid.as_bytes(), pid.as_bytes()]);
    let pt = cipher.decrypt(ct[..12].into(), Payload { msg: &ct[12..], aad: &ad }).expect("push decrypts");
    serde_json::from_slice(&pt).unwrap()
}
