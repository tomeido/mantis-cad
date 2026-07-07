//! Built-in component library.
//!
//! Full set (type_name in parens is FROZEN once shipped):
//! Params:    number_slider(min,max,step,value params) · bool_toggle(value) ·
//!            panel(shows input; param "text" when unwired) · point_xyz ·
//!            pi_const
//! Maths:     add · subtract · multiply · divide · power · modulo · negate ·
//!            sin · cos · sqrt · abs · min · max · remap
//!            (add/subtract/multiply polymorphic: Number±Number, Vector±Vector,
//!             Vector*Number ...)
//! Sets:      series(start,step,count) · range(a,b,steps) · list_item ·
//!            list_length · repeat
//! Vector:    vector_xyz · deconstruct_vector · unit_x/y/z(factor) · distance ·
//!            dot · cross · amplitude · rotate_vector(axis,angle) ·
//!            xy_plane(origin) · plane_normal(origin,normal)
//! Curve:     line(a,b) · polyline(points,closed) · circle(plane,radius) ·
//!            arc(plane,radius,a0,a1) · nurbs_curve(points,degree,closed) ·
//!            divide_curve(curve,n -> points) · eval_curve(curve,t -> point,
//!            tangent) · curve_length
//! Surface:   extrude(curve,dir) · revolve(curve,axis origin+dir,angle) ·
//!            loft(curves) · pipe(curve,radius) · planar_srf(curve) ·
//!            box_mesh(plane,x,y,z) · sphere(center,radius) ·
//!            cylinder(plane,radius,height) · cone · torus
//! Transform: move(geo,motion) · rotate(geo,plane,angle) ·
//!            scale(geo,center,factor) · mirror(geo,plane)
//!            (geo: ValueKind::Any -> Vector/Plane/Curve/Mesh)
//! Analysis:  bbox · area · volume · mesh_info(v,f,bytes) · data_size
//!
//! Tessellation params where sensible are item_default inputs (segments
//! default 32, clamped to at most 2048 to keep worst-case memory bounded).

mod analysis;
mod curves;
mod maths;
mod params;
mod sets;
mod surface;
mod transform;
mod util;
mod vectors;

use crate::component::{Component, PortSpec};
use crate::value::{ParamValue, Value};
use std::collections::BTreeMap;
use std::sync::Arc;

/// A built-in component described by plain (capture-free) function pointers —
/// keeps the library table-driven without macros; trivially `Send + Sync`.
pub(crate) struct FnComponent {
    pub(crate) type_name: &'static str,
    pub(crate) label: &'static str,
    pub(crate) category: &'static str,
    pub(crate) inputs: fn() -> Vec<PortSpec>,
    pub(crate) outputs: fn() -> Vec<PortSpec>,
    pub(crate) eval: fn(&[Value], &BTreeMap<String, ParamValue>) -> Result<Vec<Value>, String>,
}

impl Component for FnComponent {
    fn type_name(&self) -> &'static str {
        self.type_name
    }
    fn label(&self) -> &'static str {
        self.label
    }
    fn category(&self) -> &'static str {
        self.category
    }
    fn inputs(&self) -> Vec<PortSpec> {
        (self.inputs)()
    }
    fn outputs(&self) -> Vec<PortSpec> {
        (self.outputs)()
    }
    fn eval(
        &self,
        inputs: &[Value],
        params: &BTreeMap<String, ParamValue>,
    ) -> Result<Vec<Value>, String> {
        (self.eval)(inputs, params)
    }
}

/// All built-ins, for `Registry::standard()`.
pub fn all() -> Vec<Arc<dyn Component>> {
    let mut v: Vec<Arc<dyn Component>> = Vec::new();
    v.extend(params::all());
    v.extend(maths::all());
    v.extend(sets::all());
    v.extend(vectors::all());
    v.extend(curves::all());
    v.extend(surface::all());
    v.extend(transform::all());
    v.extend(analysis::all());
    v
}

#[cfg(test)]
mod tests;
