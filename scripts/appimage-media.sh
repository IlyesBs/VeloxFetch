#!/usr/bin/env bash
# The GStreamer plugins the Linux AppImage carries for live preview, and nothing else.
#
# linuxdeploy's GStreamer plugin copies EVERY file of the folder it is given. A system folder also holds plugins
# for the machine's own hardware (VA-API, NVIDIA, Intel QSV, V4L2, Vulkan, KMS...) and for devices and services.
# Carried into the AppImage, they run against another computer's graphics driver: plugin discovery or playback
# then crashes the whole app at start ("Segmentation fault"). So the AppImage gets a folder holding only the
# software plugins that play the formats the preview supports (docs/smart-throttle-and-preview.md); a format
# outside them only shows "can't play" on the card, never a crash.
#
# Sourced by build-release.sh; also runs alone: appimage-media.sh stage <system plugins dir> <new dir>

# Software playback only. Names without the "libgst" prefix and the ".so" suffix; absent ones are skipped
# (distributions split and rename plugins between versions).
APPIMAGE_GST_PLUGINS="
  coreelements typefindfunctions playback app autodetect pbtypes gio rawparse subparse overlaycomposition
  audioconvert audioresample audiorate audiomixer volume interleave
  videoconvertscale videoconvert videoscale videorate videofilter deinterlace opengl
  pulseaudio alsa
  isomp4 matroska avi ogg wavparse flac audioparsers apetag id3demux mpegtsdemux mpegpsdemux aiff
  videoparsersbad opusparse codecalpha
  libav vorbis opus theora vpx mpg123 dav1d openh264 fdkaac
"

# Never carried, whatever the list says: hardware, GPU, device, capture and network-service plugins.
APPIMAGE_GST_DENY_RE='^(va|vaapi|nvcodec|nvenc|nvdec|qsv|msdk|amfcodec|v4l2codecs|video4linux2|vulkan|kms|uvch264|decklink|dc1394|1394|dvb|openni2|pipewire|jack|ximagesrc|soup|curl|rtsp|webrtc|onnx)$'

# The plugins the preview cannot play without (a missing one means no media bundle at all).
APPIMAGE_GST_REQUIRED="coreelements typefindfunctions playback app audioconvert isomp4 libav"

# stage_appimage_gst_plugins SRC DST: copy the allowed plugins found in SRC into the empty folder DST. Prints
# the required plugins missing from SRC (empty: all there).
stage_appimage_gst_plugins() {
  local src="$1" dst="$2" name missing=""
  rm -rf "$dst" && mkdir -p "$dst" || return 1
  for name in $APPIMAGE_GST_PLUGINS; do
    [[ "$name" =~ $APPIMAGE_GST_DENY_RE ]] && continue
    [ -f "$src/libgst$name.so" ] && cp -L "$src/libgst$name.so" "$dst/"
  done
  for name in $APPIMAGE_GST_REQUIRED; do
    [ -f "$dst/libgst$name.so" ] || missing="$missing $name"
  done
  echo "$missing"
}

if [ "${BASH_SOURCE[0]}" = "$0" ]; then
  set -euo pipefail
  [ "${1:-}" = stage ] && [ $# = 3 ] || { echo "usage: $0 stage <system plugins dir> <new dir>" >&2; exit 2; }
  missing="$(stage_appimage_gst_plugins "$2" "$3")"
  [ -z "$missing" ] || { echo "missing:$missing" >&2; exit 1; }
fi
