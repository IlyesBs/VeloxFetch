; Extra steps of the Windows setup (Tauri NSIS installer hooks).
;
; After the files are copied, the setup downloads the programs video sites need (yt-dlp, Deno, ffmpeg)
; into "<install folder>\tools", so videos work as soon as VeloxFetch opens. The app does the download
; itself (fixed official HTTPS sources, every file checked against its published SHA-256, see
; crates/download-manager/src/manager/tools.rs) and prints its progress, shown in the setup's details.
; It never makes the setup fail: whatever could not be downloaded (no internet), the app gets at start-up.

!macro NSIS_HOOK_POSTINSTALL
  ; veloxfetch:// links start VeloxFetch (for every user of this computer, like the program itself). The link is
  ; only ever a request to open the app: it carries nothing the app acts on.
  WriteRegStr HKLM "Software\Classes\veloxfetch" "" "URL:VeloxFetch link"
  WriteRegStr HKLM "Software\Classes\veloxfetch" "URL Protocol" ""
  WriteRegStr HKLM "Software\Classes\veloxfetch\DefaultIcon" "" '"$INSTDIR\${MAINBINARYNAME}.exe",0'
  WriteRegStr HKLM "Software\Classes\veloxfetch\shell\open\command" "" '"$INSTDIR\${MAINBINARYNAME}.exe" "%1"'

  DetailPrint "Downloading required dependencies (about 300 MB). This can take a few minutes..."
  SetDetailsPrint both
  nsExec::ExecToLog '"$INSTDIR\${MAINBINARYNAME}.exe" --install-video-tools'
  Pop $0
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  ${If} $UpdateMode <> 1
    DeleteRegKey HKLM "Software\Classes\veloxfetch"
    ; Start at login is the user's choice, stored per user: removed with the program.
    DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "VeloxFetch"
  ${EndIf}
  ; The downloaded tools are not in the setup's file list: remove them with the program (but keep them
  ; when this uninstall is part of an update: the new version uses them).
  ${If} $UpdateMode <> 1
    RMDir /r "$INSTDIR\tools"
    RMDir "$INSTDIR"
  ${EndIf}
!macroend
