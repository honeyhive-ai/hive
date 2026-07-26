//! Vault sources — ported from `VaultSource` / `GitHubVaultSource` /
//! `GitLabVaultSource` / `HTTPSVaultSource` in `HiveModels.swift`. A vault is a
//! reference-material source mounted into the workspace; this models *where*
//! the content comes from. Fetching lives in `hive-runtime`.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A vault source as mounted in the workspace, plus optional agent targeting.
///
/// `VaultSource` is an internally-tagged enum, so we can't add a field to it
/// directly. Instead this wrapper `#[serde(flatten)]`s the source, which keeps
/// the on-the-wire JSON shape identical to a bare `VaultSource`
/// (`{"kind":"gitHub", …}`) and appends `agentIds`. Old `vault_sources` records
/// (no `agentIds`) therefore still deserialize, with `agent_ids` defaulting to
/// empty (⇒ global). Empty ⇒ applies to the primary runtime and every agent;
/// non-empty ⇒ only the listed agent ids (the primary never receives it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MountedVault {
    #[serde(flatten)]
    pub source: VaultSource,
    #[serde(default)]
    pub agent_ids: Vec<Uuid>,
}

impl MountedVault {
    /// Mount a source with no agent targeting (global).
    pub fn new(source: VaultSource) -> Self {
        Self {
            source,
            agent_ids: Vec::new(),
        }
    }

    /// A fetchable raw URL for the underlying source.
    pub fn raw_url(&self) -> String {
        self.source.raw_url()
    }

    /// A short human label for the underlying source.
    pub fn label(&self) -> String {
        self.source.label()
    }

    /// Whether this vault applies to the given responder. `None` = the primary
    /// runtime (only globals apply); `Some(id)` = a specific agent (globals plus
    /// any vault that targets its id).
    pub fn applies_to(&self, agent_id: Option<Uuid>) -> bool {
        self.agent_ids.is_empty() || agent_id.is_some_and(|id| self.agent_ids.contains(&id))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum VaultSource {
    GitHub {
        owner: String,
        repo: String,
        #[serde(default)]
        path: String,
        #[serde(default = "default_branch")]
        branch: String,
    },
    GitLab {
        project: String,
        #[serde(default)]
        path: String,
        #[serde(default = "default_branch")]
        branch: String,
    },
    Https {
        url: String,
    },
}

fn default_branch() -> String {
    "HEAD".to_string()
}

impl VaultSource {
    /// A fetchable raw URL for this source.
    pub fn raw_url(&self) -> String {
        match self {
            VaultSource::GitHub {
                owner,
                repo,
                path,
                branch,
            } => format!(
                "https://raw.githubusercontent.com/{owner}/{repo}/{branch}/{}",
                path.trim_start_matches('/')
            ),
            VaultSource::GitLab {
                project,
                path,
                branch,
            } => format!(
                "https://gitlab.com/{project}/-/raw/{branch}/{}",
                path.trim_start_matches('/')
            ),
            VaultSource::Https { url } => url.clone(),
        }
    }

    /// A short human label for the source.
    pub fn label(&self) -> String {
        match self {
            VaultSource::GitHub { owner, repo, .. } => format!("github:{owner}/{repo}"),
            VaultSource::GitLab { project, .. } => format!("gitlab:{project}"),
            VaultSource::Https { url } => url.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_raw_url() {
        let s = VaultSource::GitHub {
            owner: "acme".into(),
            repo: "docs".into(),
            path: "guide.md".into(),
            branch: "main".into(),
        };
        assert_eq!(
            s.raw_url(),
            "https://raw.githubusercontent.com/acme/docs/main/guide.md"
        );
        assert_eq!(s.label(), "github:acme/docs");
    }

    #[test]
    fn gitlab_raw_url() {
        let s = VaultSource::GitLab {
            project: "group/proj".into(),
            path: "README.md".into(),
            branch: "HEAD".into(),
        };
        assert_eq!(s.raw_url(), "https://gitlab.com/group/proj/-/raw/HEAD/README.md");
    }

    #[test]
    fn defaults_branch_to_head_on_decode() {
        let json = r#"{"kind":"gitHub","owner":"a","repo":"b","path":"x"}"#;
        let s: VaultSource = serde_json::from_str(json).unwrap();
        match s {
            VaultSource::GitHub { branch, .. } => assert_eq!(branch, "HEAD"),
            _ => panic!(),
        }
    }

    #[test]
    fn old_flat_vault_json_deserializes_as_global_mounted_vault() {
        // A record written before agent targeting existed: the bare enum shape.
        let json = r#"{"kind":"gitHub","owner":"a","repo":"b","path":"x","branch":"main"}"#;
        let v: MountedVault = serde_json::from_str(json).unwrap();
        assert!(v.agent_ids.is_empty());
        assert_eq!(v.raw_url(), "https://raw.githubusercontent.com/a/b/main/x");
        assert!(v.applies_to(None));
        assert!(v.applies_to(Some(Uuid::new_v4())));
    }

    #[test]
    fn mounted_vault_round_trips_flat_with_agent_ids() {
        let target = Uuid::new_v4();
        let v = MountedVault {
            source: VaultSource::Https {
                url: "https://example.com/doc".into(),
            },
            agent_ids: vec![target],
        };
        let json = serde_json::to_string(&v).unwrap();
        // Flattened: the source's `kind` sits at the top level, not nested.
        assert!(json.contains("\"kind\":\"https\""));
        assert!(json.contains("\"agentIds\""));
        let back: MountedVault = serde_json::from_str(&json).unwrap();
        assert_eq!(back, v);
    }

    #[test]
    fn targeted_vault_applies_only_to_listed_agents() {
        let target = Uuid::new_v4();
        let other = Uuid::new_v4();
        let v = MountedVault {
            source: VaultSource::Https {
                url: "https://example.com/doc".into(),
            },
            agent_ids: vec![target],
        };
        assert!(v.applies_to(Some(target)));
        assert!(!v.applies_to(Some(other)));
        assert!(!v.applies_to(None));
    }
}
