function fmtSize(n) {
  if (!n) return "size unknown";
  const u = ["KB", "MB", "GB", "TB"];
  let v = n / 1024, i = 0;
  while (v >= 1024 && i < u.length - 1) { v /= 1024; i++; }
  return `${v < 10 ? v.toFixed(1) : Math.round(v)} ${u[i]}`;
}

const listEl = document.getElementById("list");
const emptyEl = document.getElementById("empty");
const allBar = document.getElementById("allBar");
const dot = document.getElementById("dot");
const statusText = document.getElementById("statusText");

let tab, referer;

async function init() {
  [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  referer = tab?.url || "";

  const { items, alive } = await chrome.runtime.sendMessage({ type: "getDetected", tabId: tab.id });
  dot.className = `dot ${alive ? "up" : "down"}`;
  statusText.textContent = alive ? "Connected" : "App not running";

  render(items || []);

  const { settings } = await chrome.storage.local.get("settings");
  document.getElementById("intercept").checked = !!(settings && settings.intercept);
}

function render(items) {
  listEl.innerHTML = "";
  if (!items.length) {
    emptyEl.hidden = false;
    allBar.hidden = true;
    return;
  }
  emptyEl.hidden = true;
  allBar.hidden = false;

  for (const it of items) {
    const row = document.createElement("div");
    row.className = "item";
    row.innerHTML = `
      <span class="tag">${it.type.slice(0, 3)}</span>
      <div class="meta">
        <div class="name" title="${escapeAttr(it.url)}">${escapeHtml(it.filename)}</div>
        <div class="sub">${it.type} · ${fmtSize(it.size)}</div>
      </div>
      <button class="dl">Download</button>`;
    row.querySelector(".dl").addEventListener("click", async (e) => {
      const btn = e.currentTarget;
      btn.disabled = true; btn.textContent = "Sent";
      await chrome.runtime.sendMessage({ type: "download", url: it.url, referer });
    });
    listEl.appendChild(row);
  }
}

document.getElementById("downloadAll").addEventListener("click", async (e) => {
  e.currentTarget.disabled = true;
  const res = await chrome.runtime.sendMessage({ type: "downloadAll", tabId: tab.id, referer });
  e.currentTarget.textContent = `Sent ${res.ok}/${res.total}`;
});

document.getElementById("clear").addEventListener("click", async () => {
  await chrome.runtime.sendMessage({ type: "clear", tabId: tab.id });
  render([]);
});

document.getElementById("intercept").addEventListener("change", async (e) => {
  const { settings } = await chrome.storage.local.get("settings");
  await chrome.storage.local.set({ settings: { ...(settings || {}), intercept: e.currentTarget.checked } });
});

document.getElementById("options").addEventListener("click", () => chrome.runtime.openOptionsPage());

function escapeHtml(s) { return s.replace(/[&<>]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;" }[c])); }
function escapeAttr(s) { return s.replace(/"/g, "&quot;"); }

init();
