# Rhino·Grasshopper 방식으로 모델링하기

MantisCAD는 독립 실행형 Rust CAD입니다. Rhino의 명령 중심 모델링과 Grasshopper의
노드 연결 방식을 참고하며, 명령 실행 결과도 수정 가능한 노드·슬라이더·연결로 남깁니다.
Rhino 설치나 라이선스가 필요하지 않습니다.

## 빠른 시작

1. 앱에서 **Commands…** 또는 **Ctrl+K**(macOS: **⌘K**)로 명령창을 엽니다.
2. `Circle 5`를 실행합니다. 생성한 원 노드가 선택됩니다.
3. 다시 명령창에서 `ExtrudeCrv 10`을 실행하면 반지름 5, 높이 10의 메시가 생깁니다.
4. 노드의 숫자 슬라이더나 입력값을 바꾸면 연결된 형상이 갱신됩니다.
5. **Ctrl+Z / Ctrl+Shift+Z**로 명령 하나 전체를 실행 취소·재실행합니다.
   Commit 이후의 서명된 기록은 실행 취소로 변경하지 않습니다.

명령 이름은 대소문자를 구분하지 않고 `_Circle` 같은 접두사를 허용합니다.
숫자는 공백 또는 쉼표로 구분합니다. 명령창의 각도는 **도(degree)**,
노드의 각도 포트는 **라디안**입니다. 길이는 프로젝트에서 일관되게 정한 모델 단위를
사용하며 자동 단위 변환은 없습니다. 명령창에는 검색과 실행 예제가 있습니다.

## 모델링 명령

| 작업 | 입력 예시 | 입력 형상·기준 |
|---|---|---|
| 점 | `Point 1 2 3` | XYZ 좌표 |
| 선 | `Line 0 0 0 10 0 0` | 시작점과 끝점 |
| 폴리라인 | `Polyline 0 0 0 10 0 0 10 10 0` | 2~100개의 XYZ 점, 열린 곡선 |
| NURBS 곡선 | `Curve 0 0 0 5 5 0 10 0 0` | 제어점, 보간점이 아님 |
| 원·호 | `Circle 5`, `Arc 5 0 180` | 원점의 XY 평면, 반지름·각도 |
| 사각형 | `Rectangle 10 20` | 원점에서 양의 X/Y 방향, 너비·높이 |
| 기본 입체 | `Box 10 20 30`, `Sphere 5` | 크기 또는 반지름 |
| 원통·원뿔·토러스 | `Cylinder 5 10`, `Cone 5 10`, `Torus 5 1` | 반지름·높이 또는 주반지름·관반지름 |
| 돌출 | `ExtrudeCrv 10`, `ExtrudeCrv 0 0 10` | 선택한 곡선, Z 높이 또는 방향 벡터 |
| 회전면 | `Revolve 360` | 선택한 곡선, 원점을 지나는 Z축 |
| 로프트 | `Loft` | 선택한 단면 곡선들을 연결 |
| 파이프 | `Pipe 0.5` | 선택한 경로 곡선, 관반지름 |
| 평면 | `PlanarSrf` | 선택한 닫힌 평면 곡선 |
| 이동·복사 | `Move 10 0 0`, `Copy 10 0 0` | 선택 형상, XYZ 이동량 |
| 회전·배율 | `Rotate 45`, `Scale 2` | 원점·Z축 기준 회전, 원점 기준 균일 배율 |
| 대칭 | `Mirror yz` | 원점을 지나는 `xy`, `xz`, `yz` 평면 |
| 직선 배열 | `ArrayLinear 5 10 0 0` | 원본 포함 개수, 간격 벡터 |
| 원형 배열 | `ArrayPolar 6 360` | 원본 포함 개수, 원점·Z축 기준 전체 각도 |
| 분할·평가 | `Divide 10`, `EvaluateCurve 0.5` | 선택 곡선의 분할 개수 또는 0~1 매개변수 |
| 끝점·방향 | `EndPoints`, `Reverse` | 선택한 곡선 |
| 측정 | `Length`, `Area`, `Volume`, `BoundingBox` | 길이는 곡선, 면적·체적은 메시 |
| 수열·범위·난수 | `Series 0 1 10`, `Range 0 1 10`, `Random 0 10 10 1` | 시작·간격/끝·개수/분할수, 난수는 마지막 인자가 seed |

형상 작업은 **노드 에디터에서 출력 노드를 선택한 후** 실행합니다.
Move·Rotate·Scale·Mirror·Reverse·배열은 원본 노드를 보존하고 그 미리보기를 숨깁니다.
Copy는 원본 미리보기도 유지합니다. 실패한 명령은 일부 노드만 남기지 않습니다.

Loft의 단면 순서는 노드 위치의 위→아래, 같은 높이에서는 왼쪽→오른쪽입니다.
예를 들어 `Circle 2` → `Copy 0 0 5`로 두 단면을 만들고 두 출력 노드를 함께 선택한 뒤
`Loft`를 실행합니다. 필요하면 단면 노드 위치 또는 곡선 방향을 조정합니다.

원형 배열은 회전축에서 떨어진 형상으로 시작합니다. 예를 들어 `Circle 1` →
`Move 5 0 0` → `ArrayPolar 6 360`이면 원 6개가 축 둘레에 배치됩니다.
회전면은 `Line 3 0 0 3 0 5` → `Revolve 360`처럼 축 옆의 세로 단면을 사용합니다.

**OBJ…**에서 현재 미리보기가 켜진 메시들을 하나의 OBJ로 저장할 수 있습니다.
설치형 앱은 새 파일 경로를 입력하고 **Save OBJ**, 웹 앱은 **Download OBJ**를 누릅니다.
곡선과 점은 OBJ 내보내기에 포함되지 않으므로 먼저 돌출·파이프·평면으로 만드세요.
프로젝트 정의는 기존 **file…** 메뉴의 `.mantis` 내보내기로 별도 보관할 수 있습니다.

**CAD…**에서 파일 호환과 B-rep 연산을 사용할 수 있습니다.
기본 앱의 `MeshBooleanUnion`, `MeshBooleanDifference`, `MeshBooleanIntersection`,
`MeshTrimPlane`, `MeshSplitPlane` 명령은 [입력 예제](INTEROP.md#기본-앱의-메시-연산)를 참고하세요.

## Grasshopper식 노드

**40종의 명령과 83종의 컴포넌트**를 제공합니다.
0.2.0에는 메시 Boolean 3종, 평면 Trim/Split, CAD Geometry, Stored List를 추가했습니다.
캔버스 추가 메뉴에서 이름 또는 카테고리로 검색하고 출력 포트를 입력 포트에 연결합니다.

| 새 컴포넌트 ID | 기능 |
|---|---|
| `rectangle` | 평면·너비·높이로 닫힌 사각형 곡선 생성 |
| `end_points` | 곡선의 시작점·끝점 출력 |
| `reverse_curve` | 곡선 종류를 유지하면서 방향 반전 |
| `array_linear` | 원본부터 일정 벡터 간격으로 형상 복제 |
| `array_polar` | 평면 원점·법선 기준으로 회전 복제 |
| `reverse_list` | 리스트 역순 |
| `sort_list` | 숫자 오름차순과 원래 인덱스 출력, 동일 값의 순서 유지 |
| `shift_list` | 양수는 왼쪽 이동, wrap 또는 잘라내기 |
| `cull_pattern` | 반복 패턴에서 true인 항목 제거 |
| `dispatch` | 반복 패턴의 true/false 항목을 A/B로 분리 |
| `merge` | 두 리스트를 순서대로 연결 |
| `bounds` | 숫자 리스트의 최솟값·최댓값 |
| `random` | 범위·개수·seed로 재현 가능한 숫자 생성 |

예시: `Random` → `Sort List` → `List Item`으로 정렬된 값을 선택하거나,
`Series` → `Unit Z` → `Move`로 층별 형상을 배치할 수 있습니다.
`Bounds`의 두 출력을 기존 `Remap`의 원래 범위 입력에 연결하면 값을 새 범위로 변환합니다.

Item 입력은 가장 긴 리스트 길이에 맞춰 실행하고, 짧은 리스트의 마지막 값을 반복합니다.
List 입력은 리스트 전체를 받습니다. 중첩 리스트가 생길 수 있지만 Grasshopper의
경로 기반 Data Tree·Graft·Flatten 전체 동작을 구현한 것은 아닙니다.

## 경량화와 형상 범위

- Rust/egui와 OpenGL을 사용합니다. Electron·내장 브라우저·Rhino SDK를 번들에 넣지 않습니다.
- 화면 이동이나 노드 배치만 바뀌면 이미 계산한 출력과 메시를 재사용합니다.
- 기본 프레임버퍼의 멀티샘플링은 0으로 설정해 GPU 메모리 사용을 줄이고
  소프트웨어·원격 그래픽 환경의 실행 호환성을 높였습니다.
- 실행 취소는 전체 편집 기록의 반복 복사 대신 기록 위치를 저장하고,
  재실행에는 취소한 연산 묶음만 보관합니다.
- 배열은 한 번의 컴포넌트 실행당 최대 4,096개, 추정 형상 데이터 64 MiB로 제한합니다.
  이는 앱 전체 메모리 상한이 아니며 여러 입력 형상이나 GPU 버퍼는 추가 메모리를 사용합니다.
- seed 난수는 외부 난수 라이브러리 없이 동일 입력에서 동일 값을 재생성합니다.

곡선은 해석적 곡선과 NURBS를 사용하고, 표면·입체 연산의 출력은 테셀레이션 메시입니다.
Loft는 단면 사이를 연결한 메시이며 Rhino의 모든 로프트 옵션과 같지 않습니다.
돌출은 닫힌 평면 프로파일의 끝을 막고, 파이프는 열린 경로의 양 끝을 막습니다.
면적·체적은 메시 근사값이며 체적은 닫힌 메시에서 해석해야 합니다.

0.2.0의 `.3dm`·`.gh`·`.ghx` 파일 호환, 메시 Boolean·Trim/Split과
선택형 OpenCascade B-rep Boolean·Trim·Fillet은 [호환·고급 연산 안내](INTEROP.md)를
참고하세요. Rhino 플러그인과 전체 Grasshopper Data Tree는 지원하지 않습니다.

## 참고한 공식 문서

명령의 목적과 입력 흐름은 McNeel의
[Circle](https://docs.mcneel.com/rhino/8/help/en-us/commands/circle.htm),
[ExtrudeCrv](https://docs.mcneel.com/rhino/8/help/en-us/commands/extrudecrv.htm),
[Loft](https://docs.mcneel.com/rhino/8/help/en-us/commands/loft.htm),
[Array](https://docs.mcneel.com/rhino/8/help/en-us/commands/array.htm)를 참고했습니다.
리스트 처리 방식은
[Grasshopper 데이터 구조 가이드](https://developer.rhino3d.com/guides/grasshopper/gh-algorithms-and-data-structures/data-structures/)
및 [Data Trees 설명](https://developer.rhino3d.com/en/guides/grasshopper/the-why-and-how-of-data-trees/)을
참고했으며 위 표에 MantisCAD의 실제 지원 범위를 구분했습니다.
