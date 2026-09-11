# MantisCAD 선택 호환성 모듈

기본 앱에는 Python, Rhino, OpenCascade가 포함되지 않습니다. 필요한 기능만 별도로 설치합니다. Rhino 자체 설치나 Rhino 라이선스 없이 공식 `rhino3dm`으로 실제 `.3dm` 파일을 읽고 씁니다.

## Windows 10/11 x64

1. `mantis-cad-0.2.0-compat-full-windows-x64.zip`을 풀고 `install-addon.cmd`를 실행합니다. `.3dm`만 필요하면 작은 `compat-3dm` ZIP을 선택합니다.
2. 앱에서 CAD 호환성 창을 다시 열어 감지 상태를 확인합니다.
3. 설치 위치는 `%LOCALAPPDATA%\MantisCAD\compat`입니다. 관리자 권한이나 별도 Python 설치가 필요 없습니다.

휴대용 앱은 ZIP 안의 `compat` 폴더를 `MantisCAD.exe` 옆에 복사해도 됩니다. 삭제하려면 원래 압축을 푼 폴더에서 `uninstall-addon.cmd`를 실행합니다. 수정한 파일과 Grasshopper 같은 다른 모듈은 보존합니다.

## Linux / macOS

Python 3.10 이상과 venv 기능이 필요합니다. Ubuntu는 `python3-venv`, `libgl1`을 설치합니다. 소스 묶음을 푼 폴더에서:

```sh
sh setup.sh                 # .3dm + 정확한 B-rep/STEP
sh setup.sh --backend 3dm   # .3dm만 설치
sh setup.sh --uninstall    # 설치한 모듈 제거
```

기본 설치 위치는 `~/.local/share/mantis-cad/compat`입니다. 시스템 Python 패키지는 변경하지 않습니다. 설치 중 PyPI에서 고정 버전의 바이너리 wheel을 다운로드합니다. 배포판/CPU/Python용 wheel이 없으면 설치는 실패하며 기존 설치는 유지됩니다.

## 지원 범위

- `.3dm`: 점, 선, 폴리라인, 유리 NURBS, 원/호, 메시 미리보기. 원본 개체 속성과 문서 테이블을 보존하고 변경 없는 문서는 원본 바이트 그대로 다시 저장합니다. 미리보기를 만들 수 없는 개체도 원본 형상을 보존하며 경고합니다.
- 실제 OpenCascade B-rep: Box/Sphere/Cylinder, Union/Difference/Intersection, 평면 Trim, 모서리 Fillet, STEP 입출력. Fillet의 빈 edge 목록은 전체 모서리입니다. 치수와 내부 BREP 단위는 mm입니다. 알려진 Rhino 문서 단위는 자동 환산합니다.
- Rhino의 평면 B-rep와 닫힌 삼각형 메시를 OpenCascade로 변환할 수 있습니다. 곡면 Rhino B-rep의 직접 변환은 지원하지 않으므로 STEP을 사용합니다. 메시 변환은 면으로 구성된 B-rep이며 매끈한 곡면을 복원하지 않습니다.
- OpenCascade 결과를 `.3dm`으로 저장하면 Rhino 메시와 별도 정확한 BREP 데이터가 함께 저장됩니다. Rhino에서 편집 가능한 B-rep 교환에는 STEP을 사용합니다. Rhino에서 메시가 수정되면 이전 BREP 데이터는 폐기됩니다.
- STEP은 형상을 보존하지만 어셈블리 이름, 색상, 비형상 메타데이터는 가져오지 않습니다. Rhino/Grasshopper 전체 명령이나 플러그인 실행을 제공하지 않습니다. `.gh` 바이너리는 별도 Grasshopper 모듈을 사용합니다.
- 기존 출력 파일은 덮어쓰지 않습니다. JSON 요청/응답은 64 MiB, 개별 입력/출력 CAD 파일은 32 MiB로 제한됩니다. 느리거나 복잡한 작업은 앱에서 취소할 수 있습니다.

## 개발 및 재현

설치용 소스 묶음이 아닌 전체 저장소 체크아웃에서 실행합니다.

```sh
python3 -m venv interop/.venv
interop/.venv/bin/python -m pip install -r interop/requirements-full.txt
interop/.venv/bin/python interop/test_compat.py
python3 interop/build_addon.py --cache /tmp/mantis-addon-wheels --output dist-downloads
```

Windows 묶음은 CPython 3.14.7 embedded runtime, rhino3dm 8.32.1을 포함합니다. full 묶음은 `cadquery-ocp-novtk` 7.9.3.1.1을 추가하며 VTK/CadQuery는 포함하지 않습니다. 다운로드 URL과 SHA256은 `windows-artifacts.lock.json`에 고정되어 있습니다. 각각의 라이선스와 상류 소스 링크는 `licenses/`, Python 런타임 및 wheel의 license 디렉터리에 있습니다.

프로토콜: `python -I compat.py COMMAND`에 JSON 한 개를 표준 입력으로 전달합니다. `capabilities`, `import_3dm`, `export_3dm`, `brep`, `import_step`, `export_step`을 지원하며 표준 출력은 JSON만 반환합니다. 오류는 비정상 종료 코드와 표준 오류로 전달합니다. `export_*`의 `overwrite: true`를 명시한 경우에만 기존 파일을 교체합니다.
