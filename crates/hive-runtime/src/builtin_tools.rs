//! Built-in coding tools — `read_file`, `list_dir`, `search`, `write_file`,
//! `edit_file`, `run_command` — so a local model can work a codebase without
//! an MCP server. Self-contained: [`BuiltinTools`] is a [`ToolExecutor`] plus
//! neutral tool defs (`{name, description, input_schema}`), the same shape
//! [`crate::provider::dispatch::ToolSet`] consumes.
//!
//! Everything is confined to a [`Policy`] root: paths resolve against it, `..`
//! escapes and absolute paths outside it are refused, symlinks are
//! canonicalized (also for not-yet-existing write targets, via the nearest
//! existing ancestor) and must land inside it, writes into `.git/` are
//! refused, and other linked git worktrees under the root are never entered.
//! Tools the policy doesn't allow are left out of [`BuiltinTools::defs`] and
//! refused by the executor, so a model that guesses a name still gets nothing.
//!
//! Descriptions are deliberately short and imperative — small open models
//! follow them literally.

use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use regex::{Regex, RegexBuilder};
use serde_json::{json, Value};

use crate::tool_loop::ToolExecutor;

pub const READ_FILE: &str = "read_file";
pub const LIST_DIR: &str = "list_dir";
pub const SEARCH: &str = "search";
pub const WRITE_FILE: &str = "write_file";
pub const EDIT_FILE: &str = "edit_file";
pub const RUN_COMMAND: &str = "run_command";

/// Directories never descended into by `search`/`list_dir`.
const SKIP_DIRS: &[&str] = &[".git", "target", "node_modules"];
/// Files larger than this are refused by `read_file`/`edit_file` and skipped
/// by `search` — far past anything that fits a local model's context.
const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
/// Bytes sniffed for a NUL to call a file binary.
const BINARY_SNIFF: usize = 8 * 1024;
/// Per-line truncation for `read_file` and `search` output.
const READ_LINE_CHARS: usize = 2000;
const MATCH_LINE_CHARS: usize = 300;
const DEFAULT_READ_LINES: usize = 1000;
const MAX_LIST_DEPTH: usize = 5;

/// What the built-in tools may do, and how much they may return.
#[derive(Debug, Clone)]
pub struct Policy {
    /// Everything is confined to this directory.
    pub root: PathBuf,
    /// Offer `write_file` / `edit_file`.
    pub allow_write: bool,
    /// Offer `run_command`. Off by default.
    pub allow_commands: bool,
    /// When set, `run_command` only runs commands starting with one of these
    /// prefixes (on a word boundary), and refuses shell chaining/redirection
    /// so a prefix can't be used to smuggle a second command.
    pub command_allowlist: Option<Vec<String>>,
    /// Cap on `read_file` output (and `list_dir`).
    pub max_read_bytes: usize,
    /// Cap on `run_command` output and `search` output.
    pub max_output_bytes: usize,
    /// Cap on `search` matches.
    pub max_matches: usize,
    /// Cap on `list_dir` entries.
    pub max_list_entries: usize,
    /// `run_command` timeout when the model doesn't pass one.
    pub default_command_timeout: Duration,
    /// Upper bound on a model-requested timeout.
    pub max_command_timeout: Duration,
}

impl Policy {
    /// Read-only defaults: reads/listing/search allowed, no writes, no commands.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            allow_write: false,
            allow_commands: false,
            command_allowlist: None,
            max_read_bytes: 32 * 1024,
            max_output_bytes: 16 * 1024,
            max_matches: 100,
            max_list_entries: 400,
            default_command_timeout: Duration::from_secs(60),
            max_command_timeout: Duration::from_secs(600),
        }
    }

    /// Whether the policy permits tool `name` at all.
    pub fn allows(&self, name: &str) -> bool {
        match name {
            READ_FILE | LIST_DIR | SEARCH => true,
            WRITE_FILE | EDIT_FILE => self.allow_write,
            RUN_COMMAND => self.allow_commands,
            _ => false,
        }
    }
}

/// The built-in tool executor. Cheap to clone; file work runs on the blocking
/// pool so a big search never stalls the async runtime.
#[derive(Debug, Clone)]
pub struct BuiltinTools {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    policy: Policy,
    /// Canonical root — every resolved path must start with it.
    root: PathBuf,
}

type ToolResult = Result<String, String>;

impl BuiltinTools {
    /// Fails if the root doesn't exist or isn't a directory.
    pub fn new(policy: Policy) -> io::Result<Self> {
        let root = std::fs::canonicalize(&policy.root)?;
        if !root.is_dir() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "tool root is not a directory"));
        }
        Ok(Self { inner: Arc::new(Inner { policy, root }) })
    }

    pub fn policy(&self) -> &Policy {
        &self.inner.policy
    }

    /// The canonical root every path is confined to.
    pub fn root(&self) -> &Path {
        &self.inner.root
    }

    /// Whether `name` is one of the built-in tools (allowed or not) — lets a
    /// caller route a call between these and MCP tools.
    pub fn is_builtin(name: &str) -> bool {
        matches!(name, READ_FILE | LIST_DIR | SEARCH | WRITE_FILE | EDIT_FILE | RUN_COMMAND)
    }

    /// Neutral tool defs for the tools this policy allows.
    pub fn defs(&self) -> Vec<Value> {
        all_defs().into_iter().filter(|d| self.inner.policy.allows(d["name"].as_str().unwrap_or(""))).collect()
    }

    async fn dispatch(&self, name: &str, input: &Value) -> ToolResult {
        if !Self::is_builtin(name) {
            return Err(format!("unknown tool {name:?}"));
        }
        if !self.inner.policy.allows(name) {
            return Err(format!("{name} is not allowed in this workspace"));
        }
        if name == RUN_COMMAND {
            return self.inner.run_command(input).await;
        }
        let inner = self.inner.clone();
        let name = name.to_string();
        let input = input.clone();
        tokio::task::spawn_blocking(move || match name.as_str() {
            READ_FILE => inner.read_file(&input),
            LIST_DIR => inner.list_dir(&input),
            SEARCH => inner.search(&input),
            WRITE_FILE => inner.write_file(&input),
            EDIT_FILE => inner.edit_file(&input),
            _ => Err(format!("unknown tool {name:?}")),
        })
        .await
        .unwrap_or_else(|e| Err(format!("tool failed: {e}")))
    }
}

impl ToolExecutor for BuiltinTools {
    async fn call(&self, name: &str, input: &Value) -> (String, bool) {
        match self.dispatch(name, input).await {
            Ok(out) => (out, false),
            Err(err) => (format!("Error: {err}"), true),
        }
    }
}

fn all_defs() -> Vec<Value> {
    vec![
        json!({
            "name": READ_FILE,
            "description": "Read a text file. Returns numbered lines. Read a file before editing it. Use offset and limit to page through long files.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path relative to the project root." },
                    "offset": { "type": "integer", "description": "First line to read, starting at 1." },
                    "limit": { "type": "integer", "description": "Maximum number of lines to return." }
                },
                "required": ["path"]
            }
        }),
        json!({
            "name": LIST_DIR,
            "description": "List files and folders. Folders end with /. Use this to find files before reading them.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Folder relative to the project root. Default: the root." },
                    "depth": { "type": "integer", "description": "How many levels deep to list, 1 to 5. Default 1." }
                }
            }
        }),
        json!({
            "name": SEARCH,
            "description": "Search file contents with a regular expression. Returns path:line: text for each match.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Regular expression to search for." },
                    "path": { "type": "string", "description": "File or folder to search in. Default: the project root." },
                    "glob": { "type": "string", "description": "Only search files matching this glob, e.g. *.rs or src/**/*.ts" }
                },
                "required": ["pattern"]
            }
        }),
        json!({
            "name": WRITE_FILE,
            "description": "Create a new file or replace a whole file with content. To change part of an existing file, use edit_file instead.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path relative to the project root." },
                    "content": { "type": "string", "description": "The complete new file content." }
                },
                "required": ["path", "content"]
            }
        }),
        json!({
            "name": EDIT_FILE,
            "description": "Replace text in a file. Read the file first. old_string must be copied exactly from the file, including spaces and indentation, and must appear exactly once unless replace_all is true.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path relative to the project root." },
                    "old_string": { "type": "string", "description": "Exact text to replace. Include enough surrounding lines to make it unique." },
                    "new_string": { "type": "string", "description": "Text to put in its place." },
                    "replace_all": { "type": "boolean", "description": "Replace every occurrence. Default false." }
                },
                "required": ["path", "old_string", "new_string"]
            }
        }),
        json!({
            "name": RUN_COMMAND,
            "description": "Run a shell command in the project root and return its output and exit code. Use it for builds and tests. Not interactive.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "The shell command to run." },
                    "timeout_secs": { "type": "integer", "description": "Seconds before the command is stopped. Default 60." }
                },
                "required": ["command"]
            }
        }),
    ]
}

// ---------------------------------------------------------------- arguments

fn str_arg<'a>(input: &'a Value, key: &str) -> Result<&'a str, String> {
    input
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing required string argument {key:?}"))
}

fn opt_str<'a>(input: &'a Value, key: &str) -> Option<&'a str> {
    input.get(key).and_then(Value::as_str).filter(|s| !s.trim().is_empty())
}

/// Integers as JSON numbers or numeric strings — small models send both.
fn opt_uint(input: &Value, key: &str) -> Option<u64> {
    match input.get(key)? {
        Value::Number(n) => n.as_u64().or_else(|| n.as_f64().filter(|f| *f >= 0.0).map(|f| f as u64)),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn opt_bool(input: &Value, key: &str) -> bool {
    match input.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => s.trim().eq_ignore_ascii_case("true"),
        _ => false,
    }
}

// ---------------------------------------------------------------- helpers

fn is_binary(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(BINARY_SNIFF)].contains(&0)
}

fn truncate_chars(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

/// Largest prefix of `s` that is at most `max` bytes and ends on a char boundary.
fn clip_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// A directory holding a `.git` *file* that points into `…/worktrees/…` is a
/// linked git worktree — another checkout, never ours to touch.
fn is_linked_worktree(dir: &Path) -> bool {
    let git = dir.join(".git");
    match std::fs::symlink_metadata(&git) {
        Ok(m) if m.is_file() => std::fs::read_to_string(&git)
            .map(|s| s.trim_start().starts_with("gitdir:") && s.contains("worktrees"))
            .unwrap_or(false),
        _ => false,
    }
}

/// Display form of a root-relative path: forward slashes, `.` for the root.
fn display_rel(rel: &Path) -> String {
    let s = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/");
    if s.is_empty() { ".".into() } else { s }
}

/// Translate a glob into an anchored regex: `**/` any number of directories,
/// `**` anything, `*` within one path segment, `?` one char, `{a,b}` either.
fn glob_to_regex(glob: &str) -> Result<Regex, String> {
    let mut re = String::from("^");
    let chars: Vec<char> = glob.chars().collect();
    let mut i = 0;
    let mut brace_depth = 0usize;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '*' if chars.get(i + 1) == Some(&'*') => {
                if chars.get(i + 2) == Some(&'/') {
                    re.push_str("(?:.*/)?");
                    i += 3;
                } else {
                    re.push_str(".*");
                    i += 2;
                }
                continue;
            }
            '*' => re.push_str("[^/]*"),
            '?' => re.push_str("[^/]"),
            '{' => {
                brace_depth += 1;
                re.push_str("(?:");
            }
            '}' if brace_depth > 0 => {
                brace_depth -= 1;
                re.push(')');
            }
            ',' if brace_depth > 0 => re.push('|'),
            _ => re.push_str(&regex::escape(&c.to_string())),
        }
        i += 1;
    }
    if brace_depth > 0 {
        return Err(format!("invalid glob {glob:?}: unclosed {{"));
    }
    re.push('$');
    Regex::new(&re).map_err(|e| format!("invalid glob {glob:?}: {e}"))
}

/// A resolved, confined path.
struct Resolved {
    /// Absolute, canonical (for the existing part) path inside the root.
    abs: PathBuf,
    /// Path relative to the root.
    rel: PathBuf,
}

impl Inner {
    /// Resolve a model-supplied path against the root and confine it. Shared
    /// by every tool; `for_write` additionally refuses `.git/`.
    fn resolve(&self, raw: &str, for_write: bool) -> Result<Resolved, String> {
        let raw = raw.trim();
        let input = Path::new(raw);

        // Absolute paths are accepted only when they already point inside the
        // root (as given or as canonicalized) — then treated as relative.
        let relative: PathBuf = if input.is_absolute() {
            input
                .strip_prefix(&self.root)
                .or_else(|_| input.strip_prefix(&self.policy.root))
                .map(Path::to_path_buf)
                .map_err(|_| format!("{raw} is outside the project; use a path relative to the project root"))?
        } else {
            input.to_path_buf()
        };

        // Lexically normalize; a `..` that climbs above the root is an escape.
        let mut parts: Vec<std::ffi::OsString> = Vec::new();
        for comp in relative.components() {
            match comp {
                Component::Normal(p) => parts.push(p.to_os_string()),
                Component::CurDir => {}
                Component::ParentDir => {
                    if parts.pop().is_none() {
                        return Err(format!("{raw} escapes the project root"));
                    }
                }
                Component::RootDir | Component::Prefix(_) => {
                    return Err(format!("{raw} is outside the project; use a path relative to the project root"));
                }
            }
        }

        // Canonicalize the longest existing prefix so symlinks are resolved,
        // then re-append the not-yet-existing tail (plain names only).
        let mut abs = None;
        for keep in (0..=parts.len()).rev() {
            let candidate: PathBuf = parts[..keep].iter().fold(self.root.clone(), |p, c| p.join(c));
            match std::fs::canonicalize(&candidate) {
                Ok(canon) => {
                    abs = Some(parts[keep..].iter().fold(canon, |p, c| p.join(c)));
                    break;
                }
                Err(_) if std::fs::symlink_metadata(&candidate).is_ok() => {
                    // Exists but can't be resolved: a dangling symlink, whose
                    // target could be anywhere.
                    return Err(format!("{raw} is a broken symlink"));
                }
                Err(_) => {}
            }
        }
        let abs = abs.ok_or_else(|| format!("cannot resolve {raw}"))?;
        let rel = abs
            .strip_prefix(&self.root)
            .map(Path::to_path_buf)
            .map_err(|_| format!("{raw} resolves outside the project (symlink)"))?;

        // Never step into another linked worktree living under the root.
        let rel_str = display_rel(&rel);
        if rel_str == ".claude/worktrees" || rel_str.starts_with(".claude/worktrees/") {
            return Err(format!("{raw} is inside another git worktree"));
        }
        let mut dir = self.root.clone();
        for comp in rel.components() {
            dir.push(comp);
            if dir.is_dir() && is_linked_worktree(&dir) {
                return Err(format!("{raw} is inside another git worktree"));
            }
        }

        if for_write && rel.components().any(|c| c.as_os_str() == ".git") {
            return Err(format!("{raw} is inside .git; writing there is not allowed"));
        }
        Ok(Resolved { abs, rel })
    }

    /// Read a text file for read/edit, refusing directories, huge and binary files.
    fn read_text(&self, r: &Resolved, raw: &str) -> Result<String, String> {
        let meta = std::fs::metadata(&r.abs).map_err(|_| format!("{raw} does not exist"))?;
        if meta.is_dir() {
            return Err(format!("{raw} is a directory; use list_dir"));
        }
        if meta.len() > MAX_FILE_BYTES {
            return Err(format!("{raw} is too large ({} bytes); use search to find the relevant lines", meta.len()));
        }
        let bytes = std::fs::read(&r.abs).map_err(|e| format!("cannot read {raw}: {e}"))?;
        if is_binary(&bytes) {
            return Err(format!("{raw} is a binary file"));
        }
        String::from_utf8(bytes).map_err(|_| format!("{raw} is not valid UTF-8 text"))
    }

    fn read_file(&self, input: &Value) -> ToolResult {
        let raw = str_arg(input, "path")?;
        let r = self.resolve(raw, false)?;
        let text = self.read_text(&r, raw)?;
        let lines: Vec<&str> = text.lines().collect();
        let total = lines.len();
        if total == 0 {
            return Ok(format!("({} is empty)", display_rel(&r.rel)));
        }
        let offset = opt_uint(input, "offset").unwrap_or(1).max(1) as usize;
        if offset > total {
            return Err(format!("offset {offset} is past the end of {raw} ({total} lines)"));
        }
        let limit = opt_uint(input, "limit").map(|l| l.max(1) as usize).unwrap_or(DEFAULT_READ_LINES);

        let mut out = String::new();
        let mut last = offset - 1;
        for (i, line) in lines.iter().enumerate().skip(offset - 1).take(limit) {
            let entry = format!("{:>6}\t{}\n", i + 1, truncate_chars(line, READ_LINE_CHARS));
            if !out.is_empty() && out.len() + entry.len() > self.policy.max_read_bytes {
                break;
            }
            out.push_str(clip_bytes(&entry, self.policy.max_read_bytes));
            last = i + 1;
        }
        if last < total {
            out.push_str(&format!(
                "[Showing lines {offset}-{last} of {total}. Call read_file with offset={} to read more.]\n",
                last + 1
            ));
        }
        Ok(out)
    }

    fn list_dir(&self, input: &Value) -> ToolResult {
        let raw = opt_str(input, "path").unwrap_or(".");
        let r = self.resolve(raw, false)?;
        if !r.abs.is_dir() {
            return Err(if r.abs.exists() {
                format!("{raw} is a file; use read_file")
            } else {
                format!("{raw} does not exist")
            });
        }
        let depth = (opt_uint(input, "depth").unwrap_or(1) as usize).clamp(1, MAX_LIST_DEPTH);
        let mut out = String::new();
        let mut count = 0usize;
        let mut truncated = false;
        self.list_into(&r.abs, 0, depth, &mut out, &mut count, &mut truncated);
        if out.is_empty() {
            return Ok(format!("({} is empty)", display_rel(&r.rel)));
        }
        if truncated {
            out.push_str(&format!(
                "[Listing truncated at {count} entries. List a subfolder or use a smaller depth.]\n"
            ));
        }
        Ok(out)
    }

    fn list_into(&self, dir: &Path, level: usize, depth: usize, out: &mut String, count: &mut usize, truncated: &mut bool) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        let mut entries: Vec<_> = rd.filter_map(Result::ok).collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            if *truncated {
                return;
            }
            if *count >= self.policy.max_list_entries || out.len() >= self.policy.max_read_bytes {
                *truncated = true;
                return;
            }
            let name = e.file_name().to_string_lossy().into_owned();
            let Ok(ft) = e.file_type() else { continue };
            let indent = "  ".repeat(level);
            *count += 1;
            if ft.is_symlink() {
                out.push_str(&format!("{indent}{name}@\n"));
            } else if ft.is_dir() {
                let path = e.path();
                let skipped = SKIP_DIRS.contains(&name.as_str())
                    || is_linked_worktree(&path)
                    || path.strip_prefix(&self.root).map(|p| p == Path::new(".claude/worktrees")).unwrap_or(false);
                if skipped {
                    out.push_str(&format!("{indent}{name}/ (not listed)\n"));
                } else {
                    out.push_str(&format!("{indent}{name}/\n"));
                    if level + 1 < depth {
                        self.list_into(&path, level + 1, depth, out, count, truncated);
                    }
                }
            } else {
                out.push_str(&format!("{indent}{name}\n"));
            }
        }
    }

    fn search(&self, input: &Value) -> ToolResult {
        let pattern = str_arg(input, "pattern")?;
        if pattern.is_empty() {
            return Err("pattern is empty".into());
        }
        let re = RegexBuilder::new(pattern)
            .size_limit(1 << 20)
            .build()
            .map_err(|e| format!("invalid regular expression: {e}"))?;
        let glob = opt_str(input, "glob").map(glob_to_regex).transpose()?;
        // A glob without a slash matches the file name at any depth.
        let glob_on_name = opt_str(input, "glob").map(|g| !g.contains('/')).unwrap_or(false);
        let raw = opt_str(input, "path").unwrap_or(".");
        let r = self.resolve(raw, false)?;
        if !r.abs.exists() {
            return Err(format!("{raw} does not exist"));
        }

        let root = self.root.clone();
        let walker = walkdir::WalkDir::new(&r.abs).sort_by_file_name().into_iter().filter_entry(move |e| {
            if e.depth() == 0 || !e.file_type().is_dir() {
                return true;
            }
            let name = e.file_name().to_string_lossy();
            !(SKIP_DIRS.contains(&name.as_ref())
                || is_linked_worktree(e.path())
                || e.path().strip_prefix(&root).map(|p| p == Path::new(".claude/worktrees")).unwrap_or(false))
        });

        let mut out = String::new();
        let mut matches = 0usize;
        let mut truncated = false;
        'files: for entry in walker.filter_map(Result::ok) {
            // Symlinks are not followed: a link could point anywhere.
            if !entry.file_type().is_file() {
                continue;
            }
            let rel = entry.path().strip_prefix(&self.root).unwrap_or(entry.path());
            let rel_s = display_rel(rel);
            if let Some(g) = &glob {
                let subject = if glob_on_name { entry.file_name().to_string_lossy().into_owned() } else { rel_s.clone() };
                if !g.is_match(&subject) {
                    continue;
                }
            }
            if entry.metadata().map(|m| m.len() > MAX_FILE_BYTES).unwrap_or(true) {
                continue;
            }
            let Ok(mut f) = std::fs::File::open(entry.path()) else { continue };
            let mut bytes = Vec::new();
            if f.read_to_end(&mut bytes).is_err() || is_binary(&bytes) {
                continue;
            }
            let text = String::from_utf8_lossy(&bytes);
            for (i, line) in text.lines().enumerate() {
                if !re.is_match(line) {
                    continue;
                }
                if matches >= self.policy.max_matches {
                    truncated = true;
                    break 'files;
                }
                let entry = format!("{rel_s}:{}: {}\n", i + 1, truncate_chars(line.trim_end(), MATCH_LINE_CHARS));
                if out.len() + entry.len() > self.policy.max_output_bytes {
                    truncated = true;
                    break 'files;
                }
                out.push_str(&entry);
                matches += 1;
            }
        }
        if matches == 0 {
            return Ok("No matches.".into());
        }
        if truncated {
            out.push_str(&format!("[Stopped after {matches} matches. Narrow the pattern, path or glob.]\n"));
        }
        Ok(out)
    }

    fn write_file(&self, input: &Value) -> ToolResult {
        let raw = str_arg(input, "path")?;
        let content = str_arg(input, "content")?;
        let r = self.resolve(raw, true)?;
        if r.rel.as_os_str().is_empty() || r.abs.is_dir() {
            return Err(format!("{raw} is a directory"));
        }
        if let Some(parent) = r.abs.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("cannot create folder for {raw}: {e}"))?;
        }
        let existed = r.abs.exists();
        std::fs::write(&r.abs, content).map_err(|e| format!("cannot write {raw}: {e}"))?;
        Ok(format!(
            "{} {} ({} bytes).",
            if existed { "Overwrote" } else { "Created" },
            display_rel(&r.rel),
            content.len()
        ))
    }

    fn edit_file(&self, input: &Value) -> ToolResult {
        let raw = str_arg(input, "path")?;
        let old = str_arg(input, "old_string")?;
        let new = str_arg(input, "new_string")?;
        let replace_all = opt_bool(input, "replace_all");
        if old.is_empty() {
            return Err("old_string is empty. To create a file use write_file.".into());
        }
        if old == new {
            return Err("old_string and new_string are the same; nothing to change.".into());
        }
        let r = self.resolve(raw, true)?;
        if !r.abs.exists() {
            return Err(format!("{raw} does not exist. To create it use write_file."));
        }
        let text = self.read_text(&r, raw)?;
        let count = text.matches(old).count();
        if count == 0 {
            let hint = if text.contains(old.trim()) && !old.trim().is_empty() {
                " The text exists but with different leading/trailing whitespace; copy it exactly."
            } else {
                " Read the file again and copy old_string exactly, including spaces and indentation."
            };
            return Err(format!("old_string was not found in {raw}.{hint}"));
        }
        if count > 1 && !replace_all {
            return Err(format!(
                "old_string appears {count} times in {raw}. Include more surrounding lines so it matches exactly once, or set replace_all to true."
            ));
        }
        let updated = if replace_all { text.replace(old, new) } else { text.replacen(old, new, 1) };
        std::fs::write(&r.abs, updated).map_err(|e| format!("cannot write {raw}: {e}"))?;
        Ok(format!(
            "Edited {}: replaced {count} occurrence{}.",
            display_rel(&r.rel),
            if count == 1 { "" } else { "s" }
        ))
    }

    async fn run_command(&self, input: &Value) -> ToolResult {
        let command = str_arg(input, "command")?.trim().to_string();
        if command.is_empty() {
            return Err("command is empty".into());
        }
        if let Some(list) = &self.policy.command_allowlist {
            command_allowed(&command, list)?;
        }
        let timeout = opt_uint(input, "timeout_secs")
            .map(|s| Duration::from_secs(s.max(1)))
            .unwrap_or(self.policy.default_command_timeout)
            .min(self.policy.max_command_timeout);
        run_shell(&command, &self.root, timeout, self.policy.max_output_bytes).await
    }
}

/// Characters that would let an allowlisted prefix chain, substitute or
/// redirect into something else.
const SHELL_META: &[char] = &[';', '&', '|', '`', '$', '>', '<', '(', ')', '\n', '\r'];

fn command_allowed(command: &str, allowlist: &[String]) -> Result<(), String> {
    if command.contains(SHELL_META) {
        return Err("only simple commands are allowed here (no ; & | $ ` > < or parentheses)".into());
    }
    let ok = allowlist.iter().map(|p| p.trim()).filter(|p| !p.is_empty()).any(|p| {
        command == p || (command.starts_with(p) && command[p.len()..].starts_with(char::is_whitespace))
    });
    if ok {
        Ok(())
    } else {
        Err(format!("command not allowed. Allowed commands start with: {}", allowlist.join(", ")))
    }
}

/// Run `command` through the shell in `dir` with a minimal environment, a
/// timeout (killing the whole process group), and capped output.
async fn run_shell(command: &str, dir: &Path, timeout: Duration, max_output: usize) -> ToolResult {
    use std::process::Stdio;
    use tokio::io::{AsyncRead, AsyncReadExt};

    #[cfg(unix)]
    let mut cmd = {
        let mut c = tokio::process::Command::new("sh");
        c.arg("-c").arg(command);
        c.process_group(0);
        c
    };
    #[cfg(not(unix))]
    let mut cmd = {
        let mut c = tokio::process::Command::new("cmd");
        c.arg("/C").arg(command);
        c
    };
    cmd.current_dir(dir).env_clear().stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    // Minimal env: enough to find tools and behave non-interactively, without
    // handing the model the app's secrets.
    for key in ["PATH", "HOME", "USER", "LANG", "TMPDIR", "SYSTEMROOT", "TEMP", "TMP"] {
        if let Some(v) = std::env::var_os(key) {
            cmd.env(key, v);
        }
    }
    cmd.env("TERM", "dumb").env("NO_COLOR", "1").env("CI", "1").env("GIT_TERMINAL_PROMPT", "0");

    let mut child = cmd.spawn().map_err(|e| format!("cannot start command: {e}"))?;
    let pid = child.id();

    async fn drain(mut r: impl AsyncRead + Unpin, cap: usize) -> (Vec<u8>, bool) {
        let mut kept = Vec::new();
        let mut cut = false;
        let mut buf = [0u8; 8192];
        loop {
            match r.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let room = cap.saturating_sub(kept.len());
                    kept.extend_from_slice(&buf[..n.min(room)]);
                    cut |= n > room;
                }
            }
        }
        (kept, cut)
    }
    let out_task = tokio::spawn(drain(child.stdout.take().expect("piped"), max_output));
    let err_task = tokio::spawn(drain(child.stderr.take().expect("piped"), max_output));

    let status = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(s) => Some(s.map_err(|e| format!("command failed: {e}"))?),
        Err(_) => {
            #[cfg(unix)]
            if let Some(pid) = pid {
                let _ = std::process::Command::new("kill").arg("-KILL").arg(format!("-{pid}")).status();
            }
            #[cfg(not(unix))]
            let _ = pid;
            let _ = child.kill().await;
            None
        }
    };

    // Grandchildren holding the pipes open must not hang us.
    let grace = Duration::from_secs(2);
    let (stdout, out_cut) = tokio::time::timeout(grace, out_task).await.ok().and_then(Result::ok).unwrap_or_default();
    let (stderr, err_cut) = tokio::time::timeout(grace, err_task).await.ok().and_then(Result::ok).unwrap_or_default();

    let mut text = String::new();
    let stdout = String::from_utf8_lossy(&stdout);
    let stderr = String::from_utf8_lossy(&stderr);
    if !stdout.trim().is_empty() {
        text.push_str(clip_bytes(&stdout, max_output));
        if !text.ends_with('\n') {
            text.push('\n');
        }
    }
    if !stderr.trim().is_empty() {
        text.push_str("[stderr]\n");
        text.push_str(clip_bytes(&stderr, max_output.saturating_sub(text.len()).max(1024)));
        if !text.ends_with('\n') {
            text.push('\n');
        }
    }
    if out_cut || err_cut {
        text.push_str(&format!("[Output truncated to {max_output} bytes.]\n"));
    }
    match status {
        None => Err(format!("{text}command timed out after {}s and was stopped", timeout.as_secs())),
        Some(s) if s.success() => Ok(format!("{text}[exit code 0]")),
        Some(s) => {
            let code = s.code().map(|c| c.to_string()).unwrap_or_else(|| "signal".into());
            Err(format!("{text}[exit code {code}]"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn setup() -> (TempDir, PathBuf) {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("proj");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {\n    println!(\"hi\");\n}\n").unwrap();
        fs::write(root.join("README.md"), "# Proj\nhello world\n").unwrap();
        (tmp, root)
    }

    fn tools(root: &Path, f: impl FnOnce(&mut Policy)) -> BuiltinTools {
        let mut p = Policy::new(root);
        f(&mut p);
        BuiltinTools::new(p).unwrap()
    }

    async fn ok(t: &BuiltinTools, name: &str, input: Value) -> String {
        let (out, err) = t.call(name, &input).await;
        assert!(!err, "{name} failed: {out}");
        out
    }

    async fn fail(t: &BuiltinTools, name: &str, input: Value) -> String {
        let (out, err) = t.call(name, &input).await;
        assert!(err, "{name} should have failed, got: {out}");
        out
    }

    fn names(t: &BuiltinTools) -> Vec<String> {
        t.defs().iter().map(|d| d["name"].as_str().unwrap().to_string()).collect()
    }

    #[test]
    fn defs_follow_policy() {
        let (_tmp, root) = setup();
        assert_eq!(names(&tools(&root, |_| {})), [READ_FILE, LIST_DIR, SEARCH]);
        let all = tools(&root, |p| {
            p.allow_write = true;
            p.allow_commands = true;
        });
        assert_eq!(names(&all), [READ_FILE, LIST_DIR, SEARCH, WRITE_FILE, EDIT_FILE, RUN_COMMAND]);
        for d in all.defs() {
            assert!(d["description"].as_str().unwrap().len() < 220);
            assert_eq!(d["input_schema"]["type"], "object");
        }
    }

    #[tokio::test]
    async fn disallowed_tools_are_refused() {
        let (_tmp, root) = setup();
        let t = tools(&root, |_| {});
        let out = fail(&t, WRITE_FILE, json!({"path": "x.txt", "content": "x"})).await;
        assert!(out.contains("not allowed"), "{out}");
        assert!(!root.join("x.txt").exists());
        fail(&t, EDIT_FILE, json!({"path": "README.md", "old_string": "hello", "new_string": "bye"})).await;
        let out = fail(&t, RUN_COMMAND, json!({"command": "touch ran"})).await;
        assert!(out.contains("not allowed"), "{out}");
        assert!(!root.join("ran").exists(), "commands are off by default");
        fail(&t, "rm_rf", json!({})).await;
    }

    #[tokio::test]
    async fn read_file_numbers_lines_and_pages() {
        let (_tmp, root) = setup();
        let t = tools(&root, |_| {});
        let out = ok(&t, READ_FILE, json!({"path": "src/main.rs"})).await;
        assert!(out.starts_with("     1\tfn main() {\n"), "{out}");
        assert!(out.contains("     3\t}"));
        assert!(!out.contains("Showing"));

        let body: String = (1..=50).map(|i| format!("line {i}\n")).collect();
        fs::write(root.join("long.txt"), body).unwrap();
        let out = ok(&t, READ_FILE, json!({"path": "long.txt", "offset": 10, "limit": "5"})).await;
        assert!(out.starts_with("    10\tline 10\n"), "{out}");
        assert!(out.contains("    14\tline 14\n") && !out.contains("line 15\n"));
        assert!(out.contains("offset=15"), "{out}");
        fail(&t, READ_FILE, json!({"path": "long.txt", "offset": 51})).await;
    }

    #[tokio::test]
    async fn read_file_caps_bytes() {
        let (_tmp, root) = setup();
        let t = tools(&root, |p| p.max_read_bytes = 200);
        let body: String = (1..=100).map(|i| format!("line number {i}\n")).collect();
        fs::write(root.join("big.txt"), body).unwrap();
        let out = ok(&t, READ_FILE, json!({"path": "big.txt"})).await;
        let listing = out.split("[Showing").next().unwrap();
        assert!(listing.len() <= 200, "{}", listing.len());
        assert!(out.contains("of 100. Call read_file with offset="), "{out}");
    }

    #[tokio::test]
    async fn read_file_refuses_binary_dirs_and_missing() {
        let (_tmp, root) = setup();
        fs::write(root.join("blob.bin"), [0x89, b'P', 0, 1, 2]).unwrap();
        let t = tools(&root, |_| {});
        assert!(fail(&t, READ_FILE, json!({"path": "blob.bin"})).await.contains("binary"));
        assert!(fail(&t, READ_FILE, json!({"path": "src"})).await.contains("directory"));
        assert!(fail(&t, READ_FILE, json!({"path": "nope.rs"})).await.contains("does not exist"));
        assert!(fail(&t, READ_FILE, json!({})).await.contains("path"));
    }

    #[tokio::test]
    async fn traversal_is_rejected() {
        let (tmp, root) = setup();
        fs::write(tmp.path().join("secret.txt"), "s3cret").unwrap();
        let t = tools(&root, |p| p.allow_write = true);
        for p in ["../secret.txt", "src/../../secret.txt", "./../proj/../secret.txt"] {
            let out = fail(&t, READ_FILE, json!({ "path": p })).await;
            assert!(out.contains("escapes"), "{p}: {out}");
        }
        fail(&t, WRITE_FILE, json!({"path": "../evil.txt", "content": "x"})).await;
        assert!(!tmp.path().join("evil.txt").exists());
        // `..` that stays inside is fine.
        ok(&t, READ_FILE, json!({"path": "src/../README.md"})).await;
    }

    #[tokio::test]
    async fn absolute_paths_must_be_inside_root() {
        let (tmp, root) = setup();
        fs::write(tmp.path().join("secret.txt"), "s3cret").unwrap();
        let t = tools(&root, |p| p.allow_write = true);
        let outside = tmp.path().join("secret.txt");
        assert!(fail(&t, READ_FILE, json!({"path": outside})).await.contains("outside"));
        fail(&t, READ_FILE, json!({"path": "/etc/passwd"})).await;
        fail(&t, WRITE_FILE, json!({"path": tmp.path().join("evil.txt"), "content": "x"})).await;
        assert!(!tmp.path().join("evil.txt").exists());
        // An absolute path inside the root works, as given or canonical.
        ok(&t, READ_FILE, json!({"path": root.join("README.md")})).await;
        ok(&t, READ_FILE, json!({"path": t.root().join("README.md")})).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinks_escaping_root_are_rejected() {
        use std::os::unix::fs::symlink;
        let (tmp, root) = setup();
        let outside = tmp.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret.txt"), "s3cret").unwrap();
        symlink(&outside, root.join("link_dir")).unwrap();
        symlink(outside.join("secret.txt"), root.join("link_file")).unwrap();
        symlink(outside.join("not-yet.txt"), root.join("dangling")).unwrap();
        symlink(root.join("README.md"), root.join("inside_link")).unwrap();
        let t = tools(&root, |p| p.allow_write = true);

        assert!(fail(&t, READ_FILE, json!({"path": "link_file"})).await.contains("outside"));
        assert!(fail(&t, READ_FILE, json!({"path": "link_dir/secret.txt"})).await.contains("outside"));
        fail(&t, LIST_DIR, json!({"path": "link_dir"})).await;
        // Writes through a symlinked dir to a new file, or via a dangling link.
        fail(&t, WRITE_FILE, json!({"path": "link_dir/new.txt", "content": "x"})).await;
        assert!(!outside.join("new.txt").exists());
        fail(&t, WRITE_FILE, json!({"path": "dangling", "content": "x"})).await;
        assert!(!outside.join("not-yet.txt").exists());
        fail(&t, EDIT_FILE, json!({"path": "link_file", "old_string": "s3cret", "new_string": "x"})).await;
        assert_eq!(fs::read_to_string(outside.join("secret.txt")).unwrap(), "s3cret");
        // A symlink that stays inside the root is fine.
        assert!(ok(&t, READ_FILE, json!({"path": "inside_link"})).await.contains("hello world"));
        // Search never follows links out.
        let out = ok(&t, SEARCH, json!({"pattern": "s3cret"})).await;
        assert_eq!(out, "No matches.");
    }

    #[tokio::test]
    async fn writes_into_git_are_refused() {
        let (_tmp, root) = setup();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join(".git/config"), "[core]\n").unwrap();
        let t = tools(&root, |p| p.allow_write = true);
        assert!(fail(&t, WRITE_FILE, json!({"path": ".git/hooks/pre-commit", "content": "x"})).await.contains(".git"));
        fail(&t, EDIT_FILE, json!({"path": ".git/config", "old_string": "[core]", "new_string": "x"})).await;
        fail(&t, WRITE_FILE, json!({"path": "src/../.git/HEAD", "content": "x"})).await;
        assert_eq!(fs::read_to_string(root.join(".git/config")).unwrap(), "[core]\n");
        // A file merely named like .git-something is fine.
        ok(&t, WRITE_FILE, json!({"path": ".gitignore", "content": "target\n"})).await;
    }

    #[tokio::test]
    async fn other_worktrees_are_off_limits() {
        let (_tmp, root) = setup();
        let wt = root.join("nested-wt");
        fs::create_dir_all(&wt).unwrap();
        fs::write(wt.join(".git"), "gitdir: /somewhere/.git/worktrees/nested-wt\n").unwrap();
        fs::write(wt.join("lib.rs"), "needle\n").unwrap();
        let claude_wt = root.join(".claude/worktrees/agent-1");
        fs::create_dir_all(&claude_wt).unwrap();
        fs::write(claude_wt.join("x.rs"), "needle\n").unwrap();
        let t = tools(&root, |p| p.allow_write = true);

        assert!(fail(&t, READ_FILE, json!({"path": "nested-wt/lib.rs"})).await.contains("worktree"));
        fail(&t, WRITE_FILE, json!({"path": "nested-wt/new.rs", "content": "x"})).await;
        fail(&t, READ_FILE, json!({"path": ".claude/worktrees/agent-1/x.rs"})).await;
        assert_eq!(ok(&t, SEARCH, json!({"pattern": "needle"})).await, "No matches.");
        let listing = ok(&t, LIST_DIR, json!({"depth": 4})).await;
        assert!(listing.contains("nested-wt/ (not listed)"), "{listing}");
        assert!(!listing.contains("lib.rs") && !listing.contains("x.rs"), "{listing}");
    }

    #[tokio::test]
    async fn list_dir_depth_and_skips() {
        let (_tmp, root) = setup();
        fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        fs::create_dir_all(root.join("a/b/c")).unwrap();
        fs::write(root.join("a/b/c/deep.txt"), "x").unwrap();
        let t = tools(&root, |_| {});
        let out = ok(&t, LIST_DIR, json!({})).await;
        assert!(out.contains("README.md\n") && out.contains("src/\n") && out.contains("a/\n"), "{out}");
        assert!(!out.contains("main.rs"));
        assert!(out.contains("node_modules/ (not listed)"));
        let out = ok(&t, LIST_DIR, json!({"path": "a", "depth": 3})).await;
        assert_eq!(out, "b/\n  c/\n    deep.txt\n");
        assert!(fail(&t, LIST_DIR, json!({"path": "README.md"})).await.contains("read_file"));

        let t = tools(&root, |p| p.max_list_entries = 2);
        assert!(ok(&t, LIST_DIR, json!({})).await.contains("truncated"));
    }

    #[tokio::test]
    async fn search_finds_matches_and_skips_noise() {
        let (_tmp, root) = setup();
        for d in [".git", "target/debug", "node_modules/x"] {
            fs::create_dir_all(root.join(d)).unwrap();
            fs::write(root.join(d).join("f.rs"), "println here\n").unwrap();
        }
        fs::write(root.join("src/bin.dat"), b"println\0binary").unwrap();
        fs::create_dir_all(root.join("web/src")).unwrap();
        fs::write(root.join("web/src/app.ts"), "console.log('println')\n").unwrap();
        let t = tools(&root, |_| {});

        let out = ok(&t, SEARCH, json!({"pattern": r"print\w+"})).await;
        assert!(out.contains("src/main.rs:2:     println!(\"hi\");"), "{out}");
        assert!(out.contains("web/src/app.ts:1:"));
        for skipped in [".git", "target", "node_modules", "bin.dat"] {
            assert!(!out.contains(skipped), "{skipped} in {out}");
        }
        let out = ok(&t, SEARCH, json!({"pattern": "println", "glob": "*.rs"})).await;
        assert!(out.contains("main.rs") && !out.contains("app.ts"), "{out}");
        let out = ok(&t, SEARCH, json!({"pattern": "println", "glob": "web/**/*.{ts,tsx}"})).await;
        assert!(!out.contains("main.rs") && out.contains("app.ts"), "{out}");
        let out = ok(&t, SEARCH, json!({"pattern": "println", "path": "src"})).await;
        assert!(out.contains("main.rs") && !out.contains("app.ts"), "{out}");
        assert_eq!(ok(&t, SEARCH, json!({"pattern": "zzz_nothing"})).await, "No matches.");
        assert!(fail(&t, SEARCH, json!({"pattern": "("})).await.contains("invalid regular expression"));
    }

    #[tokio::test]
    async fn search_caps_matches() {
        let (_tmp, root) = setup();
        let body: String = (0..50).map(|i| format!("hit {i}\n")).collect();
        fs::write(root.join("many.txt"), body).unwrap();
        let t = tools(&root, |p| p.max_matches = 5);
        let out = ok(&t, SEARCH, json!({"pattern": "hit"})).await;
        assert_eq!(out.lines().filter(|l| l.starts_with("many.txt:")).count(), 5);
        assert!(out.contains("Stopped after 5 matches"), "{out}");

        let t = tools(&root, |p| p.max_output_bytes = 40);
        let out = ok(&t, SEARCH, json!({"pattern": "hit"})).await;
        assert!(out.lines().filter(|l| l.starts_with("many.txt:")).count() < 5, "{out}");
    }

    #[test]
    fn glob_translation() {
        let g = glob_to_regex("src/**/*.rs").unwrap();
        assert!(g.is_match("src/main.rs") && g.is_match("src/a/b/c.rs") && !g.is_match("srcx/main.rs"));
        let g = glob_to_regex("*.{ts,tsx}").unwrap();
        assert!(g.is_match("a.ts") && g.is_match("a.tsx") && !g.is_match("a.js") && !g.is_match("d/a.ts"));
        assert!(glob_to_regex("file?.txt").unwrap().is_match("file1.txt"));
        assert!(glob_to_regex("a.b").unwrap().is_match("a.b") && !glob_to_regex("a.b").unwrap().is_match("axb"));
        assert!(glob_to_regex("{a,b").is_err());
    }

    #[tokio::test]
    async fn write_file_creates_and_overwrites() {
        let (_tmp, root) = setup();
        let t = tools(&root, |p| p.allow_write = true);
        assert!(ok(&t, WRITE_FILE, json!({"path": "new/dir/f.txt", "content": "one"})).await.starts_with("Created"));
        assert_eq!(fs::read_to_string(root.join("new/dir/f.txt")).unwrap(), "one");
        assert!(ok(&t, WRITE_FILE, json!({"path": "new/dir/f.txt", "content": "two"})).await.starts_with("Overwrote"));
        assert_eq!(fs::read_to_string(root.join("new/dir/f.txt")).unwrap(), "two");
        fail(&t, WRITE_FILE, json!({"path": "src", "content": "x"})).await;
        fail(&t, WRITE_FILE, json!({"path": ".", "content": "x"})).await;
        fail(&t, WRITE_FILE, json!({"path": "a.txt"})).await;
    }

    #[tokio::test]
    async fn edit_file_requires_unique_exact_match() {
        let (_tmp, root) = setup();
        fs::write(root.join("f.txt"), "alpha\n  beta\nalpha\n").unwrap();
        let t = tools(&root, |p| p.allow_write = true);

        let out = fail(&t, EDIT_FILE, json!({"path": "f.txt", "old_string": "alpha", "new_string": "x"})).await;
        assert!(out.contains("appears 2 times") && out.contains("replace_all"), "{out}");
        assert_eq!(fs::read_to_string(root.join("f.txt")).unwrap(), "alpha\n  beta\nalpha\n");

        let out = fail(&t, EDIT_FILE, json!({"path": "f.txt", "old_string": "gamma", "new_string": "x"})).await;
        assert!(out.contains("not found") && out.contains("exactly"), "{out}");
        let out = fail(&t, EDIT_FILE, json!({"path": "f.txt", "old_string": "beta \n", "new_string": "x"})).await;
        assert!(out.contains("whitespace"), "{out}");
        fail(&t, EDIT_FILE, json!({"path": "f.txt", "old_string": "", "new_string": "x"})).await;
        fail(&t, EDIT_FILE, json!({"path": "missing.txt", "old_string": "a", "new_string": "b"})).await;

        let out = ok(&t, EDIT_FILE, json!({"path": "f.txt", "old_string": "  beta\n", "new_string": "  BETA\n"})).await;
        assert!(out.contains("replaced 1 occurrence."), "{out}");
        assert_eq!(fs::read_to_string(root.join("f.txt")).unwrap(), "alpha\n  BETA\nalpha\n");

        let out = ok(&t, EDIT_FILE, json!({"path": "f.txt", "old_string": "alpha", "new_string": "omega", "replace_all": true})).await;
        assert!(out.contains("replaced 2 occurrences"), "{out}");
        assert_eq!(fs::read_to_string(root.join("f.txt")).unwrap(), "omega\n  BETA\nomega\n");
    }

    fn with_commands(root: &Path, f: impl FnOnce(&mut Policy)) -> BuiltinTools {
        tools(root, |p| {
            p.allow_commands = true;
            f(p);
        })
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_command_runs_in_root_with_minimal_env() {
        let (_tmp, root) = setup();
        // Cargo sets this for the test process; it must not reach the command.
        assert!(std::env::var_os("CARGO_MANIFEST_DIR").is_some());
        let t = with_commands(&root, |_| {});
        let out = ok(&t, RUN_COMMAND, json!({"command": "pwd; ls; echo \"[$CARGO_MANIFEST_DIR]\""})).await;
        assert!(out.contains(&t.root().display().to_string()), "{out}");
        assert!(out.contains("README.md") && out.contains("[]") && out.ends_with("[exit code 0]"), "{out}");

        let out = fail(&t, RUN_COMMAND, json!({"command": "echo oops >&2; exit 3"})).await;
        assert!(out.contains("[stderr]\noops") && out.contains("[exit code 3]"), "{out}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_command_times_out_and_kills_children() {
        let (_tmp, root) = setup();
        let t = with_commands(&root, |_| {});
        let started = std::time::Instant::now();
        let out = fail(&t, RUN_COMMAND, json!({"command": "echo start; sleep 30 & sleep 30; touch late", "timeout_secs": 1})).await;
        assert!(started.elapsed() < Duration::from_secs(10), "took {:?}", started.elapsed());
        assert!(out.contains("timed out after 1s") && out.contains("start"), "{out}");
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(!root.join("late").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_command_caps_output() {
        let (_tmp, root) = setup();
        let t = with_commands(&root, |p| p.max_output_bytes = 100);
        let out = ok(&t, RUN_COMMAND, json!({"command": "yes hello | head -n 1000"})).await;
        assert!(out.contains("[Output truncated to 100 bytes.]"), "{out}");
        assert!(out.len() < 300, "{}", out.len());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_command_allowlist() {
        let (_tmp, root) = setup();
        let t = with_commands(&root, |p| p.command_allowlist = Some(vec!["echo".into(), "ls -la".into()]));
        assert!(ok(&t, RUN_COMMAND, json!({"command": "echo hi"})).await.contains("hi"));
        ok(&t, RUN_COMMAND, json!({"command": "ls -la"})).await;
        ok(&t, RUN_COMMAND, json!({"command": "ls -la src"})).await;
        assert!(fail(&t, RUN_COMMAND, json!({"command": "ls"})).await.contains("not allowed"));
        assert!(fail(&t, RUN_COMMAND, json!({"command": "echoX hi"})).await.contains("not allowed"));
        assert!(fail(&t, RUN_COMMAND, json!({"command": "touch x"})).await.contains("not allowed"));
        for chained in ["echo hi; touch x", "echo hi && touch x", "echo $(touch x)", "echo `touch x`", "echo hi > x", "echo hi | sh"] {
            fail(&t, RUN_COMMAND, json!({ "command": chained })).await;
        }
        assert!(!root.join("x").exists());
    }
}
