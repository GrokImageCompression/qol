//! Tiny CLI that pokes the qol daemon's Unix-socket trigger.
//!
//! Designed to be bound to a GNOME / KDE / wlroots custom keybinding so
//! you get a working hotkey on Wayland where `xdg-desktop-portal`'s
//! GlobalShortcuts interface refuses non-sandboxed callers.
//!
//! Usage:
//!     qol-trigger              # default: toggle
//!     qol-trigger toggle       # start if idle, stop if recording
//!     qol-trigger start
//!     qol-trigger stop
//!     qol-trigger status       # prints "idle" or "recording"
//!
//! Exit codes:
//!     0   command sent + acknowledged
//!     1   socket missing / not connectable (qol not running?)
//!     2   bad arguments
//!     3   server returned ERR
//!
//! Socket path: `$XDG_RUNTIME_DIR/qol.sock`, fallback `/tmp/qol-<uid>.sock`.

use std::env;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

fn socket_path() -> PathBuf {
    if let Ok(dir) = env::var("XDG_RUNTIME_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir).join("qol.sock");
        }
    }
    let uid = unsafe { libc::getuid() };
    PathBuf::from(format!("/tmp/qol-{uid}.sock"))
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    let cmd = match args.get(1).map(|s| s.to_ascii_lowercase()).as_deref() {
        None | Some("toggle") => "TOGGLE",
        Some("start") => "START",
        Some("stop") => "STOP",
        Some("status") => "STATUS",
        Some(other) => {
            eprintln!("qol-trigger: unknown command: {other}");
            eprintln!("usage: qol-trigger [toggle|start|stop|status]");
            return ExitCode::from(2);
        }
    };

    let path = socket_path();
    let mut stream = match UnixStream::connect(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "qol-trigger: cannot connect to {} ({}). Is qol running?",
                path.display(),
                e
            );
            return ExitCode::from(1);
        }
    };
    // Short timeouts — the daemon should answer in <1 ms locally.
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));

    if let Err(e) = writeln!(stream, "{cmd}") {
        eprintln!("qol-trigger: write failed: {e}");
        return ExitCode::from(1);
    }
    // Half-close so the server reads EOF after the command.
    let _ = stream.shutdown(std::net::Shutdown::Write);

    let mut response = String::new();
    if let Err(e) = stream.read_to_string(&mut response) {
        eprintln!("qol-trigger: read failed: {e}");
        return ExitCode::from(1);
    }
    let line = response.trim();
    println!("{line}");

    if line.starts_with("ERR") {
        ExitCode::from(3)
    } else {
        ExitCode::SUCCESS
    }
}
