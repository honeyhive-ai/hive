//! HTTP plumbing shared by the model-endpoint clients (OpenAI-wire, native
//! Ollama): a connect-timeout client, one reconnect after a refused/dropped
//! connection, an idle window on the header wait, and errors that name the
//! provider and host. Kept provider-agnostic so both clients fail the same way.

use std::time::Duration;

use super::anthropic::ProviderError;
use super::openai::endpoint_host;
use super::HTTP_CONNECT_TIMEOUT;

/// Pause before the single reconnect attempt.
pub const RETRY_DELAY: Duration = Duration::from_millis(500);

/// A reqwest client with the shared connect timeout applied.
pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(HTTP_CONNECT_TIMEOUT)
        .build()
        .unwrap_or_default()
}

/// Who we're talking to, for errors and logs.
#[derive(Debug, Clone, Copy)]
pub struct Peer<'a> {
    pub provider: &'static str,
    pub url: &'a str,
}

impl Peer<'_> {
    pub fn host(&self) -> String {
        endpoint_host(self.url)
    }

    pub fn idle_error(&self, idle: Duration) -> ProviderError {
        ProviderError::Idle { provider: self.provider, host: self.host(), secs: idle.as_secs() }
    }
}

/// Send `build()`'s request, waiting at most `idle` for response headers (a
/// cold local model can take a while to load — that's "activity" from the
/// user's point of view, so the same generous window applies), and reconnect
/// up to `retries` times if the connection was refused or dropped before any
/// response arrived. `build` is called once per attempt.
pub async fn send_with_retry(
    peer: Peer<'_>,
    idle: Duration,
    retries: u32,
    mut build: impl FnMut() -> reqwest::RequestBuilder,
) -> Result<reqwest::Response, ProviderError> {
    let mut attempt = 0u32;
    loop {
        let sent = tokio::time::timeout(idle, build().send()).await;
        match sent {
            Ok(Ok(resp)) => return Ok(resp),
            Ok(Err(e)) => {
                let transient = e.is_connect() || e.is_timeout() || e.is_request();
                if transient && attempt < retries {
                    attempt += 1;
                    tracing::warn!(
                        target: "dispatch",
                        provider = peer.provider,
                        host = %peer.host(),
                        attempt,
                        "connection failed ({e}); retrying"
                    );
                    tokio::time::sleep(RETRY_DELAY).await;
                    continue;
                }
                if transient {
                    return Err(ProviderError::Unreachable {
                        provider: peer.provider,
                        host: peer.host(),
                        detail: root_cause(&e),
                    });
                }
                return Err(ProviderError::Http(e));
            }
            Err(_) => return Err(peer.idle_error(idle)),
        }
    }
}

/// The innermost error message of a reqwest error chain (the OS-level reason:
/// "Connection refused", "No route to host", …) rather than the URL wrapper.
pub fn root_cause(e: &reqwest::Error) -> String {
    let mut src: &(dyn std::error::Error + 'static) = e;
    while let Some(next) = src.source() {
        src = next;
    }
    src.to_string()
}
