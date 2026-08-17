# MantisCAD agent protocol

MantisCAD gives humans and software agents the same editing primitive:
`GraphOp`. An agent does not upload a mesh or mutate an internal database. It
discovers node types, proposes a batch of graph operations, evaluates that
batch, and seals the accepted operations into the signed history.

The protocol is deliberately file- and JSON-first so it works from any agent
runtime without linking Rust code.

## Safe edit loop

```bash
# One-time setup
cargo run -p mantis-cli -- init model.mantis.json
cargo run -p mantis-cli -- keygen --name agent-1 --out agent-1.identity.json

# Perception: discover the exact component and port contract
cargo run -p mantis-cli -- catalog --json > component-catalog.json
cargo run -p mantis-cli -- graph model.mantis.json --json > current-graph.json

# Action: validate without writing, then commit the exact same batch
cargo run -p mantis-cli -- apply model.mantis.json \
  --ops edit.ops.json --identity agent-1.identity.json \
  --message "add the primary mass" --dry-run
cargo run -p mantis-cli -- apply model.mantis.json \
  --ops edit.ops.json --identity agent-1.identity.json \
  --message "add the primary mass"

# Verification
cargo run -p mantis-cli -- verify model.mantis.json
cargo run -p mantis-cli -- audit model.mantis.json
cargo run -p mantis-cli -- graph model.mantis.json --json
```

`apply` is all-or-nothing. It replays the current chain, trials every op on a
clone, checks component names, port ranges and types, evaluates the resulting
graph, signs one block, validates the full chain, and atomically replaces the
destination file. A rejected edit leaves the prior file intact. If the process
is interrupted during storage, the destination contains either the complete
prior chain or the complete candidate chain, never a partially written JSON
document; verify the head before deciding whether to retry.

By default, an evaluation error rejects the batch. `--allow-errors` exists for
an intentionally incomplete intermediate graph and should be used explicitly.
`--timestamp MS` makes fixtures and reproducible agent runs deterministic.
`keygen --out` refuses to overwrite a key and creates the identity file with
owner-only permissions on Unix. Treat that file as a secret; only the public
key belongs in blocks or server requests.

## Operation JSON

An ops file is either a JSON array or an object with an `ops` array. IDs are
unique 128-bit values encoded as exactly 32 lowercase hex characters. Port
indices come from `catalog --json`; never guess them.

```json
[
  {
    "op": "AddNode",
    "id": "10000000000000000000000000000001",
    "type_name": "sphere",
    "pos": [80.0, 80.0]
  },
  {
    "op": "AddNode",
    "id": "10000000000000000000000000000002",
    "type_name": "move",
    "pos": [320.0, 80.0]
  },
  {
    "op": "AddNode",
    "id": "10000000000000000000000000000003",
    "type_name": "unit_z",
    "pos": [80.0, 260.0]
  },
  {
    "op": "Connect",
    "from": ["10000000000000000000000000000001", 0],
    "to": ["10000000000000000000000000000002", 0]
  },
  {
    "op": "Connect",
    "from": ["10000000000000000000000000000003", 0],
    "to": ["10000000000000000000000000000002", 1]
  },
  {
    "op": "SetParam",
    "id": "10000000000000000000000000000001",
    "key": "__preview",
    "value": { "Bool": false }
  }
]
```

Persistent parameter values use externally tagged enum JSON:
`{"Number": 2.5}`, `{"Bool": true}`, or `{"Text": "note"}`. Most numerical
inputs are ports, not parameters; create a `number_slider` and connect it when
the value should remain visibly editable by a human.

The six mutation variants are `AddNode`, `RemoveNode`, `Connect`,
`Disconnect`, `SetParam`, and `MoveNode`. Exact examples for every variant are
included in the catalog response.

## Reading results

`graph --json` returns nodes in deterministic topological order, edges, the
materialized block hash, evaluation errors, and typed output values. Large
derived geometry is summarized:

```json
{
  "kind": "Mesh",
  "vertices": 561,
  "triangles": 1024,
  "area": 12.52,
  "volume": 4.16,
  "bbox": { "min": [-1, -1, -1], "max": [1, 1, 1] }
}
```

Lists report at most the first 64 items plus their total length and a
`truncated` flag. Mesh vertices are never returned through this perception
endpoint and never stored in a block. Use `replay --obj` only when an actual
exchange mesh is required.

`audit` returns the format version, genesis and head hashes, signed block and
operation counts, and per-public-key activity. The head hash is a compact
commitment suitable for anchoring in a public ledger; doing so later proves
that the complete ordered history existed no later than the anchor transaction.

## Human/agent handoff

Graph positions and `MoveNode` operations are part of history, so an agent
should place nodes in readable left-to-right groups and avoid unnecessary
layout churn. A human can open the same document, adjust sliders or wiring,
and commit through the GUI; the next agent sees those edits by replaying the
new head.

For collaborative servers, pushes remain fast-forward only. If the remote head
moves, pull and replay it, then re-plan the local ops rather than rewriting or
silently merging signed history.

The HTTP discovery/sync surface is also machine-oriented. Start with the
versioned contract rather than guessing routes or JSON fields:

- `GET /api/v2/openapi.json` — OpenAPI 3.1 operations, parameters, bodies,
  response envelopes and core schemas.
- `GET /api/v2/info` — API version, supported chain formats, capabilities,
  application version and build revision.
- `GET /api/v2/projects` — public active projects (`?include_archived=1` also
  returns archived projects).
- `GET /api/v2/projects/{project}/info` — immutable manifest, chain CAS state,
  and the replayed public owner/writer ledger.
- `GET /api/v2/projects/{project}/blocks?from=N&limit=N` — a bounded page with
  `next_from` and the observed chain state. Follow `next_from`; do not assume a
  project fits in one response.
- `POST /api/v2/projects/{project}/blocks` — compare-and-swap append of signed
  blocks. One request is bounded to 256 blocks, 50,000 operations and the
  documented request limit, so a long tail must be sent as consecutive CAS
  chunks.
- `GET /api/v2/projects/{project}/audit` — fully verified provenance and
  public-key activity checkpoint.
- `GET|POST /api/v2/projects/{project}/access-log` — paged public access
  history and owner-signed administration records.

A push body names the exact remote prefix on which the new signed tail was
built:

```json
{
  "base_len": 4,
  "base_head": "<64 lowercase hex characters>",
  "blocks": [
    {
      "index": 4,
      "prev_hash": "<same value as base_head>",
      "timestamp_ms": 1751871234567,
      "author": "agent-1",
      "author_pk": "<64 lowercase hex characters>",
      "message": "move the primary mass",
      "ops": [
        {
          "op": "MoveNode",
          "id": "10000000000000000000000000000001",
          "pos": [120.0, 80.0]
        }
      ],
      "hash": "<64 lowercase hex characters>",
      "sig": "<128 lowercase hex characters>"
    }
  ]
}
```

Fetch and validate project info, materialize all block pages as a local
`{"blocks":[...]}` chain, run `mantis-cli apply --dry-run`, then commit the
same batch locally and submit only the new tail. After every accepted chunk,
use the returned `len` and `head` as the next CAS base. A `409` response means
the base diverged and requires a fresh Pull/re-plan; `422` means the signed
input is invalid; `403` means the author key is not an active owner/writer;
and `500` with `persistence_failed` means the candidate was not published.
`500 persistence_uncertain` is different: the complete candidate is already
visible, but the final directory durability confirmation failed. Do **not**
blindly retry it; record the returned state, stop mutating, and have the
operator restart and audit storage. The server fail-stops subsequent writes
with `503 storage_not_ready` until that recovery. Every error uses a stable
`error.code`, human-readable `error.message`, and context fields when available.

Project creation and membership changes are separately operator/owner signed;
use `mantis-admin` for those workflows. Read access is public. Never send an
identity file, `.mantis-key`, password, or raw secret key to the server.

The unversioned `/api/info`, `/api/audit`, and `/api/blocks` routes remain only
for the legacy single-chain/default-project compatibility path. New agents
should use `/api/v2`.

## Rhino and Grasshopper correspondence

The interaction model follows public McNeel concepts, without taking a binary
dependency on Rhino:

- A signed Mantis block is the durable command/undo record analogue.
- `GraphOp` replay is the construction-history analogue.
- Component input access (`Item` or `List`) and longest-list matching mirror
  the first level of Grasshopper data matching.
- Typed, summarized outputs serve the same debugging role as Grasshopper
  panels and parameter viewers.
- The viewport and selection tools follow Rhino's construction-plane,
  orbit/pan/zoom, framing, and transform-widget conventions.

References:

- <https://developer.rhino3d.com/api/rhinocommon/rhino.rhinodoc>
- <https://developer.rhino3d.com/api/rhinocommon/rhino.commands>
- <https://developer.rhino3d.com/guides/grasshopper/>
- <https://developer.rhino3d.com/guides/grasshopper/the-why-and-how-of-data-trees/>

MantisCAD currently implements items and lists, not full path-addressed
Grasshopper data trees, and uses curves plus tessellated meshes rather than a
full tolerance-aware B-rep kernel. Those are intentional, visible capability
boundaries rather than claims of Rhino file or SDK compatibility.
