//! An HTTP model turn's built-in file edits land in Hive's per-turn proposal
//! capture: the turn runs in an isolated worktree cut from the chat's
//! `hive/chat/<id>` branch, the composite executor edits there, and
//! `Worktree::diff` / `changed_files` (what the app turns into a review-gated
//! FileDiff proposal) see the change — while the main tree and the chat's
//! base worktree stay untouched.

use std::path::{Path, PathBuf};

use hive_runtime::agent_tools::{builtin_policy, CompositeTools, MAX_OFFERED_TOOLS};
use hive_runtime::git_worktree::{chat_worktree, is_git_repo, Worktree};
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
