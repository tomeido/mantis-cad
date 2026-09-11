#!/usr/bin/env python3
"""Check packaged Windows runtimes on Windows (CI), and archive CRC everywhere."""

import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile
import zipfile

ROOT = Path(__file__).resolve().parent


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    args = parser.parse_args()
    packages = sorted(args.directory.glob("mantis-cad-*-compat-*-windows-x64.zip"))
    if not packages:
        parser.error("No Windows compatibility addon ZIPs found")
    for package in packages:
        with tempfile.TemporaryDirectory(prefix="mantis-addon-smoke-") as temporary:
            target = Path(temporary)
            with zipfile.ZipFile(package) as archive:
                assert archive.testzip() is None
                archive.extractall(target)
            if os.name == "nt":
                python = target / "compat/python/python.exe"
                helper = target / "compat/compat.py"
                result = subprocess.run([str(python), "-I", str(helper), "capabilities"], input=b"{}", capture_output=True, timeout=60, check=True)
                capabilities = json.loads(result.stdout)
                assert capabilities["rhino3dm"]
                if "-full-" in package.name:
                    assert capabilities["ocp"]
                    subprocess.run([str(python), "-I", str(ROOT / "test_compat.py")], check=True, timeout=180)
                    subprocess.run([str(python), "-I", str(ROOT / "test_install.py")], check=True, timeout=60)
            print(f"Verified {package.name}" + (" including Windows runtime" if os.name == "nt" else " (ZIP CRC; runtime requires Windows)"))


if __name__ == "__main__":
    main()
