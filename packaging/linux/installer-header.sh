#!/bin/sh
# Kladde @VERSION@ installer for Linux: installs for the current user, no root needed.
#   sh kladde-x86_64-linux-installer.sh              install or update
#   sh kladde-x86_64-linux-installer.sh --uninstall  remove (settings and workspaces stay)
# Installs per user so Help > Check for updates can replace the program file.
set -eu

bin_dir="$HOME/.local/bin"
data_dir="${XDG_DATA_HOME:-$HOME/.local/share}"
exe="$bin_dir/kladde"
icon="$data_dir/icons/hicolor/256x256/apps/kladde.png"
desktop="$data_dir/applications/kladde.desktop"

refresh_menus() {
    # Desktops pick the launcher up without these too, just later.
    update-desktop-database -q "$data_dir/applications" 2>/dev/null || true
    gtk-update-icon-cache -q -t "$data_dir/icons/hicolor" 2>/dev/null || true
}

if [ "${1:-}" = "--uninstall" ]; then
    rm -f "$exe" "$exe.old" "$icon" "$desktop"
    refresh_menus
    echo "Kladde removed. Settings and workspaces are kept in $data_dir/Kladde."
    exit 0
fi
if [ $# -gt 0 ]; then
    echo "usage: sh $0 [--uninstall]" >&2
    exit 2
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
payload=$(awk '/^__PAYLOAD__$/ { print NR + 1; exit }' "$0")
tail -n "+$payload" "$0" | tar -xz -C "$tmp"

mkdir -p "$bin_dir" "$(dirname "$icon")" "$(dirname "$desktop")"
# Replaces a running Kladde safely: install unlinks the old file first.
install -m 755 "$tmp/kladde" "$exe"
install -m 644 "$tmp/kladde.png" "$icon"
cat > "$desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Kladde
Comment=Text and hex editor with workspaces
Exec="$exe" %F
Icon=kladde
Terminal=false
Categories=Utility;TextEditor;
MimeType=text/plain;
StartupWMClass=Kladde
EOF
refresh_menus

echo "Kladde @VERSION@ installed to $exe; start it from the application menu."
case ":$PATH:" in
    *":$bin_dir:"*) ;;
    *) echo "To start it from a terminal as 'kladde', add $bin_dir to PATH." ;;
esac
exit 0
__PAYLOAD__
