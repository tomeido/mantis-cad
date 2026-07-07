//! Shared input-extraction helpers for the built-in components.
//!
//! The engine has already type-checked `Access::Item` ports against their
//! declared `ValueKind`, so these mostly cannot fail through normal evaluation
//! — but components must never panic, so every helper returns a `Result`
//! (components can also be eval'd directly, e.g. from tests).

use crate::value::Value;
use mantis_kernel::{Curve, Mesh, Plane, Vec3};
use std::sync::Arc;

/// Hard ceiling for user-supplied element counts (series, repeat, divide...).
pub(crate) const MAX_COUNT: usize = 1_000_000;
/// Hard ceiling for tessellation segment counts.
pub(crate) const MAX_SEGMENTS: usize = 2048;

pub(crate) fn any<'a>(inputs: &'a [Value], i: usize, name: &str) -> Result<&'a Value, String> {
    inputs.get(i).ok_or_else(|| format!("input {name} missing"))
}

pub(crate) fn num(inputs: &[Value], i: usize, name: &str) -> Result<f64, String> {
    let v = any(inputs, i, name)?;
    v.as_number()
        .ok_or_else(|| format!("input {name}: expected Number, got {}", v.describe()))
}

/// Number that must be finite (no NaN/inf leaking into geometry).
pub(crate) fn finite(inputs: &[Value], i: usize, name: &str) -> Result<f64, String> {
    let v = num(inputs, i, name)?;
    if v.is_finite() {
        Ok(v)
    } else {
        Err(format!("input {name}: not a finite number"))
    }
}

/// Finite number that must be strictly positive (radii, lengths...).
pub(crate) fn positive(inputs: &[Value], i: usize, name: &str) -> Result<f64, String> {
    let v = finite(inputs, i, name)?;
    if v > 0.0 {
        Ok(v)
    } else {
        Err(format!("input {name}: must be > 0 (got {v})"))
    }
}

pub(crate) fn boolean(inputs: &[Value], i: usize, name: &str) -> Result<bool, String> {
    let v = any(inputs, i, name)?;
    v.as_bool()
        .ok_or_else(|| format!("input {name}: expected Bool, got {}", v.describe()))
}

pub(crate) fn vector(inputs: &[Value], i: usize, name: &str) -> Result<Vec3, String> {
    let v = any(inputs, i, name)?;
    let vec = v
        .as_vector()
        .ok_or_else(|| format!("input {name}: expected Vector, got {}", v.describe()))?;
    if vec.is_finite() {
        Ok(vec)
    } else {
        // A computed non-finite vector (e.g. an f64 overflow to ±inf upstream)
        // would seed NaN geometry; fail the node cleanly instead.
        Err(format!("input {name}: not a finite vector"))
    }
}

pub(crate) fn plane(inputs: &[Value], i: usize, name: &str) -> Result<Plane, String> {
    let v = any(inputs, i, name)?;
    let pl = v
        .as_plane()
        .ok_or_else(|| format!("input {name}: expected Plane, got {}", v.describe()))?;
    if pl.origin.is_finite() && pl.x_axis.is_finite() && pl.y_axis.is_finite() {
        Ok(pl)
    } else {
        Err(format!("input {name}: not a finite plane"))
    }
}

pub(crate) fn curve(inputs: &[Value], i: usize, name: &str) -> Result<Arc<Curve>, String> {
    let v = any(inputs, i, name)?;
    v.as_curve()
        .ok_or_else(|| format!("input {name}: expected Curve, got {}", v.describe()))
}

pub(crate) fn mesh(inputs: &[Value], i: usize, name: &str) -> Result<Arc<Mesh>, String> {
    let v = any(inputs, i, name)?;
    v.as_mesh()
        .ok_or_else(|| format!("input {name}: expected Mesh, got {}", v.describe()))
}

/// Whole-list access (engine wraps scalars on `Access::List` ports).
pub(crate) fn list<'a>(inputs: &'a [Value], i: usize, name: &str) -> Result<&'a [Value], String> {
    match any(inputs, i, name)? {
        Value::List(l) => Ok(l),
        v => Err(format!("input {name}: expected List, got {}", v.describe())),
    }
}

/// A list of vectors (each element checked, index named in errors).
pub(crate) fn vectors(inputs: &[Value], i: usize, name: &str) -> Result<Vec<Vec3>, String> {
    list(inputs, i, name)?
        .iter()
        .enumerate()
        .map(|(k, v)| {
            let vec = v.as_vector().ok_or_else(|| {
                format!("input {name}[{k}]: expected Vector, got {}", v.describe())
            })?;
            if vec.is_finite() {
                Ok(vec)
            } else {
                Err(format!("input {name}[{k}]: not a finite vector"))
            }
        })
        .collect()
}

/// A list of curves, cloned out of their Arcs (kernel ops take `&[Curve]`).
pub(crate) fn curves(inputs: &[Value], i: usize, name: &str) -> Result<Vec<Curve>, String> {
    list(inputs, i, name)?
        .iter()
        .enumerate()
        .map(|(k, v)| {
            v.as_curve().map(|c| (*c).clone()).ok_or_else(|| {
                format!("input {name}[{k}]: expected Curve, got {}", v.describe())
            })
        })
        .collect()
}

/// Non-negative integer count; negatives clamp to 0, values above `max` error
/// (guards against accidental multi-gigabyte allocations).
pub(crate) fn count(inputs: &[Value], i: usize, name: &str, max: usize) -> Result<usize, String> {
    let v = finite(inputs, i, name)?;
    let n = v.floor();
    if n < 0.0 {
        return Ok(0);
    }
    if n > max as f64 {
        return Err(format!("input {name}: {n} exceeds maximum {max}"));
    }
    Ok(n as usize)
}

/// Tessellation segment count clamped into `[min, MAX_SEGMENTS]`.
pub(crate) fn segments(inputs: &[Value], i: usize, name: &str, min: usize) -> Result<usize, String> {
    let v = finite(inputs, i, name)?;
    let lo = min.min(MAX_SEGMENTS) as f64;
    Ok(v.floor().clamp(lo, MAX_SEGMENTS as f64) as usize)
}
