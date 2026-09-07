#!/bin/sh
set -eu

app_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
if [ ! -f "$app_dir/.mantis-install" ] ||
   [ "$(cat "$app_dir/.mantis-install")" != "MantisCAD user installation v1" ]; then
    echo "Run the installed uninstaller in PREFIX/lib/mantis-cad/." >&2
    exit 1
fi
prefix=$(CDPATH= cd -- "$app_dir/../.." && pwd)
launcher="$prefix/bin/mantis-cad"
desktop="$prefix/share/applications/mantis-cad.desktop"
if [ -L "$launcher" ] && [ "$(readlink "$launcher")" = "$app_dir/mantis-app" ]; then
    rm -- "$launcher"
fi
if [ -f "$desktop" ] && grep -Fqx "X-MantisCAD-InstallDir=$app_dir" "$desktop"; then
    rm -f -- "$desktop" "$prefix/share/icons/hicolor/scalable/apps/mantis-cad.svg"
fi
# Remove only installer-owned files. Project files and preferences are preserved.
rm -f -- "$app_dir/mantis-app" "$app_dir/LICENSE" "$app_dir/INSTALL.md" \
    "$app_dir/COMMANDS.md" "$app_dir/INTEROP.md" "$app_dir/THIRD_PARTY_LICENSES.md" "$app_dir/.mantis-install" "$app_dir/uninstall.sh"
rmdir -- "$app_dir" 2>/dev/null || true
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$prefix/share/applications" >/dev/null 2>&1 || true
fi
echo "MantisCAD uninstalled. Your projects and preferences were preserved."
