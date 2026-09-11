# MantisCAD 정의·형상 변경 이력 비교 MVP

이 문서는 서명된 MantisCAD 체인의 두 커밋을 비교해, “노드 정의에서 무엇이
바뀌었고 현재 평가기가 만든 형상에 어떤 차이가 있는가”를 확인하는 1주 MVP의 계약을
기록합니다. 서명된 `GraphOp` 체인이 여전히 유일한 권위 있는 상태이며, 형상 비교
결과는 파생 보고서입니다.

## 현재 아키텍처

| crate | 책임 |
|---|---|
| `mantis-kernel` | 곡선, NURBS, 테셀레이션 메시와 기하 연산 |
| `mantis-graph` | GH-style `Graph`, `GraphOp`, 컴포넌트 레지스트리와 결정론적 평가기 |
| `mantis-chain` | `GraphOp`를 담는 SHA-256/Ed25519 서명 체인, 검증·리플레이 |
| `mantis-history` | 두 committed revision의 구간 커밋, 그래프 순변화, 파생 preview 형상 비교 |
| `mantis-protocol` | 프로젝트·ACL·동기화·workspace의 버전 계약 |
| `mantis-app` | native/wasm GUI, 노드 편집기, 3D 뷰포트, commit/push/pull/time-travel |
| `mantis-server` | 다중 프로젝트 체인/ACL 동기화 API와 wasm 정적 호스팅 |
| `mantis-admin` | 프로젝트·멤버십·백업·복원 운영 CLI |
| `mantis-cli` | 헤드리스 편집·검증·리플레이·OBJ export와 `diff` 사용자 인터페이스 |

`diff`의 실행 경로는 다음과 같습니다.

```text
chain JSON
  -> 전체 해시 링크·서명·GraphOp 리플레이 검증
  -> from/to 블록까지 각각 리플레이
  -> 두 Graph 스냅샷의 노드·연결·파라미터·레이아웃 순변화 산출
  -> Registry::standard() + Evaluator로 두 스냅샷 재평가
  -> preview 점·곡선·메시 fingerprint/요약 비교
  -> 사람용 요약 또는 schema version 1 JSON
```

`HistoryDiff.commits`는 `(from, to]` 구간의 원본 연산을 보존하고,
`definition`은 양 끝점 스냅샷의 순변화를 보여줍니다. 따라서 중간에 추가했다가
삭제한 노드는 커밋 이력에는 남지만 순변화에는 나타나지 않을 수 있습니다.

## 실행·테스트 경로

```bash
# 실제 데모 체인의 alice profile → bob loft 비교
cargo run --locked -p mantis-cli -- \
  diff examples/demo-chain.json --from 1 --to 2

# 자동화용 JSON
cargo run --locked -p mantis-cli -- \
  diff examples/demo-chain.json --from 1 --to 2 --json

# 지정을 생략하면 head 직전 → head
cargo run --locked -p mantis-cli -- diff examples/demo-chain.json

# MVP 단위/경계 테스트
cargo test --locked -p mantis-history
cargo test --locked -p mantis-cli

# 기존 기능 회귀
cargo test --locked --workspace
```

전체 제품의 대표 실행 경로는 `cargo run --locked --release -p mantis-app`
(native GUI), `cargo run --locked --release -p mantis-server`(서버),
`crates/mantis-app`에서의 Trunk build(wasm)입니다. 자체 호스팅 설정은
[`DEPLOYMENT.md`](DEPLOYMENT.md)를 따릅니다.

## 명시적 가정

1. 여기서 “Grasshopper 정의”는 **Mantis의 GH-style `Graph`**를 뜻합니다.
   Rhino/Grasshopper native `.gh`, `.ghx`, `.3dm` import는 현재 없으며, 이 파일들을
   직접 비교하는 제품으로 간주하지 않습니다.
2. revision은 하나의 검증된 Mantis 체인 안의 committed block 인덱스입니다.
   genesis는 0이며 pending/undo 작업 상태는 비교하지 않습니다.
3. 권위 있는 변경 기록은 `GraphOp`입니다. 형상·면적·체적·fingerprint는
   체인에 서명된 값이 아니라 보고서를 만드는 시점의 파생 값입니다.
4. 형상 범위는 GUI 뷰포트와 맞춘 `__preview=true` 노드의 `Vector`, `Curve`,
   `Mesh` 출력입니다. 숨겨진 노드와 스칼라/텍스트/평면 출력은 형상 장면에
   포함하지 않습니다.
5. fingerprint는 동일 평가기 빌드 내에서 변경을 찾는 정확 비교입니다.
   형상 공차, 리메싱, 동일 형상의 다른 토폴로지를 동치로 처리하지 않습니다.

## 1주 MVP 범위

현재 구현과 일치하는 제안 범위입니다.

- 체인 검증 후 두 명시적 committed revision을 리플레이합니다.
- `(from, to]` 구간의 서명 커밋/`GraphOp`와 양 끝점의 그래프 순변화를 함께
  보고합니다.
- preview 점·곡선·메시를 재평가해 객체 fingerprint, 개수, bounds, 곡선
  길이, 메시 정점/삼각형, 면적, 부호 체적을 비교합니다.
- `no_effect`, `layout_only`, `definition_only`, `geometry_changed`, `incomplete`로
  분류하고 `incomplete`에는 실패 원인을 남깁니다.
- `mantis-cli diff FILE [--from N] [--to N] [--json]`으로 사람용 요약과
  versioned JSON을 제공합니다.
- library 단위 테스트, CLI 파서/출력 테스트, 체크인된 demo chain 통합
  테스트로 기존 리플레이와 CLI 동작을 회귀 검증합니다.

다음은 1주 비범위입니다.

- `.gh`, `.ghx`, `.3dm` parser 또는 Rhino/Grasshopper SDK 연동
- 공차 기반 B-rep/토폴로지 동치성 판정
- 두 독립 체인의 3-way diff/merge, 미커밋 working graph diff
- GUI·서버 API에서의 diff 시각화·저장·공유
- 역사적 evaluator 실행 환경 복원, 대용량 보고서 streaming, 분산 평가

## blocker와 기술적 위험

| 항목 | 영향 | MVP 대응 |
|---|---|---|
| Native 파일 import 부재 | 실제 Rhino/Grasshopper `.gh`/`.ghx`/`.3dm` 산출물을 직접 비교하려는 사용자에게는 blocker | 입력을 Mantis chain JSON으로 한정하고 제품 경계를 명시 |
| 현재 엔진으로 재평가 | 같은 체인이라도 컴포넌트/커널 구현이 바뀌면 과거에 보았던 형상과 다른 보고서가 나올 수 있음 | 결과에 `engine_version`을 남기고 파생 보고서임을 명시 |
| 체인에 evaluator version 미고정 | `engine_version`은 보고서 생성 바이너리의 package version일 뿐이며, 각 커밋이 사용한 레지스트리/커널을 재현하지 못함 | 아카이브 동치성이 아닌 현재 뷰로 판독하고 후속 버전 계약으로 분리 |
| Preview-only 장면 | `__preview=false` 지오메트리와 비시각 출력의 변경은 형상 차이에 잡히지 않음 | 정의 순변화는 별도 보고하고 형상은 “visible preview”로 표기 |
| 테셀레이션 fingerprint | 메시 정점 좌표·삼각형 인덱스의 정확 바이트 차이이며, B-rep 동치성·형상 공차·리메싱 동치를 증명하지 않음 | “changed”를 현재 테셀레이션 변경으로만 해석하고 요약 지표를 함께 제공 |
| 자원 비용 | 두 revision을 전체 리플레이·평가하고 객체 보고서를 메모리에 구성하므로 큰 체인/리스트/메시에서 CPU·RAM·JSON 크기가 커짐 | 중첩 list 탐색 깊이를 8로 제한하고 초과를 완전성 진단에 기록; 검출된 차이가 없으면 `incomplete`, 전체 budget/cancellation은 후속 과제 |
| 미지/실패 컴포넌트 | 현재 registry가 알지 못하는 노드, 평가 오류, 비유한 형상이 있으면 변화 부재를 보장할 수 없음 | 검출된 형상 차이는 보고하되, 서로 다른 net definition에서 차이가 없고 비완전하면 `incomplete`와 오류 상세를 반환 |

## 테스트 fixture와 mock

**임시 mock은 없습니다.** `mantis-history` 단위 테스트는 실제 `Chain::append`로
서명된 체인을 만들고 실제 `Registry::standard()`/`Evaluator`로 box mesh, layout-only,
실패 컴포넌트 케이스를 평가합니다. CLI 경계 테스트는 실제 demo builder와
체크인된 [`examples/demo-chain.json`](../examples/demo-chain.json)을 사용하며, 프로세스를
실제로 실행해 사람용/JSON 출력과 실패 exit code를 확인합니다. 고정 identity와
timestamp는 재현 가능한 fixture이지 평가기 mock이 아닙니다.

## 다음 작업 5개

1. 체인/manifest에 컴포넌트 registry schema hash, kernel/evaluator 버전, build
   provenance를 고정하고 비호환 revision의 재평가를 거부하거나 명시적
   compatibility mode로 실행합니다.
2. `diff`에 연산·노드·출력 객체·리스트 항목·보고서 바이트 budget을
   추가하고, 취소/timeout과 summary-only 모드를 정의해 대용량 입력을 제어합니다.
3. 먼저 문서형 XML인 `.ghx` read-only importer를 스파이크하고, 컴포넌트/port/ID
   mapping과 unsupported-item 보고서를 만든 뒤 `.gh`/`.3dm`은 Rhino 연동 방식을
   별도로 결정합니다.
4. `--geometry-scope preview|all` 계약을 설계하고, hidden 노드·비시각 기하
   출력의 포함 규칙과 completeness 표시를 테스트합니다.
5. 메시에 공차 기반 정규화/거리 지표를 추가하고, 장기적으로 B-rep 또는
   위상 정보가 있는 형상 계약을 도입하기 위한 golden corpus와 동치성 정의를
   확정합니다.
