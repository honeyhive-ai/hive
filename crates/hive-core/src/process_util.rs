//! Spawning helpers shared by the app and runtime.
//!
//! Release builds set `windows_subsystem = "windows"` (see `app/src/main.rs`),
//! so Hive.exe owns no console. On Windows, when a process without a console
//! spawns a console application, the OS allocates a **brand new console** for
//! the child — a black window that pops up on screen and vanishes when the
//! child exits. An agent turn shells out to `git` a dozen times (worktree
//! isolation, diff capture, attribution), so the user sees a terminal flash on
//! every single response.
//!
//! `CREATE_NO_WINDOW` (`0x08000000`) tells Windows to give the child a console
//! that has no window. The child still gets a working console handle — and,
//! importantly, its own grandchildren inherit that windowless console — so a
//! CLI agent's nested tool calls stay invisible too, without any of them
//! needing the flag themselves.
//!
//! Use [`command`] instead of `std::process::Command::new` for anything Hive
//! spawns in the background. Both entry points are no-ops off Windows.

use std::ffi::OsStr;
use std::process::Command;

/// `CREATE_NO_WINDOW` — a console for the child, but no window for it.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Drop-in replacement for `std::process::Command::new` that never flashes a
/// console window on Windows. Returns an owned `Command`, so the usual builder
/// chain (`.args(..).output()`) works unchanged.
pub fn command(program: impl AsRef<OsStr>) -> Command {
    let mut cmd = Command::new(program);
    no_console_window(&mut cmd);
    cmd
}

/// Apply the no-window creation flag to an already-built `Command`, for callers
/// that can't swap their constructor.
pub fn no_console_window(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = cmd;
}
