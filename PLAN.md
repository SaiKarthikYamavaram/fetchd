# fetchd — Personal Internet Download Manager

## Context

User wants own download manager: faster than browser default via segmented/multi-connection downloads, plus queue management. Personal-use tool, not distributed. Full requirements gathered through an extensive interview (grilling session) — every scope decision below was explicitly settled with the user, not assumed. Plan was further refined against [IMPROVEMENTS.md](file:///home/logan/Projects/idm/IMPROVEMENTS.md), a technical review that caught disk I/O, network-edge-case, and Tauri v2 framework issues — those refinements are folded in below (superseding the earlier draft where noted).

Target directory `/home/logan/Projects/idm` — confirmed empty, not a git repo. Toolchain confirmed present: Rust 1.98.0/cargo, Node v26.8.1/npm 11.19.0, Arch system libs for Tauri (webkit2gtk-4.1, gtk3, libayatana-appindicator, librsvg, base-devel) all installed. `tauri-cli` not globally installed — not needed, template's local `npm run tauri` covers it.

A second [IMPROVEMENTS.md](file:///home/logan/Projects/idm/IMPROVEMENTS.md) pass added protocol/speed and desktop-polish refinements (HTTP/1.1 enforcement per segment, physical disk pre-allocation, filename extraction, centralized progress ticker, completion notifications, and a deferred work-stealing stretch goal) — also folded in below.

## Stack

- **Tauri v2** (Rust backend) + **React + TypeScript + Vite** frontend (standard `create-tauri-app` template)
- App name: **fetchd**, bundle id `com.saikarthik.fetchd`
- Cross-platform target (Linux/Mac/Windows), dev/test on Arch Linux

## Settled scope

**Download engine:**
- `reqwest` + `tokio` async, segmented download (4-8 fixed connections, default 8), fallback to single-connection when server lacks `Accept-Ranges`. **Files under 4 MB use a single connection** — 8 sockets for a 200 KB file is slower than one.
- **HTTP/1.1 enforced on the segmented client.** `reqwest` negotiates HTTP/2 by default, which multiplexes all streams over one TCP connection — that silently defeats segmentation's whole point (bypassing per-connection congestion/throttling). Build the segment client with `.http1_only()` so each segment gets its own physical TCP socket. `pool_max_idle_per_host(8)` keeps sockets warm across retries; it is not what creates the parallelism.
- **Never enable the `gzip`/`brotli`/`deflate` features on `reqwest`.** Automatic decompression desyncs the stream from `Content-Length`/`Content-Range` byte offsets and silently corrupts every seek-write. Dep list requests `stream` and `json` only.
- **Browser `User-Agent`** on both the probe and segment clients — many CDNs 403 the default `reqwest/<version>`.
- **Stall detection:** `.connect_timeout(10s)` on the client, plus `tokio::time::timeout(30s, stream.next())` around every chunk read, elapsing into a retryable error. A black-holed socket (Wi-Fi drop, CGNAT rebind) raises no error and would otherwise hang the segment forever, so the retry ladder below never runs without this. Do not use `reqwest`'s whole-request `.timeout()` — it would kill legitimate multi-GB transfers.
- **Storage: single pre-allocated file, not part-file concatenation.** When size is known and ranges supported, create `filename.ext.part` **inside the destination download folder** (never `app_data_dir` — a `.part` on a different mount makes the final rename fail `EXDEV` and fall back to the multi-GB copy this design exists to avoid, and it would free-space-check the wrong drive). Physically reserve disk blocks via `fs4::tokio::AsyncFileExt::allocate(total_size)` (`posix_fallocate` on Linux) — not bare `set_len`, which only creates a sparse file and lets a too-small disk fail hours later mid-download instead of at second 0. Windows gets best-effort allocation-size semantics only; the explicit free-space check is the real guard there. Each segment worker seeks to its own `[start,end]` and writes at that offset. Rename `.part` → final name on completion. Zero-cost finish — avoids needing 2x disk space and avoids a multi-GB sequential-copy freeze at 100%.
- **Verify length before rename.** The `.part` file is pre-allocated to full size, so it is always the right length regardless of what was written. Assert bytes-written == `Content-Length` before renaming; mismatch → keep `.part`, mark `Failed`, offer retry. One integer comparison, not checksum verification (out of scope).
- **Filename resolution.** Extraction order: `Content-Disposition` header (`filename`/`filename*`) → final redirect URL's path → `download.bin` fallback. Sanitize invalid OS chars (`/ \ : * ? " < > |`) and control chars, **then take `Path::file_name()` and reject empty/`.`/`..`/leading-dot** — `Content-Disposition` is remote-controlled input and char-stripping alone leaves `..` intact. Then auto-suffix on collision: `file.zip` → `file (1).zip` → `file (2).zip`, matching browser convention (settled in the original naming-conflict decision).
- Resume across app restart via seek-written `.part` file + `queue.json` progress metadata (byte offsets per segment)
- **Checkpoint invariant: a byte offset in `queue.json` must never exceed bytes durably on disk.** Under-recording costs a small re-download; over-recording makes resume seek past unwritten bytes and leaves a permanent zero-filled hole nothing detects. Every ~8 MB per segment (or 5s, whichever first): `file.sync_data().await` **then** the atomic offset write, in that order. `flush()` is not sufficient — it only reaches the OS page cache, not the device. Do not checkpoint per chunk (fsync storm).
- **Restart state model** (replaces earlier blanket "auto-resume everything"):

  | State at exit | On relaunch | Trigger |
  |---|---|---|
  | `Downloading` | → `Interrupted` → auto-resumes | debounced ~2-3s after launch |
  | `Queued` | stays `Queued` → auto-starts | when a concurrency slot opens |
  | `Paused` | stays `Paused` | manual "Resume" click only |
  | `Failed` | stays `Failed` | manual "Retry" click only |
  | `Completed` | stays `Completed` | inactive |

  Rationale: a user who explicitly paused (e.g. to free bandwidth) should never have that overridden just by reopening the app; only unplanned interruption (crash/quit/power-cut) auto-recovers.
- Redirect following (capped), retry per segment: 5 retries, backoff `2s/5s/10s/20s/30s` (~67s survival window — the old 1s/2s/4s burned its whole budget in 7s and died on any Wi-Fi blip or lid close). **Budget resets after a segment transfers ~8 MB of forward progress** — it is per stall episode, not per download lifetime, or a 4-hour download still dies to five unrelated blips.
- Retries exhausted → mark Failed, retained in queue, manual retry button
- **HEAD-blocked CDN fallback:** if `HEAD` returns 403/405/501 (common on S3 pre-signed URLs, some CDNs), fall back to a `GET` with `Range: bytes=0-0` — a `206` response's `Content-Range: bytes 0-0/TOTAL` gives both range-support confirmation and total size. `Content-Range: bytes 0-0/*` means the total is unknown → treat as no-size, single connection.
- **No `Content-Length` (chunked/dynamic):** skip `allocate`, single connection streaming to EOF, indeterminate progress (bytes only, no percent or ETA). `Content-Length: 0` → create the empty file, mark `Completed`, spawn no workers.
- **Strict 206 validation:** segment workers must confirm the response is `206 Partial Content`. If a server ignores `Range` and returns `200 OK` instead, that worker must not write at an offset (would corrupt the file) — cancel sibling segment workers and downgrade the whole download to single-connection from byte 0.
- **Resume integrity via `If-Range`:** store `ETag` (fallback `Last-Modified`) in `queue.json`, then send it as `If-Range` alongside `Range` on resume. Unchanged → `206`, resume. Changed → server returns `200` with the full body, which the strict-206 check above already handles by restarting from 0. One header replaces a second round trip plus a hand-rolled comparison and its weak/strong-ETag edge cases. Neither validator issued → discard the partial and restart from 0.
- Duplicate URL → warn, allow "add anyway"
- URL scheme allowlist: http/https only, rejected at add-time
- Disk space check before start when `Content-Length` known (`fs4` crate — no stdlib API for free space; same crate also does the physical pre-allocation above)
- Cancel → delete `.part` file immediately; Pause → keep partial + offsets, resumable (manual only, per state table above)
- **Clean cancellation:** `tokio_util::sync::CancellationToken` — one root token per download, `child_token()` per segment. Calling `.cancel()` on pause/cancel aborts every segment's network loop instantly, no dangling connections.
- Queue concurrency: max 3 simultaneous downloads (default, user-configurable), each using 4-8 segments
- **Bandwidth throttle via `tokio::sync::Semaphore`** (not a hand-rolled counter): semaphore initialized with byte permits; a background task ticks every 100ms and adds `(cap_kb_sec * 1024) / 10` permits. **Cap the bank at ~2 ticks' worth** — unbounded accumulation means a minute idle at 5 MB/s banks 300 MB of permits and the next download ignores the cap entirely until it drains. **Acquire in fixed 16 KB slices**, not `acquire_many(chunk_len)`, which blocks until the whole chunk's permits exist at once and makes low caps transfer in visible lurches. Unlimited mode bypasses the semaphore entirely. Already available via the `tokio` dep (full features) — no new crate.
- **Atomic persistence:** `queue.json`/`settings.json` writes go to `<path>.tmp`, `sync_all()`, then `std::fs::rename` to the real path — never a direct in-place write that could truncate the file to 0 bytes on a mid-write crash.
- Single-instance lock (`tauri-plugin-single-instance`) — second launch focuses existing window

**UI (React):**
- Queue view: per-row progress/speed, pause/resume/cancel, open-containing-folder on completion, retry on failure, pause-all button
- History view: completed+failed, manual clear button, no auto-cap
- Add via URL paste; Import via `.txt` file (one URL/line) using `tauri-plugin-dialog`; no export v1
- Settings view: download folder (default `~/Downloads`), max concurrent (default 3), theme toggle (dark/light, persisted), bandwidth KB/s
- System tray: minimize-to-tray, downloads continue in background, restore/quit menu
- **Native completion notification** when a download finishes while the window is hidden/minimized (`tauri-plugin-notification`)
- No queue reordering v1 (FIFO), default Tauri icon v1

**Explicitly out of scope (YAGNI, revisit only if actually needed):** browser extension, media/video site scraping, non-HTTP protocols, multi-user/cloud sync, scheduler, proxy support, auto-updater, clipboard monitor, checksum verification, queue export, drag-reorder, **dynamic work-stealing for straggler segments** (IMPROVEMENTS.md flags this itself as a P4/Phase-2 stretch goal — only worth it if segmented downloads visibly stall near completion on a slow/congested segment).

## Implementation

### Scaffolding
```bash
cd /home/logan/Projects
npm create tauri-app@latest idm -- --template react-ts --manager npm
```
Prompts: app name `fetchd`, identifier `com.saikarthik.fetchd`.

Rust deps to add (`src-tauri/Cargo.toml`): `reqwest` (features `stream`,`json` — **not** `gzip`/`brotli`), `tokio` (`full`), `tokio-util` (feature `rt`, for `CancellationToken`), `futures-util` (`StreamExt` for `bytes_stream()`), `serde_json`, `tauri-plugin-single-instance`, `tauri-plugin-dialog`, `tauri-plugin-opener`, `tauri-plugin-notification`, `fs4` (features `["tokio"]` — disk-space check + physical pre-allocation).

Frontend: no state library needed — `useState`/`useReducer` + Tauri event listeners cover this scope. No router (3-tab `useState` switch is enough).

### Tauri v2 project shape
- Standard v2 layout puts the app builder in `src-tauri/src/lib.rs` (`pub fn run()`); `main.rs` is a thin wrapper:
  ```rust
  fn main() { fetchd_lib::run(); }
  ```
  All modules below are registered from `lib.rs`, not `main.rs`.
- **Capabilities must be granted explicitly** — Tauri v2 plugins have no implicit frontend access. `src-tauri/capabilities/default.json`:
  ```json
  {
    "$schema": "../gen/schemas/desktop-schema.json",
    "identifier": "default",
    "description": "Default capabilities for fetchd",
    "windows": ["main"],
    "permissions": ["core:default", "dialog:default", "opener:default", "notification:default"]
  }
  ```
  `tauri-plugin-single-instance` registers no frontend commands, so it needs no permission entry — adding a `single-instance:default` string fails capability validation at build time.
- **Minimize-to-tray** requires intercepting close, not just registering a tray:
  ```rust
  .on_window_event(|window, event| {
      if let tauri::WindowEvent::CloseRequested { api, .. } = event {
          window.hide().unwrap();
          api.prevent_close();
      }
  })
  ```

### Rust module layout (`src-tauri/src/`)
Flat, no premature abstraction — single download strategy (segmented-with-fallback), not pluggable, so no `trait Downloader`.

- `lib.rs` — Tauri builder (`run()`): plugin registration, capabilities, tray setup, `on_window_event` close-to-tray, command registration
- `main.rs` — thin wrapper calling `fetchd_lib::run()`
- `state.rs` — `AppState` (queue + settings + throttle semaphore, in `tauri::State`), atomic JSON load/save. **Lock rule: per-segment progress is `Arc<AtomicU64>` (lock-free hot path); queue metadata sits behind a `std::sync::Mutex` and no `.await` ever happens under the guard** — parking while holding it deadlocks the executor. EMA speed state lives inside the ticker task, which is its only reader/writer, so it needs no synchronization at all.
- `download.rs` — the engine: HEAD check w/ `Range: bytes=0-0` fallback, `http1_only()` segment client, `fs4::allocate` pre-allocation + seek-write segments, strict 206 validation w/ single-connection downgrade, filename extraction/sanitize/collision, retry/backoff, `CancellationToken` wiring, `AtomicU64` progress counters, ETag/Last-Modified check on resume
- `queue.rs` — `Download` struct + status enum (`Queued|Downloading|Paused|Interrupted|Completed|Failed`), add/remove/pause/resume/cancel ops, dup-check, scheme validation
- `settings.rs` — `Settings` struct + atomic JSON load/save
- `tray.rs` — tray icon/menu, fires completion notification when window is hidden
- `throttle.rs` — `tokio::sync::Semaphore`-based limiter + 100ms replenish task
- 250ms progress-ticker task lives in `state.rs` (it reads `AppState`'s active downloads to build the snapshot) — not its own file, ~15 lines. Set `MissedTickBehavior::Delay` and derive `delta_t` from a stored `Instant`; the default `Burst` fires missed ticks back-to-back with `delta_t ≈ 0` after a runtime stall, spiking the EMA. Skip the update when `delta_t < 0.05` rather than dividing by it.
- `#[cfg(test)]` module in `download.rs` — three pure functions, no framework: `Content-Disposition` parsing (quoted, unquoted, RFC 5987 `filename*`, traversal rejection), `resolve_unique_path` (extensionless, dotfiles, `archive.tar.gz` must not become `archive (1).gz`), segment range math (inclusive `[start,end]` boundaries, last-segment remainder, single-segment case).

Data dir (via `app.path()`): `<app_config_dir>/settings.json`, `<app_data_dir>/queue.json`. **The `.part` file lives in the user's download folder next to its final name, not in `app_data_dir`** — see the `EXDEV` rationale above. Single pre-allocated file per download, no per-segment part files. `queue.json` writes are event-driven/periodic, not per-chunk (avoid I/O storm), always atomic (tmp+rename), and always preceded by `sync_data()` on the `.part` file.

### Tauri commands / events
Commands: `add_download`, `import_urls`, `start_download`, `pause_download`, `resume_download`, `cancel_download`, `retry_download`, `pause_all`, `get_queue`, `get_history`, `clear_history`, `get_settings`, `update_settings`, `open_containing_folder`.

**Progress events are centralized, not per-download.** With 3 concurrent downloads × 8 segments, up to 24 tasks read chunks simultaneously — if each emitted its own IPC event per chunk, the webview floods and the UI drops frames. Instead: segment tasks only bump in-memory `AtomicU64` byte counters; one background ticker at a fixed 250ms cadence snapshots all active downloads and emits a single batched `download://progress-batch` event. `download://status-changed` stays per-transition (rare, no flood risk).

### React structure (`src/`)
`App.tsx` (tab switch) → `components/QueueView.tsx` (owns event listeners, add/import/pause-all), `DownloadRow.tsx`, `HistoryView.tsx`, `SettingsView.tsx`. `lib/api.ts` (invoke wrappers), `lib/types.ts` (mirrors Rust structs by hand).

### Critical files
- `/home/logan/Projects/idm/src-tauri/src/lib.rs`
- `/home/logan/Projects/idm/src-tauri/src/download.rs`
- `/home/logan/Projects/idm/src-tauri/src/state.rs`
- `/home/logan/Projects/idm/src-tauri/src/queue.rs`
- `/home/logan/Projects/idm/src-tauri/capabilities/default.json`
- `/home/logan/Projects/idm/src/components/QueueView.tsx`

## Build order & verification

M1-M4 is the shippable product — a correct, resumable, segmented downloader. M5 (throttle) and M6 (history/import/tray/notifications) are polish and can be dropped without losing the tool's purpose.

1. **M1 — single-connection download, standard v2 layout, filename resolution.** `lib.rs`/`main.rs` split, `capabilities/default.json` configured. Hardcoded `download_file`, filename extraction (`Content-Disposition` → redirect URL path → `download.bin`) + sanitize + collision auto-suffix, one command, minimal UI (URL input + progress bar). Verify: `npm run tauri dev`, download a public test file (e.g. `https://ash-speed.hetzner.com/100MB.bin`), confirm final size matches and file lands in `~/Downloads`; download the same URL twice, confirm second lands as `name (1).ext`; hit a URL with a signed/opaque path but a `Content-Disposition` header, confirm the real filename is used instead of the opaque path segment.
2. **M2 — redirects, retry/backoff, disk-space check, scheme validation, HEAD fallback.** Verify: redirecting URL succeeds; simulate network failure mid-download → 3 retries with visible backoff → Failed status; `ftp://` URL rejected pre-network; oversized file rejected pre-download; a URL that 405s on `HEAD` still succeeds via the `Range: bytes=0-0` probe.
3. **M3 — segmentation: HTTP/1.1-per-segment, pre-allocated seek-write, single-connection fallback, strict 206 check.** Verify: range-supporting URL creates one physically-allocated `.part` file (confirm via `du`/`stat` block count that space is actually reserved on disk, not just sparse-metadata `set_len`), segments write at correct offsets, no concat step, instant rename on finish; check (e.g. via `ss`/`netstat` or server-side connection logs) that segments open distinct TCP connections rather than sharing one HTTP/2 socket. Force a server response that returns `200` instead of `206` on a ranged request and confirm the download downgrades to single-connection cleanly instead of corrupting the file. Non-range server falls back to single-connection without error. Try starting a download larger than free disk space and confirm it's rejected immediately (second 0), not hours in.
4. **M4 — pause/resume/cancel, restart persistence, state-table semantics, ETag check.** Verify: pause freezes `.part` growth and status becomes `Paused`; quit mid-download (kill process) and relaunch — status shows `Interrupted` then auto-resumes after ~2-3s; a download that was `Paused` at quit stays `Paused` after relaunch (no auto-resume); resume continues from saved offset (no truncation, no corruption); cancel deletes the `.part` file and queue entry; modify/replace the remote file between pause and resume (or fake a changed `ETag`) and confirm it restarts from 0 instead of merging stale bytes.
5. **M5 — queue concurrency (max 3), semaphore throttle, cancellation tokens, batched progress ticker.** Verify: 5 queued downloads → only 3 `Downloading` at once, rest `Queued`, next one auto-starts when a slot frees; throttle set to 500 KB/s → aggregate speed stays near cap; cancel mid-download confirms the underlying socket/task ends immediately (no orphaned connection lingering, check via process/connection count); with all 3 slots running 8 segments each, confirm the frontend receives one `download://progress-batch` event roughly every 250ms (not a per-chunk flood) and the UI stays responsive (no input lag) under that load.
6. **M6 — full UI: History, Settings, Import, dup-detection, single-instance, tray-with-close-intercept, atomic writes, notifications.** Verify: second launch focuses existing window (no duplicate process); closing window (X button) hides it instead of quitting — app stays in tray, active download keeps progressing, tray icon restores the window; minimize the window, let a download finish, confirm an OS notification appears; duplicate URL triggers warning; theme persists across restart; `.txt` import with one malformed line adds valid ones and reports the skip; kill the process mid-write (e.g. `kill -9` right after a queue change) and relaunch — confirm `queue.json` is still valid JSON (not truncated), proving the atomic tmp+rename write held.
