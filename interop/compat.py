#!/usr/bin/env python3
"""Optional MantisCAD bridge: one command, one bounded JSON request/response."""

import contextlib
import importlib.metadata
import json
import os
from pathlib import Path
import sys

sys.dont_write_bytecode = True

# The embedded Windows runtime is isolated; its wheels live next to this file.
_ROOT = Path(__file__).resolve().parent
sys.path.insert(0, str(_ROOT))
if (_ROOT / "site-packages").is_dir():
    sys.path.insert(0, str(_ROOT / "site-packages"))
_DLL_HANDLES = []
if os.name == "nt" and hasattr(os, "add_dll_directory"):
    for _directory in (_ROOT / "python", _ROOT / "site-packages"):
        if _directory.is_dir():
            _DLL_HANDLES.append(os.add_dll_directory(str(_directory)))
    for _directory in (_ROOT / "site-packages").glob("*.libs"):
        _DLL_HANDLES.append(os.add_dll_directory(str(_directory)))

from common import BridgeError, MAX_BYTES, canonical, strict_json


@contextlib.contextmanager
def native_stdout_to_stderr():
    """OCCT writers sometimes print through C stdout; reserve stdout for JSON."""
    sys.stdout.flush()
    saved = os.dup(1)
    try:
        os.dup2(2, 1)
        yield
    finally:
        try:
            import ctypes
            ctypes.CDLL("ucrtbase" if os.name == "nt" else None).fflush(None)
        except (OSError, AttributeError):
            pass
        sys.stdout.flush()
        os.dup2(saved, 1)
        os.close(saved)


def dispatch(command, request):
    if command == "capabilities":
        capabilities = {"protocol": 1, "rhino3dm": False, "ocp": False, "commands": []}
        try:
            import rhino3dm
            capabilities["rhino3dm"] = importlib.metadata.version("rhino3dm")
            capabilities["commands"] += ["import_3dm", "export_3dm"]
        except ImportError:
            pass
        try:
            import OCP
            capabilities["ocp"] = OCP.__version__
            capabilities["commands"] += ["brep", "import_step", "export_step"]
        except ImportError:
            pass
        return capabilities
    if command in ("import_3dm", "export_3dm"):
        import rhino_bridge
        return getattr(rhino_bridge, command)(request)
    if command in ("brep", "import_step", "export_step"):
        import ocp_bridge
        return getattr(ocp_bridge, command)(request)
    raise BridgeError(f"Unknown command: {command}")


def main():
    try:
        if len(sys.argv) != 2:
            raise BridgeError("Usage: python compat.py capabilities|import_3dm|export_3dm|brep|import_step|export_step")
        raw = sys.stdin.buffer.read(MAX_BYTES + 1)
        if len(raw) > MAX_BYTES:
            raise BridgeError("Request exceeds 64 MiB")
        request = strict_json(raw or b"{}")
        if not isinstance(request, dict):
            raise BridgeError("Request must be a JSON object")
        with native_stdout_to_stderr():
            result = dispatch(sys.argv[1], request)
        output = canonical(result).encode("utf-8")
        if len(output) > MAX_BYTES:
            raise BridgeError("Response exceeds 64 MiB; import a smaller document")
        sys.stdout.buffer.write(output + b"\n")
    except ImportError as error:
        print(f"Optional CAD backend unavailable: {error}. Install the compatibility addon.", file=sys.stderr)
        return 2
    except (BridgeError, ValueError, TypeError, KeyError, IndexError, AttributeError, RuntimeError, OSError, OverflowError) as error:
        print(f"MantisCAD compatibility error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
