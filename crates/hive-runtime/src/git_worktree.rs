//! Isolated git worktrees for concurrent agent turns.
//!
//! A file-mutating subprocess agent (claude-code / aider / pi) normally runs in
//! the shared workspace root, so two turns editing files at once clobber each
//! other's uncommitted work. This module gives each such turn its own worktree
//! on a throwaway task branch cut from the current `HEAD`: the agent edits in
//! isolation, and the resulting diff surfaces for review and merges back on
//! approval (the review-gated model). Turns that don't run in a git repo — or
//! don't mutate files — skip this entirely and run in place.
//!
//! Worktrees live under `<repo>/.hive/worktrees/<turn-id>` and are registered in
//! `.git/worktrees`, so git excludes them from the main tree's status. They are
//! removed when the turn's changes are merged or discarded.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A live isolated worktree for one turn. Drop does NOT auto-remove — call
/// [`Worktree::remove`] once the turn's diff has been captured (merged or
/// discarded), so a captured-but-unmerged diff isn't lost by an early drop.
#[derive(Debug, Clone)]
pub struct Worktree {
    /// Absolute path to the checked-out worktree (the agent's working dir).
    pub path: PathBuf,
    /// The task branch, e.g. `hive/turn-<id>`.
    pub branch: String,
    /// The repository the worktree belongs to (the main workspace root).
    repo_root: PathBuf,
}

/// Whether `root` is inside a git working tree. Isolation only applies to repos.
pub fn is_git_repo(root: &Path) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .map(|o| o.status.success() && o.stdout.starts_with(b"true"))
        .unwrap_or(false)
}

fn git(dir: &Path, args: &[&str]) -> io::Result<String> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output()?;
    if !out.status.success() {
        return Err(io::Error::other(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// A filesystem-safe branch/worktree slug for a turn id.
fn slug(turn_id: &str) -> String {
    turn_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' })
        .collect()
}

impl Worktree {
    /// Create an isolated worktree for `turn_id`: a fresh branch `hive/turn-<id>`
    /// off the current `HEAD`, checked out at `<repo>/.hive/worktrees/<id>`.
    ///
    /// Assumes `repo_root` is a git repo (guard with [`is_git_repo`] first). A
    /// repo with no commits yet has no `HEAD` to branch from — callers should
    /// treat that as "run in place" rather than isolate.
    pub fn create(repo_root: &Path, turn_id: &str) -> io::Result<Worktree> {
        let slug = slug(turn_id);
        let branch = format!("hive/turn-{slug}");
        let path = repo_root.join(".hive").join("worktrees").join(&slug);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // A stale worktree/branch from a crashed prior turn with the same id
        // would block creation — best-effort clear it first.
        let _ = Command::new("git")
            .arg("-C")
            .arg(repo_root)
            .args(["worktree", "remove", "--force"])
            .arg(&path)
            .output();
        let _ = git(repo_root, &["branch", "-D", &branch]);

        let path_str = path.to_string_lossy().into_owned();
        git(
            repo_root,
            &["worktree", "add", "-b", &branch, &path_str, "HEAD"],
        )?;
        Ok(Worktree { path, branch, repo_root: repo_root.to_path_buf() })
    }

    /// The unified diff of everything the turn changed vs the branch point,
    /// including new (untracked) files. Empty string means the turn touched no
    /// files. Staging (`add -A`) is what lets untracked files appear in the diff;
    /// it's local to this worktree's index and never touches the main tree.
    pub fn diff(&self) -> io::Result<String> {
        git(&self.path, &["add", "-A"])?;
        git(&self.path, &["diff", "--cached", "HEAD"])
    }

    /// Paths the turn changed (name-only), for a compact review summary.
    pub fn changed_files(&self) -> io::Result<Vec<String>> {
        git(&self.path, &["add", "-A"])?;
        let out = git(&self.path, &["diff", "--cached", "--name-only", "HEAD"])?;
        Ok(out.lines().filter(|l| !l.trim().is_empty()).map(str::to_owned).collect())
    }

    /// Remove the worktree checkout and delete its task branch. Call after the
    /// diff is captured (merged or discarded). Best-effort: a failure to prune
    /// leaves a stale entry that the next same-id `create` clears anyway.
    pub fn remove(self) -> io::Result<()> {
        let path_str = self.path.to_string_lossy().into_owned();
        git(&self.repo_root, &["worktree", "remove", "--force", &path_str])?;
        let _ = git(&self.repo_root, &["branch", "-D", &self.branch]);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(dir: &Path, prog: &str, args: &[&str]) {
        let ok = Command::new(prog)
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "{prog} {args:?} failed");
    }

    fn temp_repo() -> PathBuf {
        // A unique dir without Date/rand: use the process id + a static counter.
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("hive-wt-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        run(&dir, "git", &["init", "-q"]);
        run(&dir, "git", &["config", "user.email", "t@t"]);
        run(&dir, "git", &["config", "user.name", "t"]);
        std::fs::write(dir.join("a.txt"), "hello\n").unwrap();
        run(&dir, "git", &["add", "-A"]);
        run(&dir, "git", &["commit", "-qm", "init"]);
        dir
    }

    #[test]
    fn detects_repo_vs_nonrepo() {
        let repo = temp_repo();
        assert!(is_git_repo(&repo));
        let plain = std::env::temp_dir().join(format!("hive-notrepo-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&plain);
        assert!(!is_git_repo(&plain));
        let _ = std::fs::remove_dir_all(&repo);
        let _ = std::fs::remove_dir_all(&plain);
    }

    #[test]
    fn isolates_edits_and_captures_diff() {
        let repo = temp_repo();
        let wt = Worktree::create(&repo, "00000000-1111").unwrap();
        assert!(wt.path.exists());
        assert_eq!(wt.branch, "hive/turn-00000000-1111");

        // Edit an existing file AND add a new one inside the worktree.
        std::fs::write(wt.path.join("a.txt"), "hello\nworld\n").unwrap();
        std::fs::write(wt.path.join("b.txt"), "new file\n").unwrap();

        let diff = wt.diff().unwrap();
        assert!(diff.contains("a.txt") && diff.contains("+world"), "tracked edit in diff");
        assert!(diff.contains("b.txt") && diff.contains("+new file"), "untracked file in diff");

        let mut files = wt.changed_files().unwrap();
        files.sort();
        assert_eq!(files, vec!["a.txt".to_string(), "b.txt".to_string()]);

        // The MAIN tree is untouched by the worktree's edits.
        assert_eq!(std::fs::read_to_string(repo.join("a.txt")).unwrap(), "hello\n");
        assert!(!repo.join("b.txt").exists());

        wt.remove().unwrap();
        // Branch is gone after removal.
        let branches = git(&repo, &["branch", "--list", "hive/turn-00000000-1111"]).unwrap();
        assert!(branches.trim().is_empty(), "task branch pruned");
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn create_is_idempotent_after_a_stale_worktree() {
        let repo = temp_repo();
        let _first = Worktree::create(&repo, "dupe-id").unwrap();
        // A second create with the same id (as if the prior turn crashed without
        // cleanup) must succeed by clearing the stale worktree + branch.
        let second = Worktree::create(&repo, "dupe-id").unwrap();
        assert!(second.path.exists());
        second.remove().unwrap();
        let _ = std::fs::remove_dir_all(&repo);
    }
}
