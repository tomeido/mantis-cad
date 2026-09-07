"""Integration checks against real openNURBS and OpenCascade, without Rhino."""

import base64
import copy
import json
import math
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

import rhino3dm as rhino
import common
import ocp_bridge as ocp
import rhino_bridge as bridge


def p(x=0, y=0, z=0):
    return dict(x=x, y=y, z=z)


class CompatibilityTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)

    def tearDown(self):
        self.temp.cleanup()

    def box(self, origin=None, size=None):
        return ocp.brep({"operation": "box", "parameters": {
            "origin": origin or p(), "size": size or p(2, 3, 4)}})["objects"][0]

    def fixture(self):
        document = rhino.File3dm()
        document.Settings.ModelUnitSystem = rhino.UnitSystem.Millimeters
        document.Settings.ModelAbsoluteTolerance = 0.002
        layer = rhino.Layer()
        layer.Name, layer.Color = "Precision", (20, 80, 130, 255)
        layer_index = document.Layers.Add(layer)
        attributes = rhino.ObjectAttributes()
        attributes.Name, attributes.LayerIndex = "Origin", layer_index
        material = rhino.Material()
        material.Name, material.DiffuseColor = "Copper", (180, 80, 30, 255)
        attributes.MaterialIndex = document.Materials.Add(material)
        attributes.MaterialSource = rhino.ObjectMaterialSource.MaterialFromObject
        attributes.SetUserString("fixture", "한글 metadata")
        document.Objects.AddPoint(rhino.Point3d(1, 2, 3), attributes)
        document.Objects.AddCurve(rhino.LineCurve(rhino.Point3d(0, 0, 0), rhino.Point3d(2, 0, 0)))
        document.Objects.AddCurve(rhino.Circle(2).ToNurbsCurve())
        document.Objects.AddBrep(rhino.Brep.CreateFromBoundingBox(rhino.BoundingBox(0, 0, 0, 2, 3, 4)))
        path = self.root / "source.3dm"
        self.assertTrue(document.Write(str(path), 8))
        return path

    def test_exact_document_roundtrip(self):
        source = self.fixture()
        imported = bridge.import_3dm({"path": str(source)})
        self.assertEqual(len(imported["objects"]), 4)
        self.assertEqual(imported["document"]["units"], 2)
        output = self.root / "roundtrip.3dm"
        result = bridge.export_3dm(dict(imported, path=str(output)))
        self.assertTrue(result["preserved_document"])
        self.assertEqual(source.read_bytes(), output.read_bytes())

    def test_individual_source_retains_attributes_and_regenerates_edits(self):
        imported = bridge.import_3dm({"path": str(self.fixture())})
        imported["document"].pop("source_3dm_base64")
        output = self.root / "individual.3dm"
        bridge.export_3dm(dict(imported, path=str(output)))
        written = rhino.File3dm.Read(str(output))
        self.assertEqual(written.Objects[0].Attributes.GetUserString("fixture"), "한글 metadata")
        self.assertEqual(written.Objects[0].Attributes.Name, "Origin")
        self.assertEqual(written.Materials[written.Objects[0].Attributes.MaterialIndex].Name, "Copper")
        self.assertEqual(written.Objects[0].Attributes.MaterialSource, rhino.ObjectMaterialSource.MaterialFromObject)
        self.assertEqual(written.Layers[0].Color, (20, 80, 130, 255))
        self.assertAlmostEqual(written.Settings.ModelAbsoluteTolerance, 0.002)
        imported["objects"][0]["geometry"]["point"]["x"] = 17
        result = bridge.export_3dm(dict(imported, path=str(output), overwrite=True))
        self.assertTrue(any("changed geometry" in warning for warning in result["warnings"]))
        self.assertEqual(rhino.File3dm.Read(str(output)).Objects[0].Geometry.Location.X, 17)

    def test_rational_nurbs_and_signed_arcs(self):
        original = rhino.Circle(2).ToNurbsCurve()
        restored = bridge.curve_to_rhino(bridge.curve_from_rhino(original))
        for fraction in (0, 0.13, 0.5, 0.88, 1):
            a = original.PointAt(original.Domain.T0 + fraction * (original.Domain.T1 - original.Domain.T0))
            b = restored.PointAt(restored.Domain.T0 + fraction * (restored.Domain.T1 - restored.Domain.T0))
            self.assertLess(a.DistanceTo(b), 1e-10)
        for start, end in ((0.3, 2.2), (2.2, 0.3)):
            curve = bridge.curve_to_rhino({"Arc": {"plane": {"origin": p(1, 2, 3),
                "x_axis": p(1), "y_axis": p(0, 1)}, "radius": 5,
                "start_angle": start, "end_angle": end}})
            for actual, angle in ((curve.PointAtStart, start), (curve.PointAtEnd, end)):
                self.assertLess(actual.DistanceTo(rhino.Point3d(1 + 5 * math.cos(angle), 2 + 5 * math.sin(angle), 3)), 1e-9)

    def test_exact_booleans_trim_and_curved_primitives(self):
        a, b = self.box(), self.box(p(1))
        for operation, volume in (("union", 36), ("difference", 12), ("intersection", 12)):
            result = ocp.brep({"operation": operation, "objects": [a, b]})
            self.assertTrue(result["metrics"]["valid"])
            self.assertAlmostEqual(result["metrics"]["volume"], volume, places=7)
        trim = ocp.brep({"operation": "trim", "objects": [a], "parameters": {"origin": p(0, 0, 2)}})
        self.assertAlmostEqual(trim["metrics"]["volume"], 12, places=7)
        sphere = ocp.brep({"operation": "sphere", "parameters": {"radius": 2}})
        self.assertAlmostEqual(sphere["metrics"]["volume"], 4 / 3 * math.pi * 8, places=7)
        cylinder = ocp.brep({"operation": "cylinder", "parameters": {"radius": 2, "height": 3}})
        self.assertAlmostEqual(cylinder["metrics"]["volume"], math.pi * 4 * 3, places=7)

    def test_planar_rhino_brep_exact_conversion(self):
        imported = bridge.import_3dm({"path": str(self.fixture())})
        shape = ocp.shape_from_record(imported["objects"][3], [])
        self.assertAlmostEqual(ocp.metrics(shape)["volume"], 24, places=7)

    def test_source_units_scale_once_into_millimeters(self):
        imported = bridge.import_3dm({"path": str(self.fixture())})
        record = imported["objects"][3]
        warnings = []
        shape = ocp.shape_from_record(record, warnings, {"units": 4})  # millimeters -> source meters
        self.assertAlmostEqual(ocp.metrics(shape)["volume"] / 1e9, 24, places=7)
        self.assertTrue(warnings)
        exact = self.box()
        unchanged = ocp.shape_from_record(exact, [], {"units": 4})
        self.assertAlmostEqual(ocp.metrics(unchanged)["volume"], 24, places=7)
        output = self.root / "units.3dm"
        bridge.export_3dm({"path": str(output), "objects": [exact]})
        self.assertEqual(rhino.File3dm.Read(str(output)).Settings.ModelUnitSystem, rhino.UnitSystem.Millimeters)
        output_m = self.root / "meters.3dm"
        bridge.export_3dm({"path": str(output_m), "objects": [exact], "document": {"units": 4}})
        restored = bridge.import_3dm({"path": str(output_m)})
        self.assertAlmostEqual(max(v["x"] for v in restored["objects"][0]["geometry"]["mesh"]["positions"]), 0.002)
        self.assertAlmostEqual(ocp.metrics(ocp.shape_from_record(restored["objects"][0], [], restored["document"]))["volume"], 24)
        # Bypass unchanged-document shortcut: a recovered preview is already in
        # destination meters, although its exact BREP payload remains in mm.
        restored["objects"][0]["name"] = "renamed"
        subset = self.root / "renamed-meters.3dm"
        bridge.export_3dm(dict(restored, path=str(subset)))
        again = bridge.import_3dm({"path": str(subset)})
        self.assertAlmostEqual(max(v["x"] for v in again["objects"][0]["geometry"]["mesh"]["positions"]), 0.002)
        self.assertAlmostEqual(ocp.metrics(ocp.shape_from_record(again["objects"][0], [], again["document"]))["volume"], 24)

    def test_fillet_step_and_rhino_pole_mesh_roundtrip(self):
        fillet = ocp.brep({"operation": "fillet", "objects": [self.box(size=p(10, 10, 10))],
                           "parameters": {"radius": 1, "edges": []}})
        self.assertLess(fillet["metrics"]["volume"], 1000)
        self.assertGreater(fillet["metrics"]["faces"], 6)
        step = self.root / "fillet.step"
        ocp.export_step({"path": str(step), "objects": fillet["objects"]})
        restored = ocp.import_step({"path": str(step)})
        self.assertAlmostEqual(restored["metrics"]["volume"], fillet["metrics"]["volume"], places=6)
        cli_output = self.root / "cli.step"
        process = subprocess.run([sys.executable, "-I", str(Path(__file__).with_name("compat.py")), "export_step"],
                                 input=json.dumps({"path": str(cli_output), "objects": fillet["objects"]}).encode(),
                                 capture_output=True, timeout=30)
        self.assertEqual(process.returncode, 0, process.stderr)
        self.assertEqual(json.loads(process.stdout)["written"], 1)  # No native C++ stdout contaminates the protocol.
        path = self.root / "fillet.3dm"
        result = bridge.export_3dm({"path": str(path), "objects": restored["objects"]})
        self.assertTrue(any("Rhino mesh" in warning for warning in result["warnings"]))
        imported = bridge.import_3dm({"path": str(path)})
        self.assertEqual(imported["objects"][0]["source"]["format"], "ocp-brep")
        exact = ocp.shape_from_record(imported["objects"][0], [])
        self.assertAlmostEqual(ocp.metrics(exact)["volume"], fillet["metrics"]["volume"], places=6)

    def test_rhino_mesh_edit_invalidates_embedded_exact_source(self):
        path = self.root / "editable.3dm"
        bridge.export_3dm({"path": str(path), "objects": [self.box()]})
        doc = rhino.File3dm.Read(str(path))
        obj = doc.Objects[0]
        geometry, attributes = obj.Geometry, obj.Attributes
        geometry.Translate(rhino.Vector3d(10, 0, 0))
        doc.Objects.Delete(attributes.Id)
        doc.Objects.Add(geometry, attributes)
        self.assertTrue(doc.Write(str(path), 8))
        imported = bridge.import_3dm({"path": str(path)})
        self.assertEqual(imported["objects"][0]["source"]["format"], "rhino3dm")
        self.assertTrue(any("stale" in warning for warning in imported["warnings"]))
        self.assertGreaterEqual(min(v["x"] for v in imported["objects"][0]["geometry"]["mesh"]["positions"]), 10)

    def test_no_overwrite_and_invalid_input_leave_file_untouched(self):
        path = self.root / "sentinel.3dm"
        path.write_bytes(b"KEEP")
        with self.assertRaises(common.BridgeError):
            bridge.export_3dm({"path": str(path), "objects": [self.box()]})
        self.assertEqual(path.read_bytes(), b"KEEP")
        for source in ("garbage", {"format": "python", "data": "ignored"}):
            item = self.box()
            item["source"] = source
            with self.assertRaises(common.BridgeError):
                bridge.export_3dm({"path": str(path), "objects": [item], "overwrite": True})
        self.assertEqual(path.read_bytes(), b"KEEP")

    def test_cli_rejects_malformed_and_nonfinite_json_cleanly(self):
        for payload in (b"{", b"[]", b'{"radius":NaN}', b'{"radius":1e999}'):
            process = subprocess.run([sys.executable, "-I", str(Path(__file__).with_name("compat.py")), "capabilities"],
                                     input=payload, capture_output=True, timeout=20)
            self.assertNotEqual(process.returncode, 0)
            self.assertEqual(process.stdout, b"")
            self.assertNotIn(b"Traceback", process.stderr)
        process = subprocess.run([sys.executable, "-I", str(Path(__file__).with_name("compat.py")), "brep"],
                                 input=b'{"operation":"box"}', capture_output=True, timeout=30)
        self.assertEqual(process.returncode, 0, process.stderr)
        self.assertAlmostEqual(json.loads(process.stdout)["metrics"]["volume"], 1)

    def test_rejects_corrupt_source_and_nonmanifold_mesh(self):
        with self.assertRaises(common.BridgeError):
            common.decode_compressed(base64.b64encode(b"bad zlib").decode())
        with self.assertRaises(common.BridgeError):
            ocp.decode_shape(base64.b64encode(b"not BREP").decode())
        with self.assertRaises(common.BridgeError):
            ocp.mesh_to_shape({"positions": [p(), p(1), p(0, 1)], "indices": [[0, 1, 2]]})
        with self.assertRaises(common.BridgeError):
            common.mesh_data({"positions": [p()], "indices": [[0, 1, 2]]})


if __name__ == "__main__":
    unittest.main()
