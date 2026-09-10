#!/usr/bin/env bash
#
# Install spool for the current user. No root, nothing outside ~/.local, and
# reversible with --uninstall.
#
# A distro package would be the tidier answer, but the bundles tauri produces
# are .deb/.rpm/AppImage — none of which is a natural fit on an Arch-based
# system, and all of which want root. This installs what those packages would:
# the binary, a desktop entry, and the icons.

set -euo pipefail

APP=spool
LEGACY=fetchd          # what the app was called before it was renamed
BIN_DIR="${XDG_BIN_HOME:-$HOME/.local/bin}"
APP_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
ICON_ROOT="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor"
AUTOSTART="${XDG_CONFIG_HOME:-$HOME/.config}/autostart/$APP.desktop"
LEGACY_AUTOSTART="${XDG_CONFIG_HOME:-$HOME/.config}/autostart/$LEGACY.desktop"

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
release="$here/src-tauri/target/release"
icons="$here/src-tauri/icons"

say() { printf '%s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

# Remove what the pre-rename install left behind.
#
# Nothing here is shared with the current install: a separate binary on PATH, a
# second entry in the launcher, and an autostart entry pointing at a binary that
# is about to be deleted. The data directories are NOT touched — the app moves
# those itself on its first launch, which is the only place that knows whether
# the move already happened.
purge_legacy() {
  local found=
  for f in "$BIN_DIR/$LEGACY" "$APP_DIR/$LEGACY.desktop" "$LEGACY_AUTOSTART"; do
    if [[ -e "$f" ]]; then
      rm -f "$f"
      found=1
    fi
  done
  find "$ICON_ROOT" -name "$LEGACY.png" -delete 2>/dev/null || true
  if [[ -n "$found" ]]; then
    say "Removed the old $LEGACY install. Its downloads, settings and queue are"
    say "kept, and move to $APP the first time you launch it."
  fi
  return 0
}

uninstall() {
  rm -f "$BIN_DIR/$APP" "$APP_DIR/$APP.desktop" "$AUTOSTART"
  find "$ICON_ROOT" -name "$APP.png" -delete 2>/dev/null || true
  purge_legacy
  command -v update-desktop-database >/dev/null && update-desktop-database "$APP_DIR" 2>/dev/null || true
  say "Removed $APP. Downloads, settings and the queue are untouched:"
  say "  ${XDG_DATA_HOME:-$HOME/.local/share}/com.saikarthik.spool"
  say "  ${XDG_CONFIG_HOME:-$HOME/.config}/com.saikarthik.spool"
  exit 0
}

[[ "${1:-}" == "--uninstall" ]] && uninstall
[[ "${1:-}" == "" ]] || die "unknown argument: $1 (only --uninstall is accepted)"

[[ -x "$release/$APP" ]] || die "no release binary at $release/$APP — run 'npm run tauri build' first"

# The app is likely running from a previous install; replacing a busy binary
# fails with ETXTBSY, so stop it first.
for proc in "$APP" "$LEGACY"; do
  if pgrep -x "$proc" >/dev/null 2>&1; then
    say "Stopping the running $proc…"
    pkill -x "$proc" || true
    sleep 1
  fi
done

purge_legacy

install -Dm755 "$release/$APP" "$BIN_DIR/$APP"
say "Installed $BIN_DIR/$APP"

for png in "$icons"/*x*.png; do
  [[ -e "$png" ]] || continue
  size="$(basename "$png" .png)"          # e.g. 128x128, 128x128@2x
  [[ "$size" == *@* ]] && continue        # hicolor has no @2x convention
  install -Dm644 "$png" "$ICON_ROOT/$size/apps/$APP.png"
done
say "Installed icons under $ICON_ROOT"

install -d "$APP_DIR"
cat > "$APP_DIR/$APP.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=spool
GenericName=Download Manager
Comment=Segmented downloads, video sites, and browser hand-off
Exec=$BIN_DIR/$APP %U
Icon=$APP
Terminal=false
Categories=Network;FileTransfer;
Keywords=download;downloader;idm;video;yt-dlp;
StartupWMClass=spool
EOF
say "Installed $APP_DIR/$APP.desktop"

command -v update-desktop-database >/dev/null && update-desktop-database "$APP_DIR" 2>/dev/null || true
command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -qtf "$ICON_ROOT" 2>/dev/null || true

# An autostart entry left by a previous install points at wherever that build
# lived. The app rewrites it at launch, but say so rather than leaving it to
# look broken in the meantime.
if [[ -f "$AUTOSTART" ]] && ! grep -qF "$BIN_DIR/$APP" "$AUTOSTART"; then
  say "Note: the autostart entry points elsewhere; it is rewritten next launch."
fi

case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) say "Note: $BIN_DIR is not on your PATH — add it to launch from a shell." ;;
esac

say
say "Done. Launch it from your app menu, or run: $APP"
