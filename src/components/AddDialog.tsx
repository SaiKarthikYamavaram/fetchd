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

/// HLS/DASH manifests go through yt-dlp too, whatever host they sit on, so
/// they get the same options a known video site would. Kept in sync with
/// `is_stream_manifest` in src-tauri/src/ytdlp.rs — including the scheme
/// check, so the two agree on what counts.
export function isStreamManifest(url: string): boolean {
  try {
    const u = new URL(url);
    if (u.protocol !== "http:" && u.protocol !== "https:") return false;
    const path = u.pathname.toLowerCase();
    return path.endsWith(".m3u8") || path.endsWith(".mpd");
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
/// Split a pasted blob into links. Whitespace-separated with comments skipped,
/// so the same text that works in the Import box works here too.
function splitUrls(text: string): string[] {
  return text
    .split(/\s+/)
    .map((s) => s.trim())
    .filter((s) => s && !s.startsWith("#"));
}

export function AddDialog({
  url,
  token,
  multi = false,
  onClose,
  onAdded,
}: {
  url: string;
  /// Set when the extension parked this request; confirming adds it with the
  /// browser session captured at capture time.
  token?: string | null;
  /// Opened from Import: the URL field starts as a list, and stays one even
  /// after the text is cleared.
  multi?: boolean;
  onClose: () => void;
  onAdded: (msg: string | null) => void;
}) {
  // A URL the extension parked is fixed — that is the request being confirmed.
  // One typed by hand is the whole point of the dialog, so it stays editable.
  const locked = Boolean(token);
  const [value, setValue] = useState(url);
  const [dir, setDir] = useState("");
  const [name, setName] = useState("");
  const [quality, setQuality] = useState("");
  const [start, setStart] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const links = splitUrls(value);
  const batch = links.length > 1;
  // A list stays a list: shrinking the box back to one line the moment the
  // second URL is deleted is not helpful while editing.
  const asList = multi || batch;
  const one = links[0] ?? "";
  // Either route lands on yt-dlp, which is what the quality picker drives.
  const video = isVideoUrl(one) || isStreamManifest(one);

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
      // A filename is a single-download answer; a batch takes the server's.
      name: batch ? null : name.trim() || null,
      quality: video && quality ? quality : null,
      start,
    };

    try {
      if (token) {
        const dup = await api.isDuplicate(one);
        await api.addPending(token, options);
        onAdded(dup ? "Already in the queue — added again." : null);
      } else if (batch) {
        // Queued one at a time rather than through import_urls, so the folder
        // and the start-now choice apply to every link in the batch.
        const failed: string[] = [];
        for (const link of links) {
          try {
            await api.addDownload(link, options);
          } catch {
            failed.push(link);
          }
        }
        const added = links.length - failed.length;
        if (added === 0) throw new Error("None of those links could be added.");
        onAdded(
          failed.length
            ? `Added ${added} of ${links.length}. ${failed.length} skipped.`
            : `Added ${added} downloads.`,
        );
      } else {
        const dup = await api.isDuplicate(one);
        await api.addDownload(one, options);
        onAdded(dup ? "Already in the queue — added again." : null);
      }
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
          <h2 className="modal-title">{multi ? "Import links" : "Add download"}</h2>
          <button type="button" className="act" onClick={dismiss} title="Close">
            <IconX />
          </button>
        </div>

        <label className="field">
          <span>
            {links.length > 1 ? `${links.length} links` : multi ? "Links" : "URL"}
          </span>
          {asList ? (
            <textarea
              className="urls"
              value={value}
              onChange={(e) => setValue(e.currentTarget.value)}
              readOnly={locked}
              placeholder={"https://example.com/one.zip\nhttps://example.com/two.zip\n\n# lines starting with # are skipped"}
              spellCheck={false}
              rows={5}
              autoFocus={!locked}
            />
          ) : (
            <input
              value={value}
              onChange={(e) => setValue(e.currentTarget.value)}
              readOnly={locked}
              placeholder="https://…  — or paste several at once"
              spellCheck={false}
              autoFocus={!locked}
            />
          )}
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
            value={asList ? "" : name}
            onChange={(e) => setName(e.currentTarget.value)}
            // A video URL's last path segment is routing ("watch", "video"),
            // never a filename — yt-dlp names it from the title instead.
            placeholder={
              asList
                ? "One name cannot cover several links"
                : (video ? "" : suggestedName(one)) || "Automatic"
            }
            spellCheck={false}
            disabled={asList}
          />
        </label>
        <p className="help">
          {asList
            ? "Every link goes to the folder above and keeps the name its server gives it."
            : video
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
          <button type="submit" className="btn primary" disabled={busy || links.length === 0}>
            <IconDownload />{" "}
            {busy
              ? "Adding…"
              : batch
                ? `Download ${links.length}`
                : multi
                  ? "Import"
                  : start
                    ? "Download"
                    : "Add paused"}
          </button>
          <button type="button" className="btn" onClick={dismiss}>Cancel</button>
        </div>
      </form>
    </div>
  );
}
