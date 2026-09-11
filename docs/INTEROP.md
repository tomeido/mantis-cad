# Rhino·Grasshopper 호환과 고급 연산

MantisCAD 0.2.0에서는 **CAD…** 창에서 Rhino `.3dm`, Grasshopper `.ghx`·`.gh`,
STEP `.step`·`.stp` 파일을 가져오고 내보낼 수 있습니다. 지원 범위는 아래와
같으며 Rhino·Grasshopper 전체를 대체하는 호환성은 아닙니다.

## 기본 앱과 선택형 호환팩

| 기능 | 필요한 구성 |
| --- | --- |
| GHX 정의 가져오기·내보내기 | 기본 앱 |
| 메시 Boolean Union/Difference/Intersection, 평면 Trim/Split | 기본 앱 |
| `.3dm` 읽기·쓰기 | CAD 호환팩의 rhino3dm |
| 정확한 B-rep Boolean·Trim·Fillet, STEP 읽기·쓰기 | CAD 호환팩의 OpenCascade |
| 바이너리 `.gh` 읽기·쓰기 | GH 아카이브 호환팩의 공식 GH_IO |

기본 앱은 Rust 네이티브 프로그램을 유지합니다. 호환팩은 별도 프로세스로
실행되므로 평소에는 Python·OpenCascade·.NET 런타임을 앱에 적재하지 않습니다.
Rhino 설치나 Rhino 서버 연결 없이 사용합니다.

Windows에서는 기본 앱 설치 후 필요한 ZIP의 압축을 모두 풀고 아래 스크립트를
실행하세요. 호환팩 설치 후 **CAD…** 창을 다시 열면 자동으로 감지합니다.

| 0.2.0 다운로드 파일 | 설치 방법 |
| --- | --- |
| `mantis-cad-0.2.0-compat-full-windows-x64.zip` | `install-addon.cmd` 실행: 3DM·STEP·B-rep 전체 |
| `mantis-cad-0.2.0-compat-3dm-windows-x64.zip` | `install-addon.cmd` 실행: 3DM만 필요할 때 선택 |
| `mantis-cad-0.2.0-grasshopper-windows-x86_64.zip` | `install.cmd` 실행: 바이너리 GH 추가 |

Linux CAD 호환팩은 `mantis-cad-0.2.0-compat-linux-macos-setup.tar.gz`를 풀고
`sh setup.sh`를 실행합니다. 설치할 때 인터넷에서 고정 버전의 라이브러리를
받으며 Python 3.10 이상과 venv, `libgl1`이 필요합니다. GH 호환팩은
`mantis-cad-0.2.0-grasshopper-linux-x86_64.tar.gz`의 `./install.sh`로 설치합니다.
각 묶음에는 제거 방법과 라이선스가 포함되어 있습니다.

## 파일 가져오기와 저장

1. **CAD… → Import**에서 파일 경로를 입력하고 **Import file**을 누릅니다.
   파일을 앱 창으로 끌어 놓으면 경로가 입력됩니다.
2. 기존 작업에 형상 또는 노드를 추가합니다. 한 번의 실행 취소로 가져온
   항목 전체를 제거할 수 있습니다. 지원하지 않는 항목은 창의 진단에 표시됩니다.
3. **Export**에서 새 파일 경로와 확장자를 입력하고 **Export file**을 누릅니다.
   기존 파일을 덮어쓰지 않으므로 저장할 때는 새 이름을 사용하세요.
4. `.3dm`·STEP은 선택한 형상만 저장하거나, **Selected geometry only**를
   해제해 미리보기가 켜진 형상들을 저장합니다. GHX·GH는 그래프를 저장합니다.

프로젝트 자체는 기존 `.mantis` 형식으로 보관할 수 있습니다. 호환팩으로 만든
B-rep의 원본 데이터도 프로젝트에 함께 저장되므로 호환팩 없이 미리보기를
열 수 있고, 호환팩을 연결하면 후속 B-rep 연산을 수행할 수 있습니다.

## Rhino `.3dm`

McNeel 공식 [rhino3dm](https://github.com/mcneel/rhino3dm)을 사용해 실제 3DM
파일을 읽고 씁니다. 지원하는 점·곡선·메시를 편집 가능한 그래프의 CAD Geometry
노드로 가져오고, 원본 형상과 이름·레이어·속성을 함께 보관합니다.

가져온 Rhino B-rep를 수정하지 않고 다시 내보내면 원본 형상을 유지합니다.
미리보기 메시가 없는 지원 외 형상은 원본을 보존하고 진단을 표시합니다.
원본 문서는 공유 메타데이터 패널에 한 번만 보관해 블록 정의와 재질 등도
유지합니다. 가져온 전체 객체가 그대로라면 원본 문서 바이트를 재사용합니다.
편집·부분 저장은 원본 문서의 정의 테이블을 바탕으로 객체 목록을 갱신합니다.
공유 CAD 문서 패널을 지우면 단위·정의 정보를 잃지 않도록 원본 내보내기를
거절하므로 패널을 유지하거나 실행 취소로 복원하세요.
서로 다른 원본 문서의 선택 항목을 함께 내보내는 경우 단위·블록·재질의 잘못된
결합을 막기 위해 저장을 거절합니다. 원본별로 내보내세요.

일반 Move·Rotate·Scale 및 메시 노드의 출력은 계산된 점·곡선·메시입니다.
그 출력에 이전 B-rep 원본을 붙여 잘못 저장하지 않습니다. 정확한 B-rep 작업은
**Solids & fillets**에서 생성한 형상이나 STEP으로 가져온 형상에 수행하세요.

OpenCascade B-rep를 `.3dm`으로 저장할 때 Rhino에서 보이는 형상은 메시이며,
정확한 OpenCascade 원본은 사용자 데이터로 보관됩니다. Rhino의 편집 가능한
곡면 B-rep로 변환한 것으로 간주하지 마세요. 정확한 B-rep 교환에는 STEP을
사용하는 것이 적합합니다.

## Grasshopper `.ghx`와 `.gh`

GHX는 XML 아카이브를 직접 해석하고, GH는 공식
[GH_IO](https://developer.rhino3d.com/api/grasshopper/html/T_GH_IO_Serialization_GH_Archive.htm)
아카이브 변환기를 거칩니다. XML의 확장자만 `.gh`로 바꾸는 방식이 아닙니다.

지원하는 기본 컴포넌트의 GUID, 슬라이더 값, 노드 위치와 연결을 변환합니다.
MantisCAD의 더 큰 입력 위젯이 겹치지 않도록 위치의 상대적 순서를 유지하며
간격을 넓힙니다.
이름이 같아도 동작이 다른 컴포넌트를 임의로 연결하지 않습니다. 지원하지 않는
플러그인·컴포넌트·데이터 구조는 진단으로 보고하고, 내보내기에서 표현할 수
없는 노드가 있으면 저장을 거절합니다.

스크립트 컴포넌트의 코드를 실행하지 않습니다. Rhino 문서에 대한 외부 형상
참조와 전체 Data Tree·Graft·Flatten 동작, 임의의 Grasshopper 플러그인은
자동 복원하지 않습니다. 가져온 결과의 진단과 연결을 확인한 뒤 사용하세요.

현재 GUID와 포트 배치를 확인한 연산 컴포넌트는 다음 18개입니다.

| 분류 | 지원하는 Grasshopper 컴포넌트 |
| --- | --- |
| 좌표·벡터 | Construct Point, Vector XYZ, Unit X, Unit Y, Unit Z |
| 곡선 | Line, Circle, PolyLine, Nurbs Curve, Divide Curve |
| 변환·측정 | Move, Distance |
| 수·목록 | Series, Addition, Multiplication, Merge, List Length, List Item |

추가로 Number Slider(실수·정수), Boolean Toggle, 연결 없는 Panel 텍스트,
저장된 숫자·불리언·문자열 목록, 점·평면 데이터를 읽습니다. 목록은 단일 `{0}`
가지이며 최대 10,000개입니다. 소스가 연결된 형식 변환용 Number·Integer·Text
등의 파라미터는 자동 형변환을 재현하지 못하므로 진단 후 제외합니다.
내보내는 저장 목록은 한 목록 안에서 같은 자료형을 사용해야 합니다.

Vector XYZ·Nurbs Curve·Divide Curve·Move는 첫 번째 결과 출력만 연결할 수
있습니다. 나머지 출력에 의존하는 노드는 진단 후 제외합니다. 주기적 NURBS,
Divide Curve의 Kinks 옵션, 추가 가변 입력은 지원하지 않습니다. NURBS의
등간격 분할은 곡선을 평가해 호 길이를 수치적으로 근사합니다.

Series 개수, Divide Curve 분할 수, NURBS 차수, List Item 인덱스에 실수
소스가 연결되면 정수 변환 차이를 알리는 진단이 표시됩니다. Grasshopper는
정수 입력을 반올림하지만 MantisCAD의 개수·인덱스 변환은 다를 수 있습니다.
두 프로그램의 결과를 맞추려면 이 입력에는 정수 슬라이더 또는 저장된 정수
목록을 사용하세요. 이 제한은 GHX·GH 내보내기에도 적용됩니다.

## 정확한 B-rep 연산

**CAD… → Solids & fillets**에서 OpenCascade 기반 연산을 사용합니다.
이 창의 크기·반지름·좌표는 밀리미터입니다. 지원하는 Rhino 단위 정보가 있으면
입력 형상을 밀리미터로 변환하고 진단에 표시합니다. STEP도 밀리미터로 정규화합니다.

- **B-rep Box / Sphere / Cylinder:** 크기·반지름·높이·원점을 지정합니다.
- **Boolean Union / Difference / Intersection:** 그래프에서 대상 형상을
  선택합니다. Difference는 위쪽, 같은 높이에서는 왼쪽 노드를 본체로 삼습니다.
- **Trim with plane:** 평면 원점과 법선, 보존할 쪽을 지정합니다.
- **Fillet edges:** 반지름과 0부터 시작하는 모서리 번호를 입력합니다.
  모서리 번호를 비워 두면 전체 모서리에 적용합니다.

연산 결과는 독립 형상으로 추가되며, 입력 형상은 미리보기를 끕니다.
실행 취소로 결과와 입력 미리보기를 복원할 수 있습니다. 입력 슬라이더를
나중에 바꾸더라도 이미 생성한 독립 B-rep 결과를 자동으로 다시 계산하지는
않습니다. 실시간 연결 계산에는 아래 메시 노드를 사용할 수 있습니다.

삼각형 메시를 입력하면 평면 면들로 이루어진 B-rep로 변환됩니다. 원래의
매끄러운 NURBS 곡면을 역으로 복원하지 않습니다. Rhino의 임의 곡면 B-rep와
OpenCascade 사이의 완전한 변환도 지원하지 않습니다. 연산할 수 없는 입력과
실패한 필렛은 오류로 보고하고 기존 형상을 유지합니다.

## 기본 앱의 메시 연산

**Ctrl+K** 명령창 또는 그래프 추가 메뉴에서 사용합니다.

| 명령 | 입력과 결과 |
| --- | --- |
| `MeshBooleanUnion` | 선택한 두 닫힌 메시의 합집합 |
| `MeshBooleanDifference` | 위/왼쪽 메시에서 아래/오른쪽 메시 빼기 |
| `MeshBooleanIntersection` | 선택한 두 닫힌 메시의 공통 부피 |
| `MeshTrimPlane 0 0 5 0 0 1` | Z=5 평면의 음수 쪽 보존, 절단면 닫기 |
| `MeshTrimPlane 0 0 5 0 0 1 1` | 같은 평면의 양수 쪽 보존 |
| `MeshSplitPlane 0 0 5 0 0 1` | 평면 양쪽의 닫힌 메시를 두 출력으로 분리 |

평면 인자는 `원점 x y z, 법선 x y z`입니다. Boolean은 노드 두 개를
선택해야 합니다. 모든 연산이 일반 그래프 노드로 생성되어 연결·입력 수정,
실행 취소와 재계산을 지원합니다.

메시 연산은 입력 메시당 삼각형 4,096개 이내의 닫힌 형상을 대상으로 합니다.
좌표는 ±1e9, 허용오차는 1e-9~1 범위이며 기본값은 1e-7입니다. 자기 교차,
열린 표면, 뒤집힌 방향, 모서리나 점만 접촉해 비다양체가 되는 결과는 거절합니다.
메시의 곡면 정밀도는 입력 테셀레이션에 따라 결정됩니다.

## 호환팩 설치 위치

Windows CAD 호환팩 설치 스크립트는 현재 사용자의
`%LOCALAPPDATA%\MantisCAD\compat`에 설치합니다. 무설치 앱은 실행 파일 옆에
`compat` 폴더를 둘 수도 있습니다. Linux에서는 호환팩의 설치 스크립트를
사용해 `~/.local/share/mantis-cad/compat`에 설치합니다.

GH 변환기는 `compat/grasshopper/mantis-gh-io` (Windows는 `.exe`) 위치에서
찾습니다. 각 호환팩에 포함된 설치 안내를 따르세요.

개발·사용자 지정 설치에는 `MANTIS_COMPAT_DIR`, `MANTIS_COMPAT_PYTHON`,
`MANTIS_GH_CONVERTER` 환경변수로 위치를 지정할 수 있습니다.

3DM·STEP 입력 파일은 32 MiB, GHX·GH와 요청·결과는 최대 64 MiB의
제한을 적용합니다. 보존하는 원본과 미리보기의 합계 때문에 실제 가져올 수 있는
파일 크기는 더 작아질 수 있습니다. 큰 가져오기 데이터는 프로젝트·서명 기록의
일부이며 서버의 기본 프로젝트 제한(24 MiB)을 넘으면 원격 동기화가 거절됩니다. CAD 작업은 별도 프로세스에서
실행하고 취소 및 2분 제한을 지원합니다. 큰 정의나 복잡한 필렛은 작은 단위로
나누어 처리하세요.
