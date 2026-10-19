//! Live vault-retrieval check against a real OpenAI-compatible embedder and
//! reranker (e.g. a llama.cpp gateway). Skipped unless
//! `HIVE_RETRIEVAL_LIVE_URL` is set, so CI never touches the network:
//!
//! ```text
//! HIVE_RETRIEVAL_LIVE_URL=http://host:8080/v1 \
//! HIVE_RETRIEVAL_LIVE_EMBED_MODEL=embeddinggemma-300m \
//! HIVE_RETRIEVAL_LIVE_RERANK_MODEL=bge-reranker-v2-m3 \
//!   cargo test -p hive-runtime --test retrieval_live -- --nocapture
//! ```

use hive_runtime::retrieval::{RetrievalConfig, Retriever};
use std::time::Instant;

fn live() -> Option<RetrievalConfig> {
    let url = std::env::var("HIVE_RETRIEVAL_LIVE_URL").ok()?;
    let embed = std::env::var("HIVE_RETRIEVAL_LIVE_EMBED_MODEL").unwrap_or_else(|_| "embeddinggemma-300m".into());
    let rerank = std::env::var("HIVE_RETRIEVAL_LIVE_RERANK_MODEL").unwrap_or_else(|_| "bge-reranker-v2-m3".into());
    Some(RetrievalConfig {
        embed_url: url.clone(),
        embed_model: embed,
        rerank_url: url,
        rerank_model: rerank,
        top_k: 3,
        candidates: 20,
    })
}

/// Paragraphs on distinct topics, long enough to split into several chunks.
fn corpus() -> Vec<(String, String)> {
    let filler = "This section is general background and does not cover specific settings. ";
    let ops = format!(
        "{f}{f}{f}{f}{f}{f}{f}{f}{f}{f}{f}{f}{f}{f}{f}{f}\n\n\
         Backups: the nightly backup job runs at 02:30 UTC and keeps fourteen daily snapshots \
         in the cold-storage bucket. Restores are done with the restore-snapshot command and \
         take about forty minutes for the full database.\n\n\
         {f}{f}{f}{f}{f}{f}{f}{f}{f}{f}{f}{f}{f}{f}{f}{f}\n\n\
         Deploys: production deploys happen through the release pipeline after two approvals. \
         Rollbacks use the previous image tag and finish in under five minutes.\n\n{f}{f}{f}{f}{f}{f}{f}{f}",
        f = filler
    );
    let style = format!(
        "{f}{f}{f}{f}{f}{f}{f}{f}\n\n\
         Code style: Rust code is formatted with rustfmt using a max line width of 100. \
         Public functions need doc comments and errors use thiserror enums.\n\n\
         Frontend: React components live in web/src/components and use TanStack Query for \
         server state. Styling is Tailwind only; no CSS modules.\n\n{f}{f}{f}{f}{f}{f}{f}{f}{f}{f}{f}{f}{f}{f}{f}{f}",
        f = filler
    );
    let gpu = "GPU notes: the RDNA2 cards run llama.cpp with the ROCm backend. Wavefront size is \
               64 on these cards, and flash attention is enabled for the 32k context slots."
        .to_string();
    vec![("ops.md".into(), ops), ("style.md".into(), style), ("gpu.md".into(), gpu)]
}

#[tokio::test]
async fn right_passage_ranks_first() {
    let Some(cfg) = live() else {
        eprintln!("HIVE_RETRIEVAL_LIVE_URL unset; skipping live retrieval test");
        return;
    };
    let r = Retriever::new();
    let cases = [
        ("How long does a full database restore take?", "ops.md", "forty minutes"),
        ("What line width does rustfmt use here?", "style.md", "width of 100"),
        ("Which library handles server state in the web UI?", "style.md", "TanStack Query"),
        ("how do I roll back a bad release", "ops.md", "Rollbacks"),
        ("What is the wavefront size on the GPUs?", "gpu.md", "Wavefront size"),
    ];
    let chunks: usize = corpus().iter().map(|(_, t)| hive_runtime::retrieval::chunk_text(t, 1200, 200).len()).sum();
    eprintln!("corpus: {chunks} chunks");
    assert!(chunks >= 6, "the corpus splits into several chunks per doc");
    for (n, (q, source, needle)) in cases.iter().enumerate() {
        let t = Instant::now();
        let got = r.retrieve(&cfg, &corpus(), q).await.expect("retrieve");
        eprintln!(
            "{q:?} -> {} passages in {:.2}s (cached chunks {}); top {} score {:.3}: {:?}",
            got.len(),
            t.elapsed().as_secs_f64(),
            r.cached(),
            got[0].source,
            got[0].score,
            got[0].text.chars().take(90).collect::<String>()
        );
        assert_eq!(got[0].source, *source, "case {n}: {q}");
        assert!(got[0].text.contains(needle), "case {n}: top passage lacks {needle:?}: {}", got[0].text);
        assert!(got.len() <= 3);
    }
    // Cosine-only (no reranker) still finds the obvious one.
    let mut cos = cfg.clone();
    cos.rerank_url.clear();
    let got = r.retrieve(&cos, &corpus(), "nightly backup schedule time").await.expect("retrieve");
    assert!(got[0].text.contains("02:30"), "{}", got[0].text);
}
