// Shared helpers for background, popup and options. Loaded via importScripts in
// the service worker and a plain <script> in the pages.

const FETCHD = "http://127.0.0.1:47831";

// Cookies bind to the exact User-Agent that earned them, so always send the
// browser's real one.
const UA = typeof navigator !== "undefined" ? navigator.userAgent : "";

const DEFAULTS = {
  enabled: true,
  intercept: false, // hand the browser's own downloads to fetchd
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

// Send one download to fetchd. Returns true on success.
async function sendToFetchd(url, referer) {
  const cookie = await cookieHeaderFor(url);
  try {
    const res = await fetch(`${FETCHD}/add`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ url, cookie, userAgent: UA, referer }),
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
