# 🦗 MantisCAD

**Rust로 만든 초경량 파라메트릭 CAD — Rhino의 감각, Grasshopper의 두뇌, 블록체인의 협업.**

MantisCAD의 문서(document)는 3D 형상이 아니라 **"컴포넌트가 적용된 순서"** 그 자체입니다.
슬라이더를 추가하고, 원을 그리고, 로프트를 연결한 모든 편집이 `GraphOp`(그래프 연산)으로
기록되고, 이 연산들이 **서명된 해시체인 블록**에 담깁니다. 메시·정점 데이터는 체인에 절대
올라가지 않습니다 — 모든 피어가 op-log를 결정론적으로 리플레이해서 동일한 모델을 로컬에서
재생성합니다.

> 수십 MB짜리 메시 모델도 체인 위에서는 **몇 KB의 연산 기록**입니다.

```
┌─────────────────────────────────────────────────────────────┐
│  mantis-app     egui GUI: 3D 뷰포트 + 노드 에디터 (native+wasm) │
│  mantis-server  체인 동기화 HTTP 서버 + wasm 정적 호스팅          │
│  mantis-admin   프로젝트·초대키·백업을 관리하는 운영 CLI            │
│  mantis-cli     keygen / inspect / verify / replay / demo     │
├─────────────────────────────────────────────────────────────┤
│  mantis-protocol 프로젝트·ACL·동기화의 버전 고정 공용 계약            │
│  mantis-chain   GraphOp만 담는 sha256+ed25519 블록체인          │
│  mantis-graph   Grasshopper식 데이터플로 엔진, 63개 컴포넌트      │
│  mantis-kernel  기하 커널: NURBS·메시·extrude/revolve/loft/pipe │
└─────────────────────────────────────────────────────────────┘
```

## 왜 가벼운가

| | 일반 CAD 파일 | MantisCAD 체인 |
|---|---|---|
| 기록 대상 | 정점·면·NURBS 지오메트리 전체 | 컴포넌트 추가/연결/파라미터 변경 연산만 |
| 트위스트 타워 예시 | 수 MB (메시) | **수 KB (op 몇십 개)** |
| 협업 동기화 | 파일 전체 전송 | 새 블록만 전송 (git처럼) |
| 히스토리 | 없거나 별도 관리 | 체인 자체가 완전한 히스토리 (타임트래블 가능) |

결정론이 핵심 규약입니다: 평가 순서·직렬화·해시 경로에 `HashMap` 금지(`BTreeMap`/`Vec`만),
라이브러리 코드에 난수·시계 접근 금지, 노드 ID는 UI에서 생성되어 **op 안에 기록**됩니다.
블록 해시는 오직 연산+메타데이터만 커버하므로 플랫폼 간 부동소수점 미차가 체인을 포크시킬
수 없습니다 (지오메트리는 파생물일 뿐, 권위가 아닙니다).

## 아키텍처

- **mantis-kernel** — 순수 기하: `Vec3/Mat4/Plane`, 해석적 곡선(선/폴리라인/원/호) +
  유리 NURBS(de Boor, 주기적 닫힘 지원), 워터타이트 프리미티브(박스/구/실린더/콘/토러스),
  extrude(귀자르기 캡)/revolve/loft/pipe(평행이동 프레임)/planar surface, OBJ 내보내기.
- **mantis-graph** — `Component` 트레이트 + 레지스트리, 결정론적 위상정렬 평가기
  (더티 추적 캐시), Grasshopper의 longest-list 매칭, 63개 빌트인 컴포넌트
  (Params/Maths/Sets/Vector/Curve/Surface/Transform/Analysis).
- **mantis-chain** — `Block { index, prev_hash, timestamp, author, author_pk, message, ops, hash, sig }`.
  `hash = sha256(정규 JSON)`, `sig = ed25519(해시 원바이트)`. 검증은 해시 링크·서명·
  **전체 op 리플레이 가능성**까지 확인. fast-forward 병합(`try_extend`), 타임트래블 리플레이,
  공개 원장에 앵커링 가능한 검증 완료 head 체크포인트(`audit`).
- **mantis-protocol** — 프로젝트 manifest, scoped genesis, 서명된 접근권한 원장, 동기화 DTO,
  portable workspace를 앱·서버·관리 CLI·AI agent가 공유하는 버전 고정 계약.
- **mantis-app** — eframe/egui. glow 3D 뷰포트(궤도/팬/줌, z-up), 직접 구현한 노드 에디터
  (와이어 드래그, 검색 팔레트, 인라인 슬라이더, 커밋 전 Undo/Redo), 체인 패널
  (커밋/푸시/풀/타임트래블).
  네이티브와 브라우저(wasm) 동일 코드베이스.
- **mantis-server** — `tiny_http` 단일 바이너리: 공개 읽기·초대키 쓰기의 다중 프로젝트
  `/api/v2`, CAS 방식 Push, 서명된 ACL 원장, 요청 제한, 내구성 있는 원자적 저장,
  health/readiness, OpenAPI 3.1과 wasm 앱 정적 서빙. 기존 `/api/*` 단일 체인 모드도 유지.
- **mantis-admin** — 운영자/owner가 프로젝트 생성, writer 초대·회수, export·검증·복원,
  legacy 단일 체인 마이그레이션을 수행하는 CLI. 비밀키는 서버에 보관하지 않음.
- **mantis-cli** — 헤드리스: 키 생성, 컴포넌트 카탈로그, GraphOp 배치의 dry-run·평가·서명,
  그래프 JSON 관측, 감사 체크포인트, 리플레이→OBJ 내보내기.

## 협업 모델 (git과 닮음)

1. 편집하면 op가 **pending 목록**에 쌓이며 로컬 그래프에 즉시 적용됩니다.
2. **Commit** — pending ops를 ed25519 서명된 블록으로 봉인.
3. **Push** — 서버 head 위에 fast-forward로 얹음. 다른 사람이 먼저 푸시했다면:
4. **Pull** — 새 블록 검증·리플레이 후 내 pending ops를 재적용합니다. 충돌 op는 삭제하지 않고
   recovery 목록에 보존합니다.
5. 블록 슬라이더로 **과거 어느 시점의 모델이든 재생**할 수 있습니다.

개인 워크스페이스는 브라우저 IndexedDB 또는 네이티브 앱 데이터 디렉터리에 자동 저장됩니다.
자동 저장은 로컬 WIP만 보존하며 Commit/Push/Pull을 대신 실행하지 않습니다.
브라우저의 live signing identity는 브라우저/OS 프로필 보호에 의존하므로 공유 PC에서는 전용
프로필을 사용하고, 앱에서 암호화된 `.mantis-key` 백업을 별도로 보관하세요.

## 사용 방법

공개 웹 앱은 [domeido.asuscomm.com/mantis/](https://domeido.asuscomm.com/mantis/)에서 바로
실행할 수 있습니다. 동일한 OCI 이미지를 직접 호스팅하면 하나의 HTTPS 주소에서 브라우저 UI와
협업 API를 함께 사용할 수 있습니다. 브라우저의 개인 워크스페이스는 로컬에 자동 저장되고,
원격 변경은 명시적으로 Commit/Pull/Push합니다.

### GitHub Release

[Releases](https://github.com/tomeido/mantis-cad/releases)에서 Windows, Linux, macOS Intel,
macOS Apple Silicon용 preview 앱과 `SHA256SUMS`를 받을 수 있습니다. 첫 버전 태그가 발행되기
전에는 다운로드 항목이 없을 수 있습니다. 현재 preview 앱은 코드서명되지 않았으므로 체크섬을
검증해야 합니다.

### Docker로 로컬 웹 실행

첫 green `main` 빌드가 GHCR 이미지를 공개한 뒤에는 저장소를 복제하지 않고도 같은 웹 앱과
서버를 로컬에서 실행할 수 있습니다:

```bash
docker volume create mantis-data
docker run --detach --name mantis-cad \
  --init \
  --publish 127.0.0.1:7878:7878 \
  --read-only --tmpfs /tmp:rw,noexec,nosuid,size=64m \
  --cap-drop ALL --security-opt no-new-privileges:true \
  --mount source=mantis-data,target=/data \
  ghcr.io/tomeido/mantis-cad:latest
# → http://127.0.0.1:7878
```

원격 프로젝트 생성까지 하려면 위 컨테이너에 운영자 **공개키**만
`--env MANTIS_OPERATOR_KEYS=<64-hex-public-key>`로 전달하세요. 비밀키는 컨테이너에 넣지
않습니다. 아직 첫 이미지가 발행되지 않았다면 아래 Compose 빌드나 소스 실행 방식을 사용합니다.

저장소 checkout이 있는 경우 Compose로 동일한 보안 옵션과 설정 파일을 적용할 수 있습니다:

```bash
cp compose.env.example .env
docker compose up --detach
# → http://127.0.0.1:7878

# GitHub 이미지 대신 현재 checkout을 빌드
docker compose -f compose.yaml -f compose.build.yaml up --build
```

서버 데이터는 `mantis-cad_mantis-data` 볼륨에 남습니다. 프로젝트 생성·멤버 초대·백업과
자체 호스팅 설정은 [배포 및 운영 가이드](docs/DEPLOYMENT.md)를 참고하세요.

### 신뢰 가능한 백업 검증·복원

`mantis-admin`의 export 안에 든 값만 다시 신뢰 기준으로 사용하면, 내부적으로는 유효한 과거 포크를
최신 정본으로 오인할 수 있습니다. operator 공개키와 chain/access head는 반드시 별도의 서명된
백업 기록이나 배포 기록에서 가져오세요. `--operator-pk`에는 신뢰하는 키를 쉼표로 여러 개 전달할
수 있습니다.

```bash
# 권장 검증: 두 head가 모두 있어야 “canonical, externally anchored”로 판정
mantis-admin project verify \
  --file example-project.export.json \
  --operator-pk "<trusted-operator-pk-1>,<trusted-operator-pk-2>" \
  --expected-project example-project \
  --expected-genesis "<out-of-band-genesis-hash>" \
  --expected-head "<out-of-band-chain-head>" \
  --expected-access-head "<out-of-band-access-head>"

# 복원: operator 신뢰 기준과 두 head는 필수
mantis-admin project import \
  --server "https://<replacement-service.example>/mantis" \
  --file example-project.export.json \
  --operator-pk "<trusted-operator-pk-1>,<trusted-operator-pk-2>" \
  --expected-project example-project \
  --expected-genesis "<out-of-band-genesis-hash>" \
  --expected-head "<out-of-band-chain-head>" \
  --expected-access-head "<out-of-band-access-head>"
```

`project verify`는 `--operator-pk`가 필수입니다. 두 expected head 중 하나라도 빠지면 무결성과
operator 신뢰만 확인하고 외부 앵커가 없는 상태라는 경고를 냅니다. `--expected-project`와
`--expected-genesis`는 선택 사항이지만 복원 대상을 더 엄격히 고정하므로 함께 쓰는 것을 권장합니다.

### 소스에서 실행

```bash
# 네이티브 GUI
cargo run --locked --release -p mantis-app

# 브라우저 번들 + 다중 프로젝트 서버
cargo install --locked --version 0.21.14 trunk
(cd crates/mantis-app && \
  trunk build index.html --release --locked --dist ../../dist --public-url /)
MANTIS_DATA_DIR=./mantis-data MANTIS_DIST_DIR=./dist \
  cargo run --locked --release -p mantis-server
# → http://127.0.0.1:7878

# 기존 단일 체인 호환 모드
cargo run --locked --release -p mantis-server -- \
  --port 7878 --chain mantis-chain.json

# 헤드리스 데모: 트위스트 타워 체인 생성 → 검증 → OBJ로 리플레이
cargo run --locked -p mantis-cli -- demo --out demo-chain.json
cargo run --locked -p mantis-cli -- verify demo-chain.json
cargo run --locked -p mantis-cli -- replay demo-chain.json --obj tower.obj
```

## AI agent 편집 프로토콜

에이전트 전용 우회 API를 만들지 않고 GUI와 동일한 `GraphOp`를 사용합니다. 따라서 사람이 만든
모델을 에이전트가 이어서 편집하거나, 에이전트의 결과를 사람이 노드 그래프에서 그대로 확인할 수
있습니다.

```bash
cargo run -p mantis-cli -- init model.mantis.json
cargo run -p mantis-cli -- keygen --name agent-1 --out agent.identity.json
cargo run -p mantis-cli -- catalog --json > catalog.json
cargo run -p mantis-cli -- graph model.mantis.json --json > graph.json

# edit.ops.json을 만든 뒤, 쓰지 않고 전체 검증
cargo run -p mantis-cli -- apply model.mantis.json --ops edit.ops.json \
  --identity agent.identity.json --message "primary mass" --dry-run

# 동일 배치를 원자적으로 서명·커밋
cargo run -p mantis-cli -- apply model.mantis.json --ops edit.ops.json \
  --identity agent.identity.json --message "primary mass"
cargo run -p mantis-cli -- audit model.mantis.json
```

`apply`는 포트 범위·타입, 파라미터 이름·타입, 그래프 구조와 최종 평가까지 통과한 경우에만 한
블록으로 봉인합니다. 검증 실패는 기존 파일을 바꾸지 않으며, 저장 중 프로세스가 중단되어도
원자적 교체 덕분에 이전 체인 또는 완성된 새 체인 중 하나만 남고 부분 JSON은 노출되지 않습니다.
전체 JSON 계약과 예제는 [AGENT_PROTOCOL.md](AGENT_PROTOCOL.md)에 있습니다.

빌드 도구 버전은 `rust-toolchain.toml`과 워크플로에 고정되어 있습니다. Pull request에서는 native
테스트·clippy·wasm 빌드·컨테이너 smoke test를 모두 실행합니다.

## 체인 포맷 (동결)

```jsonc
{
  "index": 1,
  "prev_hash": "9f2c…",            // sha256 링크
  "timestamp_ms": 1751871234567,
  "author": "alice",
  "author_pk": "3b7a…",            // ed25519 공개키 (hex)
  "message": "tower profile",
  "ops": [                          // ← 체인에 실리는 유일한 데이터
    {"op":"AddNode","id":"…32hex…","type_name":"circle","pos":[120.0,80.0]},
    {"op":"Connect","from":["…",0],"to":["…",0]},
    {"op":"SetParam","id":"…","key":"value","value":{"Number":3.5}}
  ],
  "hash": "…",                      // sha256(위 필드들의 정규 JSON)
  "sig": "…"                        // ed25519(hash 원바이트)
}
```

## 상태

- ✅ kernel · graph · chain · protocol · app · server · admin · cli 워크스페이스 검증
  (정확한 최신 테스트 수와 빌드 결과는 CI가 기준)
- ✅ wasm 및 네이티브 동시 컴파일
- ✅ e2e 협업 검증: 푸시 / 멱등 재푸시 / 포크 409 거부 / 변조 무시 /
  검증 오류 422 / 저장 전 실패 500 롤백 / 저장 후 불확실 500 fail-stop /
  페이지 pull / 감사 체크포인트 /
  경로탐색 400 / 바이트 단위 결정론적 리플레이
- ✅ 에이전트 편집 검증: 카탈로그 발견 / 현재 그래프 관측 / dry-run / 잘못된 포트·파라미터
  거부 / 평가 / Ed25519 서명 / 원자적 파일 교체 / provenance audit
- ✅ 인간 편집 검증: 동작 단위 Undo/Redo / 다중 선택 이동·삭제 그룹화 / 커밋 불변 경계
- ✅ 적대적 멀티렌즈 리뷰 통과 — 확정 결함 수정 완료:
  - **체인 무결성**: 비유한(NaN/±Inf) op이 해시를 충돌시키고 체인을 재로드
    불능으로 만들던 결함 차단 (`ChainError::NonFinite`)
  - **타입 강제변환**: 점→평면 배선이 거부되던 문제 수정
  - **지오메트리 견고성**: 비유한 방향/축이 NaN 메시를 만들던 경로 차단
- 동봉: `examples/demo-chain.json` — 2인 협업 트위스트 타워, 38 ops / 5 KB
  → 384정점 메시로 리플레이 (지오메트리 대비 5.3× 압축)

---

### English TL;DR

MantisCAD is a featherweight Rhino-like parametric CAD in Rust. The document
IS a Grasshopper-style node graph; every edit is a `GraphOp` sealed into
sha256-linked, ed25519-signed blocks — **never geometry**. Peers replay the
op-log deterministically to rebuild identical models, so a multi-megabyte
model syncs as kilobytes. Workspace: `mantis-kernel` (geometry),
`mantis-graph` (dataflow engine, 63 components), `mantis-chain` (op-log
blockchain), `mantis-protocol` (versioned project/access/sync contracts),
`mantis-app` (egui GUI, native+wasm), `mantis-server` (public-read,
invite-write multi-project sync + static hosting), `mantis-admin` (signed
project/member operations), and `mantis-cli` (headless replay/inspect/demo).
Run it online from one HTTPS origin, as a native GitHub Release, or locally
with the same GHCR image. MIT license.
