#!/usr/bin/env bash
#
# Install fetchd for the current user. No root, nothing outside ~/.local, and
# reversible with --uninstall.
#
# A distro package would be the tidier answer, but the bundles tauri produces
# are .deb/.rpm/AppImage — none of which is a natural fit on an Arch-based
# system, and all of which want root. This installs what those packages would:
# the binary, a desktop entry, and the icons.

set -euo pipefail

APP=fetchd
BIN_DIR="${XDG_BIN_HOME:-$HOME/.local/bin}"
APP_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
ICON_ROOT="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor"
AUTOSTART="${XDG_CONFIG_HOME:-$HOME/.config}/autostart/$APP.desktop"

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
release="$here/src-tauri/target/release"
icons="$here/src-tauri/icons"

say() { printf '%s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

uninstall() {
  rm -f "$BIN_DIR/$APP" "$APP_DIR/$APP.desktop" "$AUTOSTART"
  find "$ICON_ROOT" -name "$APP.png" -delete 2>/dev/null || true
  command -v update-desktop-database >/dev/null && update-desktop-database "$APP_DIR" 2>/dev/null || true
  say "Removed $APP. Downloads, settings and the queue are untouched:"
  say "  ${XDG_DATA_HOME:-$HOME/.local/share}/com.saikarthik.fetchd"
  say "  ${XDG_CONFIG_HOME:-$HOME/.config}/com.saikarthik.fetchd"
  exit 0
}

[[ "${1:-}" == "--uninstall" ]] && uninstall
[[ "${1:-}" == "" ]] || die "unknown argument: $1 (only --uninstall is accepted)"

[[ -x "$release/$APP" ]] || die "no release binary at $release/$APP — run 'npm run tauri build' first"

# The app is likely running from a previous install; replacing a busy binary
# fails with ETXTBSY, so stop it first.
if pgrep -x "$APP" >/dev/null 2>&1; then
  say "Stopping the running $APP…"
  pkill -x "$APP" || true
  sleep 1
fi

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
Name=fetchd
GenericName=Download Manager
Comment=Segmented downloads, video sites, and browser hand-off
Exec=$BIN_DIR/$APP %U
Icon=$APP
Terminal=false
Categories=Network;FileTransfer;
Keywords=download;downloader;idm;video;yt-dlp;
StartupWMClass=fetchd
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
