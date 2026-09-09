import { useState } from "react";
import { api, type DownloadView } from "../lib/api";
import { IconEdit, IconX } from "./icons";
import { useEscape } from "../lib/useEscape";

/// Rename a download already in the list. The file on disk moves with it —
/// the finished file for a completed entry, the `.part` for an unfinished one.
export function RenameDialog({
  row,
  onClose,
}: {
  row: DownloadView;
  onClose: () => void;
}) {
  const [name, setName] = useState(row.filename);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEscape(onClose);

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    if (busy) return;
    const next = name.trim();
    if (!next || next === row.filename) {
      onClose();
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await api.rename(row.id, next);
      onClose();
    } catch (e) {
      setError(String(e));
      setBusy(false);
    }
  }

  return (
    <div className="overlay" onClick={onClose}>
      <form className="modal" onClick={(e) => e.stopPropagation()} onSubmit={submit}>
        <div className="modal-head">
          <h2 className="modal-title">Rename</h2>
          <button type="button" className="act" onClick={onClose} title="Close">
            <IconX />
          </button>
        </div>

        <label className="field">
          <span>Filename</span>
          <input
            value={name}
            onChange={(e) => setName(e.currentTarget.value)}
            spellCheck={false}
            autoFocus
          />
        </label>
        <p className="help">
          {row.status === "completed"
            ? "The finished file is renamed on disk."
            : "The partial file is renamed too, so the download still resumes."}
        </p>

        {error && <p className="err inline">{error}</p>}

        <div className="modal-actions">
          <button type="submit" className="btn primary" disabled={busy}>
            <IconEdit /> {busy ? "Renaming…" : "Rename"}
          </button>
          <button type="button" className="btn" onClick={onClose}>Cancel</button>
        </div>
      </form>
    </div>
  );
}
