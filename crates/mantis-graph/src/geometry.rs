//! Baked interchange geometry. Only validated, self-contained data enters
//! evaluation; importing and CAD engine execution happen outside the graph.

use crate::Value;
use mantis_kernel::{Curve, Mesh, Plane, Vec3};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub const MAX_RECORD_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GeometryRecord {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub layer: String,
    pub geometry: GeometryData,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<GeometrySource>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GeometrySource {
    pub format: String,
    pub data: String,
    /// Unit system of the preview coordinates. Exact OCCT BREP data uses mm;
    /// a preview recovered from Rhino may instead use that document's units.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_units: Option<i32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GeometryData {
    Point { point: Vec3 },
    Plane { plane: Plane },
    Curve { curve: Curve },
    Mesh { mesh: Mesh },
}

impl GeometryRecord {
    pub fn from_json(json: &str) -> Result<Self, String> {
        if json.len() > MAX_RECORD_BYTES {
            return Err("One geometry record exceeds the 32 MiB limit.".into());
        }
        let record: Self =
            serde_json::from_str(json).map_err(|e| format!("Invalid geometry record: {e}"))?;
        record.validate()?;
        Ok(record)
    }

    pub fn to_json(&self) -> Result<String, String> {
        self.validate()?;
        let json = serde_json::to_string(self).map_err(|e| e.to_string())?;
        if json.len() > MAX_RECORD_BYTES {
            return Err("One geometry record exceeds the 32 MiB limit.".into());
        }
        Ok(json)
    }

    pub fn from_value(value: &Value, name: String) -> Option<Self> {
        let geometry = match value {
            Value::Vector(point) => GeometryData::Point { point: *point },
            Value::Curve(curve) => GeometryData::Curve {
                curve: (**curve).clone(),
            },
            Value::Mesh(mesh) => GeometryData::Mesh {
                mesh: (**mesh).clone(),
            },
            _ => return None,
        };
        Some(Self {
            name,
            layer: String::new(),
            geometry,
            source: None,
        })
    }

    pub fn value(&self) -> Value {
        match &self.geometry {
            GeometryData::Point { point } => Value::Vector(*point),
            GeometryData::Plane { plane } => Value::Plane(*plane),
            GeometryData::Curve { curve } => Value::Curve(Arc::new(curve.clone())),
            GeometryData::Mesh { mesh } => {
                let mut mesh = mesh.clone();
                mesh.recompute_normals();
                Value::Mesh(Arc::new(mesh))
            }
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.name.len() > 4096 || self.layer.len() > 4096 {
            return Err("Geometry name or layer name is too long.".into());
        }
        if let Some(source) = &self.source {
            if source.format.len() > 64 || source.data.len() > MAX_RECORD_BYTES {
                return Err("Preserved CAD source exceeds the record limit.".into());
            }
        }
        match &self.geometry {
            GeometryData::Point { point } => valid_point(*point),
            GeometryData::Plane { plane } => valid_plane(plane),
            GeometryData::Curve { curve } => valid_curve(curve),
            GeometryData::Mesh { mesh } => {
                if mesh.positions.len() > 500_000 || mesh.indices.len() > 1_000_000 {
                    return Err("Mesh exceeds 500,000 vertices or 1,000,000 triangles.".into());
                }
                for point in mesh.positions.iter().chain(&mesh.normals) {
                    valid_point(*point)?;
                }
                if mesh
                    .indices
                    .iter()
                    .flatten()
                    .any(|i| *i as usize >= mesh.positions.len())
                {
                    return Err("Mesh contains an out-of-range vertex index.".into());
                }
                Ok(())
            }
        }
    }
}

fn valid_point(p: Vec3) -> Result<(), String> {
    if [p.x, p.y, p.z]
        .iter()
        .all(|v| v.is_finite() && v.abs() <= 1e12)
    {
        Ok(())
    } else {
        Err("Geometry coordinates must be finite and within ±1e12.".into())
    }
}

fn valid_plane(p: &Plane) -> Result<(), String> {
    valid_point(p.origin)?;
    valid_point(p.x_axis)?;
    valid_point(p.y_axis)?;
    if (p.x_axis.length() - 1.0).abs() > 1e-6
        || (p.y_axis.length() - 1.0).abs() > 1e-6
        || p.x_axis.dot(p.y_axis).abs() > 1e-6
    {
        return Err("Curve plane axes must be orthonormal.".into());
    }
    Ok(())
}

fn valid_curve(curve: &Curve) -> Result<(), String> {
    match curve {
        Curve::Line { a, b } => {
            valid_point(*a)?;
            valid_point(*b)
        }
        Curve::Polyline { points, .. } => {
            if !(2..=100_000).contains(&points.len()) {
                return Err("Polyline requires 2 to 100,000 points.".into());
            }
            points.iter().try_for_each(|p| valid_point(*p))
        }
        Curve::Circle { plane, radius } | Curve::Arc { plane, radius, .. } => {
            valid_plane(plane)?;
            if !radius.is_finite() || *radius <= 0.0 || *radius > 1e12 {
                return Err("Curve radius must be positive, finite and at most 1e12.".into());
            }
            if let Curve::Arc {
                start_angle,
                end_angle,
                ..
            } = curve
            {
                let sweep = end_angle - start_angle;
                if !start_angle.is_finite()
                    || !end_angle.is_finite()
                    || !sweep.is_finite()
                    || start_angle.abs() > 1e12
                    || end_angle.abs() > 1e12
                    || sweep.abs() > std::f64::consts::TAU + 1e-8
                    || sweep.abs() < 1e-12
                {
                    return Err(
                        "Imported arc requires a finite nonzero sweep of at most one revolution."
                            .into(),
                    );
                }
            }
            Ok(())
        }
        Curve::Nurbs(n) => {
            let count = n.control_points.len();
            if !(2..=100_000).contains(&count)
                || n.degree == 0
                || n.degree >= count
                || n.degree > 64
                || n.weights.len() != count
                || n.knots.len() != count + n.degree + 1
            {
                return Err("Invalid NURBS degree, control-point, weight or knot count.".into());
            }
            n.control_points.iter().try_for_each(|p| valid_point(*p))?;
            if n.weights
                .iter()
                .any(|v| !v.is_finite() || !(1e-12..=1e12).contains(v))
                || n.knots.iter().any(|v| !v.is_finite() || v.abs() > 1e12)
                || n.knots.windows(2).any(|w| w[0] > w[1])
                || n.knots[n.degree] >= n.knots[count]
            {
                return Err("Invalid NURBS weights or knot domain.".into());
            }
            for t in [0., 0.25, 0.5, 0.75, 1.] {
                valid_point(curve.point_at(t))?;
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_geometry_before_evaluation() {
        let mut record = GeometryRecord::from_value(
            &Value::Mesh(Arc::new(Mesh::box_mesh(&Plane::world_xy(), 2., 3., 4.))),
            "box".into(),
        )
        .unwrap();
        assert!(record.validate().is_ok());
        if let GeometryData::Mesh { mesh } = &mut record.geometry {
            mesh.indices[0][0] = u32::MAX;
        }
        assert!(record.validate().is_err());
        record.geometry = GeometryData::Point {
            point: Vec3::new(f64::NAN, 0., 0.),
        };
        assert!(record.validate().is_err());
        let mut n =
            mantis_kernel::NurbsCurve::from_points(&[Vec3::ZERO, Vec3::X], 1, false).unwrap();
        n.knots.clear();
        record.geometry = GeometryData::Curve {
            curve: Curve::Nurbs(n),
        };
        assert!(record.validate().is_err());
    }

    #[test]
    fn immutable_source_survives_serialization_and_mesh_has_normals() {
        let mut mesh = Mesh::box_mesh(&Plane::world_xy(), 2., 3., 4.);
        mesh.normals.clear();
        let record = GeometryRecord {
            name: "part".into(),
            layer: "solid".into(),
            geometry: GeometryData::Mesh { mesh },
            source: Some(GeometrySource {
                format: "ocp-brep".into(),
                data: "opaque".into(),
                preview_units: None,
            }),
        };
        let restored = GeometryRecord::from_json(&record.to_json().unwrap()).unwrap();
        assert_eq!(restored, record);
        let mesh = restored.value().as_mesh().unwrap();
        assert_eq!(mesh.normals.len(), mesh.positions.len());
        assert!((mesh.volume() - 24.).abs() < 1e-9);
    }
}
