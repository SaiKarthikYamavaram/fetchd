import { Minus, Square, X } from "lucide-react";
import { getCurrentWindow } from "@tauri-apps/api/window";

const win = getCurrentWindow();

export function Titlebar() {
  return (
    <div
      data-tauri-drag-region
      className="flex h-8 shrink-0 items-center justify-end border-b border-border/80 bg-card/90 dark:bg-card/60 select-none"
    >
      <button
        type="button"
        aria-label="Minimize"
        className="inline-flex h-8 w-10 items-center justify-center text-muted-foreground hover:bg-accent hover:text-accent-foreground"
        onClick={() => win.minimize()}
      >
        <Minus className="size-3.5" />
      </button>
      <button
        type="button"
        aria-label="Maximize"
        className="inline-flex h-8 w-10 items-center justify-center text-muted-foreground hover:bg-accent hover:text-accent-foreground"
        onClick={() => win.toggleMaximize()}
      >
        <Square className="size-3" />
      </button>
      <button
        type="button"
        aria-label="Close"
        className="inline-flex h-8 w-10 items-center justify-center text-muted-foreground hover:bg-destructive hover:text-white"
        onClick={() => win.close()}
      >
        <X className="size-3.5" />
      </button>
    </div>
  );
}
