!ifndef VERSION
!error "VERSION must be defined"
!endif

!ifndef VERSION_RESOURCE
!error "VERSION_RESOURCE must be defined"
!endif

!ifndef ARCH
!error "ARCH must be defined"
!endif

!ifndef BUILD_DIR
!error "BUILD_DIR must be defined"
!endif

!ifndef OUT_DIR
!error "OUT_DIR must be defined"
!endif

Unicode true
SetCompressor /SOLID lzma

!include "LogicLib.nsh"
!include "MUI2.nsh"
!include "x64.nsh"

!define PRODUCT_NAME "KeepPeek Desktop"
!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\KeepPeekDesktop"
!define START_MENU_DIR "$SMPROGRAMS\${PRODUCT_NAME}"

Name "${PRODUCT_NAME} ${VERSION} (${ARCH})"
OutFile "${OUT_DIR}\keeppeek-desktop-${VERSION}-windows-${ARCH}-installer.exe"
InstallDir "$LOCALAPPDATA\Programs\${PRODUCT_NAME}"
InstallDirRegKey HKCU "Software\KeepPeekDesktop" "InstallDir"
RequestExecutionLevel user
ShowInstDetails show
ShowUninstDetails show

VIProductVersion "${VERSION_RESOURCE}"
VIAddVersionKey "ProductName" "${PRODUCT_NAME}"
VIAddVersionKey "FileDescription" "${PRODUCT_NAME} installer"
VIAddVersionKey "LegalCopyright" "Copyright (C) 2026 Marcus Asteborg"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "ProductVersion" "${VERSION}"

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

Section "KeepPeek Desktop" SecDesktop
  SectionIn RO
  SetOutPath "$INSTDIR"
  File /oname=keeppeek-desktop.exe "${BUILD_DIR}\keeppeek-desktop.exe"
  WriteUninstaller "$INSTDIR\uninstall.exe"

  CreateDirectory "${START_MENU_DIR}"
  CreateShortCut "${START_MENU_DIR}\${PRODUCT_NAME}.lnk" "$INSTDIR\keeppeek-desktop.exe"
  CreateShortCut "${START_MENU_DIR}\Uninstall ${PRODUCT_NAME}.lnk" "$INSTDIR\uninstall.exe"

  WriteRegStr HKCU "Software\KeepPeekDesktop" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayName" "${PRODUCT_NAME}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayIcon" "$INSTDIR\keeppeek-desktop.exe"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "UninstallString" "$\"$INSTDIR\uninstall.exe$\""
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoRepair" 1
SectionEnd

Section "Uninstall"
  ; Remove only desktop client binaries and shortcuts. Keep config.toml and KeepPeekService intact.
  Delete "${START_MENU_DIR}\${PRODUCT_NAME}.lnk"
  Delete "${START_MENU_DIR}\Uninstall ${PRODUCT_NAME}.lnk"
  RMDir "${START_MENU_DIR}"
  Delete "$INSTDIR\keeppeek-desktop.exe"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"
  DeleteRegKey HKCU "${UNINSTALL_KEY}"
  DeleteRegKey HKCU "Software\KeepPeekDesktop"
SectionEnd

Function .onInit
!if "${ARCH}" == "aarch64"
  ${IfNot} ${IsNativeARM64}
    MessageBox MB_ICONSTOP "KeepPeek Desktop requires Windows on ARM."
    Abort
  ${EndIf}
!else
  ${IfNot} ${RunningX64}
    MessageBox MB_ICONSTOP "KeepPeek Desktop requires 64-bit Windows."
    Abort
  ${EndIf}
!endif

  SetRegView 64
  ReadRegStr $0 HKLM "SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}" "pv"
  ${If} $0 == ""
    ReadRegStr $0 HKCU "Software\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}" "pv"
  ${EndIf}
  ${If} $0 == ""
    IfSilent webview_missing_silent
    MessageBox MB_ICONEXCLAMATION "Microsoft Edge WebView2 Runtime was not detected. Install the Evergreen WebView2 Runtime before launching KeepPeek Desktop."
webview_missing_silent:
  ${EndIf}
FunctionEnd
