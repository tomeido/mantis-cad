//! 3D viewport: z-up CAD camera, scene collection from eval outputs, and a
//! small glow renderer driven through an egui paint callback.
//!
//! Camera/projection math is written with `mantis_kernel::Mat4` (f64) and
//! converted to f32 column-major arrays right before upload.

use crate::state::Document;
use mantis_graph::{EvalOutput, Graph, NodeId, Value};
use mantis_kernel::{BBox, Curve, Mat4, Mesh, Vec3};
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// colors
// ---------------------------------------------------------------------------

const BG: [f32; 4] = [0.110, 0.122, 0.141, 1.0]; // #1c1f24
const MESH_COLOR: [f32; 4] = [0.490, 0.624, 0.769, 1.0]; // #7d9fc4 steel blue
const SELECTED_COLOR: [f32; 4] = [0.910, 0.588, 0.235, 1.0]; // #e8963c orange
const CURVE_COLOR: [f32; 4] = [0.784, 0.804, 0.831, 1.0]; // #c8cdd4 light
const POINT_COLOR: [f32; 4] = [0.941, 0.784, 0.431, 1.0]; // #f0c86e
const GRID_COLOR: [f32; 4] = [0.227, 0.247, 0.278, 1.0]; // #3a3f47
const AXIS_X: [f32; 4] = [0.898, 0.325, 0.239, 1.0];
const AXIS_Y: [f32; 4] = [0.325, 0.769, 0.329, 1.0];
const AXIS_Z: [f32; 4] = [0.271, 0.463, 0.910, 1.0];

const FOVY: f64 = 45.0 * std::f64::consts::PI / 180.0;
const ZNEAR: f64 = 0.05;
const ZFAR: f64 = 500.0;
/// Segments used to tessellate nonlinear preview curves.
const CURVE_SEGS: usize = 96;

/// Surface inspection is a display setting; it never changes the document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum SurfaceDisplay {
    #[default]
    Shaded,
    Zebra,
}

const ZEBRA_HELP: &str = "Inspect reflected stripes on the preview mesh. Orbit to inspect different directions; adjust density for broad or local changes. Tessellation and vertex normals affect the result. This preview does not certify analytic G1/G2 or Class-A surface continuity.";

// ---------------------------------------------------------------------------
// camera math (pure — unit tested)
// ---------------------------------------------------------------------------

/// Right-handed perspective projection (OpenGL clip space, -1..1 depth).
pub fn perspective(fovy: f64, aspect: f64, znear: f64, zfar: f64) -> Mat4 {
    let f = 1.0 / (fovy * 0.5).tan();
    let aspect = if aspect.abs() < 1e-9 { 1.0 } else { aspect };
    let mut m = Mat4([[0.0; 4]; 4]);
    m.0[0][0] = f / aspect;
    m.0[1][1] = f;
    m.0[2][2] = (zfar + znear) / (znear - zfar);
    m.0[2][3] = -1.0;
    m.0[3][2] = 2.0 * zfar * znear / (znear - zfar);
    m
}

/// Right-handed look-at view matrix.
pub fn look_at(eye: Vec3, target: Vec3, up: Vec3) -> Mat4 {
    let f = (target - eye).normalized();
    let mut s = f.cross(up).normalized();
    if s == Vec3::ZERO {
        // Looking straight along `up`: pick a stable side vector.
        s = f.cross(Vec3::X).normalized();
        if s == Vec3::ZERO {
            s = f.cross(Vec3::Y).normalized();
        }
    }
    let u = s.cross(f);
    let mut m = Mat4::identity();
    m.0[0][0] = s.x;
    m.0[1][0] = s.y;
    m.0[2][0] = s.z;
    m.0[0][1] = u.x;
    m.0[1][1] = u.y;
    m.0[2][1] = u.z;
    m.0[0][2] = -f.x;
    m.0[1][2] = -f.y;
    m.0[2][2] = -f.z;
    m.0[3][0] = -s.dot(eye);
    m.0[3][1] = -u.dot(eye);
    m.0[3][2] = f.dot(eye);
    m
}

/// Column-major f32 array for GL upload.
pub fn mat4_to_f32(m: &Mat4) -> [f32; 16] {
    let mut out = [0.0f32; 16];
    for c in 0..4 {
        for r in 0..4 {
            out[c * 4 + r] = m.0[c][r] as f32;
        }
    }
    out
}

/// Z-up orbit camera (spherical coordinates around a target point).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Camera {
    pub target: Vec3,
    pub distance: f64,
    /// Radians around +Z, 0 = looking from +X.
    pub yaw: f64,
    /// Radians above the XY plane, clamped to just under ±90°.
    pub pitch: f64,
}

impl Default for Camera {
    fn default() -> Self {
        Camera {
            target: Vec3::ZERO,
            distance: 14.0,
            yaw: 0.9,
            pitch: 0.55,
        }
    }
}

impl Camera {
    const MAX_PITCH: f64 = 1.55; // < pi/2, keeps look_at well-conditioned

    /// Eye position in world space.
    pub fn eye(&self) -> Vec3 {
        let (sp, cp) = self.pitch.sin_cos();
        let (sy, cy) = self.yaw.sin_cos();
        self.target + Vec3::new(cp * cy, cp * sy, sp) * self.distance
    }

    /// Camera right vector (world space, horizontal).
    pub fn right(&self) -> Vec3 {
        let f = (self.target - self.eye()).normalized();
        let r = f.cross(Vec3::Z).normalized();
        if r == Vec3::ZERO {
            Vec3::new(-self.yaw.sin(), self.yaw.cos(), 0.0)
        } else {
            r
        }
    }

    /// Camera up vector (world space): right × forward.
    pub fn up(&self) -> Vec3 {
        let f = (self.target - self.eye()).normalized();
        self.right().cross(f).normalized()
    }

    pub fn orbit(&mut self, dx: f64, dy: f64) {
        self.yaw -= dx * 0.008;
        self.pitch = (self.pitch + dy * 0.008).clamp(-Self::MAX_PITCH, Self::MAX_PITCH);
    }

    /// Screen-space pan: `px_height` is the viewport height in points.
    pub fn pan(&mut self, dx: f64, dy: f64, px_height: f64) {
        let world_per_px = 2.0 * self.distance * (FOVY * 0.5).tan() / px_height.max(1.0);
        self.target =
            self.target - self.right() * (dx * world_per_px) + self.up() * (dy * world_per_px);
    }

    /// Exponential dolly from scroll.
    pub fn dolly(&mut self, scroll: f64) {
        self.distance = (self.distance * (-scroll * 0.0015).exp()).clamp(0.05, 400.0);
    }

    /// Frame the given bounding box (no-op target reset if empty).
    pub fn zoom_to_fit(&mut self, bbox: &BBox) {
        if bbox.is_empty() {
            self.target = Vec3::ZERO;
            self.distance = 14.0;
            return;
        }
        self.target = bbox.center();
        let radius = (bbox.diagonal() * 0.5).max(0.001);
        self.distance = (radius / (FOVY * 0.5).sin() * 1.15).clamp(0.1, 380.0);
    }

    /// Combined projection * view matrix for the given aspect ratio.
    pub fn view_proj(&self, aspect: f64) -> Mat4 {
        perspective(FOVY, aspect, ZNEAR, ZFAR) * look_at(self.eye(), self.target, Vec3::Z)
    }
}

// ---------------------------------------------------------------------------
// scene collection (pure — unit tested)
// ---------------------------------------------------------------------------

/// One previewable geometry item, tagged with its producing node.
#[derive(Debug, Clone)]
pub enum SceneGeom {
    Mesh(Arc<Mesh>),
    Curve(Arc<Curve>),
    Point(Vec3),
}

#[derive(Debug, Clone)]
pub struct SceneItem {
    pub node: NodeId,
    pub geom: SceneGeom,
}

/// Collect all drawable geometry from the eval outputs of preview-enabled
/// nodes, recursing into `List` values (bounded depth against pathology).
pub fn collect_scene(graph: &Graph, eval: &EvalOutput) -> Vec<SceneItem> {
    let mut items = Vec::new();
    for (id, node) in &graph.nodes {
        if !node.preview() {
            continue;
        }
        if let Some(outs) = eval.outputs.get(id) {
            for v in outs {
                collect_value(*id, v, &mut items, 0);
            }
        }
    }
    items
}

fn collect_value(node: NodeId, v: &Value, items: &mut Vec<SceneItem>, depth: usize) {
    match v {
        Value::Mesh(m) => items.push(SceneItem {
            node,
            geom: SceneGeom::Mesh(m.clone()),
        }),
        Value::Curve(c) => items.push(SceneItem {
            node,
            geom: SceneGeom::Curve(c.clone()),
        }),
        Value::Vector(p) => items.push(SceneItem {
            node,
            geom: SceneGeom::Point(*p),
        }),
        Value::List(l) if depth < 8 => {
            for e in l {
                collect_value(node, e, items, depth + 1);
            }
        }
        _ => {}
    }
}

/// Approximate "what this would weigh as geometry" for the chain-vs-geometry
/// size comparison in the menu bar.
pub fn scene_byte_size(items: &[SceneItem]) -> usize {
    items
        .iter()
        .map(|i| match &i.geom {
            SceneGeom::Mesh(m) => m.approx_byte_size(),
            SceneGeom::Curve(c) => match c.as_ref() {
                Curve::Polyline { points, .. } => points.len() * 24,
                Curve::Nurbs(n) => n.control_points.len() * 32 + n.knots.len() * 8,
                _ => 64,
            },
            SceneGeom::Point(_) => 24,
        })
        .sum()
}

// ---------------------------------------------------------------------------
// CPU-side draw batches (pure — unit tested)
// ---------------------------------------------------------------------------

/// Primitive mode of a batch, mapped to GL constants at draw time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchMode {
    Triangles,
    Lines,
    Points,
}

/// Interleaved (pos.xyz, normal.xyz) vertex data + optional indices.
#[derive(Debug, Clone)]
pub struct CpuBatch {
    pub vertices: Vec<f32>,
    pub indices: Vec<u32>,
    pub mode: BatchMode,
    pub color: [f32; 4],
    pub shaded: bool,
    pub point_size: f32,
}

impl CpuBatch {
    fn new(mode: BatchMode, color: [f32; 4], shaded: bool) -> CpuBatch {
        CpuBatch {
            vertices: Vec::new(),
            indices: Vec::new(),
            mode,
            color,
            shaded,
            point_size: 7.0,
        }
    }
    pub fn vertex_count(&self) -> usize {
        self.vertices.len() / 6
    }
    fn push_vertex(&mut self, p: Vec3, n: Vec3) {
        self.vertices.extend_from_slice(&[
            p.x as f32, p.y as f32, p.z as f32, n.x as f32, n.y as f32, n.z as f32,
        ]);
    }
    fn append_mesh(&mut self, m: &Mesh) {
        let base = self.vertex_count() as u32;
        // Imported meshes can lack usable normals. Repair only those meshes:
        // recomputing every mesh would discard intentional creases / normals.
        let repaired;
        let normals = if m.normals.len() != m.positions.len()
            || m.normals
                .iter()
                .any(|n| !n.is_finite() || n.length_sq() < 1e-18)
        {
            let mut mesh = m.clone();
            mesh.recompute_normals();
            repaired = mesh.normals;
            &repaired
        } else {
            &m.normals
        };
        for (i, p) in m.positions.iter().enumerate() {
            let n = normals[i].normalized();
            let n = if n.is_finite() && n != Vec3::ZERO {
                n
            } else {
                Vec3::Z
            };
            self.push_vertex(*p, n);
        }
        let vcount = m.positions.len() as u32;
        for t in &m.indices {
            // Skip out-of-range indices defensively (degenerate meshes).
            if t[0] < vcount && t[1] < vcount && t[2] < vcount {
                self.indices
                    .extend_from_slice(&[base + t[0], base + t[1], base + t[2]]);
            }
        }
    }
    fn append_strip_as_lines(&mut self, pts: &[Vec3], closed: bool) {
        if pts.len() < 2 {
            return;
        }
        for w in pts.windows(2) {
            self.push_vertex(w[0], Vec3::ZERO);
            self.push_vertex(w[1], Vec3::ZERO);
        }
        if closed {
            self.push_vertex(pts[pts.len() - 1], Vec3::ZERO);
            self.push_vertex(pts[0], Vec3::ZERO);
        }
    }
}

/// Statistics gathered while building the scene batches.
#[derive(Debug, Clone, Copy)]
pub struct SceneStats {
    pub triangles: usize,
    pub vertices: usize,
    pub geometry_bytes: usize,
    pub bbox: BBox,
}

impl Default for SceneStats {
    fn default() -> Self {
        SceneStats {
            triangles: 0,
            vertices: 0,
            geometry_bytes: 0,
            bbox: BBox::EMPTY,
        }
    }
}

/// Turn scene items into GPU-ready batches; selected nodes' geometry gets the
/// highlight color.
pub fn build_batches(
    items: &[SceneItem],
    selection: &BTreeSet<NodeId>,
) -> (Vec<CpuBatch>, SceneStats) {
    let mut mesh_n = CpuBatch::new(BatchMode::Triangles, MESH_COLOR, true);
    let mut mesh_s = CpuBatch::new(BatchMode::Triangles, SELECTED_COLOR, true);
    let mut curve_n = CpuBatch::new(BatchMode::Lines, CURVE_COLOR, false);
    let mut curve_s = CpuBatch::new(BatchMode::Lines, SELECTED_COLOR, false);
    let mut pts_n = CpuBatch::new(BatchMode::Points, POINT_COLOR, false);
    let mut pts_s = CpuBatch::new(BatchMode::Points, SELECTED_COLOR, false);

    let mut stats = SceneStats {
        triangles: 0,
        vertices: 0,
        geometry_bytes: scene_byte_size(items),
        bbox: BBox::EMPTY,
    };

    for item in items {
        let sel = selection.contains(&item.node);
        match &item.geom {
            SceneGeom::Mesh(m) => {
                stats.triangles += m.triangle_count();
                stats.vertices += m.vertex_count();
                stats.bbox = stats.bbox.union(m.bbox());
                if sel { &mut mesh_s } else { &mut mesh_n }.append_mesh(m);
            }
            SceneGeom::Curve(c) => {
                // A line is already exact with two endpoints. This also keeps
                // curvature comb teeth from producing redundant subdivisions.
                let pts = match c.as_ref() {
                    Curve::Line { a, b } => vec![*a, *b],
                    _ => c.tessellate(CURVE_SEGS),
                };
                stats.vertices += pts.len();
                stats.bbox = stats.bbox.union(BBox::from_points(&pts));
                let closed = c.is_closed();
                if sel { &mut curve_s } else { &mut curve_n }.append_strip_as_lines(&pts, closed);
            }
            SceneGeom::Point(p) => {
                if p.is_finite() {
                    stats.vertices += 1;
                    stats.bbox.include(*p);
                    if sel { &mut pts_s } else { &mut pts_n }.push_vertex(*p, Vec3::ZERO);
                }
            }
        }
    }

    let batches = [mesh_n, mesh_s, curve_n, curve_s, pts_n, pts_s]
        .into_iter()
        .filter(|b| !b.vertices.is_empty())
        .collect();
    (batches, stats)
}

/// Static scenery: XY grid (20×20 cells of 1.0) + origin axes (2 units).
pub fn build_static_batches() -> Vec<CpuBatch> {
    let mut grid = CpuBatch::new(BatchMode::Lines, GRID_COLOR, false);
    let n = 10i32;
    for i in -n..=n {
        let f = i as f64;
        grid.push_vertex(Vec3::new(f, -(n as f64), 0.0), Vec3::ZERO);
        grid.push_vertex(Vec3::new(f, n as f64, 0.0), Vec3::ZERO);
        grid.push_vertex(Vec3::new(-(n as f64), f, 0.0), Vec3::ZERO);
        grid.push_vertex(Vec3::new(n as f64, f, 0.0), Vec3::ZERO);
    }
    let mut ax = CpuBatch::new(BatchMode::Lines, AXIS_X, false);
    ax.push_vertex(Vec3::ZERO, Vec3::ZERO);
    ax.push_vertex(Vec3::new(2.0, 0.0, 0.0), Vec3::ZERO);
    let mut ay = CpuBatch::new(BatchMode::Lines, AXIS_Y, false);
    ay.push_vertex(Vec3::ZERO, Vec3::ZERO);
    ay.push_vertex(Vec3::new(0.0, 2.0, 0.0), Vec3::ZERO);
    let mut az = CpuBatch::new(BatchMode::Lines, AXIS_Z, false);
    az.push_vertex(Vec3::ZERO, Vec3::ZERO);
    az.push_vertex(Vec3::new(0.0, 0.0, 2.0), Vec3::ZERO);
    vec![grid, ax, ay, az]
}

// ---------------------------------------------------------------------------
// glow renderer (GL thread only)
// ---------------------------------------------------------------------------

/// Data shared between the UI thread (which prepares batches) and the paint
/// callback (which owns all GL objects).
pub struct ViewportShared {
    renderer: Option<Renderer>,
    /// `Some` = new batches awaiting upload (UI thread sets, GL thread takes).
    pending: Option<Vec<CpuBatch>>,
}

impl ViewportShared {
    pub fn new() -> Arc<Mutex<ViewportShared>> {
        Arc::new(Mutex::new(ViewportShared {
            renderer: None,
            pending: None,
        }))
    }
}

struct GpuBatch {
    vao: glow::VertexArray,
    vbo: glow::Buffer,
    ebo: Option<glow::Buffer>,
    draw_count: i32,
    mode: u32,
    color: [f32; 4],
    shaded: bool,
    point_size: f32,
}

struct Renderer {
    program: glow::Program,
    u_mvp: Option<glow::UniformLocation>,
    u_color: Option<glow::UniformLocation>,
    u_light_dir: Option<glow::UniformLocation>,
    u_shaded: Option<glow::UniformLocation>,
    u_zebra: Option<glow::UniformLocation>,
    u_camera_eye: Option<glow::UniformLocation>,
    u_stripe_density: Option<glow::UniformLocation>,
    u_point_size: Option<glow::UniformLocation>,
    static_batches: Vec<GpuBatch>,
    scene_batches: Vec<GpuBatch>,
}

const VERT_SRC: &str = r#"
layout(location = 0) in vec3 a_pos;
layout(location = 1) in vec3 a_normal;
uniform mat4 u_mvp;
uniform float u_point_size;
out vec3 v_normal;
out vec3 v_position;
void main() {
    v_normal = a_normal;
    v_position = a_pos;
    gl_Position = u_mvp * vec4(a_pos, 1.0);
    gl_PointSize = u_point_size;
}
"#;

const FRAG_SRC: &str = r#"
in vec3 v_normal;
in vec3 v_position;
uniform vec4 u_color;
uniform vec3 u_light_dir;
uniform int u_shaded;
uniform int u_zebra;
uniform vec3 u_camera_eye;
uniform float u_stripe_density;
out vec4 frag_color;
void main() {
    if (u_shaded == 1) {
        // Smooth interpolated normals; a deterministic fallback also avoids
        // normalize(0) where opposing vertex normals cancel inside a face.
        vec3 n = dot(v_normal, v_normal) > 0.000001
            ? normalize(v_normal) : vec3(0.0, 0.0, 1.0);
        if (u_zebra == 1) {
            vec3 incident = normalize(v_position - u_camera_eye);
            vec3 reflection = reflect(incident, n);
            // A fixed world-space striped environment: highlights move with
            // the eye and reveal changes in surface normals, not screen UVs.
            float phase = dot(reflection, normalize(vec3(1.0, 0.35, 0.2)))
                * u_stripe_density * 3.14159265;
            float wave = sin(phase);
            float edge = max(fwidth(phase), 0.015);
            float stripe = smoothstep(-edge, edge, wave);
            vec3 light = mix(vec3(0.96), u_color.rgb, 0.25);
            frag_color = vec4(mix(vec3(0.025), light, stripe), u_color.a);
        } else {
            float ndl = abs(dot(n, normalize(u_light_dir)));
            float shade = 0.28 + 0.72 * ndl;
            frag_color = vec4(u_color.rgb * shade, u_color.a);
        }
    } else {
        frag_color = u_color;
    }
}
"#;

fn shader_prefix() -> &'static str {
    if cfg!(target_arch = "wasm32") {
        "#version 300 es\nprecision mediump float;\nprecision mediump int;\n"
    } else {
        "#version 330 core\n"
    }
}

impl Renderer {
    fn new(gl: &glow::Context) -> Result<Renderer, String> {
        use glow::HasContext as _;
        unsafe {
            let program = gl.create_program()?;
            let sources = [
                (
                    glow::VERTEX_SHADER,
                    format!("{}{}", shader_prefix(), VERT_SRC),
                ),
                (
                    glow::FRAGMENT_SHADER,
                    format!("{}{}", shader_prefix(), FRAG_SRC),
                ),
            ];
            let mut shaders = Vec::new();
            for (kind, src) in &sources {
                let shader = gl.create_shader(*kind)?;
                gl.shader_source(shader, src);
                gl.compile_shader(shader);
                if !gl.get_shader_compile_status(shader) {
                    let log = gl.get_shader_info_log(shader);
                    gl.delete_shader(shader);
                    return Err(format!("shader compile failed: {log}"));
                }
                gl.attach_shader(program, shader);
                shaders.push(shader);
            }
            gl.link_program(program);
            for s in shaders {
                gl.detach_shader(program, s);
                gl.delete_shader(s);
            }
            if !gl.get_program_link_status(program) {
                let log = gl.get_program_info_log(program);
                gl.delete_program(program);
                return Err(format!("program link failed: {log}"));
            }
            let mut r = Renderer {
                u_mvp: gl.get_uniform_location(program, "u_mvp"),
                u_color: gl.get_uniform_location(program, "u_color"),
                u_light_dir: gl.get_uniform_location(program, "u_light_dir"),
                u_shaded: gl.get_uniform_location(program, "u_shaded"),
                u_zebra: gl.get_uniform_location(program, "u_zebra"),
                u_camera_eye: gl.get_uniform_location(program, "u_camera_eye"),
                u_stripe_density: gl.get_uniform_location(program, "u_stripe_density"),
                u_point_size: gl.get_uniform_location(program, "u_point_size"),
                program,
                static_batches: Vec::new(),
                scene_batches: Vec::new(),
            };
            r.static_batches = upload_batches(gl, &build_static_batches())?;
            Ok(r)
        }
    }

    fn replace_scene(&mut self, gl: &glow::Context, batches: &[CpuBatch]) {
        delete_batches(gl, std::mem::take(&mut self.scene_batches));
        match upload_batches(gl, batches) {
            Ok(b) => self.scene_batches = b,
            Err(_) => self.scene_batches = Vec::new(),
        }
    }

    fn paint(
        &self,
        gl: &glow::Context,
        mvp: &[f32; 16],
        camera_eye: Vec3,
        surface_display: SurfaceDisplay,
        stripe_density: f32,
    ) {
        use glow::HasContext as _;
        unsafe {
            gl.clear_color(BG[0], BG[1], BG[2], BG[3]);
            gl.clear(glow::COLOR_BUFFER_BIT | glow::DEPTH_BUFFER_BIT);
            gl.enable(glow::DEPTH_TEST);
            gl.depth_func(glow::LESS);
            gl.disable(glow::CULL_FACE); // open surfaces exist
            #[cfg(not(target_arch = "wasm32"))]
            gl.enable(glow::PROGRAM_POINT_SIZE);
            gl.use_program(Some(self.program));
            gl.uniform_matrix_4_f32_slice(self.u_mvp.as_ref(), false, mvp);
            gl.uniform_3_f32(self.u_light_dir.as_ref(), 0.35, 0.25, 0.9);
            gl.uniform_1_i32(
                self.u_zebra.as_ref(),
                i32::from(surface_display == SurfaceDisplay::Zebra),
            );
            gl.uniform_3_f32(
                self.u_camera_eye.as_ref(),
                camera_eye.x as f32,
                camera_eye.y as f32,
                camera_eye.z as f32,
            );
            gl.uniform_1_f32(self.u_stripe_density.as_ref(), stripe_density);
            for b in self.static_batches.iter().chain(self.scene_batches.iter()) {
                gl.uniform_4_f32(
                    self.u_color.as_ref(),
                    b.color[0],
                    b.color[1],
                    b.color[2],
                    b.color[3],
                );
                gl.uniform_1_i32(self.u_shaded.as_ref(), if b.shaded { 1 } else { 0 });
                gl.uniform_1_f32(self.u_point_size.as_ref(), b.point_size);
                gl.bind_vertex_array(Some(b.vao));
                match b.ebo {
                    Some(_) => gl.draw_elements(b.mode, b.draw_count, glow::UNSIGNED_INT, 0),
                    None => gl.draw_arrays(b.mode, 0, b.draw_count),
                }
            }
            gl.bind_vertex_array(None);
            gl.disable(glow::DEPTH_TEST);
        }
    }

    fn destroy(&mut self, gl: &glow::Context) {
        use glow::HasContext as _;
        unsafe {
            gl.delete_program(self.program);
        }
        delete_batches(gl, std::mem::take(&mut self.static_batches));
        delete_batches(gl, std::mem::take(&mut self.scene_batches));
    }
}

fn delete_batches(gl: &glow::Context, batches: Vec<GpuBatch>) {
    use glow::HasContext as _;
    unsafe {
        for b in batches {
            gl.delete_vertex_array(b.vao);
            gl.delete_buffer(b.vbo);
            if let Some(ebo) = b.ebo {
                gl.delete_buffer(ebo);
            }
        }
    }
}

/// View an f32 slice as raw bytes for buffer upload.
fn as_bytes_f32(data: &[f32]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(data.as_ptr() as *const u8, data.len() * 4) }
}
fn as_bytes_u32(data: &[u32]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(data.as_ptr() as *const u8, data.len() * 4) }
}

fn upload_batches(gl: &glow::Context, batches: &[CpuBatch]) -> Result<Vec<GpuBatch>, String> {
    use glow::HasContext as _;
    let mut out = Vec::with_capacity(batches.len());
    for b in batches {
        if b.vertices.is_empty() {
            continue;
        }
        unsafe {
            let vao = gl.create_vertex_array()?;
            let vbo = gl.create_buffer()?;
            gl.bind_vertex_array(Some(vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            gl.buffer_data_u8_slice(
                glow::ARRAY_BUFFER,
                as_bytes_f32(&b.vertices),
                glow::STATIC_DRAW,
            );
            let stride = 6 * 4;
            gl.enable_vertex_attrib_array(0);
            gl.vertex_attrib_pointer_f32(0, 3, glow::FLOAT, false, stride, 0);
            gl.enable_vertex_attrib_array(1);
            gl.vertex_attrib_pointer_f32(1, 3, glow::FLOAT, false, stride, 3 * 4);
            let (ebo, draw_count) = if b.mode == BatchMode::Triangles && !b.indices.is_empty() {
                let ebo = gl.create_buffer()?;
                gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(ebo));
                gl.buffer_data_u8_slice(
                    glow::ELEMENT_ARRAY_BUFFER,
                    as_bytes_u32(&b.indices),
                    glow::STATIC_DRAW,
                );
                (Some(ebo), b.indices.len() as i32)
            } else {
                (None, b.vertex_count() as i32)
            };
            gl.bind_vertex_array(None);
            gl.bind_buffer(glow::ARRAY_BUFFER, None);
            gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, None);
            out.push(GpuBatch {
                vao,
                vbo,
                ebo,
                draw_count,
                mode: match b.mode {
                    BatchMode::Triangles => glow::TRIANGLES,
                    BatchMode::Lines => glow::LINES,
                    BatchMode::Points => glow::POINTS,
                },
                color: b.color,
                shaded: b.shaded,
                point_size: b.point_size,
            });
        }
    }
    Ok(out)
}

/// Free all GL resources (called from `App::on_exit`).
pub fn destroy_gl(shared: &Arc<Mutex<ViewportShared>>, gl: &glow::Context) {
    if let Ok(mut s) = shared.lock() {
        if let Some(mut r) = s.renderer.take() {
            r.destroy(gl);
        }
    }
}

// ---------------------------------------------------------------------------
// egui integration
// ---------------------------------------------------------------------------

/// Per-app viewport state (camera + shared GL handle + cached stats).
pub struct ViewportPanel {
    pub camera: Camera,
    pub shared: Arc<Mutex<ViewportShared>>,
    pub stats: SceneStats,
    surface_display: SurfaceDisplay,
    stripe_density: f32,
}

impl ViewportPanel {
    pub fn new() -> ViewportPanel {
        ViewportPanel {
            camera: Camera::default(),
            shared: ViewportShared::new(),
            stats: SceneStats::default(),
            surface_display: SurfaceDisplay::default(),
            stripe_density: 14.0,
        }
    }

    /// Rebuild GPU batches from the current document state (call only when
    /// the scene actually changed).
    pub fn rebuild_scene(&mut self, doc: &Document, selection: &BTreeSet<NodeId>) {
        let items = collect_scene(doc.display_graph(), &doc.last_eval);
        let (batches, stats) = build_batches(&items, selection);
        self.stats = stats;
        if let Ok(mut s) = self.shared.lock() {
            s.pending = Some(batches);
        }
    }

    /// Draw the viewport into the given ui (fills the available rect).
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(&mut self.surface_display, SurfaceDisplay::Shaded, "Shaded");
            ui.selectable_value(&mut self.surface_display, SurfaceDisplay::Zebra, "Zebra")
                .on_hover_text(ZEBRA_HELP);
            if self.surface_display == SurfaceDisplay::Zebra {
                ui.add(egui::Slider::new(&mut self.stripe_density, 4.0..=40.0).text("Density"));
                ui.weak("Mesh preview").on_hover_text(ZEBRA_HELP);
            }
        });
        let rect = ui.available_rect_before_wrap();
        let response = ui.allocate_rect(rect, egui::Sense::click_and_drag());

        // --- interaction -------------------------------------------------
        let shift = ui.input(|i| i.modifiers.shift);
        if response.dragged_by(egui::PointerButton::Middle)
            || (response.dragged_by(egui::PointerButton::Primary) && shift)
        {
            let d = response.drag_delta();
            self.camera
                .pan(d.x as f64, d.y as f64, rect.height() as f64);
        } else if response.dragged_by(egui::PointerButton::Primary) {
            let d = response.drag_delta();
            self.camera.orbit(d.x as f64, d.y as f64);
        }
        if response.hovered() {
            let scroll = ui.input(|i| i.raw_scroll_delta.y);
            if scroll.abs() > 0.0 {
                self.camera.dolly(-scroll as f64);
            }
            let fit_key = ui.input(|i| i.key_pressed(egui::Key::F))
                && ui.ctx().memory(|m| m.focused().is_none());
            if response.double_clicked() || fit_key {
                let bbox = self.stats.bbox;
                self.camera.zoom_to_fit(&bbox);
            }
        }

        // --- paint callback ----------------------------------------------
        let aspect = (rect.width() / rect.height().max(1.0)) as f64;
        let mvp = mat4_to_f32(&self.camera.view_proj(aspect));
        let camera_eye = self.camera.eye();
        let surface_display = self.surface_display;
        let stripe_density = self.stripe_density;
        let shared = self.shared.clone();
        let callback = egui::PaintCallback {
            rect,
            callback: Arc::new(egui_glow::CallbackFn::new(move |info, painter| {
                let gl = painter.gl();
                let Ok(mut s) = shared.lock() else { return };
                if s.renderer.is_none() {
                    s.renderer = Renderer::new(gl).ok();
                }
                let pending = s.pending.take();
                let Some(r) = s.renderer.as_mut() else { return };
                if let Some(batches) = pending {
                    r.replace_scene(gl, &batches);
                }
                use glow::HasContext as _;
                let vp = info.viewport_in_pixels();
                unsafe {
                    gl.viewport(vp.left_px, vp.from_bottom_px, vp.width_px, vp.height_px);
                    gl.enable(glow::SCISSOR_TEST);
                    gl.scissor(vp.left_px, vp.from_bottom_px, vp.width_px, vp.height_px);
                }
                r.paint(gl, &mvp, camera_eye, surface_display, stripe_density);
            })),
        };
        ui.painter().add(callback);

        // --- overlay text ---------------------------------------------------
        let painter = ui.painter_at(rect);
        let overlay = format!(
            "{} tris · {} verts",
            self.stats.triangles, self.stats.vertices
        );
        painter.text(
            rect.left_top() + egui::vec2(8.0, 6.0),
            egui::Align2::LEFT_TOP,
            overlay,
            egui::FontId::proportional(12.0),
            egui::Color32::from_rgb(150, 158, 170),
        );
        painter.text(
            rect.left_bottom() + egui::vec2(8.0, -6.0),
            egui::Align2::LEFT_BOTTOM,
            "drag orbit · shift/middle-drag pan · scroll zoom · double-click / F fit",
            egui::FontId::proportional(11.0),
            egui::Color32::from_rgb(110, 118, 130),
        );
    }

    pub fn geometry_bytes(&self) -> usize {
        self.stats.geometry_bytes
    }
}

// ---------------------------------------------------------------------------
// tests (pure logic only — no GL)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use mantis_chain::Identity;
    use mantis_graph::{GraphOp, ParamValue};

    const EPS: f64 = 1e-9;

    #[test]
    fn camera_eye_positions() {
        let mut cam = Camera {
            target: Vec3::ZERO,
            distance: 5.0,
            yaw: 0.0,
            pitch: 0.0,
        };
        assert!((cam.eye() - Vec3::new(5.0, 0.0, 0.0)).length() < EPS);
        cam.yaw = std::f64::consts::FRAC_PI_2;
        assert!((cam.eye() - Vec3::new(0.0, 5.0, 0.0)).length() < EPS);
        cam.yaw = 0.0;
        cam.pitch = std::f64::consts::FRAC_PI_2;
        // pitch straight up: eye above target (z-up)
        assert!((cam.eye() - Vec3::new(0.0, 0.0, 5.0)).length() < EPS);
        // offset target carries through
        cam.target = Vec3::new(1.0, 2.0, 3.0);
        cam.pitch = 0.0;
        assert!((cam.eye() - Vec3::new(6.0, 2.0, 3.0)).length() < EPS);
    }

    #[test]
    fn camera_pitch_clamped() {
        let mut cam = Camera::default();
        for _ in 0..10_000 {
            cam.orbit(0.0, 100.0);
        }
        assert!(cam.pitch <= 1.55 + EPS);
        assert!(cam.eye().is_finite());
    }

    #[test]
    fn look_at_centers_target() {
        let eye = Vec3::new(10.0, -4.0, 6.0);
        let target = Vec3::new(1.0, 2.0, 3.0);
        let v = look_at(eye, target, Vec3::Z);
        let t = v.transform_point(target);
        // Target lies straight ahead on the -Z view axis.
        assert!(t.x.abs() < EPS, "{t:?}");
        assert!(t.y.abs() < EPS, "{t:?}");
        assert!((t.z + eye.distance(target)).abs() < 1e-9, "{t:?}");
        // Eye maps to origin.
        let e = v.transform_point(eye);
        assert!(e.length() < EPS);
    }

    #[test]
    fn perspective_projects_center_and_depth() {
        let p = perspective(FOVY, 1.5, 0.05, 500.0);
        // A point on the view axis stays centered in x/y after w-divide.
        let clip = p.transform_point(Vec3::new(0.0, 0.0, -10.0));
        assert!(clip.x.abs() < EPS && clip.y.abs() < EPS);
        // Near plane maps to -1, far to +1 (GL depth convention).
        let near = p.transform_point(Vec3::new(0.0, 0.0, -0.05));
        let far = p.transform_point(Vec3::new(0.0, 0.0, -500.0));
        assert!((near.z + 1.0).abs() < 1e-6, "{near:?}");
        assert!((far.z - 1.0).abs() < 1e-6, "{far:?}");
    }

    #[test]
    fn view_proj_keeps_target_on_axis() {
        let cam = Camera {
            target: Vec3::new(3.0, -2.0, 5.0),
            distance: 8.0,
            yaw: 1.1,
            pitch: 0.7,
        };
        let m = cam.view_proj(1.7);
        let c = m.transform_point(cam.target);
        assert!(c.x.abs() < 1e-9 && c.y.abs() < 1e-9, "{c:?}");
    }

    #[test]
    fn zoom_to_fit_contains_bbox() {
        let mut bb = BBox::EMPTY;
        bb.include(Vec3::new(-3.0, -1.0, 0.0));
        bb.include(Vec3::new(5.0, 7.0, 4.0));
        let mut cam = Camera::default();
        cam.zoom_to_fit(&bb);
        assert!((cam.target - bb.center()).length() < EPS);
        // Every corner projects inside the clip volume (x/y in [-1,1]).
        let m = cam.view_proj(1.0);
        for &x in &[bb.min.x, bb.max.x] {
            for &y in &[bb.min.y, bb.max.y] {
                for &z in &[bb.min.z, bb.max.z] {
                    let c = m.transform_point(Vec3::new(x, y, z));
                    assert!(c.x.abs() <= 1.0 + 1e-6 && c.y.abs() <= 1.0 + 1e-6, "{c:?}");
                }
            }
        }
        // Empty bbox resets to default framing without NaNs.
        cam.zoom_to_fit(&BBox::EMPTY);
        assert!(cam.eye().is_finite());
    }

    // -- scene collection --------------------------------------------------

    fn nid(n: u128) -> NodeId {
        NodeId(n)
    }

    #[test]
    fn collect_scene_recurses_lists_and_honors_preview() {
        let mut doc = Document::new(Identity::generate("t"));
        // slider(count=4) -> series -> point_xyz  ==> List of 4 Vectors
        doc.apply_op(GraphOp::AddNode {
            id: nid(1),
            type_name: "number_slider".into(),
            pos: (0.0, 0.0),
        })
        .unwrap();
        doc.set_param(nid(1), "value", ParamValue::Number(4.0))
            .unwrap();
        doc.apply_op(GraphOp::AddNode {
            id: nid(2),
            type_name: "series".into(),
            pos: (0.0, 0.0),
        })
        .unwrap();
        doc.apply_op(GraphOp::Connect {
            from: (nid(1), 0),
            to: (nid(2), 2),
        })
        .unwrap();
        doc.apply_op(GraphOp::AddNode {
            id: nid(3),
            type_name: "point_xyz".into(),
            pos: (0.0, 0.0),
        })
        .unwrap();
        doc.apply_op(GraphOp::Connect {
            from: (nid(2), 0),
            to: (nid(3), 0),
        })
        .unwrap();
        doc.evaluate();
        let items = collect_scene(doc.display_graph(), &doc.last_eval);
        // point_xyz previews 4 points; nothing else is geometric.
        let points: Vec<_> = items
            .iter()
            .filter(|i| matches!(i.geom, SceneGeom::Point(_)))
            .collect();
        assert_eq!(points.len(), 4);
        assert!(points.iter().all(|i| i.node == nid(3)));

        // Turning preview off removes the node's geometry.
        doc.set_param(nid(3), "__preview", ParamValue::Bool(false))
            .unwrap();
        doc.evaluate();
        let items = collect_scene(doc.display_graph(), &doc.last_eval);
        assert!(items.iter().all(|i| i.node != nid(3)));
    }

    #[test]
    fn collect_scene_includes_curves_and_meshes() {
        let mut doc = Document::new(Identity::generate("t"));
        doc.apply_op(GraphOp::AddNode {
            id: nid(1),
            type_name: "circle".into(),
            pos: (0.0, 0.0),
        })
        .unwrap();
        doc.apply_op(GraphOp::AddNode {
            id: nid(2),
            type_name: "sphere".into(),
            pos: (0.0, 0.0),
        })
        .unwrap();
        doc.evaluate();
        assert!(
            doc.last_eval.errors.is_empty(),
            "{:?}",
            doc.last_eval.errors
        );
        let items = collect_scene(doc.display_graph(), &doc.last_eval);
        assert!(items.iter().any(|i| matches!(i.geom, SceneGeom::Curve(_))));
        assert!(items.iter().any(|i| matches!(i.geom, SceneGeom::Mesh(_))));
        let bytes = scene_byte_size(&items);
        assert!(bytes > 1000, "sphere mesh should weigh in: {bytes}");
    }

    #[test]
    fn build_batches_splits_selection_and_counts() {
        let mesh = Arc::new(Mesh::box_mesh(
            &mantis_kernel::Plane::world_xy(),
            1.0,
            1.0,
            1.0,
        ));
        let items = vec![
            SceneItem {
                node: nid(1),
                geom: SceneGeom::Mesh(mesh.clone()),
            },
            SceneItem {
                node: nid(2),
                geom: SceneGeom::Mesh(mesh),
            },
            SceneItem {
                node: nid(2),
                geom: SceneGeom::Point(Vec3::ZERO),
            },
        ];
        let sel: BTreeSet<NodeId> = [nid(2)].into_iter().collect();
        let (batches, stats) = build_batches(&items, &sel);
        assert_eq!(stats.triangles, 24);
        // one normal mesh batch, one selected mesh batch, one selected point batch
        assert_eq!(batches.len(), 3);
        let tri_batches: Vec<_> = batches
            .iter()
            .filter(|b| b.mode == BatchMode::Triangles)
            .collect();
        assert_eq!(tri_batches.len(), 2);
        assert_ne!(tri_batches[0].color, tri_batches[1].color);
        assert!(!stats.bbox.is_empty());
        // Index integrity: all indices in range.
        for b in &batches {
            for &i in &b.indices {
                assert!((i as usize) < b.vertex_count());
            }
        }
    }

    #[test]
    fn mesh_batches_repair_missing_and_invalid_normals_for_reflections() {
        for normals in [
            vec![],
            vec![Vec3::ZERO; 3],
            vec![Vec3::Z],
            vec![Vec3::new(f64::NAN, 0.0, 0.0); 3],
        ] {
            let mesh = Mesh {
                positions: vec![Vec3::ZERO, Vec3::Y * 2.0, Vec3::Z * 3.0],
                normals,
                indices: vec![[0, 1, 2], [0, 1, 99]],
            };
            let original_normal_count = mesh.normals.len();
            let mut batch = CpuBatch::new(BatchMode::Triangles, MESH_COLOR, true);
            batch.append_mesh(&mesh);
            assert_eq!(batch.indices, [0, 1, 2]);
            for vertex in batch.vertices.chunks_exact(6) {
                // The YZ face reflects with +X; a default +Z normal would
                // disguise the actual surface orientation in zebra mode.
                assert_eq!(&vertex[3..], &[1.0, 0.0, 0.0]);
            }
            assert_eq!(mesh.normals.len(), original_normal_count);
        }
    }

    #[test]
    fn mesh_batches_preserve_supplied_normal_directions() {
        let mesh = Mesh {
            positions: vec![Vec3::ZERO, Vec3::X, Vec3::Y],
            normals: vec![Vec3::X * 2.0, Vec3::Y * 3.0, Vec3::Z * 4.0],
            indices: vec![[0, 1, 2]],
        };
        let mut batch = CpuBatch::new(BatchMode::Triangles, MESH_COLOR, true);
        batch.append_mesh(&mesh);
        let normals: Vec<_> = batch
            .vertices
            .chunks_exact(6)
            .map(|v| [v[3], v[4], v[5]])
            .collect();
        assert_eq!(normals, [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]);
    }

    #[test]
    fn line_curve_batches_use_only_exact_endpoints() {
        let items: Vec<_> = (0..32)
            .map(|i| SceneItem {
                node: nid(1),
                geom: SceneGeom::Curve(Arc::new(Curve::Line {
                    a: Vec3::new(i as f64, 0.0, 0.0),
                    b: Vec3::new(i as f64, 1.0, 2.0),
                })),
            })
            .collect();
        let (batches, stats) = build_batches(&items, &BTreeSet::new());
        assert_eq!(batches.len(), 1);
        let batch = &batches[0];
        assert_eq!(batch.mode, BatchMode::Lines);
        assert_eq!(batch.vertex_count(), 64);
        assert_eq!(stats.vertices, 64);
        assert_eq!(stats.bbox.min, Vec3::ZERO);
        assert_eq!(stats.bbox.max, Vec3::new(31.0, 1.0, 2.0));
        for (i, segment) in batch.vertices.chunks_exact(12).enumerate() {
            assert_eq!(&segment[..3], &[i as f32, 0.0, 0.0]);
            assert_eq!(&segment[6..9], &[i as f32, 1.0, 2.0]);
        }
    }

    #[test]
    fn curve_batches_wrap_closed_curves() {
        let circle = Arc::new(Curve::Circle {
            plane: mantis_kernel::Plane::world_xy(),
            radius: 1.0,
        });
        let items = vec![SceneItem {
            node: nid(1),
            geom: SceneGeom::Curve(circle),
        }];
        let (batches, _) = build_batches(&items, &BTreeSet::new());
        assert_eq!(batches.len(), 1);
        let b = &batches[0];
        assert_eq!(b.mode, BatchMode::Lines);
        assert!(!b.shaded, "curve overlays must bypass zebra shading");
        // closed curve with N sample points -> N segments -> 2N vertices
        assert_eq!(b.vertex_count(), CURVE_SEGS * 2);
    }

    #[test]
    fn static_batches_have_grid_and_axes() {
        let batches = build_static_batches();
        assert_eq!(batches.len(), 4);
        assert!(batches.iter().all(|b| !b.shaded));
        // grid: 21 lines each direction, 2 verts per line
        assert_eq!(batches[0].vertex_count(), 21 * 2 * 2);
        for b in &batches[1..] {
            assert_eq!(b.vertex_count(), 2);
        }
    }

    #[test]
    fn mat4_to_f32_is_column_major() {
        let m = Mat4::translation(Vec3::new(7.0, 8.0, 9.0));
        let a = mat4_to_f32(&m);
        assert_eq!(a[12], 7.0);
        assert_eq!(a[13], 8.0);
        assert_eq!(a[14], 9.0);
        assert_eq!(a[0], 1.0);
    }
}
