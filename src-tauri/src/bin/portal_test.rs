//! Standalone test for the xdg-desktop-portal GlobalShortcuts path.
//!
//! Run on Wayland (GNOME/Mutter, KDE/KWin, wlroots) to verify the portal
//! can deliver global hotkey press/release events to a background app —
//! the piece that `tauri-plugin-global-shortcut` currently can't do on
//! Wayland because Mutter refuses the X11-style key grab.
//!
//! Usage:
//!     cargo run --bin portal_test
//!
//! What it does:
//! 1. Connects to xdg-desktop-portal via D-Bus.
//! 2. Creates a GlobalShortcuts session.
//! 3. Binds one shortcut id `qol-toggle` with preferred trigger
//!    `CTRL+ALT+space`. The portal pops a dialog asking the user to
//!    confirm (or pick a different combo).
//! 4. Prints every Activated / Deactivated event to stdout until Ctrl+C.
//!
//! Success criteria:
//!     PRESSED  qol-toggle   @ <timestamp>
//!     RELEASED qol-toggle   @ <timestamp>
//! ...appears in your terminal when you press and release the bound combo,
//! while focus is on a different window. If you see those lines, the
//! Wayland hotkey path works end-to-end and the qol integration just needs
//! to wrap this same flow.

use std::time::SystemTime;

use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut};
use futures_util::StreamExt;

const SHORTCUT_ID: &str = "qol-toggle";

#[tokio::main]
async fn main() -> ashpd::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "portal_test=debug,ashpd=info,warn".into()),
        )
        .init();

    println!("== qol portal_test ==");
    println!("Connecting to xdg-desktop-portal …");
    let proxy = GlobalShortcuts::new().await?;

    println!("Creating session …");
    let session = proxy.create_session().await?;

    let shortcuts = [
        NewShortcut::new(SHORTCUT_ID, "Toggle qol dictation").preferred_trigger("CTRL+ALT+space")
    ];

    println!(
        "Asking portal to bind 1 shortcut (preferred trigger: CTRL+ALT+space).\n\
         Mutter/KWin should pop a confirmation dialog — accept it."
    );
    let bind = proxy
        .bind_shortcuts(&session, &shortcuts, None)
        .await?
        .response()?;
    println!(
        "Bind OK. Portal accepted {} shortcut(s):",
        bind.shortcuts().len()
    );
    for s in bind.shortcuts() {
        println!(
            "  - id={:?} description={:?} trigger={:?}",
            s.id(),
            s.description(),
            s.trigger_description()
        );
    }
    println!("\nWaiting for shortcut events (Ctrl+C to exit).\n");

    let mut activated = Box::pin(proxy.receive_activated().await?);
    let mut deactivated = Box::pin(proxy.receive_deactivated().await?);

    loop {
        tokio::select! {
            Some(ev) = activated.next() => {
                println!(
                    "PRESSED  {:<12} @ {:?}",
                    ev.shortcut_id(),
                    SystemTime::now(),
                );
            }
            Some(ev) = deactivated.next() => {
                println!(
                    "RELEASED {:<12} @ {:?}",
                    ev.shortcut_id(),
                    SystemTime::now(),
                );
            }
            else => {
                println!("portal event streams closed; exiting");
                break;
            }
        }
    }

    Ok(())
}
