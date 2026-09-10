import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  api,
  formatBytes,
  formatEta,
  type DownloadView,
  type ConfirmRequest,
  type ProgressRow,
  type Status,
} from "./lib/api";
import { SettingsView } from "./components/SettingsView";
import { applyTheme } from "./lib/theme";
import { DetailModal } from "./components/DetailModal";
import { ConfirmDelete } from "./components/ConfirmDelete";
import { RenameDialog } from "./components/RenameDialog";
import * as selection from "./lib/selection";
import { AddDialog } from "./components/AddDialog";
import {
  IconArchive, IconDisc, IconDoc, IconDownload, IconFile,
  IconFolder, IconImage, IconImport, IconMusic, IconOpen, IconPause, IconPlay,
  IconEdit, IconRetry, IconSettings, IconTrash, IconVideo,
} from "./components/icons";
import { Spinner, Dots } from "./components/Loaders";
import "./App.css";

/// Smoothing factor for the speed readout. TCP delivers in bursts, so the raw
/// per-tick rate swings wildly; this keeps the number readable.
const ALPHA = 0.25;

type Sample = { at: number; bytes: number; speed: number };
type Filter = "all" | "active" | "done";

const STATUS_LABEL: Record<Status, string> = {
  queued: "Queued",
  downloading: "Downloading",
  paused: "Paused",
  interrupted: "Interrupted",
  completed: "Completed",
  failed: "Failed",
};

function App() {
  const [rows, setRows] = useState<DownloadView[]>([]);
  const [url, setUrl] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [showSettings, setShowSettings] = useState(false);
  const [filter, setFilter] = useState<Filter>("all");
  const [detailId, setDetailId] = useState<string | null>(null);
  const [deleteId, setDeleteId] = useState<string | null>(null);
  const [renameId, setRenameId] = useState<string | null>(null);
  // Multi-select. Kept as ids rather than rows so it survives queue snapshots.
  // The rules (range extension, select-all, pruning) live in lib/selection.
  const [selected, setSelected] = useState<selection.Selection>(selection.EMPTY);
  // Set when the delete dialog is confirming the whole selection.
  const [deletingSelection, setDeletingSelection] = useState(false);
  // URL awaiting confirmation in the add dialog (location, quality, start).
  const [pendingUrl, setPendingUrl] = useState<string | null>(null);
  // Set when the extension parked the request; confirming replays its session.
  const [pendingToken, setPendingToken] = useState<string | null>(null);

  // Live bytes arrive far more often than the queue snapshot, so they are kept
  // out of React state and merged at render time.
  const live = useRef(new Map<string, number>());
  // Live total from progress events — for yt-dlp the size is only known once
  // the transfer is running, so the queue snapshot's total is null until then.
  const liveTotal = useRef(new Map<string, number>());
  const samples = useRef(new Map<string, Sample>());
  const [, forceRender] = useState(0);
  // Progress events arrive far faster than the display can paint. Firing a
  // re-render per event makes the window flicker, so coalesce to one per frame.
  const frame = useRef<number | null>(null);
  const scheduleRender = useCallback(() => {
    if (frame.current !== null) return;
    frame.current = requestAnimationFrame(() => {
      frame.current = null;
      forceRender((n) => n + 1);
    });
  }, []);

  const refresh = useCallback(async () => {
    try {
      setRows(await api.getQueue());
    } catch (e) {
      setError(String(e));
    }
  }, []);

  // Theme is stored in settings, so apply the saved choice on startup.
  useEffect(() => {
    api.getSettings().then((s) => applyTheme(s.theme)).catch(() => {});
  }, []);

  useEffect(() => {
    refresh();
    const unlistenQueue = listen<DownloadView[]>("queue://changed", (e) => {
      setRows(e.payload);
      // Drop per-download tracking for rows that no longer exist, or these maps
      // grow for the life of the session.
      const alive = new Set(e.payload.map((r) => r.id));
      for (const map of [live.current, liveTotal.current, samples.current]) {
        for (const id of map.keys()) if (!alive.has(id)) map.delete(id);
      }
    });
    // The extension can ask the app to confirm a capture before queuing it.
    const unlistenConfirm = listen<ConfirmRequest>("download://confirm", (e) => {
      // A second capture while a dialog is open replaces it; release the one
      // being dropped so it is not parked on the backend forever.
      setPendingToken((old) => {
        if (old && old !== e.payload.token) api.cancelPending(old).catch(() => {});
        return e.payload.token;
      });
      setPendingUrl(e.payload.url);
    });
    const unlistenProgress = listen<ProgressRow>("download://progress", (event) => {
      const { id, downloaded, total } = event.payload;
      live.current.set(id, downloaded);
      if (total) liveTotal.current.set(id, total);
      const now = performance.now();
      const prev = samples.current.get(id);
      let speed = prev?.speed ?? 0;
      if (prev) {
        const dt = (now - prev.at) / 1000;
        if (dt > 0.05) {
          const instant = Math.max(0, downloaded - prev.bytes) / dt;
          speed = prev.speed === 0 ? instant : prev.speed * (1 - ALPHA) + instant * ALPHA;
        }
      }
      samples.current.set(id, { at: now, bytes: downloaded, speed });
      scheduleRender();
    });
    return () => {
      unlistenQueue.then((fn) => fn());
      unlistenConfirm.then((fn) => fn());
      unlistenProgress.then((fn) => fn());
      if (frame.current !== null) cancelAnimationFrame(frame.current);
    };
  }, [refresh, scheduleRender]);

  function add(e: React.FormEvent) {
    e.preventDefault();
    const value = url.trim();
    if (!value) return;
    setError(null);
    setNotice(null);
    // Confirm destination and options before anything is queued.
    setPendingUrl(value);
  }

  async function importText() {
    const text = prompt("Paste one URL per line:");
    if (!text) return;
    try {
      const skipped = await api.importUrls(text);
      setNotice(skipped.length ? `Imported. Skipped ${skipped.length}.` : "Imported.");
    } catch (e) {
      setError(String(e));
    }
  }

  // Drop ids that have left the queue, so a stale selection cannot act on
  // entries that no longer exist or keep the selection bar open over nothing.
  useEffect(() => {
    setSelected((prev) => selection.prune(prev, rows.map((r) => r.id)));
  }, [rows]);

  const active = rows.filter((r) => r.status !== "completed" && r.status !== "failed");
  const done = rows.filter((r) => r.status === "completed" || r.status === "failed");
  const downloading = active.filter((r) => r.status === "downloading");
  const totalSpeed = downloading.reduce((s, r) => s + (samples.current.get(r.id)?.speed ?? 0), 0);

  const visible = useMemo(() => {
    if (filter === "active") return active;
    if (filter === "done") return done;
    return rows;
  }, [filter, rows, active, done]);

  // Look these up from the current rows each render so the open modals reflect
  // live status; close automatically if the entry is gone.
  const detailRow = detailId ? rows.find((r) => r.id === detailId) ?? null : null;
  const deleteRow = deleteId ? rows.find((r) => r.id === deleteId) ?? null : null;
  const renameRow = renameId ? rows.find((r) => r.id === renameId) ?? null : null;

  const selectedRows = visible.filter((r) => selected.ids.has(r.id));
  const selectedIds = selectedRows.map((r) => r.id);
  const allVisibleSelected = visible.length > 0 && selectedRows.length === visible.length;
  const canPause = selectedRows.some((r) => r.status === "downloading" || r.status === "queued");
  const canResume = selectedRows.some(
    (r) => r.status === "paused" || r.status === "interrupted" || r.status === "failed",
  );

  const order = visible.map((r) => r.id);

  function toggleRow(id: string, extend: boolean) {
    setSelected((prev) => selection.toggle(prev, order, id, extend));
  }

  function toggleAll() {
    setSelected((prev) => selection.toggleAll(prev, order));
  }

  function clearSelection() {
    setSelected(selection.EMPTY);
  }

  return (
    <div className="app">
      {/* The design's ambient light pools. Purely decorative, so hidden from
          assistive tech; `prefers-reduced-motion` parks them in App.css. */}
      <div className="ambient" aria-hidden="true">
        <span className="blob b1" />
        <span className="blob b2" />
        <span className="blob b3" />
        <span className="blob b4" />
      </div>

      <header className="topbar">
        <div className="brand">
          <span className="logo"><IconDownload size={18} /></span>
          <span className="wordmark">fetchd</span>
          {totalSpeed > 0 && (
            <span className="live-rate">
              <span className="dot" />
              {formatBytes(totalSpeed)}/s
            </span>
          )}
        </div>
        <div className="toolbar">
          <button className="icon-btn" title="Import from .txt" onClick={importText}>
            <IconImport />
          </button>
          <button
            className="icon-btn"
            title="Pause all"
            onClick={() => api.pauseAll()}
            disabled={!active.length}
          >
            <IconPause />
          </button>
          <button
            className={`icon-btn ${showSettings ? "active" : ""}`}
            title="Settings"
            onClick={() => setShowSettings((s) => !s)}
          >
            <IconSettings />
          </button>
        </div>
      </header>

      <main className="content">
        {showSettings && <SettingsView />}

        <form className="add" onSubmit={add}>
          <input
            value={url}
            onChange={(e) => setUrl(e.currentTarget.value)}
            placeholder="Paste a link, or send one from the browser extension…"
            spellCheck={false}
            autoFocus
          />
          <button className="add-btn" type="submit" disabled={!url.trim()}>
            Add
          </button>
        </form>

        {notice && <div className="toast notice">{notice}</div>}
        {error && <div className="toast err">{error}</div>}

        <div className="filters">
          <label className="select-all" title={allVisibleSelected ? "Deselect all" : "Select all"}>
            <input
              type="checkbox"
              checked={allVisibleSelected}
              // Some but not all: the tri-state box only exists as a DOM
              // property, so it has to be set through a ref callback.
              ref={(el) => {
                if (el) el.indeterminate = selectedRows.length > 0 && !allVisibleSelected;
              }}
              onChange={toggleAll}
              disabled={visible.length === 0}
            />
          </label>
          <FilterPill label="All" count={rows.length} on={filter === "all"} onClick={() => setFilter("all")} />
          <FilterPill label="Active" count={active.length} on={filter === "active"} onClick={() => setFilter("active")} />
          <FilterPill label="Done" count={done.length} on={filter === "done"} onClick={() => setFilter("done")} />
          {done.length > 0 && (
            <button className="clear" onClick={() => api.clearHistory()}>Clear finished</button>
          )}
        </div>

        {selectedRows.length > 0 && (
          <div className="selbar">
            <span className="selcount">{selectedRows.length} selected</span>
            <button className="btn" onClick={() => api.bulk(selectedIds, "pause")} disabled={!canPause}>
              <IconPause /> Pause
            </button>
            <button className="btn" onClick={() => api.bulk(selectedIds, "resume")} disabled={!canResume}>
              <IconPlay /> Resume
            </button>
            <button className="btn danger" onClick={() => setDeletingSelection(true)}>
              <IconTrash /> Remove
            </button>
            <button className="clear" onClick={clearSelection}>Clear</button>
          </div>
        )}

        <div className="list">
          {visible.map((row) => (
            <Row
              key={row.id}
              row={row}
              liveBytes={live.current.get(row.id)}
              liveTotal={liveTotal.current.get(row.id)}
              speed={samples.current.get(row.id)?.speed ?? 0}
              onOpen={() => setDetailId(row.id)}
              onDelete={() => setDeleteId(row.id)}
              onRename={() => setRenameId(row.id)}
              selected={selected.ids.has(row.id)}
              onSelect={(extend) => toggleRow(row.id, extend)}
            />
          ))}
          {visible.length === 0 && (
            <div className="empty">
              <span className="empty-glyph"><IconDownload size={30} /></span>
              <p className="empty-title">No downloads {filter !== "all" ? "here" : "yet"}</p>
              <p className="empty-sub">
                Paste a link above, or right-click any link in your browser and choose
                <strong> Download with fetchd</strong>.
              </p>
            </div>
          )}
        </div>
      </main>

      {detailRow && (
        <DetailModal
          row={detailRow}
          liveBytes={live.current.get(detailRow.id)}
          liveTotal={liveTotal.current.get(detailRow.id)}
          speed={samples.current.get(detailRow.id)?.speed ?? 0}
          onClose={() => setDetailId(null)}
        />
      )}
      {deleteRow && (
        <ConfirmDelete rows={[deleteRow]} onClose={() => setDeleteId(null)} />
      )}
      {deletingSelection && selectedRows.length > 0 && (
        <ConfirmDelete
          rows={selectedRows}
          onClose={() => {
            setDeletingSelection(false);
            clearSelection();
          }}
        />
      )}
      {renameRow && (
        <RenameDialog row={renameRow} onClose={() => setRenameId(null)} />
      )}
      {pendingUrl && (
        <AddDialog
          url={pendingUrl}
          token={pendingToken}
          onClose={() => {
            setPendingUrl(null);
            setPendingToken(null);
          }}
          onAdded={(msg) => {
            setUrl("");
            if (msg) setNotice(msg);
          }}
        />
      )}
    </div>
  );
}

function FilterPill({
  label, count, on, onClick,
}: { label: string; count: number; on: boolean; onClick: () => void }) {
  return (
    <button className={`pill ${on ? "on" : ""}`} onClick={onClick}>
      {label}<span className="pill-count">{count}</span>
    </button>
  );
}

function Row({
  row, liveBytes, liveTotal, speed, onOpen, onDelete, onRename, selected, onSelect,
}: {
  row: DownloadView;
  liveBytes?: number;
  liveTotal?: number;
  speed: number;
  onOpen: () => void;
  onDelete: () => void;
  onRename: () => void;
  selected: boolean;
  onSelect: (extend: boolean) => void;
}) {
  const downloaded = row.status === "downloading" && liveBytes !== undefined ? liveBytes : row.downloaded;
  // yt-dlp size is only known once running, so fall back to the live total.
  const total = row.total ?? (row.status === "downloading" ? liveTotal ?? null : null);
  const percent = total ? Math.min(100, (downloaded / total) * 100) : null;
  const remaining = total ? total - downloaded : 0;
  const eta = row.status === "downloading" && speed > 0 ? formatEta(remaining / speed) : "";
  // Only slide the placeholder bar when bytes are actually moving with no known
  // size. While a download is still resolving (yt-dlp probing, nothing
  // transferred yet) the bar sits at 0 rather than pretending to work.
  const indeterminate = percent === null && downloaded > 0;
  const running = row.status === "downloading";
  const kind = fileKind(row.filename);

  return (
    <div className={`row ${row.status}${selected ? " selected" : ""}`}>
      <label className="row-check" title="Select">
        <input
          type="checkbox"
          checked={selected}
          // Shift extends the range from the last plain click, so the handler
          // needs the modifier — `onChange` does not carry it.
          onChange={() => {}}
          onClick={(e) => onSelect(e.shiftKey)}
        />
      </label>
      {row.thumbnail ? (
        // Video preview thumbnail; overlay a small ring while downloading.
        <span className="thumb">
          <img src={row.thumbnail} alt="" loading="lazy" onError={(e) => (e.currentTarget.style.display = "none")} />
          {running && percent !== null && (
            <span className="thumb-ring" style={{ ["--p" as string]: percent }}>
              <span className="ring-num">{Math.round(percent)}</span>
            </span>
          )}
        </span>
      ) : running && percent !== null ? (
        <span className="type ring" style={{ ["--p" as string]: percent }}>
          <span className="ring-num">{Math.round(percent)}</span>
        </span>
      ) : running ? (
        // Unknown size: no percentage to show, so a quiet accent spinner.
        <span className="type downloading"><Spinner size={18} /></span>
      ) : (
        <span className={`type ${kind.cls}`}>{kind.icon}</span>
      )}

      <div className="row-main clickable" onClick={onOpen} title="View details">
        <div className="row-top">
          <span className="fname" title={row.url}>{row.filename}</span>
          <span className={`status-dot ${row.status}`} title={STATUS_LABEL[row.status]} />
        </div>

        {row.status !== "completed" && (
          <div className={`track ${indeterminate ? "indeterminate" : ""}`}>
            <div
              className="track-fill"
              style={{ width: indeterminate ? "40%" : `${percent ?? 0}%` }}
            />
          </div>
        )}

        <div className="row-meta">
          <span>
            {formatBytes(downloaded)}
            {total ? ` / ${formatBytes(total)}` : " · unknown size"}
          </span>
          <span className="meta-right">
            {running && (
              <>
                <span className="rate-tag">{formatBytes(speed)}/s</span>
                {eta && <span>{eta} left</span>}
                {row.segments > 1 && <span>{row.segments} conns</span>}
              </>
            )}
            {row.status === "completed" && <span className="done-tag">Completed</span>}
            {row.status === "failed" && <span className="fail-tag">Failed</span>}
            {(row.status === "queued" || row.status === "interrupted") && (
              <span className="prep">{STATUS_LABEL[row.status]} <Dots /></span>
            )}
            {row.status === "paused" && <span>{STATUS_LABEL[row.status]}</span>}
          </span>
        </div>

        {row.error && <p className="row-err">{row.error}</p>}
      </div>

      <div className="row-actions">
        {(row.status === "downloading" || row.status === "queued") && (
          <button className="act primary-act" title="Pause" onClick={() => api.pause(row.id)}><IconPause /></button>
        )}
        {(row.status === "paused" || row.status === "interrupted") && (
          <button className="act primary-act" title="Resume" onClick={() => api.resume(row.id)}><IconPlay /></button>
        )}
        {row.status === "failed" && (
          <button className="act primary-act" title="Retry" onClick={() => api.retry(row.id)}><IconRetry /></button>
        )}
        {row.status === "completed" && (
          <>
            <button className="act accent primary-act" title="Open file" onClick={() => api.openFile(row.path)}><IconOpen /></button>
            <button className="act" title="Open folder" onClick={() => api.revealFile(row.path)}><IconFolder /></button>
          </>
        )}
        <span className="act-sep" />
        {/* A running transfer holds its `.part` open, so renaming needs a pause
            first — hide the button rather than offer a guaranteed error. */}
        {row.status !== "downloading" && (
          <button className="act" title="Rename" onClick={onRename}><IconEdit /></button>
        )}
        <button className="act danger" title="Remove" onClick={onDelete}><IconTrash /></button>
      </div>
    </div>
  );
}

function fileKind(name: string): { icon: React.ReactNode; cls: string } {
  const ext = name.split(".").pop()?.toLowerCase() ?? "";
  if (["mp4", "mkv", "avi", "mov", "webm", "flv"].includes(ext)) return { icon: <IconVideo />, cls: "video" };
  if (["mp3", "flac", "wav", "aac", "ogg", "m4a"].includes(ext)) return { icon: <IconMusic />, cls: "audio" };
  if (["zip", "tar", "gz", "xz", "7z", "rar", "bz2"].includes(ext)) return { icon: <IconArchive />, cls: "archive" };
  if (["png", "jpg", "jpeg", "gif", "webp", "svg", "bmp"].includes(ext)) return { icon: <IconImage />, cls: "image" };
  if (["pdf", "doc", "docx", "txt", "epub"].includes(ext)) return { icon: <IconDoc />, cls: "doc" };
  if (["iso", "img", "dmg", "exe", "appimage", "deb", "rpm"].includes(ext)) return { icon: <IconDisc />, cls: "disc" };
  return { icon: <IconFile />, cls: "file" };
}

export default App;
