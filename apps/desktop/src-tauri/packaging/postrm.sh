#!/bin/sh
# Removed with the program (not on an upgrade): the downloaded video tools, and the pointer to the extension in the
# browsers' system folders (see postinst.sh). The browsers keep an extension a user already added: removing VeloxFetch
# only stops them adding it for new profiles. A folder this package created is removed only when it is left empty.
VELOX_EXTENSION_ID=keobnkoppcchnbpmmfodidkcedglbhnn
case "$1" in
  remove|purge|0)
    rm -rf /usr/lib/velox-fetch/tools ; rmdir /usr/lib/velox-fetch 2>/dev/null || true
    for dir in /usr/share/google-chrome/extensions /usr/share/chromium/extensions /usr/share/microsoft-edge/extensions; do
      rm -f "$dir/$VELOX_EXTENSION_ID.json"
      rmdir "$dir" 2>/dev/null && rmdir "$(dirname "$dir")" 2>/dev/null
    done
    true
    ;;
esac
exit 0
