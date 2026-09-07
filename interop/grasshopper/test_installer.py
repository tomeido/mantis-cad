#!/usr/bin/env python3
"""Exercise the Linux add-on installer in an isolated temporary directory."""
import importlib.util
import os
from pathlib import Path
import subprocess
import tempfile


def main():
    spec = importlib.util.spec_from_file_location("gh_package", Path(__file__).with_name("package.py"))
    package = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(package)
    with tempfile.TemporaryDirectory(prefix="mantis-gh-installer-test-") as temp:
        root = Path(temp)
        payload = root / "package/compat/grasshopper"
        payload.mkdir(parents=True)
        (payload / ".mantis-gh-pack").write_text("MantisCAD Grasshopper archive pack 0.2.0\n")
        (payload / "test-runtime").write_text("runtime")
        (payload / "uninstall.sh").write_text(package.UNINSTALL_SH)
        package.write_manifest(payload)
        installer = root / "package/install.sh"
        installer.write_text(package.INSTALL_SH)
        target = root / "target"
        env = dict(os.environ, MANTIS_GH_INSTALL_DIR=str(target))

        def install(success=True):
            result = subprocess.run(["sh", str(installer)], env=env, capture_output=True, text=True)
            assert (result.returncode == 0) == success, result.stderr

        install()
        (target / "personal.gh").write_text("preserve user work")
        install()
        assert (target / "personal.gh").read_text() == "preserve user work"
        subprocess.run(["sh", str(target / "uninstall.sh")], check=True, capture_output=True)
        assert (target / "personal.gh").read_text() == "preserve user work"
        assert not (target / "test-runtime").exists()
        install(False)  # Remaining user folder is no longer package-owned.
        outside = root / "outside"
        outside.mkdir()
        target.rename(root / "old")
        target.symlink_to(outside, target_is_directory=True)
        install(False)
        assert list(outside.iterdir()) == []
    print("PASS: install, upgrade preserves user files, owned-only uninstall, unowned/symlink collision refusal")


if __name__ == "__main__":
    main()
