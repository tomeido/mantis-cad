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
│  mantis-cli     keygen / inspect / verify / replay / demo     │
├─────────────────────────────────────────────────────────────┤
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
  **전체 op 리플레이 가능성**까지 확인. fast-forward 병합(`try_extend`), 타임트래블 리플레이.
- **mantis-app** — eframe/egui. glow 3D 뷰포트(궤도/팬/줌, z-up), 직접 구현한 노드 에디터
  (와이어 드래그, 검색 팔레트, 인라인 슬라이더), 체인 패널(커밋/푸시/풀/타임트래블).
  네이티브와 브라우저(wasm) 동일 코드베이스.
- **mantis-server** — `tiny_http` 단일 바이너리: `GET /api/info`, `GET /api/blocks?from=N`,
  `POST /api/blocks`(fast-forward만 수용, 분기 시 409), wasm 앱 정적 서빙.
- **mantis-cli** — 헤드리스: 키 생성, 체인 검사/검증, 리플레이→OBJ 내보내기, 데모 체인 생성.

## 협업 모델 (git과 닮음)

1. 편집하면 op가 **pending 목록**에 쌓이며 로컬 그래프에 즉시 적용됩니다.
2. **Commit** — pending ops를 ed25519 서명된 블록으로 봉인.
3. **Push** — 서버 head 위에 fast-forward로 얹음. 다른 사람이 먼저 푸시했다면:
4. **Pull** — 새 블록 검증·리플레이 후, 내 pending ops를 재적용(무효화된 것은 드롭 알림).
5. 블록 슬라이더로 **과거 어느 시점의 모델이든 재생**할 수 있습니다.

## 빌드 & 실행

```bash
# 네이티브 GUI
cargo run --release -p mantis-app

# 협업 서버 (체인 파일 자동 저장)
cargo run --release -p mantis-server -- --port 7878 --chain mantis-chain.json

# 브라우저 버전 (trunk 필요: cargo install trunk)
cd crates/mantis-app && trunk build --release
cargo run --release -p mantis-server -- --dist crates/mantis-app/dist
# → http://localhost:7878 접속

# 헤드리스 데모: 트위스트 타워 체인 생성 → 검증 → OBJ로 리플레이
cargo run -p mantis-cli -- demo --out demo-chain.json
cargo run -p mantis-cli -- verify demo-chain.json
cargo run -p mantis-cli -- replay demo-chain.json --obj tower.obj
```

> 이 저장소는 C 툴체인 없는 호스트에서도 개발할 수 있도록 도커 기반 빌드를 씁니다:
> `docker exec mantis-dev cargo test --workspace` (rust:1 컨테이너, `/src`에 바인드 마운트).

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
    {"op":"Connect","from":[["…"],0],"to":[["…"],0]},
    {"op":"SetParam","id":"…","key":"value","value":{"Number":3.5}}
  ],
  "hash": "…",                      // sha256(위 필드들의 정규 JSON)
  "sig": "…"                        // ed25519(hash 원바이트)
}
```

## 상태

- ✅ kernel · graph · chain: 구현 완료, 129개 테스트 통과
- 🔧 app(GUI) · server · cli: 구현/검증 진행 중
- 예정: 통합 e2e, 적대적 리뷰 패스, 데모 체인 동봉

---

### English TL;DR

MantisCAD is a featherweight Rhino-like parametric CAD in Rust. The document
IS a Grasshopper-style node graph; every edit is a `GraphOp` sealed into
sha256-linked, ed25519-signed blocks — **never geometry**. Peers replay the
op-log deterministically to rebuild identical models, so a multi-megabyte
model syncs as kilobytes. Workspace: `mantis-kernel` (geometry),
`mantis-graph` (dataflow engine, 63 components), `mantis-chain` (op-log
blockchain), `mantis-app` (egui GUI, native+wasm), `mantis-server` (sync +
static hosting), `mantis-cli` (headless replay/inspect/demo). MIT license.
