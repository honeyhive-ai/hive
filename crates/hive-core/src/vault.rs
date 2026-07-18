//! Vault sources — ported from `VaultSource` / `GitHubVaultSource` /
//! `GitLabVaultSource` / `HTTPSVaultSource` in `HiveModels.swift`. A vault is a
//! reference-material source mounted into the workspace; this models *where*
//! the content comes from. Fetching lives in `hive-runtime`.

use serde::{Deserialize, Serialize};

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
}
