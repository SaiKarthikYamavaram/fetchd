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
import { kindOf, type Kind } from "./lib/filetype";
import { AddDialog } from "./components/AddDialog";
import {
  IconArchive, IconDisc, IconDoc, IconDownload, IconFile, IconMark,
  IconFolder, IconImage, IconImport, IconMusic, IconOpen, IconPause, IconPlay,
  IconBook, IconCheck, IconCode, IconEdit, IconFont, IconPackage, IconPlus,
  IconRetry, IconSearch, IconSelect, IconSettings, IconSheet, IconSlides,
  IconSubs, IconTorrent, IconTrash, IconVideo, IconX,
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
  // Selection is a mode, entered from the toolbar. Outside it the list carries
  // no checkboxes at all and a row click opens its details as usual.
  const [selectMode, setSelectMode] = useState(false);
  /// Every command below is fire-and-forget from a click handler, so a
  /// rejection has nowhere to go but an unhandled promise. Route them through
  /// here and the failure reaches the user instead of the console.
  const report = useCallback((e: unknown) => setError(String(e)), []);
  // Name filter. Narrows whatever the status pills already picked.
  const [query, setQuery] = useState("");
  const searchRef = useRef<HTMLInputElement>(null);
  // Set when the delete dialog is confirming the whole selection.
  const [deletingSelection, setDeletingSelection] = useState(false);
  // URL awaiting confirmation in the add dialog (location, quality, start).
  const [pendingUrl, setPendingUrl] = useState<string | null>(null);
  // Set when the extension parked the request; confirming replays its session.
  const [pendingToken, setPendingToken] = useState<string | null>(null);
  // Import opens the same dialog with its URL field as a list.
  const [pendingMulti, setPendingMulti] = useState(false);

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

  // Drop ids that have left the queue, so a stale selection cannot act on
  // entries that no longer exist or keep the selection bar open over nothing.
  useEffect(() => {
    setSelected((prev) => selection.prune(prev, rows.map((r) => r.id)));
  }, [rows]);

  const active = rows.filter((r) => r.status !== "completed" && r.status !== "failed");
  const done = rows.filter((r) => r.status === "completed" || r.status === "failed");
  // Everything Resume all would act on: stopped, but not finished.
  const stopped = rows.filter(
    (r) => r.status === "paused" || r.status === "interrupted" || r.status === "failed",
  );
  const downloading = active.filter((r) => r.status === "downloading");
  const totalSpeed = downloading.reduce((s, r) => s + (samples.current.get(r.id)?.speed ?? 0), 0);

  const visible = useMemo(() => {
    const byStatus = filter === "active" ? active : filter === "done" ? done : rows;
    const q = query.trim().toLowerCase();
    if (!q) return byStatus;
    // Match the URL too: a name that came back as "download.bin" is often only
    // findable by where it came from.
    return byStatus.filter(
      (r) => r.filename.toLowerCase().includes(q) || r.url.toLowerCase().includes(q),
    );
  }, [filter, rows, active, done, query]);

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

  /// Leaving the mode drops the selection with it: a hidden selection that
  /// reappears next time you enter would act on rows nobody remembers picking.
  const exitSelectMode = useCallback(() => {
    setSelectMode(false);
    setSelected(selection.EMPTY);
  }, []);

  const modalOpen = Boolean(
    detailId || deleteId || renameId || pendingUrl !== null || deletingSelection,
  );

  // Window-level shortcuts. Nothing fires while a modal is up — each dialog
  // owns its own keys — and the list keys stay out of the way while the caret
  // is in a text field.
  //
  // Held in a ref and bound once: the handler closes over live rows, which
  // change several times a second while downloading, and re-subscribing a
  // window listener that often is pure waste.
  const onKeyRef = useRef<(e: KeyboardEvent) => void>(() => {});
  onKeyRef.current = (e: KeyboardEvent) => {
    if (modalOpen) return;
    {
      const el = e.target as HTMLElement | null;
      const typing = el?.tagName === "INPUT" || el?.tagName === "TEXTAREA" || el?.isContentEditable;
      const mod = e.ctrlKey || e.metaKey;

      // Focus the filter. Works from anywhere, typing included.
      if (mod && e.key === "f") {
        e.preventDefault();
        searchRef.current?.focus();
        searchRef.current?.select();
        return;
      }

      if (e.key === "Escape") {
        // Clear the filter first, leave the mode second: one Escape per
        // thing to undo, in the order they were set.
        if (typing && el === searchRef.current && query) {
          setQuery("");
          return;
        }
        if (selectMode) exitSelectMode();
        return;
      }

      if (typing) return;

      if (mod && e.key === "a" && selectMode) {
        e.preventDefault();
        setSelected((prev) => selection.toggleAll(prev, visible.map((r) => r.id)));
        return;
      }

      if ((e.key === "Delete" || e.key === "Backspace") && selectMode && selectedIds.length) {
        e.preventDefault();
        setDeletingSelection(true);
      }
    }
  };

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => onKeyRef.current(e);
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

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
          <span className="logo"><IconMark size={18} /></span>
          <span className="wordmark">spool</span>
          {totalSpeed > 0 && (
            <span className="live-rate">
              <span className="dot" />
              {formatBytes(totalSpeed)}/s
            </span>
          )}
        </div>
        <div className="toolbar">
          <button
            className="icon-btn"
            title="Pause all"
            onClick={() => api.pauseAll().catch(report)}
            disabled={!active.length}
          >
            <IconPause />
          </button>
          <button
            className="icon-btn"
            title="Resume all"
            onClick={() => api.resumeAll().catch(report)}
            disabled={!stopped.length}
          >
            <IconPlay />
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

        {/* The widest field on the screen belongs to the thing done most
            often. Adding is a deliberate act with several answers to give, so
            it opens the dialog that asks for them. */}
        <div className="add">
          <label className="search" title="Filter by name or URL">
            <IconSearch size={16} />
            <input
              ref={searchRef}
              value={query}
              onChange={(e) => setQuery(e.currentTarget.value)}
              placeholder="Search downloads…"
              spellCheck={false}
              aria-label="Filter downloads"
              autoFocus
            />
            {query && (
              <button
                type="button"
                className="search-clear"
                onClick={() => { setQuery(""); searchRef.current?.focus(); }}
                title="Clear filter"
              >
                <IconX size={14} />
              </button>
            )}
          </label>
          {/* Both ways of bringing a download in, side by side and spelled
              out. The toolbar keeps only what acts on the whole queue. */}
          <button
            className="import-btn"
            type="button"
            onClick={() => { setPendingMulti(true); setPendingUrl(""); }}
          >
            <IconImport size={15} /> Import
          </button>
          {/* A plus, not another arrow: Import brings a file in, Add makes a
              new entry, and the brand already owns the download glyph. */}
          <button
            className="add-btn"
            type="button"
            onClick={() => { setPendingMulti(false); setPendingUrl(""); }}
          >
            <IconPlus size={16} /> Add
          </button>
        </div>

        {/* Both clear themselves; an error lingers longer because it may need
            reading twice, and either can be dismissed outright. */}
        <Toast kind="notice" message={notice} onClose={() => setNotice(null)} after={4000} />
        <Toast kind="err" message={error} onClose={() => setError(null)} after={9000} />

        <div className="filters">
          {/* The selection controls take over this strip rather than opening a
              bar of their own below it: same row, same height, so entering the
              mode never pushes the list down. */}
          {selectMode ? (
            <>
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
              <span className="selcount">{selectedRows.length} selected</span>
              <button className="btn sel-act" onClick={() => api.bulk(selectedIds, "pause").catch(report)} disabled={!canPause}>
                <IconPause /> Pause
              </button>
              <button className="btn sel-act" onClick={() => api.bulk(selectedIds, "resume").catch(report)} disabled={!canResume}>
                <IconPlay /> Resume
              </button>
              <button
                className="btn sel-act danger"
                onClick={() => setDeletingSelection(true)}
                disabled={selectedRows.length === 0}
              >
                <IconTrash /> Remove
              </button>
              <button className="clear" onClick={exitSelectMode} title="Leave selection mode (Esc)">
                Done
              </button>
            </>
          ) : (
            <>
              <FilterPill label="All" count={rows.length} on={filter === "all"} onClick={() => setFilter("all")} />
              <FilterPill label="Active" count={active.length} on={filter === "active"} onClick={() => setFilter("active")} />
              <FilterPill label="Done" count={done.length} on={filter === "done"} onClick={() => setFilter("done")} />
              {/* Selection starts from the strip it will take over, next to the
                  pills it replaces — not from the toolbar, which is for actions
                  on the app rather than on the list. */}
              <div className="strip-right">
                <button
                  className="strip-btn"
                  onClick={() => setSelectMode(true)}
                  disabled={rows.length === 0}
                  title="Pick several downloads to act on at once"
                >
                  <IconSelect size={15} /> Select
                </button>
                {done.length > 0 && (
                  <button className="clear" onClick={() => api.clearHistory().catch(report)}>Clear finished</button>
                )}
              </div>
            </>
          )}
        </div>

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
              onFail={report}
              selectMode={selectMode}
              selected={selected.ids.has(row.id)}
              onSelect={(extend) => toggleRow(row.id, extend)}
            />
          ))}
          {visible.length === 0 && (
            <div className="empty">
              <span className="empty-glyph"><IconDownload size={30} /></span>
              <p className="empty-title">
                {query
                  ? "Nothing matches that"
                  : `No downloads ${filter !== "all" ? "here" : "yet"}`}
              </p>
              <p className="empty-sub">
                {query ? (
                  <>
                    No download matches <strong>{query}</strong>. Clear the filter, or try
                    part of a URL.
                  </>
                ) : (
                  <>
                    Hit <strong>Add</strong> to paste a link, <strong>Import</strong> for a
                    list of them, or right-click any link in your browser and choose{" "}
                    <strong>Download with spool</strong>.
                  </>
                )}
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
      {pendingUrl !== null && (
        <AddDialog
          url={pendingUrl}
          multi={pendingMulti}
          token={pendingToken}
          onClose={() => {
            setPendingUrl(null);
            setPendingToken(null);
            setPendingMulti(false);
          }}
          onAdded={(msg) => {
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

/// A self-clearing banner. Mounted always so the timer belongs to one place;
/// it renders nothing without a message.
function Toast({
  kind,
  message,
  onClose,
  after,
}: {
  kind: "notice" | "err";
  message: string | null;
  onClose: () => void;
  after: number;
}) {
  useEffect(() => {
    if (!message) return;
    // Keyed on the message, so a second identical-looking one restarts the
    // clock rather than inheriting the first one's remaining time.
    const t = window.setTimeout(onClose, after);
    return () => window.clearTimeout(t);
  }, [message, after, onClose]);

  if (!message) return null;
  return (
    <div className={`toast ${kind}`} role={kind === "err" ? "alert" : "status"}>
      <span className="toast-text">{message}</span>
      <button className="toast-close" onClick={onClose} title="Dismiss">
        <IconX size={14} />
      </button>
    </div>
  );
}

/// The poster frame for a finished video, fetched once.
///
/// yt-dlp supplies a thumbnail URL for the sites it knows; this covers
/// everything else — a plain .mp4, or a stream pulled from a manifest — by
/// taking a frame from the file on disk. Null until it arrives, and null
/// forever if ffmpeg is not installed or cannot read the file, in which case
/// the row keeps its type icon.
function usePoster(row: DownloadView): string | null {
  const [poster, setPoster] = useState<string | null>(null);

  useEffect(() => {
    // Only ask once the file exists and only when there is nothing better.
    if (row.status !== "completed" || row.thumbnail || kindOf(row.filename) !== "video") {
      setPoster(null);
      return;
    }
    let live = true;
    api
      .videoThumbnail(row.id)
      .then((data) => { if (live) setPoster(data); })
      .catch(() => { /* no poster is a fine outcome */ });
    return () => { live = false; };
  }, [row.id, row.status, row.thumbnail, row.filename]);

  return poster;
}

function Row({
  row, liveBytes, liveTotal, speed, onOpen, onDelete, onRename,
  selectMode, selected, onSelect, onFail,
}: {
  row: DownloadView;
  liveBytes?: number;
  liveTotal?: number;
  speed: number;
  onOpen: () => void;
  onDelete: () => void;
  onRename: () => void;
  selectMode: boolean;
  selected: boolean;
  onSelect: (extend: boolean) => void;
  /// Surfaces a failed command; a click handler has nowhere else to put one.
  onFail: (e: unknown) => void;
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
  const poster = usePoster(row);
  const preview = row.thumbnail ?? poster;

  return (
    <div
      className={`row ${row.status}${selected ? " selected" : ""}${selectMode ? " picking" : ""}`}
      // In selection mode the whole row is the target, so the 15px checkbox is
      // an indicator rather than the only thing you can hit.
      onClick={selectMode ? (e) => onSelect(e.shiftKey) : undefined}
    >
      {/* Selection has no column of its own: a picked row swaps its file-type
          tile for a filled check, so entering the mode never re-flows the row
          and an idle list carries no controls at all. */}
      {selected ? (
        <span className="type picked" role="img" aria-label="Selected">
          <IconCheck size={22} />
        </span>
      ) : preview ? (
        // Video preview thumbnail; overlay a small ring while downloading.
        <span className="thumb">
          <img src={preview} alt="" loading="lazy" onError={(e) => (e.currentTarget.style.display = "none")} />
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

      {/* A div with a click handler is invisible to the keyboard, so this
          carries the button role, a tab stop and the keys that go with it.
          In selection mode the row itself owns the click and this is inert. */}
      <div
        className={`row-main${selectMode ? "" : " clickable"}`}
        onClick={selectMode ? undefined : onOpen}
        title={selectMode ? undefined : "View details"}
        role={selectMode ? undefined : "button"}
        tabIndex={selectMode ? undefined : 0}
        onKeyDown={
          selectMode
            ? undefined
            : (e) => {
                if (e.key === "Enter" || e.key === " ") {
                  e.preventDefault();
                  onOpen();
                }
              }
        }
      >
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

      {/* Row actions must not toggle the row underneath them in selection
          mode; each button already handles its own click. */}
      <div className="row-actions" onClick={(e) => e.stopPropagation()}>
        {(row.status === "downloading" || row.status === "queued") && (
          <button className="act primary-act" title="Pause" onClick={() => api.pause(row.id).catch(onFail)}><IconPause /></button>
        )}
        {(row.status === "paused" || row.status === "interrupted") && (
          <button className="act primary-act" title="Resume" onClick={() => api.resume(row.id).catch(onFail)}><IconPlay /></button>
        )}
        {row.status === "failed" && (
          <button className="act primary-act" title="Retry" onClick={() => api.retry(row.id).catch(onFail)}><IconRetry /></button>
        )}
        {row.status === "completed" && (
          <>
            <button className="act accent primary-act" title="Open file" onClick={() => api.openFile(row.path).catch(onFail)}><IconOpen /></button>
            <button className="act" title="Open folder" onClick={() => api.revealFile(row.path).catch(onFail)}><IconFolder /></button>
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

/// Glyph per kind. The kind itself comes from lib/filetype, which is where the
/// extension table lives and is tested.
const KIND_ICON: Record<Kind, React.ReactNode> = {
  video: <IconVideo />,
  audio: <IconMusic />,
  archive: <IconArchive />,
  image: <IconImage />,
  doc: <IconDoc />,
  sheet: <IconSheet />,
  slides: <IconSlides />,
  book: <IconBook />,
  code: <IconCode />,
  font: <IconFont />,
  subs: <IconSubs />,
  disc: <IconDisc />,
  package: <IconPackage />,
  torrent: <IconTorrent />,
  file: <IconFile />,
};

function fileKind(name: string): { icon: React.ReactNode; cls: Kind } {
  const kind = kindOf(name);
  return { icon: KIND_ICON[kind], cls: kind };
}

export default App;
