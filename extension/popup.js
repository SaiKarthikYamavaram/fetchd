const FETCHD = "http://127.0.0.1:47831";

const statusEl = document.getElementById("status");
const interceptEl = document.getElementById("intercept");

// Show whether the fetchd app is reachable, so a failed download has an
// obvious cause.
fetch(`${FETCHD}/ping`)
  .then((r) => (r.ok ? r.text() : Promise.reject()))
  .then(() => {
    statusEl.textContent = "Connected to fetchd";
    statusEl.className = "status up";
  })
  .catch(() => {
    statusEl.textContent = "fetchd app not running";
    statusEl.className = "status down";
  });

chrome.storage.local.get({ intercept: false }, ({ intercept }) => {
  interceptEl.checked = intercept;
});

interceptEl.addEventListener("change", () => {
  chrome.storage.local.set({ intercept: interceptEl.checked });
});
