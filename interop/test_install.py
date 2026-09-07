"""Install/update/uninstall ownership checks using temporary user directories."""

import importlib.util
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("installer", Path(__file__).with_name("install-addon.py"))
installer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(installer)


class InstallTests(unittest.TestCase):
    def test_upgrade_uninstall_preserves_unknown_files_and_gh(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source, target = root / "source", root / "target"
            source.mkdir()
            (source / "compat.py").write_text("version1")
            (target / "grasshopper").mkdir(parents=True)
            gh = target / "grasshopper/.mantis-gh-pack"
            gh.write_text("MantisCAD Grasshopper archive pack 0.2.0\n")
            installer.install(source, target)
            (target / "project.3dm").write_bytes(b"preserve user data")
            (source / "compat.py").write_text("version2")
            installer.install(source, target)
            self.assertEqual((target / "compat.py").read_text(), "version2")
            installer.uninstall(target)
            self.assertFalse((target / "compat.py").exists())
            self.assertEqual((target / "project.3dm").read_bytes(), b"preserve user data")
            self.assertTrue(gh.exists())

    def test_unowned_directory_and_symlinks_are_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source, target = root / "source", root / "target"
            source.mkdir()
            (source / "compat.py").write_text("app")
            target.mkdir()
            sentinel = target / "precious.txt"
            sentinel.write_text("preserve")
            with self.assertRaises(ValueError):
                installer.install(source, target)
            link = root / "link"
            if os.name == "nt":
                # NTFS junctions do not require Windows Developer Mode and
                # exercise reparse-point handling separately from symlinks.
                subprocess.run(["cmd", "/c", "mklink", "/J", str(link), str(target)], capture_output=True, check=True)
            else:
                link.symlink_to(target, target_is_directory=True)
            with self.assertRaises(ValueError):
                installer.install(source, link)
            self.assertEqual(sentinel.read_text(), "preserve")

    def test_modified_owned_file_is_not_deleted(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source, target = root / "source", root / "target"
            source.mkdir()
            (source / "compat.py").write_text("original")
            installer.install(source, target)
            (target / "compat.py").write_text("user modification")
            installer.uninstall(target)
            self.assertEqual((target / "compat.py").read_text(), "user modification")


if __name__ == "__main__":
    unittest.main()
