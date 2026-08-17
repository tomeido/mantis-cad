# Deployment and operations

MantisCAD ships one OCI image containing the browser application,
`mantis-server`, and `mantis-admin`. The server serves the WASM bundle and API
from one origin. Only `/data` is mutable; it must be backed by durable storage.

Merging these files does not by itself create a public site. The repository
owner must publish the image and connect a self-hosted instance to an HTTPS
reverse proxy with durable storage.

## Local container

After the first green `main` workflow has published the public GHCR image, run
it without a source checkout and bind it only to the local loopback interface:

```bash
docker volume create mantis-data
docker run --detach --name mantis-cad \
  --init \
  --publish 127.0.0.1:7878:7878 \
  --read-only --tmpfs /tmp:rw,noexec,nosuid,size=64m \
  --cap-drop ALL --security-opt no-new-privileges:true \
  --mount source=mantis-data,target=/data \
  ghcr.io/tomeido/mantis-cad:latest
curl --fail http://127.0.0.1:7878/readyz
```

To allow project creation, add only an operator public key with
`--env MANTIS_OPERATOR_KEYS=<64-hex-public-key>`. Keep the operator secret and
encrypted key file on the administrator's machine.

With a source checkout, Compose applies the same hardened runtime defaults and
loads optional settings from `.env`:

```bash
cp compose.env.example .env
# Edit MANTIS_OPERATOR_KEYS in .env when remote project creation is required.
docker compose up --detach
docker compose ps
curl --fail http://127.0.0.1:7878/readyz
```

Open <http://127.0.0.1:7878>. Project data remains in the
`mantis-cad_mantis-data` named volume across container replacement. Set an
immutable image reference when reproducibility matters:

```bash
MANTIS_IMAGE=ghcr.io/tomeido/mantis-cad@sha256:<digest> docker compose up --detach
```

To build the same image from a checkout instead:

```bash
docker compose -f compose.yaml -f compose.build.yaml up --build
```

Use the `mantis-admin` binary from a GitHub Release, or run it from a checkout:

```bash
cargo run --locked --release -p mantis-admin -- --help
```

The OCI image also contains `mantis-admin`, but signing keys should normally
stay on the administrator's machine rather than being copied into the service.

Never delete the named volume during a routine upgrade. In particular, do not
use `docker compose down --volumes` unless the project history is intentionally
being destroyed and a verified backup exists.

## Create a project and invite writers

Create separate encrypted operator and owner identities. The commands prompt
for a passphrase without echo and refuse to overwrite an existing key file:

```bash
mantis-admin identity generate --name deployment-operator --out operator.mantis-key
mantis-admin identity generate --name project-owner --out owner.mantis-key
mantis-admin identity show --key operator.mantis-key
mantis-admin identity show --key owner.mantis-key
```

Put only the displayed operator public key in `MANTIS_OPERATOR_KEYS`, restart
the service, then create the project with the displayed owner public key:

```bash
mantis-admin project create \
  --server http://127.0.0.1:7878 \
  --id example-project \
  --title "Example Project" \
  --owner-pk <owner-public-key> \
  --operator-key operator.mantis-key
```

A collaborator generates their own identity and sends the owner only its
public key. Grant or revoke access with an owner key:

```bash
mantis-admin member add \
  --server http://127.0.0.1:7878 \
  --project example-project \
  --public-key <collaborator-public-key> \
  --role writer \
  --label "designer-1" \
  --admin-key owner.mantis-key

mantis-admin member list \
  --server http://127.0.0.1:7878 \
  --project example-project
```

The same commands work against the self-hosted HTTPS URL. Access changes are
owner-signed records in the project's public access ledger. Removing a member
blocks future writes by that key without rewriting historical authorship.

### Legacy single-chain migration

Stop the destination server before a filesystem migration, retain a copy of
the source, and target an otherwise unused project slug:

```bash
mantis-admin migrate-single-chain \
  --source mantis-chain.json \
  --data-dir ./mantis-data \
  --project legacy-project \
  --title "Legacy Project" \
  --owner-key owner.mantis-key
```

The command validates the entire v1 chain before writing v2 project files. It
does not grant future write access to every historical author. Add only the
writers that should retain access, then start the server with
`MANTIS_DATA_DIR=./mantis-data`.

## Self-hosted HTTPS setup

The checked-in `compose.selfhost.yaml` builds the browser app for a configurable
path prefix, keeps `/data` in a named volume, publishes no host port, and joins
an external reverse-proxy network. For the production `/mantis` deployment:

```bash
docker network create domeido-edge 2>/dev/null || true
MANTIS_EDGE_NETWORK=domeido-edge \
  MANTIS_WEB_BASE_PATH=/mantis \
  MANTIS_WEB_PUBLIC_URL=/mantis/ \
  docker compose -f compose.selfhost.yaml up --detach --build
```

Attach Caddy to the same external network and route the prefix while stripping
it before forwarding to `mantis-server`:

```caddyfile
@mantis_root path /mantis
handle @mantis_root {
    redir * /mantis/ 308
}
handle_path /mantis/* {
    reverse_proxy mantis:7878
}
```

Set `MANTIS_OPERATOR_KEYS` to a comma-separated list of operator **public**
keys when remote project creation is required. Never copy an identity secret
or `.mantis-key` into the service. Retain every historical operator public key
that signed an existing project creation proof; rotate by adding keys. Leave
`MANTIS_ALLOWED_ORIGINS` empty for same-origin UI/API hosting. A separate UI
must be listed by exact HTTPS origin; never use `*`.

The image supplies these defaults:

| Variable | Default | Purpose |
|---|---:|---|
| `MANTIS_DATA_DIR` | `/data` | Multi-project manifests, chains, and access ledgers |
| `MANTIS_DIST_DIR` | `/app/dist` | Read-only browser application |
| `MANTIS_PUBLIC_BASE_PATH` | empty | Reverse-proxy prefix advertised by OpenAPI |
| `MANTIS_OPERATOR_KEYS` | empty | Current and historical trusted project-creation public keys |
| `MANTIS_ALLOWED_ORIGINS` | empty | Same-origin only; exact extra browser origins |
| `MANTIS_MAX_PROJECT_BYTES` | `25165824` | Serialized chain quota per project (24 MiB) |

The configurable project value limits the serialized chain. Independently, a
complete signed project document and any HTTP request have a non-configurable
`33554432` byte (32 MiB) hard cap. The lower default leaves room for the
manifest, operator-signed creation proof, and signed access ledger, and the
server enforces both limits so every accepted export remains importable.

The service must stay at one replica while it uses the filesystem-backed
store. Horizontal scaling requires a shared transactional storage design and
is deliberately outside the current release. Expect a brief interruption on
container replacement; clients retain their local pending work and can retry.

## Release and deploy flow

Pull requests and `main` run formatting, lint, unit/integration tests, a WASM
release build, and a container smoke test. Both tag workflows call that same
reusable CI and stop before publishing if it fails. A `vX.Y.Z`
tag that exactly matches the workspace package version then:

1. publishes multi-architecture (`linux/amd64`, `linux/arm64`) GHCR tags, SBOM,
   and build provenance;
2. packages unsigned preview applications for Linux, macOS Intel, macOS Apple
   Silicon, and Windows, with `SHA256SUMS` in the GitHub Release.

Production deployment remains an explicit operator action. Record the published
image digest, rebuild the path-prefixed self-host image when needed, replace the
single container without deleting `/data`, and verify `/readyz` plus the public
OpenAPI document after every update.

Create an annotated release tag only from a green `main` commit:

```bash
workspace_version="$(python3 -c 'import tomllib; print(tomllib.load(open("Cargo.toml", "rb"))["workspace"]["package"]["version"])')"
release_tag="v${workspace_version}"
git tag -a "$release_tag" -m "MantisCAD ${release_tag}"
git push origin "$release_tag"
```

Preview desktop archives are not code-signed. Verify their checksum before use.
Code signing and automatic desktop updates require separate signing credentials
and are not part of this pipeline.

## Health, logs, and backups

- `GET /healthz` confirms that the process is serving requests.
- `GET /readyz` confirms that the data directory is writable and stored chains
  validate. Configure load balancers and deployment checks against this route.
- Server logs go to stdout/stderr and are available through `docker compose
  logs`. Do not log request bodies or secret-key files.
- Snapshot the Docker volume on a schedule. Periodically export each project
  and copy it to storage outside the host; an on-host snapshot is not the only
  backup.

If a write returns `persistence_uncertain`, the atomic candidate is already
visible but the final directory durability confirmation failed. The server
immediately makes `/readyz` fail and rejects later mutations with
`storage_not_ready`. Do not retry the write: remove the instance from service,
restart it against the same disk, and compare both heads and a full audit with
the independent checkpoint before restoring traffic.

For every known-good backup checkpoint, separately record the trusted operator
public key set, project ID, genesis hash, chain head, access-ledger head, running
image digest, and timestamp in a signed or write-protected out-of-band record.
The operator keys come from the deployment key register, and the heads come
from the trusted live checkpoint process. Never copy these trust inputs only
from the export being checked: a self-consistent authorized historical fork
can carry its own matching values.

Export, then verify it against that separate record. `--operator-pk` is always
required and accepts a comma-separated set, which is useful after key rotation.
`--expected-project` and `--expected-genesis` are optional additional anchors;
they are included here as the safer operational default:

```bash
mantis-admin project export \
  --server "https://<service.example>/mantis" \
  --project example-project \
  --out example-project.export.json

mantis-admin project verify \
  --file example-project.export.json \
  --operator-pk "<trusted-operator-pk-1>,<trusted-operator-pk-2>" \
  --expected-project example-project \
  --expected-genesis "<out-of-band-genesis-hash>" \
  --expected-head "<out-of-band-chain-head>" \
  --expected-access-head "<out-of-band-access-head>"
```

Verification without **both** `--expected-head` and `--expected-access-head`
checks internal integrity and operator trust but prints a warning and does not
claim the export is canonical and externally anchored. Supplying only one head
does not establish that claim.

An export includes the original operator-signed creation proof, complete chain,
and signed access ledger. To restore it into an empty staging or replacement
service, first add every required, independently trusted operator public key to
that service's `MANTIS_OPERATOR_KEYS`. Import always requires `--operator-pk`
and both out-of-band head anchors:

```bash
mantis-admin project import \
  --server "https://<replacement-service.example>/mantis" \
  --file example-project.export.json \
  --operator-pk "<trusted-operator-pk-1>,<trusted-operator-pk-2>" \
  --expected-project example-project \
  --expected-genesis "<out-of-band-genesis-hash>" \
  --expected-head "<out-of-band-chain-head>" \
  --expected-access-head "<out-of-band-access-head>"
```

Import re-verifies the complete bundle and refuses a conflicting project ID or
genesis. It does not require or transmit the original operator secret key.
`--expected-project` and `--expected-genesis` remain optional, but the operator
trust set and both expected heads are mandatory and must not be sourced solely
from fields embedded in the export.

Before a release that changes persisted metadata, restore the latest export to
a staging disk and verify its genesis, access-ledger head, chain head, and full
audit. Keep the previous container digest recorded with the backup.

## Recovery and rollback

Application rollback and data rollback are separate operations:

1. Stop writes and record `/readyz`, project ID, genesis, both project heads,
   trusted operator public keys, and the running image digest in the independent
   recovery record.
2. For an application-only regression, replace the container with the previous
   known-good image digest. Do not replace `/data`.
3. For storage corruption, preserve the affected disk, create a fresh disk,
   restore a volume snapshot or import the newest verified project exports,
   then run the full project audit before reopening writes.
4. Point clients at the recovered service only after trusted operator keys,
   project ID, genesis, access-ledger head, chain head, and audit results match
   the out-of-band backup record.

Never overwrite a suspect project in place before retaining a forensic copy.
