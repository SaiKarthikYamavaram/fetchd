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

/// How long an unanswered add-dialog request is kept before being swept.
const PENDING_TTL_SECS: u64 = 60 * 30;

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
    /// Path to the yt-dlp binary. Empty falls back to `yt-dlp` on PATH.
    #[serde(default)]
    pub ytdlp_path: String,
    /// Browser to read cookies from for yt-dlp (`--cookies-from-browser`),
    /// e.g. "brave". Empty uses the manual cookies file, if any.
    #[serde(default)]
    pub cookies_browser: String,
    /// yt-dlp quality: "best" (default), a max height ("2160".."480"), or
    /// "audio" (extract to mp3).
    #[serde(default)]
    pub video_quality: String,
    /// Proxy URL for every download, e.g. "http://host:8080" or
    /// "socks5://host:1080". Empty means direct.
    #[serde(default)]
    pub proxy: String,
    /// Sort finished downloads into per-type sub-folders (Video, Audio, ...)
    /// of the download folder. Ignored when a location is picked per download.
    #[serde(default)]
    pub categorize: bool,
    /// Only transfer inside a daily time window.
    #[serde(default)]
    pub schedule_enabled: bool,
    /// Window bounds as "HH:MM" local time. A stop earlier than the start means
    /// the window runs over midnight (e.g. 23:00-06:00).
    #[serde(default)]
    pub schedule_start: String,
    #[serde(default)]
    pub schedule_stop: String,
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
            ytdlp_path: String::new(),
            cookies_browser: String::new(),
            video_quality: "best".into(),
            proxy: String::new(),
            categorize: false,
            schedule_enabled: false,
            schedule_start: "01:00".into(),
            schedule_stop: "07:00".into(),
        }
    }
}

/// Per-download choices from the add dialog. All optional: omitted fields fall
/// back to the global settings.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AddOptions {
    /// Save location for this download only.
    pub dir: Option<String>,
    /// yt-dlp quality override ("best", "1080", "audio", ...).
    pub quality: Option<String>,
    /// Filename to save as, instead of the one the server or the video title
    /// suggests. An extension is added from the source when omitted.
    pub name: Option<String>,
    /// Start immediately (default), or add it paused for later.
    pub start: Option<bool>,
}

/// A download the extension captured but the user has not confirmed yet.
/// Held here so the browser session (cookies/UA/referer) survives until the
/// add dialog is answered.
#[derive(Debug, Clone)]
pub struct PendingAdd {
    pub url: String,
    pub session: Option<Session>,
    pub force_video: bool,
    /// When it was parked, so abandoned requests can be swept.
    pub added_at: u64,
}

/// Emitted to the frontend to open the add dialog for a captured URL.
#[derive(Debug, Clone, Serialize)]
pub struct ConfirmRequest {
    pub token: String,
    pub url: String,
    pub video: bool,
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
    /// Remote thumbnail URL for a preview, if any.
    pub thumbnail: Option<String>,
    pub engine: String,
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
    /// Captured-but-unconfirmed adds, keyed by token (see `PendingAdd`).
    pending: Mutex<HashMap<String, PendingAdd>>,
    /// Set when progress moved; a single flusher persists it (see `mark_dirty`).
    dirty: std::sync::atomic::AtomicBool,
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
            pending: Mutex::new(HashMap::new()),
            dirty: std::sync::atomic::AtomicBool::new(false),
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
        if let Some(mut session) = captured {
            // The extension knows nothing about the proxy setting.
            let proxy = self.settings().proxy;
            session.proxy = Some(proxy).filter(|p| !p.is_empty());
            return session;
        }

        let settings = self.settings();
        let mut session = Session::with_agent(settings.user_agent.clone());
        session.proxy = Some(settings.proxy.clone()).filter(|p| !p.is_empty());

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

    pub fn set_settings(&self, mut settings: Settings) {
        // Settings arrive over IPC; clamp them rather than trusting the UI.
        // An out-of-range concurrency would have `pump` spawn that many
        // transfers at once.
        settings.max_concurrent = settings.max_concurrent.clamp(1, 16);
        settings.segments = settings.segments.clamp(1, download::MAX_SEGMENTS);
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
                thumbnail: d.plan.thumbnail.clone(),
                engine: match d.plan.engine {
                    download::Engine::YtDlp => "ytdlp".into(),
                    download::Engine::Http => "http".into(),
                },
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

    /// Update only the displayed name (keeping the directory) while a yt-dlp
    /// download is still running, so the row shows the real title instead of the
    /// "…video" placeholder before it finishes.
    fn set_display_name(&self, id: &str, name: &str) {
        let changed = {
            let mut queue = self.queue.lock().unwrap();
            match queue.iter_mut().find(|d| d.id == id) {
                Some(d) => {
                    let next = d.plan.final_path.with_file_name(name);
                    let changed = d.plan.final_path != next;
                    d.plan.final_path = next;
                    changed
                }
                None => false,
            }
        };
        if changed {
            self.save_queue();
        }
    }

    /// Set the resolved output path once yt-dlp reports it (its filename is only
    /// known after resolution), and record the final size from disk so the
    /// completed row shows a real size instead of "unknown".
    fn set_final_path(&self, id: &str, path: PathBuf) {
        // Only a real file has a meaningful length. If yt-dlp errored before
        // naming an output, `path` falls back to the directory, whose metadata
        // len() is the inode block size (4096) — never record that as the size.
        let size = std::fs::metadata(&path)
            .ok()
            .filter(|m| m.is_file())
            .map(|m| m.len());
        let mut queue = self.queue.lock().unwrap();
        if let Some(d) = queue.iter_mut().find(|d| d.id == id) {
            d.plan.final_path = path;
            if let Some(size) = size {
                d.plan.total = Some(size);
                // yt-dlp keeps no per-segment counters, so mark it fully done
                // here or the completed row would read 0 / total.
                d.done = vec![size];
            }
        }
    }

    /// Record the total size once yt-dlp reports it, never shrinking (yt-dlp's
    /// per-phase totals would otherwise flip video→audio size).
    fn set_total(&self, id: &str, total: u64) {
        let changed = {
            let mut queue = self.queue.lock().unwrap();
            if let Some(d) = queue.iter_mut().find(|d| d.id == id) {
                if d.plan.total.is_none_or(|t| total > t) {
                    d.plan.total = Some(total);
                    true
                } else {
                    false
                }
            } else {
                false
            }
        };
        if changed {
            self.save_queue();
        }
    }

    /// Note that progress changed without writing to disk.
    ///
    /// Every active download used to persist the whole queue (with an fsync)
    /// on its own 2s checkpoint, so N downloads meant N full writes per tick.
    /// They now just mark the queue dirty and one flusher does a single write.
    pub fn mark_dirty(&self) {
        self.dirty.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Persist once if anything marked the queue dirty since the last flush.
    pub fn flush_if_dirty(&self) {
        if self.dirty.swap(false, std::sync::atomic::Ordering::Relaxed) {
            self.save_queue();
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

    /// Park a captured request until the user answers the add dialog. Returns
    /// the token the frontend sends back.
    pub fn stash_pending(&self, pending: PendingAdd) -> String {
        let token = format!("p{}", self.gen.fetch_add(1, Ordering::Relaxed));
        let mut map = self.pending.lock().unwrap();
        // A dialog closed by shutting the window never answers, so sweep
        // anything abandoned rather than growing forever.
        let now = queue::now_secs();
        map.retain(|_, p| now.saturating_sub(p.added_at) < PENDING_TTL_SECS);
        map.insert(token.clone(), pending);
        token
    }

    pub fn take_pending(&self, token: &str) -> Option<PendingAdd> {
        self.pending.lock().unwrap().remove(token)
    }

    /// Add a URL typed into the app. Uses the manual cookies.txt session, if
    /// any.
    pub async fn add(&self, app: &AppHandle, url: &str) -> Result<String, String> {
        self.add_with_session(app, url, None, false, AddOptions::default()).await
    }

    /// Add a URL, optionally with a session the browser extension captured for
    /// it, and optionally forcing the yt-dlp video engine. The session is
    /// stored on the entry so a later resume replays the same cookie/UA/referer.
    pub async fn add_with_session(
        &self,
        app: &AppHandle,
        url: &str,
        captured: Option<Session>,
        force_video: bool,
        opts: AddOptions,
    ) -> Result<String, String> {
        // A per-download location from the add dialog wins over the default.
        let explicit_dir = opts.dir.as_deref().filter(|d| !d.trim().is_empty());
        let dir = match explicit_dir {
            Some(d) => PathBuf::from(d),
            None => self.download_dir(app)?,
        };
        let custom_name = opts.name.as_deref().map(str::trim).filter(|n| !n.is_empty());
        let settings = self.settings();
        // A folder chosen for this download is taken literally; category
        // sorting only shapes the default location.
        let categorize = settings.categorize && explicit_dir.is_none();
        let session = self.session_for(url, captured.clone());

        let plan = if force_video
            || crate::ytdlp::is_video_site(url)
            || crate::ytdlp::is_stream_manifest(url)
        {
            // Resolve title + thumbnail up front (with a timeout) so the row
            // shows the real name and a preview immediately, not a placeholder.
            let ytdlp = if settings.ytdlp_path.is_empty() { "yt-dlp".to_string() } else { settings.ytdlp_path.clone() };
            let cookies = crate::ytdlp::Cookies {
                file: settings.cookies_file.clone(),
                browser: if settings.cookies_browser.is_empty() { None } else { Some(settings.cookies_browser.clone()) },
            };
            let proxy = Some(settings.proxy.as_str()).filter(|p| !p.is_empty());
            let (title, thumbnail) =
                crate::ytdlp::resolve_meta(&ytdlp, url, &cookies, proxy).await;
            // yt-dlp names the file itself, so the category is decided by the
            // chosen quality rather than an extension.
            let dir = if categorize {
                let bucket = if opts.quality.as_deref().unwrap_or(&settings.video_quality) == "audio" {
                    "Audio"
                } else {
                    "Video"
                };
                dir.join(bucket)
            } else {
                dir
            };
            download::video_plan(url, &dir, title, thumbnail, custom_name)?
        } else {
            let (client, _) = self.clients_for(&session)?;
            download::prepare(&client, url, &dir, settings.segments, categorize, custom_name).await?
        };

        let id = self.next_id();
        let mut entry = Download::new(id.clone(), plan);
        // Only persist a non-default session; a plain download carries none.
        if session.cookie.is_some() || session.referer.is_some() {
            entry.session = Some(session);
        }
        entry.quality = opts.quality.filter(|q| !q.trim().is_empty());
        entry.name = custom_name.map(str::to_string);
        // "Download later" parks the entry as Paused so `pump` skips it until
        // the user hits Resume.
        if opts.start == Some(false) {
            entry.status = Status::Paused;
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

    /// Stop and discard: the partial file(s) are deleted and the entry removed.
    pub fn cancel(&self, id: &str) {
        let plan = {
            let queue = self.queue.lock().unwrap();
            queue.iter().find(|d| d.id == id).map(|d| d.plan.clone())
        };

        if let Some(active) = self.active.lock().unwrap().remove(id) {
            active.token.cancel();
        }
        self.queue.lock().unwrap().retain(|d| d.id != id);

        if let Some(plan) = plan {
            delete_artifacts(&plan, true);
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
        let entry = {
            let queue = self.queue.lock().unwrap();
            queue.iter().find(|d| d.id == id).map(|d| (d.plan.clone(), d.status))
        };

        if let Some(active) = self.active.lock().unwrap().remove(id) {
            active.token.cancel();
        }
        self.queue.lock().unwrap().retain(|d| d.id != id);

        if let Some((plan, status)) = entry {
            if delete_file {
                delete_artifacts(&plan, false);
            } else if status != Status::Completed {
                // Keep a finished file, but a partial is useless once its queue
                // entry is gone — and `prepare` reserves the `.part` up front,
                // so even a never-started download has one to clean up.
                delete_artifacts(&plan, true);
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

    /// Clean shutdown on quit: cancel all active transfer tasks, persist their
    /// latest durable offsets to queue.json, and leave their entries in place so
    /// `reconcile_on_launch` marks them `Interrupted` and auto-resumes them next time.
    pub fn shutdown(&self) {
        let active_entries: Vec<(String, Active)> = {
            let mut active = self.active.lock().unwrap();
            active.drain().collect()
        };
        for (id, active) in active_entries {
            active.token.cancel();
            self.record_progress(&id, active.progress.snapshot());
        }
        self.dirty.store(false, std::sync::atomic::Ordering::Relaxed);
        self.save_queue();
    }

    /// Stop active transfers because the scheduled window closed.
    ///
    /// They go back to `Queued`, not `Paused`: pausing is a user decision that
    /// must survive, whereas these should resume by themselves when the window
    /// reopens. Partial files are kept, so this costs nothing but a reconnect.
    pub fn suspend_for_schedule(&self) {
        let active: Vec<(String, Active)> = {
            let mut map = self.active.lock().unwrap();
            map.drain().collect()
        };
        if active.is_empty() {
            return;
        }
        for (id, a) in active {
            a.token.cancel();
            self.record_progress(&id, a.progress.snapshot());
            self.set_status(&id, Status::Queued, None);
        }
        self.save_queue();
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
    let settings = state.settings();
    // Outside the scheduled window nothing new starts; entries stay Queued and
    // the scheduler tick picks them up when the window opens.
    if !within_schedule(&settings) {
        emit_queue(app, state);
        return;
    }
    let max = settings.max_concurrent.max(1);

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

        let result: Result<PathBuf, String> = if plan.engine == download::Engine::YtDlp {
            run_video(
                &app,
                &state,
                &plan,
                &progress,
                &id,
                entry.quality.clone(),
                entry.name.clone(),
                token.clone(),
            )
            .await
        } else {
            run_http(&app, &state, &entry, &plan, &progress, &id, token.clone()).await
        };

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
            Ok(path) => {
                // yt-dlp only knows the real filename once it finishes.
                if plan.engine == download::Engine::YtDlp {
                    state.set_final_path(&id, path.clone());
                }
                state.set_status(&id, Status::Completed, None);
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| entry.filename());
                notify_complete(&app, &name);
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

/// The HTTP-engine run: periodic checkpoint task, per-session clients, shared
/// throttle. Returns the final path (already known from the plan).
async fn run_http(
    app: &AppHandle,
    state: &Arc<AppState>,
    entry: &Download,
    plan: &download::DownloadPlan,
    progress: &Progress,
    id: &str,
    token: CancellationToken,
) -> Result<PathBuf, String> {
    // Persist offsets periodically so an unclean stop loses at most a few
    // seconds of progress rather than the whole transfer.
    let checkpoint = {
        let state = Arc::clone(state);
        let id = id.to_string();
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
                        state.mark_dirty();
                    }
                }
            }
        })
    };

    // Prefer the session captured for this entry (extension), falling back to
    // the manual cookies.txt.
    let session = state.session_for(&plan.url, entry.session.clone());
    let (client, segment_client) = match state.clients_for(&session) {
        Ok(pair) => pair,
        Err(e) => {
            checkpoint.abort();
            return Err(e);
        }
    };

    let throttle = state.current_throttle();
    let emitter = app.clone();
    let row_id = id.to_string();
    let result = download::run(
        &client,
        &segment_client,
        plan,
        progress,
        &throttle,
        token,
        move |downloaded, total| {
            let _ = emitter.emit(
                "download://progress",
                ProgressRow { id: row_id.clone(), downloaded, total },
            );
        },
    )
    .await;

    checkpoint.abort();
    result
}

/// The yt-dlp run: no checkpoint, no throttle, no segment clients. Cookies come
/// from the manual file or a configured browser.
async fn run_video(
    app: &AppHandle,
    state: &Arc<AppState>,
    plan: &download::DownloadPlan,
    progress: &Progress,
    id: &str,
    quality_override: Option<String>,
    name_override: Option<String>,
    token: CancellationToken,
) -> Result<PathBuf, String> {
    let settings = state.settings();
    let ytdlp = if settings.ytdlp_path.is_empty() { "yt-dlp".to_string() } else { settings.ytdlp_path.clone() };
    let dir = plan
        .final_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let cookies = crate::ytdlp::Cookies {
        file: settings.cookies_file.clone(),
        browser: if settings.cookies_browser.is_empty() {
            None
        } else {
            Some(settings.cookies_browser.clone())
        },
    };

    // The add dialog's per-download choice wins over the global setting.
    let quality = quality_override
        .filter(|q| !q.is_empty())
        .unwrap_or_else(|| {
            if settings.video_quality.is_empty() {
                "best".to_string()
            } else {
                settings.video_quality.clone()
            }
        });

    let emitter = app.clone();
    let row_id = id.to_string();
    // Keep the shared progress and plan.total updated from yt-dlp ticks, so a
    // pause/interrupt records real bytes (not 0) and views() reflects progress
    // even when queue://changed fires mid-download.
    let prog = progress.clone();
    let prog_state = Arc::clone(state);
    let prog_id = id.to_string();

    // Update the row's title as soon as yt-dlp names a file.
    let name_app = app.clone();
    let name_state = Arc::clone(state);
    let name_id = id.to_string();
    let on_file = move |path: &std::path::Path| {
        if let Some(name) = display_name(path) {
            name_state.set_display_name(&name_id, &name);
            emit_queue(&name_app, &name_state);
        }
    };

    // Honour the global speed cap for video downloads too.
    let limit_kb = if settings.bandwidth_kb > 0 { Some(settings.bandwidth_kb) } else { None };

    // yt-dlp fetches a video stream and then an audio stream, restarting its
    // byte counter for each. Reported raw, the bar would run 0->100% twice.
    // Accumulate finished phases so the figures only ever move forward.
    //
    // On a resume yt-dlp's count already includes the bytes already on disk
    // (verified: it logs "Resuming download at byte N" and reports N+), so no
    // offset is added. But resuming *after* the video stream finished reports
    // only the audio stream, which is below what was persisted — so never
    // report less than the progress we started with.
    let floor = progress.total();
    let base_done = Arc::new(AtomicU64::new(0));
    let last_done = Arc::new(AtomicU64::new(0));
    let base_total = Arc::new(AtomicU64::new(0));
    let last_total = Arc::new(AtomicU64::new(0));

    crate::ytdlp::run(
        &ytdlp,
        &plan.url,
        &dir,
        &cookies,
        &quality,
        name_override.as_deref(),
        limit_kb,
        Some(settings.proxy.as_str()).filter(|p| !p.is_empty()),
        token,
        move |t| {
            // A drop in the reported byte count means yt-dlp moved on to the
            // next stream; bank the phase that just finished.
            let prev = last_done.swap(t.downloaded, Ordering::Relaxed);
            if t.downloaded < prev {
                base_done.fetch_add(prev, Ordering::Relaxed);
                base_total.fetch_add(last_total.load(Ordering::Relaxed), Ordering::Relaxed);
            }
            if let Some(total) = t.total {
                last_total.store(total, Ordering::Relaxed);
            }

            let downloaded = (base_done.load(Ordering::Relaxed) + t.downloaded).max(floor);
            let total = t
                .total
                .map(|tt| (base_total.load(Ordering::Relaxed) + tt).max(downloaded));

            prog.set_absolute(downloaded);
            if let Some(total) = total {
                prog_state.set_total(&prog_id, total);
            }
            let _ = emitter.emit(
                "download://progress",
                ProgressRow { id: row_id.clone(), downloaded, total },
            );
        },
        on_file,
    )
    .await
}

pub fn emit_queue(app: &AppHandle, state: &Arc<AppState>) {
    let _ = app.emit("queue://changed", state.views());
}

/// Delete a download's on-disk files. HTTP has a single `.part` (plus the final
/// file); yt-dlp leaves several intermediates (`.fNNN.<ext>`, `.part`, `.ytdl`)
/// which all share the resolved name stem, so those are cleaned by prefix.
/// `partial_only` (cancel of an unfinished download) skips the final file for
/// the HTTP path, where it does not exist yet.
fn delete_artifacts(plan: &download::DownloadPlan, partial_only: bool) {
    match plan.engine {
        download::Engine::YtDlp => cleanup_by_stem(&plan.final_path),
        download::Engine::Http => {
            let _ = std::fs::remove_file(&plan.part_path);
            if !partial_only {
                let _ = std::fs::remove_file(&plan.final_path);
            }
        }
    }
}

/// Remove every file in `path`'s directory whose name starts with `path`'s file
/// stem — the set of yt-dlp intermediates for one download. Guarded so a short
/// or placeholder stem can't sweep unrelated files.
fn cleanup_by_stem(path: &std::path::Path) {
    let (Some(dir), Some(stem)) = (path.parent(), path.file_stem()) else {
        return;
    };
    let stem = stem.to_string_lossy();
    if stem.len() < 4 {
        let _ = std::fs::remove_file(path);
        return;
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with(&*stem)
                && entry.path().is_file()
            {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

/// Turn a yt-dlp intermediate path into a clean display name: drop a trailing
/// `.part`, and the per-stream `.fNNN` format tag (e.g.
/// `Title [id].f398.mp4.part` → `Title [id].mp4`).
fn display_name(path: &std::path::Path) -> Option<String> {
    let name = path.file_name()?.to_string_lossy();
    let name = name.strip_suffix(".part").unwrap_or(&name);
    let parts: Vec<&str> = name
        .split('.')
        .filter(|seg| !(seg.len() >= 2 && seg.starts_with('f') && seg[1..].chars().all(|c| c.is_ascii_digit())))
        .collect();
    let cleaned = parts.join(".");
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

#[cfg(test)]
mod display_tests {
    use super::display_name;
    use std::path::Path;

    #[test]
    fn cleans_ytdlp_intermediate_names() {
        assert_eq!(
            display_name(Path::new("/d/Big Buck Bunny [id].f398.mp4.part")).as_deref(),
            Some("Big Buck Bunny [id].mp4")
        );
        assert_eq!(
            display_name(Path::new("/d/Song [id].mp3")).as_deref(),
            Some("Song [id].mp3")
        );
        // No false-positive on a normal name component.
        assert_eq!(
            display_name(Path::new("/d/final.mkv")).as_deref(),
            Some("final.mkv")
        );
    }
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
            engine: crate::download::Engine::Http,
            thumbnail: None,
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

    fn sched(start: &str, stop: &str) -> Settings {
        Settings {
            schedule_enabled: true,
            schedule_start: start.into(),
            schedule_stop: stop.into(),
            ..Settings::default()
        }
    }

    #[test]
    fn schedule_window_parsing_and_wrap() {
        assert_eq!(parse_hm("07:30"), Some(450));
        assert_eq!(parse_hm(" 23:59 "), Some(1439));
        assert_eq!(parse_hm("24:00"), None);
        assert_eq!(parse_hm("07:60"), None);
        assert_eq!(parse_hm("bogus"), None);

        // Disabled means always allowed.
        assert!(within_schedule(&Settings::default()));

        // Unparseable bounds must not stall the queue forever.
        assert!(within_schedule(&sched("nope", "07:00")));
        // A zero-width window is treated as always on, not never.
        assert!(within_schedule(&sched("03:00", "03:00")));
    }

    /// The wrap case is the one that is easy to get wrong: 23:00-06:00 is a
    /// single overnight window, not an empty one.
    #[test]
    fn schedule_window_boundaries() {
        // Same-day window 09:00-17:00.
        let day = sched("09:00", "17:00");
        for (now, want) in [(0, false), (539, false), (540, true), (1019, true), (1020, false)] {
            assert_eq!(in_window(&day, now), want, "same-day at minute {now}");
        }

        // Overnight window 23:00-06:00.
        let night = sched("23:00", "06:00");
        for (now, want) in [(1379, false), (1380, true), (1439, true), (0, true), (359, true), (360, false)] {
            assert_eq!(in_window(&night, now), want, "overnight at minute {now}");
        }
    }

        /// The add dialog's payload must deserialize exactly as the frontend sends
    /// it; a field-name mismatch here would silently drop the chosen folder.
    #[test]
    fn add_options_deserialize_from_dialog_payload() {
        let full: AddOptions =
            serde_json::from_str(r#"{"dir":"/tmp/x","quality":"1080","start":false}"#).unwrap();
        assert_eq!(full.dir.as_deref(), Some("/tmp/x"));
        assert_eq!(full.quality.as_deref(), Some("1080"));
        assert_eq!(full.start, Some(false));

        // "Use setting" sends nulls; everything falls back to the defaults.
        let nulls: AddOptions =
            serde_json::from_str(r#"{"dir":null,"quality":null,"start":true}"#).unwrap();
        assert!(nulls.dir.is_none() && nulls.quality.is_none());
        assert_eq!(nulls.start, Some(true));

        // Omitted entirely (extension bridge path).
        let empty: AddOptions = serde_json::from_str("{}").unwrap();
        assert!(empty.dir.is_none() && empty.quality.is_none() && empty.start.is_none());
        // Absent `start` must mean "start now".
        assert!(empty.start != Some(false));
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
            engine: crate::download::Engine::Http,
            thumbnail: None,
        };
        state.queue.lock().unwrap().push(Download::new("k".into(), plan));

        // Remove from list keeps the finished file but discards the partial:
        // `prepare` reserves the `.part`, so it would otherwise be orphaned.
        let part = dir.join("keep.bin.part");
        std::fs::write(&part, b"partial").unwrap();
        state.remove("k", false);
        assert!(file.exists(), "remove without delete must keep the file");
        assert!(!part.exists(), "remove must not orphan the .part");
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
            engine: crate::download::Engine::Http,
            thumbnail: None,
        };
        state.queue.lock().unwrap().push(Download::new("k2".into(), plan2));
        state.remove("k2", true);
        assert!(!file.exists(), "remove with delete must erase the file");

        std::fs::remove_dir_all(&dir).ok();
    }
}

/// Minutes since local midnight, or `None` where the platform clock cannot be
/// read (non-unix; the scheduler then behaves as always-open).
#[cfg(unix)]
fn local_minutes() -> Option<u32> {
    // SAFETY: `localtime_r` writes into a zeroed `tm` we own, and `time` takes
    // a null pointer to mean "return the value".
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            return None;
        }
        Some(tm.tm_hour as u32 * 60 + tm.tm_min as u32)
    }
}

#[cfg(not(unix))]
fn local_minutes() -> Option<u32> {
    None
}

/// Parse "HH:MM" into minutes since midnight.
fn parse_hm(value: &str) -> Option<u32> {
    let (h, m) = value.trim().split_once(':')?;
    let h: u32 = h.trim().parse().ok()?;
    let m: u32 = m.trim().parse().ok()?;
    if h > 23 || m > 59 {
        return None;
    }
    Some(h * 60 + m)
}

/// Whether transfers are allowed right now.
///
/// A stop time earlier than the start wraps past midnight, so "23:00-06:00" is
/// a single overnight window rather than an empty one. Anything unparseable
/// leaves downloads running rather than silently stalling the queue.
pub fn within_schedule(settings: &Settings) -> bool {
    if !settings.schedule_enabled {
        return true;
    }
    let Some(now) = local_minutes() else {
        return true;
    };
    in_window(settings, now)
}

/// The window test with the clock injected, so the boundaries are testable.
fn in_window(settings: &Settings, now: u32) -> bool {
    if !settings.schedule_enabled {
        return true;
    }
    let (Some(start), Some(stop)) = (
        parse_hm(&settings.schedule_start),
        parse_hm(&settings.schedule_stop),
    ) else {
        return true;
    };
    if start == stop {
        return true; // degenerate window: treat as always on
    }
    if start < stop {
        now >= start && now < stop
    } else {
        now >= start || now < stop
    }
}
