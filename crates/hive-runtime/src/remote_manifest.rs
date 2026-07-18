//! Shared remote-manifest URL resolution — ported from
//! `RemoteManifestResolver.swift`. Used by both skill install and MCP-server
//! install so a user can paste any of the common forms and get a fetchable
//! HTTPS URL:
//!
//! - a full `https://…` URL → used as-is
//! - a GitHub blob URL (`github.com/<owner>/<repo>/blob/<ref>/<path>`) →
//!   rewritten to `raw.githubusercontent.com/<owner>/<repo>/<ref>/<path>`
//! - an `owner/repo/path…` shorthand → `raw.githubusercontent.com/owner/repo/HEAD/path`

use thiserror::Error;

#[derive(Debug, Error, PartialEq)]
pub enum ResolveError {
    #[error("empty manifest reference")]
    Empty,
    #[error("unsupported manifest reference: {0}")]
    Unsupported(String),
}

/// Resolve a user-supplied manifest reference into a fetchable HTTPS URL.
pub fn resolve_manifest_url(input: &str) -> Result<String, ResolveError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(ResolveError::Empty);
    }

    // 1) GitHub blob URL → raw URL
    if let Some(rest) = trimmed
        .strip_prefix("https://github.com/")
        .or_else(|| trimmed.strip_prefix("http://github.com/"))
    {
        // <owner>/<repo>/blob/<ref>/<path...>
        let parts: Vec<&str> = rest.splitn(5, '/').collect();
        if parts.len() == 5 && parts[2] == "blob" {
            return Ok(format!(
                "https://raw.githubusercontent.com/{}/{}/{}/{}",
                parts[0], parts[1], parts[3], parts[4]
            ));
        }
        // any other github.com URL we can't rewrite
        return Err(ResolveError::Unsupported(trimmed.to_string()));
    }

    // 2) Already an HTTPS/HTTP URL (incl. raw.githubusercontent.com) → as-is
    if trimmed.starts_with("https://") || trimmed.starts_with("http://") {
        return Ok(trimmed.to_string());
    }

    // 3) owner/repo/path… shorthand → raw on the default branch (HEAD)
    let segments: Vec<&str> = trimmed.split('/').filter(|s| !s.is_empty()).collect();
    if segments.len() >= 3 {
        let owner = segments[0];
        let repo = segments[1];
        let path = segments[2..].join("/");
        return Ok(format!(
            "https://raw.githubusercontent.com/{owner}/{repo}/HEAD/{path}"
        ));
    }

    Err(ResolveError::Unsupported(trimmed.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passes_through_https() {
        assert_eq!(
            resolve_manifest_url("https://example.com/skill.json").unwrap(),
            "https://example.com/skill.json"
        );
    }

    #[test]
    fn rewrites_github_blob_to_raw() {
        let r =
            resolve_manifest_url("https://github.com/acme/skills/blob/main/research.json").unwrap();
        assert_eq!(
            r,
            "https://raw.githubusercontent.com/acme/skills/main/research.json"
        );
    }

    #[test]
    fn expands_owner_repo_path_shorthand() {
        let r = resolve_manifest_url("acme/skills/research.json").unwrap();
        assert_eq!(
            r,
            "https://raw.githubusercontent.com/acme/skills/HEAD/research.json"
        );
    }

    #[test]
    fn nested_shorthand_path() {
        let r = resolve_manifest_url("acme/skills/dir/research.json").unwrap();
        assert_eq!(
            r,
            "https://raw.githubusercontent.com/acme/skills/HEAD/dir/research.json"
        );
    }

    #[test]
    fn rejects_empty_and_bare_names() {
        assert_eq!(resolve_manifest_url("  "), Err(ResolveError::Empty));
        assert!(matches!(
            resolve_manifest_url("just-a-name"),
            Err(ResolveError::Unsupported(_))
        ));
    }
}
