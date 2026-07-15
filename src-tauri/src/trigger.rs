//! Unix-socket control channel for the qol daemon.
//!
//! Lets the `qol <cmd>` CLI fast-path start, stop, or toggle dictation by
//! writing one line to `$XDG_RUNTIME_DIR/qol.sock`. Used as the
//! Wayland-on-GNOME workaround for hotkeys: GNOME Custom Shortcuts can
//! invoke any command on keypress, so binding `qol toggle` to a
//! key combo gives us a working trigger without needing the (broken for
//! non-sandboxed apps) `xdg-desktop-portal` GlobalShortcuts path.
//!
//! Wire protocol (one-line, newline-terminated):
//!
//!   client → server:   START | STOP | TOGGLE | STATUS
//!   server → client:   OK <msg> | STATUS idle | STATUS recording | ERR <msg>

use anyhow::{Context, Result};
use parking_lot::Mutex;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

/// Minimum gap between successive TOGGLE commands. GNOME's gsd-media-keys
/// has a long-standing bug where custom keybindings re-fire once after
/// the keyboard auto-repeat delay if the user holds the combo slightly
/// long — turning a single tap into start-then-immediate-stop. A 600 ms
/// guard quietly drops the second fire.
const TOGGLE_DEBOUNCE: Duration = Duration::from_millis(600);

/// Default socket path. Prefers `$XDG_RUNTIME_DIR/qol.sock`; falls back to
/// `/tmp/qol-<uid>.sock` so the daemon still works in unusual environments
/// where `XDG_RUNTIME_DIR` is unset.
pub fn socket_path() -> PathBuf {
    if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir).join("qol.sock");
        }
    }
    let uid = unsafe { libc::getuid() };
    PathBuf::from(format!("/tmp/qol-{uid}.sock"))
}

/// Synchronous one-shot client: connect to the running daemon's socket, send
/// one command, and return the acknowledgement line. Used by the `qol <cmd>`
/// CLI fast-path, which must not touch tokio or Tauri, so this deliberately
/// uses blocking std sockets.
pub fn send_command(cmd: &str) -> std::io::Result<String> {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    let path = socket_path();
    let mut stream = UnixStream::connect(&path)?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    writeln!(stream, "{cmd}")?;
    // Half-close so the server reads EOF and responds.
    stream.shutdown(std::net::Shutdown::Write)?;
    let mut resp = String::new();
    stream.read_to_string(&mut resp)?;
    Ok(resp.trim().to_string())
}

/// Dictation lifecycle handle the listener uses to drive sessions.
///
/// Implemented by qol's `AppState` (see `main.rs`). Kept as a trait so the
/// listener stays decoupled from Tauri state types and is easy to test.
pub trait DictationControl: Send + Sync + 'static {
    /// Start a session if not already running. Returns `true` if a new
    /// session was started; `false` if one was already in flight.
    fn start(&self) -> bool;
    /// Stop the current session. Returns `true` if a session was stopped;
    /// `false` if nothing was running.
    fn stop(&self) -> bool;
    /// True if a session is currently capturing.
    fn is_recording(&self) -> bool;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    Start,
    Stop,
    Toggle,
    Status,
}

impl Command {
    fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_uppercase().as_str() {
            "START" => Some(Self::Start),
            "STOP" => Some(Self::Stop),
            "TOGGLE" => Some(Self::Toggle),
            "STATUS" => Some(Self::Status),
            _ => None,
        }
    }
}

/// Shared per-listener state. Currently just the last TOGGLE timestamp
/// for debouncing. Cheap to clone.
#[derive(Clone, Default)]
struct ListenerState {
    last_toggle: std::sync::Arc<Mutex<Option<Instant>>>,
}

impl ListenerState {
    /// Returns true if this toggle should proceed. Updates the timestamp.
    fn should_toggle(&self) -> bool {
        let mut guard = self.last_toggle.lock();
        let now = Instant::now();
        if let Some(prev) = *guard {
            if now.duration_since(prev) < TOGGLE_DEBOUNCE {
                return false;
            }
        }
        *guard = Some(now);
        true
    }
}

/// Run the trigger listener forever on the current Tokio runtime.
///
/// Removes any stale socket at the path before binding. Spawn this once
/// at startup, e.g.
///
/// ```ignore
/// tauri::async_runtime::spawn(async move {
///     let _ = qol::trigger::run(control.clone()).await;
/// });
/// ```
pub async fn run<C: DictationControl>(control: std::sync::Arc<C>) -> Result<()> {
    let path = socket_path();
    // Remove a stale socket from a previous run (only if it's actually a
    // socket — we don't want to nuke arbitrary files).
    if path.exists() {
        if let Ok(meta) = std::fs::metadata(&path) {
            use std::os::unix::fs::FileTypeExt;
            if meta.file_type().is_socket() {
                let _ = std::fs::remove_file(&path);
            } else {
                anyhow::bail!(
                    "{} exists and is not a socket — refusing to overwrite",
                    path.display()
                );
            }
        }
    }

    let listener = UnixListener::bind(&path).with_context(|| format!("bind {}", path.display()))?;
    // Owner-only access. The socket lives in $XDG_RUNTIME_DIR which is
    // already 0700, so this is defense-in-depth.
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));

    let state = ListenerState::default();
    tracing::info!(socket = %path.display(), "trigger listener up");
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let control = control.clone();
                let state = state.clone();
                tokio::spawn(async move {
                    if let Err(e) = serve(stream, control, state).await {
                        tracing::warn!(error = ?e, "trigger client failed");
                    }
                });
            }
            Err(e) => {
                tracing::warn!(error = ?e, "trigger accept failed");
            }
        }
    }
}

async fn serve<C: DictationControl>(
    stream: UnixStream,
    control: std::sync::Arc<C>,
    state: ListenerState,
) -> Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    let n = reader.read_line(&mut line).await?;
    if n == 0 {
        return Ok(());
    }

    let response = match Command::parse(&line) {
        Some(Command::Start) => {
            if control.start() {
                "OK started\n".to_string()
            } else {
                "OK already-recording\n".to_string()
            }
        }
        Some(Command::Stop) => {
            if control.stop() {
                "OK stopped\n".to_string()
            } else {
                "OK already-idle\n".to_string()
            }
        }
        Some(Command::Toggle) => {
            if state.should_toggle() {
                if control.is_recording() {
                    control.stop();
                    "OK toggled idle\n".to_string()
                } else {
                    control.start();
                    "OK toggled recording\n".to_string()
                }
            } else {
                // Within debounce window — likely a GNOME double-fire.
                tracing::info!("trigger: debounced toggle (within 600ms of last)");
                "OK debounced\n".to_string()
            }
        }
        Some(Command::Status) => {
            if control.is_recording() {
                "STATUS recording\n".to_string()
            } else {
                "STATUS idle\n".to_string()
            }
        }
        None => format!("ERR unknown command: {}\n", line.trim()),
    };

    write_half.write_all(response.as_bytes()).await?;
    write_half.shutdown().await.ok();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    #[derive(Default)]
    struct FakeControl {
        recording: AtomicBool,
        starts: AtomicUsize,
        stops: AtomicUsize,
    }

    impl DictationControl for FakeControl {
        fn start(&self) -> bool {
            self.starts.fetch_add(1, Ordering::Relaxed);
            !self.recording.swap(true, Ordering::Relaxed)
        }
        fn stop(&self) -> bool {
            self.stops.fetch_add(1, Ordering::Relaxed);
            self.recording.swap(false, Ordering::Relaxed)
        }
        fn is_recording(&self) -> bool {
            self.recording.load(Ordering::Relaxed)
        }
    }

    #[test]
    fn listener_state_debounces_rapid_toggles() {
        let s = ListenerState::default();
        assert!(s.should_toggle(), "first toggle always proceeds");
        assert!(
            !s.should_toggle(),
            "second toggle within window must be suppressed"
        );
        assert!(
            !s.should_toggle(),
            "third toggle within window also suppressed"
        );
        // Pretend enough time passed.
        *s.last_toggle.lock() = Some(Instant::now() - TOGGLE_DEBOUNCE * 2);
        assert!(s.should_toggle(), "toggle after window proceeds");
    }

    #[test]
    fn parses_known_commands_case_insensitive() {
        assert_eq!(Command::parse("start"), Some(Command::Start));
        assert_eq!(Command::parse("STOP\n"), Some(Command::Stop));
        assert_eq!(Command::parse("  Toggle  "), Some(Command::Toggle));
        assert_eq!(Command::parse("STATUS"), Some(Command::Status));
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(Command::parse(""), None);
        assert_eq!(Command::parse("recordnow"), None);
        assert_eq!(Command::parse("st art"), None);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn toggle_flips_state() {
        let ctrl = Arc::new(FakeControl::default());
        let path = std::env::temp_dir().join(format!("qol-test-{}.sock", std::process::id()));
        // Wire up listener manually since spawn() reads env vars.
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        let ctrl_for_task = ctrl.clone();
        let state = ListenerState::default();
        let server = tokio::spawn(async move {
            for _ in 0..3 {
                let (stream, _) = listener.accept().await.unwrap();
                serve(stream, ctrl_for_task.clone(), state.clone())
                    .await
                    .unwrap();
            }
        });

        async fn send(path: &std::path::Path, cmd: &str) -> String {
            let stream = UnixStream::connect(path).await.unwrap();
            let (read_half, mut write_half) = stream.into_split();
            write_half.write_all(cmd.as_bytes()).await.unwrap();
            write_half.shutdown().await.ok();
            let mut reader = BufReader::new(read_half);
            let mut buf = String::new();
            reader.read_line(&mut buf).await.unwrap();
            buf
        }

        // Initially idle.
        assert_eq!(send(&path, "STATUS\n").await, "STATUS idle\n");
        // Toggle → recording.
        assert_eq!(send(&path, "TOGGLE\n").await, "OK toggled recording\n");
        // Wait past the debounce window before the next toggle so the
        // GNOME-double-fire guard doesn't suppress it.
        tokio::time::sleep(TOGGLE_DEBOUNCE + Duration::from_millis(50)).await;
        // Toggle → idle.
        assert_eq!(send(&path, "TOGGLE\n").await, "OK toggled idle\n");

        server.await.unwrap();
        assert!(!ctrl.is_recording());
        assert_eq!(ctrl.starts.load(Ordering::Relaxed), 1);
        assert_eq!(ctrl.stops.load(Ordering::Relaxed), 1);
        let _ = std::fs::remove_file(&path);
    }
}
