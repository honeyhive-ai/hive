//! Vault retrieval: chunk reference vaults, embed the chunks once (cached by
//! content hash), and per turn pick the few passages that answer the latest
//! message — cosine top-`candidates`, then an optional cross-encoder rerank
//! down to `top_k`.
//!
//! Measured on a local llama.cpp gateway (EmbeddingGemma 300M +
//! bge-reranker-v2-m3, rerank top 20 → top 5), this matched handing the model
//! the right file at about a fifth of the tokens, where pasting whole docs was
//! 8× slower and often overflowed a 32k slot.
//!
//! Both services speak the OpenAI-compatible shapes llama.cpp serves:
//! `POST /v1/embeddings {model, input:[..]}` → `data[].embedding` and
//! `POST /v1/rerank {model, query, documents, top_n}` →
//! `results[{index, relevance_score}]`. Any failure is returned as an error so
//! the caller can fall back to its non-retrieval path.

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

/// Chunk size and overlap, in characters.
pub const CHUNK_CHARS: usize = 1200;
pub const CHUNK_OVERLAP: usize = 200;
pub const DEFAULT_TOP_K: usize = 5;
pub const DEFAULT_CANDIDATES: usize = 20;
/// Inputs per embeddings request; small enough for a llama-server batch.
const EMBED_BATCH: usize = 16;
/// Bound on chunks indexed per turn, so a huge vault can't stall a reply.
const MAX_CHUNKS: usize = 2000;
/// The query is a chat message; long ones are cut so they fit the embedder.
const MAX_QUERY_CHARS: usize = 2000;
/// Cached embeddings are dropped wholesale past this many (memory bound).
const CACHE_CAP: usize = 50_000;
const HTTP_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, thiserror::Error)]
pub enum RetrievalError {
    #[error("retrieval http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("retrieval: {0}")]
    Bad(String),
}

/// Where the embedder and (optional) reranker live, and how many passages to
/// keep. `rerank_url`/`rerank_model` empty = rank by cosine alone.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RetrievalConfig {
    pub embed_url: String,
    pub embed_model: String,
    pub rerank_url: String,
    pub rerank_model: String,
    pub top_k: usize,
    pub candidates: usize,
}

impl RetrievalConfig {
    /// Retrieval is on once an embedder is named.
    pub fn is_configured(&self) -> bool {
        !self.embed_url.trim().is_empty() && !self.embed_model.trim().is_empty()
    }

    fn reranks(&self) -> bool {
        !self.rerank_url.trim().is_empty() && !self.rerank_model.trim().is_empty()
    }

    fn top_k(&self) -> usize {
        if self.top_k == 0 { DEFAULT_TOP_K } else { self.top_k }
    }

    /// Never fewer candidates than passages wanted.
    fn candidates(&self) -> usize {
        let c = if self.candidates == 0 { DEFAULT_CANDIDATES } else { self.candidates };
        c.max(self.top_k())
    }
}

/// Resolve a configured URL to the endpoint: a full `…/embeddings` URL is kept,
/// a base (`http://host:8080` or `…/v1`) gets the path appended.
pub fn endpoint_url(configured: &str, path: &str) -> String {
    let base = configured.trim().trim_end_matches('/');
    if base.ends_with(&format!("/{path}")) {
        base.to_string()
    } else if base.ends_with("/v1") {
        format!("{base}/{path}")
    } else {
        format!("{base}/v1/{path}")
    }
}

/// Query/document prefixes the embedding model was trained with, by family.
/// Missing them costs real recall on asymmetric (question → passage) search.
pub fn embed_prefixes(model: &str) -> (&'static str, &'static str) {
    let m = model.to_ascii_lowercase();
    if m.contains("gemma") {
        ("task: search result | query: ", "title: none | text: ")
    } else if m.contains("nomic") {
        ("search_query: ", "search_document: ")
    } else if m.contains("qwen3") {
        ("Instruct: Given a question, retrieve passages that answer it\nQuery: ", "")
    } else {
        ("", "")
    }
}

/// Split `text` into ~`size`-char chunks overlapping by ~`overlap`, cutting at
/// the last blank line, else newline, else sentence end, else space in the
/// back half of the window (a hard cut only when there is none).
pub fn chunk_text(text: &str, size: usize, overlap: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let size = size.max(1);
    let overlap = overlap.min(size / 2);
    let mut out = Vec::new();
    let mut start = 0;
    while start < n {
        let hard_end = (start + size).min(n);
        let end = if hard_end == n { n } else { cut_point(&chars, start, hard_end) };
        let chunk: String = chars[start..end].iter().collect();
        let chunk = chunk.trim();
        if !chunk.is_empty() {
            out.push(chunk.to_string());
        }
        if end >= n {
            break;
        }
        // Step back by the overlap, then forward to a word start so the next
        // chunk doesn't open mid-word.
        let mut next = end.saturating_sub(overlap).max(start + 1);
        if next < end && !chars[next - 1].is_whitespace() {
            while next < end && !chars[next].is_whitespace() {
                next += 1;
            }
        }
        while next < n && chars[next].is_whitespace() {
            next += 1;
        }
        start = next;
    }
    out
}

/// Best cut in `(start + (end-start)/2, end]`: returns the index just past the
/// separator.
fn cut_point(chars: &[char], start: usize, end: usize) -> usize {
    let floor = start + (end - start) / 2;
    let window = |pred: &dyn Fn(usize) -> bool| (floor..end).rev().find(|&i| pred(i));
    if let Some(i) = window(&|i| i + 1 < end && chars[i] == '\n' && chars[i + 1] == '\n') {
        return i + 2;
    }
    if let Some(i) = window(&|i| chars[i] == '\n') {
        return i + 1;
    }
    if let Some(i) = window(&|i| {
        matches!(chars[i], '.' | '!' | '?') && i + 1 < chars.len() && chars[i + 1].is_whitespace()
    }) {
        return i + 1;
    }
    if let Some(i) = window(&|i| chars[i] == ' ') {
        return i + 1;
    }
    end
}

pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0f32, 0f32, 0f32);
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

/// One retrieved passage: which vault it came from and its text.
#[derive(Debug, Clone, PartialEq)]
pub struct Passage {
    pub source: String,
    pub text: String,
    pub score: f32,
}

/// Holds the embedding cache across turns (one per app run).
pub struct Retriever {
    http: reqwest::Client,
    /// sha256(model, doc prefix, chunk) → embedding.
    cache: Mutex<HashMap<[u8; 32], Vec<f32>>>,
}

impl Default for Retriever {
    fn default() -> Self {
        Self::new()
    }
}

impl Retriever {
    pub fn new() -> Self {
        let http = reqwest::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { http, cache: Mutex::new(HashMap::new()) }
    }

    /// Number of cached chunk embeddings (for tests/diagnostics).
    pub fn cached(&self) -> usize {
        self.cache.lock().unwrap().len()
    }

    /// Retrieve the passages from `sources` (label, text) that best answer
    /// `query`. Empty sources or query yield no passages.
    pub async fn retrieve(
        &self,
        cfg: &RetrievalConfig,
        sources: &[(String, String)],
        query: &str,
    ) -> Result<Vec<Passage>, RetrievalError> {
        if !cfg.is_configured() {
            return Err(RetrievalError::Bad("not configured".into()));
        }
        let query: String = query.trim().chars().take(MAX_QUERY_CHARS).collect();
        let mut chunks: Vec<(usize, String)> = Vec::new();
        for (i, (_, text)) in sources.iter().enumerate() {
            for c in chunk_text(text, CHUNK_CHARS, CHUNK_OVERLAP) {
                chunks.push((i, c));
            }
        }
        if chunks.len() > MAX_CHUNKS {
            tracing::warn!("retrieval: {} chunks, indexing the first {MAX_CHUNKS}", chunks.len());
            chunks.truncate(MAX_CHUNKS);
        }
        if chunks.is_empty() || query.is_empty() {
            return Ok(Vec::new());
        }

        let model = cfg.embed_model.trim();
        let (qp, dp) = embed_prefixes(model);
        let embed_url = endpoint_url(&cfg.embed_url, "embeddings");
        let keys: Vec<[u8; 32]> = chunks.iter().map(|(_, c)| cache_key(model, dp, c)).collect();

        // Embed what the cache lacks, in batches; each batch is cached as it
        // lands so an interrupted first index still makes progress.
        let missing: Vec<usize> = {
            let cache = self.cache.lock().unwrap();
            let mut seen = std::collections::HashSet::new();
            (0..chunks.len()).filter(|&i| !cache.contains_key(&keys[i]) && seen.insert(keys[i])).collect()
        };
        for batch in missing.chunks(EMBED_BATCH) {
            let inputs: Vec<String> = batch.iter().map(|&i| format!("{dp}{}", chunks[i].1)).collect();
            let vecs = self.embed(&embed_url, model, &inputs).await?;
            let mut cache = self.cache.lock().unwrap();
            if cache.len() + vecs.len() > CACHE_CAP {
                cache.clear();
            }
            for (&i, v) in batch.iter().zip(vecs) {
                cache.insert(keys[i], v);
            }
        }
        let qv = self
            .embed(&embed_url, model, &[format!("{qp}{query}")])
            .await?
            .pop()
            .ok_or_else(|| RetrievalError::Bad("no query embedding".into()))?;

        let mut scored: Vec<(usize, f32)> = {
            let cache = self.cache.lock().unwrap();
            let mut s = Vec::with_capacity(chunks.len());
            for (i, k) in keys.iter().enumerate() {
                let v = cache.get(k).ok_or_else(|| RetrievalError::Bad("embedding evicted".into()))?;
                s.push((i, cosine(&qv, v)));
            }
            s
        };
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        // Duplicate chunks (same text twice) would waste passage slots.
        let mut seen = std::collections::HashSet::new();
        scored.retain(|(i, _)| seen.insert(keys[*i]));
        scored.truncate(cfg.candidates());

        let picked: Vec<(usize, f32)> = if cfg.reranks() && scored.len() > 1 {
            let docs: Vec<&str> = scored.iter().map(|(i, _)| chunks[*i].1.as_str()).collect();
            let order = self
                .rerank(&endpoint_url(&cfg.rerank_url, "rerank"), cfg.rerank_model.trim(), &query, &docs, cfg.top_k())
                .await?;
            order.into_iter().map(|(j, s)| (scored[j].0, s)).collect()
        } else {
            scored
        };
        Ok(picked
            .into_iter()
            .take(cfg.top_k())
            .map(|(i, score)| Passage {
                source: sources[chunks[i].0].0.clone(),
                text: chunks[i].1.clone(),
                score,
            })
            .collect())
    }

    async fn embed(&self, url: &str, model: &str, inputs: &[String]) -> Result<Vec<Vec<f32>>, RetrievalError> {
        let resp = self
            .http
            .post(url)
            .json(&json!({"model": model, "input": inputs}))
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(RetrievalError::Bad(format!("embeddings {status}: {}", snippet(&body))));
        }
        parse_embeddings(&resp.json::<Value>().await?, inputs.len())
    }

    async fn rerank(
        &self,
        url: &str,
        model: &str,
        query: &str,
        docs: &[&str],
        top_n: usize,
    ) -> Result<Vec<(usize, f32)>, RetrievalError> {
        let resp = self
            .http
            .post(url)
            .json(&json!({"model": model, "query": query, "documents": docs, "top_n": top_n}))
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(RetrievalError::Bad(format!("rerank {status}: {}", snippet(&body))));
        }
        parse_rerank(&resp.json::<Value>().await?, docs.len())
    }
}

fn snippet(s: &str) -> String {
    s.chars().take(200).collect()
}

fn cache_key(model: &str, prefix: &str, chunk: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    for part in [model, prefix, chunk] {
        h.update((part.len() as u64).to_le_bytes());
        h.update(part.as_bytes());
    }
    h.finalize().into()
}

/// `data[]` in `index` order (servers may reorder); one vector per input.
fn parse_embeddings(v: &Value, expected: usize) -> Result<Vec<Vec<f32>>, RetrievalError> {
    let data = v
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| RetrievalError::Bad("embeddings: no data[]".into()))?;
    let mut out: Vec<(usize, Vec<f32>)> = Vec::with_capacity(data.len());
    for (pos, item) in data.iter().enumerate() {
        let idx = item.get("index").and_then(Value::as_u64).map(|i| i as usize).unwrap_or(pos);
        let emb = item
            .get("embedding")
            .and_then(Value::as_array)
            .ok_or_else(|| RetrievalError::Bad("embeddings: item without embedding".into()))?
            .iter()
            .map(|x| x.as_f64().map(|f| f as f32))
            .collect::<Option<Vec<f32>>>()
            .ok_or_else(|| RetrievalError::Bad("embeddings: non-numeric value".into()))?;
        if emb.is_empty() {
            return Err(RetrievalError::Bad("embeddings: empty vector".into()));
        }
        out.push((idx, emb));
    }
    if out.len() != expected {
        return Err(RetrievalError::Bad(format!("embeddings: {} vectors for {expected} inputs", out.len())));
    }
    out.sort_by_key(|(i, _)| *i);
    Ok(out.into_iter().map(|(_, e)| e).collect())
}

/// `results[{index, relevance_score}]`, best first, indexes checked.
fn parse_rerank(v: &Value, docs: usize) -> Result<Vec<(usize, f32)>, RetrievalError> {
    let results = v
        .get("results")
        .and_then(Value::as_array)
        .ok_or_else(|| RetrievalError::Bad("rerank: no results[]".into()))?;
    let mut out = Vec::with_capacity(results.len());
    for r in results {
        let idx = r
            .get("index")
            .and_then(Value::as_u64)
            .map(|i| i as usize)
            .filter(|&i| i < docs)
            .ok_or_else(|| RetrievalError::Bad("rerank: bad index".into()))?;
        let score = r
            .get("relevance_score")
            .or_else(|| r.get("score"))
            .and_then(Value::as_f64)
            .ok_or_else(|| RetrievalError::Bad("rerank: no relevance_score".into()))? as f32;
        out.push((idx, score));
    }
    out.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    Ok(out)
}

/// Header of the retrieved-passages block appended to the latest user turn.
pub const PASSAGES_HEADER: &str = "[Retrieved from reference vaults]";

/// Format passages as a block for the end of the latest user turn. Empty in,
/// empty out.
pub fn passages_section(passages: &[Passage]) -> String {
    if passages.is_empty() {
        return String::new();
    }
    let mut out = format!(
        "{PASSAGES_HEADER}\nPassages from workspace reference material that may help with this message — treat as background knowledge:\n"
    );
    for (n, p) in passages.iter().enumerate() {
        out.push_str(&format!("--- {} (passage {}) ---\n{}\n", p.source, n + 1, p.text.trim()));
    }
    out.trim_end().to_string()
}

/// The retrieval query: the latest user turn's text (None when the prompt
/// doesn't end on a user turn, or it is blank).
pub fn latest_user_query(turns: &[crate::provider::ChatTurn]) -> Option<&str> {
    turns
        .iter()
        .rev()
        .find(|t| t.role == "user")
        .map(|t| t.content.trim())
        .filter(|s| !s.is_empty())
}

/// Append `section` to the latest user turn — the end of the prompt, so the
/// system prompt and all earlier turns stay byte-stable for prefix caching.
/// Returns false (and changes nothing) when there is no user turn.
pub fn append_to_latest_user(turns: &mut [crate::provider::ChatTurn], section: &str) -> bool {
    if section.is_empty() {
        return true;
    }
    match turns.iter_mut().rev().find(|t| t.role == "user") {
        Some(t) => {
            t.content = format!("{}\n\n{section}", t.content.trim_end());
            true
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_urls_accept_base_or_full() {
        assert_eq!(endpoint_url("http://h:8080", "embeddings"), "http://h:8080/v1/embeddings");
        assert_eq!(endpoint_url("http://h:8080/v1/", "embeddings"), "http://h:8080/v1/embeddings");
        assert_eq!(endpoint_url("http://h:8080/v1/embeddings", "embeddings"), "http://h:8080/v1/embeddings");
        assert_eq!(endpoint_url(" http://h/v1/rerank ", "rerank"), "http://h/v1/rerank");
    }

    #[test]
    fn prefixes_by_model_family() {
        assert_eq!(embed_prefixes("embeddinggemma-300m"), ("task: search result | query: ", "title: none | text: "));
        assert_eq!(embed_prefixes("nomic-embed-text"), ("search_query: ", "search_document: "));
        let (q, d) = embed_prefixes("Qwen3-Embedding-0.6B");
        assert!(q.starts_with("Instruct: Given a question") && q.ends_with("\nQuery: "));
        assert_eq!(d, "");
        assert_eq!(embed_prefixes("bge-m3"), ("", ""));
    }

    #[test]
    fn short_text_is_one_chunk() {
        assert_eq!(chunk_text("  hello world  ", 1200, 200), vec!["hello world"]);
        assert!(chunk_text("   ", 1200, 200).is_empty());
    }

    #[test]
    fn chunks_cut_at_blank_lines_and_overlap() {
        let para = |c: char| std::iter::repeat(format!("{c}{c}{c} ")).take(100).collect::<String>();
        // 3 paragraphs of 400 chars, size 1000: the first cut is the blank line
        // after paragraph 2 (in the back half), not mid-paragraph 3.
        let text = format!("{}\n\n{}\n\n{}", para('a'), para('b'), para('c'));
        let chunks = chunk_text(&text, 1000, 200);
        assert!(chunks[0].ends_with("bbb"), "{:?}", &chunks[0][chunks[0].len() - 10..]);
        assert!(!chunks[0].contains('c'));
        // Overlap: the second chunk starts inside paragraph 2, on a word start.
        assert!(chunks[1].starts_with("bbb "));
        assert!(chunks.last().unwrap().ends_with("ccc"));
        for c in &chunks {
            assert!(c.chars().count() <= 1000);
        }
    }

    #[test]
    fn chunks_fall_back_to_sentence_then_space_then_hard_cut() {
        // No line breaks: the sentence end in the back half wins over spaces.
        let text = format!("{}. {}", "x".repeat(700), "y ".repeat(400));
        let c = chunk_text(&text, 1000, 100);
        assert_eq!(c[0], format!("{}.", "x".repeat(700)));
        // No sentence end either: cut at a space, never mid-word.
        let c = chunk_text(&"yy ".repeat(800), 1000, 100);
        assert!(c[0].ends_with("yy") && c[0].chars().count() <= 1000);
        assert!(c.iter().all(|s| s.split(' ').all(|w| w == "yy")));
        // Nothing to cut at: a hard cut at the size.
        let c = chunk_text(&"q".repeat(2500), 1000, 200);
        assert_eq!(c[0].len(), 1000);
        assert_eq!(c.concat().len(), 2500);
    }

    #[test]
    fn chunking_is_char_safe_and_terminates() {
        let text = "é".repeat(3000);
        let chunks = chunk_text(&text, 1200, 200);
        assert!(chunks.len() >= 3);
        assert!(chunks.iter().all(|c| c.chars().count() <= 1200));
    }

    #[test]
    fn cosine_basics() {
        assert!((cosine(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
        assert!(cosine(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
        assert_eq!(cosine(&[1.0], &[1.0, 2.0]), 0.0);
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 1.0]), 0.0);
    }

    #[test]
    fn parses_embeddings_in_index_order() {
        let v = json!({"data": [{"index": 1, "embedding": [0.0, 1.0]}, {"index": 0, "embedding": [1.0, 0.0]}]});
        assert_eq!(parse_embeddings(&v, 2).unwrap(), vec![vec![1.0, 0.0], vec![0.0, 1.0]]);
        assert!(parse_embeddings(&v, 3).is_err());
        assert!(parse_embeddings(&json!({"error": "x"}), 1).is_err());
    }

    #[test]
    fn parses_rerank_best_first_and_rejects_bad_index() {
        let v = json!({"results": [{"index": 0, "relevance_score": -2.0}, {"index": 1, "relevance_score": 3.5}]});
        assert_eq!(parse_rerank(&v, 2).unwrap(), vec![(1, 3.5), (0, -2.0)]);
        assert!(parse_rerank(&json!({"results": [{"index": 5, "relevance_score": 1.0}]}), 2).is_err());
    }

    #[test]
    fn config_defaults_and_gating() {
        let mut c = RetrievalConfig::default();
        assert!(!c.is_configured());
        c.embed_url = "http://h".into();
        c.embed_model = "m".into();
        assert!(c.is_configured() && !c.reranks());
        assert_eq!((c.top_k(), c.candidates()), (5, 20));
        c.top_k = 30;
        assert_eq!(c.candidates(), 30, "never fewer candidates than passages");
    }

    #[test]
    fn passages_ride_the_latest_user_turn() {
        use crate::provider::ChatTurn;
        let mut turns = vec![ChatTurn::user("Ann: first"), ChatTurn::assistant("ok"), ChatTurn::user("Ann: second  ")];
        assert_eq!(latest_user_query(&turns), Some("Ann: second"));
        assert!(append_to_latest_user(&mut turns, "[P]"));
        assert_eq!(turns[0].content, "Ann: first");
        assert_eq!(turns[2].content, "Ann: second\n\n[P]");
        let mut none = vec![ChatTurn::assistant("x")];
        assert_eq!(latest_user_query(&none), None);
        assert!(!append_to_latest_user(&mut none, "[P]"));
        assert_eq!(none[0].content, "x");
    }

    #[test]
    fn passages_section_format() {
        assert_eq!(passages_section(&[]), "");
        let s = passages_section(&[Passage { source: "docs".into(), text: " body \n".into(), score: 1.0 }]);
        assert!(s.starts_with(PASSAGES_HEADER));
        assert!(s.ends_with("--- docs (passage 1) ---\nbody"));
    }
}

/// Retrieval against in-process fake embed/rerank servers.
#[cfg(test)]
mod wire {
    use super::*;
    use axum::extract::State;
    use axum::routing::post;
    use axum::{Json, Router};
    use std::sync::{Arc, Mutex as StdMutex};

    /// Toy embedder: a 3-dim bag of keywords, so cosine is predictable.
    fn toy_embed(s: &str) -> Vec<f32> {
        let s = s.to_lowercase();
        let n = |w: &str| s.matches(w).count() as f32;
        vec![n("cat") + 0.01, n("dog") + 0.01, n("fish") + 0.01]
    }

    #[derive(Clone, Default)]
    struct Fake {
        embed_inputs: Arc<StdMutex<Vec<String>>>,
        rerank_bodies: Arc<StdMutex<Vec<Value>>>,
        /// Rerank answer: reverse the candidate order (proves rerank wins).
        reverse: bool,
        fail_rerank: bool,
    }

    async fn embeddings(State(f): State<Fake>, Json(body): Json<Value>) -> Json<Value> {
        let inputs: Vec<String> = body["input"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        f.embed_inputs.lock().unwrap().extend(inputs.clone());
        // Return out of order to exercise index sorting.
        let data: Vec<Value> = inputs
            .iter()
            .enumerate()
            .rev()
            .map(|(i, s)| json!({"index": i, "embedding": toy_embed(s)}))
            .collect();
        Json(json!({"data": data}))
    }

    async fn rerank(State(f): State<Fake>, Json(body): Json<Value>) -> axum::response::Response {
        use axum::response::IntoResponse;
        f.rerank_bodies.lock().unwrap().push(body.clone());
        if f.fail_rerank {
            return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "boom").into_response();
        }
        let n = body["documents"].as_array().unwrap().len();
        let results: Vec<Value> = (0..n)
            .map(|i| {
                let score = if f.reverse { i as f64 } else { -(i as f64) };
                json!({"index": i, "relevance_score": score})
            })
            .collect();
        Json(json!({"results": results})).into_response()
    }

    async fn serve(f: Fake) -> String {
        let app = Router::new()
            .route("/v1/embeddings", post(embeddings))
            .route("/v1/rerank", post(rerank))
            .with_state(f);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }

    fn sources() -> Vec<(String, String)> {
        vec![
            ("pets".into(), "The cat sleeps all day. Cats purr.\n\nThe dog fetches the ball.".into()),
            ("water".into(), "A fish swims in the pond.".into()),
        ]
    }

    fn cfg(base: &str, rerank: bool) -> RetrievalConfig {
        RetrievalConfig {
            embed_url: base.into(),
            embed_model: "embeddinggemma-300m".into(),
            rerank_url: if rerank { format!("{base}/v1") } else { String::new() },
            rerank_model: if rerank { "bge-reranker-v2-m3".into() } else { String::new() },
            top_k: 2,
            candidates: 20,
        }
    }

    #[tokio::test]
    async fn cosine_ranks_and_caches_embeddings_by_content() {
        let f = Fake::default();
        let base = serve(f.clone()).await;
        let r = Retriever::new();
        let got = r.retrieve(&cfg(&base, false), &sources(), "where is the fish?").await.unwrap();
        assert_eq!(got[0].source, "water");
        assert_eq!(got[0].text, "A fish swims in the pond.");
        assert_eq!(got.len(), 2);
        let first = f.embed_inputs.lock().unwrap().clone();
        assert!(first.iter().any(|s| s.starts_with("title: none | text: ")), "gemma doc prefix");
        assert!(first.iter().any(|s| s == "task: search result | query: where is the fish?"));
        let cached = r.cached();
        assert_eq!(cached, 2);

        // Second turn: only the query is embedded again.
        f.embed_inputs.lock().unwrap().clear();
        let got = r.retrieve(&cfg(&base, false), &sources(), "a dog?").await.unwrap();
        assert_eq!(got[0].source, "pets");
        assert_eq!(*f.embed_inputs.lock().unwrap(), vec!["task: search result | query: a dog?".to_string()]);
        assert_eq!(r.cached(), cached);
    }

    #[tokio::test]
    async fn rerank_reorders_candidates_and_caps_top_k() {
        let f = Fake { reverse: true, ..Default::default() };
        let base = serve(f.clone()).await;
        let r = Retriever::new();
        let mut c = cfg(&base, true);
        c.top_k = 1;
        let got = r.retrieve(&c, &sources(), "fish").await.unwrap();
        // Cosine put "water" first; the (reversing) reranker promotes the last
        // candidate instead.
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].source, "pets");
        let body = f.rerank_bodies.lock().unwrap()[0].clone();
        assert_eq!(body["model"], "bge-reranker-v2-m3");
        assert_eq!(body["query"], "fish");
        assert_eq!(body["top_n"], 1);
        // Rerank sees the raw chunk, without the embedding prefix.
        assert_eq!(body["documents"][0], "A fish swims in the pond.");
    }

    #[tokio::test]
    async fn errors_surface_for_fallback() {
        let f = Fake { fail_rerank: true, ..Default::default() };
        let base = serve(f).await;
        let r = Retriever::new();
        assert!(r.retrieve(&cfg(&base, true), &sources(), "fish").await.is_err());
        // Unreachable embedder.
        let dead = cfg("http://127.0.0.1:9", false);
        assert!(r.retrieve(&dead, &sources(), "fish").await.is_err());
        // Not configured.
        assert!(r.retrieve(&RetrievalConfig::default(), &sources(), "fish").await.is_err());
    }

    #[tokio::test]
    async fn empty_inputs_retrieve_nothing_without_calls() {
        let f = Fake::default();
        let base = serve(f.clone()).await;
        let r = Retriever::new();
        assert!(r.retrieve(&cfg(&base, true), &[], "fish").await.unwrap().is_empty());
        assert!(r.retrieve(&cfg(&base, true), &sources(), "  ").await.unwrap().is_empty());
        assert!(f.embed_inputs.lock().unwrap().is_empty());
    }
}
