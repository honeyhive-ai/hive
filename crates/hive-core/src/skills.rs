//! Skills — ported from `SkillProfile` in `HiveModels.swift`. A skill is a
//! reusable instruction bundle (optionally fetched from the internet) that, when
//! loaded into a session, is injected into participants' system prompts.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::time_util::Timestamp;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillProfile {
    pub id: Uuid,
    pub name: String,
    /// The instruction text injected into the system prompt.
    pub instructions: String,
    /// Where it was installed from, if fetched remotely.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    #[serde(default)]
    pub created_at: Timestamp,
}

impl SkillProfile {
    pub fn new(name: impl Into<String>, instructions: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            instructions: instructions.into(),
            source_url: None,
            created_at: Timestamp::now(),
        }
    }
}
