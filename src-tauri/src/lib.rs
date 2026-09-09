mod cookies;
mod download;
mod queue;
mod server;
mod state;
mod throttle;
mod ytdlp;

use std::sync::Arc;
use std::time::Duration;

use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager, State, WindowEvent,
};

use state::{AddOptions, AppState, DownloadView, Settings};

/// Bring the main window back from the tray and focus it.
pub fn show_main(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

type Shared<'a> = State<'a, Arc<AppState>>;

#[tauri::command]
async fn add_download(
    app: AppHandle,
    state: Shared<'_>,
    url: String,
    options: Option<AddOptions>,
) -> Result<String, String> {
    let opts = options.unwrap_or_default();
    let start = opts.start != Some(false);
    let id = state.add_with_session(&app, &url, None, false, opts).await?;
    // "Download later" was already parked as Paused; don't start it.
    if start {
        state::pump(&app, &state);
    } else {
        state::emit_queue(&app, &state);
    }
    Ok(id)
}

/// Confirm a request the extension parked: add it with the chosen options,
/// replaying the browser session captured at capture time.
#[tauri::command]
async fn add_pending(
    app: AppHandle,
    state: Shared<'_>,
    token: String,
    options: Option<AddOptions>,
) -> Result<String, String> {
    let pending = state
        .take_pending(&token)
        .ok_or_else(|| "this request expired".to_string())?;
    let opts = options.unwrap_or_default();
    let start = opts.start != Some(false);
    let id = state
        .add_with_session(&app, &pending.url, pending.session, pending.force_video, opts)
        .await?;
    if start {
        state::pump(&app, &state);
    } else {
        state::emit_queue(&app, &state);
    }
    Ok(id)
}

/// Drop a parked request the user cancelled.
#[tauri::command]
fn cancel_pending(state: Shared<'_>, token: String) {
    state.take_pending(&token);
}

/// The folder a download would land in by default, for the add dialog to show.
#[tauri::command]
fn get_download_dir(app: AppHandle, state: Shared<'_>) -> Result<String, String> {
    state.download_dir(&app).map(|p| p.display().to_string())
}

/// Returns the URLs that were skipped, so the UI can say what did not make it
/// rather than silently dropping lines.
#[tauri::command]
async fn import_urls(
    app: AppHandle,
    state: Shared<'_>,
    text: String,
) -> Result<Vec<String>, String> {
    let mut skipped = Vec::new();

    for line in text.lines() {
        let url = line.trim();
        if url.is_empty() || url.starts_with('#') {
            continue;
        }
        if let Err(e) = state.add(&app, url).await {
            skipped.push(format!("{url}: {e}"));
        }
    }

    state::pump(&app, &state);
    Ok(skipped)
}

#[tauri::command]
fn is_duplicate(state: Shared<'_>, url: String) -> bool {
    state.has_url(&url)
}

#[tauri::command]
fn pause_download(app: AppHandle, state: Shared<'_>, id: String) {
    state.pause(&id);
    state::pump(&app, &state);
}

#[tauri::command]
fn resume_download(app: AppHandle, state: Shared<'_>, id: String) {
    state.resume(&id);
    state::pump(&app, &state);
}

#[tauri::command]
fn cancel_download(app: AppHandle, state: Shared<'_>, id: String) {
    state.cancel(&id);
    state::pump(&app, &state);
}

/// Retry is resume: the saved offsets still apply, so a failed transfer picks
/// up where it stopped instead of starting over.
#[tauri::command]
fn retry_download(app: AppHandle, state: Shared<'_>, id: String) {
    state.resume(&id);
    state::pump(&app, &state);
}

/// Remove an entry. `delete_file` also erases the file from disk.
#[tauri::command]
fn remove_download(app: AppHandle, state: Shared<'_>, id: String, delete_file: bool) {
    state.remove(&id, delete_file);
    state::emit_queue(&app, &state);
}

#[tauri::command]
fn pause_all(app: AppHandle, state: Shared<'_>) {
    state.pause_all();
    state::emit_queue(&app, &state);
}

#[tauri::command]
fn get_queue(state: Shared<'_>) -> Vec<DownloadView> {
    state.views()
}

#[tauri::command]
fn clear_history(app: AppHandle, state: Shared<'_>) {
    state.clear_history();
    state::emit_queue(&app, &state);
}

#[tauri::command]
fn get_settings(state: Shared<'_>) -> Settings {
    state.settings()
}

// Async so it runs on the Tokio runtime: `set_settings` rebuilds the bandwidth
// throttle, which spawns a refill task and would panic off-runtime.
#[tauri::command]
async fn update_settings(app: AppHandle, state: Shared<'_>, settings: Settings) -> Result<(), ()> {
    state.set_settings(settings);
    state::pump(&app, &state);
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // Must be registered first: a second launch is intercepted here and
        // focuses the running window instead of starting another process.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            show_main(app);
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            let handle = app.handle().clone();
            let data_dir = handle.path().app_data_dir()?;
            let config_dir = handle.path().app_config_dir()?;

            let state = Arc::new(AppState::new(data_dir, config_dir).map_err(std::io::Error::other)?);
            app.manage(Arc::clone(&state));

            // System tray: left-click restores the window; the menu offers an
            // explicit Show and a real Quit (the window's close button only
            // hides to tray, so Quit is the one way to actually exit).
            let show = MenuItem::with_id(app, "show", "Show fetchd", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &quit])?;

            let quit_state = Arc::clone(&state);
            TrayIconBuilder::with_id("main-tray")
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("fetchd")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(move |app, event| match event.id.as_ref() {
                    "show" => show_main(app),
                    "quit" => {
                        quit_state.shutdown();
                        app.exit(0);
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        show_main(tray.app_handle());
                    }
                })
                .build(app)?;

            // Localhost bridge the browser extension POSTs captured sessions to.
            server::start(handle.clone(), Arc::clone(&state));

            // Auto-resume what was interrupted, but not immediately: on a cold
            // start the network stack may not be up yet, and firing straight
            // into a retry ladder would burn attempts on a link that is about
            // to work.
            tauri::async_runtime::spawn(async move {
                // Apply the saved bandwidth cap now that we are on the runtime
                // (Throttle spawns a refill task).
                state.rebuild_throttle();
                tokio::time::sleep(Duration::from_secs(3)).await;
                state::pump(&handle, &state);
            });

            Ok(())
        })
        // Close button hides to tray instead of quitting, so downloads keep
        // running in the background. Quit is via the tray menu.
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let _ = window.hide();
                api.prevent_close();
            }
        })
        .invoke_handler(tauri::generate_handler![
            add_download,
            add_pending,
            cancel_pending,
            get_download_dir,
            import_urls,
            is_duplicate,
            pause_download,
            resume_download,
            cancel_download,
            retry_download,
            remove_download,
            pause_all,
            get_queue,
            clear_history,
            get_settings,
            update_settings,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
