// In-page download panel — the IDM Integration Module's signature affordance.
//
// The popup already lists what was detected, but it only appears when the user
// thinks to open it. This surfaces a small button over the page itself when
// there is a video worth grabbing, and stays out of the way otherwise.

(() => {
  if (window.__fetchdPanel) return; // survive re-injection
  window.__fetchdPanel = true;

  const ID = "fetchd-panel";
  let panel = null;
  let hideTimer = null;

  function build(label) {
    const el = document.createElement("div");
    el.id = ID;
    el.setAttribute("role", "dialog");
    el.setAttribute("aria-label", "Download with fetchd");
    // all-initial so page CSS cannot bleed into the panel
    el.style.cssText = [
      "all:initial",
      "position:fixed",
      "z-index:2147483647",
      "top:16px",
      "right:16px",
      "display:flex",
      "align-items:center",
      "gap:10px",
      "padding:10px 12px",
      "font:500 13px/1.3 system-ui,sans-serif",
      "color:#fff",
      "background:linear-gradient(135deg,#6366f1,#8b5cf6)",
      "border-radius:12px",
      "box-shadow:0 10px 30px -8px rgba(0,0,0,.55)",
      "cursor:pointer",
      "user-select:none",
    ].join(";");

    const text = document.createElement("span");
    text.textContent = label;
    text.style.cssText = "all:initial;font:600 13px system-ui,sans-serif;color:#fff";

    const close = document.createElement("span");
    close.textContent = "✕";
    close.title = "Hide";
    close.style.cssText =
      "all:initial;font:600 12px system-ui,sans-serif;color:rgba(255,255,255,.75);cursor:pointer;padding:0 2px";
    close.addEventListener("click", (e) => {
      e.stopPropagation();
      remove();
      // Don't nag again for this page view.
      window.__fetchdDismissed = true;
    });

    el.append(text, close);
    return { el, text };
  }

  function remove() {
    clearTimeout(hideTimer);
    panel?.el.remove();
    panel = null;
  }

  function show(label, onClick) {
    if (window.__fetchdDismissed) return;
    if (!panel) {
      panel = build(label);
      panel.el.addEventListener("click", async () => {
        panel.text.textContent = "Sending…";
        const ok = await onClick();
        panel.text.textContent = ok ? "Sent to fetchd" : "fetchd not running";
        hideTimer = setTimeout(remove, 2500);
      });
      document.documentElement.appendChild(panel.el);
    } else {
      panel.text.textContent = label;
    }
  }

  chrome.runtime.onMessage.addListener((msg, _s, sendResponse) => {
    if (msg.type === "showPanel") {
      show(msg.label || "Download this video", async () => {
        const res = await chrome.runtime.sendMessage({
          type: "download",
          url: msg.url,
          referer: location.href,
          video: true,
        });
        return !!res?.ok;
      });
      sendResponse({ shown: true });
    } else if (msg.type === "hidePanel") {
      remove();
      sendResponse({ shown: false });
    }
    return true;
  });
})();
