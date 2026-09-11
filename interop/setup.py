#!/usr/bin/env python3
"""Build an isolated optional Linux/macOS compatibility environment per user."""

import argparse
import importlib.util
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import venv


def main():
    root = Path(__file__).resolve().parent
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--backend", choices=("3dm", "full"), default="full")
    parser.add_argument("--target", type=Path, default=Path.home() / ".local/share/mantis-cad/compat")
    parser.add_argument("--uninstall", action="store_true")
    args = parser.parse_args()
    spec = importlib.util.spec_from_file_location("addon_installer", root / "install-addon.py")
    installer = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(installer)
    if args.uninstall:
        installer.uninstall(args.target)
        return
    if sys.version_info < (3, 10):
        parser.error("Python 3.10 or newer is required")
    with tempfile.TemporaryDirectory(prefix="mantis-compat-setup-") as staging:
        payload = Path(staging) / "compat"
        payload.mkdir()
        for filename in ("compat.py", "common.py", "rhino_bridge.py", "ocp_bridge.py", "README.md"):
            shutil.copy2(root / filename, payload / filename)
        shutil.copytree(root / "licenses", payload / "licenses")
        # --copies avoids following symlinks during the manifest install.
        use_external_pip = importlib.util.find_spec("ensurepip") is None and importlib.util.find_spec("pip") is not None
        venv.EnvBuilder(with_pip=not use_external_pip, symlinks=False).create(payload / ".venv")
        python = payload / ".venv/bin/python"
        pip = [sys.executable, "-m", "pip", "--python", str(python)] if use_external_pip else [str(python), "-m", "pip"]
        subprocess.run([*pip, "install", "--only-binary=:all:",
                        "--no-compile", "-r", str(root / f"requirements-{args.backend}.txt")], check=True)
        lib64 = payload / ".venv/lib64"
        if lib64.is_symlink():
            lib64.unlink()
        # venv scripts embed the staging path. Only python -I is used by the app;
        # remove pip/activation launchers, then make pyvenv.cfg independent of it.
        for script in (payload / ".venv/bin").iterdir():
            if not script.name.startswith("python"):
                script.unlink()
        config = payload / ".venv/pyvenv.cfg"
        lines = [line for line in config.read_text().splitlines() if not line.startswith(("command =", "executable ="))]
        config.write_text("\n".join(lines) + "\n")
        subprocess.run([str(python), "-I", str(payload / "compat.py"), "capabilities"], input=b"{}", check=True)
        installer.install(payload, args.target)


if __name__ == "__main__":
    main()
