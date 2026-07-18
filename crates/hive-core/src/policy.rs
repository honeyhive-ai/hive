//! Permission policy + workspace identity — ported from `HiveModels.swift`
//! (`PermissionScope`, `PermissionPolicy`, `Workspace`). The richer trust-grant
//! / policy-profile machinery is deferred to the authorization phase; this is
//! the slice the config loader and chat need.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PermissionScope {
    OneAction,
    Chat,
    Workspace,
    AlwaysAsk,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionPolicy {
    pub read_files: bool,
    pub write_files: bool,
    pub run_commands: bool,
    pub access_vaults: bool,
    pub access_network: bool,
    pub access_remote_runtime: bool,
    pub scope: PermissionScope,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Workspace {
    pub id: Uuid,
    pub name: String,
    pub root_path: String,
}

impl Workspace {
    pub fn new(name: impl Into<String>, root_path: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            root_path: root_path.into(),
        }
    }
}
