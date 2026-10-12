// The engine explains an error in English (`explain.rs`) and gives it a stable code. The UI shows that sentence in
// the user's language, by code; an unknown code (or English) shows the engine's own text unchanged.

import { getLang, t, type Key } from "./i18n";

const BY_CODE: Record<string, Key> = {
  http_401: "e.401",
  http_407: "e.407",
  http_403: "e.403",
  http_404: "e.404",
  http_410: "e.404",
  http_416: "e.416",
  http_429: "e.429",
  http_408: "e.408",
  http_504: "e.408",
  disk_full: "e.disk",
  access_denied: "e.access",
  folder_not_allowed: "e.folder",
  web_page: "e.webPage",
  checksum_mismatch: "e.checksum",
  source_changed: "e.sourceChanged",
  file_exists: "e.exists",
  incomplete: "e.incomplete",
  redirect_refused: "e.redirect",
  dns: "e.dns",
  timeout: "e.timeout",
  connection: "e.connection",
  proxy_invalid: "e.proxy",
};

/** The sentence for a download's error in the current language. */
export function localError(code: string | null | undefined, english: string | null | undefined): string {
  const text = english ?? "";
  if (getLang() === "en" || !code) return text;
  if (code === "disk_full") {
    // "this file needs 1.5 GB, only 200.0 MB is free"
    const m = /needs (.+?), only (.+?) is free/.exec(text);
    if (m) return t("e.diskNumbers", { need: m[1]!, free: m[2]! });
  }
  const key = BY_CODE[code];
  if (key) return t(key);
  const http = /^http_(\d{3})$/.exec(code);
  if (http) return t(Number(http[1]) >= 500 ? "e.5xx" : "e.http", { code: http[1]! });
  return text;
}
