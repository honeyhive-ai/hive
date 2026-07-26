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
    /// Optional agent targeting. Empty ⇒ global: applies to the primary runtime
    /// and every agent. Non-empty ⇒ only the listed agent ids receive this skill
    /// (the primary never does). `#[serde(default)]` keeps old records global.
    #[serde(default)]
    pub agent_ids: Vec<Uuid>,
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
            agent_ids: Vec::new(),
            created_at: Timestamp::now(),
        }
    }

    /// Whether this skill applies to the given responder. `None` = the primary
    /// runtime (only globals apply); `Some(id)` = a specific agent (globals plus
    /// any skill that targets its id).
    pub fn applies_to(&self, agent_id: Option<Uuid>) -> bool {
        self.agent_ids.is_empty() || agent_id.is_some_and(|id| self.agent_ids.contains(&id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_json_without_agent_ids_is_global() {
        let json = r#"{"id":"00000000-0000-0000-0000-0000000000aa","name":"Concise","instructions":"be brief"}"#;
        let skill: SkillProfile = serde_json::from_str(json).unwrap();
        assert!(skill.agent_ids.is_empty());
        assert!(skill.applies_to(None));
        assert!(skill.applies_to(Some(Uuid::new_v4())));
    }

    #[test]
    fn targeted_skill_applies_only_to_listed_agents() {
        let target = Uuid::new_v4();
        let other = Uuid::new_v4();
        let mut skill = SkillProfile::new("Reviewer", "review diffs");
        skill.agent_ids = vec![target];
        assert!(skill.applies_to(Some(target)));
        assert!(!skill.applies_to(Some(other)));
        assert!(!skill.applies_to(None));
    }
}
