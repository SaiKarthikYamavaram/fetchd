import { Minus, PanelLeft, Square, X } from "lucide-react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { Logo } from "./Logo";

const win = getCurrentWindow();

export function Titlebar({
  onToggleSidebar,
  sidebarCollapsed,
}: {
  onToggleSidebar?: () => void;
  sidebarCollapsed?: boolean;
} = {}) {
  return (
    <div
      data-tauri-drag-region
      className="flex h-10 shrink-0 items-center justify-between border-b border-border/80 bg-card/90 dark:bg-card/60 select-none"
    >
      <div className="flex items-center gap-1.5 px-2">
        {onToggleSidebar && (
          <button
            type="button"
            aria-label={sidebarCollapsed ? "Expand sidebar (Ctrl+B)" : "Collapse sidebar (Ctrl+B)"}
            title={sidebarCollapsed ? "Expand sidebar (Ctrl+B)" : "Collapse sidebar (Ctrl+B)"}
            className="inline-flex h-7 w-7 items-center justify-center rounded-md text-muted-foreground hover:bg-accent hover:text-accent-foreground transition-colors"
            onClick={onToggleSidebar}
          >
            <PanelLeft className="size-3.5" />
          </button>
        )}
        {sidebarCollapsed && (
          <div className="flex items-center gap-1.5 pl-0.5">
            <span className="flex size-4.5 items-center justify-center rounded-full bg-gradient-to-br from-primary to-violet-500 text-primary-foreground">
              <Logo size={11} />
            </span>
            <span className="bg-gradient-to-r from-primary to-violet-500 bg-clip-text text-xs font-bold tracking-tight text-transparent">
              spool
            </span>
          </div>
        )}
      </div>

      <div className="flex-1 h-full" data-tauri-drag-region />

      <div className="flex items-center">
        <button
          type="button"
          aria-label="Minimize"
          className="inline-flex h-10 w-12 items-center justify-center text-muted-foreground hover:bg-accent hover:text-accent-foreground"
          onClick={() => win.minimize()}
        >
          <Minus className="size-3.5" />
        </button>
        <button
          type="button"
          aria-label="Maximize"
          className="inline-flex h-10 w-12 items-center justify-center text-muted-foreground hover:bg-accent hover:text-accent-foreground"
          onClick={() => win.toggleMaximize()}
        >
          <Square className="size-3" />
        </button>
        <button
          type="button"
          aria-label="Close"
          className="inline-flex h-10 w-12 items-center justify-center text-muted-foreground hover:bg-destructive hover:text-white"
          onClick={() => win.close()}
        >
          <X className="size-3.5" />
        </button>
      </div>
    </div>
  );
}

