import { api, formatBytes, formatDate, type DownloadView } from "../lib/api";
import { IconOpen, IconFolder, IconCopy, IconX } from "./icons";

/// Full detail for one download — the "properties" screen. Live progress is
/// merged in from the parent so per-segment bars move while it runs.
export function DetailModal({
  row,
  liveBytes,
  speed,
  onClose,
}: {
  row: DownloadView;
  liveBytes?: number;
  speed: number;
  onClose: () => void;
}) {
  const downloaded =
    row.status === "downloading" && liveBytes !== undefined ? liveBytes : row.downloaded;
  const percent = row.total ? Math.min(100, (downloaded / row.total) * 100) : null;

  return (
    <div className="overlay" onClick={onClose}>
      <div className="modal" onClick={(e) => e.stopPropagation()}>
        <div className="modal-head">
          <h2 className="modal-title" title={row.filename}>{row.filename}</h2>
          <button className="act" onClick={onClose} title="Close"><IconX /></button>
        </div>

        <div className="modal-progress">
          <div className={`track ${percent === null ? "indeterminate" : ""}`}>
            <div className="track-fill" style={{ width: percent === null ? "40%" : `${percent}%` }} />
          </div>
          <div className="modal-progress-meta">
            <span>{formatBytes(downloaded)}{row.total ? ` / ${formatBytes(row.total)}` : ""}</span>
            <span>{percent !== null ? `${percent.toFixed(1)}%` : "size unknown"}</span>
            {row.status === "downloading" && <span className="rate-tag">{formatBytes(speed)}/s</span>}
          </div>
        </div>

        <dl className="detail">
          <Field label="Status" value={cap(row.status)} />
          <Field label="Saved to" value={row.path} mono copyable />
          <Field label="Source URL" value={row.url} mono copyable />
          <Field label="Size" value={row.total !== null ? formatBytes(row.total) : "unknown"} />
          <Field
            label="Connections"
            value={
              row.supports_ranges
                ? `${row.segments} (server supports byte ranges)`
                : "1 (server has no range support)"
            }
          />
          <Field label="Added" value={formatDate(row.added_at)} />
          {(row.user_agent || row.referer || row.has_cookie) && (
            <Field
              label="Browser session"
              value={[
                row.has_cookie ? "cookies attached" : null,
                row.referer ? `referer ${row.referer}` : null,
                row.user_agent ? `UA ${row.user_agent}` : null,
              ].filter(Boolean).join(" · ") || "—"}
              mono
            />
          )}
          {row.error && <Field label="Error" value={row.error} error />}
        </dl>

        {row.ranges.length > 1 && (
          <div className="segments">
            <h3>Segments</h3>
            <div className="seg-grid">
              {row.ranges.map(([start, end], i) => {
                const size = end - start + 1;
                const got = row.done[i] ?? 0;
                const pct = size > 0 ? Math.min(100, (got / size) * 100) : 100;
                return (
                  <div className="seg" key={i}>
                    <div className="seg-bar"><div style={{ width: `${pct}%` }} /></div>
                    <span className="seg-label">
                      #{i + 1} · {formatBytes(got)}/{formatBytes(size)}
                    </span>
                  </div>
                );
              })}
            </div>
          </div>
        )}

        <div className="modal-actions">
          {row.status === "completed" && (
            <>
              <button className="btn primary" onClick={() => api.openFile(row.path)}>
                <IconOpen /> Open file
              </button>
              <button className="btn" onClick={() => api.revealFile(row.path)}>
                <IconFolder /> Open folder
              </button>
            </>
          )}
          <button className="btn" onClick={() => navigator.clipboard.writeText(row.url)}>
            <IconCopy /> Copy URL
          </button>
        </div>
      </div>
    </div>
  );
}

function Field({
  label, value, mono, copyable, error,
}: { label: string; value: string; mono?: boolean; copyable?: boolean; error?: boolean }) {
  return (
    <>
      <dt>{label}</dt>
      <dd className={`${mono ? "mono" : ""} ${error ? "err-text" : ""}`}>
        <span className="dd-value">{value}</span>
        {copyable && (
          <button
            className="dd-copy act"
            title="Copy"
            onClick={() => navigator.clipboard.writeText(value)}
          >
            <IconCopy size={14} />
          </button>
        )}
      </dd>
    </>
  );
}

const cap = (s: string) => s.charAt(0).toUpperCase() + s.slice(1);
