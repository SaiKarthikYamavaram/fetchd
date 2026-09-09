# fetchd — Advanced Architecture Improvements & Edge-Case Refinements

This document tracks high-impact architectural refinements, protocol-level speed optimizations, and desktop integration details for **fetchd**.

*(Note: Foundational improvements—such as pre-allocated seek-writes, the restart state table, strict 206 validation, `CancellationToken`, semaphore throttling, and atomic JSON persistence—have already been merged into [PLAN.md](file:///home/logan/Projects/idm/PLAN.md).)*

---

## 1. Networking: Parallel TCP Enforcement (`http1_only()`)

### The Problem with HTTP/2 Multiplexing
By default, modern HTTP clients (including `reqwest`) negotiate **HTTP/2** via ALPN during the TLS handshake.
- HTTP/2 multiplexes multiple logical streams over a **single underlying TCP connection**.
- If 8 segment workers share an HTTP/2 connection pool to a server, all 8 streams traverse the **same single TCP socket**.
- This negates the primary mechanism of segmented download managers: bypassing single-connection TCP congestion windows, packet-loss penalties, and ISP/server per-stream bandwidth shaping.

### Implementation
Configure the segmented download `reqwest::Client` with `.http1_only()`:
```rust
let segmented_client = reqwest::Client::builder()
    .http1_only() // Enforces distinct physical TCP connections per segment
    .pool_max_idle_per_host(8) // Keeps segment sockets warm between retries; see note
    .connect_timeout(Duration::from_secs(10))
    .build()?;
```
*Result:* Each segment operates over its own independent physical TCP socket, maximizing aggregate throughput.

> **Note on `pool_max_idle_per_host`:** this governs how many *idle* connections the pool retains for reuse, not how many run concurrently. Under `http1_only()`, concurrent requests already open separate sockets — the parallelism comes from `http1_only()` alone. The pool setting only avoids re-handshaking when a segment reconnects after a retry.

> **Do not enable the `gzip` / `brotli` / `deflate` features on `reqwest`.** With automatic decompression on, the bytes yielded by the stream no longer correspond to the byte offsets in `Content-Length` and `Content-Range`, so every seek-write lands at the wrong offset and silently corrupts the file. The dependency list deliberately requests only `stream` and `json`.

---

## 2. Disk: Physical Block Pre-Allocation (`fallocate`) vs. Sparse Files

### The Risk of Bare `set_len` (Sparse File Holes)
Calling `tokio::fs::File::set_len(total_bytes)` on Linux/Unix systems often creates a **sparse file** (allocates metadata without physically reserving disk sectors).
- If a user starts a 20 GB download on a drive with only 12 GB free, `set_len` succeeds instantly without error.
- Hours later, as segment workers write into unallocated physical blocks, the kernel runs out of disk sectors and triggers an `ENOSPC` crash midway through the download.

### Implementation
Use `fs4::allocate` (with `features = ["tokio"]` in `Cargo.toml`) before initiating segment workers:
```rust
use fs4::tokio::AsyncFileExt;

// Physically commits disk blocks up front (posix_fallocate on Linux)
file.allocate(total_size).await?;
```
*Result:* On Linux/Unix, if disk space is insufficient the download aborts **at second 0** before consuming network bandwidth.

> **Platform caveat.** The strong guarantee is Linux/Unix only. On Windows, `fs4` sets the file's allocation size via `SetFileInformationByHandle`; it does **not** call `SetFileValidData`, which requires the `SE_MANAGE_VOLUME_NAME` privilege. Treat Windows as best-effort reservation, and keep the explicit free-space check as the real guard there.

### Destination Volume & Cross-Device Links (`EXDEV`)
Create the `.part` file **directly inside the destination folder** (e.g. `target_dir/filename.ext.part`), rather than in `<app_data_dir>`:
1. Ensures `fs4::allocate` checks the free space of the actual target drive (e.g., secondary HDD or external SSD), not the OS root drive.
2. Guarantees that the final rename from `.part` to final filename is instantaneous and never fails with `EXDEV` (*"Invalid cross-device link"*).

---

## 3. File Handling: `Content-Disposition` & Collision Avoidance

### 3.1 Real Filename Extraction (RFC 6266 / RFC 5987)
Many download URLs are opaque endpoints (e.g. `https://example.com/dl?token=xyz987` or signed S3 links) or pass through multiple redirects.
- If the filename is extracted purely from the initial URL path, files are saved as `dl` or `dl.html`.
- **Extraction Hierarchy:**
  1. Check response headers for `Content-Disposition: attachment; filename="..."` or `filename*=UTF-8''...`.
  2. If missing, extract the filename from the path of the **final redirect URL** (`response.url().path()`).
  3. If still indeterminate or empty, fall back to `download.bin`.
  4. Sanitize the filename to strip invalid OS characters (`/`, `\`, `:`, `*`, `?`, `"`, `<`, `>`, `|`) and control characters.
  5. **Guard against path traversal.** `Content-Disposition` is attacker-controlled input supplied by a remote server; a header of `filename="../../.bashrc"` must never resolve outside the download directory. After sanitizing, take `Path::file_name()` of the result and reject anything that is empty, `.`, `..`, or begins with `.`. Fall back to `download.bin` on rejection. Character stripping alone is not sufficient — it leaves `..` intact.

### 3.2 Collision Avoidance
Prevent overwriting existing files in the download destination:
```rust
fn resolve_unique_path(base_dir: &Path, filename: &str) -> PathBuf {
    let mut target = base_dir.join(filename);
    if !target.exists() {
        return target;
    }
    
    let stem = Path::new(filename).file_stem().and_then(|s| s.to_str()).unwrap_or("download");
    let ext = Path::new(filename).extension().and_then(|s| s.to_str()).unwrap_or("");
    
    let mut counter = 1;
    loop {
        let new_name = if ext.is_empty() {
            format!("{} ({})", stem, counter)
        } else {
            format!("{} ({}).{}", stem, counter, ext)
        };
        target = base_dir.join(new_name);
        if !target.exists() {
            return target;
        }
        counter += 1;
    }
}
```

---

## 4. IPC & Frontend Performance: Centralized Progress Heartbeat

### The Webview Event Flood Bottleneck
With 3 concurrent downloads running 8 segments each, up to 24 async tasks are reading network chunks simultaneously.
- If individual tasks emit `download://progress` events per chunk read, hundreds of serialized IPC messages per second flood the Tauri Webview.
- This results in high CPU usage, UI frame drops, and input lag in React.

### Implementation
- Segment tasks only increment in-memory atomic byte counters (`AtomicU64`).
- A single background Tokio ticker runs at a fixed 250ms cadence (4 updates/sec):
  ```rust
  tokio::spawn(async move {
      let mut interval = tokio::time::interval(Duration::from_millis(250));
      // Default is MissedTickBehavior::Burst: after a runtime stall, missed ticks
      // fire back-to-back with near-zero elapsed time between them. See §6.
      interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
      let mut last = std::time::Instant::now();
      loop {
          interval.tick().await;
          let now = std::time::Instant::now();
          let delta_t = now.duration_since(last).as_secs_f64();
          last = now;
          let snapshot = app_state.get_active_progress_snapshot(delta_t);
          if !snapshot.is_empty() {
              let _ = app_handle.emit("download://progress-batch", &snapshot);
          }
      }
  });
  ```
*Result:* Rock-solid 60 FPS frontend performance regardless of download speed or segment count.

---

## 5. Network Resilience: Browser User-Agent & Extended Retry Window

### 5.1 Standard Browser `User-Agent`
By default, `reqwest` sends `User-Agent: reqwest/<version>`. Many CDNs, Cloudflare-protected servers, and file-hosting mirrors reject non-browser `User-Agent` headers with `403 Forbidden`.
- **Mitigation:** Configure a standard browser user agent on the HTTP client:
  ```rust
  let client = reqwest::Client::builder()
      .user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36")
      .build()?;
  ```
- Apply the same user agent to the `HEAD` / `Range: bytes=0-0` probe client, not only the segment client — the probe is what gets 403'd first.
- *Known limitation:* a Chrome user agent sent over a connection that never offers HTTP/2 via ALPN (a direct consequence of `http1_only()`) is an inconsistency that TLS-fingerprinting stacks such as Cloudflare's JA3 checks can key on. This is the next thing to investigate if a specific host still returns `403` after the user agent is set.
- The version string goes stale. Treat it as a value to bump occasionally, not a constant.

### 5.2 Stall Detection — Precondition for Any Retry Logic
A retry ladder only runs when the underlying request produces an **error**. The most common real-world failure mode does not: when Wi-Fi drops or a CGNAT lease rebinds, an already-established TCP socket is silently black-holed. No `RST` arrives, no error surfaces, and the segment task simply awaits the next chunk forever. Bytes stop, the UI shows 0 B/s, and §5.3's backoff schedule is never entered because nothing ever failed.
- **Mitigation:** wrap every chunk read in a timeout, and treat elapsing as a retryable error:
  ```rust
  let chunk = tokio::time::timeout(Duration::from_secs(30), stream.next()).await;
  match chunk {
      Err(_elapsed) => return Err(SegmentError::Stalled), // feeds the retry ladder
      Ok(Some(Ok(bytes))) => { /* write at offset */ }
      Ok(Some(Err(e)))    => return Err(SegmentError::Network(e)),
      Ok(None)            => { /* clean EOF */ }
  }
  ```
- Pair with `.connect_timeout(Duration::from_secs(10))` on the client builder (see §1) so a dead host fails fast at handshake instead of hanging.
- Do **not** use `reqwest`'s whole-request `.timeout()` — it caps total request duration, which would kill legitimately long multi-GB segment transfers.

### 5.3 Extended Retry Schedule (Sleep & Wi-Fi Glitch Survival)
The original 3-retry backoff (`1s, 2s, 4s`) exhausts its entire budget in only 7 seconds. A momentary Wi-Fi reconnection or a brief laptop lid closure causes active downloads to fail permanently.
- **Mitigation:** Use a 5-step backoff schedule: `2s, 5s, 10s, 20s, 30s` (total survival window $\approx 67\text{ seconds}$).
- Gives transient network dropouts and laptop sleep transitions sufficient time to re-establish connections before marking the task `Failed`.
- **Requires §5.2.** Without a chunk-read timeout, a stalled segment never raises an error and this schedule is dead code.
- **Reset the budget on forward progress.** The 5 attempts are per *stall episode*, not per download lifetime. Once a segment successfully transfers a further ~8 MB, reset its counter to zero. Otherwise a four-hour download that hits five unrelated blips spread across those hours still fails permanently, which is exactly the outcome this section is trying to prevent.

---

## 6. Telemetry: Smoothed Speed & ETA via Exponential Moving Average (EMA)

### Eliminating UI Speed Jitter
Because TCP packets arrive in bursty windows, calculating speed naively as `(bytes_now - bytes_prev) / delta_t` causes the UI speed and ETA indicators to bounce wildly (e.g., jumping between 0 MB/s and 45 MB/s).
- **Mitigation:** Apply an Exponential Moving Average (EMA) inside the 250ms progress ticker:
  ```rust
  // delta_t is MEASURED, not assumed to be 0.25 - see the guard below
  let instant_speed = (bytes_now - bytes_prev) as f64 / delta_t;

  // Smoothed speed (alpha = 0.25)
  smoothed_speed = (0.75 * prev_speed) + (0.25 * instant_speed);

  let eta_seconds = if smoothed_speed > 0.0 {
      ((total_bytes - downloaded_bytes) as f64 / smoothed_speed) as u64
  } else {
      0
  };
  ```

### Never Hardcode `delta_t` to 0.25
`tokio::time::interval` defaults to `MissedTickBehavior::Burst`. If the Tokio runtime stalls — plausible under 24 concurrent segment tasks competing with blocking disk writes — the missed ticks then fire back-to-back with an actual elapsed time near zero. Dividing by a hardcoded `0.25` in that window reports a speed several times higher than reality; dividing by the true near-zero `delta_t` reports a near-infinite spike. Either way the EMA then carries the bad value for several seconds, which is the exact jitter this section exists to remove.
- Set `interval.set_missed_tick_behavior(MissedTickBehavior::Delay)` (see §4).
- Compute `delta_t` from a stored `Instant`, and skip the update entirely when `delta_t < 0.05` rather than dividing by it.

*Result:* Silky-smooth, readable speed and ETA values in the React UI.

---

## 7. Stream Robustness: Chunked Streams & 0-Byte Handling

### Edge Cases
- **`Transfer-Encoding: chunked` / Unknown `Content-Length`:**
  - Servers serving dynamic files or real-time streams do not send `Content-Length`.
  - Attempting `total_bytes / 8` panics with divide-by-zero, and `fs4::allocate` cannot allocate unknown sizes.
  - **Mitigation:** Skip `fs4::allocate`, downgrade immediately to a single connection stream to EOF, and emit progress with indeterminate progress (e.g. `"Downloaded 42 MB"` with no percentage or ETA).
- **0-Byte Files:**
  - If `Content-Length == 0`, immediately create the empty file, mark the download `Completed`, and skip spawning network workers.

---

## 8. Lifecycle: Durable Offsets on Exit and on Crash

### The Invariant
Exactly one rule governs resume correctness:

> **A byte offset recorded in `queue.json` must never exceed the bytes durably on disk in the `.part` file.**

Violate it in one direction and resume re-downloads a few megabytes — harmless. Violate it in the other and resume seeks *past* data that was never written, leaving a permanent zero-filled hole in the middle of the file. Nothing detects that: the file is the right length, the rename succeeds, and the corruption only surfaces when the user opens the archive months later. Every mechanism below exists to keep the inequality pointing the safe way.

### 8.1 Clean Quit (Tray → Quit)
An immediate `std::process::exit(0)` can leave uncommitted progress offsets.
- **Mitigation:** Intercept the quit command in `lib.rs`:
  1. Trigger the download's root `CancellationToken`.
  2. Await segment task cancellation, then call **`file.sync_data().await`**.
  3. Commit the exact byte positions to `queue.json.tmp` $\rightarrow$ atomic rename.
  4. Terminate the process cleanly.

> **`flush()` is not `sync_data()`.** `tokio::fs::File::flush()` only drains the userspace buffer into the OS page cache — the data is visible to other processes but is *not* on the physical device. A power cut or kernel panic between step 2 and step 3 discards it while `queue.json` still claims those bytes landed, producing exactly the hole described above. `sync_data()` issues the `fdatasync` that makes the offsets true. Use it, and order it strictly before the `queue.json` write.

### 8.2 Crash, `kill -9`, and Power Loss
§8.1 only runs on a *clean* quit. The restart state table's `Downloading → Interrupted → auto-resume` path exists precisely for the cases where no shutdown hook executes at all, and those get no flush, no final commit, and no ordering guarantee.
- **Mitigation:** make the checkpoint itself durable rather than relying on shutdown. Every ~8 MB per segment (or every ~5 seconds, whichever comes first):
  1. `file.sync_data().await`
  2. then write the new offsets via the atomic `tmp` + rename path.
- Between checkpoints the recorded offset simply lags the true write frontier. On resume the segment re-downloads at most one checkpoint interval — the harmless direction of the invariant.
- Do **not** checkpoint on every chunk. That fsyncs thousands of times per second and destroys throughput; the whole point of the interval is to bound the loss without bounding the speed.

---

## 9. Desktop Polish: Native Completion Notifications

### The Problem
When the application is minimized to the system tray, completed downloads finish silently, leaving the user unaware until they manually restore the window.

### Implementation
- Add `tauri-plugin-notification` to `Cargo.toml` and grant `"notification:default"` in `src-tauri/capabilities/default.json`.
- When a download transitions to `Completed` while the window is hidden/minimized:
  ```rust
  use tauri_plugin_notification::NotificationExt;

  app.notification()
      .builder()
      .title("Download Complete")
      .body(&format!("{} has finished downloading.", download.filename))
      .show()?;
  ```

---

## 10. Resume Validation: `If-Range` Instead of Manual Header Comparison

### The Problem
PLAN.md stores `ETag` / `Last-Modified` at download start and, on resume, re-fetches the headers and compares them by hand to decide whether the partial file is still valid. That is an extra round trip, a second code path that can disagree with the first, and a comparison whose edge cases (weak vs. strong ETags, absent ETag, date-only precision) all have to be handled locally.

### Implementation
HTTP already specifies this exact operation. Send the stored validator with the ranged request:
```
Range: bytes=1048576-2097151
If-Range: "686897696a7c876b7e"
```
- Resource unchanged → server returns `206 Partial Content`. Resume normally.
- Resource changed → server ignores the `Range` and returns `200 OK` with the full body.

The `200` case is already handled: it is the same signal as the strict-206 validation in PLAN.md, which cancels sibling segments and restarts from byte 0. So this collapses into existing logic — one header replaces a round trip, a stored-comparison branch, and its edge cases.

- Prefer the `ETag` as the validator; fall back to `Last-Modified` when no `ETag` was issued.
- If the server sent neither, there is nothing to validate against — discard the partial file and restart from 0 rather than resuming blind.

---

## 11. Throttle: Bounding the Semaphore's Permit Bank

### Two Failure Modes in the Token-Bucket Ticker
The throttle adds `(cap_kb_sec * 1024) / 10` permits every 100 ms and has segment tasks `acquire_many(chunk_len)` before writing. Two problems:

**1. Unbounded accumulation.** Permits are never capped. With the app idle at a 5 MB/s cap for 60 seconds, the semaphore banks 300 MB of permits. The next download then runs at full line rate until that bank drains, completely ignoring the cap for its first several seconds — which is most of the transfer for a small file.

**2. Oversized single acquisitions.** `acquire_many(n)` blocks until `n` permits exist simultaneously. A 64 KB chunk against a 32 KB/s cap needs 20 consecutive ticks to accumulate, so the segment transfers in visible 2-second lurches instead of a smooth trickle.

### Implementation
- **Cap the bank at roughly two ticks' worth.** Before adding permits, if `semaphore.available_permits()` already exceeds `2 * per_tick`, add nothing (or `forget_permits` on the excess). A burst allowance of ~200 ms is enough to absorb scheduling jitter without defeating the cap.
- **Acquire in fixed slices.** Rather than one `acquire_many(chunk_len)`, consume the chunk in 16 KB slices, acquiring before each. Throughput is identical and the pacing is smooth regardless of how large a chunk `reqwest` hands back.
- Keep the unlimited path branch-free: when no cap is configured, bypass the semaphore entirely rather than acquiring against an effectively infinite bank.

---

## 12. Integrity: Verify Length Before the Final Rename

### The Problem
The `.part` file is pre-allocated to the full size, so it is *always* the correct length on disk regardless of how much was actually written. A segment that terminated early, an off-by-one in the range arithmetic, or a checkpoint hole (§8) all produce a file that passes every implicit check and gets renamed to the final name as though it succeeded.

### Implementation
Before `.part` $\rightarrow$ final rename, assert that the sum of bytes written across all segments equals the `Content-Length` established at start. Mismatch → keep the `.part` file and mark the download `Failed` with a retry available, rather than renaming a corrupt file into place.

This is deliberately **not** checksum verification (explicitly out of scope): no hashing, no extra pass over the data. It is one comparison of two integers already in memory, and it is the only thing standing between a silent range-arithmetic bug and a file the user believes is good.

---

## 13. Concurrency: Lock Discipline for `AppState`

### The Deadlock This Prevents
`AppState` is read and written by up to 24 segment tasks, the 250 ms ticker, and every IPC command handler. Holding a `std::sync::Mutex` guard across an `.await` point is the standard way this shape deadlocks: the task parks while holding the lock, the executor schedules another task on that thread, and it blocks forever on the same mutex. The pressure is now higher because §6's EMA needs mutable per-download `prev_speed` inside the ticker and §8 awaits task shutdown while touching the same state.

### Implementation
- **Hot path is lock-free.** Per-segment byte counters are `Arc<AtomicU64>`, bumped with `fetch_add(n, Ordering::Relaxed)`. No segment task ever takes a lock to report progress.
- **Queue metadata behind `std::sync::Mutex`, with no `.await` inside the guard.** Take the lock, clone or mutate, drop it, *then* await. If a critical section genuinely must await, that is the signal to use `tokio::sync::Mutex` for that specific structure — not to hold the std one longer.
- **The ticker's EMA state lives in the ticker task**, not in shared state. It is the only reader and writer, so it needs no synchronization at all.

---

## 14. Verification: The Three Functions Worth Unit-Testing

Most of this engine is I/O and is verified by the milestone checks in PLAN.md. Three pure functions are not, and each has a failure mode that is silent rather than loud:

1. **`Content-Disposition` parsing** — quoted strings, unquoted tokens, RFC 5987 `filename*=UTF-8''…` percent-decoding, both parameters present (`filename*` wins), and the traversal-rejection cases from §3.1.
2. **`resolve_unique_path`** — collision suffixing, extensionless names, dotfiles, and names containing dots that are not extensions (`archive.tar.gz` must not become `archive (1).gz`).
3. **Segment range arithmetic** — `Range` headers are *inclusive* on both ends, so the boundary between segments is the classic off-by-one; the last segment must absorb the remainder when the size is not divisible by the segment count; and the small-file and single-segment cases must not produce inverted or empty ranges.

One `#[cfg(test)]` module in `download.rs`. No framework, no fixtures, no mocking — these are pure functions taking strings and integers.

---

## 15. Phase 2 / Stretch: Dynamic Work-Stealing (Slow Segment Mitigation)

### The "Straggler Segment" Problem
In fixed-range partitioning (e.g. 8 equal slices), 7 segments might finish in 30 seconds, while 1 segment connects to a congested CDN node or suffers high packet loss. The download remains stalled at ~95% with 7 idle connection slots.

### Future Mitigation Options
1. **Dynamic Chunk Pool:** Divide the total file into 2 MB–4 MB chunks placed in a shared queue; workers pull the next chunk as soon as they finish their current one.
2. **Dynamic Range Splitting (Work-Stealing):** When an idle worker has no work and another worker has $> 10\text{ MB}$ remaining at low speed, the idle worker splits the remaining range in half and downloads the upper partition.

---

## 16. Anti-Bot Challenges: What Actually Works (Measured)

### The Problem
Some hosts answer every request with `403` and `cf-mitigated: challenge`. `file-examples.com` is one; the block covers even its homepage.

### What Was Tried, and What Happened
Measured against `https://file-examples.com/storage/.../file_example_MP4_480_1_5MG.mp4` on 2026-09-09:

| Attempt | Result |
| :--- | :--- |
| Bare request | `403` |
| Browser `User-Agent` only | `403` |
| Full browser header set (`Accept`, `Accept-Language`, `Sec-Fetch-*`, `Upgrade-Insecure-Requests`) | `403` |
| `Referer` from the site's own origin | `403` |
| Forced HTTP/1.1 | `403` |
| Forced HTTP/2 | `403` |
| **TLS fingerprint impersonation** (`curl_cffi`, `chrome124` / `chrome131` / `chrome`) | **`403`** |

The last row is the decisive one. It rules out the fix that looks most promising on paper — swapping `reqwest` for an impersonating fork such as `rquest` to match Chrome's JA3/JA4 TLS ClientHello. **That is a large dependency change that would not have worked.** This host uses a *managed* challenge: it requires actually executing JavaScript and completing Turnstile. No HTTP client can pass it, regardless of how convincingly it disguises itself.

Worth keeping in proportion: this is not the common case. Measured the same day, `proof.ovh.net` (`206`), `speed.cloudflare.com` (`200`), GitHub release assets (`206`), and `cdn.kernel.org` (`206`) all worked with no special handling. Cloudflare being in front of a host is not itself a problem — only an enabled challenge is.

### What Actually Works: Replay a Solved Session
This is precisely what commercial download managers do, and it is worth being clear that they do not solve challenges either:

- **IDM** ships a browser extension (IDM Integration Module). The *browser* solves the challenge, and the extension hands IDM the URL together with that browser's cookies, `User-Agent` and `Referer`. IDM replays a session it did not earn.
- **JDownloader** does the same, and additionally opens a window for manual CAPTCHA solving.

So the correct design is not to defeat the challenge but to accept a session from something that already did. fetchd implements the same mechanism one step less automatically, via a Netscape `cookies.txt` file (`src-tauri/src/cookies.rs`, `Settings::cookies_file`). Every "export cookies" browser extension writes that format, as does `yt-dlp --cookies-from-browser`. It is plain text, so it costs no dependencies — whereas reading Chromium's own cookie database on Linux would mean AES-decrypting it with a key from the Secret Service portal.

Two constraints that make this fail silently if ignored:
1. **The `User-Agent` must match the browser the cookies came from.** A `cf_clearance` cookie is bound to the exact agent (and the IP) that earned it. Hence `Settings::user_agent`.
2. **Cookies must not leak across hosts.** They are attached as a per-download default header; `reqwest`'s `remove_sensitive_headers` strips `Cookie` on a cross-host redirect and on an HTTPS→HTTP downgrade (verified in `reqwest-0.13.4/src/redirect.rs:239`).

### Deliberately Not Done
- **Browser extension.** The fully-automatic version, and exactly what IDM does — but explicitly out of scope in PLAN.md.
- **Reading the Chromium cookie DB directly.** Would remove the manual export step, at the cost of D-Bus keyring access plus PBKDF2 and AES-CBC (three or four new crates). Revisit only if the `cookies.txt` step proves annoying in practice.
- **TLS impersonation.** Measured above. Does not work on managed challenges.

---

## 17. Priority & Implementation Roadmap

Ordered by *what breaks if it is missing*, not by effort. The P0 band is silent data corruption: nothing in the UI reports it and the user finds out when they open the file. The P1 band is downloads that fail or hang when they did not have to. Everything below that is quality.

| Priority | Refinement / Feature | § | Primary Module | Failure if omitted |
| :---: | :--- | :---: | :--- | :--- |
| **P0** | Durable checkpoints — `sync_data()` before offset commit, crash path included | 8 | `download.rs` | **Silent corruption.** Resume seeks past unwritten bytes, leaving a zero-filled hole |
| **P0** | Create `.part` in target folder (avoid `EXDEV`) | 2 | `download.rs` | Rename fails across mounts; falls back to a multi-GB copy the design exists to avoid |
| **P0** | Path-traversal guard on `Content-Disposition` | 3.1 | `download.rs` | **Remote server writes outside the download directory** |
| **P0** | No `gzip`/`brotli` features on `reqwest` | 1 | `Cargo.toml` | **Silent corruption.** Decompressed stream desyncs every seek-write offset |
| **P0** | Verify total length before final rename | 12 | `download.rs` | A truncated download is renamed into place as though it succeeded |
| **P1** | `http1_only()` on multi-segment client | 1 | `download.rs` | Segmentation provides no speedup — all 8 streams share one TCP socket |
| **P1** | Chunk-read timeout (stall detection) | 5.2 | `download.rs` | Black-holed socket hangs forever; the retry ladder never runs |
| **P1** | Browser `User-Agent` string | 5.1 | `download.rs` | `403 Forbidden` from Cloudflare-fronted hosts |
| **P1** | `Content-Disposition` & collision naming | 3 | `download.rs` | Files saved as `dl`, or existing files overwritten |
| **P1** | `If-Range` resume validation | 10 | `download.rs` | Stale and fresh bytes merge into one corrupt file after the remote changes |
| **P1** | Lock discipline — no `.await` under a `Mutex` guard | 13 | `state.rs` | Deadlock under concurrent load; the app hangs with downloads active |
| **P2** | Physical allocation (`fs4::allocate`) | 2 | `download.rs` | `ENOSPC` hours in instead of at second 0 (Linux/Unix only) |
| **P2** | Chunked stream & 0-byte guards | 7 | `download.rs` | Divide-by-zero panic on servers omitting `Content-Length` |
| **P2** | Retry backoff schedule + reset on progress | 5.3 | `download.rs` | Long downloads fail permanently on transient Wi-Fi blips |
| **P2** | Central 250 ms progress ticker | 4 | `state.rs` | IPC flood; UI frame drops and input lag |
| **P2** | Throttle permit bank cap | 11 | `throttle.rs` | Bandwidth cap ignored for the first seconds after any idle period |
| **P2** | Graceful shutdown flush on tray quit | 8.1 | `lib.rs` | Lost progress on clean exit (crash path is covered by the P0 row) |
| **P2** | Unit tests for the three pure functions | 14 | `download.rs` | Off-by-one range bugs surface as corrupt files, not test failures |
| **P3** | Smoothed speed (EMA) & ETA, with measured `delta_t` | 6 | `state.rs` | Erratic speed readout; cosmetic |
| **P3** | Native completion notifications | 9 | `tray.rs` | User unaware of completion while minimized |
| **P4** | Work-stealing / dynamic chunking | 15 | `download.rs` | Stalls near 100% on one congested segment. Phase 2 stretch goal |
