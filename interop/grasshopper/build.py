#!/usr/bin/env python3
"""Build an optional standalone .gh archive converter using the official SDK.

Only GH_IO.dll is referenced; RhinoCommon, Grasshopper.dll and Rhino itself are
never installed or loaded. .NET is bundled in the resulting optional add-on.
"""
import argparse
import hashlib
import os
from pathlib import Path
import shutil
import subprocess
import urllib.request
import zipfile

GH_VERSION = "8.34.26223.11001"
URL = f"https://api.nuget.org/v3-flatcontainer/grasshopper/{GH_VERSION}/grasshopper.{GH_VERSION}.nupkg"
PACKAGE_SHA256 = "4697993bbee0f9570f783684cf03813c28acd5aae094fe1b2c0fc76d8fac8a5e"
ROOT = Path(__file__).resolve().parent


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dotnet", default="dotnet")
    parser.add_argument("--rid", choices=["linux-x64", "win-x64"], required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--cache", type=Path, default=Path.home() / ".cache/mantis-gh-build")
    parser.add_argument("--linux-libs", type=Path, help="Optional extracted Debian/Ubuntu GDI packages root (usr/lib/x86_64-linux-gnu and usr/share/doc)")
    args = parser.parse_args()
    args.cache.mkdir(parents=True, exist_ok=True)
    package = args.cache / f"grasshopper.{GH_VERSION}.nupkg"
    if not package.exists():
        print(f"Downloading official McNeel GH_IO SDK {GH_VERSION}")
        with urllib.request.urlopen(URL, timeout=90) as response:
            package.write_bytes(response.read())
    if hashlib.sha256(package.read_bytes()).hexdigest() != PACKAGE_SHA256:
        raise RuntimeError("Official Grasshopper package hash does not match the pinned release")
    with zipfile.ZipFile(package) as archive:
        dll = args.cache / "GH_IO.dll"
        dll.write_bytes(archive.read("lib/net7.0/GH_IO.dll"))
        metadata = archive.read("Grasshopper.nuspec")
    args.output.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ, DOTNET_CLI_TELEMETRY_OPTOUT="1", DOTNET_NOLOGO="1")
    subprocess.run([
        args.dotnet, "publish", str(ROOT / "MantisGhIo.csproj"), "-c", "Release",
        "-r", args.rid, "--self-contained", "true", "-o", str(args.output.resolve()),
        f"-p:GhIoPath={dll.resolve()}", "-p:DebugType=None", "-p:DebugSymbols=false",
    ], check=True, env=env)
    for path in args.output.glob("*.xml"):
        path.unlink()
    (args.output / "McNeel-Grasshopper-package-metadata.xml").write_bytes(metadata)
    shutil.copyfile(ROOT / "NOTICE.md", args.output / "NOTICE.md")
    shutil.copyfile(ROOT / "README.md", args.output / "README.md")
    shutil.copytree(ROOT / "licenses", args.output / "licenses", dirs_exist_ok=True)
    if args.rid == "linux-x64":
        if args.linux_libs:
            library_dir = args.output / "lib"
            library_dir.mkdir(exist_ok=True)
            for library in (args.linux_libs / "usr/lib/x86_64-linux-gnu").glob("*.so*"):
                destination = library_dir / library.name
                if destination.exists() or destination.is_symlink():
                    destination.unlink()
                if library.is_symlink():
                    destination.symlink_to(os.readlink(library))
                else:
                    shutil.copy2(library, destination)
            notices = args.output / "licenses/linux"
            notices.mkdir(exist_ok=True, parents=True)
            for copyright in (args.linux_libs / "usr/share/doc").glob("*/copyright"):
                shutil.copyfile(copyright, notices / f"{copyright.parent.name}.txt")
            common = Path("/usr/share/common-licenses")
            if common.is_dir():
                shutil.copytree(common, notices / "common-licenses", symlinks=True, dirs_exist_ok=True)
        # Use a launcher so adjacent optional GDI dependencies can be supplied
        # without changing the user's global library path.
        binary = args.output / "mantis-gh-io"
        binary.rename(args.output / "mantis-gh-io-bin")
        binary.write_text('#!/bin/sh\nset -eu\nbase=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)\nexport LD_LIBRARY_PATH="$base/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"\nexec "$base/mantis-gh-io-bin" "$@"\n')
        binary.chmod(0o755)
    print(f"Built {args.output} ({sum(p.stat().st_size for p in args.output.rglob('*') if p.is_file()) / 1024**2:.1f} MiB)")


if __name__ == "__main__":
    main()
