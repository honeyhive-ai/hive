//! `hive-runtime` — orchestration ported from the Swift `HiveRuntime` target:
//! the SQLite event store, identity store, provider adapters, MCP manager,
//! subprocess bridges, and the disk/relay/LAN/P2P transports.
//!
//! Phase 1 lands the SQLite event store and a file-based identity store.
//! Crypto/signing, providers, MCP, and transports arrive in later phases.

pub mod chat_service;
pub mod context;
pub mod directives;
pub mod envelope_verifier;
pub mod event_store;
pub mod git_attribution;
pub mod github;
pub mod identity_store;
pub mod mcp;
pub mod mcp_oauth;
pub mod mentions;
pub mod peer;
#[cfg(feature = "p2p")]
pub mod peer_iroh;
pub mod prompt;
pub mod provider;
pub mod relay_client;
pub mod remote_manifest;
pub mod sync_engine;
pub mod tool_loop;
pub mod vault_fetcher;

pub use chat_service::{turns_for, turns_from, ChatService};
pub use context::{Compactor, CompactionResult, Summarizer};
pub use mcp::{McpRegistry, McpServerSpec, McpTool, McpTransport};
pub use mentions::{parse_mentions, MentionTargets};
pub use relay_client::{
    AccountDevice, AccountIdentity, FetchedEnvelope, Friend, FriendPresence, FriendRequestOutcome,
    IncomingFriendRequest, InboxEvent, IssuedRelayToken, MemberEntry, RelayClient, RelayError,
    RelayProbe, RelayTokenEntry, RelayUserEntry,
};
pub use remote_manifest::resolve_manifest_url;
pub use sync_engine::{SyncEngine, SyncError};
pub use provider::dispatch::{self, ResolvedRuntime};
pub use provider::{AnthropicClient, ChatTurn, OpenAiClient, ProviderError};

pub use envelope_verifier::{
    verify_stream, DeviceKeyResolver, DeviceRoster, QuarantineReason, VerificationOutcome,
};
pub use event_store::{EventStore, EventStoreError};
pub use identity_store::{
    FileKeyVault, IdentityError, IdentityStore, KeyVault, StoredIdentity,
};

use hive_proto::AppInfo;

/// Assemble the [`AppInfo`] payload returned by the `get_app_info` IPC command.
pub fn app_info() -> AppInfo {
    AppInfo {
        name: "Hive".to_string(),
        core_version: hive_core::VERSION.to_string(),
        build_profile: if cfg!(debug_assertions) { "debug" } else { "release" }.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_info_reports_core_version() {
        let info = app_info();
        assert_eq!(info.name, "Hive");
        assert_eq!(info.core_version, hive_core::VERSION);
    }
}
