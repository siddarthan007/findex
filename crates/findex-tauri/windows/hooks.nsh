!include "LogicLib.nsh"
!include "StrFunc.nsh"
!include "WinMessages.nsh"
${StrRep}
${UnStrRep}

!macro FINDEX_NORMALIZE_USER_PATH OUTPUT INPUT
  ${StrRep} ${OUTPUT} ${INPUT} ";$INSTDIR" ""
  ${StrRep} ${OUTPUT} ${OUTPUT} "$INSTDIR;" ""
  ${If} ${OUTPUT} == "$INSTDIR"
    StrCpy ${OUTPUT} ""
  ${EndIf}
!macroend

!macro FINDEX_UNINSTALL_NORMALIZE_USER_PATH OUTPUT INPUT
  ${UnStrRep} ${OUTPUT} ${INPUT} ";$INSTDIR" ""
  ${UnStrRep} ${OUTPUT} ${OUTPUT} "$INSTDIR;" ""
  ${If} ${OUTPUT} == "$INSTDIR"
    StrCpy ${OUTPUT} ""
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ; The bundled `findex` sidecar is the CLI and contains the TUI subcommand.
  ; Normalize first so repair installs never duplicate the PATH segment.
  ReadRegStr $0 HKCU "Environment" "Path"
  !insertmacro FINDEX_NORMALIZE_USER_PATH $1 $0
  ${If} $1 == ""
    WriteRegExpandStr HKCU "Environment" "Path" "$INSTDIR"
  ${Else}
    WriteRegExpandStr HKCU "Environment" "Path" "$1;$INSTDIR"
  ${EndIf}
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\App Paths\findex.exe" "" "$INSTDIR\findex.exe"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\App Paths\findex.exe" "Path" "$INSTDIR"
  SendMessage ${HWND_BROADCAST} ${WM_SETTINGCHANGE} 0 "STR:Environment" /TIMEOUT=5000
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  ReadRegStr $0 HKCU "Environment" "Path"
  !insertmacro FINDEX_UNINSTALL_NORMALIZE_USER_PATH $1 $0
  ${If} $1 == ""
    DeleteRegValue HKCU "Environment" "Path"
  ${Else}
    WriteRegExpandStr HKCU "Environment" "Path" "$1"
  ${EndIf}
  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\App Paths\findex.exe"
  SendMessage ${HWND_BROADCAST} ${WM_SETTINGCHANGE} 0 "STR:Environment" /TIMEOUT=5000
!macroend
