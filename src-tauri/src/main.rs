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

use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Manager, RunEvent};
use tauri_plugin_global_shortcut::{Code, Modifiers, Shortcut, ShortcutState};

use crate::config::Config;
use crate::inject::InjectorHandle;
use crate::session::Session;
use crate::shortcut::parse_shortcut;

pub struct AppState {
    pub cfg: Mutex<Config>,
    pub injector: InjectorHandle,
    pub session: Mutex<Option<Session>>,
    pub paused: AtomicBool,
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

    let state = AppState {
        cfg: Mutex::new(cfg.clone()),
        injector,
        session: Mutex::new(None),
        paused: AtomicBool::new(false),
    };

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
                    let state = app.state::<AppState>();
                    if state.paused.load(Ordering::Relaxed) {
                        return;
                    }
                    match event.state() {
                        ShortcutState::Pressed => start_session(&state),
                        ShortcutState::Released => stop_session(&state),
                    }
                })
                .build(),
        )
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            get_config,
            save_config,
            test_aavaaz
        ])
        .setup(|app| {
            let open = MenuItem::with_id(app, TRAY_OPEN, "Open settings", true, None::<&str>)?;
            let pause = MenuItem::with_id(app, TRAY_PAUSE, "Pause dictation", true, None::<&str>)?;
            let sep = PredefinedMenuItem::separator(app)?;
            let quit = MenuItem::with_id(app, TRAY_QUIT, "Quit qol", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &pause, &sep, &quit])?;

            let _tray = TrayIconBuilder::with_id("qol-tray")
                .tooltip("qol — voice dictation")
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
                        let state = app.state::<AppState>();
                        let was = state.paused.fetch_xor(true, Ordering::Relaxed);
                        let now_paused = !was;
                        tracing::info!(paused = now_paused, "toggle pause");
                        if now_paused {
                            stop_session(&state);
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

fn start_session(state: &tauri::State<AppState>) {
    let mut slot = state.session.lock();
    if slot.is_some() {
        return;
    }
    let cfg = state.cfg.lock().clone();
    match Session::start(cfg, state.injector.clone()) {
        Ok(sess) => {
            tracing::info!("session started");
            *slot = Some(sess);
        }
        Err(e) => tracing::error!(error = ?e, "session start failed"),
    }
}

fn stop_session(state: &tauri::State<AppState>) {
    let Some(sess) = state.session.lock().take() else {
        return;
    };
    tauri::async_runtime::spawn(async move {
        sess.stop().await;
        tracing::info!("session stopped");
    });
}

#[tauri::command]
fn get_config(state: tauri::State<AppState>) -> Config {
    state.cfg.lock().clone()
}

#[tauri::command]
fn save_config(new_cfg: Config, state: tauri::State<AppState>) -> Result<(), String> {
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
