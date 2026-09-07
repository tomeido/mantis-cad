#!/usr/bin/env python3
"""Check real Linux packages and exercise install, upgrade, and safe removal."""

import hashlib
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time
import tomllib


def digest(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def run(*args, success=True):
    result = subprocess.run([str(arg) for arg in args], capture_output=True, text=True)
    if success:
        if result.returncode:
            raise RuntimeError(result.stdout + result.stderr)
    elif result.returncode == 0:
        raise AssertionError("Unsafe installation unexpectedly succeeded")
    return result


def verify_desktop_launch(prefix):
    if not shutil.which("gio"):
        return
    executable = prefix / "lib/mantis-cad/mantis-app"
    # Launch a tiny probe through the real desktop parser; no GUI/display needed.
    # The subsequent upgrade restores and verifies the real application binary.
    executable.write_text('#!/bin/sh\nprintf ok > "$0.desktop-smoke"\n')
    run("gio", "launch", prefix / "share/applications/mantis-cad.desktop")
    marker = executable.with_name("mantis-app.desktop-smoke")
    deadline = time.monotonic() + 5
    while not marker.exists() and time.monotonic() < deadline:
        time.sleep(0.01)
    assert marker.read_text() == "ok", "Desktop launcher did not execute the installed path"
    marker.unlink()


def main():
    output = Path(sys.argv[1] if len(sys.argv) > 1 else "dist-downloads").resolve()
    for line in (output / "SHA256SUMS").read_text().splitlines():
        expected, name = line.split("  ", 1)
        package = output / name
        if package.is_file():
            assert digest(package) == expected, f"Checksum mismatch: {name}"
    manifest = Path(__file__).resolve().parent.parent / "Cargo.toml"
    version = tomllib.loads(manifest.read_text())["workspace"]["package"]["version"]
    archive = output / f"mantis-cad-v{version}-linux-x86_64.tar.gz"
    deb = output / f"mantis-cad_{version}_amd64.deb"
    with tempfile.TemporaryDirectory(prefix="mantis-install-smoke-") as temporary:
        root = Path(temporary)
        with tarfile.open(archive) as package:
            package.extractall(root / "extracted", filter="data")
        source = next((root / "extracted").iterdir())
        prefix = root / r'user folder with $ and % " and \ chars'
        install = source / "install.sh"
        run("sh", install, "--prefix", prefix)
        app = prefix / "lib/mantis-cad"
        launcher = prefix / "bin/mantis-cad"
        assert launcher.is_symlink()
        assert digest(launcher) == digest(source / "mantis-app")
        desktop = prefix / "share/applications/mantis-cad.desktop"
        entry = desktop.read_text()
        assert '\\\\$' in entry and '%%' in entry, "Desktop Exec escaping failed"
        (app / "user-project.json").write_text('{"keep":true}')
        (prefix / "unrelated-file").write_text("keep")
        verify_desktop_launch(prefix)
        run("sh", install, "--prefix", prefix)  # In-place upgrade.
        assert digest(launcher) == digest(source / "mantis-app")
        assert (app / "user-project.json").read_text() == '{"keep":true}'
        (prefix / "share/icons/hicolor/scalable/apps/mantis-cad.svg").unlink()
        run("sh", app / "uninstall.sh")
        assert not launcher.exists() and not launcher.is_symlink()
        assert not desktop.exists()
        assert not (app / "mantis-app").exists()
        assert (app / "user-project.json").exists()
        assert (prefix / "unrelated-file").exists()
        # An unrecognized directory may contain user data and must be preserved.
        run("sh", install, "--prefix", prefix, success=False)
        assert (app / "user-project.json").exists()
        protected = root / "protected"
        (protected / "bin").mkdir(parents=True)
        (protected / "bin/mantis-cad").write_text("existing command")
        run("sh", install, "--prefix", protected, success=False)
        assert (protected / "bin/mantis-cad").read_text() == "existing command"
        extracted_deb = root / "deb"
        run("dpkg-deb", "--extract", deb, extracted_deb)
        assert digest(extracted_deb / "usr/bin/mantis-cad") == digest(source / "mantis-app")
        metadata = run("dpkg-deb", "--field", deb).stdout
        assert "Package: mantis-cad" in metadata and "libc6 (>= " in metadata
    print("PASS: checksums, TAR/DEB payloads, user install, upgrade, safe uninstall, collision protection")


if __name__ == "__main__":
    main()
