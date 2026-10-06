#!/bin/sh
# Remove the downloaded video tools with the program (not on an upgrade).
case "$1" in
  remove|purge|0) rm -rf /usr/lib/velox-fetch/tools ; rmdir /usr/lib/velox-fetch 2>/dev/null || true ;;
esac
exit 0
