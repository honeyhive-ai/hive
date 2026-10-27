//! The host's control connection to the relay (§6.2) and the accept
//! connections it dials per `open` (§6.3).

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;

use super::session::{self, Mode, Timeouts};
use super::{b64u, b64u_fixed, TunnelHost, SUBPROTOCOL};

/// `hello` / `welcome` deadline after connecting (§10).
const HANDSHAKE: Duration = Duration::from_secs(10);
/// The relay pings every 20 s; a control connection silent this long is dead.
const SILENT: Duration = Duration::from_secs(60);
/// Backoff cap (§10), and the minimum wait after `superseded` (4009).
const MAX_BACKOFF: Duration = Duration::from_secs(60);

pub type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Why a control connection ended.
#[derive(Debug)]
enum End {
    /// Closed with a relay close code (§12).
    Code(u16, String),
    /// Our side stopped.
    Stopped,
    Other(anyhow::Error),
}

/// Dial `url` offering `hive-tunnel.1`; the relay must select it.
pub async fn dial(url: &str) -> Result<Ws> {
    let mut req = url.into_client_request().with_context(|| format!("bad relay URL {url}"))?;
    req.headers_mut().insert("sec-websocket-protocol", HeaderValue::from_static(SUBPROTOCOL));
    let (ws, resp) = tokio::time::timeout(HANDSHAKE, tokio_tungstenite::connect_async(req))
        .await
        .map_err(|_| anyhow!("connecting to {url} timed out"))?
        .map_err(|e| match e {
            tokio_tungstenite::tungstenite::Error::Http(r) => anyhow!("relay refused the upgrade: HTTP {}", r.status()),
            e => anyhow!("connecting to the relay: {e}"),
        })?;
    let proto = resp.headers().get("sec-websocket-protocol").and_then(|v| v.to_str().ok());
    if proto != Some(SUBPROTOCOL) {
        bail!("relay did not select the {SUBPROTOCOL} subprotocol");
    }
    Ok(ws)
}

/// ±20 % jitter.
fn jitter(d: Duration) -> Duration {
    let r = u16::from_le_bytes(super::random_bytes::<2>()) as f64 / u16::MAX as f64;
    d.mul_f64(0.8 + 0.4 * r)
}

/// Keep the control connection up until `stop`. Reconnects with backoff
/// 1 … 60 s (±20 %); waits ≥ 60 s after 4009; stops on 4001 (§12).
pub async fn run(host: Arc<TunnelHost>, router: Router, mut stop: watch::Receiver<bool>, t: Timeouts) {
    let mut backoff = Duration::from_secs(1);
    loop {
        if *stop.borrow() {
            return;
        }
        let (end, welcomed) = connect_once(&host, &router, &mut stop, t).await;
        host.set_control(None);
        if welcomed {
            backoff = Duration::from_secs(1);
        }
        let wait = match &end {
            End::Stopped => return,
            End::Code(4001, why) => {
                eprintln!(
                    "remote: the relay rejected this host's credentials (4001 {why}); remote access is stopped. \
                     Check `hive remote status`, or run `hive remote reset` if the host key changed."
                );
                host.update_status(|s| s.last_error = Some(format!("4001 {why}")));
                return;
            }
            End::Code(4009, _) => {
                eprintln!(
                    "remote: SUPERSEDED (4009): another process connected to the relay with this host's key. \
                     Two `hive serve`s share one data dir? Waiting 60 s."
                );
                host.update_status(|s| s.last_error = Some("4009 superseded".into()));
                jitter(MAX_BACKOFF).max(MAX_BACKOFF)
            }
            End::Code(code, why) => {
                eprintln!("remote: relay closed the control connection ({code} {why}); retrying in {}s", backoff.as_secs());
                host.update_status(|s| s.last_error = Some(format!("{code} {why}")));
                jitter(backoff)
            }
            End::Other(e) => {
                eprintln!("remote: {e:#}; retrying in {}s", backoff.as_secs());
                host.update_status(|s| s.last_error = Some(format!("{e:#}")));
                jitter(backoff)
            }
        };
        backoff = (backoff * 2).min(MAX_BACKOFF);
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = super::stopped(&mut stop) => return,
        }
    }
}

fn close_code(m: &Message) -> Option<(u16, String)> {
    match m {
        Message::Close(Some(f)) => Some((u16::from(f.code), f.reason.to_string())),
        Message::Close(None) => Some((1005, String::new())),
        _ => None,
    }
}

/// Read the next text message as JSON (pings are answered by tungstenite).
async fn next_json(ws: &mut Ws) -> Result<Value, End> {
    loop {
        match ws.next().await {
            Some(Ok(Message::Text(t))) => {
                return serde_json::from_str(&t).map_err(|e| End::Other(anyhow!("relay sent bad JSON: {e}")));
            }
            Some(Ok(m)) => {
                if let Some((c, r)) = close_code(&m) {
                    return Err(End::Code(c, r));
                }
            }
            Some(Err(e)) => return Err(End::Other(anyhow!("control connection: {e}"))),
            None => return Err(End::Other(anyhow!("control connection closed"))),
        }
    }
}

async fn connect_once(host: &Arc<TunnelHost>, router: &Router, stop: &mut watch::Receiver<bool>, t: Timeouts) -> (End, bool) {
    let url = host.settings.ws_url("/v1/tunnel/host");
    let mut ws = match dial(&url).await {
        Ok(ws) => ws,
        Err(e) => return (End::Other(e), false),
    };
    // challenge → hello → welcome
    let welcome = async {
        let ch = next_json(&mut ws).await?;
        if ch["t"] != "challenge" {
            return Err(End::Other(anyhow!("expected challenge, got {}", ch["t"])));
        }
        if ch["v"] != 1 {
            return Err(End::Other(anyhow!("relay speaks tunnel version {}", ch["v"])));
        }
        let nonce: [u8; 32] = b64u_fixed(ch["nonce"].as_str().unwrap_or_default()).map_err(End::Other)?;
        let mut hello = json!({
            "t": "hello",
            "v": 1,
            "hostId": host.host_id(),
            "hostPub": b64u(host.keys.sign_pub().as_bytes()),
            "sig": b64u(host.keys.sign_challenge(&nonce)),
            "agent": format!("hive-serve/{}", env!("CARGO_PKG_VERSION")),
        });
        if let Some(r) = &host.settings.reg_token {
            hello["reg"] = json!(r);
        }
        ws.send(Message::Text(hello.to_string())).await.map_err(|e| End::Other(e.into()))?;
        loop {
            let m = next_json(&mut ws).await?;
            match m["t"].as_str() {
                Some("welcome") => return Ok(m),
                Some("error") => {
                    eprintln!("remote: relay error: {} {}", m["code"], m["message"]);
                }
                _ => {}
            }
        }
    };
    let welcome = match tokio::time::timeout(HANDSHAKE, welcome).await {
        Ok(Ok(w)) => w,
        Ok(Err(e)) => return (e, false),
        Err(_) => return (End::Other(anyhow!("relay handshake timed out")), false),
    };
    let status = welcome["status"].as_str().unwrap_or("active").to_string();
    if let Some(n) = welcome["maxSessions"].as_u64() {
        host.max_sessions.store(n.clamp(1, 1024) as usize, Ordering::SeqCst);
    }
    let push = welcome["push"].as_bool().unwrap_or(false);
    host.push_available.store(push, Ordering::SeqCst);
    println!("remote: connected to {} as host {} (status {status})", host.settings.relay, host.host_id());
    if status == "pending" {
        println!("remote: the relay registered this host as pending: pairing works, sessions wait for entitlement.");
    }
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    host.set_control(Some(tx));
    host.update_status(|s| {
        s.status = status.clone();
        s.push = push;
        s.last_error = None;
    });
    // Re-announce windows and unacked revocations on every connection.
    host.announce_windows();
    host.send_revocations();

    let session_stop = stop.clone();
    let end = loop {
        tokio::select! {
            m = tokio::time::timeout(SILENT, ws.next()) => {
                let m = match m {
                    Err(_) => break End::Other(anyhow!("relay silent for {}s", SILENT.as_secs())),
                    Ok(None) => break End::Other(anyhow!("control connection closed")),
                    Ok(Some(Err(e))) => break End::Other(anyhow!("control connection: {e}")),
                    Ok(Some(Ok(m))) => m,
                };
                if let Some((c, r)) = close_code(&m) {
                    break End::Code(c, r);
                }
                let Message::Text(text) = m else { continue };
                let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
                handle(host, router, &v, session_stop.clone(), t);
            }
            Some(out) = rx.recv() => {
                if let Err(e) = ws.send(Message::Text(out)).await {
                    break End::Other(anyhow!("control send: {e}"));
                }
            }
            _ = super::stopped(stop) => {
                let _ = ws.close(None).await;
                break End::Stopped;
            }
        }
    };
    (end, true)
}

fn handle(host: &Arc<TunnelHost>, router: &Router, v: &Value, stop: watch::Receiver<bool>, t: Timeouts) {
    match v["t"].as_str() {
        Some("open") => {
            let Some(sid) = v["sid"].as_str().map(str::to_string) else { return };
            let mode = match (v["mode"].as_str(), v["pid"].as_str(), v["eid"].as_str()) {
                (Some("session"), Some(pid), _) => Mode::Session { pid: pid.to_string() },
                (Some("enroll"), _, Some(eid)) => Mode::Enroll { eid: eid.to_string() },
                _ => {
                    reject(host, &sid);
                    return;
                }
            };
            // Cheap early refusals: unknown/revoked pid, closed window, capacity.
            let admissible = match &mode {
                Mode::Session { pid } => host.registry.is_active(pid),
                Mode::Enroll { eid } => host.window_psk(eid).is_some(),
            };
            if !admissible || host.sessions.load(Ordering::SeqCst) >= host.max_sessions.load(Ordering::SeqCst) {
                reject(host, &sid);
                return;
            }
            host.sessions.fetch_add(1, Ordering::SeqCst);
            let host = host.clone();
            let router = router.clone();
            tokio::spawn(async move {
                let url = host.settings.ws_url(&format!("/v1/tunnel/accept?sid={sid}"));
                let res = match dial(&url).await {
                    Ok(ws) => session::run(host.clone(), router, ws, mode.clone(), stop, t).await,
                    Err(e) => Err(e.context("dialing accept")),
                };
                host.sessions.fetch_sub(1, Ordering::SeqCst);
                if let Err(e) = res {
                    eprintln!("remote: session {mode:?} ended: {e:#}");
                }
            });
        }
        Some("status") => {
            let status = v["status"].as_str().unwrap_or_default().to_string();
            println!("remote: relay status for this host is now {status}");
            host.update_status(|s| s.status = status);
        }
        Some("ack") => {
            host.on_ack(v["id"].as_str().unwrap_or_default(), v["ok"].as_bool().unwrap_or(false), v["code"].as_str());
        }
        Some("error") => eprintln!("remote: relay error: {} {}", v["code"], v["message"]),
        _ => {}
    }
}

fn reject(host: &TunnelHost, sid: &str) {
    host.send_plain(json!({ "t": "reject", "sid": sid, "code": "host_rejected" }));
}
