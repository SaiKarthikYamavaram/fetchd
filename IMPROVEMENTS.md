# spool — Codebase Audit, Critical Bug Fixes & Architectural Roadmap

This document catalogs the findings, critical bugs, performance bottlenecks, and architectural refinements discovered during the comprehensive audit of **spool** across the segmented HTTP engine, `yt-dlp` video pipeline, token-bucket bandwidth throttle, local extension bridge, Chrome MV3 extension, and React UI.

---

## 1. Subsystem Audit Overview

| Area | Subsystems & Components | Primary Files |
| :--- | :--- | :--- |
| **HTTP Engine** | Multi-segment concurrent downloads, HTTP/1.1 enforcement, range assembly, and disk pre-allocation. | [`download.rs`](file:///home/logan/Projects/idm/src-tauri/src/download.rs), [`state.rs`](file:///home/logan/Projects/idm/src-tauri/src/state.rs) |
| **Video Engine** | `yt-dlp` subprocess streaming stdout parser, metadata probes, format merging, and browser cookie forwarding. | [`ytdlp.rs`](file:///home/logan/Projects/idm/src-tauri/src/ytdlp.rs), [`state.rs`](file:///home/logan/Projects/idm/src-tauri/src/state.rs) |
| **Bandwidth Throttling** | Global token-bucket rate limiter with periodic refill, permit capping, and `Weak` task lifecycle. | [`throttle.rs`](file:///home/logan/Projects/idm/src-tauri/src/throttle.rs) |
| **Extension Bridge** | Loopback HTTP bridge (`tiny_http` on `127.0.0.1:47831`) handling `/ping` health checks and `/add` capture requests. | [`server.rs`](file:///home/logan/Projects/idm/src-tauri/src/server.rs) |
| **Browser Extension** | Chrome MV3 media sniffer, DOM media crawler, video site detector, and extension popup. | [`extension/background.js`](file:///home/logan/Projects/idm/extension/background.js), [`extension/popup.js`](file:///home/logan/Projects/idm/extension/popup.js), [`extension/common.js`](file:///home/logan/Projects/idm/extension/common.js) |
| **Frontend UI** | React 19 dashboard, video previews, detail properties modal, batch import inspector, and settings panel. | [`App.tsx`](file:///home/logan/Projects/idm/src/App.tsx), [`DetailModal.tsx`](file:///home/logan/Projects/idm/src/components/DetailModal.tsx), [`SettingsView.tsx`](file:///home/logan/Projects/idm/src/components/SettingsView.tsx) |

---

## 2. High-Priority Bugs & Edge Cases

### 🐛 Bug 1: Video Progress Desync & Zeroing on Pause/Interruption
- **Location:** [`state.rs:558-603`](file:///home/logan/Projects/idm/src-tauri/src/state.rs#L558-L603) and [`state.rs:682-741`](file:///home/logan/Projects/idm/src-tauri/src/state.rs#L682-L741)
- **Problem:**
  When `spawn_transfer` runs, `progress` is initialized as `Progress::new(1)`. While `run_http` continuously advances `progress`, `run_video` **never updates `progress`**—it only emits raw `download://progress` frontend events.
  1. `state.views()` calculates `downloaded` via `active.get(&d.id).map(|a| a.progress.total())`. Whenever `queue://changed` fires while a video is downloading (e.g. another item finishes, pauses, or settings change), `state.views()` returns `downloaded: 0`.
  2. When a video download is paused or interrupted, line 582 executes:
     ```rust
     state.record_progress(&id, progress.snapshot());
     ```
     Because `progress` was never touched inside `run_video`, `snapshot()` is `[0]`, **erasing all recorded downloaded bytes from `d.done`**.
  3. In addition, `d.plan.total` remains `None` throughout the download even after `yt-dlp` discovers the exact total size, meaning any view that queries `plan.total` (such as the detail modal) treats the download as indeterminate.
- **Fix:**
  1. Pass `progress: &Arc<Progress>` into `run_video`.
  2. On each parsed `FDPROG` tick, update `progress.counter(0).store(t.downloaded, Ordering::Relaxed)`.
  3. If `t.total > 0`, persist `d.plan.total = Some(t.total)` in state so views and persistence retain the real file size.

```rust
// Proposed fix in state.rs (run_video callback)
move |t| {
    progress.counter(0).store(t.downloaded, std::sync::atomic::Ordering::Relaxed);
    if let Some(total) = t.total {
        state.set_total(&row_id, total);
    }
    let _ = emitter.emit(
        "download://progress",
        ProgressRow { id: row_id.clone(), downloaded: t.downloaded, total: t.total },
    );
}
```

---

### 🐛 Bug 2: Video `part_path` Set to Directory — Orphaned Partial Files on Cancel/Delete
- **Location:** [`download.rs:723-746`](file:///home/logan/Projects/idm/src-tauri/src/download.rs#L723-L746) and [`state.rs:415-459`](file:///home/logan/Projects/idm/src-tauri/src/state.rs#L415-L459)
- **Problem:**
  `download::video_plan` sets:
  ```rust
  part_path: dest_dir.to_path_buf()
  ```
  When a video download is cancelled (`state.cancel`) or removed with `delete_file = true` (`state.remove`), the backend executes:
  ```rust
  let _ = std::fs::remove_file(&part);
  ```
  Calling `std::fs::remove_file` on `dest_dir` fails with `EISDIR` (*Is a directory*). As a consequence, any `.part`, `.ytdl`, or intermediate audio/video stream files (`*.fNNN.mp4`, `*.fNNN.webm`) created by `yt-dlp` are permanently orphaned on disk.
- **Fix:**
  Store the expected `.part` file prefix or intermediate path in `part_path` (e.g. `dest_dir.join(format!("{name}.part"))`), and upon cancellation or deletion of video entries, clean up `yt-dlp` artifacts matching the title or ID prefix in `dest_dir`.

---

### 🐛 Bug 3: `set_final_path` Directory Inode Size Misattribution
- **Location:** [`state.rs:328-340`](file:///home/logan/Projects/idm/src-tauri/src/state.rs#L328-L340)
- **Problem:**
  ```rust
  fn set_final_path(&self, id: &str, path: PathBuf) {
      let size = std::fs::metadata(&path).ok().map(|m| m.len());
      let mut queue = self.queue.lock().unwrap();
      if let Some(d) = queue.iter_mut().find(|d| d.id == id) {
          d.plan.final_path = path;
          if let Some(size) = size {
              d.plan.total = Some(size);
              d.done = vec![size];
          }
      }
  }
  ```
  If `yt-dlp` fails to parse a destination line or errors early, `final_path` falls back to `dir` (a directory). On Linux/Unix, `std::fs::metadata(&dir).map(|m| m.len())` returns `4096` bytes (the ext4 directory inode block size). The download is recorded as completed with a corrupted size of `4096 B` (`4.0 KB`).
- **Fix:**
  Ensure `path.is_file()` before querying length:
  ```rust
  let size = std::fs::metadata(&path)
      .ok()
      .filter(|m| m.is_file())
      .map(|m| m.len());
  ```

---

### 🐛 Bug 4: Single-Threaded Bridge Blocking on Slow Video Metadata Resolves
- **Location:** [`server.rs:57-96`](file:///home/logan/Projects/idm/src-tauri/src/server.rs#L57-L96) and [`server.rs:121-131`](file:///home/logan/Projects/idm/src-tauri/src/server.rs#L121-L131)
- **Problem:**
  The extension bridge runs a single-threaded loop over `server.incoming_requests()`. In `handle_add`, it blocks synchronously on `rx.recv()` waiting for `state.add_with_session`.
  ```rust
  let (tx, rx) = std::sync::mpsc::channel();
  tauri::async_runtime::spawn(async move {
      let result = state.add_with_session(&app, &url, Some(session), force_video).await;
      // ...
      let _ = tx.send(result);
  });
  rx.recv().map_err(|_| "internal error".to_string())?
  ```
  For video URLs, `add_with_session` invokes `ytdlp::resolve_meta`, which executes `yt-dlp --dump-single-json` with a 15-second network timeout. While this metadata probe runs:
  1. The bridge thread is blocked and cannot respond to `/ping` requests from the extension popup.
  2. The extension popup indicator flips from green to red ("App not running").
  3. Batch additions ("Download all links") stall sequentially.
- **Fix:**
  Spawn a lightweight Tokio task or thread per incoming request so that slow metadata extraction does not starve health checks or concurrent requests.

---

### 🐛 Bug 5: Orphaned FFmpeg Child Subprocesses on Video Cancellation
- **Location:** [`ytdlp.rs:194-200`](file:///home/logan/Projects/idm/src-tauri/src/ytdlp.rs#L194-L200)
- **Problem:**
  When a video download is paused or cancelled, the cancellation token fires:
  ```rust
  _ = token.cancelled() => {
      let _ = child.kill().await;
      return Err("cancelled".into());
  }
  ```
  `child.kill().await` sends `SIGKILL` only to the immediate parent process (`yt-dlp`). If `yt-dlp` has spawned `ffmpeg` for postprocessing (e.g. remuxing audio and video streams or converting formats), the child `ffmpeg` process is orphaned, reparented to `init` (PID 1), and continues running in the background consuming 100% CPU.
- **Fix:**
  Spawn `yt-dlp` inside its own process group using Unix `process_group(0)` and terminate the entire process group with `libc::kill(-pid, libc::SIGKILL)` upon cancellation:

```rust
// In ytdlp.rs:
#[cfg(unix)]
{
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
}

// Upon cancellation:
#[cfg(unix)]
if let Some(pid) = child.id() {
    unsafe { libc::kill(-(pid as i32), libc::SIGKILL); }
} else {
    let _ = child.kill().await;
}
```

---

### 🐛 Bug 6: Extension Popup Crash on Restricted/Detached Tabs
- **Location:** [`extension/popup.js:27-32`](file:///home/logan/Projects/idm/extension/popup.js#L27-L32)
- **Problem:**
  ```javascript
  [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  referer = tab?.url || "";

  const { items, alive } = await chrome.runtime.sendMessage({ type: "getDetected", tabId: tab.id });
  ```
  When the extension popup is opened in an isolated DevTools window, an extension options page, or certain system chrome tabs, `chrome.tabs.query` can return an empty array `[]`, leaving `tab` as `undefined`. While line 28 uses optional chaining (`tab?.url`), line 30 accesses `tab.id` directly, throwing an unhandled `TypeError: Cannot read properties of undefined (reading 'id')` and leaving the popup in a blank, broken state.
- **Fix:**
  Guard `tab` before querying detected media:
  ```javascript
  [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  if (!tab) {
    statusText.textContent = "No active tab";
    return;
  }
  referer = tab.url || "";
  ```

---

## 3. Performance & Concurrency Bottlenecks

### ⚡ 1. Keystroke Churn in Settings View
- **Location:** [`SettingsView.tsx:14-20`](file:///home/logan/Projects/idm/src/components/SettingsView.tsx#L14-L20) and [`state.rs:374-379`](file:///home/logan/Projects/idm/src-tauri/src/state.rs#L374-L379)
- **Issue:**
  Typing into any input field in `SettingsView` triggers `onChange` $\rightarrow$ `api.updateSettings(next)` on every keystroke.
  On the backend, `set_settings`:
  1. Executes a synchronous disk write (`settings.json`).
  2. Tears down and respawns the throttle refill task (`rebuild_throttle`).
  3. Executes `state::pump(&app, &state)`.
  Typing a file path like `/home/logan/Downloads` causes ~20 disk writes, 20 semaphore teardowns, and 20 queue pumps within seconds.
- **Improvement:**
  Debounce settings updates in the UI (e.g. 300ms debounce), or commit text fields on `onBlur` and `Enter`.

---

### ⚡ 2. Throttle Slice Contention at High Bandwidths
- **Location:** [`throttle.rs:48-79`](file:///home/logan/Projects/idm/src-tauri/src/throttle.rs#L48-L79)
- **Issue:**
  `inner.slice` is hardcoded to a maximum of `SLICE = 16 KB`. For high bandwidth limits (e.g. 50 MB/s or 100 MB/s), a single 1 MB network chunk from `reqwest` requires 64 sequential semaphore acquisitions in a tight loop. With 24 concurrent segments, this causes unnecessary lock contention and context-switching overhead in Tokio.
- **Improvement:**
  Scale `slice` dynamically with the configured bandwidth:
  ```rust
  let slice = (per_tick / 4).clamp(16 * 1024, 256 * 1024);
  ```

---

### ⚡ 3. `yt-dlp` Completely Bypasses Bandwidth Limits
- **Location:** [`state.rs:682-741`](file:///home/logan/Projects/idm/src-tauri/src/state.rs#L682-L741)
- **Issue:**
  While HTTP segmented downloads strictly adhere to `state.current_throttle()`, `run_video` does not pass any rate limiter to `yt-dlp`. If a user configures a global speed cap (e.g. 1 MB/s), video downloads ignore it completely and saturate the network.
- **Improvement:**
  Pass `--limit-rate <kb_per_sec>K` to `yt-dlp` when `bandwidth_kb > 0`:
  ```rust
  if settings.bandwidth_kb > 0 {
      cmd.arg("--limit-rate").arg(format!("{}K", settings.bandwidth_kb));
  }
  ```

---

## 4. Architectural & UI Refinements

### 1. Detail Modal Usability for Video Downloads
- **Location:** [`DetailModal.tsx`](file:///home/logan/Projects/idm/src/components/DetailModal.tsx)
- **Enhancements:**
  1. Feed `liveTotal` from `App.tsx` into `DetailModal`. For video downloads where `total` is resolved dynamically by `yt-dlp`, the modal currently displays `"size unknown"` and an indeterminate bar even though the main row displays accurate percentages.
  2. For video downloads (`row.engine === "ytdlp"`), the `Connections` field displays `"1 (server has no range support)"`. It should display `"yt-dlp engine"` instead.

### 2. Extended `yt-dlp` Postprocessor Output Patterns
- **Location:** [`ytdlp.rs:251-260`](file:///home/logan/Projects/idm/src-tauri/src/ytdlp.rs#L251-L260)
- **Enhancements:**
  `parse_final` only recognizes `[Merger]` and `[ExtractAudio]`.
  Adding patterns for `[FixupM3u8]`, `[FixupM4a]`, and `[VideoRemuxer]` prevents temporary intermediate stream files from being saved as the final path when downloading HLS/DASH streams on sites such as Twitter/X, Twitch, or Vimeo.

### 3. Graceful Application Shutdown & Buffer Sync
- **Location:** [`lib.rs:164`](file:///home/logan/Projects/idm/src-tauri/src/lib.rs#L164)
- **Enhancements:**
  System tray `Quit` invokes `app.exit(0)` immediately.
  Implement a graceful shutdown routine:
  1. Trigger root cancellation tokens for active downloads.
  2. Await segment cancellations and call `file.sync_data()` on open file handles.
  3. Commit the exact in-memory progress counters to `queue.json` before exiting.

---

## 5. Priority & Implementation Matrix

| Priority | Issue / Refinement | Category | Affected Files | Risk / Impact if Omitted |
| :---: | :--- | :---: | :--- | :--- |
| **P0** | Video progress zeroing on pause/restart | Bug | [`state.rs`](file:///home/logan/Projects/idm/src-tauri/src/state.rs) | **Progress wiped out.** Resuming a paused video resets downloaded bytes to 0. |
| **P0** | Orphaned FFmpeg subprocesses on cancel | Bug | [`ytdlp.rs`](file:///home/logan/Projects/idm/src-tauri/src/ytdlp.rs) | **Resource leak.** Zombie `ffmpeg` processes peg CPU at 100% after cancel. |
| **P0** | Video `part_path` directory `EISDIR` error | Bug | [`download.rs`](file:///home/logan/Projects/idm/src-tauri/src/download.rs), [`state.rs`](file:///home/logan/Projects/idm/src-tauri/src/state.rs) | **Orphaned files.** Cancelled/deleted video downloads leave debris on disk. |
| **P1** | Single-threaded bridge blocks on video resolve | Concurrency | [`server.rs`](file:///home/logan/Projects/idm/src-tauri/src/server.rs) | **UI Freeze / Health Check Timeout.** Popup shows "App not running" during metadata fetch. |
| **P1** | Directory inode size misattribution (4096 B) | Bug | [`state.rs`](file:///home/logan/Projects/idm/src-tauri/src/state.rs) | Completed video displays corrupt 4.0 KB total size if path fallback occurs. |
| **P1** | Extension popup crash on detached/empty tab | Stability | [`extension/popup.js`](file:///home/logan/Projects/idm/extension/popup.js) | Popup crashes with unhandled TypeError in DevTools or system tabs. |
| **P1** | Pass `--limit-rate` to `yt-dlp` | Feature/Perf | [`state.rs`](file:///home/logan/Projects/idm/src-tauri/src/state.rs), [`ytdlp.rs`](file:///home/logan/Projects/idm/src-tauri/src/ytdlp.rs) | Configured bandwidth limits bypassed during video downloads. |
| **P2** | Throttle slice contention at high speed | Performance | [`throttle.rs`](file:///home/logan/Projects/idm/src-tauri/src/throttle.rs) | High semaphore acquisition churn at 50+ MB/s transfers. |
| **P2** | Debounce `SettingsView` updates | Performance | [`SettingsView.tsx`](file:///home/logan/Projects/idm/src/components/SettingsView.tsx) | Rapid synchronous disk writes and task rebuilds on every keystroke. |
| **P2** | Graceful tray quit & `sync_data()` | Reliability | [`lib.rs`](file:///home/logan/Projects/idm/src-tauri/src/lib.rs), [`state.rs`](file:///home/logan/Projects/idm/src-tauri/src/state.rs) | Potential loss of last progress checkpoint on clean quit. |
| **P3** | Detail Modal `liveTotal` & engine tag | UI Polish | [`DetailModal.tsx`](file:///home/logan/Projects/idm/src/components/DetailModal.tsx) | Video modal shows "size unknown" and incorrect connection description. |
| **P3** | Extended `yt-dlp` postprocessor outputs | Robustness | [`ytdlp.rs`](file:///home/logan/Projects/idm/src-tauri/src/ytdlp.rs) | Intermediate remuxing names retained on HLS/DASH downloads. |
