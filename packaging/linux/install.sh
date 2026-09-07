#!/bin/sh
set -eu

prefix="${HOME}/.local"
if [ "$#" -gt 0 ]; then
    if [ "$#" -ne 2 ] || [ "$1" != "--prefix" ]; then
        echo "Usage: ./install.sh [--prefix /absolute/install/directory]" >&2
        exit 2
    fi
    prefix="$2"
fi
case "$prefix" in
    /*) ;;
    *) echo "The install prefix must be an absolute path." >&2; exit 2 ;;
esac
case "$prefix" in
    *'='*) echo "The install prefix must not contain = (desktop entry restriction)." >&2; exit 2 ;;
esac
if printf '%s' "$prefix" | LC_ALL=C grep -q '[[:cntrl:]]'; then
    echo "The install prefix must not contain control characters." >&2
    exit 2
fi
# Desktop entries are line based; reject path separators that cannot be encoded.
case "$prefix" in
    *"
"*) echo "The install prefix must not contain a newline." >&2; exit 2 ;;
esac
source_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
mkdir -p "$prefix"
prefix=$(CDPATH= cd -- "$prefix" && pwd)
app_dir="$prefix/lib/mantis-cad"
launcher="$prefix/bin/mantis-cad"
desktop="$prefix/share/applications/mantis-cad.desktop"
icon="$prefix/share/icons/hicolor/scalable/apps/mantis-cad.svg"
marker="MantisCAD user installation v1"

if [ -e "$app_dir" ] || [ -L "$app_dir" ]; then
    if [ -L "$app_dir" ] || [ ! -f "$app_dir/.mantis-install" ] ||
       [ "$(cat "$app_dir/.mantis-install")" != "$marker" ]; then
        echo "Refusing to replace an existing, unrecognized directory: $app_dir" >&2
        exit 1
    fi
fi
if [ -e "$launcher" ] || [ -L "$launcher" ]; then
    if [ ! -L "$launcher" ] || [ "$(readlink "$launcher")" != "$app_dir/mantis-app" ]; then
        echo "Refusing to replace an existing command: $launcher" >&2
        exit 1
    fi
fi
if [ -e "$desktop" ] && ! grep -Fqx "X-MantisCAD-InstallDir=$app_dir" "$desktop"; then
    echo "Refusing to replace an existing desktop entry: $desktop" >&2
    exit 1
fi
if [ -e "$icon" ] && [ ! -f "$app_dir/.mantis-install" ]; then
    echo "Refusing to replace an existing icon: $icon" >&2
    exit 1
fi
mkdir -p "$app_dir" "$prefix/bin" "$prefix/share/applications" "$(dirname "$icon")"
cp "$source_dir/mantis-app" "$app_dir/mantis-app.new"
chmod 755 "$app_dir/mantis-app.new"
mv -f "$app_dir/mantis-app.new" "$app_dir/mantis-app"
cp "$source_dir/LICENSE" "$source_dir/INSTALL.md" "$source_dir/COMMANDS.md" "$source_dir/INTEROP.md" "$source_dir/uninstall.sh" "$app_dir/"
cp "$source_dir/THIRD_PARTY_LICENSES.md" "$app_dir/"
chmod 755 "$app_dir/uninstall.sh"
printf '%s\n' "$marker" > "$app_dir/.mantis-install"
ln -sfn "$app_dir/mantis-app" "$launcher"
cp "$source_dir/mantis-cad.svg" "$icon"
# Escape the quoted argument, then the Desktop Entry string value itself.
exec_path=$(printf '%s' "$app_dir/mantis-app" | sed 's/\\/\\\\/g; s/"/\\"/g; s/`/\\`/g; s/\$/\\$/g; s/%/%%/g' | sed 's/\\/\\\\/g')
{
    printf '%s\n' '[Desktop Entry]' 'Version=1.0' 'Type=Application' 'Name=MantisCAD'
    # GLib validates the first executable before expanding %% field escapes.
    # env keeps a literal percent in a custom installation path launchable.
    printf 'Exec=/usr/bin/env "%s"\n' "$exec_path"
    printf '%s\n' 'Icon=mantis-cad' 'Terminal=false' 'Categories=Graphics;3DGraphics;Engineering;'
    printf 'X-MantisCAD-InstallDir=%s\n' "$app_dir"
} > "$desktop"
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$prefix/share/applications" >/dev/null 2>&1 || true
fi
echo "Installed MantisCAD: $launcher"
echo "Open MantisCAD from the application menu, or run: \"$launcher\""
echo "Uninstall: \"$app_dir/uninstall.sh\""
