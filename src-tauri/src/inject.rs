use anyhow::{anyhow, Result};
use enigo::{Enigo, Keyboard, Settings};
use std::sync::mpsc::{self, Sender};
use std::thread;

use crate::config::InjectMethod;

/// Send+Sync handle to a dedicated thread that owns the !Send `Enigo` instance.
#[derive(Clone)]
pub struct InjectorHandle {
    tx: Sender<InjectCmd>,
}

enum InjectCmd {
    Inject { text: String, method: InjectMethod },
}

impl InjectorHandle {
    pub fn spawn() -> Result<Self> {
        let (tx, rx) = mpsc::channel::<InjectCmd>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();

        thread::Builder::new()
            .name("qol-inject".into())
            .spawn(move || {
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
                    match cmd {
                        InjectCmd::Inject { text, method } => {
                            if let Err(e) = do_inject(&mut enigo, &text, method) {
                                tracing::error!(error = ?e, "inject failed");
                            }
                        }
                    }
                }
            })?;

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self { tx }),
            Ok(Err(e)) => Err(anyhow!(e)),
            Err(e) => Err(anyhow!("injector thread died: {e}")),
        }
    }

    pub fn inject(&self, text: String, method: InjectMethod) {
        let _ = self.tx.send(InjectCmd::Inject { text, method });
    }
}

fn do_inject(enigo: &mut Enigo, text: &str, method: InjectMethod) -> Result<()> {
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

pub fn active_app_name() -> Option<String> {
    active_win_pos_rs::get_active_window()
        .ok()
        .map(|w| w.app_name)
}
