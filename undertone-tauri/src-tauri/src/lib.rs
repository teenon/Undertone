//! Undertone Tauri app — desktop mixer that talks to the `undertone-daemon`
//! over its Unix-socket JSON protocol.

use std::sync::Arc;

use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Manager, State, WindowEvent};
use tokio::sync::Mutex;
use tracing::{error, info, warn};
use undertone_ipc::{IpcClient, Method, socket_path};

/// Shared handle to the (lazily connected) daemon client. The Mutex
/// serialises requests from concurrent Tauri commands; the Option lets
/// us defer the actual connection until the frontend asks.
struct DaemonClient(Arc<Mutex<Option<IpcClient>>>);

impl DaemonClient {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(None)))
    }
}

/// Open (or reuse) the connection to the daemon socket.
#[tauri::command]
async fn connect_daemon(state: State<'_, DaemonClient>) -> Result<(), String> {
    let mut guard = state.0.lock().await;
    if guard.is_some() {
        return Ok(());
    }
    let client = IpcClient::connect(&socket_path())
        .await
        .map_err(|e| format!("failed to connect to daemon at {:?}: {e}", socket_path()))?;
    info!(socket = ?socket_path(), "connected to undertone-daemon");
    *guard = Some(client);
    Ok(())
}

async fn call(
    state: &State<'_, DaemonClient>,
    method: Method,
) -> Result<serde_json::Value, String> {
    let guard = state.0.lock().await;
    let client = guard
        .as_ref()
        .ok_or_else(|| "daemon not connected — call connect_daemon first".to_string())?;
    let response = client
        .request(method)
        .await
        .map_err(|e| format!("ipc error: {e}"))?;
    response.result.map_err(|e| format!("daemon error {}: {}", e.code, e.message))
}

#[tauri::command]
async fn get_state_snapshot(
    state: State<'_, DaemonClient>,
) -> Result<serde_json::Value, String> {
    call(&state, Method::GetState).await
}

#[tauri::command]
async fn set_mic_mute(
    state: State<'_, DaemonClient>,
    muted: bool,
) -> Result<serde_json::Value, String> {
    call(&state, Method::SetMicMute { muted }).await
}

#[tauri::command]
async fn set_mic_gain(
    state: State<'_, DaemonClient>,
    gain: f32,
) -> Result<serde_json::Value, String> {
    call(&state, Method::SetMicGain { gain }).await
}

#[tauri::command]
async fn set_headphone_volume(
    state: State<'_, DaemonClient>,
    volume: f32,
) -> Result<serde_json::Value, String> {
    call(&state, Method::SetHeadphoneVolume { volume }).await
}

#[tauri::command]
async fn get_mic_chain(state: State<'_, DaemonClient>) -> Result<serde_json::Value, String> {
    call(&state, Method::GetMicChain).await
}

#[tauri::command]
async fn set_effect_bypass(
    state: State<'_, DaemonClient>,
    effect: String,
    bypassed: bool,
) -> Result<serde_json::Value, String> {
    call(&state, Method::SetEffectBypass { effect, bypassed }).await
}

#[tauri::command]
async fn set_effect_param(
    state: State<'_, DaemonClient>,
    effect: String,
    param: String,
    value: f32,
) -> Result<serde_json::Value, String> {
    call(&state, Method::SetEffectParam { effect, param, value }).await
}

#[tauri::command]
async fn load_effect_preset(
    state: State<'_, DaemonClient>,
    name: String,
) -> Result<serde_json::Value, String> {
    call(&state, Method::LoadEffectPreset { name }).await
}

#[tauri::command]
async fn reset_effect_chain(
    state: State<'_, DaemonClient>,
) -> Result<serde_json::Value, String> {
    call(&state, Method::ResetEffectChain).await
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "undertone_tauri_lib=info".into()),
        )
        .try_init();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            app.manage(DaemonClient::new());

            // System tray. Reuses the existing app icon. Best-effort:
            // if the desktop has no tray support (some Wayland
            // compositors) we log and continue — close-to-hide still
            // works, the user just can't bring the window back from
            // tray.
            let show_item = MenuItem::with_id(app, "show", "Show", true, None::<&str>)?;
            let hide_item = MenuItem::with_id(app, "hide", "Hide", true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show_item, &hide_item, &quit_item])?;

            let tray = TrayIconBuilder::with_id("undertone-tray")
                .icon(app.default_window_icon().expect("no default icon").clone())
                .tooltip("Undertone")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => toggle_window(app, Some(true)),
                    "hide" => toggle_window(app, Some(false)),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        toggle_window(tray.app_handle(), None);
                    }
                })
                .build(app);

            if let Err(e) = tray {
                warn!(error = %e, "Failed to create system tray icon — close-to-hide still works but no tray indicator");
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // Intercept the window's X button: hide instead of exit so
            // the React state survives. The tray "Quit" entry is the
            // only path that actually terminates the process.
            if let WindowEvent::CloseRequested { api, .. } = event {
                if let Err(e) = window.hide() {
                    error!(error = %e, "Failed to hide window on close-requested");
                } else {
                    api.prevent_close();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            connect_daemon,
            get_state_snapshot,
            set_mic_mute,
            set_mic_gain,
            set_headphone_volume,
            get_mic_chain,
            set_effect_bypass,
            set_effect_param,
            load_effect_preset,
            reset_effect_chain,
        ])
        .run(tauri::generate_context!())
        .unwrap_or_else(|e| {
            error!(error = %e, "tauri runtime exited with error");
        });
}

/// Show/hide/toggle the main window. `None` toggles based on current
/// visibility; `Some(true)` forces show, `Some(false)` forces hide.
fn toggle_window(app: &tauri::AppHandle, force: Option<bool>) {
    let Some(window) = app.get_webview_window("main") else {
        warn!("toggle_window: no main window present");
        return;
    };
    let visible = window.is_visible().unwrap_or(false);
    let target = force.unwrap_or(!visible);
    let result = if target {
        window.show().and_then(|()| window.set_focus())
    } else {
        window.hide()
    };
    if let Err(e) = result {
        error!(error = %e, "toggle_window failed");
    }
}
