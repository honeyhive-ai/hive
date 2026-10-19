//! An HTTP model turn's built-in file edits land in Hive's per-turn proposal
//! capture: the turn runs in an isolated worktree cut from the chat's
//! `hive/chat/<id>` branch, the composite executor edits there, and
//! `Worktree::diff` / `changed_files` (what the app turns into a review-gated
//! FileDiff proposal) see the change — while the main tree and the chat's
//! base worktree stay untouched.

use std::path::{Path, PathBuf};

use hive_runtime::agent_tools::{builtin_policy, CompositeTools, MAX_OFFERED_TOOLS};
use hive_runtime::git_worktree::{chat_worktree, is_git_repo, LazyTurnWorktree, Worktree};
use hive_runtime::provider::dispatch::NoTools;
use hive_runtime::tool_loop::ToolExecutor;
use hive_runtime::BuiltinTools;
use serde_json::json;

fn run(dir: &Path, args: &[&str]) {
    let ok = hive_core::process_util::command("git").arg("-C").arg(dir).args(args).output().unwrap().status.success();
    assert!(ok, "git {args:?} failed");
}

fn temp_repo(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("hive-agentic-wt-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    run(&dir, &["init", "-q"]);
    run(&dir, &["config", "user.email", "t@t"]);
    run(&dir, &["config", "user.name", "t"]);
    run(&dir, &["config", "core.autocrlf", "false"]);
    std::fs::write(dir.join("calc.py"), "def add(a, b):\n    return a - b\n").unwrap();
    run(&dir, &["add", "-A"]);
    run(&dir, &["commit", "-qm", "init"]);
    dir
}

#[tokio::test]
async fn builtin_edits_in_an_isolated_turn_become_the_captured_diff() {
    let repo = temp_repo("capture");
    assert!(is_git_repo(&repo));
    let chat = "chat-1234";
    let base = chat_worktree(&repo, chat).unwrap();
    let wt = Worktree::create_from(&repo, chat, &format!("hive/chat/{chat}")).unwrap();

    // The turn's tools: built-ins rooted at the per-turn worktree, writes on
    // because the turn is isolated; no MCP.
    let builtin = BuiltinTools::new(builtin_policy(&wt.path, true, false, false)).unwrap();
    let tools: CompositeTools<NoTools> = CompositeTools::new(Some(builtin), None, MAX_OFFERED_TOOLS);
    let names: Vec<&str> = tools.defs().iter().filter_map(|d| d["name"].as_str()).collect();
    assert!(names.contains(&"edit_file") && !names.contains(&"run_command"), "{names:?}");

    let (out, err) = tools
        .call("edit_file", &json!({ "path": "calc.py", "old_string": "return a - b", "new_string": "return a + b" }))
        .await;
    assert!(!err, "{out}");
    let (out, err) = tools.call("write_file", &json!({ "path": "test_calc.py", "content": "from calc import add\nassert add(2, 3) == 5\n" })).await;
    assert!(!err, "{out}");

    // What the app captures as the proposal.
    let diff = wt.diff().unwrap();
    assert!(diff.contains("-    return a - b") && diff.contains("+    return a + b"), "{diff}");
    assert!(diff.contains("test_calc.py"), "new files are part of the proposal: {diff}");
    let mut files = wt.changed_files().unwrap();
    files.sort();
    assert_eq!(files, ["calc.py", "test_calc.py"]);

    // Nothing leaked into the main tree or the chat's base worktree.
    assert_eq!(std::fs::read_to_string(repo.join("calc.py")).unwrap(), "def add(a, b):\n    return a - b\n");
    assert_eq!(std::fs::read_to_string(base.join("calc.py")).unwrap(), "def add(a, b):\n    return a - b\n");
    assert!(!repo.join("test_calc.py").exists());

    wt.remove().unwrap();
    let _ = std::fs::remove_dir_all(&repo);
}

#[tokio::test]
async fn in_place_turn_without_opt_in_cannot_write() {
    let repo = temp_repo("inplace");
    let builtin = BuiltinTools::new(builtin_policy(&repo, false, false, true)).unwrap();
    let tools: CompositeTools<NoTools> = CompositeTools::new(Some(builtin), None, MAX_OFFERED_TOOLS);
    let names: Vec<&str> = tools.defs().iter().filter_map(|d| d["name"].as_str()).collect();
    assert_eq!(names, ["read_file", "list_dir", "search"], "read-only, and commands stay off without writes");
    let (out, err) = tools
        .call("edit_file", &json!({ "path": "calc.py", "old_string": "return a - b", "new_string": "return a + b" }))
        .await;
    assert!(err && out.contains("not allowed"), "{out}");
    let (_, err) = tools.call("run_command", &json!({ "command": "touch pwned" })).await;
    assert!(err);
    assert!(!repo.join("pwned").exists());
    assert!(std::fs::read_to_string(repo.join("calc.py")).unwrap().contains("a - b"));
    let _ = std::fs::remove_dir_all(&repo);
}

/// The app's lazy path: built-ins read the chat's dir, and the per-turn
/// worktree is cut by the first write — what `run_prepared_turn` wires up.
fn lazy_turn(repo: &Path, chat: &str) -> (CompositeTools<NoTools>, std::sync::Arc<LazyTurnWorktree>) {
    let lazy = std::sync::Arc::new(LazyTurnWorktree::new(repo, chat));
    let builtin = BuiltinTools::lazy(builtin_policy(repo, true, false, false), lazy.clone(), false).unwrap();
    (CompositeTools::new(Some(builtin), None, MAX_OFFERED_TOOLS), lazy)
}

fn turn_worktrees(repo: &Path) -> Vec<String> {
    let out = hive_core::process_util::command("git").arg("-C").arg(repo).args(["worktree", "list"]).output().unwrap();
    String::from_utf8_lossy(&out.stdout).lines().filter(|l| l.contains("hive/turn-")).map(str::to_owned).collect()
}

#[tokio::test]
async fn a_read_only_turn_cuts_no_worktree() {
    let repo = temp_repo("lazy-read");
    let (tools, lazy) = lazy_turn(&repo, "chat-read");
    let names: Vec<&str> = tools.defs().iter().filter_map(|d| d["name"].as_str()).collect();
    assert!(names.contains(&"edit_file"), "edits are offered before any worktree exists: {names:?}");
    let (out, err) = tools.call("read_file", &json!({ "path": "calc.py" })).await;
    assert!(!err && out.contains("a - b"), "{out}");
    let (_, err) = tools.call("search", &json!({ "pattern": "def add" })).await;
    assert!(!err);
    assert!(lazy.current().is_none());
    assert!(lazy.finish().is_none(), "nothing to capture or remove");
    assert!(turn_worktrees(&repo).is_empty(), "{:?}", turn_worktrees(&repo));
    assert!(!repo.join(".hive/worktrees/chat-read").exists(), "not even the chat base was touched");
    let _ = std::fs::remove_dir_all(&repo);
}

#[tokio::test]
async fn the_first_write_cuts_the_worktree_and_its_edits_become_the_captured_diff() {
    let repo = temp_repo("lazy-write");
    let (tools, lazy) = lazy_turn(&repo, "chat-lazy");
    assert!(!tools.call("read_file", &json!({ "path": "calc.py" })).await.1);
    assert!(lazy.current().is_none());
    let (out, err) = tools
        .call("edit_file", &json!({ "path": "calc.py", "old_string": "return a - b", "new_string": "return a + b" }))
        .await;
    assert!(!err, "{out}");
    let cut = lazy.current().expect("cut on the first write");
    assert_eq!(turn_worktrees(&repo).len(), 1);
    // Reads now see the turn's own edit.
    let (out, _) = tools.call("read_file", &json!({ "path": "calc.py" })).await;
    assert!(out.contains("a + b"), "{out}");

    // What the app captures as the proposal, exactly as for an eager turn.
    let wt = lazy.finish().unwrap();
    assert_eq!(wt.path, cut.path);
    let diff = wt.diff().unwrap();
    assert!(diff.contains("+    return a + b"), "{diff}");
    assert_eq!(wt.changed_files().unwrap(), ["calc.py"]);
    assert!(std::fs::read_to_string(repo.join("calc.py")).unwrap().contains("a - b"), "main tree untouched");

    // After a turn has finished, a straggling write can't cut a second,
    // orphaned worktree.
    let (late, late_lazy) = lazy_turn(&repo, "chat-late");
    assert!(late_lazy.finish().is_none());
    let (out, err) = late.call("write_file", &json!({ "path": "late.txt", "content": "x" })).await;
    assert!(err, "{out}");
    wt.remove().unwrap();
    assert!(turn_worktrees(&repo).is_empty(), "{:?}", turn_worktrees(&repo));
    let _ = std::fs::remove_dir_all(&repo);
}
