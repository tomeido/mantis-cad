#!/usr/bin/env python3
"""Install an optional addon per user; preserve every file we do not own."""

import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import shutil
import stat
import sys
import tempfile

MANIFEST = ".mantis-python-addon.json"


def is_redirect(path):
    try:
        metadata = path.lstat()
    except FileNotFoundError:
        return False
    return stat.S_ISLNK(metadata.st_mode) or bool(getattr(metadata, "st_file_attributes", 0) & stat.FILE_ATTRIBUTE_REPARSE_POINT)


def default_target():
    if os.name == "nt":
        return Path(os.environ["LOCALAPPDATA"]) / "MantisCAD" / "compat"
    return Path.home() / ".local" / "share" / "mantis-cad" / "compat"


def digest(path):
    with path.open("rb") as stream:
        result = hashlib.sha256()
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            result.update(chunk)
        return result.hexdigest()


def child(root, relative):
    path = PurePosixPath(relative)
    if path.is_absolute() or not path.parts or any(part in ("", ".", "..") or ":" in part or "\\" in part for part in path.parts):
        raise ValueError("Unsafe addon manifest path")
    target = root.joinpath(*path.parts)
    current = target
    while current != root.parent:
        if is_redirect(current):
            raise ValueError(f"Refusing to follow an installation symlink or reparse point: {current}")
        current = current.parent
    return target


def manifest(root):
    marker = child(root, MANIFEST)
    if not marker.exists():
        return {}
    value = json.loads(marker.read_text(encoding="utf-8"))
    if not isinstance(value, dict) or value.get("product") != "MantisCAD Python compatibility addon" or not isinstance(value.get("files"), dict):
        raise ValueError("This directory does not have a recognized addon manifest")
    for relative, checksum in value["files"].items():
        child(root, relative)
        if not isinstance(checksum, str) or len(checksum) != 64:
            raise ValueError("Invalid addon file checksum")
    return value["files"]


def install(source, target):
    source, target = source.absolute(), target.absolute()
    if source == target or source in target.parents or target in source.parents:
        raise ValueError("Installation source and destination must be separate directories")
    previous = manifest(target)
    if target.exists() and any(target.iterdir()) and not previous:
        gh_marker = child(target, "grasshopper/.mantis-gh-pack")
        gh_owned = gh_marker.is_file() and gh_marker.read_text(encoding="utf-8").startswith("MantisCAD Grasshopper archive pack ")
        if not gh_owned:
            raise ValueError(f"Refusing an existing directory without a recognized addon ownership marker: {target}")
    files = {}
    if is_redirect(source):
        raise ValueError("Addon source directory must not be a symlink or reparse point")
    for path in source.rglob("*"):
        if is_redirect(path):
            raise ValueError(f"Addon payload must not contain symlinks or reparse points: {path}")
        if path.is_file():
            relative = path.relative_to(source).as_posix()
            if relative == MANIFEST:
                raise ValueError("Payload must not replace the installer ownership manifest")
            destination = child(target, relative)
            if destination.exists() and relative not in previous:
                raise ValueError(f"Existing unmanaged file would be overwritten: {destination}")
            files[relative] = digest(path)
    if "compat.py" not in files:
        raise ValueError("Addon payload is missing compat.py")
    target.mkdir(parents=True, exist_ok=True)
    for relative in files:
        destination = child(target, relative)
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source / relative, destination)
    for relative, checksum in previous.items():
        destination = child(target, relative)
        if relative not in files and destination.is_file() and digest(destination) == checksum:
            try:
                destination.unlink()
            except PermissionError:
                pass  # A running backend may still hold this known old DLL.
    retained = {relative: checksum for relative, checksum in previous.items() if child(target, relative).exists()}
    retained.update(files)
    marker = target / MANIFEST
    descriptor, name = tempfile.mkstemp(prefix=".mantis-addon-manifest-", dir=target)
    os.close(descriptor)
    temporary = Path(name)
    temporary.write_text(json.dumps({"product": "MantisCAD Python compatibility addon", "files": retained}, indent=2) + "\n", encoding="utf-8")
    temporary.replace(marker)
    print(f"Installed compatibility addon: {target}")


def uninstall(target):
    target = target.absolute()
    files = manifest(target)
    if not files:
        raise ValueError("No installed Python compatibility addon was found")
    preserved = {}
    directories = set()
    for relative, checksum in files.items():
        path = child(target, relative)
        if path.is_file():
            if digest(path) != checksum:
                preserved[relative] = checksum
                continue
            try:
                path.unlink()
            except PermissionError:
                # On Windows an interpreter cannot delete its own running EXE/DLL.
                preserved[relative] = checksum
                continue
        directories.update(parent for parent in path.parents if parent == target or target in parent.parents)
    for directory in sorted(directories, key=lambda p: len(p.parts), reverse=True):
        try:
            directory.rmdir()
        except OSError:
            pass
    if preserved:
        print(f"Preserved {len(preserved)} modified or in-use addon files in {target}")
    else:
        (target / MANIFEST).unlink(missing_ok=True)
        try:
            target.rmdir()
        except OSError:
            pass
    print("Removed installer-owned addon files; unrelated files were preserved")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", type=Path, default=default_target())
    parser.add_argument("--source", type=Path, default=Path(__file__).resolve().parent / "compat")
    parser.add_argument("--uninstall", action="store_true")
    args = parser.parse_args()
    try:
        uninstall(args.target) if args.uninstall else install(args.source, args.target)
    except (ValueError, OSError) as error:
        parser.exit(1, f"Addon installation error: {error}\n")


if __name__ == "__main__":
    main()
