mod cookies;
mod download;
mod queue;
mod server;
mod state;

use std::sync::Arc;
use std::time::Duration;

use tauri::{AppHandle, Manager, State};

use state::{AppState, DownloadView, Settings};

type Shared<'a> = State<'a, Arc<AppState>>;

#[tauri::command]
async fn add_download(app: AppHandle, state: Shared<'_>, url: String) -> Result<String, String> {
    let id = state.add(&app, &url).await?;
    state::pump(&app, &state);
    Ok(id)
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

#[tauri::command]
fn update_settings(app: AppHandle, state: Shared<'_>, settings: Settings) {
    state.set_settings(settings);
    state::pump(&app, &state);
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let handle = app.handle().clone();
            let data_dir = handle.path().app_data_dir()?;
            let config_dir = handle.path().app_config_dir()?;

            let state = Arc::new(AppState::new(data_dir, config_dir).map_err(std::io::Error::other)?);
            app.manage(Arc::clone(&state));

            // Localhost bridge the browser extension POSTs captured sessions to.
            server::start(handle.clone(), Arc::clone(&state));

            // Auto-resume what was interrupted, but not immediately: on a cold
            // start the network stack may not be up yet, and firing straight
            // into a retry ladder would burn attempts on a link that is about
            // to work.
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(Duration::from_secs(3)).await;
                state::pump(&handle, &state);
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            add_download,
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
