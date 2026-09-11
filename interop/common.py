"""Bounded, data-only protocol utilities shared by optional CAD backends."""

import base64
import hashlib
import json
import math
import os
from pathlib import Path
import tempfile
import zlib

MAX_BYTES = 64 * 1024 * 1024
MAX_FILE_BYTES = 32 * 1024 * 1024
MAX_OBJECTS = 10000
MAX_VERTICES = 200000
MAX_TRIANGLES = 400000


class BridgeError(ValueError):
    pass


def canonical(value):
    return json.dumps(value, ensure_ascii=False, allow_nan=False, sort_keys=True, separators=(",", ":"))


def fingerprint(value):
    return hashlib.sha256(canonical(value).encode("utf-8")).hexdigest()


def strict_json(value):
    def invalid(number):
        raise BridgeError(f"Non-finite JSON number is not allowed: {number}")
    try:
        def finite_float(text):
            result = float(text)
            return result if math.isfinite(result) else invalid(text)
        return json.loads(value, parse_constant=invalid, parse_float=finite_float)
    except (ValueError, RecursionError) as error:
        raise BridgeError(f"Invalid JSON: {error}") from error


def number(value, label="number", positive=False):
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise BridgeError(f"{label} must be a finite number")
    result = float(value)
    if not math.isfinite(result) or abs(result) > 1e12 or (positive and result <= 0):
        raise BridgeError(f"{label} is outside the supported finite range")
    return result


def point(value, label="point"):
    if not isinstance(value, dict):
        raise BridgeError(f"{label} must have x, y, z coordinates")
    return {axis: number(value.get(axis), f"{label}.{axis}") for axis in ("x", "y", "z")}


def xyz(value):
    p = point(value)
    return p["x"], p["y"], p["z"]


def vector(value):
    result = point(value, "direction")
    length = math.sqrt(sum(v * v for v in result.values()))
    if length < 1e-12:
        raise BridgeError("Direction must not be zero")
    return {axis: coordinate / length for axis, coordinate in result.items()}


def bounded_list(value, maximum, label):
    if not isinstance(value, list) or len(value) > maximum:
        raise BridgeError(f"{label} must be a list with at most {maximum} entries")
    return value


def records(request):
    value = bounded_list(request.get("objects", []), MAX_OBJECTS, "objects")
    for item in value:
        if not isinstance(item, dict) or not isinstance(item.get("geometry"), dict):
            raise BridgeError("Every object must contain a geometry record")
        for key in ("name", "layer"):
            if not isinstance(item.get(key, ""), str) or len(item.get(key, "")) > 1024:
                raise BridgeError(f"Object {key} must be a string of at most 1024 characters")
        source = item.get("source")
        if source is not None and (not isinstance(source, dict)
                or source.get("format") not in ("rhino3dm", "ocp-brep")
                or not isinstance(source.get("data"), str)):
            raise BridgeError("Object source must contain a supported format and a string payload")
    return value


def mesh_data(mesh):
    if not isinstance(mesh, dict):
        raise BridgeError("mesh must be an object")
    positions = [point(p) for p in bounded_list(mesh.get("positions"), MAX_VERTICES, "positions")]
    indices = bounded_list(mesh.get("indices"), MAX_TRIANGLES, "indices")
    for triangle in indices:
        if not isinstance(triangle, list) or len(triangle) != 3:
            raise BridgeError("Mesh indices must contain triangles")
        if any(isinstance(i, bool) or not isinstance(i, int) or i < 0 or i >= len(positions) for i in triangle):
            raise BridgeError("Mesh triangle index is outside the vertex array")
    return positions, indices


def decode_base64(value, maximum=MAX_FILE_BYTES):
    if not isinstance(value, str) or len(value) > (maximum + 2) // 3 * 4:
        raise BridgeError("Encoded source is too large or not a string")
    try:
        decoded = base64.b64decode(value, validate=True)
    except ValueError as error:
        raise BridgeError("Invalid base64 source payload") from error
    if len(decoded) > maximum:
        raise BridgeError("Decoded source exceeds the size limit")
    return decoded


def decode_compressed(value):
    decoder = zlib.decompressobj()
    try:
        result = decoder.decompress(decode_base64(value), MAX_FILE_BYTES + 1)
    except zlib.error as error:
        raise BridgeError("Invalid compressed openNURBS source") from error
    if len(result) > MAX_FILE_BYTES or decoder.unconsumed_tail or decoder.unused_data or not decoder.eof:
        raise BridgeError("Compressed source is truncated, concatenated, or exceeds 32 MiB")
    return result


def read_file(value, extensions):
    path = checked_path(value, extensions)
    if not path.is_file() or path.stat().st_size > MAX_FILE_BYTES:
        raise BridgeError("Input file is missing or larger than 32 MiB")
    data = path.read_bytes()
    if not data or len(data) > MAX_FILE_BYTES:
        raise BridgeError("Input file is empty or larger than 32 MiB")
    return path, data


def checked_path(value, extensions):
    if not isinstance(value, str) or not value or "\0" in value:
        raise BridgeError("A valid file path is required")
    path = Path(value).expanduser().absolute()
    if path.suffix.lower() not in extensions:
        raise BridgeError(f"Expected file extension: {', '.join(extensions)}")
    return path


def atomic_export(request, extensions, writer):
    path = checked_path(request.get("path"), extensions)
    overwrite = request.get("overwrite", False)
    if not isinstance(overwrite, bool):
        raise BridgeError("overwrite must be true or false")
    if path.exists() and not overwrite:
        raise BridgeError("Output already exists; choose another path or explicitly enable overwrite")
    if not path.parent.is_dir():
        raise BridgeError("The output directory does not exist")
    descriptor, temporary_name = tempfile.mkstemp(prefix=".mantis-interop-", suffix=path.suffix, dir=path.parent)
    os.close(descriptor)
    temporary = Path(temporary_name)
    try:
        writer(temporary)
        if not temporary.stat().st_size or temporary.stat().st_size > MAX_FILE_BYTES:
            raise BridgeError("Export is empty or exceeds the 32 MiB file limit")
        with temporary.open("rb") as stream:
            os.fsync(stream.fileno())
        if overwrite:
            os.replace(temporary, path)
        else:
            # Atomic no-clobber commit on NTFS and ordinary Unix filesystems.
            os.link(temporary, path)
            temporary.unlink()
    except FileExistsError as error:
        raise BridgeError("Output was created by another process; it was not overwritten") from error
    finally:
        temporary.unlink(missing_ok=True)
    return str(path)
