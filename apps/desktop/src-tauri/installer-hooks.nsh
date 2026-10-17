; Extra steps of the Windows setup (Tauri NSIS installer hooks).
;
; After the files are copied, the setup downloads the programs video sites need (yt-dlp, Deno, ffmpeg)
; into "<install folder>\tools", so videos work as soon as VeloxFetch opens. The app does the download
; itself (fixed official HTTPS sources, every file checked against its published SHA-256, see
; crates/download-manager/src/manager/tools.rs) and prints its progress, shown in the setup's details.
; It never makes the setup fail: whatever could not be downloaded (no internet), the app gets at start-up.
;
; It also points Chrome, Edge and Brave at the VeloxFetch extension in the Chrome Web Store, for every user of this
; computer (Chromium's documented "external extensions": HKLM\Software\<browser>\Extensions\<id>, value update_url,
; in the 32-bit registry view where the browsers read it). The browser downloads the extension from its store and
; shows its own "New extension added" message: the user enables it with one click, or removes it like any other
; extension. No browser file or profile is touched. The id is CHROME_STORE_ID (crates/browser-bridge/src/caller.rs)
; and the keys are WINDOWS_MACHINE_KEYS (crates/system-integration/src/store_extension.rs); a test keeps them in step.

!define VELOX_EXTENSION_ID "keobnkoppcchnbpmmfodidkcedglbhnn"
!define VELOX_STORE_UPDATE_URL "https://clients2.google.com/service/update2/crx"

; The setup works in the 64-bit view on 64-bit Windows: back to it after the browsers' keys.
!macro VELOX_REG_VIEW_BACK
  ${If} ${RunningX64}
    !if "${ARCH}" != "x86"
      SetRegView 64
    !endif
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ; veloxfetch:// links start VeloxFetch (for every user of this computer, like the program itself). The link is
  ; only ever a request to open the app: it carries nothing the app acts on.
  WriteRegStr HKLM "Software\Classes\veloxfetch" "" "URL:VeloxFetch link"
  WriteRegStr HKLM "Software\Classes\veloxfetch" "URL Protocol" ""
  WriteRegStr HKLM "Software\Classes\veloxfetch\DefaultIcon" "" '"$INSTDIR\${MAINBINARYNAME}.exe",0'
  WriteRegStr HKLM "Software\Classes\veloxfetch\shell\open\command" "" '"$INSTDIR\${MAINBINARYNAME}.exe" "%1"'

  SetRegView 32
  WriteRegStr HKLM "Software\Google\Chrome\Extensions\${VELOX_EXTENSION_ID}" "update_url" "${VELOX_STORE_UPDATE_URL}"
  WriteRegStr HKLM "Software\Microsoft\Edge\Extensions\${VELOX_EXTENSION_ID}" "update_url" "${VELOX_STORE_UPDATE_URL}"
  WriteRegStr HKLM "Software\BraveSoftware\Brave-Browser\Extensions\${VELOX_EXTENSION_ID}" "update_url" "${VELOX_STORE_UPDATE_URL}"
  !insertmacro VELOX_REG_VIEW_BACK

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
    ; The browsers stop offering the extension (one a user already added stays, like any other extension).
    SetRegView 32
    DeleteRegKey HKLM "Software\Google\Chrome\Extensions\${VELOX_EXTENSION_ID}"
    DeleteRegKey HKLM "Software\Microsoft\Edge\Extensions\${VELOX_EXTENSION_ID}"
    DeleteRegKey HKLM "Software\BraveSoftware\Brave-Browser\Extensions\${VELOX_EXTENSION_ID}"
    !insertmacro VELOX_REG_VIEW_BACK
  ${EndIf}
  ; The downloaded tools are not in the setup's file list: remove them with the program (but keep them
  ; when this uninstall is part of an update: the new version uses them).
  ${If} $UpdateMode <> 1
    RMDir /r "$INSTDIR\tools"
    RMDir "$INSTDIR"
  ${EndIf}
!macroend
