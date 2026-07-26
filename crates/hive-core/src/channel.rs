//! Workspace channels — the organization layer (spec §11). A channel is a named
//! topic that groups chats; it carries no configuration of its own (all config
//! lives on the workspace). Channels ride the **workspace-config log** (a
//! reserved event stream keyed off the workspace id) so they sync E2EE to every
//! member through the existing per-session sync machinery.

use serde::{Deserialize, Serialize};

use crate::time_util::Timestamp;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Channel {
    pub id: String,
    pub name: String,
    /// Optional one-line purpose. Empty/None when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    /// Sort position in the sidebar (ascending). Ties break by id.
    #[serde(default)]
    pub position: i32,
    #[serde(default)]
    pub archived: bool,
    /// The channel's drop-in chat, created with it and titled after it. Never
    /// empty for a live channel; it lives and dies with the channel.
    #[serde(default)]
    pub default_chat_id: String,
    #[serde(default)]
    pub created_at: Timestamp,
}

impl Channel {
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            purpose: None,
            position: 0,
            archived: false,
            default_chat_id: String::new(),
            created_at: Timestamp::now(),
        }
    }
}
