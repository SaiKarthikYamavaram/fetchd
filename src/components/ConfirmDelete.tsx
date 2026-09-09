import { api, type DownloadView } from "../lib/api";
import { IconTrash } from "./icons";
import { useEscape } from "../lib/useEscape";

/// Removing an entry has two distinct meanings, so ask which. "Remove from
/// list" keeps the downloaded file; "Delete file too" erases it from disk.
/// For an unfinished download the file on disk is only a partial, so the
/// wording adapts.
export function ConfirmDelete({
  row,
  onClose,
}: {
  row: DownloadView;
  onClose: () => void;
}) {
  useEscape(onClose);

  const finished = row.status === "completed";

  function removeOnly() {
    api.remove(row.id, false);
    onClose();
  }
  function deleteFile() {
    api.remove(row.id, true);
    onClose();
  }

  return (
    <div className="overlay" onClick={onClose}>
      <div className="modal confirm" onClick={(e) => e.stopPropagation()}>
        <div className="confirm-icon"><IconTrash size={22} /></div>
        <h2 className="confirm-title">Remove “{row.filename}”?</h2>
        <p className="confirm-sub">
          {finished
            ? "Keep the downloaded file, or delete it from disk as well."
            : "This cancels the download. The partial file can be kept or deleted."}
        </p>
        <div className="confirm-actions">
          <button className="btn" onClick={onClose}>Cancel</button>
          <button className="btn" onClick={removeOnly}>Remove from list</button>
          <button className="btn danger" onClick={deleteFile}>
            <IconTrash size={15} /> Delete file
          </button>
        </div>
      </div>
    </div>
  );
}
