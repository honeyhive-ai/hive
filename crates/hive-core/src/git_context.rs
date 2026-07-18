//! Git working-tree context — ported from `GitContext.swift`. Shells out to
//! `git` (porcelain v1) against a workspace root to produce the status pill
//! snapshot, per-file change list, and unified working-tree diffs for the Diff
//! canvas. Reads the working tree, so it covers changes made by anyone (Hive
//! proposals, the primary runtime, or subprocess agents).

use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GitChangeKind {
    Modified,
    Added,
    Deleted,
    Renamed,
    Untracked,
    Conflicted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitSnapshot {
    pub branch: Option<String>,
    pub modified_count: u32,
    pub untracked_count: u32,
    pub staged_count: u32,
    pub ahead_count: u32,
    pub behind_count: u32,
    pub is_repository: bool,
}

impl GitSnapshot {
    pub fn not_a_repository() -> Self {
        Self {
            branch: None,
            modified_count: 0,
            untracked_count: 0,
            staged_count: 0,
            ahead_count: 0,
            behind_count: 0,
            is_repository: false,
        }
    }

    pub fn is_clean(&self) -> bool {
        self.modified_count == 0 && self.untracked_count == 0 && self.staged_count == 0
    }

    pub fn dirty_count(&self) -> u32 {
        self.modified_count + self.untracked_count + self.staged_count
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitFileChange {
    pub id: String,
    pub path: String,
    pub kind: GitChangeKind,
    pub is_staged: bool,
}

/// A single file's uncommitted change as a unified diff. For tracked files the
/// patch comes from `git diff HEAD`; for untracked files git has no baseline,
/// so an all-added patch is synthesized from the file contents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitFileDiff {
    pub id: String,
    pub path: String,
    pub kind: GitChangeKind,
    pub patch: String,
    pub added_lines: u32,
    pub removed_lines: u32,
}

/// Run `git` in `path`, returning stdout on a zero exit, else `None`.
fn run(args: &[&str], path: &str) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

fn is_directory(path: &str) -> bool {
    Path::new(path).is_dir()
}

fn kind_for(code: char) -> GitChangeKind {
    match code {
        'M' => GitChangeKind::Modified,
        'A' => GitChangeKind::Added,
        'D' => GitChangeKind::Deleted,
        'R' | 'C' => GitChangeKind::Renamed,
        'U' => GitChangeKind::Conflicted,
        _ => GitChangeKind::Modified,
    }
}

/// Parse the porcelain `## branch...remote [ahead N, behind M]` header.
fn parse_branch_line(line: &str) -> (Option<String>, u32, u32) {
    let body = line.trim_start_matches("##").trim();
    if body.starts_with("HEAD") {
        return (None, 0, 0);
    }
    let branch_part = if let Some(idx) = body.find("...") {
        &body[..idx]
    } else if let Some(idx) = body.find(' ') {
        &body[..idx]
    } else {
        body
    };

    let mut ahead = 0;
    let mut behind = 0;
    if let (Some(open), Some(close)) = (body.find('['), body.find(']')) {
        if open < close {
            let inside = &body[open + 1..close];
            for chunk in inside.split(',') {
                let trimmed = chunk.trim();
                if let Some(n) = trimmed.strip_prefix("ahead ") {
                    ahead = n.trim().parse().unwrap_or(0);
                } else if let Some(n) = trimmed.strip_prefix("behind ") {
                    behind = n.trim().parse().unwrap_or(0);
                }
            }
        }
    }
    let branch = if branch_part.is_empty() {
        None
    } else {
        Some(branch_part.to_string())
    };
    (branch, ahead, behind)
}

fn count_changes(patch: &str) -> (u32, u32) {
    let mut added = 0;
    let mut removed = 0;
    for line in patch.split('\n') {
        if line.starts_with("+++") || line.starts_with("---") {
            continue;
        }
        if line.starts_with('+') {
            added += 1;
        } else if line.starts_with('-') {
            removed += 1;
        }
    }
    (added, removed)
}

/// Read-only git reader bound to no particular path; methods take the root.
#[derive(Debug, Default, Clone, Copy)]
pub struct GitContextReader;

impl GitContextReader {
    pub fn new() -> Self {
        Self
    }

    /// Branch + dirty counts. Returns `not_a_repository` on any failure.
    pub fn snapshot(&self, path: &str) -> GitSnapshot {
        if !is_directory(path) {
            return GitSnapshot::not_a_repository();
        }
        let porcelain = match run(&["status", "--porcelain=v1", "--branch"], path) {
            Some(p) if !p.is_empty() => p,
            _ => return GitSnapshot::not_a_repository(),
        };

        let lines: Vec<&str> = porcelain.split('\n').collect();
        let branch_line = match lines.first() {
            Some(l) if l.starts_with("##") => *l,
            _ => return GitSnapshot::not_a_repository(),
        };
        let (branch, ahead, behind) = parse_branch_line(branch_line);

        let mut modified = 0;
        let mut untracked = 0;
        let mut staged = 0;
        for line in lines.iter().skip(1).filter(|l| !l.is_empty()) {
            if line.starts_with("??") {
                untracked += 1;
                continue;
            }
            let chars: Vec<char> = line.chars().take(2).collect();
            if let Some(&x) = chars.first() {
                if x != ' ' && x != '?' {
                    staged += 1;
                }
            }
            if let Some(&y) = chars.get(1) {
                if y != ' ' && y != '?' {
                    modified += 1;
                }
            }
        }

        GitSnapshot {
            branch,
            modified_count: modified,
            untracked_count: untracked,
            staged_count: staged,
            ahead_count: ahead,
            behind_count: behind,
            is_repository: true,
        }
    }

    /// Per-file porcelain changes. Empty when `path` isn't a repository.
    pub fn files(&self, path: &str) -> Vec<GitFileChange> {
        if !is_directory(path) {
            return Vec::new();
        }
        let porcelain = match run(&["status", "--porcelain=v1"], path) {
            Some(p) if !p.is_empty() => p,
            _ => return Vec::new(),
        };

        let mut out = Vec::new();
        for raw in porcelain.split('\n') {
            if raw.is_empty() || raw.chars().count() < 4 {
                continue;
            }
            let chars: Vec<char> = raw.chars().collect();
            let x = chars[0];
            let y = chars[1];
            let file_path: String = chars[3..].iter().collect();

            if x == '?' && y == '?' {
                out.push(GitFileChange {
                    id: format!("untracked:{file_path}"),
                    path: file_path,
                    kind: GitChangeKind::Untracked,
                    is_staged: false,
                });
                continue;
            }
            if x == 'U' || y == 'U' || (x == 'A' && y == 'A') || (x == 'D' && y == 'D') {
                out.push(GitFileChange {
                    id: format!("conflict:{file_path}"),
                    path: file_path,
                    kind: GitChangeKind::Conflicted,
                    is_staged: false,
                });
                continue;
            }
            if x != ' ' {
                out.push(GitFileChange {
                    id: format!("staged:{file_path}:{x}"),
                    path: file_path.clone(),
                    kind: kind_for(x),
                    is_staged: true,
                });
            }
            if y != ' ' {
                out.push(GitFileChange {
                    id: format!("unstaged:{file_path}:{y}"),
                    path: file_path,
                    kind: kind_for(y),
                    is_staged: false,
                });
            }
        }
        out
    }

    /// Unified working-tree diffs for every uncommitted change (deduped by
    /// path). Empty when `path` isn't a repository.
    pub fn working_tree_diffs(&self, path: &str) -> Vec<GitFileDiff> {
        if !is_directory(path) {
            return Vec::new();
        }
        let mut seen = std::collections::HashSet::new();
        let mut diffs = Vec::new();
        for change in self.files(path) {
            if !seen.insert(change.path.clone()) {
                continue;
            }
            let patch = if change.kind == GitChangeKind::Untracked {
                let file_path = Path::new(path).join(&change.path);
                match std::fs::read_to_string(&file_path) {
                    Ok(content) if content.is_empty() => "(new empty file)".to_string(),
                    Ok(content) => content
                        .split('\n')
                        .map(|l| format!("+{l}"))
                        .collect::<Vec<_>>()
                        .join("\n"),
                    Err(_) => "(new or binary file — open in the Editor to view)".to_string(),
                }
            } else {
                run(&["diff", "HEAD", "--", &change.path], path).unwrap_or_default()
            };

            let (added, removed) = count_changes(&patch);
            diffs.push(GitFileDiff {
                id: change.path.clone(),
                path: change.path,
                kind: change.kind,
                patch: if patch.is_empty() {
                    "(no textual diff)".to_string()
                } else {
                    patch
                },
                added_lines: added,
                removed_lines: removed,
            });
        }
        diffs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_branch_with_ahead_behind() {
        let (b, a, behind) = parse_branch_line("## main...origin/main [ahead 1, behind 2]");
        assert_eq!(b.as_deref(), Some("main"));
        assert_eq!(a, 1);
        assert_eq!(behind, 2);
    }

    #[test]
    fn detached_head_has_no_branch() {
        let (b, a, behind) = parse_branch_line("## HEAD (no branch)");
        assert!(b.is_none());
        assert_eq!((a, behind), (0, 0));
    }

    #[test]
    fn counts_added_and_removed_skipping_headers() {
        let patch = "--- a/x\n+++ b/x\n+added one\n+added two\n-removed one\n context";
        let (added, removed) = count_changes(patch);
        assert_eq!(added, 2);
        assert_eq!(removed, 1);
    }

    #[test]
    fn non_repository_path_yields_neutral_snapshot() {
        let snap = GitContextReader::new().snapshot("/nonexistent/path/xyz");
        assert!(!snap.is_repository);
        assert!(snap.is_clean());
    }
}
