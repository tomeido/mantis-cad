"""Real openNURBS .3dm exchange using McNeel's official rhino3dm bindings."""

import base64
import math
import zlib
import rhino3dm as rhino

from common import (BridgeError, MAX_VERTICES, atomic_export, bounded_list, canonical,
                    decode_base64, decode_compressed, fingerprint, mesh_data, number, point, read_file,
                    records, strict_json, xyz)

OCP_USER_STRING = "MantisCAD:OpenCascadeBREP:v1"
OCP_PREVIEW_HASH = "MantisCAD:OpenCascadePreviewSHA256:v1"


def unit_scale_to_mm(document, warnings):
    document = document or {}
    if not isinstance(document, dict):
        raise BridgeError("document metadata must be an object")
    units = document.get("units", 2)
    if isinstance(units, bool) or not isinstance(units, int):
        raise BridgeError("Document units must be a Rhino UnitSystem integer")
    if units == 0:
        warnings.append("Source has no unit system; coordinates interpreted as millimeters")
        return 1.0
    system = rhino.UnitSystem(units)
    if system in (rhino.UnitSystem.CustomUnits, rhino.UnitSystem.Unset):
        raise BridgeError("Custom or unset source units need explicit conversion before B-rep/STEP exchange")
    return number(rhino.UnitSystem.UnitScale(system, rhino.UnitSystem.Millimeters), "unit conversion", positive=True)


def mesh_fingerprint(mesh):
    preview = mesh_from_rhino(mesh)
    return fingerprint({"positions": preview["positions"], "indices": preview["indices"]})


def decode_preserved(saved):
    document = rhino.File3dm.FromByteArray(decode_compressed(saved["object_3dm_zlib"]))
    if document is None or len(document.Objects) != 1:
        raise BridgeError("Invalid preserved Rhino object document")
    return document, document.Objects[0]


def p3(value):
    return rhino.Point3d(*xyz(value))


def serial_point(value):
    return {"x": float(value.X), "y": float(value.Y), "z": float(value.Z)}


def rhino_plane(value):
    return rhino.Plane(p3(value["origin"]), rhino.Vector3d(*xyz(value["x_axis"])),
                       rhino.Vector3d(*xyz(value["y_axis"])))


def curve_to_rhino(value):
    if not isinstance(value, dict) or len(value) != 1:
        raise BridgeError("Curve must contain exactly one supported curve type")
    kind, curve = next(iter(value.items()))
    if kind == "Line":
        result = rhino.LineCurve(p3(curve["a"]), p3(curve["b"]))
    elif kind == "Polyline":
        points = [p3(v) for v in bounded_list(curve["points"], MAX_VERTICES, "curve points")]
        if len(points) < 2:
            raise BridgeError("Polyline needs at least two points")
        if curve.get("closed") and points[0] != points[-1]:
            points.append(points[0])
        result = rhino.PolylineCurve(points)
    elif kind in ("Circle", "Arc"):
        plane = rhino_plane(curve["plane"])
        radius = number(curve["radius"], "radius", positive=True)
        circle = rhino.Circle(radius)
        circle.Plane = plane
        if kind == "Circle":
            result = rhino.NurbsCurve.CreateFromCircle(circle)
        else:
            start = number(curve["start_angle"], "start angle")
            end = number(curve["end_angle"], "end angle")
            angle = end - start
            if abs(angle) < 1e-12 or abs(angle) > math.tau + 1e-10:
                raise BridgeError("Arc sweep must be nonzero and at most 2*pi in magnitude")
            rotation = rhino.Transform.Rotation(min(start, end), plane.ZAxis, plane.Origin)
            arc = rhino.Arc(circle, abs(angle))
            result = rhino.NurbsCurve.CreateFromArc(arc)
            result.Transform(rotation)
            if angle < 0:
                result.Reverse()
    elif kind == "Nurbs":
        points = [point(p) for p in bounded_list(curve["control_points"], MAX_VERTICES, "control points")]
        degree = curve["degree"]
        if isinstance(degree, bool) or not isinstance(degree, int) or degree < 1 or degree > 25 or len(points) <= degree:
            raise BridgeError("NURBS degree/control-point count is invalid")
        weights = [number(w, "weight", positive=True) for w in curve["weights"]]
        knots = [number(k, "knot") for k in curve["knots"]]
        if len(weights) != len(points) or len(knots) != len(points) + degree + 1:
            raise BridgeError("NURBS knot/weight lengths are invalid")
        if any(a > b for a, b in zip(knots, knots[1:])) or knots[degree] >= knots[len(points)]:
            raise BridgeError("NURBS knot domain is invalid")
        result = rhino.NurbsCurve(3, True, degree + 1, len(points))
        for i, (p, weight) in enumerate(zip(points, weights)):
            result.Points[i] = rhino.Point4d(p["x"] * weight, p["y"] * weight, p["z"] * weight, weight)
        for i, knot in enumerate(knots[1:-1]):
            result.Knots[i] = knot
    else:
        raise BridgeError(f"Unsupported curve type: {kind}")
    if result is None or not result.IsValid:
        raise BridgeError("The curve could not be represented as valid Rhino geometry")
    return result


def curve_from_rhino(curve):
    if isinstance(curve, rhino.LineCurve):
        return {"Line": {"a": serial_point(curve.PointAtStart), "b": serial_point(curve.PointAtEnd)}}
    polyline = curve.TryGetPolyline()
    if polyline is not None:
        if len(polyline) > MAX_VERTICES:
            raise BridgeError("Polyline exceeds the point limit")
        points = [serial_point(p) for p in polyline]
        closed = bool(curve.IsClosed)
        if closed and points and points[0] == points[-1]:
            points.pop()
        return {"Polyline": {"points": points, "closed": closed}}
    nurbs = curve.ToNurbsCurve()
    if nurbs is None or len(nurbs.Points) > MAX_VERTICES:
        raise BridgeError("Curve cannot be represented within the NURBS preview limit")
    weights, points = [], []
    for control in nurbs.Points:
        weight = number(control.W, "NURBS weight", positive=True)
        weights.append(weight)
        points.append({"x": control.X / weight, "y": control.Y / weight, "z": control.Z / weight})
    knots = list(nurbs.Knots)
    return {"Nurbs": {"degree": nurbs.Degree, "control_points": points, "weights": weights,
                       "knots": [knots[0], *knots, knots[-1]]}}


def mesh_from_rhino(mesh):
    if len(mesh.Vertices) > MAX_VERTICES or len(mesh.Faces) > 200000:
        raise BridgeError("Rhino mesh exceeds the preview limit")
    positions = [serial_point(mesh.Vertices.Point3dAt(i)) for i in range(len(mesh.Vertices))]
    indices = []
    for face in mesh.Faces:
        a, b, c, d = face
        indices.append([a, b, c])
        if c != d:
            indices.append([a, c, d])
    result = {"positions": positions, "indices": indices, "normals": []}
    mesh_data(result)
    return result


def geometry_to_rhino(geometry, warnings=None):
    kind = geometry.get("kind")
    if kind == "point":
        return rhino.Point(p3(geometry["point"]))
    if kind == "curve":
        return curve_to_rhino(geometry["curve"])
    if kind == "mesh":
        positions, indices = mesh_data(geometry["mesh"])
        if not positions or not indices:
            raise BridgeError("An empty preview cannot be exported without its original source geometry")
        mesh = rhino.Mesh()
        mesh.Vertices.UseDoublePrecisionVertices = True
        for p in positions:
            mesh.Vertices.AddPoint3d(*xyz(p))
        for a, b, c in indices:
            mesh.Faces.AddFace(a, b, c)
        # OCCT triangulations can contain collapsed triangles at surface poles.
        # Rhino rejects those zero-area faces even with double precision enabled.
        removed = mesh.Faces.CullDegenerateFaces()
        if removed:
            mesh.Vertices.CullUnused()
            mesh.Compact()
            if warnings is not None:
                warnings.append(f"Removed {removed} degenerate preview triangles for Rhino mesh validity; exact BREP is unchanged")
        mesh.Normals.ComputeNormals()
        if not mesh.IsValid:
            raise BridgeError("Invalid Rhino mesh")
        return mesh
    raise BridgeError(f"Unsupported geometry kind: {kind}")


def preview(geometry, warnings, name):
    if isinstance(geometry, rhino.Point):
        return {"kind": "point", "point": serial_point(geometry.Location)}
    if isinstance(geometry, rhino.Curve):
        return {"kind": "curve", "curve": curve_from_rhino(geometry)}
    if isinstance(geometry, rhino.Mesh):
        return {"kind": "mesh", "mesh": mesh_from_rhino(geometry)}
    if isinstance(geometry, rhino.Extrusion):
        mesh = geometry.GetMesh(rhino.MeshType.Render)
        if mesh is not None:
            return {"kind": "mesh", "mesh": mesh_from_rhino(mesh)}
        geometry = geometry.ToBrep()
    if isinstance(geometry, rhino.Brep):
        combined = rhino.Mesh()
        cached = True
        for face in geometry.Faces:
            mesh = face.GetMesh(rhino.MeshType.Render)
            if mesh is None:
                cached = False
                break
            combined.Append(mesh)
        if cached and len(combined.Faces):
            return {"kind": "mesh", "mesh": mesh_from_rhino(combined)}
        # Planar B-reps can be converted and triangulated exactly by optional OCCT.
        try:
            import ocp_bridge
            shape = ocp_bridge.planar_rhino_brep(geometry)
            return {"kind": "mesh", "mesh": ocp_bridge.tessellate(shape)}
        except (ImportError, BridgeError, RuntimeError, ValueError):
            pass
    warnings.append(f"{name or '(unnamed)'}: {type(geometry).__name__} has no supported preview; its exact original geometry is preserved")
    return {"kind": "mesh", "mesh": {"positions": [], "normals": [], "indices": []}}


def import_3dm(request):
    _, data = read_file(request.get("path"), (".3dm",))
    document = rhino.File3dm.FromByteArray(data)
    if document is None:
        raise BridgeError("The file is not a readable .3dm document")
    objects, warnings = [], []
    for obj in document.Objects:
        if obj.Attributes.IsInstanceDefinitionObject:
            continue  # Kept in the preserved document's definition tables.
        if len(objects) >= 10000:
            raise BridgeError("The document exceeds 10000 visible objects")
        attributes = obj.Attributes
        layer = document.Layers.FindIndex(attributes.LayerIndex) if hasattr(document.Layers, "FindIndex") else document.Layers[attributes.LayerIndex]
        geometry = preview(obj.Geometry, warnings, attributes.Name)
        # CommonObject.Decode currently returns an untyped CommonObject for
        # ObjectAttributes. A small compressed real .3dm retains all attributes
        # and geometry without reconstructing a lossy subset of their fields.
        single = rhino.File3dm()
        single.Objects.Add(obj.Geometry, attributes)
        source = {"object_3dm_zlib": base64.b64encode(zlib.compress(base64.b64decode(single.Encode()), 9)).decode("ascii"),
                  "layer": layer.Encode() if layer else None, "geometry_hash": fingerprint(geometry),
                  "material_index": attributes.MaterialIndex}
        record = {"name": attributes.Name, "layer": layer.FullPath if layer else "",
                  "geometry": geometry, "source": {"format": "rhino3dm", "data": canonical(source)}}
        ocp_source = attributes.GetUserString(OCP_USER_STRING)
        if ocp_source:
            expected_hash = attributes.GetUserString(OCP_PREVIEW_HASH)
            if isinstance(obj.Geometry, rhino.Mesh) and expected_hash and expected_hash == mesh_fingerprint(obj.Geometry):
                decode_base64(ocp_source)
                record["source"] = {"format": "ocp-brep", "data": ocp_source,
                                    "preview_units": int(document.Settings.ModelUnitSystem)}
                warnings.append(f"{attributes.Name or '(unnamed)'}: recovered exact OpenCascade BREP stored alongside the unchanged Rhino mesh")
            else:
                warnings.append(f"{attributes.Name or '(unnamed)'}: Rhino geometry was changed or has no matching fingerprint; stale OpenCascade BREP ignored")
        objects.append(record)
    metadata = {"units": int(document.Settings.ModelUnitSystem),
                "absolute_tolerance": document.Settings.ModelAbsoluteTolerance,
                "source_3dm_base64": base64.b64encode(data).decode("ascii"),
                "objects_sha256": fingerprint(objects),
                "layers": [{"index": layer.Index, "definition": layer.Encode()} for layer in document.Layers],
                "materials": [{"index": index, "definition": material.Encode()} for index, material in enumerate(document.Materials)]}
    return {"objects": objects, "warnings": warnings, "document": metadata}


def export_3dm(request):
    objects = records(request)
    metadata = request.get("document") or {}
    if not isinstance(metadata, dict):
        raise BridgeError("document metadata must be an object")
    original = decode_base64(metadata["source_3dm_base64"]) if metadata.get("source_3dm_base64") else None
    if original and metadata.get("objects_sha256") == fingerprint(objects):
        if rhino.File3dm.FromByteArray(original) is None:
            raise BridgeError("Preserved document source is invalid")
        path = atomic_export(request, (".3dm",), lambda target: target.write_bytes(original))
        return {"written": len(objects), "path": path, "warnings": [], "preserved_document": True}
    document = rhino.File3dm.FromByteArray(original) if original else rhino.File3dm()
    if document is None:
        raise BridgeError("Preserved document source is invalid")
    material_indices = {}
    if not original:
        for item in metadata.get("layers", []):
            layer = rhino.CommonObject.Decode(item["definition"])
            if not isinstance(layer, rhino.Layer):
                raise BridgeError("Invalid layer metadata")
            document.Layers.Add(layer)
        for item in metadata.get("materials", []):
            material = rhino.CommonObject.Decode(item["definition"])
            if not isinstance(material, rhino.Material):
                raise BridgeError("Invalid material metadata")
            material_indices[item["index"]] = document.Materials.Add(material)
    for obj in list(document.Objects):
        if not obj.Attributes.IsInstanceDefinitionObject:
            document.Objects.Delete(obj.Attributes.Id)
    if "units" in metadata:
        document.Settings.ModelUnitSystem = rhino.UnitSystem(int(metadata["units"]))
    elif any((record.get("source") or {}).get("format") == "ocp-brep" for record in objects):
        document.Settings.ModelUnitSystem = rhino.UnitSystem.Millimeters
    if "absolute_tolerance" in metadata:
        document.Settings.ModelAbsoluteTolerance = number(metadata["absolute_tolerance"], "absolute tolerance", positive=True)
    layers = {layer.FullPath: layer.Index for layer in document.Layers}
    warnings = []
    for record in objects:
        source = record.get("source") or {}
        attributes = rhino.ObjectAttributes()
        geometry = None
        saved = None
        if source.get("format") == "rhino3dm":
            saved = strict_json(source["data"])
            if saved.get("geometry_hash") == fingerprint(record["geometry"]):
                preserved_document, preserved_object = decode_preserved(saved)
                geometry, attributes = preserved_object.Geometry, preserved_object.Attributes
                # A standalone one-object archive has no material table, so
                # openNURBS resets its numeric material index on decode.
                attributes.MaterialIndex = saved.get("material_index", attributes.MaterialIndex)
                if isinstance(geometry, rhino.InstanceReference) and not original:
                    raise BridgeError("Block instances need the original document tables; export the preserved full document or use explicit curve/mesh objects")
            else:
                warnings.append(f"{record.get('name', '')}: changed geometry exported from its current representation")
        if geometry is None:
            geometry = geometry_to_rhino(record["geometry"], warnings)
        attributes.Name = record.get("name", "")
        if attributes.MaterialIndex in material_indices:
            attributes.MaterialIndex = material_indices[attributes.MaterialIndex]
        layer_name = record.get("layer", "") or "Default"
        if layer_name not in layers:
            layer = rhino.CommonObject.Decode(saved["layer"]) if saved and saved.get("layer") else rhino.Layer()
            if layer is None:
                raise BridgeError("Invalid preserved layer")
            layer.Name = layer_name.split("::")[-1]
            layers[layer_name] = document.Layers.Add(layer)
        attributes.LayerIndex = layers[layer_name]
        if source.get("format") == "ocp-brep":
            decode_base64(source["data"])
            source_factor = unit_scale_to_mm({"units": source.get("preview_units") or 2}, warnings)
            factor = unit_scale_to_mm(metadata, warnings) / source_factor
            if factor != 1:
                geometry.Transform(rhino.Transform.Scale(rhino.Point3d(0, 0, 0), 1 / factor))
                warnings.append(f"{attributes.Name or '(unnamed)'}: BREP preview converted into destination document units")
            attributes.SetUserString(OCP_USER_STRING, source["data"])
            if not isinstance(geometry, rhino.Mesh):
                raise BridgeError("OpenCascade export requires its mesh preview")
            attributes.SetUserString(OCP_PREVIEW_HASH, mesh_fingerprint(geometry))
            warnings.append(f"{attributes.Name or '(unnamed)'}: exported as a Rhino mesh with embedded exact OpenCascade BREP; use STEP for editable B-rep exchange")
        identifier = document.Objects.Add(geometry, attributes)
        if not identifier.int:
            raise BridgeError("Rhino could not add an object to the output document")
    def write(target):
        if not document.Write(str(target), 8):
            raise BridgeError("Rhino failed to write the .3dm document")
    path = atomic_export(request, (".3dm",), write)
    return {"written": len(objects), "path": path, "warnings": warnings, "preserved_document": False}
