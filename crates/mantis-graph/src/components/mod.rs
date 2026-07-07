//! Built-in component library (graph-agent implements).
//!
//! Target set (~35, Grasshopper-inspired; type_name in parens is FROZEN once
//! shipped):
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
//!            dot · cross · amplitude · rotate_vector(axis,angle)
//! Plane:     xy_plane(origin) · plane_normal(origin,normal)
//! Curve:     line(a,b) · polyline(points,closed) · circle(plane,radius) ·
//!            arc(plane,radius,a0,a1) · nurbs_curve(points,degree,closed?) ·
//!            divide_curve(curve,n -> points) · eval_curve(curve,t -> point,
//!            tangent) · curve_length
//! Surface:   extrude(curve,dir) · revolve(curve,axis line?origin+dir,angle) ·
//!            loft(curves) · pipe(curve,radius) · planar_srf(curve) ·
//!            box_mesh(plane,x,y,z) · sphere(center,radius) ·
//!            cylinder(plane,radius,height) · cone · torus
//! Transform: move(geo,vector) · rotate(geo,plane-or-axis,angle) ·
//!            scale(geo,center,factor) · mirror(geo,plane)
//!            (geo: ValueKind::Any -> Vector/Curve/Mesh; Plane passes through
//!             transformed too)
//! Analysis:  bbox · area · volume · mesh_info(v,f,bytes) · data_size
//!
//! Every component: tessellation params where sensible get item_default
//! inputs (e.g. segments default 32).

use crate::component::Component;
use std::sync::Arc;

/// All built-ins, for Registry::standard().
pub fn all() -> Vec<Arc<dyn Component>> {
    todo!("graph-agent")
}
