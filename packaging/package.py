#!/usr/bin/env python3
"""Build small desktop installers using only Python's standard library.

Native packaging tools: dpkg-deb (Linux), NSIS (Windows), hdiutil (macOS).
The same entry point is used locally and by the release workflow.
"""

import argparse
import gzip
import hashlib
import os
from pathlib import Path
import re
import shutil
import struct
import subprocess
import tarfile
import tempfile
import time
import tomllib
import zipfile


ROOT = Path(__file__).resolve().parent.parent
TARGETS = {
    "x86_64-unknown-linux-gnu": ("linux-x86_64", "linux"),
    "x86_64-pc-windows-msvc": ("windows-x86_64", "windows"),
    "x86_64-pc-windows-gnu": ("windows-x86_64", "windows"),
    "x86_64-apple-darwin": ("macos-x86_64", "macos"),
    "aarch64-apple-darwin": ("macos-aarch64", "macos"),
}
WINDOWS_SYSTEM_DLLS = set("""
advapi32 avrt bcrypt bcryptprimitives cfgmgr32 combase comctl32 comdlg32 crypt32
d3d11 d3d12 d3dcompiler_47 dcomp dbghelp dinput8 dnsapi dwmapi dwrite dxgi
gdi32 hid imm32 iphlpapi kernel32 mf mfplat mfreadwrite mmdevapi msimg32
msvcrt ncrypt normaliz ntdll ole32 oleaut32 opengl32 powrprof propsys psapi
rpcrt4 runtimeobject secur32 setupapi shell32 shcore shlwapi synchronization
ucrtbase user32 userenv usp10 uuid uxtheme version windowscodecs winhttp wininet
winmm winspool wintrust wldap32 ws2_32 wtsapi32 xinput1_4
""".split())
WINDOWS_RUNTIME_DLLS = {"libgcc_s_seh-1.dll", "libstdc++-6.dll", "libwinpthread-1.dll"}


def run(*args, **kwargs):
    subprocess.run([str(arg) for arg in args], check=True, cwd=ROOT, **kwargs)


def copy(source, destination):
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(source, destination)
    destination.chmod(0o755 if source.stat().st_mode & 0o111 else 0o644)


def digest(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def tar_archive(directory, destination, epoch):
    with destination.open("wb") as output:
        with gzip.GzipFile(filename="", fileobj=output, mode="wb", mtime=epoch) as gz:
            with tarfile.open(fileobj=gz, mode="w") as archive:
                for path in [directory, *sorted(directory.rglob("*"))]:
                    info = archive.gettarinfo(path, str(path.relative_to(directory.parent)))
                    info.uid = info.gid = 0
                    info.uname = info.gname = "root"
                    info.mtime = epoch
                    if info.isfile():
                        with path.open("rb") as content:
                            archive.addfile(info, content)
                    else:
                        archive.addfile(info)


def zip_archive(directory, destination, epoch):
    stamp = time.gmtime(max(epoch, 315532800))[:6]
    with zipfile.ZipFile(destination, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as archive:
        for path in sorted(directory.rglob("*")):
            if path.is_file():
                info = zipfile.ZipInfo(str(path.relative_to(directory.parent)).replace("\\", "/"), stamp)
                info.compress_type = zipfile.ZIP_DEFLATED
                info.external_attr = (path.stat().st_mode & 0xFFFF) << 16
                archive.writestr(info, path.read_bytes())


def glibc_requirement(binary):
    # Detect the actual linked baseline, including local builds on newer Linux.
    versions = re.findall(rb"GLIBC_([0-9]+)\.([0-9]+)", binary.read_bytes())
    if not versions:
        raise RuntimeError("Could not determine the Linux binary's glibc requirement")
    return ".".join(map(str, max((int(major), int(minor)) for major, minor in versions)))


def pe_imports(binary):
    """Read PE imports without depending on Visual Studio or binutils tools."""
    data = binary.read_bytes()
    if data[:2] != b"MZ":
        raise RuntimeError(f"Not a Windows executable: {binary}")
    pe = struct.unpack_from("<I", data, 0x3C)[0]
    if data[pe:pe + 4] != b"PE\0\0":
        raise RuntimeError(f"Invalid Windows PE header: {binary}")
    sections = struct.unpack_from("<H", data, pe + 6)[0]
    optional_size = struct.unpack_from("<H", data, pe + 20)[0]
    optional = pe + 24
    magic = struct.unpack_from("<H", data, optional)[0]
    if magic not in (0x10B, 0x20B):
        raise RuntimeError(f"Unsupported PE format: {binary}")
    directory = optional + (112 if magic == 0x20B else 96)
    imports_rva = struct.unpack_from("<I", data, directory + 8)[0]
    if not imports_rva:
        return []

    def file_offset(rva):
        for index in range(sections):
            section = optional + optional_size + index * 40
            virtual_size, address, raw_size, raw_offset = struct.unpack_from("<IIII", data, section + 8)
            if address <= rva < address + max(virtual_size, raw_size):
                return raw_offset + rva - address
        raise RuntimeError(f"Invalid PE address in {binary}")

    offset = file_offset(imports_rva)
    names = []
    while True:
        descriptor = struct.unpack_from("<IIIII", data, offset)
        if not any(descriptor):
            return names
        name_offset = file_offset(descriptor[3])
        names.append(data[name_offset:data.index(b"\0", name_offset)].decode("ascii").lower())
        offset += 20


def bundle_windows_runtime(binaries, package, runtime_dirs):
    pending = list(binaries)
    seen = set()
    while pending:
        binary = pending.pop()
        for name in pe_imports(binary):
            if name in seen:
                continue
            seen.add(name)
            if name.startswith(("api-ms-win-", "ext-ms-win-")) or name.removesuffix(".dll") in WINDOWS_SYSTEM_DLLS:
                continue
            if name not in WINDOWS_RUNTIME_DLLS:
                raise RuntimeError(f"Unbundled Windows dependency {name} in {binary.name}")
            candidates = [binary.parent / name, *(directory / name for directory in runtime_dirs)]
            runtime = next((path for path in candidates if path.is_file()), None)
            if runtime is None:
                raise RuntimeError(f"Missing {name}; provide its folder with --runtime-dir")
            copy(runtime, package / name)
            pending.append(runtime)


def package_linux(package, output, binary, version, epoch):
    copy(binary, package / "mantis-app")
    for name in ("install.sh", "uninstall.sh"):
        copy(ROOT / "packaging/linux" / name, package / name)
        (package / name).chmod(0o755)
    copy(ROOT / "packaging/mantis-cad.svg", package / "mantis-cad.svg")
    tarball = output / f"{package.name}.tar.gz"
    tar_archive(package, tarball, epoch)

    deb = package.parent / "deb"
    copy(binary, deb / "usr/bin/mantis-cad")
    copy(ROOT / "packaging/linux/mantis-cad.desktop", deb / "usr/share/applications/mantis-cad.desktop")
    copy(ROOT / "packaging/mantis-cad.svg", deb / "usr/share/icons/hicolor/scalable/apps/mantis-cad.svg")
    copy(ROOT / "LICENSE", deb / "usr/share/doc/mantis-cad/copyright")
    copy(ROOT / "docs/INSTALL.md", deb / "usr/share/doc/mantis-cad/INSTALL.md")
    copy(ROOT / "docs/COMMANDS.md", deb / "usr/share/doc/mantis-cad/COMMANDS.md")
    copy(ROOT / "docs/INTEROP.md", deb / "usr/share/doc/mantis-cad/INTEROP.md")
    copy(ROOT / "crates/mantis-kernel/THIRD_PARTY_LICENSES.md", deb / "usr/share/doc/mantis-cad/THIRD_PARTY_LICENSES.md")
    installed_kib = (sum(path.stat().st_size for path in deb.rglob("*") if path.is_file()) + 1023) // 1024
    depends = (
        f"libc6 (>= {glibc_requirement(binary)}), libgcc-s1, libx11-6, libxcursor1, "
        "libxi6, libxrandr2, libxinerama1, libxkbcommon0, libgl1, libegl1, "
        "libwayland-client0, libwayland-cursor0, libwayland-egl1"
    )
    (deb / "DEBIAN").mkdir()
    (deb / "DEBIAN/control").write_text(
        f"Package: mantis-cad\nVersion: {version}\nArchitecture: amd64\n"
        "Maintainer: MantisCAD contributors <noreply@github.com>\n"
        "Section: graphics\nPriority: optional\n"
        f"Installed-Size: {installed_kib}\nDepends: {depends}\n"
        "Homepage: https://github.com/tomeido/mantis-cad\n"
        "Description: Lightweight native parametric 3D CAD\n"
        " Curve and surface modeling with a visual parametric graph.\n",
        encoding="utf-8",
    )
    debfile = output / f"mantis-cad_{version}_amd64.deb"
    run("dpkg-deb", "--root-owner-group", "-Zxz", "-z9", "--build", deb, debfile,
        env={**os.environ, "SOURCE_DATE_EPOCH": str(epoch)})
    return [tarball, debfile]


def package_windows(package, output, binary, version, epoch, args):
    copy(binary, package / "MantisCAD.exe")
    # Include only required MinGW runtime DLLs, including transitive imports.
    # MSVC builds use a static CRT and therefore normally need none of them.
    bundle_windows_runtime([binary], package, args.runtime_dir)
    zipfile_path = output / f"{package.name}.zip"
    zip_archive(package, zipfile_path, epoch)
    installer = output / f"{package.name}-setup.exe"
    makensis = args.makensis or shutil.which("makensis") or shutil.which("makensis.exe")
    if not makensis and os.name == "nt":
        candidate = Path(os.environ.get("ProgramFiles(x86)", r"C:\Program Files (x86)")) / "NSIS/makensis.exe"
        if candidate.is_file():
            makensis = str(candidate)
    if not makensis:
        raise RuntimeError("NSIS is required; install it and pass --makensis if it is not on PATH")
    flag = "/" if os.name == "nt" else "-"
    run(makensis, f"{flag}V2", f"{flag}DVERSION={version}", f"{flag}DPACKAGE_DIR={package}",
        f"{flag}DOUTPUT={installer}", ROOT / "packaging/windows/installer.nsi")
    return [zipfile_path, installer]


def package_macos(package, output, binary, version):
    contents = package / "MantisCAD.app/Contents"
    copy(binary, contents / "MacOS/MantisCAD")
    plist = (ROOT / "packaging/macos/Info.plist").read_text(encoding="utf-8")
    (contents / "Info.plist").write_text(plist.replace("@VERSION@", version), encoding="utf-8")
    zipfile_path = output / f"{package.name}.zip"
    run("ditto", "-c", "-k", "--sequesterRsrc", "--keepParent", package, zipfile_path)
    (package / "Applications").symlink_to("/Applications", target_is_directory=True)
    dmg = output / f"{package.name}.dmg"
    run("hdiutil", "create", "-volname", "MantisCAD", "-srcfolder", package,
        "-ov", "-format", "UDZO", dmg)
    return [zipfile_path, dmg]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", choices=TARGETS, required=True)
    parser.add_argument("--output", type=Path, default=ROOT / "dist-downloads")
    parser.add_argument("--skip-build", action="store_true", help="Package already-built binaries")
    parser.add_argument("--binary-dir", type=Path, help="Override target/TARGET/release directory")
    parser.add_argument("--with-tools", action="store_true", help="Also create a separate optional server/CLI archive")
    parser.add_argument("--makensis", help="Path to the NSIS compiler")
    parser.add_argument("--runtime-dir", action="append", type=Path, default=[], help="Optional MinGW runtime DLL directory")
    args = parser.parse_args()
    version = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))["workspace"]["package"]["version"]
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version):
        parser.error("Installer versions must use three numeric components (major.minor.patch)")
    artifact, system = TARGETS[args.target]
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    target_dir = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target"))
    if not target_dir.is_absolute():
        target_dir = ROOT / target_dir
    binary_dir = (args.binary_dir or target_dir / args.target / "release").resolve()
    env = os.environ.copy()
    if system == "windows" and args.target.endswith("msvc"):
        env["RUSTFLAGS"] = env.get("RUSTFLAGS", "") + " -C target-feature=+crt-static"
    if system == "macos":
        env.setdefault("MACOSX_DEPLOYMENT_TARGET", "13.0")
    if not args.skip_build:
        command = ["cargo", "build", "--locked", "--release", "--target", args.target, "-p", "mantis-app"]
        if args.with_tools:
            for name in ("mantis-server", "mantis-admin", "mantis-cli"):
                command += ["-p", name]
        run(*command, env=env)
    suffix = ".exe" if system == "windows" else ""
    binary = binary_dir / f"mantis-app{suffix}"
    if not binary.is_file():
        parser.error(f"The built app does not exist: {binary}")
    epoch = int(os.environ.get("SOURCE_DATE_EPOCH") or subprocess.check_output(
        ["git", "show", "-s", "--format=%ct", "HEAD"], cwd=ROOT, text=True).strip())
    with tempfile.TemporaryDirectory(prefix="mantis-package-") as temp:
        package = Path(temp) / f"mantis-cad-v{version}-{artifact}"
        copy(ROOT / "LICENSE", package / "LICENSE")
        copy(ROOT / "docs/INSTALL.md", package / "INSTALL.md")
        copy(ROOT / "docs/COMMANDS.md", package / "COMMANDS.md")
        copy(ROOT / "docs/INTEROP.md", package / "INTEROP.md")
        copy(ROOT / "crates/mantis-kernel/THIRD_PARTY_LICENSES.md", package / "THIRD_PARTY_LICENSES.md")
        if system == "linux":
            products = package_linux(package, output, binary, version, epoch)
        elif system == "windows":
            products = package_windows(package, output, binary, version, epoch, args)
        else:
            products = package_macos(package, output, binary, version)
        if args.with_tools:
            tool_package = Path(temp) / f"mantis-cad-v{version}-{artifact}-tools"
            for name in ("mantis-server", "mantis-admin", "mantis-cli"):
                copy(binary_dir / f"{name}{suffix}", tool_package / f"{name}{suffix}")
            copy(ROOT / "LICENSE", tool_package / "LICENSE")
            copy(ROOT / "docs/DEPLOYMENT.md", tool_package / "DEPLOYMENT.md")
            if system == "windows":
                bundle_windows_runtime(list(tool_package.glob("*.exe")), tool_package, args.runtime_dir)
                archive = output / f"{tool_package.name}.zip"
                zip_archive(tool_package, archive, epoch)
            else:
                archive = output / f"{tool_package.name}.tar.gz"
                tar_archive(tool_package, archive, epoch)
            products.append(archive)
    # Keep platform invocations composable into one checksummed download folder.
    packages = sorted(path for path in output.glob("mantis-cad*") if path.is_file())
    checksum_path = output / "SHA256SUMS"
    checksum_path.write_text("".join(
        f"{digest(path)}  {path.name}\n"
        for path in packages), encoding="utf-8")
    for product in products:
        print(f"{product.name}: {product.stat().st_size / (1024 * 1024):.2f} MiB")
    print(f"SHA-256 checksums: {checksum_path}")


if __name__ == "__main__":
    main()
