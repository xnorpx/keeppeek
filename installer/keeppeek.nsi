!ifndef VERSION
!error "VERSION must be defined"
!endif

!ifndef VERSION_RESOURCE
!error "VERSION_RESOURCE must be defined"
!endif

!ifndef ARCH
!error "ARCH must be defined"
!endif

!ifndef OUT_DIR
!error "OUT_DIR must be defined"
!endif

!ifdef WRITE_UNINSTALLER_ONLY
  !ifdef SIGNED_UNINSTALLER
    !error "WRITE_UNINSTALLER_ONLY and SIGNED_UNINSTALLER cannot both be defined"
  !endif
  !ifndef UNINST_OUT
    !error "UNINST_OUT must be defined when WRITE_UNINSTALLER_ONLY is set"
  !endif
!else
  !ifndef BUILD_DIR
    !error "BUILD_DIR must be defined"
  !endif
!endif

Unicode true
SetCompressor /SOLID lzma

!include "LogicLib.nsh"
!include "MUI2.nsh"
!include "StrFunc.nsh"
!ifndef WRITE_UNINSTALLER_ONLY
  !include "FileFunc.nsh"
  !include "Sections.nsh"
  !include "x64.nsh"
  ${Using:StrFunc} StrStr
!endif
!ifndef SIGNED_UNINSTALLER
  ${Using:StrFunc} UnStrStr
!endif

!define PRODUCT_NAME "KeepPeek"
!define SERVICE_NAME "KeepPeekService"
!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\KeepPeek"
!define START_MENU_DIR "$SMPROGRAMS\${PRODUCT_NAME}"

Name "${PRODUCT_NAME} ${VERSION} (${ARCH})"
!ifdef WRITE_UNINSTALLER_ONLY
  OutFile "${OUT_DIR}\keeppeek-uninst-gen.exe"
  SilentInstall silent
  AutoCloseWindow true
!else
  OutFile "${OUT_DIR}\keeppeek-${VERSION}-windows-${ARCH}-installer.exe"
  ShowInstDetails show
!endif
InstallDir "$PROGRAMFILES64\${PRODUCT_NAME}"
InstallDirRegKey HKLM "Software\KeepPeek" "InstallDir"
RequestExecutionLevel admin
!ifndef SIGNED_UNINSTALLER
  ShowUninstDetails show
!endif

VIProductVersion "${VERSION_RESOURCE}"
VIAddVersionKey "ProductName" "${PRODUCT_NAME}"
VIAddVersionKey "FileDescription" "${PRODUCT_NAME} installer"
VIAddVersionKey "LegalCopyright" "Copyright (C) 2026 Marcus Asteborg"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "ProductVersion" "${VERSION}"

!ifndef WRITE_UNINSTALLER_ONLY
  !insertmacro MUI_PAGE_WELCOME
  !insertmacro MUI_PAGE_DIRECTORY
  !define MUI_PAGE_CUSTOMFUNCTION_LEAVE ComponentsPageLeave
  !insertmacro MUI_PAGE_COMPONENTS
  !insertmacro MUI_PAGE_INSTFILES
  !insertmacro MUI_PAGE_FINISH
!endif

!ifndef SIGNED_UNINSTALLER
  !insertmacro MUI_UNPAGE_CONFIRM
  !insertmacro MUI_UNPAGE_INSTFILES
!endif

!insertmacro MUI_LANGUAGE "English"

!ifdef WRITE_UNINSTALLER_ONLY
Section ""
  WriteUninstaller "${UNINST_OUT}"
SectionEnd
!else
Section "-StopServiceBeforeUpgrade"
  ; Stop the existing service before any section overwrites keeppeek.exe.
  Call StopServiceBeforeUpgrade
SectionEnd

Section "KeepPeek Desktop" SecDesktop
  SetOutPath "$INSTDIR"
  File /oname=keeppeek-desktop.exe "${BUILD_DIR}\keeppeek-desktop.exe"

  SetShellVarContext all
  CreateDirectory "${START_MENU_DIR}"
  CreateShortCut "${START_MENU_DIR}\KeepPeek Desktop.lnk" "$INSTDIR\keeppeek-desktop.exe"

  ReadRegStr $0 HKLM "SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}" "pv"
  ${If} $0 == ""
    ReadRegStr $0 HKCU "Software\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}" "pv"
  ${EndIf}
  ${If} $0 == ""
    IfSilent webview_missing_silent
    MessageBox MB_ICONEXCLAMATION "Microsoft Edge WebView2 Runtime was not detected. Install the Evergreen WebView2 Runtime before launching KeepPeek Desktop."
webview_missing_silent:
  ${EndIf}
SectionEnd

Section "KeepPeek Server with TUI" SecServer
  SetOutPath "$INSTDIR"
  File /oname=keeppeek.exe "${BUILD_DIR}\keeppeek.exe"
  File /oname=keeppeek-supervisor.exe "${BUILD_DIR}\keeppeek-supervisor.exe"
SectionEnd

Section /o "KeepPeek Windows Service" SecService
  SetOutPath "$INSTDIR"
  File /oname=keeppeek.exe "${BUILD_DIR}\keeppeek.exe"
  File /oname=keeppeek-service.exe "${BUILD_DIR}\keeppeek-service.exe"
  File /oname=keeppeek-supervisor.exe "${BUILD_DIR}\keeppeek-supervisor.exe"
  Call InstallService
SectionEnd

Section "-UninstallSupport"
  SetOutPath "$INSTDIR"
!ifdef SIGNED_UNINSTALLER
  File /oname=uninstall.exe "${SIGNED_UNINSTALLER}"
!else
  WriteUninstaller "$INSTDIR\uninstall.exe"
!endif

  ${If} ${SectionIsSelected} ${SecDesktop}
    SetShellVarContext all
    CreateDirectory "${START_MENU_DIR}"
    CreateShortCut "${START_MENU_DIR}\Uninstall ${PRODUCT_NAME}.lnk" "$INSTDIR\uninstall.exe"
  ${EndIf}

  StrCpy $0 "$INSTDIR\uninstall.exe"
  ${If} ${FileExists} "$INSTDIR\keeppeek-desktop.exe"
    StrCpy $0 "$INSTDIR\keeppeek-desktop.exe"
  ${ElseIf} ${FileExists} "$INSTDIR\keeppeek.exe"
    StrCpy $0 "$INSTDIR\keeppeek.exe"
  ${ElseIf} ${FileExists} "$INSTDIR\keeppeek-service.exe"
    StrCpy $0 "$INSTDIR\keeppeek-service.exe"
  ${EndIf}

  WriteRegStr HKLM "Software\KeepPeek" "InstallDir" "$INSTDIR"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "DisplayName" "${PRODUCT_NAME}"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "DisplayIcon" "$0"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "UninstallString" "$\"$INSTDIR\uninstall.exe$\""
  WriteRegDWORD HKLM "${UNINSTALL_KEY}" "NoModify" 1
  WriteRegDWORD HKLM "${UNINSTALL_KEY}" "NoRepair" 1
SectionEnd

!insertmacro MUI_FUNCTION_DESCRIPTION_BEGIN
  !insertmacro MUI_DESCRIPTION_TEXT ${SecDesktop} "Install the native KeepPeek Desktop UI (keeppeek-desktop.exe) and Start Menu shortcut."
  !insertmacro MUI_DESCRIPTION_TEXT ${SecServer} "Install the interactive KeepPeek Server with TUI (keeppeek.exe) and supervisor CLI (keeppeek-supervisor.exe)."
  !insertmacro MUI_DESCRIPTION_TEXT ${SecService} "Install keeppeek-service.exe, keeppeek.exe, and keeppeek-supervisor.exe, and configure KeepPeekService in the Windows Service Control Manager."
!insertmacro MUI_FUNCTION_DESCRIPTION_END
!endif

!ifndef SIGNED_UNINSTALLER
Section "Uninstall"
  ; Remove only installed binaries, service registration, and shortcuts. Keep user config.toml intact.
  SetRegView 64
  SetShellVarContext all
  Call un.StopAndRemoveService
  Delete "${START_MENU_DIR}\KeepPeek Desktop.lnk"
  Delete "${START_MENU_DIR}\Uninstall ${PRODUCT_NAME}.lnk"
  RMDir "${START_MENU_DIR}"
  Delete "$INSTDIR\keeppeek.exe"
  Delete "$INSTDIR\keeppeek-service.exe"
  Delete "$INSTDIR\keeppeek-supervisor.exe"
  Delete "$INSTDIR\keeppeek-desktop.exe"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"
  DeleteRegKey HKLM "${UNINSTALL_KEY}"
  DeleteRegKey HKLM "Software\KeepPeek"
SectionEnd
!endif

!ifndef WRITE_UNINSTALLER_ONLY
!macro ApplySectionOption section enable_flag disable_flag
  ${GetOptions} $CMDLINE "${enable_flag}" $0
  ${IfNot} ${Errors}
    !insertmacro SelectSection ${section}
  ${EndIf}
  ${GetOptions} $CMDLINE "${disable_flag}" $0
  ${IfNot} ${Errors}
    !insertmacro UnselectSection ${section}
  ${EndIf}
!macroend

Function StopServiceBeforeUpgrade
  ${If} ${SectionIsSelected} ${SecService}
    Call StopAndRemoveService
  ${EndIf}
FunctionEnd

Function ComponentsPageLeave
  ${IfNot} ${SectionIsSelected} ${SecDesktop}
  ${AndIfNot} ${SectionIsSelected} ${SecServer}
  ${AndIfNot} ${SectionIsSelected} ${SecService}
    MessageBox MB_ICONEXCLAMATION "Select at least one component to install (KeepPeek Desktop, KeepPeek Server with TUI, or KeepPeek Windows Service)."
    Abort
  ${EndIf}
FunctionEnd

Function .onInit
!if "${ARCH}" == "aarch64"
  ${IfNot} ${IsNativeARM64}
    MessageBox MB_ICONSTOP "KeepPeek requires Windows on ARM."
    Abort
  ${EndIf}
!else
  ${IfNot} ${RunningX64}
    MessageBox MB_ICONSTOP "KeepPeek requires 64-bit Windows."
    Abort
  ${EndIf}
!endif

  SetRegView 64

  !insertmacro ApplySectionOption ${SecDesktop} "/DESKTOP" "/NO_DESKTOP"
  !insertmacro ApplySectionOption ${SecServer} "/SERVER" "/NO_SERVER"

  StrCpy $2 "0"
  ${GetOptions} $CMDLINE "/SERVICE" $0
  ${IfNot} ${Errors}
    !insertmacro SelectSection ${SecService}
    StrCpy $2 "1"
  ${EndIf}
  ${GetOptions} $CMDLINE "/NO_SERVICE" $0
  ${IfNot} ${Errors}
    !insertmacro UnselectSection ${SecService}
    StrCpy $2 "1"
  ${EndIf}
  ${If} $2 == "0"
    nsExec::ExecToStack '"$SYSDIR\sc.exe" query "${SERVICE_NAME}"'
    Pop $0
    Pop $1
    ${If} $0 == 0
      !insertmacro SelectSection ${SecService}
    ${EndIf}
  ${EndIf}

  IfSilent 0 init_done
    ${IfNot} ${SectionIsSelected} ${SecDesktop}
    ${AndIfNot} ${SectionIsSelected} ${SecServer}
    ${AndIfNot} ${SectionIsSelected} ${SecService}
      SetErrorLevel 2
      Abort
    ${EndIf}
init_done:
FunctionEnd
!endif

!macro StopAndRemoveService prefix
Function ${prefix}StopAndRemoveService
  nsExec::ExecToStack '"$SYSDIR\sc.exe" query "${SERVICE_NAME}"'
  Pop $0
  Pop $1
  ${If} $0 != 0
    Return
  ${EndIf}

  DetailPrint "Stopping ${SERVICE_NAME}"
  ExecWait '"$SYSDIR\sc.exe" stop "${SERVICE_NAME}"' $0
  Call ${prefix}WaitForServiceStop
  DetailPrint "Removing ${SERVICE_NAME}"
  ExecWait '"$SYSDIR\sc.exe" delete "${SERVICE_NAME}"' $0
  Call ${prefix}WaitForServiceRemoval
FunctionEnd

Function ${prefix}WaitForServiceStop
  StrCpy $0 0
wait:
  nsExec::ExecToStack '"$SYSDIR\sc.exe" query "${SERVICE_NAME}"'
  Pop $1
  Pop $2
  ${If} $1 == 1060
    Return
  ${EndIf}
  !if "${prefix}" == "un."
    ${UnStrStr} $3 $2 "STOPPED"
  !else
    ${StrStr} $3 $2 "STOPPED"
  !endif
  ${If} $3 != ""
    Return
  ${EndIf}
  IntOp $0 $0 + 1
  ${If} $0 >= 60
    IfSilent stop_timeout
    MessageBox MB_ICONSTOP "${SERVICE_NAME} did not stop. Stop it manually and run the installer again."
stop_timeout:
    Abort
  ${EndIf}
  Sleep 1000
  Goto wait
FunctionEnd

Function ${prefix}WaitForServiceRemoval
  StrCpy $0 0
wait:
  nsExec::ExecToStack '"$SYSDIR\sc.exe" query "${SERVICE_NAME}"'
  Pop $1
  Pop $2
  ${If} $1 == 1060
    Return
  ${EndIf}
  IntOp $0 $0 + 1
  ${If} $0 >= 60
    IfSilent removal_timeout
    MessageBox MB_ICONSTOP "${SERVICE_NAME} could not be removed. Reboot Windows and run the installer again."
removal_timeout:
    Abort
  ${EndIf}
  Sleep 1000
  Goto wait
FunctionEnd
!macroend

!ifndef WRITE_UNINSTALLER_ONLY
  !insertmacro StopAndRemoveService ""
!endif
!ifndef SIGNED_UNINSTALLER
  !insertmacro StopAndRemoveService "un."
!endif

!ifndef WRITE_UNINSTALLER_ONLY
Function InstallService
  DetailPrint "Installing ${SERVICE_NAME}"
  ExecWait '"$SYSDIR\sc.exe" create "${SERVICE_NAME}" binPath= "$\"$INSTDIR\keeppeek-service.exe$\"" start= auto DisplayName= "${PRODUCT_NAME} Service"' $0
  ${If} $0 != 0
    DetailPrint "${SERVICE_NAME} could not be created"
    IfSilent create_failed
    MessageBox MB_ICONSTOP "KeepPeek was installed, but ${SERVICE_NAME} could not be created. Install it manually with sc.exe."
create_failed:
    Return
  ${EndIf}

  ExecWait '"$SYSDIR\sc.exe" failure "${SERVICE_NAME}" reset= 86400 actions= restart/5000/restart/15000/restart/30000' $0
  ${If} $0 != 0
    DetailPrint "Warning: could not configure ${SERVICE_NAME} failure recovery actions (exit $0)"
  ${EndIf}
  ExecWait '"$SYSDIR\sc.exe" failureflag "${SERVICE_NAME}" 1' $0
  ${If} $0 != 0
    DetailPrint "Warning: could not enable ${SERVICE_NAME} non-crash failure recovery flag (exit $0)"
  ${EndIf}

  ExecWait '"$SYSDIR\sc.exe" start "${SERVICE_NAME}"' $0
  ${If} $0 != 0
    DetailPrint "${SERVICE_NAME} could not be started"
    IfSilent start_failed
    MessageBox MB_ICONEXCLAMATION "${SERVICE_NAME} was installed but could not be started. Check its configuration and logs."
start_failed:
  ${EndIf}
FunctionEnd
!endif
