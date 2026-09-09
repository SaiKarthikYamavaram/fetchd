//! Download engine.
//!
//! Covers M1-M4: filename resolution, redirects, retry ladder, disk-space
//! check, HEAD-blocked probe fallback, segmented transfer with strict `206`
//! validation and single-connection downgrade, pre-allocated seek-writes,
//! durable checkpoints, and `If-Range` resume.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use fs4::tokio::AsyncFileExt;
use futures_util::StreamExt;
use reqwest::header::{
    ACCEPT_RANGES, CONTENT_DISPOSITION, CONTENT_RANGE, ETAG, IF_RANGE, LAST_MODIFIED, RANGE,
};
use reqwest::{Client, StatusCode, Url};
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

use crate::throttle::Throttle;

/// Many CDNs reject the default `reqwest/<version>` agent with 403.
pub const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 \
                              (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36";

/// A chunk read that produces nothing for this long is treated as a failure.
/// A dropped Wi-Fi link or a rebound CGNAT lease black-holes an established
/// socket without ever raising an error, so without this the task waits
/// forever and the retry ladder below is never entered.
pub const CHUNK_TIMEOUT: Duration = Duration::from_secs(30);

/// Backoff between segment retries, in seconds. The old 1/2/4 schedule burned
/// its whole budget in 7 seconds and died on any Wi-Fi blip or lid close;
/// this survives roughly 67 seconds of disconnection.
pub const BACKOFF_SECS: [u64; 5] = [2, 5, 10, 20, 30];

/// A segment that transfers this much after a failure has its retry budget
/// reset. The budget is per stall episode, not per download lifetime.
pub const RETRY_RESET_BYTES: u64 = 8 * 1024 * 1024;

/// Checkpoint cadence. Each checkpoint costs an `fdatasync`, so this trades a
/// bounded amount of re-downloaded data against write throughput.
pub const CHECKPOINT_BYTES: u64 = 8 * 1024 * 1024;

/// Below this, the extra sockets cost more than they gain.
pub const MIN_SEGMENTED_SIZE: u64 = 4 * 1024 * 1024;

pub const DEFAULT_SEGMENTS: u32 = 8;
pub const MAX_SEGMENTS: u32 = 8;

/// Refuse to start unless this much space remains free beyond the file itself.
const DISK_HEADROOM: u64 = 64 * 1024 * 1024;

const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);
const MAX_REDIRECTS: usize = 10;

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// A browser session captured for one download.
///
/// This is the whole mechanism behind getting past an interactive anti-bot
/// challenge: the browser solves the challenge and earns a cookie (a
/// `cf_clearance`, say), and fetchd replays that exact session. Because the
/// cookie is bound to the `User-Agent` and `Referer` that earned it, all three
/// travel together. A default session (no cookie, no referer) is the ordinary
/// case for links that need no authentication.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Session {
    pub user_agent: String,
    pub cookie: Option<String>,
    pub referer: Option<String>,
    /// Proxy URL applied to both clients; `None` is a direct connection.
    #[serde(default)]
    pub proxy: Option<String>,
}

impl Default for Session {
    fn default() -> Self {
        Session {
            user_agent: USER_AGENT.to_string(),
            cookie: None,
            referer: None,
            proxy: None,
        }
    }
}

impl Session {
    pub fn with_agent(user_agent: Option<String>) -> Self {
        Session {
            user_agent: user_agent.unwrap_or_else(|| USER_AGENT.to_string()),
            ..Session::default()
        }
    }
}

/// Headers a real browser always sends. Some CDNs reject a request carrying a
/// browser `User-Agent` but none of its companions, so send the whole set.
///
/// `Accept-Encoding` is deliberately absent: advertising `gzip` would invite a
/// compressed body, and since decompression is off (see `build_client`) the
/// byte offsets would no longer line up with `Content-Length`.
fn browser_headers(session: &Session) -> reqwest::header::HeaderMap {
    use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, ACCEPT_LANGUAGE, COOKIE, REFERER};

    let mut headers = HeaderMap::new();

    // Replays a session a browser already established. reqwest strips both of
    // these automatically on a cross-host redirect or an HTTPS->HTTP
    // downgrade, so a cookie for one site cannot leak to another.
    if let Some(cookie) = &session.cookie {
        if let Ok(value) = HeaderValue::from_str(cookie) {
            headers.insert(COOKIE, value);
        }
    }
    if let Some(referer) = &session.referer {
        if let Ok(value) = HeaderValue::from_str(referer) {
            headers.insert(REFERER, value);
        }
    }

    headers.insert(
        ACCEPT,
        HeaderValue::from_static(
            "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8",
        ),
    );
    headers.insert(ACCEPT_LANGUAGE, HeaderValue::from_static("en-US,en;q=0.9"));
    headers.insert("Sec-Fetch-Dest", HeaderValue::from_static("document"));
    headers.insert("Sec-Fetch-Mode", HeaderValue::from_static("navigate"));
    headers.insert("Sec-Fetch-Site", HeaderValue::from_static("none"));
    headers.insert("Upgrade-Insecure-Requests", HeaderValue::from_static("1"));
    headers
}

/// Turn an unsuccessful response into a message that says what to do next.
///
/// A Cloudflare/Akamai interactive challenge is not a transport failure and no
/// retry or header tweak will clear it: the server wants a browser to run a
/// script. Saying "403 Forbidden" hides that and sends the user hunting for a
/// bug in the downloader.
fn describe_http_error(status: StatusCode, headers: &reqwest::header::HeaderMap) -> String {
    let challenged = headers.contains_key("cf-mitigated")
        || headers.contains_key("cf-chl-bypass")
        || headers
            .get(reqwest::header::SERVER)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.eq_ignore_ascii_case("cloudflare"))
            && matches!(status, StatusCode::FORBIDDEN | StatusCode::SERVICE_UNAVAILABLE);

    if challenged {
        return format!(
            "{status}: the site is serving an anti-bot browser challenge. \
             This link cannot be fetched by any download manager without a \
             browser session; open it in a browser and download from there, \
             or use a direct link that is not behind the challenge."
        );
    }

    match status {
        StatusCode::FORBIDDEN => format!("{status}: the server refused the request. \
             The link may be expired, region-locked, or require a login."),
        StatusCode::NOT_FOUND => format!("{status}: no such file at this URL."),
        StatusCode::UNAUTHORIZED => format!("{status}: this URL needs authentication."),
        other => format!("server returned an error: {other}"),
    }
}

/// Client for probes and single-connection transfers.
///
/// `gzip`/`brotli` are deliberately absent from the `reqwest` feature list:
/// automatic decompression desyncs the stream from the `Content-Length` and
/// `Content-Range` byte offsets that seek-writes depend on.
pub fn build_client(session: &Session) -> Result<Client, String> {
    apply_proxy(
        Client::builder(),
        session,
    )
        .user_agent(session.user_agent.clone())
        .default_headers(browser_headers(session))
        .connect_timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::limited(MAX_REDIRECTS))
        .build()
        .map_err(|e| format!("failed to build HTTP client: {e}"))
}

/// Client for segment workers.
///
/// `http1_only()` is the whole point of segmentation: `reqwest` negotiates
/// HTTP/2 by default, and HTTP/2 multiplexes every stream over one TCP
/// connection. Eight segments sharing one socket share one congestion window,
/// which is exactly what a download manager exists to avoid.
pub fn build_segment_client(session: &Session) -> Result<Client, String> {
    apply_proxy(
        Client::builder(),
        session,
    )
        .user_agent(session.user_agent.clone())
        .default_headers(browser_headers(session))
        .http1_only()
        .connect_timeout(Duration::from_secs(10))
        // Keeps sockets warm across retries. This is NOT what creates the
        // parallelism -- concurrent HTTP/1.1 requests already open their own.
        .pool_max_idle_per_host(MAX_SEGMENTS as usize)
        .redirect(reqwest::redirect::Policy::limited(MAX_REDIRECTS))
        .build()
        .map_err(|e| format!("failed to build segment client: {e}"))
}

/// Route a client through the configured proxy, if any. An unparseable proxy
/// URL is ignored rather than failing every download.
fn apply_proxy(builder: reqwest::ClientBuilder, session: &Session) -> reqwest::ClientBuilder {
    match session.proxy.as_deref().filter(|p| !p.is_empty()) {
        Some(url) => match reqwest::Proxy::all(url) {
            Ok(proxy) => builder.proxy(proxy),
            Err(e) => {
                eprintln!("fetchd: ignoring invalid proxy {url}: {e}");
                builder
            }
        },
        None => builder,
    }
}

/// Sub-folder for a filename's type, used when category sorting is on.
/// Mirrors the type buckets the UI and extension already use.
pub fn category_for(filename: &str) -> &'static str {
    let ext = filename.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "mp4" | "mkv" | "webm" | "avi" | "mov" | "flv" | "m4v" | "ts" => "Video",
        "mp3" | "flac" | "wav" | "aac" | "ogg" | "m4a" | "opus" => "Audio",
        "zip" | "tar" | "gz" | "xz" | "7z" | "rar" | "bz2" | "zst" => "Archives",
        "pdf" | "doc" | "docx" | "epub" | "txt" | "odt" | "rtf" => "Documents",
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" | "avif" => "Images",
        "iso" | "img" | "dmg" | "exe" | "appimage" | "deb" | "rpm" | "msi" => "Programs",
        _ => "Other",
    }
}

/// Reject anything that is not plain HTTP(S) before touching the network.
pub fn validate_url(raw: &str) -> Result<Url, String> {
    let url = Url::parse(raw).map_err(|e| format!("invalid URL: {e}"))?;
    match url.scheme() {
        "http" | "https" => Ok(url),
        other => Err(format!("unsupported scheme `{other}` (only http/https)")),
    }
}

// ---------------------------------------------------------------------------
// Filename resolution
// ---------------------------------------------------------------------------

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Minimal percent-decoder for RFC 5987 `filename*` values and URL path
/// segments.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Split a header value on `;`, ignoring separators inside quoted strings.
fn split_params(header: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut escaped = false;

    for c in header.chars() {
        if escaped {
            current.push(c);
            escaped = false;
        } else if c == '\\' && in_quotes {
            escaped = true;
        } else if c == '"' {
            in_quotes = !in_quotes;
            current.push(c);
        } else if c == ';' && !in_quotes {
            parts.push(std::mem::take(&mut current));
        } else {
            current.push(c);
        }
    }
    parts.push(current);
    parts
}

fn unquote(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.len() >= 2 && trimmed.starts_with('"') && trimmed.ends_with('"') {
        let inner = &trimmed[1..trimmed.len() - 1];
        let mut out = String::with_capacity(inner.len());
        let mut escaped = false;
        for c in inner.chars() {
            if escaped {
                out.push(c);
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else {
                out.push(c);
            }
        }
        out
    } else {
        trimmed.to_string()
    }
}

/// Extract a filename from a `Content-Disposition` header (RFC 6266).
///
/// `filename*` (RFC 5987) wins over a plain `filename` when both are present.
pub fn filename_from_content_disposition(header: &str) -> Option<String> {
    let mut plain = None;

    for part in split_params(header) {
        let Some((key, value)) = part.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();

        if key == "filename*" {
            // ext-value is `charset'language'percent-encoded-value`.
            let encoded = match value.split_once('\'').and_then(|(_, r)| r.split_once('\'')) {
                Some((_lang, v)) => v,
                None => value,
            };
            let decoded = percent_decode(encoded.trim());
            if !decoded.is_empty() {
                return Some(decoded);
            }
        } else if key == "filename" && plain.is_none() {
            let v = unquote(value);
            if !v.is_empty() {
                plain = Some(v);
            }
        }
    }

    plain
}

/// Reduce an untrusted name to a single safe path component.
///
/// `Content-Disposition` is supplied by the remote server, so a value like
/// `../../.bashrc` must never escape the download directory. Stripping invalid
/// characters is not enough on its own -- it leaves `..` fully intact -- so the
/// directory portion is dropped first and traversal names are rejected
/// outright. Returns `None` when nothing usable survives.
pub fn sanitize_filename(raw: &str) -> Option<String> {
    // Drop any directory portion using both separators: a Windows-style
    // `..\..\evil.exe` is not split by `Path::file_name` on Unix.
    let last = raw.rsplit(['/', '\\']).next().unwrap_or("");

    let cleaned: String = last
        .chars()
        .filter(|c| !c.is_control())
        .map(|c| match c {
            ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            other => other,
        })
        .collect();

    // Windows silently drops trailing dots and spaces; do it explicitly so the
    // name we record matches the name on disk.
    let name = cleaned.trim().trim_end_matches(['.', ' ']).trim();

    if name.is_empty() || name == "." || name == ".." || name.starts_with('.') {
        return None;
    }
    Some(name.to_string())
}

/// Pick a filename: `Content-Disposition`, then the final redirect URL's path,
/// then a fixed fallback.
pub fn resolve_filename(content_disposition: Option<&str>, final_url: &Url) -> String {
    content_disposition
        .and_then(filename_from_content_disposition)
        .and_then(|name| sanitize_filename(&name))
        .or_else(|| {
            let last = final_url.path().rsplit('/').next().unwrap_or("");
            sanitize_filename(&percent_decode(last))
        })
        .unwrap_or_else(|| "download.bin".to_string())
}

/// Auto-suffix on collision: `file.zip` -> `file (1).zip`, matching browsers.
pub fn resolve_unique_path(base_dir: &Path, filename: &str) -> PathBuf {
    let target = base_dir.join(filename);
    if !target.exists() && !part_path(&target).exists() {
        return target;
    }

    let path = Path::new(filename);
    // `file_stem` splits at the LAST dot, so `archive.tar.gz` becomes
    // `archive.tar` + `gz` and suffixes to `archive.tar (1).gz`.
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("download");
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");

    for counter in 1.. {
        let candidate = if ext.is_empty() {
            format!("{stem} ({counter})")
        } else {
            format!("{stem} ({counter}).{ext}")
        };
        let target = base_dir.join(candidate);
        if !target.exists() && !part_path(&target).exists() {
            return target;
        }
    }
    unreachable!("counter range is unbounded")
}

/// `foo.zip` -> `foo.zip.part`, kept in the same directory as the final file.
///
/// The `.part` must live on the destination volume: a rename across mounts
/// fails with `EXDEV` and degrades into a full multi-gigabyte copy, which is
/// exactly what the pre-allocated single-file design exists to avoid. It also
/// means the free-space check below measures the drive we actually write to.
pub fn part_path(final_path: &Path) -> PathBuf {
    let mut name = final_path.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    final_path.with_file_name(name)
}

// ---------------------------------------------------------------------------
// Probe
// ---------------------------------------------------------------------------

/// What the server will tell us before we commit to a transfer.
#[derive(Debug, Clone)]
pub struct RemoteInfo {
    pub total: Option<u64>,
    pub supports_ranges: bool,
    pub filename: String,
    /// `ETag` if present, else `Last-Modified`. Replayed as `If-Range`.
    pub validator: Option<String>,
}

/// Parse the total size out of a `Content-Range: bytes 0-0/12345` header.
/// A `/*` total means the server will not say, so the size stays unknown.
pub fn parse_content_range_total(value: &str) -> Option<u64> {
    let (_unit, rest) = value.trim().split_once(' ')?;
    let (_range, total) = rest.rsplit_once('/')?;
    let total = total.trim();
    if total == "*" {
        return None;
    }
    total.parse().ok()
}

fn header_str(headers: &reqwest::header::HeaderMap, name: reqwest::header::HeaderName) -> Option<String> {
    headers.get(name).and_then(|v| v.to_str().ok()).map(str::to_string)
}

/// Read `Content-Length` from the headers rather than calling
/// `Response::content_length()`.
///
/// The latter reports the length of the *body* being received, which for a
/// `HEAD` reply is always 0 — the header is the only place the real size
/// lives. Trusting the method there makes every probe report a zero-byte file.
fn content_length(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    header_str(headers, reqwest::header::CONTENT_LENGTH)?.trim().parse().ok()
}

/// Ask the server for size, range support and a filename.
///
/// Tries `HEAD` first. S3 pre-signed URLs and several CDNs answer `HEAD` with
/// 403/405/501 (or reject it outright), so on failure this falls back to a
/// `GET` with `Range: bytes=0-0`: a `206` reply proves range support and its
/// `Content-Range` carries the total size, for the cost of one byte.
pub async fn probe(client: &Client, url: &Url) -> Result<RemoteInfo, String> {
    if let Ok(response) = client.head(url.clone()).send().await {
        if response.status().is_success() {
            let headers = response.headers().clone();
            let cd = header_str(&headers, CONTENT_DISPOSITION);
            let supports_ranges = header_str(&headers, ACCEPT_RANGES)
                .map(|v| v.to_ascii_lowercase().contains("bytes"))
                .unwrap_or(false);

            return Ok(RemoteInfo {
                total: content_length(&headers),
                supports_ranges,
                filename: resolve_filename(cd.as_deref(), response.url()),
                validator: header_str(&headers, ETAG)
                    .or_else(|| header_str(&headers, LAST_MODIFIED)),
            });
        }
    }

    // HEAD blocked or errored: one-byte ranged GET tells us everything.
    let response = client
        .get(url.clone())
        .header(RANGE, "bytes=0-0")
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;

    let status = response.status();
    if !status.is_success() {
        return Err(describe_http_error(status, response.headers()));
    }
    let headers = response.headers().clone();
    let cd = header_str(&headers, CONTENT_DISPOSITION);
    let filename = resolve_filename(cd.as_deref(), response.url());
    let validator = header_str(&headers, ETAG).or_else(|| header_str(&headers, LAST_MODIFIED));

    if status == StatusCode::PARTIAL_CONTENT {
        let total = header_str(&headers, CONTENT_RANGE)
            .as_deref()
            .and_then(parse_content_range_total);
        Ok(RemoteInfo { total, supports_ranges: true, filename, validator })
    } else {
        // Server ignored the Range and sent the whole body: no range support.
        Ok(RemoteInfo {
            total: content_length(&headers),
            supports_ranges: false,
            filename,
            validator,
        })
    }
}

// ---------------------------------------------------------------------------
// Segment planning
// ---------------------------------------------------------------------------

/// Split `total` bytes into inclusive `[start, end]` byte ranges.
///
/// `Range` headers are inclusive at both ends, so the last byte of segment N
/// is one less than the first byte of segment N+1. The final segment absorbs
/// the remainder when the size does not divide evenly.
pub fn plan_segments(total: u64, segments: u32) -> Vec<(u64, u64)> {
    if total == 0 {
        return Vec::new();
    }
    let n = if total < MIN_SEGMENTED_SIZE {
        1
    } else {
        segments.clamp(1, MAX_SEGMENTS).min(total as u32).max(1)
    } as u64;

    let per = total / n;
    let mut ranges = Vec::with_capacity(n as usize);
    for i in 0..n {
        let start = i * per;
        let end = if i == n - 1 { total - 1 } else { start + per - 1 };
        ranges.push((start, end));
    }
    ranges
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug)]
enum SegErr {
    /// Transient: worth another attempt after a backoff.
    Retryable(String),
    /// Permanent: no amount of retrying will help.
    Fatal(String),
    /// The server answered a ranged request with `200 OK`, meaning it ignored
    /// `Range` entirely and is streaming the whole file from byte 0. Writing
    /// that stream at a segment offset would corrupt the file.
    RangeIgnored,
    Cancelled,
}

impl SegErr {
    fn message(&self) -> String {
        match self {
            SegErr::Retryable(m) | SegErr::Fatal(m) => m.clone(),
            SegErr::RangeIgnored => "server ignored Range header".into(),
            SegErr::Cancelled => "cancelled".into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Transfer
// ---------------------------------------------------------------------------

/// One segment's byte counts.
///
/// `live` counts everything handed to the kernel; `durable` counts only what
/// an `fdatasync` has confirmed is on the device. They exist separately
/// because they answer different questions and have different consequences:
///
/// - The UI wants `live`. Showing only synced bytes would make the progress
///   bar jump in 8 MB steps.
/// - `queue.json` must record `durable`. An offset ahead of the device is the
///   dangerous direction: after a power cut, resume would seek past bytes that
///   were never written and leave a permanent zero-filled hole in the file.
///   Recording behind the frontier merely re-downloads a little.
#[derive(Debug, Default)]
pub struct SegmentProgress {
    live: AtomicU64,
    durable: AtomicU64,
}

impl SegmentProgress {
    fn starting_at(offset: u64) -> Self {
        SegmentProgress {
            live: AtomicU64::new(offset),
            durable: AtomicU64::new(offset),
        }
    }

    fn add(&self, n: u64) {
        self.live.fetch_add(n, Ordering::Relaxed);
    }

    /// Set both counters to an absolute value. Used by the yt-dlp path, which
    /// reports cumulative downloaded bytes rather than deltas.
    fn set(&self, n: u64) {
        self.live.store(n, Ordering::Relaxed);
        self.durable.store(n, Ordering::Relaxed);
    }

    /// Call only after the write is on the device.
    fn commit(&self) {
        self.durable.store(self.live.load(Ordering::Relaxed), Ordering::Relaxed);
    }

    fn live(&self) -> u64 {
        self.live.load(Ordering::Relaxed)
    }

    fn durable(&self) -> u64 {
        self.durable.load(Ordering::Relaxed)
    }

    /// A retry restarts from the last durable point, so anything written but
    /// not yet synced is discarded and fetched again.
    fn rewind_to_durable(&self) {
        self.live.store(self.durable.load(Ordering::Relaxed), Ordering::Relaxed);
    }

    fn reset(&self) {
        self.live.store(0, Ordering::Relaxed);
        self.durable.store(0, Ordering::Relaxed);
    }
}

/// Lock-free progress: one counter pair per segment.
///
/// Per-segment rather than one shared total, so a retry that resumes at
/// `start + done` neither loses nor double-counts. A single shared counter
/// would over-report after any retry — enough to make the final length check
/// reject a perfectly good file.
#[derive(Clone)]
pub struct Progress {
    counters: Arc<Vec<Arc<SegmentProgress>>>,
}

impl Progress {
    pub fn new(n: usize) -> Self {
        Progress {
            counters: Arc::new((0..n.max(1)).map(|_| Arc::new(SegmentProgress::default())).collect()),
        }
    }

    /// Rebuild from persisted per-segment offsets so a resumed download picks
    /// up exactly where the last durable checkpoint left it.
    pub fn resumed(done: &[u64]) -> Self {
        Progress {
            counters: Arc::new(
                done.iter()
                    .map(|d| Arc::new(SegmentProgress::starting_at(*d)))
                    .collect::<Vec<_>>(),
            ),
        }
    }

    fn counter(&self, i: usize) -> Arc<SegmentProgress> {
        Arc::clone(&self.counters[i])
    }

    /// Set the first counter to an absolute byte count (yt-dlp path).
    pub fn set_absolute(&self, n: u64) {
        self.counters[0].set(n);
    }

    /// What the user sees.
    pub fn total(&self) -> u64 {
        self.counters.iter().map(|c| c.live()).sum()
    }

    /// What gets written to `queue.json`: durable bytes only.
    pub fn snapshot(&self) -> Vec<u64> {
        self.counters.iter().map(|c| c.durable()).collect()
    }

    fn reset(&self) {
        for c in self.counters.iter() {
            c.reset();
        }
    }
}

/// Which engine handles a download.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    /// fetchd's own segmented HTTP engine.
    #[default]
    Http,
    /// Delegated to yt-dlp (streaming sites).
    YtDlp,
}

/// Everything decided before the first byte moves: where the file goes, how
/// big it is, how it splits, and what validator proves it has not changed.
///
/// Persisted in `queue.json` so a resume after a restart reproduces the exact
/// same layout instead of re-probing and possibly choosing differently.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DownloadPlan {
    pub url: String,
    pub final_path: PathBuf,
    pub part_path: PathBuf,
    pub total: Option<u64>,
    pub supports_ranges: bool,
    pub validator: Option<String>,
    pub ranges: Vec<(u64, u64)>,
    #[serde(default)]
    pub engine: Engine,
    /// Remote thumbnail URL for a preview (video downloads). `None` for files.
    #[serde(default)]
    pub thumbnail: Option<String>,
}

impl DownloadPlan {
    pub fn filename(&self) -> String {
        self.final_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    pub fn segment_count(&self) -> usize {
        self.ranges.len().max(1)
    }
}

/// Build a yt-dlp plan. `title`/`thumbnail` come from a metadata resolve when
/// available; the display name falls back to a host placeholder and is replaced
/// with the real filename once yt-dlp finishes.
pub fn video_plan(
    url: &str,
    dest_dir: &Path,
    title: Option<String>,
    thumbnail: Option<String>,
    custom: Option<&str>,
) -> Result<DownloadPlan, String> {
    let parsed = validate_url(url)?;
    let host = parsed.host_str().unwrap_or("video").to_string();
    // A name typed in the add dialog wins over the resolved title. yt-dlp still
    // picks the container, so the extension is appended once it names the file.
    let name = custom
        .and_then(sanitize_filename)
        .or_else(|| title.as_deref().and_then(sanitize_filename))
        .unwrap_or_else(|| format!("{host} video"));
    Ok(DownloadPlan {
        url: parsed.to_string(),
        final_path: dest_dir.join(&name),
        part_path: dest_dir.join(format!("{name}.part")),
        total: None,
        supports_ranges: false,
        validator: None,
        ranges: Vec::new(),
        engine: Engine::YtDlp,
        thumbnail,
    })
}

/// Give `name` the extension from `fallback` when the user did not type one.
/// Renaming "report" over "invoice.pdf" should still land a `.pdf`, but a
/// deliberate "notes.txt" is left exactly as typed.
pub fn keep_extension(name: &str, fallback: &str) -> String {
    if Path::new(name).extension().is_some() {
        return name.to_string();
    }
    match Path::new(fallback).extension().and_then(|e| e.to_str()) {
        Some(ext) => format!("{name}.{ext}"),
        None => name.to_string(),
    }
}

/// Probe the server and reserve a name, without transferring anything.
/// HTTP engine only — video routing happens before this in `state`.
pub async fn prepare(
    client: &Client,
    url: &str,
    dest_dir: &Path,
    segments: u32,
    categorize: bool,
    custom_name: Option<&str>,
) -> Result<DownloadPlan, String> {
    let parsed = validate_url(url)?;
    let mut info = probe(client, &parsed).await?;

    // A name typed in the add dialog replaces the one the server suggested.
    if let Some(name) = custom_name.and_then(sanitize_filename) {
        info.filename = keep_extension(&name, &info.filename);
    }

    // The type is only known once the probe has resolved a filename, so the
    // category sub-folder is chosen here rather than by the caller.
    let owned_dir;
    let dest_dir = if categorize {
        owned_dir = dest_dir.join(category_for(&info.filename));
        owned_dir.as_path()
    } else {
        dest_dir
    };

    tokio::fs::create_dir_all(dest_dir)
        .await
        .map_err(|e| format!("cannot create {}: {e}", dest_dir.display()))?;

    // Reject an oversized download at second 0 rather than hours in.
    if let Some(total) = info.total {
        check_disk_space(dest_dir, total)?;
    }

    let final_path = resolve_unique_path(dest_dir, &info.filename);
    let part = part_path(&final_path);

    // Claim the name now by creating the `.part`. `resolve_unique_path` treats
    // an existing `.part` as taken, so without this two adds started before
    // either begins transferring would pick the same name and write the same
    // file. `create_new` fails if someone won the race first, so re-resolve.
    let (final_path, part) = match tokio::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&part)
        .await
    {
        Ok(_) => (final_path, part),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let retry = resolve_unique_path(dest_dir, &info.filename);
            let retry_part = part_path(&retry);
            let _ = tokio::fs::File::create(&retry_part).await;
            (retry, retry_part)
        }
        Err(e) => return Err(format!("cannot create {}: {e}", part.display())),
    };

    let ranges = match info.total {
        Some(total) if info.supports_ranges => plan_segments(total, segments),
        _ => Vec::new(),
    };

    Ok(DownloadPlan {
        url: parsed.to_string(),
        final_path,
        part_path: part,
        total: info.total,
        supports_ranges: info.supports_ranges,
        validator: info.validator,
        ranges,
        engine: Engine::Http,
        thumbnail: None,
    })
}

/// Run (or resume) a prepared plan.
///
/// `progress` carries the starting offsets; build it with `Progress::resumed`
/// to continue an interrupted transfer, or `Progress::new` to start fresh.
#[allow(clippy::too_many_arguments)]
pub async fn run<F>(
    client: &Client,
    segment_client: &Client,
    plan: &DownloadPlan,
    progress: &Progress,
    throttle: &Throttle,
    token: CancellationToken,
    on_progress: F,
) -> Result<PathBuf, String>
where
    F: Fn(u64, Option<u64>) + Send + Sync + 'static,
{
    let url = validate_url(&plan.url)?;

    // Nothing to stream.
    if plan.total == Some(0) {
        File::create(&plan.final_path)
            .await
            .map_err(|e| format!("cannot create {}: {e}", plan.final_path.display()))?;
        on_progress(0, Some(0));
        return Ok(plan.final_path.clone());
    }

    let info = RemoteInfo {
        total: plan.total,
        supports_ranges: plan.supports_ranges,
        filename: plan.filename(),
        validator: plan.validator.clone(),
    };

    let on_progress = Arc::new(on_progress);
    let ticker = {
        let progress = progress.clone();
        let on_progress = Arc::clone(&on_progress);
        let total = plan.total;
        let token = token.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(PROGRESS_INTERVAL);
            // Default is Burst: after a runtime stall the missed ticks fire
            // back to back, which would report a nonsense instantaneous rate.
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    _ = token.cancelled() => break,
                    _ = interval.tick() => on_progress(progress.total(), total),
                }
            }
        })
    };

    let outcome = transfer(
        client,
        segment_client,
        &url,
        &plan.part_path,
        &info,
        plan.ranges.clone(),
        progress,
        throttle,
        &token,
    )
    .await;

    ticker.abort();
    let written = outcome?;

    // The `.part` is pre-allocated to full size, so its length on disk proves
    // nothing. This comparison is the only thing that catches a short read.
    if let Some(expected) = plan.total {
        if written != expected {
            return Err(format!(
                "incomplete download: got {written} bytes, expected {expected}. \
                 Partial file kept at {}",
                plan.part_path.display()
            ));
        }
    }

    tokio::fs::rename(&plan.part_path, &plan.final_path)
        .await
        .map_err(|e| format!("cannot finalize {}: {e}", plan.final_path.display()))?;

    on_progress(written, plan.total);
    Ok(plan.final_path.clone())
}

/// Free space on the volume that will hold the file.
fn check_disk_space(dest_dir: &Path, needed: u64) -> Result<(), String> {
    let available = fs4::available_space(dest_dir)
        .map_err(|e| format!("cannot check free space on {}: {e}", dest_dir.display()))?;
    if available < needed.saturating_add(DISK_HEADROOM) {
        return Err(format!(
            "not enough disk space: need {}, {} free",
            human_bytes(needed),
            human_bytes(available)
        ));
    }
    Ok(())
}

fn human_bytes(n: u64) -> String {
    // Floor at KB to match the UI: never surface raw bytes to the user.
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    let mut value = n as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    let dec = if value == 0.0 || value >= 10.0 { 0 } else { 1 };
    format!("{value:.dec$} {}", UNITS[unit])
}

/// Stream one range into `path` at its own offset, retrying on transient
/// failures.
///
/// `done` counts bytes already written for this segment, so every attempt
/// resumes at `start + done` — no gap, no rewritten bytes, no double count.
#[allow(clippy::too_many_arguments)]
async fn run_segment(
    client: Client,
    url: Url,
    start: u64,
    end: u64,
    path: PathBuf,
    validator: Option<String>,
    done: Arc<SegmentProgress>,
    throttle: Throttle,
    token: CancellationToken,
) -> Result<(), SegErr> {
    let mut attempt = 0usize;

    loop {
        if token.is_cancelled() {
            return Err(SegErr::Cancelled);
        }
        let offset = start + done.live();
        if offset > end {
            return Ok(());
        }

        let before = done.durable();
        match stream_range(&client, &url, offset, end, &path, &validator, &done, &throttle, &token).await {
            Ok(()) => return Ok(()),
            Err(SegErr::Cancelled) => return Err(SegErr::Cancelled),
            Err(SegErr::Fatal(m)) => return Err(SegErr::Fatal(m)),
            Err(SegErr::RangeIgnored) => return Err(SegErr::RangeIgnored),
            Err(SegErr::Retryable(m)) => {
                // The failed attempt may have left unsynced bytes in flight.
                // Restart from the last point known to be on the device.
                done.rewind_to_durable();

                // A segment that made real headway since the last failure gets
                // a fresh budget: the ladder is per stall episode, not per
                // download lifetime, or a long transfer dies to a handful of
                // unrelated blips spread over hours.
                if done.durable().saturating_sub(before) >= RETRY_RESET_BYTES {
                    attempt = 0;
                }

                if attempt >= BACKOFF_SECS.len() {
                    return Err(SegErr::Fatal(format!(
                        "giving up after {} retries: {m}",
                        BACKOFF_SECS.len()
                    )));
                }
                let wait = Duration::from_secs(BACKOFF_SECS[attempt]);
                attempt += 1;

                tokio::select! {
                    _ = token.cancelled() => return Err(SegErr::Cancelled),
                    _ = tokio::time::sleep(wait) => {}
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn stream_range(
    client: &Client,
    url: &Url,
    offset: u64,
    end: u64,
    path: &Path,
    validator: &Option<String>,
    done: &Arc<SegmentProgress>,
    throttle: &Throttle,
    token: &CancellationToken,
) -> Result<(), SegErr> {
    let mut request = client
        .get(url.clone())
        .header(RANGE, format!("bytes={offset}-{end}"));

    // `If-Range` is the standard way to make a resume safe: the server returns
    // 206 if the resource is unchanged, or 200 with the whole body if it is
    // not. The 200 branch below already treats that as "restart from zero", so
    // this replaces a second round trip plus a hand-rolled ETag comparison.
    if let Some(v) = validator {
        request = request.header(IF_RANGE, v.clone());
    }

    let response = request
        .send()
        .await
        .map_err(|e| SegErr::Retryable(format!("request failed: {e}")))?;

    let status = response.status();
    if status == StatusCode::OK {
        // Range ignored or resource changed. Never write this at an offset.
        return Err(SegErr::RangeIgnored);
    }
    if status == StatusCode::RANGE_NOT_SATISFIABLE {
        return Err(SegErr::Fatal("server rejected the byte range".into()));
    }
    if status != StatusCode::PARTIAL_CONTENT {
        if status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS {
            return Err(SegErr::Retryable(format!("server returned {status}")));
        }
        return Err(SegErr::Fatal(describe_http_error(status, response.headers())));
    }

    let mut file = OpenOptions::new()
        .write(true)
        .open(path)
        .await
        .map_err(|e| SegErr::Fatal(format!("cannot open {}: {e}", path.display())))?;
    file.seek(std::io::SeekFrom::Start(offset))
        .await
        .map_err(|e| SegErr::Fatal(format!("seek failed: {e}")))?;

    let mut stream = response.bytes_stream();
    let mut since_checkpoint = 0u64;

    loop {
        let next = tokio::select! {
            _ = token.cancelled() => return Err(SegErr::Cancelled),
            r = tokio::time::timeout(CHUNK_TIMEOUT, stream.next()) => r,
        };

        let next = next.map_err(|_| SegErr::Retryable("connection stalled: no data for 30s".into()))?;
        let Some(chunk) = next else { break };
        let chunk = chunk.map_err(|e| SegErr::Retryable(format!("transfer failed: {e}")))?;

        // Spend bandwidth budget before writing. Stays cancellable so a pause
        // does not hang waiting on permits.
        tokio::select! {
            _ = token.cancelled() => return Err(SegErr::Cancelled),
            _ = throttle.take(chunk.len()) => {}
        }

        file.write_all(&chunk)
            .await
            .map_err(|e| SegErr::Fatal(format!("write failed: {e}")))?;

        done.add(chunk.len() as u64);
        since_checkpoint += chunk.len() as u64;

        // Durability checkpoint. `flush` alone only reaches the OS page cache,
        // so the durable counter must not advance until an actual fdatasync
        // has returned — otherwise a power cut leaves the recorded offset
        // pointing at bytes that never made it to the device, and resume seeks
        // past a zero-filled hole.
        if since_checkpoint >= CHECKPOINT_BYTES {
            file.sync_data()
                .await
                .map_err(|e| SegErr::Fatal(format!("sync failed: {e}")))?;
            done.commit();
            since_checkpoint = 0;
        }
    }

    file.sync_data()
        .await
        .map_err(|e| SegErr::Fatal(format!("sync failed: {e}")))?;
    done.commit();
    Ok(())
}

/// Single-connection transfer from byte 0. Used when the server has no range
/// support, when the size is unknown, and as the downgrade path when a server
/// answers a ranged request with `200`.
async fn stream_whole(
    client: &Client,
    url: &Url,
    path: &Path,
    progress: &Progress,
    throttle: &Throttle,
    token: &CancellationToken,
) -> Result<u64, String> {
    let mut attempt = 0usize;

    loop {
        // This path always restarts from byte 0, so the counter restarts too.
        progress.reset();

        let result = stream_whole_once(client, url, path, &progress.counter(0), throttle, token).await;
        match result {
            Ok(written) => return Ok(written),
            Err(SegErr::Cancelled) => return Err("cancelled".into()),
            Err(SegErr::Fatal(m)) => return Err(m),
            Err(e) => {
                if attempt >= BACKOFF_SECS.len() {
                    return Err(format!("giving up after {} retries: {}", BACKOFF_SECS.len(), e.message()));
                }
                let wait = Duration::from_secs(BACKOFF_SECS[attempt]);
                attempt += 1;
                tokio::select! {
                    _ = token.cancelled() => return Err("cancelled".into()),
                    _ = tokio::time::sleep(wait) => {}
                }
            }
        }
    }
}

async fn stream_whole_once(
    client: &Client,
    url: &Url,
    path: &Path,
    done: &Arc<SegmentProgress>,
    throttle: &Throttle,
    token: &CancellationToken,
) -> Result<u64, SegErr> {
    let response = client
        .get(url.clone())
        .send()
        .await
        .map_err(|e| SegErr::Retryable(format!("request failed: {e}")))?;

    let status = response.status();
    if !status.is_success() {
        if status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS {
            return Err(SegErr::Retryable(format!("server returned {status}")));
        }
        return Err(SegErr::Fatal(describe_http_error(status, response.headers())));
    }

    let mut file = File::create(path)
        .await
        .map_err(|e| SegErr::Fatal(format!("cannot create {}: {e}", path.display())))?;

    let mut stream = response.bytes_stream();
    let mut written = 0u64;
    let mut since_checkpoint = 0u64;

    loop {
        let next = tokio::select! {
            _ = token.cancelled() => return Err(SegErr::Cancelled),
            r = tokio::time::timeout(CHUNK_TIMEOUT, stream.next()) => r,
        };

        let next = next.map_err(|_| SegErr::Retryable("connection stalled: no data for 30s".into()))?;
        let Some(chunk) = next else { break };
        let chunk = chunk.map_err(|e| SegErr::Retryable(format!("transfer failed: {e}")))?;

        tokio::select! {
            _ = token.cancelled() => return Err(SegErr::Cancelled),
            _ = throttle.take(chunk.len()) => {}
        }

        file.write_all(&chunk)
            .await
            .map_err(|e| SegErr::Fatal(format!("write failed: {e}")))?;

        written += chunk.len() as u64;
        done.add(chunk.len() as u64);
        since_checkpoint += chunk.len() as u64;

        if since_checkpoint >= CHECKPOINT_BYTES {
            file.sync_data().await.map_err(|e| SegErr::Fatal(format!("sync failed: {e}")))?;
            done.commit();
            since_checkpoint = 0;
        }
    }

    file.sync_data().await.map_err(|e| SegErr::Fatal(format!("sync failed: {e}")))?;
    done.commit();
    Ok(written)
}


/// Pick a strategy and run it, returning the byte count actually written.
#[allow(clippy::too_many_arguments)]
async fn transfer(
    client: &Client,
    segment_client: &Client,
    url: &Url,
    part: &Path,
    info: &RemoteInfo,
    ranges: Vec<(u64, u64)>,
    progress: &Progress,
    throttle: &Throttle,
    token: &CancellationToken,
) -> Result<u64, String> {
    // No size, no range support, or a file too small to be worth splitting:
    // one connection, streamed to EOF.
    if ranges.len() <= 1 {
        return stream_whole(client, url, part, progress, throttle, token).await;
    }

    let total = info.total.expect("ranges implies a known total");

    // Physically reserve the blocks. `set_len` alone would make a sparse file
    // that succeeds instantly and then dies with ENOSPC hours later.
    //
    // `create(true).write(true)` without `truncate`: on a resume the `.part`
    // already holds real bytes, and truncating it here would silently discard
    // everything the saved offsets say we already have.
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(part)
        .await
        .map_err(|e| format!("cannot open {}: {e}", part.display()))?;
    file.allocate(total)
        .await
        .map_err(|e| format!("cannot reserve {} on disk: {e}", human_bytes(total)))?;
    file.sync_all().await.map_err(|e| format!("sync failed: {e}"))?;
    drop(file);

    // One root token per download, a child per segment: cancelling the root
    // tears down every segment's network loop at once, with no dangling
    // sockets left behind.
    let segment_token = token.child_token();
    let range_ignored = Arc::new(AtomicBool::new(false));
    let mut handles = Vec::with_capacity(ranges.len());

    for (i, (start, end)) in ranges.into_iter().enumerate() {
        let handle = tokio::spawn(run_segment(
            segment_client.clone(),
            url.clone(),
            start,
            end,
            part.to_path_buf(),
            info.validator.clone(),
            progress.counter(i),
            throttle.clone(),
            segment_token.child_token(),
        ));
        handles.push(handle);
    }

    let mut first_error: Option<String> = None;
    for handle in handles {
        match handle.await {
            Ok(Ok(())) => {}
            Ok(Err(SegErr::RangeIgnored)) => {
                range_ignored.store(true, Ordering::Relaxed);
                segment_token.cancel();
            }
            Ok(Err(e)) => {
                if first_error.is_none() && !matches!(e, SegErr::Cancelled) {
                    first_error = Some(e.message());
                }
                segment_token.cancel();
            }
            Err(e) => {
                if first_error.is_none() {
                    first_error = Some(format!("segment task failed: {e}"));
                }
                segment_token.cancel();
            }
        }
    }

    if range_ignored.load(Ordering::Relaxed) {
        // The server ignored Range (or the resource changed under an
        // If-Range). Every segment offset is now meaningless, so throw the
        // partial away and take the whole file down one connection.
        if token.is_cancelled() {
            return Err("cancelled".into());
        }
        return stream_whole(client, url, part, progress, throttle, token).await;
    }

    if let Some(err) = first_error {
        return Err(err);
    }
    if token.is_cancelled() {
        return Err("cancelled".into());
    }

    Ok(progress.total())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_disposition_quoted() {
        let got = filename_from_content_disposition(r#"attachment; filename="report.pdf""#);
        assert_eq!(got.as_deref(), Some("report.pdf"));
    }

    #[test]
    fn content_disposition_unquoted() {
        let got = filename_from_content_disposition("attachment; filename=report.pdf");
        assert_eq!(got.as_deref(), Some("report.pdf"));
    }

    #[test]
    fn content_disposition_semicolon_inside_quotes() {
        let got = filename_from_content_disposition(r#"attachment; filename="a;b.zip""#);
        assert_eq!(got.as_deref(), Some("a;b.zip"));
    }

    #[test]
    fn content_disposition_rfc5987() {
        let got =
            filename_from_content_disposition("attachment; filename*=UTF-8''caf%C3%A9%20menu.pdf");
        assert_eq!(got.as_deref(), Some("café menu.pdf"));
    }

    #[test]
    fn content_disposition_extended_wins_over_plain() {
        let got = filename_from_content_disposition(
            r#"attachment; filename="fallback.bin"; filename*=UTF-8''real%20name.zip"#,
        );
        assert_eq!(got.as_deref(), Some("real name.zip"));
    }

    #[test]
    fn content_disposition_absent_filename() {
        assert_eq!(filename_from_content_disposition("inline"), None);
    }

    #[test]
    fn sanitize_rejects_traversal() {
        assert_eq!(sanitize_filename("../../.bashrc"), None);
        assert_eq!(sanitize_filename(".."), None);
        assert_eq!(sanitize_filename("."), None);
        assert_eq!(sanitize_filename(""), None);
        assert_eq!(sanitize_filename("/etc/passwd").as_deref(), Some("passwd"));
        assert_eq!(sanitize_filename(r"..\..\evil.exe").as_deref(), Some("evil.exe"));
    }

    #[test]
    fn sanitize_replaces_invalid_chars() {
        assert_eq!(
            sanitize_filename(r#"a:b*c?d"e<f>g|h"#).as_deref(),
            Some("a_b_c_d_e_f_g_h")
        );
    }

    #[test]
    fn sanitize_strips_trailing_dots_and_control_chars() {
        assert_eq!(sanitize_filename("file.txt...").as_deref(), Some("file.txt"));
        assert_eq!(sanitize_filename("fi\u{7}le.txt").as_deref(), Some("file.txt"));
    }

    #[test]
    fn sanitize_keeps_ordinary_names() {
        assert_eq!(sanitize_filename("archive.tar.gz").as_deref(), Some("archive.tar.gz"));
    }

    #[test]
    fn resolve_filename_prefers_content_disposition() {
        let url = Url::parse("https://example.com/dl?token=xyz987").unwrap();
        assert_eq!(
            resolve_filename(Some(r#"attachment; filename="real.zip""#), &url),
            "real.zip"
        );
    }

    #[test]
    fn resolve_filename_falls_back_to_url_path() {
        let url = Url::parse("https://example.com/files/report%20final.pdf").unwrap();
        assert_eq!(resolve_filename(None, &url), "report final.pdf");
    }

    #[test]
    fn resolve_filename_falls_back_to_default() {
        let url = Url::parse("https://example.com/").unwrap();
        assert_eq!(resolve_filename(None, &url), "download.bin");
        let url2 = Url::parse("https://example.com/x").unwrap();
        assert_eq!(resolve_filename(Some("attachment; filename=\"../..\""), &url2), "x");
    }

    #[test]
    fn custom_name_keeps_source_extension() {
        // No extension typed: the source's is appended.
        assert_eq!(keep_extension("report", "invoice.pdf"), "report.pdf");
        // An extension typed: taken literally, even a different one.
        assert_eq!(keep_extension("notes.txt", "invoice.pdf"), "notes.txt");
        // Nothing to borrow.
        assert_eq!(keep_extension("report", "download"), "report");
        // A dotted name whose last part is the extension.
        assert_eq!(keep_extension("v1.2.3", "app.tar"), "v1.2.3");
    }

    #[test]
    fn custom_name_is_sanitised() {
        // A path in the name field must not escape the download folder.
        assert_eq!(sanitize_filename("../../etc/passwd").as_deref(), Some("passwd"));
        assert_eq!(sanitize_filename("a/b/c.bin").as_deref(), Some("c.bin"));
        assert_eq!(sanitize_filename("  ").as_deref(), None);
    }

    #[test]
    fn unique_path_suffixes_on_collision() {
        let dir = std::env::temp_dir().join(format!("fetchd-unique-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let first = resolve_unique_path(&dir, "archive.tar.gz");
        assert_eq!(first.file_name().unwrap(), "archive.tar.gz");
        std::fs::write(&first, b"x").unwrap();

        // Multi-dot names keep the full stem: NOT `archive (1).gz`.
        let second = resolve_unique_path(&dir, "archive.tar.gz");
        assert_eq!(second.file_name().unwrap(), "archive.tar (1).gz");
        std::fs::write(&second, b"x").unwrap();

        let third = resolve_unique_path(&dir, "archive.tar.gz");
        assert_eq!(third.file_name().unwrap(), "archive.tar (2).gz");

        let plain = dir.join("README");
        std::fs::write(&plain, b"x").unwrap();
        assert_eq!(
            resolve_unique_path(&dir, "README").file_name().unwrap(),
            "README (1)"
        );

        // An in-flight `.part` also reserves its final name, so two concurrent
        // downloads of the same URL never write to the same file.
        std::fs::write(dir.join("busy.zip.part"), b"x").unwrap();
        assert_eq!(
            resolve_unique_path(&dir, "busy.zip").file_name().unwrap(),
            "busy (1).zip"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn part_path_stays_beside_final_file() {
        let p = part_path(Path::new("/home/u/Downloads/a.zip"));
        assert_eq!(p, PathBuf::from("/home/u/Downloads/a.zip.part"));
        assert_eq!(p.parent(), Path::new("/home/u/Downloads/a.zip").parent());
    }

    #[test]
    fn url_scheme_allowlist() {
        assert!(validate_url("https://example.com/f.zip").is_ok());
        assert!(validate_url("http://example.com/f.zip").is_ok());
        assert!(validate_url("ftp://example.com/f.zip").is_err());
        assert!(validate_url("file:///etc/passwd").is_err());
        assert!(validate_url("not a url").is_err());
    }

    #[test]
    fn content_range_total_parsing() {
        assert_eq!(parse_content_range_total("bytes 0-0/12345"), Some(12345));
        assert_eq!(parse_content_range_total("bytes 100-199/200"), Some(200));
        // Server declines to state the total.
        assert_eq!(parse_content_range_total("bytes 0-0/*"), None);
        assert_eq!(parse_content_range_total("bytes */1234"), Some(1234));
        assert_eq!(parse_content_range_total("garbage"), None);
    }

    /// Range headers are inclusive at both ends, so the seam between segments
    /// is the classic off-by-one. These assertions are the reason this
    /// function is separate from the transfer code.
    #[test]
    fn segments_tile_the_file_exactly() {
        for (total, n) in [
            (100u64, 1u32),
            (8 * 1024 * 1024, 8),
            (8 * 1024 * 1024 + 7, 8), // remainder must land on the last segment
            (5 * 1024 * 1024, 3),
            (4 * 1024 * 1024, 8),
        ] {
            let ranges = plan_segments(total, n);
            assert!(!ranges.is_empty(), "total={total} n={n}");
            assert_eq!(ranges[0].0, 0, "first segment must start at 0");
            assert_eq!(
                ranges.last().unwrap().1,
                total - 1,
                "last segment must end at the final byte (total={total})"
            );

            let mut covered = 0u64;
            for (i, (start, end)) in ranges.iter().enumerate() {
                assert!(start <= end, "inverted range at {i}: {start}..{end}");
                if i > 0 {
                    // No gap and no overlap at the seam.
                    assert_eq!(*start, ranges[i - 1].1 + 1, "seam broken at {i}");
                }
                covered += end - start + 1;
            }
            assert_eq!(covered, total, "coverage mismatch for total={total}");
        }
    }

    #[test]
    fn segments_respect_the_small_file_threshold() {
        // Below the threshold, extra sockets cost more than they gain.
        assert_eq!(plan_segments(1024, 8), vec![(0, 1023)]);
        assert_eq!(plan_segments(MIN_SEGMENTED_SIZE - 1, 8).len(), 1);
        assert_eq!(plan_segments(MIN_SEGMENTED_SIZE, 8).len(), 8);
        // Nothing to fetch.
        assert!(plan_segments(0, 8).is_empty());
        // Never more segments than the cap.
        assert_eq!(plan_segments(100 * 1024 * 1024, 99).len(), MAX_SEGMENTS as usize);
    }

    /// `set_len` would pass every assertion here except the last one: a sparse
    /// file reports the right length while occupying no blocks, which is how a
    /// too-small disk fails hours into a download instead of at second 0.
    #[cfg(unix)]
    #[tokio::test]
    async fn allocate_reserves_real_disk_blocks() {
        use std::os::unix::fs::MetadataExt;

        let dir = std::env::temp_dir().join(format!("fetchd-alloc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("reserved.bin");
        let size: u64 = 4 * 1024 * 1024;

        let file = File::create(&path).await.unwrap();
        file.allocate(size).await.unwrap();
        file.sync_all().await.unwrap();
        drop(file);

        let meta = std::fs::metadata(&path).unwrap();
        assert_eq!(meta.len(), size, "logical size");
        // st_blocks counts 512-byte units actually committed on the device.
        assert!(
            meta.blocks() * 512 >= size,
            "file is sparse: {} bytes reserved for a {size}-byte file",
            meta.blocks() * 512
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn categories_map_by_extension() {
        assert_eq!(category_for("clip.MP4"), "Video");
        assert_eq!(category_for("song.flac"), "Audio");
        assert_eq!(category_for("archive.tar.gz"), "Archives");
        assert_eq!(category_for("paper.pdf"), "Documents");
        assert_eq!(category_for("shot.jpeg"), "Images");
        assert_eq!(category_for("distro.iso"), "Programs");
        // Unknown and extensionless both fall through to Other.
        assert_eq!(category_for("data.xyz"), "Other");
        assert_eq!(category_for("README"), "Other");
    }

    #[test]
    fn human_bytes_reads_sensibly() {
        assert_eq!(human_bytes(0), "0 KB");
        assert_eq!(human_bytes(512), "0.5 KB");
        assert_eq!(human_bytes(1024), "1.0 KB");
        assert_eq!(human_bytes(512 * 1024), "512 KB");
        assert_eq!(human_bytes(1536 * 1024), "1.5 MB");
        assert_eq!(human_bytes(100 * 1024 * 1024), "100 MB");
    }

    // Network tests. Excluded from the default run because they depend on
    // third-party hosts staying up: `cargo test -- --ignored --nocapture`.

    const SMALL: &str = "https://proof.ovh.net/files/1Mb.dat";
    const BIG: &str = "https://proof.ovh.net/files/10Mb.dat";

    fn clients() -> (Client, Client) {
        let s = Session::default();
        (build_client(&s).unwrap(), build_segment_client(&s).unwrap())
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("fetchd-net-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// `prepare` + `run` in one call, the way the tests want it.
    async fn download_file<F>(
        client: &Client,
        segment_client: &Client,
        url: &str,
        dest_dir: &Path,
        segments: u32,
        token: CancellationToken,
        on_progress: F,
    ) -> Result<PathBuf, String>
    where
        F: Fn(u64, Option<u64>) + Send + Sync + 'static,
    {
        let plan = prepare(client, url, dest_dir, segments, false, None).await?;
        let progress = Progress::new(plan.segment_count());
        run(client, segment_client, &plan, &progress, &Throttle::unlimited(), token, on_progress).await
    }

    #[tokio::test]
    #[ignore]
    async fn network_probe_reports_size_and_ranges() {
        let (client, _) = clients();
        let info = probe(&client, &Url::parse(SMALL).unwrap()).await.unwrap();
        assert_eq!(info.total, Some(1_048_576));
        assert!(info.supports_ranges, "host should advertise byte ranges");
        assert_eq!(info.filename, "1Mb.dat");
    }

    #[tokio::test]
    #[ignore]
    async fn network_single_connection_download() {
        let dir = scratch("single");
        let (client, seg) = clients();

        // 1 MB is under MIN_SEGMENTED_SIZE, so this exercises the
        // single-connection path even though the server supports ranges.
        let path = download_file(&client, &seg, SMALL, &dir, 8, CancellationToken::new(), |_, _| {})
            .await
            .unwrap();

        assert_eq!(path.file_name().unwrap(), "1Mb.dat");
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 1_048_576);
        assert!(!part_path(&path).exists(), ".part must be renamed, not left behind");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    #[ignore]
    async fn network_custom_name_lands_on_disk() {
        let dir = scratch("custom-name");
        let (client, _) = clients();

        // No extension typed: the source's ".dat" is kept.
        let plan = prepare(&client, SMALL, &dir, 4, false, Some("my report"))
            .await
            .unwrap();
        assert_eq!(plan.filename(), "my report.dat");
        assert!(plan.part_path.exists(), "the name should be reserved up front");

        // A path in the name must not escape the download folder.
        let escaped = prepare(&client, SMALL, &dir, 4, false, Some("../../evil.bin"))
            .await
            .unwrap();
        assert_eq!(escaped.filename(), "evil.bin");
        assert_eq!(escaped.final_path.parent().unwrap(), dir.as_path());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    #[ignore]
    async fn network_segmented_download_matches_byte_for_byte() {
        let dir = scratch("segmented");
        let (client, seg) = clients();

        let path = download_file(&client, &seg, BIG, &dir, 8, CancellationToken::new(), |_, _| {})
            .await
            .unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 10 * 1024 * 1024);

        // The real proof that the segment offsets are right: fetch the same
        // file down one connection and compare every byte.
        let reference = dir.join("reference.bin");
        let progress = Progress::new(1);
        let written = stream_whole(
            &client,
            &Url::parse(BIG).unwrap(),
            &reference,
            &progress,
            &Throttle::unlimited(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(written, 10 * 1024 * 1024);

        assert_eq!(
            std::fs::read(&path).unwrap(),
            std::fs::read(&reference).unwrap(),
            "segmented output differs from single-connection output"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Two adds racing for the same URL must not resolve to the same file.
    /// Before the `.part` was reserved in `prepare`, both saw a free name.
    #[tokio::test]
    #[ignore]
    async fn network_concurrent_prepares_get_distinct_paths() {
        let dir = scratch("race");
        let (client, _) = clients();

        let (a, b) = tokio::join!(
            prepare(&client, SMALL, &dir, 8, false, None),
            prepare(&client, SMALL, &dir, 8, false, None),
        );
        let (a, b) = (a.unwrap(), b.unwrap());

        assert_ne!(a.final_path, b.final_path, "both adds claimed the same file");
        assert_ne!(a.part_path, b.part_path, "both adds claimed the same .part");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    #[ignore]
    async fn network_suffixes_on_second_download() {
        let dir = scratch("suffix");
        let (client, seg) = clients();
        let t = CancellationToken::new();

        let first = download_file(&client, &seg, SMALL, &dir, 8, t.clone(), |_, _| {}).await.unwrap();
        let second = download_file(&client, &seg, SMALL, &dir, 8, t, |_, _| {}).await.unwrap();

        assert_eq!(first.file_name().unwrap(), "1Mb.dat");
        assert_eq!(second.file_name().unwrap(), "1Mb (1).dat");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    #[ignore]
    async fn network_opaque_url_still_gets_a_name() {
        let dir = scratch("opaque");
        let (client, seg) = clients();

        let path = download_file(
            &client,
            &seg,
            "https://speed.cloudflare.com/__down?bytes=65536",
            &dir,
            8,
            CancellationToken::new(),
            |_, _| {},
        )
        .await
        .unwrap();

        assert_eq!(path.file_name().unwrap(), "__down");
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 65_536);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    #[ignore]
    async fn network_rejects_bad_scheme_before_touching_network() {
        let dir = scratch("scheme");
        let (client, seg) = clients();
        let err = download_file(&client, &seg, "ftp://example.com/f.zip", &dir, 8, CancellationToken::new(), |_, _| {})
            .await
            .unwrap_err();
        assert!(err.contains("unsupported scheme"), "got: {err}");
        assert!(!dir.exists(), "nothing should be created for a rejected URL");
    }

    #[tokio::test]
    #[ignore]
    async fn network_cancel_stops_the_transfer() {
        let dir = scratch("cancel");
        let (client, seg) = clients();
        let token = CancellationToken::new();

        let child = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            child.cancel();
        });

        let result = download_file(&client, &seg, BIG, &dir, 8, token, |_, _| {}).await;
        assert!(result.is_err(), "cancelled download must not report success");
        // The final file must never appear for a cancelled transfer.
        assert!(!dir.join("10Mb.dat").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The M4 claim, end to end: interrupt a segmented transfer, throw away
    /// the in-memory state, resume from nothing but the persisted per-segment
    /// offsets, and get a file identical to one fetched in a single pass.
    ///
    /// A resume that seeks to the wrong offset still produces a
    /// correctly-sized file, so only the byte comparison proves anything.
    #[tokio::test]
    #[ignore]
    async fn network_resume_from_persisted_offsets_is_byte_identical() {
        let dir = scratch("resume");
        let (client, seg) = clients();
        let total = 10 * 1024 * 1024u64;

        let plan = prepare(&client, BIG, &dir, 8, false, None).await.unwrap();
        assert_eq!(plan.ranges.len(), 8, "expected a segmented plan");

        // First attempt: cancel it mid-flight.
        let progress = Progress::new(plan.segment_count());
        let token = CancellationToken::new();
        let killer = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(250)).await;
            killer.cancel();
        });
        let interrupted = run(&client, &seg, &plan, &progress, &Throttle::unlimited(), token, |_, _| {}).await;
        assert!(interrupted.is_err(), "cancelled run must not report success");

        // This is all that survives a crash: the durable offsets.
        let persisted = progress.snapshot();
        let got = persisted.iter().sum::<u64>();
        assert!(got < total, "should not have finished before the cancel");
        assert!(plan.part_path.exists(), ".part must survive an interruption");

        // Resume with a fresh Progress built only from those numbers.
        let resumed = Progress::resumed(&persisted);
        let path = run(&client, &seg, &plan, &resumed, &Throttle::unlimited(), CancellationToken::new(), |_, _| {})
            .await
            .unwrap();

        assert_eq!(std::fs::metadata(&path).unwrap().len(), total);

        let reference = dir.join("reference.bin");
        let fresh = Progress::new(1);
        stream_whole(
            &client,
            &Url::parse(BIG).unwrap(),
            &reference,
            &fresh,
            &Throttle::unlimited(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();

        assert_eq!(
            std::fs::read(&path).unwrap(),
            std::fs::read(&reference).unwrap(),
            "resumed file differs from a single-pass download"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Durable must never run ahead of live, because `queue.json` records
    /// durable and a resume trusts it absolutely.
    #[test]
    fn durable_never_exceeds_live() {
        let seg = SegmentProgress::default();
        seg.add(1000);
        assert_eq!(seg.live(), 1000);
        assert_eq!(seg.durable(), 0, "unsynced bytes must not count as durable");

        seg.commit();
        assert_eq!(seg.durable(), 1000);

        // Bytes written after the last sync are lost on rewind, not trusted.
        seg.add(500);
        seg.rewind_to_durable();
        assert_eq!(seg.live(), 1000);
        assert_eq!(seg.durable(), 1000);
    }

    #[tokio::test]
    #[ignore]
    async fn network_progress_is_reported() {
        let dir = scratch("progress");
        let (client, seg) = clients();
        let seen = Arc::new(AtomicU64::new(0));
        let sink = Arc::clone(&seen);

        let path = download_file(&client, &seg, BIG, &dir, 8, CancellationToken::new(), move |n, total| {
            assert_eq!(total, Some(10 * 1024 * 1024));
            sink.fetch_max(n, Ordering::Relaxed);
        })
        .await
        .unwrap();

        assert_eq!(seen.load(Ordering::Relaxed), 10 * 1024 * 1024);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
