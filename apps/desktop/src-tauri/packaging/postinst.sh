#!/bin/sh
# Linux packages (deb / rpm), after installing:
#
# 1. The VeloxFetch extension from the Chrome Web Store, for every user: Chrome, Brave (it reads Chromium's folder),
#    Chromium and Edge find a small pointer "extension <id>, updated from the Chrome Web Store" in their system
#    folder and add the published extension themselves (Chromium's documented "external extensions"). Only the
#    pointer is written: no browser file or profile is touched, the browser downloads the extension from its store,
#    and the user can remove it like any other extension. The id is CHROME_STORE_ID in
#    crates/browser-bridge/src/caller.rs and the folders are LINUX_SYSTEM_FOLDERS in
#    crates/system-integration/src/store_extension.rs (a test keeps them in step).
#
# 2. The programs video sites need (yt-dlp, Deno, ffmpeg), into /usr/lib/velox-fetch/tools, so videos work as soon as
#    VeloxFetch opens. They are in this package, compressed (/usr/lib/VeloxFetch/video-tools): VeloxFetch unpacks them
#    with no internet and checks each against the SHA-256 in its list, then removes the compressed copies. Only a
#    package built without them downloads them (fixed official HTTPS sources, SHA-256 checked).
#
# Never fails the installation: whatever could not be done, the app does at start-up (or from Settings > Browser).
VELOX_EXTENSION_ID=keobnkoppcchnbpmmfodidkcedglbhnn
for dir in /usr/share/google-chrome/extensions /usr/share/chromium/extensions /usr/share/microsoft-edge/extensions; do
  if mkdir -p "$dir" 2>/dev/null; then
    printf '{"external_update_url":"https://clients2.google.com/service/update2/crx"}\n' > "$dir/$VELOX_EXTENSION_ID.json.tmp" &&
      chmod 644 "$dir/$VELOX_EXTENSION_ID.json.tmp" &&
      mv -f "$dir/$VELOX_EXTENSION_ID.json.tmp" "$dir/$VELOX_EXTENSION_ID.json" || rm -f "$dir/$VELOX_EXTENSION_ID.json.tmp"
  fi
done

mkdir -p /usr/lib/velox-fetch/tools
if command -v velox-desktop >/dev/null 2>&1; then
  velox-desktop --install-video-tools || true
fi
exit 0
