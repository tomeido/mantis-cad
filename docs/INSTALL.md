# MantisCAD 설치 및 다운로드

MantisCAD는 Rust와 OpenGL로 실행되는 네이티브 프로그램입니다. Electron,
별도 브라우저, Rhino/Grasshopper, 서버 설치 없이 로컬 모델링을 시작할 수
있습니다. 기본 설치 파일에는 데스크톱 앱만 포함되며 서버·관리자·CLI 도구는
별도의 `-tools` 압축 파일로 배포합니다.

네이티브 앱은 OpenGL 3.3을 지원하는 그래픽 드라이버가 필요합니다.

빌드한 설치 파일은 저장소의 `dist-downloads/` 폴더에 생성됩니다.
공개 배포가 완료된 버전은 [GitHub Releases](https://github.com/tomeido/mantis-cad/releases)에서
받을 수 있습니다. 로컬 빌드와 공개 릴리스는 별개이며, 아직 게시하지 않은
파일은 Releases에 나타나지 않습니다.

| 운영체제 | 설치 파일 | 설치 없이 실행 |
| --- | --- | --- |
| Windows 10/11, 64비트 | `mantis-cad-vVERSION-windows-x86_64-setup.exe` | 같은 이름의 `.zip` |
| Ubuntu/Debian, x86-64 | `mantis-cad_VERSION_amd64.deb` | `mantis-cad-vVERSION-linux-x86_64.tar.gz` |
| macOS 13 이상, Apple Silicon | `mantis-cad-vVERSION-macos-aarch64.dmg` | 같은 이름의 `.zip` |
| macOS 13 이상, Intel | `mantis-cad-vVERSION-macos-x86_64.dmg` | 같은 이름의 `.zip` |

각 운영체제 파일은 해당 환경에서 빌드해야 합니다. Linux에서 Windows용
MinGW 교차 빌드도 가능합니다. 표는 지원하는 패키지 형식이며, 실제 생성된
파일과 테스트한 운영체제는 해당 릴리스 설명을 확인하세요.

0.2.0의 `.3dm`·STEP·B-rep·바이너리 `.gh` 기능은 별도 선택형 호환팩을
추가합니다. 기본 앱 설치 후 [호환팩 안내](INTEROP.md)를 확인하세요.

## Windows

`-setup.exe`를 실행하고 설치를 완료한 뒤 시작 메뉴에서 **MantisCAD**를
여세요. 관리자 권한 없이 현재 사용자의 `%LOCALAPPDATA%\Programs\MantisCAD`에
설치됩니다. 제거는 Windows 설정의 설치된 앱에서 진행합니다. ZIP 버전은
압축을 모두 풀고 `MantisCAD.exe`를 실행하면 됩니다.

현재 패키지는 코드 서명되지 않은 미리보기 버전입니다. Windows에서 알 수
없는 게시자로 표시될 수 있습니다. 출처와 아래 SHA-256 값을 확인하세요.

## Linux

Ubuntu/Debian에서는 다운로드 폴더에서 실행하세요. `VERSION`은 파일의 실제
버전(예: `0.2.0`)으로 바꿉니다.

```bash
sudo apt install ./mantis-cad_VERSION_amd64.deb
mantis-cad
```

시스템 설치 권한 없이 사용하려면 TAR 파일을 풀고 설치합니다.

```bash
tar -xzf mantis-cad-vVERSION-linux-x86_64.tar.gz
cd mantis-cad-vVERSION-linux-x86_64
./install.sh
~/.local/bin/mantis-cad
```

설치하면 애플리케이션 메뉴에 MantisCAD가 추가됩니다. 기본 위치는
`~/.local`이며 `./install.sh --prefix /원하는/절대/경로`로 변경할 수 있습니다.
설치 없이 압축 폴더에서 `./mantis-app`으로 실행할 수도 있습니다.

OpenGL 3.3을 지원하는 그래픽 환경과 X11 또는 Wayland가 필요합니다.
DEB 설치는 관련 시스템 라이브러리를 자동으로 가져옵니다. TAR 버전도 같은
라이브러리가 필요하며 실행 파일의 glibc 요구 버전 이상이어야 합니다.
공식 CI는 Ubuntu 22.04에서 Linux용 파일을 빌드합니다. 더 최신 Ubuntu에서
직접 빌드한 파일은 요구 버전이 높아질 수 있습니다. DEB 메타데이터의
`Depends`에 실제 빌드의 glibc 최소 버전이 기록됩니다.

제거:

```bash
# DEB 설치
sudo apt remove mantis-cad
# 사용자 폴더 설치
~/.local/lib/mantis-cad/uninstall.sh
```

사용자가 저장한 프로젝트와 환경 설정은 제거하지 않습니다.

## macOS

DMG를 열고 `MantisCAD.app`을 **Applications**로 끌어다 놓으세요.
현재 앱은 코드 서명·공증되지 않은 미리보기 버전입니다. macOS가 실행을
차단하는 경우 출처를 확인한 후 시스템 설정의 **개인정보 보호 및 보안**에서
해당 앱의 실행을 허용할 수 있습니다. 제거는 Applications의 앱을 휴지통으로
옮깁니다.

## 무결성 확인

설치 파일과 `SHA256SUMS`를 같은 폴더에 받은 뒤 확인합니다.

```bash
# Linux (다운로드하지 않은 파일은 건너뜀)
sha256sum --ignore-missing --check SHA256SUMS
# macOS (각 값과 SHA256SUMS 내용 비교)
shasum -a 256 mantis-cad-vVERSION-macos-aarch64.dmg
```

Windows PowerShell에서는 다음 출력의 `Hash`와 `SHA256SUMS`의 같은 파일 값을
비교합니다.

```powershell
Get-FileHash .\mantis-cad-vVERSION-windows-x86_64-setup.exe -Algorithm SHA256
```

## 직접 설치 파일 만들기

저장소의 고정 Rust 도구 체인, Python 3.11 이상, 각 플랫폼의 패키징 도구가
필요합니다. 데스크톱 릴리스는 `opt-level=3`, thin LTO와 심볼 제거를 사용해
기하 연산 속도를 유지하며 크기를 줄입니다. 런타임이나 추가 언어 환경은
설치 파일에 포함하지 않습니다.

```bash
# Linux: dpkg-deb 필요
python3 packaging/package.py --target x86_64-unknown-linux-gnu
# Windows: Visual Studio C++ Build Tools와 NSIS 필요
python packaging/package.py --target x86_64-pc-windows-msvc
# Apple Silicon (해당 Mac에서 실행)
python3 packaging/package.py --target aarch64-apple-darwin
# Intel Mac (해당 Mac에서 실행)
python3 packaging/package.py --target x86_64-apple-darwin
```

`--with-tools`를 추가하면 서버·CLI 도구를 별도 압축 파일로 생성합니다.
`--skip-build --binary-dir PATH`는 이미 빌드된 실행 파일을 패키징합니다.
Windows 패키징은 실행 파일의 DLL 의존성을 확인하고 필요한 MinGW 런타임만
포함합니다. 교차 빌드의 런타임 DLL 위치는 `--runtime-dir PATH`로 지정하며,
필요한 DLL을 찾지 못하면 불완전한 설치 파일을 만들지 않고 오류로 종료합니다.
GitHub의 `Native release` 워크플로도 같은 스크립트를 사용합니다.
수동 실행은 다운로드 가능한 CI 아티팩트를 만들고, 버전 태그로 실행하면
검증 후 GitHub Release에 게시합니다.
