//! Repo bindings for a headless host — the CLI's equivalent of the desktop's
//! device-local workspace code-dir bindings (`workspace_dirs` in the app's
//! settings).
//!
//! An agent turn on this box that may edit files needs a git checkout to cut
//! its per-turn worktree from. Checkouts live under one directory
//! (`HIVE_REPOS_DIR`, default `<HIVE_DATA_DIR>/repos`), and a small TOML file
//! (`<HIVE_DATA_DIR>/repo-bindings.toml`) maps the workspace — and optionally
//! single chats — to one of them:
//!
//! ```toml
//! workspace = "hive"              # a name under HIVE_REPOS_DIR, or an absolute path
//! [chats]
//! "6f1c…-chat-uuid" = "rdna-gateway"
//! ```
//!
//! Resolution for a turn: the chat's binding, else the workspace binding,
//! else none (the turn gets no file tools; MCP tools still apply). Written by
//! `hive bind-repo`; the binding is device-local and never synced, exactly
//! like the desktop's.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The binding file's name inside the data dir.
pub const BINDINGS_FILE: &str = "repo-bindings.toml";

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct RepoBindings {
    /// The workspace's checkout (name under the repos dir, or absolute path).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// Per-chat overrides, keyed by chat id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub chats: BTreeMap<String, String>,
}

/// Where checkouts live: `HIVE_REPOS_DIR`, else `<data_dir>/repos`.
pub fn repos_dir(data_dir: &Path) -> PathBuf {
    crate::env_opt("HIVE_REPOS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| data_dir.join("repos"))
}

impl RepoBindings {
    pub fn load(data_dir: &Path) -> Self {
        std::fs::read_to_string(data_dir.join(BINDINGS_FILE))
            .ok()
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, data_dir: &Path) -> Result<()> {
        std::fs::create_dir_all(data_dir)?;
        std::fs::write(data_dir.join(BINDINGS_FILE), toml::to_string_pretty(self)?)?;
        Ok(())
    }

    /// The checkout a turn in `chat` uses: the chat's binding, else the
    /// workspace's. Relative names resolve under `repos_dir`.
    pub fn resolve(&self, repos_dir: &Path, chat: Uuid) -> Option<PathBuf> {
        let raw = self.chats.get(&chat.to_string()).or(self.workspace.as_ref())?;
        Some(resolve_path(repos_dir, raw))
    }
}

fn resolve_path(repos_dir: &Path, raw: &str) -> PathBuf {
    let p = Path::new(raw.trim());
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        repos_dir.join(p)
    }
}

/// `hive bind-repo <name|path> [--chat <id>]` / `hive bind-repo --clear [--chat <id>]`.
pub fn cmd_bind_repo(data_dir: &Path, target: Option<&str>, chat: Option<&str>, clear: bool) -> Result<()> {
    let mut b = RepoBindings::load(data_dir);
    let chat = chat.map(|c| Uuid::parse_str(c.trim()).context("--chat must be a chat id")).transpose()?;
    if clear {
        match chat {
            Some(c) => {
                b.chats.remove(&c.to_string());
            }
            None => b.workspace = None,
        }
        b.save(data_dir)?;
        println!("binding cleared.");
        return Ok(());
    }
    let Some(target) = target.map(str::trim).filter(|t| !t.is_empty()) else {
        bail!("usage: hive bind-repo <name-under-repos-dir | /absolute/path> [--chat <chat-id>]  (or --clear)");
    };
    let dir = resolve_path(&repos_dir(data_dir), target);
    if !hive_runtime::git_worktree::is_git_repo(&dir) {
        bail!("{} is not a git checkout (clone it there first)", dir.display());
    }
    match chat {
        Some(c) => {
            b.chats.insert(c.to_string(), target.to_string());
        }
        None => b.workspace = Some(target.to_string()),
    }
    b.save(data_dir)?;
    println!("bound {} to {}.", chat.map(|c| format!("chat {c}")).unwrap_or_else(|| "the workspace".into()), dir.display());
    Ok(())
}

/// `hive repos` — show the repos dir and the bindings.
pub fn cmd_repos(data_dir: &Path) -> Result<()> {
    let dir = repos_dir(data_dir);
    let b = RepoBindings::load(data_dir);
    println!("repos dir : {}", dir.display());
    match &b.workspace {
        Some(w) => println!("workspace : {w} -> {}", resolve_path(&dir, w).display()),
        None => println!("workspace : (unbound: agent turns get no file tools)"),
    }
    for (chat, target) in &b.chats {
        println!("chat {chat} : {target} -> {}", resolve_path(&dir, target).display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_binding_wins_over_workspace_and_names_resolve_under_repos_dir() {
        let chat = Uuid::new_v4();
        let other = Uuid::new_v4();
        let mut b = RepoBindings { workspace: Some("main".into()), ..Default::default() };
        b.chats.insert(chat.to_string(), "/abs/elsewhere".into());
        let repos = Path::new("/data/repos");
        assert_eq!(b.resolve(repos, chat), Some(PathBuf::from("/abs/elsewhere")));
        assert_eq!(b.resolve(repos, other), Some(PathBuf::from("/data/repos/main")));
        assert_eq!(RepoBindings::default().resolve(repos, other), None);
    }

    #[test]
    fn round_trips_through_toml() {
        let dir = tempfile::tempdir().unwrap();
        let mut b = RepoBindings { workspace: Some("hive".into()), ..Default::default() };
        b.chats.insert(Uuid::nil().to_string(), "x".into());
        b.save(dir.path()).unwrap();
        assert_eq!(RepoBindings::load(dir.path()), b);
    }
}
