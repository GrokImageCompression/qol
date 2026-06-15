//! Keystroke injection.
//!
//! `Enigo` and platform handles are `!Send`, so we confine them to a
//! dedicated OS thread and expose a `Send + Sync` channel handle.
//!
//! On Linux, GNOME Wayland blocks synthetic X11 input. When we detect a
//! Wayland session we route through `ydotool` instead (requires the
//! `ydotoold` daemon running with uinput access — see README).

use anyhow::{anyhow, Result};
use enigo::{Enigo, Keyboard, Settings};
use std::process::Command as ProcCommand;
use std::sync::mpsc::{self, Sender};
use std::thread;

use crate::config::InjectMethod;

#[derive(Clone)]
pub struct InjectorHandle {
    tx: Sender<InjectCmd>,
    backend: Backend,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Enigo,
    Ydotool,
}

enum InjectCmd {
    Type { text: String, method: InjectMethod },
    Backspace(usize),
    SelectAll,
    Newline,
    Paragraph,
}

impl InjectorHandle {
    pub fn spawn() -> Result<Self> {
        let backend = detect_backend();
        tracing::info!(?backend, "selected injection backend");

        let (tx, rx) = mpsc::channel::<InjectCmd>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();

        thread::Builder::new()
            .name("qol-inject".into())
            .spawn(move || match backend {
                Backend::Enigo => run_enigo(rx, ready_tx),
                Backend::Ydotool => run_ydotool(rx, ready_tx),
            })?;

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self { tx, backend }),
            Ok(Err(e)) => Err(anyhow!(e)),
            Err(e) => Err(anyhow!("injector thread died: {e}")),
        }
    }

    pub fn backend(&self) -> Backend {
        self.backend
    }

    pub fn inject(&self, text: String, method: InjectMethod) {
        let _ = self.tx.send(InjectCmd::Type { text, method });
    }

    pub fn backspace(&self, n: usize) {
        if n == 0 {
            return;
        }
        let _ = self.tx.send(InjectCmd::Backspace(n));
    }

    pub fn newline(&self) {
        let _ = self.tx.send(InjectCmd::Newline);
    }

    pub fn paragraph(&self) {
        let _ = self.tx.send(InjectCmd::Paragraph);
    }

    pub fn select_all(&self) {
        let _ = self.tx.send(InjectCmd::SelectAll);
    }
}

fn detect_backend() -> Backend {
    #[cfg(target_os = "linux")]
    {
        let is_wayland = std::env::var("WAYLAND_DISPLAY").is_ok()
            || std::env::var("XDG_SESSION_TYPE")
                .map(|s| s.eq_ignore_ascii_case("wayland"))
                .unwrap_or(false);
        if is_wayland && which_ydotool() {
            return Backend::Ydotool;
        }
    }
    Backend::Enigo
}

#[cfg(target_os = "linux")]
fn which_ydotool() -> bool {
    ProcCommand::new("ydotool")
        .arg("--help")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

// ─────────────────────────────── Enigo backend ───────────────────────────────

fn run_enigo(rx: mpsc::Receiver<InjectCmd>, ready_tx: mpsc::Sender<Result<(), String>>) {
    let mut enigo = match Enigo::new(&Settings::default()) {
        Ok(e) => {
            let _ = ready_tx.send(Ok(()));
            e
        }
        Err(e) => {
            let _ = ready_tx.send(Err(format!("enigo init: {e}")));
            return;
        }
    };

    while let Ok(cmd) = rx.recv() {
        if let Err(e) = enigo_dispatch(&mut enigo, cmd) {
            tracing::error!(error = ?e, "enigo inject failed");
        }
    }
}

fn enigo_dispatch(enigo: &mut Enigo, cmd: InjectCmd) -> Result<()> {
    use enigo::{Direction, Key};
    match cmd {
        InjectCmd::Type { text, method } => enigo_type(enigo, &text, method),
        InjectCmd::Backspace(n) => {
            for _ in 0..n {
                enigo
                    .key(Key::Backspace, Direction::Click)
                    .map_err(|e| anyhow!("backspace: {e}"))?;
            }
            Ok(())
        }
        InjectCmd::Newline => enigo
            .key(Key::Return, Direction::Click)
            .map_err(|e| anyhow!("enter: {e}")),
        InjectCmd::Paragraph => {
            enigo
                .key(Key::Return, Direction::Click)
                .map_err(|e| anyhow!("enter1: {e}"))?;
            enigo
                .key(Key::Return, Direction::Click)
                .map_err(|e| anyhow!("enter2: {e}"))
        }
        InjectCmd::SelectAll => enigo_combo(enigo, 'a'),
    }
}

fn enigo_type(enigo: &mut Enigo, text: &str, method: InjectMethod) -> Result<()> {
    use enigo::{Direction, Key};
    match method {
        InjectMethod::Type => {
            enigo.text(text).map_err(|e| anyhow!("type: {e}"))?;
        }
        InjectMethod::Paste => {
            #[cfg(target_os = "macos")]
            let modifier = Key::Meta;
            #[cfg(not(target_os = "macos"))]
            let modifier = Key::Control;
            enigo
                .key(modifier, Direction::Press)
                .map_err(|e| anyhow!("mod press: {e}"))?;
            enigo
                .key(Key::Unicode('v'), Direction::Click)
                .map_err(|e| anyhow!("v click: {e}"))?;
            enigo
                .key(modifier, Direction::Release)
                .map_err(|e| anyhow!("mod release: {e}"))?;
        }
    }
    Ok(())
}

fn enigo_combo(enigo: &mut Enigo, c: char) -> Result<()> {
    use enigo::{Direction, Key};
    #[cfg(target_os = "macos")]
    let modifier = Key::Meta;
    #[cfg(not(target_os = "macos"))]
    let modifier = Key::Control;
    enigo
        .key(modifier, Direction::Press)
        .map_err(|e| anyhow!("mod press: {e}"))?;
    enigo
        .key(Key::Unicode(c), Direction::Click)
        .map_err(|e| anyhow!("{c} click: {e}"))?;
    enigo
        .key(modifier, Direction::Release)
        .map_err(|e| anyhow!("mod release: {e}"))?;
    Ok(())
}

// ────────────────────────────── ydotool backend ──────────────────────────────

fn run_ydotool(rx: mpsc::Receiver<InjectCmd>, ready_tx: mpsc::Sender<Result<(), String>>) {
    // Smoke-test: if `ydotool` runs at all we're good. The daemon may fail
    // per-call if uinput perms are wrong; we surface that as a runtime warning.
    let _ = ready_tx.send(Ok(()));

    while let Ok(cmd) = rx.recv() {
        let result = match cmd {
            InjectCmd::Type { text, .. } => ydotool_type(&text),
            InjectCmd::Backspace(n) => ydotool_key_repeat("14:1 14:0", n),
            InjectCmd::Newline => ydotool_key("28:1 28:0"),
            InjectCmd::Paragraph => ydotool_key("28:1 28:0").and_then(|_| ydotool_key("28:1 28:0")),
            InjectCmd::SelectAll => ydotool_key("29:1 30:1 30:0 29:0"),
        };
        if let Err(e) = result {
            tracing::error!(error = ?e, "ydotool inject failed");
        }
    }
}

/// Build a `ydotool` command pointed at the daemon socket.
///
/// The `ydotool` client defaults to `$XDG_RUNTIME_DIR/.ydotool_socket`, but the
/// Fedora system service runs `ydotoold` as root with the socket at
/// `/tmp/.ydotool_socket`. Honour an explicit `YDOTOOL_SOCKET` override if the
/// environment already sets one, otherwise fall back to that system path so the
/// app works regardless of how the session environment is configured.
fn ydotool_cmd() -> ProcCommand {
    let mut cmd = ProcCommand::new("ydotool");
    if std::env::var_os("YDOTOOL_SOCKET").is_none() {
        cmd.env("YDOTOOL_SOCKET", "/tmp/.ydotool_socket");
    }
    cmd
}

fn ydotool_type(text: &str) -> Result<()> {
    let status = ydotool_cmd()
        .arg("type")
        .arg("--")
        .arg(text)
        .status()
        .map_err(|e| anyhow!("spawn ydotool: {e}"))?;
    if !status.success() {
        return Err(anyhow!("ydotool exited {status}"));
    }
    Ok(())
}

fn ydotool_key(combo: &str) -> Result<()> {
    let status = ydotool_cmd()
        .arg("key")
        .args(combo.split_whitespace())
        .status()
        .map_err(|e| anyhow!("spawn ydotool: {e}"))?;
    if !status.success() {
        return Err(anyhow!("ydotool exited {status}"));
    }
    Ok(())
}

fn ydotool_key_repeat(combo: &str, n: usize) -> Result<()> {
    for _ in 0..n {
        ydotool_key(combo)?;
    }
    Ok(())
}

pub fn active_app_name() -> Option<String> {
    active_win_pos_rs::get_active_window()
        .ok()
        .map(|w| w.app_name)
}
