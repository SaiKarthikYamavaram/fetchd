import { useEffect, useRef, useState } from "react";
import { api, type Settings } from "../lib/api";
import { IconFolder } from "./icons";
import { applyTheme } from "../lib/theme";

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
        <div className="field-inline">
          <input
            value={settings.download_dir ?? ""}
            placeholder="~/Downloads"
            spellCheck={false}
            onChange={(e) =>
              update({ download_dir: e.currentTarget.value.trim() || null })
            }
          />
          <button
            type="button"
            className="btn"
            onClick={async () => {
              const picked = await api.pickFolder(settings.download_dir ?? undefined);
              // Commit immediately: a picked path is a deliberate choice, not
              // mid-typing, so it should not wait on the debounce.
              if (picked) update({ download_dir: picked }, true);
            }}
          >
            <IconFolder /> Browse
          </button>
        </div>
      </label>

      <label className="check">
        <input
          type="checkbox"
          checked={settings.categorize}
          onChange={(e) => update({ categorize: e.currentTarget.checked }, true)}
        />
        <span>Sort into folders by type (Video, Audio, Archives, …)</span>
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
        <span>Appearance</span>
        <select
          value={settings.theme || "system"}
          onChange={(e) => {
            const theme = e.currentTarget.value;
            applyTheme(theme); // repaint now, don't wait for the round trip
            update({ theme }, true);
          }}
        >
          <option value="system">Match system</option>
          <option value="light">Light</option>
          <option value="dark">Dark</option>
        </select>
      </label>

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

      <label className="field">
        <span>Proxy (blank = direct)</span>
        <input
          value={settings.proxy}
          placeholder="http://host:8080  or  socks5://host:1080"
          spellCheck={false}
          onChange={(e) => update({ proxy: e.currentTarget.value.trim() })}
        />
      </label>
      <p className="help">
        Applies to file downloads and to yt-dlp. Takes effect on the next
        download; transfers already running keep their current connection.
      </p>

      <hr />

      <h3>Schedule</h3>
      <label className="check">
        <input
          type="checkbox"
          checked={settings.schedule_enabled}
          onChange={(e) => update({ schedule_enabled: e.currentTarget.checked }, true)}
        />
        <span>Only download during a set time window</span>
      </label>
      {settings.schedule_enabled && (
        <>
          <div className="field-row">
            <label className="field">
              <span>Start</span>
              <input
                type="time"
                value={settings.schedule_start || "01:00"}
                onChange={(e) => update({ schedule_start: e.currentTarget.value }, true)}
              />
            </label>
            <label className="field">
              <span>Stop</span>
              <input
                type="time"
                value={settings.schedule_stop || "07:00"}
                onChange={(e) => update({ schedule_stop: e.currentTarget.value }, true)}
              />
            </label>
          </div>
          <p className="help">
            A stop time earlier than the start runs overnight (23:00–06:00 is one
            window). Outside it, downloads wait as <em>Queued</em> and resume by
            themselves when the window opens — partial files are kept.
          </p>
        </>
      )}

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
