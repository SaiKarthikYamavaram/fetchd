import { useEffect } from "react";

/// Close a modal with Escape. Every overlay in the app is dismissible by
/// clicking outside, so the keyboard needs the same escape hatch.
export function useEscape(onEscape: () => void) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        onEscape();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onEscape]);
}
