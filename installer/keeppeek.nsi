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

!include "FileFunc.nsh"
!include "LogicLib.nsh"
!include "MUI2.nsh"
!include "Sections.nsh"
!include "StrFunc.nsh"
!include "x64.nsh"

${Using:StrFunc} StrStr
${Using:StrFunc} UnStrStr

!define PRODUCT_NAME "KeepPeek"
!define SERVICE_NAME "KeepPeekService"
!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\KeepPeek"

Name "${PRODUCT_NAME} ${VERSION} (${ARCH})"
OutFile "${OUT_DIR}\keeppeek-${VERSION}-windows-${ARCH}-installer.exe"
InstallDir "$PROGRAMFILES64\${PRODUCT_NAME}"
InstallDirRegKey HKLM "Software\KeepPeek" "InstallDir"
RequestExecutionLevel admin
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
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

Section "KeepPeek binaries" SecBinaries
  SectionIn RO
  Call StopAndRemoveService
  SetOutPath "$INSTDIR"
  File /oname=keeppeek.exe "${BUILD_DIR}\keeppeek.exe"
  File /oname=keeppeek-service.exe "${BUILD_DIR}\keeppeek-service.exe"
  WriteUninstaller "$INSTDIR\uninstall.exe"
  WriteRegStr HKLM "Software\KeepPeek" "InstallDir" "$INSTDIR"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "DisplayName" "${PRODUCT_NAME}"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "DisplayIcon" "$INSTDIR\keeppeek.exe"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "UninstallString" "$\"$INSTDIR\uninstall.exe$\""
  WriteRegDWORD HKLM "${UNINSTALL_KEY}" "NoModify" 1
  WriteRegDWORD HKLM "${UNINSTALL_KEY}" "NoRepair" 1
SectionEnd

Section /o "Run KeepPeek as a Windows service" SecService
  Call InstallService
SectionEnd

Section "Uninstall"
  Call un.StopAndRemoveService
  Delete "$INSTDIR\keeppeek.exe"
  Delete "$INSTDIR\keeppeek-service.exe"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"
  DeleteRegKey HKLM "${UNINSTALL_KEY}"
  DeleteRegKey HKLM "Software\KeepPeek"
SectionEnd

Function .onInit
!if "${ARCH}" == "aarch64"
  ${IfNot} ${IsNativeARM64}
    MessageBox MB_ICONSTOP "KeepPeek requires Windows on ARM."
    Abort
  ${EndIf}
!else
  ${IfNot} ${IsNativeAMD64}
    MessageBox MB_ICONSTOP "KeepPeek requires 64-bit Windows on x64."
    Abort
  ${EndIf}
!endif

  SetRegView 64
  ${GetOptions} $CMDLINE "/SERVICE" $0
  ${IfNot} ${Errors}
    SectionGetFlags ${SecService} $1
    IntOp $1 $1 | ${SF_SELECTED}
    SectionSetFlags ${SecService} $1
  ${Else}
    nsExec::ExecToStack '"$SYSDIR\sc.exe" query "${SERVICE_NAME}"'
    Pop $0
    Pop $1
    ${If} $0 == 0
      SectionGetFlags ${SecService} $1
      IntOp $1 $1 | ${SF_SELECTED}
      SectionSetFlags ${SecService} $1
    ${EndIf}
  ${EndIf}
FunctionEnd

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

!insertmacro StopAndRemoveService ""
!insertmacro StopAndRemoveService "un."

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

  ExecWait '"$SYSDIR\sc.exe" start "${SERVICE_NAME}"' $0
  ${If} $0 != 0
    DetailPrint "${SERVICE_NAME} could not be started"
    IfSilent start_failed
    MessageBox MB_ICONEXCLAMATION "${SERVICE_NAME} was installed but could not be started. Check its configuration and logs."
start_failed:
  ${EndIf}
FunctionEnd
