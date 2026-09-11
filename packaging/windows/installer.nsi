Unicode true
!include "MUI2.nsh"
!include "x64.nsh"

Name "MantisCAD"
OutFile "${OUTPUT}"
InstallDir "$LOCALAPPDATA\Programs\MantisCAD"
InstallDirRegKey HKCU "Software\MantisCAD" "InstallDir"
RequestExecutionLevel user
SetCompressor /SOLID lzma
SetCompressorDictSize 16
VIProductVersion "${VERSION}.0"
VIAddVersionKey "ProductName" "MantisCAD"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "FileDescription" "MantisCAD per-user installer"
VIAddVersionKey "LegalCopyright" "MantisCAD contributors (MIT)"

!define MUI_ABORTWARNING
!define MUI_FINISHPAGE_RUN "$INSTDIR\MantisCAD.exe"
!define MUI_FINISHPAGE_RUN_NOTCHECKED
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "Korean"
!insertmacro MUI_LANGUAGE "English"

Function .onInit
  ${IfNot} ${RunningX64}
    MessageBox MB_ICONSTOP "MantisCAD requires 64-bit Windows."
    Abort
  ${EndIf}
  SetShellVarContext current
FunctionEnd

!macro GuardInstallFile FILE
  IfFileExists "$INSTDIR\${FILE}" install_collision 0
!macroend

Section "MantisCAD" SEC_APP
  IfFileExists "$INSTDIR\.mantis-install" check_marker 0
  !insertmacro GuardInstallFile "MantisCAD.exe"
  !insertmacro GuardInstallFile "LICENSE"
  !insertmacro GuardInstallFile "INSTALL.md"
  !insertmacro GuardInstallFile "COMMANDS.md"
  !insertmacro GuardInstallFile "INTEROP.md"
  !insertmacro GuardInstallFile "THIRD_PARTY_LICENSES.md"
  !insertmacro GuardInstallFile "Uninstall.exe"
  !insertmacro GuardInstallFile "libgcc_s_seh-1.dll"
  !insertmacro GuardInstallFile "libstdc++-6.dll"
  !insertmacro GuardInstallFile "libwinpthread-1.dll"
  Goto install_safe
check_marker:
  FileOpen $0 "$INSTDIR\.mantis-install" r
  FileRead $0 $1
  FileClose $0
  StrCmp $1 "MantisCAD user installation v1" install_safe install_collision
install_collision:
  MessageBox MB_ICONSTOP "This folder contains files from another installation. Choose a different folder."
  Abort
install_safe:
  SetOutPath "$INSTDIR"
  SetOverwrite on
  File "${PACKAGE_DIR}\MantisCAD.exe"
  !if /FileExists "${PACKAGE_DIR}\*.dll"
    File "${PACKAGE_DIR}\*.dll"
  !endif
  File "${PACKAGE_DIR}\LICENSE"
  File "${PACKAGE_DIR}\INSTALL.md"
  File "${PACKAGE_DIR}\COMMANDS.md"
  File "${PACKAGE_DIR}\INTEROP.md"
  File "${PACKAGE_DIR}\THIRD_PARTY_LICENSES.md"
  FileOpen $0 "$INSTDIR\.mantis-install" w
  FileWrite $0 "MantisCAD user installation v1"
  FileClose $0
  WriteUninstaller "$INSTDIR\Uninstall.exe"
  CreateDirectory "$SMPROGRAMS\MantisCAD"
  CreateShortcut "$SMPROGRAMS\MantisCAD\MantisCAD.lnk" "$INSTDIR\MantisCAD.exe"
  CreateShortcut "$SMPROGRAMS\MantisCAD\Uninstall.lnk" "$INSTDIR\Uninstall.exe"
  WriteRegStr HKCU "Software\MantisCAD" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\MantisCAD" "DisplayName" "MantisCAD"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\MantisCAD" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\MantisCAD" "Publisher" "MantisCAD contributors"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\MantisCAD" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\MantisCAD" "DisplayIcon" "$INSTDIR\MantisCAD.exe"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\MantisCAD" "UninstallString" '"$INSTDIR\Uninstall.exe"'
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\MantisCAD" "QuietUninstallString" '"$INSTDIR\Uninstall.exe" /S'
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\MantisCAD" "NoModify" 1
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\MantisCAD" "NoRepair" 1
SectionEnd

Function un.onInit
  IfFileExists "$INSTDIR\.mantis-install" 0 un_invalid
  FileOpen $0 "$INSTDIR\.mantis-install" r
  FileRead $0 $1
  FileClose $0
  StrCmp $1 "MantisCAD user installation v1" un_valid un_invalid
un_invalid:
  MessageBox MB_ICONSTOP "Run the uninstaller from the installed MantisCAD folder."
  Abort
un_valid:
FunctionEnd

Section "Uninstall"
  SetShellVarContext current
  Delete "$INSTDIR\MantisCAD.exe"
  Delete "$INSTDIR\libgcc_s_seh-1.dll"
  Delete "$INSTDIR\libstdc++-6.dll"
  Delete "$INSTDIR\libwinpthread-1.dll"
  Delete "$INSTDIR\LICENSE"
  Delete "$INSTDIR\INSTALL.md"
  Delete "$INSTDIR\COMMANDS.md"
  Delete "$INSTDIR\INTEROP.md"
  Delete "$INSTDIR\THIRD_PARTY_LICENSES.md"
  Delete "$INSTDIR\.mantis-install"
  Delete "$INSTDIR\Uninstall.exe"
  Delete "$SMPROGRAMS\MantisCAD\MantisCAD.lnk"
  Delete "$SMPROGRAMS\MantisCAD\Uninstall.lnk"
  RMDir "$SMPROGRAMS\MantisCAD"
  RMDir "$INSTDIR"
  DeleteRegKey HKCU "Software\MantisCAD"
  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\MantisCAD"
SectionEnd
