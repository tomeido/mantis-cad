"""Optional exact OpenCascade B-rep modeling and STEP exchange (without VTK)."""

import base64
from collections import Counter
import io
import math

from OCP.BRep import BRep_Builder, BRep_Tool
from OCP.BRepAlgoAPI import BRepAlgoAPI_Common, BRepAlgoAPI_Cut, BRepAlgoAPI_Fuse
from OCP.BRepBuilderAPI import (BRepBuilderAPI_MakeEdge, BRepBuilderAPI_MakeFace,
                               BRepBuilderAPI_MakePolygon, BRepBuilderAPI_MakeSolid,
                               BRepBuilderAPI_MakeWire, BRepBuilderAPI_Sewing,
                               BRepBuilderAPI_Transform)
from OCP.BRepCheck import BRepCheck_Analyzer
from OCP.BRepFilletAPI import BRepFilletAPI_MakeFillet
from OCP.BRepGProp import BRepGProp
from OCP.BRepLib import BRepLib
from OCP.BRepMesh import BRepMesh_IncrementalMesh
from OCP.BRepPrimAPI import (BRepPrimAPI_MakeBox, BRepPrimAPI_MakeCylinder,
                            BRepPrimAPI_MakeHalfSpace, BRepPrimAPI_MakeSphere)
from OCP.BRepTools import BRepTools
from OCP.BRepBndLib import BRepBndLib
from OCP.Bnd import Bnd_Box
from OCP.GProp import GProp_GProps
from OCP.Geom import Geom_BSplineCurve
from OCP.IFSelect import IFSelect_RetDone
from OCP.STEPControl import STEPControl_AsIs, STEPControl_Reader, STEPControl_Writer
from OCP.TColgp import TColgp_Array1OfPnt
from OCP.TColStd import TColStd_Array1OfInteger, TColStd_Array1OfReal
from OCP.TopAbs import TopAbs_EDGE, TopAbs_FACE, TopAbs_REVERSED, TopAbs_SHELL, TopAbs_SOLID
from OCP.TopExp import TopExp, TopExp_Explorer
from OCP.TopLoc import TopLoc_Location
from OCP.TopoDS import TopoDS, TopoDS_Compound, TopoDS_Shape
from OCP.TopTools import TopTools_FormatVersion_VERSION_3, TopTools_IndexedMapOfShape
from OCP.gp import gp_Ax2, gp_Dir, gp_Pln, gp_Pnt, gp_Trsf

from common import (BridgeError, MAX_TRIANGLES, MAX_VERTICES, atomic_export,
                    decode_base64, mesh_data, number, point, read_file, records,
                    strict_json, vector, xyz)

ZERO = {"x": 0, "y": 0, "z": 0}
Z = {"x": 0, "y": 0, "z": 1}


def explore(shape, kind):
    explorer = TopExp_Explorer(shape, kind)
    while explorer.More():
        yield explorer.Current()
        explorer.Next()


def valid(shape):
    if shape.IsNull() or not BRepCheck_Analyzer(shape).IsValid():
        raise BridgeError("OpenCascade produced or received an invalid B-rep")
    return shape


def decode_shape(source):
    data = decode_base64(source)
    if not data.lstrip().startswith((b"DBRep_DrawableShape", b"CASCADE Topology")):
        raise BridgeError("Source is not an OpenCascade BREP document")
    shape = TopoDS_Shape()
    BRepTools.Read_s(shape, io.BytesIO(data), BRep_Builder())
    return valid(shape)


def encode_shape(shape):
    output = io.BytesIO()
    BRepTools.Write_s(shape, output, False, False, TopTools_FormatVersion_VERSION_3)
    return base64.b64encode(output.getvalue()).decode("ascii")


def compound(shapes):
    result = TopoDS_Compound()
    builder = BRep_Builder()
    builder.MakeCompound(result)
    for shape in shapes:
        builder.Add(result, shape)
    return result


def sew_faces(faces, require_solid):
    sewing = BRepBuilderAPI_Sewing(1e-7)
    for face in faces:
        sewing.Add(face)
    sewing.Perform()
    shape = sewing.SewedShape()
    if require_solid:
        solids = []
        for raw_shell in explore(shape, TopAbs_SHELL):
            shell = TopoDS.Shell_s(raw_shell)
            builder = BRepBuilderAPI_MakeSolid(shell)
            solid = builder.Solid()
            if not BRepLib.OrientClosedSolid_s(solid):
                raise BridgeError("The surface shell cannot form a closed solid")
            solids.append(valid(solid))
        if not solids:
            raise BridgeError("No closed shell could be constructed from the input")
        shape = solids[0] if len(solids) == 1 else compound(solids)
    return valid(shape)


def mesh_to_shape(mesh):
    positions, indices = mesh_data(mesh)
    if not indices or len(indices) > 20000:
        raise BridgeError("B-rep conversion needs a closed mesh with 1–20000 triangles")
    # Rendering meshes often split identical positions to keep sharp normals.
    # Weld exact duplicates for the topology check without moving any vertex.
    lookup, welded = {}, []
    for p in positions:
        key = xyz(p)
        if key not in lookup:
            lookup[key] = len(lookup)
        welded.append(lookup[key])
    topology = [[welded[index] for index in tri] for tri in indices]
    edges = Counter(tuple(sorted((a, b))) for tri in topology for a, b in zip(tri, tri[1:] + tri[:1]))
    if any(count != 2 for count in edges.values()):
        raise BridgeError("Mesh must be closed and manifold before conversion to a solid B-rep")
    faces = []
    for tri in indices:
        polygon = BRepBuilderAPI_MakePolygon()
        for index in tri:
            polygon.Add(gp_Pnt(*xyz(positions[index])))
        polygon.Close()
        if not polygon.IsDone():
            raise BridgeError("Mesh contains an invalid triangle")
        builder = BRepBuilderAPI_MakeFace(polygon.Wire(), True)
        if not builder.IsDone():
            raise BridgeError("Mesh triangle could not become a planar B-rep face")
        faces.append(builder.Face())
    return sew_faces(faces, True)


def rhino_edge(edge):
    import rhino_bridge
    if edge.IsLinear():
        a, b = edge.PointAtStart, edge.PointAtEnd
        return BRepBuilderAPI_MakeEdge(gp_Pnt(a.X, a.Y, a.Z), gp_Pnt(b.X, b.Y, b.Z)).Edge()
    data = rhino_bridge.curve_from_rhino(edge)
    if "Nurbs" not in data:
        raise BridgeError("This planar B-rep edge cannot be converted exactly")
    curve = data["Nurbs"]
    n = len(curve["control_points"])
    poles, weights = TColgp_Array1OfPnt(1, n), TColStd_Array1OfReal(1, n)
    for i, (p, w) in enumerate(zip(curve["control_points"], curve["weights"]), 1):
        poles.SetValue(i, gp_Pnt(*xyz(p)))
        weights.SetValue(i, w)
    unique, counts = [], []
    for knot in curve["knots"]:
        if unique and knot == unique[-1]:
            counts[-1] += 1
        else:
            unique.append(knot)
            counts.append(1)
    knots = TColStd_Array1OfReal(1, len(unique))
    multiplicities = TColStd_Array1OfInteger(1, len(unique))
    for i, (knot, count) in enumerate(zip(unique, counts), 1):
        knots.SetValue(i, knot)
        multiplicities.SetValue(i, count)
    geometry = Geom_BSplineCurve(poles, weights, knots, multiplicities, curve["degree"], False)
    return BRepBuilderAPI_MakeEdge(geometry).Edge()


def planar_rhino_brep(brep):
    """Preserve true planar faces and their trimmed loop boundaries, including holes."""
    import rhino3dm
    faces = []
    if len(brep.Faces) > 20000:
        raise BridgeError("B-rep exceeds the face-conversion limit")
    for face in brep.Faces:
        if not face.IsPlanar(1e-7):
            raise BridgeError("Exact Rhino-to-OpenCascade conversion currently supports planar B-rep faces only; use STEP for curved B-reps")
        outer, inner = None, []
        for loop in face.Loops:
            wire = BRepBuilderAPI_MakeWire()
            for trim in loop.Trims:
                if trim.EdgeIndex < 0:
                    continue
                edge = rhino_edge(brep.Edges[trim.EdgeIndex])
                if trim.IsReversed:
                    edge.Reverse()
                wire.Add(edge)
            if not wire.IsDone():
                raise BridgeError("Trimmed planar boundary could not be assembled exactly")
            if loop.LoopType == rhino3dm.BrepLoopType.Outer:
                outer = wire.Wire()
            elif loop.LoopType == rhino3dm.BrepLoopType.Inner:
                inner.append(wire.Wire())
            else:
                raise BridgeError("Unsupported singular or slit loop in planar B-rep")
        if outer is None:
            raise BridgeError("Planar B-rep face has no outer boundary")
        builder = BRepBuilderAPI_MakeFace(outer, True)
        for hole in inner:
            builder.Add(hole)
        if not builder.IsDone():
            raise BridgeError("Planar face could not be reconstructed")
        result = builder.Face()
        if face.OrientationIsReversed:
            result.Reverse()
        faces.append(result)
    return sew_faces(faces, bool(brep.IsSolid))


def _shape_from_record(record, warnings):
    source = record.get("source") or {}
    if source.get("format") == "ocp-brep":
        return decode_shape(source["data"])
    if source.get("format") == "rhino3dm":
        import rhino3dm
        from common import fingerprint
        saved = strict_json(source["data"])
        if saved.get("geometry_hash") == fingerprint(record["geometry"]):
            import rhino_bridge
            preserved_document, preserved_object = rhino_bridge.decode_preserved(saved)
            geometry = preserved_object.Geometry
            if isinstance(geometry, rhino3dm.Extrusion):
                geometry = geometry.ToBrep()
            if isinstance(geometry, rhino3dm.Brep):
                return planar_rhino_brep(geometry)
    if record["geometry"].get("kind") == "mesh":
        warnings.append(f"{record.get('name') or '(unnamed)'}: converted the closed triangle mesh into a faceted B-rep; smooth analytic surfaces were not reconstructed")
        return mesh_to_shape(record["geometry"]["mesh"])
    raise BridgeError("This operation needs preserved B-rep geometry or a closed triangle mesh")


def shape_from_record(record, warnings, document=None):
    shape = _shape_from_record(record, warnings)
    if (record.get("source") or {}).get("format") == "ocp-brep":
        return shape  # Stored OpenCascade payloads always use millimeters.
    import rhino_bridge
    factor = rhino_bridge.unit_scale_to_mm(document, warnings)
    if factor != 1:
        transform = gp_Trsf()
        transform.SetScale(gp_Pnt(0, 0, 0), factor)
        shape = valid(BRepBuilderAPI_Transform(shape, transform, True).Shape())
        warnings.append(f"{record.get('name') or '(unnamed)'}: source coordinates scaled by {factor:g} to millimeters")
    return shape


def tessellate(shape, deflection=0.05):
    bounds = Bnd_Box()
    BRepBndLib.Add_s(shape, bounds, False)
    xmin, ymin, zmin, xmax, ymax, zmax = bounds.Get()
    diagonal = math.sqrt((xmax - xmin) ** 2 + (ymax - ymin) ** 2 + (zmax - zmin) ** 2)
    # Preview-only accuracy floor prevents enormous tessellations for huge
    # coordinate ranges or adversarial tiny deflection values. BREP stays exact.
    effective = max(number(deflection, "deflection", positive=True), diagonal * 0.001, 1e-6)
    mesher = BRepMesh_IncrementalMesh(shape, effective, False, 0.3, False)
    if not mesher.IsDone():
        raise BridgeError("B-rep preview tessellation failed")
    positions, triangles = [], []
    for raw_face in explore(shape, TopAbs_FACE):
        face = TopoDS.Face_s(raw_face)
        location = TopLoc_Location()
        triangulation = BRep_Tool.Triangulation_s(face, location)
        if triangulation is None:
            continue
        if len(positions) + triangulation.NbNodes() > MAX_VERTICES or len(triangles) + triangulation.NbTriangles() > MAX_TRIANGLES:
            raise BridgeError("B-rep preview exceeds the mesh limit; increase deflection")
        offset = len(positions)
        for i in range(1, triangulation.NbNodes() + 1):
            p = triangulation.Node(i).Transformed(location.Transformation())
            positions.append({"x": p.X(), "y": p.Y(), "z": p.Z()})
        for i in range(1, triangulation.NbTriangles() + 1):
            a, b, c = triangulation.Triangle(i).Get()
            if face.Orientation() == TopAbs_REVERSED:
                b, c = c, b
            triangles.append([offset + a - 1, offset + b - 1, offset + c - 1])
    normals = [[0.0, 0.0, 0.0] for _ in positions]
    for a, b, c in triangles:
        p, q, r = (positions[index] for index in (a, b, c))
        u = [q[axis] - p[axis] for axis in ("x", "y", "z")]
        v = [r[axis] - p[axis] for axis in ("x", "y", "z")]
        cross = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]]
        for index in (a, b, c):
            for axis in range(3):
                normals[index][axis] += cross[axis]
    normalized = []
    for normal in normals:
        length = math.sqrt(sum(v * v for v in normal))
        normalized.append(dict(zip(("x", "y", "z"), [v / length for v in normal] if length > 1e-20 else [0, 0, 1])))
    return {"positions": positions, "normals": normalized, "indices": triangles}


def metrics(shape):
    volume, area = GProp_GProps(), GProp_GProps()
    BRepGProp.VolumeProperties_s(shape, volume)
    BRepGProp.SurfaceProperties_s(shape, area)
    return {"volume": volume.Mass(), "area": area.Mass(), "valid": True,
            "faces": len(list(explore(shape, TopAbs_FACE))), "edges": len(unique_edges(shape))}


def unique_edges(shape):
    mapped = TopTools_IndexedMapOfShape()
    TopExp.MapShapes_s(shape, TopAbs_EDGE, mapped)
    return [TopoDS.Edge_s(mapped.FindKey(i)) for i in range(1, mapped.Extent() + 1)]


def shape_record(shape, name, deflection=0.05):
    valid(shape)
    source = encode_shape(shape)  # Excludes triangulations; exact B-rep stays compact.
    return {"name": name, "layer": "MantisCAD BRep", "geometry": {"kind": "mesh", "mesh": tessellate(shape, deflection)},
            "source": {"format": "ocp-brep", "data": source}}


def brep(request):
    operation = request.get("operation")
    parameters = request.get("parameters") or {}
    if not isinstance(parameters, dict):
        raise BridgeError("parameters must be an object")
    warnings = []
    inputs = records(request)
    if operation == "box":
        origin = gp_Pnt(*xyz(parameters.get("origin", ZERO)))
        size = point(parameters.get("size", {"x": 1, "y": 1, "z": 1}))
        shape = BRepPrimAPI_MakeBox(origin, *(number(size[axis], "box size", positive=True) for axis in ("x", "y", "z"))).Shape()
    elif operation == "sphere":
        shape = BRepPrimAPI_MakeSphere(gp_Pnt(*xyz(parameters.get("center", ZERO))), number(parameters.get("radius", 1), "radius", positive=True)).Shape()
    elif operation == "cylinder":
        axis = gp_Ax2(gp_Pnt(*xyz(parameters.get("origin", ZERO))), gp_Dir(*xyz(vector(parameters.get("direction", Z)))))
        shape = BRepPrimAPI_MakeCylinder(axis, number(parameters.get("radius", 1), "radius", positive=True), number(parameters.get("height", 1), "height", positive=True)).Shape()
    elif operation in ("union", "difference", "intersection"):
        if len(inputs) < 2 or len(inputs) > 64:
            raise BridgeError("Boolean operations require 2–64 objects")
        shape = shape_from_record(inputs[0], warnings, request.get("document"))
        constructor = {"union": BRepAlgoAPI_Fuse, "difference": BRepAlgoAPI_Cut, "intersection": BRepAlgoAPI_Common}[operation]
        for record in inputs[1:]:
            algorithm = constructor(shape, shape_from_record(record, warnings, request.get("document")))
            algorithm.SetRunParallel(False)
            algorithm.Build()
            if not algorithm.IsDone():
                raise BridgeError("OpenCascade Boolean operation failed")
            algorithm.SimplifyResult(True, True)
            shape = algorithm.Shape()
    elif operation == "trim":
        if len(inputs) != 1:
            raise BridgeError("Plane trim requires one object")
        origin = point(parameters.get("origin", ZERO))
        normal = vector(parameters.get("normal", Z))
        keep = parameters.get("keep", "positive")
        if keep not in ("positive", "negative"):
            raise BridgeError("Trim keep must be positive or negative")
        direction = 1 if keep == "positive" else -1
        plane = gp_Pln(gp_Pnt(*xyz(origin)), gp_Dir(*xyz(normal)))
        face = BRepBuilderAPI_MakeFace(plane).Face()
        reference = gp_Pnt(*(origin[axis] + direction * normal[axis] for axis in ("x", "y", "z")))
        half_space = BRepPrimAPI_MakeHalfSpace(face, reference).Solid()
        algorithm = BRepAlgoAPI_Common(shape_from_record(inputs[0], warnings, request.get("document")), half_space)
        algorithm.Build()
        if not algorithm.IsDone():
            raise BridgeError("OpenCascade plane trim failed")
        shape = algorithm.Shape()
    elif operation == "fillet":
        if len(inputs) != 1:
            raise BridgeError("Edge fillet requires one object")
        shape = shape_from_record(inputs[0], warnings, request.get("document"))
        radius = number(parameters.get("radius", 0.1), "fillet radius", positive=True)
        edges = unique_edges(shape)
        selected = parameters.get("edges") or list(range(len(edges)))
        if not isinstance(selected, list) or not selected or len(selected) > 10000:
            raise BridgeError("No supported edges selected for fillet")
        builder = BRepFilletAPI_MakeFillet(shape)
        for index in sorted(set(selected)):
            if isinstance(index, bool) or not isinstance(index, int) or index < 0 or index >= len(edges):
                raise BridgeError("Fillet edge index is outside the B-rep")
            builder.Add(radius, edges[index])
        builder.Build()
        if not builder.IsDone():
            raise BridgeError("OpenCascade fillet failed; reduce radius or select fewer edges")
        shape = builder.Shape()
    else:
        raise BridgeError(f"Unsupported B-rep operation: {operation}")
    valid(shape)
    if not list(explore(shape, TopAbs_FACE)):
        return {"objects": [], "warnings": warnings + ["The exact operation produced an empty result"], "metrics": {"volume": 0, "area": 0, "faces": 0, "edges": 0}}
    record = shape_record(shape, f"BRep {operation}", parameters.get("deflection", 0.05))
    return {"objects": [record], "warnings": warnings, "metrics": metrics(shape)}


def import_step(request):
    path, _ = read_file(request.get("path"), (".step", ".stp"))
    reader = STEPControl_Reader()
    if reader.ReadFile(str(path)) != IFSelect_RetDone or reader.TransferRoots() == 0:
        raise BridgeError("OpenCascade could not read STEP geometry")
    shape = valid(reader.OneShape())
    return {"objects": [shape_record(shape, path.stem)], "warnings": ["STEP geometry imported exactly; assembly names, colors and non-geometric metadata are not imported"],
            "document": {"units": 2, "format": "STEP", "coordinate_units": "millimeters"}, "metrics": metrics(shape)}


def export_step(request):
    warnings = []
    objects = records(request)
    if not objects:
        raise BridgeError("STEP export needs at least one object")
    shapes = [shape_from_record(record, warnings, request.get("document")) for record in objects]
    def write(target):
        writer = STEPControl_Writer()
        for shape in shapes:
            if writer.Transfer(shape, STEPControl_AsIs) != IFSelect_RetDone:
                raise BridgeError("OpenCascade could not transfer a B-rep to STEP")
        if writer.Write(str(target)) != IFSelect_RetDone:
            raise BridgeError("OpenCascade could not write STEP")
    path = atomic_export(request, (".step", ".stp"), write)
    return {"written": len(objects), "path": path, "warnings": warnings}
