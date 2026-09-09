// Shared helpers for background, popup and options. Loaded via importScripts in
// the service worker and a plain <script> in the pages.

const FETCHD = "http://127.0.0.1:47831";

// Cookies bind to the exact User-Agent that earned them, so always send the
// browser's real one.
const UA = typeof navigator !== "undefined" ? navigator.userAgent : "";

const DEFAULTS = {
  enabled: true,
  intercept: false, // hand the browser's own downloads to fetchd
  // Let the app ask for a folder/quality per download instead of using the
  // defaults. The app window comes forward with its add dialog.
  askBeforeDownload: true,
  // Show the in-page button on video pages (IDM's floating panel).
  showPanel: true,
  grabMedia: true, // detect streamable/attachment media on pages
  minSizeKb: 512, // ignore anything smaller
  types: { video: true, audio: true, archive: true, document: true, image: false, other: true },
  excludeDomains: [], // hosts to never touch
};

async function getSettings() {
  const stored = await chrome.storage.local.get("settings");
  return { ...DEFAULTS, ...(stored.settings || {}), types: { ...DEFAULTS.types, ...((stored.settings || {}).types || {}) } };
}

async function setSettings(patch) {
  const cur = await getSettings();
  const next = { ...cur, ...patch };
  await chrome.storage.local.set({ settings: next });
  return next;
}

// Map a Content-Type / filename to one of the type buckets, or null if it is
// not something worth downloading (html, json, css, ...).
function classify(contentType, filename) {
  const ct = (contentType || "").toLowerCase().split(";")[0].trim();
  const ext = (filename || "").toLowerCase().split(".").pop() || "";

  if (ct.startsWith("video/") || ["mp4", "mkv", "webm", "avi", "mov", "flv", "m3u8", "ts"].includes(ext)) return "video";
  if (ct.startsWith("audio/") || ["mp3", "flac", "wav", "aac", "ogg", "m4a"].includes(ext)) return "audio";
  if (
    ["application/zip", "application/x-tar", "application/x-7z-compressed", "application/x-rar-compressed",
     "application/gzip", "application/x-bzip2"].includes(ct) ||
    ["zip", "tar", "gz", "xz", "7z", "rar", "bz2"].includes(ext)
  ) return "archive";
  if (
    ["application/pdf", "application/msword", "application/epub+zip"].includes(ct) ||
    ["pdf", "doc", "docx", "epub"].includes(ext)
  ) return "document";
  if (ct.startsWith("image/") || ["png", "jpg", "jpeg", "gif", "webp", "svg", "bmp"].includes(ext)) return "image";

  // A generic binary or an explicit attachment is downloadable but untyped.
  if (ct === "application/octet-stream" || ["iso", "img", "dmg", "exe", "appimage", "deb", "rpm", "bin"].includes(ext)) return "other";

  return null;
}

function hostOf(url) {
  try { return new URL(url).hostname; } catch { return ""; }
}

// Sites whose videos are adaptive streams (DASH/HLS) that header-detection
// cannot surface as a file — they go through the yt-dlp engine instead. Kept in
// sync with VIDEO_HOSTS in src-tauri/src/ytdlp.rs.
const VIDEO_HOSTS = [
  "youtube.com", "youtu.be", "m.youtube.com", "music.youtube.com",
  "vimeo.com", "dailymotion.com", "twitch.tv", "clips.twitch.tv",
  "tiktok.com", "instagram.com", "facebook.com", "fb.watch",
  "twitter.com", "x.com", "reddit.com", "soundcloud.com",
  "bilibili.com", "nicovideo.jp", "streamable.com",
];

// HLS/DASH manifests are playlists, not files — downloading one over HTTP just
// saves a few KB of text. They have to go through yt-dlp, which fetches the
// segments and muxes them.
function isStreamManifest(url) {
  return /\.(m3u8|mpd)(\?|#|$)/i.test(url || "");
}

function isVideoSite(url) {
  const h = hostOf(url).toLowerCase();
  return VIDEO_HOSTS.some((d) => h === d || h.endsWith(`.${d}`));
}

// Clean a browser tab title into a video name: strip the trailing site suffix
// ("… - YouTube", "… on Vimeo") and a leading unread-count badge ("(3) …").
function videoTitle(tabTitle) {
  if (!tabTitle) return null;
  let t = tabTitle.replace(/^\(\d+\)\s*/, "");
  t = t.replace(/\s*[-|]\s*(YouTube|Vimeo|Dailymotion|Twitch|TikTok|Reddit|SoundCloud|Bilibili)\s*$/i, "");
  t = t.replace(/\s+on Vimeo$/i, "");
  return t.trim() || null;
}

function isExcluded(url, settings) {
  const host = hostOf(url);
  return settings.excludeDomains.some((d) => host === d || host.endsWith(`.${d}`));
}

function filenameFromUrl(url) {
  try {
    const name = decodeURIComponent(new URL(url).pathname.split("/").pop() || "");
    return name || url;
  } catch { return url; }
}

async function cookieHeaderFor(url) {
  try {
    const cookies = await chrome.cookies.getAll({ url });
    if (!cookies.length) return null;
    return cookies.map((c) => `${c.name}=${c.value}`).join("; ");
  } catch { return null; }
}

// Send one download to fetchd. `video` forces the yt-dlp engine (for streaming
// sites). `ask` overrides the "ask before download" setting — batch grabs pass
// false so a 30-link grab doesn't open 30 dialogs. Returns true on success.
async function sendToFetchd(url, referer, video = false, ask = null) {
  const cookie = await cookieHeaderFor(url);
  const askBeforeDownload = ask === null ? (await getSettings()).askBeforeDownload : ask;
  try {
    const res = await fetch(`${FETCHD}/add`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ url, cookie, userAgent: UA, referer, video, ask: askBeforeDownload }),
    });
    return res.ok;
  } catch {
    return false;
  }
}

async function fetchdAlive() {
  try {
    const res = await fetch(`${FETCHD}/ping`);
    return res.ok;
  } catch { return false; }
}
