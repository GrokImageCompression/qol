#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

mod audio;
mod commands;
mod config;
mod inject;
mod polish;
mod session;
mod shortcut;
mod transport;
mod trigger;

use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, RunEvent};
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_global_shortcut::{Code, Modifiers, Shortcut, ShortcutState};

use crate::config::Config;
use crate::inject::InjectorHandle;
use crate::session::Session;
use crate::shortcut::parse_shortcut;
use crate::trigger::DictationControl;

const TRAY_ID: &str = "qol-tray";
const TOOLTIP_IDLE: &str = "qol — voice dictation";
const TOOLTIP_RECORDING: &str = "qol — recording";

pub struct AppState {
    pub cfg: Mutex<Config>,
    pub injector: InjectorHandle,
    pub session: Mutex<Option<Session>>,
    pub paused: AtomicBool,
    /// Set once during setup; used to drive the tray indicator and notifications.
    pub handle: OnceLock<AppHandle>,
}

/// Swap the tray icon + tooltip to reflect whether we're capturing.
fn set_recording_indicator(app: &AppHandle, recording: bool) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };
    let bytes: &[u8] = if recording {
        include_bytes!("../icons/tray-recording.png")
    } else {
        include_bytes!("../icons/tray.png")
    };
    if let Ok(img) = tauri::image::Image::from_bytes(bytes) {
        let _ = tray.set_icon(Some(img));
    }
    // The recording dot is red; template mode would tint it away on macOS.
    let _ = tray.set_icon_as_template(!recording);
    let _ = tray.set_tooltip(Some(if recording {
        TOOLTIP_RECORDING
    } else {
        TOOLTIP_IDLE
    }));
}

fn notify(app: &AppHandle, title: &str, body: &str) {
    use tauri_plugin_notification::NotificationExt;
    let _ = app.notification().builder().title(title).body(body).show();
}

impl AppState {
    fn start_locked(&self) -> bool {
        let mut slot = self.session.lock();
        if slot.is_some() {
            return false;
        }
        let cfg = self.cfg.lock().clone();
        // On an unexpected transport end (e.g. Aavaaz unreachable), flip the
        // indicator back to idle and tell the user. The stale session in the
        // slot self-heals on the next start/stop toggle.
        let on_end = {
            let handle = self.handle.get().cloned();
            Box::new(move |err: Option<String>| {
                if let (Some(app), Some(msg)) = (handle, err) {
                    set_recording_indicator(&app, false);
                    notify(&app, "qol: dictation stopped", &msg);
                }
            })
        };
        match Session::start(cfg, self.injector.clone(), on_end) {
            Ok(sess) => {
                tracing::info!("session started");
                *slot = Some(sess);
                if let Some(app) = self.handle.get() {
                    set_recording_indicator(app, true);
                }
                true
            }
            Err(e) => {
                tracing::error!(error = ?e, "session start failed");
                if let Some(app) = self.handle.get() {
                    notify(app, "qol: couldn't start dictation", &e.to_string());
                }
                false
            }
        }
    }

    fn stop_locked(&self) -> bool {
        let Some(sess) = self.session.lock().take() else {
            return false;
        };
        if let Some(app) = self.handle.get() {
            set_recording_indicator(app, false);
        }
        tauri::async_runtime::spawn(async move {
            sess.stop().await;
            tracing::info!("session stopped");
        });
        true
    }
}

impl DictationControl for AppState {
    fn start(&self) -> bool {
        if self.paused.load(Ordering::Relaxed) {
            return false;
        }
        self.start_locked()
    }
    fn stop(&self) -> bool {
        self.stop_locked()
    }
    fn is_recording(&self) -> bool {
        self.session.lock().is_some()
    }
}

const TRAY_OPEN: &str = "qol_open";
const TRAY_PAUSE: &str = "qol_pause";
const TRAY_QUIT: &str = "qol_quit";

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "qol=info,warn".into()),
        )
        .init();

    let cfg = Config::load().expect("load config");
    let injector = InjectorHandle::spawn().expect("init keystroke injector");

    let state = Arc::new(AppState {
        cfg: Mutex::new(cfg.clone()),
        injector,
        session: Mutex::new(None),
        paused: AtomicBool::new(false),
        handle: OnceLock::new(),
    });

    // Start the Unix-socket trigger listener (used by `qol-trigger` as the
    // Wayland-on-GNOME workaround for global hotkeys). Runs on every
    // platform — cheap, and useful even on X11 for scripting.
    {
        let control = state.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(e) = trigger::run(control).await {
                tracing::error!(error = ?e, "trigger listener exited");
            }
        });
    }

    let shortcut = parse_shortcut(&cfg.hotkey)
        .unwrap_or_else(|| Shortcut::new(Some(Modifiers::SUPER), Code::Space));

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            if let Some(win) = app.get_webview_window("main") {
                let _ = win.show();
                let _ = win.set_focus();
            }
        }))
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_notification::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_shortcut(shortcut)
                .expect("register shortcut")
                .with_handler(move |app, sc, event| {
                    if sc != &shortcut {
                        return;
                    }
                    let state = app.state::<Arc<AppState>>();
                    if state.paused.load(Ordering::Relaxed) {
                        return;
                    }
                    match event.state() {
                        ShortcutState::Pressed => {
                            state.start_locked();
                        }
                        ShortcutState::Released => {
                            state.stop_locked();
                        }
                    }
                })
                .build(),
        )
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            get_config,
            save_config,
            test_aavaaz,
            set_polish_api_key,
            has_polish_api_key,
            get_autostart,
            set_autostart
        ])
        .setup(|app| {
            {
                let handle = app.handle().clone();
                let _ = app.state::<Arc<AppState>>().handle.set(handle);
            }

            let open = MenuItem::with_id(app, TRAY_OPEN, "Open settings", true, None::<&str>)?;
            let pause = MenuItem::with_id(app, TRAY_PAUSE, "Pause dictation", true, None::<&str>)?;
            let sep = PredefinedMenuItem::separator(app)?;
            let quit = MenuItem::with_id(app, TRAY_QUIT, "Quit qol", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &pause, &sep, &quit])?;

            let _tray = TrayIconBuilder::with_id(TRAY_ID)
                .tooltip(TOOLTIP_IDLE)
                .menu(&menu)
                .show_menu_on_left_click(true)
                .on_menu_event(move |app, event| match event.id().as_ref() {
                    TRAY_OPEN => {
                        if let Some(win) = app.get_webview_window("main") {
                            let _ = win.show();
                            let _ = win.unminimize();
                            let _ = win.set_focus();
                        }
                    }
                    TRAY_PAUSE => {
                        let state = app.state::<Arc<AppState>>();
                        let was = state.paused.fetch_xor(true, Ordering::Relaxed);
                        let now_paused = !was;
                        tracing::info!(paused = now_paused, "toggle pause");
                        if now_paused {
                            state.stop_locked();
                        }
                    }
                    TRAY_QUIT => {
                        app.exit(0);
                    }
                    _ => {}
                })
                .build(app)?;

            if let Some(win) = app.get_webview_window("main") {
                let _ = win.hide();
                let win_clone = win.clone();
                win.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = win_clone.hide();
                    }
                });
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("build app")
        .run(|_app, event| {
            if let RunEvent::ExitRequested { .. } = event {
                tracing::info!("exit requested");
            }
        });
}

#[tauri::command]
fn get_config(state: tauri::State<Arc<AppState>>) -> Config {
    state.cfg.lock().clone()
}

#[tauri::command]
fn save_config(new_cfg: Config, state: tauri::State<Arc<AppState>>) -> Result<(), String> {
    new_cfg.save().map_err(|e| e.to_string())?;
    *state.cfg.lock() = new_cfg;
    Ok(())
}

#[tauri::command]
async fn test_aavaaz(url: String) -> Result<String, String> {
    use tokio_tungstenite::connect_async;
    connect_async(&url)
        .await
        .map(|_| format!("connected to {url}"))
        .map_err(|e| format!("connect failed: {e}"))
}

// Sync commands run on a Tauri worker thread, off the tokio reactor, so the
// blocking keyring I/O below can't wedge the async runtime. keyring::Error's
// Display never contains the secret, so mapping it to a string is safe.

/// Store the polish key in the OS keyring under this base_url, or delete the
/// entry when `key` is empty. Never logs the key.
#[tauri::command]
fn set_polish_api_key(base_url: String, key: String) -> Result<(), String> {
    let entry =
        keyring::Entry::new(polish::KEYRING_SERVICE, &base_url).map_err(|e| e.to_string())?;
    if key.is_empty() {
        // empty means "clear it"; a missing entry is already that state.
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    } else {
        entry.set_password(&key).map_err(|e| e.to_string())
    }
}

/// Whether a polish key is stored for this base_url. Never returns the value.
#[tauri::command]
fn has_polish_api_key(base_url: String) -> bool {
    polish::keyring_get(&base_url).is_some()
}

/// Whether qol is registered to launch at login. OS state, not config.json.
#[tauri::command]
fn get_autostart(app: AppHandle) -> bool {
    app.autolaunch().is_enabled().unwrap_or(false)
}

#[tauri::command]
fn set_autostart(app: AppHandle, enabled: bool) -> Result<(), String> {
    let mgr = app.autolaunch();
    if enabled { mgr.enable() } else { mgr.disable() }.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    // Both tray states must decode at runtime; set_recording_indicator relies
    // on it, and the image-png feature + assets are easy to drop by accident.
    #[test]
    fn tray_icons_decode() {
        assert!(tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png")).is_ok());
        assert!(
            tauri::image::Image::from_bytes(include_bytes!("../icons/tray-recording.png")).is_ok()
        );
    }
}
