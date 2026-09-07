#!/usr/bin/env python3
"""Package a published GH archive helper as a separate per-user add-on."""
import argparse
from pathlib import Path
import shutil
import tarfile
import tempfile
import zipfile

VERSION = "0.2.0"
ROOT = Path(__file__).resolve().parent

INSTALL_SH = '''#!/bin/sh
set -eu
base=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
target="${MANTIS_GH_INSTALL_DIR:-$HOME/.local/share/mantis-cad/compat/grasshopper}"
if [ -L "$target" ] || { [ -e "$target" ] && { [ ! -d "$target" ] || [ ! -f "$target/.mantis-gh-pack" ] || [ -L "$target/.mantis-gh-pack" ]; }; }; then
    printf 'Refusing to replace an unowned folder or symbolic link: %s\\n' "$target" >&2
    exit 1
fi
if [ -d "$target" ] && find "$target" -type l -exec test -d {} \\; -print | grep -q .; then
    printf 'Refusing to traverse a symbolic-link directory inside the installation.\\n' >&2
    exit 1
fi
parent=$(dirname -- "$target")
mkdir -p -- "$parent"
stage=$(mktemp -d "$parent/.mantis-gh-install-XXXXXX")
backup=""
cleanup() { rm -rf -- "$stage"; }
trap cleanup EXIT HUP INT TERM
if [ -d "$target" ]; then cp -a "$target/." "$stage/"; fi
cp -a --remove-destination "$base/compat/grasshopper/." "$stage/"
if [ -e "$target" ]; then
    backup=$(mktemp -d "$parent/.mantis-gh-backup-XXXXXX")
    rmdir -- "$backup"
    mv -- "$target" "$backup"
fi
if mv -- "$stage" "$target"; then
    [ -z "$backup" ] || rm -rf -- "$backup"
else
    [ -z "$backup" ] || mv -- "$backup" "$target"
    exit 1
fi
printf 'Installed Grasshopper archive pack: %s\\n' "$target"
'''

INSTALL_PS1 = r'''$ErrorActionPreference = 'Stop'
$target = if ($env:MANTIS_GH_INSTALL_DIR) { $env:MANTIS_GH_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'MantisCAD\compat\grasshopper' }
if (Test-Path -LiteralPath $target) {
    $existing = Get-Item -LiteralPath $target -Force
    if (-not $existing.PSIsContainer -or ($existing.Attributes -band [IO.FileAttributes]::ReparsePoint) -or -not (Test-Path -LiteralPath (Join-Path $target '.mantis-gh-pack') -PathType Leaf)) {
        throw "Refusing to replace an unowned folder or reparse point: $target"
    }
    if (Get-ChildItem -LiteralPath $target -Recurse -Force | Where-Object { $_.Attributes -band [IO.FileAttributes]::ReparsePoint } | Select-Object -First 1) {
        throw 'Refusing to traverse reparse points inside the installation'
    }
}
$parent = Split-Path -Parent $target
New-Item -ItemType Directory -Force -Path $parent | Out-Null
$stage = Join-Path $parent ('.mantis-gh-install-' + [Guid]::NewGuid().ToString('N'))
$backup = $null
try {
    New-Item -ItemType Directory -Path $stage | Out-Null
    if (Test-Path -LiteralPath $target) {
        Get-ChildItem -LiteralPath $target -Force | Copy-Item -Destination $stage -Recurse -Force
    }
    Copy-Item -Path (Join-Path $PSScriptRoot 'compat\grasshopper\*') -Destination $stage -Recurse -Force
    if (Test-Path -LiteralPath $target) {
        $backup = Join-Path $parent ('.mantis-gh-backup-' + [Guid]::NewGuid().ToString('N'))
        Move-Item -LiteralPath $target -Destination $backup
    }
    try { Move-Item -LiteralPath $stage -Destination $target }
    catch {
        if ($backup) { Move-Item -LiteralPath $backup -Destination $target; $backup = $null }
        throw
    }
    if ($backup) { Remove-Item -LiteralPath $backup -Recurse -Force }
    Write-Host "Installed Grasshopper archive pack: $target"
} finally {
    if (Test-Path -LiteralPath $stage) { Remove-Item -LiteralPath $stage -Recurse -Force }
}
'''

UNINSTALL_SH = '''#!/bin/sh
set -eu
target=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
[ -f "$target/.mantis-gh-pack" ] && [ -f "$target/.mantis-gh-files" ] || exit 1
while IFS= read -r relative; do
    case "$relative" in ''|/*|../*|*/../*|*/..) printf 'Invalid package manifest\\n' >&2; exit 1;; esac
    parent="$target/$(dirname -- "$relative")"
    while [ "$parent" != "$target" ] && [ "$parent" != "$target/." ]; do
        [ ! -L "$parent" ] || { printf 'Refusing to traverse a symbolic-link directory.\\n' >&2; exit 1; }
        parent=$(dirname -- "$parent")
    done
    if [ -f "$target/$relative" ] || [ -L "$target/$relative" ]; then rm -- "$target/$relative"; fi
done < "$target/.mantis-gh-files"
while IFS= read -r relative; do
    case "$relative" in ''|/*|../*|*/../*|*/..) printf 'Invalid directory manifest\\n' >&2; exit 1;; esac
    rmdir -- "$target/$relative" 2>/dev/null || true
done < "$target/.mantis-gh-dirs"
rm -- "$target/.mantis-gh-files"
rm -- "$target/.mantis-gh-dirs"
rmdir -- "$target" 2>/dev/null || true
printf 'Removed Grasshopper archive pack files. Other files and folders were preserved.\\n'
'''

UNINSTALL_PS1 = r'''$ErrorActionPreference = 'Stop'
$target = $PSScriptRoot
if (-not (Test-Path -LiteralPath (Join-Path $target '.mantis-gh-pack') -PathType Leaf)) { throw 'Missing package ownership marker' }
$files = Get-Content -LiteralPath (Join-Path $target '.mantis-gh-files')
$directories = Get-Content -LiteralPath (Join-Path $target '.mantis-gh-dirs')
foreach ($relative in $files) {
    if (-not $relative -or [IO.Path]::IsPathRooted($relative) -or ($relative -split '[/\\]' -contains '..')) { throw 'Invalid package manifest' }
    $path = Join-Path $target $relative
    $parent = Split-Path -Parent $path
    while ($parent -and $parent -ne $target) {
        if ((Get-Item -LiteralPath $parent -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Refusing to traverse a reparse-point directory' }
        $parent = Split-Path -Parent $parent
    }
    if (Test-Path -LiteralPath $path -PathType Leaf) { Remove-Item -LiteralPath $path -Force }
}
foreach ($relative in $directories) {
    if (-not $relative -or [IO.Path]::IsPathRooted($relative) -or ($relative -split '[/\\]' -contains '..')) { throw 'Invalid directory manifest' }
    $path = Join-Path $target $relative
    if ((Test-Path -LiteralPath $path -PathType Container) -and -not (Get-ChildItem -LiteralPath $path -Force | Select-Object -First 1)) { Remove-Item -LiteralPath $path -Force }
}
Remove-Item -LiteralPath (Join-Path $target '.mantis-gh-files') -Force
Remove-Item -LiteralPath (Join-Path $target '.mantis-gh-dirs') -Force
if (-not (Get-ChildItem -LiteralPath $target -Force | Select-Object -First 1)) { Remove-Item -LiteralPath $target -Force }
Write-Host 'Removed Grasshopper archive pack files. Other files and folders were preserved.'
'''


def write_manifest(payload):
    files = sorted(p.relative_to(payload).as_posix() for p in payload.rglob("*") if p.is_file() or p.is_symlink())
    (payload / ".mantis-gh-files").write_text("\n".join(files) + "\n")
    directories = sorted((p.relative_to(payload) for p in payload.rglob("*") if p.is_dir() and not p.is_symlink()), key=lambda p: (-len(p.parts), p.as_posix()))
    (payload / ".mantis-gh-dirs").write_text("".join(p.as_posix() + "\n" for p in directories))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--published", type=Path, required=True)
    parser.add_argument("--platform", choices=["windows", "linux"], required=True)
    parser.add_argument("--output", type=Path, default=ROOT.parents[1] / "dist-downloads")
    args = parser.parse_args()
    executable = "mantis-gh-io.exe" if args.platform == "windows" else "mantis-gh-io"
    if not (args.published / executable).is_file():
        raise RuntimeError(f"Published helper is missing {executable}")
    args.output.mkdir(parents=True, exist_ok=True)
    name = f"mantis-cad-{VERSION}-grasshopper-{args.platform}-x86_64"
    with tempfile.TemporaryDirectory(prefix="mantis-gh-package-") as temp:
        folder = Path(temp) / name
        shutil.copytree(args.published, folder / "compat/grasshopper", symlinks=True)
        payload = folder / "compat/grasshopper"
        (payload / ".mantis-gh-pack").write_text(f"MantisCAD Grasshopper archive pack {VERSION}\n")
        shutil.copyfile(ROOT / "README.md", folder / "README.md")
        if args.platform == "windows":
            (payload / "uninstall.ps1").write_text(UNINSTALL_PS1, encoding="utf-8")
            (payload / "uninstall.cmd").write_bytes(b'@echo off\r\npowershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0uninstall.ps1"\r\npause\r\n')
            (folder / "install.ps1").write_text(INSTALL_PS1, encoding="utf-8")
            (folder / "install.cmd").write_bytes(b'@echo off\r\npowershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0install.ps1"\r\nif errorlevel 1 (pause & exit /b 1)\r\npause\r\n')
            write_manifest(payload)
            destination = args.output / f"{name}.zip"
            with zipfile.ZipFile(destination, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as archive:
                for path in sorted(folder.rglob("*")):
                    if path.is_file():
                        archive.write(path, path.relative_to(folder.parent))
            with zipfile.ZipFile(destination) as archive:
                if archive.testzip() is not None:
                    raise RuntimeError("Archive CRC verification failed")
        else:
            uninstaller = payload / "uninstall.sh"
            uninstaller.write_text(UNINSTALL_SH)
            uninstaller.chmod(0o755)
            installer = folder / "install.sh"
            installer.write_text(INSTALL_SH)
            installer.chmod(0o755)
            write_manifest(payload)
            destination = args.output / f"{name}.tar.gz"
            with tarfile.open(destination, "w:gz", compresslevel=9) as archive:
                archive.add(folder, arcname=name)
        print(f"{destination} ({destination.stat().st_size / 1024**2:.1f} MiB)")


if __name__ == "__main__":
    main()
