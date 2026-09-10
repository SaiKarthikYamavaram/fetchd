mod cookies;
mod download;
mod queue;
mod server;
mod state;
mod thumbs;
mod throttle;
mod ytdlp;

use std::sync::Arc;
use std::time::Duration;

use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
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
fn rename_download(app: AppHandle, state: Shared<'_>, id: String, name: String) -> Result<(), String> {
    state.rename(&id, &name)?;
    state::emit_queue(&app, &state);
    Ok(())
}

#[tauri::command]
fn bulk_action(app: AppHandle, state: Shared<'_>, ids: Vec<String>, action: state::BulkAction) {
    state.bulk(&ids, action);
    // `pump` starts anything the removals freed a slot for, and emits.
    state::pump(&app, &state);
}

#[tauri::command]
fn pause_all(app: AppHandle, state: Shared<'_>) {
    state.pause_all();
    state::emit_queue(&app, &state);
}

/// A poster frame for a finished video, as a `data:` URI.
///
/// Returned inline rather than as a file path: the webview cannot read an
/// arbitrary local file without opening the asset protocol to the whole disk,
/// and a 320px JPEG is a few KB. `None` covers everything that is not a video,
/// not finished, or that ffmpeg could not read — the row falls back to its
/// type icon, which is a perfectly good answer.
#[tauri::command]
async fn video_thumbnail(state: Shared<'_>, id: String) -> Result<Option<String>, String> {
    let Some(path) = state.finished_file(&id) else {
        return Ok(None);
    };
    if !thumbs::is_video_file(&path) {
        return Ok(None);
    }

    let cached = thumbs::cache_path(&state.thumb_dir(), &id);
    // Re-extracting on every render would launch a process per row per paint.
    if tokio::fs::metadata(&cached).await.map(|m| m.len() > 0).unwrap_or(false) {
        return Ok(encode_thumb(&cached).await);
    }

    let secs = thumbs::duration("ffprobe", &path).await;
    match thumbs::extract("ffmpeg", &path, &cached, secs).await {
        Some(_) => Ok(encode_thumb(&cached).await),
        None => Ok(None),
    }
}

async fn encode_thumb(path: &std::path::Path) -> Option<String> {
    use base64::Engine;
    let bytes = tokio::fs::read(path).await.ok()?;
    Some(format!(
        "data:image/jpeg;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ))
}

/// Put the autostart entry in whatever state the settings ask for.
///
/// Reconciled rather than toggled: the entry is a file on disk that the user
/// (or another app, or a reinstall) can change behind our back, so the
/// setting is the intent and this makes the disk match it.
fn apply_autostart(app: &AppHandle, want: bool) {
    use tauri_plugin_autostart::ManagerExt;

    let manager = app.autolaunch();
    let enabled = manager.is_enabled().unwrap_or(false);
    if enabled == want {
        return;
    }
    let result = if want { manager.enable() } else { manager.disable() };
    if let Err(e) = result {
        // Not fatal: a desktop without an autostart directory, or a sandbox
        // that forbids writing one, should not break saving settings.
        eprintln!("fetchd: could not update the autostart entry: {e}");
    }
}

#[tauri::command]
fn resume_all(app: AppHandle, state: Shared<'_>) {
    state.resume_all();
    state::pump(&app, &state);
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
    let want_autostart = settings.start_on_login;
    state.set_settings(settings);
    apply_autostart(&app, want_autostart);
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
        // Remembers the window's size and position across launches. Without
        // it every launch reopens at the configured default, which is smaller
        // than the add dialog needs.
        .plugin(tauri_plugin_window_state::Builder::default().build())
        // `--hidden` is what the autostart entry passes, so a login launch can
        // go straight to the tray while a launcher launch shows itself.
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--hidden"]),
        ))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            let handle = app.handle().clone();
            let data_dir = handle.path().app_data_dir()?;
            let config_dir = handle.path().app_config_dir()?;

            let state = Arc::new(AppState::new(data_dir, config_dir).map_err(std::io::Error::other)?);
            app.manage(Arc::clone(&state));

            // The autostart entry can drift from the setting — a reinstall, or
            // the user editing their desktop's startup list — so make the disk
            // match the intent on every launch.
            apply_autostart(&handle, state.settings().start_on_login);

            // A login launch passes --hidden; start in the tray rather than
            // opening a window the user did not ask for. Ignored unless the
            // setting agrees, so a stale autostart entry cannot silently
            // swallow a manual launch.
            let hidden = std::env::args().any(|a| a == "--hidden")
                && state.settings().start_minimised;
            if hidden {
                if let Some(window) = handle.get_webview_window("main") {
                    let _ = window.hide();
                }
            }

            // System tray: left-click restores the window; the menu offers an
            // explicit Show and a real Quit (the window's close button only
            // hides to tray, so Quit is the one way to actually exit).
            //
            // Pause all and Resume all are here too: the app spends most of a
            // long download minimised, and reaching for the queue's brakes
            // should not mean raising the window first.
            let show = MenuItem::with_id(app, "show", "Show fetchd", true, None::<&str>)?;
            let pause = MenuItem::with_id(app, "pause_all", "Pause all", true, None::<&str>)?;
            let resume = MenuItem::with_id(app, "resume_all", "Resume all", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let sep = PredefinedMenuItem::separator(app)?;
            let menu = Menu::with_items(app, &[&show, &sep, &pause, &resume, &sep, &quit])?;

            let tray_state = Arc::clone(&state);
            let quit_state = Arc::clone(&state);
            TrayIconBuilder::with_id("main-tray")
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("fetchd")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(move |app, event| match event.id.as_ref() {
                    "show" => show_main(app),
                    "pause_all" => {
                        tray_state.pause_all();
                        state::emit_queue(app, &tray_state);
                    }
                    "resume_all" => {
                        tray_state.resume_all();
                        state::pump(app, &tray_state);
                    }
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
            // Scheduler: reopen the queue when the window starts, and stop
            // transfers when it ends. Checked once a minute — the window has
            // minute resolution, so anything finer is wasted wakeups.
            let sched_state = Arc::clone(&state);
            let sched_app = handle.clone();
            tauri::async_runtime::spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_secs(60));
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                let mut was_open = true;
                loop {
                    interval.tick().await;
                    let open = state::within_schedule(&sched_state.settings());
                    if open {
                        state::pump(&sched_app, &sched_state);
                    } else if was_open {
                        sched_state.suspend_for_schedule();
                        state::emit_queue(&sched_app, &sched_state);
                    }
                    was_open = open;
                }
            });

            // One flusher for the whole queue: active downloads only mark it
            // dirty, so progress is persisted with a single write per tick
            // rather than one per download.
            let flush_state = Arc::clone(&state);
            tauri::async_runtime::spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_secs(2));
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    interval.tick().await;
                    flush_state.flush_if_dirty();
                }
            });

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
                // With background running on, the close button hides to the
                // tray and transfers carry on. With it off, close means quit —
                // and quitting has to go through `shutdown` so in-flight
                // progress is checkpointed rather than lost.
                let background = window
                    .app_handle()
                    .try_state::<Arc<AppState>>()
                    .map(|s| s.settings().run_in_background)
                    .unwrap_or(true);

                if background {
                    let _ = window.hide();
                    api.prevent_close();
                } else if let Some(state) = window.app_handle().try_state::<Arc<AppState>>() {
                    state.shutdown();
                }
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
            rename_download,
            bulk_action,
            pause_all,
            resume_all,
            video_thumbnail,
            get_queue,
            clear_history,
            get_settings,
            update_settings,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
