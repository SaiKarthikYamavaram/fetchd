// fetchd browser integration — service worker.
//
// Beyond routing a single link, this mirrors what a download manager's browser
// module does: it watches page responses for downloadable media, keeps a
// per-tab list, badges the toolbar with the count, and lets the popup grab any
// of them (or every link/image on the page) — all replayed through fetchd with
// the browser's own cookies so authenticated and challenge-protected files
// work.

importScripts("common.js");

// ---------------------------------------------------------------------------
// Per-tab detected media, kept in session storage so it survives the service
// worker being suspended but clears when the browser closes.
// ---------------------------------------------------------------------------

const keyFor = (tabId) => `detected_${tabId}`;

async function getDetected(tabId) {
  const k = keyFor(tabId);
  const s = await chrome.storage.session.get(k);
  return s[k] || [];
}

async function addDetected(tabId, item) {
  const list = await getDetected(tabId);
  if (list.some((x) => x.url === item.url)) return; // dedup
  list.unshift(item);
  if (list.length > 50) list.pop();
  await chrome.storage.session.set({ [keyFor(tabId)]: list });
  updateBadge(tabId, list.length);
}

async function clearDetected(tabId) {
  await chrome.storage.session.remove(keyFor(tabId));
  updateBadge(tabId, 0);
}

function updateBadge(tabId, count) {
  chrome.action.setBadgeText({ tabId, text: count ? String(count) : "" });
  chrome.action.setBadgeBackgroundColor({ tabId, color: "#6366f1" });
}

// ---------------------------------------------------------------------------
// Detection: inspect response headers for downloadable content.
// ---------------------------------------------------------------------------

chrome.webRequest.onHeadersReceived.addListener(
  (details) => {
    // Fire-and-forget; the listener itself is synchronous (non-blocking).
    void maybeDetect(details);
  },
  { urls: ["<all_urls>"], types: ["main_frame", "sub_frame", "xmlhttprequest", "media", "other"] },
  ["responseHeaders"]
);

async function maybeDetect(details) {
  if (details.tabId < 0) return; // not tied to a tab
  const settings = await getSettings();
  if (!settings.enabled || !settings.grabMedia) return;
  if (isExcluded(details.url, settings)) return;

  const headers = Object.fromEntries(
    (details.responseHeaders || []).map((h) => [h.name.toLowerCase(), h.value || ""])
  );

  const disposition = headers["content-disposition"] || "";
  const contentType = headers["content-type"] || "";
  const length = parseInt(headers["content-length"] || "0", 10);

  const filename = filenameFromDisposition(disposition) || filenameFromUrl(details.url);
  const type = classify(contentType, filename);
  if (!type) return;

  // Type must be enabled, and size must clear the floor (unless the server
  // sent no length — common for streams, which we still want to surface).
  if (!settings.types[type]) return;
  if (length && length < settings.minSizeKb * 1024) return;

  // An explicit attachment is always worth showing; otherwise require it to be
  // media or a sizeable binary, so ordinary page images/scripts don't flood.
  const isAttachment = /attachment/i.test(disposition);
  if (!isAttachment && (type === "image" || type === "document") && !length) return;

  await addDetected(details.tabId, {
    url: details.url,
    filename,
    type,
    size: length || 0,
  });
}

function filenameFromDisposition(disposition) {
  // filename*=UTF-8''name  wins over  filename="name"
  const ext = /filename\*=(?:UTF-8'')?([^;]+)/i.exec(disposition);
  if (ext) { try { return decodeURIComponent(ext[1].trim().replace(/"/g, "")); } catch { /* fall through */ } }
  const plain = /filename="?([^";]+)"?/i.exec(disposition);
  return plain ? plain[1].trim() : null;
}

// Clear a tab's list when it navigates to a new page.
chrome.tabs.onUpdated.addListener((tabId, info) => {
  if (info.status === "loading" && info.url) clearDetected(tabId);
});
chrome.tabs.onRemoved.addListener((tabId) => clearDetected(tabId));

// ---------------------------------------------------------------------------
// Context menus
// ---------------------------------------------------------------------------

chrome.runtime.onInstalled.addListener(() => {
  chrome.contextMenus.removeAll(() => {
    chrome.contextMenus.create({
      id: "fetchd-link", title: "Download with fetchd",
      contexts: ["link", "audio", "video", "image"],
    });
    chrome.contextMenus.create({
      id: "fetchd-video", title: "Download video with fetchd (yt-dlp)",
      contexts: ["page", "link", "video"],
    });
    chrome.contextMenus.create({
      id: "fetchd-all-links", title: "Download all links on this page",
      contexts: ["page"],
    });
    chrome.contextMenus.create({
      id: "fetchd-all-images", title: "Download all images on this page",
      contexts: ["page"],
    });
  });
});

chrome.contextMenus.onClicked.addListener(async (info, tab) => {
  const referer = info.pageUrl || (tab && tab.url) || "";
  if (info.menuItemId === "fetchd-link") {
    const url = info.linkUrl || info.srcUrl;
    if (url) await sendOne(url, referer);
  } else if (info.menuItemId === "fetchd-video") {
    // Prefer an explicit link/media target; otherwise the page URL itself
    // (yt-dlp resolves the video from a watch page).
    const url = info.linkUrl || info.srcUrl || info.pageUrl || (tab && tab.url);
    if (url) await sendOne(url, referer, true);
  } else if (info.menuItemId === "fetchd-all-links") {
    await grabFromPage(tab, "links", referer);
  } else if (info.menuItemId === "fetchd-all-images") {
    await grabFromPage(tab, "images", referer);
  }
});

// Pull every link or image URL out of the page DOM and send the ones whose
// type is enabled in settings.
async function grabFromPage(tab, mode, referer) {
  if (!tab) return;
  let results;
  try {
    results = await chrome.scripting.executeScript({
      target: { tabId: tab.id },
      func: (m) => {
        const sel = m === "images" ? "img[src]" : "a[href]";
        const attr = m === "images" ? "src" : "href";
        return Array.from(document.querySelectorAll(sel))
          .map((el) => el[attr])
          .filter((u) => /^https?:/i.test(u));
      },
      args: [mode],
    });
  } catch {
    notify("Cannot read this page", "The browser blocked script access here.");
    return;
  }

  const urls = [...new Set(results?.[0]?.result || [])];
  const settings = await getSettings();
  const wanted = urls.filter((u) => {
    if (isExcluded(u, settings)) return false;
    const t = classify("", filenameFromUrl(u));
    return t && settings.types[t];
  });

  if (!wanted.length) {
    notify("Nothing to download", `No matching ${mode} found on this page.`);
    return;
  }
  let ok = 0;
  for (const u of wanted) if (await sendToFetchd(u, referer)) ok++;
  notify("Sent to fetchd", `${ok} of ${wanted.length} ${mode} queued.`);
}

// ---------------------------------------------------------------------------
// Intercept the browser's own downloads (opt-in).
// ---------------------------------------------------------------------------

chrome.downloads.onCreated.addListener(async (item) => {
  const settings = await getSettings();
  if (!settings.enabled || !settings.intercept) return;

  const url = item.finalUrl || item.url;
  if (!url || !/^https?:/i.test(url)) return;
  if (isExcluded(url, settings)) return;

  const type = classify(item.mime, item.filename || filenameFromUrl(url));
  if (!type || !settings.types[type]) return;
  if (item.fileSize > 0 && item.fileSize < settings.minSizeKb * 1024) return;

  try {
    await chrome.downloads.cancel(item.id);
    await chrome.downloads.erase({ id: item.id });
  } catch {
    return; // already finished / uncancellable — don't double-download
  }
  await sendOne(url, item.referrer || url);
});

// ---------------------------------------------------------------------------
// Messages from popup
// ---------------------------------------------------------------------------

chrome.runtime.onMessage.addListener((msg, _sender, sendResponse) => {
  (async () => {
    if (msg.type === "getDetected") {
      sendResponse({ items: await getDetected(msg.tabId), alive: await fetchdAlive() });
    } else if (msg.type === "download") {
      sendResponse({ ok: await sendToFetchd(msg.url, msg.referer) });
    } else if (msg.type === "downloadAll") {
      const items = await getDetected(msg.tabId);
      let ok = 0;
      for (const it of items) if (await sendToFetchd(it.url, msg.referer)) ok++;
      sendResponse({ ok, total: items.length });
    } else if (msg.type === "clear") {
      await clearDetected(msg.tabId);
      sendResponse({ ok: true });
    }
  })();
  return true; // async response
});

async function sendOne(url, referer, video = false) {
  const ok = await sendToFetchd(url, referer, video);
  notify(ok ? "Sent to fetchd" : "fetchd not reachable",
         ok ? (video ? "Video queued" : filenameFromUrl(url)) : "Start the fetchd app and try again.");
}

function notify(title, message) {
  chrome.notifications?.create({ type: "basic", iconUrl: "icon128.png", title, message: message || "" });
}
