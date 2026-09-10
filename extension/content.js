// In-page download panel — the IDM Integration Module's signature affordance.
//
// The popup already lists what was detected, but it only appears when the user
// thinks to open it. This surfaces a small button over the page itself when
// there is a video worth grabbing, and stays out of the way otherwise.

(() => {
  if (window.__spoolPanel) return; // survive re-injection
  window.__spoolPanel = true;

  const ID = "spool-panel";
  let panel = null;
  let hideTimer = null;

  // The panel lives in a CLOSED SHADOW ROOT, not in the page.
  //
  // `all: initial` on an inline style is not enough on its own: it has normal
  // priority, so any page rule carrying `!important` (`div, span { color:
  // crimson !important }` is a real pattern on styled sites) overrides it and
  // the panel inherits the page's type and colour. A shadow boundary excludes
  // page CSS structurally, so the styles below are the only ones that apply —
  // and real CSS means :hover and keyframes instead of JS mouse handlers.
  //
  // The host element still sits in the page, so its own few properties are
  // pinned with `!important`; an inline `!important` outranks any page rule.
  const HOST = [
    "all:initial!important",
    "position:fixed!important",
    "z-index:2147483647!important",
    "top:16px!important",
    "right:16px!important",
    "display:block!important",
  ].join(";");

  // Kept in sync with extension/theme.css and src/App.css by hand.
  const STYLE = `
    :host { all: initial; }
    .panel {
      display: flex;
      align-items: center;
      gap: 10px;
      padding: 9px 11px 9px 9px;
      font: 500 13px/1.3 Inter, system-ui, -apple-system, "Segoe UI", Roboto, sans-serif;
      color: #EDEDEF;
      background: rgba(11, 11, 15, 0.92);
      border: 1px solid rgba(255, 255, 255, 0.10);
      border-radius: 12px;
      box-shadow: 0 0 0 1px rgba(0, 0, 0, 0.4), 0 10px 34px rgba(0, 0, 0, 0.55),
                  0 0 50px rgba(94, 106, 210, 0.28);
      backdrop-filter: saturate(1.4) blur(14px);
      -webkit-backdrop-filter: saturate(1.4) blur(14px);
      cursor: pointer;
      user-select: none;
      /* Entrance: 6px and 300ms of expo-out, the app's motion for everything. */
      opacity: 0;
      transform: translateY(-6px);
      transition: opacity .3s cubic-bezier(.16, 1, .3, 1),
                  transform .3s cubic-bezier(.16, 1, .3, 1),
                  box-shadow .2s cubic-bezier(.16, 1, .3, 1);
    }
    .panel.in { opacity: 1; transform: translateY(0); }
    .panel.in:hover {
      box-shadow: 0 0 0 1px rgba(0, 0, 0, 0.4), 0 12px 40px rgba(0, 0, 0, 0.6),
                  0 0 70px rgba(94, 106, 210, 0.4);
    }
    .panel.in:active { transform: scale(0.98); }

    /* The accent lives on this one tile rather than the whole panel, so the
       surface stays monochrome and the glow reads as a light source. */
    .mark {
      display: inline-flex;
      align-items: center;
      justify-content: center;
      flex-shrink: 0;
      width: 24px;
      height: 24px;
      border-radius: 8px;
      background: linear-gradient(135deg, #5E6AD2, #8B5CF6);
      box-shadow: 0 3px 10px rgba(94, 106, 210, 0.5),
                  inset 0 1px 0 0 rgba(255, 255, 255, 0.2);
    }
    .label {
      font-weight: 560;
      letter-spacing: -0.005em;
      white-space: nowrap;
    }
    .close {
      display: inline-flex;
      align-items: center;
      justify-content: center;
      flex-shrink: 0;
      width: 20px;
      height: 20px;
      margin-left: 2px;
      border-radius: 6px;
      font-size: 11px;
      color: #8A8F98;
      transition: color .2s cubic-bezier(.16, 1, .3, 1),
                  background .2s cubic-bezier(.16, 1, .3, 1);
    }
    .close:hover { color: #EDEDEF; background: rgba(255, 255, 255, 0.08); }

    @media (prefers-reduced-motion: reduce) {
      .panel { transition: none; opacity: 1; transform: none; }
      .panel.in:active { transform: none; }
    }
  `;

  const GLYPH =
    '<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="#fff" ' +
    'stroke-width="2" stroke-linecap="round" stroke-linejoin="round">' +
    '<path d="M12 3v12"/><path d="m7 10 5 5 5-5"/><path d="M5 21h14"/></svg>';

  function build(label) {
    const host = document.createElement("div");
    host.id = ID;
    host.style.cssText = HOST;

    const root = host.attachShadow({ mode: "closed" });
    const style = document.createElement("style");
    style.textContent = STYLE;

    const el = document.createElement("div");
    el.className = "panel";
    el.setAttribute("role", "dialog");
    el.setAttribute("aria-label", "Download with spool");
    el.innerHTML =
      `<span class="mark">${GLYPH}</span>` +
      `<span class="label"></span>` +
      `<span class="close" role="button" title="Hide">✕</span>`;

    const text = el.querySelector(".label");
    text.textContent = label;

    el.querySelector(".close").addEventListener("click", (e) => {
      e.stopPropagation();
      remove();
      // Don't nag again for this page view.
      window.__spoolDismissed = true;
    });

    root.append(style, el);
    return { host, el, text };
  }

  function remove() {
    clearTimeout(hideTimer);
    panel?.host.remove();
    panel = null;
  }

  function show(label, onClick) {
    if (window.__spoolDismissed) return;
    if (!panel) {
      panel = build(label);
      panel.el.addEventListener("click", async () => {
        panel.text.textContent = "Sending…";
        const ok = await onClick();
        panel.text.textContent = ok ? "Sent to spool" : "spool not running";
        hideTimer = setTimeout(remove, 2500);
      });
      document.documentElement.appendChild(panel.host);
      // Next frame, so the transition has a starting value to animate from.
      requestAnimationFrame(() => panel?.el.classList.add("in"));
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
