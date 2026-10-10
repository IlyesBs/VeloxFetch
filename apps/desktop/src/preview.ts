// Where the media player reads a preview from: the app's own preview protocol by id (never a path), or, where the
// system's web view cannot stream a growing file from it (Linux, macOS), the app's private local address that serves
// the same bytes.

import { convertFileSrc } from "@tauri-apps/api/core";
import { api } from "./api";

let base: string | null = null;
let loading: Promise<void> | null = null;

/** Ask the app once (at start-up and before a player opens); a failure keeps the preview protocol. */
export function loadPreviewBase(): Promise<void> {
  loading ??= api.getPreviewBase().then(
    (b) => {
      base = b && /^http:\/\/127\.0\.0\.1:\d+\/[0-9a-f]{64}\/$/.test(b) ? b : null;
    },
    () => {
      loading = null; // try again next time
    },
  );
  return loading;
}

/** `path`: `12` (a download) or `m3` (a video job). */
export function previewSrc(path: string, from: string | null = base): string {
  return from ? `${from}${path}` : convertFileSrc(path, "velox-preview");
}
