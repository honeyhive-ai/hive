//! Vault + manifest fetching — ported from `VaultFetcher.swift` /
//! `SkillFetcher.swift` / `MCPServerFetcher.swift`. Fetches text content over
//! HTTPS for a [`VaultSource`] or a resolved manifest URL.
//!
//! The URL resolution is pure + tested in `remote_manifest`; the network fetch
//! here is integration-only.

use hive_core::VaultSource;

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("fetch failed ({status})")]
    Status { status: u16 },
}

/// GET a URL and return its body text (errors on non-2xx).
pub async fn fetch_text(url: &str) -> Result<String, FetchError> {
    let resp = reqwest::Client::new().get(url).send().await?;
    if !resp.status().is_success() {
        return Err(FetchError::Status {
            status: resp.status().as_u16(),
        });
    }
    Ok(resp.text().await?)
}

/// Fetch the raw content for a vault source.
pub async fn fetch_vault(source: &VaultSource) -> Result<String, FetchError> {
    fetch_text(&source.raw_url()).await
}
