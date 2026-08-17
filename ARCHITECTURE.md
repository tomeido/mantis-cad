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
  mantis-protocol versioned project/access/sync/workspace contracts shared by every client
  mantis-app      eframe GUI: glow 3D viewport + hand-rolled node editor + chain panel (native+wasm)
  mantis-server   tiny_http: chain sync/audit API + static hosting of the wasm build
  mantis-admin    operator/owner CLI for projects, membership, export, verify, and migration
  mantis-cli      headless: agent discovery/edit/audit + replay / export OBJ / demo
```

Dependency DAG: kernel ← graph ← chain ← protocol ← {app, server, admin}, with
`cli` using the lower graph/chain layers directly. Lower crates MUST NOT depend
on higher ones. kernel/graph/chain/protocol must compile for wasm32 (no threads,
no std::net, no filesystem in library code paths).

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
  still-valid pending ops. Ops that no longer apply are retained in an explicit
  recovery list rather than silently discarded.

### Server HTTP API (mantis-server)

```
GET  /api/v2/info             -> API/app/chain versions, git SHA, capabilities
GET  /api/v2/openapi.json     -> OpenAPI 3.1 discovery document
GET  /api/v2/projects         -> public project summaries
POST /api/v2/projects         -> operator-signed ProjectBootstrapV1
GET  /api/v2/projects/{id}/info
GET  /api/v2/projects/{id}/create
                              -> operator-signed creation proof
GET  /api/v2/projects/{id}/audit
GET  /api/v2/projects/{id}/blocks?from=N&limit=N
POST /api/v2/projects/{id}/blocks
                              -> PushRequestV2 {base_len,base_head,blocks};
                                 durable compare-and-swap append
GET  /api/v2/projects/{id}/access-log?from=N&limit=N
POST /api/v2/projects/{id}/access-log
                              -> owner-signed access records
GET  /healthz                 -> process liveness
GET  /readyz                  -> validated writable project storage
GET  /<path>                  -> static files from MANTIS_DIST_DIR
```

Reads are public. Project creation requires a trusted operator signature;
blocks require a currently allowlisted author; access changes require a current
owner signature. An HTTP success is returned only after validation and durable
persistence. If the atomic replacement is visible but its final directory sync
cannot be confirmed, the candidate remains visible, the server returns
`persistence_uncertain`, and every later mutation fail-stops until restart and
audit. Errors use one stable `{error:{code,message,...}}` envelope and include
the current head where conflict recovery needs it. Browser CORS is same-origin
by default, with exact additional origins configured by
`MANTIS_ALLOWED_ORIGINS`; wildcard CORS is not accepted in v2 mode.

`MANTIS_MAX_PROJECT_BYTES` defaults to a 24 MiB serialized-chain quota. A
separate, fixed 32 MiB cap covers the complete signed project document and HTTP
request; both are enforced so an accepted project can always be exported and
restored through the public bootstrap endpoint.

`--chain` retains the single-file v1 server. `MANTIS_DATA_DIR` selects v2
multi-project storage and also maps legacy `/api/info`, `/api/audit`, and
`/api/blocks` calls to the project named `default`. A process uses only one
storage mode even though data-dir mode exposes those compatibility aliases.

## Build and distribution

Rust and the wasm target are pinned in `rust-toolchain.toml`; Trunk is pinned by
the Dockerfile and workflows. Native development uses Cargo directly:

```
cargo build --locked --workspace
cargo test --locked --workspace
```

The production OCI image contains the static wasm bundle, `mantis-server`, and
`mantis-admin`, and runs as a non-root user. `compose.yaml` additionally makes
the root filesystem read-only and leaves only `/data` and a small `/tmp`
writable; `compose.build.yaml` replaces the GHCR image with a source build.
A self-hosted deployment runs the same image as a single instance and persists
only its `/data` volume. TLS terminates at the reverse proxy; the application
container does not publish a host port in the hardened edge-network setup.

GUI conventions: viewport draws all `Mesh`/`Curve`/`Vector` outputs of every
node whose `preview` flag is on (param key `"__preview"`, default true).
Sliders/params edited in the node editor emit one coalesced `SetParam` on
release, `MoveNode` coalesced on drag end. Pending user actions have snapshot-
based Undo/Redo; a signed commit clears that volatile history and is an immutable
undo boundary.

Agent conventions: `mantis-cli catalog --json` is the discoverable component
contract; `graph --json` is bounded perception (derived geometry summarized,
never dumped); and `apply` trials, validates, evaluates, signs, revalidates, and
atomically persists one operation batch. See `AGENT_PROTOCOL.md`.
