import { useState } from "react";
import { Pencil } from "lucide-react";
import { api, type DownloadView } from "../lib/api";
import { Button } from "./ui/button";
import { Dialog, DialogContent, DialogFooter, DialogHeader, DialogTitle } from "./ui/dialog";
import { Input } from "./ui/input";
import { Label } from "./ui/label";

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
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent>
        <form onSubmit={submit}>
          <DialogHeader>
            <DialogTitle>Rename</DialogTitle>
          </DialogHeader>

          <div className="space-y-1.5 py-4">
            <Label htmlFor="rename-name">Filename</Label>
            <Input
              id="rename-name"
              value={name}
              onChange={(e) => setName(e.currentTarget.value)}
              spellCheck={false}
              autoFocus
            />
          </div>
          <p className="text-sm text-muted-foreground">
            {row.status === "completed"
              ? "The finished file is renamed on disk."
              : "The partial file is renamed too, so the download still resumes."}
          </p>

          {error && <p className="mt-2 text-sm text-destructive">{error}</p>}

          <DialogFooter className="mt-4">
            <Button type="button" variant="outline" onClick={onClose}>Cancel</Button>
            <Button type="submit" disabled={busy}>
              <Pencil /> {busy ? "Renaming…" : "Rename"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
