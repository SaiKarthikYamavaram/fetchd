import { useEffect, useState } from "react";
import { api, type AddOptions } from "../lib/api";
import { IconFolder, IconDownload, IconX } from "./icons";
import { useEscape } from "../lib/useEscape";

/// Hosts fetchd routes to yt-dlp. Kept in sync with VIDEO_HOSTS in
/// src-tauri/src/ytdlp.rs — used only to decide whether to offer the quality
/// picker, so drift just hides an option rather than breaking a download.
const VIDEO_HOSTS = [
  "youtube.com", "youtu.be", "vimeo.com", "dailymotion.com", "twitch.tv",
  "tiktok.com", "instagram.com", "facebook.com", "fb.watch", "twitter.com",
  "x.com", "reddit.com", "soundcloud.com", "bilibili.com", "nicovideo.jp",
  "streamable.com",
];

export function isVideoUrl(url: string): boolean {
  try {
    const h = new URL(url).hostname.toLowerCase();
    return VIDEO_HOSTS.some((d) => h === d || h.endsWith(`.${d}`));
  } catch {
    return false;
  }
}

/// Best guess at the filename, shown as the placeholder so the field hints at
/// what "automatic" will produce. The real name can still differ — the server's
/// Content-Disposition or the video's title wins when the field is left blank.
export function suggestedName(url: string): string {
  try {
    const last = new URL(url).pathname.split("/").filter(Boolean).pop() ?? "";
    return decodeURIComponent(last);
  } catch {
    return "";
  }
}

/// The pre-download dialog: choose where the file lands and how it is fetched
/// before anything starts, instead of silently using the defaults.
export function AddDialog({
  url,
  token,
  onClose,
  onAdded,
}: {
  url: string;
  /// Set when the extension parked this request; confirming adds it with the
  /// browser session captured at capture time.
  token?: string | null;
  onClose: () => void;
  onAdded: (msg: string | null) => void;
}) {
  const [dir, setDir] = useState("");
  const [name, setName] = useState("");
  const [quality, setQuality] = useState("");
  const [start, setStart] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const video = isVideoUrl(url);

  // Escape must go through dismiss so a parked extension request is released.
  useEscape(() => {
    if (token) api.cancelPending(token).catch(() => {});
    onClose();
  });

  useEffect(() => {
    // Show the real default folder rather than a vague placeholder.
    api.getDownloadDir().then(setDir).catch(() => setDir(""));
  }, []);

  /// Cancelling a parked (extension) request also frees it on the backend.
  function dismiss() {
    if (token) api.cancelPending(token).catch(() => {});
    onClose();
  }

  async function browse() {
    try {
      const picked = await api.pickFolder(dir || undefined);
      if (picked) setDir(picked);
    } catch (e) {
      setError(String(e));
    }
  }

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    if (busy) return;
    setBusy(true);
    setError(null);

    const options: AddOptions = {
      dir: dir.trim() || null,
      name: name.trim() || null,
      quality: video && quality ? quality : null,
      start,
    };

    try {
      const dup = await api.isDuplicate(url);
      if (token) {
        await api.addPending(token, options);
      } else {
        await api.addDownload(url, options);
      }
      onAdded(dup ? "Already in the queue — added again." : null);
      onClose();
    } catch (e) {
      setError(String(e));
      setBusy(false);
    }
  }

  return (
    <div className="overlay" onClick={dismiss}>
      <form className="modal" onClick={(e) => e.stopPropagation()} onSubmit={submit}>
        <div className="modal-head">
          <h2 className="modal-title">Add download</h2>
          <button type="button" className="act" onClick={dismiss} title="Close">
            <IconX />
          </button>
        </div>

        <label className="field">
          <span>URL</span>
          <input value={url} readOnly spellCheck={false} />
        </label>

        <label className="field">
          <span>Save to</span>
          <div className="field-inline">
            <input
              value={dir}
              onChange={(e) => setDir(e.currentTarget.value)}
              placeholder="Download folder"
              spellCheck={false}
            />
            <button type="button" className="btn" onClick={browse}>
              <IconFolder /> Browse
            </button>
          </div>
        </label>

        <label className="field">
          <span>Save as</span>
          <input
            value={name}
            onChange={(e) => setName(e.currentTarget.value)}
            // A video URL's last path segment is routing ("watch", "video"),
            // never a filename — yt-dlp names it from the title instead.
            placeholder={(video ? "" : suggestedName(url)) || "Automatic"}
            spellCheck={false}
          />
        </label>
        <p className="help">
          {video
            ? "Leave blank to use the video's title. The container is picked by yt-dlp."
            : "Leave blank to use the server's name. Without an extension, the source's is kept."}
        </p>

        {video && (
          <label className="field">
            <span>Video quality</span>
            <select value={quality} onChange={(e) => setQuality(e.currentTarget.value)}>
              <option value="">Use setting</option>
              <option value="best">Best available</option>
              <option value="2160">2160p (4K)</option>
              <option value="1440">1440p</option>
              <option value="1080">1080p</option>
              <option value="720">720p</option>
              <option value="480">480p</option>
              <option value="audio">Audio only (mp3)</option>
            </select>
          </label>
        )}

        <label className="check">
          <input type="checkbox" checked={start} onChange={(e) => setStart(e.currentTarget.checked)} />
          <span>Start now (uncheck to add it paused)</span>
        </label>

        {error && <p className="err inline">{error}</p>}

        <div className="modal-actions">
          <button type="submit" className="btn primary" disabled={busy}>
            <IconDownload /> {busy ? "Adding…" : start ? "Download" : "Add paused"}
          </button>
          <button type="button" className="btn" onClick={dismiss}>Cancel</button>
        </div>
      </form>
    </div>
  );
}
