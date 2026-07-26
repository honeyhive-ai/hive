//! Workspace-owned runtimes (spec §12.5). A **detached** agent — one that keeps
//! working when its owner's laptop is closed, hosted on a workspace worker —
//! must run on a **workspace-owned credential**, never a member's personal API
//! key (personal keys never leave the owner's device). A `WorkspaceRuntime` is
//! that: a provider/model the workspace owns, with a credential the workspace
//! provides. It rides the config log, so it syncs to every member and any worker.
//!
//! The raw secret does not have to ride the synced log: `SecretRef` names an
//! out-of-band secret (an env var / secret mount on the worker), keeping keys off
//! the stream entirely. `Inline` carries the secret E2EE on the config log for
//! zero-provisioning setups (the workspace key already unlocks it). Neither is
//! key *escrow* — the workspace credential is independent of anyone's personal key.

use serde::{Deserialize, Serialize};

use crate::runtime::ModelProviderKind;
use crate::time_util::Timestamp;

/// How a workspace runtime's secret is obtained. `None` for keyless runtimes
/// (e.g. a local Ollama).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum WorkspaceCredential {
    /// Resolve from a named secret the worker holds out-of-band — env
    /// `HIVE_WS_SECRET_<secretRef>`. The raw key never rides the synced log.
    SecretRef { secret_ref: String },
    /// The secret, carried E2EE on the config log. Shared with members; no
    /// worker provisioning needed (the workspace key unlocks it).
    Inline { secret: String },
    /// No credential (keyless local runtime).
    None,
}

impl Default for WorkspaceCredential {
    fn default() -> Self {
        WorkspaceCredential::None
    }
}

impl WorkspaceCredential {
    /// The env var name a `SecretRef` resolves from (`HIVE_WS_SECRET_<ref>`).
    pub fn env_var(secret_ref: &str) -> String {
        format!("HIVE_WS_SECRET_{secret_ref}")
    }
    /// True when the raw secret is NOT carried on the synced log.
    pub fn is_off_log(&self) -> bool {
        !matches!(self, WorkspaceCredential::Inline { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceRuntime {
    pub id: String,
    pub name: String,
    pub provider: ModelProviderKind,
    pub model: String,
    #[serde(default)]
    pub endpoint: String,
    #[serde(default)]
    pub credential: WorkspaceCredential,
    #[serde(default)]
    pub created_at: Timestamp,
}

impl WorkspaceRuntime {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        provider: ModelProviderKind,
        model: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            provider,
            model: model.into(),
            endpoint: String::new(),
            credential: WorkspaceCredential::None,
            created_at: Timestamp::now(),
        }
    }
}
