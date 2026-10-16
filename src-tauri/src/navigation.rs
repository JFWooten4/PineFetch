//! Native navigation requests survive frontend startup and WebView reloads.
use serde::Serialize;
use std::sync::Mutex;
#[cfg(target_os = "macos")]
use tauri::{Emitter, Manager};

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) enum AppView {
    Download,
    History,
    LinkDump,
    Settings,
}

#[derive(Clone, Copy, Serialize)]
pub(crate) struct NavigationRequest {
    revision: u64,
    view: AppView,
}

#[derive(Default)]
pub(crate) struct NavigationState(Mutex<Option<NavigationRequest>>);

#[tauri::command]
pub(crate) fn get_navigation_request(
    state: tauri::State<'_, NavigationState>,
) -> Result<Option<NavigationRequest>, String> {
    state
        .0
        .lock()
        .map(|request| *request)
        .map_err(|_| "Navigation state unavailable".into())
}

#[cfg(target_os = "macos")]
pub(crate) fn open_view(app: &tauri::AppHandle, view: AppView) -> Result<(), String> {
    let state = app.state::<NavigationState>();
    let request = {
        let mut current = state.0.lock().map_err(|_| "Navigation state unavailable")?;
        let request = NavigationRequest {
            revision: current.map_or(1, |previous| previous.revision + 1),
            view,
        };
        *current = Some(request);
        request
    };
    // Persist first: a click during startup is restored after listeners are ready.
    app.emit_to("main", "navigation:requested", request)
        .map_err(|err| err.to_string())?;
    let window = app
        .get_webview_window("main")
        .ok_or("Main window unavailable")?;
    app.show().map_err(|err| err.to_string())?;
    window.unminimize().map_err(|err| err.to_string())?;
    window.show().map_err(|err| err.to_string())?;
    window.set_focus().map_err(|err| err.to_string())
}
