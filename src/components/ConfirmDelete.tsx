import { api, type DownloadView } from "../lib/api";
import { IconTrash } from "./icons";
import { useEscape } from "../lib/useEscape";

/// Removing an entry has two distinct meanings, so ask which. "Remove from
/// list" keeps the downloaded file; "Delete file too" erases it from disk.
/// For an unfinished download the file on disk is only a partial, so the
/// wording adapts.
///
/// Takes a list so a multi-row selection asks the same question once, rather
/// than needing a second near-identical dialog.
export function ConfirmDelete({
  rows,
  onClose,
}: {
  rows: DownloadView[];
  onClose: () => void;
}) {
  useEscape(onClose);

  const many = rows.length > 1;
  // With a mixed selection the cautious wording wins: say "partial" unless
  // every entry is finished.
  const finished = rows.every((r) => r.status === "completed");
  const ids = rows.map((r) => r.id);

  function removeOnly() {
    api.bulk(ids, "remove");
    onClose();
  }
  function deleteFile() {
    api.bulk(ids, "remove_with_file");
    onClose();
  }

  return (
    <div className="overlay" onClick={onClose}>
      <div className="modal confirm" onClick={(e) => e.stopPropagation()}>
        <div className="confirm-icon"><IconTrash size={22} /></div>
        <h2 className="confirm-title">
          {many ? `Remove ${rows.length} downloads?` : `Remove “${rows[0].filename}”?`}
        </h2>
        <p className="confirm-sub">
          {finished
            ? many
              ? "Keep the downloaded files, or delete them from disk as well."
              : "Keep the downloaded file, or delete it from disk as well."
            : many
              ? "This cancels any that are unfinished. Their partial files can be kept or deleted."
              : "This cancels the download. The partial file can be kept or deleted."}
        </p>
        <div className="confirm-actions">
          <button className="btn" onClick={onClose}>Cancel</button>
          <button className="btn" onClick={removeOnly}>Remove from list</button>
          <button className="btn danger" onClick={deleteFile}>
            <IconTrash size={15} /> {many ? "Delete files" : "Delete file"}
          </button>
        </div>
      </div>
    </div>
  );
}
