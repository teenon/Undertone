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
    ensure_connected(&state).await
}

/// Idempotent connect: opens a new `IpcClient` if the slot is empty,
/// no-op otherwise. Held lock is dropped before returning so callers
/// (including the retry path in `call`) can immediately re-acquire.
async fn ensure_connected(state: &State<'_, DaemonClient>) -> Result<(), String> {
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

/// Send one IPC request. Self-healing: every call first ensures
/// there's a live connection (re-opening one if the slot is empty
/// — typical case is the previous reconnect raced the daemon
/// restart and left us disconnected). If the request *itself* then
/// fails with a connection-shaped error we drop the cached client,
/// reconnect, and retry once. Other errors propagate as-is.
async fn call(
    state: &State<'_, DaemonClient>,
    method: Method,
) -> Result<serde_json::Value, String> {
    // Best-effort up-front connect. If the socket isn't ready yet
    // (daemon mid-restart) we still try the request, which produces
    // a clearer error and the next 500 ms poll re-enters here.
    let _ = ensure_connected(state).await;
    match call_once(state, &method).await {
        Ok(v) => Ok(v),
        Err(e) if looks_like_dead_connection(&e) => {
            warn!(error = %e, "daemon connection looks dead; dropping and reconnecting");
            *state.0.lock().await = None;
            ensure_connected(state).await?;
            call_once(state, &method).await
        }
        Err(e) => Err(e),
    }
}

async fn call_once(
    state: &State<'_, DaemonClient>,
    method: &Method,
) -> Result<serde_json::Value, String> {
    let guard = state.0.lock().await;
    let client = guard
        .as_ref()
        .ok_or_else(|| "daemon not connected — call connect_daemon first".to_string())?;
    let response = client
        .request(method.clone())
        .await
        .map_err(|e| format!("ipc error: {e}"))?;
    response.result.map_err(|e| format!("daemon error {}: {}", e.code, e.message))
}

/// Heuristic for "this error means the socket is gone, retry from
/// scratch will probably work". Matches the strings produced by
/// `IpcError`'s `Display` impl plus the underlying `io::Error`
/// kinds we observe when the daemon is restarted.
fn looks_like_dead_connection(err: &str) -> bool {
    let needles = [
        "Broken pipe",
        "Connection reset",
        "channel closed",
        "Channel closed",
        "unexpected end of file",
        "Unexpected EOF",
        "not connected",       // own "daemon not connected" sentinel
        "Connection refused",  // socket exists but daemon not yet listening
        "No such file",        // socket file gone (daemon mid-restart)
    ];
    needles.iter().any(|n| err.contains(n))
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
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            // A second launch just focuses the existing window rather
            // than starting a new process. Prevents duplicate daemon
            // connections and orphaned tray icons when the user clicks
            // the app icon while it's already autostart-running.
            toggle_window(app, Some(true));
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--minimized"]),
        ))
        .setup(|app| {
            app.manage(DaemonClient::new());

            // The main window is configured with `visible: false` so we
            // can decide whether to show it based on launch context.
            // Autostart (from the generated .desktop) passes `--minimized`
            // so we stay hidden in the tray; any other launch (manual,
            // menu) shows the window normally.
            let minimized = std::env::args().any(|a| a == "--minimized");
            if !minimized {
                if let Some(window) = app.get_webview_window("main") {
                    if let Err(e) = window.show() {
                        warn!(error = %e, "failed to show main window on startup");
                    }
                }
            }

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
