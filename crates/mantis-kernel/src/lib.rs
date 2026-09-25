//! mantis-kernel — featherweight geometry kernel for MantisCAD.
//!
//! Pure, deterministic geometry: no I/O, no clock, no randomness, no threads.
//! Everything here must compile for wasm32-unknown-unknown.

pub mod curve;
pub mod curve_quality;
pub mod math;
pub mod mesh;
pub mod ops;
pub mod solid;

pub use curve::{Curve, NurbsCurve};
pub use curve_quality::{ContinuityReport, ContinuityTolerance, CurvatureSample};
pub use math::{BBox, Mat4, Plane, Vec3};
pub use mesh::Mesh;
