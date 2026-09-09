//! Shared application state and the queue scheduler.
//!
//! Locking rule for everything here: the `Mutex` guards are held for plain
//! data access only, and never across an `.await`. Parking a task on an
//! executor thread while holding a `std::sync::Mutex` is how this shape
//! deadlocks, and with up to 24 segment tasks plus a ticker plus IPC handlers
//! all touching the queue, it would deadlock reliably.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use reqwest::Client;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::NotificationExt;
use tokio_util::sync::CancellationToken;

use crate::cookies;
use crate::download::{self, Progress, Session};
use crate::queue::{self, Download, Status};
use crate::throttle::Throttle;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub download_dir: Option<PathBuf>,
    pub max_concurrent: usize,
    pub segments: u32,
    pub theme: String,
    /// Netscape `cookies.txt` exported from a browser.
    ///
    /// The only way past an interactive anti-bot challenge: the browser solves
    /// it, and fetchd replays the cookie it earned. Header spoofing and TLS
    /// fingerprint impersonation both fail against a managed challenge.
    pub cookies_file: Option<PathBuf>,
    /// Must match the browser the cookies came from — a `cf_clearance` cookie
    /// is bound to the exact User-Agent that earned it.
    pub user_agent: Option<String>,
    /// Aggregate download speed cap in KB/s across all transfers. 0 or absent
    /// means unlimited.
    #[serde(default)]
    pub bandwidth_kb: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            download_dir: None,
            max_concurrent: 3,
            segments: download::DEFAULT_SEGMENTS,
            theme: "system".into(),
            cookies_file: None,
            user_agent: None,
            bandwidth_kb: 0,
        }
    }
}

/// One row as the frontend sees it.
#[derive(Debug, Clone, Serialize)]
pub struct DownloadView {
    pub id: String,
    pub url: String,
    pub filename: String,
    pub status: Status,
    pub downloaded: u64,
    pub total: Option<u64>,
    pub segments: usize,
    pub error: Option<String>,
    pub path: String,
    // Detail-view fields.
    pub added_at: u64,
    /// Inclusive `[start, end]` byte range per segment.
    pub ranges: Vec<(u64, u64)>,
    /// Bytes done per segment, index-aligned with `ranges`.
    pub done: Vec<u64>,
    pub supports_ranges: bool,
    /// Session captured by the extension, if any (cookie value withheld).
    pub user_agent: Option<String>,
    pub referer: Option<String>,
    pub has_cookie: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProgressRow {
    pub id: String,
    pub downloaded: u64,
    pub total: Option<u64>,
}

/// A transfer currently occupying a slot.
struct Active {
    token: CancellationToken,
    progress: Progress,
    /// Distinguishes successive runs of the same id. A task that has been
    /// superseded (paused then resumed as a new run) sees a different gen here
    /// and must not touch shared state on its way out.
    gen: u64,
}

pub struct AppState {
    queue: Mutex<Vec<Download>>,
    active: Mutex<HashMap<String, Active>>,
    settings: Mutex<Settings>,
    next_id: AtomicU64,
    /// Monotonic run counter; see `Active::gen`.
    gen: AtomicU64,
    /// Shared bandwidth limiter; rebuilt when the cap setting changes.
    throttle: Mutex<Throttle>,
    data_dir: PathBuf,
    config_dir: PathBuf,
}

impl AppState {
    pub fn new(data_dir: PathBuf, config_dir: PathBuf) -> Result<Self, String> {
        let settings: Settings =
            queue::load_json(&config_dir.join("settings.json")).unwrap_or_default();

        let mut downloads: Vec<Download> =
            queue::load_json(&data_dir.join("queue.json")).unwrap_or_default();

        // Apply the restart state table before anything can observe the queue.
        for d in &mut downloads {
            d.status = queue::reconcile_on_launch(d.status);
        }

        let next = downloads
            .iter()
            .filter_map(|d| d.id.parse::<u64>().ok())
            .max()
            .unwrap_or(0);

        Ok(AppState {
            queue: Mutex::new(downloads),
            active: Mutex::new(HashMap::new()),
            settings: Mutex::new(settings),
            next_id: AtomicU64::new(next + 1),
            gen: AtomicU64::new(1),
            // Built unlimited here (this runs off the async runtime, and
            // Throttle spawns a task); `rebuild_throttle` applies the saved cap
            // from a runtime context during setup.
            throttle: Mutex::new(Throttle::unlimited()),
            data_dir,
            config_dir,
        })
    }

    /// Build a client pair carrying whatever cookies apply to this URL.
    ///
    /// The session to use for `url`.
    ///
    /// Precedence: a session captured by the browser extension for this exact
    /// download wins, because it carries the live cookie that just cleared a
    /// challenge. Otherwise fall back to the manual `cookies.txt` matched
    /// against the URL. The extension path is the one that gets past an
    /// interactive challenge; the file is the manual equivalent.
    pub fn session_for(&self, url: &str, captured: Option<Session>) -> Session {
        if let Some(session) = captured {
            return session;
        }

        let settings = self.settings();
        let mut session = Session::with_agent(settings.user_agent.clone());

        if let Some(path) = &settings.cookies_file {
            if let Ok(parsed) = reqwest::Url::parse(url) {
                match cookies::load(path) {
                    Ok(jar) => session.cookie = cookies::header_for(&jar, &parsed),
                    Err(e) => eprintln!("fetchd: {e}"),
                }
            }
        }
        session
    }

    /// Build a client pair from a session. Built per download rather than
    /// shared, because `Cookie` and `Referer` are host-specific.
    pub fn clients_for(&self, session: &Session) -> Result<(Client, Client), String> {
        Ok((
            download::build_client(session)?,
            download::build_segment_client(session)?,
        ))
    }

    pub fn settings(&self) -> Settings {
        self.settings.lock().unwrap().clone()
    }

    pub fn set_settings(&self, settings: Settings) {
        *self.settings.lock().unwrap() = settings;
        self.save_settings();
        self.rebuild_throttle();
    }

    /// Rebuild the shared limiter from the current bandwidth setting. Must be
    /// called from within the async runtime — `Throttle::new` spawns a refill
    /// task. A cap of 0 yields an unlimited (zero-overhead) throttle.
    pub fn rebuild_throttle(&self) {
        let kb = self.settings().bandwidth_kb;
        *self.throttle.lock().unwrap() = Throttle::new(kb);
    }

    fn current_throttle(&self) -> Throttle {
        self.throttle.lock().unwrap().clone()
    }

    pub fn download_dir(&self, app: &AppHandle) -> Result<PathBuf, String> {
        if let Some(dir) = self.settings().download_dir {
            return Ok(dir);
        }
        app.path()
            .download_dir()
            .map_err(|e| format!("cannot locate the downloads folder: {e}"))
    }

    fn next_id(&self) -> String {
        self.next_id.fetch_add(1, Ordering::Relaxed).to_string()
    }

    // -- queue access -------------------------------------------------------

    pub fn views(&self) -> Vec<DownloadView> {
        let active = self.active.lock().unwrap();
        self.queue
            .lock()
            .unwrap()
            .iter()
            .map(|d| DownloadView {
                id: d.id.clone(),
                url: d.url.clone(),
                filename: d.filename(),
                status: d.status,
                // A running transfer's live counters are ahead of what has
                // been persisted, so prefer them when present.
                downloaded: active
                    .get(&d.id)
                    .map(|a| a.progress.total())
                    .unwrap_or_else(|| d.downloaded()),
                total: d.plan.total,
                segments: d.plan.segment_count(),
                error: d.error.clone(),
                path: d.plan.final_path.display().to_string(),
                added_at: d.added_at,
                ranges: d.plan.ranges.clone(),
                // Prefer the live per-segment counters while running.
                done: active
                    .get(&d.id)
                    .map(|a| a.progress.snapshot())
                    .unwrap_or_else(|| d.done.clone()),
                supports_ranges: d.plan.supports_ranges,
                user_agent: d.session.as_ref().map(|s| s.user_agent.clone()),
                referer: d.session.as_ref().and_then(|s| s.referer.clone()),
                has_cookie: d.session.as_ref().is_some_and(|s| s.cookie.is_some()),
            })
            .collect()
    }

    pub fn has_url(&self, url: &str) -> bool {
        self.queue
            .lock()
            .unwrap()
            .iter()
            .any(|d| d.url == url && !d.is_terminal())
    }

    fn set_status(&self, id: &str, status: Status, error: Option<String>) {
        let mut queue = self.queue.lock().unwrap();
        if let Some(d) = queue.iter_mut().find(|d| d.id == id) {
            d.status = status;
            d.error = error;
        }
    }

    fn record_progress(&self, id: &str, done: Vec<u64>) {
        let mut queue = self.queue.lock().unwrap();
        if let Some(d) = queue.iter_mut().find(|d| d.id == id) {
            d.done = done;
        }
    }

    pub fn save_queue(&self) {
        let snapshot = self.queue.lock().unwrap().clone();
        if let Err(e) = queue::save_json(&self.data_dir.join("queue.json"), &snapshot) {
            eprintln!("fetchd: could not save queue: {e}");
        }
    }

    fn save_settings(&self) {
        let snapshot = self.settings.lock().unwrap().clone();
        if let Err(e) = queue::save_json(&self.config_dir.join("settings.json"), &snapshot) {
            eprintln!("fetchd: could not save settings: {e}");
        }
    }

    // -- commands -----------------------------------------------------------

    /// Add a URL typed into the app. Uses the manual cookies.txt session, if
    /// any.
    pub async fn add(&self, app: &AppHandle, url: &str) -> Result<String, String> {
        self.add_with_session(app, url, None).await
    }

    /// Add a URL, optionally with a session the browser extension captured for
    /// it. The session is stored on the entry so a later resume replays the
    /// same cookie/UA/referer.
    pub async fn add_with_session(
        &self,
        app: &AppHandle,
        url: &str,
        captured: Option<Session>,
    ) -> Result<String, String> {
        let dir = self.download_dir(app)?;
        let segments = self.settings().segments;
        let session = self.session_for(url, captured);
        let (client, _) = self.clients_for(&session)?;
        let plan = download::prepare(&client, url, &dir, segments).await?;

        let id = self.next_id();
        let mut entry = Download::new(id.clone(), plan);
        // Only persist a non-default session; a plain download carries none.
        if session.cookie.is_some() || session.referer.is_some() {
            entry.session = Some(session);
        }
        self.queue.lock().unwrap().push(entry);
        self.save_queue();
        Ok(id)
    }

    /// Ask a running transfer to stop, leaving the partial file in place.
    pub fn pause(&self, id: &str) {
        if let Some(active) = self.active.lock().unwrap().remove(id) {
            active.token.cancel();
            self.record_progress(id, active.progress.snapshot());
        }
        self.set_status(id, Status::Paused, None);
        self.save_queue();
    }

    /// Stop and discard: the partial file is deleted and the entry removed.
    pub fn cancel(&self, id: &str) {
        let part = {
            let queue = self.queue.lock().unwrap();
            queue.iter().find(|d| d.id == id).map(|d| d.plan.part_path.clone())
        };

        if let Some(active) = self.active.lock().unwrap().remove(id) {
            active.token.cancel();
        }
        self.queue.lock().unwrap().retain(|d| d.id != id);

        if let Some(part) = part {
            let _ = std::fs::remove_file(part);
        }
        self.save_queue();
    }

    pub fn resume(&self, id: &str) {
        self.set_status(id, Status::Queued, None);
        self.save_queue();
    }

    /// Drop an entry from the list. With `delete_file`, also erase the finished
    /// file (and any leftover `.part`) from disk; otherwise the file is left in
    /// place and only the list entry goes.
    pub fn remove(&self, id: &str, delete_file: bool) {
        let paths = {
            let queue = self.queue.lock().unwrap();
            queue
                .iter()
                .find(|d| d.id == id)
                .map(|d| (d.plan.final_path.clone(), d.plan.part_path.clone()))
        };

        if let Some(active) = self.active.lock().unwrap().remove(id) {
            active.token.cancel();
        }
        self.queue.lock().unwrap().retain(|d| d.id != id);

        if delete_file {
            if let Some((final_path, part)) = paths {
                let _ = std::fs::remove_file(&final_path);
                let _ = std::fs::remove_file(&part);
            }
        }
        self.save_queue();
    }

    pub fn pause_all(&self) {
        let ids: Vec<String> = self
            .queue
            .lock()
            .unwrap()
            .iter()
            .filter(|d| matches!(d.status, Status::Downloading | Status::Queued | Status::Interrupted))
            .map(|d| d.id.clone())
            .collect();
        for id in ids {
            self.pause(&id);
        }
    }

    pub fn clear_history(&self) {
        self.queue.lock().unwrap().retain(|d| !d.is_terminal());
        self.save_queue();
    }

    /// Atomically claim the next waiting entry, or `None` when the slot limit
    /// is reached or nothing is waiting. Marking the chosen entry `Downloading`
    /// happens under the same lock as the check, so two concurrent callers can
    /// never claim the same id. Running slots are counted by status, which
    /// already includes whatever was just claimed.
    fn claim_next(&self, max: usize) -> Option<Download> {
        let mut queue = self.queue.lock().unwrap();
        let running = queue.iter().filter(|d| d.status == Status::Downloading).count();
        if running >= max {
            return None;
        }
        let d = queue
            .iter_mut()
            .find(|d| matches!(d.status, Status::Queued | Status::Interrupted))?;
        d.status = Status::Downloading;
        d.error = None;
        Some(d.clone())
    }
}

/// Start whatever the concurrency limit allows, then emit a batched update.
///
/// This is the single place a transfer is spawned; every command just changes
/// status and calls it. `Interrupted` is included because that is exactly the
/// state an unclean shutdown leaves behind, and the plan says it auto-resumes.
pub fn pump(app: &AppHandle, state: &Arc<AppState>) {
    let max = state.settings().max_concurrent.max(1);

    // The claim in `claim_next` marks each entry Downloading under the queue
    // lock, so a second pump racing this one (e.g. one from a paused task's
    // completion against one from `resume`) cannot pick the same id and spawn a
    // duplicate transfer onto the same `.part`.
    while let Some(entry) = state.claim_next(max) {
        spawn_transfer(app.clone(), Arc::clone(state), entry);
    }

    emit_queue(app, state);
}

fn spawn_transfer(app: AppHandle, state: Arc<AppState>, entry: Download) {
    let id = entry.id.clone();
    let token = CancellationToken::new();

    // Resume from the persisted per-segment offsets. The length check guards
    // against a queue.json written when the plan had a different shape.
    let progress = if entry.done.len() == entry.plan.segment_count() {
        Progress::resumed(&entry.done)
    } else {
        Progress::new(entry.plan.segment_count())
    };

    // If an entry for this id is somehow still active (a prior task not yet
    // torn down), do not start a second writer on the same file. The claim in
    // `pump` already marked it Downloading; leave that task to finish.
    let generation = state.gen.fetch_add(1, Ordering::Relaxed);
    {
        let mut active = state.active.lock().unwrap();
        if active.contains_key(&id) {
            return;
        }
        active.insert(
            id.clone(),
            Active { token: token.clone(), progress: progress.clone(), gen: generation },
        );
    }
    state.save_queue();
    emit_queue(&app, &state);

    // `tauri::async_runtime::spawn`, not `tokio::spawn`: this runs from sync
    // command handlers (pause/resume/cancel/retry) which are NOT on a Tokio
    // worker thread, so a bare `tokio::spawn` there panics "there is no reactor
    // running" and aborts the whole process. The async_runtime handle spawns
    // onto Tauri's runtime from any context.
    tauri::async_runtime::spawn(async move {
        let plan = entry.plan.clone();

        // Persist offsets periodically so an unclean stop loses at most a few
        // seconds of progress rather than the whole transfer.
        let checkpoint = {
            let state = Arc::clone(&state);
            let id = id.clone();
            let progress = progress.clone();
            let token = token.clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(2));
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    tokio::select! {
                        _ = token.cancelled() => break,
                        _ = interval.tick() => {
                            state.record_progress(&id, progress.snapshot());
                            state.save_queue();
                        }
                    }
                }
            })
        };

        // Prefer the session captured for this entry (extension), falling back
        // to the manual cookies.txt. The clients are rebuilt per run rather
        // than reused, so an edited cookies file takes effect on the next
        // resume; a stale extension cookie simply 403s and the user re-sends.
        let session = state.session_for(&plan.url, entry.session.clone());
        let clients = state.clients_for(&session);
        let (client, segment_client) = match clients {
            Ok(pair) => pair,
            Err(e) => {
                state.active.lock().unwrap().remove(&id);
                state.set_status(&id, Status::Failed, Some(e));
                state.save_queue();
                emit_queue(&app, &state);
                return;
            }
        };

        // Snapshot the shared limiter for this run. A later cap change rebuilds
        // the throttle; in-flight transfers keep the one they started with,
        // which is fine — the next resume picks up the new cap.
        let throttle = state.current_throttle();

        let emitter = app.clone();
        let row_id = id.clone();
        let result = download::run(
            &client,
            &segment_client,
            &plan,
            &progress,
            &throttle,
            token.clone(),
            move |downloaded, total| {
                let _ = emitter.emit(
                    "download://progress",
                    ProgressRow { id: row_id.clone(), downloaded, total },
                );
            },
        )
        .await;

        checkpoint.abort();

        // Only act if this task still owns the id. If it was paused and then
        // resumed as a fresh run, a newer task now owns `active[id]`, and this
        // stale task must not remove that entry, overwrite its progress, or
        // flip its status — doing so is what corrupted state on pause/resume.
        let owner = {
            let mut active = state.active.lock().unwrap();
            match active.get(&id) {
                Some(a) if a.gen == generation => {
                    active.remove(&id);
                    true
                }
                _ => false,
            }
        };
        if !owner {
            return;
        }

        state.record_progress(&id, progress.snapshot());

        match result {
            Ok(_) => {
                state.set_status(&id, Status::Completed, None);
                notify_complete(&app, &entry.filename());
            }
            Err(e) if token.is_cancelled() => {
                // A cancelled transfer was paused or removed; whichever
                // command did it already set the right status.
                let _ = e;
            }
            Err(e) => state.set_status(&id, Status::Failed, Some(e)),
        }

        state.save_queue();
        emit_queue(&app, &state);

        // A finished transfer frees a slot.
        pump(&app, &state);
    });
}

pub fn emit_queue(app: &AppHandle, state: &Arc<AppState>) {
    let _ = app.emit("queue://changed", state.views());
}

/// Notify only when the window is hidden/minimized — a visible window already
/// shows the row flip to Completed, so a notification then would be noise.
fn notify_complete(app: &AppHandle, filename: &str) {
    let hidden = app
        .get_webview_window("main")
        .map(|w| !w.is_visible().unwrap_or(true))
        .unwrap_or(true);
    if !hidden {
        return;
    }
    let _ = app
        .notification()
        .builder()
        .title("Download complete")
        .body(format!("{filename} finished downloading."))
        .show();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::download::DownloadPlan;

    fn app() -> AppState {
        let dir = std::env::temp_dir().join(format!("fetchd-state-{}", uid()));
        std::fs::create_dir_all(&dir).unwrap();
        AppState::new(dir.clone(), dir).unwrap()
    }

    fn uid() -> u64 {
        use std::sync::atomic::AtomicU64;
        static N: AtomicU64 = AtomicU64::new(0);
        std::process::id() as u64 * 100_000 + N.fetch_add(1, Ordering::Relaxed)
    }

    fn push(state: &AppState, id: &str) {
        let plan = DownloadPlan {
            url: format!("https://example.com/{id}.bin"),
            final_path: format!("/tmp/{id}.bin").into(),
            part_path: format!("/tmp/{id}.bin.part").into(),
            total: Some(1000),
            supports_ranges: true,
            validator: None,
            ranges: vec![(0, 999)],
        };
        state.queue.lock().unwrap().push(Download::new(id.into(), plan));
    }

    fn status_of(state: &AppState, id: &str) -> Status {
        state.queue.lock().unwrap().iter().find(|d| d.id == id).unwrap().status
    }

    /// The core of the pause/resume ANR fix: claiming is atomic and never hands
    /// the same id to two callers, and it stops at the concurrency limit.
    #[test]
    fn claim_is_atomic_and_bounded() {
        let state = app();
        for i in 0..5 {
            push(&state, &format!("d{i}"));
        }

        // With max = 2, only two entries may be claimed; the rest stay Queued.
        let a = state.claim_next(2).unwrap();
        let b = state.claim_next(2).unwrap();
        assert_ne!(a.id, b.id, "claim handed out the same id twice");
        assert!(state.claim_next(2).is_none(), "claim exceeded the slot limit");

        assert_eq!(status_of(&state, &a.id), Status::Downloading);
        assert_eq!(status_of(&state, &b.id), Status::Downloading);

        // A freed slot lets exactly one more through.
        state.set_status(&a.id, Status::Completed, None);
        let c = state.claim_next(2).unwrap();
        assert_ne!(c.id, b.id);
        assert!(state.claim_next(2).is_none());
    }

    /// A repeated claim of a single entry (what two racing pumps would attempt)
    /// yields it exactly once.
    #[test]
    fn a_single_entry_is_claimed_once() {
        let state = app();
        push(&state, "only");
        assert_eq!(state.claim_next(3).unwrap().id, "only");
        assert!(state.claim_next(3).is_none(), "same entry claimed twice");
    }

    #[test]
    fn interrupted_is_claimable_paused_is_not() {
        let state = app();
        push(&state, "x");
        push(&state, "y");
        state.set_status("x", Status::Interrupted, None);
        state.set_status("y", Status::Paused, None);

        // Interrupted auto-resumes; Paused is a user decision and must not.
        let claimed = state.claim_next(5).unwrap();
        assert_eq!(claimed.id, "x");
        assert!(state.claim_next(5).is_none());
        assert_eq!(status_of(&state, "y"), Status::Paused);
    }

    #[test]
    fn remove_keeps_or_deletes_file() {
        let state = app();
        let dir = std::env::temp_dir().join(format!("fetchd-rm-{}", uid()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("keep.bin");
        std::fs::write(&file, b"data").unwrap();

        let plan = DownloadPlan {
            url: "https://example.com/keep.bin".into(),
            final_path: file.clone(),
            part_path: dir.join("keep.bin.part"),
            total: Some(4),
            supports_ranges: false,
            validator: None,
            ranges: vec![],
        };
        state.queue.lock().unwrap().push(Download::new("k".into(), plan));

        // Remove from list, keep the file.
        state.remove("k", false);
        assert!(file.exists(), "remove without delete must keep the file");
        assert!(state.queue.lock().unwrap().is_empty());

        // Re-add and remove with delete.
        let plan2 = DownloadPlan {
            url: "https://example.com/keep.bin".into(),
            final_path: file.clone(),
            part_path: dir.join("keep.bin.part"),
            total: Some(4),
            supports_ranges: false,
            validator: None,
            ranges: vec![],
        };
        state.queue.lock().unwrap().push(Download::new("k2".into(), plan2));
        state.remove("k2", true);
        assert!(!file.exists(), "remove with delete must erase the file");

        std::fs::remove_dir_all(&dir).ok();
    }
}
