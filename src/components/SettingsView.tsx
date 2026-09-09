import { useEffect, useRef, useState } from "react";
import { api, type Settings } from "../lib/api";

export function SettingsView() {
  const [settings, setSettings] = useState<Settings | null>(null);
  const [saved, setSaved] = useState(false);
  const debounceTimer = useRef<number | null>(null);
  const latestSettings = useRef<Settings | null>(null);

  useEffect(() => {
    api.getSettings().then((s) => {
      setSettings(s);
      latestSettings.current = s;
    });
  }, []);

  useEffect(() => {
    return () => {
      if (debounceTimer.current && latestSettings.current) {
        window.clearTimeout(debounceTimer.current);
        api.updateSettings(latestSettings.current);
      }
    };
  }, []);

  if (!settings) return null;

  function commit(next: Settings) {
    api.updateSettings(next);
    setSaved(true);
    window.setTimeout(() => setSaved(false), 1200);
  }

  function update(patch: Partial<Settings>, immediate = false) {
    const next = { ...settings!, ...patch };
    setSettings(next);
    latestSettings.current = next;

    if (debounceTimer.current) {
      window.clearTimeout(debounceTimer.current);
      debounceTimer.current = null;
    }

    if (immediate) {
      commit(next);
    } else {
      debounceTimer.current = window.setTimeout(() => {
        commit(next);
        debounceTimer.current = null;
      }, 400);
    }
  }

  return (
    <section className="settings">
      <div className="section-head">
        <h2>Settings</h2>
        {saved && <span className="saved">Saved</span>}
      </div>

      <label className="field">
        <span>Download folder</span>
        <input
          value={settings.download_dir ?? ""}
          placeholder="~/Downloads"
          onChange={(e) =>
            update({ download_dir: e.currentTarget.value.trim() || null })
          }
        />
      </label>

      <div className="field-row">
        <label className="field">
          <span>Concurrent downloads</span>
          <input
            type="number"
            min={1}
            max={10}
            value={settings.max_concurrent}
            onChange={(e) =>
              update({ max_concurrent: Math.max(1, Number(e.currentTarget.value) || 1) })
            }
          />
        </label>

        <label className="field">
          <span>Connections per download</span>
          <input
            type="number"
            min={1}
            max={8}
            value={settings.segments}
            onChange={(e) =>
              update({
                segments: Math.min(8, Math.max(1, Number(e.currentTarget.value) || 1)),
              })
            }
          />
        </label>
      </div>

      <label className="field">
        <span>Speed limit (KB/s, 0 = unlimited)</span>
        <input
          type="number"
          min={0}
          step={50}
          value={settings.bandwidth_kb}
          onChange={(e) =>
            update({ bandwidth_kb: Math.max(0, Number(e.currentTarget.value) || 0) })
          }
        />
      </label>

      <hr />

      <h3>Sites that block downloaders</h3>
      <p className="help">
        Some hosts sit behind an interactive anti-bot challenge and answer with{" "}
        <code>403</code>. No download manager can solve one — not this app, not IDM.
        What IDM actually does is let the <em>browser</em> solve it and then reuse
        that session. Do the same here: export your cookies and point fetchd at
        the file.
      </p>
      <p className="help">
        Export with a "cookies.txt" browser extension, or run{" "}
        <code>yt-dlp --cookies-from-browser brave --cookies ~/cookies.txt --skip-download URL</code>.
        The User-Agent below must match the browser the cookies came from — a{" "}
        <code>cf_clearance</code> cookie is bound to the exact agent that earned it.
      </p>

      <label className="field">
        <span>Cookies file (Netscape format)</span>
        <input
          value={settings.cookies_file ?? ""}
          placeholder="/home/you/cookies.txt"
          spellCheck={false}
          onChange={(e) =>
            update({ cookies_file: e.currentTarget.value.trim() || null })
          }
        />
      </label>

      <label className="field">
        <span>User-Agent</span>
        <input
          value={settings.user_agent ?? ""}
          placeholder="(default Chrome on Linux)"
          spellCheck={false}
          onChange={(e) =>
            update({ user_agent: e.currentTarget.value.trim() || null })
          }
        />
      </label>

      <hr />

      <h3>Video downloads (yt-dlp)</h3>
      <p className="help">
        Streaming sites (YouTube, Vimeo, and ~1800 more) are handled by{" "}
        <code>yt-dlp</code>, which must be installed. Right-click a page and choose{" "}
        <em>Download video with fetchd</em>, or paste a video URL — known sites are
        auto-detected.
      </p>

      <label className="field">
        <span>Quality</span>
        <select
          value={settings.video_quality || "best"}
          onChange={(e) => update({ video_quality: e.currentTarget.value }, true)}
        >
          <option value="best">Best available</option>
          <option value="2160">2160p (4K)</option>
          <option value="1440">1440p</option>
          <option value="1080">1080p</option>
          <option value="720">720p</option>
          <option value="480">480p</option>
          <option value="audio">Audio only (mp3)</option>
        </select>
      </label>

      <div className="field-row">
        <label className="field">
          <span>yt-dlp path</span>
          <input
            value={settings.ytdlp_path}
            placeholder="yt-dlp"
            spellCheck={false}
            onChange={(e) => update({ ytdlp_path: e.currentTarget.value.trim() })}
          />
        </label>
        <label className="field">
          <span>Cookies from browser</span>
          <input
            value={settings.cookies_browser}
            placeholder="e.g. brave, chrome, firefox"
            spellCheck={false}
            onChange={(e) => update({ cookies_browser: e.currentTarget.value.trim() })}
          />
        </label>
      </div>
      <p className="help">
        Cookies from a browser let yt-dlp fetch age-restricted or members-only
        videos. Leave blank to use the cookies file above, or nothing.
      </p>
    </section>
  );
}
