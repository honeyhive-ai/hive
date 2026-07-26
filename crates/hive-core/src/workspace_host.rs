//! Workspace hosts (spec §12.4). A **host** is a machine that executes an
//! agent's turns: a member's `Device`, or an always-on `Worker` the workspace
//! owns. An agent binds to a host (`WorkspaceAgent.host_id`); a **detached**
//! agent is one bound to a worker, so it keeps running when its owner's laptop is
//! closed. Hosts ride the config log, so every member and worker sees the roster.
//!
//! A host's `id` is the machine's **device id** (from its identity) — a stable,
//! already-existing per-box identifier, so registering a host needs no new
//! provisioning: a box registers itself under its own device id.

use serde::{Deserialize, Serialize};

use crate::time_util::Timestamp;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HostKind {
    /// A member's machine — online only while their app/CLI runs.
    Device,
    /// An always-on machine the workspace owns (VM, container, office box).
    Worker,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceHost {
    /// The machine's device id (stringified). Stable per box.
    pub id: String,
    pub kind: HostKind,
    pub label: String,
    /// Actor who registered/owns the host (empty if unset).
    #[serde(default)]
    pub owner_actor_id: String,
    #[serde(default)]
    pub created_at: Timestamp,
}

impl WorkspaceHost {
    pub fn new(id: impl Into<String>, kind: HostKind, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            kind,
            label: label.into(),
            owner_actor_id: String::new(),
            created_at: Timestamp::now(),
        }
    }
}
