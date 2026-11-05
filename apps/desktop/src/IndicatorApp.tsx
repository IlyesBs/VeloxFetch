// The download indicator: a small window of its own on the desktop, over the other programs, with the downloads in
// progress (indicator.rs decides when it shows; this page draws it, sizes it and moves it). It keeps only what it
// shows: the downloads and the video downloads, from the same events as the main window.
import { useEffect } from "react";
import { api, onManagerEvent } from "./api";
import { DownloadIndicator } from "./components/DownloadIndicator";
import { store } from "./hooks";
import { EMPTY_METRICS } from "./store";
import { LANGS, setLang } from "./i18n";
import type { ThemePref } from "./theme";

/** Before the first paint: a see-through page (the bar is the only thing drawn) in the saved theme. The window has
 *  no frame to colour, so this is all `applyTheme` would do here. */
export function applyIndicatorTheme(pref: ThemePref): void {
  const root = document.documentElement;
  root.classList.add("indicator-window");
  if (pref === "system") root.removeAttribute("data-theme");
  else root.setAttribute("data-theme", pref);
}

/** The downloads and videos, kept up to date; reloaded whenever the bar shows again (events are best-effort). */
async function follow(): Promise<() => void> {
  const unlisten = await onManagerEvent((ev) => {
    if (["added", "updated", "removed", "tick", "media", "mediaRemoved"].includes(ev.type)) store.dispatch(ev);
  });
  const reload = async () => {
    try {
      const [downloads, media] = await Promise.all([api.listDownloads(), api.listMediaJobs()]);
      store.load(downloads, EMPTY_METRICS, undefined, undefined, media);
    } catch {
      /* the next events or the next showing fill it in */
    }
  };
  await reload();
  const onVisible = () => document.visibilityState === "visible" && void reload();
  document.addEventListener("visibilitychange", onVisible);
  return () => {
    unlisten();
    document.removeEventListener("visibilitychange", onVisible);
  };
}

/** The theme and the language chosen in the main window apply here at once (theme.ts and i18n.ts save them; this
 *  page only hears the change, it stores nothing). */
function followLook(e: StorageEvent) {
  if (e.key === "velox.theme") applyIndicatorTheme(e.newValue === "light" || e.newValue === "dark" ? e.newValue : "system");
  const lang = e.newValue;
  if (e.key === "velox.lang" && lang && LANGS.some((l) => l.lang === lang)) void setLang(lang);
}

export default function IndicatorApp() {
  useEffect(() => {
    let cleanup: (() => void) | undefined;
    let cancelled = false;
    void follow().then((c) => (cancelled ? c() : (cleanup = c)));
    window.addEventListener("storage", followLook);
    return () => {
      cancelled = true;
      cleanup?.();
      window.removeEventListener("storage", followLook);
    };
  }, []);
  return (
    <div className="indicator-root" data-testid="indicator-window">
      <DownloadIndicator />
    </div>
  );
}
