//! `demo` subcommand: builds a small two-author chain that tells the
//! MantisCAD collaboration story — alice commits the parametric tower
//! profile, bob extends the chain and lofts it into a twisted tower mesh.
//!
//! Every op is applied to a live `Graph` while being recorded, so node ids
//! and wiring are valid by construction; the sealed chain replays cleanly.

use mantis_chain::{Chain, Identity};
use mantis_graph::{Graph, GraphOp, NodeId, ParamValue};

/// Records ops while applying them to a working graph, so the demo chain
/// can never contain an op that does not replay.
struct OpRecorder {
    graph: Graph,
    ops: Vec<GraphOp>,
}

impl OpRecorder {
    fn new() -> OpRecorder {
        OpRecorder { graph: Graph::new(), ops: Vec::new() }
    }

    fn push(&mut self, op: GraphOp) -> Result<(), String> {
        self.graph
            .apply(&op)
            .map_err(|e| format!("demo op {} failed: {e}", op.describe()))?;
        self.ops.push(op);
        Ok(())
    }

    fn add(&mut self, id: u128, type_name: &str, pos: (f32, f32)) -> Result<(), String> {
        self.push(GraphOp::AddNode { id: NodeId(id), type_name: type_name.into(), pos })
    }

    fn set_num(&mut self, id: u128, key: &str, v: f64) -> Result<(), String> {
        self.push(GraphOp::SetParam {
            id: NodeId(id),
            key: key.into(),
            value: ParamValue::Number(v),
        })
    }

    fn set_text(&mut self, id: u128, key: &str, v: &str) -> Result<(), String> {
        self.push(GraphOp::SetParam {
            id: NodeId(id),
            key: key.into(),
            value: ParamValue::Text(v.into()),
        })
    }

    fn connect(&mut self, from: (u128, u16), to: (u128, u16)) -> Result<(), String> {
        self.push(GraphOp::Connect {
            from: (NodeId(from.0), from.1),
            to: (NodeId(to.0), to.1),
        })
    }

    /// Drain the ops accumulated since the last call (one block's worth).
    fn take_ops(&mut self) -> Vec<GraphOp> {
        std::mem::take(&mut self.ops)
    }
}

// Node ids (fixed for a deterministic demo document). The sequence number is
// placed in the TOP 32 bits so `NodeId`'s short display (first 8 hex chars)
// is distinct per node in `replay` output.
const fn demo_id(n: u128) -> u128 {
    n << 96
}
const RADIUS: u128 = demo_id(1);
const LEVELS: u128 = demo_id(2);
const SERIES: u128 = demo_id(3);
const CENTER: u128 = demo_id(4);
const PLANE: u128 = demo_id(5);
const CIRCLE: u128 = demo_id(6);
const TWIST: u128 = demo_id(7);
const ANGLES: u128 = demo_id(8);
const LIFTS: u128 = demo_id(9);
const MOVE: u128 = demo_id(10);
const ROTATE: u128 = demo_id(11);
const LOFT: u128 = demo_id(12);

/// Build the demo chain: genesis + alice's "tower profile" + bob's
/// "loft the tower". Pure — identities and timestamps are passed in.
///
/// The graph (12 nodes). The profile circle is offset from the tower axis
/// by its own radius (it passes through the axis), so the per-level twist
/// is visible in the lofted result:
/// ```text
/// radius ──┬───────────────────────────────────▶ circle.radius
///          └─▶ point_xyz.x ─▶ xy_plane.origin ─▶ circle.plane
/// levels ──▶ series.count ─┬─▶ unit_z.factor ──▶ move.motion
///                          └─▶ multiply.a ─────▶ rotate.angle
/// twist ───▶ multiply.b
/// circle ──▶ move ──▶ rotate ──▶ loft ──▶ twisted tower mesh
/// ```
pub fn build_demo_chain(
    alice: &Identity,
    bob: &Identity,
    t1_ms: u64,
    t2_ms: u64,
) -> Result<Chain, String> {
    let mut r = OpRecorder::new();

    // -- block 1 (alice): the parametric tower profile -----------------------
    r.add(RADIUS, "number_slider", (-460.0, -140.0))?;
    r.set_text(RADIUS, "label", "radius")?;
    r.set_num(RADIUS, "min", 0.5)?;
    r.set_num(RADIUS, "max", 8.0)?;
    r.set_num(RADIUS, "value", 3.0)?;

    r.add(LEVELS, "number_slider", (-460.0, -60.0))?;
    r.set_text(LEVELS, "label", "levels")?;
    r.set_num(LEVELS, "min", 2.0)?;
    r.set_num(LEVELS, "max", 40.0)?;
    r.set_num(LEVELS, "step", 1.0)?;
    r.set_num(LEVELS, "value", 12.0)?;

    // Z heights / level indices: [0, 1, ..., levels-1].
    r.add(SERIES, "series", (-260.0, -60.0))?;
    r.connect((LEVELS, 0), (SERIES, 2))?; // count

    // Profile circle sits off the tower axis (offset = radius) so the
    // per-level twist is visible in the loft.
    r.add(CENTER, "point_xyz", (-320.0, 40.0))?;
    r.connect((RADIUS, 0), (CENTER, 0))?; // x
    r.add(PLANE, "xy_plane", (-200.0, 40.0))?;
    r.connect((CENTER, 0), (PLANE, 0))?; // origin
    r.add(CIRCLE, "circle", (-100.0, 0.0))?;
    r.connect((PLANE, 0), (CIRCLE, 0))?; // plane
    r.connect((RADIUS, 0), (CIRCLE, 1))?; // radius

    let mut chain = Chain::new();
    chain
        .append(r.take_ops(), "tower profile", alice, t1_ms)
        .map_err(|e| format!("demo: sealing block 1 failed: {e}"))?;

    // -- block 2 (bob): stack, twist and loft ---------------------------------
    r.add(TWIST, "number_slider", (-460.0, 100.0))?;
    r.set_text(TWIST, "label", "twist/level (rad)")?;
    r.set_num(TWIST, "min", 0.0)?;
    r.set_num(TWIST, "max", 1.0)?;
    r.set_num(TWIST, "value", 0.18)?;

    // Per-level rotation angles: series * twist.
    r.add(ANGLES, "multiply", (-260.0, 120.0))?;
    r.connect((SERIES, 0), (ANGLES, 0))?;
    r.connect((TWIST, 0), (ANGLES, 1))?;

    // Per-level lift vectors: (0, 0, level).
    r.add(LIFTS, "unit_z", (-260.0, -140.0))?;
    r.connect((SERIES, 0), (LIFTS, 0))?; // factor

    // One circle per level, lifted then twisted around the world Z axis.
    r.add(MOVE, "move", (60.0, 0.0))?;
    r.connect((CIRCLE, 0), (MOVE, 0))?; // geometry
    r.connect((LIFTS, 0), (MOVE, 1))?; // motion (list -> maps per level)

    r.add(ROTATE, "rotate", (200.0, 0.0))?;
    r.connect((MOVE, 0), (ROTATE, 0))?; // geometry (list)
    r.connect((ANGLES, 0), (ROTATE, 2))?; // angle (list, zipped per level)

    r.add(LOFT, "loft", (340.0, 0.0))?;
    r.connect((ROTATE, 0), (LOFT, 0))?; // curves (whole list)

    chain
        .append(r.take_ops(), "loft the tower", bob, t2_ms)
        .map_err(|e| format!("demo: sealing block 2 failed: {e}"))?;

    Ok(chain)
}
