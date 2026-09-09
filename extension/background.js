// fetchd browser integration — service worker.
//
// The point of this extension is the one thing a download manager cannot do on
// its own: get past an interactive anti-bot challenge. The browser has already
// solved the challenge and holds the resulting cookies (cf_clearance and the
// rest). This worker reads those cookies for the target URL and hands them to
// fetchd along with the exact User-Agent and Referer they are bound to, so
// fetchd replays a session the browser earned.

const FETCHD = "http://127.0.0.1:47831";

// Cookies are bound to the User-Agent that earned them, so send the browser's
// real one rather than letting fetchd guess.
const USER_AGENT = navigator.userAgent;

chrome.runtime.onInstalled.addListener(() => {
  chrome.contextMenus.create({
    id: "fetchd-link",
    title: "Download with fetchd",
    contexts: ["link", "audio", "video", "image", "selection", "page"],
  });
  chrome.storage.local.get({ intercept: false }, () => {});
});

chrome.contextMenus.onClicked.addListener((info) => {
  const url = info.linkUrl || info.srcUrl || info.pageUrl;
  if (url) sendToFetchd(url, info.pageUrl || url);
});

// Optional: intercept every browser download and route it to fetchd instead.
// Off by default (toggle in the popup) so the extension is inert until asked.
chrome.downloads.onCreated.addListener(async (item) => {
  const { intercept } = await chrome.storage.local.get({ intercept: false });
  if (!intercept || !item.finalUrl && !item.url) return;

  const url = item.finalUrl || item.url;
  if (!/^https?:/i.test(url)) return;

  // Cancel the browser's own download and let fetchd take it.
  try {
    await chrome.downloads.cancel(item.id);
    await chrome.downloads.erase({ id: item.id });
  } catch (_) {
    // If it already finished or cannot be cancelled, do not double-download.
    return;
  }
  sendToFetchd(url, item.referrer || url);
});

// Build a Cookie header for `url` from the browser's own jar.
//
// chrome.cookies.getAll with a URL returns exactly the cookies the browser
// would itself send to that URL — correct domain, path, secure and host-only
// scoping already applied — so no cookie for another site can leak in.
async function cookieHeaderFor(url) {
  try {
    const cookies = await chrome.cookies.getAll({ url });
    if (!cookies.length) return null;
    return cookies.map((c) => `${c.name}=${c.value}`).join("; ");
  } catch (_) {
    return null;
  }
}

async function sendToFetchd(url, referer) {
  const cookie = await cookieHeaderFor(url);

  try {
    const res = await fetch(`${FETCHD}/add`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ url, cookie, userAgent: USER_AGENT, referer }),
    });

    if (res.ok) {
      notify("Sent to fetchd", shortName(url));
    } else {
      const msg = await res.text();
      notify("fetchd rejected the download", msg.slice(0, 180));
    }
  } catch (_) {
    notify("fetchd is not running", "Start the fetchd app, then try again.");
  }
}

function shortName(url) {
  try {
    const p = new URL(url).pathname.split("/").pop();
    return p || url;
  } catch (_) {
    return url;
  }
}

function notify(title, message) {
  chrome.notifications?.create({
    type: "basic",
    iconUrl: "icon128.png",
    title,
    message: message || "",
  });
}
