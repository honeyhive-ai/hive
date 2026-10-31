//! In-memory mock relay for hive-runtime tests.
//!
//! Speaks the subset of the hrt1 `/v1` surface the sync-engine and relay-client
//! tests exercise — envelope push/fetch, pairing codes, and keyring rotations —
//! so those tests keep validating the client and the E2EE guarantees without
//! depending on a separate relay crate/binary. It's **content-blind**: it stores
//! whatever opaque JSON body it's handed, verbatim, exactly as a real relay does.
//!
//! `GET /envelopes` pages like the real relay: at most `limit` rows (clamped to
//! the router's page cap, which is also the default), with `X-Hive-Next-After`
//! and, when rows remain, `X-Hive-More: true`. [`router_with_page_cap`] sets a
//! tiny cap so tests cross page boundaries with a handful of envelopes.
//!
//! Deliberately omitted: the SSE `/events` stream (the client degrades to polling
//! on 404, which the tests rely on) and everything social/directory/admin, which
//! no test here touches.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};

#[derive(Default)]
struct MockState {
    /// workspace → pushed bodies; the server sequence is the 1-based index.
    envelopes: HashMap<String, Vec<Value>>,
    /// workspace → published key rotations (opaque).
    keyrings: HashMap<String, Vec<Value>>,
    /// workspace → device slot → presence blob (overwritten in place).
    presence: HashMap<String, HashMap<String, Value>>,
    /// pairing code → opaque payload.
    pairings: HashMap<String, String>,
    pair_seq: u64,
    /// Most envelopes one `GET /envelopes` returns (and the default limit).
    page_cap: usize,
    /// How many `GET /envelopes` requests were served (paging assertions).
    envelope_gets: usize,
}

/// The real relay's default and maximum page size.
pub const DEFAULT_PAGE_CAP: usize = 1000;

type Shared = Arc<Mutex<MockState>>;

/// A fresh mock relay router with its own in-memory state.
pub fn router() -> Router {
    router_with_page_cap(DEFAULT_PAGE_CAP)
}

/// A mock relay whose envelope pages hold at most `page_cap` rows.
pub fn router_with_page_cap(page_cap: usize) -> Router {
    router_with_stats(page_cap).0
}

/// Like [`router_with_page_cap`], plus a counter of `GET /envelopes` requests.
pub fn router_with_stats(page_cap: usize) -> (Router, EnvelopeGets) {
    let state: Shared = Arc::new(Mutex::new(MockState { page_cap: page_cap.max(1), ..Default::default() }));
    let gets = EnvelopeGets(state.clone());
    let router = Router::new()
        .route("/v1/health", get(|| async { "ok" }))
        .route(
            "/v1/workspaces/:ws/envelopes",
            post(push_envelope).get(list_envelopes),
        )
        .route(
            "/v1/workspaces/:ws/keyring",
            post(push_keyring).get(list_keyring),
        )
        .route("/v1/workspaces/:ws/presence", post(put_presence).get(list_presence))
        .route("/v1/pair", post(create_pair))
        .route("/v1/pair/:code", get(resolve_pair))
        .with_state(state);
    (router, gets)
}

/// Reads how many `GET /envelopes` requests a mock relay has served.
#[derive(Clone)]
pub struct EnvelopeGets(Shared);

impl EnvelopeGets {
    pub fn count(&self) -> usize {
        self.0.lock().unwrap().envelope_gets
    }
}

async fn push_envelope(
    State(st): State<Shared>,
    Path(ws): Path<String>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let mut st = st.lock().unwrap();
    let list = st.envelopes.entry(ws).or_default();
    list.push(body);
    Json(json!({ "seq": list.len() as u64 }))
}

#[derive(serde::Deserialize)]
struct AfterQuery {
    #[serde(default)]
    after: u64,
    #[serde(default)]
    limit: Option<usize>,
}

async fn list_envelopes(
    State(st): State<Shared>,
    Path(ws): Path<String>,
    Query(q): Query<AfterQuery>,
) -> impl axum::response::IntoResponse {
    let mut st = st.lock().unwrap();
    st.envelope_gets += 1;
    let cap = st.page_cap;
    let limit = q.limit.filter(|n| *n > 0).map_or(cap, |n| n.min(cap));
    let tail: Vec<(u64, &Value)> = st
        .envelopes
        .get(&ws)
        .map(|list| {
            list.iter()
                .enumerate()
                .map(|(i, body)| (i as u64 + 1, body))
                .filter(|(seq, _)| *seq > q.after)
                .collect()
        })
        .unwrap_or_default();
    let more = tail.len() > limit;
    let page: Vec<Value> = tail
        .iter()
        .take(limit)
        .map(|(seq, body)| json!({ "seq": seq, "body": body }))
        .collect();
    let next = tail.get(limit.min(tail.len()).wrapping_sub(1)).map_or(q.after, |(seq, _)| *seq);
    let mut headers = axum::http::HeaderMap::new();
    headers.insert("x-hive-next-after", next.to_string().parse().unwrap());
    if more {
        headers.insert("x-hive-more", "true".parse().unwrap());
    }
    (headers, Json(Value::Array(page)))
}

#[derive(serde::Deserialize)]
struct PresenceReq {
    device_id: String,
    data: Value,
}

async fn put_presence(State(st): State<Shared>, Path(ws): Path<String>, Json(req): Json<PresenceReq>) -> Json<Value> {
    st.lock().unwrap().presence.entry(ws).or_default().insert(req.device_id, req.data);
    Json(json!({ "ok": true }))
}

async fn list_presence(State(st): State<Shared>, Path(ws): Path<String>) -> Json<Value> {
    let map = st.lock().unwrap().presence.get(&ws).cloned().unwrap_or_default();
    Json(json!(map))
}

async fn push_keyring(
    State(st): State<Shared>,
    Path(ws): Path<String>,
    Json(body): Json<Value>,
) -> Json<Value> {
    st.lock().unwrap().keyrings.entry(ws).or_default().push(body);
    Json(json!({ "ok": true }))
}

async fn list_keyring(State(st): State<Shared>, Path(ws): Path<String>) -> Json<Value> {
    let rows = st.lock().unwrap().keyrings.get(&ws).cloned().unwrap_or_default();
    Json(Value::Array(rows))
}

#[derive(serde::Deserialize)]
struct PairReq {
    payload: String,
    #[serde(default)]
    ttl_secs: Option<u64>,
}

async fn create_pair(State(st): State<Shared>, Json(req): Json<PairReq>) -> Json<Value> {
    let mut st = st.lock().unwrap();
    st.pair_seq += 1;
    // Codes are stored uppercase; resolve normalizes too (the real relay is
    // case/separator tolerant). Echo the requested TTL back like the relay does.
    let code = format!("PAIR{:04}", st.pair_seq);
    let expires_in = req.ttl_secs.unwrap_or(900);
    st.pairings.insert(code.clone(), req.payload);
    Json(json!({ "code": code, "expires_in": expires_in }))
}

async fn resolve_pair(
    State(st): State<Shared>,
    Path(code): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    match st.lock().unwrap().pairings.get(&code.to_uppercase()) {
        Some(payload) => Ok(Json(json!({ "payload": payload }))),
        None => Err(StatusCode::NOT_FOUND),
    }
}
