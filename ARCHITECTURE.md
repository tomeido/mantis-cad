# MantisCAD — Architecture Contract

A featherweight Rhino-like parametric CAD tool in Rust. Grasshopper-style node
graph is the *only* source of truth; collaboration happens by recording **graph
operations (GraphOps) on a hash-linked, signed chain** — never geometry. Any
peer replays the op-log deterministically to reconstruct the identical model,
so a multi-megabyte model syncs as a few kilobytes of ops.

```
crates/
  mantis-kernel   pure geometry: Vec3/Mat4/Plane, Curve, Mesh, extrude/revolve/loft/pipe
  mantis-graph    node engine: Value, Component registry, Graph, GraphOp, deterministic eval
  mantis-chain    op-log blockchain: Block{ops}, sha256 links, ed25519 sigs, replay -> Graph
  mantis-app      eframe GUI: glow 3D viewport + hand-rolled node editor + chain panel (native+wasm)
  mantis-server   tiny_http: chain sync API + static hosting of the wasm build
  mantis-cli      headless: keygen / inspect / replay / export OBJ / demo chain generator
```

Dependency DAG: kernel ← graph ← chain ← {app, server, cli}. Lower crates MUST
NOT depend on higher ones. kernel/graph/chain must compile for wasm32 (no
threads, no std::net, no filesystem in library code paths).

## Iron rules (determinism)

1. **The chain records only `GraphOp`s** — component insertions, connections,
   parameter changes. Never meshes, never vertices.
2. Replay must be bit-identical in *graph structure* on every platform:
   - No `HashMap`/`HashSet` in any path that affects evaluation order,
     serialization, or hashing. Use `BTreeMap`/`Vec`.
   - No randomness and no clock reads inside kernel/graph/chain evaluation.
     `NodeId`s are generated at the UI edge and *recorded inside the op*.
   - Topological evaluation order ties broken by ascending `NodeId`.
3. Block hashes cover ops + metadata only, so cross-platform floating-point
   drift can never fork the chain (geometry is derived, not authoritative).
4. `serde_json` serialization of ops must round-trip losslessly. All public
   graph/chain types derive `Serialize + Deserialize + Clone + Debug + PartialEq`.

## Cross-crate API contract

The stub sources in each crate are the authoritative signatures. Implement
bodies; do **not** change existing public signatures (adding new items is fine).

### Data model summary

- `Value` (graph): Null | Number(f64) | Bool | Text | Vector(Vec3) | Plane |
  Curve(Arc<Curve>) | Mesh(Arc<Mesh>) | List(Vec<Value>)
- Ports: `Access::Item` ports receiving a `List` are auto-mapped by the engine
  with Grasshopper "longest list" semantics; `Access::List` ports get the list whole.
- `GraphOp`: AddNode | RemoveNode | Connect | Disconnect | SetParam | MoveNode.
  `Graph::apply` validates and mutates; it is the ONLY mutation path.
- `Block`: index, prev_hash(hex sha256), timestamp_ms, author, author_pk(hex),
  message, ops, hash, sig. `hash = sha256(canonical json of signable fields)`,
  `sig = ed25519(hash bytes)`.
- Commit model is git-like: UI edits accumulate as pending ops applied live to
  the working graph; "Commit" seals them into a signed block; push/pull sync
  with the server; on divergence the client pulls, replays, and re-applies
  still-valid pending ops (invalid ones dropped with a warning).

### Server HTTP API (mantis-server)

```
GET  /api/info                -> {"len":N,"head":"<hex>"}
GET  /api/blocks?from=N       -> JSON array of blocks N..end
POST /api/blocks              -> body: JSON array of new blocks that must chain
                                 onto current head; 200 {"len":N} on success,
                                 409 + {"len":N,"head":..} if head moved
GET  /<path>                  -> static files from --dist dir (wasm app), / -> index.html
```
All responses `Access-Control-Allow-Origin: *`.

## Build environment

Host has no C toolchain; **all cargo commands run inside the `mantis-dev`
docker container** (rust:1 with wasm32 target added, project bind-mounted at /src):

```
docker exec mantis-dev cargo build --workspace
docker exec mantis-dev cargo test -p mantis-kernel
```

GUI conventions: viewport draws all `Mesh`/`Curve`/`Vector` outputs of every
node whose `preview` flag is on (param key `"__preview"`, default true).
Sliders/params edited in the node editor emit one coalesced `SetParam` on
release, `MoveNode` coalesced on drag end.
