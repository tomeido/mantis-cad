#!/usr/bin/env python3
"""Reproduce small optional Windows addons from SHA256-pinned upstream files."""

import argparse
import gzip
import hashlib
import json
from pathlib import Path, PurePosixPath
import shutil
import struct
import sys
import tarfile
import tempfile
import tomllib
import urllib.request
import zipfile

ROOT = Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT.parent / "packaging"))
from package import WINDOWS_SYSTEM_DLLS, pe_imports

SOURCES = ("compat.py", "common.py", "rhino_bridge.py", "ocp_bridge.py", "README.md")


def sha256(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def fetch(cache, name, data):
    path = cache / name
    if not path.is_file() or sha256(path) != data["sha256"]:
        cache.mkdir(parents=True, exist_ok=True)
        temporary = path.with_suffix(path.suffix + ".download")
        with urllib.request.urlopen(data["url"], timeout=60) as source, temporary.open("wb") as target:
            shutil.copyfileobj(source, target)
        if sha256(temporary) != data["sha256"]:
            temporary.unlink()
            raise RuntimeError(f"SHA256 mismatch: {name}")
        temporary.replace(path)
    return path


def extract(archive, target):
    target.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(archive) as package:
        if package.testzip() is not None:
            raise RuntimeError(f"Archive CRC failure: {archive}")
        for item in package.infolist():
            path = PurePosixPath(item.filename)
            if path.is_absolute() or ".." in path.parts or "\\" in item.filename or any(":" in part for part in path.parts):
                raise RuntimeError("Unsafe upstream archive member")
            package.extract(item, target)


def pe_closure(payload):
    binaries = [path for path in payload.rglob("*") if path.suffix.lower() in (".exe", ".dll", ".pyd")]
    available = {path.name.lower() for path in binaries}
    # These ship with Windows 10/11, unlike optional Visual C++ runtimes.
    system = WINDOWS_SYSTEM_DLLS | {"netapi32", "powrprof", "profapi", "cabinet", "imagehlp", "wsock32", "glu32"}
    missing = {}
    for path in binaries:
        data = path.read_bytes()
        header = struct.unpack_from("<I", data, 0x3C)[0]
        if struct.unpack_from("<H", data, header + 4)[0] != 0x8664:
            raise RuntimeError(f"Non-x64 binary in Windows addon: {path}")
        absent = [name for name in pe_imports(path) if name not in available
                  and not name.startswith(("api-ms-win-", "ext-ms-win-"))
                  and name.removesuffix(".dll") not in system]
        if absent:
            missing[str(path.relative_to(payload))] = absent
    if missing:
        raise RuntimeError(f"Unbundled Windows dependencies: {json.dumps(missing, indent=2)}")
    return len(binaries)


def zip_payload(source, path):
    with tempfile.NamedTemporaryFile(prefix=".mantis-addon-", suffix=".zip", dir=path.parent, delete=False) as temporary:
        staged = Path(temporary.name)
    try:
        with zipfile.ZipFile(staged, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as archive:
            for item in sorted(source.rglob("*")):
                if item.is_file():
                    info = zipfile.ZipInfo(item.relative_to(source).as_posix(), date_time=(2026, 1, 1, 0, 0, 0))
                    info.compress_type = zipfile.ZIP_DEFLATED
                    info.external_attr = 0o644 << 16
                    archive.writestr(info, item.read_bytes(), compresslevel=9)
        with zipfile.ZipFile(staged) as archive:
            if archive.testzip() is not None:
                raise RuntimeError("Built addon failed ZIP CRC check")
        staged.replace(path)
    finally:
        staged.unlink(missing_ok=True)


def windows_addon(variant, inputs, output, version):
    with tempfile.TemporaryDirectory(prefix="mantis-addon-build-") as temporary:
        staging = Path(temporary)
        payload = staging / "compat"
        payload.mkdir()
        for filename in SOURCES:
            shutil.copy2(ROOT / filename, payload / filename)
        shutil.copy2(ROOT.parent / "LICENSE", payload / "LICENSE")
        shutil.copytree(ROOT / "licenses", payload / "licenses")
        for name, path in inputs.items():
            if name.startswith("python-"):
                extract(path, payload / "python")
            elif variant == "full" or name.startswith("rhino3dm-"):
                extract(path, payload / "site-packages")
        # rhino3dm needs MSVCP140, which CPython's embedded package does not
        # include. Reuse the unmodified redistributable bundled by the pinned
        # OCP wheel, under its standard loader filename for rhino3dm as well.
        ocp_wheel = next(path for name, path in inputs.items() if name.startswith("cadquery_ocp_novtk-"))
        with zipfile.ZipFile(ocp_wheel) as archive:
            runtime = next(name for name in archive.namelist() if PurePosixPath(name).name.startswith("msvcp140-") and name.endswith(".dll"))
            (payload / "python/msvcp140.dll").write_bytes(archive.read(runtime))
        (payload / "python/python314._pth").write_text("python314.zip\n.\n..\n../site-packages\nimport site\n", encoding="utf-8")
        shutil.copy2(ROOT / "windows-artifacts.lock.json", payload / "windows-artifacts.lock.json")
        shutil.copy2(ROOT / "install-addon.py", staging / "install-addon.py")
        for command in ("install", "uninstall"):
            flags = " --uninstall" if command == "uninstall" else ""
            script = ('@echo off\r\n"%~dp0compat\\python\\python.exe" -I "%~dp0install-addon.py"'
                      + flags + ' %*\r\nif errorlevel 1 (\r\n  echo Addon operation failed.\r\n  pause\r\n  exit /b 1\r\n)\r\npause\r\n')
            (staging / f"{command}-addon.cmd").write_bytes(script.encode("ascii"))
        count = pe_closure(payload)
        artifact = output / f"mantis-cad-{version}-compat-{variant}-windows-x64.zip"
        zip_payload(staging, artifact)
        print(json.dumps({"file": str(artifact), "bytes": artifact.stat().st_size,
                          "installed_bytes": sum(p.stat().st_size for p in payload.rglob("*") if p.is_file()),
                          "pe_binaries_checked": count, "sha256": sha256(artifact)}))


def source_addon(output, version):
    artifact = output / f"mantis-cad-{version}-compat-linux-macos-setup.tar.gz"
    with artifact.open("wb") as output_stream, gzip.GzipFile(fileobj=output_stream, mode="wb", filename="", mtime=1767225600, compresslevel=9) as compressed, tarfile.open(fileobj=compressed, mode="w") as archive:
        paths = [ROOT / name for name in (*SOURCES, "setup.sh", "setup.py", "install-addon.py", "requirements-3dm.txt", "requirements-full.txt")]
        paths += sorted(path for path in (ROOT / "licenses").rglob("*") if path.is_file())
        for path in paths:
            name = f"mantis-cad-{version}-compat/" + path.relative_to(ROOT).as_posix()
            info = archive.gettarinfo(str(path), arcname=name)
            info.uid = info.gid = 0
            info.uname = info.gname = ""
            info.mtime = 1767225600
            info.mode = 0o755 if path.suffix == ".sh" else 0o644
            with path.open("rb") as stream:
                archive.addfile(info, stream)
    print(json.dumps({"file": str(artifact), "bytes": artifact.stat().st_size, "sha256": sha256(artifact)}))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cache", type=Path, default=ROOT / "build/cache")
    parser.add_argument("--output", type=Path, default=ROOT.parent / "dist-downloads")
    parser.add_argument("--variant", choices=("3dm", "full", "both"), default="both")
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    version = tomllib.loads((ROOT.parent / "Cargo.toml").read_text())["workspace"]["package"]["version"]
    lock = json.loads((ROOT / "windows-artifacts.lock.json").read_text())
    inputs = {name: fetch(args.cache, name, data) for name, data in lock["artifacts"].items()}
    for variant in ("3dm", "full") if args.variant == "both" else (args.variant,):
        windows_addon(variant, inputs, args.output, version)
    source_addon(args.output, version)


if __name__ == "__main__":
    main()
