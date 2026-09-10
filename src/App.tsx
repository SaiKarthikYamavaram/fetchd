import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { toast } from "sonner";
import {
  Archive, BookOpen, Captions, Check, Code2, Copy, Disc, Download, ExternalLink,
  File, FileText, Folder, Image, ListChecks, Loader2, Magnet, MoreVertical, Music,
  Package, Pause, Pencil, Play, Plus, Presentation, RotateCcw, Search, Settings,
  Sheet, Trash2, Type as TypeIcon, Video, X,
} from "lucide-react";
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
import { RingProgress } from "./components/Loaders";
import { Badge } from "./components/ui/badge";
import { Button } from "./components/ui/button";
import { Card } from "./components/ui/card";
import { Checkbox } from "./components/ui/checkbox";
import {
  DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "./components/ui/dropdown-menu";
import { Input } from "./components/ui/input";
import { Progress } from "./components/ui/progress";
import { Tabs, TabsList, TabsTrigger } from "./components/ui/tabs";
import { Toaster } from "./components/ui/sonner";
import { cn } from "cn";
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

const STATUS_DOT: Record<Status, string> = {
  queued: "bg-muted-foreground",
  downloading: "bg-primary",
  paused: "bg-amber-500",
  interrupted: "bg-amber-500",
  completed: "bg-emerald-500",
  failed: "bg-destructive",
};

/// The spool mark: a ring broken into four segments — the connections a
/// download is split across.
function Logo({ size = 18 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={4.2}>
      <circle cx="12" cy="12" r="9.5" strokeDasharray="12.435 2.487" transform="rotate(-90 12 12)" />
    </svg>
  );
}

function App() {
  const [rows, setRows] = useState<DownloadView[]>([]);
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
  /// here and the failure reaches the user as a toast instead of the console.
  const report = useCallback((e: unknown) => toast.error(String(e)), []);
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
      report(e);
    }
  }, [report]);

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
    <div className="mx-auto flex h-screen max-w-3xl flex-col px-4">
      <Toaster />

      <header className="flex items-center justify-between gap-3 border-b py-3">
        <div className="flex items-center gap-2">
          <span className="text-primary"><Logo size={18} /></span>
          <span className="font-semibold">spool</span>
          {totalSpeed > 0 && (
            <Badge variant="secondary" className="gap-1.5">
              <span className="size-1.5 rounded-full bg-primary animate-pulse" />
              {formatBytes(totalSpeed)}/s
            </Badge>
          )}
        </div>
        <div className="flex items-center gap-1">
          <Button
            variant="ghost"
            size="icon"
            title="Pause all"
            onClick={() => api.pauseAll().catch(report)}
            disabled={!active.length}
          >
            <Pause />
          </Button>
          <Button
            variant="ghost"
            size="icon"
            title="Resume all"
            onClick={() => api.resumeAll().catch(report)}
            disabled={!stopped.length}
          >
            <Play />
          </Button>
          <Button
            variant={showSettings ? "secondary" : "ghost"}
            size="icon"
            title="Settings"
            onClick={() => setShowSettings((s) => !s)}
          >
            <Settings />
          </Button>
        </div>
      </header>

      <main className="flex-1 overflow-y-auto py-4">
        {showSettings ? (
          <SettingsView />
        ) : (
          <>
            {/* The widest field on the screen belongs to the thing done most
                often. Adding is a deliberate act with several answers to give,
                so it opens the dialog that asks for them. */}
            <div className="flex gap-2">
              <div className="relative flex-1">
                <Search className="pointer-events-none absolute left-2.5 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
                <Input
                  ref={searchRef}
                  value={query}
                  onChange={(e) => setQuery(e.currentTarget.value)}
                  placeholder="Search downloads…"
                  spellCheck={false}
                  aria-label="Filter downloads"
                  autoFocus
                  className="pl-8 pr-8"
                />
                {query && (
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon-xs"
                    className="absolute right-1.5 top-1/2 -translate-y-1/2"
                    onClick={() => { setQuery(""); searchRef.current?.focus(); }}
                    title="Clear filter"
                  >
                    <X />
                  </Button>
                )}
              </div>
              {/* Both ways of bringing a download in, side by side and spelled
                  out. The toolbar keeps only what acts on the whole queue. */}
              <Button
                variant="outline"
                type="button"
                onClick={() => { setPendingMulti(true); setPendingUrl(""); }}
              >
                <Download /> Import
              </Button>
              {/* A plus, not another arrow: Import brings a file in, Add makes
                  a new entry, and the brand already owns the download glyph. */}
              <Button
                type="button"
                onClick={() => { setPendingMulti(false); setPendingUrl(""); }}
              >
                <Plus /> Add
              </Button>
            </div>

            <div className="mt-3 flex h-9 items-center gap-2">
              {/* The selection controls take over this strip rather than
                  opening a bar of their own below it: same row, same height,
                  so entering the mode never pushes the list down. */}
              {selectMode ? (
                <>
                  <Checkbox
                    checked={allVisibleSelected ? true : selectedRows.length > 0 ? "indeterminate" : false}
                    onCheckedChange={toggleAll}
                    disabled={visible.length === 0}
                    title={allVisibleSelected ? "Deselect all" : "Select all"}
                  />
                  <span className="text-sm text-muted-foreground">{selectedRows.length} selected</span>
                  <div className="ml-auto flex items-center gap-1.5">
                    <Button size="sm" variant="outline" onClick={() => api.bulk(selectedIds, "pause").catch(report)} disabled={!canPause}>
                      <Pause /> Pause
                    </Button>
                    <Button size="sm" variant="outline" onClick={() => api.bulk(selectedIds, "resume").catch(report)} disabled={!canResume}>
                      <Play /> Resume
                    </Button>
                    <Button size="sm" variant="destructive" onClick={() => setDeletingSelection(true)} disabled={selectedRows.length === 0}>
                      <Trash2 /> Remove
                    </Button>
                    <Button size="sm" variant="ghost" onClick={exitSelectMode} title="Leave selection mode (Esc)">
                      Done
                    </Button>
                  </div>
                </>
              ) : (
                <>
                  <Tabs value={filter} onValueChange={(v) => setFilter(v as Filter)}>
                    <TabsList>
                      <TabsTrigger value="all">All <Badge variant="secondary">{rows.length}</Badge></TabsTrigger>
                      <TabsTrigger value="active">Active <Badge variant="secondary">{active.length}</Badge></TabsTrigger>
                      <TabsTrigger value="done">Done <Badge variant="secondary">{done.length}</Badge></TabsTrigger>
                    </TabsList>
                  </Tabs>
                  {/* Selection starts from the strip it will take over, next
                      to the pills it replaces — not from the toolbar, which
                      is for actions on the app rather than on the list. */}
                  <div className="ml-auto flex items-center gap-1.5">
                    <Button
                      size="sm"
                      variant="outline"
                      onClick={() => setSelectMode(true)}
                      disabled={rows.length === 0}
                      title="Pick several downloads to act on at once"
                    >
                      <ListChecks /> Select
                    </Button>
                    {done.length > 0 && (
                      <Button size="sm" variant="ghost" onClick={() => api.clearHistory().catch(report)}>Clear finished</Button>
                    )}
                  </div>
                </>
              )}
            </div>

            <div className="mt-3 space-y-2">
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
                <div className="flex flex-col items-center gap-2 py-16 text-center">
                  <Download className="size-8 text-muted-foreground" />
                  <p className="font-medium">
                    {query
                      ? "Nothing matches that"
                      : `No downloads ${filter !== "all" ? "here" : "yet"}`}
                  </p>
                  <p className="max-w-sm text-sm text-muted-foreground">
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
          </>
        )}
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
            if (msg) toast.success(msg);
          }}
        />
      )}
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
    <Card
      className={cn(
        "flex-row items-center gap-3 p-3",
        selected && "border-primary bg-accent/40",
        selectMode && "cursor-pointer",
      )}
      // In selection mode the whole row is the target, so the tile is an
      // indicator rather than the only thing you can hit.
      onClick={selectMode ? (e) => onSelect(e.shiftKey) : undefined}
    >
      {/* Selection has no column of its own: a picked row swaps its file-type
          tile for a filled check, so entering the mode never re-flows the row
          and an idle list carries no controls at all. */}
      <div className="relative flex size-10 shrink-0 items-center justify-center rounded-md bg-muted text-muted-foreground">
        {selected ? (
          <span className="flex size-10 items-center justify-center rounded-md bg-primary text-primary-foreground" role="img" aria-label="Selected">
            <Check className="size-5" />
          </span>
        ) : preview ? (
          // Video preview thumbnail; overlay a ring while downloading.
          <>
            <img
              className="size-10 rounded-md object-cover"
              src={preview}
              alt=""
              loading="lazy"
              onError={(e) => (e.currentTarget.style.display = "none")}
            />
            {running && percent !== null && (
              <span className="absolute -right-1 -bottom-1 rounded-full bg-background">
                <RingProgress percent={percent} size={20} />
              </span>
            )}
          </>
        ) : running && percent !== null ? (
          <RingProgress percent={percent} size={36} />
        ) : running ? (
          // Unknown size: no percentage to show, so a quiet spinner.
          <Loader2 className="size-5 animate-spin text-primary" />
        ) : (
          kind.icon
        )}
      </div>

      {/* A div with a click handler is invisible to the keyboard, so this
          carries the button role, a tab stop and the keys that go with it.
          In selection mode the row itself owns the click and this is inert. */}
      <div
        className={cn("min-w-0 flex-1", !selectMode && "cursor-pointer")}
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
        <div className="flex items-center justify-between gap-2">
          <span className="truncate text-sm font-medium" title={row.url}>{row.filename}</span>
          <span className={cn("size-2 shrink-0 rounded-full", STATUS_DOT[row.status])} title={STATUS_LABEL[row.status]} />
        </div>

        {row.status !== "completed" && (
          <Progress
            value={indeterminate ? 40 : percent ?? 0}
            className={cn("mt-1.5 h-1.5", indeterminate && "animate-pulse")}
          />
        )}

        <div className="mt-1 flex items-center justify-between text-xs text-muted-foreground">
          <span>
            {formatBytes(downloaded)}
            {total ? ` / ${formatBytes(total)}` : " · unknown size"}
          </span>
          <span className="flex items-center gap-2">
            {running && (
              <>
                <span>{formatBytes(speed)}/s</span>
                {eta && <span>{eta} left</span>}
                {row.segments > 1 && <span>{row.segments} conns</span>}
              </>
            )}
            {row.status === "completed" && <span className="text-emerald-600 dark:text-emerald-400">Completed</span>}
            {row.status === "failed" && <span className="text-destructive">Failed</span>}
            {(row.status === "queued" || row.status === "interrupted") && (
              <span className="flex items-center gap-1">{STATUS_LABEL[row.status]} <Loader2 className="size-3 animate-spin" /></span>
            )}
            {row.status === "paused" && <span>{STATUS_LABEL[row.status]}</span>}
          </span>
        </div>

        {row.error && <p className="mt-1 truncate text-xs text-destructive">{row.error}</p>}
      </div>

      {/* Row actions must not toggle the row underneath them in selection
          mode; each button already handles its own click. */}
      <div className="flex items-center gap-1" onClick={(e) => e.stopPropagation()}>
        {(row.status === "downloading" || row.status === "queued") && (
          <Button variant="ghost" size="icon" title="Pause" onClick={() => api.pause(row.id).catch(onFail)}><Pause /></Button>
        )}
        {(row.status === "paused" || row.status === "interrupted") && (
          <Button variant="ghost" size="icon" title="Resume" onClick={() => api.resume(row.id).catch(onFail)}><Play /></Button>
        )}
        {row.status === "failed" && (
          <Button variant="ghost" size="icon" title="Retry" onClick={() => api.retry(row.id).catch(onFail)}><RotateCcw /></Button>
        )}
        {row.status === "completed" && (
          <Button variant="ghost" size="icon" title="Open file" onClick={() => api.openFile(row.path).catch(onFail)}><ExternalLink /></Button>
        )}
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button variant="ghost" size="icon" title="More">
              <MoreVertical />
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end">
            {row.status === "completed" && (
              <DropdownMenuItem onClick={() => api.revealFile(row.path).catch(onFail)}>
                <Folder /> Open folder
              </DropdownMenuItem>
            )}
            <DropdownMenuItem onClick={() => navigator.clipboard.writeText(row.url)}>
              <Copy /> Copy URL
            </DropdownMenuItem>
            {/* A running transfer holds its `.part` open, so renaming needs a
                pause first — hide the option rather than offer a guaranteed
                error. */}
            {row.status !== "downloading" && (
              <DropdownMenuItem onClick={onRename}>
                <Pencil /> Rename
              </DropdownMenuItem>
            )}
            <DropdownMenuSeparator />
            <DropdownMenuItem variant="destructive" onClick={onDelete}>
              <Trash2 /> Remove
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      </div>
    </Card>
  );
}

/// Glyph per kind. The kind itself comes from lib/filetype, which is where the
/// extension table lives and is tested.
const KIND_ICON: Record<Kind, React.ReactNode> = {
  video: <Video className="size-5" />,
  audio: <Music className="size-5" />,
  archive: <Archive className="size-5" />,
  image: <Image className="size-5" />,
  doc: <FileText className="size-5" />,
  sheet: <Sheet className="size-5" />,
  slides: <Presentation className="size-5" />,
  book: <BookOpen className="size-5" />,
  code: <Code2 className="size-5" />,
  font: <TypeIcon className="size-5" />,
  subs: <Captions className="size-5" />,
  disc: <Disc className="size-5" />,
  package: <Package className="size-5" />,
  torrent: <Magnet className="size-5" />,
  file: <File className="size-5" />,
};

function fileKind(name: string): { icon: React.ReactNode; cls: Kind } {
  const kind = kindOf(name);
  return { icon: KIND_ICON[kind], cls: kind };
}

export default App;
