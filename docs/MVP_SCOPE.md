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
  -> 선택 범위(preview 또는 all)의 점·곡선·메시 fingerprint/요약 비교
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

# 숨긴 노드의 형상과 중간 출력까지 포함
cargo run --locked -p mantis-cli -- \
  diff examples/demo-chain.json --geometry-scope all --json

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
   네이티브 앱은 기본 GHX importer와 선택형 호환팩의 `.gh`·`.3dm`·STEP
   가져오기를 제공합니다. `diff`의 입력은 Mantis chain JSON이며 가져온 결과도
   커밋 후 비교할 수 있습니다. 원본 CAD 파일을 직접 비교하지 않습니다. 지원 범위는
   [`INTEROP.md`](INTEROP.md)를 따릅니다.
2. revision은 하나의 검증된 Mantis 체인 안의 committed block 인덱스입니다.
   genesis는 0이며 pending/undo 작업 상태는 비교하지 않습니다.
3. 권위 있는 변경 기록은 `GraphOp`입니다. 기본 컴포넌트의 형상 출력은
   파생 값이며, 가져온 CAD 원본·미리보기와 외부 B-rep 결과는 예외로
   노드 파라미터에 기록되어 커밋 후 서명 범위에 포함됩니다. 비교 보고서의
   형상 요약·면적·체적·fingerprint는 보고서를 만드는 시점의 파생 값입니다.
4. 기본 `preview` 범위는 GUI 뷰포트와 맞춘 미리보기 활성 노드의 `Vector`,
   `Curve`, `Mesh` 출력입니다. `--geometry-scope all`은 `__preview=false`인
   노드도 포함합니다. 두 범위 모두 리스트 안의 형상을 탐색하며 스칼라/텍스트/평면은
   형상 객체로 세지 않습니다. `all`은 중간 출력도 별도 객체로 집계하므로 면적·체적
   합계가 최종 솔리드의 합집합 측정값을 뜻하지 않습니다. 가져온 CAD도 기록된 데이터에서
   평가하며 선택형 CAD backend를 다시 실행하지 않습니다.
5. fingerprint는 동일 평가기 빌드 내에서 변경을 찾는 정확 비교입니다.
   형상 공차, 리메싱, 동일 형상의 다른 토폴로지를 동치로 처리하지 않습니다.

## 1주 MVP 범위

현재 구현과 일치하는 제안 범위입니다.

- 체인 검증 후 두 명시적 committed revision을 리플레이합니다.
- `(from, to]` 구간의 서명 커밋/`GraphOp`와 양 끝점의 그래프 순변화를 함께
  보고합니다.
- 선택 범위의 점·곡선·메시를 재평가해 객체 fingerprint, 개수, bounds, 곡선
  길이, 메시 정점/삼각형, 면적, 부호 체적을 비교합니다.
- `no_effect`, `layout_only`, `definition_only`, `geometry_changed`, `incomplete`로
  분류하고 `incomplete`에는 실패 원인을 남깁니다.
- `mantis-cli diff FILE [--from N] [--to N] [--geometry-scope preview|all] [--json]`으로
  사람용 요약과 versioned JSON을 제공합니다. `geometry scope`/`geometry.scope`에
  범위를 표시하며, scope 필드가 없는 기존 schema version 1 JSON은 `preview`입니다.
  라이브러리 `compare_revisions`는 기존 preview 기본값을 유지하고
  `compare_revisions_with_scope`로 범위를 선택합니다. 서명·동기화 형식은 유지합니다.
- 파생 보고서에 `engine_version`과 `evaluator_provenance`를 기록합니다.
  `registry_contract_hash`는 `mantis-registry-contract-v1` 도메인의 SHA-256입니다.
  이름순 컴포넌트 ID, 순서 있는 입력/출력 포트의 이름·종류·접근 방식·기본값 유무와
  값을 포함하며 UI label/category는 제외합니다. 명시적 variant tag와 고정 폭
  little-endian 길이/수치 인코딩을 사용하고 기본값의 부동소수점 비트(±0 포함),
  리스트와 형상 데이터(메시 normal 포함)를 보존합니다.
  `build_id`는 컴파일 시 `MANTIS_GIT_SHA`의 앞뒤 공백을 제거한 값이며, 없거나
  비어 있으면 `unknown`입니다. 릴리스 파이프라인은 해당 값에 Git revision을 넣습니다.
  로컬 빌드도 `MANTIS_GIT_SHA=<revision> cargo build --locked -p mantis-cli`로 지정할 수
  있으며 실행 시 환경변수로는 바뀌지 않습니다. 이 값은 빌드자가 제공하는 표식으로,
  수정된 소스·컴파일 옵션·의존성까지 보증하는 바이너리 해시는 아닙니다.
  계약 해시 역시 평가 함수·커널 코드·노드 파라미터의 의미를 포함하지 않습니다.
  기존 schema version 1 보고서에서 provenance가 없으면 미상(`None`)으로 읽고
  현재 평가기 정보를 소급해 채우지 않습니다. 이 추가는 서명 체인/manifest를 변경하지 않습니다.
- CLI `diff` 입력은 공백을 포함해 최대 32 MiB (33,554,432바이트)로 제한합니다.
  최대 한도+1바이트만 읽고, 초과하면 파싱·체인 검증·평가 전에 한도를 명시한
  오류와 종료 코드 1을 반환하며 보고서를 출력하지 않습니다. 라이브러리 직접 호출에는
  이 파일 입력 제한이 적용되지 않습니다.
- library 단위 테스트, CLI 파서/출력 테스트, 체크인된 demo chain 통합
  테스트로 기존 리플레이와 CLI 동작을 회귀 검증합니다.

다음은 1주 비범위입니다.

- `.gh`, `.ghx`, `.3dm`, STEP 원본 파일의 직접 비교와 완전한 Rhino/Grasshopper 호환
- 공차 기반 B-rep/토폴로지 동치성 판정
- 두 독립 체인의 3-way diff/merge, 미커밋 working graph diff
- GUI·서버 API에서의 diff 시각화·저장·공유
- 역사적 evaluator 실행 환경 복원, 대용량 보고서 streaming, 분산 평가

## blocker와 기술적 위험

| 항목 | 영향 | MVP 대응 |
|---|---|---|
| Native 파일 직접 비교·완전 호환 부재 | 지원 외 컴포넌트, 전체 Data Tree, 외부 Rhino 형상 참조 등은 가져온 그래프로 복원되지 않음 | 지원되는 결과를 Mantis 체인에 커밋해 비교하고, 제외된 항목은 가져오기 진단과 `INTEROP.md`에서 확인 |
| 현재 엔진으로 재평가 | 같은 체인이라도 컴포넌트/커널 구현이 바뀌면 과거에 보았던 형상과 다른 보고서가 나올 수 있음 | `engine_version`, registry 계약 해시, build ID로 보고서를 생성한 평가기 정보를 표시 |
| 체인에 evaluator version 미고정 | 보고서의 provenance는 현재 평가기의 선언된 포트 계약과 빌드 표식이며, 각 커밋이 사용한 레지스트리/커널을 재현하지 못함. build ID가 `unknown`이면 빌드 출처도 미상 | 아카이브 동치성이 아닌 현재 뷰로 판독. 서명 형식에 evaluator를 고정하는 작업은 별도 version migration으로 분리 |
| 형상 비교 범위 | 기본 `preview`는 `__preview=false` 형상을 제외하고, `all`도 스칼라/텍스트/평면은 형상 객체로 세지 않음 | 숨긴 형상은 `--geometry-scope all`로 비교하고 보고서에 범위 표시. 비형상 변경은 정의 순변화에서 확인 |
| 테셀레이션 fingerprint | 메시 정점 좌표·삼각형 인덱스의 정확 바이트 차이이며, B-rep 동치성·형상 공차·리메싱 동치를 증명하지 않음 | “changed”를 현재 테셀레이션 변경으로만 해석하고 요약 지표를 함께 제공 |
| 자원 비용 | 두 revision을 전체 리플레이·평가하고 객체 보고서를 메모리에 구성하므로 작은 입력도 큰 리스트/메시를 만들면 CPU·RAM·JSON 크기가 커짐 | CLI `diff` 입력을 32 MiB로 제한하고 초과 시 평가 전에 거부. 중첩 list 탐색 깊이를 8로 제한하고 초과를 완전성 진단에 기록; 검출된 차이가 없으면 `incomplete`. 노드·출력 객체·보고서 크기 예산과 cancellation은 후속 과제 |
| 미지/실패 컴포넌트 | 현재 registry가 알지 못하는 노드, 평가 오류, 비유한 형상이 있으면 변화 부재를 보장할 수 없음 | 검출된 형상 차이는 보고하되, 서로 다른 net definition에서 차이가 없고 비완전하면 `incomplete`와 오류 상세를 반환 |

형상 출력이 가능한 노드의 오류만 선택 범위의 완전성 진단에 포함합니다. 따라서 숨긴
미지 컴포넌트의 실패는 `preview`에서 제외되고 `all`에서는 포함됩니다. 숨긴 상위 노드의
오류가 미리보기 활성 하위 노드로 전파되면 `preview`에서도 진단하며, 스칼라 전용
노드의 독립적인 실패는 두 범위 모두 형상의 완전성을 떨어뜨리지 않습니다.

## 테스트 fixture와 mock

형상 비교 테스트에 임시 evaluator mock은 없습니다. `mantis-history` 단위 테스트는 실제 `Chain::append`로
서명된 체인을 만들고 실제 `Registry::standard()`/`Evaluator`로 box mesh, layout-only,
실패 컴포넌트 케이스를 평가합니다. CLI 경계 테스트는 실제 demo builder와
체크인된 [`examples/demo-chain.json`](../examples/demo-chain.json)을 사용하며, 프로세스를
실제로 실행해 사람용/JSON 출력과 실패 exit code를 확인합니다. 고정 identity와
timestamp는 재현 가능한 fixture이지 평가기 mock이 아닙니다.
registry 계약 해시 테스트는 테스트용 포트 선언 fixture로 등록 순서의 독립성,
계약 변경 감지, 정확한 기본값 인코딩을 확인하며 해당 fixture의 평가 함수는 호출하지 않습니다.

## 다음 작업 5개

1. 구현된 파생 보고서 provenance를 바탕으로, 별도 version migration에서 체인/manifest에
   컴포넌트 registry 계약 hash, kernel/evaluator 버전, build provenance를 고정하고
   비호환 revision의 재평가를 거부하거나 명시적 compatibility mode로 실행합니다.
2. `diff`에 연산·노드·출력 객체·리스트 항목·보고서 바이트 budget을
   추가하고, 취소/timeout과 summary-only 모드를 정의해 대용량 입력을 제어합니다.
3. 구현된 GHX·GH·3DM·STEP 가져오기의 fixture와 진단 회귀 검증을 확장하고,
   지원 외 컴포넌트·포트·데이터 구조의 처리 범위를 `INTEROP.md`와 함께 관리합니다.
4. 구현된 `--geometry-scope preview|all`을 GUI·서버 diff 경로에 노출하고,
   사용자 선택 범위와 완전성 진단을 함께 표시합니다.
5. 메시에 공차 기반 정규화/거리 지표를 추가하고, 장기적으로 B-rep 또는
   위상 정보가 있는 형상 계약을 도입하기 위한 golden corpus와 동치성 정의를
   확정합니다.
