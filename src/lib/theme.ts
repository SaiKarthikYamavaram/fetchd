/// Apply the theme choice to the document root.
///
/// "system" removes the attribute so the `prefers-color-scheme` media query
/// decides; "light"/"dark" pin it and win over the OS preference.
export function applyTheme(theme: string | undefined) {
  const root = document.documentElement;
  if (theme === "light" || theme === "dark") {
    root.dataset.theme = theme;
  } else {
    delete root.dataset.theme;
  }
  try {
    if (typeof window !== "undefined" && window.localStorage) {
      if (theme) {
        window.localStorage.setItem("spool_theme", theme);
      } else {
        window.localStorage.removeItem("spool_theme");
      }
    }
  } catch {}
}
