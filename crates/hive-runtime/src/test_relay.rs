//! In-memory mock relay for hive-runtime tests.
//!
//! Speaks the subset of the hrt1 `/v1` surface the sync-engine and relay-client
//! tests exercise — envelope push/fetch, pairing codes, and keyring rotations —
//! so those tests keep validating the client and the E2EE guarantees without
//! depending on a separate relay crate/binary. It's **content-blind**: it stores
//! whatever opaque JSON body it's handed, verbatim, exactly as a real relay does.
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
    /// pairing code → opaque payload.
    pairings: HashMap<String, String>,
    pair_seq: u64,
}

type Shared = Arc<Mutex<MockState>>;

/// A fresh mock relay router with its own in-memory state.
pub fn router() -> Router {
    let state: Shared = Arc::new(Mutex::new(MockState::default()));
    Router::new()
        .route("/v1/health", get(|| async { "ok" }))
        .route(
            "/v1/workspaces/:ws/envelopes",
            post(push_envelope).get(list_envelopes),
        )
        .route(
            "/v1/workspaces/:ws/keyring",
            post(push_keyring).get(list_keyring),
        )
        // Presence is transient metadata; tests don't assert on it, so accept + drop.
        .route("/v1/workspaces/:ws/presence", post(|| async { Json(json!({"ok": true})) }))
        .route("/v1/pair", post(create_pair))
        .route("/v1/pair/:code", get(resolve_pair))
        .with_state(state)
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
}

async fn list_envelopes(
    State(st): State<Shared>,
    Path(ws): Path<String>,
    Query(q): Query<AfterQuery>,
) -> Json<Value> {
    let st = st.lock().unwrap();
    let rows: Vec<Value> = st
        .envelopes
        .get(&ws)
        .map(|list| {
            list.iter()
                .enumerate()
                .map(|(i, body)| (i as u64 + 1, body))
                .filter(|(seq, _)| *seq > q.after)
                .map(|(seq, body)| json!({ "seq": seq, "body": body }))
                .collect()
        })
        .unwrap_or_default();
    Json(Value::Array(rows))
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
