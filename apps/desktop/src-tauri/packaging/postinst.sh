#!/bin/sh
# Linux packages (deb / rpm): after installing, download the programs video sites need (yt-dlp, Deno,
# ffmpeg) into /usr/lib/velox-fetch/tools, so videos work as soon as VeloxFetch opens. VeloxFetch does the
# download itself (fixed official HTTPS sources, SHA-256 checked). Never fails the installation: whatever
# could not be downloaded, the app gets at start-up.
mkdir -p /usr/lib/velox-fetch/tools
if command -v velox-desktop >/dev/null 2>&1; then
  velox-desktop --install-video-tools || true
fi
exit 0
